use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use thiserror::Error;
use uuid::Uuid;

/// A saved serve token represented only by its BLAKE3 digest.
#[derive(Clone)]
pub struct SecretToken {
    digest: [u8; 32],
}

impl SecretToken {
    fn from_token(token: &str) -> Self {
        Self {
            digest: *blake3::hash(token.as_bytes()).as_bytes(),
        }
    }

    /// Checks a presented token without comparing or logging token bytes.
    #[must_use]
    pub fn matches(&self, candidate: &str) -> bool {
        let candidate = blake3::hash(candidate.as_bytes());
        let mut difference = 0_u8;
        for (saved, presented) in self.digest.iter().zip(candidate.as_bytes()) {
            difference |= *saved ^ *presented;
        }
        difference == 0
    }
}

impl fmt::Debug for SecretToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretToken([REDACTED])")
    }
}

/// A failure while creating, loading, or reading a serve token.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum TokenError {
    /// The token path is not absolute.
    #[error("token path must be absolute: {path}")]
    RelativePath {
        /// The rejected path.
        path: PathBuf,
    },
    /// The token file does not exist.
    #[error("token file does not exist: {path}")]
    Missing {
        /// The missing token path.
        path: PathBuf,
    },
    /// The token file is accessible by group or other users on POSIX.
    #[error("token file is open to other users: {path} has mode {mode:04o}")]
    OpenToOtherUsers {
        /// The token file path.
        path: PathBuf,
        /// The file mode observed on disk.
        mode: u32,
    },
    /// The token path is a symbolic link.
    #[error("token file must not be a symbolic link: {path}")]
    Symlink {
        /// The rejected token path.
        path: PathBuf,
    },
    /// The token path is not a regular file.
    #[error("token path is not a regular file: {path}")]
    NotRegularFile {
        /// The rejected token path.
        path: PathBuf,
    },
    /// The token file does not have the required mode.
    #[error("token file must have mode 0600: {path} has mode {mode:04o}")]
    ModeNot0600 {
        /// The token file path.
        path: PathBuf,
        /// The file mode observed on disk.
        mode: u32,
    },
    /// The first token line is empty.
    #[error("token file is empty: {path}")]
    Empty {
        /// The empty token path.
        path: PathBuf,
    },
    /// The first token line does not match the dal token format.
    #[error("token file has an invalid token: {path}")]
    Invalid {
        /// The invalid token path.
        path: PathBuf,
    },
    /// A token file already exists and replacement was not requested.
    #[error("token file already exists: {path}")]
    AlreadyExists {
        /// The existing token path.
        path: PathBuf,
    },
    /// A file operation failed at the token path.
    #[error("token file operation failed for {path}: {source}")]
    Io {
        /// The path involved in the failed operation.
        path: PathBuf,
        /// The underlying operating-system error.
        #[source]
        source: io::Error,
    },
}

/// Creates and durably writes a fresh token, returning it once to the caller.
///
/// # Errors
///
/// Returns [`TokenError`] when the path is not absolute, the file already exists without `force`,
/// or any file write, sync, or rename step fails.
pub fn create(path: &Path, force: bool) -> Result<String, TokenError> {
    ensure_absolute(path)?;
    let token = format!("dal_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let bytes = format!("{token}\n");

    if !force {
        let mut file = open_exclusive(path).map_err(|source| {
            if source.kind() == io::ErrorKind::AlreadyExists {
                TokenError::AlreadyExists {
                    path: path.to_owned(),
                }
            } else {
                TokenError::Io {
                    path: path.to_owned(),
                    source,
                }
            }
        })?;
        if let Err(source) = write_sync(&mut file, bytes.as_bytes()) {
            drop(file);
            let _ = fs::remove_file(path);
            return Err(TokenError::Io {
                path: path.to_owned(),
                source,
            });
        }
        sync_parent(path)?;
        return Ok(token);
    }

    let temporary = append_suffix(path, ".tmp");
    if temporary.exists() {
        fs::remove_file(&temporary).map_err(|source| TokenError::Io {
            path: temporary.clone(),
            source,
        })?;
    }
    let mut file = open_exclusive(&temporary).map_err(|source| TokenError::Io {
        path: temporary.clone(),
        source,
    })?;
    if let Err(source) = write_sync(&mut file, bytes.as_bytes()) {
        drop(file);
        let _ = fs::remove_file(&temporary);
        return Err(TokenError::Io {
            path: temporary,
            source,
        });
    }
    drop(file);
    if let Err(source) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(TokenError::Io {
            path: path.to_owned(),
            source,
        });
    }
    sync_parent(path)?;
    Ok(token)
}

/// Creates and durably writes a fresh token, returning it once to the caller.
///
/// This is the plan-named alias for [`create`].
///
/// # Errors
///
/// Returns [`TokenError`] when the path is not absolute, the file already exists without `force`,
/// or any file write, sync, or rename step fails.
pub fn create_token(path: &Path, force: bool) -> Result<String, TokenError> {
    create(path, force)
}

/// Loads a token file and retains only the digest used for authentication.
///
/// # Errors
///
/// Returns [`TokenError`] when the path is not absolute, the file is missing, has unsafe
/// permissions, is empty, or does not match the expected token shape.
pub fn load(path: &Path) -> Result<SecretToken, TokenError> {
    let bytes = read_token_file(path)?;
    let (mut token, has_more_lines) = first_token_line(&bytes);
    if has_more_lines {
        tracing::warn!(path = %path.display(), "serve token file has multiple lines; using the first");
    }
    if token.last() == Some(&b'\r') {
        token = &token[..token.len() - 1];
    }
    if token.is_empty() {
        return Err(TokenError::Empty {
            path: path.to_owned(),
        });
    }
    if !valid_token(token) {
        return Err(TokenError::Invalid {
            path: path.to_owned(),
        });
    }
    let token = std::str::from_utf8(token).map_err(|_| TokenError::Invalid {
        path: path.to_owned(),
    })?;
    Ok(SecretToken::from_token(token))
}

/// Reads a token file for an authenticated client without hashing the token.
///
/// The returned string contains the first trimmed token line. Callers must treat it as a secret and
/// must not log or display it.
///
/// # Errors
///
/// Returns [`TokenError`] when the path is not absolute, the file is missing, is not a regular
/// file or is a symbolic link, does not have mode 0600 on POSIX, is empty, or does not match the
/// expected token shape.
pub fn read_connect_token(path: &Path) -> Result<String, TokenError> {
    let bytes = read_token_file(path)?;
    let (token, _) = first_token_line(&bytes);
    let token = token.trim_ascii();
    if token.is_empty() {
        return Err(TokenError::Empty {
            path: path.to_owned(),
        });
    }
    if !valid_token(token) {
        return Err(TokenError::Invalid {
            path: path.to_owned(),
        });
    }
    std::str::from_utf8(token)
        .map(str::to_owned)
        .map_err(|_| TokenError::Invalid {
            path: path.to_owned(),
        })
}

fn read_token_file(path: &Path) -> Result<Vec<u8>, TokenError> {
    ensure_absolute(path)?;
    let mut file = open_token_file(path)?;
    let mut bytes = Vec::with_capacity(80);
    Read::by_ref(&mut file)
        .take(4096)
        .read_to_end(&mut bytes)
        .map_err(|source| TokenError::Io {
            path: path.to_owned(),
            source,
        })?;
    Ok(bytes)
}

fn first_token_line(bytes: &[u8]) -> (&[u8], bool) {
    let end = bytes.iter().position(|byte| *byte == b'\n');
    let has_more_lines = end.is_some_and(|end| end + 1 < bytes.len());
    let token = match end {
        Some(end) => &bytes[..end],
        None => bytes,
    };
    (token, has_more_lines)
}

/// Loads a token file and retains only the digest used for authentication.
///
/// This is the plan-named alias for [`load`].
///
/// # Errors
///
/// Returns [`TokenError`] when the path is not absolute, the file is missing, has unsafe
/// permissions, is empty, or does not match the expected token shape.
pub fn load_token(path: &Path) -> Result<SecretToken, TokenError> {
    load(path)
}

fn open_token_file(path: &Path) -> Result<File, TokenError> {
    let link = fs::symlink_metadata(path).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            TokenError::Missing {
                path: path.to_owned(),
            }
        } else {
            TokenError::Io {
                path: path.to_owned(),
                source,
            }
        }
    })?;
    if link.file_type().is_symlink() {
        return Err(TokenError::Symlink {
            path: path.to_owned(),
        });
    }
    if !link.file_type().is_file() {
        return Err(TokenError::NotRegularFile {
            path: path.to_owned(),
        });
    }
    let file = File::open(path).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            TokenError::Missing {
                path: path.to_owned(),
            }
        } else {
            TokenError::Io {
                path: path.to_owned(),
                source,
            }
        }
    })?;
    let opened = file.metadata().map_err(|source| TokenError::Io {
        path: path.to_owned(),
        source,
    })?;
    if !same_file(&link, &opened) || !opened.is_file() {
        return Err(TokenError::Symlink {
            path: path.to_owned(),
        });
    }
    check_connect_file_mode(path, &opened)?;
    Ok(file)
}

#[cfg(unix)]
fn same_file(link: &fs::Metadata, opened: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    link.dev() == opened.dev() && link.ino() == opened.ino()
}

#[cfg(not(unix))]
fn same_file(_link: &fs::Metadata, _opened: &fs::Metadata) -> bool {
    true
}

#[cfg(unix)]
fn check_connect_file_mode(path: &Path, metadata: &fs::Metadata) -> Result<(), TokenError> {
    use std::os::unix::fs::PermissionsExt;

    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(TokenError::OpenToOtherUsers {
            path: path.to_owned(),
            mode,
        });
    }
    if mode != 0o600 {
        return Err(TokenError::ModeNot0600 {
            path: path.to_owned(),
            mode,
        });
    }
    Ok(())
}

#[cfg(not(unix))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "unix checks the mode bits; other platforms accept the file unchanged"
)]
fn check_connect_file_mode(_path: &Path, _metadata: &fs::Metadata) -> Result<(), TokenError> {
    Ok(())
}

fn ensure_absolute(path: &Path) -> Result<(), TokenError> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(TokenError::RelativePath {
            path: path.to_owned(),
        })
    }
}

fn valid_token(token: &[u8]) -> bool {
    token.len() == 68
        && token.starts_with(b"dal_")
        && token[4..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

fn open_exclusive(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) = file.set_permissions(fs::Permissions::from_mode(0o600)) {
            drop(file);
            let _ = fs::remove_file(path);
            return Err(error);
        }
    }
    Ok(file)
}

fn write_sync(file: &mut File, bytes: &[u8]) -> io::Result<()> {
    file.write_all(bytes)?;
    file.sync_all()
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Windows has no directory-sync door; the file sync is the durable edge.
#[cfg_attr(
    windows,
    expect(
        clippy::unnecessary_wraps,
        reason = "directory sync fails only on POSIX"
    )
)]
fn sync_parent(path: &Path) -> Result<(), TokenError> {
    #[cfg(windows)]
    {
        let _ = path;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let Some(parent) = path.parent() else {
            return Ok(());
        };
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| TokenError::Io {
                path: parent.to_owned(),
                source,
            })
    }
}
