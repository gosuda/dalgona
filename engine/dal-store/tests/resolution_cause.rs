#![expect(clippy::expect_used, reason = "integration tests fail loudly")]
//! Store-side contradiction validation for resolved resolution causes.
//!
//! Every case runs through a real [`Store`] in a temporary directory:
//! accepted shapes append durably and survive reopen byte-for-byte, while
//! contradictory shapes are refused before any byte is written.

mod support;

use std::{fs, num::NonZeroU64};

use dal_core::{
    Answer, ClientId, Entry, EntryId, EntryKind, JournalPart, Product, RawJson, Record, RequestId,
    ResolutionCause, SessionId, Workspace, encode,
};
use dal_store::{AppendOutcome, Store, StoreError};
use support::temp_dir::TempDir;

fn timestamp() -> jiff::Timestamp {
    "2026-09-28T10:15:30.123Z"
        .parse()
        .expect("fixed test timestamp parses")
}

fn entry_id(value: u64) -> EntryId {
    EntryId::new(NonZeroU64::new(value).expect("nonzero test entry id"))
}

fn request_id(ordinal: u64) -> RequestId {
    RequestId::parse(&format!("0192aa00-0000-7000-8000-{ordinal:012}"))
        .expect("fixed request id parses")
}

fn user_entry(id: u64) -> Record {
    Record::User(Entry {
        id: entry_id(id),
        parent: NonZeroU64::new(id.saturating_sub(1)).map(EntryId::new),
        at: timestamp(),
        kind: EntryKind::User {
            parts: vec![JournalPart::Text {
                text: "start".into(),
            }],
        },
    })
}

fn resolved(
    ordinal: u64,
    by: &str,
    cause: Option<ResolutionCause>,
    answer: Answer,
    was_default: bool,
) -> Record {
    Record::Resolved {
        at: timestamp(),
        request: request_id(ordinal),
        answer,
        by: ClientId::new(by),
        was_default,
        cause,
    }
}

fn store_in(temp: &TempDir) -> (Store, Workspace) {
    let workspace =
        Workspace::new(temp.path().join("workspace")).expect("temporary workspace is absolute");
    let store = Store::new(
        temp.path().join("data"),
        workspace.clone(),
        Product::Dalgona,
    );
    (store, workspace)
}

fn encoded_lines(records: &[Record]) -> Vec<Vec<u8>> {
    records
        .iter()
        .map(|record| encode(record).expect("record encodes"))
        .collect()
}

fn resolved_records(records: &[Record]) -> Vec<Record> {
    records
        .iter()
        .filter(|record| matches!(record, Record::Resolved { .. }))
        .cloned()
        .collect()
}

/// A coherent cause pairing a real writer could have produced.
struct Coherent {
    by: &'static str,
    cause: ResolutionCause,
    answer: Answer,
    was_default: bool,
}

fn coherent_causes() -> Vec<Coherent> {
    vec![
        Coherent {
            by: "tui",
            cause: ResolutionCause::Answered,
            answer: Answer::Approve,
            was_default: false,
        },
        Coherent {
            by: "tui",
            cause: ResolutionCause::Answered,
            answer: Answer::ApproveForSession,
            was_default: false,
        },
        Coherent {
            by: "core",
            cause: ResolutionCause::Answered,
            answer: Answer::Approve,
            was_default: false,
        },
        Coherent {
            by: "core",
            cause: ResolutionCause::TimedOut,
            answer: Answer::Decline,
            was_default: true,
        },
        Coherent {
            by: "core",
            cause: ResolutionCause::TimedOut,
            answer: Answer::Cancel,
            was_default: true,
        },
        Coherent {
            by: "core",
            cause: ResolutionCause::NoController,
            answer: Answer::Decline,
            was_default: true,
        },
        Coherent {
            by: "core",
            cause: ResolutionCause::Cancelled,
            answer: Answer::Cancel,
            was_default: false,
        },
        Coherent {
            by: "core",
            cause: ResolutionCause::Withdrawn,
            answer: Answer::Cancel,
            was_default: false,
        },
    ]
}

/// A record set the contract refuses, with the cause name expected in the
/// refusal reason.
struct Contradiction {
    by: &'static str,
    cause: ResolutionCause,
    answer: Answer,
    was_default: bool,
    cause_name: &'static str,
}

fn contradictions() -> Vec<Contradiction> {
    vec![
        Contradiction {
            by: "tui",
            cause: ResolutionCause::Answered,
            answer: Answer::Approve,
            was_default: true,
            cause_name: "Answered",
        },
        Contradiction {
            by: "tui",
            cause: ResolutionCause::Answered,
            answer: Answer::ApproveForSession,
            was_default: true,
            cause_name: "Answered",
        },
        Contradiction {
            by: "core",
            cause: ResolutionCause::Cancelled,
            answer: Answer::Cancel,
            was_default: true,
            cause_name: "Cancelled",
        },
        Contradiction {
            by: "core",
            cause: ResolutionCause::Withdrawn,
            answer: Answer::Cancel,
            was_default: true,
            cause_name: "Withdrawn",
        },
        Contradiction {
            by: "core",
            cause: ResolutionCause::TimedOut,
            answer: Answer::Decline,
            was_default: false,
            cause_name: "TimedOut",
        },
        Contradiction {
            by: "core",
            cause: ResolutionCause::NoController,
            answer: Answer::Decline,
            was_default: false,
            cause_name: "NoController",
        },
        Contradiction {
            by: "core",
            cause: ResolutionCause::Cancelled,
            answer: Answer::Approve,
            was_default: false,
            cause_name: "Cancelled",
        },
        Contradiction {
            by: "core",
            cause: ResolutionCause::Cancelled,
            answer: Answer::Decline,
            was_default: false,
            cause_name: "Cancelled",
        },
        Contradiction {
            by: "core",
            cause: ResolutionCause::Withdrawn,
            answer: Answer::Approve,
            was_default: false,
            cause_name: "Withdrawn",
        },
        Contradiction {
            by: "core",
            cause: ResolutionCause::Withdrawn,
            answer: Answer::ApproveForSession,
            was_default: false,
            cause_name: "Withdrawn",
        },
    ]
}

#[tokio::test]
async fn legacy_cause_none_appends_and_reopens() {
    let temp = TempDir::new("resolution-cause-legacy");
    let (store, _) = store_in(&temp);
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    // Legacy shapes stay unconstrained: a default answer and a session
    // answer both append without any cause.
    let legacy = vec![
        resolved(1, "tui", None, Answer::Approve, true),
        resolved(2, "tui", None, Answer::ApproveForSession, false),
        resolved(3, "core", None, Answer::Cancel, false),
    ];
    let outcome = journal
        .append(vec![user_entry(1)])
        .await
        .expect("user entry appends");
    assert!(
        matches!(outcome, AppendOutcome::Durable(_)),
        "the seed entry reaches disk"
    );
    for record in legacy {
        let outcome = journal
            .append(vec![record])
            .await
            .expect("legacy resolution appends");
        assert!(
            matches!(outcome, AppendOutcome::Durable(_)),
            "legacy resolution reaches disk"
        );
    }
    let before = encoded_lines(journal.records());
    journal.close().await.expect("session closes");
    let (reopened, _) = store.open_session(id).await.expect("session reopens");
    let after = encoded_lines(reopened.records());
    assert_eq!(
        after.len(),
        before.len() + 1,
        "reopen appends exactly one recovery boot"
    );
    assert_eq!(
        &after[..before.len()],
        &before[..],
        "durable prefix survives reopen byte-for-byte"
    );
    let kept = resolved_records(reopened.records());
    assert_eq!(kept.len(), 3, "every legacy resolution reopens");
    for record in &kept {
        assert!(
            matches!(record, Record::Resolved { cause: None, .. }),
            "reopen never infers a cause: {record:?}"
        );
    }
}

#[tokio::test]
async fn coherent_causes_append_and_reopen() {
    let temp = TempDir::new("resolution-cause-coherent");
    let (store, _) = store_in(&temp);
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    let outcome = journal
        .append(vec![user_entry(1)])
        .await
        .expect("user entry appends");
    assert!(
        matches!(outcome, AppendOutcome::Durable(_)),
        "the seed entry reaches disk"
    );
    let mut expected = Vec::new();
    for (ordinal, shape) in coherent_causes().iter().enumerate() {
        let record = resolved(
            u64::try_from(ordinal).expect("small case index") + 1,
            shape.by,
            Some(shape.cause),
            shape.answer.clone(),
            shape.was_default,
        );
        let outcome = journal
            .append(vec![record.clone()])
            .await
            .expect("coherent resolution appends");
        assert!(
            matches!(outcome, AppendOutcome::Durable(_)),
            "coherent resolution reaches disk"
        );
        expected.push(record);
    }
    let before = encoded_lines(journal.records());
    journal.close().await.expect("session closes");
    let (reopened, _) = store.open_session(id).await.expect("session reopens");
    let after = encoded_lines(reopened.records());
    assert_eq!(
        after.len(),
        before.len() + 1,
        "reopen appends exactly one recovery boot"
    );
    assert_eq!(
        &after[..before.len()],
        &before[..],
        "durable prefix survives reopen byte-for-byte"
    );
    assert_eq!(
        resolved_records(reopened.records()),
        expected,
        "every coherent resolution reopens with its cause"
    );
}

#[tokio::test]
async fn contradictory_causes_are_refused() {
    let temp = TempDir::new("resolution-cause-refused");
    let (store, _) = store_in(&temp);
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    let outcome = journal
        .append(vec![user_entry(1)])
        .await
        .expect("user entry appends");
    assert!(
        matches!(outcome, AppendOutcome::Durable(_)),
        "the seed entry reaches disk"
    );
    for (ordinal, contradiction) in contradictions().into_iter().enumerate() {
        let record = resolved(
            u64::try_from(ordinal).expect("small case index") + 1,
            contradiction.by,
            Some(contradiction.cause),
            contradiction.answer,
            contradiction.was_default,
        );
        let error = journal
            .append(vec![record])
            .await
            .expect_err("contradiction refuses");
        assert!(
            matches!(&error, StoreError::Invalid { reason } if reason.contains("resolution cause") && reason.contains(contradiction.cause_name)),
            "refusal names the resolution cause: {error}"
        );
    }
    journal.close().await.expect("session closes");
}

/// What the store contract says about one cause, answer, and default triple.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Verdict {
    Accept,
    Refuse,
    /// The store leaves this triple to another layer, so nothing is pinned.
    /// Only `approve_for_session` on a deadline cause is left: the store
    /// admits it and core fold preflight refuses it unless the cause is
    /// `answered`, so session-grant Answered-only-ness is not the store's rule.
    Unpinned,
}

use Verdict::{Accept, Refuse, Unpinned};

/// Verdicts for the answers `[approve, approve_for_session, decline, cancel,
/// value]`, written out per cause and `was_default` so every one of the 50
/// combinations is either pinned here or explicitly left unpinned.
fn verdict_table() -> Vec<(ResolutionCause, bool, [Verdict; 5])> {
    vec![
        (
            ResolutionCause::Answered,
            false,
            [Accept, Accept, Accept, Accept, Accept],
        ),
        (
            ResolutionCause::Answered,
            true,
            [Refuse, Refuse, Refuse, Refuse, Refuse],
        ),
        (
            ResolutionCause::TimedOut,
            true,
            [Accept, Unpinned, Accept, Accept, Accept],
        ),
        (
            ResolutionCause::TimedOut,
            false,
            [Refuse, Refuse, Refuse, Refuse, Refuse],
        ),
        (
            ResolutionCause::NoController,
            true,
            [Accept, Unpinned, Accept, Accept, Accept],
        ),
        (
            ResolutionCause::NoController,
            false,
            [Refuse, Refuse, Refuse, Refuse, Refuse],
        ),
        (
            ResolutionCause::Cancelled,
            false,
            [Refuse, Refuse, Refuse, Accept, Refuse],
        ),
        (
            ResolutionCause::Cancelled,
            true,
            [Refuse, Refuse, Refuse, Refuse, Refuse],
        ),
        (
            ResolutionCause::Withdrawn,
            false,
            [Refuse, Refuse, Refuse, Accept, Refuse],
        ),
        (
            ResolutionCause::Withdrawn,
            true,
            [Refuse, Refuse, Refuse, Refuse, Refuse],
        ),
    ]
}

#[tokio::test]
async fn every_cause_answer_and_default_combination_is_pinned_or_explicitly_unpinned() {
    let answers = [
        Answer::Approve,
        Answer::ApproveForSession,
        Answer::Decline,
        Answer::Cancel,
        Answer::Value(RawJson::parse(r#"{"choice":"b"}"#).expect("value answer parses")),
    ];
    let temp = TempDir::new("resolution-cause-matrix");
    let (store, _) = store_in(&temp);
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user_entry(1)])
        .await
        .expect("user entry appends");
    let table = verdict_table();
    assert_eq!(
        table.len() * answers.len(),
        50,
        "5 causes x 2 defaults x 5 answers"
    );
    let mut ordinal = 0_u64;
    let mut accepted = Vec::new();
    let mut checked = 0_usize;
    for (cause, was_default, verdicts) in table {
        for (answer, verdict) in answers.iter().zip(verdicts) {
            if verdict == Unpinned {
                continue;
            }
            ordinal += 1;
            let record = resolved(ordinal, "core", Some(cause), answer.clone(), was_default);
            let before = journal.records().len();
            let outcome = journal.append(vec![record.clone()]).await;
            let label = format!("{cause:?} / {answer:?} / was_default {was_default}");
            match (verdict, outcome) {
                (Accept, Ok(AppendOutcome::Durable(_))) => {
                    accepted.push(record);
                    assert_eq!(journal.records().len(), before + 1, "{label}: one record");
                }
                (Refuse, Err(StoreError::Invalid { reason })) => {
                    assert!(
                        reason.contains("resolution cause")
                            && reason.contains(&format!("{cause:?}")),
                        "{label}: refusal names the cause, got {reason}"
                    );
                    assert_eq!(journal.records().len(), before, "{label}: nothing staged");
                }
                (expected, other) => {
                    panic!("{label}: expected {expected:?}, store answered {other:?}")
                }
            }
            checked += 1;
        }
    }
    assert_eq!(checked, 48, "50 combinations minus the 2 unpinned ones");
    assert_eq!(accepted.len(), 15, "the contract admits exactly 15 triples");
    journal.close().await.expect("session closes");
    let (reopened, _) = store.open_session(id).await.expect("session reopens");
    assert_eq!(
        resolved_records(reopened.records()),
        accepted,
        "exactly the admitted resolutions are durable, in order"
    );
}

/// Approval attribution (`by`) must be nonempty and at most 128 bytes, for
/// every record that carries it, and the bound counts bytes, not characters.
#[tokio::test]
async fn approval_attribution_holds_its_byte_limit_from_both_sides() {
    let temp = TempDir::new("resolution-cause-attribution");
    let (store, _) = store_in(&temp);
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user_entry(1)])
        .await
        .expect("user entry appends");
    let cases = [
        (String::new(), false, "empty"),
        ("a".to_string(), true, "1 byte"),
        ("a".repeat(128), true, "128 bytes"),
        ("a".repeat(129), false, "129 bytes"),
        (
            "\u{e9}".repeat(64),
            true,
            "64 two-byte characters, 128 bytes",
        ),
        (
            "\u{e9}".repeat(65),
            false,
            "65 two-byte characters, 130 bytes",
        ),
    ];
    let mut admitted = 0_usize;
    let mut tag = 0_u64;
    for (by, expected, label) in cases {
        tag += 1;
        let records = [
            resolved(tag, &by, None, Answer::Approve, false),
            Record::AllowAlways {
                at: timestamp(),
                tool: format!("tool-{tag}").into(),
                by: ClientId::new(by.as_str()),
            },
            Record::GrantGiven {
                at: timestamp(),
                ext: format!("ext-{tag}").into(),
                set: vec!["read".into()],
                scope: "session".into(),
                by: ClientId::new(by.as_str()),
            },
        ];
        for record in records {
            let kind = record.tag();
            let outcome = journal.append(vec![record]).await;
            if expected {
                assert!(
                    matches!(outcome, Ok(AppendOutcome::Durable(_))),
                    "{kind} with {label} is admitted, got {outcome:?}"
                );
                admitted += 1;
            } else {
                assert!(
                    matches!(&outcome, Err(StoreError::Invalid { reason }) if reason.contains("attribution")),
                    "{kind} with {label} is refused as attribution, got {outcome:?}"
                );
            }
        }
    }
    assert_eq!(
        admitted,
        3 * 3,
        "three admitted lengths across three records"
    );
    journal.close().await.expect("session closes");
    let (reopened, _) = store.open_session(id).await.expect("session reopens");
    let durable = reopened
        .records()
        .iter()
        .filter(|record| {
            matches!(
                record,
                Record::Resolved { .. } | Record::AllowAlways { .. } | Record::GrantGiven { .. }
            )
        })
        .count();
    assert_eq!(
        durable, admitted,
        "only the admitted attributions are durable"
    );
}

#[tokio::test]
async fn a_contradiction_anywhere_in_a_batch_refuses_the_whole_batch() {
    let temp = TempDir::new("resolution-cause-batch");
    let (store, _) = store_in(&temp);
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user_entry(1)])
        .await
        .expect("user entry appends");
    let journal_path = store.session_file(id);
    let before = fs::read(&journal_path).expect("journal file reads");
    let stored = journal.records().len();
    let good = |ordinal| {
        resolved(
            ordinal,
            "tui",
            Some(ResolutionCause::Answered),
            Answer::Approve,
            false,
        )
    };
    let bad = |ordinal| {
        resolved(
            ordinal,
            "core",
            Some(ResolutionCause::Cancelled),
            Answer::Approve,
            false,
        )
    };
    let batches = [
        vec![good(1), bad(2)],
        vec![bad(3), good(4)],
        vec![good(5), good(6), bad(7)],
        vec![good(8), bad(9), good(10)],
    ];
    for batch in batches {
        let error = journal
            .append(batch.clone())
            .await
            .expect_err("a batch holding a contradiction is refused");
        assert!(
            matches!(&error, StoreError::Invalid { reason } if reason.contains("Cancelled")),
            "refusal names the contradicting cause: {error}"
        );
        assert_eq!(
            fs::read(&journal_path).expect("journal file rereads"),
            before,
            "no record of a refused batch reaches disk"
        );
        assert_eq!(
            journal.records().len(),
            stored,
            "no record of a refused batch is staged"
        );
    }
    let outcome = journal
        .append(vec![good(1)])
        .await
        .expect("a coherent record from a refused batch appends on its own");
    assert!(
        matches!(outcome, AppendOutcome::Durable(_)),
        "the retry reaches disk"
    );
    assert_eq!(
        resolved_records(journal.records()).len(),
        1,
        "only the retried record exists"
    );
    journal.close().await.expect("session closes");
}

#[tokio::test]
async fn refused_appends_leave_durable_bytes_unchanged() {
    let temp = TempDir::new("resolution-cause-bytes");
    let (store, _) = store_in(&temp);
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![
            user_entry(1),
            resolved(1, "tui", None, Answer::Approve, false),
        ])
        .await
        .expect("seed batch appends");
    let journal_path = store.session_file(id);
    let before = fs::read(&journal_path).expect("journal file reads");
    let stored = journal.records().len();
    for (ordinal, contradiction) in contradictions().into_iter().enumerate() {
        let record = resolved(
            u64::try_from(ordinal).expect("small case index") + 2,
            contradiction.by,
            Some(contradiction.cause),
            contradiction.answer,
            contradiction.was_default,
        );
        let error = journal
            .append(vec![record])
            .await
            .expect_err("contradiction refuses");
        assert!(
            matches!(error, StoreError::Invalid { .. }),
            "contradiction fails as invalid"
        );
        assert_eq!(
            fs::read(&journal_path).expect("journal file rereads"),
            before,
            "a refused append leaves durable bytes unchanged"
        );
        assert_eq!(
            journal.records().len(),
            stored,
            "a refused append stages nothing"
        );
    }
    journal
        .append(vec![resolved(
            20,
            "tui",
            Some(ResolutionCause::Answered),
            Answer::Decline,
            false,
        )])
        .await
        .expect("a later coherent append still succeeds");
    journal.close().await.expect("session closes");
}
