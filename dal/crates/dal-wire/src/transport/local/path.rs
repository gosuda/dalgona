#[cfg(unix)]
use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use super::LocalSocketError;

/// Returns the per-user default RPC socket path.
#[must_use]
pub fn default_rpc_path(data: &Path) -> PathBuf {
    data.join("rpc").join("dal.sock")
}

#[cfg(unix)]
pub(super) fn os_error(path: &Path, error: &io::Error) -> LocalSocketError {
    LocalSocketError::Os {
        path: path.to_owned(),
        message: error.to_string(),
    }
}
