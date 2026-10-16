#![expect(clippy::expect_used, reason = "integration tests fail loudly")]
//! Store-side contradiction validation for resolved resolution causes.
//!
//! Every case runs through a real [`Store`] in a temporary directory:
//! accepted shapes append durably and survive reopen byte-for-byte, while
//! contradictory shapes are refused before any byte is written.

mod support;

use std::{fs, num::NonZeroU64};

use dal_core::{
    Answer, ClientId, Entry, EntryId, EntryKind, JournalPart, Product, Record, RequestId,
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
