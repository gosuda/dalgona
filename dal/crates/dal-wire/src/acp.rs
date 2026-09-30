//! Agent Client Protocol branches, versions 1 and 2.
//!
//! One [`serve_acp`] task owns a single line-framed transport. The protocol
//! version is selected once per connection by `initialize`: `1` selects v1,
//! `2` selects v2, and any other value selects v2. This module owns the serve
//! loop, version selection, client-attributed sessions, and the shared prompt
//! pipeline; `v1` and `v2` own branch dispatch, and `map` owns update,
//! request, stop, and content mapping.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use dal_agent::{Agent, Host};
use dal_core::SessionId;
use futures::{FutureExt, StreamExt, stream::FuturesUnordered};
use sonic_rs::{JsonValueTrait, Value};
use tokio::sync::{Mutex, oneshot};
use tokio_util::sync::CancellationToken;

use crate::error::WireError;
use crate::jsonrpc::{ErrorObject, Id, Message, decode_jsonrpc};
use crate::transport::{FrameWriter, ReadFrameError, Transport};

mod content;
mod init;
pub(crate) mod map;
mod notify;
pub(crate) mod replay;
mod session;
pub(crate) mod v1;
pub(crate) mod v2;

pub(crate) use content::{prompt_parts, slash_command};
use init::initialize;
pub(crate) use notify::{note_mcp_servers, send_commands_update, send_notice, send_update};
pub(crate) use session::{
    agent_for, cancel_outstanding, check_idle, check_model, open_workspace, submit_prompt,
    view_head,
};
use session::{cancel_session, complete_pending, fail_pending, fail_pending_by_client, teardown};

/// An ACP branch selected for one connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcpVersion {
    /// Agent Client Protocol version 1.
    V1,
    /// Agent Client Protocol version 2.
    V2,
}

/// Mutable per-connection ACP state shared by handlers in this task.
pub(crate) struct AcpConn {
    /// The selected branch.
    pub version: AcpVersion,
    /// Whether `initialize` completed on this connection.
    pub initialized: bool,
    /// This connection's attributed client id.
    pub client: dal_core::ClientId,
    /// Sessions bound by this connection; dropping releases the holds.
    pub agents: HashMap<SessionId, Agent>,
    /// Sessions this connection opened; transport loss cancels and closes them.
    pub opened: HashSet<SessionId>,
    /// Whether the v1 client advertised the `elicitation.form` capability.
    pub elicitation_form: bool,
    /// Server-initiated request counter.
    pub next_req: u64,
    /// Outstanding server-initiated requests: client id to session plus sender.
    pub pending: HashMap<String, (SessionId, oneshot::Sender<ServerAnswer>)>,
    /// Core request ids awaiting a client answer: core id to client id.
    pub outstanding: HashMap<dal_core::RequestId, String>,
    /// Cancel token per in-flight client request id.
    pub inflight: HashMap<String, CancellationToken>,
}

/// A client answer to one server-initiated request.
#[derive(Clone, Debug)]
pub(crate) struct ServerAnswer {
    /// The client's raw result value.
    pub result: Value,
}

/// Serves ACP on one transport until it ends.
///
/// Transport loss cancels the turns of sessions this connection opened and
/// closes those sessions; all other sessions keep running.
///
/// # Errors
///
/// Returns [`WireError`] when the transport fails or a frame cannot be written.
pub async fn serve_acp(host: Host, mut transport: Transport) -> Result<(), WireError> {
    let writer = transport.writer();
    let state = Arc::new(Mutex::new(AcpConn::new()));
    let stop = CancellationToken::new();
    let mut pending: FuturesUnordered<futures::future::BoxFuture<'static, ()>> =
        FuturesUnordered::new();

    loop {
        if pending.len() >= crate::rpc::MAX_IN_FLIGHT {
            pending.next().await;
            continue;
        }
        tokio::select! {
            biased;
            frame = transport.read_frame() => {
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
            _ = pending.next(), if !pending.is_empty() => {}
        }
    }

    teardown(&host, &state).await;
    if !pending.is_empty() {
        let _ = tokio::time::timeout(crate::rpc::DRAIN_GRACE, async {
            while pending.next().await.is_some() {}
        })
        .await;
    }
    Ok(())
}

impl AcpConn {
    /// Creates the state of one fresh, uninitialized connection.
    pub(crate) fn new() -> Self {
        Self {
            version: AcpVersion::V2,
            initialized: false,
            client: crate::protocol::mint_client_id("acp"),
            agents: HashMap::new(),
            opened: HashSet::new(),
            elicitation_form: false,
            next_req: 0,
            pending: HashMap::new(),
            outstanding: HashMap::new(),
            inflight: HashMap::new(),
        }
    }
}

/// Handles one decoded frame; returns a handler future for requests.
async fn on_frame(
    host: &Host,
    state: Arc<Mutex<AcpConn>>,
    writer: &FrameWriter,
    stop: &CancellationToken,
    line: &str,
) -> Option<futures::future::BoxFuture<'static, ()>> {
    if is_batch(line) {
        return on_batch(host, state, writer, stop, line).await;
    }
    let message = match decode_jsonrpc(line) {
        Ok(message) => message,
        Err(error) => {
            crate::rpc::send(
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
            if !initialized && method != "initialize" {
                crate::rpc::send(
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
            let child = stop.child_token();
            let key = crate::rpc::id_key(&id);
            state
                .lock()
                .await
                .inflight
                .insert(key.clone(), child.clone());
            let host = host.clone();
            let writer = writer.clone();
            Some(
                async move {
                    let reply = if method == "session/prompt" {
                        dispatch(&host, &state, &writer, &id, &method, &params, &child).await
                    } else {
                        tokio::select! {
                            biased;
                            () = child.cancelled() => Some(Message::Error {
                                id,
                                error: ErrorObject {
                                    code: -32800,
                                    message: format!("request {key} was cancelled"),
                                    data: None,
                                },
                            }),
                            reply = dispatch(&host, &state, &writer, &id, &method, &params, &child) => reply,
                        }
                    };
                    if let Some(reply) = reply {
                        crate::rpc::send(&writer, &reply).await;
                    }
                    state.lock().await.inflight.remove(&key);
                }
                .boxed(),
            )
        }
        Message::Notification { method, params } => {
            on_notification(host, &state, writer, &method, &params).await;
            None
        }
        Message::Result { id, result } => {
            complete_pending(&state, &id, ServerAnswer { result }).await;
            None
        }
        Message::Error { id, error } => {
            tracing::debug!(request = %crate::rpc::id_key(&id), code = error.code, "client error frame");
            fail_pending(&state, &id).await;
            None
        }
    }
}

/// Detects a JSON array frame without fully decoding it.
fn is_batch(line: &str) -> bool {
    line.trim_start().starts_with('[')
}

/// Handles one batch frame per the branch batch rules.
async fn on_batch(
    host: &Host,
    state: Arc<Mutex<AcpConn>>,
    writer: &FrameWriter,
    stop: &CancellationToken,
    line: &str,
) -> Option<futures::future::BoxFuture<'static, ()>> {
    let version = state.lock().await.version;
    let refusal = if version == AcpVersion::V1 {
        Err("batches are not supported".to_owned())
    } else {
        match sonic_rs::from_str::<Vec<Value>>(line) {
            Ok(items) if items.is_empty() => Err("batch is empty".to_owned()),
            Ok(items) => Ok(items),
            Err(error) => Err(format!("frame is not valid JSON: {error}")),
        }
    };
    let items = match refusal {
        Ok(items) => items,
        Err(message) => {
            let error = ErrorObject {
                code: -32600,
                message,
                data: None,
            };
            crate::rpc::send(
                writer,
                &Message::Error {
                    id: Id::Null,
                    error,
                },
            )
            .await;
            return None;
        }
    };
    let (host, writer, stop) = (host.clone(), writer.clone(), stop.clone());
    Some(
        async move {
            let mut replies = Vec::new();
            for item in items {
                batch_item(&host, &state, &writer, &stop, &item, &mut replies).await;
            }
            let frame = format!("[{}]", replies.join(","));
            if writer.write_frame(&frame).await.is_err() {
                tracing::debug!("batch reply write failed");
            }
        }
        .boxed(),
    )
}

/// Runs one batch member and appends its reply, if any.
async fn batch_item(
    host: &Host,
    state: &Arc<Mutex<AcpConn>>,
    writer: &FrameWriter,
    stop: &CancellationToken,
    item: &Value,
    replies: &mut Vec<String>,
) {
    let text = sonic_rs::to_string(item).unwrap_or_else(|_| "{}".to_owned());
    let message = match decode_jsonrpc(&text) {
        Ok(message) => message,
        Err(error) => {
            let error_object = ErrorObject {
                code: error.code,
                message: error.message,
                data: None,
            };
            push_reply(
                replies,
                &Message::Error {
                    id: error.id,
                    error: error_object,
                },
            );
            return;
        }
    };
    match message {
        Message::Request { id, method, .. } if is_lifecycle(&method) => {
            let error = ErrorObject {
                code: -32600,
                message: format!("{method} may not appear in a batch"),
                data: None,
            };
            push_reply(replies, &Message::Error { id, error });
        }
        Message::Request { id, method, params } => {
            let child = stop.child_token();
            if let Some(reply) =
                v2::dispatch(host, state, writer, &id, &method, &params, &child).await
            {
                push_reply(replies, &reply);
            }
        }
        Message::Notification { method, params } => {
            on_notification(host, state, writer, &method, &params).await;
        }
        _ => {}
    }
}

/// Encodes one batch reply, dropping encoding failures.
fn push_reply(replies: &mut Vec<String>, reply: &Message) {
    if let Ok(frame) = crate::jsonrpc::encode_jsonrpc(reply) {
        replies.push(frame);
    }
}

/// Returns true for v2 methods refused inside a batch.
fn is_lifecycle(method: &str) -> bool {
    matches!(
        method,
        "initialize" | "session/new" | "session/resume" | "session/prompt"
    )
}

/// Dispatches one request to the selected branch.
async fn dispatch(
    host: &Host,
    state: &Arc<Mutex<AcpConn>>,
    writer: &FrameWriter,
    id: &Id,
    method: &str,
    params: &Value,
    cancel: &CancellationToken,
) -> Option<Message> {
    if method == "initialize" {
        return initialize(state, writer, id, params).await;
    }
    let version = state.lock().await.version;
    match version {
        AcpVersion::V1 => v1::dispatch(host, state, writer, id, method, params, cancel).await,
        AcpVersion::V2 => v2::dispatch(host, state, writer, id, method, params, cancel).await,
    }
}

/// Handles client notifications: prompt/session cancel and request cancel.
async fn on_notification(
    host: &Host,
    state: &Arc<Mutex<AcpConn>>,
    writer: &FrameWriter,
    method: &str,
    params: &Value,
) {
    match method {
        "session/cancel" => cancel_session(host, state, writer, params).await,
        "$/cancel_request" => {
            let target = params
                .get("requestId")
                .and_then(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .or_else(|| value.as_i64().map(|number| number.to_string()))
                })
                .filter(|name| !name.is_empty());
            if let Some(target) = target {
                fail_pending_by_client(state, &target).await;
                if let Some(token) = state.lock().await.inflight.get(&target) {
                    token.cancel();
                }
            }
        }
        _ => tracing::debug!(method = %method, "dropping client notification"),
    }
}
