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

/// Opens an existing private file for reading.
///
/// On Windows the file opens without following a reparse point and its
/// security policy is checked against the opened handle, not a path. A file
/// with an unsafe DACL, a reparse point, or a non-file type is rejected, and a
/// rejected file's DACL is also tightened to the private policy best-effort,
/// so stored secrets stop being shared while the open still fails closed. A
/// later open then treats the file as migrated.
///
/// On Unix the file opens as a plain read: neither permission bits nor
/// symlinks are checked here. A caller that must refuse a shared or linked
/// file checks the path with `symlink_metadata` and the returned handle with
/// [`file_is_private`].
///
/// # Errors
/// Fails for a missing file and, on Windows, for symlinks/reparse points,
/// non-files, or unsafe permissions.
pub fn open_private_file(path: &Path) -> io::Result<File> {
    #[cfg(windows)]
    {
        windows::open(path, false)
    }
    #[cfg(unix)]
    {
        File::open(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "private file reads are unsupported on this platform",
        ))
    }
}

/// Opens an existing private file for reading and writing without creating it.
///
/// Windows checks the handle exactly as [`open_private_file`] does; Unix opens
/// the file as it is and checks no permission bits.
///
/// # Errors
/// Fails if the file cannot be opened or, on Windows, its permissions are unsafe.
pub fn open_private_file_read_write(path: &Path) -> io::Result<File> {
    #[cfg(windows)]
    {
        windows::open(path, true)
    }
    #[cfg(unix)]
    {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "private file reads are unsupported on this platform",
        ))
    }
}

/// Opens an existing private lock file or creates one atomically as private.
/// An existing file with an unsafe Windows DACL is rejected after the DACL is
/// tightened best-effort, so the next call opens it as private. On Unix a new
/// file is created with mode 0600 and an existing one opens as it is.
///
/// # Errors
/// Fails when the file cannot be opened, created, or validated.
pub fn open_private_lock(path: &Path) -> io::Result<File> {
    #[cfg(windows)]
    {
        windows::open_lock(path)
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true);
        options.mode(0o600);
        options.open(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "private lock files are unsupported on this platform",
        ))
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
    #[cfg(any(unix, windows))]
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
                open_private_file(&target)
                    .expect_err("reject unprotected ACL")
                    .kind(),
                io::ErrorKind::PermissionDenied,
            );
        }

        #[test]
        fn a_rejected_file_is_tightened_then_migrates() {
            let dir = tempfile::tempdir().expect("temp");
            let target = dir.path().join("legacy.json");
            std::fs::write(&target, b"secret").expect("create with inherited ACL");
            assert_eq!(
                open_private_file(&target)
                    .expect_err("reject unprotected ACL")
                    .kind(),
                io::ErrorKind::PermissionDenied,
            );
            // The rejection hardened the file to the private DACL rather than
            // leaving the stored secret shared; the next open migrates it.
            assert!(file_is_private(&File::open(&target).expect("open")).expect("check"));
            assert!(open_private_file(&target).is_ok());
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

        #[test]
        fn each_group_or_other_permission_bit_alone_makes_a_file_shared() {
            let dir = tempfile::tempdir().expect("scratch dir");
            let path = dir.path().join("auth.json");
            let file = create_new(&path, FileMode::Mode0600).expect("create owner-only");
            for shared in [0o040, 0o020, 0o010, 0o004, 0o002, 0o001] {
                let mode = 0o600 | shared;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
                    .expect("chmod");
                assert!(
                    !file_is_private(&file).expect("read mode"),
                    "mode {mode:o} exposes the file beyond its owner"
                );
            }
            for owner_only in [0o700, 0o600, 0o400, 0o200] {
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(owner_only))
                    .expect("chmod");
                assert!(
                    file_is_private(&file).expect("read mode"),
                    "mode {owner_only:o} stays within the owner"
                );
            }
        }

        #[test]
        fn creating_over_an_existing_file_fails_and_leaves_it_alone() {
            let dir = tempfile::tempdir().expect("scratch dir");
            let path = dir.path().join("auth.json");
            std::fs::write(&path, b"existing secret").expect("seed an existing file");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
                .expect("chmod 644");
            let error = create_private_file_new(&path)
                .expect_err("an existing file is never reused as a private one");
            assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
            assert_eq!(
                std::fs::read(&path).expect("reread"),
                b"existing secret",
                "the existing bytes are untouched"
            );
            assert_eq!(
                std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777,
                0o644,
                "the existing mode is neither reused nor rewritten"
            );
        }
    }
}
