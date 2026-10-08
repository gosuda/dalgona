#![expect(clippy::expect_used, reason = "integration tests fail loudly")]
#![expect(clippy::panic, reason = "integration tests fail loudly")]
#![expect(clippy::disallowed_methods, reason = "integration tests fail loudly")]
//! Journal batches: torn-write rollback, damage stability, limits, and receipts.

mod support;

use std::{
    fs,
    num::NonZeroU64,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use dal_core::{
    Entry, EntryId, EntryKind, JournalPart, Product, Record, SessionId, Workspace, encode,
};
use dal_store::{AppendOutcome, JournalError, Store};
use support::{fault_sink::FaultSink, temp_dir::TempDir};

fn entry_id(value: u64) -> EntryId {
    EntryId::new(NonZeroU64::new(value).expect("nonzero test entry id"))
}

fn timestamp() -> jiff::Timestamp {
    jiff::Timestamp::now()
}

fn user(id: u64, text: &str) -> Record {
    Record::User(Entry {
        id: entry_id(id),
        parent: None,
        at: timestamp(),
        kind: EntryKind::User {
            parts: vec![JournalPart::Text { text: text.into() }],
        },
    })
}

fn setup(tag: &str) -> (TempDir, Store) {
    let temp = TempDir::new(tag);
    let workspace =
        Workspace::new(temp.path().join("workspace")).expect("temporary workspace is absolute");
    let store = Store::new(temp.path().join("data"), workspace, Product::Dalgona);
    (temp, store)
}

fn journal_path(data_root: &std::path::Path, id: SessionId) -> PathBuf {
    let sessions = data_root.join("sessions");
    let name = id.to_string();
    let mut stack = vec![sessions];
    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir).expect("list data-root subtree");
        for entry in entries {
            let path = entry.expect("read data-root entry").path();
            if path.is_dir() {
                let journal = path.join("journal.jsonl");
                if path.file_name().is_some_and(|base| base == name.as_str()) {
                    assert!(journal.is_file(), "journal file exists");
                    return journal;
                }
                stack.push(path);
            }
        }
    }
    panic!("journal file exists");
}

fn batch_bytes(records: &[Record]) -> Vec<u8> {
    let mut out = Vec::new();
    for record in records {
        out.extend_from_slice(&encode(record).expect("record encodes"));
    }
    out
}

#[tokio::test]
async fn failed_write_rolls_back_cleanly() {
    let (temp, store) = setup("journal-rollback");
    let data_root = temp.path().join("data");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "durable prefix")])
        .await
        .expect("prefix is durable");
    journal.close().await.expect("session closes");
    let path = journal_path(&data_root, id);
    let size_before = fs::metadata(&path).expect("journal stat").len();
    let prefix = fs::read(&path).expect("read durable prefix");
    let batch = batch_bytes(&[
        user(2, "second"),
        user(3, "third"),
        Record::Name {
            at: timestamp(),
            name: Some("fourth".into()),
        },
    ]);
    let sink = FaultSink {
        write_after_bytes: Some(10),
        ..Default::default()
    };
    let failure = sink
        .append_partial(&path, &batch)
        .expect_err("partial write fails");
    assert_eq!(
        failure.to_string(),
        "injected fault: write",
        "the write failpoint fires"
    );
    assert_eq!(
        fs::metadata(&path).expect("journal stat").len(),
        size_before + 10,
        "only the partial bytes follow the prior file"
    );
    let (mut reopened, report) = store.open_session(id).await.expect("open repairs");
    assert_eq!(
        report.torn.as_ref().expect("torn tail reported").offset,
        size_before,
        "the tail starts where the prefix ended"
    );
    let repaired = fs::read(&path).expect("read repaired journal");
    let prefix_len = usize::try_from(size_before).expect("journal size fits memory");
    assert_eq!(
        repaired[..prefix_len],
        prefix[..],
        "repair removes only the partial bytes"
    );
    reopened
        .append(vec![user(2, "second after repair")])
        .await
        .expect("the next append is allowed");
    reopened.close().await.expect("session closes");
}

#[tokio::test]
async fn failed_rollback_marks_damaged() {
    let (temp, store) = setup("journal-damaged");
    let data_root = temp.path().join("data");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "durable prefix")])
        .await
        .expect("prefix is durable");
    journal.close().await.expect("session closes");
    let path = journal_path(&data_root, id);
    let offset = fs::metadata(&path).expect("journal stat").len();
    let mut staged = fs::read(&path).expect("read staged journal");
    staged.extend_from_slice(b"{\"v\":1,\"type\":\"nope\"}\n");
    fs::write(&path, &staged).expect("stage a complete bad line");
    let bytes_before = fs::read(&path).expect("read staged journal");
    let first = store.open_session(id).await.expect_err("open refuses");
    let text = first.to_string();
    assert!(
        text.contains(&format!("is damaged at byte {offset}")),
        "exact Damaged offset, got {text}"
    );
    assert!(
        text.contains("dalgon did not change it"),
        "exact Damaged stability note, got {text}"
    );
    assert_eq!(
        fs::read(&path).expect("reread journal"),
        bytes_before,
        "a damaged prefix is never mutated"
    );
    let second = store
        .open_session(id)
        .await
        .expect_err("open still refuses");
    assert_eq!(
        second.to_string(),
        text,
        "damage is stable and never falsely healthy"
    );
    let mut staged = fs::read(&path).expect("read damaged journal");
    staged.truncate(usize::try_from(offset).expect("journal fits memory"));
    fs::write(&path, &staged).expect("truncate the bad line");
    let (mut reopened, _) = store.open_session(id).await.expect("reopen repairs");
    reopened
        .append(vec![user(2, "after repair")])
        .await
        .expect("append succeeds after repair");
    reopened.close().await.expect("session closes");
}

#[tokio::test]
async fn oversize_line_stops_fold() {
    let (temp, store) = setup("journal-toolong");
    let data_root = temp.path().join("data");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "prefix")])
        .await
        .expect("prefix is durable");
    journal.close().await.expect("session closes");
    let path = journal_path(&data_root, id);
    let offset = fs::metadata(&path).expect("journal stat").len();
    let mut staged = fs::read(&path).expect("read staged journal");
    staged.extend(std::iter::repeat_n(b'x', 67_108_865));
    staged.push(b'\n');
    fs::write(&path, &staged).expect("stage an overlong line");
    let error = store.open_session(id).await.expect_err("fold stops");
    match &error {
        dal_store::StoreError::Journal(JournalError::TooLong { offset: at, .. }) => {
            assert_eq!(*at, offset, "stop is at the line start");
        }
        _ => panic!("expected TooLong, got {error:?}"),
    }
    assert_eq!(
        error.to_string(),
        format!(
            "{}: the record at byte {offset} is longer than 67108864 bytes",
            path.display()
        ),
        "exact TooLong message"
    );
}

#[tokio::test]
async fn shard_receipts_strictly_ordered() {
    static NEXT_ENTRY: AtomicU64 = AtomicU64::new(1);
    let (temp, store) = setup("journal-shards");
    let _ = &temp;
    let mut handles = Vec::new();
    for _ in 0..8 {
        let id = SessionId::new_v7();
        let mut journal = store.create_session(id);
        handles.push(tokio::spawn(async move {
            let mut offsets = Vec::new();
            for _ in 0..25 {
                let entry = NEXT_ENTRY.fetch_add(1, Ordering::Relaxed);
                let outcome = journal
                    .append(vec![user(entry, &format!("batch {entry}"))])
                    .await
                    .expect("batch appends");
                match outcome {
                    AppendOutcome::Durable(receipt) => offsets.push((receipt.offset, entry)),
                    _ => panic!("every batch is durable"),
                }
            }
            journal.close().await.expect("session closes");
            (id, offsets)
        }));
    }
    let mut total = 0;
    for handle in handles {
        let (id, offsets) = handle.await.expect("worker joins");
        assert_eq!(offsets.len(), 25, "every batch is acknowledged");
        let mut seen: Vec<u64> = offsets.iter().map(|(offset, _)| *offset).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), 25, "per-journal offsets are unique");
        let acked: Vec<u64> = offsets.iter().map(|(offset, _)| *offset).collect();
        let mut ordered = acked.clone();
        ordered.sort_unstable();
        assert_eq!(
            acked, ordered,
            "per-journal receipts are strictly increasing"
        );
        let (reopened, _) = store.open_session(id).await.expect("session reopens");
        for (_, entry) in &offsets {
            assert!(
                reopened.records().iter().any(|record| match record {
                    Record::User(row) => row.id.get() == *entry,
                    _ => false,
                }),
                "every acknowledged record is present after reopen"
            );
        }
        total += offsets.len();
    }
    assert_eq!(total, 200, "all 200 batches land");
}

const RECORD_LIMIT: usize = 67_108_864;

/// An assistant record whose encoded journal line, newline included, is exactly `line_len` bytes
/// once appended after user entry 1.
fn assistant_line_of(line_len: usize) -> Record {
    let build = |text: String| {
        Record::Assistant(Entry {
            id: entry_id(2),
            parent: Some(entry_id(1)),
            at: "2026-09-28T10:15:30.123Z"
                .parse()
                .expect("fixed test timestamp parses"),
            kind: EntryKind::Assistant {
                api: dal_core::Family::Chat,
                model: "test-model".into(),
                content: vec![dal_core::Block::Text { text: text.into() }],
                usage: dal_core::Usage {
                    input_tokens: 0,
                    cached_input_tokens: 0,
                    output_tokens: 0,
                    reasoning_tokens: None,
                    cache_write_tokens: 0,
                    cost_usd: None,
                },
                stop: dal_core::AssistantStop::Done,
            },
        })
    };
    let base = encode(&build(String::new()))
        .expect("empty record encodes")
        .len();
    let record = build("a".repeat(line_len - base));
    assert_eq!(
        encode(&record).expect("sized record encodes").len(),
        line_len,
        "test record hits the requested line length"
    );
    record
}

fn with_id(record: Record, id: u64, parent: u64) -> Record {
    match record {
        Record::Assistant(mut entry) => {
            entry.id = entry_id(id);
            entry.parent = Some(entry_id(parent));
            Record::Assistant(entry)
        }
        other => other,
    }
}

#[tokio::test]
async fn record_limit_holds_from_both_sides_across_append_and_replay() {
    let (temp, store) = setup("journal-limit");
    let data_root = temp.path().join("data");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "prefix")])
        .await
        .expect("prefix is durable");

    journal
        .append(vec![assistant_line_of(RECORD_LIMIT - 1)])
        .await
        .expect("a line of limit-1 bytes is accepted");
    journal
        .append(vec![with_id(assistant_line_of(RECORD_LIMIT), 3, 2)])
        .await
        .expect("a line of exactly the limit is accepted");
    let before = journal.records().len();
    let error = journal
        .append(vec![with_id(assistant_line_of(RECORD_LIMIT + 1), 4, 3)])
        .await
        .expect_err("a line of limit+1 bytes is refused");
    assert!(
        matches!(&error, dal_store::StoreError::Invalid { reason }
            if reason.contains("exceeds the 67108864-byte limit")),
        "descriptive Invalid, got {error:?}"
    );
    assert_eq!(journal.records().len(), before, "refusal appends nothing");
    journal.close().await.expect("session closes");

    let (mut reopened, _) = store
        .open_session(id)
        .await
        .expect("every accepted line replays");
    let replayed = reopened
        .records()
        .iter()
        .filter(|record| matches!(record, Record::Assistant(_)))
        .count();
    assert_eq!(replayed, 2, "both boundary lines survive replay");
    reopened.close().await.expect("session closes");

    let path = journal_path(&data_root, id);
    let offset = fs::metadata(&path).expect("journal stat").len();
    let over = with_id(assistant_line_of(RECORD_LIMIT + 1), 4, 3);
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("open journal for staging");
    std::io::Write::write_all(&mut file, &encode(&over).expect("over-limit line encodes"))
        .expect("stage a limit+1 line");
    drop(file);
    let error = store
        .open_session(id)
        .await
        .expect_err("replay refuses a limit+1 line");
    match &error {
        dal_store::StoreError::Journal(JournalError::TooLong { offset: at, .. }) => {
            assert_eq!(*at, offset, "refusal names the start of the long line");
        }
        _ => panic!("expected TooLong, got {error:?}"),
    }
}

#[tokio::test]
async fn torn_tail_limit_holds_from_both_sides() {
    let (temp, store) = setup("journal-tail-limit");
    let data_root = temp.path().join("data");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "prefix")])
        .await
        .expect("prefix is durable");
    journal.close().await.expect("session closes");
    let path = journal_path(&data_root, id);
    let prefix = fs::read(&path).expect("read prefix");

    let mut staged = prefix.clone();
    staged.extend(std::iter::repeat_n(b'x', RECORD_LIMIT - 1));
    fs::write(&path, &staged).expect("stage a tail of limit-1 bytes");
    let (mut repaired, report) = store
        .open_session(id)
        .await
        .expect("a tail below the limit is quarantined");
    let torn = report.torn.expect("torn tail reported");
    assert_eq!(torn.bytes, u64::try_from(RECORD_LIMIT - 1).expect("fits"));
    repaired.close().await.expect("session closes");

    let mut staged = fs::read(&path).expect("read repaired journal");
    staged.extend(std::iter::repeat_n(b'x', RECORD_LIMIT));
    let tail_start = u64::try_from(staged.len() - RECORD_LIMIT).expect("fits");
    fs::write(&path, &staged).expect("stage a tail of exactly the limit");
    let error = store
        .open_session(id)
        .await
        .expect_err("a tail at the limit is refused");
    match &error {
        dal_store::StoreError::Journal(JournalError::TooLong { offset, .. }) => {
            assert_eq!(*offset, tail_start, "refusal names the tail start");
        }
        _ => panic!("expected TooLong, got {error:?}"),
    }
    assert_eq!(
        fs::read(&path).expect("reread journal"),
        staged,
        "a refused open leaves the journal untouched"
    );
}

#[tokio::test]
async fn oversized_record_before_the_first_user_entry_is_refused_at_append() {
    let (_temp, store) = setup("journal-lazy-limit");
    let mut journal = store.create_session(SessionId::new_v7());
    let huge = Record::Reminder(Entry {
        id: entry_id(1),
        parent: None,
        at: timestamp(),
        kind: EntryKind::Reminder {
            text: "r".repeat(RECORD_LIMIT).into(),
            source: "test".into(),
        },
    });
    let refused = journal.append(vec![huge]).await;
    assert!(
        refused.is_err(),
        "an over-limit record is refused when appended, not buffered: {refused:?}"
    );
    journal
        .append(vec![user(2, "still usable")])
        .await
        .expect("the session is not poisoned by the refused record");
    journal.close().await.expect("session closes");
}
