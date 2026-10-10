//! Version-1 dal protocol connection state and request dispatch.
//!
//! One [`serve_rpc`] task owns a single line-framed transport. Requests run
//! concurrently in one [`FuturesUnordered`] set: the workspace forbids
//! `tokio::spawn`, so the read loop, method handlers, subscription pumps, and
//! login waiters all live in this task. Replies and notifications share the
//! transport's [`FrameWriter`], which serializes every frame.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use dal_agent::login::LoginId;
use dal_agent::{Agent, Host};
use futures::{FutureExt, StreamExt, stream::FuturesUnordered};
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::error::WireError;
use crate::jsonrpc::{ErrorObject, Id, Message, decode_jsonrpc, encode_jsonrpc};
use crate::protocol::{capability_error, mint_client_id, negotiate_capabilities};
use crate::transport::{FrameWriter, ReadFrameError, Transport};

mod auth;
mod fail;
mod misc;
pub(crate) mod session;
pub(crate) mod subs;

pub(crate) use fail::{
    agent_error, decode_params, hint_value, host_error, id_key, invalid_params,
    normalize_invalid_params, scheme_error, server_draining, to_value,
};
pub(crate) use subs::{host_notifier, send_resync, session_pump};

/// Maximum in-flight requests before the reader pauses.
pub(crate) const MAX_IN_FLIGHT: usize = 256;

/// Maximum rejection frames queued after a connection starts draining.
pub(crate) const MAX_DRAIN_REPLIES: usize = 64;

/// Grace period for draining handlers after transport end.
pub(crate) const DRAIN_GRACE: Duration = Duration::from_secs(1);

/// Mutable per-connection state shared by handlers in this task.
pub(crate) struct Conn {
    /// Whether `initialize` completed on this connection.
    pub initialized: bool,
    /// This connection's attributed client id (`<name>#<n>`).
    pub client: dal_core::ClientId,
    /// Negotiated capability names.
    pub caps: Vec<String>,
    /// Whether the client declared the `approval` answerer role in
    /// `initialize`. Read from the raw posted list: the protocol enabled
    /// set keeps only method capabilities, so this never appears in the
    /// `initialize` reply.
    pub answer_approval: bool,
    /// Whether the client declared the `ask` answerer role in `initialize`,
    /// spelled `ask` or `question`. Read from the raw posted list, never
    /// echoed in the reply.
    pub answer_ask: bool,
    /// Sessions touched by this connection; dropping releases the holds.
    pub agents: HashMap<dal_core::SessionId, Agent>,
    /// Active session subscriptions: generation fence plus cancel token.
    pub subs: HashMap<dal_core::SessionId, (u64, CancellationToken)>,
    /// Active host subscription cancel token, when subscribed.
    pub host_sub: Option<CancellationToken>,
    /// Cancel token per in-flight request id for `$/cancel_request`.
    pub inflight: HashMap<String, CancellationToken>,
    /// Running OAuth logins by their minted `LoginId`; closing the connection
    /// cancels them.
    pub logins: HashMap<LoginId, CancellationToken>,
    /// Next subscription fence value.
    pub fence: u64,
}

impl Conn {
    fn new(client: dal_core::ClientId) -> Self {
        Self {
            initialized: false,
            client,
            caps: Vec::new(),
            answer_approval: false,
            answer_ask: false,
            agents: HashMap::new(),
            subs: HashMap::new(),
            host_sub: None,
            inflight: HashMap::new(),
            logins: HashMap::new(),
            fence: 0,
        }
    }

    fn has(&self, capability: &str) -> bool {
        self.caps.iter().any(|name| name == capability)
    }
}

/// Serves version-1 dal RPC on one transport until it ends.
///
/// Malformed frames answer one null-id error and leave the connection usable.
/// Client `Result`/`Error` frames are logged and dropped. On disconnect the
/// connection drops its subscriptions and session holds without cancelling
/// any turn, then drains in-flight handlers for at most one second.
///
/// # Errors
///
/// Returns [`WireError`] when the transport fails or a reply cannot be framed.
pub async fn serve_rpc(host: Host, transport: Transport) -> Result<(), WireError> {
    serve_rpc_draining(host, transport, CancellationToken::new()).await
}

/// Serves one connection like [`serve_rpc`] until `drain` fires.
///
/// # Errors
///
/// Returns [`WireError`] when the transport fails or a reply cannot be framed.
pub async fn serve_rpc_draining(
    host: Host,
    mut transport: Transport,
    drain: CancellationToken,
) -> Result<(), WireError> {
    let mut draining_until: Option<tokio::time::Instant> = None;
    let writer = transport.writer();
    let state = Arc::new(Mutex::new(Conn::new(mint_client_id("rpc"))));
    let stop = CancellationToken::new();
    let mut pending: FuturesUnordered<Pending> = FuturesUnordered::new();
    let mut drain_replies = 0;

    'serve: loop {
        tokio::select! {
            biased;
            () = tokio::time::sleep_until(draining_until.unwrap_or_else(tokio::time::Instant::now)),
                if draining_until.is_some() => break,
            () = drain.cancelled(), if draining_until.is_none() => {
                draining_until = Some(tokio::time::Instant::now() + DRAIN_GRACE);
            }
            frame = transport.read_frame(), if pending.len() < MAX_IN_FLIGHT => {
                let ended = matches!(
                    frame,
                    Err(ReadFrameError::EndOfInput
                        | ReadFrameError::Closed
                        | ReadFrameError::FrameTooLarge(_))
                );
                if let Ok(line) = frame
                    && let Some(task) = on_frame(
                        &host,
                        Arc::clone(&state),
                        &writer,
                        &stop,
                        draining_until.is_some(),
                        &mut drain_replies,
                        &line,
                    )
                    .await
                {
                    pending.push(task);
                }
                if ended {
                    break;
                }
            }
            frame = writer.next_queued_frame() => {
                if let Some(text) = frame {
                    // Keep drain cancellation and its deadline polled while a
                    // backpressured peer is still receiving a queued frame.
                    let write = writer.write_frame(&text);
                    tokio::pin!(write);
                    loop {
                        tokio::select! {
                            biased;
                            () = tokio::time::sleep_until(
                                draining_until.unwrap_or_else(tokio::time::Instant::now),
                            ), if draining_until.is_some() => break 'serve,
                            () = drain.cancelled(), if draining_until.is_none() => {
                                draining_until = Some(tokio::time::Instant::now() + DRAIN_GRACE);
                            }
                            result = &mut write => {
                                if let Err(error) = result {
                                    tracing::debug!(%error, "frame write failed");
                                }
                                break;
                            }
                        }
                    }
                }
            }
            _ = pending.next(), if !pending.is_empty() => {}
        }
    }

    cancel_logins(&state).await;
    if !pending.is_empty() {
        let grace = draining_until.map_or(DRAIN_GRACE, |until| {
            until.saturating_duration_since(tokio::time::Instant::now())
        });
        let _ =
            tokio::time::timeout(grace, async { while pending.next().await.is_some() {} }).await;
    }
    cancel_all(&state).await;
    if draining_until.is_none() {
        while let Some(text) = writer.take_queued_frame() {
            let _ = writer.write_frame(&text).await;
        }
    }
    transport.close().await;
    Ok(())
}

/// Answers one request with an error and starts no handler.
async fn reject(writer: &FrameWriter, id: Id, error: ErrorObject) -> Option<Pending> {
    send(writer, &Message::Error { id, error }).await;
    None
}

type Pending = futures::future::BoxFuture<'static, ()>;

/// Handles one decoded frame; returns a handler future for requests.
async fn on_frame(
    host: &Host,
    state: Arc<Mutex<Conn>>,
    writer: &FrameWriter,
    stop: &CancellationToken,
    draining: bool,
    drain_replies: &mut usize,
    line: &str,
) -> Option<Pending> {
    if draining && *drain_replies >= MAX_DRAIN_REPLIES {
        return None;
    }
    let message = match decode_jsonrpc(line) {
        Ok(message) => message,
        Err(error) => {
            if draining {
                *drain_replies += 1;
            }
            let object = ErrorObject {
                code: error.code,
                message: error.message,
                data: None,
            };
            return reject(writer, error.id, object).await;
        }
    };
    match message {
        Message::Request { id, .. } if draining => {
            *drain_replies += 1;
            reject(writer, id, server_draining()).await
        }
        Message::Request { id, method, params } => {
            let initialized = state.lock().await.initialized;
            if !initialized && method != "initialize" && method != "protocol/schema" {
                let object = ErrorObject {
                    code: -32006,
                    message: "initialize must be the first request".to_owned(),
                    data: None,
                };
                return reject(writer, id, object).await;
            }
            if initialized && method == "initialize" {
                let object = ErrorObject {
                    code: -32600,
                    message: "initialize was already called".to_owned(),
                    data: None,
                };
                return reject(writer, id, object).await;
            }
            let child = stop.child_token();
            let key = id_key(&id);
            state
                .lock()
                .await
                .inflight
                .insert(key.clone(), child.clone());
            let host = host.clone();
            let writer = writer.clone();
            Some(
                async move {
                    handle_request(RequestArgs {
                        host,
                        state,
                        writer,
                        id,
                        key,
                        method,
                        params,
                        cancel: child,
                    })
                    .await;
                }
                .boxed(),
            )
        }
        Message::Notification { method, params } => {
            if method == "$/cancel_request" {
                cancel_request(&state, &params).await;
            } else {
                tracing::debug!(method = %method, "dropping client notification");
            }
            None
        }
        Message::Result { id, .. } | Message::Error { id, .. } => {
            tracing::debug!(request_id = %id_key(&id), "dropping client response frame");
            None
        }
    }
}
/// One in-flight request with its routing and cancellation facts.
struct RequestArgs {
    /// The serving host for handler calls.
    host: Host,
    /// Mutable per-connection state for inflight tracking.
    state: Arc<Mutex<Conn>>,
    /// Shared writer for replies and notifications.
    writer: FrameWriter,
    /// The request id for the reply envelope.
    id: Id,
    /// The cancel-table key for this request id.
    key: String,
    /// The method name for dispatch.
    method: String,
    /// The method params for dispatch.
    params: Value,
    /// The per-request cancel token.
    cancel: CancellationToken,
}
async fn handle_request(args: RequestArgs) {
    let RequestArgs {
        host,
        state,
        writer,
        id,
        key,
        method,
        params,
        cancel,
    } = args;
    let reply = tokio::select! {
        biased;
        () = cancel.cancelled() => Some(Message::Error {
            id,
            error: ErrorObject {
                code: -32800,
                message: format!("request {key} was cancelled"),
                data: None,
            },
        }),
        reply = dispatch(&host, &state, &writer, &id, &method, &params) => reply,
    };
    if let Some(reply) = reply {
        send(&writer, &reply).await;
    }
    state.lock().await.inflight.remove(&key);
}

/// Cancels the in-flight request named by a `$/cancel_request` notification.
async fn cancel_request(state: &Arc<Mutex<Conn>>, params: &Value) {
    let target = params
        .get("requestId")
        .and_then(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .or_else(|| value.as_i64().map(|number| number.to_string()))
        })
        .filter(|name| !name.is_empty());
    let Some(target) = target else {
        return;
    };
    if let Some(token) = state.lock().await.inflight.get(&target) {
        token.cancel();
    }
}

/// Cancels every in-flight request, subscription pump, and host notifier.
async fn cancel_all(state: &Arc<Mutex<Conn>>) {
    let locked = state.lock().await;
    for token in locked.inflight.values() {
        token.cancel();
    }
    for (_, token) in locked.subs.values() {
        token.cancel();
    }
    if let Some(token) = locked.host_sub.as_ref() {
        token.cancel();
    }
}

/// Cancels every running OAuth login so the flows end and publish their
/// outcome while the connection drains.
async fn cancel_logins(state: &Arc<Mutex<Conn>>) {
    for token in state.lock().await.logins.values() {
        token.cancel();
    }
}

/// Queues one message for the serving task to write; producers never block
/// on the writer lock, so the returned future is already resolved. A failed
/// enqueue only logs, the read loop observes closure.
pub(crate) fn send(writer: &FrameWriter, message: &Message) -> std::future::Ready<()> {
    match encode_jsonrpc(message) {
        Ok(frame) => {
            if let Err(error) = writer.enqueue_frame(frame) {
                tracing::debug!(%error, "reply write failed");
            }
        }
        Err(error) => tracing::error!(%error, "reply encoding failed"),
    }
    std::future::ready(())
}

/// Dispatches one request to its handler.
async fn dispatch(
    host: &Host,
    state: &Arc<Mutex<Conn>>,
    writer: &FrameWriter,
    id: &Id,
    method: &str,
    params: &Value,
) -> Option<Message> {
    let ok = |result: Value| {
        Some(Message::Result {
            id: id.clone(),
            result,
        })
    };
    let fail = |error: ErrorObject| {
        Some(Message::Error {
            id: id.clone(),
            error,
        })
    };
    macro_rules! run {
        ($cap:literal, $call:expr) => {
            guarded(state, $cap, method, params, $call)
                .await
                .map_or_else(fail, ok)
        };
    }
    match method {
        "initialize" => initialize(state, params).await.map_or_else(fail, ok),
        "protocol/schema" => ok(crate::protocol::protocol_schema()),
        "session/list" => run!("sessions", || async { session::list(host, params) }),
        "session/open" => run!("sessions", || session::open(host, state, params)),
        "session/close" => run!("sessions", || session::close(host, state, params)),
        "session/view" => run!("sessions", || session::view(host, state, params)),
        "session/subscribe" => match require_cap(state, "sessions").await {
            Err(error) => fail(error),
            Ok(()) => session::subscribe(host, state, writer, id, params).await,
        },
        "session/unsubscribe" => {
            run!("sessions", || session::unsubscribe(state, params))
        }
        "session/submit" => run!("sessions", || session::submit(host, state, params)),
        "session/answer" => run!("sessions", || session::answer(host, state, params)),
        "blob/read" => run!("blobs", || misc::blob_read(host, state, params)),
        "commands/list" => ok(misc::commands_list(host, params)),
        "models/list" => run!("models", || misc::models_list(host, params)),
        "docs/read" => run!("docs", || async { misc::docs_read(host, params) }),
        "host/subscribe" => match require_cap(state, "host.updates").await {
            Err(error) => fail(error),
            Ok(()) => misc::host_subscribe(host, state, writer, id, params).await,
        },
        "host/unsubscribe" => {
            run!("host.updates", || misc::host_unsubscribe(state, params))
        }
        "auth/status" => run!("auth", || auth::auth_status(host, params)),
        "auth/login" => match require_cap(state, "auth").await {
            Err(error) => fail(error),
            Ok(()) => auth::auth_login(host, state, writer, id, params).await,
        },
        "auth/cancel" => run!("auth", || async { auth::auth_cancel(host, params) }),
        "auth/logout" => run!("auth", || auth::auth_logout(host, params)),
        _ => fail(ErrorObject {
            code: -32601,
            message: format!(r#"unknown method "{method}""#),
            data: None,
        }),
    }
}

/// Checks one capability without running a handler.
async fn require_cap(state: &Arc<Mutex<Conn>>, capability: &str) -> Result<(), ErrorObject> {
    if state.lock().await.has(capability) {
        Ok(())
    } else {
        let (code, message) = capability_error(capability);
        Err(ErrorObject {
            code,
            message,
            data: None,
        })
    }
}

/// Runs a capability-guarded handler.
async fn guarded<Fut>(
    state: &Arc<Mutex<Conn>>,
    capability: &str,
    method: &str,
    _params: &Value,
    run: impl FnOnce() -> Fut,
) -> Result<Value, ErrorObject>
where
    Fut: Future<Output = Result<Value, ErrorObject>>,
{
    require_cap(state, capability).await?;
    run()
        .await
        .map_err(|error| normalize_invalid_params(method, error))
}

/// Handles `initialize`: negotiates version 1 and the capability intersection.
async fn initialize(state: &Arc<Mutex<Conn>>, params: &Value) -> Result<Value, ErrorObject> {
    let version = params.get("protocolVersion").and_then(Value::as_i64);
    if let Some(number) = version
        && number < 1
    {
        return Err(ErrorObject {
            code: -32600,
            message: format!("protocol version {number} is not supported: dalgon speaks 1"),
            data: None,
        });
    }
    let requested: Vec<String> = params
        .get("capabilities")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let negotiated = negotiate_capabilities(&requested);
    let name = crate::protocol::client_name(params, "rpc");
    let answer_approval = requested.iter().any(|name| name == "approval");
    // The ask role accepts two spellings, `ask` and `question`. Either
    // declaration grants the role; capability-gated methods ignore both,
    // so the `initialize` reply never echoes them.
    let answer_ask = requested
        .iter()
        .any(|name| name == "ask" || name == "question");
    let mut locked = state.lock().await;
    locked.client = mint_client_id(&name);
    locked.caps.clone_from(&negotiated);
    locked.answer_approval = answer_approval;
    locked.answer_ask = answer_ask;
    locked.initialized = true;
    let client = locked.client.as_str().to_owned();
    drop(locked);
    let capabilities: Vec<Value> = negotiated
        .into_iter()
        .map(|name| Value::from(name.as_str()))
        .collect();
    Ok(sonic_rs::json!({
        "protocolVersion": 1,
        "serverInfo": {"name": "dal", "version": crate_version()},
        "capabilities": capabilities,
        "clientId": client,
    }))
}

/// Returns the provider label when a non-harness route has no resolvable credential.
pub(crate) fn missing_credential(host: &Host, route: &dal_core::ModelRoute) -> Option<String> {
    if matches!(route, dal_core::ModelRoute::Harness { .. }) {
        return None;
    }
    let provider = provider_label(route);
    (!host.has_credential(&provider)).then_some(provider)
}

/// Returns the provider label for one model route.
pub(crate) fn provider_label(route: &dal_core::ModelRoute) -> String {
    match route {
        dal_core::ModelRoute::Synthetic { id } => id.split('/').next().unwrap_or("").to_owned(),
        dal_core::ModelRoute::Api { family, .. } => match family {
            dal_core::Family::Chat | dal_core::Family::Responses => "openai".to_owned(),
            dal_core::Family::Codex => "openai-codex".to_owned(),
            dal_core::Family::Anthropic => "anthropic".to_owned(),
        },
        dal_core::ModelRoute::Harness { .. } => "dalgon".to_owned(),
    }
}

/// Derives the `SET <ENV>` hint name for one provider id.
pub(crate) fn env_name(provider: &str) -> String {
    format!("{}_API_KEY", provider.to_uppercase().replace('-', "_"))
}
/// Crate version for `initialize` and ACP agent info.
pub(crate) fn crate_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Looks up one member of a method's params object.
///
/// An omitted member is `Ok(None)`. Params that are not an object are an
/// error, so a caller never reads "no members" out of a malformed request.
fn member<'a>(
    method: &str,
    params: &'a Value,
    name: &str,
) -> Result<Option<&'a Value>, ErrorObject> {
    if !params.is_object() {
        return Err(invalid_params(method, "params must be an object"));
    }
    Ok(params.get(name))
}

/// Reads an optional string member.
///
/// Omitted is `Ok(None)`; a present member of any other type, `null`
/// included, is `-32602`, so a malformed value never selects the default.
pub(crate) fn opt_string(
    method: &str,
    params: &Value,
    name: &str,
) -> Result<Option<String>, ErrorObject> {
    let Some(value) = member(method, params, name)? else {
        return Ok(None);
    };
    let text = value
        .as_str()
        .ok_or_else(|| invalid_params(method, format!("member `{name}` must be a string")))?;
    Ok(Some(text.to_owned()))
}

/// Reads an optional string member that a client may send as `null`.
///
/// Omitted and `null` are both `Ok(None)`; any other non-string is `-32602`.
pub(crate) fn opt_nullable_string(
    method: &str,
    params: &Value,
    name: &str,
) -> Result<Option<String>, ErrorObject> {
    if member(method, params, name)?.is_some_and(Value::is_null) {
        return Ok(None);
    }
    opt_string(method, params, name)
}

/// Reads a required string member.
pub(crate) fn req_string(method: &str, params: &Value, name: &str) -> Result<String, ErrorObject> {
    opt_string(method, params, name)?
        .ok_or_else(|| invalid_params(method, format!("missing member `{name}`")))
}

/// Reads an optional integer member; omitted is `Ok(None)`, any other
/// non-integer is `-32602`.
pub(crate) fn opt_i64(
    method: &str,
    params: &Value,
    name: &str,
) -> Result<Option<i64>, ErrorObject> {
    let Some(value) = member(method, params, name)? else {
        return Ok(None);
    };
    let number = value
        .as_i64()
        .ok_or_else(|| invalid_params(method, format!("member `{name}` must be an integer")))?;
    Ok(Some(number))
}
