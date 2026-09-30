//! Shared connection state, the resumable reader, and reconnection.

use std::collections::{HashMap, VecDeque};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use dal_core::{Gen, Seq, SessionId, Update};
use sonic_rs::{JsonValueMutTrait, JsonValueTrait, Value};
use tokio::sync::Notify;

use super::decode::{cursor, host_update, protocol_error, session_update};
use super::{RemoteEndpoint, RemoteHostUpdate, backoff};
use crate::error::WireError;
use crate::jsonrpc::{Id, Message, decode_jsonrpc, encode_jsonrpc};
use crate::transport::{FrameWriter, LocalTransport, ReadFrameError, Transport};

/// One subscription to renew after a reconnect: session, fence, and cursor.
type Resubscribe = (SessionId, u64, Option<(Gen, Seq)>);

/// The in-progress reader step, kept across cancelled callers.
type Slot = Pin<Box<dyn Future<Output = Event> + Send>>;

/// The outcome of one reader step.
enum Event {
    /// One frame read, with the transport to keep reading.
    Frame(Transport, Result<String, ReadFrameError>),
    /// A new connection is initialized and resubscribed.
    Connected(Transport),
    /// Reconnection stopped on a non-transport error.
    Failed {
        /// The error to report to the driving caller.
        error: WireError,
        /// The next retry index.
        attempt: u32,
    },
}

/// One connection shared by a host and its agents.
pub(super) struct Shared {
    /// The endpoint to reconnect to.
    endpoint: RemoteEndpoint,
    /// The bearer token retained for authenticated reconnects.
    auth_token: Option<Arc<str>>,
    /// Request, subscription, and writer state.
    pub(super) core: Arc<Core>,
    /// The resumable reader step.
    slot: tokio::sync::Mutex<Slot>,
}

/// State reachable from the reader and from reconnection.
pub(super) struct Core {
    /// Mutable state; never held across an await.
    state: Mutex<State>,
    /// Woken after every state change.
    changed: Notify,
}

/// Mutable connection state.
pub(super) struct State {
    /// The next request id.
    next_id: i64,
    /// The live writer; `None` while disconnected.
    writer: Option<FrameWriter>,
    /// Outstanding requests and their replies once received.
    pending: HashMap<i64, Option<Result<Value, WireError>>>,
    /// Session subscriptions.
    pub(super) subs: HashMap<SessionId, SubState>,
    /// The host subscription queue, while one is active.
    pub(super) host_sub: Option<(u64, VecDeque<RemoteHostUpdate>)>,
    /// The next subscription fence.
    next_fence: u64,
}

/// One session subscription's local state.
pub(super) struct SubState {
    /// Identifies the handle that owns this slot.
    pub(super) fence: u64,
    /// The last accepted `(gen, seq)`; older updates are duplicates.
    pub(super) last: Option<(Gen, Seq)>,
    /// Accepted, undelivered updates.
    pub(super) queue: VecDeque<Arc<Update>>,
    /// A server `resync` awaits a view repaint.
    pub(super) resync: bool,
}

impl Shared {
    /// Connects once, without retrying, and initializes the connection.
    pub(super) async fn connect(
        endpoint: RemoteEndpoint,
        auth_token: Option<Arc<str>>,
    ) -> Result<Arc<Self>, WireError> {
        let core = Arc::new(Core {
            state: Mutex::new(State {
                next_id: 1,
                writer: None,
                pending: HashMap::new(),
                subs: HashMap::new(),
                host_sub: None,
                next_fence: 1,
            }),
            changed: Notify::new(),
        });
        let transport = establish(&core, &endpoint, auth_token.as_deref()).await?;
        Ok(Arc::new(Self {
            endpoint,
            auth_token,
            core,
            slot: tokio::sync::Mutex::new(read(transport)),
        }))
    }

    /// Waits until `ready` yields, driving the shared reader meanwhile.
    ///
    /// Cancel safe: an interrupted read or reconnect stays in the slot and
    /// the next waiter resumes it.
    pub(super) async fn wait<T>(
        &self,
        mut ready: impl FnMut(&mut State) -> Option<Result<T, WireError>>,
    ) -> Result<T, WireError> {
        loop {
            let notified = self.core.changed.notified();
            if let Some(outcome) = ready(&mut self.core.state()) {
                return outcome;
            }
            tokio::select! {
                biased;
                () = notified => {}
                mut slot = self.slot.lock() => self.step(&mut slot).await?,
            }
        }
    }

    /// Calls one method and waits for its reply.
    ///
    /// A request in flight when the connection drops fails with
    /// [`WireError::Transport`]; it is never re-sent, so server work is
    /// neither duplicated nor cancelled.
    pub(super) async fn call(&self, method: &str, params: Value) -> Result<Value, WireError> {
        let (id, writer) = self
            .wait(|state| {
                let writer = state.writer.clone()?;
                let id = state.next_id();
                state.pending.insert(id, None);
                Some(Ok((id, writer)))
            })
            .await?;
        let pending = PendingGuard {
            core: &self.core,
            id,
        };
        let frame = encode_jsonrpc(&Message::Request {
            id: Id::Integer(id),
            method: method.to_owned(),
            params,
        })?;
        writer.write_frame(&frame).await?;
        let reply = self
            .wait(|state| state.pending.get_mut(&id).and_then(Option::take))
            .await;
        drop(pending);
        reply
    }

    /// Runs one reader step: a frame, a lost connection, or a reconnect.
    async fn step(&self, slot: &mut Slot) -> Result<(), WireError> {
        match slot.as_mut().await {
            Event::Frame(transport, Ok(line)) => {
                *slot = read(transport);
                self.core.dispatch(&line)
            }
            Event::Frame(transport, Err(ReadFrameError::InvalidUtf8)) => {
                *slot = read(transport);
                Err(WireError::Protocol {
                    code: -32700,
                    message: "frame is not valid UTF-8".to_owned(),
                })
            }
            Event::Frame(transport, Err(error)) => {
                *slot = reconnect(
                    Arc::clone(&self.core),
                    self.endpoint.clone(),
                    self.auth_token.clone(),
                    0,
                );
                drop(transport);
                tracing::debug!(%error, "remote connection lost; reconnecting");
                self.core.lost();
                Ok(())
            }
            Event::Connected(transport) => {
                *slot = read(transport);
                Ok(())
            }
            Event::Failed { error, attempt } => {
                *slot = reconnect(
                    Arc::clone(&self.core),
                    self.endpoint.clone(),
                    self.auth_token.clone(),
                    attempt,
                );
                Err(error)
            }
        }
    }
}

/// Removes one pending request when its caller finishes or is cancelled.
struct PendingGuard<'a> {
    core: &'a Core,
    id: i64,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.core.state().pending.remove(&self.id);
    }
}

impl State {
    /// Allocates one request id.
    fn next_id(&mut self) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Allocates one subscription fence.
    pub(super) fn next_fence(&mut self) -> u64 {
        let fence = self.next_fence;
        self.next_fence += 1;
        fence
    }
}

impl Core {
    /// Locks the state, recovering it if a holder panicked.
    pub(super) fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Wakes every waiter after a state change.
    pub(super) fn wake(&self) {
        self.changed.notify_waiters();
    }

    /// Fails outstanding requests and clears the writer.
    fn lost(&self) {
        let mut state = self.state();
        state.writer = None;
        for reply in state.pending.values_mut() {
            if reply.is_none() {
                *reply = Some(Err(WireError::Transport(
                    "remote connection was lost before the reply".to_owned(),
                )));
            }
        }
        drop(state);
        self.wake();
    }

    /// Decodes and applies one server frame.
    fn dispatch(&self, line: &str) -> Result<(), WireError> {
        let message = decode_jsonrpc(line).map_err(|error| WireError::Protocol {
            code: error.code,
            message: error.message,
        })?;
        let outcome = self.apply(message);
        self.wake();
        outcome
    }

    /// Applies one decoded server message.
    fn apply(&self, message: Message) -> Result<(), WireError> {
        match message {
            Message::Result {
                id: Id::Integer(id),
                result,
            } => {
                self.reply(id, Ok(result));
                Ok(())
            }
            Message::Error {
                id: Id::Integer(id),
                error,
            } => {
                self.reply(id, Err(protocol_error(error)));
                Ok(())
            }
            Message::Error { id, error } => Err(WireError::Protocol {
                code: error.code,
                message: format!("server rejected a frame (id {id:?}): {}", error.message),
            }),
            Message::Notification { method, params } if method == "session/update" => {
                self.on_session_update(&params)
            }
            Message::Notification { method, params } if method == "host/update" => {
                self.on_host_update(&params)
            }
            Message::Notification { method, .. } => {
                tracing::debug!(%method, "ignoring unknown remote notification");
                Ok(())
            }
            Message::Result { id, .. } => {
                tracing::debug!(?id, "ignoring remote reply with a foreign id");
                Ok(())
            }
            Message::Request { method, .. } => Err(WireError::Protocol {
                code: -32600,
                message: format!("server sent request \"{method}\"; version 1 has none"),
            }),
        }
    }

    /// Stores one reply for its waiting caller.
    fn reply(&self, id: i64, reply: Result<Value, WireError>) {
        let mut state = self.state();
        if let Some(slot) = state.pending.get_mut(&id) {
            *slot = Some(reply);
        } else {
            tracing::debug!(id, "dropping reply for an abandoned request");
        }
    }

    /// Accepts one `session/update`, dropping duplicates by `(gen, seq)`.
    fn on_session_update(&self, params: &Value) -> Result<(), WireError> {
        let (session, pair, kind) = session_update(params)?;
        let mut state = self.state();
        let Some(sub) = state.subs.get_mut(&session) else {
            return Ok(());
        };
        let Some(kind) = kind else {
            sub.queue.clear();
            sub.resync = true;
            sub.last = sub.last.max(Some(pair));
            return Ok(());
        };
        if sub.last.is_some_and(|last| pair <= last) {
            return Ok(());
        }
        sub.last = Some(pair);
        sub.queue.push_back(Arc::new(Update {
            r#gen: pair.0,
            seq: pair.1,
            kind,
        }));
        Ok(())
    }

    /// Queues one `host/update` for the active host subscription.
    fn on_host_update(&self, params: &Value) -> Result<(), WireError> {
        let Some(update) = host_update(params)? else {
            return Ok(());
        };
        if let Some((_, queue)) = self.state().host_sub.as_mut() {
            queue.push_back(update);
        }
        Ok(())
    }

    /// Records a subscribe reply; a live-only subscription starts at its head.
    pub(super) fn subscribed(
        &self,
        session: SessionId,
        fence: u64,
        result: &Value,
    ) -> Result<(), WireError> {
        let head = cursor(result)?;
        if let Some(sub) = self.state().subs.get_mut(&session)
            && sub.fence == fence
            && sub.last.is_none()
        {
            sub.last = Some(head);
        }
        Ok(())
    }
}

/// Builds `session/subscribe` params resuming after one cursor.
pub(super) fn subscribe_params(session: SessionId, after: Option<(Gen, Seq)>) -> Value {
    let mut params = sonic_rs::json!({"sessionId": session.to_string()});
    if let Some((generation, seq)) = after
        && let Some(object) = params.as_object_mut()
    {
        object.insert("gen", Value::from(generation.get()));
        object.insert("after", Value::from(seq.get()));
    }
    params
}

/// Starts reading the next frame.
fn read(mut transport: Transport) -> Slot {
    Box::pin(async move {
        let frame = transport.read_frame().await;
        Event::Frame(transport, frame)
    })
}

/// Reconnects with backoff until connected or a non-transport failure.
fn reconnect(
    core: Arc<Core>,
    endpoint: RemoteEndpoint,
    auth_token: Option<Arc<str>>,
    attempt: u32,
) -> Slot {
    Box::pin(async move {
        let mut attempt = attempt;
        loop {
            tokio::time::sleep(backoff(attempt)).await;
            attempt = attempt.saturating_add(1);
            match establish(&core, &endpoint, auth_token.as_deref()).await {
                Ok(transport) => return Event::Connected(transport),
                Err(WireError::Transport(message)) => {
                    tracing::debug!(%message, attempt, "remote reconnect failed");
                }
                Err(error) => return Event::Failed { error, attempt },
            }
        }
    })
}

/// Opens, initializes, and resubscribes one connection, then publishes its writer.
async fn establish(
    core: &Core,
    endpoint: &RemoteEndpoint,
    auth_token: Option<&str>,
) -> Result<Transport, WireError> {
    let mut transport = open_endpoint(endpoint, auth_token).await?;
    let capabilities: Vec<Value> = crate::protocol::CAPABILITIES
        .iter()
        .map(|name| Value::from(*name))
        .collect();
    let params = sonic_rs::json!({
        "protocolVersion": 1,
        "clientInfo": {"name": "dal-remotehost", "version": crate::rpc::crate_version()},
        "capabilities": capabilities,
    });
    let result = request(core, &mut transport, "initialize", params).await?;
    let version = result
        .get("protocolVersion")
        .and_then(JsonValueTrait::as_i64);
    if version != Some(1) {
        return Err(WireError::Protocol {
            code: -32600,
            message: format!("server negotiated protocol version {version:?}, expected 1"),
        });
    }
    let cursors: Vec<Resubscribe> = core
        .state()
        .subs
        .iter()
        .map(|(session, sub)| (*session, sub.fence, sub.last))
        .collect();
    for (session, fence, last) in cursors {
        let params = subscribe_params(session, last);
        match request(core, &mut transport, "session/subscribe", params).await {
            Ok(result) => core.subscribed(session, fence, &result)?,
            Err(WireError::Protocol { code, message }) => {
                tracing::debug!(code, %message, "resubscribe refused; repainting");
                if let Some(sub) = core.state().subs.get_mut(&session)
                    && sub.fence == fence
                {
                    sub.queue.clear();
                    sub.resync = true;
                }
            }
            Err(error) => return Err(error),
        }
    }
    if core.state().host_sub.is_some() {
        request(core, &mut transport, "host/subscribe", sonic_rs::json!({})).await?;
        if let Some((_, queue)) = core.state().host_sub.as_mut() {
            queue.push_back(RemoteHostUpdate::Reconnected);
        }
    }
    core.state().writer = Some(transport.writer());
    core.wake();
    Ok(transport)
}

/// Sends one request on a connection being established and reads its reply,
/// applying interleaved notifications.
async fn request(
    core: &Core,
    transport: &mut Transport,
    method: &str,
    params: Value,
) -> Result<Value, WireError> {
    let id = core.state().next_id();
    let frame = encode_jsonrpc(&Message::Request {
        id: Id::Integer(id),
        method: method.to_owned(),
        params,
    })?;
    transport.write_frame(&frame).await?;
    loop {
        let line = transport
            .read_frame()
            .await
            .map_err(|error| WireError::Transport(format!("remote connection failed: {error}")))?;
        let message = decode_jsonrpc(&line).map_err(|error| WireError::Protocol {
            code: error.code,
            message: error.message,
        })?;
        match message {
            Message::Result {
                id: Id::Integer(reply),
                result,
            } if reply == id => return Ok(result),
            Message::Error {
                id: Id::Integer(reply),
                error,
            } if reply == id => return Err(protocol_error(error)),
            other => {
                let outcome = core.apply(other);
                core.wake();
                outcome?;
            }
        }
    }
}

/// Opens one endpoint transport.
async fn open_endpoint(
    endpoint: &RemoteEndpoint,
    auth_token: Option<&str>,
) -> Result<Transport, WireError> {
    match endpoint {
        RemoteEndpoint::LocalSocket(path) if auth_token.is_none() => open_local(path).await,
        RemoteEndpoint::LocalSocket(_) => Err(WireError::Transport(
            "bearer authentication requires a WebSocket endpoint".to_owned(),
        )),
        RemoteEndpoint::WebSocket(url) => match auth_token {
            Some(token) => {
                crate::transport::WebSocketTransport::connect_with_auth(url.as_str(), token)
                    .await
                    .map(Transport::websocket)
            }
            None => crate::transport::WebSocketTransport::connect(url.clone())
                .await
                .map(Transport::websocket),
        },
    }
}

/// Opens one local-socket client transport.
#[cfg(unix)]
async fn open_local(path: &std::path::Path) -> Result<Transport, WireError> {
    let stream = tokio::net::UnixStream::connect(path)
        .await
        .map_err(|error| {
            WireError::Transport(format!(
                "local socket {} connect failed: {error}",
                path.display()
            ))
        })?;
    let (reader, writer) = stream.into_split();
    Ok(Transport::Local(LocalTransport::new(reader, writer)))
}

/// Opens one local-socket client transport (unsupported outside unix).
#[cfg(not(unix))]
async fn open_local(path: &std::path::Path) -> Result<Transport, WireError> {
    Err(WireError::Transport(format!(
        "local socket clients are not supported on this platform: {}",
        path.display()
    )))
}
