//! ENG-11 store proof gaps: crash-mid-write recovery, fork/clone identity and
//! independence, owner-only private files without secret leakage, and the
//! spec-named 200-iteration SIGKILL sweep.
//!
//! Every test below drives a real temporary store on the local filesystem.
//! No mocks, no fault-injection seams: torn bytes are real bytes, killed
//! writers are real processes, and mode bits are real mode bits.

#![expect(clippy::expect_used, reason = "integration tests fail loudly")]
#![expect(clippy::panic, reason = "integration tests fail loudly")]
#![expect(clippy::disallowed_methods, reason = "integration tests fail loudly")]

mod support;

use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    num::NonZeroU64,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use dal_core::{Entry, EntryId, EntryKind, JournalPart, Product, Record, SessionId, Workspace};
use dal_store::{Store, StoreError};
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

fn setup(tag: &str) -> (TempDir, Store) {
    let temp = TempDir::new(tag);
    let workspace =
        Workspace::new(temp.path().join("workspace")).expect("temporary workspace is absolute");
    let store = Store::new(temp.path().join("data"), workspace, Product::Dalgona);
    (temp, store)
}

fn user_texts(records: &[Record]) -> Vec<(u64, String)> {
    let mut rows = Vec::new();
    for record in records {
        let Record::User(entry) = record else {
            continue;
        };
        let EntryKind::User { parts } = &entry.kind else {
            continue;
        };
        let [JournalPart::Text { text }] = parts.as_slice() else {
            continue;
        };
        rows.push((entry.id.get(), text.to_string()));
    }
    rows
}

fn journal_lines(path: &Path) -> Vec<Vec<u8>> {
    fs::read(path)
        .expect("journal bytes read")
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(<[u8]>::to_vec)
        .collect()
}

/// Package metadata (REQ-01): the crate publishes under its spec name with
/// the workspace edition and lint set. The unit under test is the same
/// package the manifest describes.
#[test]
fn package_metadata_matches_spec() {
    assert_eq!(env!("CARGO_PKG_NAME"), "dal-store", "spec crate name");
    let manifest = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("package manifest reads");
    for needle in [
        "publish = true",
        "edition.workspace = true",
        "[lints]",
        "workspace = true",
    ] {
        assert!(
            manifest.contains(needle),
            "manifest carries {needle:?}:\n{manifest}"
        );
    }
}

/// A crash between two durable batches must repair with a typed report:
/// torn bytes are quarantined aside, the acknowledged prefix replays
/// verbatim, and the session stays writable.
#[tokio::test]
async fn crash_mid_write_drop_without_flush_recovers_typed() {
    let (_temp, store) = setup("store-proof-crash");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "durable prefix one")])
        .await
        .expect("first batch is durable");
    journal
        .append(vec![user(2, "durable prefix two")])
        .await
        .expect("second batch is durable");
    let path = store.session_file(id);
    let prefix = fs::read(&path).expect("durable prefix is on disk");

    // Unflushed torn write: bytes reach the OS page cache but no userspace
    // flush, no sync, no terminating newline follows. Dropping the File
    // closes the descriptor the way a dead process's descriptors close.
    let torn: &[u8] = b"{\"v\":1,\"type\":\"us";
    {
        let mut raw = fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("raw journal handle opens");
        raw.write_all(torn).expect("torn bytes hit the page cache");
    }
    // The process dies here: no journal close, no flush, the lock is freed
    // by handle drop alone.
    drop(journal);

    let (mut reopened, report) = store
        .open_session(id)
        .await
        .expect("reopen after a mid-write crash succeeds; damage here is a violation");
    let tail = report.torn.expect("the torn tail is typed, not silent");
    let prefix_len = u64::try_from(prefix.len()).expect("prefix length fits");
    let torn_len = u64::try_from(torn.len()).expect("torn length fits");
    assert_eq!(
        tail.offset, prefix_len,
        "quarantine starts exactly at the end of the durable prefix"
    );
    assert_eq!(tail.bytes, torn_len, "every torn byte is accounted for");
    assert!(
        tail.kept_at.is_file(),
        "the torn tail is preserved aside for forensics"
    );
    assert_eq!(
        fs::read(&tail.kept_at).expect("quarantined tail reads"),
        torn,
        "quarantine holds the exact torn bytes"
    );
    assert_eq!(
        user_texts(reopened.records()),
        vec![
            (1, "durable prefix one".to_owned()),
            (2, "durable prefix two".to_owned()),
        ],
        "the acknowledged prefix replays verbatim"
    );
    reopened
        .append(vec![user(3, "after the crash")])
        .await
        .expect("the repaired session stays writable");
    reopened.close().await.expect("repaired session closes");

    let (again, again_report) = store
        .open_session(id)
        .await
        .expect("second reopen succeeds");
    assert!(
        again_report.torn.is_none(),
        "repair runs exactly once; no second tail is reported"
    );
    assert_eq!(
        user_texts(again.records()).len(),
        3,
        "all three batches survive the second reopen"
    );
}

fn find_blob_digest(records: &[Record]) -> dal_core::BlobId {
    for record in records {
        let Record::User(entry) = record else {
            continue;
        };
        let EntryKind::User { parts } = &entry.kind else {
            continue;
        };
        let [JournalPart::TextBlob { blob, .. }] = parts.as_slice() else {
            continue;
        };
        return dal_core::BlobId::parse(blob).expect("digest parses");
    }
    panic!("a blob-backed user entry exists");
}

fn forked_user_ids(records: &[Record]) -> Vec<u64> {
    records
        .iter()
        .filter_map(|record| match record {
            Record::User(entry) => Some(entry.id.get()),
            _ => None,
        })
        .collect()
}

/// The branch path without its endpoints' framing: the session header
/// describes the owning session and each open mints a fresh boot record,
/// so identity across fork/clone holds on the copied tree records.
fn tree_records(records: &[Record]) -> Vec<&Record> {
    records
        .iter()
        .filter(|record| !matches!(record, Record::Session(_) | Record::Boot { .. }))
        .collect()
}

fn assert_fork_matches_source_prefix(forked_records: &[Record], source_records: &[Record]) {
    assert_eq!(
        forked_user_ids(forked_records),
        vec![1],
        "fork carries exactly the root-to-parent path"
    );
    assert!(
        forked_records.len() < source_records.len(),
        "a fork at a mid-path anchor is shorter than its source"
    );
    assert_eq!(
        tree_records(forked_records),
        tree_records(source_records)[..tree_records(forked_records).len()],
        "forked records are typed-identical to the source prefix with ids preserved"
    );
}

fn assert_clone_matches_source(
    cloned_records: &[Record],
    source_records: &[Record],
    id: SessionId,
    big: &str,
    blob_bytes: &[u8],
) {
    assert_eq!(
        cloned_records.len(),
        source_records.len(),
        "clone carries every record"
    );
    assert_eq!(
        tree_records(cloned_records),
        tree_records(source_records),
        "cloned records are typed-identical to the source with ids preserved"
    );
    let from_session = match &cloned_records[0] {
        Record::Session(header) => {
            header
                .from
                .as_ref()
                .expect("clone carries a from header")
                .session
        }
        other => panic!("clone starts with a session header, got {other:?}"),
    };
    assert_eq!(from_session, id, "clone names its source session");
    assert!(
        cloned_records.iter().any(|record| matches!(
            record,
            Record::Label { label: Some(label), .. } if label.as_ref() == "keep"
        )),
        "labels survive the clone"
    );
    assert_eq!(
        blob_bytes,
        big.as_bytes(),
        "clone shares the exact blob bytes"
    );
}

#[cfg(target_os = "linux")]
fn assert_blob_inode_shared(
    store: &Store,
    source: SessionId,
    clone: SessionId,
    digest: dal_core::BlobId,
) {
    use std::os::unix::fs::MetadataExt;
    let name = digest.to_string();
    let source_blob = store
        .session_file(source)
        .parent()
        .expect("session dir")
        .join("blobs")
        .join(&name);
    let clone_blob = store
        .session_file(clone)
        .parent()
        .expect("clone dir")
        .join("blobs")
        .join(&name);
    assert_eq!(
        fs::metadata(&source_blob).expect("stat source blob").ino(),
        fs::metadata(&clone_blob).expect("stat clone blob").ino(),
        "Linux shares the blob inode instead of copying bytes"
    );
}

fn assert_branch_lines_identical(
    source_lines: &[Vec<u8>],
    clone_lines: &[Vec<u8>],
    fork_lines: &[Vec<u8>],
) {
    assert_eq!(
        clone_lines.len(),
        source_lines.len(),
        "clone journal holds the same line count"
    );
    assert_eq!(
        &clone_lines[2..],
        &source_lines[2..],
        "clone record lines are byte-identical to the source"
    );
    assert_eq!(
        &fork_lines[2..],
        &source_lines[2..fork_lines.len()],
        "fork record lines are byte-identical to the source prefix"
    );
}

async fn assert_branch_sides_stay_independent(
    store: &Store,
    id: SessionId,
    forked_id: SessionId,
    cloned_id: SessionId,
) {
    let source_bytes = fs::read(store.session_file(id)).expect("source bytes");
    let clone_bytes = fs::read(store.session_file(cloned_id)).expect("clone bytes");

    let (mut fork_again, _) = store.open_session(forked_id).await.expect("fork reopens");
    fork_again
        .append(vec![user(9, "fork-only write")])
        .await
        .expect("fork appends independently");
    fork_again.close().await.expect("fork closes");
    assert_eq!(
        fs::read(store.session_file(id)).expect("source bytes after fork write"),
        source_bytes,
        "a fork write leaves the source journal bytes untouched"
    );
    assert_eq!(
        fs::read(store.session_file(cloned_id)).expect("clone bytes after fork write"),
        clone_bytes,
        "a fork write leaves the clone journal bytes untouched"
    );

    let (mut source_again, _) = store.open_session(id).await.expect("source reopens");
    source_again
        .append(vec![user(10, "source-only write")])
        .await
        .expect("source appends independently");
    source_again.close().await.expect("source closes");
    let (fork_check, _) = store.open_session(forked_id).await.expect("fork reopens");
    assert!(
        !user_texts(fork_check.records())
            .iter()
            .any(|(entry, _)| *entry == 10),
        "the source-only write never appears in the fork"
    );
    let (clone_check, _) = store.open_session(cloned_id).await.expect("clone reopens");
    assert!(
        !user_texts(clone_check.records())
            .iter()
            .any(|(entry, _)| *entry == 10),
        "the source-only write never appears in the clone"
    );
}

/// Fork and clone must carry the source path over exactly (typed record
/// equality, byte-equal journal lines and blob payloads) and then live
/// independently: later writes on either side leave the other side's
/// journal bytes untouched.
#[tokio::test]
async fn fork_and_clone_are_typed_identical_then_independent() {
    let (_temp, store) = setup("store-proof-branch");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "branch root")])
        .await
        .expect("root appends");
    let big = "b".repeat(20_000);
    journal
        .append(vec![user(2, &big)])
        .await
        .expect("blob-backed text appends");
    journal
        .append(vec![user(3, "branch leaf")])
        .await
        .expect("leaf appends");
    journal
        .append(vec![Record::Label {
            at: timestamp(),
            entry: entry_id(3),
            label: Some("keep".into()),
        }])
        .await
        .expect("label appends");
    // Snapshot semantics: later fork/clone writes must not move this copy.
    let source_records = journal.records().to_vec();

    let (mut forked, text) = journal.fork(entry_id(2)).await.expect("fork succeeds");
    assert_eq!(text, big, "fork returns the anchor text");
    let forked_records = forked.records().to_vec();
    assert_fork_matches_source_prefix(&forked_records, &source_records);

    let mut cloned = journal.clone_session().await.expect("clone succeeds");
    let cloned_records = cloned.records().to_vec();
    let digest = find_blob_digest(&cloned_records);
    let blob_bytes = cloned.read_blob(digest).expect("cloned blob reads");
    assert_clone_matches_source(&cloned_records, &source_records, id, &big, &blob_bytes);
    #[cfg(target_os = "linux")]
    assert_blob_inode_shared(&store, id, cloned.id(), digest);
    let forked_id = forked.id();
    let cloned_id = cloned.id();
    forked.close().await.expect("fork closes");
    cloned.close().await.expect("clone closes");
    journal.close().await.expect("source closes");

    // The re-encoded record lines are byte-identical past the framing:
    // line 0 is the retargeted header (new id, timestamp, from) and line 1
    // is the freshly minted boot record; both files are read before any
    // reopen appends a second boot.
    let source_lines = journal_lines(&store.session_file(id));
    let clone_lines = journal_lines(&store.session_file(cloned_id));
    let fork_lines = journal_lines(&store.session_file(forked_id));
    assert_branch_lines_identical(&source_lines, &clone_lines, &fork_lines);

    // Independence: each side's later writes touch only its own bytes.
    assert_branch_sides_stay_independent(&store, id, forked_id, cloned_id).await;
}

#[cfg(unix)]
fn find_named_file(root: &Path, name: &str) -> PathBuf {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir).expect("list session dir");
        for entry in entries {
            let path = entry.expect("read entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.file_name().is_some_and(|file| file == name) {
                return path;
            }
        }
    }
    panic!("{name} exists under {}", root.display());
}

/// Owner-only files on Unix, and error paths that name digests without ever
/// echoing secret bytes.
#[cfg(unix)]
#[tokio::test]
async fn private_files_are_owner_only_and_errors_carry_no_secrets() {
    use std::os::unix::fs::PermissionsExt;

    use dal_store::file_is_private;

    let (_temp, store) = setup("store-proof-private");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    // A canary secret long enough to leave the journal and become a blob.
    let canary = format!("s3cr3t-payload-{id}-");
    let secret = format!("{canary}{}", "x".repeat(20_000));
    journal
        .append(vec![user(1, &secret)])
        .await
        .expect("secret appends");
    journal
        .sidecar()
        .expect("session has a sidecar")
        .write("token", secret.as_bytes())
        .expect("sidecar stores the secret");

    let journal_path = store.session_file(id);
    let session_dir = journal_path.parent().expect("session dir").to_path_buf();
    let digest = find_blob_digest(journal.records());
    let blob_path = session_dir.join("blobs").join(digest.to_string());
    let sidecar_path = find_named_file(&session_dir, "token");
    let lock_path = session_dir.join("lock");

    for path in [&journal_path, &lock_path, &blob_path, &sidecar_path] {
        let mode = fs::metadata(path)
            .unwrap_or_else(|_| panic!("stat {}", path.display()))
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "{} is owner-only", path.display());
        let file = fs::File::open(path).expect("private file opens");
        assert!(
            file_is_private(&file).expect("privacy is checkable"),
            "{} verifies as private",
            path.display()
        );
    }
    let dir_mode = fs::metadata(&session_dir)
        .expect("stat session dir")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dir_mode, 0o700, "session dir is owner-only");

    // Negative control: the check is not vacuous.
    let lax_path = session_dir.join("lax-copy");
    fs::write(&lax_path, b"not secret").expect("lax file writes");
    let mut perms = fs::metadata(&lax_path).expect("stat lax").permissions();
    perms.set_mode(0o644);
    fs::set_permissions(&lax_path, perms).expect("lax file relaxes");
    let lax = fs::File::open(&lax_path).expect("lax opens");
    assert!(
        !file_is_private(&lax).expect("lax privacy is checkable"),
        "a group-readable file does not verify as private"
    );
    fs::remove_file(&lax_path).expect("lax copy removed");

    // Corrupt the blob on disk, then prove the refusal names the digest
    // while never echoing the secret bytes.
    fs::write(&blob_path, "H".repeat(secret.len())).expect("flip the blob bytes");
    let refused = journal
        .read_blob(digest)
        .expect_err("bytes that do not hash to the digest are refused");
    let message = refused.to_string();
    assert!(
        message.contains(&digest.to_string()),
        "the refusal names the digest: {message}"
    );
    assert!(
        !message.contains(&canary),
        "the refusal never echoes secret bytes: {message}"
    );
    journal.close().await.expect("session closes");
}

/// Spec `sigkill-survives-every-reopen`: 200 child append iterations with a
/// SIGKILL at a varying point in each run. Every printed acknowledgement is
/// durable and therefore present after reopen; a kill before any
/// acknowledgement may leave nothing at all, but never damage.
#[cfg(unix)]
#[ignore = "200 kill/reopen iterations; run explicitly per the spec verification"]
#[tokio::test]
async fn sigkill_survives_every_reopen() {
    use std::os::unix::process::ExitStatusExt;

    const ITERATIONS: usize = 200;
    const BATCHES: usize = 6;

    for iteration in 0..ITERATIONS {
        // A fresh workspace per iteration: session names are workspace-unique,
        // so sharing one workspace would turn the helper's fixed batch names
        // into false collisions instead of crash evidence.
        let temp = TempDir::new("store-proof-kill200");
        let workspace =
            Workspace::new(temp.path().join("workspace")).expect("temporary workspace is absolute");
        let data_root = temp.path().join("data");
        let store = Store::new(data_root.clone(), workspace.clone(), Product::Dalgona);
        // Vary the kill point deterministically across 0..=5 acknowledgements.
        let kill_after = (iteration * 37 + 11) % BATCHES;
        let id = SessionId::new_v7();
        let mut child = Command::new(env!("CARGO_BIN_EXE_append-child"))
            .arg(&data_root)
            .arg(workspace.as_path())
            .arg(id.to_string())
            .arg("append")
            .arg(BATCHES.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("helper process spawns");
        let stdout = child.stdout.take().expect("helper stdout is piped");
        let stderr = child.stderr.take().expect("helper stderr is piped");
        let mut reader = BufReader::new(stdout);
        let mut acked = Vec::new();
        for _ in 0..kill_after {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if let Some(rest) = line.strip_prefix("acked ") {
                        acked.push(rest.trim().parse::<usize>().expect("acked index parses"));
                    }
                }
            }
        }
        // SIGKILL, exactly like a machine crash: no handlers run. A kill
        // that lands on an already-exited (zombie) helper still reports
        // success from kill but reaps the helper's own exit status, so the
        // invariant keys on the reaped status: violent SIGKILL death, or a
        // clean exit whose acknowledgements are all durable.
        let _ = child.kill();
        let status = child.wait().expect("helper is reaped");
        drop(reader);
        let mut diagnostics = String::new();
        let _ = BufReader::new(stderr).read_to_string(&mut diagnostics);
        if status.signal() != Some(9) {
            assert!(
                status.success(),
                "iteration {iteration}: a helper that outran the kill exits cleanly, \
                 got {status:?} after {acked:?}: {diagnostics}"
            );
        }

        match store.open_session(id).await {
            Ok((mut reopened, report)) => {
                for index in &acked {
                    let want = format!("child batch {index}");
                    assert!(
                        reopened.records().iter().any(|record| matches!(
                            record,
                            Record::Name { name, .. } if name.as_deref() == Some(want.as_str())
                        )),
                        "iteration {iteration}: acknowledged batch {index} survives the kill"
                    );
                }
                if let Some(torn) = report.torn {
                    assert!(
                        torn.kept_at.is_file(),
                        "iteration {iteration}: a torn tail from the kill is quarantined"
                    );
                }
                assert!(
                    report.aborted.is_none(),
                    "iteration {iteration}: the child opens no turn"
                );
                reopened.close().await.expect("recovered session closes");
            }
            Err(error) => {
                assert!(
                    acked.is_empty(),
                    "iteration {iteration}: acknowledged batches must reopen, got {error:?}"
                );
                assert!(
                    matches!(&error, StoreError::NotFound { .. }),
                    "iteration {iteration}: a pre-materialization kill reports absence, \
                     never damage, got {error:?}"
                );
            }
        }
    }
}

/// Atomic publication (REQ-03) with owner-only privacy (REQ-33):
/// `write_atomic` lands exact bytes at mode 0600 with no temp residue,
/// `write_atomic_new` never replaces an existing target, and plain
/// `write_atomic` still replaces.
#[cfg(unix)]
#[test]
fn atomic_publish_is_owner_only_and_new_never_replaces() {
    use std::os::unix::fs::PermissionsExt;

    use dal_store::{FileMode, file_is_private, write_atomic, write_atomic_new};

    let temp = TempDir::new("store-proof-atomic");
    let target = temp.path().join("cache.json");
    write_atomic(&target, b"{\"v\":1}", FileMode::Mode0600).expect("first publish lands");
    assert_eq!(
        fs::read(&target).expect("published bytes read"),
        b"{\"v\":1}",
        "publication lands the exact bytes"
    );
    let mode = fs::metadata(&target)
        .expect("stat target")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "published file is owner-only");
    assert!(
        file_is_private(&fs::File::open(&target).expect("target opens"))
            .expect("privacy is checkable"),
        "published file verifies as private"
    );
    let residue: Vec<PathBuf> = fs::read_dir(temp.path())
        .expect("list temp dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().contains(".tmp-"))
        })
        .collect();
    assert!(
        residue.is_empty(),
        "no temp file survives publication: {residue:?}"
    );

    match write_atomic_new(&target, b"{\"v\":2}", FileMode::Mode0600) {
        Err(StoreError::Io { source, .. }) => assert_eq!(
            source.kind(),
            std::io::ErrorKind::AlreadyExists,
            "no-replace reports the collision"
        ),
        other => panic!("the second creator is refused, got {other:?}"),
    }
    assert_eq!(
        fs::read(&target).expect("target reread"),
        b"{\"v\":1}",
        "the refused creator changes nothing"
    );

    write_atomic(&target, b"{\"v\":2}", FileMode::Mode0600).expect("replace publishes");
    assert_eq!(
        fs::read(&target).expect("replaced bytes read"),
        b"{\"v\":2}",
        "plain publish still replaces"
    );
}

/// Batch durability (REQ-13) over content-addressed blobs (REQ-08): every
/// durable receipt names exactly the bytes on disk, receipts chain
/// contiguously, and a blob-backed batch is backed by a real
/// `blobs/<64-hex-digest>` file before any record names it.
#[tokio::test]
async fn durable_receipt_matches_bytes_on_disk_with_blobs_published_first() {
    use dal_store::AppendOutcome;

    let (_temp, store) = setup("store-proof-durable");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    let (first_offset, first_len) = match journal
        .append(vec![user(1, "small prefix")])
        .await
        .expect("prefix appends")
    {
        AppendOutcome::Durable(receipt) => (receipt.offset, receipt.len),
        other => panic!("the first user entry is durable, got {other:?}"),
    };
    assert_eq!(first_offset, 0, "a fresh journal starts at offset zero");
    let big = "b".repeat(20_000);
    let (second_offset, second_len) = match journal
        .append(vec![user(2, &big)])
        .await
        .expect("blob-backed batch appends")
    {
        AppendOutcome::Durable(receipt) => (receipt.offset, receipt.len),
        other => panic!("the blob-backed batch is durable, got {other:?}"),
    };
    assert_eq!(
        second_offset,
        first_offset + first_len,
        "receipts chain contiguously with no gap or overlap"
    );
    assert_eq!(
        fs::metadata(store.session_file(id))
            .expect("stat journal")
            .len(),
        second_offset + second_len,
        "the receipt names exactly the bytes on disk"
    );
    let digest = find_blob_digest(journal.records());
    assert_eq!(
        digest.to_string().len(),
        64,
        "the blob name is a full digest"
    );
    let blob_path = store
        .session_file(id)
        .parent()
        .expect("session dir")
        .join("blobs")
        .join(digest.to_string());
    assert!(
        blob_path.is_file(),
        "the blob file is durable before the batch names it"
    );
    assert_eq!(
        store.read_blob(id, digest).expect("blob reads"),
        big.as_bytes(),
        "the published blob carries the exact bytes"
    );
    journal.close().await.expect("session closes");
    let (reopened, _) = store.open_session(id).await.expect("session reopens");
    assert_eq!(
        forked_user_ids(reopened.records()),
        vec![1, 2],
        "both durable batches replay after reopen with ids preserved"
    );
}

/// Lock-free blob reads with typed absence (REQ-10) and lock-guarded
/// delete (REQ-31): an open session's blobs read through the `Store`
/// handle without the lock; deleting while open reports `Locked` and
/// changes nothing; after close the tree is gone and reads report `Gone`.
#[tokio::test]
async fn open_journal_reads_blobs_without_lock_and_delete_reports_gone() {
    use dal_core::BlobId;
    use dal_store::{AppendOutcome, BlobError};

    let (_temp, store) = setup("store-proof-delete");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    match journal
        .append(vec![user(1, "readable while open")])
        .await
        .expect("small entry appends")
    {
        AppendOutcome::Durable(_) => {}
        other => panic!("the small entry is durable, got {other:?}"),
    }
    let big = "d".repeat(20_000);
    match journal
        .append(vec![user(2, &big)])
        .await
        .expect("blob-backed entry appends")
    {
        AppendOutcome::Durable(_) => {}
        other => panic!("the blob-backed entry is durable, got {other:?}"),
    }
    let digest = find_blob_digest(journal.records());

    assert_eq!(
        store.read_blob(id, digest).expect("lock-free read"),
        big.as_bytes(),
        "an open session's blobs read without acquiring its lock"
    );
    match store.read_blob(id, BlobId::from_bytes(b"nope")) {
        Err(BlobError::NotFound { .. }) => {}
        other => panic!("a missing digest in a live session is NotFound, got {other:?}"),
    }
    match store.delete(id) {
        Err(StoreError::Locked { .. }) => {}
        other => panic!("deleting an open journal reports Locked, got {other:?}"),
    }
    assert!(
        store.session_file(id).is_file(),
        "the refused delete changes nothing"
    );
    journal.close().await.expect("session closes");

    store.delete(id).expect("delete succeeds after close");
    assert!(
        !store.session_file(id).exists(),
        "the journal file is gone with the tree"
    );
    match store.read_blob(id, digest) {
        Err(BlobError::Gone) => {}
        other => panic!("blob reads after deletion report Gone, got {other:?}"),
    }
}

/// Handle properties (REQ-12): `Journal` is `Send` but neither `Sync` nor
/// `Clone`, and mutation takes `&mut self` (`store.rs:1038`). `Send` is
/// pinned below (the build breaks if it is lost); the negative bounds are
/// structural and were verified against this exact tree with bound probes:
/// `assert_sync::<Journal>` fails with `` `Cell<()>` cannot be shared
/// between threads safely `` and `assert_clone::<Journal>` fails with
/// `the trait Clone is not implemented`, so neither bound can silently hold.
#[tokio::test]
async fn journal_handle_is_send_not_sync_not_clone() {
    fn assert_send<T: Send>() {}
    assert_send::<dal_store::Journal>();

    let (_temp, store) = setup("store-proof-handle");
    let mut journal = store.create_session(SessionId::new_v7());
    journal
        .append(vec![user(1, "handle")])
        .await
        .expect("exclusive append works through &mut self");
    journal.close().await.expect("session closes");
}
