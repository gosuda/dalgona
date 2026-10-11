//! Owner-only file creation and verification.
//!
//! Unix carries the policy in mode bits. Other platforms create files with
//! platform defaults, and `file_is_private` is informational there without
//! inspecting platform-specific permissions. Add another platform's owner
//! check here behind its `cfg` before relying on it.

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

/// Checks whether `file` has owner-only Unix permission bits.
///
/// On Unix this is the absence of group and other permission bits. On other
/// platforms this is informational only: it returns `Ok(true)` without
/// inspecting platform-specific permissions.
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
    reason = "the non-Unix variant is informational; this stub shares the fallible signature"
)]
fn file_is_private_impl(file: &File) -> io::Result<bool> {
    let _ = file;
    Ok(true)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
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
