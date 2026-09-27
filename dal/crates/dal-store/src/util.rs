//! Path keys, atomic publication, and session-name checks.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

use unicode_segmentation::UnicodeSegmentation;

use crate::error::StoreError;

/// Mode of journal, lock, info, and sidecar files.
pub(crate) const MODE_FILE: u32 = 0o600;
/// Mode of session, blob, and sidecar directories.
pub(crate) const MODE_DIR: u32 = 0o700;
/// Maximum session-name length in grapheme clusters.
const NAME_MAX: usize = 64;

/// The workspace directory name: cleaned base, cut to 32 bytes, plus 12 hex digits.
#[must_use]
pub fn workspace_key(path: &Path) -> String {
    let base = path.file_name().map_or(&b""[..], |name| name.as_encoded_bytes());
    let mut cleaned = String::new();
    for byte in base {
        if cleaned.len() == 32 {
            break;
        }
        let ch = if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-') {
            char::from(*byte)
        } else {
            '_'
        };
        cleaned.push(ch);
    }
    if cleaned.is_empty() {
        cleaned.push_str("root");
    }
    let digest = blake3::hash(path.as_os_str().as_encoded_bytes());
    let hex = digest.to_hex();
    format!("{cleaned}-{}", &hex[..12])
}

pub(crate) fn create_dir(path: &Path) -> Result<(), StoreError> {
    fs::create_dir_all(path).map_err(|source| io_err(path, source))?;
    set_mode(path, MODE_DIR)
}

pub(crate) fn set_mode(path: &Path, mode: u32) -> Result<(), StoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .map_err(|source| io_err(path, source))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}

pub(crate) fn open_options() -> OpenOptions {
    OpenOptions::new()
}

#[cfg(unix)]
pub(crate) fn with_mode(options: &mut OpenOptions, mode: u32) {
    use std::os::unix::fs::OpenOptionsExt;
    let _ = options.mode(mode);
}

#[cfg(not(unix))]
pub(crate) fn with_mode(options: &mut OpenOptions, mode: u32) {
    let _ = (options, mode);
}

/// Writes `bytes` to a temp file, syncs it, renames it over `path`, and syncs the directory.
///
/// # Errors
/// Returns [`StoreError::Io`] when a step fails. A failed attempt leaves the target unchanged.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<(), StoreError> {
    publish(path, bytes, mode, false)
}

/// Like [`write_atomic`], but fails when `path` already exists.
///
/// # Errors
/// Returns [`StoreError::Io`] when the target exists or a step fails.
pub fn write_atomic_new(path: &Path, bytes: &[u8], mode: u32) -> Result<(), StoreError> {
    publish(path, bytes, mode, true)
}

fn publish(path: &Path, bytes: &[u8], mode: u32, exclusive: bool) -> Result<(), StoreError> {
    if exclusive && path.exists() {
        return Err(io_err(
            path,
            io::Error::new(io::ErrorKind::AlreadyExists, "target exists"),
        ));
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = dir.join(format!(".{name}.tmp-{}", random_hex(8)?));
    let write_result = (|| {
        let mut options = open_options();
        options.write(true).create(true).truncate(true);
        with_mode(&mut options, mode);
        let mut file = options.open(&tmp).map_err(|source| io_err(&tmp, source))?;
        file.write_all(bytes).map_err(|source| io_err(&tmp, source))?;
        file.sync_all().map_err(|source| io_err(&tmp, source))?;
        fs::rename(&tmp, path).map_err(|source| io_err(path, source))?;
        sync_dir(dir)
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    write_result
}

pub(crate) fn sync_dir(dir: &Path) -> Result<(), StoreError> {
    #[cfg(windows)]
    {
        let _ = dir;
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        File::open(dir)
            .and_then(|file| file.sync_all())
            .map_err(|source| io_err(dir, source))
    }
}

pub(crate) fn random_hex(nbytes: usize) -> Result<String, StoreError> {
    let mut file = File::open("/dev/urandom").map_err(|source| io_err(Path::new("/dev/urandom"), source))?;
    let mut bytes = vec![0_u8; nbytes];
    io::Read::read_exact(&mut file, &mut bytes)
        .map_err(|source| io_err(Path::new("/dev/urandom"), source))?;
    Ok(hex_encode(&bytes))
}

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

pub(crate) fn io_err(path: &Path, source: io::Error) -> StoreError {
    StoreError::Io {
        path: path.to_path_buf(),
        source: format!("{source}").into(),
    }
}

/// Trims, collapses line-break runs, and checks the session-name rule.
///
/// # Errors
/// Returns [`StoreError::InvalidName`] when the normalized name is empty, too
/// long, contains a control character, or is only `0-9a-f-`.
pub fn normalize_name(raw: &str) -> Result<Box<str>, StoreError> {
    let trimmed = raw.trim();
    let mut out = String::new();
    let mut breaking = false;
    for ch in trimmed.chars() {
        if ch == '\r' || ch == '\n' {
            if !breaking {
                out.push(' ');
                breaking = true;
            }
        } else {
            breaking = false;
            out.push(ch);
        }
    }
    let clusters = out.graphemes(true).count();
    let id_only = !out.is_empty() && out.bytes().all(|byte| byte.is_ascii_hexdigit() || byte == b'-');
    if !(1..=NAME_MAX).contains(&clusters) || out.chars().any(char::is_control) || id_only {
        return Err(StoreError::InvalidName);
    }
    Ok(out.into())
}

pub(crate) fn stamp(ts: jiff::Timestamp) -> String {
    let text = format!("{ts:.3}");
    if text.len() >= 16 {
        format!("{} {}", &text[..10], &text[11..16])
    } else {
        text
    }
}

pub(crate) fn millis(ts: jiff::Timestamp) -> String {
    format!("{ts:.3}")
}

pub(crate) fn temp_tests(label: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/store-tests");
    let path = root.join(format!("{label}-{}", uuid::Uuid::now_v7()));
    let _ = fs::create_dir_all(&path);
    path
}
