//! Reusable Responses-family WebSocket sessions and the typed Responses event
//! stream shared by Codex and `OpenAI` Responses providers.
//!
//! The session map holds only idle sockets. A turn moves its socket into the
//! stream source; cancellation sends a bounded close frame, while any error
//! drops the socket. Only a clean terminal response returns it to the idle slot.

use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use dal_core::{Family, SessionId};
use futures::{SinkExt, StreamExt, stream};
use reqwest::Url;
use sonic_rs::JsonValueTrait;
use tokio::{
    net::TcpStream,
    sync::{Mutex as AsyncMutex, OwnedMutexGuard},
    time::{sleep, timeout},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_with_config,
    tungstenite::{
        Message,
        client::IntoClientRequest,
        error::Error as WsError,
        http::{HeaderValue, StatusCode},
        protocol::{CloseFrame, WebSocketConfig},
    },
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    error::{LimitError, ProviderError},
    family::{codex, responses},
    http::{self, STREAM_IDLE_TIMEOUT, WS_MESSAGE_LIMIT},
    retry::{self, RequestState, RetryDecision},
    stream::{EventStream, NoticeSink, StreamEvent},
};

/// The exact fallback notice prefix. The final error text follows one space.
const FALLBACK_NOTICE: &str = "Falling back from WebSockets to HTTPS transport.";
/// Idle sockets live for less than five minutes and no more than 55 minutes.
#[expect(
    clippy::duration_suboptimal_units,
    reason = "Duration::from_mins is not a stable const fn (rust#140881)"
)]
const WS_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// A socket never outlives one login flow; a stale link fails closed.
#[expect(
    clippy::duration_suboptimal_units,
    reason = "Duration::from_mins is not a stable const fn (rust#140881)"
)]
const WS_MAX_AGE: Duration = Duration::from_secs(55 * 60);
const BETA_HEADER: &str = "responses_websockets=2026-02-06";

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;
type SleepFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
type Sleeper = Arc<dyn Fn(Duration) -> SleepFuture + Send + Sync>;

/// A family-specific request accepted by the shared WebSocket state machine.
///
/// The enum keeps credential and header provenance explicit: Codex has its
/// identity headers, while Responses has only its configured authentication.
#[derive(Clone, Copy)]
pub(crate) enum WsWire<'a> {
    /// The Codex request with Codex-only identity headers.
    Codex(&'a codex::CodexWire),
    /// An `OpenAI` Responses request using provider-configured authentication.
    Responses(&'a responses::ResponsesWire),
}

impl WsWire<'_> {
    fn family(&self) -> Family {
        match self {
            Self::Codex(_) => Family::Codex,
            Self::Responses(_) => Family::Responses,
        }
    }

    fn model(&self) -> &str {
        match self {
            Self::Codex(wire) => &wire.model,
            Self::Responses(wire) => &wire.model,
        }
    }

    fn session_id(&self) -> SessionId {
        match self {
            Self::Codex(wire) => wire.session_id,
            Self::Responses(wire) => wire.session_id,
        }
    }

    fn body(&self) -> &[u8] {
        match self {
            Self::Codex(wire) => &wire.body,
            Self::Responses(wire) => &wire.body,
        }
    }

    fn secret(&self) -> &str {
        match self {
            Self::Codex(wire) => codex::access_token(wire),
            Self::Responses(wire) => &wire.secret,
        }
    }

    fn oauth(&self) -> bool {
        match self {
            Self::Codex(_) => true,
            Self::Responses(wire) => wire.oauth,
        }
    }

    fn user_agent(&self) -> &str {
        match self {
            Self::Codex(wire) => &wire.user_agent,
            Self::Responses(wire) => &wire.user_agent,
        }
    }
}

/// All inputs for one Codex or Responses WebSocket turn.
///
/// `session_id` is checked against the family wire's explicit session identity
/// before any connection is opened; it is independent of the prompt cache key.
pub(crate) struct WsRequest<'a> {
    /// Provider id used to isolate connection and fallback state.
    pub(crate) provider: &'a str,
    /// Runtime session id expected by the family wire.
    pub(crate) session_id: SessionId,
    /// Configured HTTPS base URL; the Responses endpoint is derived safely.
    pub(crate) base_url: &'a str,
    /// Typed family request including its credential provenance.
    pub(crate) wire: WsWire<'a>,
    /// Retry count after the initial WebSocket attempt.
    pub(crate) stream_max_retries: u32,
    /// Whether the one OAuth refresh has already run for this request.
    pub(crate) refreshed: bool,
    /// Receives retry and one-time HTTPS fallback notices.
    pub(crate) notices: &'a NoticeSink,
    /// Cancels connection, retry, and stream operations.
    pub(crate) cancel: &'a CancellationToken,
}

/// The WebSocket result for one Responses-family turn.
pub(crate) enum WsTurn {
    /// The caller consumes this stream as the turn's provider response.
    Stream(EventStream),
    /// The caller must issue this turn over HTTPS; the stream is not replayed.
    HttpsFallback,
}

/// Live sessions keyed by session id and provider id.
type SessionTable = HashMap<(SessionId, Box<str>), Arc<Session>>;

/// WebSocket sessions, owned by the provider set and scoped by session and
/// provider id. No background task is used; idle expiry is checked at turn
/// start.
pub(crate) struct WsSessions {
    sessions: Mutex<SessionTable>,
    clock: Clock,
    sleeper: Sleeper,
}

struct Session {
    turn: Arc<AsyncMutex<()>>,
    state: Mutex<SessionState>,
}

#[derive(Default)]
struct SessionState {
    idle: Option<IdleConnection>,
    fallback_model: Option<Box<str>>,
}

struct IdleConnection {
    socket: Socket,
    model: Box<str>,
    since: Instant,
    born: Instant,
}

struct Source {
    socket: Option<Socket>,
    decoder: responses::Decoder,
    ready: VecDeque<Result<StreamEvent, ProviderError>>,
    pending: Option<Message>,
    session: Arc<Session>,
    family: Family,
    model: Box<str>,
    born: Instant,
    clock: Clock,
    cancel: CancellationToken,
    finished: bool,
    turn: Option<OwnedMutexGuard<()>>,
    secret: Box<str>,
}

struct HandshakeFailure {
    status: Option<u16>,
    code: Option<String>,
    message: String,
    error: ProviderError,
    mapped: Option<ProviderError>,
}

#[derive(Clone, Copy)]
struct RetryAttempt<'a> {
    attempt: u32,
    max_attempts: u32,
    max_retries: u32,
    request: &'a RequestState<'a>,
    notices: &'a NoticeSink,
    cancel: &'a CancellationToken,
}

struct TurnDrive<'a> {
    session: Arc<Session>,
    url: Url,
    frame: String,
    wire: WsWire<'a>,
    state: RequestState<'a>,
    max_attempts: u32,
    max_retries: u32,
    notices: &'a NoticeSink,
    cancel: &'a CancellationToken,
    turn: Option<OwnedMutexGuard<()>>,
}

impl WsSessions {
    /// Creates an empty session pool using monotonic time and Tokio's timer.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            clock: Arc::new(Instant::now),
            sleeper: Arc::new(|duration| Box::pin(sleep(duration))),
        }
    }

    #[cfg(test)]
    fn with_clock_and_sleeper(clock: Clock, sleeper: Sleeper) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            clock,
            sleeper,
        }
    }

    /// Opens a WebSocket turn or selects HTTPS after the configured WS retry
    /// budget is spent.
    ///
    /// `stream_max_retries` counts retries after the first attempt, so its
    /// default value of five produces six failed handshakes before fallback.
    /// The fallback flag is retained for this session/provider/model until the
    /// model changes. `notices` receives the fallback notice once on the
    /// transition, never on later HTTPS-only turns.
    pub(crate) async fn open(&self, request: WsRequest<'_>) -> Result<WsTurn, ProviderError> {
        let WsRequest {
            provider,
            session_id,
            base_url,
            wire,
            stream_max_retries,
            refreshed,
            notices,
            cancel,
        } = request;
        let family = wire.family();
        let model = wire.model();
        if session_id != wire.session_id() {
            return Err(ProviderError::InvalidRequest {
                message: format!("{family:?} WebSocket session does not match its request"),
            });
        }
        let session = self.session(provider, session_id);
        let turn = tokio::select! {
            () = cancel.cancelled() => return Err(cancelled(family)),
            guard = Arc::clone(&session.turn).lock_owned() => guard,
        };
        if Self::fallback_active(&session, model) {
            return Ok(WsTurn::HttpsFallback);
        }
        let url = websocket_url(base_url, family)?;
        let frame = Self::text_frame(wire, family)?;
        let reusable = self.take_reusable(&session, model);
        let drive = TurnDrive {
            session,
            url,
            frame,
            wire,
            state: RequestState {
                family,
                provider,
                model,
                oauth: wire.oauth(),
                refreshed,
                delivered: false,
            },
            max_attempts: stream_max_retries.saturating_add(1),
            max_retries: stream_max_retries,
            notices,
            cancel,
            turn: Some(turn),
        };
        Box::pin(self.drive(drive, reusable)).await
    }

    fn text_frame(wire: WsWire<'_>, family: Family) -> Result<String, ProviderError> {
        let frame = codex::websocket_frame(wire.body())?;
        if frame.len() > WS_MESSAGE_LIMIT {
            return Err(ProviderError::Limit(LimitError::WsMessage));
        }
        String::from_utf8(frame).map_err(|_| ProviderError::Protocol {
            family,
            detail: format!("{family:?} generated a non-UTF-8 WebSocket frame"),
        })
    }

    async fn drive(
        &self,
        mut drive: TurnDrive<'_>,
        mut idle: Option<IdleConnection>,
    ) -> Result<WsTurn, ProviderError> {
        let mut last_failure = None;
        for attempt in 1..=drive.max_attempts {
            let retry = RetryAttempt {
                attempt,
                max_attempts: drive.max_attempts,
                max_retries: drive.max_retries,
                request: &drive.state,
                notices: drive.notices,
                cancel: drive.cancel,
            };
            let (mut socket, born) = match idle.take() {
                Some(connection) => (connection.socket, connection.born),
                None => match connect(&drive.url, drive.wire, self.now(), drive.cancel).await {
                    Ok(connection) => connection,
                    Err(failure) if failure.status == Some(401) => {
                        lock(&drive.session.state).idle = None;
                        return Ok(WsTurn::HttpsFallback);
                    }
                    Err(failure) => {
                        if let Some(error) = self.retry_failure(failure, retry).await? {
                            last_failure = Some(error);
                            break;
                        }
                        continue;
                    }
                },
            };
            let send_result = tokio::select! {
                () = drive.cancel.cancelled() => {
                    close_socket(&mut socket).await;
                    return Err(cancelled(drive.state.family));
                }
                result = socket.send(Message::Text(drive.frame.clone().into())) => result,
            };
            if send_result.is_err() {
                let failure =
                    transport_failure(drive.state.family, "WebSocket request frame send failed");
                if let Some(error) = self.retry_failure(failure, retry).await? {
                    last_failure = Some(error);
                    break;
                }
                continue;
            }
            let pending = match first_frame(&mut socket, drive.wire, drive.cancel).await {
                Ok(frame) => frame,
                Err(failure) if failure.status == Some(401) => {
                    close_socket(&mut socket).await;
                    lock(&drive.session.state).idle = None;
                    return Ok(WsTurn::HttpsFallback);
                }
                Err(failure) => {
                    if let Some(error) = self.retry_failure(failure, retry).await? {
                        last_failure = Some(error);
                        break;
                    }
                    continue;
                }
            };
            return Ok(self.emit_stream(&mut drive, socket, pending, born));
        }
        Ok(Self::latch_fallback(&drive, last_failure))
    }

    fn emit_stream(
        &self,
        drive: &mut TurnDrive<'_>,
        socket: Socket,
        pending: Message,
        born: Instant,
    ) -> WsTurn {
        let source = Source {
            socket: Some(socket),
            decoder: responses::Decoder::new(drive.state.family, drive.state.model.to_owned()),
            ready: VecDeque::new(),
            pending: Some(pending),
            session: Arc::clone(&drive.session),
            family: drive.state.family,
            model: drive.state.model.into(),
            born,
            clock: Arc::clone(&self.clock),
            cancel: drive.cancel.clone(),
            finished: false,
            secret: drive.wire.secret().into(),
            turn: drive.turn.take(),
        };
        let session_on_cancel = Arc::clone(&drive.session);
        let events = stream::unfold(source, |mut source| async move {
            source.next().await.map(|item| (item, source))
        });
        WsTurn::Stream(EventStream::new(events, move || {
            lock(&session_on_cancel.state).idle = None;
        }))
    }

    fn latch_fallback(drive: &TurnDrive<'_>, last_failure: Option<ProviderError>) -> WsTurn {
        let mut state = lock(&drive.session.state);
        state.idle = None;
        state.fallback_model = Some(drive.state.model.into());
        drop(state);
        let last_failure = last_failure.unwrap_or_else(|| ProviderError::Transport {
            family: drive.state.family,
            reason: String::from("WebSocket handshake failed"),
        });
        let failure_text = last_failure.to_string();
        let error = redact(&failure_text, drive.wire.secret()).into_owned();
        (drive.notices)(format!("{FALLBACK_NOTICE} {error}"));
        WsTurn::HttpsFallback
    }

    /// Drops every idle connection and policy entry belonging to `session`.
    pub(crate) fn end_session(&self, session: &SessionId) {
        lock(&self.sessions).retain(|(id, _), _| id != session);
    }

    fn session(&self, provider: &str, session: SessionId) -> Arc<Session> {
        let mut sessions = lock(&self.sessions);
        Arc::clone(
            sessions
                .entry((session, provider.into()))
                .or_insert_with(|| {
                    Arc::new(Session {
                        turn: Arc::new(AsyncMutex::new(())),
                        state: Mutex::new(SessionState::default()),
                    })
                }),
        )
    }

    fn fallback_active(session: &Session, model: &str) -> bool {
        let mut state = lock(&session.state);
        if state.fallback_model.as_deref() == Some(model) {
            return true;
        }
        if state.fallback_model.is_some() {
            state.fallback_model = None;
            state.idle = None;
        }
        false
    }

    fn take_reusable(&self, session: &Session, model: &str) -> Option<IdleConnection> {
        let now = self.now();
        let mut state = lock(&session.state);
        let connection = state.idle.take()?;
        let idle = now
            .checked_duration_since(connection.since)
            .unwrap_or_default();
        let age = now
            .checked_duration_since(connection.born)
            .unwrap_or_default();
        if connection.model.as_ref() == model && idle < WS_IDLE_TIMEOUT && age < WS_MAX_AGE {
            Some(connection)
        } else {
            None
        }
    }

    fn now(&self) -> Instant {
        (self.clock)()
    }

    async fn retry_delay(
        &self,
        family: Family,
        attempt: u32,
        max_retries: u32,
        notices: &NoticeSink,
        cancel: &CancellationToken,
    ) -> Result<(), ProviderError> {
        let delay = retry::delay_for_attempt(attempt, None, retry_jitter())
            .map_err(|too_long| too_long.into_error(String::from("WebSocket retry")))?;
        notices(format!(
            "Retrying in {:.1}s (attempt {attempt}/{max_retries}).",
            delay.as_secs_f64()
        ));
        tokio::select! {
            () = cancel.cancelled() => Err(cancelled(family)),
            () = (self.sleeper)(delay) => Ok(()),
        }
    }
    async fn retry_failure(
        &self,
        failure: Box<HandshakeFailure>,
        attempt: RetryAttempt<'_>,
    ) -> Result<Option<ProviderError>, ProviderError> {
        let RetryAttempt {
            attempt,
            max_attempts,
            max_retries,
            request,
            notices,
            cancel,
        } = attempt;
        if let Some(error) = failure.mapped {
            return Err(error);
        }
        match retry::classify(
            failure.status,
            failure.code.as_deref(),
            &failure.message,
            request,
        ) {
            RetryDecision::Retry(error) if attempt == max_attempts => Ok(Some(error)),
            RetryDecision::Retry(_) => {
                self.retry_delay(request.family, attempt, max_retries, notices, cancel)
                    .await?;
                Ok(None)
            }
            RetryDecision::Fail(error) => Err(error),
            RetryDecision::RefreshOnce => Err(failure.error),
        }
    }
}

impl Default for WsSessions {
    fn default() -> Self {
        Self::new()
    }
}

fn retry_jitter() -> f64 {
    let bytes = Uuid::new_v4().into_bytes();
    let sample = u32::from_be_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
    jitter_for_sample(sample)
}

fn jitter_for_sample(sample: u32) -> f64 {
    0.9 + 0.2 * (f64::from(sample) / f64::from(u32::MAX))
}

impl Source {
    async fn next(&mut self) -> Option<Result<StreamEvent, ProviderError>> {
        if self.finished {
            return None;
        }
        loop {
            if let Some(event) = self.ready.pop_front() {
                if matches!(&event, Ok(StreamEvent::Stop { .. })) {
                    self.finished = true;
                    self.turn = None;
                }
                return Some(event);
            }
            let next = if let Some(pending) = self.pending.take() {
                Some(Ok(pending))
            } else {
                let Some(socket) = self.socket.as_mut() else {
                    return Some(Err(ProviderError::StreamCut));
                };
                tokio::select! {
                    () = self.cancel.cancelled() => {
                        close_socket(socket).await;
                        return Some(Err(cancelled(self.family)));
                    }
                    next = timeout(STREAM_IDLE_TIMEOUT, socket.next()) => match next {
                        Ok(next) => next,
                        Err(_) => return Some(Err(ProviderError::StreamCut)),
                    },
                }
            };
            match next {
                Some(Ok(Message::Text(text))) => {
                    if text.len() > WS_MESSAGE_LIMIT {
                        return Some(Err(ProviderError::Limit(LimitError::WsMessage)));
                    }
                    if let Some(error) =
                        websocket_error(text.as_str(), self.family, &self.model, &self.secret)
                    {
                        return Some(Err(error));
                    }
                    let mut batch = Vec::new();
                    let terminal = match self.decoder.feed(text.as_str(), &mut batch) {
                        Ok(terminal) => terminal,
                        Err(mut error) => {
                            redact_provider_error(&mut error, &self.secret);
                            return Some(Err(error));
                        }
                    };
                    self.ready.extend(batch.into_iter().map(Ok));
                    if terminal {
                        self.return_idle();
                    }
                }
                Some(Ok(Message::Binary(bytes))) => {
                    if bytes.len() > WS_MESSAGE_LIMIT {
                        return Some(Err(ProviderError::Limit(LimitError::WsMessage)));
                    }
                    return Some(Err(ProviderError::Protocol {
                        family: self.family,
                        detail: String::from("binary WebSocket frame"),
                    }));
                }
                Some(Ok(Message::Close(frame))) => {
                    return Some(Err(close_error(frame, &self.secret)));
                }
                Some(Ok(_)) => {}
                Some(Err(WsError::Capacity(_))) => {
                    return Some(Err(ProviderError::Limit(LimitError::WsMessage)));
                }
                Some(Err(_)) | None => {
                    return Some(Err(ProviderError::WsClosed { code: None }));
                }
            }
        }
    }

    fn return_idle(&mut self) {
        let Some(socket) = self.socket.take() else {
            return;
        };
        lock(&self.session.state).idle = Some(IdleConnection {
            socket,
            model: self.model.clone(),
            since: (self.clock)(),
            born: self.born,
        });
    }
}

fn websocket_url(base_url: &str, family: Family) -> Result<Url, ProviderError> {
    let mut url = http::endpoint(family, base_url, responses::PATH)?;
    let scheme = match url.scheme() {
        "https" => "wss",
        "http" => "ws",
        _ => {
            return Err(ProviderError::Transport {
                family,
                reason: String::from("WebSocket URL must use https or loopback http"),
            });
        }
    };
    url.set_scheme(scheme)
        .map_err(|()| ProviderError::Transport {
            family,
            reason: format!("could not derive the {family:?} WebSocket URL"),
        })?;
    Ok(url)
}

async fn connect(
    url: &Url,
    wire: WsWire<'_>,
    born: Instant,
    cancel: &CancellationToken,
) -> Result<(Socket, Instant), Box<HandshakeFailure>> {
    let family = wire.family();
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|_| transport_failure(family, "invalid WebSocket request"))?;
    match wire {
        WsWire::Codex(wire) => {
            for (name, value) in &wire.headers {
                let value = HeaderValue::from_str(value)
                    .map_err(|_| transport_failure(family, "invalid WebSocket header"))?;
                request.headers_mut().insert(*name, value);
            }
        }
        WsWire::Responses(wire) => {
            if let Some((name, value)) = &wire.auth_header {
                let value = HeaderValue::from_str(value)
                    .map_err(|_| transport_failure(family, "invalid configured auth header"))?;
                request.headers_mut().insert(*name, value);
            }
        }
    }
    let user_agent = HeaderValue::from_str(wire.user_agent())
        .map_err(|_| transport_failure(family, "invalid user-agent"))?;
    request.headers_mut().insert("user-agent", user_agent);
    request
        .headers_mut()
        .insert("openai-beta", HeaderValue::from_static(BETA_HEADER));
    request.headers_mut().remove("accept");
    let config = WebSocketConfig::default()
        .max_message_size(Some(WS_MESSAGE_LIMIT))
        .max_frame_size(Some(WS_MESSAGE_LIMIT));
    let connected = tokio::select! {
        () = cancel.cancelled() => return Err(cancel_failure(family)),
        connected = timeout(
            http::RESPONSE_HEADER_TIMEOUT,
            connect_async_with_config(request, Some(config), false),
        ) => connected,
    };
    let (socket, response) = match connected {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => return Err(handshake_failure(error, wire)),
        Err(_) => return Err(transport_failure(family, "WebSocket handshake timed out")),
    };
    if response.status() != StatusCode::SWITCHING_PROTOCOLS {
        return Err(status_failure(
            response.status().as_u16(),
            None,
            "WebSocket handshake was not upgraded",
            wire,
        ));
    }
    Ok((socket, born))
}

async fn first_frame(
    socket: &mut Socket,
    wire: WsWire<'_>,
    cancel: &CancellationToken,
) -> Result<Message, Box<HandshakeFailure>> {
    let family = wire.family();
    let secret = wire.secret();
    loop {
        let next = tokio::select! {
            () = cancel.cancelled() => {
                close_socket(socket).await;
                return Err(cancel_failure(family));
            }
            next = timeout(STREAM_IDLE_TIMEOUT, socket.next()) => match next {
                Ok(next) => next,
                Err(_) => return Err(transport_failure(family, "WebSocket timed out before its first event")),
            },
        };
        match next {
            Some(Ok(Message::Text(text))) => {
                if text.len() > WS_MESSAGE_LIMIT {
                    return Err(mapped_failure(
                        ProviderError::Limit(LimitError::WsMessage),
                        "WebSocket message exceeds the configured limit",
                        family,
                    ));
                }
                validate_first_text(text.as_str(), wire)?;
                return Ok(Message::Text(text));
            }
            Some(Ok(Message::Binary(bytes))) => {
                let error = if bytes.len() > WS_MESSAGE_LIMIT {
                    ProviderError::Limit(LimitError::WsMessage)
                } else {
                    ProviderError::Protocol {
                        family,
                        detail: String::from("binary WebSocket frame"),
                    }
                };
                return Err(mapped_failure(
                    error,
                    "invalid first WebSocket event",
                    family,
                ));
            }
            Some(Ok(Message::Close(frame))) => {
                return Err(Box::new(HandshakeFailure {
                    status: None,
                    code: None,
                    message: String::from("WebSocket closed before its first event"),
                    error: close_error(frame, secret),
                    mapped: None,
                }));
            }
            Some(Ok(_)) => {}
            Some(Err(WsError::Capacity(_))) => {
                return Err(mapped_failure(
                    ProviderError::Limit(LimitError::WsMessage),
                    "WebSocket message exceeds the configured limit",
                    family,
                ));
            }
            Some(Err(_)) | None => {
                return Err(transport_failure(
                    family,
                    "WebSocket closed before its first event",
                ));
            }
        }
    }
}

fn validate_first_text(data: &str, wire: WsWire<'_>) -> Result<(), Box<HandshakeFailure>> {
    let family = wire.family();
    if is_connection_limit(data) {
        return Err(transport_failure(
            family,
            "websocket connection limit reached",
        ));
    }
    if is_error_frame(data) {
        return Err(status_failure(
            websocket_status(data).unwrap_or(200),
            Some(data.to_owned()),
            "WebSocket returned an error frame",
            wire,
        ));
    }
    Ok(())
}

fn handshake_failure(error: WsError, wire: WsWire<'_>) -> Box<HandshakeFailure> {
    let family = wire.family();
    if let WsError::Http(response) = error {
        let status = response.status().as_u16();
        let body = response.body().as_deref().unwrap_or_default();
        let text = String::from_utf8_lossy(body);
        return status_failure(
            status,
            Some(text.into_owned()),
            "WebSocket handshake failed",
            wire,
        );
    }
    transport_failure(family, "WebSocket handshake failed")
}

fn status_failure(
    status: u16,
    body: Option<String>,
    fallback: &str,
    wire: WsWire<'_>,
) -> Box<HandshakeFailure> {
    let family = wire.family();
    let model = wire.model();
    let body = body.map(|body| redact(&body, wire.secret()).into_owned());
    let parsed = body
        .as_deref()
        .and_then(|body| sonic_rs::from_str::<sonic_rs::Value>(body).ok());
    let message = parsed
        .as_ref()
        .and_then(|value| value.get("error"))
        .and_then(|error| error.get("message"))
        .and_then(JsonValueTrait::as_str)
        .or_else(|| {
            parsed
                .as_ref()
                .and_then(|value| value.get("detail"))
                .and_then(JsonValueTrait::as_str)
        })
        .unwrap_or(fallback)
        .to_owned();
    let code = parsed.as_ref().and_then(error_code).map(String::from);
    let mapped = if family == Family::Codex && matches!(status, 400 | 403 | 404) {
        body.as_deref()
            .and_then(|body| crate::usage::map_codex_error(status, body, model))
    } else {
        None
    };
    Box::new(HandshakeFailure {
        status: Some(status),
        code,
        message: message.clone(),
        error: ProviderError::Status {
            family,
            status,
            message,
        },
        mapped,
    })
}

fn mapped_failure(error: ProviderError, message: &str, family: Family) -> Box<HandshakeFailure> {
    Box::new(HandshakeFailure {
        status: None,
        code: None,
        message: String::from(message),
        error: ProviderError::Transport {
            family,
            reason: String::from(message),
        },
        mapped: Some(error),
    })
}

fn transport_failure(family: Family, message: &str) -> Box<HandshakeFailure> {
    Box::new(HandshakeFailure {
        status: None,
        code: None,
        message: String::from(message),
        error: ProviderError::Transport {
            family,
            reason: String::from(message),
        },
        mapped: None,
    })
}

fn cancel_failure(family: Family) -> Box<HandshakeFailure> {
    mapped_failure(cancelled(family), "request cancelled", family)
}

fn is_connection_limit(data: &str) -> bool {
    let Ok(value) = sonic_rs::from_str::<sonic_rs::Value>(data) else {
        return false;
    };
    let nested = value.get("error");
    let top_code = value.get("code").and_then(JsonValueTrait::as_str);
    let nested_type = nested
        .and_then(|error| error.get("type"))
        .and_then(JsonValueTrait::as_str);
    let nested_code = nested
        .and_then(|error| error.get("code"))
        .and_then(JsonValueTrait::as_str);
    [top_code, nested_type, nested_code].contains(&Some("websocket_connection_limit_reached"))
}

fn is_error_frame(data: &str) -> bool {
    let Ok(value) = sonic_rs::from_str::<sonic_rs::Value>(data) else {
        return false;
    };
    value
        .get("type")
        .is_some_and(|kind| kind.as_str() == Some("error"))
}

fn error_code(value: &sonic_rs::Value) -> Option<&str> {
    let nested = value.get("error");
    let error_type = nested
        .and_then(|error| error.get("type"))
        .and_then(JsonValueTrait::as_str);
    let error_code = nested
        .and_then(|error| error.get("code"))
        .and_then(JsonValueTrait::as_str);
    let top_code = value.get("code").and_then(JsonValueTrait::as_str);
    [error_type, error_code, top_code]
        .into_iter()
        .flatten()
        .find(|code| {
            matches!(
                *code,
                "websocket_connection_limit_reached"
                    | "usage_limit_reached"
                    | "usage_not_included"
                    | "rate_limit_error"
                    | "rate_limit_exceeded"
                    | "overloaded_error"
                    | "server_is_overloaded"
                    | "insufficient_quota"
                    | "credit_balance_exhausted"
                    | "organization_spend_limit_exceeded"
                    | "project_spend_limit_exceeded"
                    | "organization_usage_limit_exceeded"
                    | "context_length_exceeded"
                    | "context_window_exceeded"
            )
        })
        .or(error_type)
        .or(error_code)
        .or(top_code)
}

fn websocket_status(data: &str) -> Option<u16> {
    let value = sonic_rs::from_str::<sonic_rs::Value>(data).ok()?;
    value
        .get("status")
        .and_then(JsonValueTrait::as_u64)
        .and_then(|status| u16::try_from(status).ok())
}

fn websocket_error(data: &str, family: Family, model: &str, secret: &str) -> Option<ProviderError> {
    let value = sonic_rs::from_str::<sonic_rs::Value>(data).ok()?;
    if value.get("type").and_then(JsonValueTrait::as_str) != Some("error") {
        return None;
    }
    let status = value
        .get("status")
        .and_then(JsonValueTrait::as_u64)
        .and_then(|n| u16::try_from(n).ok());
    let message = value
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(JsonValueTrait::as_str)
        .or_else(|| value.get("message").and_then(JsonValueTrait::as_str))
        .or_else(|| value.get("detail").and_then(JsonValueTrait::as_str))
        .unwrap_or_default();
    let message = redact(message, secret).into_owned();
    let code = error_code(&value);
    let safe_data = redact(data, secret);
    if let Some(status) = status {
        if family == Family::Codex
            && let Some(error) = crate::usage::map_codex_error(status, safe_data.as_ref(), model)
        {
            return Some(error);
        }
        if status == 429 {
            match code {
                Some("usage_limit_reached") => {
                    return Some(ProviderError::UsageLimit {
                        model: model.to_owned(),
                        message,
                    });
                }
                Some("usage_not_included") => {
                    return Some(ProviderError::UsageNotIncluded { message });
                }
                _ => {}
            }
        }
    }
    Some(ProviderError::Status {
        family,
        status: status.unwrap_or(200),
        message,
    })
}

fn close_error(frame: Option<CloseFrame>, secret: &str) -> ProviderError {
    ProviderError::WsClosed {
        code: frame.map(|frame| {
            let reason = redact(frame.reason.as_str(), secret).into_owned();
            (u16::from(frame.code), reason)
        }),
    }
}

async fn close_socket(socket: &mut Socket) {
    let _ = timeout(
        Duration::from_millis(100),
        socket.send(Message::Close(None)),
    )
    .await;
}

fn cancelled(family: Family) -> ProviderError {
    ProviderError::Transport {
        family,
        reason: String::from("request cancelled"),
    }
}

fn redact<'a>(text: &'a str, secret: &str) -> std::borrow::Cow<'a, str> {
    if secret.is_empty() || !text.contains(secret) {
        std::borrow::Cow::Borrowed(text)
    } else {
        std::borrow::Cow::Owned(text.replace(secret, "<redacted>"))
    }
}

fn redact_provider_error(error: &mut ProviderError, secret: &str) {
    if secret.is_empty() {
        return;
    }
    let redact_string = |value: &mut String| {
        if value.contains(secret) {
            *value = value.replace(secret, "<redacted>");
        }
    };
    match error {
        ProviderError::Transport {
            reason: message, ..
        }
        | ProviderError::Status { message, .. }
        | ProviderError::InvalidRequest { message }
        | ProviderError::RateLimited { message, .. }
        | ProviderError::RetryAfterTooLong { message, .. }
        | ProviderError::Quota { message }
        | ProviderError::UsageNotIncluded { message }
        | ProviderError::ReserveUnavailable { message, .. }
        | ProviderError::TokenExchange { message, .. }
        | ProviderError::DeviceCode { message, .. } => redact_string(message),
        ProviderError::ContextOverflow { code, message, .. } => {
            redact_string(code);
            redact_string(message);
        }
        ProviderError::Protocol { detail, .. } => redact_string(detail),
        ProviderError::UsageLimit { model, message } => {
            redact_string(model);
            redact_string(message);
        }
        ProviderError::WsClosed {
            code: Some((_, reason)),
        } => redact_string(reason),
        _ => {}
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
