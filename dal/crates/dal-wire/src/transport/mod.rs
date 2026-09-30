mod local;
mod memory;
mod stdio;
mod websocket;

pub use local::{
    ConnectionFuture, LocalSocketError, LocalTransport, default_rpc_path, serve_local,
};
pub use memory::{MemoryPeer, MemoryTransport};
pub use stdio::StdioTransport;
pub use websocket::{WebSocketTransport, serve_websocket};

use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    sync::Mutex,
};

use crate::error::WireError;

/// Maximum number of bytes in one newline-terminated frame.
pub const MAX_FRAME_BYTES: usize = 16_777_216;

/// A failure while receiving a complete transport frame.
#[derive(Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ReadFrameError {
    /// The input ended between frames.
    #[error("end of input")]
    EndOfInput,
    /// The frame exceeded the maximum size.
    #[error("frame exceeds {0} bytes")]
    FrameTooLarge(usize),
    /// The transport is closed.
    #[error("transport is closed")]
    Closed,
    /// A complete frame was not valid UTF-8.
    #[error("frame is not valid UTF-8")]
    InvalidUtf8,
}

/// One protocol transport.
#[non_exhaustive]
pub enum Transport {
    /// Newline-delimited standard input and output.
    Stdio(StdioTransport),
    /// A newline-delimited local stream socket or named pipe.
    Local(LocalTransport),
    /// An in-memory transport for deterministic boundary tests.
    Memory(MemoryTransport),
    /// A WebSocket transport with one JSON-RPC message per text frame.
    WebSocket(WebSocketTransport),
}

impl Transport {
    /// Builds a WebSocket transport from its halves.
    #[must_use]
    pub fn websocket(transport: WebSocketTransport) -> Self {
        Self::WebSocket(transport)
    }

    /// Returns the shared serialized writer for concurrent replies on this transport.
    #[must_use]
    pub fn writer(&self) -> FrameWriter {
        match self {
            Self::Stdio(transport) => transport.writer(),
            Self::Local(transport) => transport.writer(),
            Self::Memory(transport) => transport.writer(),
            Self::WebSocket(transport) => transport.writer(),
        }
    }
}

impl Transport {
    /// Reads the next complete frame.
    ///
    /// # Errors
    ///
    /// Returns [`ReadFrameError`] when the input ends, a frame exceeds the limit,
    /// the transport is closed, or a frame is not valid UTF-8.
    pub async fn read_frame(&mut self) -> Result<String, ReadFrameError> {
        match self {
            Self::Stdio(transport) => transport.0.read_frame().await,
            Self::Local(transport) => transport.0.read_frame().await,
            Self::Memory(transport) => transport.read_frame().await,
            Self::WebSocket(transport) => transport.read_frame().await,
        }
    }

    /// Writes one frame atomically relative to other writers on this transport.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when the frame contains a line break or the transport is closed.
    pub async fn write_frame(&mut self, frame: &str) -> Result<(), WireError> {
        match self {
            Self::Stdio(transport) => transport.0.writer.write_frame(frame).await,
            Self::Local(transport) => transport.0.writer.write_frame(frame).await,
            Self::Memory(transport) => transport.write_frame(frame).await,
            Self::WebSocket(transport) => transport.write_frame(frame).await,
        }
    }

    /// Closes the transport and makes later operations fail closed.
    pub async fn close(&mut self) {
        match self {
            Self::Stdio(transport) => transport.0.close().await,
            Self::Local(transport) => transport.0.close().await,
            Self::Memory(transport) => transport.close().await,
            Self::WebSocket(transport) => transport.close().await,
        }
    }
}

/// A cloneable, serialized writer for concurrent protocol replies.
#[derive(Clone)]
pub struct FrameWriter(Arc<WriterInner>);

struct WriterInner {
    writer: Mutex<BoxWrite>,
    closed: Arc<AtomicBool>,
}

impl FrameWriter {
    fn new(writer: BoxWrite, closed: Arc<AtomicBool>) -> Self {
        Self(Arc::new(WriterInner {
            writer: Mutex::new(writer),
            closed,
        }))
    }

    /// Builds a writer from a boxed async writer with its own closed flag.
    pub(crate) fn from_box(writer: BoxWrite, closed: Arc<AtomicBool>) -> Self {
        Self::new(writer, closed)
    }

    /// Shuts the underlying writer down for transports that close by code.
    pub(crate) async fn shutdown_sink(&self) {
        self.close().await;
    }

    /// Writes one frame and its LF delimiter under a single per-transport lock.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when the frame contains a line break or the transport is closed.
    pub async fn write_frame(&self, frame: &str) -> Result<(), WireError> {
        if frame.contains('\n') {
            return Err(WireError::Frame);
        }
        if self.0.closed.load(Ordering::Acquire) {
            return Err(closed_error());
        }

        let mut writer = self.0.writer.lock().await;
        if self.0.closed.load(Ordering::Acquire) {
            return Err(closed_error());
        }
        let mut cancellation = IncompleteWrite {
            closed: Arc::clone(&self.0.closed),
            active: true,
        };
        let mut line = String::with_capacity(frame.len() + 1);
        line.push_str(frame);
        line.push('\n');
        if let Err(error) = writer.write_all(line.as_bytes()).await {
            self.0.closed.store(true, Ordering::Release);
            cancellation.active = false;
            return Err(io_error(&error));
        }
        if let Err(error) = writer.flush().await {
            self.0.closed.store(true, Ordering::Release);
            cancellation.active = false;
            return Err(io_error(&error));
        }
        cancellation.active = false;
        Ok(())
    }

    async fn close(&self) {
        if self.0.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Err(error) = self.0.writer.lock().await.shutdown().await {
            tracing::debug!(%error, "transport shutdown failed");
        }
    }
}

struct IncompleteWrite {
    closed: Arc<AtomicBool>,
    active: bool,
}

impl Drop for IncompleteWrite {
    fn drop(&mut self) {
        if self.active {
            self.closed.store(true, Ordering::Release);
        }
    }
}

type BoxRead = Box<dyn AsyncRead + Send + Unpin>;
pub(crate) type BoxWrite = Box<dyn AsyncWrite + Send + Unpin>;

/// A line-framed transport over separate asynchronous reader and writer handles.
pub(crate) struct LineTransport {
    reader: BufReader<BoxRead>,
    writer: FrameWriter,
    closed: Arc<AtomicBool>,
    eof: bool,
}

impl LineTransport {
    pub(crate) fn new<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let closed = Arc::new(AtomicBool::new(false));
        Self {
            reader: BufReader::new(Box::new(reader)),
            writer: FrameWriter::new(Box::new(writer), Arc::clone(&closed)),
            closed,
            eof: false,
        }
    }

    async fn read_frame(&mut self) -> Result<String, ReadFrameError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(ReadFrameError::Closed);
        }
        if self.eof {
            return Err(ReadFrameError::EndOfInput);
        }
        loop {
            let mut frame = Vec::with_capacity(1024);
            let mut reached_eof = false;
            loop {
                let available = match self.reader.fill_buf().await {
                    Ok(available) => available,
                    Err(error) => {
                        self.closed.store(true, Ordering::Release);
                        tracing::debug!(%error, "transport read failed");
                        return Err(ReadFrameError::Closed);
                    }
                };
                if available.is_empty() {
                    self.eof = true;
                    reached_eof = true;
                    break;
                }

                if let Some(end) = available.iter().position(|byte| *byte == b'\n') {
                    if frame.len().saturating_add(end) > MAX_FRAME_BYTES {
                        self.reader.consume(end + 1);
                        self.closed.store(true, Ordering::Release);
                        return Err(ReadFrameError::FrameTooLarge(MAX_FRAME_BYTES));
                    }
                    frame.extend_from_slice(&available[..end]);
                    self.reader.consume(end + 1);
                    break;
                }

                if frame.len().saturating_add(available.len()) > MAX_FRAME_BYTES {
                    let len = available.len();
                    self.reader.consume(len);
                    self.closed.store(true, Ordering::Release);
                    return Err(ReadFrameError::FrameTooLarge(MAX_FRAME_BYTES));
                }
                let len = available.len();
                frame.extend_from_slice(available);
                self.reader.consume(len);
            }

            if frame.last() == Some(&b'\r') {
                frame.pop();
            }
            if frame.is_empty() {
                if reached_eof {
                    return Err(ReadFrameError::EndOfInput);
                }
                continue;
            }

            return String::from_utf8(frame).map_err(|_| ReadFrameError::InvalidUtf8);
        }
    }

    pub(crate) fn writer(&self) -> FrameWriter {
        self.writer.clone()
    }

    async fn close(&mut self) {
        self.closed.store(true, Ordering::Release);
        self.writer.close().await;
    }
}

fn io_error(error: &io::Error) -> WireError {
    match error.kind() {
        io::ErrorKind::BrokenPipe
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::NotConnected => WireError::Transport("connection is closed".to_owned()),
        _ => WireError::Transport(error.to_string()),
    }
}

fn closed_error() -> WireError {
    WireError::Transport("connection is closed".to_owned())
}
