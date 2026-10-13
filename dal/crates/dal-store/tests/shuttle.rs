#![expect(clippy::expect_used, reason = "integration tests fail loudly")]
#![expect(clippy::disallowed_methods, reason = "integration tests fail loudly")]
//! Interleaving proof: concurrent appends stay FIFO, unique, and lossless.

mod support;

use std::{num::NonZeroU64, sync::Arc};

use dal_core::{Entry, EntryId, EntryKind, JournalPart, Product, Record, SessionId, Workspace};
use dal_store::{AppendOutcome, Store, StoreError};
use support::temp_dir::TempDir;

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

#[tokio::test]
async fn shuttle_interleaving_race_proof() {
    let temp = TempDir::new("store-shuttle");
    let workspace =
        Workspace::new(temp.path().join("workspace")).expect("temporary workspace is absolute");
    let store = Store::new(temp.path().join("data"), workspace, Product::Dalgona);
    let held_id = SessionId::new_v7();
    let mut held = store.create_session(held_id);
    held.append(vec![user(1, "held")])
        .await
        .expect("held session appends");

    let gate = Arc::new(tokio::sync::Barrier::new(10));
    let mut workers = Vec::new();
    for worker in 0..8u64 {
        let store = store.clone();
        let gate = gate.clone();
        workers.push(tokio::spawn(async move {
            let id = SessionId::new_v7();
            let mut journal = store.create_session(id);
            gate.wait().await;
            let mut offsets = Vec::new();
            for batch in 0..20u64 {
                let entry = worker * 1000 + batch + 1;
                let outcome = journal
                    .append(vec![user(entry, &format!("w{worker} b{batch}"))])
                    .await
                    .expect("batch appends");
                match outcome {
                    AppendOutcome::Durable(receipt) => offsets.push((receipt.offset, entry)),
                    _ => panic!("every batch is durable"),
                }
            }
            journal.close().await.expect("worker closes");
            (id, offsets)
        }));
    }
    let racer_store = store.clone();
    let racer_gate = gate.clone();
    let racer = tokio::spawn(async move {
        racer_gate.wait().await;
        let mut refused = 0u32;
        for _ in 0..20 {
            match racer_store.open_session(held_id).await {
                Err(StoreError::Locked { .. }) => refused += 1,
                Err(other) => panic!("expected Locked while held, got {other:?}"),
                Ok(_) => panic!("no racer wins while the session is held"),
            }
        }
        refused
    });
    gate.wait().await;

    let refused = racer.await.expect("racer joins without deadlock");
    assert_eq!(refused, 20, "every lock race loses while held");
    for worker in workers {
        let (id, offsets) = worker.await.expect("worker joins without deadlock");
        assert_eq!(offsets.len(), 20, "no data is lost");
        let mut seen: Vec<u64> = offsets.iter().map(|(offset, _)| *offset).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), 20, "per-journal receipts are unique");
        let acked: Vec<u64> = offsets.iter().map(|(offset, _)| *offset).collect();
        let mut ordered = acked.clone();
        ordered.sort_unstable();
        assert_eq!(acked, ordered, "per-journal receipts are FIFO");
        let (reopened, _) = store.open_session(id).await.expect("worker reopens");
        for (_, entry) in &offsets {
            assert!(
                reopened.records().iter().any(|record| match record {
                    Record::User(row) => row.id.get() == *entry,
                    _ => false,
                }),
                "every receipted entry is present"
            );
        }
    }
    held.close().await.expect("held session closes");
    let (reopened, _) = store
        .open_session(held_id)
        .await
        .expect("holder wins after close");
    assert_eq!(
        reopened.records().len(),
        4,
        "exactly one lock winner exists"
    );
}
