#![expect(clippy::expect_used, reason = "integration tests fail loudly")]
#![expect(clippy::panic, reason = "integration tests fail loudly")]
//! The mail delivery trust boundary at store open and append.
//!
//! A `mail_delivered` line materializes a tree entry and consumes waiting
//! mailbox rows, so the store validates the whole record like it validates
//! `tool_start` and `turn_end`: the turn must be open, every position must
//! be waiting exactly once with the text the buffered row carries, the
//! waiting set stays inside the shared capacity bound, and the derived
//! entry passes the ordinary duplicate and monotonic checks. Every refusal
//! is a typed `Damaged` verdict on the tampered line's offset; a well-
//! formed delivery reopens, advances the entry index, and keeps crash
//! repair ids unique.

mod support;

use std::{fs, num::NonZeroU64};

use dal_core::{
    AssistantRoute, AssistantStop, Block, CallId, DeliveredMail, Entry, EntryId, EntryKind, Family,
    JournalPart, ListQuery, Mail, MailMode, Product, RawJson, Record, SessionId, TurnId, Usage,
    Workspace, encode,
};
use dal_store::{AbortedTurn, Store, StoreError};
use support::temp_dir::TempDir;

fn entry_id(value: u64) -> EntryId {
    EntryId::new(NonZeroU64::new(value).expect("nonzero test entry id"))
}

fn turn_id(value: u64) -> TurnId {
    TurnId::new(NonZeroU64::new(value).expect("nonzero test turn id"))
}

fn timestamp() -> jiff::Timestamp {
    "2026-09-28T10:15:30.123Z"
        .parse()
        .expect("fixed test timestamp parses")
}

fn usage() -> Usage {
    Usage {
        input_tokens: 7,
        cached_input_tokens: 2,
        output_tokens: 5,
        reasoning_tokens: Some(1),
        cache_write_tokens: 3,
        premium_requests: None,
        cost_usd: None,
    }
}

fn setup(tag: &str) -> (TempDir, Store) {
    let temp = TempDir::new(tag);
    let workspace =
        Workspace::new(temp.path().join("workspace")).expect("temporary workspace is absolute");
    let store = Store::new(temp.path().join("data"), workspace, Product::Dalgona);
    (temp, store)
}

fn user(id: u64, text: &str) -> Record {
    Record::User(Entry {
        id: entry_id(id),
        parent: NonZeroU64::new(id.saturating_sub(1)).map(EntryId::new),
        at: timestamp(),
        kind: EntryKind::User {
            parts: vec![JournalPart::Text { text: text.into() }],
        },
    })
}

fn waiting_mail(to: SessionId, text: &str) -> Record {
    Record::Mail(Mail {
        at: timestamp(),
        from: SessionId::new_v7(),
        to,
        mode: MailMode::NextTurn,
        text: text.into(),
        reply_to: None,
    })
}

fn delivered(turn: u64, entry: u64, mails: &[(u64, &str)]) -> Record {
    Record::MailDelivered {
        at: timestamp(),
        turn: turn_id(turn),
        entry: entry_id(entry),
        mails: mails
            .iter()
            .map(|(position, text)| DeliveredMail {
                position: entry_id(*position),
                text: (*text).into(),
            })
            .collect(),
    }
}

/// An assistant entry that reports one tool call no `tool_result` ever
/// answers, so reopening mints one repair entry past the delivery.
fn assistant_with_unreported_call(id: u64) -> Entry {
    Entry {
        id: entry_id(id),
        parent: NonZeroU64::new(id.saturating_sub(1)).map(EntryId::new),
        at: timestamp(),
        kind: EntryKind::Assistant {
            api: Family::Chat,
            model: "test-model".into(),
            provider: None,
            route: Some(AssistantRoute::Api(Family::Chat)),
            content: vec![Block::ToolCall {
                id: CallId::new("call-unreported"),
                name: "search".into(),
                input: RawJson::parse("{}").expect("empty input parses"),
                replay: None,
            }],
            usage: usage(),
            stop: AssistantStop::Done,
            replay: None,
        },
    }
}

/// Stages a durable journal whose single delivery is well-formed: turn 1
/// open, mail position 1 waiting with text `first`, delivery entry 2.
async fn staged_delivery_journal(tag: &str) -> (TempDir, Store, SessionId) {
    let (temp, store) = setup(tag);
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "durable prefix")])
        .await
        .expect("prefix is durable");
    journal
        .append(vec![Record::TurnStart {
            at: timestamp(),
            turn: turn_id(1),
        }])
        .await
        .expect("turn starts");
    journal
        .append(vec![waiting_mail(id, "first")])
        .await
        .expect("mail waits");
    journal
        .append(vec![delivered(1, 2, &[(1, "first")])])
        .await
        .expect("delivery is durable");
    journal.close().await.expect("session closes");
    (temp, store, id)
}

/// Rewrites the one `mail_delivered` line, leaving every other byte alone.
///
/// Returns the staged bytes and the byte offset of that line, which is
/// where the tamper happened and where a refusal must point.
fn edit_delivered_line(journal: &[u8], from: &str, to: &str) -> (Vec<u8>, u64) {
    let text = std::str::from_utf8(journal).expect("journal is utf-8");
    let mut cursor = 0_u64;
    let mut delivered_at = None;
    let mut out = Vec::with_capacity(journal.len() + to.len());
    for line in text.lines() {
        let line_len = u64::try_from(line.len()).expect("line length fits u64");
        if line.contains("\"mail_delivered\"") {
            assert!(
                line.contains(from),
                "tamper target {from} is in the delivery line: {line}"
            );
            delivered_at = Some(cursor);
            out.extend_from_slice(line.replace(from, to).as_bytes());
        } else {
            out.extend_from_slice(line.as_bytes());
        }
        out.push(b'\n');
        cursor += line_len + 1;
    }
    (out, delivered_at.expect("a mail_delivered line exists"))
}

/// Appends one whole encoded record line to the durable journal bytes and
/// returns the staged bytes plus the appended line's offset.
fn append_line(journal: &[u8], record: &Record) -> (Vec<u8>, u64) {
    let line = encode(record).expect("record encodes");
    let mut staged = journal.to_vec();
    let offset = u64::try_from(staged.len()).expect("journal length fits u64");
    staged.extend_from_slice(&line);
    (staged, offset)
}

fn expect_damaged(error: &StoreError, offset: u64, needle: &str) {
    match error {
        StoreError::Damaged {
            offset: found,
            reason,
            ..
        } => {
            assert_eq!(*found, offset, "the refusal names the offending line");
            assert!(reason.contains(needle), "`{needle}` expected, got {reason}");
        }
        _ => panic!("expected Damaged, got {error:?}"),
    }
}

#[tokio::test]
async fn delivery_outside_the_open_turn_is_refused_at_open() {
    let (_temp, store, id) = staged_delivery_journal("mail-trust-wrong-turn").await;
    let path = store.session_file(id);
    let on_disk = fs::read(&path).expect("read durable journal");
    let (staged, offset) = edit_delivered_line(&on_disk, "\"turn\":1", "\"turn\":99");
    fs::write(&path, &staged).expect("stage the tampered journal");
    let error = store
        .open_session(id)
        .await
        .expect_err("a delivery outside the open turn is refused");
    expect_damaged(&error, offset, "not open");
    assert_eq!(
        fs::read(&path).expect("reread journal"),
        staged,
        "a refused open leaves the journal untouched"
    );
    let second = store
        .open_session(id)
        .await
        .expect_err("open keeps refusing");
    expect_damaged(&second, offset, "not open");
}

#[tokio::test]
async fn delivery_of_a_position_that_never_waited_is_refused() {
    let (_temp, store, id) = staged_delivery_journal("mail-trust-unknown-position").await;
    let path = store.session_file(id);
    let on_disk = fs::read(&path).expect("read durable journal");
    let (staged, offset) = edit_delivered_line(&on_disk, "\"position\":1", "\"position\":424242");
    fs::write(&path, &staged).expect("stage the tampered journal");
    let error = store
        .open_session(id)
        .await
        .expect_err("a delivery of an unknown position is refused");
    expect_damaged(&error, offset, "never waiting");
}

#[tokio::test]
async fn redelivery_of_a_consumed_position_is_refused() {
    let (_temp, store, id) = staged_delivery_journal("mail-trust-redelivery").await;
    let path = store.session_file(id);
    let on_disk = fs::read(&path).expect("read durable journal");
    let repeat = delivered(1, 3, &[(1, "first")]);
    let (staged, offset) = append_line(&on_disk, &repeat);
    fs::write(&path, &staged).expect("stage the repeated delivery");
    let error = store
        .open_session(id)
        .await
        .expect_err("a second delivery of a consumed position is refused");
    expect_damaged(&error, offset, "never waiting");
}

#[tokio::test]
async fn delivery_with_forged_text_is_refused() {
    let (_temp, store, id) = staged_delivery_journal("mail-trust-forged-text").await;
    let path = store.session_file(id);
    let on_disk = fs::read(&path).expect("read durable journal");
    let (staged, offset) =
        edit_delivered_line(&on_disk, "\"text\":\"first\"", "\"text\":\"forged\"");
    fs::write(&path, &staged).expect("stage the forged delivery");
    let error = store
        .open_session(id)
        .await
        .expect_err("a delivery whose text differs from the buffered row is refused");
    expect_damaged(&error, offset, "different text");
}

#[tokio::test]
async fn reused_delivery_entry_is_refused() {
    let (_temp, store) = setup("mail-trust-reused-entry");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "durable prefix")])
        .await
        .expect("prefix is durable");
    journal
        .append(vec![Record::TurnStart {
            at: timestamp(),
            turn: turn_id(1),
        }])
        .await
        .expect("turn starts");
    journal
        .append(vec![waiting_mail(id, "first"), waiting_mail(id, "second")])
        .await
        .expect("two mails wait");
    journal
        .append(vec![delivered(1, 2, &[(1, "first")])])
        .await
        .expect("the first delivery is durable");
    journal.close().await.expect("session closes");
    let path = store.session_file(id);
    let on_disk = fs::read(&path).expect("read durable journal");
    let (staged, offset) = append_line(&on_disk, &delivered(1, 2, &[(2, "second")]));
    fs::write(&path, &staged).expect("stage the reused delivery entry");
    let error = store
        .open_session(id)
        .await
        .expect_err("a delivery that reuses a tree entry is refused");
    expect_damaged(&error, offset, "duplicate entry id 2");
}

#[tokio::test]
async fn one_hundred_waiting_mails_reopen_and_the_101st_is_refused() {
    let (_temp, store) = setup("mail-trust-capacity");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "durable prefix")])
        .await
        .expect("prefix is durable");
    let waiting: Vec<Record> = (1..=100)
        .map(|index| waiting_mail(id, &format!("mail {index}")))
        .collect();
    journal
        .append(waiting)
        .await
        .expect("one hundred waiting mails append");
    let error = journal
        .append(vec![waiting_mail(id, "one mail too many")])
        .await
        .expect_err("the 101st waiting mail is refused at append");
    match &error {
        StoreError::Invalid { reason } => {
            assert!(
                reason.contains("waiting capacity"),
                "`waiting capacity` expected, got {reason}"
            );
        }
        _ => panic!("expected Invalid, got {error:?}"),
    }
    journal.close().await.expect("session closes");
    let (reopened, _) = store
        .open_session(id)
        .await
        .expect("one hundred waiting mails reopen");
    assert_eq!(
        reopened
            .records()
            .iter()
            .filter(|record| matches!(record, Record::Mail(_)))
            .count(),
        100,
        "the refused row never landed"
    );

    // The same bound holds on data at rest: staging the 101st row behind
    // the store's back refuses the open. The open handle holds the session
    // lock, so release it before reopening.
    drop(reopened);
    let path = store.session_file(id);
    let on_disk = fs::read(&path).expect("read durable journal");
    let (staged, offset) = append_line(&on_disk, &waiting_mail(id, "staged past the bound"));
    fs::write(&path, &staged).expect("stage the overflowing journal");
    let error = store
        .open_session(id)
        .await
        .expect_err("a journal past the waiting bound is refused at open");
    expect_damaged(&error, offset, "waiting capacity");
}

#[tokio::test]
async fn a_delivery_that_consumes_no_mail_is_refused() {
    let (_temp, store) = setup("mail-trust-empty-delivery");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "durable prefix")])
        .await
        .expect("prefix is durable");
    journal
        .append(vec![Record::TurnStart {
            at: timestamp(),
            turn: turn_id(1),
        }])
        .await
        .expect("turn starts");
    journal
        .append(vec![waiting_mail(id, "first")])
        .await
        .expect("mail waits");
    journal.close().await.expect("session closes");
    let path = store.session_file(id);
    let on_disk = fs::read(&path).expect("read durable journal");
    // A delivery that consumes no waiting row would materialize the entry
    // and leave the mail waiting, so the next turn would deliver it twice.
    let (staged, offset) = append_line(&on_disk, &delivered(1, 3, &[]));
    fs::write(&path, &staged).expect("stage the empty delivery");
    let error = store
        .open_session(id)
        .await
        .expect_err("a delivery that consumes no mail is refused at open");
    expect_damaged(&error, offset, "consumes no waiting mail");
}

#[tokio::test]
async fn valid_delivery_advances_entry_state_and_keeps_recovery_ids_unique() {
    let (_temp, store) = setup("mail-trust-valid");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "durable prefix")])
        .await
        .expect("prefix is durable");
    journal
        .append(vec![Record::TurnStart {
            at: timestamp(),
            turn: turn_id(1),
        }])
        .await
        .expect("turn starts");
    journal
        .append(vec![Record::Assistant(assistant_with_unreported_call(2))])
        .await
        .expect("assistant entry appends");
    journal
        .append(vec![waiting_mail(id, "first")])
        .await
        .expect("mail waits");
    let delivery = delivered(1, 3, &[(1, "first")]);
    journal
        .append(vec![delivery.clone()])
        .await
        .expect("delivery is durable");
    journal
        .close()
        .await
        .expect("session closes with the turn open");

    let (reopened, report) = store
        .open_session(id)
        .await
        .expect("a valid delivery reopens");
    assert_eq!(
        report.aborted,
        Some(AbortedTurn {
            turn: turn_id(1),
            interrupted: 0,
            not_run: 1,
        }),
        "recovery closes the turn the previous process left open"
    );
    let records = reopened.records();
    assert_eq!(
        records
            .iter()
            .filter(|record| matches!(record, Record::MailDelivered { .. }))
            .count(),
        1,
        "the delivery applies exactly once across reopen"
    );
    assert!(
        records.iter().any(|record| record == &delivery),
        "the delivery record survives reopen unchanged"
    );
    let repair: Vec<&Entry> = records
        .iter()
        .filter_map(|record| match record {
            Record::ToolResult(entry) => Some(entry),
            _ => None,
        })
        .collect();
    assert_eq!(repair.len(), 1, "recovery marks the one unreported call");
    assert_eq!(
        repair[0].id,
        entry_id(4),
        "the repair entry mints past the delivered entry: the delivery advanced the id counter"
    );
    assert_eq!(
        repair[0].parent,
        Some(entry_id(3)),
        "the repair entry parents on the delivered entry: the delivery advanced the leaf"
    );
    let mut ids: Vec<EntryId> = Vec::new();
    for record in records {
        if let Some(entry) = record.entry() {
            ids.push(entry.id);
        }
    }
    let unique = ids.clone();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), unique.len(), "no entry id repeats after reopen");
    assert_eq!(
        unique,
        vec![entry_id(1), entry_id(2), entry_id(4)],
        "the tree holds the prefix, the assistant, and the one repair entry"
    );
}

/// The entry appended after a delivery parents on the delivered entry.
///
/// The delivery materializes tree entry 2, so the store's append index must
/// treat it as the new leaf. Otherwise the next entry parents on entry 1 and
/// the durable delivery falls off the served leaf path, and a fork or clone
/// taken from that index cannot carry it.
#[tokio::test]
async fn the_entry_after_a_delivery_parents_on_the_delivered_entry() {
    let (temp, store, id) = staged_delivery_journal("leaf-after-delivery").await;
    let (mut journal, _) = store.open_session(id).await.expect("session reopens");
    journal
        .append(vec![user(3, "after the delivery")])
        .await
        .expect("the append after a delivery lands");
    journal.close().await.expect("session closes");
    let (reopened, _) = store.open_session(id).await.expect("session reopens again");
    let parent = reopened
        .records()
        .iter()
        .find_map(|record| match record {
            Record::User(entry) if entry.id == entry_id(3) => Some(entry.parent),
            _ => None,
        })
        .expect("the later user entry is durable");
    drop(reopened);
    drop(temp);
    assert_eq!(
        parent,
        Some(entry_id(2)),
        "the entry after a delivery parents on the delivered entry"
    );
}

/// A clone taken after a delivery carries the delivered entry and the path
/// above it.
///
/// The delivered entry exists only through its `mail_delivered` line, so a
/// branch projection that indexes ordinary entry records alone stops its
/// parent walk at the delivery and drops the delivered text and every
/// earlier entry from the clone.
#[tokio::test]
async fn a_clone_after_a_delivery_carries_the_delivered_entry() {
    let (temp, store, id) = staged_delivery_journal("clone-after-delivery").await;
    let (mut journal, _) = store.open_session(id).await.expect("session reopens");
    journal
        .append(vec![user(3, "after the delivery")])
        .await
        .expect("the append after a delivery lands");
    let clone = store
        .clone_session(&mut journal)
        .await
        .expect("the clone is published");
    let clone_id = clone.id();
    drop(clone);
    journal.close().await.expect("session closes");
    let (reopened, _) = store
        .open_session(clone_id)
        .await
        .expect("the clone reopens");
    let entries: Vec<(EntryId, Option<EntryId>, Vec<JournalPart>)> = reopened
        .records()
        .iter()
        .filter_map(|record| match record {
            Record::User(Entry {
                id,
                parent,
                kind: EntryKind::User { parts },
                ..
            }) => Some((*id, *parent, parts.clone())),
            _ => None,
        })
        .collect();
    drop(reopened);
    drop(temp);
    let text = |text: &str| vec![JournalPart::Text { text: text.into() }];
    assert_eq!(
        entries,
        vec![
            (entry_id(1), None, text("durable prefix")),
            (entry_id(2), Some(entry_id(1)), text("first")),
            (entry_id(3), Some(entry_id(2)), text("after the delivery")),
        ],
        "the clone keeps the whole path through the delivered entry"
    );
}

/// The listed message count walks the visible path through a delivery.
///
/// The delivered entry exists only through its `mail_delivered` line, so a
/// listing fold that tracks ordinary entry records alone loses the count
/// while the leaf is the delivery and restarts it at the entry after it.
#[tokio::test]
async fn the_listed_count_includes_the_delivered_entry() {
    let (temp, store, id) = staged_delivery_journal("list-after-delivery").await;
    let count = |store: &Store| {
        store
            .list(ListQuery {
                limit: Some(500),
                cursor: None,
                search: None,
            })
            .expect("listing succeeds")
            .items
            .iter()
            .find(|info| info.id == id)
            .expect("the session is listed")
            .message_count
    };
    assert_eq!(count(&store), Some(2), "the leaf is the delivered entry");
    let (mut journal, _) = store.open_session(id).await.expect("session reopens");
    journal
        .append(vec![user(3, "after the delivery")])
        .await
        .expect("the append after a delivery lands");
    journal.close().await.expect("session closes");
    let listed = count(&store);
    drop(temp);
    assert_eq!(listed, Some(3), "the path is 1 -> 2 -> 3");
}
