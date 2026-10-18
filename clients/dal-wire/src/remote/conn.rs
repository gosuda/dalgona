//! Shared connection state, the resumable reader, and reconnection.

use dal_core::{Gen, Seq, SessionId, Update};
use sonic_rs::{JsonValueMutTrait, JsonValueTrait, Value};
use std::collections::{HashMap, VecDeque};
use std::ops::Deref;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use tokio::sync::{Notify, watch};
use tokio::task::JoinSet;

use super::decode::{cursor, host_update, protocol_error, session_update};
use super::{RemoteEndpoint, RemoteHostUpdate, backoff};
use crate::error::WireError;
use crate::jsonrpc::{Id, Message, decode_jsonrpc, encode_jsonrpc};
#[cfg(unix)]
use crate::transport::LocalTransport;
use crate::transport::{FrameWriter, ReadFrameError, Transport};

/// One subscription to renew after a reconnect: session, fence, and cursor.
type Resubscribe = (SessionId, u64, Option<(Gen, Seq)>);

/// One in-progress reader future.
type Slot = Pin<Box<dyn Future<Output = Event> + Send>>;

/// The resumable reader step, kept across cancelled callers.
enum Reader {
    /// Reading the next frame of one transport.
    Reading {
        /// The epoch of the transport being read.
        epoch: u64,
        /// The pending read.
        frame: Slot,
    },
    /// Opening and initializing a replacement transport.
    Connecting(Slot),
}

/// The outcome of one reader step.
enum Event {
    /// One frame read, with the transport to keep reading.
    Frame {
        /// The epoch of the transport the frame was read from.
        epoch: u64,
        /// The transport to keep reading.
        transport: Transport,
        /// The frame or the read failure.
        frame: Result<String, ReadFrameError>,
    },
    /// A new connection is initialized, resubscribed, and published.
    Connected {
        /// The epoch the connection was published at.
        epoch: u64,
        /// The transport to read.
        transport: Transport,
    },
    /// Reconnection stopped on a non-transport error.
    Failed {
        /// The error to report to the driving caller.
        error: WireError,
        /// The next retry index.
        attempt: u32,
    },
}

/// One connection shared by a host and its agents.
///
/// Owns the connection plus the cleanup tasks released by dropped
/// subscriptions. Tasks hold `Connection`, never `Shared`, so dropping the
/// last handle aborts them through `unsub_tasks` even mid-send.
pub(super) struct Shared {
    /// The connection and its serials.
    conn: Arc<Connection>,
    /// Tracked unsubscribe tasks spawned from subscription drops.
    unsub_tasks: Mutex<JoinSet<()>>,
    /// Runtime captured at connect; lets drops on plain threads schedule
    /// cleanup without depending on thread-local runtime context.
    handle: tokio::runtime::Handle,
}

/// The endpoint, reader, and subscription state one connection serves.
pub(super) struct Connection {
    /// The endpoint to reconnect to.
    endpoint: RemoteEndpoint,
    /// The bearer token retained for authenticated reconnects.
    auth_token: Option<Arc<str>>,
    /// Request, subscription, and writer state.
    pub(super) core: Arc<Core>,
    /// The resumable reader step.
    reader: tokio::sync::Mutex<Reader>,
    /// Serializes session subscribe and unsubscribe sends. Unrelated RPCs
    /// never take it; only subscription lifecycle ops do.
    pub(super) sub_serial: tokio::sync::Mutex<()>,
}

impl Deref for Shared {
    type Target = Connection;
    fn deref(&self) -> &Self::Target {
        &self.conn
    }
}

/// State reachable from the reader and from reconnection.
pub(super) struct Core {
    /// Mutable state; never held across an await.
    state: Mutex<State>,
    /// Woken after every state change.
    changed: Notify,
    /// Mirrors the epoch of [`State::link`]. Sent only while the state lock
    /// is held, so a reader parked on a retired transport wakes without
    /// anyone taking the reader.
    epoch: watch::Sender<u64>,
}

/// The transport requests are written on.
enum Link {
    /// One transport is open.
    Live {
        /// The epoch of this transport.
        epoch: u64,
        /// Its writer.
        writer: FrameWriter,
    },
    /// No transport is open.
    Down {
        /// The epoch of this disconnected state.
        epoch: u64,
    },
}

impl Link {
    /// Returns the epoch of this state; every transition advances it by one.
    fn epoch(&self) -> u64 {
        match self {
            Self::Live { epoch, .. } | Self::Down { epoch } => *epoch,
        }
    }
}

/// A request id reserved on one live transport.
struct Grant<'a> {
    /// The epoch of the transport the request rides on.
    epoch: u64,
    /// That transport's writer.
    writer: FrameWriter,
    /// Removes the reserved request once its caller is done.
    pending: PendingGuard<'a>,
}

/// Mutable connection state.
pub(super) struct State {
    /// The next request id.
    next_id: i64,
    /// The transport requests are written on.
    link: Link,
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
        Ok(Arc::new(Self {
            conn: Connection::connect(endpoint, auth_token).await?,
            unsub_tasks: Mutex::new(JoinSet::new()),
            handle: tokio::runtime::Handle::current(),
        }))
    }

    /// Spawns a tracked release for a dropped current subscription.
    ///
    /// The task holds `Connection`, never `Shared`: dropping the last
    /// handle drops `unsub_tasks`, aborting the task even mid-send.
    /// Scheduling uses the handle retained at connect, so drops on plain
    /// threads (`spawn_blocking`, std threads) still release the server pump.
    pub(super) fn spawn_unsubscribe(self: &Arc<Self>, session: SessionId) {
        let conn = Arc::clone(&self.conn);
        let mut tasks = self
            .unsub_tasks
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        while tasks.try_join_next().is_some() {}
        tasks.spawn_on(unsubscribe_task(conn, session), &self.handle);
    }
}

impl Connection {
    /// Connects once, without retrying, and initializes the connection.
    pub(super) async fn connect(
        endpoint: RemoteEndpoint,
        auth_token: Option<Arc<str>>,
    ) -> Result<Arc<Self>, WireError> {
        let (epoch, _) = watch::channel(0);
        let core = Arc::new(Core {
            state: Mutex::new(State {
                next_id: 1,
                link: Link::Down { epoch: 0 },
                pending: HashMap::new(),
                subs: HashMap::new(),
                host_sub: None,
                next_fence: 1,
            }),
            changed: Notify::new(),
            epoch,
        });
        let (epoch, transport) = establish(&core, &endpoint, auth_token.as_deref()).await?;
        Ok(Arc::new(Self {
            endpoint,
            auth_token,
            core,
            reader: tokio::sync::Mutex::new(read(epoch, transport)),
            sub_serial: tokio::sync::Mutex::new(()),
        }))
    }

    /// Removes the subscription slot only when `fence` still owns it.
    ///
    /// Returns true when the caller takes over server-side release. The
    /// guard never crosses an await: the check and removal are one critical
    /// section on the state mutex.
    pub(super) fn release(&self, session: SessionId, fence: u64) -> bool {
        let mut state = self.core.state();
        if state
            .subs
            .get(&session)
            .is_some_and(|sub| sub.fence == fence)
        {
            state.subs.remove(&session);
            true
        } else {
            false
        }
    }

    /// Closes transport `epoch` after a refused release, unless it is
    /// already gone.
    ///
    /// The server keeps answering a session until it sees the transport
    /// close, so a refusal leaves the pump state unknown and the transport is
    /// torn down; `establish` then resubscribes the still-live sessions from
    /// their cursors. Retiring is a no-op once `epoch` is no longer live, so
    /// a cleanup that lost a race to a replacement never touches it.
    ///
    /// Never waits for the reader: a step parked on `epoch` observes the
    /// retirement through the epoch watch and drops the transport itself;
    /// an idle reader is replaced here.
    fn invalidate(&self, epoch: u64) {
        if !self.core.retire(epoch) {
            return;
        }
        if let Ok(mut reader) = self.reader.try_lock()
            && matches!(*reader, Reader::Reading { epoch: reading, .. } if reading == epoch)
        {
            *reader = self.reconnecting(0);
        }
    }

    /// Waits until `ready` yields, driving the shared reader meanwhile.
    ///
    /// Cancel safe: an interrupted read or reconnect stays in the reader and
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
                mut reader = self.reader.lock() => self.step(&mut reader).await?,
            }
        }
    }

    /// Calls one method and waits for its reply.
    ///
    /// A request in flight when the connection drops fails with
    /// [`WireError::Transport`]; it is never re-sent, so server work is
    /// neither duplicated nor cancelled.
    pub(super) async fn call(&self, method: &str, params: Value) -> Result<Value, WireError> {
        let grant = self.grant().await?;
        self.send(grant, method, params).await
    }

    /// Reserves a request id on the live transport, waiting for one while
    /// disconnected.
    async fn grant(&self) -> Result<Grant<'_>, WireError> {
        let (epoch, writer, id) = self
            .wait(|state| {
                let Link::Live { epoch, writer } = &state.link else {
                    return None;
                };
                let (epoch, writer) = (*epoch, writer.clone());
                let id = state.next_id();
                state.pending.insert(id, None);
                Some(Ok((epoch, writer, id)))
            })
            .await?;
        Ok(Grant {
            epoch,
            writer,
            pending: PendingGuard {
                core: &self.core,
                id,
            },
        })
    }

    /// Sends one request on its granted transport and waits for the reply.
    async fn send(
        &self,
        grant: Grant<'_>,
        method: &str,
        params: Value,
    ) -> Result<Value, WireError> {
        let Grant {
            writer, pending, ..
        } = grant;
        let id = pending.id;
        let frame = encode_jsonrpc(&Message::Request {
            id: Id::Integer(id),
            method: method.to_owned(),
            params,
        })?;
        writer.write_frame(&frame).await?;
        drop(writer);
        let reply = self
            .wait(|state| state.pending.get_mut(&id).and_then(Option::take))
            .await;
        drop(pending);
        reply
    }

    /// Runs one reader step: a frame, a reconnect, or the retirement of the
    /// transport being read.
    ///
    /// Cancel safe: an unfinished future stays in `reader`, and a finished
    /// one is always replaced before this returns.
    async fn step(&self, reader: &mut Reader) -> Result<(), WireError> {
        let event = match reader {
            Reader::Reading { epoch, frame } => {
                let epoch = *epoch;
                let mut live = self.core.epoch.subscribe();
                tokio::select! {
                    biased;
                    _ = live.wait_for(|current| *current != epoch) => None,
                    event = frame.as_mut() => Some(event),
                }
            }
            Reader::Connecting(connect) => Some(connect.as_mut().await),
        };
        if let Some(event) = event {
            self.advance(reader, event)
        } else {
            *reader = self.reconnecting(0);
            Ok(())
        }
    }

    /// Applies one finished reader future, replacing it in every case.
    ///
    /// Outcomes of a retired epoch are dropped unread: their transport is
    /// gone and `establish` resumes from the surviving cursors.
    fn advance(&self, reader: &mut Reader, event: Event) -> Result<(), WireError> {
        match event {
            Event::Frame {
                epoch,
                transport,
                frame: Ok(line),
            } if self.core.is_live(epoch) => {
                *reader = read(epoch, transport);
                self.core.dispatch(&line)
            }
            Event::Frame {
                epoch,
                transport,
                frame: Err(ReadFrameError::InvalidUtf8),
            } if self.core.is_live(epoch) => {
                *reader = read(epoch, transport);
                Err(WireError::Protocol {
                    code: -32700,
                    message: "frame is not valid UTF-8".to_owned(),
                })
            }
            Event::Frame {
                epoch,
                transport,
                frame,
            } => {
                drop(transport);
                if let Err(error) = frame {
                    tracing::debug!(%error, "remote connection lost; reconnecting");
                }
                self.core.retire(epoch);
                *reader = self.reconnecting(0);
                Ok(())
            }
            Event::Connected { epoch, transport } => {
                *reader = if self.core.is_live(epoch) {
                    read(epoch, transport)
                } else {
                    self.reconnecting(0)
                };
                Ok(())
            }
            Event::Failed { error, attempt } => {
                *reader = self.reconnecting(attempt);
                Err(error)
            }
        }
    }

    /// Starts reconnecting from retry index `attempt`.
    fn reconnecting(&self, attempt: u32) -> Reader {
        Reader::Connecting(reconnect(
            Arc::clone(&self.core),
            self.endpoint.clone(),
            self.auth_token.clone(),
            attempt,
        ))
    }
}

/// Releases the server pump for a dropped handle unless a successor arrived.
///
/// Holds `Connection`, never `Shared`, so dropping the connection aborts
/// this task through its owning `JoinSet` even mid-send: no owner cycle.
/// The serial mutually excludes the check+send pairs of subscribe,
/// unsubscribe, and repaint, so a stale send can never land after a
/// successor's subscribe. Any slot entry means a live successor, since
/// fences are never reused.
async fn unsubscribe_task(conn: Arc<Connection>, session: SessionId) {
    let _serial = conn.sub_serial.lock().await;
    if conn.core.state().subs.contains_key(&session) {
        return;
    }
    let grant = match conn.grant().await {
        Ok(grant) => grant,
        Err(error) => {
            tracing::debug!(%error, "dropped subscription release could not reserve a request");
            return;
        }
    };
    // A refusal describes only the transport the request rode.
    let epoch = grant.epoch;
    let params = sonic_rs::json!({"sessionId": session.to_string()});
    // The server answers Ok({}) even for unknown sessions (rpc/session.rs
    // removes and cancels when present, no-ops otherwise), so no error code
    // means already-absent: every error leaves the pump state unknown and
    // invalidates through the existing loss path instead of silently keeping
    // a dead server subscription.
    if let Err(error) = conn.send(grant, "session/unsubscribe", params).await {
        tracing::debug!(%error, "dropped subscription release failed; invalidating connection");
        conn.invalidate(epoch);
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

    /// Returns whether transport `epoch` is the live one.
    fn is_live(&self, epoch: u64) -> bool {
        matches!(self.state().link, Link::Live { epoch: live, .. } if live == epoch)
    }

    /// Retires live transport `epoch` in one critical section: drops its
    /// writer, fails its outstanding requests, and advances the epoch watch
    /// so a step parked on it wakes.
    ///
    /// Returns false, touching nothing, when `epoch` is not live.
    fn retire(&self, epoch: u64) -> bool {
        let mut state = self.state();
        if !matches!(state.link, Link::Live { epoch: live, .. } if live == epoch) {
            return false;
        }
        self.relink(&mut state, Link::Down { epoch: epoch + 1 });
        for reply in state.pending.values_mut() {
            if reply.is_none() {
                *reply = Some(Err(WireError::Transport(
                    "remote connection was lost before the reply".to_owned(),
                )));
            }
        }
        drop(state);
        self.wake();
        true
    }

    /// Replaces the link and publishes its epoch under the held state lock,
    /// so the watch never disagrees with the state it mirrors.
    fn relink(&self, state: &mut State, link: Link) {
        let epoch = link.epoch();
        state.link = link;
        self.epoch.send_replace(epoch);
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

/// Starts reading the next frame of transport `epoch`.
fn read(epoch: u64, mut transport: Transport) -> Reader {
    Reader::Reading {
        epoch,
        frame: Box::pin(async move {
            let frame = transport.read_frame().await;
            Event::Frame {
                epoch,
                transport,
                frame,
            }
        }),
    }
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
                Ok((epoch, transport)) => return Event::Connected { epoch, transport },
                Err(WireError::Transport(message)) => {
                    tracing::debug!(%message, attempt, "remote reconnect failed");
                }
                Err(error) => return Event::Failed { error, attempt },
            }
        }
    })
}

/// Opens, initializes, and resubscribes one connection, then publishes it
/// at the next epoch.
async fn establish(
    core: &Core,
    endpoint: &RemoteEndpoint,
    auth_token: Option<&str>,
) -> Result<(u64, Transport), WireError> {
    let mut transport = open_endpoint(endpoint, auth_token).await?;
    let capabilities: Vec<Value> = crate::protocol::CAPABILITIES
        .iter()
        .map(|name| Value::from(*name))
        // The remote connection serves terminal front ends, which declare
        // both answerer roles. The names stay out of `CAPABILITIES`: the
        // server ignores them for method gating and reads them only for
        // answerer selection.
        .chain(["approval", "ask"].map(Value::from))
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
    let mut state = core.state();
    let epoch = state.link.epoch() + 1;
    core.relink(
        &mut state,
        Link::Live {
            epoch,
            writer: transport.writer(),
        },
    );
    drop(state);
    core.wake();
    Ok((epoch, transport))
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
fn open_local(path: &std::path::Path) -> std::future::Ready<Result<Transport, WireError>> {
    std::future::ready(Err(WireError::Transport(format!(
        "local socket clients are not supported on this platform: {}",
        path.display()
    ))))
}

#[cfg(all(test, unix))]
mod tests {
    use std::collections::VecDeque;
    use std::future::poll_fn;
    use std::num::NonZeroU64;
    use std::pin::pin;
    use std::sync::Arc;
    use std::task::Poll;
    use std::time::Duration;

    use dal_core::{Gen, Seq, SessionId, Update};
    use sonic_rs::{JsonValueTrait, Value};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;
    use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

    use super::{Connection, SubState, unsubscribe_task};
    use crate::error::WireError;
    use crate::remote::RemoteEndpoint;

    const DROPPED: &str = "018f0f62-3b00-7000-8000-0000000000d1";
    const WAITING: &str = "018f0f62-3b00-7000-8000-0000000000d2";

    /// The scripted server end of one client connection.
    struct Peer {
        reader: BufReader<OwnedReadHalf>,
        writer: OwnedWriteHalf,
    }

    impl Peer {
        /// Accepts one client and answers its `initialize`.
        async fn initialized(listener: &UnixListener) -> std::io::Result<Self> {
            let (stream, _) = listener.accept().await?;
            let (reader, writer) = stream.into_split();
            let mut peer = Self {
                reader: BufReader::new(reader),
                writer,
            };
            let (id, _) = peer.expect("initialize").await?;
            peer.reply(
                id,
                sonic_rs::json!({
                    "protocolVersion": 1,
                    "serverInfo": {"name": "dal", "version": "0"},
                    "capabilities": [],
                    "clientId": "conn-test#1",
                }),
            )
            .await?;
            Ok(peer)
        }

        /// Reads the next client frame; `None` once the client closed.
        async fn frame(&mut self) -> std::io::Result<Option<Value>> {
            let mut line = String::new();
            let read = self.reader.read_line(&mut line).await?;
            if read == 0 {
                return Ok(None);
            }
            sonic_rs::from_str(&line)
                .map(Some)
                .map_err(std::io::Error::other)
        }

        async fn expect(&mut self, method: &str) -> std::io::Result<(Value, Value)> {
            let frame = self
                .frame()
                .await?
                .ok_or_else(|| std::io::Error::other("client closed the connection"))?;
            assert_eq!(
                frame.get("method").and_then(|m| m.as_str()),
                Some(method),
                "unexpected frame: {frame}"
            );
            let id = frame
                .get("id")
                .cloned()
                .ok_or_else(|| std::io::Error::other("request has an id"))?;
            let params = frame.get("params").cloned().unwrap_or_default();
            Ok((id, params))
        }

        async fn send(&mut self, frame: &Value) -> std::io::Result<()> {
            let mut text = sonic_rs::to_string(frame).map_err(std::io::Error::other)?;
            text.push('\n');
            self.writer.write_all(text.as_bytes()).await?;
            Ok(())
        }

        async fn reply(&mut self, id: Value, result: Value) -> std::io::Result<()> {
            self.send(&sonic_rs::json!({"jsonrpc": "2.0", "id": id, "result": result}))
                .await?;
            Ok(())
        }

        /// Confirms the resubscribe of `session` after `(1, after)`.
        async fn resubscribed(&mut self, session: &str, after: u64) -> std::io::Result<()> {
            let (id, params) = self.expect("session/subscribe").await?;
            assert_eq!(
                params.get("sessionId").and_then(|s| s.as_str()),
                Some(session)
            );
            assert_eq!(
                params.get("after").and_then(JsonValueTrait::as_u64),
                Some(after)
            );
            self.reply(id, sonic_rs::json!({"gen": 1, "seq": after}))
                .await?;
            Ok(())
        }

        async fn update(&mut self, session: &str, seq: u64) -> std::io::Result<()> {
            self.send(&sonic_rs::json!({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": session,
                    "gen": 1,
                    "seq": seq,
                    "update": {"type": "remote_test_marker"},
                },
            }))
            .await?;
            Ok(())
        }
    }

    fn bind(dir: &std::path::Path) -> std::io::Result<(UnixListener, RemoteEndpoint)> {
        let path = dir.join("rpc.sock");
        let listener = UnixListener::bind(&path)?;
        Ok((listener, RemoteEndpoint::LocalSocket(path)))
    }

    fn session(id: &str) -> Result<SessionId, dal_core::IdError> {
        SessionId::parse(id)
    }

    /// Registers a live local subscription to `id` resuming after `(1, seq)`.
    fn subscribe(conn: &Connection, id: SessionId, seq: NonZeroU64) {
        let mut state = conn.core.state();
        let fence = state.next_fence();
        let last = (Gen::new(NonZeroU64::MIN), Seq::new(seq));
        state.subs.insert(
            id,
            SubState {
                fence,
                last: Some(last),
                queue: VecDeque::new(),
                resync: false,
            },
        );
    }

    /// Waits for the next update queued for `id`, driving the shared reader.
    async fn next(
        conn: &Connection,
        id: SessionId,
    ) -> Result<Arc<Update>, crate::error::WireError> {
        conn.wait(|state| {
            state
                .subs
                .get_mut(&id)
                .and_then(|sub| sub.queue.pop_front())
                .map(Ok)
        })
        .await
    }

    fn position(update: &Update) -> (u64, u64) {
        (update.r#gen.get(), update.seq.get())
    }

    /// A refused release closes the transport it rode even while another
    /// waiter holds the reader parked on that quiet transport.
    ///
    /// Every wake polls the waiter before the cleanup, so the reader is
    /// handed back to the waiter between the refusal and the teardown and the
    /// teardown cannot take it. Both futures are polled on every wake.
    #[tokio::test]
    async fn refused_release_closes_the_transport_under_a_parked_reader() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (listener, endpoint) = bind(dir.path()).expect("bind socket");
        let server = async {
            let mut first = Peer::initialized(&listener).await.expect("accept client");
            let (id, params) = first
                .expect("session/unsubscribe")
                .await
                .expect("unsubscribe request");
            assert_eq!(
                params.get("sessionId").and_then(|s| s.as_str()),
                Some(DROPPED)
            );
            first
                .send(&sonic_rs::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32601, "message": "method not found"},
                }))
                .await
                .expect("write frame");
            assert!(
                first.frame().await.expect("read frame").is_none(),
                "the refused transport must close"
            );
            let mut second = Peer::initialized(&listener).await.expect("accept client");
            second.resubscribed(WAITING, 5).await.expect("resubscribe");
            second.update(WAITING, 6).await.expect("update");
            second
        };
        let client = async {
            let conn = Connection::connect(endpoint, None).await.expect("connect");
            let waiting = session(WAITING).expect("session id");
            subscribe(&conn, waiting, NonZeroU64::new(5).expect("nonzero seq"));
            let mut waiter = pin!(next(&conn, waiting));
            let mut cleanup = pin!(unsubscribe_task(
                Arc::clone(&conn),
                session(DROPPED).expect("session id")
            ));
            let mut released = false;
            let update = poll_fn(|cx| {
                if let Poll::Ready(update) = waiter.as_mut().poll(cx) {
                    return Poll::Ready(update);
                }
                if !released {
                    released = cleanup.as_mut().poll(cx).is_ready();
                }
                Poll::Pending
            })
            .await
            .expect("update");
            assert!(released, "the refused release finishes");
            assert_eq!(position(&update), (1, 6));
        };
        tokio::time::timeout(
            Duration::from_secs(10),
            Box::pin(async { tokio::join!(server, client) }),
        )
        .await
        .expect("the refused release repair finishes");
    }

    /// A cleanup whose transport was already replaced leaves the successor
    /// alone: its outstanding request is answered and its later updates
    /// arrive on the same transport.
    #[tokio::test]
    async fn stale_invalidation_spares_the_successor_transport() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (listener, endpoint) = bind(dir.path()).expect("bind socket");
        let server = async {
            drop(Peer::initialized(&listener).await.expect("accept client"));
            let mut second = Peer::initialized(&listener).await.expect("accept client");
            second.resubscribed(WAITING, 5).await.expect("resubscribe");
            second.update(WAITING, 6).await.expect("update");
            let (id, _) = second.expect("session/list").await.expect("list request");
            second
                .reply(id, sonic_rs::json!({"sessions": []}))
                .await
                .expect("reply");
            second.update(WAITING, 7).await.expect("update");
            second
        };
        let client = async {
            let conn = Connection::connect(endpoint, None).await.expect("connect");
            let waiting = session(WAITING).expect("session id");
            subscribe(&conn, waiting, NonZeroU64::new(5).expect("nonzero seq"));
            // Captured from a real grant on the first transport, before any
            // reader step can observe its close.
            let stale = conn.grant().await.expect("grant").epoch;
            assert_eq!(
                position(next(&conn, waiting).await.expect("update").as_ref()),
                (1, 6)
            );
            let mut call = pin!(conn.call("session/list", sonic_rs::json!({})));
            let parked = poll_fn(|cx| Poll::Ready(call.as_mut().poll(cx).is_pending())).await;
            assert!(parked, "the successor request awaits its reply");
            conn.invalidate(stale);
            let reply = call.await.expect("the successor reply survives");
            assert!(reply.get("sessions").is_some());
            assert_eq!(
                position(next(&conn, waiting).await.expect("update").as_ref()),
                (1, 7)
            );
        };
        tokio::time::timeout(
            Duration::from_secs(10),
            Box::pin(async { tokio::join!(server, client) }),
        )
        .await
        .expect("the stale cleanup finishes");
    }

    /// A `401` upgrade denial over real loopback ends `reconnect` with `Auth`
    /// instead of retrying forever.
    #[tokio::test]
    async fn reconnect_stops_on_auth_via_loopback() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let address = listener.local_addr().expect("listener address");
        let serve = async {
            let (mut stream, _) = listener.accept().await.expect("accept client");
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            loop {
                let read = stream.read(&mut chunk).await.expect("read request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
                if request.len() > 16_384 {
                    break;
                }
            }
            stream
                .write_all(
                    b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("deny upgrade");
        };
        let url = url::Url::parse(&format!("ws://{address}/")).expect("socket url");
        let endpoint = RemoteEndpoint::WebSocket(url);
        let (epoch, _) = tokio::sync::watch::channel(0);
        let core = Arc::new(super::Core {
            state: std::sync::Mutex::new(super::State {
                next_id: 1,
                link: super::Link::Down { epoch: 0 },
                pending: std::collections::HashMap::new(),
                subs: std::collections::HashMap::new(),
                host_sub: None,
                next_fence: 1,
            }),
            changed: tokio::sync::Notify::new(),
            epoch,
        });
        let outcome = tokio::time::timeout(
            Duration::from_secs(10),
            Box::pin(async {
                tokio::join!(
                    serve,
                    super::reconnect(Arc::clone(&core), endpoint, None, 0)
                )
            }),
        )
        .await
        .expect("auth stops the reconnect");
        match outcome.1 {
            super::Event::Failed { error, .. } => {
                assert!(
                    matches!(error, WireError::Auth),
                    "a 401 denial stops with Auth, got {error:?}"
                );
            }
            super::Event::Connected { epoch, .. } => {
                panic!("a 401 denial must not reconnect, got epoch {epoch}")
            }
            super::Event::Frame { .. } => panic!("reconnect never yields a frame"),
        }
    }
}
