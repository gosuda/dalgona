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

use dal_agent::{Agent, Host};
use futures::{FutureExt, StreamExt, stream::FuturesUnordered};
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::error::WireError;
use crate::jsonrpc::{ErrorObject, Id, Message, decode_jsonrpc, encode_jsonrpc};
use crate::protocol::{capability_error, mint_client_id, negotiate_capabilities};
use crate::transport::{FrameWriter, ReadFrameError, Transport};

mod fail;
mod misc;
pub(crate) mod session;
pub(crate) mod subs;

pub(crate) use fail::{
    agent_error, decode_params, hint_value, host_error, id_key, invalid_params, scheme_error,
    to_value,
};
pub(crate) use subs::{host_notifier, send_resync, session_pump};

/// Maximum in-flight requests before the reader pauses.
pub(crate) const MAX_IN_FLIGHT: usize = 256;

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
    /// Sessions touched by this connection; dropping releases the holds.
    pub agents: HashMap<dal_core::SessionId, Agent>,
    /// Active session subscriptions: generation fence plus cancel token.
    pub subs: HashMap<dal_core::SessionId, (u64, CancellationToken)>,
    /// Active host subscription cancel token, when subscribed.
    pub host_sub: Option<CancellationToken>,
    /// Cancel token per in-flight request id for `$/cancel_request`.
    pub inflight: HashMap<String, CancellationToken>,
    /// Next subscription fence value.
    pub fence: u64,
}

impl Conn {
    fn new(client: dal_core::ClientId) -> Self {
        Self {
            initialized: false,
            client,
            caps: Vec::new(),
            agents: HashMap::new(),
            subs: HashMap::new(),
            host_sub: None,
            inflight: HashMap::new(),
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
pub async fn serve_rpc(host: Host, mut transport: Transport) -> Result<(), WireError> {
    let writer = transport.writer();
    let state = Arc::new(Mutex::new(Conn::new(mint_client_id("rpc"))));
    let stop = CancellationToken::new();
    let mut pending: FuturesUnordered<Pending> = FuturesUnordered::new();

    loop {
        tokio::select! {
            biased;
            frame = transport.read_frame(), if pending.len() < MAX_IN_FLIGHT => {
                let ended = matches!(
                    frame,
                    Err(ReadFrameError::EndOfInput
                        | ReadFrameError::Closed
                        | ReadFrameError::FrameTooLarge(_))
                );
                if let Ok(line) = frame
                    && let Some(task) =
                        on_frame(&host, Arc::clone(&state), &writer, &stop, &line).await
                {
                    pending.push(task);
                }
                if ended {
                    break;
                }
            }
            frame = writer.next_queued_frame() => {
                if let Some(text) = frame
                    && let Err(error) = writer.write_frame(&text).await
                {
                    tracing::debug!(%error, "frame write failed");
                }
            }
            _ = pending.next(), if !pending.is_empty() => {}
        }
    }

    if !pending.is_empty() {
        let _ = tokio::time::timeout(DRAIN_GRACE, async {
            while pending.next().await.is_some() {}
        })
        .await;
    }
    cancel_all(&state).await;
    while let Some(text) = writer.take_queued_frame() {
        let _ = writer.write_frame(&text).await;
    }
    Ok(())
}

type Pending = futures::future::BoxFuture<'static, ()>;

/// Handles one decoded frame; returns a handler future for requests.
async fn on_frame(
    host: &Host,
    state: Arc<Mutex<Conn>>,
    writer: &FrameWriter,
    stop: &CancellationToken,
    line: &str,
) -> Option<Pending> {
    let message = match decode_jsonrpc(line) {
        Ok(message) => message,
        Err(error) => {
            send(
                writer,
                &Message::Error {
                    id: error.id,
                    error: ErrorObject {
                        code: error.code,
                        message: error.message,
                        data: None,
                    },
                },
            )
            .await;
            return None;
        }
    };
    match message {
        Message::Request { id, method, params } => {
            let initialized = state.lock().await.initialized;
            if !initialized && method != "initialize" && method != "protocol/schema" {
                send(
                    writer,
                    &Message::Error {
                        id,
                        error: ErrorObject {
                            code: -32006,
                            message: "initialize must be the first request".to_owned(),
                            data: None,
                        },
                    },
                )
                .await;
                return None;
            }
            if initialized && method == "initialize" {
                send(
                    writer,
                    &Message::Error {
                        id,
                        error: ErrorObject {
                            code: -32600,
                            message: "initialize was already called".to_owned(),
                            data: None,
                        },
                    },
                )
                .await;
                return None;
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
        "auth/status" => run!("auth", || misc::auth_status(host, params)),
        "auth/login" => run!("auth", || misc::auth_login(host, params)),
        "auth/logout" => run!("auth", || async { misc::auth_logout() }),
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
    run().await.map_err(|error| {
        if error.code == -32602 && !error.message.starts_with("invalid params for") {
            invalid_params(method, error.message)
        } else {
            error
        }
    })
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
    let mut locked = state.lock().await;
    locked.client = mint_client_id(&name);
    locked.caps.clone_from(&negotiated);
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

/// Reads an optional string member.
pub(crate) fn opt_string(params: &Value, name: &str) -> Option<String> {
    params
        .get(name)
        .and_then(|value| value.as_str())
        .map(str::to_owned)
}

/// Reads an optional integer member.
pub(crate) fn opt_i64(params: &Value, name: &str) -> Option<i64> {
    params.get(name).and_then(Value::as_i64)
}
