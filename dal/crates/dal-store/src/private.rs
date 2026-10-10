//! Owner-only file creation and verification.
//!
//! POSIX carries the policy in mode bits. Other platforms create the file
//! and report it private, matching the behavior before owner-only checks
//! existed; adding another platform means writing that platform's owner
//! check here behind its `cfg`.

use std::{fs::File, io, path::Path};

use crate::util::FileMode;

#[cfg(unix)]
pub(crate) fn create_new(path: &Path, mode: FileMode) -> io::Result<File> {
    use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt};
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode.bits())
        .open(path)
}

#[cfg(not(unix))]
pub(crate) fn create_new(path: &Path, mode: FileMode) -> io::Result<File> {
    let _ = mode;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

/// Whether `file` is closed to every principal but its owner.
///
/// On Unix this is the absence of group and other permission bits.
/// Platforms without owner bits accept every file, which keeps `auth.json`
/// reads no stricter than they were before this check existed.
///
/// # Errors
/// Returns the I/O error when the file's metadata cannot be read.
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

#[cfg(not(unix))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the unix variant fails on metadata reads; this stub shares the signature"
)]
fn file_is_private_impl(file: &File) -> io::Result<bool> {
    let _ = file;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

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
