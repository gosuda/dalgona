#![expect(clippy::expect_used, reason = "integration tests fail loudly")]
#![expect(clippy::collapsible_if, reason = "integration tests fail loudly")]
#![expect(clippy::too_many_lines, reason = "integration tests fail loudly")]
//! Session lifecycles: leaves, branches, compaction, labels, fork, and clone.

mod support;

use std::{fs, num::NonZeroU64};

use dal_core::{
    AssistantStop, BlobId, Block, CallId, Entry, EntryId, EntryKind, Family, JournalPart, Product,
    RawJson, Record, SessionId, TurnId, Usage, Workspace, encode,
};
use dal_store::{FileMode, Store, StoreError, write_atomic_new};
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

fn usage() -> Usage {
    Usage {
        input_tokens: 1,
        cached_input_tokens: 0,
        output_tokens: 1,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        cost_usd: None,
    }
}

fn assistant_with_calls(id: u64, calls: &[&str]) -> Record {
    let input = RawJson::parse("{}").expect("empty object parses");
    Record::Assistant(Entry {
        id: entry_id(id),
        parent: None,
        at: timestamp(),
        kind: EntryKind::Assistant {
            api: Family::Chat,
            model: "test-model".into(),
            content: calls
                .iter()
                .map(|call| Block::ToolCall {
                    id: CallId::new(*call),
                    name: "read".into(),
                    input: input.clone(),
                })
                .collect(),
            usage: usage(),
            stop: AssistantStop::ToolUse,
        },
    })
}

fn tool_result(id: u64, call: &str) -> Record {
    Record::ToolResult(Entry {
        id: entry_id(id),
        parent: None,
        at: timestamp(),
        kind: EntryKind::ToolResult {
            call: CallId::new(call),
            name: "read".into(),
            error: false,
            parts: vec![JournalPart::Text {
                text: "done".into(),
            }],
            changes: vec![],
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

#[tokio::test]
async fn lazy_blob_put_is_rejected_without_creating_session_files() {
    let (_temp, store) = setup("life-blob-put-lazy");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    let error = journal
        .put_blob(b"too early".to_vec())
        .expect_err("a lazy session has no durable storage yet");
    assert!(matches!(error, StoreError::Invalid { .. }));
    assert!(!store.session_file(id).exists());
}

#[tokio::test]
async fn file_blob_put_deduplicates_and_survives_reopen() {
    let (_temp, store) = setup("life-blob-file-put");
    let id = SessionId::new_v7();
    let bytes = b"post materialization payload".to_vec();
    let other = b"second payload".to_vec();
    let expected = BlobId::from_bytes(&bytes);
    let expected_other = BlobId::from_bytes(&other);
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "first")])
        .await
        .expect("append materializes the lazy session");
    let first = journal
        .put_blob(bytes.clone())
        .expect("blob write on a materialized journal");
    let repeated = journal
        .put_blob(bytes.clone())
        .expect("repeated blob write");
    let second = journal
        .put_blob(other.clone())
        .expect("distinct blob write");
    assert_eq!(first, expected);
    assert_eq!(repeated, expected);
    assert_eq!(second, expected_other);
    journal.close().await.expect("close file journal");
    drop(journal);

    let (mut reopened, _) = store.open_session(id).await.expect("reopen session");
    assert_eq!(
        reopened.read_blob(expected).expect("read durable blob"),
        bytes
    );
    assert_eq!(
        reopened
            .read_blob(expected_other)
            .expect("read second blob"),
        other
    );
    reopened.close().await.expect("close reopened journal");
}

#[tokio::test]
async fn fork_shares_a_preexisting_image_blob_without_copying_its_file() {
    let (_temp, store) = setup("life-fork-blob-share");
    let id = SessionId::new_v7();
    let bytes = b"shared pre-fork image bytes".to_vec();
    let mut source = store.create_session(id);
    source
        .append(vec![user(1, "seed")])
        .await
        .expect("seed materializes the session");
    let blob = source.put_blob(bytes.clone()).expect("write image blob");
    source
        .append(vec![
            Record::User(Entry {
                id: entry_id(2),
                parent: Some(entry_id(1)),
                at: timestamp(),
                kind: EntryKind::User {
                    parts: vec![JournalPart::ImageBlob {
                        mime: "image/png".into(),
                        blob: blob.to_string().into(),
                        bytes: u64::try_from(bytes.len()).expect("image length fits"),
                    }],
                },
            }),
            Record::User(Entry {
                id: entry_id(3),
                parent: Some(entry_id(2)),
                at: timestamp(),
                kind: EntryKind::User {
                    parts: vec![JournalPart::Text {
                        text: "branch here".into(),
                    }],
                },
            }),
        ])
        .await
        .expect("append image and branch messages");

    let (mut fork, text) = source.fork(entry_id(3)).await.expect("fork image session");
    assert_eq!(text, "branch here");
    assert_eq!(source.read_blob(blob).expect("read source blob"), bytes);
    assert_eq!(fork.read_blob(blob).expect("read fork blob"), bytes);
    let source_path = store
        .session_file(id)
        .parent()
        .expect("source session directory")
        .join("blobs")
        .join(blob.to_string());
    let fork_path = store
        .session_file(fork.id())
        .parent()
        .expect("fork session directory")
        .join("blobs")
        .join(blob.to_string());
    assert_eq!(
        fs::read_dir(source_path.parent().expect("source blob dir"))
            .expect("source files")
            .count(),
        1
    );
    assert_eq!(
        fs::read_dir(fork_path.parent().expect("fork blob dir"))
            .expect("fork files")
            .count(),
        1
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let source_meta = fs::metadata(&source_path).expect("source metadata");
        let fork_meta = fs::metadata(&fork_path).expect("fork metadata");
        assert_eq!(source_meta.ino(), fork_meta.ino());
        assert_eq!(source_meta.nlink(), 2);
    }
    source.close().await.expect("close source");
    fork.close().await.expect("close fork");
}

#[tokio::test]
async fn memory_blob_put_deduplicates_and_reads_back() {
    let (_temp, store) = setup("life-blob-memory");
    let mut journal = store.ephemeral_session(SessionId::new_v7());
    let bytes = b"ephemeral blob".to_vec();
    let expected = BlobId::from_bytes(&bytes);
    assert_eq!(
        journal.put_blob(bytes.clone()).expect("first write"),
        expected
    );
    assert_eq!(
        journal.put_blob(bytes.clone()).expect("repeat write"),
        expected
    );
    assert_eq!(journal.read_blob(expected).expect("read blob"), bytes);
}

fn leaf_records(records: &[Record]) -> Vec<Option<EntryId>> {
    records
        .iter()
        .filter_map(|record| match record {
            Record::Leaf { to, .. } => Some(*to),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn leaf_move_survives_reopen() {
    let (_temp, store) = setup("life-leaf");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "first")])
        .await
        .expect("user appends");
    journal
        .append(vec![assistant_with_calls(2, &["a", "b"])])
        .await
        .expect("assistant appends");
    journal
        .append(vec![tool_result(3, "a")])
        .await
        .expect("result appends");
    journal
        .append(vec![Record::Leaf {
            at: timestamp(),
            to: Some(entry_id(1)),
        }])
        .await
        .expect("leaf moves to the user");
    journal.close().await.expect("session closes");
    let (reopened, _) = store.open_session(id).await.expect("session reopens");
    let leaves = leaf_records(reopened.records());
    assert_eq!(leaves, vec![Some(entry_id(1))], "leaf remains the user");
    assert_eq!(leaves.len(), 1, "one leaf record exists");
    for want in [1, 2, 3] {
        assert!(
            reopened.records().iter().any(|record| match record {
                Record::User(entry) | Record::Assistant(entry) | Record::ToolResult(entry) =>
                    entry.id.get() == want,
                _ => false,
            }),
            "all four entries remain"
        );
    }
    assert!(
        reopened
            .records()
            .iter()
            .any(|record| matches!(record, Record::Session(_))),
        "the session header remains"
    );
}

#[tokio::test]
async fn synthetic_result_on_unresolved_branch() {
    let (_temp, store) = setup("life-branch");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "run")])
        .await
        .expect("user appends");
    journal
        .append(vec![assistant_with_calls(2, &["a", "b"])])
        .await
        .expect("assistant appends");
    journal
        .append(vec![tool_result(3, "a")])
        .await
        .expect("one result appends");
    journal
        .append(vec![Record::Leaf {
            at: timestamp(),
            to: Some(entry_id(1)),
        }])
        .await
        .expect("branch at the user");
    let dangling: Vec<String> = journal
        .records()
        .iter()
        .filter_map(|record| match record {
            Record::ToolResult(entry) => match &entry.kind {
                EntryKind::ToolResult { call, .. } if call.as_str() == "b" => {
                    Some(call.as_str().to_owned())
                }
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert!(
        dangling.is_empty(),
        "the journal has no synthetic result record"
    );
    assert_eq!(
        dal_store::MISSING_ON_BRANCH,
        "This tool call has no result on this branch.",
        "the fold supplies the exact projection text"
    );
    journal.close().await.expect("session closes");
}

#[tokio::test]
async fn compaction_boundary_by_leaf_position() {
    let (_temp, store) = setup("life-compact");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    for index in 1u64..=60 {
        journal
            .append(vec![user(index, &format!("entry {index}"))])
            .await
            .expect("entry appends");
    }
    journal
        .append(vec![Record::Compaction(Entry {
            id: entry_id(61),
            parent: None,
            at: timestamp(),
            kind: EntryKind::Compaction {
                summary: Some("cut".into()),
                first_kept: Some(entry_id(51)),
                tokens_before: 600,
                replay: None,
                usage: None,
                parts: Vec::new(),
                parts_tokens: 0,
            },
        })])
        .await
        .expect("compaction with first_kept 51 appends");
    for index in 62u64..=64 {
        journal
            .append(vec![user(index, &format!("entry {index}"))])
            .await
            .expect("post-compaction entry appends");
    }
    journal.close().await.expect("session closes");
    let (mut reopened, _) = store.open_session(id).await.expect("session reopens");
    let compactions: Vec<EntryId> = reopened
        .records()
        .iter()
        .filter_map(|record| match record {
            Record::Compaction(entry) => Some(entry.id),
            _ => None,
        })
        .collect();
    assert_eq!(
        compactions,
        vec![entry_id(61)],
        "context begins at compaction"
    );
    reopened
        .append(vec![Record::Leaf {
            at: timestamp(),
            to: Some(entry_id(40)),
        }])
        .await
        .expect("leaf moves to 40");
    for want in [1u64, 40, 60, 64] {
        assert!(
            reopened.records().iter().any(|record| match record {
                Record::User(entry) => entry.id.get() == want,
                _ => false,
            }),
            "moving leaf to 40 restores the full uncompacted path"
        );
    }
    reopened.close().await.expect("session closes");
}

#[tokio::test]
async fn parts_compaction_roundtrips_through_store_and_replay() {
    let (_temp, store) = setup("life-parts-replay");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    let image = b"compacted image bytes".to_vec();
    journal
        .append(vec![user(1, "original source")])
        .await
        .expect("first user materializes the session");
    let blob = journal
        .put_blob(image.clone())
        .expect("publish replacement image");
    journal
        .append(vec![
            Record::TurnStart {
                at: timestamp(),
                turn: TurnId::new(NonZeroU64::MIN),
            },
            Record::Compaction(Entry {
                id: entry_id(2),
                parent: Some(entry_id(1)),
                at: timestamp(),
                kind: EntryKind::Compaction {
                    summary: None,
                    first_kept: Some(entry_id(1)),
                    tokens_before: 40,
                    replay: None,
                    usage: Some(usage()),
                    parts: vec![
                        JournalPart::Text {
                            text: "history image".into(),
                        },
                        JournalPart::ImageBlob {
                            mime: "image/png".into(),
                            blob: blob.to_string().into(),
                            bytes: u64::try_from(image.len()).expect("image length fits"),
                        },
                    ],
                    parts_tokens: 9,
                },
            }),
            Record::Ext {
                at: timestamp(),
                ext: "history".into(),
                kind: "letter".into(),
                body: RawJson::parse(
                    r#"{"kind":"compaction","id":"history/1.1","spans":[[1,0,0,15]]}"#,
                )
                .expect("letter record parses"),
            },
        ])
        .await
        .expect("append compaction and extension record batch");
    let (before_fold, before_effects) =
        dal_core::Session::replay(journal.records().to_vec(), timestamp())
            .expect("fold initial journal");
    journal.close().await.expect("close journal");
    drop(journal);

    let (mut reopened, _) = store.open_session(id).await.expect("reopen journal");
    let compaction = reopened
        .records()
        .iter()
        .find_map(|record| match record {
            Record::Compaction(entry) => Some(entry),
            _ => None,
        })
        .expect("parts compaction is decoded");
    assert!(matches!(
        &compaction.kind,
        EntryKind::Compaction { parts, parts_tokens: 9, .. }
            if matches!(parts.as_slice(), [JournalPart::Text { text }, JournalPart::ImageBlob { blob: found, .. }]
                if text.as_ref() == "history image" && found.as_ref() == blob.to_string())
    ));
    assert_eq!(
        reopened.read_blob(blob).expect("read replayed image"),
        image
    );
    let (after_fold, _) = dal_core::Session::replay(reopened.records().to_vec(), timestamp())
        .expect("fold reopened journal");
    assert_eq!(before_fold.leaf_entry(), after_fold.leaf_entry());
    assert_eq!(
        before_fold
            .leaf_entries()
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        after_fold
            .leaf_entries()
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>()
    );
    let turn_end_usage = |effects: &[dal_core::Effect]| {
        effects.iter().find_map(|effect| match effect {
            dal_core::Effect::Emit(emit) => emit.records.iter().find_map(|record| match record {
                Record::TurnEnd { usage, .. } => *usage,
                _ => None,
            }),
            _ => None,
        })
    };
    assert_eq!(turn_end_usage(&before_effects), Some(usage()));
    assert_eq!(
        reopened.records().iter().find_map(|record| match record {
            Record::TurnEnd { usage, .. } => *usage,
            _ => None,
        }),
        Some(usage())
    );
    let ext_records: Vec<_> = after_fold
        .leaf_ext()
        .map(|record| {
            (
                record.ext.to_owned(),
                record.kind.to_owned(),
                record.body.as_str().to_owned(),
            )
        })
        .collect();
    assert_eq!(ext_records.len(), 1);
    assert_eq!(ext_records[0].0, "history");
    assert_eq!(ext_records[0].1, "letter");
    assert!(ext_records[0].2.contains("history/1.1"));
    reopened.close().await.expect("close reopened journal");
}

#[tokio::test]
async fn compaction_image_parts_spill_and_survive_reopen() {
    let (_temp, store) = setup("life-parts-spill");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "source")])
        .await
        .expect("source appends");
    let groups = 16_384_usize.div_ceil(3);
    let expected = [1_u8, 2, 3].repeat(groups);
    let base64 = "AQID".repeat(groups);
    let byte_count = u64::try_from(expected.len()).expect("image length fits");
    journal
        .append(vec![Record::Compaction(Entry {
            id: entry_id(2),
            parent: Some(entry_id(1)),
            at: timestamp(),
            kind: EntryKind::Compaction {
                summary: None,
                first_kept: Some(entry_id(1)),
                tokens_before: 100,
                replay: None,
                usage: None,
                parts: vec![JournalPart::Image {
                    mime: "image/png".into(),
                    base64: base64.into(),
                }],
                parts_tokens: 20,
            },
        })])
        .await
        .expect("append image compaction");
    let blob = journal
        .records()
        .iter()
        .find_map(|record| match record {
            Record::Compaction(entry) => match &entry.kind {
                EntryKind::Compaction { parts, .. } => match parts.as_slice() {
                    [JournalPart::ImageBlob { blob, bytes, .. }] if *bytes == byte_count => {
                        BlobId::parse(blob).ok()
                    }
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        })
        .expect("large inline image is spilled to a content-addressed blob");
    assert_eq!(
        journal.read_blob(blob).expect("read spilled image"),
        expected
    );
    journal.close().await.expect("close journal");
    drop(journal);

    let (mut reopened, _) = store.open_session(id).await.expect("reopen journal");
    assert_eq!(
        reopened.read_blob(blob).expect("read replayed image"),
        expected
    );
    assert!(reopened.records().iter().any(|record| matches!(
        record,
        Record::Compaction(Entry {
            kind: EntryKind::Compaction {
                parts,
                parts_tokens: 20,
                ..
            },
            ..
        }) if matches!(parts.as_slice(), [JournalPart::ImageBlob { blob: found, bytes, .. }]
            if found.as_ref() == blob.to_string() && *bytes == byte_count)
    )));
    reopened.close().await.expect("close reopened journal");
}

#[tokio::test]
async fn remote_compaction_replay_roundtrip() {
    let (_temp, store) = setup("life-replay");
    let id = SessionId::new_v7();
    let replay = RawJson::parse("{\"items\":[{\"op\":\"add\",\"text\":\"kept\"}]}")
        .expect("replay payload parses");
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "before")])
        .await
        .expect("user appends");
    journal
        .append(vec![Record::Compaction(Entry {
            id: entry_id(2),
            parent: None,
            at: timestamp(),
            kind: EntryKind::Compaction {
                summary: None,
                first_kept: Some(entry_id(1)),
                tokens_before: 10,
                replay: Some(replay),
                usage: None,
                parts: Vec::new(),
                parts_tokens: 0,
            },
        })])
        .await
        .expect("remote compaction persists");
    let before: Vec<Vec<u8>> = journal
        .records()
        .iter()
        .map(|record| encode(record).expect("record encodes"))
        .collect();
    journal.close().await.expect("session closes");
    let (reopened, _) = store.open_session(id).await.expect("session reopens");
    let after: Vec<Vec<u8>> = reopened
        .records()
        .iter()
        .take(before.len())
        .map(|record| encode(record).expect("record encodes"))
        .collect();
    assert_eq!(before, after, "replay items survive byte-for-byte");
    let kept = reopened
        .records()
        .iter()
        .find_map(|record| match record {
            Record::Compaction(entry) => match &entry.kind {
                EntryKind::Compaction { replay, .. } => replay.clone(),
                _ => None,
            },
            _ => None,
        })
        .expect("compaction replays");
    assert_eq!(
        kept.as_str(),
        "{\"items\":[{\"op\":\"add\",\"text\":\"kept\"}]}",
        "replay payload is preserved"
    );
}

#[tokio::test]
async fn label_removal_appends_record() {
    let (_temp, store) = setup("life-label");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "task")])
        .await
        .expect("user appends");
    for label in [Some("todo"), None] {
        journal
            .append(vec![Record::Label {
                at: timestamp(),
                entry: entry_id(1),
                label: label.map(Into::into),
            }])
            .await
            .expect("label change appends");
    }
    journal.close().await.expect("session closes");
    let (reopened, _) = store.open_session(id).await.expect("session reopens");
    let labels: Vec<Option<String>> = reopened
        .records()
        .iter()
        .filter_map(|record| match record {
            Record::Label { entry, label, .. } if entry == &entry_id(1) => {
                Some(label.as_deref().map(str::to_owned))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        labels,
        vec![Some("todo".to_owned()), None],
        "exactly two label records exist"
    );
    assert!(
        labels.last().expect("a label exists").is_none(),
        "the current label is absent"
    );
}

#[tokio::test]
async fn cancelled_message_rejects_tool_call() {
    let (_temp, store) = setup("life-cancelled");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "go")])
        .await
        .expect("user appends");
    let before = journal.records().len();
    let input = RawJson::parse("{}").expect("empty object parses");
    let error = journal
        .append(vec![Record::Assistant(Entry {
            id: entry_id(2),
            parent: None,
            at: timestamp(),
            kind: EntryKind::Assistant {
                api: Family::Chat,
                model: "test-model".into(),
                content: vec![Block::ToolCall {
                    id: CallId::new("x"),
                    name: "read".into(),
                    input,
                }],
                usage: usage(),
                stop: AssistantStop::Cancelled,
            },
        })])
        .await
        .expect_err("cancelled tool call is rejected");
    assert!(
        matches!(error, StoreError::Invalid { .. }),
        "cancelled message is Invalid"
    );
    assert_eq!(
        error.to_string(),
        "a cancelled assistant message holds a tool call or reasoning",
        "exact rejection text"
    );
    assert_eq!(journal.records().len(), before, "nothing is written");
}

#[tokio::test]
async fn ephemeral_session_leaves_no_trace() {
    let (temp, store) = setup("life-ephemeral");
    let before: Vec<std::path::PathBuf> = fs::read_dir(temp.path())
        .expect("list data root")
        .map(|entry| entry.expect("read entry").path())
        .collect();
    let id = SessionId::new_v7();
    let mut journal = store.ephemeral_session(id);
    let image = "a".repeat(1_000_000);
    for index in 1u64..=100 {
        journal
            .append(vec![Record::User(Entry {
                id: entry_id(index),
                parent: None,
                at: timestamp(),
                kind: EntryKind::User {
                    parts: vec![JournalPart::Image {
                        mime: "image/png".into(),
                        base64: image.clone().into(),
                    }],
                },
            })])
            .await
            .expect("ephemeral append succeeds");
    }
    let after: Vec<std::path::PathBuf> = fs::read_dir(temp.path())
        .expect("relist data root")
        .map(|entry| entry.expect("read entry").path())
        .collect();
    assert_eq!(before, after, "data-root listing is unchanged");
    assert_eq!(journal.generation().get(), 1, "generation remains 1");
    assert_eq!(
        journal.records().len(),
        102,
        "paging over records stays available"
    );
    assert!(
        journal.sidecar().is_none(),
        "no sidecar handle exists for memory sessions"
    );
}

#[tokio::test]
async fn fork_and_clone_edge_matrix() {
    let (temp, store) = setup("life-fork");
    let data_root = temp.path().join("data");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "first")])
        .await
        .expect("first appends");
    journal
        .append(vec![user(2, "second")])
        .await
        .expect("second appends");
    journal
        .append(vec![user(3, "third")])
        .await
        .expect("third appends");
    let unknown = journal
        .fork(entry_id(999))
        .await
        .expect_err("unknown anchor fails");
    assert_eq!(
        unknown.to_string(),
        format!("session {id} has no entry 999"),
        "exact UnknownEntry text"
    );
    journal
        .append(vec![assistant_with_calls(4, &["q"])])
        .await
        .expect("assistant appends");
    let not_user = journal
        .fork(entry_id(4))
        .await
        .expect_err("non-user anchor fails");
    assert_eq!(
        not_user.to_string(),
        "entry 4 is not a user message; /fork starts from a user message",
        "exact NotUserMessage text"
    );
    let (mut forked, text) = journal.fork(entry_id(2)).await.expect("fork succeeds");
    let forked_ids: Vec<u64> = forked
        .records()
        .iter()
        .filter_map(|record| match record {
            Record::User(entry) => Some(entry.id.get()),
            _ => None,
        })
        .collect();
    assert_eq!(forked_ids, vec![1], "copied path ends at the anchor parent");
    assert_eq!(text, "second", "fork returns the anchor text");
    let forked_id = forked.id();
    forked.close().await.expect("forked session closes");
    journal.close().await.expect("source closes");

    let mut early = store.create_session(SessionId::new_v7());
    early
        .append(vec![user(1, "lone")])
        .await
        .expect("lone user appends");
    let (lazy, lazy_text) = early
        .fork(entry_id(1))
        .await
        .expect("first-user fork works");
    assert_eq!(lazy_text, "lone", "first-user fork returns its text");
    let lazy_dir = data_root.join("sessions").join(lazy.id().to_string());
    assert!(
        !lazy_dir.exists() || fs::read_dir(&lazy_dir).is_err(),
        "first-user fork stays lazy"
    );
    early.close().await.expect("early session closes");

    let mut labeled = store.create_session(SessionId::new_v7());
    labeled
        .append(vec![user(1, "labeled root")])
        .await
        .expect("root appends");
    let big = "p".repeat(20_000);
    labeled
        .append(vec![user(2, &big)])
        .await
        .expect("image-sized text appends");
    labeled
        .append(vec![Record::Label {
            at: timestamp(),
            entry: entry_id(2),
            label: Some("keep".into()),
        }])
        .await
        .expect("label appends");
    labeled
        .append(vec![Record::Model(Entry {
            id: entry_id(3),
            parent: None,
            at: timestamp(),
            kind: EntryKind::Model {
                route: dal_core::ModelRoute::Api {
                    family: Family::Chat,
                    model: "m".into(),
                },
            },
        })])
        .await
        .expect("setting appends");
    let source_id = labeled.id();
    let mut cloned = labeled.clone_session().await.expect("clone succeeds");
    let header = match &cloned.records()[0] {
        Record::Session(header) => header.clone(),
        _ => panic!("clone starts with a session header"),
    };
    let from = header.from.expect("clone carries a from header");
    assert_eq!(from.session, source_id, "clone names its source");
    assert!(
        cloned.records().iter().any(|record| matches!(
            record,
            Record::Label { label: Some(label), .. } if label.as_ref() == "keep"
        )),
        "labels survive the clone"
    );
    assert!(
        cloned
            .records()
            .iter()
            .any(|record| matches!(record, Record::Model(_))),
        "settings survive the clone"
    );
    let digest = cloned
        .records()
        .iter()
        .find_map(|record| match record {
            Record::User(entry) => match &entry.kind {
                EntryKind::User { parts } => match &parts[..] {
                    [JournalPart::TextBlob { blob, .. }] => Some(blob.clone()),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        })
        .expect("cloned blob part exists");
    assert_eq!(
        cloned
            .read_blob(dal_core::BlobId::parse(&digest).expect("digest parses"))
            .expect("cloned blob reads"),
        big.as_bytes(),
        "clone shares blob bytes"
    );
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        let blob_name = dal_core::BlobId::parse(&digest)
            .expect("digest parses")
            .to_string();
        let mut ino = None;
        for root in [data_root.join("sessions")] {
            let mut stack = vec![root];
            while let Some(dir) = stack.pop() {
                for entry in fs::read_dir(&dir).expect("list sessions") {
                    let path = entry.expect("read entry").path();
                    if path.is_dir() {
                        let candidate = path.join("blobs").join(&blob_name);
                        if candidate.is_file() {
                            ino.get_or_insert_with(Vec::new)
                                .push(fs::metadata(&candidate).expect("stat blob").ino());
                        }
                        stack.push(path);
                    }
                }
            }
        }
        let ino = ino.expect("blob inodes found");
        assert!(
            ino.len() >= 2 && ino.iter().all(|first| first == &ino[0]),
            "Linux shares the blob inode"
        );
    }
    cloned.close().await.expect("clone closes");
    labeled.close().await.expect("source closes");
    let _ = forked_id;

    let mut empty = store.create_session(SessionId::new_v7());
    let empty_id = empty.id();
    let nothing = empty.clone_session().await.expect_err("empty clone fails");
    assert_eq!(
        nothing.to_string(),
        format!("session {empty_id} has no entries to clone"),
        "exact NothingToClone text"
    );
}

#[tokio::test]
async fn mail_cursor_reads_without_consuming() {
    let (_temp, store) = setup("life-mail-cursor");
    let id = SessionId::new_v7();
    let from = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "start")])
        .await
        .expect("user appends");
    for (index, text) in ["one", "two"].into_iter().enumerate() {
        journal
            .append(vec![Record::Mail(dal_core::Mail {
                at: timestamp(),
                from,
                to: id,
                mode: dal_core::MailMode::NextTurn,
                text: text.into(),
                reply_to: None,
            })])
            .await
            .expect("mail appends");
        let _ = index;
    }
    journal.close().await.expect("session closes");
    let read_after = |records: &[Record], cursor: u64| -> (Vec<String>, u64) {
        let mut scanned = 0u64;
        let mut out = Vec::new();
        for record in records {
            let line = encode(record).expect("record encodes");
            let end = scanned + u64::try_from(line.len()).expect("line fits memory");
            if end > cursor {
                if let Record::Mail(mail) = record {
                    out.push(mail.text.to_string());
                }
            }
            scanned = end;
        }
        (out, scanned)
    };
    let (reopened, _) = store.open_session(id).await.expect("session reopens");
    let (first, next) = read_after(reopened.records(), 0);
    let (second, again) = read_after(reopened.records(), 0);
    assert_eq!(first, vec!["one", "two"], "mail reads after the cursor");
    assert_eq!(second, first, "a second read returns the same records");
    assert_eq!(again, next, "and the same next cursor");
    let (partial, _) = read_after(reopened.records(), next);
    assert!(partial.is_empty(), "reading past the end returns nothing");
}

#[tokio::test]
async fn atomic_new_preserves_existing_target() {
    let temp = TempDir::new("life-atomic");
    let path = temp.path().join("winner.json");
    let first = "f".repeat(1_000_000);
    let second = "s".repeat(1_000_000);
    let path_a = path.clone();
    let path_b = path.clone();
    let winner_a = first.clone();
    let winner_b = second.clone();
    let handle_a = std::thread::spawn(move || {
        write_atomic_new(&path_a, winner_a.as_bytes(), FileMode::Mode0600).is_ok()
    });
    let handle_b = std::thread::spawn(move || {
        write_atomic_new(&path_b, winner_b.as_bytes(), FileMode::Mode0600).is_ok()
    });
    let won_a = handle_a.join().expect("writer a joins");
    let won_b = handle_b.join().expect("writer b joins");
    assert!(won_a ^ won_b, "exactly one racer succeeds");
    let visible = fs::read(&path).expect("winner bytes are visible");
    assert_eq!(
        visible.len(),
        1_000_000,
        "the winner's complete bytes are visible"
    );
    assert!(
        visible == first.as_bytes() || visible == second.as_bytes(),
        "the visible bytes are one winner"
    );
    let keep = b"existing";
    fs::write(&path, keep).expect("stage an existing target");
    let retry_a = write_atomic_new(&path, first.as_bytes(), FileMode::Mode0600);
    let retry_b = write_atomic_new(&path, second.as_bytes(), FileMode::Mode0600);
    assert!(
        retry_a.is_err() && retry_b.is_err(),
        "no racer replaces the target"
    );
    assert_eq!(
        fs::read(&path).expect("target reread"),
        keep,
        "the target is never replaced"
    );
}
