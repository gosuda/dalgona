use super::{
    Answer, ApprovalMode, BTreeMap, Block, CallId, CancelScope, ClientId, Command, DecodeError,
    Entry, EntryId, Family, FileChange, HookOutcome, InferFailure, Inference, JobId, JobKind,
    JobOutcome, Mode, ModelRoute, Name, Part, RawJson, Record, Rejection, Reply, Request,
    RequestId, RequestParams, Stop, StreamChannel, StreamEvent, StreamVerdict, ThinkingLevel,
    ToolClass, TurnId, Unit, UpdateKind, Usage,
};
use crate::ext::ToolData;

/// Assistant content retained by the actor for one interrupted stream.
#[derive(Clone, Debug, PartialEq)]
pub struct PartialResponse {
    /// The text, reasoning, and calls received before interruption.
    pub content: Vec<Block>,
    /// Usage reported before interruption.
    pub usage: Usage,
}

pub(super) const MAX_STEERS: usize = 16;
pub(super) const MAX_WAKE_RUN: u32 = 20;
pub(super) const MAX_INTERRUPTS: u32 = 3;
pub(super) const PRODUCT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Compaction trigger and retained-context policy supplied by the actor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompactLimits {
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

/// One extension-owned record committed with a parts compaction.
#[derive(Clone, Debug, PartialEq)]
pub struct CompactionExtRecord {
    /// The extension that owns the record.
    pub ext: Name,
    /// The extension-defined record kind.
    pub kind: Box<str>,
    /// The strict JSON body.
    pub body: RawJson,
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
    /// Replacement parts for a local image-bearing compaction.
    pub parts: Vec<super::JournalPart>,
    /// Token cost of the replacement parts.
    pub parts_tokens: u64,
    /// Extension records committed atomically with the replacement.
    pub letters: Vec<CompactionExtRecord>,
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
        /// Typed data beside the text, such as read and search views (R06).
        data: Option<ToolData>,
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
    /// A non-interrupting stream rule reminder.
    StreamReminder {
        /// Owning turn.
        turn: TurnId,
        /// The rule that matched.
        rule: Box<str>,
        /// The reminder text to persist.
        text: Box<str>,
    },
    /// One synthetic inner inference completed.
    Inferred {
        /// The journal timestamp.
        at: jiff::Timestamp,
        /// Who ran the inner inference.
        who: crate::Owner,
        /// Why it ran.
        purpose: crate::InferredPurpose,
        /// Its normalized usage.
        usage: Usage,
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
        /// Milliseconds the tool ran on a monotonic clock, approval waits
        /// excluded; `None` when the call never ran.
        elapsed_ms: Option<u64>,
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
        compact: CompactLimits,
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
        /// First retained entry selected by the fold for this cut.
        first_kept: Option<EntryId>,
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
        /// Whether the turn ended because the context window overflowed, by
        /// the fold's own classification of the provider reply.
        overflowed: bool,
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
    pub compact: CompactLimits,
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

#[derive(Clone, Debug, PartialEq)]
pub(super) enum QueuedInput {
    Steer(Vec<Part>),
    FollowUp { turn: TurnId, source: TurnSource },
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct QuestionRef {
    pub(super) tool: Option<Box<str>>,
}
/// Usage and file-change totals accumulated for the open turn.
///
/// Mirrors the `dal-store` turn validator: every usage-bearing record the fold
/// journals and every file change in its tool results folds into these totals,
/// and `end_turn` writes them into `Record::TurnEnd`. Keep the summation rules
/// in sync with the validator when either side changes.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct TurnTotals {
    seen: bool,
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
    reasoning_tokens: u64,
    reasoning_known: bool,
    cache_write_tokens: u64,
    cost_usd: f64,
    cost_known: bool,
    changes: Vec<FileChange>,
}

impl Default for TurnTotals {
    fn default() -> Self {
        Self {
            seen: false,
            input_tokens: 0,
            cached_input_tokens: 0,
            output_tokens: 0,
            reasoning_tokens: 0,
            reasoning_known: true,
            cache_write_tokens: 0,
            cost_usd: 0.0,
            cost_known: true,
            changes: Vec::new(),
        }
    }
}

impl TurnTotals {
    /// Drops the open turn's totals; called when a turn opens and closes.
    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }

    /// Folds one usage report into the open turn's totals.
    ///
    /// # Errors
    ///
    /// Returns [`Rejection::Invalid`] when a token total overflows `u64` or
    /// the cost total stops being finite.
    pub(super) fn add_usage(&mut self, usage: Usage) -> Result<(), Rejection> {
        self.seen = true;
        self.input_tokens = checked_add(self.input_tokens, usage.input_tokens)?;
        self.cached_input_tokens =
            checked_add(self.cached_input_tokens, usage.cached_input_tokens)?;
        self.output_tokens = checked_add(self.output_tokens, usage.output_tokens)?;
        match usage.reasoning_tokens {
            Some(reasoning) => {
                self.reasoning_tokens = checked_add(self.reasoning_tokens, reasoning)?;
            }
            None => self.reasoning_known = false,
        }
        self.cache_write_tokens = checked_add(self.cache_write_tokens, usage.cache_write_tokens)?;
        match usage.cost_usd {
            Some(cost) => {
                let total = self.cost_usd + cost;
                if !total.is_finite() {
                    return Err(Rejection::Invalid {
                        reason: "turn usage cost total overflows".into(),
                    });
                }
                self.cost_usd = total;
            }
            None => self.cost_known = false,
        }
        Ok(())
    }

    /// Folds tool-result file changes into the open turn's totals, merging
    /// repeat paths by summing their added and removed lines.
    ///
    /// # Errors
    ///
    /// Returns [`Rejection::Invalid`] when an added or removed total overflows.
    pub(super) fn add_changes(&mut self, changes: &[FileChange]) -> Result<(), Rejection> {
        for change in changes {
            if let Some(known) = self
                .changes
                .iter_mut()
                .find(|known| known.path == change.path)
            {
                known.added = checked_add(known.added, change.added)?;
                known.removed = checked_add(known.removed, change.removed)?;
            } else {
                self.changes.push(change.clone());
            }
        }
        Ok(())
    }

    /// Returns the open turn's summed usage, or `None` when the turn journaled
    /// no usage-bearing record.
    pub(super) fn usage(&self) -> Option<Usage> {
        self.seen.then_some(Usage {
            input_tokens: self.input_tokens,
            cached_input_tokens: self.cached_input_tokens,
            output_tokens: self.output_tokens,
            reasoning_tokens: self.reasoning_known.then_some(self.reasoning_tokens),
            cache_write_tokens: self.cache_write_tokens,
            cost_usd: self.cost_known.then_some(self.cost_usd),
        })
    }

    /// Returns the open turn's merged file changes in first-seen path order.
    pub(super) fn changes(&self) -> Vec<FileChange> {
        self.changes.clone()
    }
}

fn checked_add(total: u64, value: u64) -> Result<u64, Rejection> {
    total.checked_add(value).ok_or_else(|| Rejection::Invalid {
        reason: "turn usage total overflows".into(),
    })
}

/// How far the turn's context-overflow recovery has gone.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum Overflow {
    /// No overflow this turn, or recovered after compaction.
    #[default]
    Clear,
    /// A compaction is under way or done for this turn's overflow.
    Compacted,
    /// The overflow could not be recovered and the turn is ending on it.
    Unrecovered,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct TurnFlags {
    pub(super) interrupts: u32,
    pub(super) overflow: Overflow,
    pub(super) pending_suppressed: Vec<Box<str>>,
    pub(super) suppressed_notice: bool,
    pub(super) end_after_boundary: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct ManualCompletion {
    pub(super) job_settled: bool,
    pub(super) result_settled: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Tree {
    pub(super) entries: BTreeMap<EntryId, Entry>,
    pub(super) leaf: Option<EntryId>,
    pub(super) labels: BTreeMap<EntryId, Box<str>>,
}
