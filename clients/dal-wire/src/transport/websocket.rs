//! WebSocket dal-protocol transport over the Hyper upgrade.
//!
//! Each text frame carries one complete version-1 JSON-RPC message with the
//! same method table, schema, limits, auth, and connection lifetime as RPC.
//! Binary frames close the connection with code 1003; oversized messages
//! close the transport without a close frame, per the pinned
//! `tokio-tungstenite` limit semantics. Servers and clients apply the same
//! message cap. Token-bearing headers and subprotocols never enter logs.
//! Disconnection leaves sessions and turns running.
//!
//! One driver task per connection owns both socket halves and keeps the
//! connection alive: it pings the peer every `PING_INTERVAL` and closes the
//! connection once `PONG_TIMEOUT` passes without a received pong. Only a
//! pong moves that deadline. A peer that still exchanges messages but
//! withholds pongs has a stalled protocol stack and is closed like any
//! other silent peer. The timer lives in the driver, not in `read_frame`,
//! because the serve loop stops polling `read_frame` while it holds its
//! in-flight limit or waits on a reply write. The deadline is strict about
//! one case: while the serve loop is not taking frames and one is already
//! waiting, the driver cannot read past it to reach the peer's pong, so a
//! consumer that stalls that long with a frame waiting loses the
//! connection. The driver still takes a local close while the end of the
//! connection waits for the reader, so `close` never waits out that
//! deadline. A closed connection leaves sessions and turns running like
//! any other disconnect.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll, ready};
use std::time::Duration;

use futures::StreamExt;
use hyper::header::HeaderValue;
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;
use tokio::time::{Instant, Interval, MissedTickBehavior};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role, frame::coding::CloseCode};
use tokio_tungstenite::tungstenite::{Bytes, Message, protocol::WebSocketConfig};
use tokio_util::sync::PollSender;

use crate::error::WireError;
use crate::token::SecretToken;
use crate::transport::{FrameWriter, ReadFrameError};

/// Maximum WebSocket message size in bytes (16 MiB).
const WS_FRAME_CAP: usize = 16_777_216;

/// Time between keepalive pings on an open connection.
const PING_INTERVAL: Duration = Duration::from_secs(15);

/// Time without a received pong after which the connection closes.
const PONG_TIMEOUT: Duration = Duration::from_secs(45);

/// Requests the frame writer may leave queued for the driver. The writer
/// lock serializes writers and each holds it until its frame is flushed, so
/// one slot suffices.
const OUT_QUEUE: usize = 1;

/// Messages the driver may hand over before it waits for the frame reader.
const IN_QUEUE: usize = 1;

type WsError = tokio_tungstenite::tungstenite::Error;
type WsStream = Pin<Box<dyn futures::Stream<Item = Result<Message, WsError>> + Send>>;
type WsSink = Pin<Box<dyn futures::Sink<Message, Error = WsError> + Send>>;

/// One WebSocket protocol transport.
pub struct WebSocketTransport {
    /// Messages the driver read from the peer.
    incoming: mpsc::Receiver<Incoming>,
    /// The shared writer for protocol replies.
    writer: FrameWriter,
    /// Owns the connection driver; dropping the transport aborts it.
    tasks: JoinSet<()>,
    /// True once the connection ended or was closed locally.
    finished: bool,
}

/// One item the driver hands to the frame reader.
enum Incoming {
    /// A complete text message.
    Text(String),
    /// The connection ended for this reason; nothing follows.
    Ended(ReadFrameError),
}

/// One request from the frame writer to the driver.
enum Outgoing {
    /// A text message to send; resolves once the socket has flushed it.
    Text(String, oneshot::Sender<()>),
    /// A normal close frame to send; resolves once the socket has flushed it.
    Close(oneshot::Sender<()>),
}

impl WebSocketTransport {
    /// Builds a transport from boxed stream and sink halves and starts its driver.
    fn halves(stream: WsStream, sink: WsSink) -> Self {
        let closed = Arc::new(AtomicBool::new(false));
        let (outgoing, commands) = mpsc::channel(OUT_QUEUE);
        let (inbound, incoming) = mpsc::channel(IN_QUEUE);
        let writer = FrameWriter::from_box(
            Box::new(WsWrite {
                outgoing: PollSender::new(outgoing),
                flushed: None,
                closing: None,
                shut: false,
            }),
            Arc::clone(&closed),
        );
        let mut ticker = tokio::time::interval_at(Instant::now() + PING_INTERVAL, PING_INTERVAL);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let driver = Driver {
            stream,
            out: Output {
                sink,
                slot: None,
                ack: None,
                flushing: false,
            },
            commands,
            inbound: PollSender::new(inbound),
            held: None,
            ticker,
            deadline: Instant::now() + PONG_TIMEOUT,
            close_code: None,
            ping_due: false,
            finishing: false,
            closed,
        };
        let mut tasks = JoinSet::new();
        tasks.spawn(driver.run());
        Self {
            incoming,
            writer,
            tasks,
            finished: false,
        }
    }

    /// Builds a server transport from an upgraded stream with the frame cap applied.
    pub async fn accept(stream: Upgraded) -> Self {
        Self::server(TokioIo::new(stream)).await
    }

    /// Builds a server transport over any byte stream with the frame cap applied.
    pub(crate) async fn server<S>(io: S) -> Self
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let socket = tokio_tungstenite::WebSocketStream::from_raw_socket(
            io,
            Role::Server,
            Some(frame_cap_config()),
        )
        .await;
        let (sink, stream) = socket.split();
        Self::halves(Box::pin(stream), Box::pin(sink))
    }

    /// Connects one client transport to a WebSocket URL.
    ///
    /// An upgrade denial with `401`/`403` maps to [`WireError::Auth`] and is
    /// permanent; every other failure maps to [`WireError::Transport`] and
    /// stays retryable for the remote reconnect loop.
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
    /// An upgrade denial with `401`/`403` maps to [`WireError::Auth`] and is
    /// permanent; every other failure maps to [`WireError::Transport`] and
    /// stays retryable for the remote reconnect loop.
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
        let (socket, _) =
            tokio_tungstenite::connect_async_with_config(request, Some(frame_cap_config()), false)
                .await
                .map_err(|error| match &error {
                    WsError::Http(response) if matches!(response.status().as_u16(), 401 | 403) => {
                        tracing::debug!(
                            status = %response.status(),
                            "websocket upgrade denied; not retrying"
                        );
                        WireError::Auth
                    }
                    _ => WireError::Transport(format!("websocket connect failed: {error}")),
                })?;
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
    /// The connection driver answers pings and keeps the heartbeat while
    /// this future is not polled.
    ///
    /// # Errors
    ///
    /// Returns [`ReadFrameError`] when the peer closes, sends binary data
    /// (closed with code 1003), exceeds the frame cap, stops answering
    /// pings, or the stream fails.
    pub async fn read_frame(&mut self) -> Result<String, ReadFrameError> {
        if self.finished {
            return Err(ReadFrameError::Closed);
        }
        match self.incoming.recv().await {
            Some(Incoming::Text(text)) => Ok(text),
            Some(Incoming::Ended(error)) => {
                self.finished = true;
                Err(error)
            }
            None => {
                self.finished = true;
                Err(ReadFrameError::Closed)
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
        self.finished = true;
        self.writer.shutdown_sink().await;
        self.tasks.shutdown().await;
    }
}

/// The message limits both ends of a connection apply: a larger message ends
/// the connection instead of growing the read buffer.
fn frame_cap_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(WS_FRAME_CAP))
        .max_frame_size(Some(WS_FRAME_CAP))
}

/// The connection driver: the only owner of the socket halves.
struct Driver {
    /// The inbound message stream.
    stream: WsStream,
    /// The outbound sink and the message being written to it.
    out: Output,
    /// Requests from the frame writer.
    commands: mpsc::Receiver<Outgoing>,
    /// Hand-off to the frame reader.
    inbound: PollSender<Incoming>,
    /// An item waiting for the frame reader; the driver reads nothing more
    /// from the peer until it has gone.
    held: Option<Incoming>,
    /// Paces the keepalive pings.
    ticker: Interval,
    /// The instant the connection closes unless a pong arrives first.
    deadline: Instant,
    /// A close frame to send once the sink is idle.
    close_code: Option<CloseCode>,
    /// A ping is due once the sink is idle.
    ping_due: bool,
    /// The connection is ending: no more reads, pings, or frames to send,
    /// but close requests are still taken.
    finishing: bool,
    /// The frame writer's closed flag, raised when the driver ends.
    closed: Arc<AtomicBool>,
}

/// The sink and the single message in flight to it.
struct Output {
    sink: WsSink,
    /// The next message for the sink.
    slot: Option<Message>,
    /// The acknowledgement owed once the slot's message is flushed.
    ack: Option<oneshot::Sender<()>>,
    /// The sink holds data, or the protocol layer holds a reply, that needs a flush.
    flushing: bool,
}

/// What woke the driver.
enum Event {
    /// The ping interval elapsed.
    Tick,
    /// No pong arrived within the timeout.
    Expired,
    /// The sink finished the message in flight, or failed.
    Wrote(Result<(), WsError>),
    /// The socket produced a message, an error, or its end.
    Read(Option<Result<Message, WsError>>),
    /// The frame reader took the held item (`true`) or is gone (`false`).
    Delivered(bool),
    /// The frame writer sent a request, or every writer is gone.
    Command(Option<Outgoing>),
}

impl Output {
    /// Reports whether a message or a flush is still in flight.
    fn busy(&self) -> bool {
        self.slot.is_some() || self.flushing
    }

    /// Queues `message` for the sink; `ack` resolves once it is flushed.
    fn start(&mut self, message: Message, ack: Option<oneshot::Sender<()>>) {
        self.slot = Some(message);
        self.ack = ack;
    }

    /// Abandons whatever is in flight; dropped acknowledgements fail their writers.
    fn clear(&mut self) {
        self.slot = None;
        self.ack = None;
        self.flushing = false;
    }

    /// Sends the queued message, then flushes the sink.
    fn poll_send(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), WsError>> {
        if self.slot.is_some() {
            ready!(self.sink.as_mut().poll_ready(cx))?;
            if let Some(message) = self.slot.take() {
                self.sink.as_mut().start_send(message)?;
                self.flushing = true;
            }
        }
        ready!(self.sink.as_mut().poll_flush(cx))?;
        self.flushing = false;
        if let Some(ack) = self.ack.take() {
            let _ = ack.send(());
        }
        Poll::Ready(Ok(()))
    }
}

impl Driver {
    /// Runs the connection until it ends, then drops the socket halves.
    async fn run(mut self) {
        loop {
            if Instant::now() >= self.deadline {
                tracing::debug!("websocket peer sent no pong in time; closing");
                break;
            }
            self.refill();
            if self.done() {
                break;
            }
            let event = self.next_event().await;
            if !self.handle(event) {
                break;
            }
        }
    }

    /// Starts the next close frame or ping when the sink is idle.
    fn refill(&mut self) {
        if self.out.busy() {
            return;
        }
        if let Some(code) = self.close_code.take() {
            self.out.start(close_message(code), None);
        } else if self.ping_due && !self.finishing {
            self.ping_due = false;
            self.out.start(Message::Ping(Bytes::new()), None);
        }
    }

    /// Reports whether the connection has ended and nothing is left to deliver.
    fn done(&self) -> bool {
        self.finishing && !self.out.busy() && self.close_code.is_none() && self.held.is_none()
    }

    /// Waits for the next thing the driver has to act on.
    async fn next_event(&mut self) -> Event {
        let writing = self.out.busy();
        let holding = self.held.is_some();
        let reading = !holding && !self.finishing;
        let commanding = !writing;
        tokio::select! {
            () = tokio::time::sleep_until(self.deadline) => Event::Expired,
            _ = self.ticker.tick() => Event::Tick,
            result = std::future::poll_fn(|cx| self.out.poll_send(cx)), if writing => {
                Event::Wrote(result)
            }
            item = self.stream.next(), if reading => Event::Read(item),
            reserved = std::future::poll_fn(|cx| self.inbound.poll_reserve(cx)), if holding => {
                Event::Delivered(reserved.is_ok())
            }
            command = self.commands.recv(), if commanding => Event::Command(command),
        }
    }

    /// Applies one event; returns false when the driver must stop at once.
    ///
    /// A text request that arrives while the connection is ending is dropped,
    /// which fails its writer: no frame follows the end. A close request is
    /// always taken, so a held item the frame reader never collects cannot
    /// delay it.
    fn handle(&mut self, event: Event) -> bool {
        match event {
            Event::Expired | Event::Delivered(false) => return false,
            Event::Tick => self.ping_due = true,
            Event::Wrote(Ok(())) => {}
            Event::Wrote(Err(error)) => self.on_write_error(&error),
            Event::Read(item) => self.on_read(item),
            Event::Delivered(true) => {
                if let Some(item) = self.held.take()
                    && self.inbound.send_item(item).is_err()
                {
                    return false;
                }
            }
            Event::Command(Some(Outgoing::Text(..))) if self.finishing => {}
            Event::Command(Some(Outgoing::Text(text, ack))) => {
                self.out.start(Message::Text(text.into()), Some(ack));
            }
            Event::Command(Some(Outgoing::Close(ack))) => self.close_now(Some(ack)),
            Event::Command(None) => self.close_now(None),
        }
        true
    }

    /// Abandons the output after a failed write and ends the connection.
    ///
    /// The protocol layer also reports a finished close handshake as an
    /// error (`ConnectionClosed`); that is the normal end, not a failure.
    fn on_write_error(&mut self, error: &WsError) {
        if !matches!(error, WsError::ConnectionClosed | WsError::AlreadyClosed) {
            tracing::debug!(%error, "websocket write failed");
        }
        self.out.clear();
        self.close_code = None;
        self.finishing = true;
    }

    /// Applies one message, error, or end read from the peer.
    fn on_read(&mut self, item: Option<Result<Message, WsError>>) {
        match item {
            Some(Ok(Message::Text(text))) => self.held = Some(Incoming::Text(text.to_string())),
            Some(Ok(Message::Pong(_))) => self.deadline = Instant::now() + PONG_TIMEOUT,
            Some(Ok(Message::Ping(_))) => self.out.flushing = true,
            Some(Ok(Message::Frame(_))) => {}
            Some(Ok(Message::Binary(_))) => {
                self.end(ReadFrameError::Closed, Some(CloseCode::Unsupported));
            }
            Some(Ok(Message::Close(_))) | None => {
                self.out.flushing = true;
                self.end(ReadFrameError::EndOfInput, None);
            }
            Some(Err(WsError::Capacity(_))) => self.end(ReadFrameError::Closed, None),
            Some(Err(_)) => self.end(ReadFrameError::Closed, Some(CloseCode::Normal)),
        }
    }

    /// Ends the connection: the frame reader gets `error`, the peer a close
    /// frame with `close` when there is one.
    fn end(&mut self, error: ReadFrameError, close: Option<CloseCode>) {
        self.finishing = true;
        self.held = Some(Incoming::Ended(error));
        self.close_code = close;
    }

    /// Closes on request: a normal close frame that `ack` resolves on, and no reads.
    ///
    /// A connection that is already ending has flushed its close frame or
    /// forgone it, and the sink is idle whenever a request arrives, so the
    /// request only drops the item held for the frame reader and resolves
    /// `ack` at once.
    fn close_now(&mut self, ack: Option<oneshot::Sender<()>>) {
        self.held = None;
        if self.finishing {
            if let Some(ack) = ack {
                let _ = ack.send(());
            }
            return;
        }
        self.finishing = true;
        self.out.start(close_message(CloseCode::Normal), ack);
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
    }
}

/// Builds a close message with no reason text.
fn close_message(code: CloseCode) -> Message {
    Message::Close(Some(CloseFrame {
        code,
        reason: "".into(),
    }))
}

/// Builds the error for a write after the connection ended.
fn broken_pipe() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::BrokenPipe, "websocket is closed")
}

/// An [`AsyncWrite`] adapter that hands frames to the connection driver.
struct WsWrite {
    outgoing: PollSender<Outgoing>,
    /// Resolves once the driver has flushed the latest frame handed over.
    flushed: Option<oneshot::Receiver<()>>,
    /// Resolves once the driver has flushed the close frame it was asked for.
    closing: Option<oneshot::Receiver<()>>,
    /// True once the close finished.
    shut: bool,
}

impl AsyncWrite for WsWrite {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let Ok(text) = std::str::from_utf8(buf) else {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "websocket frame is not UTF-8",
            )));
        };
        let text = text.strip_suffix('\n').unwrap_or(text);
        if ready!(this.outgoing.poll_reserve(cx)).is_err() {
            return Poll::Ready(Err(broken_pipe()));
        }
        let (ack, flushed) = oneshot::channel();
        if this
            .outgoing
            .send_item(Outgoing::Text(text.to_owned(), ack))
            .is_err()
        {
            return Poll::Ready(Err(broken_pipe()));
        }
        this.flushed = Some(flushed);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if let Some(flushed) = this.flushed.as_mut() {
            let result = ready!(Pin::new(flushed).poll(cx));
            this.flushed = None;
            if result.is_err() {
                return Poll::Ready(Err(broken_pipe()));
            }
        }
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if this.shut {
            return Poll::Ready(Ok(()));
        }
        if this.closing.is_none() {
            // A driver that has already ended has nothing left to close.
            if ready!(this.outgoing.poll_reserve(cx)).is_err() {
                this.shut = true;
                return Poll::Ready(Ok(()));
            }
            let (ack, closing) = oneshot::channel();
            if this.outgoing.send_item(Outgoing::Close(ack)).is_err() {
                this.shut = true;
                return Poll::Ready(Ok(()));
            }
            this.closing = Some(closing);
        }
        if let Some(closing) = this.closing.as_mut() {
            // The driver resolves the acknowledgement once the close frame is
            // flushed and drops it when it ends first: either finishes the close.
            let _ = ready!(Pin::new(closing).poll(cx));
        }
        this.shut = true;
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
