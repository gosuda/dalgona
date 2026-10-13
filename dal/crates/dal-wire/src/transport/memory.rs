use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::task::{Context, Poll, ready};

use tokio::io::AsyncWrite;
use tokio::sync::mpsc;
use tokio_util::sync::PollSender;

use crate::error::WireError;

use super::{FrameWriter, MAX_FRAME_BYTES, ReadFrameError, Transport};

/// An in-memory endpoint for deterministic transport tests.
pub struct MemoryTransport {
    incoming: Option<mpsc::Receiver<String>>,
    writer: FrameWriter,
    closed: bool,
}

/// The peer endpoint paired with a [`MemoryTransport`].
pub struct MemoryPeer {
    incoming: mpsc::Sender<String>,
    outgoing: mpsc::Receiver<String>,
}

struct MemoryWrite {
    outgoing: PollSender<String>,
    pending: Vec<u8>,
}

impl MemoryWrite {
    fn poll_drain(&mut self, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            if ready!(self.outgoing.poll_reserve(cx)).is_err() {
                return Poll::Ready(Err(broken_pipe()));
            }
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let text = String::from_utf8_lossy(&line[..end]).into_owned();
            if self.outgoing.send_item(text).is_err() {
                return Poll::Ready(Err(broken_pipe()));
            }
        }
        Poll::Ready(Ok(()))
    }
}

fn broken_pipe() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::BrokenPipe, "connection is closed")
}

impl AsyncWrite for MemoryWrite {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        ready!(this.poll_drain(cx))?;
        this.pending.extend_from_slice(buf);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.get_mut().poll_drain(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        ready!(this.poll_drain(cx))?;
        this.outgoing.close();
        Poll::Ready(Ok(()))
    }
}

impl MemoryTransport {
    /// Creates a bounded pair of message-oriented endpoints.
    #[must_use]
    pub fn pair(capacity: usize) -> (Transport, MemoryPeer) {
        let (client_tx, server_rx) = mpsc::channel(capacity.max(1));
        let (server_tx, client_rx) = mpsc::channel(capacity.max(1));
        let writer = FrameWriter::from_box(
            Box::new(MemoryWrite {
                outgoing: PollSender::new(server_tx),
                pending: Vec::new(),
            }),
            Arc::new(AtomicBool::new(false)),
        );
        (
            Transport::Memory(Self {
                incoming: Some(server_rx),
                writer,
                closed: false,
            }),
            MemoryPeer {
                incoming: client_tx,
                outgoing: client_rx,
            },
        )
    }

    pub(super) async fn read_frame(&mut self) -> Result<String, ReadFrameError> {
        if self.closed {
            return Err(ReadFrameError::Closed);
        }
        let Some(incoming) = self.incoming.as_mut() else {
            self.closed = true;
            return Err(ReadFrameError::Closed);
        };
        let Some(frame) = incoming.recv().await else {
            self.closed = true;
            self.incoming = None;
            return Err(ReadFrameError::EndOfInput);
        };
        if frame.len() > MAX_FRAME_BYTES {
            self.closed = true;
            self.incoming = None;
            return Err(ReadFrameError::FrameTooLarge(MAX_FRAME_BYTES));
        }
        Ok(frame)
    }

    pub(super) fn writer(&self) -> FrameWriter {
        self.writer.clone()
    }

    pub(super) async fn write_frame(&mut self, frame: &str) -> Result<(), WireError> {
        if self.closed || frame.len() > MAX_FRAME_BYTES {
            return Err(WireError::Frame);
        }
        self.writer.write_frame(frame).await
    }

    pub(super) async fn close(&mut self) {
        self.closed = true;
        self.incoming = None;
        self.writer.close().await;
    }
}

impl MemoryPeer {
    /// Sends one complete frame to the transport endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when the peer endpoint is closed.
    pub async fn send_frame(&self, frame: impl Into<String>) -> Result<(), WireError> {
        self.incoming
            .send(frame.into())
            .await
            .map_err(|_| WireError::Transport("connection is closed".to_owned()))
    }

    /// Reads one complete frame written by the transport endpoint.
    #[must_use]
    pub async fn read_frame(&mut self) -> Option<String> {
        self.outgoing.recv().await
    }
}
