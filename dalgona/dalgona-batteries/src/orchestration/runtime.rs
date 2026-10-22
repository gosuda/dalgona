// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;

use dal_agent::error::ServiceError;
use dal_agent::ext::{
    BoxFuture, Caller, Hook, HookCx, HookError, ObserveHook, Scope, Services, StatusCx, StatusPoll,
    StatusSnapshot,
};
use dal_core::ext::{
    BeforeTurn, InputEvent, InputVerdict, SessionEnd, SessionStart, Settled, ToolCallEvent,
    ToolCallVerdict, ToolResultEvent, TurnEnd,
};
use dal_core::{
    AgentReport, AgentState, AgentsOp, AgentsReply, ArtifactFile, Budget, CallId, EntryId,
    ExitStatusKind, JobId, JobOutcome, JobStateView, JobsOp, JobsReply, Name, Notice, OnError,
    RawJson, RunOutput, RunRequest, ScopeSpec, SessionId, SidecarOp, Stop, TurnOp,
};
use sonic_rs::JsonContainerTrait;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant as TokioInstant, timeout_at};
use tokio_util::sync::CancellationToken;

use super::agents_tool::{
    REPORT_TOOL_NAME, Report, ReportCell, ReportOutcome, ReportStatus, submit,
};
use super::arbiter::Arbiter;
use super::goal::adapter::{self, GoalStore};
use super::goal::ops::{GoalScope, TodoSummary, format_duration};
use super::monitor::state::{MonitorConfig, MonitorState};
use super::monitor::status::{InflightCounts, StatusGate, StatusPayload, status_line};
use super::pool::{
    ChildDecision, ChildEnd, GRACE_SECONDS, GraceCause, IndexCollector, StopReason, TaskResult,
    TaskState, decide, decide_grace, grace_text, last_message_text,
};
use super::stuck::{
    GuardState, GuardVerdict, SleepClassifier, clear_pending_attempts, on_tool_call, reset,
    rewrite_exec_args,
};
use super::worktree::{Base, IsolationRefusal};
use super::{JobsView, OrchestrationConfig, StopKind};
/// Maximum number of list-and-cancel passes during descendant shutdown.
pub(crate) const CANCEL_SWEEP_PASS_LIMIT: usize = 64;

/// Name the jobs service records one workflow run under.
const RUN_JOB_NAME: &str = "agents-run";

/// Name the jobs service records one workflow task under.
const TASK_JOB_NAME: &str = "agents-task";

/// Deadline for one git call of the isolation backend.
const GIT_CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// Stdout bytes one git call keeps: the largest prefix the run service's
/// spawn door accepts, `MAX_STDOUT_PREFIX_BYTES` in `dal-agent`'s `proc`
/// module. The door refuses a larger request before launching, so every git
/// call of the isolation backend and of salvage would fail at spawn. Output
/// past the limit sets `stdout_prefix_overflowed`, which `complete` turns
/// into an unread call. The tests drive the real host to pin this value to
/// the cap the door enforces.
const GIT_STDOUT_PREFIX_LIMIT: u32 = 262_145;

/// File of a task worktree's creation receipt that records the base commit
/// the tree started from: `<data root>/isolation/<session>/<task>/base.txt`.
/// Open salvage captures a lost tree's work against this commit, so commits
/// the task made inside its tree are kept with its edits.
const RECEIPT_BASE_FILE: &str = "base.txt";

/// Skip reason of a step the run never started because it was cancelled.
const CANCELLED_SKIP: &str = "the run was cancelled";

mod merge_locks;

#[cfg(test)]
mod tests;

use merge_locks::MergeLocks;

/// The report cells of child sessions by session id.
type ReportCells = Arc<Mutex<HashMap<SessionId, ReportCell>>>;

#[derive(Clone)]
pub(crate) struct Runtime {
    owners: Arc<Mutex<HashMap<SessionId, Owner>>>,
    /// The read-only view of every session this runtime owns, published by
    /// each owner task and handed to the batteries that read it.
    view: super::live::View,
    /// One merge lock per workspace path, shared by every session on it.
    merge_locks: MergeLocks,
    /// The report cell of each live child session, read by its parent's run
    /// when the child ends.
    reports: ReportCells,
    config: Arc<OrchestrationConfig>,
    sleep: Option<Arc<SleepClassifier>>,
    /// The test parking gate the owner consults before serving a `jobs`
    /// list. Tests arm it to hold the owner while producers flood the
    /// mailbox; production never arms it.
    #[cfg(test)]
    jobs_parking: Arc<ParkingGate>,
}

struct Owner {
    sender: mpsc::Sender<Message>,
    snapshot: Arc<Mutex<StatusGate>>,
    _run: tokio_util::task::AbortOnDropHandle<()>,
    /// The deepest this session's mailbox ever filled, sampled at the real
    /// receive boundary. Tests read it; production never does.
    #[cfg(test)]
    mailbox_pressure: Arc<MailboxPressure>,
}

/// Test-only gauge of one owner mailbox: the deepest the channel ever
/// filled while the owner drained it. The bound itself comes from the
/// channel; this records how close a run came to it.
#[cfg(test)]
#[derive(Default)]
struct MailboxPressure {
    fill_peak: AtomicUsize,
}

/// Test-only single-shot parking gate on the owner side of the `jobs`
/// service list. Tests arm it to park the owner mid-message so producers
/// flood the mailbox past capacity; production never arms it. Exactly one
/// waiter parks at a time because the owner handles one message at a time.
#[cfg(test)]
#[derive(Default)]
pub(super) struct ParkingGate {
    armed: AtomicBool,
    parked: AtomicUsize,
    wake: tokio::sync::Notify,
}

#[cfg(test)]
impl ParkingGate {
    /// Parks the owner at the next `jobs` list it serves.
    pub(super) fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }

    /// Serves the list, or parks until the test releases the owner. A
    /// release that lands between the arm check and the wait leaves a
    /// permit, so the waiter can never miss it.
    pub(super) async fn pass(&self) {
        if !self.armed.load(Ordering::SeqCst) {
            return;
        }
        self.parked.fetch_add(1, Ordering::SeqCst);
        self.wake.notified().await;
    }

    /// How many owners parked so far; the flood tests wait for at least one.
    pub(super) fn parked(&self) -> usize {
        self.parked.load(Ordering::SeqCst)
    }

    /// Lets the parked owner through once; later lists run unparked. The
    /// single permit survives a release that lands before the waiter waits,
    /// so one release always frees the one parked owner.
    pub(super) fn release(&self) {
        self.armed.store(false, Ordering::SeqCst);
        self.wake.notify_one();
    }
}

enum Message {
    Input {
        reply: oneshot::Sender<Result<(), ServiceError>>,
        /// The prompt's opening token, carried so the owner can drop an
        /// already-cancelled prompt before mutating any state. A prompt the
        /// waiter gave up on (user cancel or shutdown) must leave the
        /// battery exactly as the cancel-before-send path does.
        cancel: CancellationToken,
    },
    BeforeTurn(oneshot::Sender<()>),
    ToolCall(ToolCallEvent, oneshot::Sender<ToolCallVerdict>),
    Tool {
        caller: Caller,
        call: CallId,
        name: Box<str>,
        args: RawJson,
        reply: oneshot::Sender<Result<String, ServiceError>>,
    },
    ToolResult(ToolResultEvent, oneshot::Sender<()>),
    TurnEnd(TurnEnd, oneshot::Sender<()>),
    Settled(Settled, oneshot::Sender<Result<(), ServiceError>>),
    Command {
        name: Box<str>,
        args: Box<str>,
        reply: oneshot::Sender<Result<String, ServiceError>>,
    },
    RunTask {
        run: JobId,
        task: JobId,
    },
    RunEnd {
        run: JobId,
    },
    ReserveChildren {
        count: usize,
        reply: oneshot::Sender<Result<(), u32>>,
    },
    Close {
        deadline: TokioInstant,
        reply: oneshot::Sender<()>,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReportArgs {
    status: String,
    report: String,
}

/// One granted continuation: the progress signature and prompt kind that the
/// goal counters record once, when the continuation is delivered.
#[derive(Clone)]
struct GoalGrant {
    signature: String,
    prompt: super::goal::policy::PromptKind,
}
/// Pre-delivery goal state kept until a wake is acknowledged, so a refused
/// wake can restore both the counters and the grant.
struct GoalCheckpoint {
    goal: Option<GoalStore>,
    grant: Option<GoalGrant>,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "the owner task's latches are flat flags: guard cancellation, turn tool use, turn activity, prompt provenance, and the recovery arm"
)]
struct SessionState {
    session: SessionId,
    parent: Option<SessionId>,
    caller: Caller,
    services: Arc<dyn Services>,
    sender: mpsc::Sender<Message>,
    receiver: mpsc::Receiver<Message>,
    workspace: PathBuf,
    snapshot: Arc<Mutex<StatusGate>>,
    config: Arc<OrchestrationConfig>,
    guard: GuardState,
    guard_cancel: bool,
    arbiter: Arbiter,
    sleep: Option<Arc<SleepClassifier>>,
    goal: Option<GoalStore>,
    report: ReportCell,
    monitors: MonitorState,
    open_asks: HashSet<CallId>,
    inflight_jobs: usize,
    /// Live jobs silent past the stuck threshold, carried in the payload.
    silent_jobs: usize,
    runs: HashMap<JobId, RunHandle>,
    /// Ended reports taken from the host but not yet delivered: the wake
    /// rule ages them here instead of taking and releasing every tick.
    held: Vec<super::arbiter::JobReport>,
    run_reports: HashSet<JobId>,
    children_started: u32,
    monitor_wake_ids: Vec<super::monitor::state::MonitorId>,
    line_cursors: HashMap<JobId, u64>,
    last_stop: StopKind,
    turn_tool_called: bool,
    turn_active: bool,
    /// A user prompt opened the current turn; consumed by `before_turn`.
    prompt_seen: bool,
    /// The active or last-settled turn was started by a user prompt.
    turn_user_started: bool,
    /// The prompt that reactivated a provider-error block runs the next
    /// verdict on the Recovery path.
    goal_recovery: bool,
    goal_timer: Option<(TokioInstant, String)>,
    /// The continuation granted by a verdict and not yet delivered, dropped,
    /// or voided. It carries what delivery records.
    goal_grant: Option<GoalGrant>,
    /// The grant for the services the delivery poll uses was refused; the
    /// poll stays off until a prompt, command, or tool call.
    delivery_refused: bool,
    /// Job reports a delivered wake carried whose acknowledgement the host
    /// has not confirmed. A wake that reached the session is never repeated;
    /// each tick retries only this acknowledgement.
    unconfirmed_jobs: Vec<Unconfirmed>,
    /// The last delivery-poll failure already reported to the owner; a
    /// repeated failure with the same cause is not reported again.
    last_delivery_error: Option<String>,
    /// The last monitor-poll failure already reported to the owner; a
    /// repeated failure with the same cause is not reported again.
    last_monitor_error: Option<String>,
    /// The last turn ended because the context window overflowed.
    last_turn_overflowed: bool,
    /// Shared with every run of this session, so patches reach one checkout
    /// one at a time.
    merge_locks: MergeLocks,
    /// The cross-battery projection cell this owner publishes to.
    projection: super::live::Cell,
    /// The report cells of the child sessions this runtime hosts.
    reports: ReportCells,
    /// Waits the owner answers without blocking its message loop; dropping
    /// the state aborts them.
    waits: tokio::task::JoinSet<()>,
    /// The deepest this mailbox ever filled, sampled at the real receive
    /// boundary and at the test parking gate. Tests read it; production
    /// never does.
    #[cfg(test)]
    mailbox_pressure: Arc<MailboxPressure>,
    /// The test parking gate this owner consults before serving a `jobs`
    /// list. Tests arm it to hold the owner while producers flood the
    /// mailbox; production never arms it.
    #[cfg(test)]
    jobs_parking: Arc<ParkingGate>,
}

/// A delivered job report the host has not confirmed. `omitted` records that
/// a successful commit already left the id out of its reply.
struct Unconfirmed {
    id: JobId,
    omitted: bool,
}

/// One live workflow run the owner task tracks on behalf of its
/// coordinator. The token cancels the run; the control channel carries
/// per-task cancels; dropping the handle aborts a wedged coordinator.
struct RunHandle {
    cancel: CancellationToken,
    control: mpsc::Sender<RunControl>,
    tasks: HashSet<JobId>,
    base: Option<Base>,
    _coordinator: tokio_util::task::AbortOnDropHandle<()>,
}

/// A coordinator control message.
#[derive(Clone, Copy)]
enum RunControl {
    CancelTask(JobId),
}

/// The owner's clock for arbitration and status timing: tokio's clock in its
/// standard form, so paused test time advances the wake and quiet rules.
fn now() -> Instant {
    TokioInstant::now().into_std()
}

impl Runtime {
    pub(crate) fn new(config: OrchestrationConfig) -> Result<Self, super::RegistrationError> {
        let sleep = if config.sleep.enabled {
            Some(Arc::new(
                SleepClassifier::new().map_err(|_| super::RegistrationError::InvalidParameters)?,
            ))
        } else {
            None
        };
        Ok(Self {
            owners: Arc::new(Mutex::new(HashMap::new())),
            view: super::live::View::default(),
            merge_locks: MergeLocks::default(),
            reports: Arc::new(Mutex::new(HashMap::new())),
            config: Arc::new(config),
            sleep,
            #[cfg(test)]
            jobs_parking: Arc::new(ParkingGate::default()),
        })
    }

    pub(crate) fn config(&self) -> &OrchestrationConfig {
        &self.config
    }

    fn sender(&self, session: SessionId) -> Option<mpsc::Sender<Message>> {
        self.owners
            .lock()
            .ok()
            .and_then(|owners| owners.get(&session).map(|owner| owner.sender.clone()))
    }

    async fn hook_request<T>(
        &self,
        session: SessionId,
        deadline: TokioInstant,
        cancel: tokio_util::sync::CancellationToken,
        build: impl FnOnce(oneshot::Sender<T>) -> Message,
    ) -> Result<T, HookError>
    where
        T: Send + 'static,
    {
        let Some(sender) = self.sender(session) else {
            return Err(failed("orchestration owner is not running"));
        };
        let (reply, response) = oneshot::channel();
        let message = build(reply);
        tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(HookError::Cancelled),
            sent = timeout_at(deadline, sender.send(message)) => match sent {
                Ok(Ok(())) => {}
                Ok(Err(_)) => return Err(failed("orchestration owner is closed")),
                Err(_) => return Err(failed("orchestration hook deadline exceeded")),
            }
        }
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(HookError::Cancelled),
            received = timeout_at(deadline, response) => match received {
                Ok(Ok(value)) => Ok(value),
                Ok(Err(_)) => Err(failed("orchestration owner is closed")),
                Err(_) => Err(failed("orchestration hook deadline exceeded")),
            }
        }
    }

    pub(crate) async fn tool(
        &self,
        session: SessionId,
        caller: Caller,
        call: CallId,
        name: &str,
        args: RawJson,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<String, ServiceError> {
        let Some(sender) = self.sender(session) else {
            return Err(ServiceError::failed(
                None,
                "orchestration owner is not running",
            ));
        };
        let (reply, response) = oneshot::channel();
        let message = Message::Tool {
            caller,
            call,
            name: name.into(),
            args,
            reply,
        };
        let deadline = TokioInstant::now() + std::time::Duration::from_secs(600);
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(ServiceError::Cancelled),
            sent = timeout_at(deadline, sender.send(message)) => match sent {
                Ok(Ok(())) => {
                    tokio::select! {
                        biased;
                        () = cancel.cancelled() => Err(ServiceError::Cancelled),
                        received = timeout_at(deadline, response) => match received {
                            Ok(Ok(result)) => result,
                            Ok(Err(_)) => Err(ServiceError::failed(None, "orchestration owner is closed")),
                            Err(_) => Err(ServiceError::failed(None, "orchestration tool deadline exceeded")),
                        }
                    }
                }
                Ok(Err(_)) => Err(ServiceError::failed(None, "orchestration owner is closed")),
                Err(_) => Err(ServiceError::failed(None, "orchestration tool deadline exceeded")),
            }
        }
    }

    pub(crate) async fn command(
        &self,
        session: SessionId,
        name: &str,
        args: &str,
    ) -> Result<String, ServiceError> {
        let Some(sender) = self.sender(session) else {
            return Err(ServiceError::failed(
                None,
                "orchestration owner is not running",
            ));
        };
        let (reply, response) = oneshot::channel();
        let message = Message::Command {
            name: name.into(),
            args: args.into(),
            reply,
        };
        let deadline = TokioInstant::now() + std::time::Duration::from_secs(60);
        timeout_at(deadline, sender.send(message))
            .await
            .map_err(|_| ServiceError::failed(None, "orchestration command deadline exceeded"))?
            .map_err(|_| ServiceError::failed(None, "orchestration owner is closed"))?;
        timeout_at(deadline, response)
            .await
            .map_err(|_| ServiceError::failed(None, "orchestration command deadline exceeded"))?
            .map_err(|_| ServiceError::failed(None, "orchestration owner is closed"))?
    }

    /// Salvages this session's lost worktrees before the owner starts.
    /// Filesystem-gated: without leftover directories this makes no service
    /// call. Split from `open` so the session setup stays under the
    /// function-length cap; the call order is unchanged.
    async fn salvage_session_open(
        &self,
        session: SessionId,
        workspace: &Path,
        caller: &Caller,
        services: &dyn Services,
    ) {
        if let Some(root) = self.config.data_root.as_ref() {
            salvage_open_worktrees(services, caller, session, workspace, root).await;
        }
    }

    async fn open(&self, start: SessionStart, cx: HookCx) -> Result<(), HookError> {
        let session = cx.session;
        let parent = cx.parent;
        let caller = cx.caller.clone();
        let services = Arc::clone(&cx.services);
        self.salvage_session_open(
            session,
            start.workspace.as_path(),
            &caller,
            services.as_ref(),
        )
        .await;
        if parent.is_some() && self.config.agents.enabled {
            let report_tool =
                super::tools::report_tool(self).map_err(|error| failed(&error.to_string()))?;
            services
                .add_session_tools(&caller, vec![report_tool])
                .await
                .map_err(|error| failed(&error.to_string()))?;
        }
        let report = ReportCell::default();
        if parent.is_some() {
            self.reports
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(session, report.clone());
        }
        let (sender, receiver) = mpsc::channel(super::constants::BATTERY_MAILBOX);
        let snapshot = Arc::new(Mutex::new(StatusGate::new()));
        #[cfg(test)]
        let mailbox_pressure = Arc::new(MailboxPressure::default());
        let state = SessionState {
            session,
            parent,
            caller,
            services: Arc::clone(&services),
            sender: sender.clone(),
            receiver,
            workspace: start.workspace.as_path().to_path_buf(),
            snapshot: Arc::clone(&snapshot),
            config: Arc::clone(&self.config),
            guard: GuardState::default(),
            guard_cancel: false,
            arbiter: Arbiter::new(),
            sleep: self.sleep.clone(),
            goal: None,
            report,
            monitors: MonitorState::default(),
            open_asks: HashSet::new(),
            inflight_jobs: 0,
            silent_jobs: 0,
            runs: HashMap::new(),
            held: Vec::new(),
            run_reports: HashSet::new(),
            children_started: 0,
            monitor_wake_ids: Vec::new(),
            line_cursors: HashMap::new(),
            last_stop: StopKind::Completed,
            turn_tool_called: false,
            turn_active: false,
            prompt_seen: false,
            turn_user_started: false,
            goal_recovery: false,
            goal_timer: None,
            goal_grant: None,
            delivery_refused: false,
            unconfirmed_jobs: Vec::new(),
            last_delivery_error: None,
            last_monitor_error: None,
            last_turn_overflowed: false,
            merge_locks: self.merge_locks.clone(),
            projection: self.view.open(session),
            reports: Arc::clone(&self.reports),
            waits: tokio::task::JoinSet::new(),
            #[cfg(test)]
            mailbox_pressure: Arc::clone(&mailbox_pressure),
            #[cfg(test)]
            jobs_parking: Arc::clone(&self.jobs_parking),
        };
        #[expect(
            clippy::disallowed_methods,
            reason = "the session registry owns and cancels this per-session task"
        )]
        let run = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(state.run()));
        if let Ok(mut owners) = self.owners.lock() {
            owners.insert(
                session,
                Owner {
                    sender,
                    snapshot,
                    _run: run,
                    #[cfg(test)]
                    mailbox_pressure,
                },
            );
        }
        Ok(())
    }

    async fn close(&self, session: SessionId, _end: SessionEnd) {
        if let Some(sender) = self.sender(session) {
            let (reply, response) = oneshot::channel();
            let deadline = TokioInstant::now() + Duration::from_secs(4);
            if sender
                .send(Message::Close { deadline, reply })
                .await
                .is_ok()
            {
                let _ = response.await;
            }
        }
        if let Ok(mut owners) = self.owners.lock() {
            owners.remove(&session);
        }
        self.view.close(session);
        self.reports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&session);
    }

    pub(crate) fn view(&self) -> super::live::View {
        self.view.clone()
    }

    #[cfg(test)]
    fn snapshot(&self, session: SessionId) -> Option<StatusSnapshot> {
        let owners = self.owners.lock().ok()?;
        let owner = owners.get(&session)?;
        owner.snapshot.lock().ok().map(|gate| gate.latest().clone())
    }

    /// The shared parking gate of this runtime's owners, for the tests that
    /// hold an owner while producers flood its mailbox.
    #[cfg(test)]
    pub(super) fn parking_gate(&self) -> Arc<ParkingGate> {
        Arc::clone(&self.jobs_parking)
    }

    /// How full one session's mailbox is right now: capacity minus remaining
    /// permits. The flood tests watch this reach zero while the owner parks.
    #[cfg(test)]
    pub(super) fn mailbox_fill_now(&self, session: SessionId) -> Option<usize> {
        self.sender(session)
            .map(|sender| super::constants::BATTERY_MAILBOX.saturating_sub(sender.capacity()))
    }

    /// The deepest one session's mailbox ever filled while its owner drained
    /// it. The stress tests bound this by the channel capacity after proving
    /// every queued delivery still landed exactly once.
    #[cfg(test)]
    pub(super) fn mailbox_fill_peak(&self, session: SessionId) -> Option<usize> {
        let owners = self.owners.lock().ok()?;
        let owner = owners.get(&session)?;
        Some(owner.mailbox_pressure.fill_peak.load(Ordering::SeqCst))
    }

    fn poll(&self, session: SessionId) -> Option<StatusSnapshot> {
        let owners = self.owners.lock().ok()?;
        let owner = owners.get(&session)?;
        owner.snapshot.lock().ok().map(|mut gate| gate.emit(now()))
    }
}

impl SessionState {
    async fn run(mut self) {
        if self.config.goal.enabled && self.parent.is_none() {
            self.load_goal().await;
        }
        self.publish_status();
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(250));
        loop {
            tokio::select! {
                message = self.receiver.recv() => {
                    let Some(message) = message else {
                        return;
                    };
                    #[cfg(test)]
                    self.note_fill();
                    if self.handle_message(message).await {
                        return;
                    }
                }
                _ = ticker.tick() => {
                    self.tick().await;
                }
            }
        }
    }

    async fn handle_message(&mut self, message: Message) -> bool {
        match message {
            Message::Input { reply, cancel } => {
                // The waiter may have given up while this prompt sat queued
                // (user cancel or shutdown). Such a prompt is a no-op: the
                // owner applies no battery state at all, matching the
                // cancel-before-send path, and sends nothing because the
                // receiver is already dropped. A deadline-expired prompt
                // still runs: the core admits it either way.
                if !cancel.is_cancelled() {
                    let result = self.input().await;
                    self.publish_status();
                    let _ = reply.send(result);
                }
            }
            Message::BeforeTurn(reply) => self.before_turn(reply),
            Message::ToolCall(event, reply) => {
                if event.tool.as_str() == "ask" {
                    self.open_asks.insert(event.call.clone());
                }
                self.turn_tool_called = true;
                let verdict = self.tool_call(event).await;
                self.publish_status();
                let _ = reply.send(verdict);
            }
            Message::Tool {
                caller,
                call,
                name,
                args,
                reply,
            } => match self.tool(caller, call, &name, &args).await {
                ToolReply::Done(result) => {
                    self.publish_status();
                    let _ = reply.send(result);
                }
                ToolReply::Wait(plan) => {
                    while self.waits.try_join_next().is_some() {}
                    self.waits.spawn(async move {
                        let _ = reply.send(wait_for_jobs(plan).await);
                    });
                    self.publish_status();
                }
            },
            Message::ToolResult(event, reply) => {
                self.open_asks.remove(&event.call);
                self.publish_status();
                let _ = reply.send(());
            }
            Message::TurnEnd(event, reply) => {
                if self.config.loop_guard.enabled {
                    clear_pending_attempts(&mut self.guard);
                }
                if event.stop == dal_core::Stop::Cancelled && !self.guard_cancel {
                    self.arbiter.on_user_cancel();
                }
                if event.stop == dal_core::Stop::Failed && !event.overflowed {
                    self.block_goal_on_provider_error().await;
                }
                self.last_stop = stop_kind(event.stop);
                self.last_turn_overflowed = event.overflowed;
                self.guard_cancel = false;
                self.turn_active = false;
                self.publish_status();
                let _ = reply.send(());
            }
            Message::Settled(event, reply) => {
                let result = self.settled(event).await;
                self.publish_status();
                let _ = reply.send(result);
            }
            Message::Command { name, args, reply } => {
                let result = self.command(&name, &args).await;
                self.publish_status();
                let _ = reply.send(result);
            }
            Message::RunTask { run, task } => {
                if let Some(handle) = self.runs.get_mut(&run) {
                    handle.tasks.insert(task);
                }
            }
            Message::RunEnd { run } => {
                self.runs.remove(&run);
                self.inflight_jobs = self.inflight_jobs.saturating_sub(1);
            }
            Message::ReserveChildren { count, reply } => {
                let left =
                    super::admission::AGENTS_PER_SESSION.saturating_sub(self.children_started);
                let result = match u32::try_from(count) {
                    Ok(count) if count <= left => {
                        self.children_started += count;
                        Ok(())
                    }
                    _ => Err(left),
                };
                let _ = reply.send(result);
            }
            Message::Close { deadline, reply } => {
                self.close(deadline).await;
                let _ = reply.send(());
                return true;
            }
        }
        false
    }

    async fn tick(&mut self) {
        self.void_stale_goal_grant();
        if let Some((deadline, prompt)) = self.goal_timer.as_ref()
            && TokioInstant::now() >= *deadline
        {
            let prompt = prompt.clone();
            self.goal_timer = None;
            self.arbiter.admit_goal(prompt, now());
        }
        let _ = self.poll_monitors().await;
        self.retry_job_acknowledgement().await;
        match self.deliver_ready().await {
            Ok(()) => self.last_delivery_error = None,
            Err(error) => self.report_delivery_error(error.to_string()),
        }
        self.publish_status();
    }

    async fn idle_for_delivery(&mut self) -> Result<bool, ServiceError> {
        match self.services.turn(&self.caller, TurnOp::IsIdle).await {
            Ok(dal_core::TurnOpReply::Idle(idle)) => Ok(idle),
            Ok(_) => Ok(false),
            Err(error) => {
                // A refused grant stays refused until the user acts again:
                // polling every tick would raise the same question forever.
                self.delivery_refused =
                    matches!(error, ServiceError::Denied(_) | ServiceError::Declined);
                Err(error)
            }
        }
    }

    async fn deliver_ready(&mut self) -> Result<(), ServiceError> {
        self.void_stale_goal_grant();
        if self.arbiter.mode() != super::ControllerMode::Run {
            return Ok(());
        }
        if self.delivery_refused || !self.open_asks.is_empty() {
            return Ok(());
        }
        if !self.idle_for_delivery().await? {
            return Ok(());
        }
        let _ = self.jobs_list().await?;
        let fresh = self.completed_jobs().await?;
        let fresh_ids: Vec<JobId> = fresh.iter().map(|report| report.id).collect();
        for report in fresh {
            self.hold_report(report);
        }
        let held_ids: Vec<JobId> = self.held.iter().map(|report| report.id).collect();
        self.arbiter.note_ended(&held_ids, now());
        let jobs = if self.arbiter.jobs_wake_due(self.inflight_jobs, now()) {
            std::mem::take(&mut self.held)
        } else {
            Vec::new()
        };
        // A wake for another source evaluates the goal verdict on the Idle
        // path; a granted continuation joins this wake's injection, and a
        // recovery text keeps it waiting for the next wake.
        if self.arbiter.wake_has_other_sources(&jobs)
            && let Err(error) = self.evaluate_idle_goal().await
        {
            // Restore before releasing: a failed release must not strand
            // reports the host still counts as taken. Extend rather than
            // assign: a wake that is not due leaves `jobs` empty while the
            // waiting reports are still in `held`.
            self.held.extend(jobs);
            self.release_reports(fresh_ids).await?;
            return Err(error);
        }
        let attempted: Vec<JobId> = jobs.iter().map(|report| report.id).collect();
        let ready = self.arbiter.collect(jobs);
        if ready.is_empty() {
            return Ok(());
        }
        let (text, sources, job_ids, monitor_batches) = self
            .arbiter
            .compose(&ready, super::arbiter::INJECTION_BUDGET);
        let omitted = attempted
            .into_iter()
            .filter(|id| !job_ids.contains(id))
            .collect();
        if let Err(error) = self.release_reports(omitted).await {
            // `collect` drained the arbiter and `jobs` came out of `held`; no
            // report was released, so the host never offers them again. Put
            // all of it back for the next tick before reporting the failure.
            self.requeue_ready(&ready);
            self.rehold_jobs(&ready);
            return Err(error);
        }
        if text.is_empty() {
            self.requeue_ready(&ready);
            return Ok(());
        }
        let monitor_only = !sources.is_empty() && sources.iter().all(|source| *source == "monitor");
        let carries_goal = sources.contains(&"goal");
        let checkpoint = match self.checkpoint_goal_delivery(carries_goal).await {
            Ok(checkpoint) => checkpoint,
            Err(error) => {
                self.arbiter.release(&job_ids);
                self.release_and_requeue(&ready, job_ids).await?;
                return Err(error);
            }
        };
        let sources = sources.into_iter().map(Into::into).collect();
        let delivered = self
            .services
            .turn(
                &self.caller,
                TurnOp::Wake {
                    text: text.into_boxed_str(),
                    sources,
                    job_ids: job_ids.clone(),
                },
            )
            .await;
        match delivered {
            Ok(dal_core::TurnOpReply::Woken) => {
                self.commit_delivered_wake(&ready, job_ids, monitor_only, monitor_batches)
                    .await;
            }
            Ok(_) => {
                self.abandon_wake(&ready, job_ids, checkpoint, carries_goal, None)
                    .await?;
            }
            Err(ServiceError::Denied(dal_core::DenyReason::WakeLimit)) => {
                self.arbiter.stop();
                self.abandon_wake(&ready, job_ids, checkpoint, carries_goal, None)
                    .await?;
            }
            Err(error) => {
                self.abandon_wake(&ready, job_ids, checkpoint, carries_goal, Some(error))
                    .await?;
            }
        }
        Ok(())
    }

    /// Commits the job reports a delivered wake carried and returns the ids
    /// the host newly marked delivered.
    async fn commit_wake_jobs(&self, job_ids: &[JobId]) -> Result<Vec<JobId>, ServiceError> {
        match self
            .services
            .jobs(
                &self.caller,
                JobsOp::Commit {
                    ids: job_ids.to_vec(),
                },
            )
            .await?
        {
            JobsReply::Committed { ids } if ids.iter().all(|id| job_ids.contains(id)) => Ok(ids),
            JobsReply::Committed { .. } => Err(ServiceError::failed(
                None,
                "the host committed a different set of job reports",
            )),
            _ => Err(ServiceError::failed(
                None,
                "job reports could not be committed",
            )),
        }
    }

    /// Acknowledges the reports of a wake that already reached the session.
    /// An id stays pending until the host confirms it, so a failed or partial
    /// acknowledgement is retried without repeating the wake. A repeated
    /// commit returns none, so an id the host omits twice is not held and
    /// settles. The first failure of a wake raises a notice.
    async fn acknowledge_jobs(&mut self, pending: Vec<Unconfirmed>, announce: bool) {
        if pending.is_empty() {
            return;
        }
        let ids: Vec<JobId> = pending.iter().map(|job| job.id).collect();
        let (kept, cause) = match self.commit_wake_jobs(&ids).await {
            Ok(committed) => {
                let kept: Vec<Unconfirmed> = pending
                    .into_iter()
                    .filter(|job| !committed.contains(&job.id))
                    .filter(|job| !job.omitted)
                    .map(|job| Unconfirmed {
                        id: job.id,
                        omitted: true,
                    })
                    .collect();
                (kept, "the host confirmed only part of them".to_owned())
            }
            Err(error) => (pending, error.to_string()),
        };
        if kept.is_empty() {
            return;
        }
        if announce {
            self.services.notify(
                &self.caller,
                Notice {
                    turn: None,
                    kind: "orchestration.delivery".into(),
                    text: format!(
                        "{} job report(s) reached the session but are not yet confirmed as delivered: {cause}. The acknowledgement is retried; the wake is not repeated.",
                        kept.len()
                    )
                    .into(),
                },
            );
        }
        self.unconfirmed_jobs.extend(kept);
    }

    async fn retry_job_acknowledgement(&mut self) {
        let pending = std::mem::take(&mut self.unconfirmed_jobs);
        self.acknowledge_jobs(pending, false).await;
    }
    /// Reports a delivery-poll failure to the owner once per distinct cause.
    /// The poll retries every tick, so repeating the same text would spam.
    fn report_delivery_error(&mut self, cause: String) {
        if self.last_delivery_error.as_deref() == Some(cause.as_str()) {
            return;
        }
        self.services.notify(
            &self.caller,
            Notice {
                turn: None,
                kind: "orchestration.delivery".into(),
                text: format!("automatic delivery failed: {cause}. It will be retried.").into(),
            },
        );
        self.last_delivery_error = Some(cause);
    }

    /// A wake for another source evaluates the goal verdict on the Idle
    /// path. A granted continuation joins that wake's injection, behind a
    /// recovery text when one waits. The wake turn itself records its goal
    /// accounting at its own end, so this admission records nothing.
    async fn evaluate_idle_goal(&mut self) -> Result<(), ServiceError> {
        let active = self
            .goal
            .as_ref()
            .filter(|store| store.error.is_none())
            .and_then(|store| store.sidecar.as_ref())
            .and_then(|sidecar| sidecar.goal.as_ref())
            .is_some_and(|goal| goal.status == super::GoalStatus::Active);
        if !active {
            return Ok(());
        }
        let todos = self.todo_summary().await?;
        let inflight = self.inflight_counts().await?;
        let signature = self.idle_signature(&todos);
        let decision = self.goal_verdict(
            super::goal::policy::GoalPath::Idle,
            &signature,
            true,
            &todos,
            &inflight,
        );
        let (prompt, stall) = match decision {
            super::goal::policy::Verdict::Continue { prompt, stall } => (prompt, stall),
            super::goal::policy::Verdict::Deny(reason) => {
                self.deny_goal(reason, dal_core::Timestamp::now());
                return Ok(());
            }
        };
        let live_parts = Self::live_parts(&inflight);
        let Some((_, prompt_text)) =
            self.build_goal_prompt(prompt, &if stall { live_parts } else { Vec::new() })
        else {
            return Ok(());
        };
        self.goal_grant = Some(GoalGrant { signature, prompt });
        self.arbiter.admit_goal(prompt_text, now());
        Ok(())
    }

    /// The signature an Idle admission records in the prompt: the last
    /// delivered signature, or a fresh one when no continuation has run.
    fn idle_signature(&self, todos: &TodoSummary) -> String {
        let goal = self
            .goal
            .as_ref()
            .and_then(|store| store.sidecar.as_ref())
            .and_then(|sidecar| sidecar.goal.as_ref());
        if let Some(stored) = goal.and_then(|goal| goal.last_signature.as_deref()) {
            return stored.to_owned();
        }
        let id = goal.map(|goal| goal.id.as_ref()).unwrap_or_default();
        super::goal::policy::progress_signature(id, todos.open, todos.total, "")
    }

    /// Applies the monitor-only wake budget after a committed wake.
    fn settle_monitor_wake(&mut self, monitor_only: bool, count: usize) {
        let config = self.monitor_config();
        let delivered: Vec<_> = self
            .monitor_wake_ids
            .drain(..count.min(self.monitor_wake_ids.len()))
            .collect();
        for effect in super::monitor::delivery::update_monitor_only_wake(
            &mut self.monitors,
            &delivered,
            monitor_only,
            &config,
        ) {
            self.apply_monitor_effect(effect);
        }
    }

    fn requeue_monitor_remainder(&mut self, ready: &[super::arbiter::Ready], count: usize) {
        for batch in ready
            .iter()
            .filter_map(|item| match item {
                super::arbiter::Ready::Monitor(batches) => Some(batches),
                _ => None,
            })
            .flatten()
            .skip(count)
        {
            self.arbiter.push_monitor(batch.clone(), now());
        }
    }

    fn monitor_config(&self) -> MonitorConfig {
        MonitorConfig {
            enabled: self.config.monitor.enabled,
            coalesce_ms: self.config.monitor.coalesce_ms,
            rate_limit_ms: self.config.monitor.rate_limit_ms,
            max_lines: self.config.monitor.max_lines,
            max_chars: self.config.monitor.max_chars,
            wake_budget: self.config.monitor.wake_budget,
        }
    }

    fn apply_monitor_effect(&mut self, effect: super::monitor::state::MonitorEffect) {
        match effect {
            super::monitor::state::MonitorEffect::Batch(batch) => {
                self.monitor_wake_ids.push(batch.monitor);
                self.arbiter.push_monitor(batch.text(), now());
            }
            super::monitor::state::MonitorEffect::Notice(text) => self.services.notify(
                &self.caller,
                Notice {
                    turn: None,
                    kind: "orchestration.monitor".into(),
                    text,
                },
            ),
            super::monitor::state::MonitorEffect::Stopped(_) => {}
        }
    }

    /// Polls every watched job's output lines and flushes deliverable
    /// monitor batches to the arbiter as P3. One job's failing read neither
    /// starves the later jobs of the same tick nor fails silently: the read
    /// is retried next tick and the failure is reported once per cause.
    async fn poll_monitors(&mut self) -> Result<(), ServiceError> {
        if !self.config.monitor.enabled {
            return Ok(());
        }
        let config = self.monitor_config();
        let mut effects = Vec::new();
        let mut failure = None;
        for job in self.monitors.job_ids() {
            if let Some(cause) = self.poll_one_monitor(job, &config, &mut effects).await {
                failure.get_or_insert(cause);
            }
        }
        effects.extend(super::monitor::delivery::flush(
            &mut self.monitors,
            dal_core::Timestamp::now(),
            &config,
        ));
        for effect in effects {
            self.apply_monitor_effect(effect);
        }
        match failure {
            Some(cause) => self.report_monitor_error(cause),
            None => self.last_monitor_error = None,
        }
        Ok(())
    }

    /// Reads one watched job's new lines into `effects`. Returns the failure
    /// cause when the read fails; the cursor stays, so the next tick retries
    /// the same read.
    async fn poll_one_monitor(
        &mut self,
        job: JobId,
        config: &MonitorConfig,
        effects: &mut Vec<super::monitor::state::MonitorEffect>,
    ) -> Option<String> {
        let read = match self.read_job_lines(job).await {
            Ok(read) => read,
            Err(error) => return Some(error.to_string()),
        };
        for line in &read.lines {
            effects.extend(super::monitor::delivery::on_output(
                &mut self.monitors,
                job,
                &line.text,
                dal_core::Timestamp::now(),
                config,
            ));
        }
        if read.ended {
            super::monitor::state::on_job_end(&mut self.monitors, job);
            self.line_cursors.remove(&job);
        }
        None
    }

    /// Reports a monitor-poll failure to the owner once per distinct cause.
    /// The poll retries every tick, so repeating the same text would spam.
    fn report_monitor_error(&mut self, cause: String) {
        if self.last_monitor_error.as_deref() == Some(cause.as_str()) {
            return;
        }
        self.services.notify(
            &self.caller,
            Notice {
                turn: None,
                kind: "orchestration.monitor".into(),
                text: format!("monitor polling failed: {cause}. It will be retried.").into(),
            },
        );
        self.last_monitor_error = Some(cause);
    }

    /// Reads one watched job's new lines without waiting.
    async fn read_job_lines(&mut self, job: JobId) -> Result<dal_core::JobLines, ServiceError> {
        let after = self.line_cursors.get(&job).copied();
        let reply = self
            .services
            .jobs(
                &self.caller,
                JobsOp::Lines {
                    id: job,
                    after,
                    timeout: Some(Duration::ZERO),
                },
            )
            .await?;
        let JobsReply::Lines(lines) = reply else {
            return Err(ServiceError::failed(
                None,
                "jobs service returned an unexpected reply to a line read",
            ));
        };
        self.line_cursors.insert(job, lines.next);
        Ok(lines)
    }

    async fn completed_jobs(&self) -> Result<Vec<super::arbiter::JobReport>, ServiceError> {
        let reply = self
            .services
            .jobs(&self.caller, JobsOp::Take { limit: 512 })
            .await?;
        let JobsReply::Taken(reports) = reply else {
            return Err(ServiceError::failed(
                None,
                "jobs service returned no ended reports",
            ));
        };
        Ok(reports
            .into_iter()
            .filter(|report| !self.arbiter.is_committed(&report.id))
            .map(|report| super::arbiter::JobReport {
                id: report.id,
                text: report.text.into(),
                from_run: self.run_reports.contains(&report.id),
            })
            .collect())
    }

    async fn release_reports(&self, ids: Vec<JobId>) -> Result<(), ServiceError> {
        if ids.is_empty() {
            return Ok(());
        }
        match self
            .services
            .jobs(&self.caller, JobsOp::Release { ids })
            .await?
        {
            JobsReply::Released { .. } => Ok(()),
            _ => Err(ServiceError::failed(
                None,
                "job reports could not be released",
            )),
        }
    }

    /// Holds a report for the next wake unless it is already held.
    fn hold_report(&mut self, report: super::arbiter::JobReport) {
        if !self.held.iter().any(|held| held.id == report.id) {
            self.held.push(report);
        }
    }

    /// Puts the job reports of `ready` back in `held`. Assembling a wake
    /// drains `held`, and `Ready::Jobs` is not requeued because a release
    /// normally hands the reports back to the host; when that release fails
    /// the host still counts them as taken, so only `held` can redeliver them.
    fn rehold_jobs(&mut self, ready: &[super::arbiter::Ready]) {
        let reports = ready
            .iter()
            .filter_map(|item| match item {
                super::arbiter::Ready::Jobs(reports) => Some(reports),
                _ => None,
            })
            .flatten();
        for report in reports {
            self.hold_report(report.clone());
        }
    }

    /// Releases the reports of a wake that never started a turn, then
    /// requeues what that wake drained. The requeue runs even when the
    /// release fails, so the failure reaches the caller without losing work;
    /// the reports the host could not take back are held here instead.
    async fn release_and_requeue(
        &mut self,
        ready: &[super::arbiter::Ready],
        ids: Vec<JobId>,
    ) -> Result<(), ServiceError> {
        let released = self.release_reports(ids).await;
        self.requeue_ready(ready);
        if released.is_err() {
            self.rehold_jobs(ready);
        }
        released
    }

    fn requeue_ready(&mut self, ready: &[super::arbiter::Ready]) {
        for item in ready {
            match item {
                super::arbiter::Ready::Recovery(text) => {
                    self.arbiter.admit_recovery(text.clone(), now());
                }
                super::arbiter::Ready::Monitor(batches) => {
                    for batch in batches {
                        self.arbiter.push_monitor(batch.clone(), now());
                    }
                }
                super::arbiter::Ready::Goal(text) => {
                    self.arbiter.admit_goal(text.clone(), now());
                }
                super::arbiter::Ready::Jobs(_) => {}
            }
        }
    }

    async fn load_goal(&mut self) {
        let session = self.session.to_string();
        self.goal = Some(adapter::load(self.services.as_ref(), &self.caller, &session).await);
        // A refused grant must not be asked again by the delivery poll: the
        // next ask belongs to the user's next goal command.
        if self.goal.as_ref().is_some_and(GoalStore::grant_refused) {
            self.delivery_refused = true;
        }
        if self
            .goal
            .as_ref()
            .is_some_and(|store| adapter::persisted_mode(store) == Some("stopped"))
        {
            self.arbiter.stop();
        }
    }

    /// Repeats a goal load that failed because the sidecar was unreachable.
    /// The session-start load runs before any front end can answer a grant
    /// question, so a fresh install fails it; the first goal use retries once
    /// the user may have answered.
    async fn reload_unreachable_goal(&mut self) {
        if self.parent.is_none()
            && self
                .goal
                .as_ref()
                .is_some_and(GoalStore::sidecar_unreachable)
        {
            self.load_goal().await;
        }
    }

    fn before_turn(&mut self, reply: oneshot::Sender<()>) {
        self.turn_active = true;
        // Wake openings skip the input hook, so the latch is set only when a
        // user prompt opened this turn.
        self.turn_user_started = std::mem::take(&mut self.prompt_seen);
        // The settled hook reads this turn's tool use after the turn-end hook,
        // so the next turn opening clears it.
        self.turn_tool_called = false;
        self.publish_status();
        let _ = reply.send(());
    }

    async fn input(&mut self) -> Result<(), ServiceError> {
        self.delivery_refused = false;
        self.guard_cancel = false;
        if self.config.loop_guard.enabled {
            reset(&mut self.guard);
        }
        self.prompt_seen = true;
        self.arbiter.on_user_prompt();
        // A prompt drops the scheduled continuation; the turn it starts
        // evaluates the goal again at its own end.
        self.drop_goal_continuation();
        if let Some(store) = self.goal.as_mut() {
            let mode = self.arbiter.mode();
            let should_save = store.saved && store.sidecar.is_some();
            if let Some(sidecar) = store.sidecar.as_mut() {
                if let Some(goal) = sidecar.goal.as_mut() {
                    if super::goal::policy::provider_block_active(goal) {
                        self.goal_recovery = true;
                    }
                    super::goal::policy::on_user_prompt(goal);
                }
                sidecar.controller = mode;
            }
            if should_save {
                let Some(sidecar) = store.sidecar.as_ref() else {
                    return Ok(());
                };
                adapter::save(self.services.as_ref(), &self.caller, sidecar).await?;
            }
        }
        Ok(())
    }

    async fn close(&mut self, deadline: TokioInstant) {
        self.goal_timer = None;
        self.projection.clear();
        for handle in self.runs.values() {
            handle.cancel.cancel();
        }
        super::monitor::state::stop_all(&mut self.monitors);
        let held = std::mem::take(&mut self.held);
        if !held.is_empty() {
            let ids = held.iter().map(|report| report.id).collect();
            let _ = timeout_at(deadline, self.release_reports(ids)).await;
        }
        let services = self.services.as_ref();
        let caller = &self.caller;
        let runs = &mut self.runs;
        let receiver = &mut self.receiver;
        let mut aborted: Vec<(HashSet<JobId>, String)> = Vec::new();
        let (sweep, ()) = tokio::join!(
            timeout_at(deadline, cancel_descendants(services, caller)),
            async {
                while !runs.is_empty() {
                    match timeout_at(deadline, receiver.recv()).await {
                        Ok(Some(Message::RunEnd { run })) => {
                            runs.remove(&run);
                        }
                        Ok(Some(Message::RunTask { run, task })) => {
                            if let Some(handle) = runs.get_mut(&run) {
                                handle.tasks.insert(task);
                            }
                        }
                        Ok(Some(Message::ReserveChildren { reply, .. })) => {
                            let _ = reply.send(Err(0));
                        }
                        Ok(Some(_)) => {}
                        Ok(None) | Err(_) => {
                            aborted = std::mem::take(runs)
                                .into_values()
                                .map(|handle| {
                                    let base =
                                        handle.base.map(|base| base.commit).unwrap_or_default();
                                    (handle.tasks, base)
                                })
                                .collect();
                            break;
                        }
                    }
                }
            }
        );
        if !aborted.is_empty() {
            self.retain_aborted_worktrees(aborted).await;
        }
        let failures = match sweep {
            Ok(sweep) => sweep.failure_text(),
            Err(_) => Some("the cancellation grace period expired".to_owned()),
        };
        if let Some(failures) = failures {
            self.services.notify(
                &self.caller,
                Notice {
                    turn: None,
                    kind: "orchestration.cancel".into(),
                    text: format!(
                        "Some child sessions were not cancelled when the session ended: {failures}. They may still be running."
                    )
                    .into(),
                },
            );
        }
    }
    /// Retains every worktree of runs the close deadline aborted, leaving
    /// each tree in place and recording its recovery path. No move, prune,
    /// or removal runs here: a parked `git add -A` may still be writing.
    async fn retain_aborted_worktrees(&self, aborted: Vec<(HashSet<JobId>, String)>) {
        let Some(root) = self.config.data_root.as_ref() else {
            return;
        };
        let session = self.session.to_string();
        let mut failures = Vec::new();
        for (tasks, base) in aborted {
            for task in tasks {
                let dir = root
                    .join("worktrees")
                    .join(session.as_str())
                    .join(task.to_string());
                if !dir.is_dir() {
                    continue;
                }
                if let Err(error) = self.retain_one_aborted(task, &base, &dir).await {
                    failures.push(error);
                }
            }
        }
        if failures.is_empty() {
            return;
        }
        self.services.notify(
            &self.caller,
            Notice {
                turn: None,
                kind: "orchestration.cancel".into(),
                text: format!(
                    "Some aborted worktrees could not be retained: {}. They remain on disk.",
                    failures.join("; ")
                )
                .into(),
            },
        );
    }

    /// Records one aborted worktree in place for hand recovery.
    async fn retain_one_aborted(&self, task: JobId, base: &str, dir: &Path) -> Result<(), String> {
        let reason = "the session closed before the run finished; the worktree was left in place";
        let at = super::goal::sidecar::format_millis(dal_core::Timestamp::now());
        let record = super::worktree::retained_body(reason, base, &dir.display().to_string(), &at)
            .map_err(|error| error.to_string())?;
        self.services
            .sidecar(
                &self.caller,
                SidecarOp::Artifact {
                    job: task,
                    file: ArtifactFile::RetainedJson,
                    bytes: record.into_bytes(),
                },
            )
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    async fn command(&mut self, name: &str, args: &str) -> Result<String, ServiceError> {
        self.delivery_refused = false;
        if self.parent.is_some() {
            return Ok(super::monitor::status::subagent_reply(name));
        }
        match name {
            "goal" => self.goal_command(args).await,
            "continuation" => self.continuation_command(args).await,
            "abort" => self.abort_command().await,
            _ => Err(ServiceError::failed(None, "unknown orchestration command")),
        }
    }

    async fn goal_command(&mut self, args: &str) -> Result<String, ServiceError> {
        self.reload_unreachable_goal().await;
        let store = self.goal.as_mut().ok_or_else(|| {
            ServiceError::failed(None, "goal: the session store is not available.")
        })?;
        let session = self.session.to_string();
        let ctx = GoalScope {
            session: &session,
            saved: store.saved,
            depth: 0,
        };
        let mode = self.arbiter.mode();
        let cleared =
            super::goal::ops::parse_goal_command(args) == super::goal::ops::GoalCommand::Clear;
        let reply = adapter::command(
            args,
            store,
            &ctx,
            self.services.as_ref(),
            &self.caller,
            mode,
        )
        .await;
        // A clear leaves no goal for a scheduled continuation.
        if cleared {
            self.drop_goal_continuation();
        }
        reply
    }

    async fn continuation_command(&mut self, args: &str) -> Result<String, ServiceError> {
        let reply = match args.trim() {
            "run" => {
                self.arbiter.on_continuation_run();
                super::goal::ops::continuation_line(&self.arbiter.mode())
            }
            "pause" => {
                self.arbiter.pause("paused by the user");
                super::goal::ops::continuation_line(&self.arbiter.mode())
            }
            "stop" => {
                self.arbiter.stop();
                super::goal::ops::continuation_line(&self.arbiter.mode())
            }
            _ => return Ok(super::goal::ops::continuation_unknown()),
        };
        self.persist_controller(&reply).await
    }

    async fn persist_controller(&mut self, reply: &str) -> Result<String, ServiceError> {
        let Some(store) = self.goal.as_mut() else {
            return Ok(reply.to_owned());
        };
        let mode = self.arbiter.mode();
        adapter::update_mode(store, mode);
        if !store.saved {
            return Ok(super::goal::ops::continuation_unsaved(
                reply,
                "this session has no sidecar store",
            ));
        }
        let Some(sidecar) = store.sidecar.as_ref() else {
            return Ok(super::goal::ops::continuation_unsaved(
                reply,
                "the goal sidecar is unavailable",
            ));
        };
        match adapter::save(self.services.as_ref(), &self.caller, sidecar).await {
            Ok(()) => Ok(reply.to_owned()),
            Err(error) => Ok(super::goal::ops::continuation_unsaved(
                reply,
                &error.to_string(),
            )),
        }
    }

    async fn abort_command(&mut self) -> Result<String, ServiceError> {
        let mut failures = Vec::new();
        let turn_was_running = match self.services.turn(&self.caller, TurnOp::IsIdle).await {
            Ok(dal_core::TurnOpReply::Idle(idle)) => !idle,
            Ok(_) => {
                failures.push(
                    "the turn service returned an unexpected reply while checking whether the turn is idle"
                        .to_owned(),
                );
                false
            }
            Err(error) => {
                failures.push(format!("the turn state could not be checked: {error}"));
                false
            }
        };
        if turn_was_running {
            match self.services.turn(&self.caller, TurnOp::Cancel).await {
                Ok(dal_core::TurnOpReply::Cancelled) => {}
                Ok(_) => failures.push(
                    "the turn service returned an unexpected reply to a cancel request".to_owned(),
                ),
                Err(error) => failures.push(format!("the turn could not be cancelled: {error}")),
            }
        }
        let jobs = match self.jobs_list().await {
            Ok(jobs) => jobs,
            Err(error) => {
                failures.push(format!("the jobs could not be listed: {error}"));
                Vec::new()
            }
        };
        let mut jobs_cancelled = 0;
        for job in jobs
            .iter()
            .filter(|job| matches!(job.state, JobStateView::Running | JobStateView::Detached))
        {
            match self.cancel_one_job(job.id).await {
                Ok(()) => jobs_cancelled += 1,
                Err(error) => failures.push(format!("{}: {error}", job.id)),
            }
        }
        for handle in self.runs.values() {
            handle.cancel.cancel();
        }
        let monitors_stopped = super::monitor::state::stop_all(&mut self.monitors);
        let sweep = cancel_descendants(self.services.as_ref(), &self.caller).await;
        self.drop_goal_continuation();
        self.arbiter.on_abort();
        let mut reply =
            super::monitor::status::abort_reply(turn_was_running, jobs_cancelled, monitors_stopped);
        if let Some(child_failures) = sweep.failure_text() {
            reply.push_str(" Some child sessions were not cancelled: ");
            reply.push_str(&child_failures);
            reply.push_str(". Run /abort again to retry.");
        }
        if !failures.is_empty() {
            reply.push_str(" Some abort operations failed: ");
            reply.push_str(&failures.join("; "));
            reply.push('.');
        }
        self.persist_controller(&reply).await
    }

    async fn tool(
        &mut self,
        caller: Caller,
        call: CallId,
        name: &str,
        args: &RawJson,
    ) -> ToolReply {
        self.delivery_refused = false;
        if name == "agents" {
            return self.agents_tool(caller, call, args).await;
        }
        ToolReply::Done(match name {
            "create_goal" | "update_goal" | "get_goal" => self.goal_tool(name, args.as_str()).await,
            "monitor" => self.monitor_tool(args).await,
            _ if name == REPORT_TOOL_NAME => self.report_tool(args.as_str()),
            _ => Err(ServiceError::failed(None, "unknown orchestration tool")),
        })
    }

    async fn goal_tool(&mut self, name: &str, args: &str) -> Result<String, ServiceError> {
        if self.parent.is_some() {
            return Err(ServiceError::failed(
                None,
                "goal: a subagent cannot hold a goal. Report to the agent that started you.",
            ));
        }
        self.reload_unreachable_goal().await;
        let todos = self.todo_summary().await?;
        let inflight = self.inflight_counts().await?;
        let store = self.goal.as_mut().ok_or_else(|| {
            ServiceError::failed(None, "goal: the session store is not available.")
        })?;
        let session = self.session.to_string();
        let ctx = GoalScope {
            session: &session,
            saved: store.saved,
            depth: 0,
        };
        adapter::tool(
            name,
            args,
            store,
            &adapter::GoalCx {
                ctx: &ctx,
                todos: &todos,
                inflight: &inflight,
                services: self.services.as_ref(),
                caller: &self.caller,
            },
        )
        .await
    }

    async fn monitor_tool(&mut self, args: &RawJson) -> Result<String, ServiceError> {
        let request = super::monitor::state::parse_request(args)
            .map_err(|error| ServiceError::failed(None, error.to_string()))?;
        let jobs = self.jobs_list().await?;
        let view = SessionJobsView { jobs: &jobs };
        let config = MonitorConfig {
            enabled: self.config.monitor.enabled,
            coalesce_ms: self.config.monitor.coalesce_ms,
            rate_limit_ms: self.config.monitor.rate_limit_ms,
            max_lines: self.config.monitor.max_lines,
            max_chars: self.config.monitor.max_chars,
            wake_budget: self.config.monitor.wake_budget,
        };
        let reply = super::monitor::state::watch(
            &mut self.monitors,
            &request,
            &view,
            dal_core::Timestamp::now(),
            &config,
        )
        .map_err(|error| ServiceError::failed(None, error.to_string()))?;
        Ok(reply.text())
    }

    fn report_tool(&self, args: &str) -> Result<String, ServiceError> {
        if self.parent.is_none() {
            return Err(ServiceError::failed(
                None,
                super::agents_tool::NO_NESTED_RUNS,
            ));
        }
        let input: ReportArgs = sonic_rs::from_str(args)
            .map_err(|error| ServiceError::failed(None, error.to_string()))?;
        let status = match input.status.as_str() {
            "done" => ReportStatus::Done,
            "blocked" => ReportStatus::Blocked,
            "failed" => ReportStatus::Failed,
            _ => return Err(ServiceError::failed(None, "report: status is invalid.")),
        };
        let (result, text) = submit(&self.report, status, &input.report);
        match result {
            ReportOutcome::Empty | ReportOutcome::Stored | ReportOutcome::Duplicate => {
                Ok(text.to_owned())
            }
        }
    }

    /// Records how full the mailbox is right now against the test peak. The
    /// owner drains one message at a time, so a sample at each receive plus
    /// one at each test park brackets the true high-water mark of the run.
    #[cfg(test)]
    fn note_fill(&self) {
        let fill = super::constants::BATTERY_MAILBOX.saturating_sub(self.sender.capacity());
        self.mailbox_pressure
            .fill_peak
            .fetch_max(fill, Ordering::SeqCst);
    }

    async fn jobs_list(&mut self) -> Result<Vec<dal_core::JobStatus>, ServiceError> {
        #[cfg(test)]
        {
            self.note_fill();
            self.jobs_parking.pass().await;
        }
        let jobs = match self.services.jobs(&self.caller, JobsOp::List).await? {
            JobsReply::Listed(jobs) => jobs,
            JobsReply::Unavailable { reason } => return Err(ServiceError::failed(None, reason)),
            _ => {
                return Err(ServiceError::failed(
                    None,
                    "jobs service returned an unexpected reply",
                ));
            }
        };
        self.inflight_jobs = jobs
            .iter()
            .filter(|job| matches!(job.state, JobStateView::Running | JobStateView::Detached))
            .count();
        let now = dal_core::Timestamp::now();
        self.silent_jobs = jobs
            .iter()
            .filter(|job| {
                matches!(job.state, JobStateView::Running | JobStateView::Detached)
                    && super::stuck::silence_suffix(job.last_activity_at, now).is_some()
            })
            .count();
        Ok(jobs)
    }

    async fn todo_summary(&self) -> Result<TodoSummary, ServiceError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct TodoRecord {
            list: Vec<TodoItem>,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct TodoItem {
            subject: String,
            state: String,
        }
        let records = self.services.records(&self.caller, "todo").await?;
        let Some(record) = records.last() else {
            return Ok(TodoSummary {
                open: 0,
                total: 0,
                first_titles: Vec::new(),
            });
        };
        let decoded: TodoRecord = sonic_rs::from_str(record.as_str())
            .map_err(|error| ServiceError::failed(None, error.to_string()))?;
        let mut first_titles = Vec::with_capacity(3);
        let mut open = 0;
        for item in &decoded.list {
            if matches!(item.state.as_str(), "pending" | "in_progress") {
                open += 1;
                if first_titles.len() < 3 {
                    first_titles.push(item.subject.clone().into_boxed_str());
                }
            } else if !matches!(item.state.as_str(), "done" | "cancelled") {
                return Err(ServiceError::failed(
                    None,
                    "todo record has an unknown state",
                ));
            }
        }
        Ok(TodoSummary {
            open,
            total: decoded.list.len(),
            first_titles,
        })
    }

    fn live_parts(inflight: &InflightCounts) -> Vec<Box<str>> {
        let mut parts = Vec::new();
        if inflight.jobs > 0 {
            parts.push(format!("{} jobs", inflight.jobs).into_boxed_str());
        }
        if inflight.monitors > 0 {
            parts.push(format!("{} monitors", inflight.monitors).into_boxed_str());
        }
        if inflight.asks > 0 {
            parts.push("asks".into());
        }
        if inflight.goal_timer > 0 {
            parts.push("goal".into());
        }
        if inflight.loop_guard > 0 {
            parts.push("loop guard".into());
        }
        parts
    }

    async fn inflight_counts(&mut self) -> Result<InflightCounts, ServiceError> {
        let _ = self.jobs_list().await?;
        Ok(super::monitor::status::inflight_counts(
            self.inflight_jobs,
            self.monitors.live_count(),
            self.open_asks.len(),
            self.goal_grant.is_some(),
            self.guard.episode.is_some(),
        ))
    }

    async fn settled(&mut self, event: Settled) -> Result<(), ServiceError> {
        if self.parent.is_some() || self.goal.is_none() {
            return Ok(());
        }
        // A provider-error stop already blocked the goal mechanically at
        // the turn end; no verdict runs for it.
        if self.last_stop == StopKind::Error && !self.last_turn_overflowed {
            return Ok(());
        }
        let recovery = std::mem::take(&mut self.goal_recovery);
        let path = if recovery {
            super::goal::policy::GoalPath::Recovery
        } else if self.turn_user_started {
            super::goal::policy::GoalPath::UserGrace
        } else {
            super::goal::policy::GoalPath::AfterTurn
        };
        let todos = self.todo_summary().await?;
        let inflight = self.inflight_counts().await?;
        let idle = matches!(
            self.services.turn(&self.caller, TurnOp::IsIdle).await?,
            dal_core::TurnOpReply::Idle(true)
        );
        let Some(signature) = self.goal_signature(&event.reply_text, &todos) else {
            return Ok(());
        };
        let decision = self.goal_verdict(path, &signature, idle, &todos, &inflight);
        self.apply_goal_verdict(
            decision,
            &event.reply_text,
            &signature,
            &inflight,
            dal_core::Timestamp::now(),
        );
        self.save_goal().await
    }

    /// Writes the goal sidecar when the store persists.
    async fn save_goal(&mut self) -> Result<(), ServiceError> {
        if let Some(store) = self.goal.as_ref()
            && store.saved
            && let Some(sidecar) = store.sidecar.as_ref()
        {
            adapter::save(self.services.as_ref(), &self.caller, sidecar).await?;
        }
        Ok(())
    }

    /// Restores a goal delivery that was persisted before a wake was refused.
    async fn restore_goal_delivery(
        &mut self,
        previous_goal: Option<GoalStore>,
        previous_grant: Option<GoalGrant>,
    ) -> Result<(), ServiceError> {
        self.goal = previous_goal;
        self.goal_grant = previous_grant;
        self.save_goal().await
    }
    /// Records a goal continuation and persists it before Wake acknowledges
    /// the new turn, returning the pre-delivery state for rollback. A failed
    /// write restores memory so it still matches the file.
    async fn checkpoint_goal_delivery(
        &mut self,
        carries_goal: bool,
    ) -> Result<GoalCheckpoint, ServiceError> {
        if !carries_goal {
            return Ok(GoalCheckpoint {
                goal: None,
                grant: None,
            });
        }
        let previous_goal = self.goal.clone();
        let previous_grant = self.goal_grant.clone();
        self.record_goal_delivery();
        if let Err(error) = self.save_goal().await {
            self.goal = previous_goal;
            self.goal_grant = previous_grant;
            return Err(error);
        }
        Ok(GoalCheckpoint {
            goal: previous_goal,
            grant: previous_grant,
        })
    }

    /// Settles a wake the host accepted without repeating it.
    async fn commit_delivered_wake(
        &mut self,
        ready: &[super::arbiter::Ready],
        job_ids: Vec<JobId>,
        monitor_only: bool,
        monitor_batches: usize,
    ) {
        self.arbiter.commit(&job_ids);
        for id in &job_ids {
            self.run_reports.remove(id);
        }
        self.settle_monitor_wake(monitor_only, monitor_batches);
        self.requeue_monitor_remainder(ready, monitor_batches);
        let pending = job_ids
            .into_iter()
            .map(|id| Unconfirmed { id, omitted: false })
            .collect();
        self.acknowledge_jobs(pending, true).await;
    }

    /// Restores a persisted delivery after a wake that never started a turn,
    /// then releases its reports for redelivery and requeues the wake's
    /// sources. Returns a failed release, else a failed rollback write, else
    /// the wake failure when one caused the refusal; the requeue has run in
    /// every case.
    async fn abandon_wake(
        &mut self,
        ready: &[super::arbiter::Ready],
        job_ids: Vec<JobId>,
        checkpoint: GoalCheckpoint,
        carries_goal: bool,
        wake_error: Option<ServiceError>,
    ) -> Result<(), ServiceError> {
        let rollback_error = if carries_goal {
            self.restore_goal_delivery(checkpoint.goal, checkpoint.grant)
                .await
                .err()
        } else {
            None
        };
        self.arbiter.release(&job_ids);
        self.release_and_requeue(ready, job_ids).await?;
        if let Some(error) = rollback_error {
            return Err(error);
        }
        if let Some(error) = wake_error {
            return Err(error);
        }
        Ok(())
    }

    /// Blocks an active goal mechanically after a provider-error stop. When
    /// the sidecar write fails, the block is rolled back so memory still
    /// matches the file, and the failure is reported; the next
    /// provider-error stop retries the block.
    async fn block_goal_on_provider_error(&mut self) {
        let now = dal_core::Timestamp::now();
        let Some(store) = self.goal.as_mut() else {
            return;
        };
        let Some(goal) = store.sidecar.as_mut().and_then(|side| side.goal.as_mut()) else {
            return;
        };
        if goal.status != super::GoalStatus::Active {
            return;
        }
        let previous = goal.clone();
        goal.status = super::GoalStatus::Blocked;
        goal.blocked = Some(super::goal::sidecar::BlockedReason {
            reason: super::goal::policy::PROVIDER_REASON.into(),
            at: now,
            mechanical: true,
        });
        goal.updated_at = now;
        if let Err(error) = self.save_goal().await {
            if let Some(goal) = self
                .goal
                .as_mut()
                .and_then(|store| store.sidecar.as_mut())
                .and_then(|side| side.goal.as_mut())
            {
                *goal = previous;
            }
            self.services.notify(
                &self.caller,
                Notice {
                    turn: None,
                    kind: "orchestration.goal".into(),
                    text: format!(
                        "the goal could not be blocked after a provider error: {error}. It stays active and will be retried."
                    )
                    .into(),
                },
            );
        }
    }

    fn goal_signature(&self, reply_text: &str, todos: &TodoSummary) -> Option<String> {
        let goal = self
            .goal
            .as_ref()
            .and_then(|store| store.sidecar.as_ref())
            .and_then(|sidecar| sidecar.goal.as_ref())?;
        if goal.status != super::GoalStatus::Active {
            return None;
        }
        Some(super::goal::policy::progress_signature(
            &goal.id,
            todos.open,
            todos.total,
            reply_text,
        ))
    }

    fn goal_verdict(
        &self,
        path: super::goal::policy::GoalPath,
        signature: &str,
        idle: bool,
        todos: &TodoSummary,
        inflight: &InflightCounts,
    ) -> super::goal::policy::Verdict {
        let Some(goal) = self
            .goal
            .as_ref()
            .and_then(|store| store.sidecar.as_ref())
            .and_then(|sidecar| sidecar.goal.as_ref())
        else {
            return super::goal::policy::Verdict::Deny(
                super::goal::policy::DenyReason::NotEligible,
            );
        };
        let input = super::goal::policy::VerdictInput {
            goal,
            path,
            idle,
            pending_user_messages: !self.open_asks.is_empty(),
            continuation_pending: self.goal_grant.is_some(),
            last_turn_context_overflow: self.last_turn_overflowed,
            last_stop: self.last_stop,
            signature,
            open_todos: todos.open,
            total_todos: todos.total,
            inflight,
        };
        super::goal::policy::verdict(&input)
    }

    fn apply_goal_verdict(
        &mut self,
        decision: super::goal::policy::Verdict,
        reply_text: &str,
        signature: &str,
        inflight: &InflightCounts,
        now: dal_core::Timestamp,
    ) {
        match decision {
            super::goal::policy::Verdict::Continue { prompt, stall } => {
                let tool_called = self.turn_tool_called;
                let live_parts = Self::live_parts(inflight);
                let Some((goal, prompt_text)) =
                    self.build_goal_prompt(prompt, &if stall { live_parts } else { Vec::new() })
                else {
                    return;
                };
                super::goal::policy::record_turn_output(goal, reply_text, tool_called, 0, 0);
                goal.updated_at = now;
                self.goal_grant = Some(GoalGrant {
                    signature: signature.to_owned(),
                    prompt,
                });
                self.schedule_goal(prompt_text);
            }
            super::goal::policy::Verdict::Deny(reason) => self.deny_goal(reason, now),
        }
    }

    /// Builds one continuation prompt against the current goal.
    fn build_goal_prompt(
        &mut self,
        prompt: super::goal::policy::PromptKind,
        live_parts: &[Box<str>],
    ) -> Option<(&mut super::goal::sidecar::Goal, String)> {
        let goal = self.goal.as_mut()?.sidecar.as_mut()?.goal.as_mut()?;
        let number = goal.unattended.saturating_add(1);
        let prompt_text = super::goal::prompt::build_prompt(goal, prompt, number, live_parts);
        Some((goal, prompt_text))
    }

    /// Arms the arbiter timer for one continuation prompt. P4 is ready at
    /// once after an automatic turn and ten seconds after a turn that a
    /// user prompt started.
    fn schedule_goal(&mut self, prompt_text: String) {
        let delay = if self.turn_user_started {
            super::goal::policy::CONTINUATION_DELAY_MS
        } else {
            0
        };
        self.goal_timer = Some((
            TokioInstant::now() + std::time::Duration::from_millis(delay),
            prompt_text,
        ));
    }

    /// Records the continuation immediately before Wake so the sidecar is
    /// durable before the host acknowledges the new turn. A refused wake
    /// restores the grant and counters.
    fn record_goal_delivery(&mut self) {
        let Some(grant) = self.goal_grant.take() else {
            return;
        };
        let Some(goal) = self
            .goal
            .as_mut()
            .and_then(|store| store.sidecar.as_mut())
            .and_then(|sidecar| sidecar.goal.as_mut())
        else {
            return;
        };
        super::goal::policy::record_delivery(goal, &grant.signature, grant.prompt);
        goal.updated_at = dal_core::Timestamp::now();
    }

    /// Drops every trace of a granted continuation: the timer, the grant, and
    /// the text that waits in the arbiter. Nothing is counted.
    fn drop_goal_continuation(&mut self) {
        self.goal_timer = None;
        self.goal_grant = None;
        self.arbiter.drop_goal();
    }

    /// Drops a granted continuation whose goal is no longer active, such as
    /// one that `/goal pause` or a block reached after the grant.
    fn void_stale_goal_grant(&mut self) {
        if self.goal_grant.is_none() {
            return;
        }
        let active = self
            .goal
            .as_ref()
            .and_then(|store| store.sidecar.as_ref())
            .and_then(|sidecar| sidecar.goal.as_ref())
            .is_some_and(|goal| goal.status == super::GoalStatus::Active);
        if !active {
            self.drop_goal_continuation();
        }
    }

    /// Applies a mechanical deny: blocks the goal with the deny's exact
    /// reason. `NotEligible`, `SingleFlight`, and `Stale` change nothing.
    fn deny_goal(&mut self, reason: super::goal::policy::DenyReason, now: dal_core::Timestamp) {
        let Some(blocked) = reason.mechanical_reason() else {
            return;
        };
        let Some(goal) = self
            .goal
            .as_mut()
            .and_then(|store| store.sidecar.as_mut())
            .and_then(|sidecar| sidecar.goal.as_mut())
        else {
            return;
        };
        goal.status = super::GoalStatus::Blocked;
        goal.blocked = Some(super::goal::sidecar::BlockedReason {
            reason: blocked.into(),
            at: now,
            mechanical: true,
        });
        goal.updated_at = now;
    }

    async fn agents_tool(&mut self, caller: Caller, call: CallId, args: &RawJson) -> ToolReply {
        if self.parent.is_some() {
            return ToolReply::Done(Err(ServiceError::failed(
                None,
                super::agents_tool::NO_NESTED_RUNS,
            )));
        }
        let saved = self
            .config
            .workflows
            .as_ref()
            .and_then(|workflows| sonic_rs::to_string(workflows).ok())
            .and_then(|text| RawJson::parse(&text).ok());
        let action = match super::agents_tool::decode_action(args, saved.as_ref()) {
            Ok(action) => action,
            Err(error) => {
                return ToolReply::Done(Err(ServiceError::failed(None, error.to_string())));
            }
        };
        ToolReply::Done(match action {
            super::agents_tool::AgentAction::Wait { ids, timeout_s } => {
                return ToolReply::Wait(WaitPlan {
                    services: Arc::clone(&self.services),
                    caller,
                    ids,
                    timeout_s,
                });
            }
            super::agents_tool::AgentAction::Cancel { ids } => self.cancel_ids(ids).await,
            super::agents_tool::AgentAction::List { ids } => self.list_agents(ids).await,
            super::agents_tool::AgentAction::Run {
                label,
                workflow,
                input,
            } => {
                self.run_workflow(&caller, call, &label, workflow, input)
                    .await
            }
        })
    }

    /// Cancels run jobs, single tasks of live runs, known background jobs,
    /// and child sessions the model named.
    async fn cancel_ids(&mut self, ids: Vec<String>) -> Result<String, ServiceError> {
        let mut sessions = Vec::new();
        let mut cancelled = 0;
        let mut failures = Vec::new();
        let mut seen_jobs = HashSet::new();
        let mut seen_sessions = HashSet::new();
        for display in &ids {
            let Ok(job) = JobId::parse(display) else {
                failures.push(super::agents_tool::unknown_id(display));
                continue;
            };
            let known = self
                .services
                .jobs(&self.caller, JobsOp::Find { id: job })
                .await?;
            if matches!(known, JobsReply::Found(None)) {
                let session = SessionId::parse(display)
                    .map_err(|error| ServiceError::failed(None, error.to_string()))?;
                if seen_sessions.insert(session) {
                    sessions.push(session);
                }
                continue;
            }
            if !seen_jobs.insert(job) {
                continue;
            }
            match self.cancel_one_job(job).await {
                Ok(()) => cancelled += 1,
                Err(error) => failures.push(format!("{job}: {error}")),
            }
        }
        for &session in &sessions {
            match self
                .services
                .agents(&self.caller, AgentsOp::Cancel { id: session })
                .await
            {
                Ok(AgentsReply::Cancelled { .. }) => cancelled += 1,
                Ok(_) => failures.push(format!(
                    "{session}: the agents service returned an unexpected reply to a cancel request"
                )),
                Err(error) => failures.push(format!("{session}: {error}")),
            }
        }
        if failures.is_empty() {
            return Ok(format!("cancelled {cancelled} jobs and child sessions."));
        }
        Err(ServiceError::failed(
            None,
            format!(
                "cancelled {cancelled} jobs and child sessions; could not cancel {}.",
                failures.join("; ")
            ),
        ))
    }

    /// Cancels one run, task, or known background job. A run cancel also
    /// stops its coordinator; a task cancel reaches the coordinator, which
    /// ends that child alone.
    async fn cancel_one_job(&mut self, job: JobId) -> Result<(), ServiceError> {
        if let Some(handle) = self.runs.get(&job) {
            handle.cancel.cancel();
            return Ok(());
        }
        let owner = self
            .runs
            .iter()
            .find(|(_, handle)| handle.tasks.contains(&job))
            .map(|(run, handle)| (*run, handle.control.clone()));
        if let Some((_, control)) = owner {
            control
                .send(RunControl::CancelTask(job))
                .await
                .map_err(|_| {
                    ServiceError::failed(None, "the run ended before the task could be cancelled")
                })?;
            return Ok(());
        }
        let known = matches!(
            self.services
                .jobs(&self.caller, JobsOp::Find { id: job })
                .await?,
            JobsReply::Found(Some(_))
        );
        if !known {
            return Err(ServiceError::failed(
                None,
                super::agents_tool::unknown_id(&job.to_string()),
            ));
        }
        self.jobs_cancel(job).await
    }

    async fn jobs_cancel(&mut self, job: JobId) -> Result<(), ServiceError> {
        match self
            .services
            .jobs(&self.caller, JobsOp::Cancel { id: job })
            .await?
        {
            JobsReply::Cancelled { .. } => Ok(()),
            JobsReply::Refused(error) => Err(ServiceError::failed(None, error.to_string())),
            _ => Err(ServiceError::failed(
                None,
                "jobs service returned an unexpected reply to a cancel request",
            )),
        }
    }

    /// Lists child sessions, then live run and task jobs. Named ids show
    /// only those sessions and jobs. Live jobs idle for more than ten
    /// minutes carry their silence suffix.
    async fn list_agents(&mut self, requested: Vec<String>) -> Result<String, ServiceError> {
        let AgentsReply::Listed(agents) =
            self.services.agents(&self.caller, AgentsOp::List).await?
        else {
            return Err(ServiceError::failed(
                None,
                "agents service returned an unexpected reply",
            ));
        };
        let mut lines: Vec<String> = agents
            .iter()
            .filter(|agent| {
                requested.is_empty() || requested.iter().any(|id| id == &agent.id.to_string())
            })
            .map(|agent| format!("{} {} {:?}", agent.id, agent.name, agent.state))
            .collect();
        let jobs = self.jobs_list().await?;
        let now = dal_core::Timestamp::now();
        for job in jobs
            .iter()
            .filter(|job| {
                matches!(job.state, JobStateView::Running | JobStateView::Detached)
                    && (requested.is_empty()
                        || requested.iter().any(|id| id == &job.id.to_string()))
            })
            .take(super::agents_tool::LIST_RUNS_LIMIT)
        {
            let mut line = format!("{} {}: running", job.id, job.label);
            if let Some(suffix) = super::stuck::silence_suffix(job.last_activity_at, now) {
                line.push_str(&suffix);
            }
            lines.push(line);
        }
        if let Some(table) = self
            .config
            .workflows
            .as_ref()
            .and_then(|workflows| sonic_rs::to_string(workflows).ok())
            .and_then(|text| RawJson::parse(&text).ok())
        {
            for (name, invalid) in super::workflow::saved_names(&table) {
                match invalid {
                    None => lines.push(format!("workflow {name}")),
                    Some(reason) => lines.push(format!("workflow {name}: invalid: {reason}")),
                }
            }
        }
        Ok(if lines.is_empty() {
            "No child sessions.".to_owned()
        } else {
            lines.join("\n")
        })
    }

    /// Runs one workflow in the background: admission, worktree preflight,
    /// one run job, then a coordinator task that owns the children and
    /// settles the single top-level report.
    async fn run_workflow(
        &mut self,
        caller: &Caller,
        call: CallId,
        label: &str,
        workflow: super::workflow::Workflow,
        input: Option<String>,
    ) -> Result<String, ServiceError> {
        if !self.config.agents.enabled {
            return Err(ServiceError::failed(
                None,
                super::agents_tool::SUBAGENTS_OFF,
            ));
        }
        let limits = super::admission::Limits {
            max_runs: self.config.agents.max_runs as usize,
            agents_per_session: super::admission::AGENTS_PER_SESSION,
            session_used: self.children_started,
        };
        let planned = super::admission::admit(&workflow, self.runs.len(), &limits, |step| {
            let tools = step
                .tools
                .iter()
                .map(|name| Name::parse(name))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?;
            dal_core::AgentStart {
                call: call.clone(),
                name: Some(step.name.clone().into()),
                prompt: step.prompt.clone().into(),
                model: step.model.clone().map(String::into_boxed_str),
                role: step.role.clone().map(String::into_boxed_str),
                system: step.system.clone().map(String::into_boxed_str),
                tools: Some(tools.into_boxed_slice()),
                workspace: None,
            }
            .validate()
            .map_err(|error| error.to_string())
        })
        .map_err(|error| ServiceError::failed(None, error.to_string()))?;
        let base = self.worktree_base(caller, &workflow).await?;
        let run = self
            .spawn_job(caller, RUN_JOB_NAME, label)
            .await
            .map_err(|error| grant_failure("jobs", error))?;
        self.children_started += u32::try_from(planned)
            .map_err(|error| ServiceError::failed(None, error.to_string()))?;
        let steps = workflow.steps.len();
        let items_from = workflow
            .steps
            .iter()
            .filter_map(|step| match &step.items {
                super::workflow::Items::From(source) => Some(source.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let (control, control_receiver) = mpsc::channel(16);
        let cancel = CancellationToken::new();
        let coordinator = Coordinator {
            services: Arc::clone(&self.services),
            caller: caller.clone(),
            sender: self.sender.clone(),
            session: self.session,
            workspace: self.workspace.clone(),
            data_root: self.config.data_root.clone(),
            base: base.clone(),
            run,
            call,
            label: label.to_owned(),
            input,
            cancel: cancel.clone(),
            tasks: Mutex::new(LiveTasks::default()),
            merge_locks: self.merge_locks.clone(),
            reports: Arc::clone(&self.reports),
            child_max_steps: self.config.agents.child_max_steps,
            child_max_minutes: self.config.agents.child_max_minutes,
        }
        .spawn(workflow, control_receiver);
        self.runs.insert(
            run,
            RunHandle {
                cancel,
                control,
                tasks: HashSet::new(),
                base,
                _coordinator: coordinator,
            },
        );
        self.run_reports.insert(run);
        self.inflight_jobs += 1;
        let items_from = items_from.iter().map(String::as_str).collect::<Vec<_>>();
        Ok(super::agents_tool::run_result_text(
            &run.to_string(),
            label,
            steps,
            planned,
            &items_from,
        ))
    }

    /// Spawns one job row the orchestration extension owns and settles.
    async fn spawn_job(
        &self,
        caller: &Caller,
        name: &str,
        label: &str,
    ) -> Result<JobId, ServiceError> {
        let name =
            Name::parse(name).map_err(|error| ServiceError::failed(None, error.to_string()))?;
        let payload = RawJson::parse(&format!(
            "{{\"label\":{}}}",
            sonic_rs::to_string(label)
                .map_err(|error| ServiceError::failed(None, error.to_string()))?
        ))
        .map_err(|error| ServiceError::failed(None, error.to_string()))?;
        match self
            .services
            .jobs(
                caller,
                JobsOp::Spawn {
                    name,
                    payload,
                    parent: None,
                },
            )
            .await?
        {
            JobsReply::Spawned { id } => Ok(id),
            JobsReply::Unavailable { reason } => Err(ServiceError::failed(None, reason)),
            _ => Err(ServiceError::failed(
                None,
                "jobs service returned an unexpected reply to a spawn request",
            )),
        }
    }

    /// Resolves the shared base commit for every worktree task of the run,
    /// or `None` when no step isolates. A refusal fails the whole run
    /// before any job starts.
    async fn worktree_base(
        &self,
        caller: &Caller,
        workflow: &super::workflow::Workflow,
    ) -> Result<Option<Base>, ServiceError> {
        let Some(step) = workflow
            .steps
            .iter()
            .find(|step| step.isolation == super::workflow::Isolation::Worktree)
        else {
            return Ok(None);
        };
        let workspace_text = self.workspace.display().to_string();
        let refuse = |refusal: IsolationRefusal| {
            ServiceError::failed(None, refusal.text(&step.name, &workspace_text))
        };
        if self.config.data_root.is_none() {
            return Err(refuse(IsolationRefusal::NoDataRoot));
        }
        self.check_git_version(caller, &refuse).await?;
        let top = self.resolve_toplevel(caller, &refuse).await?;
        let head = self.resolve_head(caller, &top, &refuse).await?;
        let stash = self.resolve_stash(caller, &top, &refuse).await?;
        let canonical_top = tokio::fs::canonicalize(&top)
            .await
            .unwrap_or_else(|_| top.clone());
        let canonical_workspace = tokio::fs::canonicalize(&self.workspace)
            .await
            .unwrap_or_else(|_| self.workspace.clone());
        let relative_workspace = workspace_relative(&canonical_workspace, &canonical_top)
            .ok_or_else(|| refuse(super::worktree::IsolationRefusal::NotARepository))?;
        Ok(Some(Base {
            top,
            commit: if stash.is_empty() { head } else { stash },
            relative_workspace,
        }))
    }

    async fn check_git_version(
        &self,
        caller: &Caller,
        refuse: &impl Fn(IsolationRefusal) -> ServiceError,
    ) -> Result<(), ServiceError> {
        let version = git(
            self.services.as_ref(),
            caller,
            &self.workspace,
            super::worktree::argv_version()
                .into_iter()
                .map(str::to_owned)
                .collect(),
        )
        .await
        .map_err(|error| refuse(denied_refusal(&error)))?;
        if !complete(&version) {
            return Err(refuse(IsolationRefusal::NoGit));
        }
        let text = String::from_utf8_lossy(&version.stdout_prefix);
        let found =
            super::worktree::parse_version(&text).ok_or_else(|| refuse(IsolationRefusal::NoGit))?;
        if found < super::worktree::MIN_GIT_VERSION {
            return Err(refuse(IsolationRefusal::TooOld {
                version: text.trim().to_owned(),
            }));
        }
        Ok(())
    }

    async fn resolve_toplevel(
        &self,
        caller: &Caller,
        refuse: &impl Fn(IsolationRefusal) -> ServiceError,
    ) -> Result<PathBuf, ServiceError> {
        match git(
            self.services.as_ref(),
            caller,
            &self.workspace,
            super::worktree::argv_toplevel(&self.workspace.display().to_string()),
        )
        .await
        {
            Ok(output) if complete(&output) => Ok(PathBuf::from(
                String::from_utf8_lossy(&output.stdout_prefix).trim(),
            )),
            Ok(_) => Err(refuse(super::worktree::IsolationRefusal::NotARepository)),
            Err(error) => Err(refuse(denied_refusal(&error))),
        }
    }

    async fn resolve_head(
        &self,
        caller: &Caller,
        top: &Path,
        refuse: &impl Fn(IsolationRefusal) -> ServiceError,
    ) -> Result<String, ServiceError> {
        match git(
            self.services.as_ref(),
            caller,
            &self.workspace,
            super::worktree::argv_verify_head(&top.display().to_string()),
        )
        .await
        {
            Ok(output) if complete(&output) => Ok(String::from_utf8_lossy(&output.stdout_prefix)
                .trim()
                .to_owned()),
            Ok(_) => Err(refuse(super::worktree::IsolationRefusal::NoHead)),
            Err(error) => Err(refuse(denied_refusal(&error))),
        }
    }

    async fn resolve_stash(
        &self,
        caller: &Caller,
        top: &Path,
        refuse: &impl Fn(IsolationRefusal) -> ServiceError,
    ) -> Result<String, ServiceError> {
        match git(
            self.services.as_ref(),
            caller,
            &self.workspace,
            super::worktree::argv_stash_create(&top.display().to_string()),
        )
        .await
        {
            Ok(output) if complete(&output) => Ok(String::from_utf8_lossy(&output.stdout_prefix)
                .trim()
                .to_owned()),
            Ok(output) => Err(refuse(super::worktree::IsolationRefusal::NoBase {
                reason: stderr_line(&output),
            })),
            Err(error) => Err(refuse(denied_refusal(&error))),
        }
    }

    async fn tool_call(&mut self, event: ToolCallEvent) -> ToolCallVerdict {
        let mut verdict = ToolCallVerdict::Allow;
        if self.config.loop_guard.enabled {
            match on_tool_call(&mut self.guard, event.tool.as_str(), &event.args) {
                Ok(effects) => {
                    if let Some(text) = effects.steer {
                        let _ = self
                            .services
                            .turn(&self.caller, TurnOp::Steer { text })
                            .await;
                    }
                    if let Some(text) = effects.warning {
                        self.services.notify(
                            &self.caller,
                            Notice {
                                turn: Some(event.turn),
                                kind: "orchestration.loop_guard".into(),
                                text,
                            },
                        );
                    }
                    if let Some(text) = effects.p1_recovery {
                        self.arbiter.admit_recovery(text.into(), now());
                    }
                    if let Some(reason) = effects.pause_reason {
                        self.arbiter.pause(reason);
                    }
                    if effects.cancel_turn {
                        self.guard_cancel = true;
                        let _ = self.services.turn(&self.caller, TurnOp::Cancel).await;
                    }
                    if let GuardVerdict::Block { reason } = effects.verdict {
                        verdict = ToolCallVerdict::Block { reason };
                    }
                }
                Err(error) => {
                    verdict = ToolCallVerdict::Block {
                        reason: format!("loop guard could not inspect tool arguments: {error}")
                            .into_boxed_str(),
                    };
                }
            }
        }
        if matches!(verdict, ToolCallVerdict::Allow)
            && self.config.sleep.enabled
            && event.tool.as_str() == "exec"
            && let Some(classifier) = self.sleep.as_ref()
        {
            let parsed = sonic_rs::from_str::<sonic_rs::Value>(event.args.as_str());
            if let Ok(args) = parsed
                && let Some(command) = args
                    .as_object()
                    .and_then(|object| object.get(&"command"))
                    .and_then(sonic_rs::JsonValueTrait::as_str)
                && let Some(wait) = classifier.classify(command)
                && let Ok(Some(args)) = rewrite_exec_args(&event.args, wait)
            {
                verdict = ToolCallVerdict::Rewrite { args };
            }
        }
        verdict
    }

    /// Publishes the read-only live-work, goal, and status projections for
    /// other batteries, and releases the activity snapshot to the host poll
    /// only when it changed and the emission rate allows. Every status
    /// publish refreshes all three from the same state, so consumers see
    /// what the activity row shows without touching the owner task.
    fn publish_status(&mut self) {
        let inflight = super::monitor::status::inflight_counts(
            self.inflight_jobs,
            self.monitors.live_count(),
            self.open_asks.len(),
            self.goal_grant.is_some(),
            self.guard.episode.is_some(),
        );
        let goal = self
            .goal
            .as_ref()
            .and_then(|store| store.sidecar.as_ref())
            .and_then(|sidecar| sidecar.goal.as_ref())
            .map(super::live::GoalView::from_goal);
        let session_idle = !self.turn_active;
        let others = inflight.jobs + inflight.monitors + usize::from(inflight.goal_timer > 0);
        let run_mode = self.arbiter.mode() == super::ControllerMode::Run;
        let ready_items = usize::from(
            self.arbiter.has_non_job_items() || (run_mode && self.arbiter.has_waiting()),
        );
        let quiet = self.arbiter.quiet(session_idle, others, ready_items, now());
        let line = status_line(self.arbiter.mode(), session_idle, inflight);
        let status = StatusPayload::new(
            self.arbiter.mode(),
            quiet,
            inflight,
            self.silent_jobs,
            goal.as_ref(),
        );
        self.projection.publish(super::live::Published {
            inflight: super::live::InflightCounts {
                jobs: inflight.jobs,
                monitors: inflight.monitors,
                asks: inflight.asks,
                goal_timer: inflight.goal_timer > 0,
                loop_guard: inflight.loop_guard > 0,
            },
            goal,
            status,
        });
        if let Ok(mut gate) = self.snapshot.lock() {
            gate.publish(StatusSnapshot {
                quiet,
                text: Some(line.into_boxed_str()),
            });
        }
    }
}

struct SessionJobsView<'a> {
    jobs: &'a [dal_core::JobStatus],
}

impl JobsView for SessionJobsView<'_> {
    fn resolve_job(&self, display: &str) -> Option<JobId> {
        let id = JobId::parse(display).ok()?;
        self.jobs.iter().any(|job| job.id == id).then_some(id)
    }

    fn is_live_top_level_exec(&self, id: JobId) -> bool {
        self.jobs.iter().any(|job| {
            job.id == id && matches!(job.state, JobStateView::Running | JobStateView::Detached)
        })
    }
}

/// What a `wait` needs once the owner hands it off its message loop.
struct WaitPlan {
    services: Arc<dyn Services>,
    caller: Caller,
    ids: Vec<String>,
    timeout_s: u16,
}

/// The answer to one orchestration tool call: now, or after a wait the owner
/// runs beside its message loop so coordinator messages keep flowing.
enum ToolReply {
    Done(Result<String, ServiceError>),
    Wait(WaitPlan),
}

/// Waits for run and task jobs, returning the ended reports and a
/// still-running line whose live jobs carry their silence suffix.
async fn wait_for_jobs(plan: WaitPlan) -> Result<String, ServiceError> {
    let WaitPlan {
        services,
        caller,
        ids,
        timeout_s,
    } = plan;
    let timeout = Duration::from_secs(u64::from(timeout_s));
    let mut texts = Vec::new();
    let mut still_running = Vec::new();
    for display in &ids {
        let id = JobId::parse(display).map_err(|_| unknown_id_error(display))?;
        let wait = JobsOp::Wait {
            id,
            timeout: Some(timeout),
        };
        match services.jobs(&caller, wait).await? {
            JobsReply::Waited { .. } => {
                texts.push(ended_job_text(services.as_ref(), &caller, id).await?);
            }
            JobsReply::Status(status) => {
                still_running.push(running_line(display, &status));
            }
            JobsReply::Refused(_) => return Err(unknown_id_error(display)),
            _ => {
                return Err(ServiceError::failed(
                    None,
                    "jobs service returned an unexpected reply to a wait request",
                ));
            }
        }
    }
    if !still_running.is_empty() {
        texts.push(format!("still running: {}", still_running.join(", ")));
    }
    Ok(texts.join("\n\n"))
}

/// The settled report text of one ended job.
async fn ended_job_text(
    services: &dyn Services,
    caller: &Caller,
    id: JobId,
) -> Result<String, ServiceError> {
    match services.jobs(caller, JobsOp::Text { id }).await? {
        JobsReply::Text { text, .. } => Ok(text.to_string()),
        _ => Ok(String::new()),
    }
}

/// One live job id with its silence suffix, when it has been quiet long.
fn running_line(display: &str, status: &dal_core::JobStatus) -> String {
    let mut line = display.to_owned();
    if let Some(suffix) =
        super::stuck::silence_suffix(status.last_activity_at, dal_core::Timestamp::now())
    {
        line.push_str(&suffix);
    }
    line
}

/// The error a wait or cancel returns for an id the session does not know.
fn unknown_id_error(display: &str) -> ServiceError {
    ServiceError::failed(None, super::agents_tool::unknown_id(display))
}

/// Runs one git argv through the granted run service with an explicit
/// working directory and a bounded deadline.
async fn git(
    services: &dyn Services,
    caller: &Caller,
    cwd: &Path,
    argv: Vec<String>,
) -> Result<RunOutput, ServiceError> {
    let request = RunRequest {
        argv: argv.into_iter().map(std::ffi::OsString::from).collect(),
        cwd: Some(cwd.to_path_buf()),
        stdin: None,
        timeout: Some(GIT_CALL_TIMEOUT),
        env: vec![(Box::from("GIT_OPTIONAL_LOCKS"), Box::from("0"))],
        stdout_prefix_limit: GIT_STDOUT_PREFIX_LIMIT,
    };
    services.run(caller, request).await
}

/// Maps a denied git call to its isolation refusal.
fn denied_refusal(error: &ServiceError) -> super::worktree::IsolationRefusal {
    let reason = match error {
        ServiceError::Denied(reason) => format!("{reason:?}"),
        other => other.to_string(),
    };
    super::worktree::IsolationRefusal::Denied { reason }
}

/// Renders the missing-grant error for one service the run path needs.
fn grant_failure(service: &str, error: ServiceError) -> ServiceError {
    match error {
        ServiceError::Denied(reason) => ServiceError::failed(
            None,
            super::agents_tool::service_denied(service, &format!("{reason:?}")),
        ),
        other => other,
    }
}

/// The first stderr line of a failed git call, for task failure texts.
fn stderr_line(output: &RunOutput) -> String {
    String::from_utf8_lossy(&output.stderr_tail)
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// The settled verdict for a child whose grace turn never started: it
/// failed without a report, and the reason says why.
fn refused_grace(reason: String) -> super::pool::Settled {
    super::pool::Settled {
        state: TaskState::Failed(reason),
        note: None,
    }
}

/// Builds the pool's verdict input for one ended child turn from its
/// stored report and terminal stop. The first turn never counts the
/// deadline as fired; only the grace interrupt ends a turn that way.
fn child_end(
    stored: Option<Report>,
    report: &AgentReport,
    max_rounds: u32,
    max_minutes: u32,
) -> ChildEnd {
    let stop = match report.stop {
        Stop::EndTurn => StopReason::EndTurn,
        Stop::MaxSteps => StopReason::MaxSteps,
        Stop::Length => StopReason::Length,
        Stop::Cancelled => StopReason::Cancelled,
        Stop::Filter => StopReason::Filter,
        Stop::Failed => StopReason::Error("the child turn failed".to_owned()),
    };
    ChildEnd {
        report: stored,
        stop,
        deadline_hit: false,
        max_rounds,
        max_minutes,
        last_text: report.text.to_string(),
    }
}

/// Whether a git call exited cleanly with all of its stdout kept.
fn complete(output: &RunOutput) -> bool {
    output.status == ExitStatusKind::Exited(0) && !output.stdout_prefix_overflowed
}

/// Why a git call did not succeed: the service error, the first stderr line
/// of a call that exited badly, or the overflow of a call that outgrew the
/// stdout prefix cap. `None` means it succeeded with all of its stdout kept.
fn git_refusal(result: &Result<RunOutput, ServiceError>) -> Option<String> {
    match result {
        Ok(output) if complete(output) => None,
        Ok(output) if output.stdout_prefix_overflowed => {
            let line = stderr_line(output);
            if line.is_empty() {
                Some("the git output exceeded the stdout prefix limit".to_owned())
            } else {
                Some(line)
            }
        }
        Ok(output) => Some(stderr_line(output)),
        Err(error) => Some(error.to_string()),
    }
}

/// Salvage-local git argv. Each starts with `git` and carries its explicit
/// `-C` root like the worktree-backend forms, but these read-only shapes
/// (status, net diff, worktree list) predate any battery policy for them, so
/// they stay local to open salvage instead of joining that registry.
fn salvage_argv_status(dir: &str) -> Vec<String> {
    vec![
        "git".to_owned(),
        "-C".to_owned(),
        dir.to_owned(),
        "status".to_owned(),
        "--porcelain".to_owned(),
        "--untracked-files=all".to_owned(),
        "--ignored".to_owned(),
    ]
}

fn salvage_argv_net_diff(dir: &str, base: &str) -> Vec<String> {
    vec![
        "git".to_owned(),
        "-C".to_owned(),
        dir.to_owned(),
        "diff".to_owned(),
        "--binary".to_owned(),
        base.to_owned(),
    ]
}

fn salvage_argv_worktree_list(top: &str) -> Vec<String> {
    vec![
        "git".to_owned(),
        "-C".to_owned(),
        top.to_owned(),
        "worktree".to_owned(),
        "list".to_owned(),
        "--porcelain".to_owned(),
    ]
}

/// Splits one `status --porcelain` output into `(untracked, ignored)`: the
/// two kinds of entry no diff against the base can carry. Tracked entries
/// need no class of their own, because the net diff reads them. Any
/// malformed line invalidates the whole read: doubt preserves the tree.
fn salvage_status_class(stdout: &[u8]) -> Option<(bool, bool)> {
    let mut untracked = false;
    let mut ignored = false;
    for line in stdout.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        if line.len() < 3 || line[2] != b' ' {
            return None;
        }
        if line.starts_with(b"!!") {
            ignored = true;
        } else if line.starts_with(b"??") {
            untracked = true;
        }
    }
    Some((untracked, ignored))
}

fn salvage_argv_net_names(dir: &str, base: &str) -> Vec<String> {
    vec![
        "git".to_owned(),
        "-C".to_owned(),
        dir.to_owned(),
        "diff".to_owned(),
        "--name-only".to_owned(),
        "-z".to_owned(),
        base.to_owned(),
    ]
}

/// Whether `text` is a full object name: 40 or 64 lowercase hex digits. A
/// base read from disk reaches a git argv, so nothing else may pass; an
/// option-shaped or truncated record is refused, not interpreted.
fn valid_object_name(text: &str) -> bool {
    matches!(text.len(), 40 | 64)
        && text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// The base commit a task's creation receipt records, or `None` when the
/// file is missing or is not exactly one full object name and a newline.
async fn receipt_base(receipt: &Path) -> Option<String> {
    let text = tokio::fs::read_to_string(receipt.join(RECEIPT_BASE_FILE))
        .await
        .ok()?;
    let name = text.strip_suffix('\n')?;
    valid_object_name(name).then(|| name.to_owned())
}

/// Records the creation receipt of one task worktree: its private
/// `<data root>/isolation/<session>/<task>/` directory and, in `base.txt`,
/// the base commit the tree starts from. Open salvage trusts only trees that
/// carry the receipt and takes a lost tree's work against that base. The
/// directory and the file go through the store's private-directory and
/// atomic-write helpers like every other record of that directory, so a
/// crash leaves either no file or a whole one.
async fn record_isolation_receipt(
    data_root: &Path,
    session: SessionId,
    task: JobId,
    base: &str,
) -> Result<(), String> {
    let dir = data_root
        .join("isolation")
        .join(session.to_string())
        .join(task.to_string());
    let record = format!("{base}\n").into_bytes();
    tokio::task::spawn_blocking(move || {
        dal_store::create_private_dir_all(&dir).map_err(|error| error.to_string())?;
        dal_store::write_atomic(
            &dir.join(RECEIPT_BASE_FILE),
            &record,
            dal_store::FileMode::Mode0600,
        )
        .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Reads the host's durable held-job set. The jobs service has no query for
/// it: `Unhold` answers with the whole set under one lock, and a job that
/// was never held changes nothing, so a fresh id reads the set without
/// touching it. A failed or unexpected answer is `None`, and salvage then
/// touches nothing.
async fn read_held_set(services: &dyn Services, caller: &Caller) -> Option<HashSet<JobId>> {
    let probe = JobsOp::Unhold {
        id: JobId::new_v7(),
    };
    match services.jobs(caller, probe).await {
        Ok(JobsReply::Held(ids)) => Some(ids.into_iter().collect()),
        _ => None,
    }
}

/// Whether nothing exists at `path`. Any other answer, including an
/// unreadable one, counts as present: doubt preserves the tree.
async fn certainly_absent(path: &Path) -> bool {
    matches!(
        tokio::fs::symlink_metadata(path).await,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    )
}

/// Crash/reopen salvage for one session open.
///
/// Only directories of this session (`<data root>/worktrees/<session>/`)
/// named by a task job id are candidates. A candidate salvages when the live
/// job table does not hold it: task jobs are children of their run job and
/// the jobs ledger records only top-level endings, so after a restart a
/// crashed task has no row, and absence from the table is the lost signal; a
/// listed ended-`lost` job salvages the same way. The store lock refuses a
/// second opener with `SessionBusy` before this hook runs, so an absent id
/// is not another process's live tree. Live jobs, jobs in the host's durable
/// held set, and jobs that ended any other way keep their trees in place, so
/// no pid is ever read; a job list or held-set read that fails touches
/// nothing. A settled task whose tree was left in place has no row
/// either and salvages like a lost one: the capture below saves its work
/// before the tree goes.
/// Provenance is conjunctive. The battery's creation receipt
/// (`<data root>/isolation/<session>/<task>/`, written when the worktree is
/// created) must exist, must carry no `retained.json` marker (a tree left
/// in place for hand recovery is never taken), and must record a valid base
/// commit in `base.txt`; the top checkout's worktree list must also register
/// the path. Any check failing leaves the tree in place. For each proven
/// tree the battery reads `status --porcelain` first: any ignored or
/// untracked entry, or an unreadable or malformed status, leaves the tree in
/// place, so ignored files (invisible to every diff) and untracked files
/// (never in the patch: no staging step runs here) are never destroyed.
/// Otherwise it saves ONE net patch, `diff --binary <base>`: the working
/// tree against the commit the tree started from, so tracked work that is
/// staged, unstaged, or committed inside the tree lands in it together. An
/// empty patch leaves the tree in place. The raw status is saved as
/// `status.txt` through the scoped artifact operation after the patch, and
/// the worktree is removed only after both are durable. A failed status
/// record leaves the tree in place. Prune runs only when every candidate
/// salvaged with nothing left behind. It never merges: no apply argv runs
/// here. Any failure leaves the tree in place and names it in the one
/// summary notice, which also reports that the parent checkout was not
/// changed.
#[expect(
    clippy::too_many_lines,
    reason = "the salvage sweep reads as one ordered pass (scan, classify, report, prune); splitting it would scatter the failure-then-notice ordering its witnesses pin"
)]
async fn salvage_open_worktrees(
    services: &dyn Services,
    caller: &Caller,
    session: SessionId,
    workspace: &Path,
    data_root: &Path,
) {
    let session_dir = data_root.join("worktrees").join(session.to_string());
    let Ok(mut entries) = tokio::fs::read_dir(&session_dir).await else {
        return;
    };
    let mut candidates = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        if entry.file_type().await.is_ok_and(|kind| kind.is_dir())
            && let Some(task) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<JobId>().ok())
        {
            candidates.push((task, entry.path()));
        }
    }
    if candidates.is_empty() {
        return;
    }
    let Ok(JobsReply::Listed(jobs)) = services.jobs(caller, JobsOp::List).await else {
        return;
    };
    // The durable held set outlives a restart even when the held job's row
    // does not, so the table alone cannot exclude it.
    let Some(held) = read_held_set(services, caller).await else {
        return;
    };
    let mut lost = Vec::new();
    for (task, dir) in candidates {
        // A held job keeps its tree whatever else the table says.
        if held.contains(&task) {
            continue;
        }
        let row = jobs.iter().find(|job| job.id == task);
        let ended_lost = match row {
            // No ledger row: the task died with its process. The session
            // lock already excluded a second opener, so this tree is ours.
            None => true,
            Some(job) => matches!(job.state, JobStateView::Done(JobOutcome::Lost)),
        };
        // Anything else — a live job, or a job that ended another way —
        // keeps its tree in place.
        if ended_lost {
            lost.push((task, dir));
        }
    }
    // readdir order is filesystem-dependent; sort for deterministic argv.
    lost.sort_by_key(|(task, _)| task.to_string());
    if lost.is_empty() {
        return;
    }
    let workspace_text = workspace.display().to_string();
    let top = match git(
        services,
        caller,
        workspace,
        super::worktree::argv_toplevel(&workspace_text),
    )
    .await
    {
        Ok(output) if complete(&output) => Some(PathBuf::from(
            String::from_utf8_lossy(&output.stdout_prefix).trim(),
        )),
        _ => None,
    };
    let Some(top) = top else {
        let left = lost.iter().map(|(task, dir)| {
            format!(
                "{task} left at {}: the checkout top could not be resolved",
                dir.display()
            )
        });
        notify_salvage(services, caller, &left.collect::<Vec<_>>(), "");
        return;
    };
    let top_text = top.display().to_string();
    let listed = match git(
        services,
        caller,
        &top,
        salvage_argv_worktree_list(&top_text),
    )
    .await
    {
        Ok(output) if complete(&output) => output,
        _ => {
            // Without registration proof no tree may be touched.
            let left = lost.iter().map(|(task, dir)| {
                format!(
                    "{task} left at {}: worktree registration could not be read",
                    dir.display()
                )
            });
            notify_salvage(services, caller, &left.collect::<Vec<_>>(), "");
            return;
        }
    };
    let mut registered = HashSet::new();
    for line in listed.stdout_prefix.split(|byte| *byte == b'\n') {
        if let Some(path) = line.strip_prefix(b"worktree ".as_slice()) {
            registered.insert(path.to_vec());
        }
    }
    let mut salvaged = Vec::new();
    let mut left = Vec::new();
    for (task, dir) in lost {
        let dir_text = dir.display().to_string();
        // Creation receipt: only the battery makes isolation/<session>/<task>.
        let receipt = data_root
            .join("isolation")
            .join(session.to_string())
            .join(task.to_string());
        if !tokio::fs::symlink_metadata(&receipt)
            .await
            .is_ok_and(|meta| meta.file_type().is_dir())
        {
            left.push(format!(
                "{task} left at {dir_text}: no creation record; left in place"
            ));
            continue;
        }
        // A retained record marks a tree left in place for hand recovery. An
        // aborted run keeps its trees while a parked writer may still use
        // them, so salvage never takes a marked tree.
        if !certainly_absent(&receipt.join(ArtifactFile::RetainedJson.file_name())).await {
            left.push(format!(
                "{task} left at {dir_text}: a retained record marks it for hand recovery; left in place"
            ));
            continue;
        }
        if !registered.contains(dir_text.as_bytes()) {
            left.push(format!(
                "{task} left at {dir_text}: not a registered worktree of this checkout; left in place"
            ));
            continue;
        }
        // The base the tree started from: the one commit the net patch is
        // taken against. Without it commits made inside the tree could not
        // be told from the base, so no capture is attempted.
        let Some(base) = receipt_base(&receipt).await else {
            left.push(format!(
                "{task} left at {dir_text}: its creation record names no valid base commit; left in place"
            ));
            continue;
        };
        let status = git(services, caller, &dir, salvage_argv_status(&dir_text)).await;
        let status = match status {
            Ok(status) if complete(&status) => status,
            _ => {
                left.push(format!(
                    "{task} left at {dir_text}: the worktree status could not be read"
                ));
                continue;
            }
        };
        let Some((untracked, ignored)) = salvage_status_class(&status.stdout_prefix) else {
            left.push(format!(
                "{task} left at {dir_text}: the worktree status was not valid; left in place"
            ));
            continue;
        };
        if ignored {
            left.push(format!(
                "{task} left at {dir_text}: the worktree holds ignored files; left for hand recovery"
            ));
            continue;
        }
        // Untracked bytes never enter the net patch (no staging step runs
        // here), so a tree holding them stays in place with its contents.
        if untracked {
            left.push(format!(
                "{task} left at {dir_text}: the worktree holds untracked files; left for hand recovery"
            ));
            continue;
        }
        // One net patch: `diff <base>` reads the working tree against the
        // commit the tree started from, so tracked work that is staged,
        // unstaged, or committed inside the tree lands in it together and no
        // second diff may merge into it.
        let diff = git(
            services,
            caller,
            &dir,
            salvage_argv_net_diff(&dir_text, &base),
        )
        .await;
        let diff = match diff {
            Ok(diff) if complete(&diff) => diff,
            _ => {
                left.push(format!(
                    "{task} left at {dir_text}: the changes could not be computed"
                ));
                continue;
            }
        };
        // Nothing differs from the base: there is no work to capture, and an
        // uncertain classification never removes a tree.
        if diff.stdout_prefix.is_empty() {
            left.push(format!(
                "{task} left at {dir_text}: no captured changes; left in place"
            ));
            continue;
        }
        let names = git(
            services,
            caller,
            &dir,
            salvage_argv_net_names(&dir_text, &base),
        )
        .await;
        let names = match names {
            Ok(names) if complete(&names) => names,
            _ => {
                left.push(format!(
                    "{task} left at {dir_text}: the changed paths could not be read"
                ));
                continue;
            }
        };
        let paths_valid = parse_nul_names(&names.stdout_prefix)
            .is_some_and(|changed| super::delivery::changed_paths_valid(&changed));
        if !paths_valid {
            left.push(format!(
                "{task} left at {dir_text}: the changed paths were not valid names"
            ));
            continue;
        }
        let patch_path = receipt.join(ArtifactFile::DeltaPatch.file_name());
        let saved = services
            .sidecar(
                caller,
                SidecarOp::Artifact {
                    job: task,
                    file: ArtifactFile::DeltaPatch,
                    bytes: diff.stdout_prefix,
                },
            )
            .await;
        if let Err(error) = saved {
            left.push(format!(
                "{task} left at {dir_text}: the patch could not be saved: {error}"
            ));
            continue;
        }
        // The pre-stage status stays a distinct record next to the patch. It
        // records tracked staging state only: untracked and ignored trees are
        // refused above, so no untracked line can appear here. It travels
        // through the same durable artifact owner as the patch and is saved
        // before the removal, so a tree is never destroyed without both
        // records persisted.
        let status_saved = services
            .sidecar(
                caller,
                SidecarOp::Artifact {
                    job: task,
                    file: ArtifactFile::StatusTxt,
                    bytes: status.stdout_prefix,
                },
            )
            .await;
        if let Err(error) = status_saved {
            left.push(format!(
                "{task} left at {dir_text}: the status record could not be saved: {error}"
            ));
            continue;
        }
        let removed = git(
            services,
            caller,
            &top,
            super::worktree::argv_worktree_remove(&top_text, &dir_text),
        )
        .await;
        if let Some(why) = git_refusal(&removed) {
            left.push(format!("{task} left at {dir_text}: {why}"));
            continue;
        }
        salvaged.push(format!("{task} kept at {}", patch_path.display()));
    }
    // Prune only when every candidate salvaged with nothing left behind: a
    // prune beside a left tree could strand its registration.
    let mut prune_note = String::new();
    if !salvaged.is_empty() && left.is_empty() {
        let pruned = git(
            services,
            caller,
            &top,
            super::worktree::argv_worktree_prune(&top_text),
        )
        .await;
        if let Some(why) = git_refusal(&pruned) {
            prune_note = format!(" (worktree prune did not run: {why})");
        }
    } else if !left.is_empty() {
        prune_note = format!(
            " (worktree prune skipped: {} tree(s) left in place)",
            left.len()
        );
    }
    let mut parts = salvaged;
    parts.extend(left);
    notify_salvage(services, caller, &parts, &prune_note);
}

/// Sends the one open-salvage summary notice. The caller only invokes this
/// with a nonempty `parts`, so the notice always names a tree.
fn notify_salvage(services: &dyn Services, caller: &Caller, parts: &[String], prune_note: &str) {
    if parts.is_empty() {
        return;
    }
    let text = format!(
        "open salvage: {}. The parent checkout was not changed (no merge).{prune_note}",
        parts.join("; ")
    );
    services.notify(
        caller,
        Notice {
            turn: None,
            kind: "orchestration.salvage".into(),
            text: text.into(),
        },
    );
}

/// The kept outcome when staging, diffing, or saving the patch failed: the
/// tree stays in place at `dir` and no patch exists. The notice carries the
/// recovery path so `isolation: kept` never strands the sole copy.
fn kept_without_patch(reason: &str, dir: &Path) -> WorktreeEnd {
    WorktreeEnd {
        isolation: Some(super::worktree::IsolationOutcome::Kept {
            patch: None,
            reason: Some(reason.to_owned()),
        }),
        changed: Vec::new(),
        notice: format!(
            "isolation: kept at {}; {reason}. Recover the tree at that path.",
            dir.display()
        ),
    }
}
/// Derives the workspace path relative to its repository top.
/// Returns `None` when the workspace is not under the top so the caller
/// refuses instead of widening to the worktree root. Empty is valid only
/// for the top itself.
fn workspace_relative(workspace: &Path, top: &Path) -> Option<PathBuf> {
    let relative = workspace.strip_prefix(top).ok()?.to_path_buf();
    if relative.as_os_str().is_empty() && workspace != top {
        return None;
    }
    Some(relative)
}
#[cfg_attr(
    unix,
    expect(
        clippy::unnecessary_wraps,
        reason = "non-Unix refuses invalid UTF-8, so the Option is required portably"
    )
)]
/// Splits `git diff --name-only -z` output on NUL only, never on lines.
/// A trailing NUL terminator and empty output both yield no empty entry;
/// any interior empty entry survives so `changed_paths_valid` still refuses it.
/// Raw bytes survive untouched: on Unix each entry keeps its exact bytes so
/// a non-UTF-8 name can never decode to the replacement character and
/// collide with a workspace that literally names it; elsewhere invalid
/// UTF-8 refuses the whole list and the caller keeps the worktree unapplied.
fn parse_nul_names(bytes: &[u8]) -> Option<Vec<PathBuf>> {
    if bytes.is_empty() {
        return Some(Vec::new());
    }
    let mut trimmed = bytes;
    if trimmed.last() == Some(&0) {
        trimmed = &trimmed[..trimmed.len() - 1];
    }
    if trimmed.is_empty() {
        return Some(Vec::new());
    }
    #[cfg(unix)]
    {
        Some(
            trimmed
                .split(|byte| *byte == 0)
                .map(path_from_raw)
                .collect(),
        )
    }
    #[cfg(not(unix))]
    {
        trimmed
            .split(|byte| *byte == 0)
            .map(path_from_raw)
            .collect()
    }
}

/// Builds one changed path from its raw NUL-separated bytes, keeping the
/// exact bytes so a non-UTF-8 name never decodes to the replacement character.
#[cfg(unix)]
fn path_from_raw(chunk: &[u8]) -> PathBuf {
    PathBuf::from(<std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(chunk))
}

/// Builds one changed path from its raw NUL-separated bytes, refusing
/// names the platform cannot represent without lossy decoding.
#[cfg(not(unix))]
fn path_from_raw(chunk: &[u8]) -> Option<PathBuf> {
    std::str::from_utf8(chunk).ok().map(PathBuf::from)
}

/// Whether every changed path stays inside the run's nested workspace.
/// Empty `relative` is the top itself, so every relative path is in scope;
/// otherwise the check is component-wise `starts_with`, never a string prefix.
fn changed_within_workspace(changed: &[PathBuf], relative: &Path) -> bool {
    if relative.as_os_str().is_empty() {
        return true;
    }
    changed
        .iter()
        .all(|path| <PathBuf as AsRef<Path>>::as_ref(path).starts_with(relative))
}

/// One in-flight task of a pool or single step.
struct PendingTask {
    index: usize,
    task: JobId,
    label: String,
    item: Box<str>,
    dir: Option<PathBuf>,
    started: Instant,
    handle: dal_agent::ext::ScopeHandle,
}

/// One step's contribution to the run result.
enum StepOutcome {
    Tasks {
        name: String,
        pool: bool,
        results: Vec<TaskResult>,
    },
    Skipped {
        name: String,
        reason: String,
    },
}

/// The ledger of one run's steps: which started, which ended, and what each
/// contributed.
struct StepBook {
    reports: Vec<super::workflow::StepResult>,
    outcomes: Vec<Option<StepOutcome>>,
    started: Vec<bool>,
    ended: Vec<bool>,
    active: tokio::task::JoinSet<(usize, super::workflow::Step, Result<StepOutcome, String>)>,
}

impl StepBook {
    fn new(steps: usize) -> Self {
        Self {
            reports: Vec::new(),
            outcomes: (0..steps).map(|_| None).collect(),
            started: vec![false; steps],
            ended: vec![false; steps],
            active: tokio::task::JoinSet::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum ItemError {
    #[error("{0}")]
    Skip(String),
    #[error("{0}")]
    Failed(String),
}

/// The scope handles of one run's started children by task job, plus the
/// cancels that arrived before their child started. Shared under one lock
/// so publishing a handle and recording a cancel cannot interleave.
#[derive(Default)]
struct LiveTasks {
    live: HashMap<JobId, dal_agent::ext::ScopeHandle>,
    cancelled: HashSet<JobId>,
}

/// One background workflow run: owns its children through a scope, applies
/// the worktree isolation policy, settles every task job, and settles the
/// run's single top-level report. The owner task hears every task job and
/// the run end through its mailbox; nothing else shares battery state.
struct Coordinator {
    services: Arc<dyn Services>,
    caller: Caller,
    sender: mpsc::Sender<Message>,
    session: SessionId,
    workspace: PathBuf,
    data_root: Option<PathBuf>,
    base: Option<Base>,
    run: JobId,
    call: CallId,
    label: String,
    input: Option<String>,
    cancel: CancellationToken,
    /// Live scope handles by task job, plus task cancels that arrived
    /// before their child started. One lock makes publishing a handle and
    /// recording a cancel atomic: a cancel can neither miss a starting
    /// child nor strand an entry for one.
    tasks: Mutex<LiveTasks>,
    merge_locks: MergeLocks,
    reports: ReportCells,
    /// Tool rounds one child turn may use, for the grace reason.
    child_max_steps: u32,
    /// Minutes one child turn may run, for the grace reason.
    child_max_minutes: u32,
}

/// The worktree outcome of one ended task plus its notice lines.
struct WorktreeEnd {
    isolation: Option<super::worktree::IsolationOutcome>,
    changed: Vec<PathBuf>,
    notice: String,
}

impl Coordinator {
    #[expect(
        clippy::disallowed_methods,
        reason = "the owning RunHandle holds this task and aborts it when the run map drops"
    )]
    fn spawn(
        self,
        workflow: super::workflow::Workflow,
        control: mpsc::Receiver<RunControl>,
    ) -> tokio_util::task::AbortOnDropHandle<()> {
        tokio_util::task::AbortOnDropHandle::new(tokio::spawn(
            Arc::new(self).execute(workflow, control),
        ))
    }

    async fn execute(
        self: Arc<Self>,
        workflow: super::workflow::Workflow,
        control: mpsc::Receiver<RunControl>,
    ) {
        let started = Instant::now();
        let outcome = self.run_steps(&workflow, control).await;
        match outcome {
            Ok(steps) => self.finish(steps, started).await,
            Err(message) => self.fail(message).await,
        }
        let _ = self.sender.send(Message::RunEnd { run: self.run }).await;
    }

    async fn run_steps(
        self: &Arc<Self>,
        workflow: &super::workflow::Workflow,
        mut control: mpsc::Receiver<RunControl>,
    ) -> Result<Vec<StepOutcome>, String> {
        let mut book = StepBook::new(workflow.steps.len());
        let mut failure = None;
        loop {
            self.admit_steps(workflow, &mut book);
            if book.ended.iter().all(|done| *done) {
                break;
            }
            tokio::select! {
                Some(control) = control.recv() => {
                    self.on_control(control);
                }
                joined = book.active.join_next() => {
                    match joined {
                        Some(Ok((index, step, Ok(outcome)))) => {
                            Self::end_step(&mut book, index, &step, outcome);
                        }
                        Some(Ok((index, _, Err(message)))) => {
                            book.ended[index] = true;
                            self.cancel.cancel();
                            failure.get_or_insert(message);
                        }
                        Some(Err(error)) => {
                            self.cancel.cancel();
                            while book.active.join_next().await.is_some() {}
                            return Err(format!("a workflow step stopped unexpectedly: {error}"));
                        }
                        None => return Err("the workflow has unresolved dependencies".into()),
                    }
                }
            }
        }
        match failure {
            Some(message) => Err(message),
            None => Ok(book.outcomes.into_iter().flatten().collect()),
        }
    }

    /// Starts every step whose dependencies ended. A cancelled run starts
    /// nothing: its ready steps end skipped, which can ready their
    /// dependents in turn.
    fn admit_steps(self: &Arc<Self>, workflow: &super::workflow::Workflow, book: &mut StepBook) {
        loop {
            let mut skipped = false;
            for (index, step) in workflow.steps.iter().enumerate() {
                if book.started[index] || !step.after.iter().all(|&dep| book.ended[dep]) {
                    continue;
                }
                book.started[index] = true;
                if self.cancel.is_cancelled() {
                    let outcome = StepOutcome::Skipped {
                        name: step.name.clone(),
                        reason: CANCELLED_SKIP.to_owned(),
                    };
                    Self::end_step(book, index, step, outcome);
                    skipped = true;
                    continue;
                }
                let coordinator = Arc::clone(self);
                let step = step.clone();
                let reports = book.reports.clone();
                book.active.spawn(async move {
                    let result = coordinator.run_step(&step, &reports).await;
                    (index, step, result)
                });
            }
            if !skipped {
                return;
            }
        }
    }

    /// Records one ended step's outcome and the report its dependents read.
    fn end_step(
        book: &mut StepBook,
        index: usize,
        step: &super::workflow::Step,
        outcome: StepOutcome,
    ) {
        let report = match &outcome {
            StepOutcome::Tasks { results, .. } => Self::step_report(step, results),
            StepOutcome::Skipped { .. } => super::workflow::StepResult {
                name: step.name.clone(),
                task_report: None,
                pool_items: None,
            },
        };
        book.reports.push(report);
        book.ended[index] = true;
        book.outcomes[index] = Some(outcome);
    }

    async fn run_step(
        &self,
        step: &super::workflow::Step,
        reports: &[super::workflow::StepResult],
    ) -> Result<StepOutcome, String> {
        match Self::resolve_items(step, reports) {
            Ok(items) => {
                if matches!(step.items, super::workflow::Items::From(_)) {
                    let (reply, result) = oneshot::channel();
                    self.sender
                        .send(Message::ReserveChildren {
                            count: items.len(),
                            reply,
                        })
                        .await
                        .map_err(|_| "the orchestration owner closed".to_owned())?;
                    if let Err(left) = result
                        .await
                        .map_err(|_| "the orchestration owner closed".to_owned())?
                    {
                        return Err(super::pool::pool_budget_short(
                            &step.name,
                            items.len(),
                            left,
                        ));
                    }
                }
                Ok(StepOutcome::Tasks {
                    name: step.name.clone(),
                    pool: !matches!(step.items, super::workflow::Items::Task),
                    results: self.execute_step(step, &items, reports).await?,
                })
            }
            Err(ItemError::Skip(reason)) => Ok(StepOutcome::Skipped {
                name: step.name.clone(),
                reason,
            }),
            Err(ItemError::Failed(reason)) => Err(reason),
        }
    }

    /// Resolves one step's items, or the skip reason when the step never
    /// starts.
    fn resolve_items(
        step: &super::workflow::Step,
        reports: &[super::workflow::StepResult],
    ) -> Result<Vec<Option<String>>, ItemError> {
        match &step.items {
            super::workflow::Items::Task => Ok(vec![None]),
            super::workflow::Items::Literal(items) => Ok(items.iter().cloned().map(Some).collect()),
            super::workflow::Items::From(source) => Self::resolve_from(source, reports),
        }
    }

    fn resolve_from(
        source: &str,
        reports: &[super::workflow::StepResult],
    ) -> Result<Vec<Option<String>>, ItemError> {
        let Some(found) = reports
            .iter()
            .find(|report| report.name == source)
            .and_then(|report| report.task_report.as_deref())
        else {
            return Err(ItemError::Skip(skip_text(super::pool::unresolved_skip(
                source,
            ))));
        };
        let parts = super::pool::split_items(found);
        if parts.len() > super::pool::ITEM_LINES_LIMIT {
            return Err(ItemError::Failed(super::pool::too_many_items(
                source,
                parts.len(),
            )));
        }
        if parts.is_empty() {
            return Err(ItemError::Skip(skip_text(super::pool::no_items_skip(
                source,
            ))));
        }
        Ok(parts.into_iter().map(Some).collect())
    }

    /// Records one finished step for downstream `{{step:name}}` rendering.
    fn step_report(
        step: &super::workflow::Step,
        results: &[TaskResult],
    ) -> super::workflow::StepResult {
        let is_task = matches!(step.items, super::workflow::Items::Task);
        let task_report = match results.first() {
            Some(TaskResult {
                state: TaskState::Done(report),
                ..
            }) if is_task => Some(report.text.clone().into()),
            _ => None,
        };
        let pool_items = (!is_task).then(|| {
            results
                .iter()
                .map(|result| super::workflow::PoolItemResult {
                    item: result.item.clone(),
                    state: result.state.word().into(),
                    summary: super::delivery::preview(&result.body, 200).into_boxed_str(),
                })
                .collect()
        });
        super::workflow::StepResult {
            name: step.name.clone(),
            task_report,
            pool_items,
        }
    }

    /// Starts every child of one step under a scope of `workers` handles
    /// and stores results by item index, not finish order.
    async fn execute_step(
        &self,
        step: &super::workflow::Step,
        items: &[Option<String>],
        reports: &[super::workflow::StepResult],
    ) -> Result<Vec<TaskResult>, String> {
        let mut collector = IndexCollector::new(items.len());
        let spec = ScopeSpec {
            limit: u16::from(step.workers),
            on_error: OnError::Settle,
            budget: Budget::default(),
        };
        let scope = Scope::over(
            Arc::clone(&self.services),
            &self.caller,
            self.cancel.child_token(),
            spec,
        )
        .map_err(|error| error.to_string())?;
        let mut pending: HashMap<dal_agent::ext::ScopeHandleId, PendingTask> = HashMap::new();
        for (index, item) in items.iter().enumerate() {
            if let Err(error) = self
                .start_item(
                    step,
                    reports,
                    (index, item.as_deref()),
                    &scope,
                    &mut collector,
                    &mut pending,
                )
                .await
            {
                for task in pending.values() {
                    task.handle.cancel();
                }
                self.await_items(&scope, &mut pending, &mut collector).await;
                return Err(error);
            }
        }
        self.await_items(&scope, &mut pending, &mut collector).await;
        Ok(collector.into_ordered())
    }

    /// Starts one item: its task job, its worktree when the step isolates,
    /// and its child session inside the scope. An isolating step waits for
    /// a free worker before it creates the worktree, so queued items hold no
    /// checkout.
    async fn start_item(
        &self,
        step: &super::workflow::Step,
        reports: &[super::workflow::StepResult],
        member: (usize, Option<&str>),
        scope: &Scope,
        collector: &mut IndexCollector,
        pending: &mut HashMap<dal_agent::ext::ScopeHandleId, PendingTask>,
    ) -> Result<(), String> {
        let (index, item) = member;
        let task = self
            .spawn_task_job()
            .await
            .map_err(|error| error.to_string())?;
        let _ = self
            .sender
            .send(Message::RunTask {
                run: self.run,
                task,
            })
            .await;
        // The store rejects names composed only of session-id characters
        // (`a`, `deed`, `0123`) as ambiguous with an id; numbering every
        // task name keeps a hex-only step name usable.
        let label = item.map_or_else(
            || format!("{} {}", step.name, index + 1),
            |item| super::pool::item_label(&step.name, index, item),
        );
        if step.isolation == super::workflow::Isolation::Worktree {
            self.await_worker_slot(step, scope, pending, collector)
                .await;
        }
        // A run cancel and a task cancel that arrived while the item waited
        // for its worker slot both settle the item before it holds anything.
        if self.cancel.is_cancelled() || self.take_cancelled(task) {
            self.finish_unstarted_task(task, member, label, TaskState::Cancelled, collector)
                .await;
            return Ok(());
        }
        let dir = if step.isolation == super::workflow::Isolation::Worktree {
            match self.add_worktree(task).await {
                Ok(dir) => Some(dir),
                Err(reason) => {
                    self.finish_unstarted_task(
                        task,
                        member,
                        label,
                        TaskState::Failed(format!("worktree: {reason}")),
                        collector,
                    )
                    .await;
                    return Ok(());
                }
            }
        } else {
            None
        };
        let handle = self
            .child_start(step, item, reports, index, &label, dir.as_deref())
            .and_then(|start| scope.agent(start).map_err(|error| error.to_string()));
        let handle = match handle {
            Ok(handle) => handle,
            Err(reason) => {
                self.settle_unspawned(task, member, &label, dir.as_deref(), reason, collector)
                    .await;
                return Ok(());
            }
        };
        // Publishes the handle atomically against a concurrent cancel: the
        // handle is always published, and a run cancel or pre-start task
        // cancel observed here cancels it at once. The item then settles
        // through the normal path, so its worktree is removed only after
        // the child ends.
        let cancel_now = {
            let mut tasks = self
                .tasks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let cancel_now = self.cancel.is_cancelled() || tasks.cancelled.remove(&task);
            tasks.live.insert(task, handle.clone());
            cancel_now
        };
        if cancel_now {
            handle.cancel();
        }
        pending.insert(
            handle.id(),
            PendingTask {
                index,
                task,
                label,
                item: item.unwrap_or_default().into(),
                dir,
                started: Instant::now(),
                handle,
            },
        );
        Ok(())
    }

    /// Settles an item whose child never started: its worktree is kept or
    /// removed, and its task job ends with the reason.
    async fn settle_unspawned(
        &self,
        task: JobId,
        member: (usize, Option<&str>),
        label: &str,
        dir: Option<&Path>,
        reason: String,
        collector: &mut IndexCollector,
    ) {
        let (index, item) = member;
        let state = if self.cancel.is_cancelled() {
            TaskState::Cancelled
        } else {
            TaskState::Failed(reason.clone())
        };
        let end = self
            .end_worktree(task, dir, &state, &reason, "0.0s", label)
            .await;
        let mut text = super::delivery::task_text(
            task,
            label,
            self.run,
            state.word(),
            "0.0s",
            &end.changed,
            &reason,
        );
        if !end.notice.is_empty() {
            text.push('\n');
            text.push_str(&end.notice);
        }
        self.settle_job(task, job_outcome(&state), &text).await;
        collector.insert(
            index,
            TaskResult {
                id: task,
                state,
                changed: end.changed,
                isolation: end.isolation,
                body: reason.into_boxed_str(),
                item: item.unwrap_or_default().into(),
            },
        );
    }

    /// Waits until an isolating step has a free worker. A queued item then
    /// creates its worktree only once a child can start in it.
    async fn await_worker_slot(
        &self,
        step: &super::workflow::Step,
        scope: &Scope,
        pending: &mut HashMap<dal_agent::ext::ScopeHandleId, PendingTask>,
        collector: &mut IndexCollector,
    ) {
        let workers = usize::from(step.workers);
        while pending.len() >= workers && self.await_one(scope, pending, collector).await {}
    }

    /// Awaits every started child, honouring per-task cancels and the run
    /// cancellation.
    async fn await_items(
        &self,
        scope: &Scope,
        pending: &mut HashMap<dal_agent::ext::ScopeHandleId, PendingTask>,
        collector: &mut IndexCollector,
    ) {
        while !pending.is_empty() && self.await_one(scope, pending, collector).await {}
    }

    /// Awaits one started child and settles it, worktree included. `false`
    /// means the scope holds no more children.
    async fn await_one(
        &self,
        scope: &Scope,
        pending: &mut HashMap<dal_agent::ext::ScopeHandleId, PendingTask>,
        collector: &mut IndexCollector,
    ) -> bool {
        let Some(handle) = scope.next().await else {
            return false;
        };
        let Some(task) = pending.remove(&handle.id()) else {
            return true;
        };
        {
            let mut tasks = self
                .tasks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            tasks.live.remove(&task.task);
            // A cancel that landed after the child ended has no handle to
            // reach; drop its entry with the handle.
            tasks.cancelled.remove(&task.task);
        }
        let index = task.index;
        let result = self.finish_child(task, handle).await;
        collector.insert(index, result);
        true
    }

    fn on_control(&self, control: RunControl) {
        let RunControl::CancelTask(job) = control;
        let handle = {
            let mut tasks = self
                .tasks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            tasks.live.get(&job).cloned().or_else(|| {
                // The task registered but its child never started: it
                // waits for a worker slot or a worktree, so no handle
                // exists yet. `start_item` publishes under this same
                // lock and observes the entry instead.
                tasks.cancelled.insert(job);
                None
            })
        };
        if let Some(handle) = handle {
            handle.cancel();
        }
    }

    /// Takes a task cancel that arrived before its child started.
    fn take_cancelled(&self, task: JobId) -> bool {
        self.tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cancelled
            .remove(&task)
    }

    /// Builds one child's start spec; the scope starts and awaits it.
    fn child_start(
        &self,
        step: &super::workflow::Step,
        item: Option<&str>,
        reports: &[super::workflow::StepResult],
        index: usize,
        label: &str,
        dir: Option<&Path>,
    ) -> Result<dal_core::AgentStart, String> {
        let prompt =
            super::workflow::render::render(step, item, reports, self.input.as_deref(), self.run);
        let preamble = super::pool::preamble(label, &prompt);
        let mut tool_names = Vec::with_capacity(step.tools.len() + 1);
        for tool in &step.tools {
            let name = Name::parse(tool).map_err(|error| error.to_string())?;
            tool_names.push(name);
        }
        tool_names.push(Name::parse(REPORT_TOOL_NAME).map_err(|error| error.to_string())?);
        let workspace = match step.isolation {
            super::workflow::Isolation::Worktree => {
                let dir = dir.ok_or("worktree isolation requires a task worktree")?;
                let base = self
                    .base
                    .as_ref()
                    .ok_or("worktree isolation requires a verified base")?;
                Some(
                    dal_core::Workspace::new(dir.join(&base.relative_workspace))
                        .map_err(|error| error.to_string())?,
                )
            }
            super::workflow::Isolation::Shared => None,
        };
        Ok(dal_core::AgentStart {
            call: CallId::new(format!("{}-{}-{index}", self.call, step.name)),
            name: Some(label.to_owned().into_boxed_str()),
            prompt: preamble.into_boxed_str(),
            model: step.model.clone().map(String::into_boxed_str),
            role: step.role.clone().map(String::into_boxed_str),
            system: step.system.clone().map(String::into_boxed_str),
            tools: Some(tool_names.into_boxed_slice()),
            workspace,
        })
    }

    /// Settles one cancelled or failed task before any child starts.
    async fn finish_unstarted_task(
        &self,
        task: JobId,
        member: (usize, Option<&str>),
        label: String,
        state: TaskState,
        collector: &mut IndexCollector,
    ) {
        let (index, item) = member;
        let body = match &state {
            TaskState::Failed(reason) => reason.clone().into_boxed_str(),
            _ => state.word().into(),
        };
        let result = TaskResult {
            id: task,
            state,
            changed: Vec::new(),
            isolation: None,
            body,
            item: item.unwrap_or(&label).into(),
        };
        let outcome = job_outcome(&result.state);
        let text = super::delivery::task_text(
            task,
            &label,
            self.run,
            result.state.word(),
            "0.0s",
            &[],
            &result.body,
        );
        self.settle_job(task, outcome, &text).await;
        collector.insert(index, result);
    }

    /// Maps one finished child to its task result, ends its worktree, and
    /// settles its task job with the full task text.
    async fn finish_child(
        &self,
        task: PendingTask,
        handle: dal_agent::ext::ScopeHandle,
    ) -> TaskResult {
        let settled = match handle.result().await {
            Ok(dal_agent::ext::ScopeValue::Agent(report)) => self.settle_child(report).await,
            Err(dal_agent::ext::ScopeError::Cancelled) => super::pool::Settled {
                state: TaskState::Cancelled,
                note: None,
            },
            Err(other) => super::pool::Settled {
                state: TaskState::Failed(other.to_string()),
                note: None,
            },
            Ok(_) => super::pool::Settled {
                state: TaskState::Failed("the child returned no agent report".into()),
                note: None,
            },
        };
        let state = settled.state;
        let mut body = match &state {
            TaskState::Done(report) | TaskState::Blocked(report) => report.text.clone(),
            TaskState::Failed(text) => text.clone(),
            _ => String::new(),
        };
        if let Some(note) = &settled.note {
            body.push_str(" (");
            body.push_str(note);
            body.push(')');
        }
        let duration = format_duration(task.started.elapsed().as_secs());
        let end = self
            .end_worktree(
                task.task,
                task.dir.as_deref(),
                &state,
                &body,
                &duration,
                &task.label,
            )
            .await;
        let (state, body) = match (&state, &end.isolation) {
            (
                TaskState::Done(_),
                Some(super::worktree::IsolationOutcome::Kept {
                    patch: None,
                    reason: Some(why),
                }),
            ) => {
                let lost = format!("the subagent finished, but its changes were not kept: {why}.");
                (TaskState::Failed(lost.clone()), format!("{lost}\n\n{body}"))
            }
            _ => (state, body),
        };
        let mut text = super::delivery::task_text(
            task.task,
            &task.label,
            self.run,
            state.word(),
            &duration,
            &end.changed,
            &body,
        );
        if !end.notice.is_empty() {
            text.push('\n');
            text.push_str(&end.notice);
        }
        self.settle_job(task.task, job_outcome(&state), &text).await;
        TaskResult {
            id: task.task,
            state,
            changed: end.changed,
            isolation: end.isolation,
            body: body.into_boxed_str(),
            item: task.item,
        }
    }

    /// Takes the report a child stored through its own `report` tool.
    fn take_report(&self, child: SessionId) -> Option<Report> {
        self.reports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&child)
            .and_then(|cell| cell.get())
    }

    /// Settles one ended child: a stored report settles at once, and a
    /// child that ended without one earns a single grace prompt whose
    /// silence fails it. The child stays live across the grace turn and
    /// is released only once its verdict is in hand.
    async fn settle_child(&self, report: AgentReport) -> super::pool::Settled {
        let child = report.session;
        let stored = self.take_report(child);
        let end = child_end(
            stored,
            &report,
            self.child_max_steps,
            self.child_max_minutes,
        );
        let settled = match decide(&end) {
            ChildDecision::Settle(settled) => settled,
            ChildDecision::Grace(cause) => {
                self.run_grace_turn(child, report.entry, cause, &end.last_text)
                    .await
            }
        };
        release_child(self.services.as_ref(), &self.caller, child).await;
        settled
    }

    /// Runs the one grace turn: prompts the silent child with an interrupt
    /// deadline, then settles with the report that turn leaves. A prompt
    /// the agents service refuses fails the task without a report.
    async fn run_grace_turn(
        &self,
        child: SessionId,
        previous: EntryId,
        cause: GraceCause,
        first_text: &str,
    ) -> super::pool::Settled {
        let prompted = self
            .services
            .agents(
                &self.caller,
                AgentsOp::Prompt {
                    id: child,
                    text: grace_text(cause).into_boxed_str(),
                    interrupt: Some(Duration::from_secs_f64(GRACE_SECONDS)),
                    max_steps: std::num::NonZeroU32::new(1),
                },
            )
            .await;
        match prompted {
            Ok(AgentsReply::Prompted { .. }) => {
                let (late, grace_text) = self.await_grace_report(child, previous).await;
                let mut settled = decide_grace(late.as_ref(), cause);
                // Silence through grace shows the child's last message, from
                // the grace turn when it said something, else the first turn.
                let shown = if grace_text.trim().is_empty() {
                    first_text
                } else {
                    &grace_text
                };
                if late.is_none()
                    && !shown.trim().is_empty()
                    && let TaskState::Failed(text) = &mut settled.state
                {
                    text.push('\n');
                    text.push_str(&last_message_text(shown));
                }
                settled
            }
            Ok(reply) => refused_grace(format!(
                "the agents service refused the grace turn: {reply:?}"
            )),
            Err(error) => refused_grace(format!("the grace turn did not start: {error}")),
        }
    }

    /// Waits for the grace turn to end and reads the report it stored and
    /// its last assistant text; `previous` is the entry the first turn
    /// ended on. A wait that never moves past it, or dies early, reads as
    /// silence with no text.
    async fn await_grace_report(
        &self,
        child: SessionId,
        previous: EntryId,
    ) -> (Option<Report>, String) {
        let wait = async {
            loop {
                let waited = self
                    .services
                    .agents(
                        &self.caller,
                        AgentsOp::Await {
                            id: child,
                            timeout: None,
                        },
                    )
                    .await;
                match waited {
                    Ok(AgentsReply::Await { report }) if report.entry != previous => {
                        break (self.take_report(child), report.text.to_string());
                    }
                    Ok(_) => {}
                    Err(_) => break (self.take_report(child), String::new()),
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        };
        match tokio::time::timeout(Duration::from_secs_f64(GRACE_SECONDS + 5.0), wait).await {
            Ok(waited) => waited,
            Err(_) => (self.take_report(child), String::new()),
        }
    }

    /// Runs one git argv from `cwd` under the run's scoped grant.
    async fn git_in(&self, cwd: &Path, argv: Vec<String>) -> Result<RunOutput, ServiceError> {
        git(self.services.as_ref(), &self.caller, cwd, argv).await
    }

    /// Applies the worktree end policy of one ended task.
    async fn end_worktree(
        &self,
        task: JobId,
        dir: Option<&Path>,
        state: &TaskState,
        body: &str,
        duration: &str,
        label: &str,
    ) -> WorktreeEnd {
        let (Some(dir), Some(base), Some(root)) =
            (dir, self.base.as_ref(), self.data_root.as_ref())
        else {
            return WorktreeEnd {
                isolation: None,
                changed: Vec::new(),
                notice: String::new(),
            };
        };
        let dir_text = dir.display().to_string();
        let staged = self
            .git_in(dir, super::worktree::argv_add_all(&dir_text))
            .await;
        if !matches!(&staged, Ok(output) if complete(output)) {
            return kept_without_patch("the changes could not be staged", dir);
        }
        let diff = self
            .git_in(
                dir,
                super::worktree::argv_cached_diff(&dir_text, &base.commit),
            )
            .await;
        let names = self
            .git_in(
                dir,
                super::worktree::argv_cached_names(&dir_text, &base.commit),
            )
            .await;
        let (diff, names) = match (diff, names) {
            (Ok(diff), Ok(names)) if complete(&diff) && complete(&names) => (diff, names),
            (Ok(diff), Ok(names))
                if diff.stdout_prefix_overflowed || names.stdout_prefix_overflowed =>
            {
                return kept_without_patch("the changes are larger than this run can save", dir);
            }
            _ => return kept_without_patch("the changes could not be computed", dir),
        };
        let Some(changed) = parse_nul_names(&names.stdout_prefix) else {
            return kept_without_patch("the changed paths were not valid names", dir);
        };
        if !super::delivery::changed_paths_valid(&changed) {
            return kept_without_patch("the changed paths were not relative", dir);
        }
        if diff.stdout_prefix.is_empty() {
            let notice = self.remove_worktree(&base.top, dir).await;
            return WorktreeEnd {
                isolation: Some(super::worktree::IsolationOutcome::Clean),
                changed,
                notice,
            };
        }
        let patch_path = root
            .join("isolation")
            .join(self.session.to_string())
            .join(task.to_string())
            .join("delta.patch");
        if let Err(error) = self
            .write_artifact(task, ArtifactFile::DeltaPatch, diff.stdout_prefix)
            .await
        {
            return kept_without_patch(&format!("the patch could not be saved: {error}"), dir);
        }
        let summary = super::delivery::task_text(
            task,
            label,
            self.run,
            state.word(),
            duration,
            &changed,
            body,
        );
        let mut notice = match self
            .write_artifact(task, ArtifactFile::SummaryTxt, summary.into_bytes())
            .await
        {
            Ok(()) => String::new(),
            Err(error) => format!(" (summary.txt could not be saved: {error})"),
        };
        if !state.is_done() {
            notice.push_str(&self.remove_worktree(&base.top, dir).await);
            return WorktreeEnd {
                isolation: Some(super::worktree::IsolationOutcome::Kept {
                    patch: Some(patch_path),
                    reason: None,
                }),
                changed,
                notice,
            };
        }
        let mut merged = self
            .merge_worktree(task, &base.top, dir, &patch_path, &changed)
            .await;
        merged.notice.push_str(&notice);
        merged
    }

    /// Applies or retains one done task's delta under the merge lock.
    async fn merge_worktree(
        &self,
        task: JobId,
        top: &Path,
        dir: &Path,
        patch: &Path,
        changed: &[PathBuf],
    ) -> WorktreeEnd {
        let escapes = self
            .base
            .as_ref()
            .is_some_and(|base| !changed_within_workspace(changed, &base.relative_workspace));
        if escapes {
            return self
                .retain_worktree(
                    task,
                    top,
                    dir,
                    patch,
                    "the changes escape the task workspace",
                    changed,
                )
                .await;
        }
        let _hold = self.merge_locks.hold(top).await;
        let top_text = top.display().to_string();
        let patch_text = patch.display().to_string();
        let checked = self
            .git_in(
                &self.workspace,
                super::worktree::argv_apply_check(&top_text, &patch_text),
            )
            .await;
        if let Some(why) = git_refusal(&checked) {
            return self
                .retain_worktree(task, top, dir, patch, &why, changed)
                .await;
        }
        let applied = self
            .git_in(
                &self.workspace,
                super::worktree::argv_apply(&top_text, &patch_text),
            )
            .await;
        if let Some(why) = git_refusal(&applied) {
            return self
                .retain_worktree(task, top, dir, patch, &why, changed)
                .await;
        }
        let notice = self.remove_worktree(top, dir).await;
        WorktreeEnd {
            isolation: Some(super::worktree::IsolationOutcome::Merged),
            changed: changed.to_vec(),
            notice,
        }
    }

    /// Retains one unmergeable worktree and records why. A move that fails
    /// leaves the tree where it is and says so.
    async fn retain_worktree(
        &self,
        task: JobId,
        top: &Path,
        dir: &Path,
        patch: &Path,
        why: &str,
        changed: &[PathBuf],
    ) -> WorktreeEnd {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis())
            .unwrap_or_default();
        let target = PathBuf::from(format!("{}.retained-{millis}", dir.display()));
        let top_text = top.display().to_string();
        let moved = self
            .git_in(
                &self.workspace,
                super::worktree::argv_worktree_move(
                    &top_text,
                    &dir.display().to_string(),
                    &target.display().to_string(),
                ),
            )
            .await;
        let mut notes = String::new();
        let worktree = if let Some(failure) = git_refusal(&moved) {
            let _ = write!(notes, " (the worktree could not be moved: {failure})");
            dir.to_path_buf()
        } else {
            let pruned = self
                .git_in(
                    &self.workspace,
                    super::worktree::argv_worktree_prune(&top_text),
                )
                .await;
            if let Some(failure) = git_refusal(&pruned) {
                let _ = write!(notes, " (stale worktree records remain: {failure})");
            }
            target
        };
        let reason = format!("the changes did not apply cleanly ({why})");
        let at = super::goal::sidecar::format_millis(dal_core::Timestamp::now());
        let base = self
            .base
            .as_ref()
            .map(|base| base.commit.clone())
            .unwrap_or_default();
        let saved = match super::worktree::retained_body(
            &reason,
            &base,
            &worktree.display().to_string(),
            &at,
        ) {
            Ok(record) => self
                .write_artifact(task, ArtifactFile::RetainedJson, record.into_bytes())
                .await
                .map_err(|error| error.to_string()),
            Err(error) => Err(error.to_string()),
        };
        if let Err(error) = saved {
            let _ = write!(notes, " (retained.json could not be saved: {error})");
        }
        let mut notice = super::worktree::retained_notice(
            &worktree.display().to_string(),
            why,
            &self.workspace.display().to_string(),
            &patch.display().to_string(),
        );
        notice.push_str(&notes);
        WorktreeEnd {
            isolation: Some(super::worktree::IsolationOutcome::Retained {
                worktree,
                patch: patch.to_path_buf(),
                reason,
            }),
            changed: changed.to_vec(),
            notice,
        }
    }

    /// Removes one worktree; a refusal keeps the outcome and appends its
    /// notice line.
    async fn remove_worktree(&self, top: &Path, dir: &Path) -> String {
        let removed = self
            .git_in(
                &self.workspace,
                super::worktree::argv_worktree_remove(
                    &top.display().to_string(),
                    &dir.display().to_string(),
                ),
            )
            .await;
        match git_refusal(&removed) {
            Some(why) => format!(" (worktree left at {}: {why})", dir.display()),
            None => String::new(),
        }
    }

    /// Writes one isolation artifact through the sidecar service.
    async fn write_artifact(
        &self,
        task: JobId,
        file: ArtifactFile,
        bytes: Vec<u8>,
    ) -> Result<(), ServiceError> {
        match self
            .services
            .sidecar(
                &self.caller,
                SidecarOp::Artifact {
                    job: task,
                    file,
                    bytes,
                },
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(error) => Err(ServiceError::failed(
                Some(dal_core::Service::Sidecar),
                error.to_string(),
            )),
        }
    }

    /// Spawns one task job under the run job.
    async fn spawn_task_job(&self) -> Result<JobId, ServiceError> {
        let name = Name::parse(TASK_JOB_NAME)
            .map_err(|error| ServiceError::failed(None, error.to_string()))?;
        let payload =
            RawJson::parse("{}").map_err(|error| ServiceError::failed(None, error.to_string()))?;
        match self
            .services
            .jobs(
                &self.caller,
                JobsOp::Spawn {
                    name,
                    payload,
                    parent: Some(self.run),
                },
            )
            .await?
        {
            JobsReply::Spawned { id } => Ok(id),
            JobsReply::Unavailable { reason } => Err(ServiceError::failed(None, reason)),
            JobsReply::Refused(error) => Err(ServiceError::failed(None, error.to_string())),
            _ => Err(ServiceError::failed(
                None,
                "jobs service returned an unexpected reply to a spawn request",
            )),
        }
    }

    /// Creates one detached worktree for a task at the run base.
    async fn add_worktree(&self, task: JobId) -> Result<PathBuf, String> {
        let (Some(root), Some(base)) = (self.data_root.as_ref(), self.base.as_ref()) else {
            return Err("no isolated worktree root is available".into());
        };
        let dir = root
            .join("worktrees")
            .join(self.session.to_string())
            .join(task.to_string());
        let output = git(
            self.services.as_ref(),
            &self.caller,
            &self.workspace,
            super::worktree::argv_worktree_add(
                &base.top.display().to_string(),
                &dir.display().to_string(),
                &base.commit,
            ),
        )
        .await
        .map_err(|error| error.to_string())?;
        let result: Result<RunOutput, ServiceError> = Ok(output);
        if let Some(why) = git_refusal(&result) {
            return Err(why);
        }
        // Creation receipt: open-time salvage trusts only trees that carry
        // it plus worktree-list registration, and captures work against the
        // base it records. Fail loudly so no task starts unsalvageable
        // later; the tree is fresh and empty, so it goes with the failure.
        if let Err(error) = record_isolation_receipt(root, self.session, task, &base.commit).await {
            let removal = self.remove_worktree(&base.top, &dir).await;
            return Err(format!(
                "the isolation receipt could not be recorded: {error}{removal}"
            ));
        }
        Ok(dir)
    }

    /// Settles the run's single top-level report.
    async fn finish(&self, steps: Vec<StepOutcome>, started: Instant) {
        let duration = format_duration(started.elapsed().as_secs());
        let result = super::delivery::RunResult {
            id: self.run,
            label: self.label.clone(),
            tasks: steps
                .iter()
                .flat_map(StepOutcome::results)
                .cloned()
                .collect(),
        };
        let total = result.tasks.len();
        let unfinished = result.unfinished();
        let sections = sections_of(&steps);
        let cancelled = self.cancel.is_cancelled();
        let skipped_by_cancel = steps.iter().any(StepOutcome::skipped_by_cancel);
        let (text, outcome) = if result.is_done() && !skipped_by_cancel {
            (
                super::delivery::run_notice(
                    self.run,
                    &self.label,
                    "done",
                    &duration,
                    &sections,
                    super::delivery::RUN_NOTICE_LIMIT,
                ),
                JobOutcome::Exited { code: 0 },
            )
        } else if cancelled {
            (
                super::delivery::cancel_summary(
                    self.run,
                    &self.label,
                    &duration,
                    &sections,
                    super::delivery::RUN_NOTICE_LIMIT,
                ),
                JobOutcome::Cancelled,
            )
        } else {
            let message = super::pool::unfinished_text(unfinished, total)
                .unwrap_or_else(|| "tasks did not finish".to_owned());
            (
                super::delivery::run_notice(
                    self.run,
                    &self.label,
                    "failed",
                    &duration,
                    &sections,
                    super::delivery::RUN_NOTICE_LIMIT,
                ),
                JobOutcome::Failed {
                    message: message.into(),
                },
            )
        };
        self.settle_job(self.run, outcome, &text).await;
    }

    /// Fails the whole run before its report renders.
    async fn fail(&self, message: String) {
        let text = format!("run {} \"{}\" failed: {message}", self.run, self.label);
        self.settle_job(
            self.run,
            JobOutcome::Failed {
                message: message.into(),
            },
            &text,
        )
        .await;
    }

    /// Ends one owned job through the jobs service; a failure at run end
    /// has no caller left, so it becomes a notice.
    async fn settle_job(&self, job: JobId, outcome: JobOutcome, text: &str) {
        let reply = self
            .services
            .jobs(
                &self.caller,
                JobsOp::Settle {
                    id: job,
                    outcome,
                    text: text.into(),
                },
            )
            .await;
        let failure = match reply {
            Ok(JobsReply::Settled { .. }) => None,
            Ok(JobsReply::Refused(error)) => Some(error.to_string()),
            Ok(JobsReply::Unavailable { reason }) => Some(reason.to_string()),
            Ok(_) => Some("unexpected reply".to_owned()),
            Err(error) => Some(error.to_string()),
        };
        if let Some(failure) = failure {
            self.services.notify(
                &self.caller,
                Notice {
                    turn: None,
                    kind: "orchestration.run".into(),
                    text: format!("The job {job} could not be recorded: {failure}.").into(),
                },
            );
        }
    }
}

/// The skip text of one skip helper's task state.
fn skip_text(state: TaskState) -> String {
    match state {
        TaskState::Skipped(text) => text,
        _ => String::new(),
    }
}

/// Maps one task state to its job outcome.
fn job_outcome(state: &TaskState) -> JobOutcome {
    match state {
        TaskState::Done(_) | TaskState::Blocked(_) => JobOutcome::Exited { code: 0 },
        TaskState::Cancelled => JobOutcome::Cancelled,
        TaskState::Failed(message) | TaskState::Skipped(message) => JobOutcome::Failed {
            message: message.clone().into(),
        },
    }
}

/// Builds the notice sections of one run from its step outcomes.
fn sections_of(steps: &[StepOutcome]) -> Vec<super::delivery::StepNotice<'_>> {
    steps
        .iter()
        .map(|step| match step {
            StepOutcome::Skipped { name, reason } => super::delivery::StepNotice {
                name,
                tasks: Vec::new(),
                pool: false,
                skipped: Some(reason),
            },
            StepOutcome::Tasks {
                name,
                pool,
                results,
            } => super::delivery::StepNotice {
                name,
                pool: *pool,
                skipped: None,
                tasks: results
                    .iter()
                    .map(|result| super::delivery::TaskNotice {
                        id: result.id,
                        label: &result.item,
                        state: &result.state,
                        changed: &result.changed,
                        preview_text: &result.body,
                        suffix: super::delivery::isolation_suffix(result.isolation.as_ref()),
                    })
                    .collect(),
            },
        })
        .collect()
}

impl StepOutcome {
    /// Whether this step never started because the run was cancelled.
    fn skipped_by_cancel(&self) -> bool {
        matches!(self, Self::Skipped { reason, .. } if reason == CANCELLED_SKIP)
    }

    fn results(&self) -> &[TaskResult] {
        match self {
            StepOutcome::Tasks { results, .. } => results,
            StepOutcome::Skipped { .. } => &[],
        }
    }
}

fn failed(message: &str) -> HookError {
    HookError::Failed {
        message: message.into(),
    }
}

/// The result of cancelling every active descendant of one session.
#[derive(Default)]
struct Sweep {
    failures: Vec<String>,
}

impl Sweep {
    /// The failures joined for a reply or notice, or `None` when every
    /// cancel worked.
    fn failure_text(&self) -> Option<String> {
        if self.failures.is_empty() {
            None
        } else {
            Some(self.failures.join("; "))
        }
    }
}

/// Cancels every queued or running session the host lists for `caller`.
/// The host may reveal deeper descendants while the first ones close, so
/// the list is read again until no unvisited session is left or the fixed
/// pass limit is reached. A visited set keeps each session to one cancel,
/// and a failure never stops the walk: it is recorded next to its session.
async fn cancel_descendants(services: &dyn Services, caller: &Caller) -> Sweep {
    let mut sweep = Sweep::default();
    let mut visited = HashSet::new();
    for pass in 0..CANCEL_SWEEP_PASS_LIMIT {
        let listed = match services.agents(caller, AgentsOp::List).await {
            Ok(AgentsReply::Listed(agents)) => agents,
            Ok(_) => {
                sweep.failures.push(
                    "the agents service returned an unexpected reply to a list request".to_owned(),
                );
                return sweep;
            }
            Err(error) => {
                sweep
                    .failures
                    .push(format!("the child sessions could not be listed: {error}"));
                return sweep;
            }
        };
        let pending: Vec<SessionId> = listed
            .iter()
            .filter(|agent| matches!(agent.state, AgentState::Queued | AgentState::Running))
            .map(|agent| agent.id)
            .filter(|id| visited.insert(*id))
            .collect();
        if pending.is_empty() {
            return sweep;
        }
        for id in pending {
            match services.agents(caller, AgentsOp::Cancel { id }).await {
                Ok(AgentsReply::Cancelled { .. }) => {}
                Ok(_) => sweep.failures.push(format!(
                    "{id}: the agents service returned an unexpected reply to a cancel request"
                )),
                Err(error) => sweep.failures.push(format!("{id}: {error}")),
            }
        }
        if pass + 1 == CANCEL_SWEEP_PASS_LIMIT {
            let active = match services.agents(caller, AgentsOp::List).await {
                Ok(AgentsReply::Listed(agents)) => agents
                    .into_iter()
                    .filter(|agent| matches!(agent.state, AgentState::Queued | AgentState::Running))
                    .map(|agent| agent.id.to_string())
                    .collect::<Vec<_>>(),
                Ok(_) => {
                    sweep.failures.push(
                        "the agents service returned an unexpected reply while checking the remaining active sessions"
                            .to_owned(),
                    );
                    return sweep;
                }
                Err(error) => {
                    sweep.failures.push(format!(
                        "the remaining active child sessions could not be listed: {error}"
                    ));
                    return sweep;
                }
            };
            if !active.is_empty() {
                sweep.failures.push(format!(
                    "the cancellation sweep reached its pass limit; active child sessions: {}",
                    active.join(", ")
                ));
            }
            return sweep;
        }
    }
    sweep
}

/// Closes a run child whose report has been taken. The run owns the child
/// and never addresses it again, so nothing else would close it: with no
/// client attached, no front end would either, and every finished child
/// would keep its runtime until the whole session ended. The close is
/// awaited before the run starts its next child, so a new child never
/// begins while an earlier release is still pending. A refused close does
/// not fail the run, because the report is already in hand; it is reported
/// as a notice instead.
async fn release_child(services: &dyn Services, caller: &Caller, child: SessionId) {
    let failure = match services
        .agents(caller, AgentsOp::Cancel { id: child })
        .await
    {
        Ok(AgentsReply::Cancelled { .. }) => return,
        Ok(_) => "the agents service returned an unexpected reply to a close request".to_owned(),
        Err(error) => error.to_string(),
    };
    services.notify(
        caller,
        Notice {
            turn: None,
            kind: "orchestration.release".into(),
            text: format!(
                "The finished child session {child} was not closed: {failure}. Its report is kept, but it may still hold memory until this session ends."
            )
            .into(),
        },
    );
}

pub(crate) struct SessionStartHook(pub(crate) Runtime);

impl ObserveHook<SessionStart> for SessionStartHook {
    fn call(&self, start: SessionStart, cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        let runtime = self.0.clone();
        Box::pin(async move { runtime.open(start, cx).await })
    }
}

pub(crate) struct SessionEndHook(pub(crate) Runtime);

impl ObserveHook<SessionEnd> for SessionEndHook {
    fn call(&self, end: SessionEnd, cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        let runtime = self.0.clone();
        Box::pin(async move {
            runtime.close(cx.session, end).await;
            Ok(())
        })
    }
}

pub(crate) struct BeforeTurnHook(pub(crate) Runtime);

impl Hook<BeforeTurn, Option<String>> for BeforeTurnHook {
    fn call(
        &self,
        _event: BeforeTurn,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<Option<String>, HookError>> {
        let runtime = self.0.clone();
        let session = cx.session;
        let deadline = cx.deadline;
        let cancel = cx.cancel;
        Box::pin(async move {
            runtime
                .hook_request(session, deadline, cancel, |reply| {
                    Message::BeforeTurn(reply)
                })
                .await?;
            Ok(None)
        })
    }
}

pub(crate) struct InputHook(pub(crate) Runtime);

impl Hook<InputEvent, InputVerdict> for InputHook {
    fn call(
        &self,
        _input: InputEvent,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<InputVerdict, HookError>> {
        let runtime = self.0.clone();
        let session = cx.session;
        let deadline = cx.deadline;
        // The hook's select arms race the waiter against the opening token;
        // the owner's queued copy re-checks it at dequeue, so an
        // already-cancelled prompt never mutates state.
        let queued = cx.cancel.clone();
        let cancel = cx.cancel;
        Box::pin(async move {
            match runtime
                .hook_request(session, deadline, cancel, move |reply| Message::Input {
                    reply,
                    cancel: queued,
                })
                .await?
            {
                Ok(()) => Ok(InputVerdict::Continue),
                Err(error) => Err(failed(&error.to_string())),
            }
        })
    }
}

pub(crate) struct ToolCallHook(pub(crate) Runtime);

impl Hook<ToolCallEvent, ToolCallVerdict> for ToolCallHook {
    fn call(
        &self,
        event: ToolCallEvent,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<ToolCallVerdict, HookError>> {
        let runtime = self.0.clone();
        let session = cx.session;
        let deadline = cx.deadline;
        let cancel = cx.cancel;
        Box::pin(async move {
            runtime
                .hook_request(session, deadline, cancel, |reply| {
                    Message::ToolCall(event, reply)
                })
                .await
        })
    }
}

pub(crate) struct ToolResultHook(pub(crate) Runtime);

impl ObserveHook<ToolResultEvent> for ToolResultHook {
    fn call(
        &self,
        event: ToolResultEvent,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<(), HookError>> {
        let runtime = self.0.clone();
        let session = cx.session;
        let deadline = cx.deadline;
        let cancel = cx.cancel;
        Box::pin(async move {
            runtime
                .hook_request(session, deadline, cancel, |reply| {
                    Message::ToolResult(event, reply)
                })
                .await
        })
    }
}

pub(crate) struct TurnEndHook(pub(crate) Runtime);

impl ObserveHook<TurnEnd> for TurnEndHook {
    fn call(&self, event: TurnEnd, cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        let runtime = self.0.clone();
        let session = cx.session;
        let deadline = cx.deadline;
        let cancel = cx.cancel;
        Box::pin(async move {
            runtime
                .hook_request(session, deadline, cancel, |reply| {
                    Message::TurnEnd(event, reply)
                })
                .await
        })
    }
}

pub(crate) struct SettledHook(pub(crate) Runtime);

impl ObserveHook<Settled> for SettledHook {
    fn call(&self, event: Settled, cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        let runtime = self.0.clone();
        let session = cx.session;
        let deadline = cx.deadline;
        let cancel = cx.cancel;
        Box::pin(async move {
            match runtime
                .hook_request(session, deadline, cancel, |reply| {
                    Message::Settled(event, reply)
                })
                .await?
            {
                Ok(()) => Ok(()),
                Err(error) => Err(failed(&error.to_string())),
            }
        })
    }
}

pub(crate) struct Status(pub(crate) Runtime);

impl StatusPoll for Status {
    fn snapshot(&self, cx: &StatusCx) -> StatusSnapshot {
        self.0.poll(cx.session).unwrap_or(StatusSnapshot {
            quiet: true,
            text: None,
        })
    }
}

fn stop_kind(stop: dal_core::Stop) -> StopKind {
    match stop {
        dal_core::Stop::EndTurn => StopKind::Completed,
        dal_core::Stop::Length | dal_core::Stop::MaxSteps => StopKind::Length,
        dal_core::Stop::Filter => StopKind::Filter,
        dal_core::Stop::Cancelled => StopKind::Cancelled,
        dal_core::Stop::Failed => StopKind::Error,
    }
}
