//! Agent-side recovery proof over the Store-owned job ledger.
//!
//! The ledger's own replay, repair, and refusal behavior is proved where it
//! lives, in `dal-store`. This module proves only what the Agent adds on top
//! of it: a [`JobTable`] over a real [`Store`](dal_store::Store) reopens every
//! acknowledged report once, and keeps settling, after an interrupted append
//! left a torn tail in the session's ledger file.

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use dal_core::{JobId, JobOutcome, JobReport, Name};
use tokio_util::sync::CancellationToken;

use super::{JobRecord, JobTable, tests::StoreRig};

fn ids(reports: &[JobReport]) -> Vec<JobId> {
    reports.iter().map(|report| report.id).collect()
}

/// The ledger file the Store keeps in the session `jobs/` directory.
///
/// The jobs settled here are owned rows that write no job log, so the ledger
/// is the only file in the directory; the test names no ledger file itself.
fn ledger_file(rig: &StoreRig) -> PathBuf {
    let mut files = fs::read_dir(rig.jobs_dir())
        .expect("read the session jobs directory")
        .map(|entry| entry.expect("jobs directory entry").path());
    let file = files.next().expect("the Store wrote the ledger file");
    assert!(
        files.next().is_none(),
        "the session jobs directory holds only the ledger file"
    );
    file
}

fn ledger_len(rig: &StoreRig) -> u64 {
    fs::metadata(ledger_file(rig))
        .expect("read the ledger file length")
        .len()
}

/// Cuts the ledger file to its first `len` bytes: what a process killed,
/// or a disk that filled, in the middle of an append leaves behind.
fn truncate_to(path: &Path, len: u64) {
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open the ledger file to tear it")
        .set_len(len)
        .expect("tear the ledger tail");
}

async fn open_table(rig: &StoreRig, stage: &str) -> JobTable {
    rig.open(&HashSet::new())
        .await
        .unwrap_or_else(|error| panic!("open the job table {stage}: {error:?}"))
}

/// Ends one owned top-level job and persists its report.
async fn settle_flushed(table: &mut JobTable, rig: &StoreRig, label: &str, text: &str) -> JobId {
    let owner = Name::test();
    let id = JobId::new_v7();
    table
        .spawn_owned(
            JobRecord::new(id, label, rig.log(label), CancellationToken::new())
                .owned_by(owner.clone()),
        )
        .expect("reserve the owned job");
    table
        .settle_owned(&owner, id, JobOutcome::Exited { code: 0 }, text)
        .expect("settle the owned job");
    table.flush().await.expect("persist the ended report");
    id
}

#[tokio::test]
async fn job_table_reopen_and_next_settlement_survive_a_torn_ledger_tail() {
    let rig = StoreRig::new();

    let mut table = open_table(&rig, "in an empty directory").await;
    let acknowledged = settle_flushed(&mut table, &rig, "acknowledged", "first").await;
    let acknowledged_len = ledger_len(&rig);
    let torn = settle_flushed(&mut table, &rig, "torn", "second").await;
    let full_len = ledger_len(&rig);
    drop(table);
    assert_ne!(acknowledged, torn);
    truncate_to(
        &ledger_file(&rig),
        acknowledged_len + (full_len - acknowledged_len) / 2,
    );

    let mut table = open_table(&rig, "after the torn append").await;
    assert_eq!(
        ids(&table.take(10)),
        vec![acknowledged],
        "only the acknowledged report is offered after the tear"
    );
    let next = settle_flushed(&mut table, &rig, "next", "third").await;
    drop(table);

    let mut table = open_table(&rig, "after a settlement after a torn tail").await;
    assert_eq!(
        ids(&table.take(10)),
        vec![acknowledged, next],
        "the acknowledged report and the next one are both offered, once each"
    );
    assert_eq!(table.take(10), Vec::<JobReport>::new());
}
