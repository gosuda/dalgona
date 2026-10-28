use std::{
    fs,
    future::Future,
    num::NonZeroU64,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    task::{Context, Poll, Waker},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::blob::INLINE_LIMIT;
use dal_core::{
    AssistantRoute, AssistantStop, Block, CallId, Entry, EntryId, EntryKind, Family, JournalPart,
    ListQuery, Product, RawJson, Record, SessionId, TurnEndStop, TurnId, Usage, Workspace,
};

use super::*;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "dal-store-{}-{time}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("create store test directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn store(temp: &TempDir) -> Store {
    let workspace = Workspace::new(temp.path().join("workspace"))
        .expect("temporary workspace path is absolute");
    Store::new(temp.path().join("data"), workspace, Product::Dalgona)
}

fn entry_id(value: u64) -> EntryId {
    EntryId::new(NonZeroU64::new(value).unwrap_or(NonZeroU64::MIN))
}

fn timestamp() -> jiff::Timestamp {
    jiff::Timestamp::now()
}

fn user(id: u64, text: impl Into<Box<str>>) -> Record {
    Record::User(Entry {
        id: entry_id(id),
        parent: None,
        at: timestamp(),
        kind: EntryKind::User {
            parts: vec![JournalPart::Text { text: text.into() }],
        },
    })
}

fn usage() -> Usage {
    Usage {
        input_tokens: 0,
        cached_input_tokens: 0,
        output_tokens: 0,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        premium_requests: None,
        cost_usd: None,
    }
}

fn assistant_with_calls(id: u64, calls: &[&str]) -> Record {
    let input = RawJson::parse("{}").expect("valid tool input");
    let content = calls
        .iter()
        .map(|call| Block::ToolCall {
            id: CallId::new(*call),
            name: "read_file".into(),
            input: input.clone(),
            replay: None,
        })
        .collect();
    Record::Assistant(Entry {
        id: entry_id(id),
        parent: None,
        at: timestamp(),
        kind: EntryKind::Assistant {
            api: Family::Chat,
            model: "test-model".into(),
            provider: None,
            route: Some(AssistantRoute::Api(Family::Chat)),
            content,
            usage: usage(),
            stop: AssistantStop::ToolUse,
            replay: None,
        },
    })
}

/// The compaction half of the batch the real compaction flow appends: one
/// entry whose companion history letter follows in the same journal batch.
fn batch_compaction(id: u64) -> Record {
    Record::Compaction(Entry {
        id: entry_id(id),
        parent: Some(entry_id(1)),
        at: timestamp(),
        kind: EntryKind::Compaction {
            summary: None,
            first_kept: None,
            tokens_before: 20,
            replay: None,
            usage: None,
            parts: vec![JournalPart::Text {
                text: "replacement".into(),
            }],
            parts_tokens: 3,
        },
    })
}

/// The letter half of that batch: an opaque Ext record to the store, whose
/// bytes the batteries history schema pairs with the compaction entry.
fn history_letter() -> Record {
    Record::Ext {
        at: timestamp(),
        ext: "history".into(),
        kind: "letter".into(),
        body: RawJson::parse(r#"{"kind":"compaction","id":"history/1.1","spans":[[1,0,0,6]]}"#)
            .expect("letter body parses"),
    }
}

#[tokio::test]
async fn lazy_first_user_writes_one_durable_header_boot_and_user_batch() {
    let temp = TempDir::new();
    let store = store(&temp);
    let data_root = store.inner.data_root.clone();
    let id = SessionId::new_v7();
    assert!(!data_root.exists());
    let mut journal = store.create_session(id);

    assert_eq!(
        journal
            .append(vec![Record::Name {
                at: timestamp(),
                name: Some("parser fix".into()),
            }])
            .await
            .expect("buffer name"),
        AppendOutcome::Buffered
    );
    assert!(!data_root.exists());

    let receipt = match journal
        .append(vec![user(1, "first message")])
        .await
        .expect("durable first user")
    {
        AppendOutcome::Durable(receipt) => receipt,
        other => panic!("expected a durable receipt, got {other:?}"),
    };
    let bytes = fs::read(journal.paths.journal()).expect("read journal");
    assert_eq!(receipt.offset, 0);
    assert_eq!(
        receipt.len,
        u64::try_from(bytes.len()).expect("file length fits u64")
    );
    let records = bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| dal_core::decode(line).expect("decode complete line").record)
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 4);
    assert!(matches!(&records[0], Record::Session(_)));
    assert!(matches!(&records[1], Record::Boot { .. }));
    assert!(matches!(&records[2], Record::Name { .. }));
    assert!(matches!(&records[3], Record::User(_)));
    assert!(journal.paths.info().is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let directory_mode = fs::metadata(journal.paths.directory())
            .expect("stat session directory")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(directory_mode, 0o700);
        let jobs_path = journal.paths.jobs();
        let jobs_mode = fs::metadata(jobs_path)
            .expect("stat jobs directory")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(jobs_mode, 0o700);
    }

    journal.close().await.expect("close file session");
}

#[tokio::test]
async fn failed_first_reservation_leaves_the_id_available_for_retry() {
    let temp = TempDir::new();
    let store = store(&temp);
    let data_root = store.inner.data_root.clone();
    fs::write(&data_root, b"not a directory").expect("block session materialization");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    let before = journal.records().to_vec();
    let turn = TurnId::new(NonZeroU64::MIN);
    let reservation = Record::TurnReserved {
        at: timestamp(),
        turn,
    };

    journal
        .append(vec![reservation.clone()])
        .await
        .expect_err("first write cannot create its directory");
    assert!(journal.is_lazy());
    assert_eq!(journal.records(), before);

    fs::remove_file(&data_root).expect("unblock session materialization");
    assert!(matches!(
        journal
            .append(vec![reservation])
            .await
            .expect("retry the same reservation"),
        AppendOutcome::Durable(_)
    ));
    journal.close().await.expect("close reserved session");
    let (mut reopened, _) = store
        .open_session(id)
        .await
        .expect("reopen reserved session");
    let reservations = reopened
        .records()
        .iter()
        .filter_map(|record| match record {
            Record::TurnReserved { turn, .. } => Some(*turn),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(reservations, vec![turn]);
    reopened.close().await.expect("close reopened session");
}

#[tokio::test]
async fn materialize_flushes_a_lazy_journal_to_durable() {
    let temp = TempDir::new();
    let store = store(&temp);
    let data_root = store.inner.data_root.clone();
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![Record::Name {
            at: timestamp(),
            name: Some("queued".into()),
        }])
        .await
        .expect("buffer name");
    assert!(journal.is_lazy() && !data_root.exists());

    journal.materialize().await.expect("materialize lazy");
    assert!(!journal.is_lazy());
    let records = fs::read(journal.paths.journal())
        .expect("read journal")
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| dal_core::decode(line).expect("decode complete line").record)
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 3);
    assert!(matches!(&records[2], Record::Name { .. }));
    journal.materialize().await.expect("materialize is a no-op");

    journal.close().await.expect("close file session");
    let (reopened, _) = store.open_session(id).await.expect("reopen materialized");
    assert!(
        reopened
            .records()
            .iter()
            .any(|record| matches!(record, Record::Name { .. }))
    );
}

#[tokio::test]
async fn reopened_records_end_with_recovery_boot() {
    let temp = TempDir::new();
    let store = store(&temp);
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "first message")])
        .await
        .expect("durable first user");
    let first = journal.records().to_vec();
    assert_eq!(first.len(), 3);
    assert!(matches!(&first[0], Record::Session(_)));
    assert!(matches!(
        &first[1],
        Record::Boot { r#gen, .. } if r#gen.get() == 1
    ));
    assert!(matches!(
        &first[2],
        Record::User(entry) if entry.id.get() == 1
    ));
    journal.close().await.expect("close file session");

    let (mut reopened, report) = store.open_session(id).await.expect("reopen session");
    let records = reopened.records();
    assert_eq!(records.len(), 4);
    assert!(matches!(&records[0], Record::Session(_)));
    assert!(matches!(
        &records[1],
        Record::Boot { r#gen, .. } if r#gen.get() == 1
    ));
    assert!(matches!(
        &records[2],
        Record::User(entry) if entry.id.get() == 1
    ));
    assert!(matches!(
        &records[3],
        Record::Boot { r#gen, .. } if r#gen.get() == report.r#gen.get()
    ));
    assert_eq!(reopened.generation().get(), report.r#gen.get());
    reopened.close().await.expect("close reopened session");
}

#[tokio::test]
async fn ephemeral_blobs_and_records_leave_no_files_before_or_after_close() {
    let temp = TempDir::new();
    let store = store(&temp);
    let data_root = store.inner.data_root.clone();
    let mut journal = store.ephemeral_session(SessionId::new_v7());
    let text = "x".repeat(INLINE_LIMIT);
    let expected = text.as_bytes().to_vec();

    assert_eq!(
        journal
            .append(vec![user(1, text)])
            .await
            .expect("append in memory"),
        AppendOutcome::Memory
    );
    assert!(journal.is_ephemeral());
    assert!(journal.sidecar().is_none());
    let user_record = journal
        .records
        .iter()
        .find(|record| matches!(record, Record::User(_)))
        .expect("user record");
    let blob_ids = blob::named_blobs(user_record);
    let [blob_id] = blob_ids.as_slice() else {
        panic!("threshold-sized user text names one blob");
    };
    assert_eq!(
        journal.read_blob(*blob_id).expect("read memory blob"),
        expected
    );
    assert!(!data_root.exists());
    assert_eq!(
        store
            .list(ListQuery {
                limit: None,
                cursor: None,
                search: None,
            })
            .expect("list empty workspace")
            .items,
        [] as [dal_core::SessionInfo; 0]
    );

    journal.close().await.expect("close memory session");
    assert_eq!(
        journal
            .read_blob(*blob_id)
            .expect("memory blob survives close"),
        expected
    );
    assert!(!data_root.exists());
}

#[tokio::test]
async fn locked_session_cannot_be_opened_or_deleted_until_close() {
    let temp = TempDir::new();
    let store = store(&temp);
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    let text = "b".repeat(INLINE_LIMIT);
    let blob_id = BlobId::from_bytes(text.as_bytes());
    journal
        .append(vec![user(1, text)])
        .await
        .expect("create file session");

    assert!(matches!(
        store.open_session(id).await,
        Err(StoreError::Locked { .. })
    ));
    assert!(matches!(store.delete(id), Err(StoreError::Locked { .. })));

    journal.close().await.expect("release session lock");
    store.delete(id).expect("delete closed session");
    assert!(
        !store.paths(id).directory().exists(),
        "deleted session tree must be gone"
    );
    assert!(matches!(store.read_blob(id, blob_id), Err(BlobError::Gone)));
}

/// A parent directory whose sync fails must surface the error: the removal
/// would otherwise report success while its directory entry could vanish.
#[tokio::test]
async fn delete_reports_a_failed_parent_directory_sync() {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new();
    let store = store(&temp);
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "b".repeat(INLINE_LIMIT))])
        .await
        .expect("create file session");
    journal.close().await.expect("release session lock");

    #[cfg(unix)]
    {
        let workspace_dir = store
            .paths(id)
            .directory()
            .parent()
            .expect("workspace key dir")
            .to_path_buf();
        let mut permissions = fs::metadata(&workspace_dir)
            .expect("stat workspace directory")
            .permissions();
        permissions.set_mode(0o300);
        fs::set_permissions(&workspace_dir, permissions).expect("drop directory read permission");
        let enforced = fs::read_dir(&workspace_dir).is_err();
        if !enforced {
            let _ = fs::set_permissions(&workspace_dir, fs::Permissions::from_mode(0o700));
        }
        assert!(
            enforced,
            "chmod is not enforced for this user (root or an ACL bypass), so the directory-sync failure is unreachable; run this test unprivileged"
        );

        let result = store.delete(id);
        let _ = fs::set_permissions(&workspace_dir, fs::Permissions::from_mode(0o700));
        assert!(
            matches!(result, Err(StoreError::Io { .. })),
            "got {result:?}"
        );
    }
    #[cfg(not(unix))]
    {
        store.delete(id).expect("delete closed session");
    }
}

#[tokio::test]
async fn live_journal_locks_at_once_for_same_process_open_and_aliased_store() {
    let temp = TempDir::new();
    let store = store(&temp);
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "hold the session lock")])
        .await
        .expect("create file session");

    // A live in-process journal reports Locked at once — the open must
    // not sit out its sixty-second retry budget against our own guard.
    let started = std::time::Instant::now();
    assert!(matches!(
        store.open_session(id).await,
        Err(StoreError::Locked { .. })
    ));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "a same-process open against a live journal must not wait out the retry budget"
    );

    // The same data root reached through another spelling owns the same
    // holder: the registry keys canonicalize, so the aliased store fails
    // fast instead of retrying the budget against an unseen lock. The
    // `..` alias needs an existing intermediate component but no symlink
    // rights, so it works unprivileged on every runner.
    let via = temp.path().join("via");
    fs::create_dir(&via).expect("create alias intermediate");
    let aliased = Store::new(
        via.join("..").join("data"),
        Workspace::new(temp.path().join("workspace")).expect("workspace path"),
        Product::Dalgona,
    );
    let aliased_started = std::time::Instant::now();
    assert!(matches!(
        aliased.open_session(id).await,
        Err(StoreError::Locked { .. })
    ));
    assert!(
        aliased_started.elapsed() < std::time::Duration::from_secs(30),
        "an aliased store must see the live in-process holder, not retry the budget"
    );

    // A same-process reopen succeeds once close retires the guard: the
    // release transient is retried, not reported.
    journal.close().await.expect("close journal");
    let (mut reopened, _) = store.open_session(id).await.expect("reopen after close");
    reopened.close().await.expect("close reopened session");
}

#[tokio::test]
async fn recovery_aborts_calls_once_with_started_and_not_run_outcomes() {
    let temp = TempDir::new();
    let store = store(&temp);
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "run the tools")])
        .await
        .expect("create session");
    let turn = TurnId::new(NonZeroU64::MIN);
    journal
        .append(vec![
            Record::TurnStart {
                at: timestamp(),
                turn,
            },
            assistant_with_calls(2, &["started", "not-started"]),
        ])
        .await
        .expect("start unfinished turn");
    journal
        .append(vec![Record::ToolStart {
            at: timestamp(),
            turn,
            call: CallId::new("started"),
        }])
        .await
        .expect("start first call");
    journal.close().await.expect("close interrupted session");

    let (mut reopened, report) = store
        .open_session(id)
        .await
        .expect("repair interrupted session");
    let aborted = report.aborted.as_ref().expect("aborted turn fact");
    assert_eq!(aborted.turn, turn);
    assert_eq!(aborted.interrupted, 1);
    assert_eq!(aborted.not_run, 1);
    let mut interrupted_text = false;
    let mut not_run_text = false;
    for record in &reopened.records {
        let Record::ToolResult(entry) = record else {
            continue;
        };
        let EntryKind::ToolResult { parts, .. } = &entry.kind else {
            continue;
        };
        for part in parts {
            if let JournalPart::Text { text } = part {
                interrupted_text |= text.as_ref() == crate::error::INTERRUPTED_CALL;
                not_run_text |= text.as_ref() == crate::error::NOT_RUN_CALL;
            }
        }
    }
    assert!(interrupted_text);
    assert!(not_run_text);
    assert_eq!(
        reopened
            .records
            .iter()
            .filter(|record| matches!(
                record,
                Record::TurnEnd {
                    stop: TurnEndStop::Aborted,
                    ..
                }
            ))
            .count(),
        1
    );
    reopened.close().await.expect("close repaired session");

    let (mut second, second_report) = store
        .open_session(id)
        .await
        .expect("reopen repaired session");
    assert!(second_report.aborted.is_none());
    second.close().await.expect("close second open");
}

#[tokio::test]
async fn fork_without_an_anchor_reports_unknown_entry() {
    let temp = TempDir::new();
    let store = store(&temp);
    let id = SessionId::new_v7();
    let mut source = store.create_session(id);

    let error = store
        .fork(&mut source, entry_id(1))
        .await
        .expect_err("empty source has no fork anchor");

    assert!(matches!(
        error,
        StoreError::UnknownEntry { id: error_id, entry }
            if error_id == id && entry == entry_id(1)
    ));
    assert!(!source.paths.directory().exists());
    source.close().await.expect("close lazy source");
}

#[tokio::test]
async fn fork_is_lazy_and_clone_shares_blobs_before_publishing_journal() {
    let temp = TempDir::new();
    let store = store(&temp);
    let source_id = SessionId::new_v7();
    let mut source = store.create_session(source_id);
    let text = "c".repeat(INLINE_LIMIT);
    let bytes = text.as_bytes().to_vec();
    let blob_id = BlobId::from_bytes(&bytes);
    source
        .append(vec![user(1, text.clone())])
        .await
        .expect("create source session");

    let (mut fork, restart_text) = store
        .fork(&mut source, entry_id(1))
        .await
        .expect("fork first user entry");
    assert_eq!(restart_text, text);
    assert!(matches!(&fork.state, State::Lazy { .. }));
    assert!(!fork.paths.directory().exists());

    let mut clone = store
        .clone_session(&mut source)
        .await
        .expect("clone active path");
    let copied_user = clone
        .records
        .iter()
        .find_map(|record| record.entry())
        .expect("copied tree entry");
    assert_eq!(copied_user.id, entry_id(1));
    #[cfg(unix)]
    let source_blob = source
        .paths
        .directory()
        .join("blobs")
        .join(blob_id.to_string());
    let clone_blob = clone
        .paths
        .directory()
        .join("blobs")
        .join(blob_id.to_string());
    assert_eq!(
        fs::read(&clone_blob).expect("read shared clone blob"),
        bytes
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            fs::metadata(&source_blob)
                .expect("source blob metadata")
                .ino(),
            fs::metadata(&clone_blob)
                .expect("clone blob metadata")
                .ino()
        );
    }
    if let Record::Session(header) = &clone.records[0] {
        let from = header.from.as_ref().expect("clone provenance");
        assert_eq!(from.session, source_id);
        assert_eq!(from.entry, Some(entry_id(1)));
    } else {
        panic!("clone starts with a session header");
    }

    fork.close().await.expect("close lazy fork");
    clone.close().await.expect("close clone");
    source.close().await.expect("close source");
}

#[tokio::test]
async fn cancelled_admitted_append_settles_before_the_next_append() {
    let temp = TempDir::new();
    let store = store(&temp);
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "cancel test")])
        .await
        .expect("create file session");

    let shards = crate::shard::shared().expect("start shard workers");
    let (started, release) = shards.hold_worker_for_test(id).expect("queue worker hold");
    tokio::task::spawn_blocking(move || started.recv())
        .await
        .expect("wait task")
        .expect("worker started hold");

    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    let future = journal.append(vec![Record::Name {
        at: timestamp(),
        name: Some("settled before next".into()),
    }]);
    let mut future = Box::pin(future);
    assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
    drop(future);
    release.send(()).expect("release held shard");

    journal
        .append(vec![Record::Archive {
            at: timestamp(),
            archived: false,
        }])
        .await
        .expect("next append settles and follows prior append");
    assert!(journal.records.iter().any(|record| matches!(
        record,
        Record::Name {
            name: Some(name),
            ..
        } if name.as_ref() == "settled before next"
    )));
    journal.close().await.expect("close after settlement");
}

#[tokio::test]
async fn cancelled_close_restores_live_flag_and_reopen_reports_locked() {
    let temp = TempDir::new();
    let store = store(&temp);
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "cancel-close test")])
        .await
        .expect("create file session");
    let lock_path = journal.paths.lock();
    assert!(lock::live_in_process(&lock_path));

    // Hold the shard worker so the lane's Retire reply can never land:
    // polling `close` then parks deterministically inside the window the
    // flag names — guards already detached, `ReliveOnDrop` armed, the
    // `lane.close().await` still pending.
    let shards = crate::shard::shared().expect("start shard workers");
    let (started, release) = shards.hold_worker_for_test(id).expect("queue worker hold");
    tokio::task::spawn_blocking(move || started.recv())
        .await
        .expect("wait task")
        .expect("worker started hold");

    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    let mut close = Box::pin(journal.close());
    assert!(matches!(close.as_mut().poll(&mut context), Poll::Pending));
    assert!(
        !lock::live_in_process(&lock_path),
        "retirement detaches the guards before the lane await"
    );

    // Cancelling the close future must re-mark the holder live: the
    // journal still owns the guards, so a same-process reopen reports
    // Locked at once rather than retrying the release that never comes.
    drop(close);
    assert!(lock::live_in_process(&lock_path));
    let started_at = std::time::Instant::now();
    assert!(matches!(
        store.open_session(id).await,
        Err(StoreError::Locked { .. })
    ));
    assert!(
        started_at.elapsed() < std::time::Duration::from_secs(30),
        "a reopen against a cancelled close must not wait out the retry budget"
    );

    // Releasing the worker lets the queued Retire retire the lane; the
    // journal's drop then marks the guard transient so the reopen
    // retries through the real release and succeeds.
    release.send(()).expect("release held shard");
    drop(journal);
    let (mut reopened, _) = store
        .open_session(id)
        .await
        .expect("reopen after cancelled close and journal drop");
    reopened.close().await.expect("close reopened session");
}

/// A journal dropped while the worker still owes an admitted batch its file
/// write keeps the session locked until the worker retires the journal.
/// Otherwise a reopen scans the file before the batch lands and attaches a
/// second writer at the same end offset, which the late batch then overwrites.
#[tokio::test]
async fn dropped_journal_keeps_its_lock_until_the_worker_finishes_an_admitted_append() {
    let temp = TempDir::new();
    let store = store(&temp);
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "drop test")])
        .await
        .expect("create file session");

    let shards = crate::shard::shared().expect("start shard workers");
    let (started, release) = shards.hold_worker_for_test(id).expect("queue worker hold");
    tokio::task::spawn_blocking(move || started.recv())
        .await
        .expect("wait task")
        .expect("worker started hold");

    let mut context = Context::from_waker(Waker::noop());
    let future = journal.append(vec![Record::Name {
        at: timestamp(),
        name: Some("admitted".into()),
    }]);
    let mut future = Box::pin(future);
    assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
    drop(future);
    drop(journal);

    let release_later = async {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        release.send(()).expect("release held shard");
    };
    let (reopened, ()) = tokio::join!(store.open_session(id), release_later);
    let (mut reopened, report) = reopened.expect("reopen once the worker retired the journal");

    assert!(report.torn.is_none(), "the admitted batch landed whole");
    let admitted = reopened
        .records()
        .iter()
        .filter(|record| {
            matches!(
                record,
                Record::Name {
                    name: Some(name),
                    ..
                } if name.as_ref() == "admitted"
            )
        })
        .count();
    assert_eq!(
        admitted, 1,
        "the reopen scanned the file after the batch landed"
    );
    reopened.close().await.expect("close reopened session");
}

/// A first append creates the session directory chain. The journal's own sync
/// covers only the session directory, so each new directory's entry in its
/// parent is synced too — on every attempt, not just the one that created it.
/// A parent that cannot be synced fails the append instead of acknowledging
/// records a power loss could take with the directory, and a retry while the
/// parent is still unsyncable fails again rather than trusting the directories
/// the first attempt left behind.
#[cfg(unix)]
#[tokio::test]
async fn first_append_syncs_the_entry_of_every_directory_it_creates() {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new();
    let store = store(&temp);
    let id = SessionId::new_v7();
    let sessions = temp.path().join("data").join("sessions");
    fs::create_dir_all(&sessions).expect("create the sessions directory");
    // Write and search without read: the workspace directory can be created
    // inside it, but the directory cannot be opened to sync the new entry.
    fs::set_permissions(&sessions, fs::Permissions::from_mode(0o300))
        .expect("drop directory read permission");
    let enforced = fs::read_dir(&sessions).is_err();
    let mut journal = store.create_session(id);

    let refused = journal
        .append(vec![user(1, "entry a crash could drop")])
        .await;
    assert!(
        journal.is_lazy(),
        "a create that could not be made durable leaves the session lazy"
    );
    // The failed attempt leaves the chain in place; the retry must re-owe the
    // same sync instead of reporting success for an entry that was never made
    // durable.
    let retried_while_broken = journal.append(vec![user(1, "retry before repair")]).await;
    fs::set_permissions(&sessions, fs::Permissions::from_mode(0o700))
        .expect("restore directory permissions");

    assert!(
        enforced,
        "chmod is not enforced for this user (root or an ACL bypass), so the directory-sync failure is unreachable; run this test unprivileged"
    );
    assert!(
        matches!(&refused, Err(StoreError::Io { path, .. }) if *path == sessions),
        "got {refused:?}"
    );
    assert!(
        matches!(&retried_while_broken, Err(StoreError::Io { path, .. }) if *path == sessions),
        "the retry finds the chain present and must fail on the same sync, got {retried_while_broken:?}"
    );
    assert!(
        journal.is_lazy(),
        "two undurable creates still leave the session lazy"
    );
    journal
        .append(vec![user(1, "synced")])
        .await
        .expect("the retry after repair creates the session");
    journal.close().await.expect("session closes");
}

#[tokio::test]
async fn blob_publish_failure_keeps_record_unpublished_and_marks_broken() {
    let temp = TempDir::new();
    let store = store(&temp);
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "existing record")])
        .await
        .expect("create file session");
    let before = fs::read(journal.paths.journal()).expect("read acknowledged bytes");
    let blob_dir = journal.paths.directory().join("blobs");
    fs::remove_dir(&blob_dir).expect("remove empty blob directory");
    fs::write(&blob_dir, b"not a directory").expect("block blob publication");

    let error = journal
        .append(vec![user(2, "d".repeat(INLINE_LIMIT))])
        .await
        .expect_err("blob publication fails");
    assert!(matches!(error, StoreError::Blob(BlobError::Io { .. })));
    assert_eq!(
        fs::read(journal.paths.journal()).expect("read journal after blob failure"),
        before
    );
    assert_eq!(journal.records.len(), 3);
    assert!(matches!(
        journal
            .append(vec![Record::Archive {
                at: timestamp(),
                archived: false,
            }])
            .await,
        Err(StoreError::Broken { .. })
    ));
    journal.close().await.expect("close broken journal");
}

#[tokio::test]
async fn compaction_and_letter_records_roll_back_as_one_batch() {
    let temp = TempDir::new();
    let store = store(&temp);
    let compaction = Record::Compaction(Entry {
        id: entry_id(2),
        parent: Some(entry_id(1)),
        at: timestamp(),
        kind: EntryKind::Compaction {
            summary: None,
            first_kept: None,
            tokens_before: 20,
            replay: None,
            usage: None,
            parts: vec![JournalPart::Text {
                text: "replacement".into(),
            }],
            parts_tokens: 3,
        },
    });
    let compaction_len = dal_core::encode(&compaction)
        .expect("compaction encodes")
        .len();
    store
        .set_faults_for_test(Faults {
            write_after_bytes: Some(compaction_len + 2),
            ..Faults::default()
        })
        .expect("configure failure inside the second record");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "source")])
        .await
        .expect("materialize the source session");
    let before = fs::read(journal.paths.journal()).expect("read durable prefix");
    let letter_body =
        RawJson::parse(r#"{"kind":"compaction","id":"history/1.1","spans":[[1,0,0,6]]}"#)
            .expect("letter body parses");
    let letter = Record::Ext {
        at: timestamp(),
        ext: "history".into(),
        kind: "letter".into(),
        body: letter_body,
    };
    let result = journal.append(vec![compaction, letter]).await;
    assert!(matches!(result, Err(StoreError::WriteFailed { .. })));
    journal.close().await.expect("close rolled-back journal");
    drop(journal);

    store
        .set_faults_for_test(Faults::default())
        .expect("clear append failure");
    let (mut reopened, _) = store.open_session(id).await.expect("reopen session");
    assert!(
        reopened
            .records()
            .iter()
            .all(|record| !matches!(record, Record::Compaction(_)))
    );
    assert!(
        reopened
            .records()
            .iter()
            .all(|record| !matches!(record, Record::Ext { kind, .. } if kind.as_ref() == "letter"))
    );
    let (fold, _) = dal_core::Session::replay(reopened.records().to_vec(), timestamp())
        .expect("replay durable prefix");
    assert_eq!(fold.leaf_entry(), Some(entry_id(1)));
    assert!(fold.leaf_ext().next().is_none());
    let after = fs::read(store.session_file(id)).expect("read rolled-back journal");
    assert_eq!(&after[..before.len()], before.as_slice());
    reopened.close().await.expect("close reopened journal");
}

#[tokio::test]
async fn rolled_back_write_reports_typed_source_without_changing_the_journal() {
    let temp = TempDir::new();
    let store = store(&temp);
    store
        .set_faults_for_test(Faults {
            write_after_bytes: Some(1),
            ..Faults::default()
        })
        .expect("configure partial-write failpoint");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "durable prefix")])
        .await
        .expect("create durable journal before write failure");
    let path = journal.paths.journal();
    let before = fs::read(&path).expect("read durable prefix");

    let error = journal
        .append(vec![Record::Name {
            at: timestamp(),
            name: Some("will not commit".into()),
        }])
        .await
        .expect_err("partial write rolls back");

    assert!(matches!(
        &error,
        StoreError::WriteFailed { id: failed_id, cause }
            if *failed_id == id
                && matches!(
                    cause.as_ref(),
                    crate::error::JournalError::Io { op: "write", .. }
                )
    ));
    assert_eq!(
        error.to_string(),
        format!(
            "could not write session {id}: write {}: injected partial write. dalgon removed the partial record. Remove the cause, then resume the session.",
            path.display()
        )
    );
    assert!(std::error::Error::source(&error).is_some());
    assert_eq!(fs::read(path).expect("read rolled-back journal"), before);
    assert_eq!(journal.records.len(), 3);
    journal
        .close()
        .await
        .expect("close journal after rolled-back write");
}

#[tokio::test]
async fn failed_rollback_marks_broken_until_reopen_repairs_tail() {
    let temp = TempDir::new();
    let store = store(&temp);
    store
        .set_faults_for_test(Faults {
            write_after_bytes: Some(1),
            truncate_error: true,
            ..Faults::default()
        })
        .expect("configure append failpoint");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "durable prefix")])
        .await
        .expect("create durable file before enabling writer fault");

    let error = journal
        .append(vec![Record::Name {
            at: timestamp(),
            name: Some("will not commit".into()),
        }])
        .await
        .expect_err("partial write and rollback fail");
    assert!(matches!(
        error,
        StoreError::Journal(crate::error::JournalError::Damaged { .. })
    ));
    assert!(matches!(
        journal
            .append(vec![Record::Archive {
                at: timestamp(),
                archived: false,
            }])
            .await,
        Err(StoreError::Broken { .. })
    ));
    journal.close().await.expect("close broken writer");

    store
        .set_faults_for_test(Faults::default())
        .expect("clear append failpoint");
    let (mut reopened, report) = store.open_session(id).await.expect("reopen and repair");
    assert!(report.torn.is_some());
    assert!(reopened.records.iter().all(|record| {
        !matches!(record, Record::Name { name: Some(name), .. } if name.as_ref() == "will not commit")
    }));
    reopened.close().await.expect("close repaired writer");
}

/// A batch whose durability sync fails must roll back as one unit even
/// though every byte landed: the receipt never surfaces before the sync, so
/// the journal returns to the previous prefix instead of keeping a
/// compaction whose letter records were not accepted. The typed error names
/// the `sync` stage, distinguishing this from the partial-write rollback in
/// `compaction_and_letter_records_roll_back_as_one_batch`, and the cleared
/// fault lets a fresh handle accept the same batch again.
#[tokio::test]
async fn failed_batch_sync_rolls_back_the_whole_batch_and_resumes() {
    let temp = TempDir::new();
    let store = store(&temp);
    store
        .set_faults_for_test(Faults {
            sync_error: true,
            ..Faults::default()
        })
        .expect("configure durability sync failure");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "source")])
        .await
        .expect("materialize the source session");
    let before = fs::read(journal.paths.journal()).expect("read durable prefix");

    let error = journal
        .append(vec![batch_compaction(2), history_letter()])
        .await
        .expect_err("durability sync fails");
    assert!(matches!(
        &error,
        StoreError::WriteFailed { id: failed_id, cause }
            if *failed_id == id
                && matches!(cause.as_ref(), crate::error::JournalError::Io { op: "sync", .. })
    ));
    assert_eq!(
        fs::read(journal.paths.journal()).expect("read rolled-back journal"),
        before
    );
    assert_eq!(journal.records.len(), 3);
    journal.close().await.expect("close rolled-back journal");

    store
        .set_faults_for_test(Faults::default())
        .expect("clear sync failure");
    let (mut reopened, report) = store.open_session(id).await.expect("reopen session");
    assert!(report.torn.is_none());
    let records = reopened.records();
    assert!(
        records
            .iter()
            .all(|record| !matches!(record, Record::Compaction(_)))
    );
    assert!(
        records
            .iter()
            .all(|record| !matches!(record, Record::Ext { kind, .. } if kind.as_ref() == "letter"))
    );
    let (fold, _) =
        dal_core::Session::replay(records.to_vec(), timestamp()).expect("replay previous prefix");
    assert_eq!(fold.leaf_entry(), Some(entry_id(1)));
    assert!(fold.leaf_ext().next().is_none());

    reopened
        .append(vec![batch_compaction(2), history_letter()])
        .await
        .expect("retry the batch after the sync-stage rollback");
    let committed = reopened.records();
    assert!(
        committed
            .iter()
            .any(|record| matches!(record, Record::Compaction(entry) if entry.id == entry_id(2)))
    );
    assert!(
        committed
            .iter()
            .any(|record| matches!(record, Record::Ext { kind, .. } if kind.as_ref() == "letter"))
    );
    reopened.close().await.expect("close retried journal");
}

/// When the durability sync fails and the truncate-back also fails, the
/// journal is damaged with every batch byte still in the file. Because the
/// batch is one contiguous write, reopen then finds a complete, consistent
/// batch — never a compaction prefix without its letter records — and no
/// torn tail. This is a same-process reopen after a pre-handoff sync fault,
/// not a crash proof: the injected fault returns before the off-thread
/// `sync_all` starts, so the test pins the failed-rollback boundary only.
#[tokio::test]
async fn failed_batch_sync_rollback_reopens_the_whole_batch() {
    let temp = TempDir::new();
    let store = store(&temp);
    store
        .set_faults_for_test(Faults {
            sync_error: true,
            truncate_error: true,
            ..Faults::default()
        })
        .expect("configure sync and rollback failure");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "source")])
        .await
        .expect("materialize the source session");

    let error = journal
        .append(vec![batch_compaction(2), history_letter()])
        .await
        .expect_err("sync failure and failed rollback");
    assert!(matches!(
        error,
        StoreError::Journal(crate::error::JournalError::Damaged { .. })
    ));
    assert!(matches!(
        journal
            .append(vec![Record::Archive {
                at: timestamp(),
                archived: false,
            }])
            .await,
        Err(StoreError::Broken { .. })
    ));
    journal.close().await.expect("close damaged journal");

    store
        .set_faults_for_test(Faults::default())
        .expect("clear sync and rollback failure");
    let (mut reopened, report) = store.open_session(id).await.expect("reopen session");
    assert!(
        report.torn.is_none(),
        "a fully written batch leaves no torn tail to repair"
    );
    let records = reopened.records();
    assert!(
        records
            .iter()
            .any(|record| matches!(record, Record::Compaction(entry) if entry.id == entry_id(2)))
    );
    assert!(
        records
            .iter()
            .any(|record| matches!(record, Record::Ext { kind, .. } if kind.as_ref() == "letter"))
    );
    let (fold, _) =
        dal_core::Session::replay(records.to_vec(), timestamp()).expect("replay the whole batch");
    assert_eq!(fold.leaf_entry(), Some(entry_id(2)));
    assert!(fold.leaf_ext().next().is_some());
    reopened.close().await.expect("close reopened journal");
}

/// Reopening a failed batch must not retain a compaction without its letter.
#[tokio::test]
async fn partial_batch_write_with_failed_rollback_keeps_no_orphan_compaction() {
    let temp = TempDir::new();
    let store = store(&temp);
    let compaction = batch_compaction(2);
    let compaction_len = dal_core::encode(&compaction)
        .expect("compaction encodes")
        .len();
    store
        .set_faults_for_test(Faults {
            write_after_bytes: Some(compaction_len + 2),
            truncate_error: true,
            ..Faults::default()
        })
        .expect("configure partial write and failed rollback");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "source")])
        .await
        .expect("materialize the source session");

    let error = journal
        .append(vec![compaction, history_letter()])
        .await
        .expect_err("partial write and failed rollback");
    assert!(matches!(
        error,
        StoreError::Journal(crate::error::JournalError::Damaged { .. })
    ));
    journal.close().await.expect("close damaged journal");

    store
        .set_faults_for_test(Faults::default())
        .expect("clear append faults");
    let (mut reopened, report) = store.open_session(id).await.expect("reopen session");
    assert!(report.torn.is_some());
    let records = reopened.records();
    assert!(
        records
            .iter()
            .all(|record| !matches!(record, Record::Compaction(_))),
        "the compaction record must not survive its failed batch"
    );
    assert!(
        records
            .iter()
            .all(|record| !matches!(record, Record::Ext { kind, .. } if kind.as_ref() == "letter"))
    );
    let (fold, _) =
        dal_core::Session::replay(records.to_vec(), timestamp()).expect("replay previous prefix");
    assert_eq!(fold.leaf_entry(), Some(entry_id(1)));
    assert!(fold.leaf_ext().next().is_none());
    reopened.close().await.expect("close reopened journal");
}
