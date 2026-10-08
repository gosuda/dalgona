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
    AssistantStop, Block, CallId, Entry, EntryId, EntryKind, Family, JournalPart, ListQuery,
    Product, RawJson, Record, SessionId, TurnEndStop, TurnId, Usage, Workspace,
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
        })
        .collect();
    Record::Assistant(Entry {
        id: entry_id(id),
        parent: None,
        at: timestamp(),
        kind: EntryKind::Assistant {
            api: Family::Chat,
            model: "test-model".into(),
            content,
            usage: usage(),
            stop: AssistantStop::ToolUse,
        },
    })
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
    assert!(
        store
            .list(ListQuery {
                limit: None,
                cursor: None,
                search: None,
            })
            .expect("list empty workspace")
            .items
            .is_empty()
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
    assert!(matches!(store.read_blob(id, blob_id), Err(BlobError::Gone)));
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

#[test]
fn session_file_returns_the_journal_path() {
    let temp = TempDir::new();
    let store = store(&temp);
    let id = SessionId::new_v7();
    let path = store.session_file(id);
    assert_eq!(
        path.file_name().and_then(|name| name.to_str()),
        Some("journal.jsonl"),
        "the session file is the session journal path"
    );
    assert!(path.as_os_str().len() > "journal.jsonl".len());
}

#[test]
fn session_jobs_dir_returns_the_jobs_path() {
    let temp = TempDir::new();
    let store = store(&temp);
    let id = SessionId::new_v7();
    let path = store.session_jobs_dir(id);
    assert_eq!(
        path.file_name().and_then(|name| name.to_str()),
        Some("jobs"),
        "the session jobs dir is the session jobs path"
    );
    assert!(path.as_os_str().len() > "jobs".len());
}
