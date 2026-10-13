//! Path keys, atomic publication, and session-name checks.

use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

#[cfg(not(windows))]
use std::fs::File;

use tempfile::TempPath;
use unicode_segmentation::UnicodeSegmentation;
use uuid::Uuid;

use crate::{error::StoreError, private};

/// Mode of journal, lock, info, and sidecar files.
pub(crate) const MODE_FILE: u32 = 0o600;
/// Permission mode used by private journal, lock, cache, and sidecar files.
///
/// `Mode0600` provides owner read and write access on Unix. Other platforms use
/// their platform defaults.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileMode {
    /// Owner read and write only on Unix; platform defaults elsewhere.
    Mode0600,
}

impl FileMode {
    #[cfg(unix)]
    pub(crate) const fn bits(self) -> u32 {
        match self {
            Self::Mode0600 => MODE_FILE,
        }
    }
}

/// Maximum session-name length in grapheme clusters.
const NAME_MAX: usize = 64;

/// The spelling a path carries for durable identity: every symlink resolved,
/// with a Windows verbatim local root folded back to the drive spelling so
/// stored and displayed paths stay readable. A path that cannot be resolved
/// keeps its given form, so callers stay total.
#[must_use]
pub fn canonical_path(path: &Path) -> PathBuf {
    simplify_verbatim(std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
}

/// Folds `\\?\C:\...` back to `C:\...`; verbatim UNC and device roots stay
/// verbatim because they have no plain spelling.
#[cfg(windows)]
fn simplify_verbatim(path: PathBuf) -> PathBuf {
    use std::path::{Component, Prefix};

    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return path;
    };
    let Prefix::VerbatimDisk(letter) = prefix.kind() else {
        return path;
    };
    let mut simplified = PathBuf::from(format!("{}:\\", char::from(letter)));
    simplified.extend(components);
    simplified
}

#[cfg(not(windows))]
fn simplify_verbatim(path: PathBuf) -> PathBuf {
    path
}

/// The workspace directory name: cleaned base, cut to 32 bytes, plus 12 hex digits.
#[must_use]
pub(crate) fn workspace_key(path: &Path) -> String {
    let base = path
        .file_name()
        .map_or(&b""[..], |name| name.as_encoded_bytes());
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
/// Creates `path` and its parents with owner-only permissions on Unix.
///
/// Existing directories are left unchanged. On other platforms, directory permissions follow
/// the platform defaults.
///
/// # Errors
/// Returns an I/O error when a directory cannot be created.
pub fn create_private_dir_all(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let _ = builder.mode(0o700);
    }
    builder.create(path)
}

/// Writes `bytes` to a temp file, syncs it, renames it over `path`, and syncs the directory.
///
/// # Errors
/// Returns [`StoreError::Io`] when a step fails. Errors before publication leave the previous
/// target unchanged; a directory-sync error can occur after publication.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: FileMode) -> Result<(), StoreError> {
    write_with_temp(path, bytes, mode, Publication::Replace)
}

/// Like [`write_atomic`], but atomically publishes only when `path` does not exist, by a
/// no-replace rename (hard link when the platform lacks one), so a concurrent creator is never
/// overwritten.
///
/// # Errors
/// Returns [`StoreError::Io`] when the target exists or a step fails. A directory-sync error can
/// occur after publication.
pub fn write_atomic_new(path: &Path, bytes: &[u8], mode: FileMode) -> Result<(), StoreError> {
    write_with_temp(path, bytes, mode, Publication::NoReplace)
}

#[derive(Clone, Copy)]
enum Publication {
    Replace,
    NoReplace,
}

fn write_with_temp(
    path: &Path,
    bytes: &[u8],
    mode: FileMode,
    publication: Publication,
) -> Result<(), StoreError> {
    let Some(name) = path.file_name() else {
        return Err(io_err(
            path,
            io::Error::new(io::ErrorKind::InvalidInput, "target path has no file name"),
        ));
    };
    let dir = containing_dir(path);
    let mut temp_name = OsString::from(".");
    temp_name.push(name);
    temp_name.push(format!(".tmp-{}", random_hex()));
    let temp = dir.join(temp_name);
    publish_temp(&temp, path, bytes, mode, publication)
}

fn containing_dir(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

fn publish_temp(
    temp: &Path,
    target: &Path,
    bytes: &[u8],
    mode: FileMode,
    publication: Publication,
) -> Result<(), StoreError> {
    let dir = containing_dir(target);
    let mut file = private::create_new(temp, mode).map_err(|source| io_err(temp, source))?;

    let result = (|| {
        file.write_all(bytes)
            .map_err(|source| io_err(temp, source))?;
        file.sync_all().map_err(|source| io_err(temp, source))?;
        drop(file);
        rename_temp(temp, target, publication)?;
        sync_dir(dir)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

/// Moves the complete, synced `temp` to `target`. `NoReplace` fails with `AlreadyExists` rather
/// than overwrite a target, including one created concurrently.
fn rename_temp(temp: &Path, target: &Path, publication: Publication) -> Result<(), StoreError> {
    match publication {
        Publication::Replace => fs::rename(temp, target).map_err(|source| io_err(target, source)),
        Publication::NoReplace => TempPath::try_from_path(temp)
            .map_err(|source| io_err(temp, source))?
            .persist_noclobber(target)
            // The error owns the unpublished temp; dropping it removes the file.
            .map_err(|failed| io_err(target, failed.error)),
    }
}

/// Windows has no directory-sync door; the `Result` is load-bearing on POSIX.
#[cfg_attr(
    windows,
    expect(
        clippy::unnecessary_wraps,
        reason = "directory sync fails only on POSIX"
    )
)]
pub(crate) fn sync_dir(dir: &Path) -> Result<(), StoreError> {
    #[cfg(windows)]
    {
        let _ = dir;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        File::open(dir)
            .and_then(|file| file.sync_all())
            .map_err(|source| io_err(dir, source))
    }
}

pub(crate) fn random_hex() -> String {
    let uuid = Uuid::new_v4();
    hex_encode(&uuid.as_bytes()[..8])
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
        source: Box::new(source),
    }
}

/// Trims, collapses line-break runs, and checks the session-name rule.
///
/// # Errors
/// Returns [`StoreError::InvalidName`] when the normalized name is empty, too
/// long, contains a control character, or is only `0-9a-f-`.
pub(crate) fn normalize_name(raw: &str) -> Result<Box<str>, StoreError> {
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
    let id_only = !out.is_empty()
        && out
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f' | b'-'));
    if !(1..=NAME_MAX).contains(&clusters) || out.chars().any(char::is_control) || id_only {
        return Err(StoreError::InvalidName);
    }
    Ok(out.into())
}

#[cfg(test)]
mod tests {
    use std::{
        fs, io,
        path::{Path, PathBuf},
        sync::{Arc, Barrier},
        thread,
    };

    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "dal-store-util-{}-{}",
                std::process::id(),
                random_hex()
            ));
            fs::create_dir(&path).expect("create test directory");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn assert_no_temporary_files(dir: &Path) {
        let entries = fs::read_dir(dir)
            .expect("read test directory")
            .map(|entry| entry.expect("read directory entry").file_name())
            .collect::<Vec<_>>();
        assert!(
            entries
                .iter()
                .all(|name| !name.to_string_lossy().contains(".tmp-"))
        );
    }
    #[cfg(unix)]
    #[test]
    fn directory_tree_is_created_with_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new();
        let path = temp.0.join("sessions").join("workspace").join("session");

        create_private_dir_all(&path).expect("create private session tree");

        assert!(path.is_dir());
        assert_eq!(
            fs::metadata(path)
                .expect("stat session directory")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[test]
    fn atomic_replace_writes_complete_bytes_and_private_mode() {
        let dir = TempDir::new();
        let target = dir.0.join("cache");

        write_atomic(&target, b"first", FileMode::Mode0600).expect("write first value");
        assert_eq!(fs::read(&target).expect("read first value"), b"first");
        write_atomic(&target, b"second value", FileMode::Mode0600)
            .expect("replace with second value");
        assert_eq!(
            fs::read(&target).expect("read second value"),
            b"second value"
        );
        assert_no_temporary_files(&dir.0);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&target)
                .expect("stat target")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn atomic_new_preserves_existing_bytes_and_cleans_its_temp() {
        let dir = TempDir::new();
        let target = dir.0.join("created-once");

        write_atomic_new(&target, b"original", FileMode::Mode0600).expect("publish first value");
        let error =
            write_atomic_new(&target, b"replacement", FileMode::Mode0600).expect_err("must fail");
        assert!(matches!(
            error,
            StoreError::Io { source, .. } if source.kind() == io::ErrorKind::AlreadyExists
        ));
        assert_eq!(fs::read(&target).expect("read winner"), b"original");
        assert_no_temporary_files(&dir.0);
    }

    #[test]
    fn failed_temp_creation_preserves_target_and_existing_temp() {
        let dir = TempDir::new();
        let target = dir.0.join("cache");
        let temp = dir.0.join(".cache.tmp-fixed");
        fs::write(&target, b"previous bytes").expect("write prior target");
        fs::write(&temp, b"other temp bytes").expect("write existing temp");

        let error = publish_temp(
            &temp,
            &target,
            b"replacement",
            FileMode::Mode0600,
            Publication::Replace,
        )
        .expect_err("exclusive temp creation must fail");
        assert!(matches!(
            error,
            StoreError::Io { source, .. } if source.kind() == io::ErrorKind::AlreadyExists
        ));
        assert_eq!(
            fs::read(&target).expect("read prior target"),
            b"previous bytes"
        );
        assert_eq!(
            fs::read(&temp).expect("read existing temp"),
            b"other temp bytes"
        );
    }

    #[test]
    fn failed_replace_preserves_target_and_removes_temp() {
        let dir = TempDir::new();
        let target = dir.0.join("directory-target");
        fs::create_dir(&target).expect("create directory target");

        assert!(write_atomic(&target, b"not a directory", FileMode::Mode0600).is_err());
        assert!(target.is_dir());
        assert_no_temporary_files(&dir.0);
    }

    /// APFS rejects non-UTF-8 filenames at create/open (EILSEQ), so the
    /// preservation claim is only exercisable on byte-name filesystems.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn atomic_write_preserves_non_utf8_target_name() {
        use std::os::unix::ffi::OsStrExt;

        let dir = TempDir::new();
        let target = dir.0.join(std::ffi::OsStr::from_bytes(b"cache-\xff"));
        write_atomic(&target, b"opaque path", FileMode::Mode0600)
            .expect("write through non-UTF-8 target");
        assert_eq!(fs::read(&target).expect("read target"), b"opaque path");
        assert_no_temporary_files(&dir.0);
    }

    #[test]
    fn concurrent_atomic_new_has_one_complete_winner() {
        const WRITERS: usize = 8;
        let dir = TempDir::new();
        let target = dir.0.join("race");
        let barrier = Arc::new(Barrier::new(WRITERS));
        let workers = (0..WRITERS)
            .map(|index| {
                let target = target.clone();
                let barrier = Arc::clone(&barrier);
                let payload = vec![u8::try_from(index).expect("small writer index"); 128];
                thread::spawn(move || {
                    barrier.wait();
                    let result = write_atomic_new(&target, &payload, FileMode::Mode0600);
                    (payload, result)
                })
            })
            .collect::<Vec<_>>();

        let mut winner = None;
        for worker in workers {
            let (payload, result) = worker.join().expect("join writer");
            match result {
                Ok(()) => {
                    assert!(winner.replace(payload).is_none());
                }
                Err(error) => assert!(matches!(
                    error,
                    StoreError::Io { source, .. } if source.kind() == io::ErrorKind::AlreadyExists
                )),
            }
        }
        let winner = winner.expect("one writer publishes");
        assert_eq!(fs::read(&target).expect("read published value"), winner);
        assert_no_temporary_files(&dir.0);
    }

    #[test]
    fn random_suffix_is_sixteen_lowercase_hex_digits() {
        let value = random_hex();
        assert_eq!(value.len(), 16);
        assert!(
            value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
    }

    #[test]
    fn workspace_key_is_sanitized_bounded_and_path_scoped() {
        let key = workspace_key(Path::new("/home/user/Repo With !"));
        assert!(key.starts_with("Repo_With__-"));
        let (_, suffix) = key.rsplit_once('-').expect("key has a separator");
        assert_eq!(suffix.len(), 12);
        assert!(
            suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
        assert_eq!(
            workspace_key(Path::new("/one/shared")),
            workspace_key(Path::new("/one/shared"))
        );
        assert_ne!(
            workspace_key(Path::new("/one/shared")),
            workspace_key(Path::new("/two/shared"))
        );
        assert!(workspace_key(Path::new("/root/ABC")).starts_with("ABC-"));
        assert!(workspace_key(Path::new("/")).starts_with("root-"));

        let long = format!("/root/{}", "g".repeat(40));
        let long_key = workspace_key(Path::new(&long));
        assert!(long_key.starts_with(&"g".repeat(32)));
        assert_eq!(long_key.len(), 45);
    }

    #[cfg(unix)]
    #[test]
    fn workspace_key_replaces_non_utf8_basename_bytes() {
        use std::os::unix::ffi::OsStrExt;

        let path = Path::new(std::ffi::OsStr::from_bytes(b"/work/a\xffb"));
        assert!(workspace_key(path).starts_with("a_b-"));
    }

    #[test]
    fn normalized_names_cannot_match_lowercase_id_prefixes() {
        assert!(matches!(
            normalize_name("cafe"),
            Err(StoreError::InvalidName)
        ));
        assert!(matches!(
            normalize_name("0192-abcd"),
            Err(StoreError::InvalidName)
        ));
        assert_eq!(
            normalize_name(" CAFE ").expect("uppercase name").as_ref(),
            "CAFE"
        );
        assert_eq!(
            normalize_name("Cafe").expect("mixed-case name").as_ref(),
            "Cafe"
        );
        assert_eq!(
            normalize_name("café").expect("non-ASCII name").as_ref(),
            "café"
        );
        assert_eq!(
            normalize_name("  a\r\n\nb  ")
                .expect("collapsed line-break run")
                .as_ref(),
            "a b"
        );
        assert!(matches!(
            normalize_name(" \t "),
            Err(StoreError::InvalidName)
        ));
        assert!(matches!(
            normalize_name("na\u{7}me"),
            Err(StoreError::InvalidName)
        ));
        assert!(normalize_name(&"g".repeat(64)).is_ok());
        assert!(matches!(
            normalize_name(&"g".repeat(65)),
            Err(StoreError::InvalidName)
        ));
        let exactly_64_clusters = format!("{}e\u{301}", "g".repeat(63));
        let too_many_clusters = format!("{}e\u{301}", "g".repeat(64));
        assert!(normalize_name(&exactly_64_clusters).is_ok());
        assert!(matches!(
            normalize_name(&too_many_clusters),
            Err(StoreError::InvalidName)
        ));
    }
}
