//! Content-addressed blobs. A put finishes before any record names the digest.

use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

use dal_core::BlobId;

use crate::{
    error::BlobError,
    util::{self, MODE_FILE},
};

/// Bytes at or above this length leave the journal and become a blob.
pub const INLINE_LIMIT: usize = 16_384;
/// A blob larger than this is rejected.
pub const MAX_BLOB: u64 = 67_108_864;

/// Publishes `bytes` under `dir` if that digest is not already present.
///
/// # Errors
/// Returns [`BlobError::TooLarge`] above the cap, and [`BlobError::Io`] on a failed publish.
pub fn put(dir: &Path, bytes: &[u8]) -> Result<BlobId, BlobError> {
    let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if len > MAX_BLOB {
        return Err(BlobError::TooLarge { bytes: len });
    }
    let id = BlobId::from_bytes(bytes);
    let dest = dir.join(id.to_string());
    if dest.exists() {
        return Ok(id);
    }
    let tmp = dir.join(format!(".tmp-{}", util::random_hex(8).map_err(io_blob)?));
    let published = write_then_rename(&tmp, &dest, dir, bytes);
    if published.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    published?;
    Ok(id)
}

fn write_then_rename(tmp: &Path, dest: &Path, dir: &Path, bytes: &[u8]) -> Result<(), BlobError> {
    let mut options = util::open_options();
    options.write(true).create(true).truncate(true);
    util::with_mode(&mut options, MODE_FILE);
    let mut file = options.open(tmp).map_err(io_blob)?;
    file.write_all(bytes).map_err(io_blob)?;
    file.sync_all().map_err(io_blob)?;
    fs::rename(tmp, dest).map_err(io_blob)?;
    sync_dir(dir)
}

fn sync_dir(dir: &Path) -> Result<(), BlobError> {
    #[cfg(windows)]
    {
        let _ = dir;
        return Ok(());
    }
    #[cfg(not(windows))]
    File::open(dir)
        .and_then(|file| file.sync_all())
        .map_err(io_blob)
}

/// Reads a blob. `Gone` when the session directory is absent, `NotFound` when only the digest is.
///
/// # Errors
/// Returns [`BlobError::Gone`], [`BlobError::NotFound`], or [`BlobError::Io`].
pub fn read(session_dir: &Path, id: &BlobId) -> Result<Vec<u8>, BlobError> {
    if !session_dir.exists() {
        return Err(BlobError::Gone);
    }
    let path = session_dir.join("blobs").join(id.to_string());
    match fs::read(&path) {
        Ok(bytes) => Ok(bytes),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Err(BlobError::NotFound {
            hex: id.to_string().into(),
        }),
        Err(source) => Err(io_blob(source)),
    }
}

/// Hard-links every named digest from `from` into `to`. A same-file link is a no-op.
///
/// # Errors
/// Returns [`BlobError::Io`] when a digest file cannot be linked or copied.
pub fn share(
    from: &Path,
    to: &Path,
    ids: impl IntoIterator<Item = BlobId>,
) -> Result<(), BlobError> {
    for id in ids {
        let source = from.join(id.to_string());
        let dest = to.join(id.to_string());
        if dest.exists() || !source.exists() {
            continue;
        }
        if fs::hard_link(&source, &dest).is_err() {
            fs::copy(&source, &dest).map_err(io_blob)?;
        }
    }
    Ok(())
}

pub(crate) fn io_blob(source: impl std::fmt::Display) -> BlobError {
    BlobError::Io {
        source: format!("{source}").into(),
    }
}

/// Decodes standard base64, ignoring ASCII whitespace. `None` when the alphabet is wrong.
#[must_use]
pub fn decode_base64(text: &str) -> Option<Vec<u8>> {
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
    let bytes: Vec<u8> = text.bytes().filter(|byte| !byte.is_ascii_whitespace()).collect();
    if bytes.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let (a, b, c, d) = (chunk[0], chunk[1], chunk[2], chunk[3]);
        let pad = u8::from(c == b'=') + u8::from(d == b'=');
        if pad == 1 && c == b'=' {
            return None;
        }
        let av = value(a)?;
        let bv = value(b)?;
        let cv = if c == b'=' { 0 } else { value(c)? };
        let dv = if d == b'=' { 0 } else { value(d)? };
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
pub fn named_blobs(record: &dal_core::Record) -> Vec<BlobId> {
    let mut ids = Vec::new();
    let Some(entry) = record.entry() else {
        return ids;
    };
    match &entry.kind {
        dal_core::EntryKind::User { parts } | dal_core::EntryKind::ToolResult { parts, .. } => {
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
        | dal_core::JournalPart::ImageBlob { blob, .. } => blob.as_ref(),
        _ => return,
    };
    if let Ok(id) = BlobId::parse(hex) {
        ids.push(id);
    }
}

/// Replaces inline parts at or above [`INLINE_LIMIT`] with blob parts, publishing first.
///
/// # Errors
/// Returns [`BlobError`] when a spilled value is over the cap or the publish fails.
pub fn spill_record(record: &mut dal_core::Record, dir: &Path) -> Result<(), BlobError> {
    match record {
        dal_core::Record::User(entry) | dal_core::Record::ToolResult(entry) => {
            spill_parts(&mut entry.kind, dir)
        }
        _ => Ok(()),
    }
}

fn spill_parts(kind: &mut dal_core::EntryKind, dir: &Path) -> Result<(), BlobError> {
    let parts = match kind {
        dal_core::EntryKind::User { parts } | dal_core::EntryKind::ToolResult { parts, .. } => parts,
        _ => return Ok(()),
    };
    for part in parts.iter_mut() {
        spill_part(part, dir)?;
    }
    Ok(())
}

fn spill_part(part: &mut dal_core::JournalPart, dir: &Path) -> Result<(), BlobError> {
    let decoded = match part {
        dal_core::JournalPart::Text { text } => {
            if text.len() < INLINE_LIMIT {
                return Ok(());
            }
            Some(text.as_bytes().to_vec())
        }
        dal_core::JournalPart::Image { base64, .. } => match decode_base64(base64) {
            Some(raw) if raw.len() >= INLINE_LIMIT => Some(raw),
            _ => return Ok(()),
        },
        _ => return Ok(()),
    };
    let raw = &decoded.expect("decoded part exists");
    let id = put(dir, raw)?;
    let bytes = u64::try_from(raw.len()).unwrap_or(u64::MAX);
    let blob = id.to_string().into();
    let dal_core::JournalPart::Text { text } = std::mem::replace(
        part,
        dal_core::JournalPart::TextBlob { blob, bytes },
    ) else {
        let mime = take_mime();
        *part = dal_core::JournalPart::ImageBlob { mime, blob, bytes };
        return Ok(());
    };
    let _ = text;
    Ok(())
}

fn take_mime() -> Box<str> {
    String::new().into()
}

pub(crate) fn blob_dir(session_dir: &Path) -> PathBuf {
    session_dir.join("blobs")
}
