#![expect(clippy::expect_used, reason = "integration tests fail loudly")]
#![expect(clippy::panic, reason = "integration tests fail loudly")]
#![expect(clippy::collapsible_if, reason = "integration tests fail loudly")]
//! Blob spill thresholds, limits, reads, and orphans.

mod support;

use std::{fs, num::NonZeroU64, path::PathBuf};

use dal_core::{
    BlobId, Entry, EntryId, EntryKind, JournalPart, Product, Record, SessionId, Workspace,
};
use dal_store::{BlobError, Store};
use support::temp_dir::TempDir;

fn entry_id(value: u64) -> EntryId {
    EntryId::new(NonZeroU64::new(value).expect("nonzero test entry id"))
}

fn timestamp() -> jiff::Timestamp {
    jiff::Timestamp::now()
}

fn text_record(id: u64, text: &str) -> Record {
    Record::User(Entry {
        id: entry_id(id),
        parent: NonZeroU64::new(id.saturating_sub(1)).map(EntryId::new),
        at: timestamp(),
        kind: EntryKind::User {
            parts: vec![JournalPart::Text { text: text.into() }],
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

fn session_dir(data_root: &std::path::Path, id: SessionId) -> PathBuf {
    let sessions = data_root.join("sessions");
    let name = id.to_string();
    let mut found = None;
    let mut stack = vec![sessions];
    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir).expect("list data-root subtree");
        for entry in entries {
            let entry = entry.expect("read data-root entry");
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|base| base == name.as_str()) {
                    found = Some(path);
                } else {
                    stack.push(path);
                }
            }
        }
    }
    found.expect("session directory exists")
}

fn blob_files(session: &std::path::Path) -> Vec<PathBuf> {
    let blobs = session.join("blobs");
    if !blobs.is_dir() {
        return Vec::new();
    }
    fs::read_dir(&blobs)
        .expect("list blobs directory")
        .map(|entry| entry.expect("read blob entry").path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|base| !base.to_string_lossy().starts_with(".tmp-"))
        })
        .collect()
}

fn user_text(record: &Record) -> &str {
    match record {
        Record::User(entry) => match &entry.kind {
            EntryKind::User { parts } => match &parts[..] {
                [JournalPart::Text { text }] => text.as_ref(),
                _ => panic!("expected one inline text part"),
            },
            _ => panic!("expected a user entry"),
        },
        _ => panic!("expected a user record"),
    }
}

#[tokio::test]
async fn inline_vs_blob_threshold() {
    let (temp, store, _) = setup("blob-threshold");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    let inline = "i".repeat(16_383);
    journal
        .append(vec![text_record(1, &inline)])
        .await
        .expect("inline append succeeds");
    assert_eq!(
        user_text(&journal.records()[2]),
        inline.as_str(),
        "16383 bytes remain inline"
    );
    let big = "b".repeat(16_384);
    journal
        .append(vec![text_record(2, &big)])
        .await
        .expect("threshold append succeeds");
    let spilled = &journal.records()[3];
    let (digest, bytes) = match spilled {
        Record::User(entry) => match &entry.kind {
            EntryKind::User { parts } => match &parts[..] {
                [JournalPart::TextBlob { blob, bytes }] => (blob.clone(), *bytes),
                _ => panic!("16384 bytes spill to exactly one blob part"),
            },
            _ => panic!("expected a user entry"),
        },
        _ => panic!("expected a user record"),
    };
    assert_eq!(bytes, 16_384, "blob part names its byte length");
    assert_eq!(
        digest.as_ref(),
        BlobId::from_bytes(big.as_bytes()).to_string().as_str(),
        "the second names the blake3 hex file"
    );
    let dir = session_dir(&temp.path().join("data"), id);
    assert_eq!(blob_files(&dir).len(), 1, "one digest file is published");
    journal
        .append(vec![text_record(3, &big)])
        .await
        .expect("duplicate append succeeds");
    assert_eq!(
        blob_files(&dir).len(),
        1,
        "a second put of equal bytes creates no file"
    );
    journal.close().await.expect("session closes");
}

#[tokio::test]
async fn oversize_blob_rejected() {
    let (_temp, store, _) = setup("blob-oversize");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![text_record(1, "prefix")])
        .await
        .expect("prefix appends");
    let before = journal.records().len();
    let huge = "h".repeat(67_108_865);
    let error = journal
        .append(vec![text_record(2, &huge)])
        .await
        .expect_err("oversize blob is rejected");
    match &error {
        dal_store::StoreError::Blob(BlobError::TooLarge { bytes }) => {
            assert_eq!(*bytes, 67_108_865, "rejected length is reported");
        }
        _ => panic!("expected BlobError::TooLarge, got {error:?}"),
    }
    assert_eq!(
        error.to_string(),
        "blob of 67108865 bytes is larger than the limit of 67108864 bytes",
        "exact oversize text"
    );
    assert_eq!(journal.records().len(), before, "no record is appended");
}

#[tokio::test]
async fn blob_read_notfound_then_gone() {
    let (temp, store, _) = setup("blob-gone");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    let big = "g".repeat(16_384);
    journal
        .append(vec![text_record(1, &big)])
        .await
        .expect("blob append succeeds");
    let digest = match &journal.records()[2] {
        Record::User(entry) => match &entry.kind {
            EntryKind::User { parts } => match &parts[..] {
                [JournalPart::TextBlob { blob, .. }] => BlobId::parse(blob).expect("digest parses"),
                _ => panic!("expected a blob part"),
            },
            _ => panic!("expected a user entry"),
        },
        _ => panic!("expected a user record"),
    };
    let dir = session_dir(&temp.path().join("data"), id);
    fs::remove_file(dir.join("blobs").join(digest.to_string())).expect("delete the digest file");
    let missing = journal
        .read_blob(digest)
        .expect_err("deleted digest is missing");
    assert!(
        matches!(missing, BlobError::NotFound { .. }),
        "deleted file reads NotFound"
    );
    assert_eq!(
        missing.to_string(),
        format!("blob {digest} is not in this session"),
        "exact NotFound text"
    );
    journal.close().await.expect("session closes");
    store.delete(id).expect("session deletes");
    let gone = journal
        .read_blob(digest)
        .expect_err("deleted session is gone");
    assert!(
        matches!(gone, BlobError::Gone),
        "deleted directory reads Gone"
    );
    assert_eq!(
        gone.to_string(),
        "the session was deleted, so its blobs are gone",
        "exact Gone text"
    );
}

#[tokio::test]
async fn orphan_blob_survives_reopen() {
    let (temp, store, _) = setup("blob-orphan");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![text_record(1, "durable prefix")])
        .await
        .expect("prefix appends");
    journal.close().await.expect("session closes");
    let dir = session_dir(&temp.path().join("data"), id);
    let orphan_bytes = b"orphaned before the journal batch";
    let orphan = BlobId::from_bytes(orphan_bytes);
    fs::create_dir_all(dir.join("blobs")).expect("blobs dir exists");
    fs::write(dir.join("blobs").join(orphan.to_string()), orphan_bytes)
        .expect("stage an orphan digest");
    let (reopened, _) = store.open_session(id).await.expect("reopen succeeds");
    assert!(
        dir.join("blobs").join(orphan.to_string()).exists(),
        "an orphan may remain"
    );
    for record in reopened.records() {
        if let Record::User(entry) = record {
            if let EntryKind::User { parts } = &entry.kind {
                for part in parts {
                    let digest = match part {
                        JournalPart::TextBlob { blob, .. }
                        | JournalPart::ImageBlob { blob, .. }
                        | JournalPart::Blob { blob, .. } => Some(blob),
                        _ => None,
                    };
                    if let Some(digest) = digest {
                        assert!(
                            dir.join("blobs").join(digest.as_ref()).exists(),
                            "no durable record names a missing blob"
                        );
                    }
                }
            }
        }
    }
}

#[tokio::test]
async fn blob_io_failure_does_not_publish_record() {
    let (temp, store, _) = setup("blob-io");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![text_record(1, "healthy prefix")])
        .await
        .expect("prefix appends");
    let dir = session_dir(&temp.path().join("data"), id);
    let blobs = dir.join("blobs");
    if blobs.is_dir() {
        fs::remove_dir_all(&blobs).expect("clear the blobs directory");
    }
    fs::write(&blobs, b"blocker").expect("block the blobs directory");
    let big = "f".repeat(16_384);
    let before = journal.records().len();
    let error = journal
        .append(vec![text_record(2, &big)])
        .await
        .expect_err("blob write fails");
    assert!(
        matches!(error, dal_store::StoreError::Blob(BlobError::Io { .. })),
        "real blob write returns the blob I/O error, got {error:?}"
    );
    assert_eq!(journal.records().len(), before, "the record stays absent");
    fs::remove_file(dir.join("blobs")).expect("unblock the blobs directory");
    let later = journal.append(vec![text_record(2, &big)]).await;
    match later {
        Err(dal_store::StoreError::Broken { .. }) => {}
        Err(other) => panic!("later mutations return Broken, got {other:?}"),
        Ok(_) => panic!("later mutations return Broken"),
    }
}

#[tokio::test]
async fn corrupted_blob_bytes_are_refused_not_returned() {
    let (temp, store, _) = setup("blob-corrupt");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    let big = "g".repeat(16_384);
    journal
        .append(vec![text_record(1, &big)])
        .await
        .expect("blob append succeeds");
    let digest = match &journal.records()[2] {
        Record::User(entry) => match &entry.kind {
            EntryKind::User { parts } => match &parts[..] {
                [JournalPart::TextBlob { blob, .. }] => BlobId::parse(blob).expect("digest parses"),
                _ => panic!("expected a blob part"),
            },
            _ => panic!("expected a user entry"),
        },
        _ => panic!("expected a user record"),
    };
    let path = session_dir(&temp.path().join("data"), id)
        .join("blobs")
        .join(digest.to_string());
    assert_eq!(
        journal.read_blob(digest).expect("intact blob reads"),
        big.as_bytes()
    );

    fs::write(&path, "h".repeat(16_384)).expect("flip the blob bytes");
    let flipped = journal
        .read_blob(digest)
        .expect_err("bytes that do not hash to the digest are refused");
    assert!(
        matches!(&flipped, BlobError::Io { source } if source.kind() == std::io::ErrorKind::InvalidData),
        "descriptive InvalidData, got {flipped:?}"
    );
    assert!(
        flipped.to_string().contains(&digest.to_string()),
        "the message names the digest: {flipped}"
    );

    fs::write(&path, b"").expect("truncate the blob to zero bytes");
    let emptied = store
        .read_blob(id, digest)
        .expect_err("a zero-length blob is not the blob");
    assert!(
        matches!(&emptied, BlobError::Io { source } if source.kind() == std::io::ErrorKind::InvalidData),
        "a truncated blob is corrupt, not missing, got {emptied:?}"
    );
    journal.close().await.expect("session closes");
}

#[tokio::test]
async fn blob_write_to_a_session_that_never_existed_reports_gone() {
    let (_temp, store, _) = setup("blob-no-session");
    let error = store
        .write_blob(SessionId::new_v7(), b"orphan")
        .expect_err("no session directory exists");
    assert!(matches!(error, BlobError::Gone), "got {error:?}");
}

#[tokio::test]
async fn blob_limit_holds_from_both_sides() {
    let (temp, store, _) = setup("blob-limit");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![text_record(1, "prefix")])
        .await
        .expect("prefix append");

    let at_limit = vec![b'L'; 67_108_864];
    let digest = store
        .write_blob(id, &at_limit)
        .expect("a blob of exactly the limit is accepted");
    assert_eq!(digest, BlobId::from_bytes(&at_limit));
    assert_eq!(
        store.read_blob(id, digest).expect("reads back").len(),
        at_limit.len()
    );
    let below = vec![b'B'; 67_108_863];
    store
        .write_blob(id, &below)
        .expect("a blob of limit-1 bytes is accepted");
    let over = vec![b'O'; 67_108_865];
    let error = store
        .write_blob(id, &over)
        .expect_err("a blob of limit+1 bytes is refused");
    assert!(
        matches!(error, BlobError::TooLarge { bytes: 67_108_865 }),
        "got {error:?}"
    );
    assert!(
        !session_dir(&temp.path().join("data"), id)
            .join("blobs")
            .join(BlobId::from_bytes(&over).to_string())
            .exists(),
        "a refused blob leaves no file"
    );
    journal.close().await.expect("session closes");
}

/// A compaction whose replacement text is large enough to leave the journal
/// for a blob. It carries no user entry, so a lazy session buffers it.
#[cfg(unix)]
fn compaction_with_text(id: u64, text: &str) -> Record {
    Record::Compaction(Entry {
        id: entry_id(id),
        parent: None,
        at: timestamp(),
        kind: EntryKind::Compaction {
            summary: None,
            first_kept: None,
            tokens_before: 1,
            replay: None,
            usage: None,
            parts: vec![JournalPart::Text { text: text.into() }],
            parts_tokens: 1,
        },
    })
}

/// Records buffered before the first user entry name blobs the session has not
/// published yet. A first create that fails must leave those bytes buffered,
/// or the retry would write a journal naming a blob that no file holds.
#[cfg(unix)]
#[tokio::test]
async fn failed_first_create_keeps_the_blobs_buffered_records_name() {
    let (temp, store, _) = setup("blob-first-create-retry");
    let root = temp.path().join("data");
    let id = SessionId::new_v7();
    let big = "c".repeat(16_384);
    let digest = BlobId::from_bytes(big.as_bytes());
    let mut journal = store.create_session(id);
    journal
        .append(vec![compaction_with_text(1, &big)])
        .await
        .expect("the compaction buffers before any user entry");
    assert!(journal.is_lazy(), "nothing is durable yet");

    fs::write(&root, b"not a directory").expect("block the data root");
    journal
        .append(vec![text_record(2, "first")])
        .await
        .expect_err("the first create fails");
    fs::remove_file(&root).expect("clear the blocker");
    assert!(journal.is_lazy(), "a failed first create leaves it lazy");

    journal
        .append(vec![text_record(2, "first")])
        .await
        .expect("the retry creates the session");
    assert_eq!(
        store
            .read_blob(id, digest)
            .expect("the blob the buffered compaction names is published"),
        big.as_bytes()
    );
    assert!(
        fs::read_to_string(store.session_file(id))
            .expect("read the journal")
            .contains(&digest.to_string()),
        "the durable journal names the blob that now exists"
    );
    journal.close().await.expect("session closes");
}

/// A blob file past the cap is damaged or planted. A read refuses it from the
/// file length instead of allocating whatever the file holds.
#[cfg(unix)]
#[tokio::test]
async fn oversized_blob_file_is_refused_before_it_is_read() {
    let (temp, store, _) = setup("blob-oversized-file");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![text_record(1, "prefix")])
        .await
        .expect("prefix append");
    let planted = BlobId::from_bytes(b"planted");
    let path = session_dir(&temp.path().join("data"), id)
        .join("blobs")
        .join(planted.to_string());
    let file = fs::File::create(&path).expect("plant a blob file");
    file.set_len(67_108_865)
        .expect("grow the file past the cap without writing it");
    drop(file);

    for error in [
        store
            .read_blob(id, planted)
            .expect_err("the store refuses an oversized blob file"),
        journal
            .read_blob(planted)
            .expect_err("the journal refuses an oversized blob file"),
    ] {
        assert!(
            matches!(error, BlobError::TooLarge { bytes: 67_108_865 }),
            "got {error:?}"
        );
        assert_eq!(
            error.to_string(),
            "blob of 67108865 bytes is larger than the limit of 67108864 bytes"
        );
    }
    journal.close().await.expect("session closes");
}
