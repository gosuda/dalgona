//! Reusable Responses-family WebSocket sessions and the typed Responses event
//! stream shared by Codex and OpenAI Responses providers.
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
const WS_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
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
    /// An OpenAI Responses request using provider-configured authentication.
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

/// WebSocket sessions, owned by the provider set and scoped by session and
/// provider id. No background task is used; idle expiry is checked at turn
/// start.
pub(crate) struct WsSessions {
    sessions: Mutex<HashMap<(SessionId, Box<str>), Arc<Session>>>,
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
    _turn: Option<OwnedMutexGuard<()>>,
    secret: Box<str>,
}

struct HandshakeFailure {
    status: Option<u16>,
    code: Option<String>,
    message: String,
    error: ProviderError,
    mapped: Option<ProviderError>,
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
        if self.fallback_active(&session, model) {
            return Ok(WsTurn::HttpsFallback);
        }

        let url = websocket_url(base_url, family)?;
        let frame = codex::websocket_frame(wire.body())?;
        if frame.len() > WS_MESSAGE_LIMIT {
            return Err(ProviderError::Limit(LimitError::WsMessage));
        }
        let frame = String::from_utf8(frame).map_err(|_| ProviderError::Protocol {
            family,
            detail: format!("{family:?} generated a non-UTF-8 WebSocket frame"),
        })?;
        let reusable = self.take_reusable(&session, model);
        let max_attempts = stream_max_retries.saturating_add(1);
        let request_state = RequestState {
            family,
            provider,
            model,
            oauth: wire.oauth(),
            refreshed,
            delivered: false,
        };
        let mut idle = reusable;
        let mut last_failure = None;

        for attempt in 1..=max_attempts {
            let (mut socket, born) = match idle.take() {
                Some(connection) => (connection.socket, connection.born),
                None => match connect(&url, wire, self.now(), cancel).await {
                    Ok(connection) => connection,
                    Err(failure) if failure.status == Some(401) => {
                        lock(&session.state).idle = None;
                        return Ok(WsTurn::HttpsFallback);
                    }
                    Err(failure) => {
                        if let Some(error) = self
                            .retry_failure(
                                failure,
                                attempt,
                                max_attempts,
                                stream_max_retries,
                                &request_state,
                                notices,
                                cancel,
                            )
                            .await?
                        {
                            last_failure = Some(error);
                            break;
                        }
                        continue;
                    }
                },
            };
            let send_result = tokio::select! {
                () = cancel.cancelled() => {
                    close_socket(&mut socket).await;
                    return Err(cancelled(family));
                }
                result = socket.send(Message::Text(frame.clone().into())) => result,
            };
            if send_result.is_err() {
                let failure = transport_failure(family, "WebSocket request frame send failed");
                if let Some(error) = self
                    .retry_failure(
                        failure,
                        attempt,
                        max_attempts,
                        stream_max_retries,
                        &request_state,
                        notices,
                        cancel,
                    )
                    .await?
                {
                    last_failure = Some(error);
                    break;
                }
                continue;
            }
            let pending = match first_frame(&mut socket, wire, cancel).await {
                Ok(frame) => frame,
                Err(failure) if failure.status == Some(401) => {
                    close_socket(&mut socket).await;
                    lock(&session.state).idle = None;
                    return Ok(WsTurn::HttpsFallback);
                }
                Err(failure) => {
                    if let Some(error) = self
                        .retry_failure(
                            failure,
                            attempt,
                            max_attempts,
                            stream_max_retries,
                            &request_state,
                            notices,
                            cancel,
                        )
                        .await?
                    {
                        last_failure = Some(error);
                        break;
                    }
                    continue;
                }
            };

            let source = Source {
                socket: Some(socket),
                decoder: responses::Decoder::new(family, model.to_owned()),
                ready: VecDeque::new(),
                pending: Some(pending),
                session: Arc::clone(&session),
                family,
                model: model.into(),
                born,
                clock: Arc::clone(&self.clock),
                cancel: cancel.clone(),
                finished: false,
                secret: wire.secret().into(),
                _turn: Some(turn),
            };
            let session_on_cancel = Arc::clone(&session);
            let events = stream::unfold(source, |mut source| async move {
                source.next().await.map(|item| (item, source))
            });
            return Ok(WsTurn::Stream(EventStream::new(events, move || {
                lock(&session_on_cancel.state).idle = None;
            })));
        }

        let mut state = lock(&session.state);
        state.idle = None;
        state.fallback_model = Some(model.into());
        drop(state);
        let last_failure = last_failure.unwrap_or_else(|| ProviderError::Transport {
            family,
            reason: String::from("WebSocket handshake failed"),
        });
        let error = redact(last_failure.to_string(), wire.secret()).into_owned();
        notices(format!("{FALLBACK_NOTICE} {error}"));
        Ok(WsTurn::HttpsFallback)

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

    fn fallback_active(&self, session: &Session, model: &str) -> bool {
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
        let idle = now.checked_duration_since(connection.since).unwrap_or_default();
        let age = now.checked_duration_since(connection.born).unwrap_or_default();
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
        failure: HandshakeFailure,
        attempt: u32,
        max_attempts: u32,
        max_retries: u32,
        request: &RequestState<'_>,
        notices: &NoticeSink,
        cancel: &CancellationToken,
    ) -> Result<Option<ProviderError>, ProviderError> {
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
                    self._turn = None;
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
    url.set_scheme(scheme).map_err(|()| ProviderError::Transport {
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
) -> Result<(Socket, Instant), HandshakeFailure> {
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
) -> Result<Message, HandshakeFailure> {
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
                return Err(mapped_failure(error, "invalid first WebSocket event", family));
            }
            Some(Ok(Message::Close(frame))) => {
                return Err(HandshakeFailure {
                    status: None,
                    code: None,
                    message: String::from("WebSocket closed before its first event"),
                    error: close_error(frame, secret),
                    mapped: None,
                });
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
                return Err(transport_failure(family, "WebSocket closed before its first event"));
            }
        }
    }
}

fn validate_first_text(data: &str, wire: WsWire<'_>) -> Result<(), HandshakeFailure> {
    let family = wire.family();
    if is_connection_limit(data) {
        return Err(transport_failure(family, "websocket connection limit reached"));
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

fn handshake_failure(error: WsError, wire: WsWire<'_>) -> HandshakeFailure {
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
) -> HandshakeFailure {
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
    let code = parsed
        .as_ref()
        .and_then(error_code)
        .map(String::from);
    let mapped = if family == Family::Codex && matches!(status, 400 | 403 | 404) {
        body.as_deref()
            .and_then(|body| crate::usage::map_codex_error(status, body, model))
    } else {
        None
    };
    HandshakeFailure {
        status: Some(status),
        code,
        message: message.clone(),
        error: ProviderError::Status {
            family,
            status,
            message,
        },
        mapped,
    }
}

fn mapped_failure(error: ProviderError, message: &str, family: Family) -> HandshakeFailure {
    HandshakeFailure {
        status: None,
        code: None,
        message: String::from(message),
        error: ProviderError::Transport {
            family,
            reason: String::from(message),
        },
        mapped: Some(error),
    }
}

fn transport_failure(family: Family, message: &str) -> HandshakeFailure {
    HandshakeFailure {
        status: None,
        code: None,
        message: String::from(message),
        error: ProviderError::Transport {
            family,
            reason: String::from(message),
        },
        mapped: None,
    }
}

fn cancel_failure(family: Family) -> HandshakeFailure {
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
    [
        top_code,
        nested_type,
        nested_code,
    ]
    .contains(&Some("websocket_connection_limit_reached"))
}

fn is_error_frame(data: &str) -> bool {
    sonic_rs::from_str::<sonic_rs::Value>(data)
        .ok()
        .and_then(|value| value.get("type").and_then(JsonValueTrait::as_str))
        == Some("error")
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

fn websocket_error(
    data: &str,
    family: Family,
    model: &str,
    secret: &str,
) -> Option<ProviderError> {
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
            && let Some(error) =
                crate::usage::map_codex_error(status, safe_data.as_ref(), model)
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
        ProviderError::Transport { reason, .. } => redact_string(reason),
        ProviderError::Status { message, .. }
        | ProviderError::InvalidRequest { message }
        | ProviderError::RateLimited { message, .. }
        | ProviderError::RetryAfterTooLong { message, .. }
        | ProviderError::Quota { message }
        | ProviderError::UsageNotIncluded { message }
        | ProviderError::ReserveUnavailable { message, .. }
        | ProviderError::TokenExchange { message, .. }
        | ProviderError::DeviceCode { message, .. } => redact_string(message),
        ProviderError::ContextOverflow {
            code, message, ..
        } => {
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
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::{
        error::Error,
        sync::{
            Arc,
            atomic::{AtomicU64, AtomicUsize, Ordering},
        },
    };

    use dal_core::{
        ContextItem, Family, ModelRequest, ModelRoute, Part, Purpose, RequestParams, SessionId,
        ThinkingLevel,
    };

    use futures::SinkExt;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        time::{Duration, Instant, timeout},
    };
    use tokio_tungstenite::{
        accept_async,
        tungstenite::{
            Message,
            handshake::server::{Request, Response},
            protocol::{CloseFrame, frame::coding::CloseCode},
        },
    };

    use super::*;
    use crate::{
        auth::credential::{Credential, SecretString},
        family::{codex::CodexWire, responses::ResponsesRequest},
        provider::AuthStyle,
        stream::{NoticeSink, StreamEvent},
        thinking::WireThinking,
    };

    const COMPLETE: &str = r#"{"type":"response.completed","sequence_number":2,"response":{"output":[],"usage":null}}"#;
    const TEXT_DELTA: &str = r#"{"type":"response.output_text.delta","sequence_number":1,"delta":"x"}"#;

    #[test]
    fn retry_jitter_maps_uniform_samples_to_the_full_allowed_range() {
        assert_eq!(jitter_for_sample(0), 0.9);
        assert!((jitter_for_sample(u32::MAX / 2) - 1.0).abs() < 1.0e-9);
        assert!((jitter_for_sample(u32::MAX) - 1.1).abs() < f64::EPSILON);
    }

    fn wire(model: &str) -> CodexWire {
        wire_for_session(model, SessionId::new_v7())
    }

    fn wire_for_session(model: &str, session_id: SessionId) -> CodexWire {
        let session = session_id.to_string();
        CodexWire {
            headers: vec![
                ("authorization", String::from("Bearer test-token")),
                ("chatgpt-account-id", String::from("account")),
                ("originator", String::from(crate::auth::oauth::CODEX_ORIGINATOR)),
                ("session-id", session.clone()),
                ("thread-id", session.clone()),
                ("x-client-request-id", session.clone()),
            ],
            body: format!(
                r#"{{"model":"{model}","input":[],"store":false,"stream":true,"include":["reasoning.encrypted_content"],"prompt_cache_key":"{session}"}}"#
            )
            .into_bytes(),
            model: model.into(),
            session_id,
            user_agent: String::from("dalgon/test (Linux test; x86_64)"),
        }
    }

    fn responses_wire(
        model: &str,
        session_id: SessionId,
        auth: AuthStyle,
    ) -> crate::family::responses::ResponsesWire {
        let request = ModelRequest {
            purpose: Purpose::Turn,
            model: ModelRoute::Api {
                family: Family::Responses,
                model: model.into(),
            },
            system: Arc::from("Test instructions"),
            tools: Vec::new().into(),
            context: Arc::from([ContextItem::User {
                parts: vec![Part::Text {
                    text: "full user history".into(),
                }],
            }]),
            params: RequestParams {
                thinking: ThinkingLevel::High,
                effort: None,
                temperature: None,
            },
            cache_key: Some(format!("{session_id}:1").into_boxed_str()),
        };
        let credential = Credential::ApiKey {
            key: SecretString::from("responses-test-key"),
        };
        crate::family::responses::wire(ResponsesRequest {
            request: &request,
            thinking: WireThinking::OpenAi {
                effort: Some("high"),
            },
            reasoning_summary: false,
            auth,
            credential: &credential,
            session_id,
            user_agent: "dalgon/test (Linux test; x86_64)",
        })
        .expect("Responses request has a valid route and credential")
    }

    fn ws_request<'a>(
        provider: &'a str,
        session_id: SessionId,
        base_url: &'a str,
        wire: WsWire<'a>,
        stream_max_retries: u32,
        refreshed: bool,
        notices: &'a NoticeSink,
        cancel: &'a CancellationToken,
    ) -> WsRequest<'a> {
        WsRequest {
            provider,
            session_id,
            base_url,
            wire,
            stream_max_retries,
            refreshed,
            notices,
            cancel,
        }
    }

    fn fast_sessions(offset: Arc<AtomicU64>) -> WsSessions {
        let origin = Instant::now();
        let clock: Clock = Arc::new(move || origin + Duration::from_secs(offset.load(Ordering::SeqCst)));
        let sleeper: Sleeper = Arc::new(|_| Box::pin(async {}));
        WsSessions::with_clock_and_sleeper(clock, sleeper)
    }

    fn notices() -> (NoticeSink, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let target = Arc::clone(&seen);
        (Arc::new(move |notice| lock(&target).push(notice)), seen)
    }

    async fn drain(mut stream: EventStream) -> Result<Vec<StreamEvent>, ProviderError> {
        let mut events = Vec::new();
        while let Some(item) = stream.next().await {
            events.push(item?);
        }
        Ok(events)
    }

    async fn server_replies(listener: TcpListener, handshakes: Arc<AtomicUsize>, requests: usize) {
        for _ in 0..requests {
            let (tcp, _) = listener.accept().await.expect("client connects");
            let mut socket = accept_async(tcp).await.expect("client handshake is valid");
            handshakes.fetch_add(1, Ordering::SeqCst);
            while let Some(Ok(Message::Text(_))) = socket.next().await {
                socket
                    .send(Message::Text(COMPLETE.into()))
                    .await
                    .expect("response frame reaches client");
            }
        }
    }

    async fn open_stream(
        sessions: &WsSessions,
        base: &str,
        wire: &CodexWire,
        notices: &NoticeSink,
    ) -> EventStream {
        let cancel = CancellationToken::new();
        match sessions
            .open(ws_request(
                "openai-codex",
                wire.session_id,
                base,
                WsWire::Codex(wire),
                5,
                false,
                notices,
                &cancel,
            ))
            .await
            .expect("WebSocket turn opens")
        {
            WsTurn::Stream(stream) => stream,
            WsTurn::HttpsFallback => panic!("loopback WebSocket must not fall back"),
        }
    }

    #[tokio::test]
    async fn websocket_reuses_session_socket_within_idle_window() -> Result<(), Box<dyn Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}/backend-api/codex", listener.local_addr()?);
        let handshakes = Arc::new(AtomicUsize::new(0));
        let server_count = Arc::clone(&handshakes);
        tokio::spawn(server_replies(listener, server_count, 1));
        let offset = Arc::new(AtomicU64::new(0));
        let sessions = fast_sessions(Arc::clone(&offset));
        let wire = wire("gpt-6-luna");
        let (notice, _) = notices();

        assert!(drain(open_stream(&sessions, &base, &wire, &notice).await).await.is_ok());
        offset.store(60, Ordering::SeqCst);
        assert!(drain(open_stream(&sessions, &base, &wire, &notice).await).await.is_ok());
        assert_eq!(handshakes.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[tokio::test]
    async fn websocket_idle_expiry_reconnects_after_six_minutes() -> Result<(), Box<dyn Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}/backend-api/codex", listener.local_addr()?);
        let handshakes = Arc::new(AtomicUsize::new(0));
        let server_count = Arc::clone(&handshakes);
        tokio::spawn(server_replies(listener, server_count, 2));
        let offset = Arc::new(AtomicU64::new(0));
        let sessions = fast_sessions(Arc::clone(&offset));
        let wire = wire("gpt-6-luna");
        let (notice, _) = notices();

        assert!(drain(open_stream(&sessions, &base, &wire, &notice).await).await.is_ok());
        offset.store(6 * 60, Ordering::SeqCst);
        assert!(drain(open_stream(&sessions, &base, &wire, &notice).await).await.is_ok());

        assert_eq!(handshakes.load(Ordering::SeqCst), 2);
        Ok(())
    }

    #[tokio::test]
    async fn six_failed_handshakes_enable_https_until_the_model_changes() -> Result<(), Box<dyn Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}/backend-api/codex", listener.local_addr()?);
        let accepted = Arc::new(AtomicUsize::new(0));
        let server_accepted = Arc::clone(&accepted);
        tokio::spawn(async move {
            for _ in 0..12 {
                if let Ok((tcp, _)) = listener.accept().await {
                    server_accepted.fetch_add(1, Ordering::SeqCst);
                    drop(tcp);
                }
            }
        });
        let sessions = fast_sessions(Arc::new(AtomicU64::new(0)));
        let session = SessionId::new_v7();
        let first_wire = wire_for_session("gpt-6-luna", session);
        let second_wire = wire_for_session("gpt-5.6-luna", session);
        let (notice, seen) = notices();
        let cancel = CancellationToken::new();

        let first = sessions
            .open(ws_request(
                "openai-codex",
                session,
                &base,
                WsWire::Codex(&first_wire),
                5,
                false,
                &notice,
                &cancel,
            ))
            .await?;
        assert_eq!(accepted.load(Ordering::SeqCst), 6);
        assert!(matches!(first, WsTurn::HttpsFallback));
        assert_eq!(
            lock(&seen)
                .iter()
                .filter(|notice| notice.starts_with(FALLBACK_NOTICE))
                .count(),
            1
        );
        assert!(lock(&seen).iter().all(|notice| !notice.contains("test-token")));

        let second = sessions
            .open(ws_request(
                "openai-codex",
                session,
                &base,
                WsWire::Codex(&first_wire),
                5,
                false,
                &notice,
                &cancel,
            ))
            .await?;
        assert!(matches!(second, WsTurn::HttpsFallback));
        assert_eq!(accepted.load(Ordering::SeqCst), 6);

        let changed = sessions
            .open(ws_request(
                "openai-codex",
                session,
                &base,
                WsWire::Codex(&second_wire),
                5,
                false,
                &notice,
                &cancel,
            ))
            .await?;
        assert!(matches!(changed, WsTurn::HttpsFallback));
        assert_eq!(accepted.load(Ordering::SeqCst), 12);
        assert_eq!(
            lock(&seen)
                .iter()
                .filter(|notice| notice.starts_with(FALLBACK_NOTICE))
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn websocket_connection_limit_before_first_event_reconnects() -> Result<(), Box<dyn Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}/backend-api/codex", listener.local_addr()?);
        let handshakes = Arc::new(AtomicUsize::new(0));
        let server_count = Arc::clone(&handshakes);
        tokio::spawn(async move {
            for attempt in 0..2 {
                let (tcp, _) = listener.accept().await.expect("client connects");
                let mut socket = accept_async(tcp).await.expect("handshake succeeds");
                server_count.fetch_add(1, Ordering::SeqCst);
                let _ = socket.next().await;
                let response = if attempt == 0 {
                    r#"{"type":"error","error":{"type":"server_error","code":"websocket_connection_limit_reached","message":"limit"}}"#
                } else {
                    COMPLETE
                };
                socket
                    .send(Message::Text(response.into()))
                    .await
                    .expect("event reaches client");
            }
        });
        let sessions = fast_sessions(Arc::new(AtomicU64::new(0)));
        let (notice, _) = notices();
        let wire = wire("gpt-6-luna");
        let stream = open_stream(&sessions, &base, &wire, &notice).await;

        assert!(drain(stream).await.is_ok());
        assert_eq!(handshakes.load(Ordering::SeqCst), 2);
        Ok(())
    }

    #[tokio::test]
    async fn websocket_close_frame_preserves_code_and_reason() -> Result<(), Box<dyn Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}/backend-api/codex", listener.local_addr()?);
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("client connects");
            let mut socket = accept_async(tcp).await.expect("handshake succeeds");
            let _ = socket.next().await;
            socket
                .send(Message::Text(TEXT_DELTA.into()))
                .await
                .expect("first event reaches client");
            socket
                .send(Message::Close(Some(CloseFrame {
                    code: CloseCode::Error,
                    reason: "busy".into(),
                })))
                .await
                .expect("close reaches client");
        });
        let sessions = fast_sessions(Arc::new(AtomicU64::new(0)));
        let (notice, _) = notices();
        let wire = wire("gpt-6-luna");
        let mut stream = open_stream(&sessions, &base, &wire, &notice).await;
        assert!(matches!(
            stream.next().await,
            Some(Ok(StreamEvent::TextDelta { text })) if text == "x"
        ));
        let error = stream
            .next()
            .await
            .expect("close is an error")
            .expect_err("close before terminal fails");
        assert_eq!(
            error.to_string(),
            "websocket closed by server before response.completed. (code 1011: busy)"
        );
        Ok(())
    }

    #[tokio::test]
    async fn dropping_after_first_delta_releases_the_socket_within_250ms() -> Result<(), Box<dyn Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}/backend-api/codex", listener.local_addr()?);
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("client connects");
            let mut socket = accept_async(tcp).await.expect("handshake succeeds");
            let _ = socket.next().await;
            socket
                .send(Message::Text(TEXT_DELTA.into()))
                .await
                .expect("delta reaches client");
            let closed = timeout(Duration::from_millis(250), socket.next()).await;
            let _ = closed_tx.send(matches!(closed, Ok(None | Some(Err(_)))));
        });
        let sessions = fast_sessions(Arc::new(AtomicU64::new(0)));
        let (notice, _) = notices();
        let wire = wire("gpt-6-luna");
        let mut stream = open_stream(&sessions, &base, &wire, &notice).await;
        assert!(matches!(
            stream.next().await,
            Some(Ok(StreamEvent::TextDelta { text })) if text == "x"
        ));
        drop(stream);
        assert!(closed_rx.await?);
        Ok(())
    }

    #[tokio::test]
    async fn websocket_frame_uses_beta_header_and_success_stream_terminates() -> Result<(), Box<dyn Error>> {

        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}/backend-api/codex", listener.local_addr()?);
        let wire = wire("gpt-6-luna");
        let expected_session = wire.session_id.to_string();
        let expected_frame = format!(
            r#"{{"type":"response.create","model":"gpt-6-luna","input":[],"store":false,"include":["reasoning.encrypted_content"],"prompt_cache_key":"{expected_session}"}}"#
        );
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("client connects");
            let mut socket = tokio_tungstenite::accept_hdr_async(
                tcp,
                |request: &Request, response: Response| {
                    let headers = request.headers();
                    assert_eq!(
                        headers
                            .get("openai-beta")
                            .and_then(|value| value.to_str().ok()),
                        Some("responses_websockets=2026-02-06")
                    );
                    assert_eq!(
                        headers
                            .get("originator")
                            .and_then(|value| value.to_str().ok()),
                        Some(crate::auth::oauth::CODEX_ORIGINATOR)
                    );
                    assert_eq!(
                        headers
                            .get("user-agent")
                            .and_then(|value| value.to_str().ok()),
                        Some("dalgon/test (Linux test; x86_64)")
                    );
                    assert!(headers.get("authorization").is_some());
                    assert!(headers.get("chatgpt-account-id").is_some());
                    assert_eq!(
                        headers
                            .get("session-id")
                            .and_then(|value| value.to_str().ok()),
                        Some(expected_session.as_str())
                    );
                    assert_eq!(headers.get("session-id"), headers.get("thread-id"));
                    assert_eq!(headers.get("session-id"), headers.get("x-client-request-id"));
                    assert!(headers.get("accept").is_none());
                    Ok(response)
                },
            )
            .await
            .expect("Codex headers pass the handshake");
            let expected = expected_frame;
            assert!(matches!(
                socket.next().await,
                Some(Ok(Message::Text(frame))) if frame.as_str() == expected
            ));
            socket
                .send(Message::Text(COMPLETE.into()))
                .await
                .expect("terminal event reaches client");
        });
        let sessions = fast_sessions(Arc::new(AtomicU64::new(0)));
        let (notice, _) = notices();
        let events = drain(open_stream(&sessions, &base, &wire, &notice).await).await?;
        assert!(matches!(events.last(), Some(StreamEvent::Stop { .. })));
        Ok(())
    }

    #[tokio::test]
    async fn responses_api_key_handshake_frame_and_reuse() -> Result<(), Box<dyn Error>> {
        for auth in [AuthStyle::Bearer, AuthStyle::XApiKey] {
            let listener = TcpListener::bind("127.0.0.1:0").await?;
            let base = format!("http://{}/v1", listener.local_addr()?);
            let session = SessionId::new_v7();
            let wire = responses_wire("gpt-5", session, auth);
            let frame = crate::family::codex::websocket_frame(&wire.body)?;
            let frame = String::from_utf8(frame)?;
            let frame_json = sonic_rs::from_str::<sonic_rs::Value>(&frame)?;
            assert_eq!(
                frame_json.get("type").and_then(JsonValueTrait::as_str),
                Some("response.create")
            );
            assert!(frame_json.get("stream").is_none());
            assert!(frame_json.get("previous_response_id").is_none());
            let expected_cache_key = format!("{session}:1");
            assert_eq!(
                frame_json
                    .get("prompt_cache_key")
                    .and_then(JsonValueTrait::as_str),
                Some(expected_cache_key.as_str())
            );
            assert!(frame.contains("full user history"));
            assert!(!frame.contains("responses-test-key"));
            let server_frame = frame.clone();
            let handshakes = Arc::new(AtomicUsize::new(0));
            let server_handshakes = Arc::clone(&handshakes);
            let server = tokio::spawn(async move {
                let (tcp, _) = listener.accept().await.expect("client connects");
                let mut socket = tokio_tungstenite::accept_hdr_async(
                    tcp,
                    move |request: &Request, response: Response| {
                        let headers = request.headers();
                        assert_eq!(
                            headers
                                .get("openai-beta")
                                .and_then(|value| value.to_str().ok()),
                            Some(BETA_HEADER)
                        );
                        assert_eq!(
                            headers
                                .get("user-agent")
                                .and_then(|value| value.to_str().ok()),
                            Some("dalgon/test (Linux test; x86_64)")
                        );
                        match auth {
                            AuthStyle::Bearer => {
                                assert_eq!(
                                    headers
                                        .get("authorization")
                                        .and_then(|value| value.to_str().ok()),
                                    Some("Bearer responses-test-key")
                                );
                                assert!(headers.get("x-api-key").is_none());
                            }
                            AuthStyle::XApiKey => {
                                assert_eq!(
                                    headers
                                        .get("x-api-key")
                                        .and_then(|value| value.to_str().ok()),
                                    Some("responses-test-key")
                                );
                                assert!(headers.get("authorization").is_none());
                            }
                        }
                        for name in [
                            "chatgpt-account-id",
                            "originator",
                            "session-id",
                            "thread-id",
                            "x-client-request-id",
                        ] {
                            assert!(headers.get(name).is_none(), "unexpected header {name}");
                        }
                        assert!(headers.get("accept").is_none());
                        Ok(response)
                    },
                )
                .await
                .expect("Responses headers pass the handshake");
                server_handshakes.fetch_add(1, Ordering::SeqCst);
                for _ in 0..2 {
                    assert!(matches!(
                        socket.next().await,
                        Some(Ok(Message::Text(actual))) if actual.as_str() == server_frame
                    ));
                    socket
                        .send(Message::Text(COMPLETE.into()))
                        .await
                        .expect("terminal event reaches client");
                }
            });
            let sessions = fast_sessions(Arc::new(AtomicU64::new(0)));
            let (notice, _) = notices();
            let cancel = CancellationToken::new();
            for _ in 0..2 {
                let stream = match sessions
                    .open(ws_request(
                        "openai-responses",
                        session,
                        &base,
                        WsWire::Responses(&wire),
                        5,
                        false,
                        &notice,
                        &cancel,
                    ))
                    .await?
                {
                    WsTurn::Stream(stream) => stream,
                    WsTurn::HttpsFallback => {
                        return Err("Responses loopback unexpectedly fell back".into());
                    }
                };
                let events = drain(stream).await?;
                assert_eq!(
                    events
                        .iter()
                        .filter(|event| matches!(event, StreamEvent::Stop { .. }))
                        .count(),
                    1
                );
            }
            assert_eq!(handshakes.load(Ordering::SeqCst), 1);
            server.await?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn responses_first_frame_401_falls_back_without_latching() -> Result<(), Box<dyn Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}/v1", listener.local_addr()?);
        let session = SessionId::new_v7();
        let wire = responses_wire("gpt-5", session, AuthStyle::Bearer);
        let handshakes = Arc::new(AtomicUsize::new(0));
        let server_count = Arc::clone(&handshakes);
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("first client connects");
            let mut first = accept_async(tcp).await.expect("first handshake succeeds");
            server_count.fetch_add(1, Ordering::SeqCst);
            let _ = first.next().await;
            first
                .send(Message::Text(
                    r#"{"type":"error","status":401,"error":{"code":"invalid_api_key","message":"denied"}}"#
                        .into(),
                ))
                .await
                .expect("401 error frame reaches client");
            assert!(matches!(first.next().await, Some(Ok(Message::Close(_)))));

            let (tcp, _) = listener.accept().await.expect("second client connects");
            let mut second = accept_async(tcp).await.expect("second handshake succeeds");
            server_count.fetch_add(1, Ordering::SeqCst);
            assert!(matches!(second.next().await, Some(Ok(Message::Text(_)))));
            second
                .send(Message::Text(COMPLETE.into()))
                .await
                .expect("second response reaches client");
        });
        let sessions = fast_sessions(Arc::new(AtomicU64::new(0)));
        let (notice, seen) = notices();
        let cancel = CancellationToken::new();
        let first = sessions
            .open(ws_request(
                "openai-responses",
                session,
                &base,
                WsWire::Responses(&wire),
                5,
                false,
                &notice,
                &cancel,
            ))
            .await?;
        assert!(matches!(first, WsTurn::HttpsFallback));
        assert!(
            lock(&seen)
                .iter()
                .all(|notice| !notice.starts_with(FALLBACK_NOTICE))
        );

        let second = sessions
            .open(ws_request(
                "openai-responses",
                session,
                &base,
                WsWire::Responses(&wire),
                5,
                false,
                &notice,
                &cancel,
            ))
            .await?;
        let WsTurn::Stream(stream) = second else {
            return Err("401 unexpectedly latched WebSocket fallback".into());
        };
        assert!(drain(stream).await.is_ok());
        assert_eq!(handshakes.load(Ordering::SeqCst), 2);
        server.await?;
        Ok(())
    }

    #[tokio::test]
    async fn responses_handshake_401_falls_back_to_lifecycle_immediately() -> Result<(), Box<dyn Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}/v1", listener.local_addr()?);
        let session = SessionId::new_v7();
        let wire = responses_wire("gpt-5", session, AuthStyle::Bearer);
        let attempts = Arc::new(AtomicUsize::new(0));
        let server_attempts = Arc::clone(&attempts);
        let server = tokio::spawn(async move {
            let (mut tcp, _) = listener.accept().await.expect("first client connects");
            server_attempts.fetch_add(1, Ordering::SeqCst);
            let mut request = Vec::new();
            let mut buffer = [0_u8; 256];
            loop {
                let count = tcp.read(&mut buffer).await.expect("request bytes arrive");
                request.extend_from_slice(&buffer[..count]);
                if count == 0 || request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            tcp.write_all(
                b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .expect("401 response reaches client");

            let (tcp, _) = listener.accept().await.expect("second client connects");
            server_attempts.fetch_add(1, Ordering::SeqCst);
            let mut socket = accept_async(tcp).await.expect("second handshake succeeds");
            assert!(matches!(socket.next().await, Some(Ok(Message::Text(_)))));
            socket
                .send(Message::Text(COMPLETE.into()))
                .await
                .expect("second response reaches client");
        });
        let sessions = fast_sessions(Arc::new(AtomicU64::new(0)));
        let (notice, seen) = notices();
        let cancel = CancellationToken::new();
        let first = sessions
            .open(ws_request(
                "openai-responses",
                session,
                &base,
                WsWire::Responses(&wire),
                5,
                false,
                &notice,
                &cancel,
            ))
            .await?;
        assert!(matches!(first, WsTurn::HttpsFallback));
        assert!(lock(&seen)
            .iter()
            .all(|notice| !notice.starts_with(FALLBACK_NOTICE)));

        let second = sessions
            .open(ws_request(
                "openai-responses",
                session,
                &base,
                WsWire::Responses(&wire),
                5,
                false,
                &notice,
                &cancel,
            ))
            .await?;
        let WsTurn::Stream(stream) = second else {
            return Err("401 unexpectedly latched WebSocket fallback".into());
        };
        assert!(drain(stream).await.is_ok());
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        server.await?;
        Ok(())
    }

    #[tokio::test]
    async fn responses_pre_event_overload_retries_through_retry_policy() -> Result<(), Box<dyn Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}/v1", listener.local_addr()?);
        let session = SessionId::new_v7();
        let wire = responses_wire("gpt-5", session, AuthStyle::Bearer);
        let handshakes = Arc::new(AtomicUsize::new(0));
        let server_count = Arc::clone(&handshakes);
        let server = tokio::spawn(async move {
            for attempt in 0..2 {
                let (tcp, _) = listener.accept().await.expect("client connects");
                let mut socket = accept_async(tcp).await.expect("handshake succeeds");
                server_count.fetch_add(1, Ordering::SeqCst);
                let _ = socket.next().await;
                let frame = if attempt == 0 {
                    r#"{"type":"error","status":503,"error":{"type":"server_is_overloaded","message":"busy"}}"#
                } else {
                    COMPLETE
                };
                socket
                    .send(Message::Text(frame.into()))
                    .await
                    .expect("server frame reaches client");
            }
        });
        let sessions = fast_sessions(Arc::new(AtomicU64::new(0)));
        let (notice, seen) = notices();
        let cancel = CancellationToken::new();
        let turn = sessions
            .open(ws_request(
                "openai-responses",
                session,
                &base,
                WsWire::Responses(&wire),
                5,
                false,
                &notice,
                &cancel,
            ))
            .await?;
        let WsTurn::Stream(stream) = turn else {
            return Err("overload retries unexpectedly fell back".into());
        };
        assert!(drain(stream).await.is_ok());
        assert_eq!(handshakes.load(Ordering::SeqCst), 2);
        assert!(
            lock(&seen)
                .iter()
                .any(|notice| notice.starts_with("Retrying in "))
        );
        server.await?;
        Ok(())
    }


    #[tokio::test]
    async fn responses_errors_redact_api_key_without_scrubbing_success_events() -> Result<(), Box<dyn Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}/v1", listener.local_addr()?);
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("client connects");
            let mut socket = accept_async(tcp).await.expect("handshake succeeds");
            let _ = socket.next().await;
            socket
                .send(Message::Text(TEXT_DELTA.into()))
                .await
                .expect("success event reaches client");
            socket
                .send(Message::Text(
                    r#"{"type":"error","status":400,"error":{"type":"invalid_request_error","message":"rejected responses-test-key"}}"#
                        .into(),
                ))
                .await
                .expect("error event reaches client");
        });
        let session = SessionId::new_v7();
        let wire = responses_wire("gpt-5", session, AuthStyle::XApiKey);
        let sessions = fast_sessions(Arc::new(AtomicU64::new(0)));
        let (notice, _) = notices();
        let cancel = CancellationToken::new();
        let turn = sessions
            .open(ws_request(
                "named-openai",
                session,
                &base,
                WsWire::Responses(&wire),
                5,
                false,
                &notice,
                &cancel,
            ))
            .await?;
        let WsTurn::Stream(mut stream) = turn else {
            return Err("Responses loopback unexpectedly fell back".into());
        };
        assert!(matches!(
            stream.next().await,
            Some(Ok(StreamEvent::TextDelta { text })) if text == "x"
        ));
        let error = stream
            .next()
            .await
            .expect("error frame is delivered")
            .expect_err("error frame ends the stream");
        let message = error.to_string();
        assert!(message.contains("<redacted>"));
        assert!(!message.contains("responses-test-key"));
        server.await?;
        Ok(())
    }
    #[tokio::test]
    async fn responses_six_failed_handshakes_fall_back_once() -> Result<(), Box<dyn Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}/v1", listener.local_addr()?);
        let accepted = Arc::new(AtomicUsize::new(0));
        let server_accepted = Arc::clone(&accepted);
        let server = tokio::spawn(async move {
            for _ in 0..6 {
                let (tcp, _) = listener.accept().await.expect("client connects");
                server_accepted.fetch_add(1, Ordering::SeqCst);
                drop(tcp);
            }
        });
        let session = SessionId::new_v7();
        let wire = responses_wire("gpt-5", session, AuthStyle::XApiKey);
        let sessions = fast_sessions(Arc::new(AtomicU64::new(0)));
        let (notice, seen) = notices();
        let cancel = CancellationToken::new();
        let turn = sessions
            .open(ws_request(
                "named-openai",
                session,
                &base,
                WsWire::Responses(&wire),
                5,
                false,
                &notice,
                &cancel,
            ))
            .await?;
        assert!(matches!(turn, WsTurn::HttpsFallback));
        assert_eq!(accepted.load(Ordering::SeqCst), 6);
        assert_eq!(
            lock(&seen)
                .iter()
                .filter(|notice| notice.starts_with(FALLBACK_NOTICE))
                .count(),
            1
        );
        assert!(lock(&seen)
            .iter()
            .all(|notice| !notice.contains("responses-test-key")));

        let repeated = sessions
            .open(ws_request(
                "named-openai",
                session,
                &base,
                WsWire::Responses(&wire),
                5,
                false,
                &notice,
                &cancel,
            ))
            .await?;
        assert!(matches!(repeated, WsTurn::HttpsFallback));
        assert_eq!(accepted.load(Ordering::SeqCst), 6);
        assert_eq!(
            lock(&seen)
                .iter()
                .filter(|notice| notice.starts_with(FALLBACK_NOTICE))
                .count(),
            1
        );
        server.await?;
        Ok(())
    }

    #[tokio::test]
    async fn responses_cancellation_sends_a_close_frame() -> Result<(), Box<dyn Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}/v1", listener.local_addr()?);
        let (close_tx, close_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("client connects");
            let mut socket = accept_async(tcp).await.expect("handshake succeeds");
            let _ = socket.next().await;
            socket
                .send(Message::Text(TEXT_DELTA.into()))
                .await
                .expect("delta reaches client");
            let closed = matches!(socket.next().await, Some(Ok(Message::Close(_))));
            let _ = close_tx.send(closed);
        });
        let session = SessionId::new_v7();
        let wire = responses_wire("gpt-5", session, AuthStyle::Bearer);
        let sessions = fast_sessions(Arc::new(AtomicU64::new(0)));
        let (notice, _) = notices();
        let cancel = CancellationToken::new();
        let turn = sessions
            .open(ws_request(
                "openai-responses",
                session,
                &base,
                WsWire::Responses(&wire),
                5,
                false,
                &notice,
                &cancel,
            ))
            .await?;
        let WsTurn::Stream(mut stream) = turn else {
            return Err("Responses loopback unexpectedly fell back".into());
        };
        assert!(matches!(
            stream.next().await,
            Some(Ok(StreamEvent::TextDelta { text })) if text == "x"
        ));
        cancel.cancel();
        assert!(matches!(
            stream.next().await,
            Some(Err(ProviderError::Transport {
                family: Family::Responses,
                ..
            }))
        ));
        assert!(close_rx.await?);
        server.await?;
        Ok(())
    }

    #[tokio::test]
    async fn websocket_rejects_a_message_above_the_shared_limit() -> Result<(), Box<dyn Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}/backend-api/codex", listener.local_addr()?);
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("client connects");
            let mut socket = accept_async(tcp).await.expect("handshake succeeds");
            let _ = socket.next().await;
            let payload = String::from_utf8(vec![b'x'; WS_MESSAGE_LIMIT + 1])
                .expect("ASCII frame is UTF-8");
            let _ = socket.send(Message::Text(payload.into())).await;
        });
        let sessions = fast_sessions(Arc::new(AtomicU64::new(0)));
        let (notice, _) = notices();
        let wire = wire("gpt-6-luna");
        let cancel = CancellationToken::new();
        let result = sessions
            .open(ws_request(
                "openai-codex",
                wire.session_id,
                &base,
                WsWire::Codex(&wire),
                5,
                false,
                &notice,
                &cancel,
            ))
            .await;

        assert!(matches!(
            result,
            Err(ProviderError::Limit(LimitError::WsMessage))
        ));
        Ok(())
    }
}
