#![expect(clippy::expect_used, reason = "integration tests fail loudly")]
#![expect(clippy::panic, reason = "integration tests fail loudly")]
#![expect(clippy::disallowed_methods, reason = "integration tests fail loudly")]
//! Adversarial proof pass over dal-store crash, corruption, and contention seams.
//!
//! Every case stages a real filesystem boundary (torn bytes, a digest path
//! replaced by a directory, a stale lock marker, a killed writer process); no
//! fault-injection hooks are needed. Each test pins the recovery contract for
//! one violation class: torn tails (garbage, huge-garbage, journal-shaped,
//! non-UTF-8), quarantine ordering and crash windows, blob digest-entry
//! corruption and caps, stale-lock takeover and double-acquire, sidecar and
//! append stability across reopen sequences, and a killed concurrent writer
//! whose acknowledged records must survive.

mod support;

use std::{
    fs,
    io::{BufRead, ErrorKind},
    num::NonZeroU64,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use dal_core::{
    BlobId, Entry, EntryId, EntryKind, JournalPart, Product, Record, SessionId, TurnEndStop,
    TurnId, Workspace, encode,
};
use dal_store::{AppendOutcome, BlobError, JournalError, Store, StoreError};
use support::temp_dir::TempDir;

fn entry_id(value: u64) -> EntryId {
    EntryId::new(NonZeroU64::new(value).expect("nonzero test entry id"))
}

fn turn_id(value: u64) -> TurnId {
    TurnId::new(NonZeroU64::new(value).expect("nonzero test turn id"))
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

fn setup(tag: &str) -> (TempDir, Store) {
    let temp = TempDir::new(tag);
    let workspace =
        Workspace::new(temp.path().join("workspace")).expect("temporary workspace is absolute");
    let store = Store::new(temp.path().join("data"), workspace, Product::Dalgona);
    (temp, store)
}

fn session_dir(data_root: &Path, id: SessionId) -> PathBuf {
    let sessions = data_root.join("sessions");
    let name = id.to_string();
    let mut stack = vec![sessions];
    while let Some(dir) = stack.pop() {
        let entries =
            fs::read_dir(&dir).expect("list data-root subtree while locating the session dir");
        for entry in entries {
            let path = entry.expect("read data-root entry").path();
            if path.is_dir() {
                if path.file_name().is_some_and(|base| base == name.as_str()) {
                    return path;
                }
                stack.push(path);
            }
        }
    }
    panic!("session directory exists");
}

fn torn_side_name(prefix_len: usize) -> String {
    format!("torn-{prefix_len}.jsonl")
}

/// Creates a durable one-record session and returns its staged journal bytes.
async fn staged_durable_journal(tag: &str) -> (TempDir, Store, SessionId, PathBuf, Vec<u8>) {
    let (temp, store) = setup(tag);
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "durable prefix")])
        .await
        .expect("prefix is durable");
    journal.close().await.expect("session closes");
    let data_root = temp.path().join("data");
    let path = session_dir(&data_root, id).join("journal.jsonl");
    let prefix = fs::read(&path).expect("read durable prefix");
    (temp, store, id, path, prefix)
}

#[tokio::test]
async fn complete_garbage_line_is_refused_and_never_silently_accepted() {
    let (_temp, store, id, path, prefix) = staged_durable_journal("adv-garbage-line").await;
    // A garbage tail that ends in a newline forms a "complete" record line.
    // The decode boundary must refuse it; quarantine is only for torn tails.
    let mut staged = prefix.clone();
    staged.extend_from_slice(b"garbage-not-a-record\n");
    fs::write(&path, &staged).expect("stage a newline-terminated garbage line");
    let error = store
        .open_session(id)
        .await
        .expect_err("a complete garbage line is refused");
    match &error {
        StoreError::Damaged { offset, reason, .. } => {
            assert_eq!(
                *offset,
                u64::try_from(prefix.len()).expect("prefix fits u64"),
                "the refusal names the garbage line's start"
            );
            assert!(
                reason.contains("no format version")
                    || reason.contains("invalid JSON")
                    || reason.contains("unknown record type"),
                "the reason is a decode boundary, got {reason}"
            );
        }
        _ => panic!("expected Damaged, got {error:?}"),
    }
    assert_eq!(
        fs::read(&path).expect("reread journal"),
        staged,
        "a refused open leaves the journal untouched"
    );
    let second = store
        .open_session(id)
        .await
        .expect_err("open still refuses");
    assert_eq!(
        second.to_string(),
        error.to_string(),
        "damage is stable across opens"
    );
    assert_eq!(
        fs::read(&path).expect("reread journal"),
        staged,
        "the second refused open also leaves the journal untouched"
    );
}

#[tokio::test]
async fn garbage_tail_without_newline_is_quarantined_and_the_session_recovers() {
    let (_temp, store, id, path, prefix) = staged_durable_journal("adv-garbage-tail").await;
    let tail: &[u8] = b"garbage-without-trailing-newline";
    let mut staged = prefix.clone();
    staged.extend_from_slice(tail);
    fs::write(&path, &staged).expect("stage a newline-less garbage tail");
    let (mut reopened, report) = store
        .open_session(id)
        .await
        .expect("a torn garbage tail is quarantined");
    let torn = report.torn.as_ref().expect("torn tail is reported");
    assert_eq!(
        torn.bytes,
        u64::try_from(tail.len()).expect("tail fits u64"),
        "the whole garbage tail is quarantined"
    );
    assert_eq!(
        fs::read(torn.kept_at.as_path()).expect("read side file"),
        tail,
        "the side file preserves the tail verbatim"
    );
    assert!(
        fs::read(&path)
            .expect("read repaired journal")
            .starts_with(&prefix),
        "repair restores exactly the durable prefix"
    );
    assert!(report.aborted.is_none(), "no turn was open");
    reopened
        .append(vec![user(2, "after garbage recovery")])
        .await
        .expect("the session accepts records after garbage-tail repair");
    reopened.close().await.expect("session closes");
}

#[tokio::test]
async fn megabyte_garbage_tail_is_quarantined_verbatim_across_read_windows() {
    let (_temp, store, id, path, prefix) = staged_durable_journal("adv-mb-tail").await;
    // One megabyte of NULs spans many backward-read windows and holds no
    // newline, so the scan must find the prefix's last line from far back.
    let tail = vec![b'\0'; 1_048_576];
    let mut staged = prefix.clone();
    staged.extend_from_slice(&tail);
    fs::write(&path, &staged).expect("stage a one-megabyte garbage tail");
    let (mut reopened, report) = store
        .open_session(id)
        .await
        .expect("a megabyte torn tail is quarantined");
    let torn = report.torn.as_ref().expect("torn tail is reported");
    assert_eq!(
        torn.bytes,
        u64::try_from(tail.len()).expect("tail fits u64"),
        "the whole tail is quarantined"
    );
    assert_eq!(
        fs::read(torn.kept_at.as_path())
            .expect("read side file")
            .len(),
        tail.len(),
        "the side file preserves the whole tail"
    );
    assert!(
        fs::read(&path)
            .expect("read repaired journal")
            .starts_with(&prefix),
        "the journal is truncated back to the prefix"
    );
    reopened.close().await.expect("session closes");
}

#[tokio::test]
async fn truncated_journal_shaped_record_is_quarantined_never_partial_decoded() {
    let (_temp, store, id, path, prefix) = staged_durable_journal("adv-trunc-record").await;
    // A torn suffix that still looks like the start of a record encoding must
    // quarantine whole; no partial decode may accept a fragment as a record.
    let fragment: &[u8] = b"{\"v\":1,\"type\":\"us";
    let mut staged = prefix.clone();
    staged.extend_from_slice(fragment);
    fs::write(&path, &staged).expect("stage a torn journal-shaped tail");
    let (mut reopened, report) = store
        .open_session(id)
        .await
        .expect("a torn journal-shaped tail is quarantined");
    let torn = report.torn.as_ref().expect("torn tail is reported");
    assert_eq!(
        torn.offset,
        u64::try_from(prefix.len()).expect("prefix fits u64"),
        "the tail starts where the prefix ended"
    );
    assert_eq!(
        torn.bytes,
        u64::try_from(fragment.len()).expect("fragment fits u64"),
        "the exact torn byte count is reported"
    );
    assert_eq!(
        fs::read(torn.kept_at.as_path()).expect("read side file"),
        fragment,
        "the fragment is preserved verbatim"
    );
    reopened.close().await.expect("session closes");
}

#[tokio::test]
async fn non_utf8_torn_tail_is_quarantined_bytewise() {
    let (_temp, store, id, path, prefix) = staged_durable_journal("adv-nonutf8").await;
    let mut staged = prefix.clone();
    staged.extend_from_slice(&[0xff, 0xfe, 0x00, 0x7f]);
    fs::write(&path, &staged).expect("stage a non-UTF-8 torn tail");
    let (mut reopened, report) = store
        .open_session(id)
        .await
        .expect("a non-UTF-8 torn tail is quarantined");
    let torn = report.torn.as_ref().expect("torn tail is reported");
    assert_eq!(torn.bytes, 4, "the raw tail is measured in bytes");
    assert_eq!(
        fs::read(torn.kept_at.as_path()).expect("read side file"),
        [0xff_u8, 0xfe, 0x00, 0x7f],
        "raw bytes survive quarantine"
    );
    reopened.close().await.expect("session closes");
}

#[tokio::test]
async fn failed_quarantine_leaves_the_torn_tail_on_the_journal() {
    let (temp, store, id, path, prefix) = staged_durable_journal("adv-quar-order").await;
    let tail: &[u8] = b"{\"v\":1,\"type\":\"us";
    let mut staged = prefix.clone();
    staged.extend_from_slice(tail);
    fs::write(&path, &staged).expect("stage a torn tail");
    // Occupy the side-file path with a directory: the quarantine write fails
    // while the session directory stays writable. Repair must quarantine
    // BEFORE truncating; a truncate-first order would lose the tail forever.
    let session = session_dir(&temp.path().join("data"), id);
    let side = session.join(torn_side_name(prefix.len()));
    fs::create_dir(&side).expect("block the side-file path with a directory");
    let error = store
        .open_session(id)
        .await
        .expect_err("quarantine failure must fail the open");
    assert_eq!(
        fs::read(&path).expect("reread journal"),
        staged,
        "a failed quarantine leaves the tail on the journal"
    );
    match &error {
        StoreError::Journal(JournalError::Io {
            op: "quarantine", ..
        }) => {}
        _ => panic!("expected a quarantine I/O failure, got {error:?}"),
    }
    // Once the path is free the same open repairs fully.
    fs::remove_dir(&side).expect("free the side-file path");
    let (mut reopened, report) = store
        .open_session(id)
        .await
        .expect("open repairs after the blocker is removed");
    let torn = report.torn.as_ref().expect("torn tail is reported");
    assert_eq!(
        fs::read(torn.kept_at.as_path()).expect("read side file"),
        tail,
        "the tail still reaches a side file"
    );
    assert!(
        fs::read(&path)
            .expect("read repaired journal")
            .starts_with(&prefix),
        "the journal is truncated only after durable quarantine"
    );
    reopened.close().await.expect("session closes");
}

#[tokio::test]
async fn crash_between_side_file_and_truncate_is_completed_by_the_next_open() {
    let (temp, store, id, path, prefix) = staged_durable_journal("adv-quar-crash").await;
    let tail: &[u8] = b"{\"v\":1,\"type\":\"us";
    let mut staged = prefix.clone();
    staged.extend_from_slice(tail);
    fs::write(&path, &staged).expect("stage a torn tail");
    // Reproduce the crash window by hand: the side file is durable but the
    // truncation never happened. The next open must finish the repair.
    let session = session_dir(&temp.path().join("data"), id);
    let side = session.join(torn_side_name(prefix.len()));
    fs::write(&side, tail).expect("pre-write the torn side file");
    let (mut reopened, report) = store
        .open_session(id)
        .await
        .expect("open completes the interrupted repair");
    let torn = report.torn.as_ref().expect("torn tail is reported");
    assert_eq!(
        torn.kept_at, side,
        "quarantine reuses the canonical side-file name"
    );
    assert_eq!(
        fs::read(&side).expect("read side file"),
        tail,
        "the side file still holds the tail after the completed repair"
    );
    assert!(
        fs::read(&path)
            .expect("read repaired journal")
            .starts_with(&prefix),
        "the journal is truncated to the durable prefix"
    );
    reopened.close().await.expect("session closes");
}

#[tokio::test]
async fn torn_tail_over_an_open_turn_still_aborts_the_turn_and_keeps_prefix() {
    let (_temp, store, id, path, prefix) = staged_durable_journal("adv-torn-turn").await;
    // The tail starts mid-turn: a user entry and an open turn are complete
    // lines behind a torn fragment. Recovery must quarantine the fragment,
    // keep the real records, and close turn 1 with one aborted end.
    // Turn ids count from 1: the journal has no turn yet, so the staged open
    // turn must be turn 1.
    let user_line = encode(&user(2, "turn work")).expect("user encodes");
    let start_line = encode(&Record::TurnStart {
        at: timestamp(),
        turn: turn_id(1),
    })
    .expect("turn start encodes");
    let fragment: &[u8] = b"{\"v\":1,\"type\":\"tu";
    let mut staged = prefix.clone();
    staged.extend_from_slice(&user_line);
    staged.extend_from_slice(&start_line);
    staged.extend_from_slice(fragment);
    fs::write(&path, &staged).expect("stage journal with open turn and torn tail");
    let (mut reopened, report) = store
        .open_session(id)
        .await
        .expect("open repairs the tail and aborts the open turn");
    let torn = report.torn.as_ref().expect("torn tail is reported");
    let complete_end = prefix.len() + user_line.len() + start_line.len();
    assert_eq!(
        torn.offset,
        u64::try_from(complete_end).expect("offset fits u64"),
        "the tail starts after the last complete line"
    );
    assert_eq!(
        torn.bytes,
        u64::try_from(fragment.len()).expect("fragment fits u64"),
        "only the fragment is quarantined"
    );
    let aborted = report.aborted.as_ref().expect("open turn is aborted");
    assert_eq!(aborted.turn, turn_id(1), "recovery names turn 1");
    let ends: Vec<_> = reopened
        .records()
        .iter()
        .filter_map(|record| match record {
            Record::TurnEnd { turn, stop, .. } => Some((*turn, stop.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        ends.last(),
        Some(&(turn_id(1), TurnEndStop::Aborted)),
        "the open turn receives exactly one aborted end"
    );
    assert!(
        reopened
            .records()
            .iter()
            .any(|record| matches!(record, Record::User(entry) if entry.id == entry_id(2))),
        "the real user record behind the turn survives"
    );
    reopened.close().await.expect("session closes");
}

#[tokio::test]
async fn appends_sidecars_and_reopens_interleave_without_loss_or_damage() {
    let (_temp, store) = setup("adv-sequence");
    let id = SessionId::new_v7();
    let path = store.session_file(id);
    let mut total_users = 0usize;
    let mut end = 0u64;
    for segment in 1..=3usize {
        let mut journal = if segment == 1 {
            store.create_session(id)
        } else {
            let (reopened, report) = store
                .open_session(id)
                .await
                .expect("reopen between segments");
            assert!(report.torn.is_none(), "no false damage between segments");
            assert!(report.aborted.is_none(), "no open turn between segments");
            // Reopen appends its boot record first; resync to the file end so
            // segment contiguity stays exact.
            end = fs::metadata(&path).expect("journal stat").len();
            reopened
        };
        for offset in 0..3usize {
            let index = total_users + offset + 1;
            let outcome = journal
                .append(vec![user(
                    u64::try_from(index).expect("index fits"),
                    "seg work",
                )])
                .await
                .expect("segment append lands");
            match outcome {
                AppendOutcome::Durable(receipt) => {
                    assert_eq!(
                        receipt.offset, end,
                        "every batch starts exactly at the previous end"
                    );
                    end = receipt.offset.saturating_add(receipt.len);
                }
                other => panic!("expected a durable receipt, got {other:?}"),
            }
        }
        total_users += 3;
        let state = format!("seg-{segment}");
        let sidecar = journal
            .sidecar()
            .expect("file-backed journal has a sidecar");
        sidecar
            .write("state", state.as_bytes())
            .expect("sidecar write during an append sequence");
        assert_eq!(
            sidecar.read("state").expect("read sidecar back"),
            state.as_bytes(),
            "the sidecar reads its just-written value"
        );
        if segment == 2 {
            sidecar
                .write("notes", b"kept")
                .expect("second sidecar write");
        }
        journal.close().await.expect("segment closes");
        assert_eq!(
            fs::metadata(&path).expect("journal stat").len(),
            end,
            "the journal file ends exactly at the last receipt after close"
        );
    }
    let (mut reopened, _) = store
        .open_session(id)
        .await
        .expect("final reopen sees the whole sequence");
    let users: Vec<_> = reopened
        .records()
        .iter()
        .filter_map(|record| match record {
            Record::User(entry) => Some(entry.id.get()),
            _ => None,
        })
        .collect();
    let expected: Vec<_> = (1..=9).collect();
    assert_eq!(users, expected, "every appended user survived in order");
    let boots = reopened
        .records()
        .iter()
        .filter(|record| matches!(record, Record::Boot { .. }))
        .count();
    assert_eq!(
        boots, 4,
        "one boot per open: materialize plus three reopens"
    );
    let sidecar = reopened.sidecar().expect("final open has a sidecar");
    assert_eq!(
        sidecar.read("state").expect("read state sidecar"),
        b"seg-3",
        "the sidecar survives every reopen"
    );
    assert_eq!(
        sidecar.read("notes").expect("read notes sidecar"),
        b"kept",
        "an untouched sidecar survives too"
    );
    reopened.close().await.expect("session closes");
}

#[tokio::test]
async fn put_blob_limit_holds_from_both_sides_and_bad_digest_entries_are_refused() {
    let (temp, store) = setup("adv-put-blob");
    let data_root = temp.path().join("data");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "materialize")])
        .await
        .expect("materialize");
    let blob_dir = session_dir(&data_root, id).join("blobs");

    let at_limit = vec![b'L'; 67_108_864];
    let digest = journal
        .put_blob(at_limit.clone())
        .expect("a blob of exactly the limit is accepted");
    assert_eq!(digest, BlobId::from_bytes(&at_limit));
    assert_eq!(
        journal.read_blob(digest).expect("reads back"),
        at_limit,
        "the exact-limit blob round-trips"
    );

    let over = vec![b'O'; 67_108_865];
    let over_digest = BlobId::from_bytes(&over);
    let error = journal
        .put_blob(over)
        .expect_err("a blob of limit+1 bytes is refused");
    assert!(
        matches!(
            &error,
            StoreError::Blob(BlobError::TooLarge { bytes: 67_108_865 })
        ),
        "got {error:?}"
    );
    assert!(
        !blob_dir.join(over_digest.to_string()).exists(),
        "a refused blob leaves no file"
    );

    // Corrupt the accepted digest's directory entry into a directory: the
    // store must refuse to read or republish it, never silently accept it.
    let digest_path = blob_dir.join(digest.to_string());
    fs::remove_file(&digest_path).expect("remove the digest file");
    fs::create_dir(&digest_path).expect("replace it with a directory");
    let republish = vec![b'L'; 67_108_864];
    let republish_error = journal
        .put_blob(republish)
        .expect_err("a directory at a digest path is not a blob");
    assert!(
        matches!(
            &republish_error,
            StoreError::Blob(BlobError::Io { source }) if source.kind() == ErrorKind::InvalidData
        ),
        "got {republish_error:?}"
    );
    let read_error = journal
        .read_blob(digest)
        .expect_err("a directory digest is not a blob");
    assert!(
        matches!(read_error, BlobError::Io { .. }),
        "got {read_error:?}"
    );
    journal.close().await.expect("session closes");
}

#[tokio::test]
async fn stale_lock_marker_is_taken_over_when_the_os_lock_is_free() {
    let (temp, store, id, _path, _prefix) = staged_durable_journal("adv-stale-takeover").await;
    let dir = session_dir(&temp.path().join("data"), id);
    let lock = dir.join("lock");
    fs::write(&lock, b"stale garbage that is not a pid\n").expect("stage a stale marker");
    let (mut reopened, report) = store
        .open_session(id)
        .await
        .expect("a stale marker never blocks a healthy takeover");
    assert!(report.torn.is_none(), "takeover is not journal damage");
    assert_eq!(
        fs::read(&lock).expect("read the rewritten marker"),
        format!("{}\n", std::process::id()).as_bytes(),
        "the takeover rewrites the marker with the new owner's pid"
    );
    reopened.close().await.expect("session closes");
}

#[tokio::test]
async fn sequential_second_open_is_refused_until_the_holder_closes() {
    let (_temp, store, id, _path, _prefix) = staged_durable_journal("adv-double-acquire").await;
    let (mut holder, _) = store.open_session(id).await.expect("first open holds");
    let second = store
        .open_session(id)
        .await
        .expect_err("double-acquire is refused");
    assert!(
        matches!(
            &second,
            StoreError::Locked { session, pid: Some(pid), .. }
                if *session == id && *pid == std::process::id()
        ),
        "the refusal names the in-process holder, got {second:?}"
    );
    holder.close().await.expect("holder closes");
    let (_again, _) = store
        .open_session(id)
        .await
        .expect("the lock frees on close");
}

#[tokio::test]
async fn killed_writer_never_loses_an_acknowledged_batch() {
    let (temp, store) = setup("adv-kill");
    let data_root = temp.path().join("data");
    let workspace_path = temp.path().join("workspace");
    let id = SessionId::new_v7();
    let mut child = Command::new(env!("CARGO_BIN_EXE_append-child"))
        .arg(&data_root)
        .arg(&workspace_path)
        .arg(id.to_string())
        .arg("append")
        .arg("300")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("helper process spawns");
    let stdout = child.stdout.take().expect("helper stdout is piped");
    let mut reader = std::io::BufReader::new(stdout);
    let mut acked = Vec::new();
    for _ in 0..3 {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                if let Some(rest) = line.strip_prefix("acked ") {
                    let index = rest.trim().parse::<usize>().expect("acked index parses");
                    acked.push(index);
                }
            }
        }
    }
    child.kill().expect("helper is killed");
    let status = child.wait().expect("helper is reaped");
    assert!(!status.success(), "the helper dies from the signal");

    let (mut reopened, report) = store
        .open_session(id)
        .await
        .expect("reopen after SIGKILL succeeds; damage here would be a violation");
    for index in &acked {
        let want = format!("child batch {index}");
        assert!(
            reopened
                .records()
                .iter()
                .any(|record| matches!(record, Record::Name { name, .. } if name.as_deref() == Some(want.as_str()))),
            "acknowledged batch {index} must exist after the kill"
        );
    }
    assert!(
        reopened
            .records()
            .iter()
            .any(|record| matches!(record, Record::User(entry) if entry.id == entry_id(1))),
        "the child's opening record survives"
    );
    assert!(
        report.aborted.is_none(),
        "the child never opens a turn, so no turn is aborted"
    );
    if let Some(torn) = report.torn {
        assert!(
            torn.kept_at.is_file(),
            "a torn tail from the kill is quarantined"
        );
    }
    reopened.close().await.expect("session closes");
}
