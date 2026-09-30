use std::path::PathBuf;

use thiserror::Error;

/// A failure while preparing or serving a local RPC endpoint.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum LocalSocketError {
    /// A user-supplied parent directory is accessible by group or other users.
    #[error("directory {path} is open to other users (mode {mode:04o})")]
    DirNotPrivate {
        /// The rejected parent directory.
        path: PathBuf,
        /// The permission bits observed on disk.
        mode: u32,
    },
    /// The encoded POSIX socket path exceeds the supported limit.
    #[error("socket path is {bytes} bytes; the maximum is 103 bytes: {path}")]
    PathTooLong {
        /// The rejected socket path.
        path: PathBuf,
        /// The number of encoded path bytes.
        bytes: usize,
    },
    /// Another listener owns the requested local endpoint.
    #[error("another dalgon rpc is listening at {path}")]
    InUse {
        /// The endpoint path or pipe name.
        path: PathBuf,
    },
    /// A platform or filesystem operation failed.
    #[error("local socket operation failed for {path}: {message}")]
    Os {
        /// The endpoint or directory involved in the failure.
        path: PathBuf,
        /// The operating-system error or fail-closed reason.
        message: String,
    },
}

#[cfg(any(windows, test))]
use std::{io, path::Path};

#[cfg(windows)]
use std::num::NonZeroU8;

#[cfg(windows)]
use futures::{StreamExt, stream::FuturesUnordered};
#[cfg(windows)]
use interprocess::os::windows::{
    ToWtf16,
    named_pipe::{PipeListenerOptions, pipe_mode},
    security_descriptor::SecurityDescriptor,
};
#[cfg(windows)]
use tokio::io::split;

#[cfg(windows)]
use crate::{error::WireError, transport::Transport};

#[cfg(windows)]
use super::{ConnectionFuture, LocalTransport};

#[cfg(windows)]
const MAX_CONNECTIONS: usize = 64;
#[cfg(windows)]
const MAX_PIPE_INSTANCES: u8 = 65;

/// Serves same-user RPC connections through the SID-scoped Windows named pipe.
#[cfg(windows)]
pub async fn serve_windows_pipe<F>(
    path: &Path,
    default_endpoint: bool,
    current_user_sid: Option<&str>,
    on_connection: F,
) -> Result<(), LocalSocketError>
where
    F: Fn(crate::transport::Transport) -> super::ConnectionFuture + Send + Sync + 'static,
{
    let sid =
        current_user_sid.ok_or_else(|| os_failure(path, "current-user SID is unavailable"))?;
    if !valid_sid(sid) {
        return Err(os_failure(path, "current-user SID is invalid"));
    }
    let pipe_name = pipe_name(path, sid, default_endpoint)
        .ok_or_else(|| os_failure(path, "pipe name must be a valid non-empty path"))?;
    let wide_pipe_name = pipe_name
        .as_str()
        .to_wtf_16()
        .map_err(|error| os_failure(path, &error.to_string()))?;
    let sddl = security_descriptor_sddl(sid);
    let wide_sddl = sddl
        .as_str()
        .to_wtf_16()
        .map_err(|error| os_failure(path, &error.to_string()))?;
    let descriptor = SecurityDescriptor::deserialize(wide_sddl.as_ref())
        .map_err(|error| os_failure(path, &error.to_string()))?;
    let instance_limit = NonZeroU8::new(MAX_PIPE_INSTANCES);
    let listener = PipeListenerOptions::new()
        .path(wide_pipe_name.as_ref())
        .security_descriptor(Some(descriptor))
        .instance_limit(instance_limit)
        .accept_remote(false)
        .create_tokio_duplex::<pipe_mode::Bytes>()
        .map_err(|error| pipe_error(path, &error))?;
    let mut active = FuturesUnordered::<ConnectionFuture>::new();

    loop {
        if active.len() >= MAX_CONNECTIONS {
            log_connection(active.next().await);
            continue;
        }
        tokio::select! {
            result = active.next(), if !active.is_empty() => log_connection(result),
            accepted = listener.accept() => handle_accept(accepted, path, &on_connection, &mut active)?,
        }
    }
}

#[cfg(any(windows, test))]
fn valid_sid(sid: &str) -> bool {
    let Some(tail) = sid.strip_prefix("S-1-") else {
        return false;
    };
    let mut fields = tail.split('-');
    let Some(authority) = fields.next() else {
        return false;
    };
    if !decimal_field(authority) {
        return false;
    }
    let mut subauthorities = 0;
    for field in fields {
        if !decimal_field(field) {
            return false;
        }
        subauthorities += 1;
    }
    subauthorities > 0
}

#[cfg(any(windows, test))]
fn decimal_field(field: &str) -> bool {
    !field.is_empty() && field.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(any(windows, test))]
fn pipe_name(path: &Path, sid: &str, default_endpoint: bool) -> Option<String> {
    if let Some(name) = path.to_str().filter(|name| name.starts_with(r"\\.\pipe\")) {
        return Some(name.to_owned());
    }
    if default_endpoint {
        let hash = blake3::hash(sid.as_bytes()).to_hex();
        let short_hash = hash.as_str().chars().take(16).collect::<String>();
        return Some(format!(r"\\.\pipe\dal-{short_hash}-rpc"));
    }
    let name = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .filter(|name| !name.is_empty())?;
    Some(format!(r"\\.\pipe\{name}"))
}

#[cfg(any(windows, test))]
fn security_descriptor_sddl(sid: &str) -> String {
    format!("D:P(A;;GA;;;{sid})")
}

#[cfg(windows)]
fn os_failure(path: &Path, message: &str) -> LocalSocketError {
    LocalSocketError::Os {
        path: path.to_owned(),
        message: message.to_owned(),
    }
}

#[cfg(any(windows, test))]
fn pipe_error(path: &Path, error: &io::Error) -> LocalSocketError {
    if matches!(error.raw_os_error(), Some(5 | 183))
        || matches!(
            error.kind(),
            io::ErrorKind::AddrInUse | io::ErrorKind::AlreadyExists
        )
    {
        return LocalSocketError::InUse {
            path: path.to_owned(),
        };
    }
    LocalSocketError::Os {
        path: path.to_owned(),
        message: error.to_string(),
    }
}

#[cfg(windows)]
fn handle_accept<Stream, F>(
    accepted: io::Result<Stream>,
    path: &Path,
    on_connection: &F,
    active: &mut FuturesUnordered<ConnectionFuture>,
) -> Result<(), LocalSocketError>
where
    Stream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
    F: Fn(crate::transport::Transport) -> super::ConnectionFuture + Send + Sync + 'static,
{
    match accepted {
        Ok(stream) => {
            let (reader, writer) = split(stream);
            active.push(on_connection(crate::transport::Transport::Local(
                LocalTransport::new(reader, writer),
            )));
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(()),
        Err(error) => Err(pipe_error(path, &error)),
    }
}

#[cfg(windows)]
fn log_connection(result: Option<Result<(), WireError>>) {
    if let Some(Err(error)) = result {
        tracing::debug!(%error, "local RPC pipe connection ended with an error");
    }
}

#[cfg(test)]
mod tests {
    use super::{decimal_field, pipe_error, pipe_name, security_descriptor_sddl, valid_sid};
    use std::{io, path::Path};

    #[test]
    fn sid_validation_rejects_sddl_injection() {
        assert!(valid_sid("S-1-5-21-1-2-3-1000"));
        assert!(!valid_sid("S-1-5-21-1000);D:(A;;GA;;;WD"));
        assert!(!decimal_field(""));
    }

    #[test]
    fn default_pipe_name_is_sid_scoped_and_custom_names_are_preserved() {
        let default = Path::new("C:/users/test/.local/share/dal/rpc/dal.sock");
        assert_eq!(
            pipe_name(default, "S-1-5-21-1-2-3-1000", true).as_deref(),
            Some(r"\\.\pipe\dal-44a79e55338ba886-rpc")
        );
        assert_eq!(
            pipe_name(default, "S-1-5-21-1-2-3-1000", false).as_deref(),
            Some(r"\\.\pipe\dal.sock")
        );
        assert_eq!(
            pipe_name(
                Path::new(r"\\.\pipe\private-dal"),
                "S-1-5-21-1-2-3-1000",
                false
            )
            .as_deref(),
            Some(r"\\.\pipe\private-dal")
        );
        assert_eq!(
            pipe_name(Path::new("private-dal"), "S-1-5-21-1-2-3-1000", false).as_deref(),
            Some(r"\\.\pipe\private-dal")
        );
        assert_eq!(pipe_name(Path::new(""), "S-1-5-21-1-2-3-1000", false), None);
    }

    #[test]
    fn security_descriptor_is_protected_and_grants_only_the_current_sid() {
        assert_eq!(
            security_descriptor_sddl("S-1-5-21-1-2-3-1000"),
            "D:P(A;;GA;;;S-1-5-21-1-2-3-1000)"
        );
    }

    #[test]
    fn pipe_name_collision_maps_to_in_use() {
        let error = io::Error::from_raw_os_error(5);
        assert!(matches!(
            pipe_error(Path::new("pipe"), &error),
            super::LocalSocketError::InUse { .. }
        ));
    }
}
