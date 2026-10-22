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
    sync::{Mutex, mpsc},
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
    /// Cancel safe: a read that is dropped before it completes loses no input.
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
///
/// [`FrameWriter::enqueue_frame`] hands a frame to the serving task's
/// outbound queue instead of writing it, so callers that only ever run
/// inside the serve loop's futures never suspend on the writer lock: a
/// queued write cannot deadlock the loop that is their only driver.
#[derive(Clone)]
pub struct FrameWriter(Arc<WriterInner>);

struct WriterInner {
    writer: Mutex<BoxWrite>,
    closed: Arc<AtomicBool>,
    /// Frames queued by producers; the serving task drains them.
    outbox_tx: mpsc::UnboundedSender<String>,
    outbox_rx: Mutex<mpsc::UnboundedReceiver<String>>,
}

impl FrameWriter {
    fn new(writer: BoxWrite, closed: Arc<AtomicBool>) -> Self {
        let (outbox_tx, outbox_rx) = mpsc::unbounded_channel();
        Self(Arc::new(WriterInner {
            writer: Mutex::new(writer),
            closed,
            outbox_tx,
            outbox_rx: Mutex::new(outbox_rx),
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

    /// Queues one frame for the serving task to write; producers never block.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when the transport is closed.
    pub(crate) fn enqueue_frame(&self, frame: String) -> Result<(), WireError> {
        if self.0.closed.load(Ordering::Acquire) || self.0.outbox_tx.send(frame).is_err() {
            return Err(closed_error());
        }
        Ok(())
    }

    /// Waits for the next queued frame; resolved by producers' enqueues.
    /// Only the serving task calls this, so it is the sole lock waiter.
    pub(crate) async fn next_queued_frame(&self) -> Option<String> {
        self.0.outbox_rx.lock().await.recv().await
    }

    /// Takes one queued frame without waiting, for end-of-loop flushing.
    pub(crate) fn take_queued_frame(&self) -> Option<String> {
        self.0.outbox_rx.try_lock().ok()?.try_recv().ok()
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

    /// Marks the writer closed and shuts the write half down, unless a write
    /// holds it.
    ///
    /// A write in flight owns the half until its peer takes the frame, so
    /// waiting for it would let a stalled peer hold the close. That half ends
    /// with the last handle instead. Later writes fail either way.
    async fn close_unless_writing(&self) {
        if self.0.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let Ok(mut writer) = self.0.writer.try_lock() else {
            return;
        };
        if let Err(error) = writer.shutdown().await {
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
///
/// `read_frame` is cancel safe: the serve loops poll it beside other events
/// and drop it whenever one of those fires. The bytes of a line that is only
/// partly received live on the transport, so a dropped read resumes the same
/// line instead of losing its head.
pub(crate) struct LineTransport {
    reader: BufReader<BoxRead>,
    writer: FrameWriter,
    closed: Arc<AtomicBool>,
    eof: bool,
    /// The line being assembled, without its LF.
    line: Vec<u8>,
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
            line: Vec::new(),
        }
    }

    /// Reads the next complete frame.
    ///
    /// Cancel safe: dropping the future between chunks keeps the received
    /// part of the line for the next call.
    async fn read_frame(&mut self) -> Result<String, ReadFrameError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(ReadFrameError::Closed);
        }
        if self.eof {
            return Err(ReadFrameError::EndOfInput);
        }
        loop {
            let reached_eof = self.fill_line().await?;
            let mut frame = std::mem::take(&mut self.line);
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

    /// Appends input to [`Self::line`] up to the next LF, which it consumes
    /// without storing, or up to the end of input.
    ///
    /// Returns true when the input ended instead. A line past the frame limit
    /// closes the transport. The only suspension point is the buffered read,
    /// which holds no bytes taken from the stream, so the future may be
    /// dropped there.
    async fn fill_line(&mut self) -> Result<bool, ReadFrameError> {
        loop {
            let available = match self.reader.fill_buf().await {
                Ok(available) => available,
                Err(error) => {
                    self.closed.store(true, Ordering::Release);
                    self.line = Vec::new();
                    tracing::debug!(%error, "transport read failed");
                    return Err(ReadFrameError::Closed);
                }
            };
            if available.is_empty() {
                self.eof = true;
                return Ok(true);
            }

            let end = available.iter().position(|byte| *byte == b'\n');
            let taken = end.unwrap_or(available.len());
            if self.line.len().saturating_add(taken) > MAX_FRAME_BYTES {
                let consumed = taken + usize::from(end.is_some());
                self.reader.consume(consumed);
                self.closed.store(true, Ordering::Release);
                self.line = Vec::new();
                return Err(ReadFrameError::FrameTooLarge(MAX_FRAME_BYTES));
            }
            self.line.extend_from_slice(&available[..taken]);
            self.reader.consume(taken + usize::from(end.is_some()));
            if end.is_some() {
                return Ok(false);
            }
        }
    }

    pub(crate) fn writer(&self) -> FrameWriter {
        self.writer.clone()
    }

    /// Closes the transport. Reads fail closed through the flag the reader
    /// shares with the writer.
    async fn close(&mut self) {
        self.writer.close_unless_writing().await;
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
