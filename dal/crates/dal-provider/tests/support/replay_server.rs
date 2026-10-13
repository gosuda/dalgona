//! Loopback replay server for the provider integration suites.
//!
//! A case directory `tests/fixtures/replay/<family>/<case>/` holds
//! `exchange.toml` plus the body and frame files it names. The server binds
//! `127.0.0.1:0`, speaks HTTP/1.1 through `hyper` and WebSocket through
//! `tokio-tungstenite`, and consumes the recorded exchanges in arrival order.
//! A request that differs from its exchange in kind, method, path and query,
//! a required header, or body bytes gets status 418 with the body
//! `replay mismatch: <index>: <field>: expected <x>, got <y>`, and the same
//! text is recorded. [`ReplayServer::finish`] stops every task and fails when
//! a mismatch was recorded or an exchange was left unconsumed. Mismatch text
//! never carries a value of a credential header or of a credential JSON or
//! form member; request bodies are read up to [`MAX_BODY`] bytes and are not
//! kept after the comparison.

use std::{
    convert::Infallible,
    error::Error,
    fmt,
    future::poll_fn,
    net::{Ipv4Addr, SocketAddr},
    path::{Component, Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, ready},
    time::Duration,
};

use futures::{SinkExt, StreamExt};
use hyper::{
    HeaderMap, Method, Request, Response, StatusCode, Uri,
    body::{Body, Bytes, Frame, Incoming, SizeHint},
    header::{
        CONNECTION, CONTENT_TYPE, HeaderName, HeaderValue, SEC_WEBSOCKET_ACCEPT, SEC_WEBSOCKET_KEY,
        SEC_WEBSOCKET_VERSION, UPGRADE,
    },
    http::request::Parts,
    server::conn::http1,
    service::service_fn,
    upgrade::OnUpgrade,
};
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use tokio::{
    net::{TcpListener, TcpSocket, TcpStream},
    sync::oneshot,
    task::JoinSet,
    time::Sleep,
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Error as WsError, Message, Utf8Bytes,
        error::ProtocolError,
        handshake::derive_accept_key,
        protocol::{CloseFrame, Role, WebSocketConfig, frame::coding::CloseCode},
    },
};

/// Largest request body the server reads before reporting a mismatch.
pub(crate) const MAX_BODY: usize = 16 << 20;
/// Largest WebSocket message or frame the server accepts from a client.
const MAX_MESSAGE: usize = 16 << 20;
/// Fixture header value that matches any present value.
const REDACTED: &str = "<redacted>";
/// Headers whose received values never appear in mismatch text.
const SECRET_HEADERS: [&str; 5] = [
    "authorization",
    "x-api-key",
    "chatgpt-account-id",
    "cookie",
    "set-cookie",
];
/// JSON members and form fields whose values never appear in mismatch text.
const SECRET_MEMBERS: [&str; 6] = [
    "access_token",
    "refresh_token",
    "id_token",
    "account_id",
    "user_id",
    "email",
];
/// Bytes of context shown before the first differing byte of a body.
const CONTEXT: usize = 24;
/// Longest excerpt of a value shown in mismatch text.
const EXCERPT: usize = 96;

/// A fixture load, bind, or verdict failure with a readable message.
#[derive(Debug)]
pub(crate) struct ReplayError(Box<str>);

impl fmt::Display for ReplayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for ReplayError {}

fn fail(message: impl Into<Box<str>>) -> ReplayError {
    ReplayError(message.into())
}

/// Returns the case directory `tests/fixtures/replay/<family>/<case>`.
#[must_use]
pub(crate) fn fixture_dir(family: &str, case: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/replay")
        .join(family)
        .join(case)
}

/// A running replay server; call [`ReplayServer::finish`] to get the verdict.
///
/// Dropping the server without `finish` aborts every listener and
/// connection task without a verdict.
#[derive(Debug)]
#[must_use = "call `finish` to learn whether the replay matched"]
pub(crate) struct ReplayServer {
    addr: SocketAddr,
    shared: Arc<Shared>,
    stop: oneshot::Sender<()>,
    tasks: JoinSet<()>,
}

/// A loaded fixture whose loopback port is bound but not listening, so
/// connects are refused until [`ReplayReservation::listen`].
#[derive(Debug)]
#[must_use = "call `listen` to start serving the reserved port"]
pub(crate) struct ReplayReservation {
    addr: SocketAddr,
    socket: TcpSocket,
    fixture: Fixture,
}

impl ReplayServer {
    /// Loads the case directory and starts serving it on `127.0.0.1:0`.
    pub(crate) async fn start(case: &Path) -> Result<Self, ReplayError> {
        let fixture = Fixture::load(case)?;
        let listener = bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).await?;
        Self::launch(listener, fixture)
    }

    /// Loads the case directory and reserves a loopback port that refuses
    /// connects until the reservation listens.
    pub(crate) fn reserve(case: &Path) -> Result<ReplayReservation, ReplayError> {
        let fixture = Fixture::load(case)?;
        let socket = TcpSocket::new_v4()
            .and_then(|socket| {
                socket.bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
                Ok(socket)
            })
            .map_err(|error| fail(format!("replay server cannot reserve a port: {error}")))?;
        let addr = socket
            .local_addr()
            .map_err(|error| fail(format!("replay server has no local address: {error}")))?;
        Ok(ReplayReservation {
            addr,
            socket,
            fixture,
        })
    }

    fn launch(listener: TcpListener, fixture: Fixture) -> Result<Self, ReplayError> {
        let addr = local_addr(&listener)?;
        let shared = Arc::new(Shared {
            exchanges: fixture.exchanges,
            state: Mutex::default(),
            open: AtomicUsize::new(0),
            max_open: AtomicUsize::new(0),
        });
        let (stop, stopped) = oneshot::channel();
        let mut tasks = JoinSet::new();
        tasks.spawn(accept_loop(listener, Arc::clone(&shared), stopped));
        Ok(Self {
            addr,
            shared,
            stop,
            tasks,
        })
    }

    /// The bound loopback address.
    #[must_use]
    pub(crate) fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// `http://127.0.0.1:<port>` without a trailing slash.
    #[must_use]
    pub(crate) fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Every mismatch recorded so far, in order.
    #[must_use]
    pub(crate) fn mismatches(&self) -> Vec<String> {
        lock(&self.shared.state)
            .mismatches
            .iter()
            .map(|message| String::from(&**message))
            .collect()
    }

    /// How many fixture exchanges requests have claimed so far.
    #[must_use]
    pub(crate) fn consumed(&self) -> usize {
        lock(&self.shared.state)
            .next
            .min(self.shared.exchanges.len())
    }

    /// Requests whose response body or WebSocket session is still open.
    #[must_use]
    pub(crate) fn open_requests(&self) -> usize {
        self.shared.open.load(Ordering::SeqCst)
    }

    /// The highest value [`ReplayServer::open_requests`] has reached.
    #[must_use]
    pub(crate) fn max_open_requests(&self) -> usize {
        self.shared.max_open.load(Ordering::SeqCst)
    }

    /// Stops the listener, aborts every connection, and returns the verdict.
    ///
    /// # Errors
    ///
    /// Returns one line per recorded mismatch or failed server task, plus
    /// `replay incomplete: <n> of <m> exchanges consumed` when exchanges
    /// remain.
    pub(crate) async fn finish(self) -> Result<(), ReplayError> {
        let Self {
            shared,
            stop,
            mut tasks,
            ..
        } = self;
        let _ = stop.send(());
        let mut lines = Vec::new();
        while let Some(joined) = tasks.join_next().await {
            if let Err(error) = joined {
                lines.push(format!("replay server task failed: {error}"));
            }
        }
        let state = lock(&shared.state);
        lines.extend(state.mismatches.iter().map(|line| String::from(&**line)));
        let total = shared.exchanges.len();
        let consumed = state.next.min(total);
        if consumed < total {
            lines.push(format!(
                "replay incomplete: {consumed} of {total} exchanges consumed"
            ));
        }
        if lines.is_empty() {
            Ok(())
        } else {
            Err(fail(lines.join("\n")))
        }
    }
}

impl ReplayReservation {
    /// The reserved loopback address.
    #[must_use]
    pub(crate) fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// `http://127.0.0.1:<port>` without a trailing slash.
    #[must_use]
    pub(crate) fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Starts listening on the reserved port and serving the fixture.
    pub(crate) fn listen(self) -> Result<ReplayServer, ReplayError> {
        let listener = self.socket.listen(1024).map_err(|error| {
            fail(format!(
                "replay server cannot listen on {}: {error}",
                self.addr
            ))
        })?;
        ReplayServer::launch(listener, self.fixture)
    }
}

async fn bind(addr: SocketAddr) -> Result<TcpListener, ReplayError> {
    TcpListener::bind(addr)
        .await
        .map_err(|error| fail(format!("replay server cannot bind {addr}: {error}")))
}

fn local_addr(listener: &TcpListener) -> Result<SocketAddr, ReplayError> {
    listener
        .local_addr()
        .map_err(|error| fail(format!("replay server has no local address: {error}")))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---------------------------------------------------------------------------
// Fixture model and parser.

#[derive(Debug)]
struct Fixture {
    exchanges: Box<[Exchange]>,
}

#[derive(Debug)]
struct Exchange {
    kind: Kind,
    request: ExpectedRequest,
    reply: Reply,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Http,
    Websocket,
}

impl fmt::Display for Kind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Http => "http",
            Self::Websocket => "websocket",
        })
    }
}

#[derive(Debug)]
struct ExpectedRequest {
    method: Method,
    path: Box<str>,
    headers: Box<[(HeaderName, Box<str>)]>,
    body: Option<Bytes>,
}

#[derive(Debug)]
enum Reply {
    Http(HttpReply),
    Frames(Box<[WsStep]>),
}

#[derive(Debug)]
struct HttpReply {
    status: StatusCode,
    headers: Box<[(HeaderName, HeaderValue)]>,
    body: Bytes,
    /// Ascending end offsets of the separate writes, ending at the limit.
    cuts: Arc<[usize]>,
    end: End,
    /// Pause between consecutive pieces.
    delay: Duration,
}

/// What the server does once the written bytes reach their limit.
#[derive(Clone, Copy, Debug)]
enum End {
    Finish,
    Stall,
    Close,
}

#[derive(Debug)]
enum WsStep {
    Receive(Utf8Bytes),
    Send(Utf8Bytes),
    Close { code: u16, reason: Utf8Bytes },
    Stall,
}

impl WsStep {
    fn describe(&self) -> String {
        match self {
            Self::Receive(text) => format!("text {}", show(&redact(text.as_bytes()))),
            Self::Send(_) => String::from("a server text frame sent"),
            Self::Close { code, .. } => format!("server close {code}"),
            Self::Stall => String::from("stall"),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileWire {
    #[serde(default)]
    exchange: Vec<ExchangeWire>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExchangeWire {
    kind: KindWire,
    request: RequestWire,
    response: ResponseWire,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum KindWire {
    Http,
    Websocket,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestWire {
    method: Option<String>,
    path: String,
    #[serde(default)]
    headers: toml::Table,
    body: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponseWire {
    status: Option<u16>,
    #[serde(default)]
    headers: toml::Table,
    body: Option<String>,
    chunks: Option<Vec<usize>>,
    stall_after: Option<usize>,
    close_after: Option<usize>,
    chunk_delay_ms: Option<u64>,
    frames: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FrameWire {
    from: SideWire,
    text: Option<String>,
    close: Option<CloseWire>,
    stall: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum SideWire {
    Client,
    Server,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseWire {
    code: u16,
    reason: String,
}

impl Fixture {
    /// Reads a case directory synchronously; fixtures are small repo files
    /// and a blocking task would let a paused clock auto-advance.
    fn load(case: &Path) -> Result<Self, ReplayError> {
        let file = case.join("exchange.toml");
        let at = file.display();
        let text =
            std::fs::read_to_string(&file).map_err(|error| fail(format!("{at}: {error}")))?;
        let wire: FileWire =
            toml::from_str(&text).map_err(|error| fail(format!("{at}: {error}")))?;
        let exchanges = wire
            .exchange
            .into_iter()
            .enumerate()
            .map(|(index, exchange)| {
                parse_exchange(case, exchange)
                    .map_err(|error| fail(format!("{at}: exchange {index}: {error}")))
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { exchanges })
    }
}

fn parse_exchange(case: &Path, wire: ExchangeWire) -> Result<Exchange, ReplayError> {
    let kind = match wire.kind {
        KindWire::Http => Kind::Http,
        KindWire::Websocket => Kind::Websocket,
    };
    let request = parse_request(case, kind, wire.request)?;
    let mut response = wire.response;
    let reply = match (kind, response.frames.take()) {
        (Kind::Http, Some(_)) => {
            return Err(fail("response.frames: http exchanges send no frames"));
        }
        (Kind::Websocket, Some(name)) => {
            if response.status.is_some()
                || !response.headers.is_empty()
                || response.body.is_some()
                || response.chunks.is_some()
                || response.stall_after.is_some()
                || response.close_after.is_some()
                || response.chunk_delay_ms.is_some()
            {
                return Err(fail(
                    "response: frames take no status, headers, body, chunks, stall_after, close_after, or chunk_delay_ms",
                ));
            }
            let bytes = read_named(case, &name)?;
            let text = std::str::from_utf8(&bytes)
                .map_err(|_| fail(format!("response.frames: {name} is not UTF-8")))?;
            Reply::Frames(parse_frames(text).map_err(|error| fail(format!("{name}:{error}")))?)
        }
        (_, None) => Reply::Http(parse_http_reply(case, response)?),
    };
    Ok(Exchange {
        kind,
        request,
        reply,
    })
}

fn parse_request(
    case: &Path,
    kind: Kind,
    wire: RequestWire,
) -> Result<ExpectedRequest, ReplayError> {
    let method = match (kind, wire.method) {
        (Kind::Http, None) => return Err(fail("request.method: required for http")),
        (Kind::Websocket, None) => Method::GET,
        (_, Some(method)) => Method::from_bytes(method.as_bytes())
            .map_err(|_| fail(format!("request.method: invalid method {method:?}")))?,
    };
    if kind == Kind::Websocket && method != Method::GET {
        return Err(fail("request.method: websocket handshakes use GET"));
    }
    if !wire.path.starts_with('/') {
        return Err(fail(format!(
            "request.path: {:?} must start with /",
            wire.path
        )));
    }
    let headers: Vec<(HeaderName, Box<str>)> = header_strings(wire.headers, "request")?
        .into_iter()
        .map(|(name, value)| (name, value.into()))
        .collect();
    let body = wire.body.map(|name| read_named(case, &name)).transpose()?;
    Ok(ExpectedRequest {
        method,
        path: wire.path.into(),
        headers: headers.into(),
        body,
    })
}

fn parse_http_reply(case: &Path, wire: ResponseWire) -> Result<HttpReply, ReplayError> {
    let status = wire
        .status
        .ok_or_else(|| fail("response.status: required without frames"))?;
    let status = StatusCode::from_u16(status)
        .ok()
        .filter(|status| !status.is_informational())
        .ok_or_else(|| fail(format!("response.status: {status} is not a final status")))?;
    let mut headers = Vec::with_capacity(wire.headers.len());
    for (name, value) in header_strings(wire.headers, "response")? {
        let value = HeaderValue::from_str(&value)
            .map_err(|_| fail(format!("response.headers.{name}: invalid header value")))?;
        headers.push((name, value));
    }
    let body = wire
        .body
        .map(|name| read_named(case, &name))
        .transpose()?
        .unwrap_or_default();
    // The lower of the two offsets wins; at equal offsets the stall wins.
    let (end, limit) = match (wire.stall_after, wire.close_after) {
        (None, None) => (End::Finish, body.len()),
        (Some(stall), Some(close)) if stall <= close => (End::Stall, stall),
        (_, Some(close)) => (End::Close, close),
        (Some(stall), None) => (End::Stall, stall),
    };
    if limit > body.len() {
        return Err(fail(format!(
            "response: limit {limit} exceeds the {}-byte body",
            body.len()
        )));
    }
    let mut cuts = wire.chunks.unwrap_or_default();
    let mut previous = 0;
    for &cut in &cuts {
        if cut <= previous || cut >= limit {
            return Err(fail(format!(
                "response.chunks: {cut} must ascend strictly inside 1..{limit}"
            )));
        }
        previous = cut;
    }
    if limit > 0 {
        cuts.push(limit);
    }
    if let Ok(text) = std::str::from_utf8(&body)
        && let Some(cut) = cuts.iter().find(|&&cut| !text.is_char_boundary(cut))
    {
        return Err(fail(format!(
            "response: offset {cut} splits a UTF-8 character"
        )));
    }
    Ok(HttpReply {
        status,
        headers: headers.into(),
        body,
        cuts: cuts.into(),
        end,
        delay: Duration::from_millis(wire.chunk_delay_ms.unwrap_or(0)),
    })
}

fn header_strings(
    table: toml::Table,
    side: &str,
) -> Result<Vec<(HeaderName, String)>, ReplayError> {
    let mut out: Vec<(HeaderName, String)> = Vec::with_capacity(table.len());
    for (key, value) in table {
        let toml::Value::String(value) = value else {
            return Err(fail(format!("{side}.headers.{key}: must be a string")));
        };
        let name = HeaderName::from_bytes(key.as_bytes())
            .map_err(|_| fail(format!("{side}.headers.{key}: invalid header name")))?;
        if out.iter().any(|(seen, _)| *seen == name) {
            return Err(fail(format!("{side}.headers.{key}: duplicate header name")));
        }
        out.push((name, value));
    }
    Ok(out)
}

/// Reads a file named relative to the case directory; the name may not
/// leave the directory.
fn read_named(case: &Path, name: &str) -> Result<Bytes, ReplayError> {
    let relative = Path::new(name);
    if name.is_empty()
        || !relative
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err(fail(format!(
            "{name:?}: must name a file inside the case directory"
        )));
    }
    std::fs::read(case.join(relative))
        .map(Bytes::from)
        .map_err(|error| fail(format!("{name}: {error}")))
}

fn parse_frames(text: &str) -> Result<Box<[WsStep]>, ReplayError> {
    let mut steps = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let number = number + 1;
        if line.trim().is_empty() {
            continue;
        }
        if matches!(steps.last(), Some(WsStep::Close { .. } | WsStep::Stall)) {
            return Err(fail(format!(
                "{number}: nothing may follow a server close or stall"
            )));
        }
        let wire: FrameWire =
            sonic_rs::from_str(line).map_err(|error| fail(format!("{number}: {error}")))?;
        steps.push(parse_frame(wire).map_err(|error| fail(format!("{number}: {error}")))?);
    }
    Ok(steps.into())
}

fn parse_frame(wire: FrameWire) -> Result<WsStep, ReplayError> {
    match (wire.from, wire.text, wire.close, wire.stall) {
        (SideWire::Client, Some(text), None, None) => Ok(WsStep::Receive(text.into())),
        (SideWire::Server, Some(text), None, None) => Ok(WsStep::Send(text.into())),
        (SideWire::Server, None, Some(close), None) => {
            if !matches!(close.code, 1000..=1003 | 1007..=1014 | 3000..=4999) {
                return Err(fail(format!("close code {} cannot be sent", close.code)));
            }
            if close.reason.len() > 123 {
                return Err(fail("close reason exceeds 123 bytes"));
            }
            Ok(WsStep::Close {
                code: close.code,
                reason: close.reason.into(),
            })
        }
        (SideWire::Server, None, None, Some(true)) => Ok(WsStep::Stall),
        _ => Err(fail(
            "expected exactly one of client text, server text, server close, or server stall true",
        )),
    }
}

// ---------------------------------------------------------------------------
// Server.

#[derive(Debug)]
struct Shared {
    exchanges: Box<[Exchange]>,
    state: Mutex<State>,
    open: AtomicUsize,
    max_open: AtomicUsize,
}

#[derive(Debug, Default)]
struct State {
    next: usize,
    mismatches: Vec<Box<str>>,
}

impl Shared {
    fn claim(&self) -> usize {
        let mut state = lock(&self.state);
        let index = state.next;
        state.next += 1;
        index
    }

    fn note(&self, line: String) {
        lock(&self.state).mismatches.push(line.into());
    }

    fn record(&self, index: usize, miss: &Miss) -> String {
        let Miss {
            field,
            expected,
            got,
        } = miss;
        let line = format!("replay mismatch: {index}: {field}: expected {expected}, got {got}");
        self.note(line.clone());
        line
    }

    fn refuse(&self, index: usize, miss: &Miss, guard: OpenGuard) -> Response<ReplayBody> {
        let line = self.record(index, miss);
        let mut response = Response::new(ReplayBody::full(Bytes::from(line), Some(guard)));
        *response.status_mut() = StatusCode::IM_A_TEAPOT;
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        response
    }
}

/// Counts one open request from arrival until its response ends.
#[derive(Debug)]
struct OpenGuard(Arc<Shared>);

impl OpenGuard {
    fn enter(shared: &Arc<Shared>) -> Self {
        let now = shared.open.fetch_add(1, Ordering::SeqCst) + 1;
        shared.max_open.fetch_max(now, Ordering::SeqCst);
        Self(Arc::clone(shared))
    }
}

impl Drop for OpenGuard {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::SeqCst);
    }
}

/// One field-level difference between a request and its exchange.
#[derive(Debug)]
struct Miss {
    field: String,
    expected: String,
    got: String,
}

impl Miss {
    fn new(field: impl Into<String>, expected: impl Into<String>, got: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            expected: expected.into(),
            got: got.into(),
        }
    }
}

/// An upgrade accepted by the service, run after `hyper` hands the socket
/// over.
#[derive(Debug)]
struct PendingSocket {
    on_upgrade: OnUpgrade,
    index: usize,
    guard: OpenGuard,
}

type Slot = Arc<Mutex<Option<PendingSocket>>>;
type Socket = WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>;

async fn accept_loop(listener: TcpListener, shared: Arc<Shared>, mut stop: oneshot::Receiver<()>) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            _ = &mut stop => break,
            Some(joined) = connections.join_next() => {
                if let Err(error) = joined {
                    shared.note(format!("replay server connection task failed: {error}"));
                }
            },
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    connections.spawn(connection(Arc::clone(&shared), stream));
                }
                Err(error) => {
                    shared.note(format!("replay server accept failed: {error}"));
                    break;
                }
            },
        }
    }
    connections.shutdown().await;
}

async fn connection(shared: Arc<Shared>, stream: TcpStream) {
    // Separate chunk writes should leave as separate segments.
    let _ = stream.set_nodelay(true);
    let slot: Slot = Arc::default();
    let service = service_fn({
        let shared = Arc::clone(&shared);
        let slot = Arc::clone(&slot);
        move |request| serve(Arc::clone(&shared), Arc::clone(&slot), request)
    });
    let _ = http1::Builder::new()
        .serve_connection(TokioIo::new(stream), service)
        .with_upgrades()
        .await;
    let pending = lock(&slot).take();
    if let Some(pending) = pending {
        websocket(&shared, pending).await;
    }
}

async fn serve(
    shared: Arc<Shared>,
    slot: Slot,
    mut request: Request<Incoming>,
) -> Result<Response<ReplayBody>, Infallible> {
    let guard = OpenGuard::enter(&shared);
    let upgrade = is_upgrade(request.headers());
    let on_upgrade = upgrade.then(|| hyper::upgrade::on(&mut request));
    let index = shared.claim();
    let (parts, body) = request.into_parts();
    let body = read_body(body).await;
    let Some(exchange) = shared.exchanges.get(index) else {
        let got = show(&redact(
            format!("{} {}", parts.method, path_of(&parts.uri)).as_bytes(),
        ));
        return Ok(shared.refuse(index, &Miss::new("exchange", "end of fixture", got), guard));
    };
    let kind = if upgrade { Kind::Websocket } else { Kind::Http };
    if let Err(miss) = check(exchange, kind, &parts, &body) {
        return Ok(shared.refuse(index, &miss, guard));
    }
    Ok(match (&exchange.reply, on_upgrade) {
        (Reply::Http(reply), _) => reply.response(guard),
        (Reply::Frames(_), Some(on_upgrade)) => match accept_key(&parts.headers) {
            Ok(accept) => {
                *lock(&slot) = Some(PendingSocket {
                    on_upgrade,
                    index,
                    guard,
                });
                switching_protocols(&accept)
            }
            Err(miss) => shared.refuse(index, &miss, guard),
        },
        (Reply::Frames(_), None) => {
            shared.refuse(index, &Miss::new("kind", "websocket", "http"), guard)
        }
    })
}

/// Why a request body could not be compared.
#[derive(Debug)]
enum BodyFault {
    TooLarge,
    Unreadable(hyper::Error),
}

impl fmt::Display for BodyFault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => write!(formatter, "more than {MAX_BODY} bytes"),
            Self::Unreadable(error) => write!(formatter, "an unreadable body ({error})"),
        }
    }
}

async fn read_body(mut body: Incoming) -> Result<Bytes, BodyFault> {
    let mut out = Vec::new();
    while let Some(frame) = poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await {
        let frame = frame.map_err(BodyFault::Unreadable)?;
        if let Ok(data) = frame.into_data() {
            if data.len() > MAX_BODY - out.len() {
                return Err(BodyFault::TooLarge);
            }
            out.extend_from_slice(&data);
        }
    }
    Ok(Bytes::from(out))
}

fn is_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get_all(UPGRADE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|token| token.trim().eq_ignore_ascii_case("websocket"))
}

fn path_of(uri: &Uri) -> &str {
    uri.path_and_query()
        .map_or_else(|| uri.path(), hyper::http::uri::PathAndQuery::as_str)
}

fn check(
    exchange: &Exchange,
    kind: Kind,
    parts: &Parts,
    body: &Result<Bytes, BodyFault>,
) -> Result<(), Miss> {
    // `kind` is what the request is: an upgrade is a websocket handshake.
    if kind != exchange.kind {
        return Err(Miss::new(
            "kind",
            exchange.kind.to_string(),
            kind.to_string(),
        ));
    }
    let want = &exchange.request;
    if parts.method != want.method {
        return Err(Miss::new(
            "method",
            quote(want.method.as_str()),
            quote(parts.method.as_str()),
        ));
    }
    let path = path_of(&parts.uri);
    if path != &*want.path {
        return Err(Miss::new(
            "path",
            show(&redact(want.path.as_bytes())),
            show(&redact(path.as_bytes())),
        ));
    }
    for (name, value) in &*want.headers {
        check_header(&parts.headers, name, value)?;
    }
    let Some(expected) = &want.body else {
        return Ok(());
    };
    match body {
        Ok(got) if got == expected => Ok(()),
        Ok(got) => {
            let (expected, got) = body_diff(expected, got);
            Err(Miss::new("body", expected, got))
        }
        Err(fault) => Err(Miss::new(
            "body",
            format!("{} bytes", expected.len()),
            fault.to_string(),
        )),
    }
}

fn check_header(headers: &HeaderMap, name: &HeaderName, want: &str) -> Result<(), Miss> {
    let field = format!("header {name}");
    let expected = if want == REDACTED {
        String::from(REDACTED)
    } else {
        quote(want)
    };
    let values: Vec<&HeaderValue> = headers.get_all(name).iter().collect();
    if values.is_empty() {
        return Err(Miss::new(field, expected, "<missing>"));
    }
    if want == REDACTED
        || values
            .iter()
            .any(|value| value.as_bytes() == want.as_bytes())
    {
        return Ok(());
    }
    let got = if SECRET_HEADERS.contains(&name.as_str()) {
        String::from("<other value>")
    } else {
        values
            .iter()
            .map(|value| show(value.as_bytes()))
            .collect::<Vec<_>>()
            .join(", ")
    };
    Err(Miss::new(field, expected, got))
}

fn accept_key(headers: &HeaderMap) -> Result<String, Miss> {
    let version = headers.get(SEC_WEBSOCKET_VERSION);
    if version.map(HeaderValue::as_bytes) != Some(&b"13"[..]) {
        let got = version.map_or_else(|| String::from("<missing>"), |value| show(value.as_bytes()));
        return Err(Miss::new("header sec-websocket-version", quote("13"), got));
    }
    let key = headers
        .get(SEC_WEBSOCKET_KEY)
        .ok_or_else(|| Miss::new("header sec-websocket-key", "<present>", "<missing>"))?;
    Ok(derive_accept_key(key.as_bytes()))
}

fn switching_protocols(accept: &str) -> Response<ReplayBody> {
    let mut response = Response::new(ReplayBody::full(Bytes::new(), None));
    *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    let headers = response.headers_mut();
    headers.insert(UPGRADE, HeaderValue::from_static("websocket"));
    headers.insert(CONNECTION, HeaderValue::from_static("Upgrade"));
    if let Ok(accept) = HeaderValue::from_str(accept) {
        headers.insert(SEC_WEBSOCKET_ACCEPT, accept);
    }
    response
}

impl HttpReply {
    fn response(&self, guard: OpenGuard) -> Response<ReplayBody> {
        let mut response = Response::new(ReplayBody {
            data: self.body.clone(),
            cuts: Arc::clone(&self.cuts),
            next: 0,
            pos: 0,
            end: self.end,
            delay: self.delay,
            sleep: None,
            flushing: false,
            open: Some(guard),
        });
        *response.status_mut() = self.status;
        let headers = response.headers_mut();
        for (name, value) in &*self.headers {
            headers.append(name.clone(), value.clone());
        }
        response
    }
}

/// Response body that writes each fixture piece as its own flushed write,
/// then finishes, stalls with the socket open, or aborts the connection.
#[derive(Debug)]
struct ReplayBody {
    data: Bytes,
    cuts: Arc<[usize]>,
    next: usize,
    pos: usize,
    end: End,
    delay: Duration,
    /// Pending pause before the next piece.
    sleep: Option<Pin<Box<Sleep>>>,
    /// Set after a piece so the next poll yields once and `hyper` flushes.
    flushing: bool,
    open: Option<OpenGuard>,
}

impl ReplayBody {
    fn full(data: Bytes, open: Option<OpenGuard>) -> Self {
        let cuts: Arc<[usize]> = if data.is_empty() {
            Arc::from([])
        } else {
            Arc::from([data.len()])
        };
        Self {
            data,
            cuts,
            next: 0,
            pos: 0,
            end: End::Finish,
            delay: Duration::ZERO,
            sleep: None,
            flushing: false,
            open,
        }
    }
}

/// The fixture asked the server to close the connection mid-body.
#[derive(Debug)]
struct ConnectionCut;

impl fmt::Display for ConnectionCut {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("replay fixture closed the connection")
    }
}

impl Error for ConnectionCut {}

impl Body for ReplayBody {
    type Data = Bytes;
    type Error = ConnectionCut;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, ConnectionCut>>> {
        let this = &mut *self;
        if let Some(sleep) = this.sleep.as_mut() {
            ready!(sleep.as_mut().poll(cx));
            this.sleep = None;
        } else if this.flushing {
            this.flushing = false;
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        if let Some(&cut) = this.cuts.get(this.next) {
            let piece = this.data.slice(this.pos..cut);
            this.pos = cut;
            this.next += 1;
            if this.delay.is_zero() || this.next == this.cuts.len() {
                this.flushing = true;
            } else {
                this.sleep = Some(Box::pin(tokio::time::sleep(this.delay)));
            }
            return Poll::Ready(Some(Ok(Frame::data(piece))));
        }
        match this.end {
            End::Finish => {
                this.open = None;
                Poll::Ready(None)
            }
            // No waker is kept: only the peer closing or shutdown ends it.
            End::Stall => Poll::Pending,
            End::Close => {
                this.open = None;
                Poll::Ready(Some(Err(ConnectionCut)))
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        matches!(self.end, End::Finish) && self.next == self.cuts.len()
    }

    fn size_hint(&self) -> SizeHint {
        match self.end {
            End::Finish => {
                SizeHint::with_exact(u64::try_from(self.data.len() - self.pos).unwrap_or(u64::MAX))
            }
            End::Stall | End::Close => SizeHint::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// WebSocket sessions.

async fn websocket(shared: &Shared, pending: PendingSocket) {
    let PendingSocket {
        on_upgrade,
        index,
        guard: _guard,
    } = pending;
    let Some(Reply::Frames(steps)) = shared.exchanges.get(index).map(|exchange| &exchange.reply)
    else {
        return;
    };
    // A client that leaves before the upgrade completes cancelled the turn.
    let Ok(upgraded) = on_upgrade.await else {
        return;
    };
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE))
        .max_frame_size(Some(MAX_MESSAGE));
    let mut socket =
        WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Server, Some(config)).await;
    play(shared, index, steps, &mut socket).await;
}

/// Plays the frame script. A client that closes, errors, or disconnects
/// ends the session quietly because cancellation is legitimate client
/// behavior; only a wrong client data frame is a mismatch.
async fn play(shared: &Shared, index: usize, steps: &[WsStep], socket: &mut Socket) {
    for (number, step) in steps.iter().enumerate() {
        match step {
            WsStep::Receive(expected) => match next_message(socket).await {
                Some(Ok(Message::Text(got))) if got.as_str() == expected.as_str() => {}
                Some(Err(
                    error @ (WsError::Capacity(_) | WsError::Protocol(_) | WsError::Utf8(_)),
                )) if !matches!(
                    error,
                    WsError::Protocol(ProtocolError::ResetWithoutClosingHandshake)
                ) =>
                {
                    let got = describe(Some(&Err(error)));
                    shared.record(
                        index,
                        &Miss::new(format!("frame {number}"), step.describe(), got),
                    );
                    return;
                }
                None | Some(Err(_) | Ok(Message::Close(_))) => return,
                Some(Ok(other)) => {
                    let got = describe(Some(&Ok(other)));
                    let field = format!("frame {number}");
                    shared.record(index, &Miss::new(field, step.describe(), got));
                    refuse_socket(socket).await;
                    return;
                }
            },
            WsStep::Send(text) => {
                if socket.send(Message::Text(text.clone())).await.is_err() {
                    return;
                }
            }
            WsStep::Close { code, reason } => {
                let frame = CloseFrame {
                    code: CloseCode::from(*code),
                    reason: reason.clone(),
                };
                if socket.close(Some(frame)).await.is_ok() {
                    drain(socket).await;
                }
                return;
            }
            WsStep::Stall => {
                await_client_end(shared, index, number, socket).await;
                return;
            }
        }
    }
    await_client_end(shared, index, steps.len(), socket).await;
}

/// Waits for the client to close; any data frame is a mismatch.
async fn await_client_end(shared: &Shared, index: usize, number: usize, socket: &mut Socket) {
    match next_message(socket).await {
        None | Some(Err(_) | Ok(Message::Close(_))) => drain(socket).await,
        Some(Ok(other)) => {
            let got = describe(Some(&Ok(other)));
            shared.record(
                index,
                &Miss::new(format!("frame {number}"), "no client frame", got),
            );
            refuse_socket(socket).await;
        }
    }
}

async fn next_message(socket: &mut Socket) -> Option<Result<Message, WsError>> {
    loop {
        match socket.next().await {
            Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => {}
            other => return other,
        }
    }
}

/// Reads until the stream ends so a queued close reply reaches the client.
async fn drain(socket: &mut Socket) {
    while let Some(Ok(_)) = socket.next().await {}
}

async fn refuse_socket(socket: &mut Socket) {
    let frame = CloseFrame {
        code: CloseCode::Policy,
        reason: Utf8Bytes::from_static("replay mismatch"),
    };
    if socket.close(Some(frame)).await.is_ok() {
        drain(socket).await;
    }
}

fn describe(message: Option<&Result<Message, WsError>>) -> String {
    match message {
        None => String::from("connection closed"),
        Some(Err(error)) => format!("a read error ({error})"),
        Some(Ok(Message::Text(text))) => format!("text {}", show(&redact(text.as_bytes()))),
        Some(Ok(Message::Binary(data))) => format!("binary {} bytes", data.len()),
        Some(Ok(Message::Close(Some(frame)))) => format!("client close {}", u16::from(frame.code)),
        Some(Ok(Message::Close(None))) => String::from("client close"),
        Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => {
            String::from("a control frame")
        }
    }
}

// ---------------------------------------------------------------------------
// Mismatch text.

fn quote(text: &str) -> String {
    show(text.as_bytes())
}

/// Quoted, escaped excerpt of at most [`EXCERPT`] bytes.
fn show(bytes: &[u8]) -> String {
    let cut = bytes.len().min(EXCERPT);
    let text = String::from_utf8_lossy(&bytes[..cut]);
    if cut < bytes.len() {
        format!("{text:?}…")
    } else {
        format!("{text:?}")
    }
}

/// Describes both bodies from their redacted forms around the first
/// difference.
fn body_diff(expected: &[u8], got: &[u8]) -> (String, String) {
    let (expected_text, got_text) = (redact(expected), redact(got));
    let Some(at) = first_difference(&expected_text, &got_text) else {
        return (
            format!("{} bytes", expected.len()),
            format!("{} bytes differing only inside redacted values", got.len()),
        );
    };
    let from = at.saturating_sub(CONTEXT);
    let lead = if from > 0 { "…" } else { "" };
    (
        format!(
            "{} bytes {lead}{}",
            expected.len(),
            show(&expected_text[from..])
        ),
        format!("{} bytes {lead}{}", got.len(), show(&got_text[from..])),
    )
}

fn first_difference(left: &[u8], right: &[u8]) -> Option<usize> {
    left.iter()
        .zip(right)
        .position(|(left, right)| left != right)
        .or_else(|| (left.len() != right.len()).then(|| left.len().min(right.len())))
}

/// Replaces the values of [`SECRET_MEMBERS`] as JSON string members or
/// form fields with [`REDACTED`].
fn redact(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let span = SECRET_MEMBERS
            .iter()
            .find_map(|name| secret_span(bytes, at, name.as_bytes()));
        if let Some((start, end)) = span {
            out.extend_from_slice(&bytes[at..start]);
            out.extend_from_slice(REDACTED.as_bytes());
            at = end;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    out
}

/// Returns the value span of a secret member starting at `at`.
fn secret_span(bytes: &[u8], at: usize, name: &[u8]) -> Option<(usize, usize)> {
    let rest = &bytes[at..];
    if rest.first() == Some(&b'"')
        && rest.get(1..=name.len()) == Some(name)
        && rest.get(name.len() + 1) == Some(&b'"')
    {
        let colon = skip_space(bytes, at + name.len() + 2);
        if bytes.get(colon) != Some(&b':') {
            return None;
        }
        let quote = skip_space(bytes, colon + 1);
        if bytes.get(quote) != Some(&b'"') {
            return None;
        }
        let start = quote + 1;
        return string_end(bytes, start).map(|end| (start, end));
    }
    let field_start = at == 0 || matches!(bytes.get(at - 1), Some(b'&' | b'?'));
    if field_start && rest.starts_with(name) && rest.get(name.len()) == Some(&b'=') {
        let start = at + name.len() + 1;
        let end = bytes[start..]
            .iter()
            .position(|byte| *byte == b'&')
            .map_or(bytes.len(), |offset| start + offset);
        return Some((start, end));
    }
    None
}

fn skip_space(bytes: &[u8], mut at: usize) -> usize {
    while matches!(bytes.get(at), Some(b' ' | b'\t' | b'\n' | b'\r')) {
        at += 1;
    }
    at
}

/// Index of the closing quote of a JSON string whose content starts at
/// `start`.
fn string_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut at = start;
    while let Some(&byte) = bytes.get(at) {
        match byte {
            b'\\' => at += 2,
            b'"' => return Some(at),
            _ => at += 1,
        }
    }
    None
}
