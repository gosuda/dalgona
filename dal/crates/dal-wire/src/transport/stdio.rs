use std::fs::File;

use tokio::io::{AsyncRead, AsyncWrite};

use super::{FrameWriter, LineTransport};

/// A newline-delimited protocol transport over caller-supplied standard streams.
pub struct StdioTransport(pub(crate) LineTransport);

impl StdioTransport {
    /// Builds a standard-stream transport without reading process-global handles.
    pub fn new<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        Self(LineTransport::new(reader, writer))
    }

    /// Builds a transport over privately owned stream files.
    ///
    /// The caller moves the process standard input and output onto these
    /// files and points the shared descriptors elsewhere, so unrelated
    /// writes and child processes cannot reach the protocol stream. Reads and
    /// writes run on the blocking pool, as with the process-global streams.
    #[must_use]
    pub fn from_files(reader: File, writer: File) -> Self {
        Self::new(
            tokio::fs::File::from_std(reader),
            tokio::fs::File::from_std(writer),
        )
    }

    /// Returns a writer handle that serializes replies from concurrent handlers.
    #[must_use]
    pub fn writer(&self) -> FrameWriter {
        self.0.writer()
    }
}
