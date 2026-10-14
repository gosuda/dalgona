//! Owner-only file creation and verification.
//!
//! Unix uses mode bits. Windows uses an explicit protected DACL and verifies
//! the owner and access-control entries on opened handles.

use std::{fs::File, io, path::Path};

use crate::util::FileMode;

#[cfg(windows)]
#[expect(unsafe_code, reason = "Windows security descriptors and handle ACLs require Win32 FFI")]
mod windows;

#[cfg(unix)]
pub(crate) fn create_new(path: &Path, mode: FileMode) -> io::Result<File> {
    use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt};
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(mode.bits())
        .open(path)
}

#[cfg(windows)]
pub(crate) fn create_new(path: &Path, mode: FileMode) -> io::Result<File> {
    windows::create_new(path, mode)
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn create_new(_path: &Path, _mode: FileMode) -> io::Result<File> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "private file permissions unsupported"))
}

/// Creates a new owner-only file, without replacing an existing file.
///
/// # Errors
/// Returns an I/O error if creation fails or the platform cannot enforce
/// private file permissions.
pub fn create_private_file(path: &Path, mode: FileMode) -> io::Result<File> {
    create_new(path, mode)
}

/// Opens a regular private file for reading, rejecting Windows reparse points,
/// foreign owners and widened or inherited DACLs.
///
/// # Errors
/// Returns an I/O error if opening or Windows ACL validation fails.
pub fn open_private_file(path: &Path) -> io::Result<File> {
    #[cfg(windows)]
    {
        windows::open_checked(path, false)
    }
    #[cfg(not(windows))]
    {
        File::open(path)
    }
}

pub(crate) fn open_private_read_write(path: &Path) -> io::Result<File> {
    #[cfg(windows)]
    {
        windows::open_checked(path, true)
    }
    #[cfg(not(windows))]
    {
        std::fs::OpenOptions::new().read(true).write(true).open(path)
    }
}

/// Opens a private read/write lock file, creating it exclusively if absent.
/// An existing Windows file must pass the same opened-handle ACL checks.
///
/// # Errors
/// Returns an I/O error for unsafe or inaccessible existing files or failures
/// during owner-only creation.
pub fn open_or_create_private_file(path: &Path) -> io::Result<File> {
    match create_new(path, FileMode::Mode0600) {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            open_private_read_write(path)
        }
        Err(error) => Err(error),
    }
}

/// Checks whether an opened file is private. Windows checks its owner,
/// protected DACL, trustees, and reparse-point flag.
///
/// # Errors
/// Returns an I/O error when file metadata or security information is
/// unavailable; unsupported platforms fail closed.
pub fn file_is_private(file: &File) -> io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Ok(file.metadata()?.permissions().mode() & 0o077 == 0)
    }
    #[cfg(windows)]
    {
        windows::is_private(file)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        Err(io::Error::new(io::ErrorKind::Unsupported, "private file checks unsupported"))
    }
}

#[cfg(windows)]
pub(crate) fn create_private_dir_all_windows(path: &Path) -> io::Result<()> {
    use std::path::{Component, PathBuf};

    let mut prefix = PathBuf::new();
    for component in path.components() {
        prefix.push(component.as_os_str());
        if matches!(component, Component::Prefix(_) | Component::RootDir | Component::CurDir) {
            continue;
        }
        match windows::create_dir(&prefix) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    if windows::dir_is_private(path)? {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "directory is not owner-only",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_created_file_is_owner_only() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let file = create_new(&dir.path().join("auth.json"), FileMode::Mode0600)
            .expect("create owner-only");
        assert!(file_is_private(&file).expect("read permissions"));
    }

    #[cfg(unix)]
    #[test]
    fn a_file_readable_by_group_is_refused() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("scratch dir");
        let path = dir.path().join("auth.json");
        let file = create_new(&path, FileMode::Mode0600).expect("create owner-only");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640))
            .expect("chmod 640");
        assert!(!file_is_private(&file).expect("read mode"));
    }

    #[cfg(windows)]
    #[test]
    fn inherited_windows_permissions_are_refused() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let path = dir.path().join("unprotected.json");
        std::fs::write(&path, b"token").expect("seed with inherited DACL");
        let file = File::open(&path).expect("open fixture");
        assert!(!file_is_private(&file).expect("inspect DACL"));
        assert!(open_private_file(&path).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn windows_atomic_replacement_keeps_a_protected_dacl() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let path = dir.path().join("secret");
        crate::util::write_atomic(&path, b"first", FileMode::Mode0600)
            .expect("write private temp and publish");
        let file = open_private_file(&path).expect("check first DACL");
        assert!(file_is_private(&file).expect("inspect first DACL"));
        drop(file);
        crate::util::write_atomic(&path, b"second", FileMode::Mode0600)
            .expect("replace private temp");
        let file = open_private_file(&path).expect("check replacement DACL");
        assert!(file_is_private(&file).expect("inspect replaced DACL"));
        assert_eq!(std::fs::read(&path).expect("read replacement"), b"second");
    }

    #[cfg(windows)]
    #[test]
    fn reparse_points_cannot_be_opened_as_private_files() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let path = dir.path().join("secret");
        create_new(&path, FileMode::Mode0600).expect("private file");
        let link = dir.path().join("linked");
        if std::os::windows::fs::symlink_file(&path, &link).is_err() {
            // Creating symlinks needs Developer Mode or SeCreateSymbolicLinkPrivilege.
            return;
        }
        assert!(open_private_file(&link).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn private_directory_is_created_with_a_protected_dacl() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let path = dir.path().join("session").join("nested");
        create_private_dir_all_windows(&path).expect("create private tree");
        assert!(windows::dir_is_private(&path).expect("inspect directory"));
    }

    #[cfg(windows)]
    #[test]
    fn a_widened_windows_dacl_is_refused() {
        use std::process::Command;

        let dir = tempfile::tempdir().expect("scratch dir");
        let path = dir.path().join("token");
        let file = create_new(&path, FileMode::Mode0600).expect("create protected file");
        drop(file);
        let result = Command::new("icacls")
            .arg(&path)
            .arg("/grant")
            .arg("*S-1-1-0:R")
            .output()
            .expect("run icacls");
        assert!(
            result.status.success(),
            "icacls: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(open_private_file(&path).is_err());
    }
}
