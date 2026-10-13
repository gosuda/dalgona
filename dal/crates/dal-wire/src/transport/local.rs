mod path;
mod pipe;
#[cfg(unix)]
mod unix;

pub use path::default_rpc_path;
pub use pipe::LocalSocketError;
#[cfg(windows)]
use pipe::serve_windows_pipe;
#[cfg(unix)]
use unix::serve_unix;

use std::{future::Future, path::Path, pin::Pin};

use tokio::io::{AsyncRead, AsyncWrite};

use crate::error::WireError;

use super::{FrameWriter, LineTransport, Transport};

/// A newline-delimited transport over an accepted local stream or named pipe.
pub struct LocalTransport(pub(crate) LineTransport);

impl LocalTransport {
    /// Builds a local transport from the accepted connection's read and write halves.
    pub fn new<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        Self(LineTransport::new(reader, writer))
    }

    /// Returns a writer handle that serializes replies from concurrent handlers.
    #[must_use]
    pub fn writer(&self) -> FrameWriter {
        self.0.writer()
    }
}

/// A connection handler owned by the local listener.
pub type ConnectionFuture = Pin<Box<dyn Future<Output = Result<(), WireError>> + Send + 'static>>;

/// Serves local RPC connections with bounded concurrency.
///
/// `default_data_root` is `Some` only when `path` is the default endpoint. This lets the
/// listener repair permissions on dal-owned `<data>/rpc` while refusing to modify a
/// user-supplied parent. Windows requires the validated current-user SID from the edge;
/// a missing identity fails closed.
///
/// # Errors
///
/// Returns [`LocalSocketError`] when the endpoint path, permissions, or listener setup is invalid.
pub async fn serve_local<F>(
    path: &Path,
    default_data_root: Option<&Path>,
    current_user_sid: Option<&str>,
    on_connection: F,
) -> Result<(), LocalSocketError>
where
    F: Fn(Transport) -> ConnectionFuture + Send + Sync + 'static,
{
    #[cfg(unix)]
    {
        let _ = current_user_sid;
        serve_unix(path, default_data_root, on_connection).await
    }
    #[cfg(windows)]
    {
        serve_windows_pipe(
            path,
            default_data_root.is_some(),
            current_user_sid,
            on_connection,
        )
        .await
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (current_user_sid, default_data_root, on_connection);
        Err(LocalSocketError::Os {
            path: path.to_owned(),
            message: "local RPC sockets are unavailable on this platform".to_owned(),
        })
    }
}
