//! Pure, deterministic session-state transitions and journal replay.
//!
//! The reducer owns protocol state only. Provider calls, clocks, I/O, locks,
//! extension execution, and request brokering remain effects of the actor.

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU64;

use crate::approval::{PlannedCall, Policy, ToolClass, Unit, plan};
use crate::command::{CancelScope, Command, Expect, Rejection, Reply};
use crate::config::{ApprovalMode, Mode};
use crate::content::Part;
use crate::ext::{HookEvent, HookOutcome, HookVerdict, Name, StreamVerdict, ToolCallVerdict};
use crate::id::{CallId, ClientId, EntryId, Gen, JobId, RequestId, TurnId};
use crate::journal::{
    AssistantStop, Block, DecodeError, Entry, EntryKind, JobEvent, JobKind, JobOutcome,
    JournalPart, Record, TurnEndStop,
};
use crate::model::{
    Family, InferFailure, Inference, ModelRoute, RequestParams, Stop, StreamChannel, StreamEvent,
    ThinkingLevel, Usage,
};
use crate::raw::RawJson;
use crate::request::{Answer, Question, Request};
use crate::update::{Notice, ToolOutcomeView, TurnCause, UpdateKind};
use crate::view::{EntryView, SettingsView, TreeDelta, TurnState};

/// Assistant content retained by the actor for one interrupted stream.
#[derive(Clone, Debug, PartialEq)]
pub struct PartialResponse {
    /// The text, reasoning, and calls received before interruption.
    pub content: Vec<Block>,
    /// Usage reported before interruption.
    pub usage: Usage,
}

const MAX_STEERS: usize = 16;
const MAX_WAKE_RUN: u32 = 20;
const MAX_INTERRUPTS: u32 = 3;
const PRODUCT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Compaction trigger and retained-context policy supplied by the actor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompactionLimits {
    /// Fraction of the context window that triggers automatic compaction.
    pub threshold: f64,
    /// Do not compact histories smaller than this token count.
    pub min_tokens: u64,
    /// Context tokens to retain before the newest user boundary.
    pub keep_tokens: u64,
    /// Whether automatic compaction is enabled.
    pub enabled: bool,
    /// Whether the actor has a compactor registered.
    pub compactor_available: bool,
}

/// Why a compaction was requested.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompactionReason {
    /// The latest request crossed the configured context-window ratio.
    Threshold,
    /// The provider rejected a request because the context overflowed.
    Overflow,
    /// The user requested compaction explicitly.
    Manual,
}

/// Token and payload data returned by a compactor.
#[derive(Clone, Debug, PartialEq)]
pub struct CompactionSummary {
    /// The compactor that produced the summary.
    pub compactor: Name,
    /// Token count before compaction.
    pub tokens_before: u64,
    /// Token count after compaction.
    pub tokens_after: u64,
    /// Optional human-readable summary stored in the journal.
    pub summary: Option<Box<str>>,
    /// First retained entry in the prior tree.
    pub first_kept: Option<EntryId>,
    /// Provider replay payload, if the compactor supplied one.
    pub replay: Option<RawJson>,
    /// Compactor inference usage, if available.
    pub usage: Option<Usage>,
}

/// One provider request prepared at a session boundary.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelRequestPlan {
    /// Turn that owns the request.
    pub turn: TurnId,
    /// Parameters to pass to the provider request builder.
    pub params: RequestParams,
}

/// One deterministic group of model-resolved calls.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedCall {
    /// Provider call identity.
    pub call: CallId,
    /// Registered tool name.
    pub name: Name,
    /// Whether this call promotes a deferred tool for later turns.
    pub promoted: bool,
    /// Result of resolving the tool name and arguments.
    pub result: Result<ToolClass, ResolveError>,
}

/// A tool-call resolution failure rendered as a model-visible result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolveError {
    /// No tool with this name is registered.
    Unknown,
    /// The tool is available only inside evaluation cells.
    EvalOnly,
    /// The tool arguments failed schema validation.
    InvalidArgs(
        /// The argument validation detail.
        Box<str>,
    ),
    /// A later call reused a prior call id in the same response.
    DuplicateCallId,
}

/// A settled dispatcher result.
#[derive(Clone, Debug, PartialEq)]
pub enum SettledOutcome {
    /// The tool returned successfully.
    Ok {
        /// The text result returned by the tool.
        text: Box<str>,
    },
    /// The tool returned an error.
    Err {
        /// The error text returned by the tool.
        text: Box<str>,
    },
    /// The call was interrupted by cancellation.
    Interrupted,
    /// The foreground call detached into a tracked job.
    Detached {
        /// The job tracking the detached call.
        job: JobId,
    },
}

/// A bounded unit number in the active turn.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct Step(
    /// The number of completed tool rounds.
    pub u32,
);

/// The source of a turn's initial input.
#[derive(Clone, Debug, PartialEq)]
pub enum TurnSource {
    /// A prompt submitted for a later turn.
    Prompt {
        /// The client that submitted the prompt.
        by: ClientId,
        /// The prompt's content parts.
        content: Vec<Part>,
    },
    /// A background wake.
    Wake {
        /// Sources that caused the wake.
        sources: Box<[Box<str>]>,
        /// Jobs whose completion caused the wake.
        jobs: Box<[JobId]>,
        /// The wake's prompt content.
        content: Vec<Part>,
    },
    /// A user-supplied follow-up queued behind the active turn.
    FollowUp {
        /// The client that submitted the follow-up.
        by: ClientId,
        /// The follow-up's content parts.
        content: Vec<Part>,
    },
}

/// The actor-visible lifecycle phase for one session.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Phase {
    /// No turn or compaction job is active.
    Idle,
    /// Input hooks are running before the first durable turn record.
    Opening {
        /// The allocated turn.
        turn: TurnId,
        /// How the turn was requested.
        source: TurnSource,
        /// Entry identity reserved for this input.
        entry: EntryId,
    },
    /// A model turn is active.
    Running {
        /// The active turn.
        turn: TurnId,
        /// Number of completed model/dispatch rounds.
        round: Step,
        /// Current turn stage.
        stage: TurnStage,
    },
    /// A turn's durable end is written; actor publication/settlement follows.
    Settling {
        /// The settled turn.
        turn: TurnId,
        /// Next queued turn id and its input, when this turn may continue.
        follow_up: Option<(TurnId, TurnSource)>,
    },
    /// A manual compaction job is active.
    Compacting {
        /// The tracked job id, when minted by the actor.
        job: Option<JobId>,
    },
    /// No further command is accepted.
    Closed,
}

/// The active step within a running turn.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum TurnStage {
    /// The turn is waiting to begin a model request.
    Boundary,
    /// The actor is resolving names and arguments for returned calls.
    Resolving {
        /// Calls committed in the assistant response, in provider order.
        pending: Vec<PendingCall>,
    },
    /// The provider stream is active.
    Streaming {
        /// Assistant text/reasoning/tool-call blocks received so far.
        blocks: Vec<Block>,
        /// Current accumulated usage.
        usage: Usage,
        /// Completed call identities from this response, in provider order.
        calls: Vec<(CallId, Box<str>)>,
        /// Injects retained after interrupt resends.
        suppressed_injects: Vec<Box<str>>,
    },
    /// Calls have been resolved and are being dispatched.
    Dispatching {
        /// Unsettled calls in deterministic call order.
        pending: Vec<PendingCall>,
    },
    /// The active turn is waiting for overflow compaction.
    Compacting {
        /// Compaction reason.
        reason: CompactionReason,
        /// Whether compaction is automatic.
        automatic: bool,
    },
}

/// A call awaiting a terminal dispatcher result.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingCall {
    /// Provider call identity.
    pub call: CallId,
    /// Registered tool name.
    pub name: Box<str>,
    /// Whether the dispatcher has begun executing the call.
    pub started: bool,
    /// Deferred tool promoted for later turns once this call settles successfully.
    pub promotes: Option<Name>,
}

/// An input to the pure session reducer.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Event {
    /// A session command; expected-turn checks are performed by the reducer.
    Command {
        /// The requested state transition.
        cmd: Command,
        /// The client that submitted the command.
        by: ClientId,
    },
    /// A guarding-hook result already checked against its event.
    Guard {
        /// Owning turn.
        turn: TurnId,
        /// Call identity when the event is `ToolCall`.
        call: Option<CallId>,
        /// Extension whose tool-call hook produced the outcome, when applicable.
        extension: Option<Box<str>>,
        /// The checked hook result; observer verdicts cannot be represented.
        outcome: HookOutcome,
    },
    /// The separate Rust-only stream watcher verdict.
    StreamVerdict {
        /// Owning turn.
        turn: TurnId,
        /// Stream-specific verdict; it is not a `HookVerdict`.
        verdict: StreamVerdict,
    },
    /// A request opened by the broker.
    RequestOpened {
        /// The request to track and present.
        request: Request,
    },
    /// The broker's answer to a request already tracked by the fold.
    GrantResolved {
        /// Request identity.
        request: RequestId,
        /// Answer selected by a client or request default.
        answer: Answer,
        /// Client attribution, absent when the broker selected a default.
        by: Option<ClientId>,
        /// Whether the answer was selected as the request default.
        was_default: bool,
    },
    /// A job was started by the actor.
    JobStarted {
        /// The started job.
        job: JobId,
        /// The operation owned by the job.
        kind: JobKind,
    },
    /// A job reached a terminal outcome.
    JobSettled {
        /// The settled job.
        job: JobId,
        /// The final job outcome.
        outcome: JobOutcome,
    },
    /// A background wake attempt.
    Wake {
        /// The wake's user-visible text.
        text: Box<str>,
        /// Sources that caused this wake.
        sources: Box<[Box<str>]>,
        /// Jobs whose completion caused this wake.
        jobs: Box<[JobId]>,
    },
    /// Text queued for the next request of a running turn.
    Steer {
        /// The turn that receives this text.
        turn: TurnId,
        /// The text to add at the next boundary.
        text: Box<str>,
    },
    /// A cancellation request; job validation and signalling stay in the actor.
    Cancel {
        /// The turn or job to cancel.
        scope: CancelScope,
        /// Stream content retained by the actor, if a turn stream was active.
        partial: Option<PartialResponse>,
    },
    /// One visible provider-stream delta.
    Stream {
        /// Owning turn.
        turn: TurnId,
        /// One model-stream event.
        event: StreamEvent,
    },
    /// Model selected for a request before its stream begins.
    RequestStarted {
        /// Owning turn.
        turn: TurnId,
        /// Selected provider model route.
        model: ModelRoute,
        /// Provider family used by this request.
        family: Family,
    },
    /// The completed result of one provider request.
    StreamEnded {
        /// Owning turn.
        turn: TurnId,
        /// Concrete provider model route that answered the request.
        model: ModelRoute,
        /// Provider family for the resulting assistant entry.
        family: Family,
        /// Provider result, classified independently from provider error details.
        result: Result<Inference, InferFailure>,
        /// Partial provider content supplied when an interrupted result is reported.
        partial: Option<PartialResponse>,
    },
    /// Resolved calls in response order and the answerer availability snapshot.
    Resolved {
        /// Owning turn.
        turn: TurnId,
        /// Resolved calls in provider order.
        calls: Vec<ResolvedCall>,
        /// Whether an answerer is attached for approval questions.
        answerer_attached: bool,
    },
    /// A dispatcher began executing one resolved call.
    CallStarted {
        /// Owning turn.
        turn: TurnId,
        /// The call whose execution began.
        call: CallId,
    },
    /// One dispatcher result, fed in plan order.
    Settled {
        /// Owning turn.
        turn: TurnId,
        /// The completed call.
        call: CallId,
        /// The call's terminal outcome.
        outcome: SettledOutcome,
    },
    /// Begin the next request at a turn boundary.
    Boundary {
        /// Owning turn.
        turn: TurnId,
    },
    /// Current actor-supplied context limits.
    Limits {
        /// Active model's context window in tokens.
        window: u64,
        /// Maximum model rounds in the turn; zero disables the bound.
        max_steps: u32,
        /// Automatic and manual compaction policy.
        compact: CompactionLimits,
    },
    /// Completion of either automatic or manual compaction.
    CompactionSettled {
        /// Turn id for an automatic/overflow compaction, absent for manual.
        turn: Option<TurnId>,
        /// The compactor outcome.
        outcome: Result<CompactionSummary, Box<str>>,
    },
    /// Process/actor shutdown boundary.
    Close,
}

/// Durable records and user-visible updates produced by one reducer step.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Emit {
    /// Records that must be durably appended before updates are published.
    pub records: Vec<Record>,
    /// Updates published only after the records receive a journal receipt.
    pub updates: Vec<UpdateKind>,
}

/// Ordered work requested from the owning actor.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Effect {
    /// Append records, then publish updates from the returned journal receipt.
    Emit(Emit),
    /// Publish a stream delta directly without allocating a journal/update batch.
    Delta {
        /// The turn producing the delta.
        turn: TurnId,
        /// The shared model channel.
        channel: StreamChannel,
        /// Newly streamed content, moved from the input event.
        text: Box<str>,
    },
    /// Build and submit the next provider request.
    Infer(ModelRequestPlan),
    /// Dispatch tool units in planned order.
    Dispatch {
        /// Owning turn.
        turn: TurnId,
        /// Tool units in planned order.
        units: Vec<Unit>,
    },
    /// Present a request through the broker.
    Ask(Request),
    /// Run the registered compactor chain.
    Compact {
        /// The turn being compacted, or none for manual compaction.
        turn: Option<TurnId>,
    },
    /// Run a command whose side effects belong to the session actor.
    Command {
        /// Command accepted by the pure state transition.
        cmd: Command,
        /// Client that submitted the command.
        by: ClientId,
    },
    /// Return a command result to its client.
    Reply(Result<Reply, Rejection>),
    /// Stop the turn task after its end records are durable.
    Stop {
        /// The turn task to stop.
        turn: TurnId,
        /// The durable stop reason.
        stop: Stop,
    },
}

/// A complete replay or transition protocol failure.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ReplayError {
    /// The journal contains a version unsupported by this build.
    #[error("unsupported journal version {found}")]
    UnknownVersion {
        /// Version found in the journal.
        found: u64,
    },
    /// A journal line could not be decoded.
    #[error(transparent)]
    Decode(#[from] DecodeError),
    /// The decoded records contradict the journal state machine.
    #[error("journal contradiction: {detail}")]
    Contradiction {
        /// Why the records cannot be replayed.
        detail: Box<str>,
    },
}

/// The compaction settings carried to the actor with each model context limit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limits {
    /// Model context window in tokens.
    pub window: u64,
    /// Maximum model rounds in one turn; zero disables the bound.
    pub max_steps: u32,
    /// Automatic compaction policy.
    pub compact: CompactionLimits,
}

/// Session-local setting values reconstructed from journal records.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// Explicitly selected model route.
    pub model: Option<ModelRoute>,
    /// Requested reasoning level.
    pub thinking: ThinkingLevel,
    /// Tool approval policy.
    pub approval: ApprovalMode,
    /// Display name.
    pub name: Option<Box<str>>,
    /// Product mode used in public settings updates.
    pub mode: Mode,
}

impl Settings {
    const fn initial() -> Self {
        Self {
            model: None,
            thinking: ThinkingLevel::Off,
            approval: ApprovalMode::Ask,
            name: None,
            mode: Mode::Normal,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum QueuedInput {
    Steer(Vec<Part>),
    FollowUp { turn: TurnId, source: TurnSource },
}

#[derive(Clone, Debug, PartialEq)]
struct QuestionRef {
    tool: Option<Box<str>>,
}
#[derive(Clone, Debug, Default, PartialEq)]
struct TurnFlags {
    interrupts: u32,
    overflowed: bool,
    pending_suppressed: Vec<Box<str>>,
    suppressed_notice: bool,
    end_after_boundary: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct ManualCompletion {
    job_settled: bool,
    result_settled: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct Tree {
    entries: BTreeMap<EntryId, Entry>,
    leaf: Option<EntryId>,
    labels: BTreeMap<EntryId, Box<str>>,
}

impl Tree {
    fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            leaf: None,
            labels: BTreeMap::new(),
        }
    }

    fn append(&mut self, entry: Entry) -> EntryView {
        let view = EntryView {
            id: entry.id,
            parent: entry.parent,
            kind: entry.kind.clone(),
        };
        self.leaf = Some(entry.id);
        self.entries.insert(entry.id, entry);
        view
    }

    fn ancestors(&self, from: Option<EntryId>) -> Vec<EntryId> {
        let mut branch = Vec::new();
        let mut cursor = from;
        while let Some(id) = cursor {
            let Some(entry) = self.entries.get(&id) else {
                break;
            };
            branch.push(id);
            cursor = entry.parent;
        }
        branch.reverse();
        branch
    }
}

/// One assistant tool call tracked while replaying its owning turn.
#[derive(Clone, Debug)]
struct ReplayCall {
    call: CallId,
    name: Box<str>,
    started: bool,
    settled: bool,
}

const TOOL_LOST: &str = "Tool call was not completed: dalgon stopped before it finished.";

fn contradiction(detail: impl Into<Box<str>>) -> ReplayError {
    ReplayError::Contradiction {
        detail: detail.into(),
    }
}

fn replay_tool_name(tool: &str, record: &str) -> Result<Name, ReplayError> {
    Name::parse(tool)
        .map_err(|_| contradiction(format!("{record} record contains an invalid tool name")))
}

/// Record-order state carried while folding one journal into a session.
struct Replay {
    session: Session,
    active_turn: Option<TurnId>,
    calls: Vec<ReplayCall>,
    seen_turns: Vec<TurnId>,
    started_jobs: Vec<(JobId, Option<JobKind>)>,
    max_turn: u64,
    last_completed_turn: u64,
    max_entry: u64,
    saw_record: bool,
}

impl Replay {
    fn new() -> Self {
        Self {
            session: Session::empty(),
            active_turn: None,
            calls: Vec::new(),
            seen_turns: Vec::new(),
            started_jobs: Vec::new(),
            max_turn: 0,
            last_completed_turn: 0,
            max_entry: 0,
            saw_record: false,
        }
    }

    fn record(&mut self, record: &Record) -> Result<(), ReplayError> {
        if let Some(entry) = record.entry() {
            if entry.id.get() <= self.max_entry {
                return Err(contradiction("entry ids must increase strictly"));
            }
            if entry
                .parent
                .is_some_and(|parent| !self.session.tree.entries.contains_key(&parent))
            {
                return Err(contradiction("entry parent must name an earlier entry"));
            }
        }
        if let Record::Session(header) = record {
            if self.saw_record {
                return Err(contradiction(
                    "session header is not first or is duplicated",
                ));
            }
            self.session.id = Some(header.id);
        }
        self.saw_record = true;
        self.protocol(record)?;
        if let Some(entry) = record.entry() {
            self.max_entry = self.max_entry.max(entry.id.get());
            self.session.tree.append(entry.clone());
            self.session.projected_bytes = self
                .session
                .projected_bytes
                .saturating_add(entry_weight(entry));
            self.session.replay_setting(entry);
        }
        self.session_state(record)
    }

    fn protocol(&mut self, record: &Record) -> Result<(), ReplayError> {
        match record {
            Record::Boot { r#gen, .. } => self.session.generation = Some(*r#gen),
            Record::TurnStart { turn, .. } => self.turn_start(*turn)?,
            Record::Assistant(entry) => self.assistant_calls(entry),
            Record::ToolStart { turn, call, .. } => self.tool_start(*turn, call)?,
            Record::ToolResult(Entry {
                kind: EntryKind::ToolResult { call, .. },
                ..
            }) => self.tool_result(call)?,
            Record::TurnEnd { turn, .. } => self.turn_end(*turn)?,
            Record::WakeAttempt { turn, count, .. } => self.wake_attempt(*turn, *count)?,
            Record::Job { job, event, .. } => self.job(*job, event)?,
            _ => {}
        }
        Ok(())
    }

    fn turn_start(&mut self, turn: TurnId) -> Result<(), ReplayError> {
        if self.active_turn.is_some() {
            return Err(contradiction("a turn started before the prior turn ended"));
        }
        if self.seen_turns.contains(&turn) {
            return Err(contradiction("turn id was reused"));
        }
        self.seen_turns.push(turn);
        self.active_turn = Some(turn);
        if self
            .session
            .wake_attempt_turn
            .is_some_and(|wake_turn| wake_turn.get() < turn.get())
        {
            self.session.wake_run = 0;
            self.session.wake_attempt_turn = None;
        }
        self.max_turn = self.max_turn.max(turn.get());
        self.calls.clear();
        Ok(())
    }

    fn assistant_calls(&mut self, entry: &Entry) {
        let EntryKind::Assistant { content, .. } = &entry.kind else {
            return;
        };
        for block in content {
            let Block::ToolCall { id, name, .. } = block else {
                continue;
            };
            if self
                .calls
                .iter()
                .any(|known| known.call == *id && !known.settled)
            {
                continue;
            }
            self.calls.push(ReplayCall {
                call: id.clone(),
                name: name.clone(),
                started: false,
                settled: false,
            });
        }
    }

    fn open_call(&mut self, call: &CallId) -> Option<&mut ReplayCall> {
        self.calls
            .iter_mut()
            .find(|known| known.call == *call && !known.settled)
    }

    fn tool_start(&mut self, turn: TurnId, call: &CallId) -> Result<(), ReplayError> {
        if self.active_turn != Some(turn) {
            return Err(contradiction("tool call started outside its owning turn"));
        }
        let Some(known) = self.open_call(call) else {
            return Err(contradiction(format!(
                "tool call {} started without an open assistant tool call",
                call.as_str()
            )));
        };
        if known.started {
            return Err(contradiction(format!(
                "tool call {} started more than once",
                call.as_str()
            )));
        }
        known.started = true;
        Ok(())
    }

    fn tool_result(&mut self, call: &CallId) -> Result<(), ReplayError> {
        if let Some(known) = self.open_call(call) {
            known.settled = true;
            return Ok(());
        }
        let detail = if self.calls.iter().any(|known| known.call == *call) {
            format!("repeated tool result for call {}", call.as_str())
        } else {
            format!("tool result for unknown call {}", call.as_str())
        };
        Err(contradiction(detail))
    }

    fn turn_end(&mut self, turn: TurnId) -> Result<(), ReplayError> {
        if self.active_turn != Some(turn) {
            return Err(contradiction("turn ended without a matching start"));
        }
        if let Some(open) = self.calls.iter().find(|known| !known.settled) {
            return Err(contradiction(format!(
                "turn ended with tool call {} missing its result",
                open.call.as_str()
            )));
        }
        self.active_turn = None;
        self.calls.clear();
        self.last_completed_turn = self.last_completed_turn.max(turn.get());
        Ok(())
    }

    fn wake_attempt(&mut self, turn: TurnId, count: u32) -> Result<(), ReplayError> {
        if count == 0 || count > MAX_WAKE_RUN {
            return Err(contradiction("wake count is outside the configured limit"));
        }
        self.session.wake_run = count;
        self.session.wake_attempt_turn = Some(turn);
        self.max_turn = self.max_turn.max(turn.get());
        Ok(())
    }

    fn job(&mut self, job: JobId, event: &JobEvent) -> Result<(), ReplayError> {
        match event {
            JobEvent::Started { kind } => {
                let kind = kind
                    .as_deref()
                    .map(|value| {
                        parse_job_kind(Some(value))
                            .ok_or_else(|| contradiction("job start has an unknown kind"))
                    })
                    .transpose()?;
                if self.started_jobs.iter().any(|(started, _)| *started == job) {
                    return Err(contradiction("job started more than once"));
                }
                self.started_jobs.push((job, kind));
            }
            JobEvent::Settled { .. }
            | JobEvent::Cancelled { .. }
            | JobEvent::Killed
            | JobEvent::TimedOut
            | JobEvent::Orphaned => {
                if !self.started_jobs.iter().any(|(started, _)| *started == job) {
                    return Err(contradiction("job ended without a start"));
                }
                self.started_jobs.retain(|(started, _)| *started != job);
            }
        }
        Ok(())
    }

    fn session_state(&mut self, record: &Record) -> Result<(), ReplayError> {
        let session = &mut self.session;
        match record {
            Record::Leaf { to: Some(id), .. } if !session.tree.entries.contains_key(id) => {
                return Err(contradiction("leaf points to an unknown entry"));
            }
            Record::Leaf { to, .. } => session.tree.leaf = *to,
            Record::Label {
                entry,
                label: Some(label),
                ..
            } => {
                session.tree.labels.insert(*entry, label.clone());
            }
            Record::Label {
                entry, label: None, ..
            } => {
                session.tree.labels.remove(entry);
            }
            Record::Name { name, .. } => session.settings.name.clone_from(name),
            Record::AllowAlways { tool, .. } => {
                session
                    .allow_always
                    .insert(replay_tool_name(tool, "allow-always")?);
            }
            Record::ToolPromoted { tool, .. } => {
                session
                    .promoted
                    .insert(replay_tool_name(tool, "tool-promoted")?);
            }
            _ => {}
        }
        Ok(())
    }

    fn finish(mut self, now: jiff::Timestamp) -> Result<(Session, Vec<Effect>), ReplayError> {
        self.calls.retain(|known| !known.settled);
        self.reserve_counters()?;
        let mut records = Vec::new();
        self.repair_open_turn(now, &mut records)?;
        records.extend(self.started_jobs.iter().map(|(job, _)| Record::Job {
            at: now,
            job: *job,
            event: JobEvent::Orphaned,
        }));
        let mut session = self.session;
        session.restore_branch_state()?;
        let generation = session.next_generation()?;
        records.push(Record::Boot {
            at: now,
            r#gen: generation,
            version: PRODUCT_VERSION.into(),
        });
        session.generation = Some(generation);
        session.phase = Phase::Idle;
        let emit = Emit {
            records,
            updates: Vec::new(),
        };
        Ok((session, vec![Effect::Emit(emit)]))
    }

    fn reserve_counters(&mut self) -> Result<(), ReplayError> {
        let repair_entries = u64::try_from(self.calls.len())
            .map_err(|_| contradiction("too many dangling calls to repair"))?;
        let next_entry = self
            .max_entry
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .map(EntryId::new)
            .ok_or_else(|| contradiction("entry id space exhausted"))?;
        if self.max_entry.checked_add(repair_entries).is_none() {
            return Err(contradiction(
                "entry id space exhausted during crash repair",
            ));
        }
        let next_turn = self
            .max_turn
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .map(TurnId::new)
            .ok_or_else(|| contradiction("turn id space exhausted"))?;
        self.session.next_entry = Some(next_entry);
        self.session.next_turn = Some(next_turn);
        self.session.last_turn = self.last_completed_turn;
        Ok(())
    }

    fn repair_open_turn(
        &mut self,
        now: jiff::Timestamp,
        records: &mut Vec<Record>,
    ) -> Result<(), ReplayError> {
        let Some(turn) = self.active_turn else {
            return Ok(());
        };
        for ReplayCall { call, name, .. } in std::mem::take(&mut self.calls) {
            let entry = self
                .session
                .entry(
                    now,
                    EntryKind::ToolResult {
                        call,
                        name,
                        error: true,
                        parts: vec![JournalPart::Text {
                            text: TOOL_LOST.into(),
                        }],
                        changes: Vec::new(),
                    },
                )
                .map_err(|_| contradiction("entry id space exhausted during crash repair"))?;
            self.session.tree.append(entry.clone());
            records.push(Record::ToolResult(entry));
        }
        records.push(Record::TurnEnd {
            at: now,
            turn,
            stop: TurnEndStop::Aborted,
            usage: None,
            changes: Vec::new(),
        });
        self.session.last_turn = turn.get();
        Ok(())
    }
}

/// The complete, replayable state of one session.
#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    phase: Phase,
    id: Option<crate::id::SessionId>,
    generation: Option<Gen>,
    /// The next turn id, or `None` once the turn id space is exhausted.
    next_turn: Option<TurnId>,
    /// The next entry id, or `None` once the entry id space is exhausted.
    next_entry: Option<EntryId>,
    last_turn: u64,
    tree: Tree,
    settings: Settings,
    request_params: RequestParams,
    allow_always: BTreeSet<Name>,
    promoted: BTreeSet<Name>,
    queued_inputs: Vec<QueuedInput>,
    open_questions: Vec<(RequestId, QuestionRef)>,
    live_jobs: Vec<(JobId, Option<JobKind>)>,
    ended_jobs: Vec<(JobId, JobOutcome)>,
    wake_run: u32,
    wake_attempt_turn: Option<TurnId>,
    limits: Option<Limits>,
    last_usage: Option<(ModelRoute, u64, u32)>,
    active_model: Option<ModelRoute>,
    active_family: Option<Family>,
    projected_bytes: u64,
    compactions: u32,
    turn_flags: TurnFlags,
    auto_failures: u32,
    breaker_open: bool,
    pending_compaction: Option<CompactionReason>,
    pending_manual_focus: Option<Box<str>>,
    argument_overrides: Vec<(CallId, RawJson)>,
    compaction_none_notified: bool,
    manual_completion: ManualCompletion,
}

impl Session {
    /// Reconstructs one deterministic session state and its crash-repair records.
    ///
    /// Empty input creates an ephemeral, unbooted session and returns a boot
    /// record for generation one. Decoding bytes is offered separately by
    /// [`Session::replay_lines`].
    ///
    /// # Errors
    /// Returns an unsupported-version, decode, or journal-contradiction error.
    pub fn replay(
        records: impl IntoIterator<Item = Record>,
        now: jiff::Timestamp,
    ) -> Result<(Self, Vec<Effect>), ReplayError> {
        let mut replay = Replay::new();
        for record in records {
            replay.record(&record)?;
        }
        replay.finish(now)
    }

    /// Restores an already booted and recovered journal without emitting effects.
    ///
    /// The store must finish crash recovery before calling this method. Unlike
    /// [`Self::replay`], restoration preserves the current boot generation.
    ///
    /// # Errors
    /// Returns a contradiction for a missing header or boot, unfinished work,
    /// invalid record order, or exhausted identifiers.
    pub fn restore(records: impl IntoIterator<Item = Record>) -> Result<Self, ReplayError> {
        let mut replay = Replay::new();
        for record in records {
            replay.record(&record)?;
        }
        if replay.session.id.is_none() || replay.session.generation.is_none() {
            return Err(contradiction(
                "restoration needs a session header and boot record",
            ));
        }
        replay.calls.retain(|call| !call.settled);
        if replay.active_turn.is_some()
            || !replay.calls.is_empty()
            || !replay.started_jobs.is_empty()
        {
            return Err(contradiction(
                "session requires recovery before restoration",
            ));
        }
        replay.reserve_counters()?;
        replay.session.restore_branch_state()?;
        replay.session.phase = Phase::Idle;
        Ok(replay.session)
    }

    /// Borrows the settings reconstructed from the current journal branch.
    #[must_use]
    pub const fn settings(&self) -> &Settings {
        &self.settings
    }

    fn restore_branch_state(&mut self) -> Result<(), ReplayError> {
        let branch = self.tree.ancestors(self.tree.leaf);
        self.restore_branch_settings(&branch);
        let is_compaction = |id: &EntryId| {
            self.tree
                .entries
                .get(id)
                .is_some_and(|entry| matches!(&entry.kind, EntryKind::Compaction { .. }))
        };
        let compactions = u32::try_from(branch.iter().filter(|&id| is_compaction(id)).count())
            .map_err(|_| contradiction("compaction counter space exhausted"))?;
        let start = branch
            .iter()
            .rposition(&is_compaction)
            .map_or(0, |index| index + 1);
        self.projected_bytes = branch
            .iter()
            .filter_map(|id| self.tree.entries.get(id))
            .map(entry_weight)
            .fold(0_u64, u64::saturating_add);
        self.last_usage = branch.iter().skip(start).rev().find_map(|id| {
            let EntryKind::Assistant {
                api, model, usage, ..
            } = &self.tree.entries.get(id)?.kind
            else {
                return None;
            };
            Some((
                ModelRoute::Api {
                    family: *api,
                    model: model.clone(),
                },
                usage.input_tokens,
                compactions,
            ))
        });
        self.compactions = compactions;
        Ok(())
    }

    fn restore_branch_settings(&mut self, branch: &[EntryId]) {
        let mut settings = Settings::initial();
        settings.name.clone_from(&self.settings.name);
        settings.mode = self.settings.mode;
        for entry in branch.iter().filter_map(|id| self.tree.entries.get(id)) {
            match &entry.kind {
                EntryKind::Model { route } => settings.model = Some(route.clone()),
                EntryKind::Thinking { level } => settings.thinking = *level,
                EntryKind::Approval { mode } => settings.approval = *mode,
                _ => {}
            }
        }
        self.request_params.thinking = settings.thinking;
        self.settings = settings;
    }

    fn next_generation(&self) -> Result<Gen, ReplayError> {
        self.generation
            .map_or(Some(1), |generation| generation.get().checked_add(1))
            .and_then(NonZeroU64::new)
            .map(Gen::new)
            .ok_or_else(|| contradiction("generation id space exhausted"))
    }

    /// Decodes lines and replays them, preserving unsupported-version errors.
    ///
    /// # Errors
    /// Returns a decode, unsupported-version, or contradiction error.
    pub fn replay_lines<'a>(
        lines: impl IntoIterator<Item = &'a [u8]>,
        now: jiff::Timestamp,
    ) -> Result<(Self, Vec<Effect>), ReplayError> {
        let mut records = Vec::new();
        for line in lines {
            let decoded = crate::journal::decode(line).map_err(|error| match error {
                DecodeError::UnsupportedVersion { found } => ReplayError::UnknownVersion { found },
                other => ReplayError::Decode(other),
            })?;
            records.push(decoded.record);
        }
        Self::replay(records, now)
    }

    /// Applies one event atomically; rejected events leave session state untouched.
    ///
    /// The caller owns and reuses `out`; no session clone is made per event.
    ///
    /// # Errors
    /// Returns a typed command rejection without applying state or effects.
    pub fn step(
        &mut self,
        event: Event,
        now: jiff::Timestamp,
        out: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        self.preflight(&event)?;
        out.clear();
        let mut emit = Emit::default();
        self.apply(event, now, &mut emit, out)?;
        if !emit.records.is_empty() || !emit.updates.is_empty() {
            out.insert(0, Effect::Emit(emit));
        }
        Ok(())
    }

    /// Borrows the current lifecycle phase.
    #[must_use]
    pub const fn phase(&self) -> &Phase {
        &self.phase
    }

    /// Returns the current tool-approval mode.
    #[must_use]
    pub const fn approval_mode(&self) -> ApprovalMode {
        self.settings.approval
    }

    /// Borrows the session-wide always-allowed tool set.
    #[must_use]
    pub const fn allow_always(&self) -> &BTreeSet<Name> {
        &self.allow_always
    }

    /// Borrows the deferred tools promoted to the model-visible tool list.
    #[must_use]
    pub const fn promoted(&self) -> &BTreeSet<Name> {
        &self.promoted
    }

    /// Returns the number of queued steer cells.
    #[must_use]
    pub fn steers_queued(&self) -> usize {
        self.queued_inputs
            .iter()
            .filter(|item| matches!(item, QueuedInput::Steer(_)))
            .count()
    }

    /// Returns the number of queued follow-up turns.
    #[must_use]
    pub fn follow_ups_queued(&self) -> usize {
        self.queued_inputs
            .iter()
            .filter(|item| matches!(item, QueuedInput::FollowUp { .. }))
            .count()
    }

    /// Whether automatic compaction is enabled and the breaker is closed.
    #[must_use]
    pub fn auto_compaction_on(&self) -> bool {
        self.limits
            .is_some_and(|limits| limits.compact.enabled && limits.compact.compactor_available)
            && !self.breaker_open
    }

    /// Returns the automatic context-compaction trigger when it is viable.
    #[must_use]
    pub fn should_compact(&self) -> Option<CompactionReason> {
        let limits = self.limits?;
        (limits.compact.compactor_available && self.threshold_compaction_due())
            .then_some(CompactionReason::Threshold)
    }

    fn threshold_compaction_due(&self) -> bool {
        let Some(limits) = self.limits else {
            return false;
        };
        if !limits.compact.enabled || self.breaker_open || limits.window == 0 {
            return false;
        }
        let current_model = self.active_model.as_ref().or(self.settings.model.as_ref());
        let Some((model, tokens, compactions)) = &self.last_usage else {
            return false;
        };
        if current_model.is_some_and(|current| current != model) || *compactions != self.compactions
        {
            return false;
        }
        if *tokens < limits.compact.min_tokens {
            return false;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "usage and window are u64 counters; the configured trigger fraction is f64"
        )]
        let window = limits.window as f64;
        #[expect(
            clippy::cast_precision_loss,
            reason = "usage and window are u64 counters; the configured trigger fraction is f64"
        )]
        let tokens = *tokens as f64;
        tokens >= window * limits.compact.threshold
    }

    /// Selects the nearest user boundary that retains at least `keep_tokens`.
    #[must_use]
    pub fn cut_point(&self, keep_tokens: u64) -> Option<EntryId> {
        let branch = self.tree.ancestors(self.tree.leaf);
        let mut candidate = branch.len().checked_sub(1)?;
        let mut retained = 0_u64;
        loop {
            let entry = self.tree.entries.get(&branch[candidate])?;
            retained = retained.saturating_add(entry_token_weight(entry));
            if retained >= keep_tokens || candidate == 0 {
                break;
            }
            candidate -= 1;
        }
        let boundary = (0..=candidate)
            .rev()
            .find(|index| {
                self.tree
                    .entries
                    .get(&branch[*index])
                    .is_some_and(|entry| matches!(&entry.kind, EntryKind::User { .. }))
            })
            .unwrap_or(candidate);
        Some(branch[boundary])
    }

    /// Whether the current leaf itself is a compaction boundary.
    #[must_use]
    pub fn compacted_at_leaf(&self) -> bool {
        self.tree
            .leaf
            .and_then(|id| self.tree.entries.get(&id))
            .is_some_and(|entry| matches!(&entry.kind, EntryKind::Compaction { .. }))
    }

    /// Returns same-model tokens since compaction, or an estimate from the active branch.
    #[must_use]
    pub fn tokens_since_last_compaction(&self) -> u64 {
        self.last_usage
            .as_ref()
            .filter(|(_, _, compactions)| *compactions == self.compactions)
            .map_or_else(
                || self.projected_bytes.div_ceil(4),
                |(_, tokens, _)| *tokens,
            )
    }
    fn measured_context_tokens(&self) -> Option<u64> {
        let current_model = self.active_model.as_ref().or(self.settings.model.as_ref());
        let (model, tokens, compactions) = self.last_usage.as_ref()?;
        if *compactions != self.compactions || current_model.is_some_and(|current| current != model)
        {
            return None;
        }
        Some(*tokens)
    }

    /// Returns a stable context-cache key for the session and compaction count.
    #[must_use]
    pub fn cache_key(&self) -> Box<str> {
        format!(
            "{}:{}",
            self.id
                .map_or_else(|| "ephemeral".into(), |id| id.to_string()),
            self.compactions
        )
        .into()
    }

    /// Builds the policy snapshot used for one dispatcher round.
    #[must_use]
    pub fn policy(&self, answerer_attached: bool) -> Policy {
        Policy {
            mode: self.settings.approval,
            answerer_attached,
            allow_always: self.allow_always.clone(),
        }
    }

    /// Returns current actor-supplied model/context limits.
    #[must_use]
    pub const fn limits(&self) -> Option<Limits> {
        self.limits
    }

    /// Returns the current compaction count.
    #[must_use]
    pub const fn compactions(&self) -> u32 {
        self.compactions
    }

    /// Returns the durable wake-run counter.
    #[must_use]
    pub const fn wake_run(&self) -> u32 {
        self.wake_run
    }
    /// Returns a rewritten tool argument value queued by a guarding hook.
    #[must_use]
    pub fn argument_override(&self, call: &CallId) -> Option<&RawJson> {
        self.argument_overrides
            .iter()
            .find(|(id, _)| id == call)
            .map(|(_, args)| args)
    }

    /// Returns the focus text of an active manual compaction.
    #[must_use]
    pub fn manual_compaction_focus(&self) -> Option<&str> {
        self.pending_manual_focus.as_deref()
    }
    fn pop_steer(&mut self) -> Option<Vec<Part>> {
        let index = self
            .queued_inputs
            .iter()
            .position(|item| matches!(item, QueuedInput::Steer(_)))?;
        let QueuedInput::Steer(parts) = self.queued_inputs.remove(index) else {
            return None;
        };
        Some(parts)
    }

    fn pop_follow_up(&mut self) -> Option<(TurnId, TurnSource)> {
        let index = self
            .queued_inputs
            .iter()
            .position(|item| matches!(item, QueuedInput::FollowUp { .. }))?;
        let QueuedInput::FollowUp { turn, source } = self.queued_inputs.remove(index) else {
            return None;
        };
        Some((turn, source))
    }

    fn empty() -> Self {
        Self {
            phase: Phase::Idle,
            id: None,
            generation: None,
            next_turn: Some(TurnId::new(NonZeroU64::MIN)),
            next_entry: Some(EntryId::new(NonZeroU64::MIN)),
            last_turn: 0,
            tree: Tree::new(),
            settings: Settings::initial(),
            request_params: RequestParams {
                thinking: ThinkingLevel::Off,
                effort: None,
                temperature: None,
            },
            allow_always: BTreeSet::new(),
            promoted: BTreeSet::new(),
            queued_inputs: Vec::new(),
            open_questions: Vec::new(),
            live_jobs: Vec::new(),
            ended_jobs: Vec::new(),
            wake_run: 0,
            wake_attempt_turn: None,
            limits: None,
            last_usage: None,
            active_model: None,
            active_family: None,
            projected_bytes: 0,
            compactions: 0,
            turn_flags: TurnFlags::default(),
            auto_failures: 0,
            breaker_open: false,
            pending_compaction: None,
            pending_manual_focus: None,
            argument_overrides: Vec::new(),
            compaction_none_notified: false,
            manual_completion: ManualCompletion::default(),
        }
    }
    fn preflight(&self, event: &Event) -> Result<(), Rejection> {
        if matches!(&self.phase, Phase::Closed) && !matches!(event, Event::Close) {
            return Err(Rejection::SessionClosed);
        }
        match event {
            Event::Command { cmd, .. } => self.preflight_command(cmd),
            Event::Steer { turn, .. } => self.preflight_steer(*turn),
            Event::Wake { .. } => self.preflight_wake(),
            Event::GrantResolved {
                request,
                answer: Answer::ApproveForSession,
                by: Some(_),
                was_default: false,
            } => self.preflight_session_grant(*request),
            Event::Guard {
                turn,
                call,
                extension,
                outcome,
            } => self.preflight_guard(*turn, call.is_some(), extension.is_some(), outcome),
            Event::Cancel {
                scope: CancelScope::Turn(turn),
                ..
            } => self.preflight_cancel(*turn),
            Event::Settled { turn, call, .. } => self.preflight_settled(*turn, call),
            Event::CompactionSettled {
                turn,
                outcome: Ok(summary),
            } if summary.tokens_after < summary.tokens_before => {
                self.preflight_compaction_entry(*turn)
            }
            _ => Ok(()),
        }
    }

    fn entry_available(&self) -> Result<(), Rejection> {
        self.next_entry
            .map(|_| ())
            .ok_or_else(|| invalid("entry id space exhausted"))
    }

    fn preflight_wake(&self) -> Result<(), Rejection> {
        if self.wake_run >= MAX_WAKE_RUN {
            return Err(Rejection::Denied {
                reason: crate::approval::DenyReason::WakeLimit,
            });
        }
        if matches!(&self.phase, Phase::Compacting { .. }) {
            return Err(Rejection::BusyTurn);
        }
        if self.next_turn.is_none()
            || matches!(&self.phase, Phase::Idle) && self.next_entry.is_none()
        {
            return Err(invalid("turn or entry id space exhausted"));
        }
        Ok(())
    }

    fn preflight_session_grant(&self, request: RequestId) -> Result<(), Rejection> {
        let Some(tool) = self
            .open_questions
            .iter()
            .find(|(id, _)| *id == request)
            .and_then(|(_, question)| question.tool.as_deref())
        else {
            return Ok(());
        };
        Name::parse(tool)
            .map(|_| ())
            .map_err(|_| invalid("approval tool name is invalid"))
    }

    fn preflight_guard(
        &self,
        turn: TurnId,
        has_call: bool,
        has_extension: bool,
        outcome: &HookOutcome,
    ) -> Result<(), Rejection> {
        match outcome.event() {
            HookEvent::BeforeTurn => match &self.phase {
                Phase::Settling {
                    follow_up: Some((next_turn, _)),
                    ..
                } if *next_turn == turn => self.entry_available(),
                _ => Ok(()),
            },
            HookEvent::ToolCall if matches!(&self.phase, Phase::Running { turn: active, stage: TurnStage::Dispatching { .. }, .. } if *active == turn) => {
                match outcome.verdict() {
                    HookVerdict::ToolCall(ToolCallVerdict::Block { .. })
                        if !has_call || !has_extension =>
                    {
                        Err(invalid(
                            "tool-call block requires call and extension identities",
                        ))
                    }
                    HookVerdict::ToolCall(ToolCallVerdict::Rewrite { .. }) if !has_call => {
                        Err(invalid("tool-call rewrite requires a call identity"))
                    }
                    _ => Ok(()),
                }
            }
            _ => Ok(()),
        }
    }

    fn preflight_cancel(&self, turn: TurnId) -> Result<(), Rejection> {
        match &self.phase {
            Phase::Opening { turn: active, .. } | Phase::Running { turn: active, .. }
                if *active == turn =>
            {
                Ok(())
            }
            Phase::Settling { turn: active, .. } if *active == turn => Err(wrong_turn(
                Expect::After(turn),
                TurnState::Settling { turn: *active },
            )),
            Phase::Running { turn: active, .. } => Err(wrong_turn(
                Expect::After(turn),
                TurnState::Running { turn: *active },
            )),
            _ => Err(wrong_turn(Expect::After(turn), TurnState::Idle)),
        }
    }

    fn preflight_settled(&self, turn: TurnId, call: &CallId) -> Result<(), Rejection> {
        match &self.phase {
            Phase::Running {
                turn: active,
                stage: TurnStage::Dispatching { pending },
                ..
            } if *active == turn
                && pending
                    .iter()
                    .any(|item| item.call == *call && item.started) =>
            {
                self.entry_available()
            }
            Phase::Running { turn: active, .. } if *active == turn => {
                Err(invalid("tool result was not pending"))
            }
            _ => Ok(()),
        }
    }

    fn preflight_compaction_entry(&self, turn: Option<TurnId>) -> Result<(), Rejection> {
        let phase_matches = match (turn, &self.phase) {
            (None, Phase::Compacting { .. }) => true,
            (
                Some(turn),
                Phase::Running {
                    turn: active,
                    stage: TurnStage::Compacting { .. },
                    ..
                },
            ) => turn == *active,
            _ => false,
        };
        if phase_matches && (self.next_entry.is_none() || self.compactions == u32::MAX) {
            Err(invalid("entry or compaction counter space exhausted"))
        } else {
            Ok(())
        }
    }
    fn preflight_command(&self, cmd: &Command) -> Result<(), Rejection> {
        if let Command::Prompt { expect, content } = cmd {
            if matches!(&self.phase, Phase::Compacting { .. }) {
                return Err(Rejection::Compacting);
            }
            if matches!(&self.phase, Phase::Idle) {
                if content.is_empty() {
                    return Err(invalid("prompt content is empty."));
                }
                let expected = match expect {
                    Expect::Idle => true,
                    Expect::After(turn) => self.last_turn == turn.get(),
                };
                if !expected {
                    return Err(wrong_turn(*expect, TurnState::Idle));
                }
                if self.next_turn.is_none() || self.next_entry.is_none() {
                    return Err(invalid("turn or entry id space exhausted"));
                }
                return Ok(());
            }
            return Err(wrong_turn(*expect, self.turn_state()));
        }
        match cmd {
            Command::Steer { turn, .. } => self.preflight_steer(*turn),
            Command::FollowUp { turn, .. } => match &self.phase {
                Phase::Running { turn: active, .. }
                    if active == turn && self.next_turn.is_some() =>
                {
                    Ok(())
                }
                Phase::Running { turn: active, .. } if active == turn => {
                    Err(invalid("turn id space exhausted"))
                }
                Phase::Running { turn: active, .. } => Err(wrong_turn(
                    Expect::After(*turn),
                    TurnState::Running { turn: *active },
                )),
                Phase::Settling { turn: active, .. } => Err(wrong_turn(
                    Expect::After(*turn),
                    TurnState::Settling { turn: *active },
                )),
                _ => Err(wrong_turn(Expect::After(*turn), TurnState::Idle)),
            },
            Command::Rename(_)
            | Command::Compact { .. }
            | Command::MoveLeaf(_)
            | Command::Fork(_)
            | Command::Clone
                if !matches!(&self.phase, Phase::Idle) =>
            {
                Err(Rejection::BusyTurn)
            }
            Command::MoveLeaf(id) if !self.tree.entries.contains_key(id) => {
                Err(invalid("unknown entry id"))
            }
            _ => Ok(()),
        }
    }

    fn preflight_steer(&self, turn: TurnId) -> Result<(), Rejection> {
        match &self.phase {
            Phase::Running { turn: active, .. } if *active == turn => {
                if self.steers_queued() == MAX_STEERS {
                    Err(Rejection::SteerFull)
                } else {
                    Ok(())
                }
            }
            Phase::Running { turn: active, .. } => Err(wrong_turn(
                Expect::After(turn),
                TurnState::Running { turn: *active },
            )),
            Phase::Settling { turn: active, .. } => Err(wrong_turn(
                Expect::After(turn),
                TurnState::Settling { turn: *active },
            )),
            _ => Err(wrong_turn(Expect::After(turn), TurnState::Idle)),
        }
    }

    fn turn_state(&self) -> TurnState {
        match &self.phase {
            Phase::Idle
            | Phase::Opening { .. }
            | Phase::Closed
            | Phase::Compacting { job: None } => TurnState::Idle,
            Phase::Running { turn, .. } => TurnState::Running { turn: *turn },
            Phase::Settling { turn, .. } => TurnState::Settling { turn: *turn },
            Phase::Compacting { job: Some(job) } => TurnState::Compacting { job: *job },
        }
    }

    fn apply(
        &mut self,
        event: Event,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        match event {
            Event::Command { cmd, by } => return self.command(cmd, by, now, emit, effects),
            Event::Guard {
                turn,
                call,
                extension,
                outcome,
            } => {
                let target = HookTarget { call, extension };
                return self.guard(turn, target, outcome, now, emit, effects);
            }
            Event::StreamVerdict { turn, verdict } => {
                return self.stream_verdict(turn, verdict, now, emit, effects);
            }
            Event::RequestOpened { request } => self.request_opened(&request),
            Event::GrantResolved {
                request,
                answer,
                by,
                was_default,
            } => return self.grant_resolved(request, &answer, by.is_some(), was_default),
            Event::JobStarted { job, kind } => self.job_started(job, kind),
            Event::JobSettled { job, outcome } => self.job_settled(job, outcome),
            Event::Wake {
                text,
                sources,
                jobs,
            } => return self.wake(text, sources, jobs, now, emit, effects),
            Event::Steer { turn: _, text } => self.queue_steer(text, effects),
            Event::Cancel { scope, partial } => {
                return self.cancel(scope, partial, now, emit, effects);
            }
            Event::Stream { turn, event } => self.stream(turn, event, emit, effects),
            Event::RequestStarted {
                turn,
                model,
                family,
            } => self.request_started(turn, model, family),
            Event::StreamEnded {
                turn,
                model,
                family,
                result,
                partial,
            } => {
                let end = StreamEnd {
                    model,
                    family,
                    result,
                    partial,
                };
                return self.stream_ended(turn, end, now, emit, effects);
            }
            Event::Resolved {
                turn,
                calls,
                answerer_attached,
            } => return self.resolved(turn, &calls, answerer_attached, now, emit, effects),
            Event::CallStarted { turn, call } => self.call_started(turn, &call, now, emit),
            Event::Settled {
                turn,
                call,
                outcome,
            } => return self.settled(turn, &call, outcome, now, emit, effects),
            Event::Boundary { turn } => return self.boundary(turn, now, true, emit, effects),
            Event::Limits {
                window,
                max_steps,
                compact,
            } => self.set_limits(window, max_steps, compact),
            Event::CompactionSettled { turn, outcome } => {
                return self.compaction_settled(turn, outcome, now, emit, effects);
            }
            Event::Close => self.close(),
        }
        Ok(())
    }

    fn request_opened(&mut self, request: &Request) {
        if let Question::Approval { tool, .. } = &request.question {
            let question = QuestionRef {
                tool: Some(tool.clone()),
            };
            self.open_questions.push((request.id, question));
        }
    }

    fn queue_steer(&mut self, text: Box<str>, effects: &mut Vec<Effect>) {
        self.queued_inputs
            .push(QueuedInput::Steer(vec![Part::Text { text }]));
        effects.push(Effect::Reply(Ok(Reply::Queued)));
    }

    fn request_started(&mut self, turn: TurnId, model: ModelRoute, family: Family) {
        if matches!(&self.phase, Phase::Running { turn: active, .. } if *active == turn) {
            self.active_model = Some(model);
            self.active_family = Some(family);
        }
    }

    fn set_limits(&mut self, window: u64, max_steps: u32, compact: CompactionLimits) {
        self.limits = Some(Limits {
            window,
            max_steps,
            compact,
        });
    }

    fn close(&mut self) {
        self.phase = Phase::Closed;
        self.queued_inputs.clear();
    }

    fn grant_resolved(
        &mut self,
        request: RequestId,
        answer: &Answer,
        attributed: bool,
        was_default: bool,
    ) -> Result<(), Rejection> {
        let Some(index) = self
            .open_questions
            .iter()
            .position(|(id, _)| *id == request)
        else {
            return Ok(());
        };
        let grant = if *answer == Answer::ApproveForSession && !was_default && attributed {
            self.open_questions[index]
                .1
                .tool
                .as_deref()
                .map(|tool| Name::parse(tool).map_err(|_| invalid("approval tool name is invalid")))
                .transpose()?
        } else {
            None
        };
        self.open_questions.remove(index);
        if let Some(name) = grant {
            self.allow_always.insert(name);
        }
        Ok(())
    }

    fn job_started(&mut self, job: JobId, kind: JobKind) {
        if self.live_jobs.iter().any(|(id, _)| *id == job) {
            return;
        }
        self.live_jobs.push((job, Some(kind)));
        if kind == JobKind::Compaction && matches!(&self.phase, Phase::Compacting { job: None }) {
            self.phase = Phase::Compacting { job: Some(job) };
        }
    }

    fn job_settled(&mut self, job: JobId, outcome: JobOutcome) {
        let Some(index) = self.live_jobs.iter().position(|(id, _)| *id == job) else {
            return;
        };
        let (_, kind) = self.live_jobs.remove(index);
        if kind != Some(JobKind::Compaction) {
            self.ended_jobs.push((job, outcome));
            return;
        }
        if matches!(&self.phase, Phase::Compacting { job: Some(active) } if *active == job) {
            self.manual_completion.job_settled = true;
            self.finish_manual_compaction_if_ready();
        }
    }

    fn command(
        &mut self,
        cmd: Command,
        by: ClientId,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        match cmd {
            Command::Prompt { content, .. } => {
                let turn = self.allocate_turn()?;
                let entry = self.allocate_entry_id()?;
                self.wake_run = 0;
                self.wake_attempt_turn = None;
                self.phase = Phase::Opening {
                    turn,
                    source: TurnSource::Prompt { by, content },
                    entry,
                };
                effects.push(Effect::Reply(Ok(Reply::Accepted {
                    turn,
                    message_id: entry,
                })));
            }
            Command::Steer { turn: _, content } => {
                self.queued_inputs.push(QueuedInput::Steer(content));
                effects.push(Effect::Reply(Ok(Reply::Queued)));
            }
            Command::FollowUp { turn: _, content } => {
                let turn = self.allocate_turn()?;
                self.queued_inputs.push(QueuedInput::FollowUp {
                    turn,
                    source: TurnSource::FollowUp { by, content },
                });
                effects.push(Effect::Reply(Ok(Reply::Queued)));
            }
            Command::Cancel {
                scope: scope @ CancelScope::Job(_),
            } => {
                effects.push(Effect::Command {
                    cmd: Command::Cancel { scope },
                    by,
                });
            }
            Command::Cancel { scope } => return self.cancel(scope, None, now, emit, effects),
            Command::SetModel(route) => {
                let kind = EntryKind::Model { route };
                return self.change_setting(kind, Record::Model, now, emit, effects);
            }
            Command::SetThinking(level) => {
                let kind = EntryKind::Thinking { level };
                return self.change_setting(kind, Record::Thinking, now, emit, effects);
            }
            Command::SetApproval(mode) => {
                let kind = EntryKind::Approval { mode };
                return self.change_setting(kind, Record::Approval, now, emit, effects);
            }
            Command::Rename(name) => {
                self.settings.name = Some(name.clone());
                emit.records.push(Record::Name {
                    at: now,
                    name: Some(name),
                });
                emit.updates
                    .push(UpdateKind::Settings(self.settings_view()));
                effects.push(Effect::Reply(Ok(Reply::Done)));
            }
            Command::MoveLeaf(id) => {
                self.tree.leaf = Some(id);
                let branch = self.tree.ancestors(Some(id));
                self.restore_branch_settings(&branch);
                self.projected_bytes = self.projected_bytes_on_branch();
                emit.records.push(Record::Leaf {
                    at: now,
                    to: Some(id),
                });
                emit.updates.push(UpdateKind::Tree(TreeDelta {
                    added: Vec::new(),
                    leaf: Some(id),
                }));
                emit.updates
                    .push(UpdateKind::Settings(self.settings_view()));
                effects.push(Effect::Reply(Ok(Reply::Done)));
            }
            Command::Compact { focus } => self.manual_compact(focus, emit, effects),
            command @ (Command::Fork(_) | Command::Clone | Command::Run { .. }) => {
                effects.push(Effect::Command { cmd: command, by });
            }
        }
        Ok(())
    }

    fn change_setting(
        &mut self,
        kind: EntryKind,
        record: fn(Entry) -> Record,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        let entry = self.entry(now, kind)?;
        self.replay_setting(&entry);
        self.tree.append(entry.clone());
        emit.records.push(record(entry));
        emit.updates
            .push(UpdateKind::Settings(self.settings_view()));
        effects.push(Effect::Reply(Ok(Reply::Done)));
        Ok(())
    }

    fn manual_compact(
        &mut self,
        focus: Option<Box<str>>,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) {
        let tokens = self.tokens_since_last_compaction();
        let limits = self.limits;
        let refusal = if self.compacted_at_leaf() {
            Some("compact.already")
        } else if !limits.is_some_and(|limits| limits.compact.compactor_available) {
            Some("compact.none")
        } else if tokens < limits.map_or(1, |limits| limits.compact.min_tokens) {
            Some("compact.nothing")
        } else {
            None
        };
        if let Some(key) = refusal {
            emit.updates.push(compact_notice(None, key));
            effects.push(Effect::Reply(Ok(Reply::Done)));
            return;
        }
        self.pending_manual_focus = focus;
        self.pending_compaction = Some(CompactionReason::Manual);
        self.manual_completion = ManualCompletion::default();
        self.phase = Phase::Compacting { job: None };
        let window = limits.map_or(0, |limits| limits.window);
        emit.updates.push(compaction_started_notice(
            None,
            CompactionReason::Manual,
            self.measured_context_tokens(),
            window,
        ));
        effects.push(Effect::Compact { turn: None });
    }

    fn wake(
        &mut self,
        text: Box<str>,
        sources: Box<[Box<str>]>,
        jobs: Box<[JobId]>,
        now: jiff::Timestamp,
        emit: &mut Emit,
        _effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        if matches!(&self.phase, Phase::Compacting { .. }) {
            return Err(Rejection::BusyTurn);
        }
        let source = TurnSource::Wake {
            sources,
            jobs,
            content: vec![Part::Text { text }],
        };
        let turn = self.allocate_turn()?;
        self.wake_run += 1;
        self.wake_attempt_turn = Some(turn);
        emit.records.push(Record::WakeAttempt {
            at: now,
            turn,
            count: self.wake_run,
        });
        if matches!(&self.phase, Phase::Idle) {
            let entry = self.allocate_entry_id()?;
            self.phase = Phase::Opening {
                turn,
                source,
                entry,
            };
            return Ok(());
        }
        self.queued_inputs
            .push(QueuedInput::FollowUp { turn, source });
        Ok(())
    }

    fn guard(
        &mut self,
        turn: TurnId,
        target: HookTarget,
        outcome: HookOutcome,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        match outcome.into_verdict() {
            HookVerdict::BeforeTurn(add) => self.begin_turn(turn, add, now, emit, effects),
            HookVerdict::BeforeRequest(Some(params)) if matches!(&self.phase, Phase::Running { turn: active, .. } if *active == turn) =>
            {
                self.request_params = params;
                Ok(())
            }
            HookVerdict::ToolCall(verdict) => {
                self.tool_call_guard(turn, target, verdict, now, emit, effects)
            }
            HookVerdict::BeforeRequest(_) | HookVerdict::Input(_) => Ok(()),
        }
    }

    /// Takes the input of the turn a `before_turn` verdict opens, reserving its entry id.
    fn take_opening_input(
        &mut self,
        turn: TurnId,
    ) -> Result<Option<(TurnSource, EntryId)>, Rejection> {
        match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Opening {
                turn: active,
                source,
                entry,
            } if active == turn => Ok(Some((source, entry))),
            Phase::Settling {
                turn: active,
                follow_up: Some((next_turn, source)),
            } if next_turn == turn => match self.allocate_entry_id() {
                Ok(entry) => Ok(Some((source, entry))),
                Err(error) => {
                    self.phase = Phase::Settling {
                        turn: active,
                        follow_up: Some((next_turn, source)),
                    };
                    Err(error)
                }
            },
            other => {
                self.phase = other;
                Ok(None)
            }
        }
    }

    fn begin_turn(
        &mut self,
        turn: TurnId,
        add: Option<Box<str>>,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        let Some((source, entry)) = self.take_opening_input(turn)? else {
            return Ok(());
        };
        let (mut content, cause) = match source {
            TurnSource::Prompt { content, .. } => (content, TurnCause::User),
            TurnSource::Wake { content, .. } => (content, TurnCause::Wake),
            TurnSource::FollowUp { content, .. } => (content, TurnCause::FollowUp),
        };
        if cause != TurnCause::Wake
            && self
                .wake_attempt_turn
                .is_some_and(|attempt| attempt.get() < turn.get())
        {
            self.wake_run = 0;
            self.wake_attempt_turn = None;
        }
        if let Some(text) = add
            && !text.is_empty()
        {
            let separator = if content
                .iter()
                .any(|part| matches!(part, Part::Text { text } if !text.is_empty()))
            {
                "\n\n"
            } else {
                ""
            };
            content.push(Part::Text {
                text: format!("{separator}{text}").into(),
            });
        }
        let entry_record = self.entry_at(
            entry,
            now,
            EntryKind::User {
                parts: content.iter().map(part_to_journal).collect(),
            },
        );
        let view = self.tree.append(entry_record.clone());
        emit.records.push(Record::TurnStart { at: now, turn });
        emit.records.push(Record::User(entry_record));
        emit.updates.push(UpdateKind::TurnStarted { turn, cause });
        emit.updates.push(UpdateKind::Tree(TreeDelta {
            added: vec![view],
            leaf: self.tree.leaf,
        }));
        self.turn_flags.interrupts = 0;
        self.turn_flags.overflowed = false;
        self.turn_flags.suppressed_notice = false;
        self.argument_overrides.clear();
        self.phase = Phase::Running {
            turn,
            round: Step(0),
            stage: TurnStage::Boundary,
        };
        self.boundary(turn, now, false, emit, effects)
    }

    fn tool_call_guard(
        &mut self,
        turn: TurnId,
        target: HookTarget,
        verdict: ToolCallVerdict,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        match verdict {
            ToolCallVerdict::Allow => Ok(()),
            ToolCallVerdict::Rewrite { args } => {
                let call = target
                    .call
                    .ok_or_else(|| invalid("tool-call rewrite omitted its call id"))?;
                if let Some((_, previous)) = self
                    .argument_overrides
                    .iter_mut()
                    .find(|(id, _)| *id == call)
                {
                    *previous = args;
                } else {
                    self.argument_overrides.push((call, args));
                }
                Ok(())
            }
            ToolCallVerdict::Block { reason } => {
                let call = target
                    .call
                    .ok_or_else(|| invalid("tool-call block omitted its call id"))?;
                let extension = target
                    .extension
                    .ok_or_else(|| invalid("tool-call block omitted its extension name"))?;
                let text = format!("blocked by {extension}: {reason}").into();
                self.block_call(turn, &call, text, now, emit, effects)
            }
        }
    }

    fn block_call(
        &mut self,
        turn: TurnId,
        call: &CallId,
        text: Box<str>,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        let (round, mut pending) = match &self.phase {
            Phase::Running {
                turn: active,
                round,
                stage: TurnStage::Dispatching { pending },
            } if *active == turn => (*round, pending.clone()),
            _ => return Ok(()),
        };
        let Some(index) = pending.iter().position(|item| item.call == *call) else {
            return Ok(());
        };
        let item = pending.remove(index);
        self.result_entry(&item.call, &item.name, text, true, now, emit)?;
        if pending.is_empty() {
            self.phase = Phase::Running {
                turn,
                round,
                stage: TurnStage::Boundary,
            };
            return self.boundary(turn, now, true, emit, effects);
        }
        self.phase = Phase::Running {
            turn,
            round,
            stage: TurnStage::Dispatching { pending },
        };
        Ok(())
    }

    fn stream_verdict(
        &mut self,
        turn: TurnId,
        verdict: StreamVerdict,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        let round = match &self.phase {
            Phase::Running {
                turn: active,
                round,
                stage: TurnStage::Streaming { .. },
            } if *active == turn => *round,
            _ => return Ok(()),
        };
        match verdict {
            StreamVerdict::Continue => {}
            StreamVerdict::Interrupt { rule, inject }
                if self.turn_flags.interrupts < MAX_INTERRUPTS =>
            {
                let entry = self.entry(
                    now,
                    EntryKind::Reminder {
                        source: format!("rule:{rule}").into(),
                        text: inject,
                    },
                )?;
                self.turn_flags.interrupts += 1;
                let view = self.tree.append(entry.clone());
                emit.records.push(Record::RuleFired {
                    at: now,
                    turn,
                    rule: rule.clone(),
                    entry: entry.id,
                });
                emit.records.push(Record::Reminder(entry));
                emit.updates.push(UpdateKind::RuleFired { turn, rule });
                emit.updates.push(UpdateKind::Tree(TreeDelta {
                    added: vec![view],
                    leaf: self.tree.leaf,
                }));
                self.phase = Phase::Running {
                    turn,
                    round,
                    stage: TurnStage::Streaming {
                        blocks: Vec::new(),
                        usage: zero_usage(),
                        calls: Vec::new(),
                        suppressed_injects: Vec::new(),
                    },
                };
                effects.push(Effect::Infer(self.request_plan(turn)));
            }
            StreamVerdict::Interrupt { inject, .. } => {
                self.turn_flags.pending_suppressed.push(inject);
                let notify = !self.turn_flags.suppressed_notice;
                self.turn_flags.suppressed_notice = true;
                if notify {
                    emit.updates.push(UpdateKind::Notice(Notice {
                        turn: Some(turn),
                        kind: "rule.suppressed".into(),
                        text: "rule.suppressed".into(),
                    }));
                }
            }
        }
        Ok(())
    }

    fn stream(
        &mut self,
        turn: TurnId,
        event: StreamEvent,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) {
        if !matches!(&self.phase, Phase::Running { turn: active, stage: TurnStage::Streaming { .. }, .. } if *active == turn)
        {
            return;
        }
        match event {
            StreamEvent::Delta { channel, text } => effects.push(Effect::Delta {
                turn,
                channel,
                text,
            }),
            StreamEvent::ToolCall { call, name, args } => {
                emit.updates.push(UpdateKind::ToolStarted {
                    call,
                    tool: name,
                    args,
                });
            }
            StreamEvent::Usage(_) | StreamEvent::Stop(_) | StreamEvent::ThinkingReplay { .. } => {}
        }
    }

    fn stream_ended(
        &mut self,
        turn: TurnId,
        end: StreamEnd,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        if !matches!(&self.phase, Phase::Running { turn: active, stage: TurnStage::Streaming { .. }, .. } if *active == turn)
        {
            return Ok(());
        }
        let StreamEnd {
            model,
            family,
            result,
            partial,
        } = end;
        self.active_model = Some(model.clone());
        self.active_family = Some(family);
        match result {
            Ok(inference) => {
                let response = CompletedResponse {
                    model,
                    family,
                    inference,
                };
                self.response_completed(turn, response, now, emit, effects)
            }
            Err(failure) => self.stream_failed(turn, failure, partial, now, emit, effects),
        }
    }

    fn stream_failed(
        &mut self,
        turn: TurnId,
        failure: InferFailure,
        partial: Option<PartialResponse>,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        let message: Box<str> = match failure {
            InferFailure::Retryable { .. } => return Ok(()),
            InferFailure::Cancelled => {
                return self.end_turn(turn, TurnEndStop::Cancelled, partial, now, emit, effects);
            }
            InferFailure::Overflow { message, .. } => {
                if !self.turn_flags.overflowed && self.overflow_compaction(turn, emit, effects) {
                    return Ok(());
                }
                format!("Context overflow recovery failed: {message}").into()
            }
            InferFailure::Fatal { message, .. } => message,
            error @ (InferFailure::SyntheticCycle { .. } | InferFailure::SyntheticDepth { .. }) => {
                error.to_string().into()
            }
        };
        self.end_turn(
            turn,
            TurnEndStop::Failed { message },
            partial,
            now,
            emit,
            effects,
        )
    }

    /// Starts overflow compaction; returns `false` when no compactor can run.
    fn overflow_compaction(
        &mut self,
        turn: TurnId,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> bool {
        let Some(limits) = self
            .limits
            .filter(|limits| limits.compact.compactor_available)
        else {
            if !self.compaction_none_notified {
                emit.updates
                    .push(compact_notice(Some(turn), "compact.none"));
                self.compaction_none_notified = true;
            }
            return false;
        };
        self.turn_flags.overflowed = true;
        self.pending_compaction = Some(CompactionReason::Overflow);
        self.phase = Phase::Running {
            turn,
            round: self.current_round(),
            stage: TurnStage::Compacting {
                reason: CompactionReason::Overflow,
                automatic: true,
            },
        };
        emit.updates.push(compaction_started_notice(
            Some(turn),
            CompactionReason::Overflow,
            self.measured_context_tokens(),
            limits.window,
        ));
        effects.push(Effect::Compact { turn: Some(turn) });
        true
    }

    fn response_completed(
        &mut self,
        turn: TurnId,
        response: CompletedResponse,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        self.turn_flags.overflowed = false;
        let InferredResponse {
            blocks,
            calls,
            usage,
            stop,
        } = blocks_from_inference(response.inference);
        let Some(stop) = stop else {
            let partial = Some(PartialResponse {
                content: blocks,
                usage: usage.unwrap_or(zero_usage()),
            });
            let message = "provider inference ended without a stop event".into();
            return self.end_turn(
                turn,
                TurnEndStop::Failed { message },
                partial,
                now,
                emit,
                effects,
            );
        };
        let usage = usage.unwrap_or(zero_usage());
        let assistant = self.entry(
            now,
            EntryKind::Assistant {
                api: response.family,
                model: response.model.id().into(),
                content: blocks,
                usage,
                stop: assistant_stop(stop),
            },
        )?;
        self.last_usage = Some((response.model, usage.input_tokens, self.compactions));
        let view = self.tree.append(assistant.clone());
        emit.records.push(Record::Assistant(assistant));
        let mut pending: Vec<PendingCall> = Vec::new();
        for (call, name) in calls {
            if pending.iter().all(|item| item.call != call) {
                pending.push(PendingCall {
                    call,
                    name,
                    started: false,
                    promotes: None,
                });
            }
        }
        emit.updates.push(UpdateKind::Tree(TreeDelta {
            added: vec![view],
            leaf: self.tree.leaf,
        }));
        emit.updates.push(UpdateKind::Usage(crate::view::UsageView {
            usage,
            context_tokens: usage.input_tokens,
            context_window: self.limits.map_or(0, |limits| limits.window),
        }));
        self.response_stopped(turn, stop, pending, now, emit, effects)
    }

    fn response_stopped(
        &mut self,
        turn: TurnId,
        stop: Stop,
        pending: Vec<PendingCall>,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        let round = self.current_round();
        let end = match stop {
            Stop::Length if !pending.is_empty() => {
                self.fail_calls(&pending, truncated_args, now, emit)?;
                self.phase = Phase::Running {
                    turn,
                    round,
                    stage: TurnStage::Boundary,
                };
                return self.boundary(turn, now, true, emit, effects);
            }
            Stop::Filter => {
                self.fail_calls(&pending, truncated_args, now, emit)?;
                self.phase = Phase::Running {
                    turn,
                    round,
                    stage: TurnStage::Boundary,
                };
                TurnEndStop::Filter
            }
            Stop::Length => TurnEndStop::Length,
            Stop::EndTurn => {
                let stage = if pending.is_empty() {
                    self.turn_flags.end_after_boundary = true;
                    TurnStage::Boundary
                } else {
                    TurnStage::Resolving { pending }
                };
                self.phase = Phase::Running { turn, round, stage };
                return Ok(());
            }
            Stop::Cancelled | Stop::Failed | Stop::MaxSteps => {
                self.phase = Phase::Running {
                    turn,
                    round,
                    stage: TurnStage::Resolving { pending },
                };
                match stop {
                    Stop::Cancelled => TurnEndStop::Cancelled,
                    Stop::MaxSteps => TurnEndStop::MaxSteps,
                    _ => TurnEndStop::Failed {
                        message: "provider ended with a failed stop".into(),
                    },
                }
            }
        };
        self.end_turn(turn, end, None, now, emit, effects)
    }

    fn resolved(
        &mut self,
        turn: TurnId,
        calls: &[ResolvedCall],
        answerer_attached: bool,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        let expected = match &self.phase {
            Phase::Running {
                turn: active,
                stage: TurnStage::Resolving { pending },
                ..
            } if *active == turn => pending.clone(),
            _ => return Ok(()),
        };
        if calls
            .iter()
            .any(|call| !expected.iter().any(|item| item.call == call.call))
            || expected
                .iter()
                .any(|item| !calls.iter().any(|call| call.call == item.call))
        {
            return Err(invalid(
                "resolved calls do not match the pending response calls",
            ));
        }
        let mut planned = Vec::new();
        let mut promotes = Vec::new();
        for item in &expected {
            let mut matching = calls.iter().filter(|call| call.call == item.call);
            let Some(call) = matching.next() else {
                continue;
            };
            if let Some(text) = resolution_failure(call, matching.next().is_some()) {
                self.result_entry(&item.call, &item.name, text, true, now, emit)?;
                continue;
            }
            let Ok(class) = &call.result else {
                continue;
            };
            planned.push(PlannedCall {
                call: item.call.clone(),
                name: call.name.clone(),
                class: class.clone(),
            });
            promotes.push(call.promoted);
        }
        let units = plan(&planned, &self.policy(answerer_attached));
        let pending = planned
            .into_iter()
            .zip(promotes)
            .map(|(call, promoted)| PendingCall {
                call: call.call,
                name: call.name.to_string().into(),
                started: false,
                promotes: promoted.then_some(call.name),
            })
            .collect::<Vec<_>>();
        let round = self.current_round();
        if pending.is_empty() {
            self.phase = Phase::Running {
                turn,
                round,
                stage: TurnStage::Boundary,
            };
            self.boundary(turn, now, true, emit, effects)?;
        } else {
            self.phase = Phase::Running {
                turn,
                round,
                stage: TurnStage::Dispatching { pending },
            };
            effects.push(Effect::Dispatch { turn, units });
        }
        Ok(())
    }

    fn call_started(&mut self, turn: TurnId, call: &CallId, now: jiff::Timestamp, emit: &mut Emit) {
        let Phase::Running {
            turn: active,
            stage: TurnStage::Dispatching { pending },
            ..
        } = &mut self.phase
        else {
            return;
        };
        if *active != turn {
            return;
        }
        let Some(item) = pending
            .iter_mut()
            .find(|item| item.call == *call && !item.started)
        else {
            return;
        };
        item.started = true;
        emit.records.push(Record::ToolStart {
            at: now,
            turn,
            call: item.call.clone(),
        });
    }

    fn settled(
        &mut self,
        turn: TurnId,
        call: &CallId,
        outcome: SettledOutcome,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        let (round, mut pending) = match &self.phase {
            Phase::Running {
                turn: active,
                round,
                stage: TurnStage::Dispatching { pending },
            } if *active == turn => (*round, pending.clone()),
            _ => return Ok(()),
        };
        let Some(index) = pending.iter().position(|item| item.call == *call) else {
            return Ok(());
        };
        let item = pending.remove(index);
        let succeeded = matches!(outcome, SettledOutcome::Ok { .. });
        let (text, is_error) = match outcome {
            SettledOutcome::Ok { text } => (text, false),
            SettledOutcome::Err { text } => (text, true),
            SettledOutcome::Interrupted => ("Tool call interrupted by user.".into(), true),
            SettledOutcome::Detached { job } => (
                format!("still running after the foreground budget; detached as job {job}. Its result arrives as a message when it ends; read job://{job} for output now.").into(),
                false,
            ),
        };
        self.result_entry(&item.call, &item.name, text.clone(), is_error, now, emit)?;
        if succeeded
            && let Some(tool) = item.promotes
            && !self.promoted.contains(&tool)
        {
            emit.records.push(Record::ToolPromoted {
                at: now,
                tool: tool.to_string().into(),
                turn: Some(turn),
                leaf: self.tree.leaf,
            });
            self.promoted.insert(tool);
        }
        emit.updates.push(UpdateKind::ToolSettled {
            call: item.call,
            outcome: ToolOutcomeView {
                is_error,
                text,
                images: Vec::new(),
            },
        });
        if pending.is_empty() {
            self.phase = Phase::Running {
                turn,
                round,
                stage: TurnStage::Boundary,
            };
            self.boundary(turn, now, true, emit, effects)?;
        } else {
            self.phase = Phase::Running {
                turn,
                round,
                stage: TurnStage::Dispatching { pending },
            };
        }
        Ok(())
    }

    fn boundary(
        &mut self,
        turn: TurnId,
        now: jiff::Timestamp,
        completed_round: bool,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        let round = match &self.phase {
            Phase::Running {
                turn: active,
                round,
                stage: TurnStage::Boundary,
            } if *active == turn => *round,
            _ => return Ok(()),
        };
        let final_response = self.turn_flags.end_after_boundary;
        let steer = self.pop_steer();
        if final_response {
            self.turn_flags.end_after_boundary = false;
            if steer.is_none() {
                return self.end_turn(turn, TurnEndStop::Done, None, now, emit, effects);
            }
        }
        self.journal_boundary_inputs(steer, now, emit)?;
        let completed_round = completed_round && !final_response;
        let next_round = if completed_round {
            let Some(next) = round.0.checked_add(1) else {
                return self.end_turn(
                    turn,
                    TurnEndStop::Failed {
                        message: "tool round counter space exhausted".into(),
                    },
                    None,
                    now,
                    emit,
                    effects,
                );
            };
            Step(next)
        } else {
            round
        };
        if completed_round
            && let Some(limits) = self.limits
            && limits.max_steps > 0
            && next_round.0 >= limits.max_steps
        {
            emit.updates.push(UpdateKind::Notice(Notice {
                turn: Some(turn),
                kind: "max_steps".into(),
                text: format!("Stopped after {} tool rounds: loop.max_steps = {}. Raise loop.max_steps in config to continue.", next_round.0, limits.max_steps).into(),
            }));
            return self.end_turn(turn, TurnEndStop::MaxSteps, None, now, emit, effects);
        }
        self.open_round(turn, next_round, emit, effects);
        Ok(())
    }

    fn journal_entry(
        &mut self,
        now: jiff::Timestamp,
        kind: EntryKind,
        record: fn(Entry) -> Record,
        emit: &mut Emit,
    ) -> Result<(), Rejection> {
        let entry = self.entry(now, kind)?;
        let view = self.tree.append(entry.clone());
        emit.records.push(record(entry));
        emit.updates.push(UpdateKind::Tree(TreeDelta {
            added: vec![view],
            leaf: self.tree.leaf,
        }));
        Ok(())
    }

    fn journal_boundary_inputs(
        &mut self,
        steer: Option<Vec<Part>>,
        now: jiff::Timestamp,
        emit: &mut Emit,
    ) -> Result<(), Rejection> {
        if let Some(parts) = steer {
            let parts = parts.iter().map(part_to_journal).collect();
            self.journal_entry(now, EntryKind::User { parts }, Record::User, emit)?;
        }
        for inject in std::mem::take(&mut self.turn_flags.pending_suppressed) {
            let kind = EntryKind::Reminder {
                source: "rule:suppressed".into(),
                text: inject,
            };
            self.journal_entry(now, kind, Record::Reminder, emit)?;
        }
        if !self.ended_jobs.is_empty() {
            let ids = self
                .ended_jobs
                .iter()
                .map(|(job, _)| job.to_string())
                .collect::<Vec<_>>();
            let kind = EntryKind::Reminder {
                source: "jobs.finished".into(),
                text: format!("jobs.finished: {}", ids.join(", ")).into(),
            };
            self.journal_entry(now, kind, Record::Reminder, emit)?;
            self.ended_jobs.clear();
        }
        Ok(())
    }

    fn open_round(
        &mut self,
        turn: TurnId,
        round: Step,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) {
        let compaction = self.should_compact();
        let compactor_missing = compaction.is_none() && self.threshold_compaction_due();
        self.active_model = None;
        self.active_family = None;
        if let Some(reason) = compaction {
            self.pending_compaction = Some(reason);
            self.phase = Phase::Running {
                turn,
                round,
                stage: TurnStage::Compacting {
                    reason,
                    automatic: true,
                },
            };
            let window = self.limits.map_or(0, |limits| limits.window);
            emit.updates.push(compaction_started_notice(
                Some(turn),
                reason,
                self.measured_context_tokens(),
                window,
            ));
            effects.push(Effect::Compact { turn: Some(turn) });
            return;
        }
        if compactor_missing && !self.compaction_none_notified {
            emit.updates
                .push(compact_notice(Some(turn), "compact.none"));
            self.compaction_none_notified = true;
        }
        self.request_params = RequestParams {
            thinking: self.settings.thinking,
            effort: None,
            temperature: None,
        };
        self.phase = Phase::Running {
            turn,
            round,
            stage: TurnStage::Streaming {
                blocks: Vec::new(),
                usage: zero_usage(),
                calls: Vec::new(),
                suppressed_injects: Vec::new(),
            },
        };
        effects.push(Effect::Infer(self.request_plan(turn)));
    }
    fn end_turn(
        &mut self,
        turn: TurnId,
        stop: TurnEndStop,
        partial: Option<PartialResponse>,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        let stage = match &self.phase {
            Phase::Running {
                turn: active,
                stage,
                ..
            } if *active == turn => stage.clone(),
            _ => return Ok(()),
        };
        let pending = unsettled_calls(stage, partial.as_ref());
        let turn_usage = partial.as_ref().map(|partial| partial.usage);
        if let Some(partial) = partial.filter(|partial| !partial.content.is_empty()) {
            self.journal_partial(partial, &stop, now, emit)?;
        }
        let result_text = if matches!(stop, TurnEndStop::Cancelled) {
            "Tool call interrupted by user."
        } else {
            TOOL_LOST
        };
        self.fail_calls(&pending, |_| result_text.into(), now, emit)?;
        let update_stop = match &stop {
            TurnEndStop::Done => Stop::EndTurn,
            TurnEndStop::Length => Stop::Length,
            TurnEndStop::Filter => Stop::Filter,
            TurnEndStop::MaxSteps => Stop::MaxSteps,
            TurnEndStop::Cancelled => Stop::Cancelled,
            TurnEndStop::Aborted | TurnEndStop::Failed { .. } => Stop::Failed,
        };
        let can_continue = matches!(
            &stop,
            TurnEndStop::Done | TurnEndStop::Length | TurnEndStop::Filter | TurnEndStop::MaxSteps
        );
        emit.records.push(Record::TurnEnd {
            at: now,
            turn,
            stop,
            usage: turn_usage,
            changes: Vec::new(),
        });
        emit.updates.push(UpdateKind::TurnEnded {
            turn,
            stop: update_stop,
        });
        self.last_turn = self.last_turn.max(turn.get());
        let follow_up = if can_continue && self.steers_queued() == 0 {
            self.pop_follow_up()
        } else {
            None
        };
        self.close_turn(turn, follow_up, emit);
        effects.push(Effect::Stop {
            turn,
            stop: update_stop,
        });
        Ok(())
    }

    fn journal_partial(
        &mut self,
        partial: PartialResponse,
        stop: &TurnEndStop,
        now: jiff::Timestamp,
        emit: &mut Emit,
    ) -> Result<(), Rejection> {
        let (Some(model), Some(family)) = (&self.active_model, self.active_family) else {
            return Err(invalid(
                "partial assistant content has no active model route",
            ));
        };
        let assistant = self.entry(
            now,
            EntryKind::Assistant {
                api: family,
                model: model.id().into(),
                content: partial.content,
                usage: partial.usage,
                stop: assistant_stop_for_end(stop),
            },
        )?;
        let view = self.tree.append(assistant.clone());
        emit.records.push(Record::Assistant(assistant));
        emit.updates.push(UpdateKind::Tree(TreeDelta {
            added: vec![view],
            leaf: self.tree.leaf,
        }));
        Ok(())
    }

    fn close_turn(
        &mut self,
        turn: TurnId,
        follow_up: Option<(TurnId, TurnSource)>,
        emit: &mut Emit,
    ) {
        if follow_up.is_none() && !self.queued_inputs.is_empty() {
            self.discard_queued(turn, emit);
        }
        self.open_questions.clear();
        self.argument_overrides.clear();
        self.pending_compaction = None;
        self.turn_flags = TurnFlags::default();
        self.active_model = None;
        self.active_family = None;
        self.phase = if follow_up.is_some() {
            Phase::Settling { turn, follow_up }
        } else {
            Phase::Idle
        };
    }

    fn cancel(
        &mut self,
        scope: CancelScope,
        partial: Option<PartialResponse>,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        match scope {
            CancelScope::Job(_) => Ok(()),
            CancelScope::Turn(turn) => {
                if matches!(&self.phase, Phase::Opening { turn: active, .. } if *active == turn) {
                    self.phase = Phase::Idle;
                    effects.push(Effect::Reply(Ok(Reply::Done)));
                    return Ok(());
                }
                if matches!(&self.phase, Phase::Running { turn: active, .. } if *active == turn) {
                    return self.end_turn(
                        turn,
                        TurnEndStop::Cancelled,
                        partial,
                        now,
                        emit,
                        effects,
                    );
                }
                Err(wrong_turn(Expect::After(turn), self.turn_state()))
            }
        }
    }

    fn compaction_settled(
        &mut self,
        turn: Option<TurnId>,
        outcome: Result<CompactionSummary, Box<str>>,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        let phase_matches = match (turn, &self.phase) {
            (None, Phase::Compacting { .. }) => true,
            (
                Some(turn),
                Phase::Running {
                    turn: active,
                    stage: TurnStage::Compacting { .. },
                    ..
                },
            ) => turn == *active,
            _ => false,
        };
        if !phase_matches {
            return Ok(());
        }
        let reason = self
            .pending_compaction
            .ok_or_else(|| invalid("compaction result has no pending request"))?;
        self.pending_compaction = None;
        if turn.is_none() {
            self.manual_completion.result_settled = true;
        }
        match outcome {
            Ok(summary) if summary.tokens_after >= summary.tokens_before => {
                self.compaction_not_shrunk(turn, &summary, emit, effects);
                Ok(())
            }
            Ok(summary) => self.compaction_applied(turn, summary, now, emit, effects),
            Err(error) => {
                let Some(turn) = turn else {
                    emit.updates.push(UpdateKind::Notice(Notice {
                        turn: None,
                        kind: "compact.manual_failed".into(),
                        text: format!("Compaction failed: {error}").into(),
                    }));
                    self.finish_manual_compaction_if_ready();
                    return Ok(());
                };
                let message: Option<Box<str>> = (reason == CompactionReason::Overflow)
                    .then(|| format!("Context overflow recovery failed: {error}").into());
                emit.updates.push(UpdateKind::Notice(Notice {
                    turn: Some(turn),
                    kind: "compaction_ended".into(),
                    text: error,
                }));
                self.record_auto_failure(Some(turn), emit);
                let Some(message) = message else {
                    self.continue_after_compaction(turn, effects);
                    return Ok(());
                };
                self.end_turn(
                    turn,
                    TurnEndStop::Failed { message },
                    None,
                    now,
                    emit,
                    effects,
                )
            }
        }
    }

    fn compaction_not_shrunk(
        &mut self,
        turn: Option<TurnId>,
        summary: &CompactionSummary,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) {
        let text: Box<str> = format!(
            "Compaction rejected: the summary did not shrink the context ({} to {} tokens). The old context is kept.",
            summary.tokens_before, summary.tokens_after,
        )
        .into();
        emit.updates.push(UpdateKind::Notice(Notice {
            turn,
            kind: "compaction_ended".into(),
            text: text.clone(),
        }));
        if let Some(turn) = turn {
            self.record_auto_failure(Some(turn), emit);
            self.continue_after_compaction(turn, effects);
        } else {
            emit.updates.push(UpdateKind::Notice(Notice {
                turn: None,
                kind: "compact.manual_failed".into(),
                text,
            }));
            self.finish_manual_compaction_if_ready();
        }
    }

    fn compaction_applied(
        &mut self,
        turn: Option<TurnId>,
        summary: CompactionSummary,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        let next_compactions = self
            .compactions
            .checked_add(1)
            .ok_or_else(|| invalid("compaction counter space exhausted"))?;
        let first_kept = summary.first_kept.or_else(|| {
            self.limits
                .and_then(|limits| self.cut_point(limits.compact.keep_tokens))
        });
        let success_text = format!(
            "Context compacted by {}: {} to {} tokens.",
            summary.compactor, summary.tokens_before, summary.tokens_after,
        );
        let entry = self.entry(
            now,
            EntryKind::Compaction {
                summary: summary.summary,
                first_kept,
                tokens_before: summary.tokens_before,
                replay: summary.replay,
                usage: summary.usage,
            },
        )?;
        let view = self.tree.append(entry.clone());
        emit.records.push(Record::Compaction(entry));
        emit.updates.push(UpdateKind::Tree(TreeDelta {
            added: vec![view],
            leaf: self.tree.leaf,
        }));
        emit.updates.push(UpdateKind::Notice(Notice {
            turn,
            kind: "compaction_ended".into(),
            text: success_text.into(),
        }));
        self.compactions = next_compactions;
        self.last_usage = None;
        self.auto_failures = 0;
        if let Some(turn) = turn {
            self.continue_after_compaction(turn, effects);
        } else {
            self.breaker_open = false;
            self.finish_manual_compaction_if_ready();
        }
        Ok(())
    }

    fn finish_manual_compaction_if_ready(&mut self) {
        if !self.manual_completion.job_settled
            || !self.manual_completion.result_settled
            || !matches!(&self.phase, Phase::Compacting { .. })
        {
            return;
        }
        self.pending_manual_focus = None;
        self.manual_completion = ManualCompletion::default();
        self.phase = Phase::Idle;
    }
    fn record_auto_failure(&mut self, turn: Option<TurnId>, emit: &mut Emit) {
        if self.auto_failures < 3 {
            self.auto_failures += 1;
        }
        if self.auto_failures == 3 && !self.breaker_open {
            self.breaker_open = true;
            emit.updates.push(compact_notice(turn, "compact.breaker"));
        }
    }
    fn continue_after_compaction(&mut self, turn: TurnId, effects: &mut Vec<Effect>) {
        self.phase = Phase::Running {
            turn,
            round: self.current_round(),
            stage: TurnStage::Streaming {
                blocks: Vec::new(),
                usage: zero_usage(),
                calls: Vec::new(),
                suppressed_injects: Vec::new(),
            },
        };
        effects.push(Effect::Infer(self.request_plan(turn)));
    }

    fn fail_calls(
        &mut self,
        calls: &[PendingCall],
        render: impl Fn(&str) -> Box<str>,
        now: jiff::Timestamp,
        emit: &mut Emit,
    ) -> Result<(), Rejection> {
        for item in calls {
            self.result_entry(&item.call, &item.name, render(&item.name), true, now, emit)?;
        }
        Ok(())
    }

    fn result_entry(
        &mut self,
        call: &CallId,
        name: &str,
        text: Box<str>,
        error: bool,
        now: jiff::Timestamp,
        emit: &mut Emit,
    ) -> Result<(), Rejection> {
        let entry = self.entry(
            now,
            EntryKind::ToolResult {
                call: call.clone(),
                name: name.into(),
                error,
                parts: vec![JournalPart::Text { text }],
                changes: Vec::new(),
            },
        )?;
        let view = self.tree.append(entry.clone());
        emit.records.push(Record::ToolResult(entry));
        emit.updates.push(UpdateKind::Tree(TreeDelta {
            added: vec![view],
            leaf: self.tree.leaf,
        }));
        Ok(())
    }

    fn discard_queued(&mut self, turn: TurnId, emit: &mut Emit) {
        if self.queued_inputs.is_empty() {
            return;
        }
        let texts = self
            .queued_inputs
            .drain(..)
            .map(|queued| match queued {
                QueuedInput::Steer(parts) => parts_to_text(&parts),
                QueuedInput::FollowUp { source, .. } => match source {
                    TurnSource::Prompt { content, .. }
                    | TurnSource::FollowUp { content, .. }
                    | TurnSource::Wake { content, .. } => parts_to_text(&content),
                },
            })
            .collect::<Vec<_>>();
        emit.updates.push(UpdateKind::Notice(Notice {
            turn: Some(turn),
            kind: "discarded".into(),
            text: texts.join("\n").into(),
        }));
    }

    fn request_plan(&self, turn: TurnId) -> ModelRequestPlan {
        ModelRequestPlan {
            turn,
            params: self.request_params.clone(),
        }
    }

    fn current_round(&self) -> Step {
        match &self.phase {
            Phase::Running { round, .. } => *round,
            _ => Step(0),
        }
    }
    fn projected_bytes_on_branch(&self) -> u64 {
        let branch = self.tree.ancestors(self.tree.leaf);
        let start = branch
            .iter()
            .rposition(|id| {
                self.tree
                    .entries
                    .get(id)
                    .is_some_and(|entry| matches!(&entry.kind, EntryKind::Compaction { .. }))
            })
            .unwrap_or(0);
        branch
            .iter()
            .skip(start)
            .filter_map(|id| self.tree.entries.get(id))
            .map(entry_weight)
            .fold(0_u64, u64::saturating_add)
    }

    fn settings_view(&self) -> SettingsView {
        SettingsView {
            model: self.settings.model.clone(),
            thinking: self.settings.thinking,
            approval: self.settings.approval,
            mode: self.settings.mode,
            name: self.settings.name.clone(),
        }
    }

    fn allocate_turn(&mut self) -> Result<TurnId, Rejection> {
        let turn = self
            .next_turn
            .ok_or_else(|| invalid("turn id space exhausted"))?;
        self.next_turn = turn
            .get()
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .map(TurnId::new);
        Ok(turn)
    }

    fn allocate_entry_id(&mut self) -> Result<EntryId, Rejection> {
        let entry = self
            .next_entry
            .ok_or_else(|| invalid("entry id space exhausted"))?;
        self.next_entry = entry
            .get()
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .map(EntryId::new);
        Ok(entry)
    }

    fn entry(&mut self, at: jiff::Timestamp, kind: EntryKind) -> Result<Entry, Rejection> {
        let id = self.allocate_entry_id()?;
        Ok(self.entry_at(id, at, kind))
    }

    fn entry_at(&mut self, id: EntryId, at: jiff::Timestamp, kind: EntryKind) -> Entry {
        let entry = Entry {
            id,
            parent: self.tree.leaf,
            at,
            kind,
        };
        self.projected_bytes = self.projected_bytes.saturating_add(entry_weight(&entry));
        entry
    }

    fn replay_setting(&mut self, entry: &Entry) {
        match &entry.kind {
            EntryKind::Model { route } => self.settings.model = Some(route.clone()),
            EntryKind::Thinking { level } => {
                self.settings.thinking = *level;
                self.request_params.thinking = *level;
            }
            EntryKind::Approval { mode } => self.settings.approval = *mode,
            _ => {}
        }
    }
}

fn invalid(text: &str) -> Rejection {
    Rejection::Invalid {
        reason: text.into(),
    }
}
fn wrong_turn(expected: Expect, actual: TurnState) -> Rejection {
    Rejection::WrongTurn { expected, actual }
}
fn zero_usage() -> Usage {
    Usage {
        input_tokens: 0,
        cached_input_tokens: 0,
        output_tokens: 0,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        cost_usd: None,
    }
}
fn assistant_stop_for_end(stop: &TurnEndStop) -> AssistantStop {
    match stop {
        TurnEndStop::Done | TurnEndStop::MaxSteps => AssistantStop::Done,
        TurnEndStop::Length => AssistantStop::Length,
        TurnEndStop::Filter => AssistantStop::Filter,
        TurnEndStop::Cancelled => AssistantStop::Cancelled,
        TurnEndStop::Aborted => AssistantStop::Failed {
            message: "turn aborted".into(),
        },
        TurnEndStop::Failed { message } => AssistantStop::Failed {
            message: message.clone(),
        },
    }
}
fn assistant_stop(stop: Stop) -> AssistantStop {
    match stop {
        Stop::EndTurn | Stop::MaxSteps => AssistantStop::Done,
        Stop::Length => AssistantStop::Length,
        Stop::Filter => AssistantStop::Filter,
        Stop::Cancelled => AssistantStop::Cancelled,
        Stop::Failed => AssistantStop::Failed {
            message: "provider ended with a failed stop".into(),
        },
    }
}
fn compact_notice(turn: Option<TurnId>, key: &str) -> UpdateKind {
    let (kind, text) = match key {
        "compact.nothing" => ("compact.nothing", "Nothing to compact (session too small)."),
        "compact.already" => ("compact.already", "Already compacted."),
        "compact.none" => (
            "compaction_none",
            "No compactor is registered, so auto-compaction does nothing in this session.",
        ),
        "compact.breaker" => (
            "compaction_off",
            "Auto-compaction is off for this session after 3 failed attempts. Run /compact to try again.",
        ),
        _ => (key, key),
    };
    UpdateKind::Notice(Notice {
        turn,
        kind: kind.into(),
        text: text.into(),
    })
}

fn compaction_started_notice(
    turn: Option<TurnId>,
    reason: CompactionReason,
    tokens: Option<u64>,
    window: u64,
) -> UpdateKind {
    let reason = match reason {
        CompactionReason::Threshold => "threshold",
        CompactionReason::Overflow => "overflow",
        CompactionReason::Manual => "manual",
    };
    let text = match (tokens, window) {
        (Some(tokens), window) if window > 0 => {
            let percent = u128::from(tokens) * 100 / u128::from(window);
            format!("Compacting context ({reason}, {percent} percent of the window).")
        }
        _ => format!("Compacting context ({reason}); usage or context window is unavailable."),
    };
    UpdateKind::Notice(Notice {
        turn,
        kind: "compaction_started".into(),
        text: text.into(),
    })
}
fn entry_weight(entry: &Entry) -> u64 {
    match &entry.kind {
        EntryKind::User { parts } | EntryKind::ToolResult { parts, .. } => {
            parts.iter().map(journal_part_bytes).sum()
        }
        EntryKind::Assistant { content, .. } => content.iter().map(block_bytes).sum(),
        EntryKind::Reminder { text, .. } => text.len() as u64,
        _ => 0,
    }
}
fn entry_token_weight(entry: &Entry) -> u64 {
    match &entry.kind {
        EntryKind::Assistant { usage, .. } => usage.input_tokens,
        _ => entry_weight(entry) / 4,
    }
}
fn journal_part_bytes(part: &JournalPart) -> u64 {
    match part {
        JournalPart::Text { text } => text.len() as u64,
        JournalPart::TextBlob { bytes, .. }
        | JournalPart::ImageBlob { bytes, .. }
        | JournalPart::Blob { bytes, .. } => *bytes,
        JournalPart::Image { base64, .. } => base64.len() as u64,
    }
}
fn block_bytes(block: &Block) -> u64 {
    match block {
        Block::Text { text } | Block::Reasoning { text, .. } => text.len() as u64,
        Block::ToolCall { input, .. } => input.as_str().len() as u64,
    }
}
fn parse_job_kind(value: Option<&str>) -> Option<JobKind> {
    match value? {
        "exec" => Some(JobKind::Exec),
        "child" => Some(JobKind::Child),
        "compaction" => Some(JobKind::Compaction),
        _ => None,
    }
}
fn parts_to_text(parts: &[Part]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            Part::Text { text } => Some(text.as_ref()),
            Part::Image { .. } | Part::Blob { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
fn part_to_journal(part: &Part) -> JournalPart {
    match part {
        Part::Text { text } => JournalPart::Text { text: text.clone() },
        Part::Blob {
            blob_id,
            mime,
            bytes,
        } if mime.starts_with("text/") => JournalPart::TextBlob {
            blob: hex(blob_id.as_bytes()),
            bytes: *bytes,
        },
        Part::Blob {
            blob_id,
            mime,
            bytes,
        } if mime.starts_with("image/") => JournalPart::ImageBlob {
            mime: mime.clone(),
            blob: hex(blob_id.as_bytes()),
            bytes: *bytes,
        },
        Part::Blob {
            blob_id,
            mime,
            bytes,
        } => JournalPart::Blob {
            mime: mime.clone(),
            blob: hex(blob_id.as_bytes()),
            bytes: *bytes,
        },
        Part::Image { mime, bytes } => JournalPart::Image {
            mime: mime.clone(),
            base64: base64(bytes),
        },
    }
}
/// The model-visible result for a call that failed resolution, or `None` when it runs.
fn resolution_failure(call: &ResolvedCall, duplicate: bool) -> Option<Box<str>> {
    let text = if duplicate || matches!(call.result, Err(ResolveError::DuplicateCallId)) {
        format!(
            "invalid arguments for {}: duplicate call id {}",
            call.name,
            call.call.as_str()
        )
    } else {
        match &call.result {
            Ok(_) | Err(ResolveError::DuplicateCallId) => return None,
            Err(ResolveError::Unknown) => format!("unknown tool: {}", call.name),
            Err(ResolveError::EvalOnly) => {
                format!("{} is callable only from eval cells", call.name)
            }
            Err(ResolveError::InvalidArgs(detail)) => {
                format!("invalid arguments for {}: {detail}", call.name)
            }
        }
    };
    Some(text.into())
}
/// Calls that still need a result when a turn ends: the stage's pending calls, then
/// tool calls from a journaled partial response that no stage tracked.
fn unsettled_calls(stage: TurnStage, partial: Option<&PartialResponse>) -> Vec<PendingCall> {
    let mut pending = match stage {
        TurnStage::Dispatching { pending } | TurnStage::Resolving { pending } => pending,
        TurnStage::Streaming { .. } | TurnStage::Boundary | TurnStage::Compacting { .. } => {
            Vec::new()
        }
    };
    let partial_calls = partial.into_iter().flat_map(|partial| &partial.content);
    for block in partial_calls {
        if let Block::ToolCall { id, name, .. } = block
            && !pending.iter().any(|item| item.call == *id)
        {
            pending.push(PendingCall {
                call: id.clone(),
                name: name.clone(),
                started: false,
                promotes: None,
            });
        }
    }
    pending
}
/// The call and extension a guarding hook result names.
struct HookTarget {
    call: Option<CallId>,
    extension: Option<Box<str>>,
}

/// The final report of one provider request, as carried by [`Event::StreamEnded`].
struct StreamEnd {
    model: ModelRoute,
    family: Family,
    result: Result<Inference, InferFailure>,
    partial: Option<PartialResponse>,
}

/// A provider request that returned a complete inference.
struct CompletedResponse {
    model: ModelRoute,
    family: Family,
    inference: Inference,
}

/// One completed provider response split into journal blocks and its stream facts.
struct InferredResponse {
    blocks: Vec<Block>,
    calls: Vec<(CallId, Box<str>)>,
    usage: Option<Usage>,
    stop: Option<Stop>,
}

fn blocks_from_inference(inference: Inference) -> InferredResponse {
    let mut blocks = Vec::new();
    let mut calls = Vec::new();
    let mut usage = None;
    let mut stop = None;
    let mut current_reasoning = None;
    for event in inference.events {
        match event {
            StreamEvent::Delta {
                channel: StreamChannel::Text,
                text,
            } => {
                blocks.push(Block::Text { text });
                current_reasoning = None;
            }
            StreamEvent::Delta {
                channel: StreamChannel::Thinking,
                text,
            } => {
                if let Some(index) = current_reasoning {
                    if let Some(Block::Reasoning { text: previous, .. }) = blocks.get_mut(index) {
                        let mut combined = previous.to_string();
                        combined.push_str(&text);
                        *previous = combined.into();
                    }
                } else {
                    blocks.push(Block::Reasoning {
                        text,
                        replay: RawJson::null(),
                    });
                    current_reasoning = Some(blocks.len() - 1);
                }
            }
            StreamEvent::ThinkingReplay { payload } => {
                if let Some(index) = current_reasoning {
                    if let Some(Block::Reasoning { replay, .. }) = blocks.get_mut(index) {
                        *replay = payload;
                    }
                } else {
                    blocks.push(Block::Reasoning {
                        text: String::new().into(),
                        replay: payload,
                    });
                    current_reasoning = Some(blocks.len() - 1);
                }
            }
            StreamEvent::Delta {
                channel: StreamChannel::ToolArgs { .. },
                ..
            } => {}
            StreamEvent::ToolCall { call, name, args } => {
                current_reasoning = None;
                calls.push((call.clone(), name.clone()));
                blocks.push(Block::ToolCall {
                    id: call,
                    name,
                    input: args,
                });
            }
            StreamEvent::Usage(measurement) => usage = Some(measurement),
            StreamEvent::Stop(reason) => stop = Some(reason),
        }
    }
    InferredResponse {
        blocks,
        calls,
        usage,
        stop,
    }
}
fn hex(bytes: &[u8]) -> Box<str> {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out.into()
}
fn base64(bytes: &[u8]) -> Box<str> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = *chunk.get(1).unwrap_or(&0);
        let c = *chunk.get(2).unwrap_or(&0);
        out.push(TABLE[(a >> 2) as usize] as char);
        out.push(TABLE[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[(((b & 15) << 2) | (c >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(c & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out.into_boxed_str()
}
fn truncated_args(name: &str) -> Box<str> {
    format!("Tool call \"{name}\" was not executed: the response hit the output token limit, so its arguments may be truncated. Re-issue the tool call with complete arguments.").into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{num::NonZeroU64, path::PathBuf};

    fn id(value: u64) -> TurnId {
        TurnId::new(NonZeroU64::new(value).unwrap())
    }
    fn entry(value: u64) -> EntryId {
        EntryId::new(NonZeroU64::new(value).unwrap())
    }
    fn stamp() -> jiff::Timestamp {
        jiff::Timestamp::UNIX_EPOCH
    }
    fn session() -> Session {
        Session::replay([], stamp()).unwrap().0
    }

    #[test]
    fn restore_preserves_boot_and_rejects_unrecovered_work() {
        let session_id = crate::SessionId::new_v7();
        let generation = Gen::new(NonZeroU64::MIN);
        let records = vec![
            Record::Session(crate::Header {
                id: session_id,
                at: stamp(),
                workspace: crate::Workspace::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")))
                    .unwrap(),
                product: crate::Product::Dal,
                from: None,
            }),
            Record::Boot {
                at: stamp(),
                r#gen: generation,
                version: PRODUCT_VERSION.into(),
            },
        ];
        let restored = Session::restore(records.clone()).unwrap();
        assert_eq!(restored.id, Some(session_id));
        assert_eq!(restored.generation, Some(generation));
        assert_eq!(restored.phase(), &Phase::Idle);
        assert_eq!(restored.settings(), session().settings());
        assert!(matches!(
            Session::restore([]),
            Err(ReplayError::Contradiction { .. })
        ));
        assert!(matches!(
            Session::restore(records[..1].iter().cloned()),
            Err(ReplayError::Contradiction { .. })
        ));

        let mut unfinished = records.clone();
        unfinished.push(Record::TurnStart {
            at: stamp(),
            turn: id(1),
        });
        assert!(matches!(
            Session::restore(unfinished),
            Err(ReplayError::Contradiction { .. })
        ));
        let (reopened, _) = Session::replay(records, stamp()).unwrap();
        assert_eq!(reopened.generation.unwrap().get(), 2);
    }
    #[test]
    fn replay_rejects_missing_parents_and_reused_entries() {
        let setting = |value, parent| {
            Record::Thinking(Entry {
                id: entry(value),
                parent,
                at: stamp(),
                kind: EntryKind::Thinking {
                    level: ThinkingLevel::Low,
                },
            })
        };
        for records in [
            vec![setting(2, Some(entry(1)))],
            vec![setting(1, Some(entry(1)))],
            vec![setting(1, None), setting(1, None)],
            vec![setting(2, None), setting(1, Some(entry(2)))],
        ] {
            assert!(matches!(
                Session::replay(records, stamp()),
                Err(ReplayError::Contradiction { .. })
            ));
        }
    }

    #[test]
    fn moving_leaf_restores_branch_settings_before_publish() {
        let mut session = session();
        let mut records = Vec::new();
        for cmd in [
            Command::SetModel(route()),
            Command::SetThinking(ThinkingLevel::Low),
            Command::SetApproval(ApprovalMode::Edits),
        ] {
            append_emitted(
                &send(&mut session, Event::Command { cmd, by: client() }).unwrap(),
                &mut records,
            );
        }
        let anchor = session.tree.leaf.unwrap();
        let mut expected = session.settings().clone();
        expected.name = Some("named session".into());
        for cmd in [
            Command::SetModel(ModelRoute::Api {
                family: Family::Responses,
                model: "other-model".into(),
            }),
            Command::SetThinking(ThinkingLevel::High),
            Command::SetApproval(ApprovalMode::All),
            Command::Rename("named session".into()),
        ] {
            append_emitted(
                &send(&mut session, Event::Command { cmd, by: client() }).unwrap(),
                &mut records,
            );
        }
        let moved = send(
            &mut session,
            Event::Command {
                cmd: Command::MoveLeaf(anchor),
                by: client(),
            },
        )
        .unwrap();
        assert_eq!(session.settings(), &expected);
        assert_eq!(session.request_params.thinking, ThinkingLevel::Low);
        assert!(moved.iter().any(|effect| {
            matches!(effect, Effect::Emit(emit) if emit.updates.iter().any(|update| {
                matches!(update, UpdateKind::Settings(settings) if *settings == session.settings_view())
            }))
        }));
        append_emitted(&moved, &mut records);
        let replayed = Session::replay(records, stamp()).unwrap().0;
        assert_eq!(replayed.settings(), &expected);
    }

    fn client() -> ClientId {
        ClientId::new("test")
    }
    fn name(value: &str) -> Name {
        Name::parse(value).unwrap()
    }
    fn route() -> ModelRoute {
        ModelRoute::Api {
            family: Family::Chat,
            model: "test-model".into(),
        }
    }
    fn usage(tokens: u64) -> Usage {
        Usage {
            input_tokens: tokens,
            cached_input_tokens: 0,
            output_tokens: 1,
            reasoning_tokens: None,
            cache_write_tokens: 0,
            cost_usd: None,
        }
    }
    fn prompt(text: &str) -> Event {
        Event::Command {
            cmd: Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text { text: text.into() }],
            },
            by: client(),
        }
    }
    fn guard(turn: TurnId) -> Event {
        Event::Guard {
            turn,
            call: None,
            extension: None,
            outcome: HookOutcome::new(HookEvent::BeforeTurn, HookVerdict::BeforeTurn(None))
                .unwrap(),
        }
    }
    fn send(session: &mut Session, event: Event) -> Result<Vec<Effect>, Rejection> {
        let mut out = Vec::new();
        session.step(event, stamp(), &mut out)?;
        Ok(out)
    }
    fn begin(session: &mut Session) -> TurnId {
        send(session, prompt("question")).unwrap();
        let turn = match session.phase() {
            Phase::Opening { turn, .. } => *turn,
            phase => panic!("prompt did not open a turn: {phase:?}"),
        };
        send(session, guard(turn)).unwrap();
        turn
    }
    fn limits(min_tokens: u64) -> Event {
        Event::Limits {
            window: 100_000,
            max_steps: 0,
            compact: CompactionLimits {
                threshold: 0.85,
                min_tokens,
                keep_tokens: 20_000,
                enabled: true,
                compactor_available: true,
            },
        }
    }
    fn inference(stop: Stop, calls: &[(&str, &str)], tokens: u64) -> Inference {
        let mut events = vec![StreamEvent::Usage(usage(tokens))];
        for (call, tool) in calls {
            events.push(StreamEvent::ToolCall {
                call: CallId::new(*call),
                name: (*tool).into(),
                args: RawJson::parse("{}").unwrap(),
            });
        }
        events.push(StreamEvent::Stop(stop));
        Inference { events }
    }
    fn stream_result(session: &mut Session, turn: TurnId, response: Inference) -> Vec<Effect> {
        send(
            session,
            Event::StreamEnded {
                turn,
                model: route(),
                family: Family::Chat,
                result: Ok(response),
                partial: None,
            },
        )
        .unwrap()
    }
    fn only_tool_result(records: &[Record]) -> &Entry {
        records
            .iter()
            .find_map(|record| match record {
                Record::ToolResult(entry) => Some(entry),
                _ => None,
            })
            .unwrap()
    }
    fn append_emitted(out: &[Effect], records: &mut Vec<Record>) {
        for effect in out {
            if let Effect::Emit(emit) = effect {
                records.extend(emit.records.iter().cloned());
            }
        }
    }
    fn same_replayed_state(live: &Session, records: &[Record]) {
        let replayed = Session::replay(records.iter().cloned(), stamp()).unwrap().0;
        assert_eq!(replayed.phase, live.phase);
        assert_eq!(replayed.tree, live.tree);
        assert_eq!(replayed.settings, live.settings);
        assert_eq!(replayed.allow_always, live.allow_always);
        assert_eq!(replayed.promoted, live.promoted);
        assert_eq!(replayed.next_turn, live.next_turn);
        assert_eq!(replayed.next_entry, live.next_entry);
        assert_eq!(replayed.last_turn, live.last_turn);
        assert_eq!(replayed.wake_run, live.wake_run);
        assert_eq!(replayed.projected_bytes, live.projected_bytes);
    }
    fn one_read_round(session: &mut Session, turn: TurnId, call: &str, tokens: u64) -> Vec<Effect> {
        stream_result(
            session,
            turn,
            inference(Stop::EndTurn, &[(call, "read_file")], tokens),
        );
        let result = ResolvedCall {
            call: CallId::new(call),
            name: name("read_file"),
            promoted: false,
            result: Ok(ToolClass::Read),
        };
        send(
            session,
            Event::Resolved {
                turn,
                calls: vec![result],
                answerer_attached: false,
            },
        )
        .unwrap();
        send(
            session,
            Event::CallStarted {
                turn,
                call: CallId::new(call),
            },
        )
        .unwrap();
        send(
            session,
            Event::Settled {
                turn,
                call: CallId::new(call),
                outcome: SettledOutcome::Ok {
                    text: "read".into(),
                },
            },
        )
        .unwrap()
    }
    fn compact_summary(before: u64, after: u64) -> CompactionSummary {
        CompactionSummary {
            compactor: name("summary"),
            tokens_before: before,
            tokens_after: after,
            summary: Some("kept context".into()),
            first_kept: Some(entry(1)),
            replay: None,
            usage: None,
        }
    }

    fn settle_manual_compaction(session: &mut Session, before: u64, after: u64) -> Vec<Effect> {
        let job = JobId::parse("01890f47-36b0-7cc4-8000-000000000002").unwrap();
        send(
            session,
            Event::JobStarted {
                job,
                kind: JobKind::Compaction,
            },
        )
        .unwrap();
        let out = send(
            session,
            Event::CompactionSettled {
                turn: None,
                outcome: Ok(compact_summary(before, after)),
            },
        )
        .unwrap();
        send(
            session,
            Event::JobSettled {
                job,
                outcome: JobOutcome::Exited { code: 0 },
            },
        )
        .unwrap();
        out
    }

    #[test]
    fn wrong_turn_changes_nothing() {
        let mut session = session();
        let before = session.clone();
        let mut out = vec![Effect::Reply(Ok(Reply::Done))];
        let old_out = out.clone();
        let result = session.step(
            Event::Command {
                cmd: Command::Steer {
                    turn: id(9),
                    content: vec![Part::Text {
                        text: "late".into(),
                    }],
                },
                by: client(),
            },
            stamp(),
            &mut out,
        );
        assert!(matches!(result, Err(Rejection::WrongTurn { .. })));
        assert_eq!(session, before);
        assert_eq!(out, old_out);
    }

    #[test]
    fn steer_cell_holds_16() {
        let mut session = session();
        let turn = begin(&mut session);
        for index in 0..MAX_STEERS {
            send(
                &mut session,
                Event::Steer {
                    turn,
                    text: index.to_string().into(),
                },
            )
            .unwrap();
        }
        assert_eq!(
            send(
                &mut session,
                Event::Steer {
                    turn,
                    text: "overflow".into()
                }
            ),
            Err(Rejection::SteerFull)
        );
        assert_eq!(session.steers_queued(), 16);
    }

    #[test]
    fn steer_at_final_response_extends_turn() {
        let mut session = session();
        let turn = begin(&mut session);
        stream_result(&mut session, turn, inference(Stop::EndTurn, &[], 10));
        let queued = send(
            &mut session,
            Event::Command {
                cmd: Command::Steer {
                    turn,
                    content: vec![Part::Text {
                        text: "continue".into(),
                    }],
                },
                by: client(),
            },
        )
        .unwrap();
        assert!(matches!(
            queued.as_slice(),
            [Effect::Reply(Ok(Reply::Queued))]
        ));
        let boundary = send(&mut session, Event::Boundary { turn }).unwrap();
        assert!(
            boundary
                .iter()
                .any(|effect| matches!(effect, Effect::Infer(_)))
        );
        assert!(
            !boundary
                .iter()
                .any(|effect| matches!(effect, Effect::Stop { .. }))
        );
    }

    #[test]
    fn cancel_discards_queued_input() {
        let mut session = session();
        let turn = begin(&mut session);
        send(
            &mut session,
            Event::Command {
                cmd: Command::FollowUp {
                    turn,
                    content: vec![Part::Text {
                        text: "first".into(),
                    }],
                },
                by: client(),
            },
        )
        .unwrap();
        send(
            &mut session,
            Event::Steer {
                turn,
                text: "second".into(),
            },
        )
        .unwrap();
        send(
            &mut session,
            Event::Command {
                cmd: Command::FollowUp {
                    turn,
                    content: vec![Part::Text {
                        text: "third".into(),
                    }],
                },
                by: client(),
            },
        )
        .unwrap();
        let out = send(
            &mut session,
            Event::Cancel {
                scope: CancelScope::Turn(turn),
                partial: None,
            },
        )
        .unwrap();
        let emit = out
            .iter()
            .find_map(|effect| {
                if let Effect::Emit(emit) = effect {
                    Some(emit)
                } else {
                    None
                }
            })
            .unwrap();
        assert!(emit.updates.iter().any(|update| matches!(
            update,
            UpdateKind::TurnEnded {
                stop: Stop::Cancelled,
                ..
            }
        )));
        assert!(emit.updates.iter().any(|update| matches!(update, UpdateKind::Notice(notice) if notice.kind.as_ref() == "discarded" && notice.text.as_ref() == "first\nsecond\nthird")));
        assert!(matches!(session.phase(), Phase::Idle));
    }

    #[test]
    fn length_stop_fails_calls() {
        let mut session = session();
        let turn = begin(&mut session);
        let out = stream_result(
            &mut session,
            turn,
            inference(Stop::Length, &[("call", "edit_file")], 20),
        );
        let records = out
            .iter()
            .find_map(|effect| {
                if let Effect::Emit(emit) = effect {
                    Some(&emit.records)
                } else {
                    None
                }
            })
            .unwrap();
        assert!(matches!(
            only_tool_result(records).kind,
            EntryKind::ToolResult { error: true, .. }
        ));
        assert!(
            matches!(&only_tool_result(records).kind, EntryKind::ToolResult { parts, .. } if matches!(parts.first(), Some(JournalPart::Text { text }) if text.as_ref() == truncated_args("edit_file").as_ref()))
        );
        assert!(
            !out.iter()
                .any(|effect| matches!(effect, Effect::Dispatch { .. } | Effect::Stop { .. }))
        );
        assert!(out.iter().any(|effect| matches!(effect, Effect::Infer(_))));
        let mut no_calls = self::session();
        let no_call_turn = begin(&mut no_calls);
        let out = stream_result(
            &mut no_calls,
            no_call_turn,
            inference(Stop::Length, &[], 10),
        );
        assert!(out.iter().any(|effect| matches!(
            effect,
            Effect::Stop {
                stop: Stop::Length,
                ..
            }
        )));
    }

    #[test]
    fn overflow_compacts_once_then_fails() {
        let mut session = session();
        send(&mut session, limits(0)).unwrap();
        let turn = begin(&mut session);
        let first = send(
            &mut session,
            Event::StreamEnded {
                turn,
                model: route(),
                family: Family::Chat,
                result: Err(InferFailure::Overflow {
                    code: "context".into(),
                    message: "too large".into(),
                }),
                partial: None,
            },
        )
        .unwrap();
        assert!(first.iter().any(
            |effect| matches!(effect, Effect::Compact { turn: Some(active) } if *active == turn)
        ));
        let compacted = send(
            &mut session,
            Event::CompactionSettled {
                turn: Some(turn),
                outcome: Ok(compact_summary(100, 50)),
            },
        )
        .unwrap();
        assert_eq!(session.compactions(), 1);
        assert!(session.cache_key().ends_with(":1"));
        assert!(
            compacted
                .iter()
                .any(|effect| matches!(effect, Effect::Infer(_)))
        );
        let failed = send(
            &mut session,
            Event::StreamEnded {
                turn,
                model: route(),
                family: Family::Chat,
                result: Err(InferFailure::Overflow {
                    code: "context".into(),
                    message: "still too large".into(),
                }),
                partial: None,
            },
        )
        .unwrap();
        assert!(failed.iter().any(|effect| matches!(
            effect,
            Effect::Stop {
                stop: Stop::Failed,
                ..
            }
        )));
        let emit = failed
            .iter()
            .find_map(|effect| {
                if let Effect::Emit(emit) = effect {
                    Some(emit)
                } else {
                    None
                }
            })
            .unwrap();
        assert!(matches!(
            emit.records.last(),
            Some(Record::TurnEnd { stop: TurnEndStop::Failed { message }, .. })
                if message.contains("Context overflow recovery failed")
        ));
    }

    #[test]
    fn threshold_compaction_updates_cache_key() {
        let mut session = session();
        send(&mut session, limits(1)).unwrap();
        let turn = begin(&mut session);
        let out = one_read_round(&mut session, turn, "threshold", 85_100);
        assert!(out.iter().any(|effect| matches!(effect, Effect::Emit(emit) if emit.updates.iter().any(|update| matches!(update, UpdateKind::Notice(notice) if notice.kind.as_ref() == "compaction_started" && notice.text.contains("85 percent of the window"))))));
        assert!(out.iter().any(
            |effect| matches!(effect, Effect::Compact { turn: Some(active) } if *active == turn)
        ));
        let out = send(
            &mut session,
            Event::CompactionSettled {
                turn: Some(turn),
                outcome: Ok(compact_summary(85_100, 20_000)),
            },
        )
        .unwrap();
        assert_eq!(session.compactions(), 1);
        assert!(session.cache_key().ends_with(":1"));
        assert!(out.iter().any(|effect| matches!(effect, Effect::Infer(_))));
    }

    #[test]
    fn missing_compactor_notifies_once_and_keeps_inference_running() {
        let mut session = session();
        send(
            &mut session,
            Event::Limits {
                window: 100_000,
                max_steps: 0,
                compact: CompactionLimits {
                    threshold: 0.85,
                    min_tokens: 1,
                    keep_tokens: 20_000,
                    enabled: true,
                    compactor_available: false,
                },
            },
        )
        .unwrap();
        let turn = begin(&mut session);
        let first = one_read_round(&mut session, turn, "first", 90_000);
        assert!(
            !first
                .iter()
                .any(|effect| matches!(effect, Effect::Compact { .. }))
        );
        assert!(
            first
                .iter()
                .any(|effect| matches!(effect, Effect::Infer(_)))
        );
        assert_eq!(first.iter().filter(|effect| matches!(effect, Effect::Emit(emit) if emit.updates.iter().any(|update| matches!(update, UpdateKind::Notice(notice) if notice.kind.as_ref() == "compaction_none")))).count(), 1);

        let second = one_read_round(&mut session, turn, "second", 90_000);
        assert!(
            !second
                .iter()
                .any(|effect| matches!(effect, Effect::Compact { .. }))
        );
        assert!(
            second
                .iter()
                .any(|effect| matches!(effect, Effect::Infer(_)))
        );
        assert!(!second.iter().any(|effect| matches!(effect, Effect::Emit(emit) if emit.updates.iter().any(|update| matches!(update, UpdateKind::Notice(notice) if notice.kind.as_ref() == "compaction_none")))));
    }

    #[test]
    fn breaker_after_three_automatic_failures() {
        let mut session = session();
        send(&mut session, limits(1)).unwrap();
        let turn = begin(&mut session);
        for index in 0..3 {
            let compact = one_read_round(&mut session, turn, &format!("call{index}"), 90_000);
            assert!(
                compact
                    .iter()
                    .any(|effect| matches!(effect, Effect::Compact { .. }))
            );
            let out = send(
                &mut session,
                Event::CompactionSettled {
                    turn: Some(turn),
                    outcome: Err("compactor unavailable".into()),
                },
            )
            .unwrap();
            if index == 2 {
                assert!(!session.auto_compaction_on());
                assert!(out.iter().any(|effect| matches!(effect, Effect::Emit(emit) if emit.updates.iter().any(|update| matches!(update, UpdateKind::Notice(notice) if notice.kind.as_ref() == "compaction_off" && notice.text.contains("3 failed attempts"))))));
            }
        }
        send(
            &mut session,
            Event::Cancel {
                scope: CancelScope::Turn(turn),
                partial: None,
            },
        )
        .unwrap();
        send(
            &mut session,
            Event::Command {
                cmd: Command::Compact { focus: None },
                by: client(),
            },
        )
        .unwrap();
        assert!(matches!(session.phase(), Phase::Compacting { .. }));
        let out = settle_manual_compaction(&mut session, 90_000, 20_000);
        assert!(out.iter().any(|effect| matches!(effect, Effect::Emit(emit) if emit.updates.iter().any(|update| matches!(update, UpdateKind::Notice(notice) if notice.kind.as_ref() == "compaction_ended")))));
        assert!(session.auto_compaction_on());
    }

    #[test]
    fn wake_limit_21st_refused() {
        let mut session = session();
        for _ in 0..20 {
            let mut out = Vec::new();
            session
                .step(
                    Event::Wake {
                        text: "wake".into(),
                        sources: Box::new([]),
                        jobs: Box::new([]),
                    },
                    stamp(),
                    &mut out,
                )
                .unwrap();
            let turn = match session.phase() {
                Phase::Opening { turn, .. } => *turn,
                phase => panic!("wake not opening: {phase:?}"),
            };
            session.step(guard(turn), stamp(), &mut out).unwrap();
            session
                .step(
                    Event::Cancel {
                        scope: CancelScope::Turn(turn),
                        partial: None,
                    },
                    stamp(),
                    &mut out,
                )
                .unwrap();
        }
        let mut out = vec![Effect::Reply(Ok(Reply::Done))];
        let before = out.clone();
        assert!(matches!(
            session.step(
                Event::Wake {
                    text: "refused".into(),
                    sources: Box::new([]),
                    jobs: Box::new([])
                },
                stamp(),
                &mut out
            ),
            Err(Rejection::Denied {
                reason: crate::approval::DenyReason::WakeLimit
            })
        ));
        assert_eq!(out, before);
        send(&mut session, prompt("user")).unwrap();
        assert_eq!(session.wake_run(), 0);
        let turn = match session.phase() {
            Phase::Opening { turn, .. } => *turn,
            _ => panic!("user prompt not opening"),
        };
        send(&mut session, guard(turn)).unwrap();
        send(
            &mut session,
            Event::Cancel {
                scope: CancelScope::Turn(turn),
                partial: None,
            },
        )
        .unwrap();
        assert!(
            send(
                &mut session,
                Event::Wake {
                    text: "again".into(),
                    sources: Box::new([]),
                    jobs: Box::new([])
                }
            )
            .is_ok()
        );
    }

    #[test]
    fn manual_compact_rejections() {
        let mut session = session();
        send(&mut session, limits(100)).unwrap();
        let turn = begin(&mut session);
        stream_result(&mut session, turn, inference(Stop::EndTurn, &[], 10));
        send(&mut session, Event::Boundary { turn }).unwrap();
        let out = send(
            &mut session,
            Event::Command {
                cmd: Command::Compact { focus: None },
                by: client(),
            },
        )
        .unwrap();
        assert!(out.iter().any(|effect| matches!(effect, Effect::Emit(emit) if emit.updates.iter().any(|update| matches!(update, UpdateKind::Notice(notice) if notice.kind.as_ref() == "compact.nothing")))));

        let mut session = self::session();
        send(&mut session, limits(1)).unwrap();
        let turn = begin(&mut session);
        stream_result(&mut session, turn, inference(Stop::EndTurn, &[], 10));
        send(&mut session, Event::Boundary { turn }).unwrap();
        send(
            &mut session,
            Event::Command {
                cmd: Command::Compact {
                    focus: Some("auth flow".into()),
                },
                by: client(),
            },
        )
        .unwrap();
        assert!(matches!(
            send(&mut session, prompt("blocked")),
            Err(Rejection::Compacting)
        ));
        let compacted = settle_manual_compaction(&mut session, 10, 5);
        assert!(compacted.iter().any(|effect| matches!(effect, Effect::Emit(emit) if emit.updates.iter().any(|update| matches!(update, UpdateKind::Notice(notice) if notice.kind.as_ref() == "compaction_ended")))));
        let out = send(
            &mut session,
            Event::Command {
                cmd: Command::Compact { focus: None },
                by: client(),
            },
        )
        .unwrap();
        assert!(out.iter().any(|effect| matches!(effect, Effect::Emit(emit) if emit.updates.iter().any(|update| matches!(update, UpdateKind::Notice(notice) if notice.kind.as_ref() == "compact.already")))));
    }

    #[test]
    fn stream_interrupt_replays_at_most_three() {
        let mut session = session();
        let turn = begin(&mut session);
        for index in 0..3 {
            let out = send(
                &mut session,
                Event::StreamVerdict {
                    turn,
                    verdict: StreamVerdict::Interrupt {
                        rule: format!("rule{index}").into(),
                        inject: format!("reminder{index}").into(),
                    },
                },
            )
            .unwrap();
            assert!(out.iter().any(|effect| matches!(effect, Effect::Infer(_))));
            assert!(out.iter().any(|effect| matches!(effect, Effect::Emit(emit) if emit.records.iter().any(|record| matches!(record, Record::RuleFired { .. })))));
        }
        let fourth = send(
            &mut session,
            Event::StreamVerdict {
                turn,
                verdict: StreamVerdict::Interrupt {
                    rule: "last".into(),
                    inject: "reminder".into(),
                },
            },
        )
        .unwrap();
        assert!(
            !fourth
                .iter()
                .any(|effect| matches!(effect, Effect::Infer(_)))
        );
        assert!(fourth.iter().any(|effect| matches!(effect, Effect::Emit(emit) if emit.updates.iter().any(|update| matches!(update, UpdateKind::Notice(notice) if notice.kind.as_ref() == "rule.suppressed")))));
    }

    #[test]
    fn replay_repairs_aborted_turn() {
        let session_id =
            crate::id::SessionId::parse("01890f47-36b0-7cc4-8000-000000000001").unwrap();
        let call = CallId::new("call");
        let header = crate::journal::Header {
            id: session_id,
            at: stamp(),
            workspace: crate::workspace::Workspace::new(PathBuf::from("/")).unwrap(),
            product: crate::journal::Product::Dal,
            from: None,
        };
        let assistant = Entry {
            id: entry(1),
            parent: None,
            at: stamp(),
            kind: EntryKind::Assistant {
                api: Family::Chat,
                model: "test".into(),
                content: vec![Block::ToolCall {
                    id: call.clone(),
                    name: "read_file".into(),
                    input: RawJson::parse("{}").unwrap(),
                }],
                usage: usage(1),
                stop: AssistantStop::ToolUse,
            },
        };
        let records = [
            Record::Session(header),
            Record::TurnStart {
                at: stamp(),
                turn: id(3),
            },
            Record::Assistant(assistant),
            Record::ToolStart {
                at: stamp(),
                turn: id(3),
                call,
            },
        ];
        let (session, effects) = Session::replay(records, stamp()).unwrap();
        assert!(matches!(session.phase(), Phase::Idle));
        assert_eq!(session.next_turn, Some(id(4)));
        let Effect::Emit(emit) = &effects[0] else {
            panic!("replay must append repair records");
        };
        assert!(matches!(
            emit.records.as_slice(),
            [
                Record::ToolResult(_),
                Record::TurnEnd {
                    stop: TurnEndStop::Aborted,
                    ..
                },
                Record::Boot { .. },
            ]
        ));
    }

    const LOST: &str = "Tool call was not completed: dalgon stopped before it finished.";

    fn open_turn(session: &mut Session, journal: &mut Vec<Record>) -> TurnId {
        append_emitted(&send(session, prompt("question")).unwrap(), journal);
        let turn = match session.phase() {
            Phase::Opening { turn, .. } => *turn,
            phase => panic!("prompt did not open a turn: {phase:?}"),
        };
        append_emitted(&send(session, guard(turn)).unwrap(), journal);
        turn
    }
    fn resolved_call(
        call: &str,
        tool: &str,
        promoted: bool,
        result: Result<ToolClass, ResolveError>,
    ) -> ResolvedCall {
        ResolvedCall {
            call: CallId::new(call),
            name: name(tool),
            promoted,
            result,
        }
    }
    fn results_of(records: &[Record], call: &str) -> Vec<(bool, Vec<JournalPart>)> {
        let call = CallId::new(call);
        records
            .iter()
            .filter_map(|record| match record {
                Record::ToolResult(entry) => match &entry.kind {
                    EntryKind::ToolResult {
                        call: result,
                        error,
                        parts,
                        ..
                    } if *result == call => Some((*error, parts.clone())),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }
    fn text_part(text: &str) -> Vec<JournalPart> {
        vec![JournalPart::Text { text: text.into() }]
    }
    fn assistant_record(entry_id: u64, calls: &[&str]) -> Record {
        Record::Assistant(Entry {
            id: entry(entry_id),
            parent: None,
            at: stamp(),
            kind: EntryKind::Assistant {
                api: Family::Chat,
                model: "test".into(),
                content: calls
                    .iter()
                    .map(|call| Block::ToolCall {
                        id: CallId::new(*call),
                        name: "read_file".into(),
                        input: RawJson::parse("{}").unwrap(),
                    })
                    .collect(),
                usage: usage(1),
                stop: AssistantStop::ToolUse,
            },
        })
    }
    fn result_record(entry_id: u64, call: &str) -> Record {
        Record::ToolResult(Entry {
            id: entry(entry_id),
            parent: None,
            at: stamp(),
            kind: EntryKind::ToolResult {
                call: CallId::new(call),
                name: "read_file".into(),
                error: false,
                parts: text_part("ok"),
                changes: Vec::new(),
            },
        })
    }
    fn start_record(turn: u64, call: &str) -> Record {
        Record::ToolStart {
            at: stamp(),
            turn: id(turn),
            call: CallId::new(call),
        }
    }
    fn turn_start_record(turn: u64) -> Record {
        Record::TurnStart {
            at: stamp(),
            turn: id(turn),
        }
    }
    fn turn_end_record(turn: u64) -> Record {
        Record::TurnEnd {
            at: stamp(),
            turn: id(turn),
            stop: TurnEndStop::Done,
            usage: None,
            changes: Vec::new(),
        }
    }
    fn contradicts(records: Vec<Record>) -> bool {
        matches!(
            Session::replay(records, stamp()),
            Err(ReplayError::Contradiction { .. })
        )
    }

    #[test]
    fn promoted_call_runs_and_promotes_only_after_success() {
        let mut session = session();
        let mut journal = Vec::new();
        let turn = open_turn(&mut session, &mut journal);
        let promotions = |journal: &[Record]| {
            journal
                .iter()
                .filter(|record| matches!(record, Record::ToolPromoted { .. }))
                .count()
        };
        for (round, call) in ["first", "again"].into_iter().enumerate() {
            append_emitted(
                &stream_result(
                    &mut session,
                    turn,
                    inference(Stop::EndTurn, &[(call, "web_search")], 10),
                ),
                &mut journal,
            );
            let resolved = send(
                &mut session,
                Event::Resolved {
                    turn,
                    calls: vec![resolved_call(call, "web_search", true, Ok(ToolClass::Read))],
                    answerer_attached: false,
                },
            )
            .unwrap();
            append_emitted(&resolved, &mut journal);
            assert!(resolved.iter().any(|effect| matches!(effect, Effect::Dispatch { units, .. }
                if matches!(units.as_slice(), [Unit::Reads { calls }] if *calls == [CallId::new(call)]))));
            assert_eq!(promotions(&journal), round);
            assert_eq!(session.promoted().contains(&name("web_search")), round > 0);
            assert!(results_of(&journal, call).is_empty());

            append_emitted(
                &send(
                    &mut session,
                    Event::CallStarted {
                        turn,
                        call: CallId::new(call),
                    },
                )
                .unwrap(),
                &mut journal,
            );
            let settled = send(
                &mut session,
                Event::Settled {
                    turn,
                    call: CallId::new(call),
                    outcome: SettledOutcome::Ok {
                        text: "found".into(),
                    },
                },
            )
            .unwrap();
            append_emitted(&settled, &mut journal);
            assert!(
                settled
                    .iter()
                    .any(|effect| matches!(effect, Effect::Infer(_)))
            );
            assert_eq!(
                results_of(&journal, call),
                vec![(false, text_part("found"))]
            );
            assert_eq!(promotions(&journal), 1);
            assert!(session.promoted().contains(&name("web_search")));
        }
        let result_at = journal
            .iter()
            .position(|record| matches!(record, Record::ToolResult(_)))
            .unwrap();
        let promoted_at = journal
            .iter()
            .position(|record| {
                matches!(record, Record::ToolPromoted { tool, turn: Some(owner), .. }
            if tool.as_ref() == "web_search" && *owner == turn)
            })
            .unwrap();
        assert!(result_at < promoted_at);
        append_emitted(
            &stream_result(&mut session, turn, inference(Stop::EndTurn, &[], 10)),
            &mut journal,
        );
        append_emitted(
            &send(&mut session, Event::Boundary { turn }).unwrap(),
            &mut journal,
        );
        assert!(matches!(session.phase(), Phase::Idle));
        same_replayed_state(&session, &journal);
        let (replayed, _) = Session::replay(journal, stamp()).unwrap();
        assert!(replayed.promoted().contains(&name("web_search")));
    }

    #[test]
    fn unsuccessful_promoting_calls_do_not_promote() {
        let outcomes = [
            SettledOutcome::Err {
                text: "failed".into(),
            },
            SettledOutcome::Interrupted,
            SettledOutcome::Detached {
                job: JobId::parse("01890f47-36b0-7cc4-8000-000000000002").unwrap(),
            },
        ];
        for outcome in outcomes {
            let mut session = session();
            let mut journal = Vec::new();
            let turn = open_turn(&mut session, &mut journal);
            append_emitted(
                &stream_result(
                    &mut session,
                    turn,
                    inference(Stop::EndTurn, &[("call", "web_search")], 10),
                ),
                &mut journal,
            );
            append_emitted(
                &send(
                    &mut session,
                    Event::Resolved {
                        turn,
                        calls: vec![resolved_call(
                            "call",
                            "web_search",
                            true,
                            Ok(ToolClass::Read),
                        )],
                        answerer_attached: false,
                    },
                )
                .unwrap(),
                &mut journal,
            );
            append_emitted(
                &send(
                    &mut session,
                    Event::CallStarted {
                        turn,
                        call: CallId::new("call"),
                    },
                )
                .unwrap(),
                &mut journal,
            );
            append_emitted(
                &send(
                    &mut session,
                    Event::Settled {
                        turn,
                        call: CallId::new("call"),
                        outcome,
                    },
                )
                .unwrap(),
                &mut journal,
            );
            assert_eq!(results_of(&journal, "call").len(), 1);
            assert!(
                !journal
                    .iter()
                    .any(|record| matches!(record, Record::ToolPromoted { .. }))
            );
            assert!(session.promoted().is_empty());
        }
    }

    #[test]
    fn every_resolved_call_gets_exactly_one_result() {
        let mut session = session();
        let mut journal = Vec::new();
        let turn = open_turn(&mut session, &mut journal);
        let provider_calls = [
            ("unknown", "nope"),
            ("eval", "cell_only"),
            ("bad", "read_file"),
            ("dup", "read_file"),
            ("promote", "web_search"),
            ("plain", "read_file"),
        ];
        append_emitted(
            &stream_result(
                &mut session,
                turn,
                inference(Stop::EndTurn, &provider_calls, 10),
            ),
            &mut journal,
        );
        let resolved = send(
            &mut session,
            Event::Resolved {
                turn,
                calls: vec![
                    resolved_call("unknown", "nope", false, Err(ResolveError::Unknown)),
                    resolved_call("eval", "cell_only", true, Err(ResolveError::EvalOnly)),
                    resolved_call(
                        "bad",
                        "read_file",
                        false,
                        Err(ResolveError::InvalidArgs("missing path".into())),
                    ),
                    resolved_call("dup", "read_file", false, Ok(ToolClass::Read)),
                    resolved_call("dup", "read_file", false, Ok(ToolClass::Read)),
                    resolved_call("promote", "web_search", true, Ok(ToolClass::Read)),
                    resolved_call("plain", "read_file", false, Ok(ToolClass::Read)),
                ],
                answerer_attached: false,
            },
        )
        .unwrap();
        append_emitted(&resolved, &mut journal);
        assert!(resolved.iter().any(|effect| matches!(effect, Effect::Dispatch { units, .. }
            if matches!(units.as_slice(), [Unit::Reads { calls }] if *calls == [CallId::new("promote"), CallId::new("plain")]))));
        assert!(
            !journal
                .iter()
                .any(|record| matches!(record, Record::ToolPromoted { .. }))
        );
        for call in ["promote", "plain"] {
            let started = Event::CallStarted {
                turn,
                call: CallId::new(call),
            };
            append_emitted(&send(&mut session, started).unwrap(), &mut journal);
            let settled = Event::Settled {
                turn,
                call: CallId::new(call),
                outcome: SettledOutcome::Ok { text: call.into() },
            };
            append_emitted(&send(&mut session, settled).unwrap(), &mut journal);
        }
        append_emitted(
            &stream_result(&mut session, turn, inference(Stop::EndTurn, &[], 10)),
            &mut journal,
        );
        append_emitted(
            &send(&mut session, Event::Boundary { turn }).unwrap(),
            &mut journal,
        );
        assert_eq!(journal.iter().filter(|record| matches!(record, Record::ToolPromoted { tool, .. } if tool.as_ref() == "web_search")).count(), 1);
        assert_eq!(
            session.promoted().iter().collect::<Vec<_>>(),
            vec![&name("web_search")]
        );
        assert!(matches!(session.phase(), Phase::Idle));

        let expected = [
            ("unknown", true, "unknown tool: nope"),
            ("eval", true, "cell_only is callable only from eval cells"),
            ("bad", true, "invalid arguments for read_file: missing path"),
            (
                "dup",
                true,
                "invalid arguments for read_file: duplicate call id dup",
            ),
            ("promote", false, "promote"),
            ("plain", false, "plain"),
        ];
        for (call, error, text) in expected {
            assert_eq!(
                results_of(&journal, call),
                vec![(error, text_part(text))],
                "{call}"
            );
        }
        same_replayed_state(&session, &journal);
    }

    #[test]
    fn replay_rejects_unpaired_tool_calls_and_results() {
        // A second result for one call.
        assert!(contradicts(vec![
            turn_start_record(1),
            assistant_record(1, &["a"]),
            start_record(1, "a"),
            result_record(2, "a"),
            result_record(3, "a"),
            turn_end_record(1),
        ]));
        // A repeated result after its turn ended.
        assert!(contradicts(vec![
            turn_start_record(1),
            assistant_record(1, &["a"]),
            result_record(2, "a"),
            turn_end_record(1),
            result_record(3, "a"),
        ]));
        // A result that answers no assistant call.
        assert!(contradicts(vec![
            turn_start_record(1),
            assistant_record(1, &["a"]),
            result_record(2, "b"),
            result_record(3, "a"),
            turn_end_record(1),
        ]));
        // A started call whose turn completes without its result.
        assert!(contradicts(vec![
            turn_start_record(1),
            assistant_record(1, &["a"]),
            start_record(1, "a"),
            turn_end_record(1)
        ]));
        // A never-started call whose turn completes without its result.
        assert!(contradicts(vec![
            turn_start_record(1),
            assistant_record(1, &["a", "b"]),
            result_record(2, "a"),
            turn_end_record(1),
        ]));
        // A start for a call no response made, and a start repeated for one call.
        assert!(contradicts(vec![
            turn_start_record(1),
            assistant_record(1, &["a"]),
            start_record(1, "b")
        ]));
        assert!(contradicts(vec![
            turn_start_record(1),
            assistant_record(1, &["a"]),
            start_record(1, "a"),
            start_record(1, "a")
        ]));

        // Resolution-error results have no start, and one response may repeat an id.
        let (session, effects) = Session::replay(
            vec![
                turn_start_record(1),
                assistant_record(1, &["a", "a", "b"]),
                result_record(2, "a"),
                start_record(1, "b"),
                result_record(3, "b"),
                turn_end_record(1),
            ],
            stamp(),
        )
        .unwrap();
        assert!(matches!(session.phase(), Phase::Idle));
        let [Effect::Emit(emit)] = effects.as_slice() else {
            panic!("replay must return one repair batch")
        };
        assert!(matches!(emit.records.as_slice(), [Record::Boot { .. }]));
    }

    #[test]
    fn replay_repairs_every_open_call_once() {
        let mut records = vec![
            turn_start_record(3),
            assistant_record(1, &["ran", "failed", "queued"]),
            start_record(3, "ran"),
            result_record(2, "failed"),
        ];
        let (session, effects) = Session::replay(records.clone(), stamp()).unwrap();
        assert!(matches!(session.phase(), Phase::Idle));
        assert_eq!(session.next_turn, Some(id(4)));
        let [Effect::Emit(repair)] = effects.as_slice() else {
            panic!("replay must return one repair batch")
        };
        assert!(matches!(
            repair.records.as_slice(),
            [
                Record::ToolResult(_),
                Record::ToolResult(_),
                Record::TurnEnd {
                    stop: TurnEndStop::Aborted,
                    ..
                },
                Record::Boot { .. },
            ]
        ));
        assert_eq!(
            results_of(&repair.records, "ran"),
            vec![(true, text_part(LOST))]
        );
        assert_eq!(
            results_of(&repair.records, "queued"),
            vec![(true, text_part(LOST))]
        );
        assert!(results_of(&repair.records, "failed").is_empty());
        assert!(matches!(&repair.records[0], Record::ToolResult(entry)
            if matches!(&entry.kind, EntryKind::ToolResult { call, .. } if *call == CallId::new("ran"))));

        records.extend(repair.records.iter().cloned());
        let (reopened, again) = Session::replay(records, stamp()).unwrap();
        assert!(matches!(reopened.phase(), Phase::Idle));
        let [Effect::Emit(second)] = again.as_slice() else {
            panic!("replay must return one repair batch")
        };
        assert!(matches!(second.records.as_slice(), [Record::Boot { .. }]));
    }

    #[test]
    fn promoted_call_lost_in_crash_is_repaired_once() {
        let mut session = session();
        let mut journal = Vec::new();
        let turn = open_turn(&mut session, &mut journal);
        append_emitted(
            &stream_result(
                &mut session,
                turn,
                inference(Stop::EndTurn, &[("call", "web_search")], 10),
            ),
            &mut journal,
        );
        append_emitted(
            &send(
                &mut session,
                Event::Resolved {
                    turn,
                    calls: vec![resolved_call(
                        "call",
                        "web_search",
                        true,
                        Ok(ToolClass::Read),
                    )],
                    answerer_attached: false,
                },
            )
            .unwrap(),
            &mut journal,
        );
        append_emitted(
            &send(
                &mut session,
                Event::CallStarted {
                    turn,
                    call: CallId::new("call"),
                },
            )
            .unwrap(),
            &mut journal,
        );

        let (replayed, effects) = Session::replay(journal.clone(), stamp()).unwrap();
        assert!(replayed.promoted().is_empty());
        assert!(
            !journal
                .iter()
                .any(|record| matches!(record, Record::ToolPromoted { .. }))
        );
        let [Effect::Emit(repair)] = effects.as_slice() else {
            panic!("replay must return one repair batch")
        };
        assert_eq!(
            results_of(&repair.records, "call"),
            vec![(true, text_part(LOST))]
        );
        assert!(matches!(
            repair.records.as_slice(),
            [
                Record::ToolResult(_),
                Record::TurnEnd {
                    stop: TurnEndStop::Aborted,
                    ..
                },
                Record::Boot { .. },
            ]
        ));

        journal.extend(repair.records.iter().cloned());
        let (_, again) = Session::replay(journal.clone(), stamp()).unwrap();
        let [Effect::Emit(second)] = again.as_slice() else {
            panic!("replay must return one repair batch")
        };
        assert!(results_of(&second.records, "call").is_empty());
        assert_eq!(results_of(&journal, "call").len(), 1);
    }

    #[test]
    fn cloned_entries_without_turn_records_replay() {
        let mut session = session();
        let mut journal = Vec::new();
        let turn = open_turn(&mut session, &mut journal);
        append_emitted(
            &stream_result(
                &mut session,
                turn,
                inference(Stop::EndTurn, &[("call", "read_file")], 10),
            ),
            &mut journal,
        );
        let header = crate::journal::Header {
            id: crate::id::SessionId::parse("01890f47-36b0-7cc4-8000-000000000001").unwrap(),
            at: stamp(),
            workspace: crate::workspace::Workspace::new(PathBuf::from("/")).unwrap(),
            product: crate::journal::Product::Dal,
            from: None,
        };
        let open = crate::journal::branch(
            &journal,
            session.tree.leaf,
            crate::journal::BranchMode::Clone,
            &header,
        )
        .unwrap();
        assert!(
            !open
                .records
                .iter()
                .any(|record| matches!(record, Record::TurnStart { .. }))
        );
        let (_, effects) = Session::replay(open.records, stamp()).unwrap();
        let [Effect::Emit(repair)] = effects.as_slice() else {
            panic!("replay must return one repair batch")
        };
        assert!(matches!(repair.records.as_slice(), [Record::Boot { .. }]));

        append_emitted(
            &send(
                &mut session,
                Event::Resolved {
                    turn,
                    calls: vec![resolved_call(
                        "call",
                        "read_file",
                        false,
                        Ok(ToolClass::Read),
                    )],
                    answerer_attached: false,
                },
            )
            .unwrap(),
            &mut journal,
        );
        append_emitted(
            &send(
                &mut session,
                Event::CallStarted {
                    turn,
                    call: CallId::new("call"),
                },
            )
            .unwrap(),
            &mut journal,
        );
        append_emitted(
            &send(
                &mut session,
                Event::Settled {
                    turn,
                    call: CallId::new("call"),
                    outcome: SettledOutcome::Ok {
                        text: "read".into(),
                    },
                },
            )
            .unwrap(),
            &mut journal,
        );
        let settled = crate::journal::branch(
            &journal,
            session.tree.leaf,
            crate::journal::BranchMode::Clone,
            &header,
        )
        .unwrap();
        assert_eq!(results_of(&settled.records, "call").len(), 1);
        Session::replay(settled.records, stamp()).unwrap();
    }

    #[test]
    fn replay_restores_wake_count() {
        let (mut session, _) = Session::replay(
            [Record::WakeAttempt {
                at: stamp(),
                turn: id(1),
                count: 7,
            }],
            stamp(),
        )
        .unwrap();
        assert_eq!(session.wake_run(), 7);
        for _ in 0..13 {
            let mut out = Vec::new();
            session
                .step(
                    Event::Wake {
                        text: "wake".into(),
                        sources: Box::new([]),
                        jobs: Box::new([]),
                    },
                    stamp(),
                    &mut out,
                )
                .unwrap();
            let turn = match session.phase() {
                Phase::Opening { turn, .. } => *turn,
                _ => panic!("wake not opening"),
            };
            session.step(guard(turn), stamp(), &mut out).unwrap();
            session
                .step(
                    Event::Cancel {
                        scope: CancelScope::Turn(turn),
                        partial: None,
                    },
                    stamp(),
                    &mut out,
                )
                .unwrap();
        }
        assert_eq!(session.wake_run(), 20);
        assert!(matches!(
            send(
                &mut session,
                Event::Wake {
                    text: "refused".into(),
                    sources: Box::new([]),
                    jobs: Box::new([])
                }
            ),
            Err(Rejection::Denied {
                reason: crate::approval::DenyReason::WakeLimit
            })
        ));
    }

    proptest::proptest! {
        #[test]
        fn settlement_invariants_hold_on_random_sequences(actions in proptest::collection::vec(0_u8..3, 0..40)) {
            let mut session = session();
            let mut journal = Vec::new();
            for (index, action) in actions.into_iter().enumerate() {
                if action == 0 {
                    let before = session.clone();
                    let mut out = vec![Effect::Reply(Ok(Reply::Done))];
                    let original = out.clone();
                    let rejected = session.step(Event::Command {
                        cmd: Command::Steer { turn: id(99), content: Vec::new() },
                        by: client(),
                    }, stamp(), &mut out);
                    proptest::prop_assert!(rejected.is_err());
                    proptest::prop_assert_eq!(&session, &before);
                    proptest::prop_assert_eq!(&out, &original);
                    same_replayed_state(&session, &journal);
                    continue;
                }
                let prompt_out = send(&mut session, prompt(&format!("q{index}"))).unwrap();
                append_emitted(&prompt_out, &mut journal);
                let turn = match session.phase() { Phase::Opening { turn, .. } => *turn, phase => panic!("expected opening, got {phase:?}") };
                let guard_out = send(&mut session, guard(turn)).unwrap();
                append_emitted(&guard_out, &mut journal);
                let cancel_out = send(&mut session, Event::Cancel { scope: CancelScope::Turn(turn), partial: None }).unwrap();
                append_emitted(&cancel_out, &mut journal);
                proptest::prop_assert!(matches!(session.phase(), Phase::Idle));
                let starts = journal.iter().filter(|record| matches!(record, Record::TurnStart { .. })).count();
                let ends = journal.iter().filter(|record| matches!(record, Record::TurnEnd { .. })).count();
                proptest::prop_assert_eq!(starts, ends);
                same_replayed_state(&session, &journal);
            }
        }
    }

    #[test]
    fn interrupted_stream_records_partial_assistant_and_call_result_once() {
        let mut session = session();
        let turn = begin(&mut session);
        send(
            &mut session,
            Event::RequestStarted {
                turn,
                model: route(),
                family: Family::Chat,
            },
        )
        .unwrap();
        let call = CallId::new("partial-call");
        let partial = PartialResponse {
            content: vec![
                Block::Text {
                    text: "partial answer".into(),
                },
                Block::ToolCall {
                    id: call.clone(),
                    name: "read_file".into(),
                    input: RawJson::parse("{}").unwrap(),
                },
            ],
            usage: usage(7),
        };
        let out = send(
            &mut session,
            Event::Cancel {
                scope: CancelScope::Turn(turn),
                partial: Some(partial),
            },
        )
        .unwrap();
        let mut records = Vec::new();
        append_emitted(&out, &mut records);
        assert_eq!(
            records
                .iter()
                .filter(|record| matches!(record, Record::Assistant(_)))
                .count(),
            1
        );
        assert_eq!(records.iter().filter(|record| matches!(record, Record::ToolResult(entry) if matches!(&entry.kind, EntryKind::ToolResult { call: result, .. } if result == &call))).count(), 1);
        assert!(matches!(
            records.last(),
            Some(Record::TurnEnd {
                stop: TurnEndStop::Cancelled,
                ..
            })
        ));

        let late = send(
            &mut session,
            Event::StreamEnded {
                turn,
                model: route(),
                family: Family::Chat,
                result: Err(InferFailure::Cancelled),
                partial: None,
            },
        )
        .unwrap();
        assert!(late.is_empty());
    }

    #[test]
    fn boundary_waits_for_resolution_before_resuming_model() {
        let mut session = session();
        let turn = begin(&mut session);
        stream_result(
            &mut session,
            turn,
            inference(Stop::EndTurn, &[("call", "read_file")], 10),
        );
        assert!(matches!(
            session.phase(),
            Phase::Running {
                stage: TurnStage::Resolving { .. },
                ..
            }
        ));

        let before = session.clone();
        let early = send(&mut session, Event::Boundary { turn }).unwrap();
        assert_eq!(session, before);
        assert!(
            !early
                .iter()
                .any(|effect| matches!(effect, Effect::Infer(_) | Effect::Dispatch { .. }))
        );

        let resolved = send(
            &mut session,
            Event::Resolved {
                turn,
                calls: vec![ResolvedCall {
                    call: CallId::new("call"),
                    name: name("read_file"),
                    promoted: false,
                    result: Ok(ToolClass::Read),
                }],
                answerer_attached: false,
            },
        )
        .unwrap();
        assert!(resolved.iter().any(|effect| matches!(effect, Effect::Dispatch { turn: dispatched, .. } if *dispatched == turn)));
        assert!(
            !resolved
                .iter()
                .any(|effect| matches!(effect, Effect::Infer(_)))
        );
    }

    #[test]
    fn queued_wakes_keep_unique_turns_and_replay_their_count() {
        let mut session = session();
        let mut journal = Vec::new();
        let prompt_out = send(&mut session, prompt("question")).unwrap();
        append_emitted(&prompt_out, &mut journal);
        let first_turn = match session.phase() {
            Phase::Opening { turn, .. } => *turn,
            phase => panic!("prompt did not open a turn: {phase:?}"),
        };
        let opened = send(&mut session, guard(first_turn)).unwrap();
        append_emitted(&opened, &mut journal);

        for text in ["first wake", "second wake"] {
            let out = send(
                &mut session,
                Event::Wake {
                    text: text.into(),
                    sources: Box::new([]),
                    jobs: Box::new([]),
                },
            )
            .unwrap();
            append_emitted(&out, &mut journal);
        }
        let ended = stream_result(&mut session, first_turn, inference(Stop::EndTurn, &[], 10));
        append_emitted(&ended, &mut journal);
        let boundary = send(&mut session, Event::Boundary { turn: first_turn }).unwrap();
        append_emitted(&boundary, &mut journal);
        let wake_turn = match session.phase() {
            Phase::Settling {
                follow_up: Some((turn, TurnSource::Wake { .. })),
                ..
            } => *turn,
            phase => panic!("first queued wake did not settle next: {phase:?}"),
        };
        let opened = send(&mut session, guard(wake_turn)).unwrap();
        append_emitted(&opened, &mut journal);
        let cancelled = send(
            &mut session,
            Event::Cancel {
                scope: CancelScope::Turn(wake_turn),
                partial: None,
            },
        )
        .unwrap();
        append_emitted(&cancelled, &mut journal);

        let attempts = journal
            .iter()
            .filter_map(|record| match record {
                Record::WakeAttempt { turn, count, .. } => Some((*turn, *count)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(attempts, vec![(id(2), 1), (id(3), 2)]);
        let starts = journal
            .iter()
            .filter_map(|record| match record {
                Record::TurnStart { turn, .. } => Some(*turn),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(starts, vec![id(1), id(2)]);
        assert_eq!(session.next_turn, Some(id(4)));
        assert_eq!(session.wake_run(), 2);
        same_replayed_state(&session, &journal);
    }

    #[test]
    fn replay_accepts_legacy_job_start_without_kind() {
        let job = JobId::parse("01890f47-36b0-7cc4-8000-000000000002").unwrap();
        let records = [Record::Job {
            at: stamp(),
            job,
            event: JobEvent::Started { kind: None },
        }];
        let (_, effects) = Session::replay(records, stamp()).unwrap();
        let [Effect::Emit(emit)] = effects.as_slice() else {
            panic!("replay must return its boot and orphan repair records");
        };
        assert!(matches!(emit.records.as_slice(), [Record::Job {
            job: orphaned,
            event: JobEvent::Orphaned,
            ..
        }, Record::Boot { .. }] if *orphaned == job));
    }

    #[test]
    fn blob_journal_parts_preserve_stored_lengths() {
        let blob = crate::BlobId::from_bytes(b"stored payload");
        let text = part_to_journal(&Part::Blob {
            blob_id: blob,
            mime: "text/plain".into(),
            bytes: 13,
        });
        assert!(matches!(text, JournalPart::TextBlob { bytes: 13, .. }));
        let image = part_to_journal(&Part::Blob {
            blob_id: blob,
            mime: "image/png".into(),
            bytes: 29,
        });
        assert!(matches!(image, JournalPart::ImageBlob { bytes: 29, .. }));
        let other = part_to_journal(&Part::Blob {
            blob_id: blob,
            mime: "application/pdf".into(),
            bytes: 41,
        });
        assert!(
            matches!(other, JournalPart::Blob { bytes: 41, mime, .. } if mime.as_ref() == "application/pdf")
        );
    }

    proptest::proptest! {
        #[test]
        fn every_dispatched_call_settles_once_and_replays(count in 1_usize..8) {
            let mut session = session();
            let mut journal = Vec::new();
            let prompt_out = send(&mut session, prompt("question")).unwrap();
            append_emitted(&prompt_out, &mut journal);
            let turn = match session.phase() {
                Phase::Opening { turn, .. } => *turn,
                phase => panic!("prompt did not open a turn: {phase:?}"),
            };
            let opened = send(&mut session, guard(turn)).unwrap();
            append_emitted(&opened, &mut journal);
            let names = (0..count).map(|index| format!("call{index}")).collect::<Vec<_>>();
            let call_defs = names.iter().map(|call| (call.as_str(), "read_file")).collect::<Vec<_>>();
            let response = stream_result(&mut session, turn, inference(Stop::EndTurn, &call_defs, 10));
            append_emitted(&response, &mut journal);
            let resolved = names.iter().map(|call| ResolvedCall {
                call: CallId::new(call.as_str()),
                name: name("read_file"),
                promoted: false,
                result: Ok(ToolClass::Read),
            }).collect::<Vec<_>>();
            let dispatched = send(&mut session, Event::Resolved {
                turn,
                calls: resolved,
                answerer_attached: false,
            }).unwrap();
            append_emitted(&dispatched, &mut journal);
            let dispatched_this_turn = dispatched.iter().any(|effect| matches!(effect, Effect::Dispatch { turn: dispatched_turn, .. } if *dispatched_turn == turn));
            proptest::prop_assert!(dispatched_this_turn);
            let units = dispatched.iter().find_map(|effect| match effect {
                Effect::Dispatch { units, .. } => Some(units.as_slice()),
                _ => None,
            }).unwrap();
            for call in &names {
                let started = send(&mut session, Event::CallStarted {
                    turn,
                    call: CallId::new(call.as_str()),
                }).unwrap();
                append_emitted(&started, &mut journal);
                let duplicate = send(&mut session, Event::CallStarted {
                    turn,
                    call: CallId::new(call.as_str()),
                }).unwrap();
                proptest::prop_assert!(duplicate.is_empty());
            }

            for call in &names {
                let settled = send(&mut session, Event::Settled {
                    turn,
                    call: CallId::new(call.as_str()),
                    outcome: SettledOutcome::Ok { text: "result".into() },
                }).unwrap();
                append_emitted(&settled, &mut journal);
            }
            let final_response = stream_result(&mut session, turn, inference(Stop::EndTurn, &[], 10));
            append_emitted(&final_response, &mut journal);
            let ended = send(&mut session, Event::Boundary { turn }).unwrap();
            append_emitted(&ended, &mut journal);
            proptest::prop_assert!(matches!(session.phase(), Phase::Idle));

            for call in &names {
                let id = CallId::new(call.as_str());
                let planned = units.iter().map(|unit| match unit {
                    Unit::Reads { calls } => calls.iter().filter(|candidate| *candidate == &id).count(),
                    Unit::Serial { call } => usize::from(call == &id),
                }).sum::<usize>();
                let starts = journal.iter().filter(|record| matches!(record, Record::ToolStart { call: started, .. } if started == &id)).count();
                let results = journal.iter().filter(|record| matches!(record, Record::ToolResult(entry) if matches!(&entry.kind, EntryKind::ToolResult { call: result, .. } if result == &id))).count();
                proptest::prop_assert_eq!(starts, 1);
                proptest::prop_assert_eq!(planned, 1);
                proptest::prop_assert_eq!(results, 1);
            }
            let turn_starts = journal.iter().filter(|record| matches!(record, Record::TurnStart { .. })).count();
            let turn_ends = journal.iter().filter(|record| matches!(record, Record::TurnEnd { .. })).count();
            proptest::prop_assert_eq!(turn_starts, turn_ends);
            same_replayed_state(&session, &journal);
        }
    }

    #[test]
    fn receipt_effect_precedes_reply_and_publish() {
        let mut session = session();
        let out = send(
            &mut session,
            Event::Command {
                cmd: Command::SetThinking(ThinkingLevel::High),
                by: client(),
            },
        )
        .unwrap();
        assert!(
            matches!(out.first(), Some(Effect::Emit(emit)) if !emit.records.is_empty() && !emit.updates.is_empty())
        );
        assert!(matches!(out.last(), Some(Effect::Reply(Ok(Reply::Done)))));
    }
}
