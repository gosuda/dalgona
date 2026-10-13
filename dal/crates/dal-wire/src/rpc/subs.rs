//! Live update pumps for session and host subscriptions.
//!
//! Each pump runs inside the `session/subscribe` (or `host/subscribe`)
//! handler future: the reply precedes every notification by construction.
//! A pump exits when its fence is replaced, its token is cancelled, or the
//! core subscription drains. Slow consumers trigger core-side lag detection,
//! which arrives as [`Delivery::Resync`]; the wire additionally coalesces its
//! own unsent queue (capacity 1024) per call, per job, and per status source.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use dal_agent::{Delivery, Host, HostSubscription, HostUpdate, Subscription};
use dal_core::{Gen, Seq, SessionId, Update, UpdateKind};
use sonic_rs::Value;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use super::{Conn, send, to_value};
use crate::jsonrpc::Message;
use crate::transport::FrameWriter;

/// Unsent queue capacity before the pump sheds to one resync.
const QUEUE_CAP: usize = 1024;

/// Drives one session subscription until replaced, cancelled, or drained.
pub(crate) async fn session_pump(
    state: Arc<Mutex<Conn>>,
    writer: FrameWriter,
    session: SessionId,
    fence: u64,
    mut subscription: Subscription,
) {
    let mut pump = Pump::new(session);
    loop {
        if !fence_live(&state, session, fence).await {
            return;
        }
        tokio::select! {
            biased;
            () = cancelled(&state, session, fence) => return,
            delivery = subscription.next() => {
                let Some(delivery) = delivery else { return };
                if !pump.deliver(&writer, delivery).await {
                    return;
                }
            }
        }
    }
}

/// Sends one `resync` update outside a pump (stale-generation subscribe).
pub(crate) async fn send_resync(writer: &FrameWriter, session: SessionId, r#gen: Gen, seq: Seq) {
    let update = sonic_rs::json!({"type": "resync", "gen": r#gen.get(), "seq": seq.get()});
    send(
        writer,
        &Message::Notification {
            method: "session/update".to_owned(),
            params: sonic_rs::json!({
                "sessionId": session.to_string(),
                "gen": r#gen.get(),
                "seq": seq.get(),
                "update": update,
            }),
        },
    )
    .await;
}

/// Returns true while this pump's fence still owns the session slot.
async fn fence_live(state: &Arc<Mutex<Conn>>, session: SessionId, fence: u64) -> bool {
    state
        .lock()
        .await
        .subs
        .get(&session)
        .is_some_and(|(live, _)| *live == fence)
}

/// Resolves when this pump's token is cancelled or the slot is replaced.
async fn cancelled(state: &Arc<Mutex<Conn>>, session: SessionId, fence: u64) {
    let token = state
        .lock()
        .await
        .subs
        .get(&session)
        .filter(|(live, _)| *live == fence)
        .map(|(_, token)| token.clone());
    match token {
        Some(token) => token.cancelled().await,
        None => std::future::pending::<()>().await,
    }
}

/// One session's unsent queue with per-source coalescing.
pub(crate) struct Pump {
    session: SessionId,
    /// Last forwarded `(r#gen, seq)`; older pairs are already sent.
    last: Option<(u64, u64)>,
    /// Buffered updates in arrival order.
    queue: VecDeque<Arc<Update>>,
    /// Coalescing index: source key to queue position.
    index: HashMap<String, usize>,
    /// Replaced sequence numbers counted as skipped.
    skipped: u64,
}

impl Pump {
    pub(crate) fn new(session: SessionId) -> Self {
        Self {
            session,
            last: None,
            queue: VecDeque::new(),
            index: HashMap::new(),
            skipped: 0,
        }
    }

    /// Forwards one delivery; returns false when the transport is gone.
    async fn deliver(&mut self, writer: &FrameWriter, delivery: Delivery) -> bool {
        match delivery {
            Delivery::Update(update) => {
                let pair = (update.r#gen.get(), update.seq.get());
                if self.last.is_some_and(|last| pair <= last) {
                    return true;
                }
                self.push(update);
                self.flush(writer).await
            }
            Delivery::Resync { generation, seq } => {
                self.queue.clear();
                self.index.clear();
                self.last = Some((generation.get(), seq.get()));
                send_resync(writer, self.session, generation, seq).await;
                true
            }
        }
    }

    /// Buffers one update, coalescing per call, job, or status source.
    pub(crate) fn push(&mut self, update: Arc<Update>) {
        if self.queue.len() >= QUEUE_CAP {
            self.shed();
        }
        if let Some(key) = coalesce_key(&update.kind) {
            if let Some(&position) = self.index.get(&key) {
                self.skipped += 1;
                self.queue[position] = update;
                return;
            }
            self.index.insert(key, self.queue.len());
        }
        self.queue.push_back(update);
    }

    #[cfg(test)]
    pub(crate) fn queued_updates(&self) -> Vec<Arc<Update>> {
        self.queue.iter().cloned().collect()
    }

    /// Discards queued deltas; the next flush resumes from the high mark.
    fn shed(&mut self) {
        tracing::debug!(skipped = self.skipped, "shedding coalesced update queue");
        self.skipped = 0;
        self.queue.clear();
        self.index.clear();
    }

    /// Writes every buffered update as one `session/update` notification.
    async fn flush(&mut self, writer: &FrameWriter) -> bool {
        while let Some(update) = self.queue.pop_front() {
            self.index.retain(|_, position| {
                if *position == 0 {
                    false
                } else {
                    *position -= 1;
                    true
                }
            });
            let body = match to_value(&update.kind) {
                Ok(body) => body,
                Err(error) => {
                    tracing::error!(error = %error.message, "update encoding failed");
                    let r#gen = update.r#gen;
                    let seq = update.seq;
                    self.last = Some((r#gen.get(), seq.get()));
                    send_resync(writer, self.session, r#gen, seq).await;
                    continue;
                }
            };
            self.last = Some((update.r#gen.get(), update.seq.get()));
            send(
                writer,
                &Message::Notification {
                    method: "session/update".to_owned(),
                    params: sonic_rs::json!({
                        "sessionId": self.session.to_string(),
                        "gen": update.r#gen.get(),
                        "seq": update.seq.get(),
                        "update": body,
                    }),
                },
            )
            .await;
        }
        true
    }
}

/// Returns the coalescing key for updates that supersede their own kind.
fn coalesce_key(kind: &UpdateKind) -> Option<String> {
    match kind {
        UpdateKind::ToolProgress { call, .. } => Some(format!("progress:{}", call.as_str())),
        UpdateKind::JobStarted { job } | UpdateKind::JobSettled { job, .. } => {
            Some(format!("job:{job}"))
        }
        UpdateKind::ExtStatus(status) => Some(format!("status:{}", status.ext)),
        _ => None,
    }
}

/// Drives one host subscription until replaced, cancelled, or drained.
///
/// The `{}` reply is written inline before the first notification; this
/// function returns `None` so the dispatcher sends no second reply.
pub(crate) async fn host_notifier(
    host: Host,
    state: Arc<Mutex<Conn>>,
    writer: FrameWriter,
    id: &crate::jsonrpc::Id,
) -> Option<Message> {
    send(
        &writer,
        &Message::Result {
            id: id.clone(),
            result: sonic_rs::json!({}),
        },
    )
    .await;
    let token = CancellationToken::new();
    state.lock().await.host_sub = Some(token.clone());
    let mut subscription: HostSubscription = host.subscribe();
    loop {
        tokio::select! {
            biased;
            () = token.cancelled() => return None,
            update = subscription.next() => {
                let update = update?;
                forward_host_update(&host, &writer, update).await;
            }
        }
    }
}

/// Forwards one host lifecycle update as a `host/update` notification.
async fn forward_host_update(host: &Host, writer: &FrameWriter, update: HostUpdate) {
    let Some(body) = host_update_body(host, &update) else {
        return;
    };
    send(
        writer,
        &Message::Notification {
            method: "host/update".to_owned(),
            params: sonic_rs::json!({"update": body}),
        },
    )
    .await;
}

/// Maps one host update to its wire body.
fn host_update_body(host: &Host, update: &HostUpdate) -> Option<Value> {
    match update {
        HostUpdate::SessionAdded { session } | HostUpdate::SessionChanged { session } => {
            session_info(host, *session)
                .map(|info| sonic_rs::json!({"type": "session_changed", "session": info}))
        }
        HostUpdate::SessionRemoved { session } => {
            Some(sonic_rs::json!({"type": "session_removed", "sessionId": session.to_string()}))
        }
        HostUpdate::ChildStarted { parent, child, .. } => Some(sonic_rs::json!({
            "type": "child_started",
            "sessionId": child.to_string(),
            "parentId": parent.to_string(),
        })),
        HostUpdate::ChildEnded { parent, child, .. } => Some(sonic_rs::json!({
            "type": "child_ended",
            "sessionId": child.to_string(),
            "parentId": parent.to_string(),
        })),
    }
}

/// Looks up one session's info row for change notifications.
fn session_info(host: &Host, session: SessionId) -> Option<Value> {
    let query = dal_core::ListQuery {
        limit: None,
        cursor: None,
        search: None,
    };
    let page = host.sessions(query).ok()?;
    page.items
        .into_iter()
        .find(|info| info.id == session)
        .and_then(|info| to_value(&info).ok())
}
