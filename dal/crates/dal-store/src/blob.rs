//! Content-addressed blobs. A put finishes before any record names the digest.

use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
};

use dal_core::BlobId;
use tempfile::{PathPersistError, TempPath};

use crate::{
    error::BlobError,
    util::{self, MODE_FILE},
};

/// Bytes at or above this length leave the journal and become a blob.
pub(crate) const INLINE_LIMIT: usize = 16_384;
/// A blob larger than this is rejected.
pub(crate) const MAX_BLOB: u64 = 67_108_864;
/// A validated blob awaiting durable file publication or in-memory storage.
///
/// The fields stay private so the digest always matches the owned bytes.
pub(crate) struct PendingBlob {
    id: BlobId,
    bytes: Vec<u8>,
}

impl PendingBlob {
    fn new(bytes: Vec<u8>) -> Self {
        let id = BlobId::from_bytes(&bytes);
        Self { id, bytes }
    }

    /// Validates one direct blob write and binds its digest to the bytes.
    pub(crate) fn prepare_put(bytes: Vec<u8>) -> Result<Self, BlobError> {
        check_blob_size(bytes.len())?;
        Ok(Self::new(bytes))
    }

    /// Returns the content digest.
    pub(crate) fn id(&self) -> BlobId {
        self.id
    }

    /// Borrows the payload without copying it.
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Moves the digest and payload out for in-memory storage.
    pub(crate) fn into_parts(self) -> (BlobId, Vec<u8>) {
        (self.id, self.bytes)
    }
}

/// Publishes `bytes` under `dir` if that digest is not already present.
///
/// # Errors
/// Returns [`BlobError::TooLarge`] above the cap, and [`BlobError::Io`] on a failed publish.
pub(crate) fn put(dir: &Path, bytes: &[u8]) -> Result<BlobId, BlobError> {
    check_blob_size(bytes.len())?;
    let id = BlobId::from_bytes(bytes);
    publish_with_id(dir, id, bytes)?;
    Ok(id)
}

/// Publishes a prepared blob using its already-computed digest.
///
/// # Errors
/// Returns [`BlobError::TooLarge`] above the cap or [`BlobError::Io`] on a failed publish.
pub(crate) fn put_prepared(dir: &Path, pending: PendingBlob) -> Result<(), BlobError> {
    let (id, bytes) = pending.into_parts();
    publish_with_id(dir, id, &bytes)
}

fn publish_with_id(dir: &Path, id: BlobId, bytes: &[u8]) -> Result<(), BlobError> {
    check_blob_size(bytes.len())?;
    let dest = dir.join(id.to_string());
    if existing_blob(&dest)? {
        sync_dir(dir)?;
        return Ok(());
    }
    copy_publish(&mut &bytes[..], dir, &dest).map_err(|error| gone_if_session_lost(dir, error))
}

/// A publish that fails `NotFound` under an absent session directory means the session is gone.
fn gone_if_session_lost(dir: &Path, error: BlobError) -> BlobError {
    let session_absent = dir.parent().is_some_and(|session| !session.exists());
    match error {
        BlobError::Io { source } if session_absent && source.kind() == io::ErrorKind::NotFound => {
            BlobError::Gone
        }
        other => other,
    }
}

fn check_blob_size(length: usize) -> Result<(), BlobError> {
    let bytes = u64::try_from(length).unwrap_or(u64::MAX);
    if bytes > MAX_BLOB {
        return Err(BlobError::TooLarge { bytes });
    }
    Ok(())
}

fn create_temp(dir: &Path) -> Result<(PathBuf, File), BlobError> {
    loop {
        let tmp = dir.join(format!(".tmp-{}", util::random_hex()));
        let mut options = util::open_options();
        options.write(true).create_new(true);
        util::with_mode(&mut options, MODE_FILE);
        match options.open(&tmp) {
            Ok(file) => return Ok((tmp, file)),
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => return Err(io_blob(source)),
        }
    }
}

fn existing_blob(path: &Path) -> Result<bool, BlobError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(source) => return Err(io_blob(source)),
    };
    if !metadata.file_type().is_file() {
        return Err(io_blob(io::Error::new(
            io::ErrorKind::InvalidData,
            "published blob is not a regular file",
        )));
    }
    match File::open(path) {
        Ok(_) => Ok(true),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(io_blob(source)),
    }
}

/// Publishes a complete, synced temp at `dest` without replacing an existing file.
/// A valid blob already at `dest` wins and `tmp` is removed.
fn publish_temp(staged_path: &Path, dest: &Path, dir: &Path) -> Result<(), BlobError> {
    let mut temp_guard = TempPath::try_from_path(staged_path).map_err(io_blob)?;
    let removed = loop {
        let PathPersistError { error, path } = match temp_guard.persist_noclobber(dest) {
            Ok(()) => break Ok(()),
            Err(failed) => failed,
        };
        if error.kind() != io::ErrorKind::AlreadyExists {
            return Err(io_blob(error));
        }
        if existing_blob(dest)? {
            break path.close().map_err(io_blob);
        }
        temp_guard = path;
    };
    sync_dir(dir)?;
    removed
}

fn sync_dir(dir: &Path) -> Result<(), BlobError> {
    #[cfg(windows)]
    {
        let _ = dir;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        File::open(dir)
            .and_then(|file| file.sync_all())
            .map_err(io_blob)
    }
}

/// Reads a blob. `Gone` when the session directory is absent, `NotFound` when only the digest is.
///
/// # Errors
/// Returns [`BlobError::Gone`], [`BlobError::NotFound`], or [`BlobError::Io`].
pub(crate) fn read(session_dir: &Path, id: &BlobId) -> Result<Vec<u8>, BlobError> {
    let path = session_dir.join("blobs").join(id.to_string());
    match fs::read(&path) {
        Ok(bytes) if BlobId::from_bytes(&bytes) == *id => Ok(bytes),
        Ok(_) => Err(io_blob(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("blob {id} does not match its digest; the file is damaged"),
        ))),
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            match fs::metadata(session_dir) {
                Ok(_) => Err(BlobError::NotFound { id: *id }),
                Err(session) if session.kind() == io::ErrorKind::NotFound => Err(BlobError::Gone),
                Err(session) => Err(io_blob(session)),
            }
        }
        Err(source) => Err(io_blob(source)),
    }
}

/// Hard-links or copies each named digest from the source blob directory into
/// the target blob directory. Existing destination blobs and same-file links
/// are left unchanged.
///
/// # Errors
/// Returns [`BlobError::NotFound`] for a missing digest, [`BlobError::Gone`]
/// when the source session is absent, [`BlobError::TooLarge`] above the cap,
/// or [`BlobError::Io`] when a filesystem operation fails.
pub(crate) fn share(
    from: &Path,
    to: &Path,
    ids: impl IntoIterator<Item = BlobId>,
) -> Result<(), BlobError> {
    for id in ids {
        let source = from.join(id.to_string());
        let dest = to.join(id.to_string());
        let mut source_file =
            File::open(&source).map_err(|error| missing_source(from, &id, error))?;
        let metadata = source_file.metadata().map_err(io_blob)?;
        if !metadata.is_file() {
            return Err(io_blob(io::Error::new(
                io::ErrorKind::InvalidData,
                "source blob is not a regular file",
            )));
        }
        if metadata.len() > MAX_BLOB {
            return Err(BlobError::TooLarge {
                bytes: metadata.len(),
            });
        }

        match fs::hard_link(&source, &dest) {
            Ok(()) => {
                drop(source_file);
                sync_dir(to)?;
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if existing_blob(&dest)? {
                    sync_dir(to)?;
                    continue;
                }
            }
            Err(_) => {}
        }

        copy_publish(&mut source_file, to, &dest)?;
    }
    Ok(())
}

/// Streams `source` into an exclusive temp under `dir`, syncs it, then publishes it
/// at `dest` without replacement. A failed copy removes the temp and never exposes `dest`.
fn copy_publish(source: &mut impl io::Read, dir: &Path, dest: &Path) -> Result<(), BlobError> {
    let (tmp, mut target_file) = create_temp(dir)?;
    let result = (|| {
        io::copy(source, &mut target_file).map_err(io_blob)?;
        target_file.sync_all().map_err(io_blob)?;
        drop(target_file);
        publish_temp(&tmp, dest, dir)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn missing_source(from: &Path, id: &BlobId, source: io::Error) -> BlobError {
    if source.kind() != io::ErrorKind::NotFound {
        return io_blob(source);
    }
    let session_dir = from
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    match fs::metadata(session_dir) {
        Ok(_) => BlobError::NotFound { id: *id },
        Err(session) if session.kind() == io::ErrorKind::NotFound => BlobError::Gone,
        Err(session) => io_blob(session),
    }
}

pub(crate) fn io_blob(source: io::Error) -> BlobError {
    BlobError::Io {
        source: Box::new(source),
    }
}

/// Decodes standard base64, ignoring ASCII whitespace. `None` when the alphabet is wrong.
#[must_use]
#[cfg(test)]
pub(crate) fn decode_base64(text: &str) -> Option<Vec<u8>> {
    decode_base64_inner(text, Base64Validation::Compatible)
}

fn decode_base64_strict(text: &str) -> Option<Vec<u8>> {
    decode_base64_inner(text, Base64Validation::Strict)
}

#[derive(Clone, Copy)]
enum Base64Validation {
    #[cfg(test)]
    Compatible,
    Strict,
}

fn decode_base64_inner(text: &str, validation: Base64Validation) -> Option<Vec<u8>> {
    fn value(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes: Vec<u8> = text
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let chunk_count = bytes.len() / 4;
    for (index, chunk) in bytes.chunks(4).enumerate() {
        let (a, b, c, d) = (chunk[0], chunk[1], chunk[2], chunk[3]);
        let pad = u8::from(c == b'=') + u8::from(d == b'=');
        if (pad == 1 && c == b'=')
            || (matches!(validation, Base64Validation::Strict)
                && pad > 0
                && index + 1 != chunk_count)
        {
            return None;
        }
        let av = value(a)?;
        let bv = value(b)?;
        let cv = if c == b'=' { 0 } else { value(c)? };
        let dv = if d == b'=' { 0 } else { value(d)? };
        if matches!(validation, Base64Validation::Strict)
            && ((pad == 2 && bv & 0x0f != 0) || (pad == 1 && cv & 0x03 != 0))
        {
            return None;
        }
        out.push((av << 2) | (bv >> 4));
        if pad < 2 {
            out.push((bv << 4) | (cv >> 2));
        }
        if pad == 0 {
            out.push((cv << 6) | dv);
        }
    }
    Some(out)
}

/// Collects blob digests named by a record's parts.
#[must_use]
pub(crate) fn named_blobs(record: &dal_core::Record) -> Vec<BlobId> {
    let mut ids = Vec::new();
    let Some(entry) = record.entry() else {
        return ids;
    };
    match &entry.kind {
        dal_core::EntryKind::User { parts }
        | dal_core::EntryKind::ToolResult { parts, .. }
        | dal_core::EntryKind::Compaction { parts, .. } => {
            for part in parts {
                push_part(part, &mut ids);
            }
        }
        _ => {}
    }
    ids
}

fn push_part(part: &dal_core::JournalPart, ids: &mut Vec<BlobId>) {
    let hex = match part {
        dal_core::JournalPart::TextBlob { blob, .. }
        | dal_core::JournalPart::ImageBlob { blob, .. }
        | dal_core::JournalPart::Blob { blob, .. } => blob.as_ref(),
        dal_core::JournalPart::Text { .. } | dal_core::JournalPart::Image { .. } => return,
    };
    if let Ok(id) = BlobId::parse(hex) {
        ids.push(id);
    }
}

/// Prepares inline blob parts without filesystem I/O. The record names each
/// prepared digest and returned values carry the bytes to publish or store.
/// Publish/store every pending blob before durably writing the transformed record.
///
/// # Errors
/// Returns [`BlobError::TooLarge`] above the cap or [`BlobError::Io`] for invalid base64.
pub(crate) fn prepare_record(record: &mut dal_core::Record) -> Result<Vec<PendingBlob>, BlobError> {
    let Some(parts) = record_parts_mut(record) else {
        return Ok(Vec::new());
    };
    let images = validate_parts(parts)?;
    Ok(prepare_parts(parts, images))
}

#[cfg(test)]
/// Replaces inline parts at or above [`INLINE_LIMIT`] with blob parts, publishing first.
///
/// # Errors
/// Returns [`BlobError`] when a spilled value is over the cap or the publish fails.
pub(crate) fn spill_record(record: &mut dal_core::Record, dir: &Path) -> Result<(), BlobError> {
    let Some(parts) = record_parts_mut(record) else {
        return Ok(());
    };
    for part in parts {
        spill_part(part, dir)?;
    }
    Ok(())
}

fn record_parts_mut(record: &mut dal_core::Record) -> Option<&mut [dal_core::JournalPart]> {
    let entry = match record {
        dal_core::Record::User(entry) | dal_core::Record::ToolResult(entry) => entry,
        dal_core::Record::Compaction(entry) => {
            return match &mut entry.kind {
                dal_core::EntryKind::Compaction { parts, .. } => Some(parts.as_mut_slice()),
                _ => None,
            };
        }
        _ => return None,
    };
    match &mut entry.kind {
        dal_core::EntryKind::User { parts } | dal_core::EntryKind::ToolResult { parts, .. } => {
            Some(parts.as_mut_slice())
        }
        _ => None,
    }
}

fn validate_parts(parts: &[dal_core::JournalPart]) -> Result<Vec<Option<Vec<u8>>>, BlobError> {
    let mut images = Vec::with_capacity(parts.len());
    for part in parts {
        match part {
            dal_core::JournalPart::Text { text } => {
                check_blob_size(text.len())?;
                images.push(None);
            }
            dal_core::JournalPart::Image { base64, .. } => {
                let raw = decode_base64_strict(base64).ok_or_else(invalid_base64)?;
                check_blob_size(raw.len())?;
                images.push(Some(raw));
            }
            dal_core::JournalPart::TextBlob { .. }
            | dal_core::JournalPart::ImageBlob { .. }
            | dal_core::JournalPart::Blob { .. } => images.push(None),
        }
    }
    Ok(images)
}

fn prepare_parts(
    parts: &mut [dal_core::JournalPart],
    images: Vec<Option<Vec<u8>>>,
) -> Vec<PendingBlob> {
    let mut pending = Vec::new();
    for (part, image) in parts.iter_mut().zip(images) {
        match part {
            dal_core::JournalPart::Text { text } if text.len() >= INLINE_LIMIT => {
                let old = std::mem::replace(part, empty_text_part());
                let dal_core::JournalPart::Text { text } = old else {
                    unreachable!("the selected part is inline text");
                };
                let blob = PendingBlob::new(String::from(text).into_bytes());
                *part = text_blob_part(blob.id, blob.bytes.len());
                pending.push(blob);
            }
            dal_core::JournalPart::Image { .. } => {
                let Some(raw) = image else {
                    unreachable!("image base64 was validated before transformation");
                };
                if raw.len() < INLINE_LIMIT {
                    continue;
                }
                let blob = PendingBlob::new(raw);
                replace_with_blob(part, blob.id, blob.bytes.len());
                pending.push(blob);
            }
            dal_core::JournalPart::Text { .. }
            | dal_core::JournalPart::TextBlob { .. }
            | dal_core::JournalPart::ImageBlob { .. }
            | dal_core::JournalPart::Blob { .. } => {}
        }
    }
    pending
}

#[cfg(test)]
fn spill_part(part: &mut dal_core::JournalPart, dir: &Path) -> Result<(), BlobError> {
    let (id, length) = match part {
        dal_core::JournalPart::Text { text } => {
            if text.len() < INLINE_LIMIT {
                return Ok(());
            }
            check_blob_size(text.len())?;
            let id = BlobId::from_bytes(text.as_bytes());
            publish_with_id(dir, id, text.as_bytes())?;
            (id, text.len())
        }
        dal_core::JournalPart::Image { base64, .. } => {
            let Some(raw) = decode_base64(base64) else {
                return Ok(());
            };
            if raw.len() < INLINE_LIMIT {
                return Ok(());
            }
            check_blob_size(raw.len())?;
            let id = BlobId::from_bytes(&raw);
            publish_with_id(dir, id, &raw)?;
            (id, raw.len())
        }
        dal_core::JournalPart::TextBlob { .. }
        | dal_core::JournalPart::ImageBlob { .. }
        | dal_core::JournalPart::Blob { .. } => return Ok(()),
    };
    replace_with_blob(part, id, length);
    Ok(())
}

fn replace_with_blob(part: &mut dal_core::JournalPart, id: BlobId, length: usize) {
    let old = std::mem::replace(part, empty_text_part());
    *part = blob_part(old, id, length);
}
fn empty_text_part() -> dal_core::JournalPart {
    dal_core::JournalPart::Text {
        text: String::new().into(),
    }
}

fn blob_part(part: dal_core::JournalPart, id: BlobId, length: usize) -> dal_core::JournalPart {
    let blob = id.to_string().into();
    let bytes = u64::try_from(length).unwrap_or(u64::MAX);
    match part {
        dal_core::JournalPart::Text { .. } => dal_core::JournalPart::TextBlob { blob, bytes },
        dal_core::JournalPart::Image { mime, .. } => {
            dal_core::JournalPart::ImageBlob { mime, blob, bytes }
        }
        _ => unreachable!("only inline text and image parts are transformed"),
    }
}

fn text_blob_part(id: BlobId, length: usize) -> dal_core::JournalPart {
    dal_core::JournalPart::TextBlob {
        blob: id.to_string().into(),
        bytes: u64::try_from(length).unwrap_or(u64::MAX),
    }
}

fn invalid_base64() -> BlobError {
    io_blob(io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid base64 image data",
    ))
}
#[cfg(test)]
mod tests {
    use std::{io::Write, process};

    use dal_core::{Entry, EntryId, EntryKind, JournalPart, Record};

    use super::*;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            loop {
                let path = std::env::temp_dir().join(format!(
                    "dal-store-blob-{}-{}",
                    process::id(),
                    util::random_hex()
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => panic!("create test directory: {error}"),
                }
            }
        }

        fn blobs(&self) -> PathBuf {
            let path = self.0.join("blobs");
            fs::create_dir(&path).expect("create blobs directory");
            path
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn user_record(parts: Vec<JournalPart>) -> Record {
        Record::User(Entry {
            id: EntryId::new(std::num::NonZeroU64::MIN),
            parent: None,
            at: jiff::Timestamp::UNIX_EPOCH,
            kind: EntryKind::User { parts },
        })
    }

    fn text_part(text: String) -> JournalPart {
        JournalPart::Text { text: text.into() }
    }

    #[test]
    fn spill_record_preserves_inline_boundary_and_publishes_blob_boundary() {
        let temp = TestDir::new();
        let blobs = temp.blobs();
        let inline = "i".repeat(INLINE_LIMIT - 1);
        let blob = "b".repeat(INLINE_LIMIT);
        let blob_id = BlobId::from_bytes(blob.as_bytes());
        let blob_hex = blob_id.to_string();
        let inline_bytes = u64::try_from(INLINE_LIMIT).expect("inline limit fits in u64");
        let mut record = user_record(vec![text_part(inline), text_part(blob.clone())]);

        spill_record(&mut record, &blobs).expect("spill record");

        let Record::User(entry) = record else {
            panic!("user record remains a user record");
        };
        let EntryKind::User { parts } = entry.kind else {
            panic!("user record keeps its kind");
        };
        assert!(matches!(
            parts.first(),
            Some(JournalPart::Text { text }) if text.len() == INLINE_LIMIT - 1
        ));
        assert!(matches!(
            parts.get(1),
            Some(JournalPart::TextBlob { blob: name, bytes })
                if name.as_ref() == blob_hex.as_str() && *bytes == inline_bytes
        ));
        assert_eq!(
            fs::read(blobs.join(blob_id.to_string())).expect("read blob"),
            blob.as_bytes()
        );

        let files_before = fs::read_dir(&blobs).expect("read blobs").count();
        assert_eq!(
            put(&blobs, blob.as_bytes()).expect("put existing bytes"),
            blob_id
        );
        assert_eq!(
            fs::read_dir(&blobs).expect("read blobs").count(),
            files_before
        );
    }

    #[test]
    fn put_accepts_the_maximum_blob_size() {
        let temp = TestDir::new();
        let blobs = temp.blobs();
        let max_len = usize::try_from(MAX_BLOB).expect("maximum blob size fits in usize");
        let bytes = vec![0x5a; max_len];

        let id = put(&blobs, &bytes).expect("maximum-size blob is accepted");

        assert_eq!(
            fs::metadata(blobs.join(id.to_string()))
                .expect("published blob")
                .len(),
            MAX_BLOB
        );
    }

    #[test]
    fn put_rejects_oversize_before_writing() {
        let temp = TestDir::new();
        let blobs = temp.blobs();
        let max_len = usize::try_from(MAX_BLOB).expect("maximum blob size fits in usize");
        let bytes = vec![0; max_len + 1];
        let expected_len = u64::try_from(bytes.len()).expect("test input fits in u64");

        let result = put(&blobs, &bytes);

        assert!(matches!(
            result,
            Err(BlobError::TooLarge { bytes }) if bytes == expected_len
        ));
        assert_eq!(fs::read_dir(&blobs).expect("read blobs").count(), 0);
    }

    #[test]
    fn put_never_replaces_an_existing_digest_file() {
        let temp = TestDir::new();
        let blobs = temp.blobs();
        let bytes = b"intended bytes";
        let id = BlobId::from_bytes(bytes);
        let published = b"already-published bytes";
        fs::write(blobs.join(id.to_string()), published).expect("seed existing digest");

        assert_eq!(put(&blobs, bytes).expect("put existing digest"), id);
        assert_eq!(
            fs::read(blobs.join(id.to_string())).expect("read existing digest"),
            published
        );
    }

    #[test]
    fn concurrent_puts_publish_one_complete_digest_file() {
        let temp = TestDir::new();
        let blobs = temp.blobs();
        let bytes = vec![0x42; 1024 * 1024];
        let expected = BlobId::from_bytes(&bytes);

        let start = std::sync::Barrier::new(8);
        let ids = std::thread::scope(|scope| {
            let writers = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        start.wait();
                        put(&blobs, &bytes)
                    })
                })
                .collect::<Vec<_>>();
            writers
                .into_iter()
                .map(|writer| writer.join().expect("writer thread").expect("put blob"))
                .collect::<Vec<_>>()
        });

        assert!(ids.iter().all(|id| *id == expected));
        assert_eq!(
            fs::read(blobs.join(expected.to_string())).expect("read concurrent blob"),
            bytes
        );
        assert_eq!(fs::read_dir(&blobs).expect("read blobs").count(), 1);
    }

    #[test]
    fn copy_publication_losing_a_race_keeps_the_winner_and_removes_its_temp() {
        let temp = TestDir::new();
        let blobs = temp.blobs();
        let id = BlobId::from_bytes(b"copied bytes");
        let dest = blobs.join(id.to_string());
        let winner = b"racing writer's bytes";
        fs::write(&dest, winner).expect("seed racing winner");
        let (staged_file, mut file) = create_temp(&blobs).expect("create copy temp");
        file.write_all(b"copied bytes").expect("write copy temp");
        file.sync_all().expect("sync copy temp");
        drop(file);

        publish_temp(&staged_file, &dest, &blobs).expect("existing digest counts as published");

        assert_eq!(fs::read(&dest).expect("read winner"), winner);
        assert!(!staged_file.exists());
        assert_eq!(fs::read_dir(&blobs).expect("read blobs").count(), 1);
    }

    #[test]
    fn failed_copy_never_exposes_the_digest_or_leaves_a_temp() {
        struct FailsAfterPrefix(usize);

        impl io::Read for FailsAfterPrefix {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.0 == 0 {
                    return Err(io::Error::other("source read failed"));
                }
                let len = buf.len().min(self.0);
                buf[..len].fill(0x61);
                self.0 -= len;
                Ok(len)
            }
        }

        let temp = TestDir::new();
        let blobs = temp.blobs();
        let dest = blobs.join(BlobId::from_bytes(b"never complete").to_string());

        let result = copy_publish(&mut FailsAfterPrefix(4096), &blobs, &dest);

        assert!(matches!(
            result,
            Err(BlobError::Io { source }) if source.kind() == io::ErrorKind::Other
        ));
        assert!(!dest.exists());
        assert_eq!(fs::read_dir(&blobs).expect("read blobs").count(), 0);
    }

    #[test]
    fn share_reports_missing_source_digest_in_live_session() {
        let temp = TestDir::new();
        let session = temp.0.join("source-session");
        let from = session.join("blobs");
        let to = temp.0.join("to");
        fs::create_dir(&session).expect("create source session");
        fs::create_dir(&to).expect("create target directory");
        let id = BlobId::from_bytes(b"missing");

        let result = share(&from, &to, [id]);

        assert!(matches!(
            result,
            Err(BlobError::NotFound { id: found }) if found == id
        ));
        assert_eq!(fs::read_dir(&to).expect("read target").count(), 0);
    }

    #[test]
    fn share_reports_gone_source_session() {
        let temp = TestDir::new();
        let from = temp.0.join("deleted-session").join("blobs");
        let to = temp.0.join("to");
        fs::create_dir(&to).expect("create target directory");
        let id = BlobId::from_bytes(b"missing");

        assert!(matches!(share(&from, &to, [id]), Err(BlobError::Gone)));
    }

    #[test]
    fn read_distinguishes_missing_blob_from_deleted_session() {
        let temp = TestDir::new();
        let session = temp.0.join("session");
        let blobs = session.join("blobs");
        fs::create_dir_all(&blobs).expect("create session blobs");
        let id = BlobId::from_bytes(b"missing");

        assert!(matches!(
            read(&session, &id),
            Err(BlobError::NotFound { id: found }) if found == id
        ));
        fs::remove_dir_all(&session).expect("delete session");
        assert!(matches!(read(&session, &id), Err(BlobError::Gone)));
    }

    #[test]
    fn prepare_record_holds_bytes_until_put_prepared() {
        let temp = TestDir::new();
        let blobs = temp.blobs();
        let text = "t".repeat(INLINE_LIMIT);
        let text_id = BlobId::from_bytes(text.as_bytes());
        let image_groups = INLINE_LIMIT.div_ceil(3);
        let image_bytes = [1_u8, 2, 3].repeat(image_groups);
        let image_base64 = "AQID".repeat(image_groups);
        let image_id = BlobId::from_bytes(&image_bytes);
        let text_hex = text_id.to_string();
        let image_hex = image_id.to_string();
        let text_len = u64::try_from(INLINE_LIMIT).expect("inline limit fits in u64");
        let image_len = u64::try_from(image_bytes.len()).expect("image length fits in u64");
        let mut record = user_record(vec![
            text_part(text.clone()),
            JournalPart::Image {
                mime: "image/png".into(),
                base64: image_base64.into(),
            },
        ]);

        let pending = prepare_record(&mut record).expect("prepare inline blobs");

        assert_eq!(fs::read_dir(&blobs).expect("read blobs").count(), 0);
        assert_eq!(pending.len(), 2);
        let mut pending = pending.into_iter();
        let text_blob = pending.next().expect("prepared text");
        let image_blob = pending.next().expect("prepared image");
        assert!(pending.next().is_none());
        assert_eq!(text_blob.id(), text_id);
        assert_eq!(text_blob.bytes(), text.as_bytes());
        assert_eq!(image_blob.id(), image_id);
        assert_eq!(image_blob.bytes(), image_bytes.as_slice());
        assert!(matches!(
            &record,
            Record::User(Entry {
                kind: EntryKind::User { parts },
                ..
            }) if matches!(
                parts.as_slice(),
                [
                    JournalPart::TextBlob { blob, bytes: text_length },
                    JournalPart::ImageBlob { mime, blob: image, bytes: image_length }
                ] if blob.as_ref() == text_hex.as_str()
                    && *text_length == text_len
                    && mime.as_ref() == "image/png"
                    && image.as_ref() == image_hex.as_str()
                    && *image_length == image_len
            )
        ));

        put_prepared(&blobs, text_blob).expect("publish prepared text");
        put_prepared(&blobs, image_blob).expect("publish prepared image");

        assert_eq!(
            fs::read(blobs.join(text_id.to_string()))
                .expect("read text")
                .as_slice(),
            text.as_bytes()
        );
        assert_eq!(
            fs::read(blobs.join(image_id.to_string())).expect("read image"),
            image_bytes
        );
    }

    #[test]
    fn prepare_record_rejects_invalid_base64_without_mutating_record() {
        let temp = TestDir::new();
        let blobs = temp.blobs();
        let mut record = user_record(vec![JournalPart::Image {
            mime: "image/png".into(),
            base64: "AQ==AAAA".into(),
        }]);
        let before = record.clone();

        let result = prepare_record(&mut record);

        assert!(matches!(
            result,
            Err(BlobError::Io { source }) if source.kind() == io::ErrorKind::InvalidData
        ));
        assert_eq!(record, before);
        assert_eq!(fs::read_dir(&blobs).expect("read blobs").count(), 0);
    }

    #[test]
    fn spill_record_keeps_unrecognized_image_inline() {
        let temp = TestDir::new();
        let blobs = temp.blobs();
        let mut record = user_record(vec![JournalPart::Image {
            mime: "image/png".into(),
            base64: "%%%%".into(),
        }]);
        let before = record.clone();

        spill_record(&mut record, &blobs).expect("unrecognized image remains inline");

        assert_eq!(record, before);
        assert_eq!(fs::read_dir(&blobs).expect("read blobs").count(), 0);
    }

    #[test]
    fn prepare_record_rejects_oversize_without_mutating_record() {
        let max_len = usize::try_from(MAX_BLOB).expect("maximum blob size fits in usize");
        let text = "x".repeat(max_len + 1);
        let mut record = user_record(vec![text_part(text)]);
        let expected_len = u64::try_from(max_len + 1).expect("test input fits in u64");

        let result = prepare_record(&mut record);

        assert!(matches!(
            result,
            Err(BlobError::TooLarge { bytes }) if bytes == expected_len
        ));
        assert!(matches!(
            &record,
            Record::User(Entry {
                kind: EntryKind::User { parts },
                ..
            }) if matches!(parts.first(), Some(JournalPart::Text { .. }))
        ));
    }

    #[test]
    fn generic_blob_part_is_named_and_shared() {
        let temp = TestDir::new();
        let source_session = temp.0.join("source");
        let from = source_session.join("blobs");
        let target_session = temp.0.join("target");
        let to = target_session.join("blobs");
        fs::create_dir_all(&from).expect("create source blobs");
        fs::create_dir_all(&to).expect("create target blobs");
        let pdf =
            b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n";
        let id = put(&from, pdf).expect("publish pdf blob");
        let mut record = user_record(vec![JournalPart::Blob {
            mime: "application/pdf".into(),
            blob: id.to_string().into(),
            bytes: u64::try_from(pdf.len()).expect("pdf length fits in u64"),
        }]);
        let pending = prepare_record(&mut record).expect("existing generic blob is unchanged");
        assert!(pending.is_empty());

        spill_record(&mut record, &from).expect("stored blob is left as is");
        let named = named_blobs(&record);
        share(&from, &to, named.iter().copied()).expect("share pdf blob");

        assert_eq!(named, vec![id]);
        assert_eq!(read(&target_session, &id).expect("read shared pdf"), pdf);
        fs::remove_file(to.join(id.to_string())).expect("remove shared pdf");
        assert!(matches!(
            read(&target_session, &id),
            Err(BlobError::NotFound { id: found }) if found == id
        ));
        fs::remove_dir_all(&target_session).expect("delete target session");
        assert!(matches!(read(&target_session, &id), Err(BlobError::Gone)));
    }

    #[test]
    fn later_spill_failure_leaves_only_published_references() {
        let temp = TestDir::new();
        let blobs = temp.blobs();
        let first = "a".repeat(INLINE_LIMIT);
        let second = "b".repeat(INLINE_LIMIT);
        let first_id = BlobId::from_bytes(first.as_bytes());
        let second_id = BlobId::from_bytes(second.as_bytes());
        fs::create_dir(blobs.join(second_id.to_string()))
            .expect("block publication of the second digest");
        let mut record = user_record(vec![text_part(first.clone()), text_part(second)]);

        let result = spill_record(&mut record, &blobs);

        assert!(matches!(result, Err(BlobError::Io { .. })));
        let named = named_blobs(&record);
        assert_eq!(named, vec![first_id]);
        assert!(named.iter().all(|id| blobs.join(id.to_string()).is_file()));
        assert_eq!(
            fs::read(blobs.join(first_id.to_string())).expect("read first blob"),
            first.as_bytes()
        );
    }

    #[test]
    fn spill_failure_never_names_a_missing_blob() {
        let temp = TestDir::new();
        let missing_dir = temp.0.join("missing-blobs");
        let original = user_record(vec![text_part("x".repeat(INLINE_LIMIT))]);
        let mut record = original;

        let result = spill_record(&mut record, &missing_dir);

        assert!(matches!(result, Err(BlobError::Io { .. })));
        assert!(matches!(
            &record,
            Record::User(Entry {
                kind: EntryKind::User { parts },
                ..
            }) if matches!(parts.first(), Some(JournalPart::Text { .. }))
        ));
        assert!(named_blobs(&record).is_empty());
    }
}
