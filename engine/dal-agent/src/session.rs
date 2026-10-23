//! Session-local publication and replay.

pub(crate) mod actor;
pub(crate) mod backend;
pub(crate) mod commands;
pub(crate) mod contain;
pub(crate) mod context;
pub(crate) mod continuation;
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
use std::sync::Arc;

use dal_core::ext::{Mail as ExtMail, Receipt, SidecarName};
use dal_core::{
    Answer, BlobId, CallId, ClientId, Command, EntryId, Name, Owner, PrivateEffectId,
    PrivateLoopId, PrivateOutcome, RawJson, Reply, RequestId, SessionId, TurnId, TurnOp,
    TurnOpReply,
};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::time::Instant;

use self::subscriber::SubscriberShared;
use crate::broker::{Broker, Resolved};
use crate::error::{AgentError, ServiceError};
use crate::ext::services::MAX_OPEN_ASKS;

/// Bounded command channel into one session actor.
pub(crate) const COMMAND_CHANNEL: usize = 64;

/// Structural bound on the withdrawals lane, derived from admission: every
/// armed service slot ([`MAX_OPEN_ASKS`] ask slots plus the one run slot)
/// contributes at most one withdrawal claim over its occupancy lifetime —
/// the broker claims each request at most once and the admission entry
/// stays occupied until the actor settles the claim — so the lane can hold
/// no more than that many items while admission keeps its slots occupied.
const WITHDRAWAL_BOUND: usize = MAX_OPEN_ASKS + 1;
const _: () = assert!(WITHDRAWAL_BOUND == MAX_OPEN_ASKS + 1);

/// Service resolutions claimed for the session actor.
///
/// The inbox owns the session broker: every claim it routes performs its
/// broker compare-and-swap under the [`Pending`] lock, so
/// [`close`](Self::close) fences claims as well as entries and each request
/// reaches the inbox at most once. Two lanes feed one drain:
///
/// - `withdrawals`: drop-time withdrawals and local-deadline expiries of
///   armed services slots. An armed slot's admission entry stays occupied
///   until [`mark_settled`](Self::mark_settled), so the lane holds at most
///   [`WITHDRAWAL_BOUND`] claims; reaching that in debug means an admission
///   entry leaked its guard, and the claim is still kept (delivery never
///   sheds).
/// - `cancelled`: turn-cancel claims of reported requests, in broker open
///   order. Each item is one distinct broker slot moved to `core` exactly
///   once, so the lane needs no numeric cap.
///
/// Claims whose question the fold never accepted (a `Asked` that was never
/// enqueued, or a closed fold) fold to `Ok` with no emit and release their
/// waiter fail-closed; [`mark_settled`](Self::mark_settled) still records
/// the withdrawal ones so the producer's admission frees without any
/// fabricated record. Turn-cancel claims owe no settlement: their waiter
/// observed the cancel and freed its slot itself.
pub(crate) struct ResolutionInbox {
    broker: Arc<Broker>,
    pending: std::sync::Mutex<Pending>,
    /// Settled withdrawal receipts admission has not consumed yet. Only a
    /// withdrawal owed a settlement enters
    /// ([`mark_settled`](ResolutionInbox::mark_settled)), and each entry
    /// leaves through [`take_settled`](ResolutionInbox::take_settled) when
    /// admission rechecks its still-occupied slot, so the set holds at most
    /// [`WITHDRAWAL_BOUND`] ids with no cap and no eviction: a receipt is
    /// never forgotten and orphan marks never accumulate.
    settled: std::sync::Mutex<HashSet<RequestId>>,
    notify: Notify,
}

/// The claimed-but-undrained resolutions of one session.
///
/// `owed` holds the receipts of withdrawal claims still queued for the
/// actor — bounded by [`WITHDRAWAL_BOUND`] — so
/// [`mark_settled`](ResolutionInbox::mark_settled) records only
/// settlements the admission checks are actually waiting for and orphan
/// marks cannot accumulate.
struct Pending {
    withdrawals: VecDeque<Resolved>,
    cancelled: VecDeque<Resolved>,
    owed: HashSet<RequestId>,
    closed: bool,
}

impl ResolutionInbox {
    pub(crate) fn new(broker: Arc<Broker>) -> Self {
        Self {
            broker,
            pending: std::sync::Mutex::new(Pending {
                withdrawals: VecDeque::with_capacity(WITHDRAWAL_BOUND),
                cancelled: VecDeque::new(),
                owed: HashSet::with_capacity(WITHDRAWAL_BOUND),
                closed: false,
            }),
            settled: std::sync::Mutex::new(HashSet::with_capacity(WITHDRAWAL_BOUND)),
            notify: Notify::new(),
        }
    }

    /// Claims one armed services slot for its holder. `expiry: None`
    /// withdraws the request (cause `Withdrawn`); `Some(unattended)` expires
    /// it through `Broker::expire_one`, which records `NoController` when
    /// `unattended` and `TimedOut` otherwise. The caller computes
    /// `unattended` before calling, so no attachment lock nests under
    /// `pending`. A closed inbox claims nothing: the actor's own open-request
    /// sweep releases those waiters fail-closed after the fence.
    ///
    /// Returns the claimed request id as the receipt. `Some(id)` queued a
    /// claim whose settlement will free the caller's admission entry.
    /// `None` means nothing claimed — the slot was already resolved by
    /// another winner (a client answer or the actor's expiry sweep, both of
    /// which journal before releasing), or the fence owns it — so no
    /// `mark_settled` will ever arrive for this id and the caller frees its
    /// admission entry at once. Without that receipt a lost race would
    /// strand the entry and wedge the battery at its cap. The receipt is a
    /// copyable id: the queued claim keeps the payload, the caller learns
    /// only whether a settlement is coming.
    pub(crate) fn withdraw(&self, id: RequestId, expiry: Option<bool>) -> Option<RequestId> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if pending.closed {
            return None;
        }
        let claim = match expiry {
            None => self.broker.withdraw(id),
            Some(unattended) => self.broker.expire_one(id, Instant::now(), unattended),
        };
        let claim = claim?;
        debug_assert!(
            pending.withdrawals.len() < WITHDRAWAL_BOUND,
            "withdrawal claims exceed the armed service slots"
        );
        let receipt = claim.request.id;
        pending.withdrawals.push_back(claim);
        pending.owed.insert(receipt);
        drop(pending);
        self.notify.notify_one();
        Some(receipt)
    }

    /// Classifies a turn cancel through the broker under the inbox lock.
    /// Reported slots are claimed and queued; unreported slots keep their
    /// claim parked for `Broker::report_opened`. An empty claim set causes
    /// no enqueue and no notify. A closed inbox claims nothing.
    pub(crate) fn cancel_turn(&self, turn: TurnId) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if pending.closed {
            return;
        }
        let claims = self.broker.cancel_turn(turn);
        if claims.is_empty() {
            return;
        }
        pending.cancelled.extend(claims);
        drop(pending);
        self.notify.notify_one();
    }

    /// Withdrawals first, then cancelled claims.
    pub(crate) fn take(&self) -> Option<Resolved> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pending
            .withdrawals
            .pop_front()
            .or_else(|| pending.cancelled.pop_front())
    }

    /// Fences claims and entries: a later `withdraw` or `cancel_turn`
    /// claims nothing (the actor's open-request sweep releases those
    /// waiters), and the finite remainder returns for the caller to fold
    /// or release.
    pub(crate) fn close(&self) -> Vec<Resolved> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pending.closed = true;
        let mut rest = Vec::with_capacity(pending.withdrawals.len() + pending.cancelled.len());
        rest.extend(pending.withdrawals.drain(..));
        rest.extend(pending.cancelled.drain(..));
        rest
    }

    /// Records that the actor settled one drained item. Only a withdrawal
    /// receipt still queued for the actor is owed a settlement: marks for
    /// turn-cancel claims or unknown ids record nothing, so the settled
    /// set stays bounded by the armed service slots and never forgets a
    /// receipt an admission check has not consumed yet.
    pub(crate) fn mark_settled(&self, id: RequestId) {
        let owed = {
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            pending.owed.remove(&id)
        };
        if !owed {
            return;
        }
        let mut settled = self
            .settled
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
    /// Wait until the last ended turn's driver confirmation has been folded.
    Settle {
        /// Sent once the actor has folded the pending turn confirmation.
        reply: oneshot::Sender<()>,
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
    /// Run one compare-and-swap state operation.
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
    /// Journal one private-loop birth before its first effect.
    PrivateLoop {
        /// The birth request.
        req: PrivateLoopRequest,
    },
    /// Journal one private-effect start before the effect runs.
    PrivateStart {
        /// The start request.
        req: PrivateStartRequest,
    },
    /// Journal one private-effect terminal outcome.
    PrivateEnd {
        /// The end request.
        req: PrivateEndRequest,
    },
    /// Attach the capability-scoped services for hook dispatch.
    Services {
        /// The session services.
        services: std::sync::Arc<dyn crate::ext::Services>,
    },
}

/// One actor-owned state operation with its reply channel.
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

/// One durable private-loop birth with its reply channel.
///
/// The reply arrives only after the actor commits the birth; a pre-append
/// validation conflict answers with the error without appending, while an
/// append failure answers with the error and folds/publishes nothing.
pub(crate) struct PrivateLoopRequest {
    /// The loop identity minted once at Loop construction.
    pub r#loop: PrivateLoopId,
    /// The loop owner minted from the registering extension.
    pub who: Owner,
    /// The captured turn, when the loop runs inside a session turn.
    pub turn: Option<TurnId>,
    /// The birth acknowledgement, or the validation/append failure.
    pub reply: oneshot::Sender<Result<(), AgentError>>,
}

/// One durable private-effect start with its reply channel.
///
/// The reply arrives only after the actor commits the start; the caller
/// must not run the effect until it does. `call` and `args` are
/// live-execution correlation only: they ride the journal record, never
/// the fold pairing row.
pub(crate) struct PrivateStartRequest {
    /// The loop identity.
    pub r#loop: PrivateLoopId,
    /// The effect identity.
    pub token: PrivateEffectId,
    /// The effect owner; must match the loop birth.
    pub who: Owner,
    /// The captured turn; must match the loop birth.
    pub turn: Option<TurnId>,
    /// The private round number.
    pub round: u32,
    /// The live-execution call identity; correlation only.
    pub call: CallId,
    /// The tool name.
    pub name: Box<str>,
    /// The raw tool arguments; journal-only, never published.
    pub args: RawJson,
    /// The start acknowledgement, or the validation/append failure.
    pub reply: oneshot::Sender<Result<(), ServiceError>>,
}

/// One durable private-effect terminal outcome with its reply channel.
///
/// The reply arrives only after the actor commits the end and publishes
/// the terminal. Exactly one end exists per (`loop`, `token`).
pub(crate) struct PrivateEndRequest {
    /// The loop identity.
    pub r#loop: PrivateLoopId,
    /// The effect identity.
    pub token: PrivateEffectId,
    /// The terminal outcome.
    pub outcome: PrivateOutcome,
    /// The end acknowledgement, or the validation/append failure.
    pub reply: oneshot::Sender<Result<(), ServiceError>>,
}

/// One mailbox delivery to this session.
pub(crate) enum MailRequest {
    /// Store one message and report its receipt.
    Send {
        /// The message to store.
        mail: ExtMail,
        /// The durable delivery receipt, or the journal append failure.
        reply: oneshot::Sender<Result<Option<Receipt>, crate::error::ServiceError>>,
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
    opening_tx: mpsc::Sender<ActorRequest>,
    control: std::sync::Arc<std::sync::Mutex<control::ControlCell>>,
    resolutions: std::sync::Arc<ResolutionInbox>,
}

impl SessionHandle {
    /// A port into the actor behind the bounded command channels, sharing the
    /// turn-control cell so a cancel fires before the actor drains the queue.
    pub(crate) fn new(
        session: SessionId,
        tx: mpsc::Sender<ActorRequest>,
        opening_tx: mpsc::Sender<ActorRequest>,
        control: std::sync::Arc<std::sync::Mutex<control::ControlCell>>,
        resolutions: std::sync::Arc<ResolutionInbox>,
    ) -> Self {
        Self {
            session,
            tx,
            opening_tx,
            control,
            resolutions,
        }
    }

    /// Returns the bounded service lane. During opening its requests may
    /// journal or queue input, but cannot advance the suspended hook chain.
    fn safe_sender(&self) -> mpsc::Sender<ActorRequest> {
        self.opening_tx.clone()
    }

    fn work_sender(&self, work: &actor::TurnWork) -> mpsc::Sender<ActorRequest> {
        if matches!(
            work,
            actor::TurnWork::Asked { .. } | actor::TurnWork::Answered { .. }
        ) {
            return self.safe_sender();
        }
        self.tx.clone()
    }

    fn turn_sender(&self, req: &TurnRequest) -> mpsc::Sender<ActorRequest> {
        if matches!(
            &req.op,
            TurnOp::IsIdle | TurnOp::Cancel | TurnOp::Steer { .. } | TurnOp::Wake { .. }
        ) {
            self.safe_sender()
        } else {
            self.tx.clone()
        }
    }

    async fn send_via(
        &self,
        sender: mpsc::Sender<ActorRequest>,
        request: ActorRequest,
    ) -> Result<(), AgentError> {
        sender
            .send(request)
            .await
            .map_err(|_| AgentError::SessionClosed { id: self.session })
    }

    async fn roundtrip_via<T>(
        &self,
        sender: mpsc::Sender<ActorRequest>,
        build: impl FnOnce(oneshot::Sender<T>) -> ActorRequest,
    ) -> Result<T, AgentError> {
        let (tx, rx) = oneshot::channel();
        sender
            .send(build(tx))
            .await
            .map_err(|_| AgentError::SessionClosed { id: self.session })?;
        rx.await
            .map_err(|_| AgentError::SessionClosed { id: self.session })
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
        self.roundtrip_via(self.tx.clone(), build).await
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
        self.roundtrip_via(self.safe_sender(), |reply| ActorRequest::Answer {
            id,
            answer,
            by,
            reply,
        })
        .await?
    }

    /// Reads one session blob through the actor.
    pub(crate) async fn blob(&self, id: BlobId) -> Result<Vec<u8>, AgentError> {
        self.roundtrip_via(self.safe_sender(), |reply| ActorRequest::Blob { id, reply })
            .await?
    }

    /// Publishes one content-addressed blob through the actor.
    pub(crate) async fn put_blob(&self, bytes: Vec<u8>) -> Result<BlobId, AgentError> {
        self.roundtrip_via(self.safe_sender(), |reply| ActorRequest::BlobPut {
            bytes,
            reply,
        })
        .await?
    }

    /// Polls every status kind through the actor and returns the statuses
    /// that are busy or carry text.
    pub(crate) async fn poll_status(&self) -> Result<Vec<dal_core::ExtStatus>, AgentError> {
        self.roundtrip_via(self.safe_sender(), |reply| ActorRequest::PollStatus {
            reply,
        })
        .await
    }

    /// Resolves once the actor has folded the driver confirmation for the
    /// last ended turn, so turn-end observer records are durable before
    /// session-end observers sweep per-turn state. A closed session
    /// resolves as [`AgentError::SessionClosed`].
    pub(crate) async fn settled(&self) -> Result<(), AgentError> {
        self.roundtrip_via(self.safe_sender(), |reply| ActorRequest::Settle { reply })
            .await
    }

    /// Classifies every open request of the opening and running turn for
    /// shutdown before any lifecycle fire: reported claims reach the inbox
    /// while it still accepts them, so their terminal record is `Cancelled`
    /// rather than `Withdrawn`. The host close paths call this before
    /// cancelling lifecycle tokens or aborting tasks; a second call
    /// classifies nothing (the tokens are already fired).
    pub(crate) fn begin_shutdown(&self) {
        self.control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cancel_opening_for_shutdown();
    }

    /// Flushes the journal, closes subscriber queues, and stops the actor.
    pub(crate) async fn shutdown(&self) {
        self.begin_shutdown();
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
        // Provider accounting in a running turn keeps its FIFO with driver
        // work. Only inner inference awaited by an opening hook bypasses it.
        let sender = {
            let control = self
                .control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if control.running().is_none() && !control.idle() {
                self.safe_sender()
            } else {
                self.tx.clone()
            }
        };
        let _ = sender
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
        self.work_sender(&work)
            .send(ActorRequest::Work { work })
            .await
            .map_err(|_| AgentError::SessionClosed { id: self.session })
    }

    /// Borrows the actor-owned resolution inbox, so the host can hand the
    /// same owner to the session services for settle-gated slot reuse.
    pub(crate) fn resolutions(&self) -> std::sync::Arc<ResolutionInbox> {
        std::sync::Arc::clone(&self.resolutions)
    }

    /// Reads or writes one sidecar value.
    pub(crate) async fn sidecar(&self, op: SidecarOp) -> Result<(), AgentError> {
        self.send_via(self.safe_sender(), ActorRequest::Sidecar { op })
            .await
    }

    /// Runs one compare-and-swap state operation.
    pub(crate) async fn state(&self, req: StateReq) -> Result<(), AgentError> {
        self.send_via(self.safe_sender(), ActorRequest::State { req })
            .await
    }

    /// Delivers one mailbox message.
    pub(crate) async fn mail(&self, req: MailRequest) -> Result<(), AgentError> {
        self.send_via(self.safe_sender(), ActorRequest::Mail { req })
            .await
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
        self.send_via(self.safe_sender(), ActorRequest::ExtRecord { req })
            .await
    }

    /// Journals one private-loop birth and awaits the actor receipt.
    ///
    /// Uses the opening-safe lane like every other durable service commit,
    /// so a birth during opening-hook suspension cannot deadlock. A closed
    /// lane surfaces as [`AgentError::SessionClosed`]; a pre-append
    /// validation conflict surfaces as [`AgentError::Invalid`] without
    /// appending. An append failure surfaces as
    /// [`AgentError::PersistenceFailed`] instead: validation passed, but
    /// the birth record did not persist. It folds/publishes nothing
    /// (irreversible store errors may leave a partial write; the next
    /// open repairs it).
    pub(crate) async fn private_loop_born(
        &self,
        r#loop: PrivateLoopId,
        who: Owner,
        turn: Option<TurnId>,
    ) -> Result<(), AgentError> {
        self.roundtrip_via(self.safe_sender(), |reply| ActorRequest::PrivateLoop {
            req: PrivateLoopRequest {
                r#loop,
                who,
                turn,
                reply,
            },
        })
        .await?
    }

    /// Journals one private-effect start and awaits the actor receipt.
    ///
    /// The caller must not run the effect until this returns `Ok`: the
    /// start-before-effect order is an order on durable writes, never on
    /// sends. A closed lane surfaces as a [`ServiceError`] failure with
    /// the standard closed-record text (same literal as the backend's
    /// closed-record error). A validation conflict appends nothing; an
    /// append failure folds/publishes no row or terminal (reopen repairs).
    pub(crate) async fn private_start(
        &self,
        r#loop: PrivateLoopId,
        token: PrivateEffectId,
        who: Owner,
        turn: Option<TurnId>,
        round: u32,
        call: CallId,
        name: Box<str>,
        args: RawJson,
    ) -> Result<(), ServiceError> {
        self.roundtrip_via(self.safe_sender(), |reply| ActorRequest::PrivateStart {
            req: PrivateStartRequest {
                r#loop,
                token,
                who,
                turn,
                round,
                call,
                name,
                args,
                reply,
            },
        })
        .await
        .map_err(|_| {
            ServiceError::failed(None, "the session closed before the record was journaled.")
        })?
    }

    /// Journals one private-effect terminal outcome and awaits the actor receipt.
    ///
    /// On `Ok` the terminal is durable, folded, and published to the shared
    /// index. A validation rejection appends nothing; an append failure
    /// folds/publishes nothing and leaves the row Started in memory, which
    /// the next open repairs to [`PrivateOutcome::Indeterminate`].
    pub(crate) async fn private_end(
        &self,
        r#loop: PrivateLoopId,
        token: PrivateEffectId,
        outcome: PrivateOutcome,
    ) -> Result<(), ServiceError> {
        self.roundtrip_via(self.safe_sender(), |reply| ActorRequest::PrivateEnd {
            req: PrivateEndRequest {
                r#loop,
                token,
                outcome,
                reply,
            },
        })
        .await
        .map_err(|_| {
            ServiceError::failed(None, "the session closed before the record was journaled.")
        })?
    }

    /// Runs one turn operation.
    pub(crate) async fn turn(&self, req: TurnRequest) -> Result<(), AgentError> {
        let sender = self.turn_sender(&req);
        self.send_via(sender, ActorRequest::Turn { req }).await
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
    use dal_core::{Owner, Question};
    use std::time::Duration;

    fn text_question(prompt: &str) -> Question {
        Question::Text {
            prompt: prompt.into(),
            placeholder: None,
        }
    }

    /// One actor port, broker, and inbox: the handle's cell is wired to the
    /// same inbox the tests drain. The receivers stay alive for the caller;
    /// a dropped receiver turns every `tx` send into `SessionClosed`.
    fn fixture() -> (
        SessionHandle,
        std::sync::Arc<Broker>,
        std::sync::Arc<ResolutionInbox>,
        mpsc::Receiver<ActorRequest>,
        mpsc::Receiver<ActorRequest>,
    ) {
        let (tx, rx) = mpsc::channel(COMMAND_CHANNEL);
        let (opening_tx, opening_rx) = mpsc::channel(COMMAND_CHANNEL);
        let broker = std::sync::Arc::new(Broker::new());
        let inbox = std::sync::Arc::new(ResolutionInbox::new(std::sync::Arc::clone(&broker)));
        let handle = SessionHandle::new(
            SessionId::new_v7(),
            tx,
            opening_tx,
            std::sync::Arc::new(std::sync::Mutex::new(control::ControlCell::new(
                std::sync::Arc::clone(&inbox),
            ))),
            std::sync::Arc::clone(&inbox),
        );
        (handle, broker, inbox, rx, opening_rx)
    }

    fn open_turnless(broker: &Broker, prompt: &str, turn: impl Into<Option<TurnId>>) -> RequestId {
        broker
            .open(
                Owner::Core,
                text_question(prompt),
                turn,
                Instant::now() + Duration::from_secs(30),
            )
            .0
            .id
    }

    /// A full command mailbox keeps every withdrawal claim, in drop order:
    /// the inbox bypasses the mailbox entirely and never sheds.
    #[tokio::test]
    async fn a_full_mailbox_keeps_every_withdrawal_claim() {
        let (handle, broker, inbox, _rx, _opening_rx) = fixture();
        for _ in 0..COMMAND_CHANNEL {
            handle
                .work(actor::TurnWork::TaskFailed {
                    turn: None,
                    message: "filler".into(),
                })
                .await
                .expect("mailbox space");
        }
        let first = open_turnless(&broker, "first?", None);
        let second = open_turnless(&broker, "second?", None);
        inbox.withdraw(first, None);
        inbox.withdraw(second, None);
        // The broker claim is single-winner: a repeat withdraw adds nothing.
        inbox.withdraw(first, None);
        let kept_first = inbox.take().expect("the first withdrawal is kept");
        let kept_second = inbox.take().expect("the second withdrawal is kept");
        assert_eq!(kept_first.request.id, first);
        assert_eq!(kept_second.request.id, second);
        assert_eq!(kept_first.cause, dal_core::ResolutionCause::Withdrawn);
        assert_eq!(kept_second.cause, dal_core::ResolutionCause::Withdrawn);
        assert!(inbox.take().is_none());
    }

    /// The armed-slot tripwire: sixteen ask slots plus one run slot, each
    /// contributing one claim, stay under the derived bound in debug.
    #[tokio::test]
    async fn armed_slots_withdrawal_claims_fit_the_derived_bound() {
        let (_handle, broker, inbox, _, _) = fixture();
        let mut ids = Vec::new();
        for n in 0..=MAX_OPEN_ASKS {
            ids.push(open_turnless(&broker, &format!("ask {n}?"), None));
        }
        for id in &ids {
            inbox.withdraw(*id, None);
        }
        for id in &ids {
            let claim = inbox.take().expect("every claim is kept in order");
            assert_eq!(claim.request.id, *id);
            assert_eq!(claim.cause, dal_core::ResolutionCause::Withdrawn);
        }
        assert!(inbox.take().is_none());
    }

    /// Expired claims carry the unattended probe: attended expiries are
    /// `TimedOut`, unattended ones `NoController`, and a request whose
    /// deadline has not passed claims nothing yet.
    #[tokio::test(start_paused = true)]
    async fn expiry_claims_carry_the_unattended_probe() {
        let (_handle, broker, inbox, _, _) = fixture();
        let attended = open_turnless(&broker, "attended?", None);
        let unattended = open_turnless(&broker, "unattended?", None);
        let early = open_turnless(&broker, "early?", None);
        inbox.withdraw(attended, Some(false));
        inbox.withdraw(unattended, Some(true));
        assert!(
            inbox.take().is_none(),
            "an unexpired request claims nothing"
        );
        tokio::time::advance(Duration::from_secs(60)).await;
        inbox.withdraw(attended, Some(false));
        inbox.withdraw(unattended, Some(true));
        let attended_claim = inbox.take().expect("the attended expiry claims");
        assert_eq!(attended_claim.request.id, attended);
        assert_eq!(attended_claim.cause, dal_core::ResolutionCause::TimedOut);
        let unattended_claim = inbox.take().expect("the unattended expiry claims");
        assert_eq!(unattended_claim.request.id, unattended);
        assert_eq!(
            unattended_claim.cause,
            dal_core::ResolutionCause::NoController
        );
        assert!(early_claim_is_still_open(&broker, early));
    }

    fn early_claim_is_still_open(broker: &Broker, id: RequestId) -> bool {
        broker
            .open_requests()
            .iter()
            .any(|request| request.id == id)
    }

    /// A turn cancel claims reported slots in broker open order and parks
    /// unreported ones for `report_opened`; a second cancel adds nothing.
    #[tokio::test]
    async fn cancel_turn_claims_reported_and_parks_unreported_once() {
        let (_handle, broker, inbox, _, _) = fixture();
        let turn = TurnId::new(std::num::NonZeroU64::new(7).unwrap());
        let first = open_turnless(&broker, "first?", turn);
        let second = open_turnless(&broker, "second?", turn);
        let third = open_turnless(&broker, "third?", turn);
        assert!(broker.report_opened(second).is_none());
        inbox.cancel_turn(turn);
        let claims: Vec<RequestId> = std::iter::from_fn(|| inbox.take())
            .map(|claim| claim.request.id)
            .collect();
        assert_eq!(claims, vec![second], "only the reported slot queues");
        let still_open = broker
            .open_requests()
            .into_iter()
            .map(|request| request.id)
            .collect::<Vec<_>>();
        assert!(
            !still_open.contains(&second),
            "a claimed slot leaves the open set: {still_open:?}"
        );
        let parked: Vec<RequestId> = broker
            .parked()
            .into_iter()
            .map(|claim| claim.request.id)
            .collect();
        assert_eq!(parked, vec![first, third], "unreported claims park");
        inbox.cancel_turn(turn);
        assert!(inbox.take().is_none(), "a repeat cancel claims nothing");
        assert!(
            broker.report_opened(first).is_some(),
            "the parked claim hands over"
        );
        assert!(broker.report_opened(third).is_some());
    }

    /// Withdrawals drain before cancelled claims, and `close` fences both
    /// lanes: post-fence producers claim nothing, and the fence's remainder
    /// returns withdrawals first.
    #[tokio::test]
    async fn close_fences_claims_and_returns_the_remainder() {
        let (_handle, broker, inbox, _, _) = fixture();
        let turn = TurnId::new(std::num::NonZeroU64::new(9).unwrap());
        let withdrawn = open_turnless(&broker, "withdrawn?", None);
        let cancelled = open_turnless(&broker, "cancelled?", turn);
        let late = open_turnless(&broker, "late?", None);
        assert!(broker.report_opened(cancelled).is_none());
        inbox.withdraw(withdrawn, None);
        inbox.cancel_turn(turn);
        let closed = inbox.close();
        assert_eq!(closed.len(), 2, "close returns the finite remainder");
        assert_eq!(closed[0].request.id, withdrawn, "withdrawals first");
        assert_eq!(closed[1].request.id, cancelled);
        // The fence: producers after close claim nothing and queue nothing.
        inbox.withdraw(late, None);
        inbox.cancel_turn(turn);
        let late_open = broker
            .open_requests()
            .iter()
            .any(|request| request.id == late);
        assert!(late_open, "a post-fence producer claims nothing");
        assert!(inbox.take().is_none());
    }

    /// Settlement is one-shot per drained claim, in both directions.
    #[tokio::test]
    async fn settlement_marks_are_recorded_once_per_claim() {
        let (_handle, broker, inbox, _, _) = fixture();
        let id = open_turnless(&broker, "settled?", None);
        inbox.withdraw(id, None);
        let claim = inbox.take().expect("the claim");
        inbox.mark_settled(claim.request.id);
        assert!(inbox.take_settled(id), "the first consume observes settle");
        assert!(!inbox.take_settled(id), "the mark is single-winner");
        assert!(
            broker.open_requests().iter().all(|open| open.id != id),
            "the claim left the slot resolved"
        );
    }

    /// Turn-cancel claims owe no settlement: their marks record nothing, so
    /// a churn of them neither grows the settled set nor evicts a withdrawal
    /// receipt that admission has not consumed yet.
    #[tokio::test]
    async fn cancellation_marks_never_evict_an_owed_receipt() {
        const CHURN: usize = 8192;
        let (_handle, broker, inbox, _, _) = fixture();
        let owed = open_turnless(&broker, "withdrawn?", None);
        assert_eq!(inbox.withdraw(owed, None), Some(owed));
        let claim = inbox.take().expect("the withdrawal claim");
        inbox.mark_settled(claim.request.id);

        let turn = TurnId::new(std::num::NonZeroU64::new(11).unwrap());
        let orphans: Vec<RequestId> = (0..CHURN)
            .map(|_| {
                let id = open_turnless(&broker, "cancelled?", turn);
                assert!(broker.report_opened(id).is_none());
                id
            })
            .collect();
        inbox.cancel_turn(turn);
        let mut drained = 0;
        while let Some(claim) = inbox.take() {
            assert_eq!(claim.cause, dal_core::ResolutionCause::Cancelled);
            inbox.mark_settled(claim.request.id);
            drained += 1;
        }
        assert_eq!(drained, CHURN, "every cancel claim reached the drain");

        assert_eq!(
            inbox.settled.lock().unwrap().len(),
            1,
            "only the owed receipt is retained"
        );
        assert!(
            orphans.iter().all(|id| !inbox.take_settled(*id)),
            "orphan cancellation marks record nothing"
        );
        assert!(
            inbox.take_settled(owed),
            "the owed receipt survives the churn until admission consumes it"
        );
        assert!(!inbox.take_settled(owed), "the receipt is single-winner");
    }

    /// A dropped actor lane turns a private-loop birth into `SessionClosed`.
    #[tokio::test]
    async fn private_loop_birth_reports_a_closed_session() {
        let (handle, _broker, _inbox, rx, opening_rx) = fixture();
        drop(rx);
        drop(opening_rx);
        let error = handle
            .private_loop_born(PrivateLoopId::new_v7(), Owner::Core, None)
            .await
            .expect_err("a closed lane reports its failure");
        assert!(
            matches!(error, AgentError::SessionClosed { .. }),
            "a closed birth keeps the agent error shape: {error:?}"
        );
    }

    /// Dropped actor lanes turn private start/end receipts into failed
    /// service errors with the standard closed-record text.
    #[tokio::test]
    async fn private_start_and_end_report_a_closed_session() {
        let (handle, _broker, _inbox, rx, opening_rx) = fixture();
        drop(rx);
        drop(opening_rx);
        let r#loop = PrivateLoopId::new_v7();
        let token = PrivateEffectId::new_v7();
        let args = RawJson::parse("{}").expect("literal test args parse");
        let start = handle
            .private_start(
                r#loop,
                token,
                Owner::Core,
                None,
                0,
                CallId::new("call-1"),
                "read".into(),
                args,
            )
            .await;
        assert!(
            matches!(&start, Err(crate::error::ServiceError::Failed { .. })),
            "a closed start keeps the service failure shape: {start:?}"
        );
        let end = handle
            .private_end(r#loop, token, PrivateOutcome::Indeterminate)
            .await;
        assert!(
            matches!(&end, Err(crate::error::ServiceError::Failed { .. })),
            "a closed end keeps the service failure shape: {end:?}"
        );
    }
}
