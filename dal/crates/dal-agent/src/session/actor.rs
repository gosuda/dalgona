//! One actor per session: fold, journal, publish, and broker wiring.
//!
//! The actor alone mutates the fold. Every `Emit` appends its records before
//! any update publishes; driver-bound effects park on the 256-bound turn
//! channel the Step-3 driver consumes.

use std::collections::{HashMap, VecDeque};
use std::num::NonZeroU64;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use dal_core::ext::{
    BeforeTurn, HookEvent, HookOutcome, HookVerdict, InputEvent, Origin, Receipt, StreamVerdict,
};
use dal_core::{
    Answer, BlobId, ClientId, Command, CompactLimits, CompactionSummary, Effect, Emit, Event,
    Family, InferFailure, Inference, MailMode, ModelRoute, Name, Part, PartialResponse, Phase,
    Policy, Record, Rejection, Reply, Request, RequestId, ResolvedCall, Session, SessionId,
    StreamEvent, Timestamp, TurnId, TurnOp, TurnOpReply, TurnSource, TurnState, UpdateKind,
    Workspace,
};
use dal_store::Journal;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use super::control::ControlCell;
use super::shared::Shared;
use super::status::{STATUS_POLL, sweep};
use super::{ActorRequest, COMMAND_CHANNEL, SessionHandle};
use crate::broker::{AnswerWait, Broker, Resolved, default_timeout};
use crate::error::{ActualTurn, AgentError, ServiceError, TurnPhase, ValidationError, deny_text};
use crate::ext::hooks::{DispatchCx, dispatch_before_turn, dispatch_input, join_before_turn};
use crate::ext::{Caller, CallerKind, Services};

/// Bound for actor-to-driver effect batches.
const DRIVER_CHANNEL: usize = 256;
/// Fallback expiry sweep when no actor event arrives.
const EXPIRY_TICK: Duration = Duration::from_secs(1);

/// One batch of driver-bound effects plus the ask waiters it may await.
pub(crate) struct TurnBatch {
    /// Effects the turn driver executes.
    pub(crate) effects: Vec<Effect>,
    /// Ask waiters handed to the driver, keyed by request.
    pub(crate) asks: Vec<(Request, AnswerWait)>,
    /// The fold approval policy snapshot for this batch.
    pub(crate) policy: Policy,
    /// The requested model route snapshot for this batch.
    pub(crate) model: Option<ModelRoute>,
    /// The active provider family snapshot for this batch.
    pub(crate) family: Option<Family>,
}

/// Driver ports the Step-3 turn task consumes.
pub(crate) struct DriverPorts {
    /// Effect batches for the driver, with the ask waiters they may await.
    pub(crate) ops_rx: mpsc::Receiver<TurnBatch>,
    /// The shared turn-bypass cell: the driver binds each turn's stream to the
    /// token `ControlCell::cancel` fires, so a cancel preempts a live `infer`
    /// out-of-band (a queued `Effect::Stop` could never reach it mid-stream).
    pub(crate) control: Arc<Mutex<ControlCell>>,
}

/// Work the Step-3 driver reports back to the actor.
pub(crate) enum TurnWork {
    /// A provider stream event.
    Streamed {
        /// The turn producing the event.
        turn: TurnId,
        /// The stream event.
        event: StreamEvent,
    },
    /// A watcher verdict on streamed content.
    WatcherVerdict {
        /// The turn producing the verdict.
        turn: TurnId,
        /// The verdict.
        verdict: StreamVerdict,
        /// Receipt: the persisted reminder entry, if the interrupt retried.
        reply: oneshot::Sender<Option<dal_core::EntryId>>,
    },
    /// A non-interrupting stream rule reminder, journaled durably.
    StreamReminder {
        /// Owning turn.
        turn: TurnId,
        /// The rule that fired.
        rule: Box<str>,
        /// The reminder text.
        text: Box<str>,
        /// Receipt: the persisted reminder entry.
        reply: oneshot::Sender<Option<dal_core::EntryId>>,
    },
    /// The driver confirmed a turn's stop: the batch queue is ordered, so an
    /// in-flight call's kill ladder already finished; this is the earliest
    /// point a cancelled turn's `TurnEnded` may reach subscribers.
    Cancelled {
        /// The stopped turn.
        turn: TurnId,
        /// The stop the turn ended with.
        stop: dal_core::Stop,
    },
    /// The driver task failed.
    TaskFailed {
        /// The failed turn, when known.
        turn: Option<TurnId>,
        /// The failure message.
        message: Box<str>,
    },
    /// The dispatcher opened a broker approval for journaling.
    Asked {
        /// The opened request.
        request: Request,
    },
    /// The broker resolved a dispatcher approval.
    Answered {
        /// The resolution receipt.
        resolved: Resolved,
    },
    /// The dispatcher began executing one resolved call.
    CallStarted {
        /// Owning turn.
        turn: TurnId,
        /// The call whose execution began.
        call: dal_core::CallId,
    },
    /// One dispatcher result, fed in plan order.
    Settled {
        /// Owning turn.
        turn: TurnId,
        /// The completed call.
        call: dal_core::CallId,
        /// The call's terminal outcome.
        outcome: dal_core::SettledOutcome,
    },
    /// The driver selected the model route opening one request stream.
    RequestStarted {
        /// Owning turn.
        turn: TurnId,
        /// Selected provider model route.
        model: ModelRoute,
        /// Provider family used by this request.
        family: Family,
        /// Model context window for this request.
        window: u64,
        /// Step budget for this request.
        max_steps: u32,
        /// Compaction limits evaluated at request open.
        compact: CompactLimits,
    },
    /// One provider request finished with its recorded inference.
    StreamEnded {
        /// Owning turn.
        turn: TurnId,
        /// Concrete provider model route that answered.
        model: ModelRoute,
        /// Provider family for the resulting assistant entry.
        family: Family,
        /// Provider result, classified independently from error details.
        result: Result<Inference, InferFailure>,
        /// Partial content when an interrupted result is reported.
        partial: Option<PartialResponse>,
    },
    /// The driver classified one response worth of tool calls.
    Resolved {
        /// Owning turn.
        turn: TurnId,
        /// Resolved calls in provider order.
        calls: Vec<ResolvedCall>,
        /// Whether an answerer is attached for approval questions.
        answerer_attached: bool,
    },
    /// The driver finished a response with no streamed calls; the fold
    /// runs the end-after-boundary path from this event.
    Boundary {
        /// Owning turn.
        turn: TurnId,
    },
    /// One compaction chain run finished with its journal summary.
    CompactionSettled {
        /// Turn id for automatic compaction, absent for manual.
        turn: Option<TurnId>,
        /// The compactor outcome.
        outcome: Result<CompactionSummary, Box<str>>,
    },
    /// One off-actor command finished with its client reply.
    CommandDone {
        /// The command result for the waiting client.
        result: Result<Reply, AgentError>,
    },
}

/// Construction arguments for one session actor.
pub(crate) struct ActorDeps {
    /// The session identity.
    pub(crate) session: SessionId,
    /// The open journal.
    pub(crate) journal: Journal,
    /// The replayed fold.
    pub(crate) fold: Session,
    /// The shared snapshot; the actor is its only writer.
    pub(crate) shared: Arc<Shared>,
    /// The session request broker.
    pub(crate) broker: Arc<Broker>,
    /// The session workspace for job log paths.
    pub(crate) workspace: Workspace,
    /// The child depth, zero for top-level sessions.
    pub(crate) depth: u32,
    /// The parent session, when this session is a subagent.
    pub(crate) parent: Option<SessionId>,
    /// Replay effects awaiting the turn driver.
    pub(crate) pending: Vec<Effect>,
    /// The host state for batch snapshots.
    pub(crate) host: Arc<crate::host::HostState>,
    /// The session data-plane cell backing scripted hook entries (R03);
    /// filled right after the actor spawns.
    pub(crate) backend: Arc<std::sync::OnceLock<Arc<crate::session::backend::Backend>>>,
    /// The owner for background work that must stop before the journal closes.
    pub(crate) tasks: crate::session::tasks::SessionTasks,
}

/// One live session actor.
pub(crate) struct Actor {
    session: SessionId,
    journal: Journal,
    fold: Session,
    broker: Arc<Broker>,
    shared: Arc<Shared>,
    control: Arc<Mutex<ControlCell>>,
    workspace: Workspace,
    sidecar: HashMap<dal_core::Name, Vec<u8>>,
    /// The child depth, zero for top-level sessions.
    depth: u32,
    parent: Option<SessionId>,
    rx: mpsc::Receiver<ActorRequest>,
    driver_tx: mpsc::Sender<TurnBatch>,
    host: Arc<crate::host::HostState>,
    pending_commands: VecDeque<ReplyTx>,
    pending_compact: bool,
    closing: bool,
    broken: Option<Box<str>>,
    /// Capability-scoped services for hook dispatch, attached after spawn.
    services: Option<Arc<dyn Services>>,

    /// The session data-plane cell backing scripted hook entries; filled
    /// right after the actor spawns.
    backend: Arc<std::sync::OnceLock<Arc<crate::session::backend::Backend>>>,
    tasks: crate::session::tasks::SessionTasks,
}

type ReplyTx = oneshot::Sender<Result<Reply, AgentError>>;

/// Pages the session's journaled mail after a cursor.
///
/// The cursor is the 1-based ordinal of the last-delivered mail record;
/// `EntryId` here names a mail position, not a conversation entry.
fn mailbox_page(
    session: SessionId,
    records: &[Record],
    after: Option<dal_core::EntryId>,
) -> (Vec<dal_core::ext::Mail>, Option<dal_core::EntryId>) {
    let mut ordinal = 0u64;
    let mut mail = Vec::new();
    let mut next = after;
    for record in records {
        let Record::Mail(record) = record else {
            continue;
        };
        let Some(nonzero) = NonZeroU64::new(ordinal.saturating_add(1)) else {
            continue;
        };
        ordinal = nonzero.get();
        let cursor = dal_core::EntryId::new(nonzero);
        if record.to != session || after.is_some_and(|after| cursor.get() <= after.get()) {
            continue;
        }
        mail.push(dal_core::ext::Mail {
            from: record.from,
            to: record.to,
            mode: record.mode,
            text: record.text.clone(),
            reply_to: record
                .reply_to
                .as_deref()
                .and_then(|reply| reply.parse::<u64>().ok())
                .and_then(NonZeroU64::new)
                .map(dal_core::EntryId::new),
        });
        next = Some(cursor);
    }
    (mail, next)
}

/// Maps a fold rejection onto the typed wake refusal reasons.
fn wake_refusal(rejection: &Rejection) -> dal_core::ext::WakeError {
    match rejection {
        Rejection::Denied {
            reason: dal_core::DenyReason::WakeLimit,
        } => dal_core::ext::WakeError::Limit,
        Rejection::BusyTurn => dal_core::ext::WakeError::Busy,
        other => dal_core::ext::WakeError::Journal {
            message: other.to_string().into(),
        },
    }
}

/// Undelivered mail bound for this session.
///
/// A `NextTurn` message always waits for a following turn, so its record
/// counts as undelivered; `Aside` and delivered `Steer` records do not.
fn undelivered_mail(session: SessionId, records: &[Record]) -> usize {
    records
        .iter()
        .filter(|record| {
            matches!(
                record,
                Record::Mail(record)
                    if record.to == session && record.mode == MailMode::NextTurn
            )
        })
        .count()
}

/// Waiting-mail capacity per recipient (the spec's 100-message bound).
const MAILBOX_WAITING_LIMIT: usize = 100;

/// Finds the reminder entry the fold emitted in one effect batch.
fn reminder_entry(effects: &[Effect]) -> Option<dal_core::EntryId> {
    effects.iter().find_map(|effect| {
        let Effect::Emit(emit) = effect else {
            return None;
        };
        emit.records.iter().find_map(|record| {
            let Record::Reminder(entry) = record else {
                return None;
            };
            Some(entry.id)
        })
    })
}

/// Spawns one session actor; the Step-3 driver consumes `DriverPorts`.
pub(crate) fn spawn(deps: ActorDeps) -> (SessionHandle, DriverPorts, tokio::task::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(COMMAND_CHANNEL);
    let (driver_tx, ops_rx) = mpsc::channel(DRIVER_CHANNEL);
    let control = Arc::new(Mutex::new(ControlCell::new()));
    let actor = Actor {
        session: deps.session,
        journal: deps.journal,
        fold: deps.fold,
        broker: deps.broker,
        shared: deps.shared,
        control: Arc::clone(&control),
        workspace: deps.workspace,
        sidecar: HashMap::new(),
        depth: deps.depth,
        parent: deps.parent,
        rx,
        driver_tx,
        host: deps.host,
        pending_commands: VecDeque::new(),
        pending_compact: false,
        closing: false,
        broken: None,
        services: None,
        backend: deps.backend,
        tasks: deps.tasks,
    };
    let handle = SessionHandle::new(deps.session, tx);
    #[expect(
        clippy::disallowed_methods,
        reason = "session-owned actor task: the host stores the handle and awaits it on close"
    )]
    let task = tokio::spawn(actor.into_run(deps.pending));
    (handle, DriverPorts { ops_rx, control }, task)
}

/// Borrows the content parts carried by one opening source.
fn opening_content(source: &TurnSource) -> Vec<Part> {
    match source {
        TurnSource::Prompt { content, .. }
        | TurnSource::Wake { content, .. }
        | TurnSource::FollowUp { content, .. } => content.clone(),
    }
}

/// Concatenates the text parts of one opening content for hook events.
fn content_text(content: &[Part]) -> String {
    let mut text = String::new();
    for part in content {
        if let Part::Text { text: piece } = part {
            text.push_str(piece);
        }
    }
    text
}

/// Mints the hook caller for one extension, skipping unparseable names.
fn hook_caller(extension: &crate::ext::Extension, turn: TurnId) -> Option<Caller> {
    let name = extension.name().parse::<Name>().ok()?;
    Some(Caller::new(
        name,
        extension.origin(),
        extension.inject(),
        CallerKind::Hook,
        Some(turn),
    ))
}

impl Actor {
    async fn into_run(mut self, pending: Vec<Effect>) {
        if !pending.is_empty() {
            let batch = self.batch(pending, Vec::new());
            if self.driver_tx.send(batch).await.is_err() {
                self.broken = Some("the turn driver is gone.".into());
            }
        }
        self.shared.sync_ext(&self.fold);
        self.poll_status();
        self.run().await;
    }

    /// Polls every registered status kind once and publishes the changes.
    fn poll_status(&self) {
        let generation = self.host.shared.generation.borrow().clone();
        let published = self.shared.ext_statuses();
        for status in sweep(
            self.session,
            &generation,
            &self.shared.ext_records(),
            &published,
        ) {
            self.publish(UpdateKind::ExtStatus(status));
        }
    }

    /// Builds one driver batch with fold policy and model snapshots.
    fn batch(&self, effects: Vec<Effect>, asks: Vec<(Request, AnswerWait)>) -> TurnBatch {
        TurnBatch {
            effects,
            asks,
            policy: self.fold.policy(self.answerer_attached()),
            model: self
                .fold
                .requested_model()
                .cloned()
                .or_else(|| self.fold.active_model().cloned()),
            family: self.fold.active_family(),
        }
    }

    /// Reports whether a frontend can answer approval questions.
    fn answerer_attached(&self) -> bool {
        self.shared.attached()
    }

    /// Locks the shared turn-bypass cell.
    fn control(&self) -> MutexGuard<'_, ControlCell> {
        self.control.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn run(mut self) {
        let mut status_tick = tokio::time::interval(STATUS_POLL);
        status_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut expiry_tick = tokio::time::interval(EXPIRY_TICK);
        expiry_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                request = self.rx.recv() => {
                    let Some(request) = request else { break };
                    self.on_request(request).await;
                    if self.closing {
                        break;
                    }
                }
                _ = status_tick.tick() => {
                    self.poll_status();
                }
                _ = expiry_tick.tick() => {
                    self.sweep_expiry().await;
                }
            }
        }
    }

    async fn on_request(&mut self, request: ActorRequest) {
        match request {
            ActorRequest::Submit { command, by, reply } => {
                self.on_submit(command, by, reply).await;
            }
            ActorRequest::Answer {
                id,
                answer,
                by,
                reply,
            } => {
                let outcome = self.on_answer(id, answer, by).await;
                let _ = reply.send(outcome);
            }
            ActorRequest::Blob { id, reply } => {
                let _ = reply.send(self.blob(id));
            }
            ActorRequest::BlobPut { bytes, reply } => {
                let _ = reply.send(self.put_blob(bytes));
            }
            ActorRequest::PollStatus { reply } => {
                self.poll_status();
                let _ = reply.send(self.shared.ext_statuses().into_values().collect());
            }
            ActorRequest::Shutdown { reply } => {
                self.on_shutdown().await;
                let _ = reply.send(());
            }
            ActorRequest::Work { work } => {
                self.on_work(work).await;
            }
            ActorRequest::Sidecar { op } => {
                self.on_sidecar(op);
            }
            ActorRequest::Mail { req } => {
                self.on_mail(req).await;
            }
            ActorRequest::Turn { req } => {
                self.on_turn(req).await;
            }
            ActorRequest::Branch { at, by, reply } => {
                let _ = reply.send(self.on_branch(at, by).await);
            }
            ActorRequest::Services { services } => {
                self.services = Some(services);
                // A submit that outran service attachment parked the fold in
                // `Phase::Opening` (`drive_opening` bails without services);
                // re-drive now that hooks can resolve.
                self.execute(Vec::new(), None).await;
            }
            ActorRequest::ExtRecord { req } => {
                self.on_ext_record(req).await;
            }
            ActorRequest::Inferred {
                who,
                purpose,
                usage,
            } => {
                self.on_inferred(who, purpose, usage).await;
            }
        }
    }

    /// Forks or clones this session's journal into a new live session.
    async fn on_branch(
        &mut self,
        at: Option<dal_core::EntryId>,
        by: ClientId,
    ) -> Result<SessionId, AgentError> {
        let max = self.host.shared.config.agents().max_depth.get();
        if self.depth + 1 > max {
            return Err(AgentError::Invalid(ValidationError::new(
                crate::error::HostError::max_depth(max).to_string(),
            )));
        }
        // `Journal::fork`/`clone_session` futures hold shared journal
        // borrows across store awaits, so they are `!Send` (node06 owns
        // the principled fix). Drive them on a scoped thread under a fresh
        // current-thread runtime: the future borrows `&mut Journal`
        // (which is `Send`) and never leaves the thread, so `!Sync`
        // never matters and no worker or test runtime is blocked.
        let journal = std::thread::scope(|scope| {
            let source = &mut self.journal;
            let worker = match at {
                Some(at) => scope.spawn(move || {
                    let runtime = local_runtime()?;
                    runtime
                        .block_on(source.fork(at))
                        .map(|branched| branched.0)
                        .map_err(|error| {
                            AgentError::Invalid(ValidationError::new(error.to_string()))
                        })
                }),
                None => scope.spawn(move || {
                    let runtime = local_runtime()?;
                    runtime.block_on(source.clone_session()).map_err(|error| {
                        AgentError::Invalid(ValidationError::new(error.to_string()))
                    })
                }),
            };
            worker
                .join()
                .map_err(|_| AgentError::Invalid(ValidationError::new("branch worker failed.")))?
        })?;
        let id = journal.id();
        let host = crate::host::Host {
            state: Arc::clone(&self.host),
        };
        let workspace = self.workspace.clone();
        host.launch_branched(journal, workspace, self.session, self.depth + 1, by)
            .await
            .map_err(|error| AgentError::Invalid(ValidationError::new(error.to_string())))?;
        Ok(id)
    }

    /// Mints the script host of one hook chain: a fresh host against the
    /// current generation, whose uncaptured environment is the empty
    /// authority, so hook-phase entries stay fail-closed (E01 R10).
    fn hook_script(&self) -> Option<crate::ext::ScriptCx> {
        let backend = self.backend.get()?;
        let generation = self.host.shared.generation.borrow().clone();
        let script = crate::session::script::SessionScriptHost::for_generation(
            self.session,
            backend,
            Arc::clone(&self.host.shared.interpreters),
            generation,
        );
        script.attach(None)
    }

    /// Steps one driver work report through the fold and executes effects.
    #[expect(
        clippy::too_many_lines,
        reason = "cohesive turn-work state machine; extraction would split one invariant"
    )]
    async fn on_work(&mut self, work: TurnWork) {
        let mut queue: VecDeque<Effect> = VecDeque::new();
        let mut effects = Vec::new();
        let mut delivered: Vec<dal_core::ext::ReadView> = Vec::new();
        let mut receipt: Option<(
            oneshot::Sender<Option<dal_core::EntryId>>,
            Option<dal_core::EntryId>,
        )> = None;
        match work {
            TurnWork::Asked { request } => {
                let _ = self.fold.step(
                    Event::RequestOpened { request },
                    Timestamp::now(),
                    &mut effects,
                );
            }
            TurnWork::Answered { resolved } => {
                self.queue_resolved(&resolved, &mut queue);
            }
            TurnWork::CallStarted { turn, call } => {
                let _ = self.fold.step(
                    Event::CallStarted { turn, call },
                    Timestamp::now(),
                    &mut effects,
                );
            }
            TurnWork::Settled {
                turn,
                call,
                outcome,
            } => {
                delivered = read_views(&outcome);
                let _ = self.fold.step(
                    Event::Settled {
                        turn,
                        call,
                        outcome,
                    },
                    Timestamp::now(),
                    &mut effects,
                );
            }
            TurnWork::Streamed { turn, event } => {
                let _ = self.fold.step(
                    Event::Stream { turn, event },
                    Timestamp::now(),
                    &mut effects,
                );
            }
            TurnWork::WatcherVerdict {
                turn,
                verdict,
                reply,
            } => {
                let stepped = self
                    .fold
                    .step(
                        Event::StreamVerdict { turn, verdict },
                        Timestamp::now(),
                        &mut effects,
                    )
                    .is_ok();
                let entry = reminder_entry(&effects);
                let retried = effects
                    .iter()
                    .any(|effect| matches!(effect, Effect::Infer(plan) if plan.turn == turn));
                if stepped && retried && entry.is_some() {
                    receipt = Some((reply, entry));
                } else {
                    let _ = reply.send(None);
                }
            }
            TurnWork::StreamReminder {
                turn,
                rule,
                text,
                reply,
            } => {
                let stepped = self
                    .fold
                    .step(
                        Event::StreamReminder { turn, rule, text },
                        Timestamp::now(),
                        &mut effects,
                    )
                    .is_ok();
                let entry = reminder_entry(&effects);
                if stepped && entry.is_some() {
                    receipt = Some((reply, entry));
                } else {
                    let _ = reply.send(None);
                }
            }
            TurnWork::Cancelled { turn, stop } => {
                // Cancelled turns defer their `TurnEnded` to this driver
                // confirmation — the kill ladder of an in-flight call has
                // completed by now; other stops already published at `Emit`.
                if stop == dal_core::Stop::Cancelled {
                    self.publish(UpdateKind::TurnEnded { turn, stop });
                }
            }
            TurnWork::TaskFailed { turn, message } => {
                self.publish(UpdateKind::Notice(dal_core::Notice {
                    turn,
                    kind: "driver.failed".into(),
                    text: message,
                }));
            }
            TurnWork::RequestStarted {
                turn,
                model,
                family,
                window,
                max_steps,
                compact,
            } => {
                let _ = self.fold.step(
                    Event::Limits {
                        window,
                        max_steps,
                        compact,
                    },
                    Timestamp::now(),
                    &mut effects,
                );
                let _ = self.fold.step(
                    Event::RequestStarted {
                        turn,
                        model,
                        family,
                    },
                    Timestamp::now(),
                    &mut effects,
                );
            }
            TurnWork::StreamEnded {
                turn,
                model,
                family,
                result,
                partial,
            } => {
                let _ = self.fold.step(
                    Event::StreamEnded {
                        turn,
                        model,
                        family,
                        result,
                        partial,
                    },
                    Timestamp::now(),
                    &mut effects,
                );
            }
            TurnWork::Resolved {
                turn,
                calls,
                answerer_attached,
            } => {
                let _ = self.fold.step(
                    Event::Resolved {
                        turn,
                        calls,
                        answerer_attached,
                    },
                    Timestamp::now(),
                    &mut effects,
                );
            }
            TurnWork::Boundary { turn } => {
                let _ = self
                    .fold
                    .step(Event::Boundary { turn }, Timestamp::now(), &mut effects);
            }
            TurnWork::CompactionSettled { turn, outcome } => {
                let _ = self.fold.step(
                    Event::CompactionSettled { turn, outcome },
                    Timestamp::now(),
                    &mut effects,
                );
            }
            TurnWork::CommandDone { result } => {
                if let Some(tx) = self.pending_commands.pop_front() {
                    let _ = tx.send(result);
                }
            }
        }
        queue.extend(effects);
        self.execute(queue.into(), None).await;
        if let Some((reply, entry)) = receipt {
            let _ = reply.send(if self.broken.is_none() { entry } else { None });
        }
        // Delivery to the root model happens when the settled result is
        // published; the evidence owner marks the complete rows at the
        // shared cursor so later captures freeze the right cutoff (R06).
        if !delivered.is_empty() {
            let at = self.shared.cursor();
            let generation = self.host.shared.generation.borrow().clone();
            if let Some(evidence) = generation.evidence() {
                for view in &delivered {
                    evidence.delivered(view, dal_core::ext::Consumer::Model, at);
                }
            }
        }
    }

    /// Flushes the journal, closes subscriber queues, and stops the loop.
    async fn on_shutdown(&mut self) {
        self.control().cancel_opening();
        self.tasks.stop().await;

        let _ = self.journal.close().await;
        self.shared.close_all();
        self.closing = true;
    }

    async fn on_submit(&mut self, command: Command, by: ClientId, reply: ReplyTx) {
        if let Some(broken) = self.broken.clone() {
            let _ = reply.send(Err(AgentError::Invalid(ValidationError::new(broken))));
            return;
        }
        self.sweep_expiry().await;
        self.pending_compact = matches!(command, Command::Compact { .. });
        if matches!(command, Command::Cancel { .. }) {
            self.cancel_before_step(&command);
        }
        let mut effects = Vec::new();
        let stepped = self.fold.step(
            Event::Command { cmd: command, by },
            Timestamp::now(),
            &mut effects,
        );
        match stepped {
            Err(rejection) => {
                let error = map_rejection(rejection, self.session, &self.control());
                let _ = reply.send(Err(error));
            }
            Ok(()) => self.execute(effects, Some(reply)).await,
        }
    }

    /// Fires the turn token before the fold step touches any channel.
    fn cancel_before_step(&self, command: &Command) {
        if let Command::Cancel { scope } = command
            && let dal_core::CancelScope::Turn(turn) = scope
        {
            let _ = self.control().cancel(*turn);
        }
    }

    async fn execute(&mut self, effects: Vec<Effect>, reply: Option<ReplyTx>) {
        let mut queue: VecDeque<Effect> = effects.into();
        let mut reply = reply;
        loop {
            let mut driver_effects = Vec::new();
            let mut asks = Vec::new();
            while let Some(effect) = queue.pop_front() {
                match effect {
                    Effect::Emit(emit) => {
                        if let Err(error) = self.append_observe(emit, &mut queue).await {
                            if let Some(tx) = reply.take() {
                                let _ = tx.send(Err(error));
                            }
                            if self.broken.is_some() {
                                return;
                            }
                        }
                    }
                    Effect::Reply(result) => {
                        if let Ok(Reply::Started(job)) = &result
                            && self.pending_compact
                        {
                            self.shared.set_compacting(*job);
                            self.pending_compact = false;
                        }
                        if let Some(tx) = reply.take() {
                            let _ = tx.send(result.map_err(|rejection| {
                                map_rejection(rejection, self.session, &self.control())
                            }));
                        }
                    }
                    Effect::Ask(request) => {
                        self.track_ask(request, &mut asks);
                    }
                    Effect::Delta {
                        turn,
                        channel,
                        text,
                    } => {
                        self.publish(UpdateKind::Delta {
                            turn,
                            channel,
                            text,
                        });
                    }
                    command @ Effect::Command { .. } => {
                        if let Some(tx) = reply.take() {
                            self.pending_commands.push_back(tx);
                        }
                        driver_effects.push(command);
                    }
                    driver => driver_effects.push(driver),
                }
            }
            self.refresh_stats();
            if !driver_effects.is_empty() || !asks.is_empty() {
                let batch = self.batch(driver_effects, asks);
                // A bounded send can park behind a convoyed driver; report
                // long waits instead of stalling the loop in silence.
                let session = self.session;
                let mut sending = Box::pin(self.driver_tx.send(batch));
                let sent = loop {
                    match tokio::time::timeout(Duration::from_secs(30), &mut sending).await {
                        Ok(sent) => break sent,
                        Err(_elapsed) => {
                            eprintln!("[dal-agent] session {session:?} driver batch outstanding");
                        }
                    }
                };
                if sent.is_err() {
                    self.broken = Some("the turn driver is gone.".into());
                }
            }
            let more = {
                // A turn opening has no deadline of its own: report long
                // drives so a stalled hook chain is visible in the log.
                let session = self.session;
                let mut opening = Box::pin(self.drive_opening());
                loop {
                    match tokio::time::timeout(Duration::from_secs(30), &mut opening).await {
                        Ok(more) => break more,
                        Err(_elapsed) => {
                            eprintln!("[dal-agent] session {session:?} turn opening outstanding");
                        }
                    }
                }
            };
            let Some(more) = more else { break };
            queue.extend(more);
        }
    }
    /// Bound for one opening-hook drive.
    const OPENING_HOOK_DEADLINE: Duration = Duration::from_secs(600);

    /// Runs the opening hooks for a phase awaiting its first durable record,
    /// then feeds the joined verdict as a Guard event.
    ///
    /// Prompt and follow-up openings run the input hooks before the
    /// `before_turn` hooks; wake openings skip the input hooks. A cancel that
    /// lands while the hooks run wins: the fold drops a verdict whose phase
    /// moved on.
    #[expect(
        clippy::too_many_lines,
        reason = "cohesive opening-hook state machine; extraction would split one invariant"
    )]
    async fn drive_opening(&mut self) -> Option<Vec<Effect>> {
        let (turn, content, run_input) = match self.fold.phase() {
            Phase::Opening { turn, source, .. } => (
                turn,
                opening_content(source),
                !matches!(source, TurnSource::Wake { .. }),
            ),
            Phase::Settling {
                follow_up: Some((next, source)),
                ..
            } => (
                next,
                opening_content(source),
                !matches!(source, TurnSource::Wake { .. }),
            ),
            _ => return None,
        };
        let (turn, content) = (*turn, content);
        let services = self.services.clone()?;
        let cancel = self.control().begin_opening(turn);
        let generation = self.host.shared.generation.borrow().clone();
        let deadline = Instant::now() + Self::OPENING_HOOK_DEADLINE;
        let mut content = content;
        if run_input {
            for (index, extension) in generation.extensions.iter().enumerate() {
                let Some(caller) = hook_caller(extension, turn) else {
                    continue;
                };
                let cx = DispatchCx {
                    caller: &caller,
                    services: &services,
                    session: self.session,
                    parent: self.parent,
                    process_env: Arc::clone(&self.host.shared.env),
                    turn: Some(turn),
                    cancel: &cancel,
                    turn_deadline: deadline,
                    script: self.hook_script(),
                };
                let step = dispatch_input(
                    extension.name(),
                    &cx,
                    matches!(extension.origin(), Origin::Builtin),
                    generation.inputs(index),
                    &InputEvent {
                        content: content.clone(),
                    },
                )
                .await;
                content = step.content;
                for notice in step.notices {
                    self.notice(turn, "hook.input", &notice);
                }
                if step.stop {
                    break;
                }
            }
        }
        let event = BeforeTurn {
            turn,
            text: content_text(&content).into(),
        };
        let mut texts = Vec::new();
        for (index, extension) in generation.extensions.iter().enumerate() {
            let Some(caller) = hook_caller(extension, turn) else {
                continue;
            };
            let cx = DispatchCx {
                caller: &caller,
                services: &services,
                session: self.session,
                parent: self.parent,
                process_env: Arc::clone(&self.host.shared.env),
                turn: Some(turn),
                cancel: &cancel,
                turn_deadline: deadline,
                script: self.hook_script(),
            };
            let step = dispatch_before_turn(
                extension.name(),
                &cx,
                generation.before_turns(index),
                &event,
            )
            .await;
            texts.extend(step.texts);
            for notice in step.notices {
                self.notice(turn, "hook.before_turn", &notice);
            }
        }
        self.control().end_opening(turn);
        let add = join_before_turn(&texts).map(Into::into);
        let Ok(outcome) = HookOutcome::new(HookEvent::BeforeTurn, HookVerdict::BeforeTurn(add))
        else {
            return None;
        };
        let mut effects = Vec::new();
        if self
            .fold
            .step(
                Event::Guard {
                    turn,
                    call: None,
                    extension: None,
                    outcome,
                },
                Timestamp::now(),
                &mut effects,
            )
            .is_ok()
        {
            return Some(effects);
        }
        None
    }

    /// Publishes one turn-scoped hook notice.
    fn notice(&self, turn: TurnId, kind: &str, text: &str) {
        self.publish(UpdateKind::Notice(dal_core::Notice {
            turn: Some(turn),
            kind: kind.into(),
            text: text.into(),
        }));
    }

    /// Appends records, then observes each update after its receipt. A refused
    /// batch returns the error text; only irreversible write failures mark the
    /// session broken.
    async fn append_observe(
        &mut self,
        emit: Emit,
        queue: &mut VecDeque<Effect>,
    ) -> Result<(), AgentError> {
        let updates = emit.updates;
        if let Err(error) = self.journal.append(emit.records).await {
            eprintln!(
                "[dal-agent] session {:?} journal append failed: {error}",
                self.session
            );
            let message: Box<str> = format!("journal write failed: {error}").into();
            if breaks_session(&error) {
                self.broken = Some(message.clone());
            }
            return Err(AgentError::Invalid(ValidationError::new(message)));
        }
        for kind in updates {
            self.observe(kind, queue);
        }
        Ok(())
    }

    /// Mirrors turn lifecycle, settles broker turns, and publishes.
    fn observe(&mut self, kind: UpdateKind, queue: &mut VecDeque<Effect>) {
        match &kind {
            UpdateKind::TurnStarted { turn, .. } => {
                self.control().begin_turn(*turn);
            }
            UpdateKind::TurnEnded { turn, stop } => {
                self.control().end_turn(*turn);
                let resolved = self
                    .broker
                    .resolve_turn(*turn, Answer::Cancel, core_client());
                for item in resolved {
                    self.queue_resolved(&item, queue);
                }
                // A cancelled turn holds `TurnEnded` until the driver
                // confirms: `Effect::Stop` lands after the in-flight call's
                // kill ladder in the ordered batch queue, so subscribers only
                // observe the end once the process tree is dead.
                if *stop == dal_core::Stop::Cancelled {
                    return;
                }
            }
            _ => {}
        }
        self.publish(kind);
    }

    /// Steps one broker resolution into queued effects without recursing.
    fn queue_resolved(&mut self, item: &Resolved, queue: &mut VecDeque<Effect>) {
        let by = if item.was_default {
            None
        } else {
            Some(item.by.clone())
        };
        let mut effects = Vec::new();
        if self
            .fold
            .step(
                Event::GrantResolved {
                    request: item.request.id,
                    answer: item.answer.clone(),
                    by,
                    was_default: item.was_default,
                },
                Timestamp::now(),
                &mut effects,
            )
            .is_ok()
        {
            queue.extend(effects);
        }
    }

    /// Publishes one update to the shared snapshot and its subscribers.
    fn publish(&self, kind: UpdateKind) {
        self.shared.publish(kind);
    }

    /// Journals one extension record on the current leaf, folds it, and
    /// answers with its journal position only after the receipt.
    async fn on_ext_record(&mut self, req: super::ExtRecordRequest) {
        let super::ExtRecordRequest {
            ext,
            kind,
            body,
            reply,
        } = req;
        let _ = reply.send(self.journal_ext(&ext, kind, body).await);
    }

    async fn journal_ext(
        &mut self,
        ext: &Name,
        kind: Box<str>,
        body: dal_core::RawJson,
    ) -> Result<dal_core::EntryId, ServiceError> {
        if let Some(broken) = &self.broken {
            return Err(ServiceError::failed(None, broken.clone()));
        }
        let position = self
            .fold
            .next_entry_id()
            .ok_or_else(|| ServiceError::failed(None, "the entry id space is exhausted."))?;
        let record = dal_core::Record::Ext {
            at: Timestamp::now(),
            ext: ext.as_str().into(),
            kind: kind.clone(),
            body: body.clone(),
        };
        if let Err(error) = self.journal.append(vec![record]).await {
            let message: Box<str> = format!("journal write failed: {error}").into();
            if breaks_session(&error) {
                self.broken = Some(message.clone());
            }
            return Err(ServiceError::failed(None, message));
        }
        self.fold.fold_ext(ext.as_str(), &kind, &body);
        self.shared.sync_ext(&self.fold);
        Ok(position)
    }

    /// Refreshes fold-owned counters in the shared snapshot.
    fn refresh_stats(&self) {
        self.shared.sync_ext(&self.fold);
        self.shared.set_fold_stats(
            queue_count(self.fold.steers_queued()),
            queue_count(self.fold.follow_ups_queued()),
            self.fold.auto_compaction_on(),
        );
    }

    async fn on_answer(
        &mut self,
        id: RequestId,
        answer: Answer,
        by: ClientId,
    ) -> Result<(), AgentError> {
        let resolved = self.broker.answer(id, answer, by)?;
        self.journal_resolved(&resolved).await;
        Ok(())
    }

    fn track_ask(&mut self, request: Request, asks: &mut Vec<(Request, AnswerWait)>) {
        if self
            .broker
            .open_requests()
            .iter()
            .any(|open| open.id == request.id)
        {
            self.publish(UpdateKind::RequestOpened(request));
            return;
        }
        let deadline = Instant::now() + default_timeout(&request.question);
        let waiter = self.broker.track(request.clone(), deadline);
        asks.push((request.clone(), waiter));
        self.publish(UpdateKind::RequestOpened(request));
    }

    async fn sweep_expiry(&mut self) {
        for item in self.broker.expire(Instant::now()) {
            self.journal_resolved(&item).await;
        }
    }

    async fn journal_resolved(&mut self, item: &Resolved) {
        let by = if item.was_default {
            None
        } else {
            Some(item.by.clone())
        };
        let mut effects = Vec::new();
        let stepped = self.fold.step(
            Event::GrantResolved {
                request: item.request.id,
                answer: item.answer.clone(),
                by,
                was_default: item.was_default,
            },
            Timestamp::now(),
            &mut effects,
        );
        if stepped.is_ok() {
            self.execute(effects, None).await;
        }
    }

    /// Publishes one content-addressed blob through the journal.
    fn put_blob(&mut self, bytes: Vec<u8>) -> Result<BlobId, AgentError> {
        self.journal.put_blob(bytes).map_err(|error| match error {
            dal_store::StoreError::Blob(dal_store::BlobError::Gone) => AgentError::SessionGone {
                session: self.session,
            },
            other => AgentError::Invalid(ValidationError::new(other.to_string())),
        })
    }

    fn blob(&self, id: BlobId) -> Result<Vec<u8>, AgentError> {
        self.journal.read_blob(id).map_err(|error| match error {
            dal_store::BlobError::NotFound { .. } => AgentError::BlobNotFound {
                id,
                session: self.session,
            },
            dal_store::BlobError::Gone => AgentError::SessionGone {
                session: self.session,
            },
            other => AgentError::Invalid(ValidationError::new(other.to_string())),
        })
    }

    /// Reads or writes one actor-owned sidecar value.
    fn on_sidecar(&mut self, op: super::SidecarOp) {
        match op {
            super::SidecarOp::Read { name, reply } => {
                let _ = reply.send(self.sidecar.get(&name).cloned());
            }
            super::SidecarOp::Write { name, bytes, reply } => {
                self.sidecar.insert(name, bytes);
                let _ = reply.send(());
            }
        }
    }

    /// Stores one mailbox message and reports its receipt.
    async fn on_mail(&mut self, req: super::MailRequest) {
        let (mail, reply) = match req {
            super::MailRequest::Send { mail, reply } => (mail, reply),
            super::MailRequest::Recv { after, reply } => {
                let (mail, next) = mailbox_page(self.session, self.journal.records(), after);
                let _ = reply.send((mail, next));
                return;
            }
        };
        let running = self.control().running();
        let receipt = match mail.mode {
            MailMode::Steer => match running {
                // The fold owns the text once the steer lands in the
                // running turn; a full steer queue refuses the message.
                Some(turn) => {
                    let mut effects = Vec::new();
                    let stepped = self.fold.step(
                        Event::Steer {
                            turn,
                            text: mail.text.clone(),
                        },
                        Timestamp::now(),
                        &mut effects,
                    );
                    match stepped {
                        Ok(()) => {
                            self.execute(effects, None).await;
                            Receipt::Delivered
                        }
                        Err(Rejection::SteerFull) => Receipt::Full,
                        Err(_) => Receipt::Buffered,
                    }
                }
                None => self.buffered_mail_receipt(),
            },
            MailMode::Aside => Receipt::Delivered,
            MailMode::NextTurn => self.buffered_mail_receipt(),
            // `MailMode` is `#[non_exhaustive]`; every known variant has
            // an explicit arm above. An unknown future mode stores like
            // a deferred message until it gains an explicit arm.
            _ => {
                debug_assert!(false, "unmapped mail mode; add an explicit arm");
                self.buffered_mail_receipt()
            }
        };
        if receipt != Receipt::Full {
            let record = Record::Mail(dal_core::Mail {
                at: jiff::Timestamp::now(),
                from: mail.from,
                to: mail.to,
                mode: mail.mode,
                text: mail.text.clone(),
                reply_to: mail.reply_to.map(|entry| entry.get().to_string().into()),
            });
            let _ = self.journal.append(vec![record]).await;
        }
        let _ = reply.send(Some(receipt));
    }

    /// Receipt for a message that waits in the journal; a full mailbox
    /// refuses with `Full` instead of silently accepting the message.
    fn buffered_mail_receipt(&self) -> Receipt {
        if undelivered_mail(self.session, self.journal.records()) >= MAILBOX_WAITING_LIMIT {
            Receipt::Full
        } else {
            Receipt::Buffered
        }
    }

    /// Runs one background-job operation against the session table.
    ///
    /// A known operation is answered from the job table; an unknown future
    /// operation answers `Unavailable` instead of fabricating job state.
    /// Runs one turn operation against the control cell and fold.
    async fn on_turn(&mut self, req: super::TurnRequest) {
        let running = self.control().running();
        let reply = match req.op {
            TurnOp::Cancel => match running {
                Some(turn) => {
                    let command = dal_core::Command::Cancel {
                        scope: dal_core::CancelScope::Turn(turn),
                    };
                    self.cancel_before_step(&command);
                    let mut effects = Vec::new();
                    let stepped = self.fold.step(
                        Event::Command {
                            cmd: command,
                            by: core_client(),
                        },
                        Timestamp::now(),
                        &mut effects,
                    );
                    if stepped.is_ok() {
                        self.execute(effects, None).await;
                    }
                    TurnOpReply::Cancelled
                }
                None => TurnOpReply::Idle(true),
            },
            TurnOp::Steer { text } => match running {
                Some(turn) => {
                    let mut effects = Vec::new();
                    let stepped =
                        self.fold
                            .step(Event::Steer { turn, text }, Timestamp::now(), &mut effects);
                    if stepped.is_ok() {
                        self.execute(effects, None).await;
                        TurnOpReply::Steered
                    } else {
                        TurnOpReply::Idle(true)
                    }
                }
                None => TurnOpReply::Idle(true),
            },
            TurnOp::Wake {
                text,
                sources,
                job_ids,
            } => {
                let mut effects = Vec::new();
                let stepped = self.fold.step(
                    Event::Wake {
                        text,
                        sources: sources.into_boxed_slice(),
                        jobs: job_ids.into_boxed_slice(),
                    },
                    Timestamp::now(),
                    &mut effects,
                );
                match stepped {
                    Ok(()) => {
                        self.execute(effects, None).await;
                        TurnOpReply::Woken
                    }
                    Err(rejection) => TurnOpReply::WakeRefused(wake_refusal(&rejection)),
                }
            }
            _ => TurnOpReply::Idle(running.is_none()),
        };
        let _ = req.reply.send(reply);
    }

    async fn on_inferred(
        &mut self,
        who: dal_core::Owner,
        purpose: dal_core::InferredPurpose,
        usage: dal_core::Usage,
    ) {
        let mut effects = Vec::new();
        if self
            .fold
            .step(
                Event::Inferred {
                    at: Timestamp::now(),
                    who,
                    purpose,
                    usage,
                },
                Timestamp::now(),
                &mut effects,
            )
            .is_err()
        {
            self.broken = Some("inferred fold step rejected".into());
            return;
        }
        self.execute(effects, None).await;
    }
}

/// Builds a current-thread runtime for bridging `!Send` store futures.
fn local_runtime() -> Result<tokio::runtime::Runtime, AgentError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| AgentError::Invalid(ValidationError::new(error.to_string())))
}
/// Core attribution for deadline and cancellation resolutions.
fn core_client() -> ClientId {
    ClientId::new("core")
}

/// Saturates a fold queue count into view stats.
fn queue_count(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// Collects the intact read views a settled outcome delivered (R06).
///
/// Search hits carry their source views; a views bundle repeats each one.
fn read_views(outcome: &dal_core::SettledOutcome) -> Vec<dal_core::ext::ReadView> {
    use dal_core::ext::{ToolData, ViewNode};
    let dal_core::SettledOutcome::Ok {
        data: Some(data), ..
    } = outcome
    else {
        return Vec::new();
    };
    let mut views = Vec::new();
    match data {
        ToolData::Read(view) => views.push(view.clone()),
        ToolData::Search(page) => views.extend(page.matches.iter().map(|hit| hit.source.clone())),
        ToolData::Views(list) => views.extend(list.iter().cloned()),
        ToolData::Display(node) => match node {
            ViewNode::Source(view) => views.push(view.clone()),
            ViewNode::Group(nodes) => {
                views.extend(nodes.iter().filter_map(|node| match node {
                    ViewNode::Source(view) => Some(view.clone()),
                    _ => None,
                }));
            }
            _ => {}
        },
        ToolData::Find(_) | ToolData::Symbols(_) => {}
    }
    views
}

/// Whether a journal failure leaves the session unwritable. Mirrors
/// `dal_store`'s `is_irreversible`: blob corruption and unrepairable journal
/// damage break the session; refused or cleanly rolled-back writes do not.
fn breaks_session(error: &dal_store::StoreError) -> bool {
    matches!(
        error,
        dal_store::StoreError::Blob(_)
            | dal_store::StoreError::Journal(
                dal_store::JournalError::Damaged { .. }
                    | dal_store::JournalError::ShardClosed { .. }
            )
            | dal_store::StoreError::Broken { .. }
    )
}

/// Maps a fold rejection to the agent error contract.
fn map_rejection(rejection: Rejection, id: SessionId, control: &ControlCell) -> AgentError {
    match rejection {
        Rejection::WrongTurn { expected, actual } => AgentError::WrongTurn {
            expected: crate::error::ExpectedTurn::from(expected),
            actual: map_turn_state(actual, control),
        },
        Rejection::SessionClosed => AgentError::SessionClosed { id },
        Rejection::BusyTurn => AgentError::Invalid(ValidationError::busy_turn(
            "command",
            control.running().unwrap_or(TurnId::new(NonZeroU64::MIN)),
        )),
        Rejection::Compacting => AgentError::Invalid(ValidationError::compacting()),
        Rejection::SteerFull => AgentError::Invalid(ValidationError::new(
            "steer queue is full (16); wait for the next step or cancel.",
        )),
        Rejection::Denied { reason } => AgentError::Invalid(ValidationError::new(format!(
            "command denied: {}",
            deny_text(&reason)
        ))),
        Rejection::Invalid { reason } => AgentError::Invalid(ValidationError::new(reason)),
    }
}

/// Maps fold turn state onto the mismatch report.
fn map_turn_state(state: TurnState, control: &ControlCell) -> ActualTurn {
    match state {
        TurnState::Idle => ActualTurn::Idle,
        TurnState::Running { turn } => ActualTurn::Turn {
            turn,
            phase: TurnPhase::Running,
        },
        TurnState::Settling { turn } => ActualTurn::Turn {
            turn,
            phase: TurnPhase::Settling,
        },
        TurnState::Compacting { .. } => {
            control
                .running()
                .map_or(ActualTurn::Idle, |turn| ActualTurn::Turn {
                    turn,
                    phase: TurnPhase::Compacting,
                })
        }
    }
}
