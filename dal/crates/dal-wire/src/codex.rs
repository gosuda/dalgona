//! Codex app-server adapter: stdio JSONL and the `/codex/ws` upgrade.
//!
//! The adapter is independent of dal JSON-RPC and the router, and every
//! frame omits `jsonrpc`. The supported method sets come from the pinned
//! schema fixture (`tests/fixtures/codex-app-server-schema.json`, openai/codex
//! `5c8fc15cc99241d3a1a415a629841540cdff4b28`): a client request outside
//! `clientRequests` returns `-32601`. Threads map to sessions, turns to
//! turns, and items to entries. Transport loss never closes or cancels a
//! thread: sessions follow the Host/Agent lifetime.

pub(crate) mod ask;
pub(crate) mod items;
pub(crate) mod schema;
pub(crate) mod threads;
mod turns;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use dal_agent::{Agent, Host};
use dal_core::{ClientId, SessionId};
use futures::{FutureExt, StreamExt, stream::FuturesUnordered};
use sonic_rs::{JsonContainerTrait, JsonValueMutTrait, JsonValueTrait, Value};
use tokio::sync::{Mutex, oneshot};
use tokio_util::sync::CancellationToken;

use crate::error::WireError;
use crate::jsonrpc::ErrorObject;
use crate::protocol::mint_client_id;
use crate::rpc::{DRAIN_GRACE, MAX_IN_FLIGHT, invalid_params};
use crate::transport::{FrameWriter, ReadFrameError, Transport};

/// A handler result: a `result` value or an error object.
type Outcome = Result<Value, ErrorObject>;

/// Serves the Codex subset on one transport (stdio JSONL or WebSocket).
///
/// Client disconnect follows the Host/Agent lifetime: turns keep running and
/// threads are never closed by transport loss.
///
/// # Errors
///
/// Returns [`WireError`] when the transport fails.
pub async fn serve_codex(host: Host, mut transport: Transport) -> Result<(), WireError> {
    let writer = transport.writer();
    let ctx = Ctx {
        host,
        state: Arc::new(Mutex::new(Conn::default())),
        writer: writer.clone(),
        stop: CancellationToken::new(),
    };
    let mut pending: FuturesUnordered<futures::future::BoxFuture<'static, ()>> =
        FuturesUnordered::new();
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
                    && let Some(task) = ctx.on_frame(&line).await
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
                    tracing::debug!(%error, "codex frame write failed");
                }
            }
            _ = pending.next(), if !pending.is_empty() => {}
        }
    }
    ctx.stop.cancel();
    if !pending.is_empty() {
        let _ = tokio::time::timeout(DRAIN_GRACE, async {
            while pending.next().await.is_some() {}
        })
        .await;
    }
    while let Some(text) = writer.take_queued_frame() {
        let _ = writer.write_frame(&text).await;
    }
    Ok(())
}

/// Mutable per-connection Codex state.
#[derive(Default)]
struct Conn {
    /// The client identity minted at `initialize`.
    client: Option<ClientId>,
    /// Notification methods the client opted out of.
    opt_out: Vec<String>,
    /// Threads opened on this connection.
    threads: HashMap<SessionId, Agent>,
    /// Threads opened on this connection as ephemeral sessions.
    ephemeral: HashSet<SessionId>,
    /// The last server request id.
    next_req: i64,
    /// Outstanding server requests by id; `None` carries a client error.
    replies: HashMap<i64, oneshot::Sender<Option<Value>>>,
}

/// Shared per-connection handles.
#[derive(Clone)]
struct Ctx {
    host: Host,
    state: Arc<Mutex<Conn>>,
    writer: FrameWriter,
    stop: CancellationToken,
}

impl Ctx {
    /// Handles one frame; returns a handler future for concurrent requests.
    async fn on_frame(&self, line: &str) -> Option<futures::future::BoxFuture<'static, ()>> {
        let value: Value = match sonic_rs::from_str(line) {
            Ok(value) => value,
            Err(error) => {
                let message = format!("frame is not valid JSON: {error}");
                self.reply(&Value::default(), Err(error_object(-32700, message)))
                    .await;
                return None;
            }
        };
        let id = value.get("id").cloned();
        let Some(method) = value.get("method").and_then(|value| value.as_str()) else {
            if let Some(id) = id {
                self.on_response(&id, &value).await;
            } else {
                let error = error_object(-32600, "frame is not a Codex message".to_owned());
                self.reply(&Value::default(), Err(error)).await;
            }
            return None;
        };
        let params = value.get("params").cloned().unwrap_or_else(empty_object);
        let Some(id) = id else {
            on_notification(method);
            return None;
        };
        if !schema::is_client_request(method) {
            let error = error_object(-32601, format!("unknown method \"{method}\""));
            self.reply(&id, Err(error)).await;
            return None;
        }
        let initialized = self.state.lock().await.client.is_some();
        if method == "initialize" {
            let outcome = if initialized {
                Err(error_object(-32600, "Already initialized".to_owned()))
            } else {
                self.initialize(&params).await
            };
            self.reply(&id, outcome).await;
            return None;
        }
        if !initialized {
            let error = error_object(-32600, "Not initialized".to_owned());
            self.reply(&id, Err(error)).await;
            return None;
        }
        let ctx = self.clone();
        let method = method.to_owned();
        Some(
            async move {
                let outcome = tokio::select! {
                    biased;
                    () = ctx.stop.cancelled() => None,
                    outcome = ctx.dispatch(&id, &method, &params) => outcome,
                };
                if let Some(outcome) = outcome {
                    ctx.reply(&id, outcome).await;
                }
            }
            .boxed(),
        )
    }

    /// Dispatches one supported request; `None` means the reply was sent inline.
    async fn dispatch(&self, id: &Value, method: &str, params: &Value) -> Option<Outcome> {
        match method {
            "thread/start" => threads::start(self, id, params).await,
            "thread/resume" => Some(threads::resume(self, params).await),
            "thread/list" => Some(threads::list(self, params).await),
            "turn/start" => turns::start(self, id, params).await,
            "turn/interrupt" => Some(turns::interrupt(self, params).await),
            other => Some(Err(error_object(
                -32601,
                format!("unknown method \"{other}\""),
            ))),
        }
    }

    /// Handles `initialize`: records the client and its notification opt-outs.
    async fn initialize(&self, params: &Value) -> Outcome {
        let name = params
            .get("clientInfo")
            .and_then(|info| info.get("name"))
            .and_then(|value| value.as_str())
            .ok_or_else(|| invalid_params("initialize", "clientInfo.name is required"))?;
        let opt_out: Vec<String> = params
            .get("capabilities")
            .and_then(|capabilities| capabilities.get("optOutNotificationMethods"))
            .and_then(|value| value.as_array())
            .map(|methods| {
                methods
                    .iter()
                    .filter_map(|method| method.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        {
            let mut state = self.state.lock().await;
            state.client = Some(mint_client_id(name));
            state.opt_out = opt_out;
        }
        Ok(sonic_rs::json!({
            "codexHome": self.host.data_root().display().to_string(),
            "platformFamily": std::env::consts::FAMILY,
            "platformOs": std::env::consts::OS,
            "userAgent": format!("dal/{}", crate::rpc::crate_version()),
        }))
    }

    /// Routes a client response to its outstanding server request.
    async fn on_response(&self, id: &Value, frame: &Value) {
        let Some(key) = id.as_i64() else {
            tracing::debug!("dropping codex response with a non-integer id");
            return;
        };
        let sender = self.state.lock().await.replies.remove(&key);
        if let Some(sender) = sender {
            let _ = sender.send(frame.get("result").cloned());
        } else {
            tracing::debug!(request_id = key, "dropping late codex response");
        }
    }

    /// Returns the connection's client identity.
    async fn client(&self) -> Result<ClientId, ErrorObject> {
        self.state
            .lock()
            .await
            .client
            .clone()
            .ok_or_else(|| error_object(-32600, "Not initialized".to_owned()))
    }

    /// Returns one thread opened on this connection.
    async fn thread(&self, method: &str, session: SessionId) -> Result<Agent, ErrorObject> {
        self.state
            .lock()
            .await
            .threads
            .get(&session)
            .cloned()
            .ok_or_else(|| invalid_params(method, format!("thread {session} is not loaded")))
    }

    /// Sends one server request and returns its id and result receiver.
    async fn request(
        &self,
        method: &str,
        params: Value,
    ) -> (i64, oneshot::Receiver<Option<Value>>) {
        let (sender, receiver) = oneshot::channel();
        let id = {
            let mut state = self.state.lock().await;
            state.next_req += 1;
            let id = state.next_req;
            state.replies.insert(id, sender);
            id
        };
        let frame = sonic_rs::json!({"id": id, "method": method, "params": params});
        self.send(&frame).await;
        (id, receiver)
    }

    /// Drops one outstanding server request.
    async fn forget(&self, id: i64) {
        self.state.lock().await.replies.remove(&id);
    }

    /// Sends one notification unless the client opted out of its method.
    async fn notify(&self, method: &str, params: Value) {
        if self
            .state
            .lock()
            .await
            .opt_out
            .iter()
            .any(|opted| opted == method)
        {
            return;
        }
        self.send(&sonic_rs::json!({"method": method, "params": params}))
            .await;
    }

    /// Sends one response frame.
    async fn reply(&self, id: &Value, outcome: Outcome) {
        let frame = match outcome {
            Ok(result) => sonic_rs::json!({"id": id, "result": result}),
            Err(error) => {
                let mut body = sonic_rs::json!({"code": error.code, "message": error.message});
                if let (Some(data), Some(object)) = (error.data, body.as_object_mut()) {
                    object.insert("data", data);
                }
                sonic_rs::json!({"id": id, "error": body})
            }
        };
        self.send(&frame).await;
    }

    /// Queues one frame for the serving task to write; producers never block
    /// on the writer lock, so the returned future is already resolved.
    /// A failed enqueue only logs, the read loop observes closure.
    fn send(&self, frame: &Value) -> std::future::Ready<()> {
        match sonic_rs::to_string(frame) {
            Ok(text) => {
                if let Err(error) = self.writer.enqueue_frame(text) {
                    tracing::debug!(%error, "codex frame write failed");
                }
            }
            Err(error) => tracing::error!(%error, "codex frame encoding failed"),
        }
        std::future::ready(())
    }
}

/// Handles a client notification; `initialized` needs no action and unpinned
/// methods are dropped.
fn on_notification(method: &str) {
    if !schema::is_client_notification(method) {
        tracing::debug!(method = %method, "dropping unknown codex notification");
    }
}

/// Builds an error object without data.
fn error_object(code: i32, message: String) -> ErrorObject {
    ErrorObject {
        code,
        message,
        data: None,
    }
}

/// Returns an empty JSON object.
fn empty_object() -> Value {
    sonic_rs::json!({})
}
