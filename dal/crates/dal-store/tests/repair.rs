#![expect(clippy::expect_used, reason = "integration tests fail loudly")]
#![expect(clippy::panic, reason = "integration tests fail loudly")]
#![expect(clippy::collapsible_if, reason = "integration tests fail loudly")]
#![expect(clippy::too_many_lines, reason = "integration tests fail loudly")]
//! Open-time repair: torn tails, aborted turns, damage, and fault states.

mod support;

use std::{fs, num::NonZeroU64, path::PathBuf};

use dal_core::{
    Block, CallId, ClientId, Entry, EntryId, EntryKind, Family, Gen, Header, JobEvent, JobId,
    JournalPart, Product, RawJson, Record, SessionId, TurnEndStop, TurnId, Usage, Workspace,
    encode,
};
use dal_store::{INTERRUPTED_CALL, NOT_RUN_CALL, Store, StoreError};
use support::{fault_sink::FaultSink, temp_dir::TempDir};

fn entry_id(value: u64) -> EntryId {
    EntryId::new(NonZeroU64::new(value).expect("nonzero test entry id"))
}

fn turn_id(value: u64) -> TurnId {
    TurnId::new(NonZeroU64::new(value).expect("nonzero test turn id"))
}

fn timestamp() -> jiff::Timestamp {
    jiff::Timestamp::now()
}

fn fixed_timestamp() -> jiff::Timestamp {
    "2026-09-28T10:15:30.123Z"
        .parse()
        .expect("fixed test timestamp parses")
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
            usage: Usage {
                input_tokens: 0,
                cached_input_tokens: 0,
                output_tokens: 0,
                reasoning_tokens: None,
                cache_write_tokens: 0,
                cost_usd: None,
            },
            stop: dal_core::AssistantStop::ToolUse,
        },
    })
}

fn tool_result(id: u64, call: &str, text: &str) -> Record {
    Record::ToolResult(Entry {
        id: entry_id(id),
        parent: None,
        at: timestamp(),
        kind: EntryKind::ToolResult {
            call: CallId::new(call),
            name: "read".into(),
            error: false,
            parts: vec![JournalPart::Text { text: text.into() }],
            changes: vec![],
        },
    })
}

fn setup(tag: &str) -> (TempDir, Store, Workspace) {
    let temp = TempDir::new(tag);
    let workspace =
        Workspace::new(temp.path().join("workspace")).expect("temporary workspace is absolute");
    let store = Store::new(
        temp.path().join("data"),
        workspace.clone(),
        Product::Dalgona,
    );
    (temp, store, workspace)
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

fn complete_turn(turn: u64) -> Vec<Record> {
    vec![
        Record::TurnStart {
            at: timestamp(),
            turn: turn_id(turn),
        },
        Record::TurnEnd {
            at: timestamp(),
            turn: turn_id(turn),
            stop: TurnEndStop::Done,
            usage: None,
            changes: vec![],
        },
    ]
}

#[tokio::test]
async fn torn_tail_quarantined_on_open() {
    let (temp, store, _) = setup("repair-torn");
    let data_root = temp.path().join("data");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "durable prefix")])
        .await
        .expect("prefix is durable");
    journal.close().await.expect("session closes");
    let path = journal_path(&data_root, id);
    let prefix = fs::read(&path).expect("read durable prefix");
    let tail = b"{\"v\":1,\"type\":\"us";
    let mut staged = prefix.clone();
    staged.extend_from_slice(tail);
    fs::write(&path, &staged).expect("stage a torn tail");
    let (mut reopened, report) = store.open_session(id).await.expect("open repairs");
    let torn = report.torn.expect("torn tail is reported");
    assert_eq!(
        usize::try_from(torn.offset).expect("offset fits memory"),
        prefix.len(),
        "exact torn offset"
    );
    assert_eq!(
        usize::try_from(torn.bytes).expect("size fits memory"),
        tail.len(),
        "exact torn byte count"
    );
    assert_eq!(
        fs::read(torn.kept_at.as_path()).expect("read side file"),
        tail,
        "the tail is preserved verbatim"
    );
    assert!(
        torn.kept_at
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("torn-")),
        "tail moves to a torn side file"
    );
    let repaired = fs::read(&path).expect("read repaired journal");
    assert_eq!(
        repaired[..prefix.len()],
        prefix[..],
        "truncate happens only after durable quarantine"
    );
    reopened.close().await.expect("session closes");
    let (_, second) = store.open_session(id).await.expect("second open succeeds");
    assert!(
        second.torn.is_none(),
        "no second tail is reported on the next open"
    );
}

#[tokio::test]
async fn long_torn_tail_single_side_file() {
    let (temp, store, _) = setup("repair-long-tail");
    let data_root = temp.path().join("data");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "prefix")])
        .await
        .expect("prefix is durable");
    journal.close().await.expect("session closes");
    let path = journal_path(&data_root, id);
    let mut staged = fs::read(&path).expect("read durable prefix");
    let anchor = encode(&user(50, "anchor")).expect("anchor encodes");
    staged.extend_from_slice(&anchor);
    let tail = vec![b'x'; 70_000];
    staged.extend_from_slice(&tail);
    fs::write(&path, &staged).expect("stage a long torn tail");
    let (_, report) = store.open_session(id).await.expect("open repairs");
    let torn = report.torn.expect("long tail is reported");
    assert_eq!(torn.bytes, 70_000, "the complete tail moves as one");
    assert_eq!(
        fs::read(torn.kept_at.as_path())
            .expect("read side file")
            .len(),
        70_000,
        "one side file holds the whole tail"
    );
    let side_files: Vec<PathBuf> = fs::read_dir(path.parent().expect("session dir"))
        .expect("list session dir")
        .map(|entry| entry.expect("read entry").path())
        .filter(|candidate| {
            candidate
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("torn-"))
        })
        .collect();
    assert_eq!(side_files.len(), 1, "a single side file is written");
}

#[tokio::test]
async fn aborted_turn_repair_on_reopen() {
    let (temp, store, _) = setup("repair-aborted");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "run the tools")])
        .await
        .expect("prefix is durable");
    for turn in [1, 2] {
        journal
            .append(complete_turn(turn))
            .await
            .expect("earlier turns settle");
    }
    journal
        .append(vec![
            Record::TurnStart {
                at: timestamp(),
                turn: turn_id(3),
            },
            assistant_with_calls(2, &["a", "b", "c"]),
        ])
        .await
        .expect("open turn 3");
    journal
        .append(vec![
            Record::ToolStart {
                at: timestamp(),
                turn: turn_id(3),
                call: CallId::new("a"),
            },
            tool_result(3, "a", "a is done"),
        ])
        .await
        .expect("resolve call a");
    journal.close().await.expect("stop with turn 3 open");
    let (mut reopened, report) = store.open_session(id).await.expect("open repairs");
    let aborted = report.aborted.expect("aborted turn is reported");
    assert_eq!(aborted.turn, turn_id(3), "recovery names turn 3");
    assert_eq!(
        aborted.interrupted, 0,
        "no call had started without a result"
    );
    assert_eq!(aborted.not_run, 2, "two calls never started");
    assert_eq!(
        aborted.notice(),
        "Turn 3 did not finish because dalgon stopped. dalgon marked 2 unfinished tool calls.",
        "exact aborted notice"
    );
    let mut recovered = 0;
    let mut kept_real = false;
    for record in reopened.records() {
        if let Record::ToolResult(entry) = record {
            if let EntryKind::ToolResult { parts, .. } = &entry.kind {
                for part in parts {
                    if let JournalPart::Text { text } = part {
                        if text.as_ref() == NOT_RUN_CALL {
                            recovered += 1;
                        }
                        kept_real |= text.as_ref() == "a is done";
                    }
                }
            }
        }
    }
    assert_eq!(recovered, 2, "recovery results for b and c in one batch");
    assert!(kept_real, "the real result for a is kept");
    reopened.close().await.expect("session closes");

    let id_b = SessionId::new_v7();
    let mut second = store.create_session(id_b);
    second
        .append(vec![user(1, "run the tools")])
        .await
        .expect("prefix is durable");
    for turn in [1, 2] {
        second
            .append(complete_turn(turn))
            .await
            .expect("earlier turns settle");
    }
    second
        .append(vec![
            Record::TurnStart {
                at: timestamp(),
                turn: turn_id(3),
            },
            assistant_with_calls(2, &["a", "b", "c"]),
        ])
        .await
        .expect("open turn 3");
    second
        .append(vec![
            tool_result(3, "a", "a is done"),
            Record::ToolStart {
                at: timestamp(),
                turn: turn_id(3),
                call: CallId::new("b"),
            },
        ])
        .await
        .expect("start call b without a result");
    second.close().await.expect("stop with turn 3 open");
    let (mut repaired, report_b) = store.open_session(id_b).await.expect("open repairs");
    let aborted_b = report_b.aborted.expect("aborted turn is reported");
    assert_eq!(aborted_b.turn, turn_id(3), "recovery names turn 3");
    assert_eq!(aborted_b.interrupted, 1, "call b started but has no result");
    assert_eq!(aborted_b.not_run, 1, "call c never started");
    let mut interrupted = false;
    let mut not_run = false;
    for record in repaired.records() {
        if let Record::ToolResult(entry) = record {
            if let EntryKind::ToolResult { parts, .. } = &entry.kind {
                for part in parts {
                    if let JournalPart::Text { text } = part {
                        interrupted |= text.as_ref() == INTERRUPTED_CALL;
                        not_run |= text.as_ref() == NOT_RUN_CALL;
                    }
                }
            }
        }
    }
    assert!(interrupted, "started call b gets the interrupted text");
    assert!(not_run, "unstarted call c gets the not-run text");
    repaired.close().await.expect("session closes");
    let _ = temp;
}

#[tokio::test]
async fn corrupt_interior_line_damaged() {
    let (temp, store, workspace) = setup("repair-corrupt");
    let data_root = temp.path().join("data");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "materialize")])
        .await
        .expect("materialize the session dir");
    journal.close().await.expect("session closes");
    let at = fixed_timestamp();
    let session_line = encode(&Record::Session(Header {
        id,
        at,
        workspace: workspace.clone(),
        product: Product::Dalgona,
        from: None,
    }))
    .expect("session encodes");
    let boot_line = encode(&Record::Boot {
        at,
        r#gen: Gen::new(NonZeroU64::new(1).expect("nonzero gen")),
        version: "0.1.0".into(),
    })
    .expect("boot encodes");
    let user_template = encode(&Record::User(Entry {
        id: entry_id(1),
        parent: None,
        at,
        kind: EntryKind::User {
            parts: vec![JournalPart::Text { text: "".into() }],
        },
    }))
    .expect("template encodes");
    let pad = 812usize.saturating_sub(session_line.len() + boot_line.len() + user_template.len());
    let user_line = encode(&Record::User(Entry {
        id: entry_id(1),
        parent: None,
        at,
        kind: EntryKind::User {
            parts: vec![JournalPart::Text {
                text: "p".repeat(pad).into(),
            }],
        },
    }))
    .expect("padded user encodes");
    let mut file = Vec::new();
    file.extend_from_slice(&session_line);
    file.extend_from_slice(&boot_line);
    file.extend_from_slice(&user_line);
    assert_eq!(file.len(), 812, "the corrupt line starts at byte 812");
    file.extend_from_slice(b"{\"v\":1,\"type\":\"nope\"}\n");
    let path = journal_path(&data_root, id);
    fs::write(&path, &file).expect("stage a corrupt interior line");
    let before = fs::read(&path).expect("read staged journal");
    let error = store.open_session(id).await.expect_err("open refuses");
    match &error {
        StoreError::Damaged { offset, reason, .. } => {
            assert_eq!(*offset, 812, "damage is at byte 812");
            assert_eq!(
                reason.as_ref(),
                "unknown record type \"nope\"",
                "decode reason"
            );
        }
        _ => panic!("expected Damaged, got {error:?}"),
    }
    assert_eq!(
        fs::read(&path).expect("reread journal"),
        before,
        "all file bytes remain unchanged"
    );
}

#[tokio::test]
async fn repair_fault_matrix() {
    let (temp, store, _) = setup("repair-matrix");
    let data_root = temp.path().join("data");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "matrix prefix")])
        .await
        .expect("prefix is durable");
    journal.close().await.expect("session closes");
    let path = journal_path(&data_root, id);
    let prefix = fs::read(&path).expect("read durable prefix");

    let sink = FaultSink {
        write_after_bytes: Some(3),
        ..Default::default()
    };
    sink.append_partial(&path, b"{\"v\":1,")
        .expect_err("write failpoint fires");
    let (mut reopened, report) = store.open_session(id).await.expect("write fault repairs");
    let torn = report.torn.expect("torn tail is quarantined");
    assert_eq!(
        usize::try_from(torn.offset).expect("offset fits memory"),
        prefix.len(),
        "quarantine offset"
    );
    assert_eq!(
        fs::read(torn.kept_at.as_path()).expect("read side file"),
        b"{\"v",
        "quarantine write is durable"
    );
    let repaired = fs::read(&path).expect("read repaired journal");
    assert_eq!(
        repaired[..prefix.len()],
        prefix[..],
        "truncate removes the tail"
    );
    reopened.close().await.expect("session closes");
    let (_, clean) = store.open_session(id).await.expect("healthy again");
    assert!(clean.torn.is_none(), "no false healthy state");

    let quarantine_sink = FaultSink {
        quarantine_error: true,
        ..Default::default()
    };
    let quarantine_error = quarantine_sink
        .quarantine(&path, 0, b"x")
        .expect_err("quarantine failpoint fires");
    assert_eq!(
        quarantine_error.to_string(),
        "injected fault: quarantine",
        "mapped quarantine error"
    );
    let dir_sink = FaultSink {
        directory_sync_error: true,
        ..Default::default()
    };
    let dir_error = dir_sink
        .sync_dir(&path)
        .expect_err("directory-sync failpoint fires");
    assert_eq!(
        dir_error.to_string(),
        "injected fault: directory sync",
        "mapped directory-sync error"
    );
    let truncate_sink = FaultSink {
        truncate_error: true,
        ..Default::default()
    };
    let truncate_error = truncate_sink
        .truncate(&path, 0)
        .expect_err("truncate failpoint fires");
    assert_eq!(
        truncate_error.to_string(),
        "injected fault: truncate",
        "mapped truncate error"
    );
    let sync_sink = FaultSink {
        sync_error: true,
        ..Default::default()
    };
    let file = fs::File::open(&path).expect("open journal file");
    let sync_error = sync_sink
        .sync_file(&file)
        .expect_err("sync failpoint fires");
    assert_eq!(
        sync_error.to_string(),
        "injected fault: sync",
        "mapped sync error"
    );
    let healthy = FaultSink::healthy();
    healthy.sync_file(&file).expect("healthy sync passes");
    healthy
        .sync_dir(&path)
        .expect("healthy directory sync passes");
}

#[tokio::test]
async fn structure_violation_decode_reasons() {
    let (temp, store, workspace) = setup("repair-structure");
    let data_root = temp.path().join("data");
    let at = fixed_timestamp();
    let header = |id: SessionId| {
        encode(&Record::Session(Header {
            id,
            at,
            workspace: workspace.clone(),
            product: Product::Dalgona,
            from: None,
        }))
        .expect("session encodes")
    };
    let boot = || {
        encode(&Record::Boot {
            at,
            r#gen: Gen::new(NonZeroU64::new(1).expect("nonzero gen")),
            version: "0.1.0".into(),
        })
        .expect("boot encodes")
    };
    let tree_user = |id: u64, parent: Option<EntryId>| {
        encode(&Record::User(Entry {
            id: entry_id(id),
            parent,
            at,
            kind: EntryKind::User {
                parts: vec![JournalPart::Text { text: "t".into() }],
            },
        }))
        .expect("user encodes")
    };
    let turn_start = |turn: u64| {
        encode(&Record::TurnStart {
            at,
            turn: turn_id(turn),
        })
        .expect("turn start encodes")
    };
    let turn_end = |turn: u64| {
        encode(&Record::TurnEnd {
            at,
            turn: turn_id(turn),
            stop: TurnEndStop::Done,
            usage: None,
            changes: vec![],
        })
        .expect("turn end encodes")
    };
    let leaf_to = |entry: u64| {
        encode(&Record::Leaf {
            at,
            to: Some(entry_id(entry)),
        })
        .expect("leaf encodes")
    };
    let seed = SessionId::new_v7();
    let mut materialize = store.create_session(seed);
    materialize
        .append(vec![user(1, "materialize")])
        .await
        .expect("materialize a session dir");
    materialize.close().await.expect("session closes");
    let stage = |id: SessionId, name: &str| -> (Vec<u8>, u64, &'static str) {
        let head = header(id);
        let boot_line = boot();
        let first = tree_user(1, None);
        let base = head.len() + boot_line.len();
        let with_first = base + first.len();
        let at = |len: usize| u64::try_from(len).expect("short fixture offsets fit");
        match name {
            "empty file" => (vec![], 0, "the file has no complete record"),
            "non-header first record" => (first, 0, "the first record is not a session header"),
            "second header" => {
                let offset = at(head.len() + boot_line.len());
                (
                    [head.clone(), boot_line, head].concat(),
                    offset,
                    "a second session header",
                )
            }
            "duplicate id" => {
                let bytes = [
                    head,
                    boot_line,
                    first.clone(),
                    tree_user(1, Some(entry_id(1))),
                ]
                .concat();
                (bytes, at(with_first), "duplicate entry id 1")
            }
            "non-earlier parent" => {
                let bytes = [
                    head,
                    boot_line,
                    first.clone(),
                    tree_user(2, Some(entry_id(9))),
                ]
                .concat();
                (
                    bytes,
                    at(with_first),
                    "record refers to entry 9, which does not exist",
                )
            }
            "missing leaf target" => {
                let bytes = [head, boot_line, first.clone(), leaf_to(999)].concat();
                (
                    bytes,
                    at(with_first),
                    "record refers to entry 999, which does not exist",
                )
            }
            "second open turn" => {
                let start = turn_start(1);
                let bytes = [head, boot_line, first.clone(), start.clone(), turn_start(2)].concat();
                (
                    bytes,
                    at(with_first + start.len()),
                    "turn 2 starts while turn 1 is open",
                )
            }
            _ => {
                let bytes = [head, boot_line, first.clone(), turn_end(1)].concat();
                (bytes, at(with_first), "turn 1 ends but it is not open")
            }
        }
    };
    for name in [
        "empty file",
        "non-header first record",
        "second header",
        "duplicate id",
        "non-earlier parent",
        "missing leaf target",
        "second open turn",
        "unopened turn end",
    ] {
        let id = SessionId::new_v7();
        let mut journal = store.create_session(id);
        journal
            .append(vec![user(1, "materialize")])
            .await
            .expect("materialize the session dir");
        journal.close().await.expect("session closes");
        let path = journal_path(&data_root, id);
        let (bytes, offset, reason) = stage(id, name);
        fs::write(&path, &bytes).expect("stage the violation");
        let before = fs::read(&path).expect("read staged journal");
        let error = store.open_session(id).await.expect_err("open refuses");
        match &error {
            StoreError::Damaged {
                offset: at,
                reason: why,
                ..
            } => {
                assert_eq!(*at, offset, "{name}: exact offset in file order");
                assert_eq!(why.as_ref(), reason, "{name}: exact reason");
            }
            _ => panic!("{name}: expected Damaged, got {error:?}"),
        }
        assert_eq!(
            fs::read(&path).expect("reread journal"),
            before,
            "{name}: the file is not mutated"
        );
    }
}

#[tokio::test]
async fn orphaned_job_and_scoped_grant_end_once() {
    let (temp, store, _) = setup("repair-orphan");
    let id = SessionId::new_v7();
    let job = JobId::parse("0192aa00-0000-7000-8000-000000000009").expect("job parses");
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "start work")])
        .await
        .expect("prefix is durable");
    journal
        .append(vec![
            Record::Job {
                at: timestamp(),
                job,
                event: JobEvent::Started { kind: None },
            },
            Record::ScopedGrant {
                at: timestamp(),
                call: CallId::new("call-9"),
                prefix: vec!["git".into()],
                roots: vec!["/tmp".into()],
                job,
                by: ClientId::new("tester"),
            },
        ])
        .await
        .expect("job starts and grant goes live");
    journal.close().await.expect("stop with a live job");
    let (mut reopened, _) = store.open_session(id).await.expect("open repairs");
    let orphaned = reopened
        .records()
        .iter()
        .filter(|record| {
            matches!(
                record,
                Record::Job {
                    event: JobEvent::Orphaned,
                    ..
                }
            )
        })
        .count();
    let ended: Vec<JobId> = reopened
        .records()
        .iter()
        .filter_map(|record| match record {
            Record::ScopedGrantEnded { job, .. } => Some(*job),
            _ => None,
        })
        .collect();
    assert_eq!(orphaned, 1, "exactly one Orphaned job event");
    assert_eq!(ended, vec![job], "exactly one matching ScopedGrantEnded");
    reopened.close().await.expect("session closes");
    let (second, _) = store.open_session(id).await.expect("second open succeeds");
    let orphaned_again = second
        .records()
        .iter()
        .filter(|record| {
            matches!(
                record,
                Record::Job {
                    event: JobEvent::Orphaned,
                    ..
                }
            )
        })
        .count();
    let ended_again = second
        .records()
        .iter()
        .filter(|record| matches!(record, Record::ScopedGrantEnded { .. }))
        .count();
    assert_eq!(
        orphaned_again, 1,
        "a second reopen appends no Orphaned again"
    );
    assert_eq!(ended_again, 1, "a second reopen appends no grant end again");
    let _ = temp;
}

#[tokio::test]
async fn windows_runner_parity() {
    let (temp, store, _) = setup("repair-parity");
    let data_root = temp.path().join("data");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "parity prefix")])
        .await
        .expect("prefix is durable");
    journal.close().await.expect("session closes");
    let path = journal_path(&data_root, id);
    let prefix = fs::read(&path).expect("read durable prefix");
    let mut staged = prefix.clone();
    staged.extend_from_slice(b"{\"v\":1,\"type\":\"us");
    fs::write(&path, &staged).expect("stage a torn tail");
    let (mut reopened, report) = store.open_session(id).await.expect("open repairs");
    assert!(report.torn.is_some(), "torn-tail repair runs everywhere");
    assert_eq!(
        fs::read(&path).expect("read repaired journal")[..prefix.len()],
        prefix[..],
        "repair keeps the prefix on every platform"
    );
    #[cfg(target_os = "windows")]
    {
        let dir_files: Vec<PathBuf> = fs::read_dir(path.parent().expect("session dir"))
            .expect("list session dir")
            .map(|entry| entry.expect("read entry").path())
            .collect();
        assert!(
            dir_files
                .iter()
                .any(|file| file.extension().is_some_and(|ext| ext == "jsonl")),
            "the journal file is present without directory sync"
        );
    }
    #[cfg(not(target_os = "windows"))]
    {
        let dir_files: Vec<PathBuf> = fs::read_dir(path.parent().expect("session dir"))
            .expect("list session dir")
            .map(|entry| entry.expect("read entry").path())
            .collect();
        assert!(
            dir_files.contains(&path),
            "the synced directory holds the repaired journal"
        );
    }
    reopened.close().await.expect("session closes");
}

async fn staged_session(tag: &str) -> (TempDir, Store, SessionId, PathBuf, Vec<u8>) {
    let (temp, store, _) = setup(tag);
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "prefix")])
        .await
        .expect("prefix is durable");
    journal.close().await.expect("session closes");
    let path = journal_path(&temp.path().join("data"), id);
    let prefix = fs::read(&path).expect("read durable prefix");
    (temp, store, id, path, prefix)
}

fn damage_of(error: &StoreError) -> (u64, &str) {
    match error {
        StoreError::Damaged { offset, reason, .. } => (*offset, reason.as_ref()),
        _ => panic!("expected Damaged, got {error:?}"),
    }
}

#[tokio::test]
async fn invalid_utf8_line_mid_stream_is_damaged_at_its_offset() {
    let (_temp, store, id, path, prefix) = staged_session("repair-utf8").await;
    let mut staged = prefix.clone();
    staged.extend_from_slice(b"{\"v\":1,\"type\":\"user\",\"x\":\"\xff\xfe\"}\n");
    staged.extend_from_slice(&encode(&user(2, "after")).expect("valid line encodes"));
    fs::write(&path, &staged).expect("stage invalid UTF-8");
    let error = store.open_session(id).await.expect_err("open refuses");
    let (offset, reason) = damage_of(&error);
    assert_eq!(offset, u64::try_from(prefix.len()).expect("fits"));
    assert!(
        reason.starts_with("invalid JSON"),
        "decode reason: {reason}"
    );
    assert_eq!(fs::read(&path).expect("reread"), staged, "file unchanged");
}

#[tokio::test]
async fn blank_line_mid_stream_is_damaged_not_skipped() {
    let (_temp, store, id, path, prefix) = staged_session("repair-blank").await;
    let mut staged = prefix.clone();
    staged.push(b'\n');
    staged.extend_from_slice(&encode(&user(2, "after")).expect("valid line encodes"));
    fs::write(&path, &staged).expect("stage a blank line");
    let error = store.open_session(id).await.expect_err("open refuses");
    let (offset, reason) = damage_of(&error);
    assert_eq!(offset, u64::try_from(prefix.len()).expect("fits"));
    assert_eq!(reason, "empty record");
    assert_eq!(fs::read(&path).expect("reread"), staged, "file unchanged");
}

#[tokio::test]
async fn zero_filled_tail_is_quarantined_and_reported() {
    let (_temp, store, id, path, prefix) = staged_session("repair-zero-fill").await;
    let mut staged = prefix.clone();
    staged.extend(std::iter::repeat_n(0_u8, 4096));
    fs::write(&path, &staged).expect("stage a zero-filled tail");
    let (mut journal, report) = store.open_session(id).await.expect("open repairs");
    let torn = report.torn.expect("the zero fill is reported");
    assert_eq!(torn.offset, u64::try_from(prefix.len()).expect("fits"));
    assert_eq!(torn.bytes, 4096);
    assert_eq!(
        fs::read(&torn.kept_at).expect("side file"),
        vec![0_u8; 4096],
        "the quarantined bytes are kept"
    );
    journal.close().await.expect("session closes");
}

#[tokio::test]
async fn record_missing_only_its_newline_is_quarantined_not_replayed() {
    let (_temp, store, id, path, prefix) = staged_session("repair-no-newline").await;
    let mut line = encode(&user(2, "unsynced")).expect("valid line encodes");
    line.pop();
    let mut staged = prefix.clone();
    staged.extend_from_slice(&line);
    fs::write(&path, &staged).expect("stage an unterminated record");
    let (mut journal, report) = store.open_session(id).await.expect("open repairs");
    let torn = report.torn.expect("the unterminated record is reported");
    assert_eq!(torn.bytes, u64::try_from(line.len()).expect("fits"));
    assert_eq!(fs::read(&torn.kept_at).expect("side file"), line);
    assert!(
        journal.records().iter().all(|record| !matches!(
            record,
            Record::User(entry) if entry.id == entry_id(2)
        )),
        "the unacknowledged record is not replayed"
    );
    journal.close().await.expect("session closes");
}

#[tokio::test]
async fn header_cut_mid_record_is_refused_and_left_alone() {
    let (_temp, store, id, path, prefix) = staged_session("repair-header-cut").await;
    let cut = prefix
        .iter()
        .position(|byte| *byte == b'\n')
        .expect("header ends")
        / 2;
    fs::write(&path, &prefix[..cut]).expect("cut the header");
    let error = store.open_session(id).await.expect_err("open refuses");
    let text = error.to_string();
    assert!(
        text.contains(&path.display().to_string()),
        "names the journal: {text}"
    );
    assert_eq!(
        fs::read(&path).expect("reread"),
        &prefix[..cut],
        "file unchanged"
    );
}

#[tokio::test]
async fn repeated_boot_generation_is_damaged_not_accepted() {
    let (_temp, store, id, path, prefix) = staged_session("repair-boot-gen").await;
    let boot = prefix
        .split_inclusive(|byte| *byte == b'\n')
        .nth(1)
        .expect("boot line")
        .to_vec();
    let mut staged = prefix.clone();
    let duplicate_at = u64::try_from(staged.len()).expect("fits");
    staged.extend_from_slice(&boot);
    fs::write(&path, &staged).expect("stage a repeated boot generation");
    let error = store
        .open_session(id)
        .await
        .expect_err("a boot generation that does not increase is refused");
    let (offset, reason) = damage_of(&error);
    assert_eq!(offset, duplicate_at);
    assert!(reason.contains("boot generation"), "reason: {reason}");
    assert_eq!(fs::read(&path).expect("reread"), staged, "file unchanged");
}

#[tokio::test]
async fn boot_generation_gap_reopens_with_the_next_higher_generation() {
    let (_temp, store, id, path, prefix) = staged_session("repair-boot-gap").await;
    let mut staged = prefix.clone();
    staged.extend_from_slice(
        &encode(&Record::Boot {
            at: fixed_timestamp(),
            r#gen: Gen::new(NonZeroU64::new(5).expect("nonzero gen")),
            version: "0.1.0".into(),
        })
        .expect("boot encodes"),
    );
    fs::write(&path, &staged).expect("stage a generation gap");
    let (mut journal, report) = store
        .open_session(id)
        .await
        .expect("a gap between generations is not damage");
    assert_eq!(
        report.r#gen.get(),
        6,
        "open continues after the highest generation"
    );
    journal.close().await.expect("session closes");
    let (mut again, report) = store.open_session(id).await.expect("reopens again");
    assert_eq!(report.r#gen.get(), 7);
    again.close().await.expect("session closes");
}
