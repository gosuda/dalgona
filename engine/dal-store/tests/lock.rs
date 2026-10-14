#![expect(clippy::expect_used, reason = "integration tests fail loudly")]
#![expect(clippy::disallowed_methods, reason = "integration tests fail loudly")]
//! Cross-process session locking and locked deletes.

mod support;

use std::{
    io::{BufRead, BufReader},
    num::NonZeroU64,
    process::{Command, Stdio},
    time::Duration,
};

use dal_core::{
    BlobId, Entry, EntryId, EntryKind, JournalPart, Product, Record, SessionId, Workspace,
};
use dal_store::{BlobError, Store, StoreError};
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

fn setup(tag: &str) -> (TempDir, Store, Workspace, std::path::PathBuf) {
    let temp = TempDir::new(tag);
    let workspace =
        Workspace::new(temp.path().join("workspace")).expect("temporary workspace is absolute");
    let data_root = temp.path().join("data");
    let store = Store::new(data_root.clone(), workspace.clone(), Product::Dalgona);
    (temp, store, workspace, data_root)
}

fn find_file(root: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).ok()?;
        for entry in entries {
            let path = entry.ok()?.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|base| base == name) {
                return Some(path);
            }
        }
    }
    None
}

#[test]
fn cross_process_lock_contention() {
    let (_temp, store, workspace, data_root) = setup("lock-contend");
    let id = SessionId::new_v7();
    let mut child = Command::new(env!("CARGO_BIN_EXE_append-child"))
        .arg(&data_root)
        .arg(workspace.as_path())
        .arg(id.to_string())
        .arg("hold")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("helper process spawns");
    let stdout = child.stdout.take().expect("helper stdout is piped");
    let mut lines = BufReader::new(stdout).lines();
    let ready = lines
        .next()
        .expect("helper prints a line")
        .expect("helper line reads");
    let holder: u32 = ready
        .strip_prefix("ready ")
        .expect("helper reports ready")
        .parse()
        .expect("helper pid parses");
    assert_eq!(holder, child.id(), "the lock holder is the helper process");

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime builds");
    runtime.block_on(async {
        let locked = store.open_session(id).await.expect_err("session is held");
        match &locked {
            StoreError::Locked { pid, .. } => {
                assert_eq!(
                    *pid,
                    Some(holder),
                    "exact Locked pid from the other process"
                );
            }
            _ => panic!("expected Locked, got {locked:?}"),
        }

        assert!(
            locked
                .to_string()
                .contains(&format!("session {id} is open in process {holder} (lock ")),
            "Locked text names owner and lock path: {locked}"
        );
    });

    child.kill().expect("helper is killed");
    let status = child.wait().expect("helper exits");
    assert!(!status.success(), "the helper dies from the signal");
    let opened = runtime.block_on(async {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            match store.open_session(id).await {
                Ok(opened) => break opened,
                Err(StoreError::Locked { .. }) => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "the lock releases after the holder exits"
                    );
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(other) => panic!("expected Locked or success, got {other:?}"),
            }
        }
    });
    let (mut journal, _) = opened;
    runtime.block_on(journal.close()).expect("session closes");
    let lock_path = find_file(&data_root, "lock").expect("lock file exists");
    let prior = std::fs::read_to_string(&lock_path).expect("lock file reads");
    assert_eq!(
        prior.trim(),
        std::process::id().to_string(),
        "the lock file carries the current holder's pid text after re-acquire"
    );
}

#[tokio::test]
async fn delete_locked_then_succeeds() {
    let (_temp, store, _, _) = setup("lock-delete");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "held open")])
        .await
        .expect("session appends");
    match store.delete(id) {
        Err(StoreError::Locked { .. }) => {}
        other => panic!("delete of an open journal receives Locked, got {other:?}"),
    }
    journal.close().await.expect("session closes");
    store.delete(id).expect("delete succeeds after close");
    let gone = journal
        .read_blob(BlobId::from_bytes(b"anything"))
        .expect_err("blobs are gone");
    assert!(matches!(gone, BlobError::Gone), "blob read returns Gone");
}

#[tokio::test]
async fn concurrent_opens_of_one_session_admit_exactly_one() {
    let (_temp, store, _, _) = setup("lock-race");
    let id = SessionId::new_v7();
    let mut seed = store.create_session(id);
    seed.append(vec![user(1, "seed")])
        .await
        .expect("seed append");
    seed.close().await.expect("seed closes");

    let (first, second) = tokio::join!(store.open_session(id), store.open_session(id));
    let (winner, loser) = match (first, second) {
        (Ok(winner), Err(loser)) | (Err(loser), Ok(winner)) => (winner, loser),
        (Ok(_), Ok(_)) => panic!("both opens received the session"),
        (Err(a), Err(b)) => panic!("both opens failed: {a:?} / {b:?}"),
    };
    assert!(
        matches!(&loser, StoreError::Locked { session, pid: Some(pid), .. }
            if *session == id && *pid == std::process::id()),
        "the loser is told who holds it, got {loser:?}"
    );
    let (mut journal, _) = winner;
    journal.close().await.expect("winner closes");
    let (mut again, _) = store.open_session(id).await.expect("lock frees on close");
    again.close().await.expect("session closes");
}

#[tokio::test]
async fn lost_lock_file_does_not_strand_the_session() {
    let (_temp, store, _, data_root) = setup("lock-marker-loss");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "seed")])
        .await
        .expect("seed append");
    journal.close().await.expect("seed closes");
    let lock = find_file(&data_root, "lock").expect("lock file exists");
    std::fs::remove_file(&lock).expect("lose the lock file");

    let (mut reopened, report) = store
        .open_session(id)
        .await
        .expect("open recreates the marker");
    assert!(report.torn.is_none(), "a lost marker is not journal damage");
    let locked = store
        .open_session(id)
        .await
        .expect_err("the recreated marker still excludes");
    assert!(
        matches!(locked, StoreError::Locked { .. }),
        "got {locked:?}"
    );
    assert!(lock.is_file(), "the lock file is back");
    reopened.close().await.expect("session closes");
}
