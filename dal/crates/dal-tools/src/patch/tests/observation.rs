//! Observation cutoffs, parent-cutoff adoption, and recipient-bound handoff
//! at the Hashline evidence owner (T-E05..T-E09).

use std::num::NonZeroU64;
use std::path::Path;
use std::sync::Arc;

use dal_core::{Consumer, GenerationId, ReadView, SessionId, SourceRow};

use super::super::{
    ir::DialectId,
    snapshot::{ReadRef, SnapshotStore},
    write::plan,
};
use super::test_session;
use crate::evidence::{Binding, SnapshotEvidence, capture};

const SOURCE: &[u8] = b"one\ntwo\nthree\nfour\nfive\nsix\n";

fn invocation(number: u64) -> Consumer {
    Consumer::Invocation(NonZeroU64::new(number).expect("nonzero invocation"))
}

fn row(line: u64, text: &str) -> SourceRow {
    SourceRow {
        line,
        text: text.into(),
        complete: true,
    }
}

/// A read of `rows` from `a.txt`, captured for `consumer` in the session's
/// store with those rows displayed.
fn read_view(session: &super::PatchSession, consumer: Consumer, rows: &[SourceRow]) -> ReadView {
    let binding = Binding {
        session: session.session,
        generation: session.generation,
        consumer,
    };
    let shown = rows.iter().map(|row| row.line);
    capture(&session.snapshots, binding, "a.txt", SOURCE, shown).view("a.txt", rows.into(), true)
}

fn slice(view: &ReadView, rows: &[SourceRow]) -> ReadView {
    ReadView {
        rows: rows.into(),
        ..view.clone()
    }
}

fn reference(view: &ReadView) -> ReadRef {
    let provenance = view.provenance.as_ref().expect("captured view");
    ReadRef::parse(&provenance.reference).expect("valid reference")
}

async fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp workspace");
    tokio::fs::write(dir.path().join("a.txt"), SOURCE)
        .await
        .expect("seed");
    dir
}

/// Plans an Enhanced replacement of `line` as `consumer` with a frozen cutoff.
async fn enhanced_plans(
    session: &mut super::PatchSession,
    view: &ReadView,
    consumer: Consumer,
    cutoff: u64,
    line: u64,
) -> bool {
    session.consumer = consumer;
    session.cutoff = Some(cutoff);
    let input = format!("{}\nPUT {line}.={line}:\n+edited\n", view.header);
    plan(session, DialectId::HashlineEnhanced, &input)
        .await
        .is_ok()
}

#[tokio::test]
async fn t_e05_t_e06_check_observation_table() {
    // (reader, delivery seq, patcher, patch cutoff, rows 1-2 eligible):
    // eligible exactly when the patcher is the recipient and delivery <= cutoff.
    let table = [
        (invocation(7), 5, invocation(7), 4, false),
        (invocation(7), 5, invocation(7), 5, true),
        (invocation(7), 5, invocation(7), 6, true),
        (Consumer::Model, 3, Consumer::Model, 2, false),
        (Consumer::Model, 3, Consumer::Model, 9, true),
        (invocation(7), 5, invocation(8), 9, false),
    ];
    for (reader, delivered_at, patcher, cutoff, eligible) in table {
        let dir = workspace().await;
        let mut session = test_session(dir.path(), false);
        let view = read_view(&session, reader, &[row(1, "one"), row(2, "two")]);
        let evidence = SnapshotEvidence::new(Arc::clone(&session.snapshots));
        assert!(evidence.deliver(&view, reader, delivered_at));
        let covers =
            session
                .snapshots
                .covers(session.session, patcher, reference(&view), 1, 2, cutoff);
        assert_eq!(
            covers, eligible,
            "covers {reader:?}@{delivered_at} for {patcher:?}@{cutoff}"
        );
        let planned = enhanced_plans(&mut session, &view, patcher, cutoff, 1).await;
        assert_eq!(
            planned, eligible,
            "plan {reader:?}@{delivered_at} for {patcher:?}@{cutoff}"
        );
    }
}

#[tokio::test]
async fn t_e07_adoption_copies_exactly_the_parent_coverage_at_the_cutoff() {
    let dir = workspace().await;
    let mut session = test_session(dir.path(), false);
    let evidence = SnapshotEvidence::new(Arc::clone(&session.snapshots));
    let window = [
        row(1, "one"),
        row(2, "two"),
        row(3, "three"),
        row(4, "four"),
        row(5, "five"),
    ];
    let whole = read_view(&session, Consumer::Model, &window);
    let early = slice(&whole, &window[..2]);
    let late = slice(&whole, &window[3..]);
    assert!(evidence.deliver(&early, Consumer::Model, 2));
    assert!(evidence.deliver(&late, Consumer::Model, 9));
    let child = invocation(3);
    let adopted = evidence
        .adopt_view(session.session, &early.header, Consumer::Model, 5, child)
        .expect("delivered before the cutoff");
    assert_eq!(adopted.path.as_ref(), "a.txt");
    assert_eq!(adopted.header, early.header);
    assert_eq!(*adopted.rows, [row(1, "one"), row(2, "two")]);
    assert!(adopted.truncated);
    let token = reference(&early);
    assert_eq!(
        session.snapshots.delivered(session.session, child, token),
        [(1, 2)]
    );
    assert!(enhanced_plans(&mut session, &adopted, child, 6, 1).await);
    assert!(!enhanced_plans(&mut session, &adopted, child, 6, 4).await);
    let bare = evidence
        .adopt_view(
            session.session,
            &token.display(),
            Consumer::Model,
            5,
            invocation(4),
        )
        .expect("a bare reference adopts too");
    assert_eq!(bare.rows, adopted.rows);
    let forged = ReadView {
        rows: [row(4, "four"), row(5, "five")].into(),
        ..adopted.clone()
    };
    assert!(!evidence.deliver(&forged, child, 7));
    assert_eq!(
        session.snapshots.delivered(session.session, child, token),
        [(1, 2)]
    );
}

#[tokio::test]
async fn t_e08_late_or_foreign_references_are_denied_without_binding() {
    let dir = workspace().await;
    let session = test_session(dir.path(), false);
    let evidence = SnapshotEvidence::new(Arc::clone(&session.snapshots));
    let view = read_view(&session, Consumer::Model, &[row(1, "one")]);
    assert!(evidence.deliver(&view, Consumer::Model, 9));
    let child = invocation(3);
    let token = reference(&view);
    let other_session = SessionId::new_v7();
    let unknown = ReadRef {
        boot: token.boot,
        seq: token.seq + 1,
    }
    .display();
    let forged_path = view.header.replace("a.txt", "b.txt");
    let attempts: [(SessionId, &str, u64); 4] = [
        (session.session, &view.header, 8),
        (other_session, &view.header, 9),
        (session.session, &unknown, 9),
        (session.session, &forged_path, 9),
    ];
    for (who, reference, cutoff) in attempts {
        assert!(
            evidence
                .adopt_view(who, reference, Consumer::Model, cutoff, child)
                .is_none(),
            "{reference} at {cutoff}"
        );
    }
    assert!(
        session
            .snapshots
            .delivered(session.session, child, token)
            .is_empty()
    );
    assert!(
        session
            .snapshots
            .lookup(
                session.session,
                session.generation,
                child,
                token,
                Path::new("a.txt")
            )
            .is_none()
    );
}

#[tokio::test]
async fn t_e09_only_an_intact_view_rebinds_provenance() {
    let dir = workspace().await;
    let session = test_session(dir.path(), false);
    let evidence = SnapshotEvidence::new(Arc::clone(&session.snapshots));
    let reader = invocation(2);
    let displayed = [row(1, "one"), row(2, "two"), row(3, "three")];
    let intact = slice(&read_view(&session, reader, &displayed), &displayed[..2]);
    let token = reference(&intact);
    let copied = ReadView {
        provenance: None,
        ..intact.clone()
    };
    let stringified = ReadView {
        rows: [row(1, "1\tone"), row(2, "2\ttwo")].into(),
        ..intact.clone()
    };
    let edited = ReadView {
        rows: [row(1, "one"), row(2, "TWO")].into(),
        ..intact.clone()
    };
    let moved = ReadView {
        path: "b.txt".into(),
        ..intact.clone()
    };
    let widened = slice(&intact, &[row(1, "one"), row(4, "four")]);
    let foreign_consumer = ReadView {
        provenance: Some(dal_core::Provenance {
            reference: intact
                .provenance
                .as_ref()
                .expect("provenance")
                .reference
                .clone(),
            consumer: invocation(9),
            session: session.session,
        }),
        ..intact.clone()
    };
    let all_truncated = ReadView {
        rows: [SourceRow {
            line: 1,
            text: "on...".into(),
            complete: false,
        }]
        .into(),
        ..intact.clone()
    };
    let forgeries = [
        copied,
        stringified,
        edited,
        moved,
        widened,
        foreign_consumer,
        all_truncated,
    ];
    for (recipient, forged) in (10..).map(invocation).zip(&forgeries) {
        assert!(
            !evidence.deliver(forged, recipient, 1),
            "forged view for {recipient:?}"
        );
        assert!(
            session
                .snapshots
                .delivered(session.session, recipient, token)
                .is_empty()
        );
    }
    assert!(evidence.deliver(&intact, Consumer::Model, 1));
    assert_eq!(
        session
            .snapshots
            .delivered(session.session, Consumer::Model, token),
        [(1, 2)]
    );
    let partial = ReadView {
        rows: [
            row(1, "one"),
            SourceRow {
                line: 2,
                text: "tw...".into(),
                complete: false,
            },
            row(3, "three"),
        ]
        .into(),
        ..intact.clone()
    };
    assert!(evidence.deliver(&partial, invocation(20), 2));
    assert_eq!(
        session
            .snapshots
            .delivered(session.session, invocation(20), token),
        [(1, 1), (3, 3)]
    );
}

#[tokio::test]
async fn delivered_rows_grant_the_recipient_onward_claims() {
    let dir = workspace().await;
    let session = test_session(dir.path(), false);
    let evidence = SnapshotEvidence::new(Arc::clone(&session.snapshots));
    let displayed = [row(1, "one"), row(2, "two"), row(3, "three")];
    let view = read_view(&session, Consumer::Model, &displayed);
    let token = reference(&view);
    let first = invocation(5);
    let second = invocation(6);
    assert!(evidence.deliver(&view, first, 1));
    let onward = ReadView {
        provenance: Some(dal_core::Provenance {
            reference: token.display(),
            consumer: first,
            session: session.session,
        }),
        ..view.clone()
    };
    assert!(evidence.deliver(&onward, second, 2));
    assert_eq!(
        session.snapshots.delivered(session.session, second, token),
        [(1, 3)]
    );
}

#[test]
fn store_evicts_captures_by_bytes_oldest_first() {
    let store = SnapshotStore::with_budget([1; 16], 100);
    let session = SessionId::new_v7();
    let generation = GenerationId::new(NonZeroU64::MIN);
    let capture = |name: &str| {
        store
            .capture(
                session,
                generation,
                Consumer::Model,
                Path::new(name),
                &[b'x'; 40],
            )
            .expect("capture")
            .0
    };
    let first = capture("a.txt");
    let second = capture("b.txt");
    let _third = capture("c.txt");
    assert!(store.snapshot(first).is_none());
    assert!(store.snapshot(second).is_some());
}
