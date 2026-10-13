//! Session-local publication and replay.

pub(crate) mod actor;
pub(crate) mod backend;
pub(crate) mod commands;
pub(crate) mod contain;
pub(crate) mod context;
pub(crate) mod control;
pub(crate) mod dispatch;
pub(crate) mod driver;
pub(crate) mod projection;
pub(crate) mod ring;
pub(crate) mod rt;
pub(crate) mod script;
pub(crate) mod service_grants;
#[cfg(test)]
mod service_path_checks;
pub(crate) mod shared;
pub(crate) mod status;
pub(crate) mod subscriber;
pub(crate) mod tasks;
pub(crate) mod turn;
use std::collections::{HashSet, VecDeque};

use dal_core::ext::{Mail as ExtMail, Receipt, SidecarName};
use dal_core::{
    Answer, BlobId, ClientId, Command, EntryId, Name, Reply, RequestId, SessionId, TurnOp,
    TurnOpReply,
};
use tokio::sync::{Notify, mpsc, oneshot};

use self::subscriber::SubscriberShared;
use crate::error::AgentError;

/// Bounded command channel into one session actor.
pub(crate) const COMMAND_CHANNEL: usize = 64;

/// Detached resolutions are bounded by the armed approval slots (one ask
/// slot plus one run slot per session), so [`PENDING_CAP`] has an order of
/// magnitude of headroom; reaching it means a slot leaked its guard, and
/// the push trips in debug while keeping the item, because drop-time
/// terminal delivery never sheds.
const PENDING_CAP: usize = 16;
/// Settled ids gate slot reuse; ids of sessions that never re-ask would
/// accumulate, so the set restarts past [`SETTLED_CAP`]. Eviction only
/// fails a reuse check closed (`ask_busy`), never reopens a slot early.
const SETTLED_CAP: usize = 4096;

/// Drop-time resolutions that bypass the command mailbox.
///
/// The synchronous producer is an [`AskSlot`](crate::ext::services) drop,
/// which cannot await the actor mailbox. Entries are keyed by [`RequestId`]:
/// the broker resolves each id at most once, and the armed approval slots
/// bound the armed guards, so `pending` holds at most a handful of items
/// against its reserved capacity. [`mark_settled`](Self::mark_settled)
/// records the ids the actor has journaled, letting the next ask reuse the
/// slot only after the previous terminal record landed.
#[derive(Default)]
pub(crate) struct ResolutionInbox {
    pending: std::sync::Mutex<VecDeque<(RequestId, actor::TurnWork)>>,
    settled: std::sync::Mutex<HashSet<RequestId>>,
    notify: Notify,
}

impl ResolutionInbox {
    pub(crate) fn new() -> Self {
        Self {
            pending: std::sync::Mutex::new(VecDeque::with_capacity(PENDING_CAP)),
            settled: std::sync::Mutex::new(HashSet::new()),
            notify: Notify::new(),
        }
    }

    /// Queues one detached resolution. The armed approval slots bound the
    /// producers, so reaching [`PENDING_CAP`] means a slot leaked its
    /// guard: trip in debug and keep the item, because drop-time terminal
    /// delivery never sheds.
    pub(crate) fn push(&self, id: RequestId, work: actor::TurnWork) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(slot) = pending.iter_mut().find(|(known, _)| *known == id) {
            slot.1 = work;
        } else {
            debug_assert!(
                pending.len() < PENDING_CAP,
                "detached resolutions exceed the armed slots"
            );
            pending.push_back((id, work));
        }
        drop(pending);
        self.notify.notify_one();
    }

    pub(crate) fn take(&self) -> Option<(RequestId, actor::TurnWork)> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
    }

    pub(crate) fn mark_settled(&self, id: RequestId) {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(known, _)| *known != id);
        let mut settled = self
            .settled
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if settled.len() >= SETTLED_CAP {
            settled.clear();
        }
        settled.insert(id);
    }

    pub(crate) fn take_settled(&self, id: RequestId) -> bool {
        self.settled
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id)
    }

    pub(crate) async fn wait(&self) {
        self.notify.notified().await;
    }
}

/// One client operation routed to the session actor.
pub(crate) enum ActorRequest {
    /// Submit a command through the fold.
    Submit {
        /// The submitted command.
        command: Command,
        /// The submitting client.
        by: ClientId,
        /// The fold-produced reply.
        reply: oneshot::Sender<Result<Reply, AgentError>>,
    },
    /// Answer one open request.
    Answer {
        /// The request to resolve.
        id: RequestId,
        /// The client's answer.
        answer: Answer,
        /// The answering client.
        by: ClientId,
        /// Resolution acknowledgement.
        reply: oneshot::Sender<Result<(), AgentError>>,
    },
    /// Read one session blob.
    Blob {
        /// The blob digest.
        id: BlobId,
        /// The blob bytes.
        reply: oneshot::Sender<Result<Vec<u8>, AgentError>>,
    },
    /// Publish one blob through the session journal.
    BlobPut {
        /// Bytes to publish under their BLAKE3 digest.
        bytes: Vec<u8>,
        /// The durable digest or store failure.
        reply: oneshot::Sender<Result<BlobId, AgentError>>,
    },
    /// Poll every status kind now and report the resulting statuses.
    PollStatus {
        /// The busy or texted statuses after the poll.
        reply: oneshot::Sender<Vec<dal_core::ExtStatus>>,
    },
    /// Flush the journal, close subscriber queues, and stop the actor.
    Shutdown {
        /// Shutdown acknowledgement.
        reply: oneshot::Sender<()>,
    },
    /// Report driver work through the fold.
    Work {
        /// The reported work.
        work: actor::TurnWork,
    },
    /// Read or write one sidecar value.
    Sidecar {
        /// The sidecar operation.
        op: SidecarOp,
    },
    /// Run one compare-and-swap state operation (R08).
    State {
        /// The state request.
        req: StateReq,
    },
    /// Deliver one mailbox message to this session.
    Mail {
        /// The mail request.
        req: MailRequest,
    },
    /// Run one turn operation.
    Turn {
        /// The turn request.
        req: TurnRequest,
    },
    /// Fork or clone this session into a new live session.
    Branch {
        /// The fork anchor, or none to clone the active path.
        at: Option<dal_core::EntryId>,
        /// The client the new session binds to.
        by: ClientId,
        /// The new session identity.
        reply: oneshot::Sender<Result<SessionId, AgentError>>,
    },
    /// Journal one attributed synthetic inner inference.
    Inferred {
        /// Who ran the inner call.
        who: dal_core::Owner,
        /// Why it ran.
        purpose: dal_core::InferredPurpose,
        /// Its normalized usage.
        usage: dal_core::Usage,
    },
    /// Journal one extension record on the current leaf.
    ExtRecord {
        /// The append request.
        req: ExtRecordRequest,
    },
    /// Attach the capability-scoped services for hook dispatch.
    Services {
        /// The session services.
        services: std::sync::Arc<dyn crate::ext::Services>,
    },
}

/// One actor-owned state operation with its reply channel (R08).
pub(crate) struct StateReq {
    /// The compare-and-swap operation.
    pub op: dal_core::ext::StateOp,
    /// The resulting record, or the expected failure.
    pub reply: oneshot::Sender<Result<dal_core::ext::StateRecord, dal_core::ext::StateError>>,
}

/// One actor-owned sidecar read or write.
pub(crate) enum SidecarOp {
    /// Read one sidecar value.
    Read {
        /// The extension that owns the sidecar.
        ext: Name,
        /// The sidecar name.
        name: SidecarName,
        /// The stored bytes, absent when never written.
        reply: oneshot::Sender<Result<Option<Vec<u8>>, Box<str>>>,
    },
    /// Write one sidecar value.
    Write {
        /// The extension that owns the sidecar.
        ext: Name,
        /// The sidecar name.
        name: SidecarName,
        /// The bytes to store.
        bytes: Vec<u8>,
        /// Write acknowledgement or storage error.
        reply: oneshot::Sender<Result<(), Box<str>>>,
    },
}

/// One durable extension-record append with its reply channel.
pub(crate) struct ExtRecordRequest {
    /// The owning extension.
    pub ext: Name,
    /// The extension-defined record kind.
    pub kind: Box<str>,
    /// The opaque record body.
    pub body: dal_core::RawJson,
    /// The journal position after the receipt, or the append failure.
    pub reply: oneshot::Sender<Result<dal_core::EntryId, crate::error::ServiceError>>,
}

/// One mailbox delivery to this session.
pub(crate) enum MailRequest {
    /// Store one message and report its receipt.
    Send {
        /// The message to store.
        mail: ExtMail,
        /// The delivery receipt.
        reply: oneshot::Sender<Option<Receipt>>,
    },
    /// Reads the non-destructive mailbox page after one cursor.
    Recv {
        /// The last mailbox cursor already read.
        after: Option<EntryId>,
        /// The messages and next cursor, oldest first.
        reply: oneshot::Sender<(Vec<ExtMail>, Option<EntryId>)>,
    },
}

/// One turn operation with its reply channel.
pub(crate) struct TurnRequest {
    /// The turn operation.
    pub op: TurnOp,
    /// The operation reply.
    pub reply: oneshot::Sender<TurnOpReply>,
}

/// The client end of a live subscription queue.
pub(crate) type SubscriberPort = std::sync::Arc<SubscriberShared>;

/// The actor port one `Agent` holds; dropping it never stops the actor.
#[derive(Clone)]
pub(crate) struct SessionHandle {
    session: SessionId,
    tx: mpsc::Sender<ActorRequest>,
    control: std::sync::Arc<std::sync::Mutex<control::ControlCell>>,
    resolutions: std::sync::Arc<ResolutionInbox>,
}

impl SessionHandle {
    /// A port into the actor behind the bounded command channel, sharing the
    /// turn-control cell so a cancel fires before the actor drains the queue.
    pub(crate) fn new(
        session: SessionId,
        tx: mpsc::Sender<ActorRequest>,
        control: std::sync::Arc<std::sync::Mutex<control::ControlCell>>,
        resolutions: std::sync::Arc<ResolutionInbox>,
    ) -> Self {
        Self {
            session,
            tx,
            control,
            resolutions,
        }
    }

    /// Fires the running turn's token before the command queues; the in-flight
    /// hook drive or stream observes the token instead of waiting for the
    /// actor to drain the mailbox.
    fn cancel_before_queue(&self, command: &Command) {
        let Command::Cancel { scope } = command else {
            return;
        };
        let dal_core::CancelScope::Turn(turn) = scope else {
            return;
        };
        let _ = self
            .control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cancel(*turn);
    }

    /// The session this port addresses.
    pub(crate) fn session(&self) -> SessionId {
        self.session
    }

    async fn roundtrip<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<T>) -> ActorRequest,
    ) -> Result<T, AgentError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(build(tx))
            .await
            .map_err(|_| AgentError::SessionClosed { id: self.session })?;
        rx.await
            .map_err(|_| AgentError::SessionClosed { id: self.session })
    }

    /// Submits a command; full channels apply backpressure to the caller.
    pub(crate) async fn submit(&self, command: Command, by: ClientId) -> Result<Reply, AgentError> {
        self.cancel_before_queue(&command);
        self.roundtrip(|reply| ActorRequest::Submit { command, by, reply })
            .await?
    }

    /// Answers one open request through the broker.
    pub(crate) async fn answer(
        &self,
        id: RequestId,
        answer: Answer,
        by: ClientId,
    ) -> Result<(), AgentError> {
        self.roundtrip(|reply| ActorRequest::Answer {
            id,
            answer,
            by,
            reply,
        })
        .await?
    }

    /// Reads one session blob through the journal.
    pub(crate) async fn blob(&self, id: BlobId) -> Result<Vec<u8>, AgentError> {
        self.roundtrip(|reply| ActorRequest::Blob { id, reply })
            .await?
    }
    /// Publishes one content-addressed blob through the journal.
    pub(crate) async fn put_blob(&self, bytes: Vec<u8>) -> Result<BlobId, AgentError> {
        self.roundtrip(|reply| ActorRequest::BlobPut { bytes, reply })
            .await?
    }

    /// Polls every status kind through the actor and returns the statuses
    /// that are busy or carry text.
    pub(crate) async fn poll_status(&self) -> Result<Vec<dal_core::ExtStatus>, AgentError> {
        self.roundtrip(|reply| ActorRequest::PollStatus { reply })
            .await
    }

    /// Flushes the journal, closes subscriber queues, and stops the actor.
    pub(crate) async fn shutdown(&self) {
        let _ = self
            .roundtrip(|reply| ActorRequest::Shutdown { reply })
            .await;
    }

    /// Journals one synthetic inner inference; a closed session drops it.
    pub(crate) async fn inferred(
        &self,
        who: dal_core::Owner,
        purpose: dal_core::InferredPurpose,
        usage: dal_core::Usage,
    ) {
        let _ = self
            .tx
            .send(ActorRequest::Inferred {
                who,
                purpose,
                usage,
            })
            .await;
    }

    /// Reports driver work through the fold; full channels apply
    /// backpressure to the caller.
    pub(crate) async fn work(&self, work: actor::TurnWork) -> Result<(), AgentError> {
        self.tx
            .send(ActorRequest::Work { work })
            .await
            .map_err(|_| AgentError::SessionClosed { id: self.session })
    }

    /// Reports one broker resolution without awaiting the actor. The inbox
    /// is owned by the actor and preserves the resolution, keyed by request,
    /// when the command mailbox is full. Causality holds: the matching
    /// `Asked` was enqueued before this resolution could exist, so the actor
    /// folds queued mailbox work before inbox work and never meets an
    /// `Answered` whose question is still queued behind it.
    pub(crate) fn work_detached(&self, work: actor::TurnWork) {
        let actor::TurnWork::Answered { resolved } = &work else {
            debug_assert!(
                false,
                "only broker resolutions may bypass the command mailbox"
            );
            let _ = self.tx.try_send(ActorRequest::Work { work });
            return;
        };
        self.resolutions.push(resolved.request.id, work);
    }

    /// Borrows the actor-owned resolution inbox, so the host can hand the
    /// same owner to the session services for settle-gated slot reuse.
    pub(crate) fn resolutions(&self) -> std::sync::Arc<ResolutionInbox> {
        std::sync::Arc::clone(&self.resolutions)
    }

    /// Reads or writes one sidecar value.
    pub(crate) async fn sidecar(&self, op: SidecarOp) -> Result<(), AgentError> {
        self.tx
            .send(ActorRequest::Sidecar { op })
            .await
            .map_err(|_| AgentError::SessionClosed { id: self.session })
    }

    /// Runs one compare-and-swap state operation (R08).
    pub(crate) async fn state(&self, req: StateReq) -> Result<(), AgentError> {
        self.tx
            .send(ActorRequest::State { req })
            .await
            .map_err(|_| AgentError::SessionClosed { id: self.session })
    }

    /// Delivers one mailbox message.
    pub(crate) async fn mail(&self, req: MailRequest) -> Result<(), AgentError> {
        self.tx
            .send(ActorRequest::Mail { req })
            .await
            .map_err(|_| AgentError::SessionClosed { id: self.session })
    }
    /// Attaches the capability-scoped services for hook dispatch.
    pub(crate) async fn services(
        &self,
        services: std::sync::Arc<dyn crate::ext::Services>,
    ) -> Result<(), AgentError> {
        self.tx
            .send(ActorRequest::Services { services })
            .await
            .map_err(|_| AgentError::SessionClosed { id: self.session })
    }

    /// Journals one extension record.
    pub(crate) async fn ext_record(&self, req: ExtRecordRequest) -> Result<(), AgentError> {
        self.tx
            .send(ActorRequest::ExtRecord { req })
            .await
            .map_err(|_| AgentError::SessionClosed { id: self.session })
    }

    /// Runs one turn operation.
    pub(crate) async fn turn(&self, req: TurnRequest) -> Result<(), AgentError> {
        self.tx
            .send(ActorRequest::Turn { req })
            .await
            .map_err(|_| AgentError::SessionClosed { id: self.session })
    }

    /// Forks or clones this session, returning the new session identity.
    pub(crate) async fn branch(
        &self,
        at: Option<dal_core::EntryId>,
        by: ClientId,
    ) -> Result<SessionId, AgentError> {
        self.roundtrip(|reply| ActorRequest::Branch { at, by, reply })
            .await?
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use dal_core::{Owner, Question, Request};

    fn withdrawn(id: RequestId) -> actor::TurnWork {
        actor::TurnWork::Answered {
            resolved: crate::broker::Resolved {
                request: Request {
                    id,
                    turn: None,
                    owner: Owner::Core,
                    question: Question::Text {
                        prompt: "proceed?".into(),
                        placeholder: None,
                    },
                    timeout: std::time::Duration::from_secs(1),
                    default: Answer::Cancel,
                },
                answer: Answer::Cancel,
                by: ClientId::new("core"),
                resolution: crate::broker::Resolution::Cancelled,
                was_default: false,
            },
        }
    }

    /// A full command mailbox keeps every detached resolution, in order:
    /// drop-time withdrawals never shed.
    #[tokio::test]
    async fn a_full_mailbox_keeps_every_detached_resolution() {
        let (tx, _rx) = mpsc::channel(COMMAND_CHANNEL);
        let inbox = std::sync::Arc::new(ResolutionInbox::new());
        let handle = SessionHandle::new(
            SessionId::new_v7(),
            tx,
            std::sync::Arc::new(std::sync::Mutex::new(control::ControlCell::new())),
            std::sync::Arc::clone(&inbox),
        );
        for _ in 0..COMMAND_CHANNEL {
            handle
                .work(actor::TurnWork::TaskFailed {
                    turn: None,
                    message: "filler".into(),
                })
                .await
                .expect("mailbox space");
        }
        let first = RequestId::new_v7();
        let second = RequestId::new_v7();
        handle.work_detached(withdrawn(first));
        handle.work_detached(withdrawn(second));
        let (kept_first, _) = inbox.take().expect("the first withdrawal is kept");
        let (kept_second, _) = inbox.take().expect("the second withdrawal is kept");
        assert_eq!(kept_first, first);
        assert_eq!(kept_second, second);
        assert!(inbox.take().is_none());
    }
}
