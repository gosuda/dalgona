//! WebSocket dal-protocol transport over the Hyper upgrade.
//!
//! Each text frame carries one complete version-1 JSON-RPC message with the
//! same method table, schema, limits, auth, and connection lifetime as RPC.
//! Binary frames close the connection with code 1003; oversized messages
//! close the transport without a close frame, per the pinned
//! `tokio-tungstenite` limit semantics. Token-bearing headers and
//! subprotocols never enter logs. Disconnection leaves sessions and turns
//! running.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll, ready};

use futures::StreamExt;
use hyper::header::HeaderValue;
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use tokio::io::AsyncWrite;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role, frame::coding::CloseCode};
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};

use crate::error::WireError;
use crate::token::SecretToken;
use crate::transport::{FrameWriter, ReadFrameError};

/// Maximum WebSocket message size in bytes (16 MiB).
const WS_FRAME_CAP: usize = 16_777_216;

/// One WebSocket protocol transport.
pub struct WebSocketTransport {
    /// The inbound message stream.
    reader: Arc<Mutex<WsStream>>,
    /// The shared writer for protocol replies.
    writer: FrameWriter,
    /// Shared close state.
    flags: Arc<WsFlags>,
}

type WsStream = Pin<Box<dyn futures::Stream<Item = Result<Message, WsError>> + Send>>;
type WsSink = Pin<Box<dyn futures::Sink<Message, Error = WsError> + Send>>;
type WsError = tokio_tungstenite::tungstenite::Error;

/// Shared close state for one WebSocket connection.
#[derive(Default)]
struct WsFlags {
    /// True once the transport is closed.
    closed: AtomicBool,
    /// True when a binary frame arrived (close code 1003).
    binary: AtomicBool,
}

impl WebSocketTransport {
    /// Builds a transport from boxed stream and sink halves.
    fn halves(stream: WsStream, sink: WsSink) -> Self {
        let flags = Arc::new(WsFlags::default());
        let writer = FrameWriter::from_box(
            Box::new(WsWrite {
                sink,
                flags: Arc::clone(&flags),
                close_sent: false,
            }),
            Arc::new(AtomicBool::new(false)),
        );
        Self {
            reader: Arc::new(Mutex::new(stream)),
            writer,
            flags,
        }
    }

    /// Builds a server transport from an upgraded stream with the frame cap applied.
    pub async fn accept(stream: Upgraded) -> Self {
        let config = WebSocketConfig::default()
            .max_message_size(Some(WS_FRAME_CAP))
            .max_frame_size(Some(WS_FRAME_CAP));
        let socket = tokio_tungstenite::WebSocketStream::from_raw_socket(
            TokioIo::new(stream),
            Role::Server,
            Some(config),
        )
        .await;
        let (sink, stream) = socket.split();
        Self::halves(Box::pin(stream), Box::pin(sink))
    }

    /// Connects one client transport to a WebSocket URL.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when the connection fails.
    pub async fn connect(url: url::Url) -> Result<Self, WireError> {
        Self::connect_request(url.as_str()).await
    }

    /// Connects with a bearer token in the WebSocket upgrade authorization header.
    ///
    /// The token is never placed in the URL, query, subprotocol, or log output.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when request construction or connection fails.
    pub async fn connect_with_auth(url: &str, token: &str) -> Result<Self, WireError> {
        let mut request = url
            .into_client_request()
            .map_err(|error| WireError::Transport(format!("websocket connect failed: {error}")))?;
        let authorization = format!("Bearer {token}")
            .parse::<tokio_tungstenite::tungstenite::http::HeaderValue>()
            .map_err(|error| {
                WireError::Transport(format!(
                    "websocket authorization header is invalid: {error}"
                ))
            })?;
        request.headers_mut().insert(
            tokio_tungstenite::tungstenite::http::header::AUTHORIZATION,
            authorization,
        );
        Self::connect_request(request).await
    }

    async fn connect_request<R>(request: R) -> Result<Self, WireError>
    where
        R: IntoClientRequest + Unpin,
    {
        let (socket, _) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|error| WireError::Transport(format!("websocket connect failed: {error}")))?;
        let (sink, stream) = socket.split();
        Ok(Self::halves(Box::pin(stream), Box::pin(sink)))
    }

    /// Returns the shared writer for protocol replies.
    #[must_use]
    pub fn writer(&self) -> FrameWriter {
        self.writer.clone()
    }

    /// Reads the next complete text frame.
    ///
    /// # Errors
    ///
    /// Returns [`ReadFrameError`] when the peer closes, sends binary data
    /// (closed with code 1003), exceeds the frame cap, or the stream fails.
    pub async fn read_frame(&mut self) -> Result<String, ReadFrameError> {
        let mut reader = self.reader.lock().await;
        loop {
            if self.flags.closed.load(Ordering::Acquire) {
                return Err(ReadFrameError::Closed);
            }
            match reader.next().await {
                None | Some(Ok(Message::Close(_))) => {
                    self.flags.closed.store(true, Ordering::Release);
                    return Err(ReadFrameError::EndOfInput);
                }
                Some(Err(_)) => {
                    self.flags.closed.store(true, Ordering::Release);
                    return Err(ReadFrameError::Closed);
                }
                Some(Ok(Message::Text(text))) => return Ok(text.to_string()),
                Some(Ok(Message::Binary(_))) => {
                    self.flags.binary.store(true, Ordering::Release);
                    self.flags.closed.store(true, Ordering::Release);
                    drop(reader);
                    self.writer.shutdown_sink().await;
                    return Err(ReadFrameError::Closed);
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => {}
            }
        }
    }

    /// Writes one text frame.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when the frame is not valid or the transport is closed.
    pub async fn write_frame(&mut self, frame: &str) -> Result<(), WireError> {
        self.writer.write_frame(frame).await
    }

    /// Closes the transport.
    pub async fn close(&mut self) {
        self.flags.closed.store(true, Ordering::Release);
        self.writer.shutdown_sink().await;
    }
}

/// An [`AsyncWrite`] adapter over a WebSocket sink for [`FrameWriter`].
struct WsWrite {
    sink: WsSink,
    flags: Arc<WsFlags>,
    close_sent: bool,
}

impl AsyncWrite for WsWrite {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        if this.flags.closed.load(Ordering::Acquire) {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "websocket is closed",
            )));
        }
        let text = match std::str::from_utf8(buf) {
            Ok(text) => text.strip_suffix('\n').unwrap_or(text),
            Err(_) => {
                return Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "websocket frame is not UTF-8",
                )));
            }
        };
        let message = Message::Text(text.to_owned().into());
        match this.sink.as_mut().poll_ready(cx) {
            Poll::Ready(Ok(())) => {
                if this.sink.as_mut().start_send(message).is_err() {
                    this.flags.closed.store(true, Ordering::Release);
                    Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "websocket is closed",
                    )))
                } else {
                    Poll::Ready(Ok(buf.len()))
                }
            }
            Poll::Ready(Err(_)) => {
                this.flags.closed.store(true, Ordering::Release);
                Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "websocket is closed",
                )))
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut().sink.as_mut().poll_flush(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Ready(Err(_)) => Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "websocket is closed",
            ))),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if !this.close_sent {
            let code = if this.flags.binary.load(Ordering::Acquire) {
                CloseCode::Unsupported
            } else {
                CloseCode::Normal
            };
            let frame = CloseFrame {
                code,
                reason: "".into(),
            };
            let ready = ready!(this.sink.as_mut().poll_ready(cx));
            this.close_sent = true;
            this.flags.closed.store(true, Ordering::Release);
            if ready.is_err()
                || this
                    .sink
                    .as_mut()
                    .start_send(Message::Close(Some(frame)))
                    .is_err()
            {
                return Poll::Ready(Ok(()));
            }
        }
        let _ = ready!(this.sink.as_mut().poll_flush(cx));
        Poll::Ready(Ok(()))
    }
}

/// Serves the dal protocol on one upgraded WebSocket connection.
///
/// The HTTP layer enforces the origin and token policy through the shared
/// upgrade helper before the handshake, and the upgrade driver re-checks the
/// presented headers after it completes; the expected policy travels here so
/// direct callers cannot bypass it. This task runs the version-1 connection
/// to completion. Disconnection leaves sessions and turns running. When
/// `drain` fires the connection answers new requests with `-32009` and closes
/// within the drain grace.
///
/// # Errors
///
/// Returns [`WireError`] when the transport fails.
pub async fn serve_websocket(
    host: dal_agent::Host,
    upgraded: Upgraded,
    token: Option<SecretToken>,
    allowed_origins: &[HeaderValue],
    drain: tokio_util::sync::CancellationToken,
) -> Result<(), WireError> {
    tracing::debug!(
        public = token.is_some(),
        origins = allowed_origins.len(),
        "serving websocket connection"
    );
    let transport = WebSocketTransport::accept(upgraded).await;
    crate::rpc::serve_rpc_draining(
        host,
        crate::transport::Transport::websocket(transport),
        drain,
    )
    .await
}
