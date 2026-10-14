//! Owner-only file creation and verification.
//!
//! Unix uses mode bits; Windows uses an explicit protected DACL.
//! Unsupported platforms fail closed rather than silently creating shared files.

use std::{fs::File, io, path::Path};

use crate::util::FileMode;

#[cfg(windows)]
#[expect(unsafe_code, reason = "Win32 security descriptors require FFI")]
mod windows;

#[cfg(unix)]
pub(crate) fn create_new(path: &Path, mode: FileMode) -> io::Result<File> {
    use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt};
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode.bits())
        .open(path)
}

#[cfg(windows)]
pub(crate) fn create_new(path: &Path, mode: FileMode) -> io::Result<File> {
    let FileMode::Mode0600 = mode;
    windows::create_new(path)
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn create_new(_path: &Path, _mode: FileMode) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private file creation is unsupported on this platform",
    ))
}

/// Creates a file exclusively with an owner-only security policy.
///
/// # Errors
/// Fails when the file exists or the operating system cannot establish private permissions.
pub fn create_private_file_new(path: &Path) -> io::Result<File> {
    create_new(path, FileMode::Mode0600)
}

/// Opens a regular private file without following a Windows reparse point.
///
/// The security policy is checked against the opened handle, not a path.
/// Existing files with unsafe permissions are rejected, never repaired silently.
///
/// # Errors
/// Fails for missing files, symlinks/reparse points, non-files, or unsafe permissions.
pub fn open_private_file(path: &Path) -> io::Result<File> {
    #[cfg(windows)]
    {
        windows::open(path, false)
    }
    #[cfg(not(windows))]
    {
        File::open(path)
    }
}

/// Opens a private regular file for reading and writing without creating it.
///
/// # Errors
/// Fails if the file cannot be opened or its permissions are unsafe.
pub fn open_private_file_read_write(path: &Path) -> io::Result<File> {
    #[cfg(windows)]
    {
        windows::open(path, true)
    }
    #[cfg(not(windows))]
    {
        std::fs::OpenOptions::new().read(true).write(true).open(path)
    }
}

/// Opens an existing private lock file or creates one atomically as private.
///
/// # Errors
/// Fails when the file cannot be opened, created, or validated.
pub fn open_private_lock(path: &Path) -> io::Result<File> {
    #[cfg(windows)]
    {
        windows::open_lock(path)
    }
    #[cfg(not(windows))]
    {
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(path)
    }
}

#[cfg(windows)]
pub(crate) fn require_private(file: &File) -> io::Result<()> {
    if file.metadata()?.is_file() && file_is_private(file)? {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "file does not have private permissions",
        ))
    }
}

#[cfg(windows)]
pub(crate) fn create_private_dir_all(path: &Path) -> io::Result<()> {
    windows::create_private_dir_all(path)
}

/// Checks whether the open file excludes unrelated users.
///
/// Unix checks permission bits; Windows verifies the handle's owner, protected
/// DACL, ACEs, regular-file type, and reparse-point flag.
///
/// # Errors
/// Returns an I/O error if the file's security information cannot be inspected.
pub fn file_is_private(file: &File) -> io::Result<bool> {
    file_is_private_impl(file)
}

/// Group and other permission bits: any of them makes a file shared.
#[cfg(unix)]
const SHARED_MASK: u32 = 0o077;

#[cfg(unix)]
fn file_is_private_impl(file: &File) -> io::Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    Ok(file.metadata()?.permissions().mode() & SHARED_MASK == 0)
}

#[cfg(windows)]
fn file_is_private_impl(file: &File) -> io::Result<bool> {
    windows::file_is_private(file)
}

#[cfg(not(any(unix, windows)))]
fn file_is_private_impl(_file: &File) -> io::Result<bool> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private file verification is unsupported on this platform",
    ))
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::*;

    #[cfg(windows)]
    mod windows_tests {
        use super::*;

        #[test]
        fn private_creation_and_atomic_replacement_preserve_acl() {
            let dir = tempfile::tempdir().expect("temp");
            let target = dir.path().join("credentials.json");
            let file = create_private_file_new(&target).expect("private creation");
            assert!(file_is_private(&file).expect("check DACL"));
            drop(file);
            crate::write_atomic(&target, b"secret", FileMode::Mode0600)
                .expect("protected replacement");
            assert!(file_is_private(&File::open(&target).expect("open")).expect("check"));
            assert!(open_private_file(&target).is_ok());
        }

        #[test]
        fn default_acl_is_not_assumed_private() {
            let dir = tempfile::tempdir().expect("temp");
            let target = dir.path().join("unprotected.json");
            std::fs::write(&target, b"secret").expect("create with inherited ACL");
            assert!(!file_is_private(&File::open(&target).expect("open")).expect("check"));
            assert_eq!(
                open_private_file(&target).expect_err("reject unprotected ACL").kind(),
                io::ErrorKind::PermissionDenied,
            );
        }

        #[test]
        fn private_directory_creation_protects_all_new_levels() {
            let dir = tempfile::tempdir().expect("temp");
            let path = dir.path().join("sessions").join("workspace");
            create_private_dir_all(&path).expect("private directory");
            create_private_dir_all(&path).expect("existing private directory");
            let file = create_private_file_new(&path.join("session")).expect("private file");
            assert!(file_is_private(&file).expect("check ACL"));
        }
    }

    #[cfg(unix)]
    mod unix {
        use std::os::unix::fs::PermissionsExt;

        use super::*;

        #[test]
        fn a_created_file_is_owner_only() {
            let dir = tempfile::tempdir().expect("scratch dir");
            let file = create_new(&dir.path().join("auth.json"), FileMode::Mode0600)
                .expect("create owner-only");
            assert!(file_is_private(&file).expect("read mode"));
        }

        #[test]
        fn a_file_readable_by_group_is_refused() {
            let dir = tempfile::tempdir().expect("scratch dir");
            let path = dir.path().join("auth.json");
            let file = create_new(&path, FileMode::Mode0600).expect("create owner-only");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640))
                .expect("chmod 640");
            assert!(!file_is_private(&file).expect("read mode"));
        }
    }
}
