//! One actor per session: fold, journal, publish, and broker wiring.
//!
//! The actor alone mutates the fold. Every `Emit` appends its records before
//! any update publishes; driver-bound effects park on the 256-bound turn
//! channel the turn driver consumes.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::num::NonZeroU64;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use dal_core::ext::{
    BeforeTurn, HookEvent, HookOutcome, HookVerdict, InputEvent, InputVerdict, Origin, Receipt,
    StateError, StateKey, StateNs, StateOp, StateRecord, StreamFireAction,
};
use dal_core::{
    Answer, BlobId, ClientId, Command, CompactLimits, CompactionSummary, Effect, Emit, EntryId,
    EntryKind, Event, Family, InferFailure, Inference, MailMode, ModelRoute, Name, Part,
    PartialResponse, Phase, Policy, PrivateConflict, PrivateEffectId, PrivateLoopId, Record,
    Rejection, Reply, Request, RequestId, ResolutionCause, ResolvedCall, Session, SessionId,
    StreamEvent, Timestamp, TurnId, TurnOp, TurnOpReply, TurnSource, TurnState, UpdateKind,
    Workspace,
};
use dal_store::Journal;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use super::control::ControlCell;
use super::driver::{WatchDelivery, opening_model};
use super::shared::Shared;
use super::status::{STATUS_POLL, sweep};
use super::{ActorRequest, COMMAND_CHANNEL, SessionHandle};
use crate::broker::{AnswerWait, Broker, Resolved, default_timeout};
use crate::error::{ActualTurn, AgentError, ServiceError, TurnPhase, ValidationError, deny_text};
use crate::ext::Services;
use crate::ext::generation::Generation;
use crate::ext::hooks::{
    HookScope, dispatch_before_turn, dispatch_input, hook_fanout, input_failed_notice,
    join_before_turn,
};
use crate::host::default_store::{DefaultUpdate, persist_default};

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

/// Driver ports the turn task consumes.
pub(crate) struct DriverPorts {
    /// Effect batches for the driver, with the ask waiters they may await.
    pub(crate) ops_rx: mpsc::Receiver<TurnBatch>,
    /// The shared turn-bypass cell: the driver binds each turn's stream to the
    /// token `ControlCell::cancel` fires, so a cancel preempts a live `infer`
    /// out-of-band (a queued `Effect::Stop` could never reach it mid-stream).
    pub(crate) control: Arc<Mutex<ControlCell>>,
}

pub(crate) enum WatchOutcome {
    Ended {
        model: ModelRoute,
        family: Family,
        result: Result<Inference, InferFailure>,
    },
    Retry {
        reminder: Option<(Box<str>, Box<str>)>,
    },
    Verdict {
        verdict: dal_core::StreamVerdict,
    },
}

impl WatchOutcome {
    fn into_event(self, turn: TurnId) -> Event {
        match self {
            Self::Ended {
                model,
                family,
                result,
            } => Event::StreamEnded {
                turn,
                model,
                family,
                result,
                partial: None,
            },
            Self::Retry { reminder } => Event::StreamRetry { turn, reminder },
            Self::Verdict { verdict } => Event::StreamVerdict { turn, verdict },
        }
    }
}

/// Work the turn driver reports back to the actor.
pub(crate) enum TurnWork {
    /// A provider stream event.
    Streamed {
        /// The turn producing the event.
        turn: TurnId,
        /// The stream event.
        event: StreamEvent,
    },
    /// A finished stream attempt and its owned rule deliveries.
    Watched {
        turn: TurnId,
        event: WatchOutcome,
        delivery: WatchDelivery,
        reply: oneshot::Sender<bool>,
    },
    /// A legacy verdict suppressed without discarding the active stream.
    WatcherVerdict {
        turn: TurnId,
        verdict: dal_core::StreamVerdict,
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
        /// Milliseconds the tool ran, approval waits excluded; `None` when
        /// the call never ran.
        elapsed_ms: Option<u64>,
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
    /// The session data-plane cell backing scripted hook entries;
    /// filled right after the actor spawns.
    pub(crate) backend: Arc<std::sync::OnceLock<Arc<crate::session::backend::Backend>>>,
    /// The owner for background work that must stop before the journal closes.
    pub(crate) tasks: crate::session::tasks::SessionTasks,
    /// The default reasoning level this session opened under.
    pub(crate) thinking: dal_core::ThinkingLevel,
}

/// Lifecycle latches for one session actor.
///
/// The four latches are independent, not one mode: the opening lane, the
/// hook reentrancy guard, the pending-compaction marker, and the shutdown
/// latch. Each transition has a name instead of a raw field assignment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "four independent latches; no combination is invalid"
)]
struct ActorFlags {
    opening_closed: bool,
    hook_waiting: bool,
    pending_compact: bool,
    closing: bool,
}
impl ActorFlags {
    fn is_opening_closed(self) -> bool {
        self.opening_closed
    }
    fn mark_opening_closed(&mut self) {
        self.opening_closed = true;
    }
    fn is_hook_waiting(self) -> bool {
        self.hook_waiting
    }
    fn enter_hook_wait(&mut self) {
        self.hook_waiting = true;
    }
    fn exit_hook_wait(&mut self) {
        self.hook_waiting = false;
    }
    fn begin_hook_suspension(&mut self) -> bool {
        std::mem::replace(&mut self.hook_waiting, true)
    }
    fn is_closing(self) -> bool {
        self.closing
    }
    fn mark_closing(&mut self) {
        self.closing = true;
    }
    fn is_pending_compact(self) -> bool {
        self.pending_compact
    }
    fn note_submit(&mut self, command: &Command) {
        self.pending_compact = matches!(command, Command::Compact { .. });
    }
    fn clear_pending_compact(&mut self) {
        self.pending_compact = false;
    }
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
    /// The session's compare-and-swap state, loaded lazily from its
    /// sidecar file on the first state operation.
    state: Option<StateMap>,
    /// The last revision the state owner minted; never reused.
    state_rev: u64,
    /// The child depth, zero for top-level sessions.
    depth: u32,
    parent: Option<SessionId>,
    rx: mpsc::Receiver<ActorRequest>,
    opening_rx: mpsc::Receiver<ActorRequest>,
    flags: ActorFlags,
    resolutions: Arc<crate::session::ResolutionInbox>,
    driver_tx: mpsc::Sender<TurnBatch>,
    host: Arc<crate::host::HostState>,
    pending_commands: VecDeque<ReplyTx>,
    settling: Option<TurnId>,
    /// Broker resolutions held until their journal emit has been observed.
    pending_releases: Vec<Resolved>,
    broken: Option<Box<str>>,
    /// Capability-scoped services for hook dispatch, attached after spawn.
    services: Option<Arc<dyn Services>>,

    /// The session data-plane cell backing scripted hook entries; filled
    /// right after the actor spawns.
    backend: Arc<std::sync::OnceLock<Arc<crate::session::backend::Backend>>>,
    tasks: crate::session::tasks::SessionTasks,
    /// The default reasoning level this session opened under; a default
    /// saved later reaches only sessions opened after it.
    thinking: dal_core::ThinkingLevel,
}

type ReplyTx = oneshot::Sender<Result<Reply, AgentError>>;

/// The sidecar file holding the session's state map.
const STATE_SIDECAR: &str = "state";

/// The in-memory state map: one value-or-tombstone per (namespace, key).
type StateMap = HashMap<(Box<str>, StateKey), (Option<dal_core::RawJson>, u64)>;

/// The current sidecar format version; unknown versions refuse to load
/// rather than silently mis-decoding a newer file (strict stored formats).
const STATE_FILE_FORMAT: u8 = 1;

/// The serialized state sidecar file.
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct StateFile {
    /// The stored format version.
    format: u8,
    /// The last minted revision; never reused.
    revisions: u64,
    /// One entry per key, values and tombstones alike.
    entries: Vec<StateFileEntry>,
}

impl Default for StateFile {
    fn default() -> Self {
        Self {
            format: STATE_FILE_FORMAT,
            revisions: 0,
            entries: Vec::new(),
        }
    }
}

/// One serialized (namespace, key) record.
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct StateFileEntry {
    /// The namespace label.
    ns: Box<str>,
    /// The key inside the namespace; validated again on load.
    key: Box<str>,
    /// Whether this entry stores a value (false marks a tombstone).
    /// `Option<RawJson>` alone cannot hold an explicit JSON `null`:
    /// serde decodes `null` as `None`, the tombstone shape.
    present: bool,
    /// The stored value; `null` or absent when `present` is false.
    value: Option<dal_core::RawJson>,
    /// The revision minted when this entry last changed.
    revision: u64,
}

/// Labels one namespace for map and file keys.
fn ns_label(ns: &StateNs) -> Box<str> {
    match ns {
        StateNs::Eval => "eval".into(),
        StateNs::Plugin {
            origin,
            plugin,
            version,
        } => {
            let origin = match origin {
                Origin::Builtin => "builtin",
                Origin::Bundled => "bundled",
                Origin::User => "user",
                _ => "other",
            };
            format!("plugin:{origin}:{}:{version}", plugin.as_str()).into()
        }
    }
}

/// Wraps a counter value as a revision.
fn revision(at: u64) -> dal_core::ext::Revision {
    dal_core::ext::Revision::new(NonZeroU64::new(at).unwrap_or(NonZeroU64::MIN))
}

/// Loads the durable state map from one sidecar read.
///
/// A missing file is an empty map. A read failure, malformed JSON, or an
/// entry whose stored key no longer parses is surfaced as
/// [`StateError::Unavailable`]: resetting to an empty map here would let
/// the next write destroy every previously stored key.
fn load_state_map(
    read: Result<Vec<u8>, dal_store::StoreError>,
) -> Result<(StateMap, u64), StateError> {
    let file = match read {
        Ok(bytes) => {
            sonic_rs::from_slice::<StateFile>(&bytes).map_err(|_| StateError::Unavailable)?
        }
        Err(dal_store::StoreError::NotFound { .. }) => StateFile::default(),
        Err(_) => return Err(StateError::Unavailable),
    };
    if file.format != STATE_FILE_FORMAT {
        return Err(StateError::Unavailable);
    }
    let mut map = StateMap::new();
    let mut seen_revisions = std::collections::HashSet::with_capacity(file.entries.len());
    for entry in file.entries {
        // Reject a damaged file rather than publish it: a zero revision
        // mints a key that can never satisfy its own CAS, a revision ahead
        // of the counter can be re-minted, a duplicate slot makes the
        // stored order load-bearing, and a reused revision lets one key's
        // token satisfy another key's CAS.
        if entry.revision == 0
            || entry.revision > file.revisions
            || !seen_revisions.insert(entry.revision)
        {
            return Err(StateError::Unavailable);
        }
        let key = StateKey::parse(&entry.key).map_err(|_| StateError::Unavailable)?;
        let value = match (entry.present, entry.value) {
            (true, value) => Some(value.unwrap_or_else(dal_core::RawJson::null)),
            (false, None) => None,
            // A tombstone carrying a payload is not a file this code wrote.
            (false, Some(_)) => return Err(StateError::Unavailable),
        };
        if map
            .insert((entry.ns, key), (value, entry.revision))
            .is_some()
        {
            return Err(StateError::Unavailable);
        }
    }
    Ok((map, file.revisions))
}

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

/// Maximum number of waiting messages per recipient.
const MAILBOX_WAITING_LIMIT: usize = 100;

/// Dedicated bound for opening-safe service ingress.
const OPENING_CHANNEL: usize = COMMAND_CHANNEL;

/// Spawns one session actor; the turn driver consumes `DriverPorts`.
pub(crate) fn spawn(deps: ActorDeps) -> (SessionHandle, DriverPorts, tokio::task::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(COMMAND_CHANNEL);
    let (opening_tx, opening_rx) = mpsc::channel(OPENING_CHANNEL);
    // The inbox owns the broker so every claim does its compare-and-swap
    // under the inbox lock; the cell needs the inbox for classification, so
    // both are built here before the actor takes the broker.
    let resolutions = Arc::new(crate::session::ResolutionInbox::new(Arc::clone(
        &deps.broker,
    )));
    let control = Arc::new(Mutex::new(ControlCell::new(Arc::clone(&resolutions))));
    let (driver_tx, ops_rx) = mpsc::channel(DRIVER_CHANNEL);
    let actor = Actor {
        session: deps.session,
        journal: deps.journal,
        fold: deps.fold,
        broker: deps.broker,
        shared: deps.shared,
        control: Arc::clone(&control),
        workspace: deps.workspace,
        state: None,
        state_rev: 0,
        depth: deps.depth,
        parent: deps.parent,
        rx,
        opening_rx,
        flags: ActorFlags::default(),
        resolutions: Arc::clone(&resolutions),
        driver_tx,
        host: deps.host,
        pending_commands: VecDeque::new(),
        settling: None,
        pending_releases: Vec::new(),
        broken: None,
        services: None,
        backend: deps.backend,
        tasks: deps.tasks,
        thinking: deps.thinking,
    };
    let handle = SessionHandle::new(
        deps.session,
        tx,
        opening_tx,
        Arc::clone(&actor.control),
        resolutions,
    );
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

/// Classifies the fold phase into the opening awaiting hooks: the turn, its
/// content, and whether the input hooks run before `before_turn`.
///
/// Prompt and follow-up openings run both hook folds; wake openings skip the
/// input fold. Settling phases contribute only when a follow-up is queued;
/// every other phase has no opening to drive.
fn opening_request(phase: &Phase) -> Option<(TurnId, Vec<Part>, bool)> {
    match phase {
        Phase::Opening { turn, source, .. } => Some((
            *turn,
            opening_content(source),
            !matches!(source, TurnSource::Wake { .. }),
        )),
        Phase::Settling {
            follow_up: Some((next, source)),
            ..
        } => Some((
            *next,
            opening_content(source),
            !matches!(source, TurnSource::Wake { .. }),
        )),
        _ => None,
    }
}

/// What [`Actor::classify`] produced from one work report.
#[derive(Default)]
struct FoldOutcome {
    /// The fold event the work maps to, when it maps directly.
    event: Option<Event>,
    /// Read views the settled call delivered to the model.
    delivered: Vec<dal_core::ext::ReadView>,
    /// A cancelled turn whose end update waits for its journal receipts.
    cancelled_end: Option<TurnId>,
    /// A just-folded `RequestOpened` whose parked turn-cancel claim hands
    /// over only after the fold accepted the event; `None` for every other
    /// work report. A rejected `RequestOpened` leaves the claim parked.
    opened: Option<RequestId>,
}

struct WatchCommit {
    emit: Emit,
    effects: Vec<Effect>,
    latches: Vec<(usize, Option<EntryId>)>,
}

fn watch_notice(turn: TurnId, event: &Event, delivery: &WatchDelivery) -> Option<dal_core::Notice> {
    let (Event::StreamRetry {
        reminder: Some((rule, _)),
        ..
    }
    | Event::StreamVerdict {
        verdict: dal_core::StreamVerdict::Interrupt { rule, .. },
        ..
    }) = event
    else {
        return None;
    };
    let owner = delivery
        .stopped_by
        .and_then(|index| delivery.watchers[index].owner.as_deref());
    let text = match owner {
        Some(name) => format!(
            "Extension \"{name}\" requested an output stream interrupt for rule \"{rule}\"."
        ),
        None => format!("A stream watcher requested an output interrupt for rule \"{rule}\"."),
    };
    Some(dal_core::Notice {
        turn: Some(turn),
        kind: "watcher.verdict".into(),
        text: text.into(),
    })
}

fn join_watch_records(
    delivery: &WatchDelivery,
    emit: &mut Emit,
    at: Timestamp,
) -> Result<Vec<(usize, Option<EntryId>)>, AgentError> {
    let invalid = || {
        AgentError::Invalid(ValidationError::new(
            "Rule delivery did not match the allocated reminder.",
        ))
    };
    let mut records = std::mem::take(&mut emit.records).into_iter().peekable();
    while let Some(record) =
        records.next_if(|record| !matches!(record, Record::Reminder(_) | Record::TurnEnd { .. }))
    {
        emit.records.push(record);
    }
    let mut latches = Vec::new();
    let reminders_before_interrupt = delivery
        .fires
        .iter()
        .enumerate()
        .filter(|(index, _)| Some(*index) != delivery.interrupt)
        .chain(
            delivery
                .interrupt
                .map(|index| (index, &delivery.fires[index])),
        );
    for (index, (owner, fire)) in reminders_before_interrupt {
        if delivery.allowed == Some(index) {
            latches.push((index, None));
            continue;
        }
        if fire.action == StreamFireAction::Interrupt && delivery.interrupt != Some(index) {
            continue;
        }
        let entry = if fire.action == StreamFireAction::Report {
            None
        } else {
            let Some(Record::Reminder(entry)) = records.next() else {
                return Err(invalid());
            };
            if !matches!(&entry.kind, EntryKind::Reminder { source, .. } if source.strip_prefix("rule:") == Some(fire.rule.as_ref()))
            {
                return Err(invalid());
            }
            let id = entry.id;
            emit.records.push(Record::Reminder(entry));
            let Some(record) = records.next() else {
                return Err(invalid());
            };
            if !matches!(&record, Record::RuleFired { entry, .. } if *entry == id) {
                return Err(invalid());
            }
            emit.records.push(record);
            Some(id)
        };
        let owned = &delivery.watchers[*owner];
        let record = owned.watcher.record_body(fire.index, at, entry);
        if entry.is_some() || record.is_some() {
            latches.push((index, entry));
        }
        if let Some(record) = record {
            let Some(ext) = &owned.owner else {
                return Err(AgentError::Invalid(ValidationError::new(
                    "A rule record has no owning extension.",
                )));
            };
            emit.records.push(Record::Ext {
                at,
                ext: ext.clone(),
                kind: record.kind,
                body: record.body,
            });
        }
    }
    emit.records.extend(records);
    Ok(latches)
}

impl Actor {
    async fn into_run(mut self, mut pending: Vec<Effect>) {
        let compact = {
            let generation = self.host.shared.generation.borrow();
            super::driver::compact_limits(self.host.shared.config.compact_ratio(), &generation)
        };
        self.step(
            Event::Limits {
                window: 0,
                max_steps: 0,
                compact,
            },
            &mut pending,
        );
        if !pending.is_empty() {
            let batch = self.batch(pending, Vec::new());
            if self.driver_tx.send(batch).await.is_err() {
                self.broken = Some("the turn driver is gone.".into());
            }
        }
        self.shared.sync_ext(&self.fold);
        self.poll_status();
        // `run` is a large future on Windows; boxing keeps `into_run` under
        // the `large_futures` limit.
        Box::pin(self.run()).await;
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
            model: self.requested_route(),
            family: self.fold.active_family(),
        }
    }

    /// The route the next request names: the one the caller selected, else
    /// the route of the live turn. `None` leaves the host default.
    fn requested_route(&self) -> Option<ModelRoute> {
        self.fold
            .requested_model()
            .or_else(|| self.fold.active_model())
            .cloned()
    }

    /// Reports whether a frontend can answer approval questions.
    fn answerer_attached(&self) -> bool {
        self.shared.attached_approval()
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
                request = self.rx.recv() => {
                    let Some(request) = request else { break };
                    self.on_request(request).await;
                    if self.flags.is_closing() {
                        break;
                    }
                    self.drain_queued().await;
                    if self.flags.is_closing() {
                        break;
                    }
                    self.drain_resolutions().await;
                    if self.flags.is_closing() {
                        break;
                    }
                }
                request = self.opening_rx.recv(), if !self.flags.is_opening_closed() => {
                    let Some(request) = request else {
                        self.flags.mark_opening_closed();
                        continue;
                    };
                    self.on_request(request).await;
                    if self.flags.is_closing() {
                        break;
                    }
                    self.drain_queued().await;
                    if self.flags.is_closing() {
                        break;
                    }
                    self.drain_resolutions().await;
                    if self.flags.is_closing() {
                        break;
                    }
                }
                () = self.resolutions.wait() => {
                    // Each captured resolution drains its preceding service
                    // requests before it is folded by drain_resolutions.
                    self.drain_queued().await;
                    if self.flags.is_closing() {
                        break;
                    }
                    self.drain_resolutions().await;
                    if self.flags.is_closing() {
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
        self.drain_resolutions().await;
        // Run exit: fence the inbox and release every remainder
        // waiter-only — folded leftovers were already settled, and parked
        // claims of never-folded requests never publish. No producer can
        // hang on a waiter the gone actor would never release.
        for resolved in self.resolutions.close() {
            self.broker.cancel(&resolved);
        }
        for parked in self.broker.parked() {
            self.broker.cancel(&parked);
        }
    }

    /// Folds mailbox requests that arrived before a woken inbox resolution.
    /// Bounded: one full mailbox plus arrivals during the drain.
    async fn drain_queued(&mut self) {
        for _ in 0..super::COMMAND_CHANNEL * 4 {
            let Ok(request) = self.rx.try_recv() else {
                break;
            };
            self.on_request(request).await;
            if self.flags.is_closing() {
                break;
            }
        }
    }

    /// Polls one opening hook while serving only the bounded service lane.
    /// Commands, driver lifecycle work, and shutdown keep their normal FIFO.
    /// Service folds may commit records or queue future input; only this
    /// suspended dispatcher may advance the opening.
    async fn pump_opening<F, T>(&mut self, future: F) -> T
    where
        F: Future<Output = T>,
    {
        tokio::pin!(future);
        self.flags.enter_hook_wait();
        let output = loop {
            tokio::select! {
                biased;
                output = &mut future => break output,
                request = self.opening_rx.recv(), if !self.flags.is_opening_closed() => {
                    match request {
                        Some(request) => self.on_request(request).await,
                        None => self.flags.mark_opening_closed(),
                    }
                }
                () = self.resolutions.wait() => self.drain_resolutions().await,
            }
        };
        self.flags.exit_hook_wait();
        output
    }

    /// Drains the bounded snapshot preceding a captured resolution. Taking
    /// the resolution first makes its Asked causally older than the snapshot;
    /// arrivals during this drain cannot extend it indefinitely.
    async fn drain_service_queued(&mut self) {
        if self.flags.is_closing() {
            return;
        }
        for _ in 0..self.opening_rx.len() {
            let Ok(request) = self.opening_rx.try_recv() else {
                break;
            };
            self.on_request(request).await;
        }
    }

    /// Journals every inbox resolution in order and marks each id settled,
    /// so the services slot reopens only after the terminal record landed.
    async fn drain_resolutions(&mut self) {
        let already_waiting = self.flags.begin_hook_suspension();
        while let Some(resolved) = self.resolutions.take() {
            self.drain_service_queued().await;
            let id = resolved.request.id;
            self.on_work(TurnWork::Answered { resolved }).await;
            // Every drained item settles its id, including unknown-question
            // fail-closed claims — that is what frees services admission.
            self.resolutions.mark_settled(id);
        }
        if !already_waiting {
            self.flags.exit_hook_wait();
        }
        if !already_waiting && !self.flags.is_closing() {
            // A queued wake in the causal snapshot may now open, after
            // the preceding withdrawal is durable and its slot reusable.
            Box::pin(self.execute(Vec::new(), None)).await;
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
                self.on_sidecar(op).await;
            }
            ActorRequest::State { req } => {
                self.on_state(req).await;
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
            ActorRequest::PrivateLoop { req } => {
                self.on_private_loop(req).await;
            }
            ActorRequest::PrivateStart { req } => {
                self.on_private_start(req).await;
            }
            ActorRequest::PrivateEnd { req } => {
                self.on_private_end(req).await;
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
        // borrows across store awaits, so they are `!Send`. Drive them on a
        // scoped thread under a fresh current-thread runtime: the future borrows `&mut Journal`
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
    /// authority, so hook-phase entries stay fail-closed.
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
    async fn on_work(&mut self, work: TurnWork) {
        let work = match work {
            TurnWork::Watched {
                turn,
                event,
                mut delivery,
                reply,
            } => {
                let result = self
                    .settle_watch(turn, event.into_event(turn), &mut delivery)
                    .await;
                if let Err(error) = &result {
                    self.broken = Some(error.to_string().into());
                    self.publish(UpdateKind::Notice(dal_core::Notice {
                        turn: Some(turn),
                        kind: "driver.failed".into(),
                        text: error.to_string().into(),
                    }));
                }
                let _ = reply.send(matches!(result, Ok(true)));
                return;
            }
            work => work,
        };
        let mut queue: VecDeque<Effect> = VecDeque::new();
        let mut effects = Vec::new();
        let mut outcome = self.classify(work, &mut queue, &mut effects);
        if let Some(event) = outcome.event {
            match outcome.opened.take() {
                Some(opened) => self.fold_opened(event, opened, &mut queue, &mut effects),
                None => self.step(event, &mut effects),
            }
        }
        queue.extend(effects);
        let durable = self.execute(queue.into(), None).await;
        if durable {
            if outcome.cancelled_end.is_some() {
                Box::pin(self.drain_resolutions()).await;
            }
            if let Some(turn) = outcome.cancelled_end {
                self.publish(UpdateKind::TurnEnded {
                    turn,
                    stop: dal_core::Stop::Cancelled,
                });
            }
        }
        self.mark_delivered(&outcome.delivered);
    }

    async fn settle_watch(
        &mut self,
        turn: TurnId,
        event: Event,
        delivery: &mut WatchDelivery,
    ) -> Result<bool, AgentError> {
        let active = self
            .control()
            .token_for(turn)
            .is_some_and(|token| !token.is_cancelled());
        if !active
            || !matches!(
                self.fold.phase(),
                Phase::Running {
                    stage: dal_core::TurnStage::Streaming { .. },
                    ..
                }
            )
        {
            return Ok(false);
        }
        let notice = watch_notice(turn, &event, delivery);
        let mut anchor = self.fold.leaf_entry();
        let commit = self.prepare_watch(turn, event, delivery)?;
        let WatchCommit {
            emit,
            effects,
            latches,
        } = commit;
        let mut queue: VecDeque<_> = effects.into();
        let updates = emit.updates;
        let first = self.journal.records().len();
        if !emit.records.is_empty() {
            self.append_observe(
                Emit {
                    records: emit.records,
                    updates: Vec::new(),
                },
                &mut queue,
            )
            .await?;
        }
        for record in &self.journal.records()[first..] {
            match record {
                Record::User(entry)
                | Record::Assistant(entry)
                | Record::ToolResult(entry)
                | Record::Reminder(entry)
                | Record::Model(entry)
                | Record::Thinking(entry)
                | Record::Approval(entry)
                | Record::Mode(entry)
                | Record::Compaction(entry)
                | Record::BranchSummary(entry) => anchor = Some(entry.id),
                Record::Leaf { to, .. } => anchor = *to,
                Record::Ext {
                    ext, kind, body, ..
                } => self.fold.fold_ext_at(anchor, ext, kind, body),
                _ => {}
            }
        }
        self.shared.sync_ext(&self.fold);
        for (index, entry) in latches {
            let (owner, fire) = &delivery.fires[index];
            delivery.watchers[*owner]
                .watcher
                .gate_record(fire.index, entry);
        }
        for update in updates {
            self.observe(update, &mut queue);
        }
        if let Some(notice) = notice {
            self.publish(UpdateKind::Notice(notice));
        }
        for owned in &mut delivery.watchers {
            for notice in owned.watcher.take_notices() {
                self.publish(UpdateKind::Notice(notice));
            }
        }
        Ok(self.execute(queue.into(), None).await)
    }

    fn prepare_watch(
        &mut self,
        turn: TurnId,
        event: Event,
        delivery: &WatchDelivery,
    ) -> Result<WatchCommit, AgentError> {
        let at = Timestamp::now();
        let mut effects = Vec::new();
        let invalid =
            |error: Rejection| AgentError::Invalid(ValidationError::new(error.to_string()));
        for (_, fire) in &delivery.fires {
            if fire.action != StreamFireAction::Reminder {
                continue;
            }
            let text = fire.text.as_ref().ok_or_else(|| {
                AgentError::Invalid(ValidationError::new("A rule reminder has no text."))
            })?;
            self.fold
                .step(
                    Event::StreamReminder {
                        turn,
                        rule: fire.rule.clone(),
                        text: text.clone(),
                    },
                    at,
                    &mut effects,
                )
                .map_err(invalid)?;
        }
        self.fold.step(event, at, &mut effects).map_err(invalid)?;
        let mut commit = WatchCommit {
            emit: Emit::default(),
            effects: Vec::new(),
            latches: Vec::new(),
        };
        for effect in effects {
            match effect {
                Effect::Emit(emit) => {
                    commit.emit.records.extend(emit.records);
                    commit.emit.updates.extend(emit.updates);
                }
                effect => commit.effects.push(effect),
            }
        }
        if commit.effects.iter().any(|effect| {
            matches!(
                effect,
                Effect::Compact { .. }
                    | Effect::Stop {
                        stop: dal_core::Stop::Cancelled,
                        ..
                    }
            )
        }) {
            return Ok(commit);
        }
        commit.latches = join_watch_records(delivery, &mut commit.emit, at)?;
        Ok(commit)
    }

    /// Maps one driver work report to its fold event, queued effects, and read views.
    fn classify(
        &mut self,
        work: TurnWork,
        queue: &mut VecDeque<Effect>,
        effects: &mut Vec<Effect>,
    ) -> FoldOutcome {
        let mut outcome = FoldOutcome::default();
        let event = match work {
            TurnWork::Asked { request } => {
                // The actor folds the event itself below, then hands over
                // the parked claim only when the fold accepted it.
                outcome.opened = Some(request.id);
                Some(Event::RequestOpened { request })
            }
            TurnWork::CallStarted { turn, call } => Some(Event::CallStarted { turn, call }),
            TurnWork::Settled {
                turn,
                call,
                outcome: settled,
                elapsed_ms,
            } => {
                outcome.delivered = read_views(&settled);
                Some(Event::Settled {
                    turn,
                    call,
                    outcome: settled,
                    elapsed_ms,
                })
            }
            TurnWork::Streamed { turn, event } => Some(Event::Stream { turn, event }),
            TurnWork::WatcherVerdict { turn, verdict } => {
                Some(Event::StreamVerdict { turn, verdict })
            }
            TurnWork::StreamEnded {
                turn,
                model,
                family,
                result,
                partial,
            } => Some(Event::StreamEnded {
                turn,
                model,
                family,
                result,
                partial,
            }),
            TurnWork::Resolved {
                turn,
                calls,
                answerer_attached,
            } => Some(Event::Resolved {
                turn,
                calls,
                answerer_attached,
            }),
            TurnWork::Boundary { turn } => Some(Event::Boundary { turn }),
            TurnWork::CompactionSettled {
                turn,
                outcome: settled,
            } => Some(Event::CompactionSettled {
                turn,
                outcome: settled,
            }),
            work => return self.classify_side(work, queue, effects, outcome),
        };
        outcome.event = event;
        outcome
    }

    /// Handles the work reports that act on actor state instead of — or
    /// before — mapping to one fold event.
    fn classify_side(
        &mut self,
        work: TurnWork,
        queue: &mut VecDeque<Effect>,
        effects: &mut Vec<Effect>,
        mut outcome: FoldOutcome,
    ) -> FoldOutcome {
        match work {
            TurnWork::Answered { resolved } => {
                self.queue_resolved_or_cancel(&resolved, queue);
            }
            TurnWork::Cancelled { turn, stop } => {
                if self.settling == Some(turn) {
                    self.settling = None;
                }
                // Cancelled turns defer their `TurnEnded` to this driver
                // confirmation — the kill ladder of an in-flight call has
                // completed by now; other stops already published at `Emit`.
                if stop == dal_core::Stop::Cancelled {
                    // The driver confirmed the cancel, so leftover asks
                    // journal `Cancelled` — the single durable record that
                    // frees every abandoned admission entry on this turn.
                    let resolved = self.broker.resolve_turn(turn, ResolutionCause::Cancelled);
                    for item in resolved {
                        self.queue_resolved_or_cancel(&item, queue);
                    }
                    outcome.cancelled_end = Some(turn);
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
                self.step(
                    Event::Limits {
                        window,
                        max_steps,
                        compact,
                    },
                    effects,
                );
                outcome.event = Some(Event::RequestStarted {
                    turn,
                    model,
                    family,
                });
            }
            TurnWork::CommandDone { result } => {
                if let Some(tx) = self.pending_commands.pop_front() {
                    let _ = tx.send(result);
                }
            }
            _ => unreachable!("classify routes every event-mapped work kind first"),
        }
        outcome
    }

    /// Steps the fold, discarding a rejected event.
    fn step(&mut self, event: Event, effects: &mut Vec<Effect>) {
        let _ = self.fold.step(event, Timestamp::now(), effects);
    }

    /// Steps the fold for one `TurnWork::Asked`, then queues the request's
    /// parked turn-cancel claim only when the fold accepted its
    /// `RequestOpened` — so the claim's `RequestResolved` always follows the
    /// open. A rejected open leaves the claim parked; the run-exit sweep
    /// releases its waiter, so no producer hangs on a fold it missed.
    fn fold_opened(
        &mut self,
        event: Event,
        opened: RequestId,
        queue: &mut VecDeque<Effect>,
        effects: &mut Vec<Effect>,
    ) {
        let accepted = self.fold.step(event, Timestamp::now(), effects).is_ok();
        queue.extend(std::mem::take(effects));
        if !accepted {
            return;
        }
        let Some(claim) = self.broker.report_opened(opened) else {
            return;
        };
        self.queue_resolved_or_cancel(&claim, queue);
    }

    /// Marks complete read views delivered at the shared cursor so later
    /// captures freeze the right cutoff.
    fn mark_delivered(&mut self, delivered: &[dal_core::ext::ReadView]) {
        if delivered.is_empty() {
            return;
        }
        let at = self.shared.cursor();
        let generation = self.host.shared.generation.borrow().clone();
        if let Some(evidence) = generation.evidence() {
            for view in delivered {
                evidence.delivered(view, dal_core::ext::Consumer::Model, at);
            }
        }
    }

    /// Runs the fenced, finite shutdown drain. Classification already
    /// happened in `SessionHandle::begin_shutdown` before any lifecycle
    /// fire, so nothing here classifies; every drain below is a bounded
    /// loop, never a recursive future. `Shutdown` travels the command
    /// lane, so this opening-lane drain cannot re-enter this function.
    async fn on_shutdown(&mut self) {
        // 1. Quiesce session tasks; their drops claim and enqueue.
        self.tasks.stop().await;
        // 2. No nested execute may open a turn past this point.
        let already = self.flags.begin_hook_suspension();
        // 3. Fence the opening lane: new sends fail `SessionClosed`, so
        // producers never publish past this point.
        self.opening_rx.close();
        self.flags.mark_opening_closed();
        // 4. Fold everything enqueued before the fence. `Receiver::close`
        // makes `recv` return `None` once the buffer and the permits
        // reserved before the close are exhausted, so this is finite.
        // Boxed: `on_request` can step `Shutdown`, which awaits this
        // function again, so the unboxed futures would recurse in type.
        while let Some(request) = self.opening_rx.recv().await {
            Box::pin(self.on_request(request)).await;
        }
        // 5. Fence the inbox claims and fold the finite remainder after
        // their `Asked`; each id settles so admission frees without a
        // fabricated record.
        for resolved in self.resolutions.close() {
            let id = resolved.request.id;
            self.on_work(TurnWork::Answered { resolved }).await;
            self.resolutions.mark_settled(id);
        }
        // 6. Overdue slots become `TimedOut` or `NoController`, as on the
        // steady-state tick.
        self.sweep_expiry().await;
        // 7. Claim every still-unresolved open request. Slots the fold
        // registered journal `Withdrawn`; unregistered slots release
        // waiter-only through the fail-closed path, never published.
        let mut queue = VecDeque::new();
        for request in self.broker.open_requests() {
            if let Some(claim) = self.broker.withdraw(request.id) {
                self.queue_resolved_or_cancel(&claim, &mut queue);
            }
        }
        self.execute(queue.into_iter().collect(), None).await;
        // 8. No wake-opening tail: the hook reentrancy guard exits once.
        if !already {
            self.flags.exit_hook_wait();
        }
        // 9. Unclassified lifecycle re-fire, after the drains, while the
        // opening-shutdown marker stays set.
        self.control().cancel_opening();
        // 10. Journal, snapshot, then the closing latch that breaks the run
        // loop.
        let _ = self.journal.close().await;
        self.shared.close_all();
        self.flags.mark_closing();
    }

    async fn on_submit(&mut self, command: Command, by: ClientId, reply: ReplyTx) {
        if let Some(broken) = self.broken.clone() {
            let _ = reply.send(Err(AgentError::Invalid(ValidationError::new(broken))));
            return;
        }
        self.sweep_expiry().await;
        self.flags.note_submit(&command);
        let default = DefaultUpdate::requested(&command);
        let mut effects = Vec::new();
        let stepped = self.fold.step(
            Event::Command { cmd: command, by },
            Timestamp::now(),
            &mut effects,
        );
        match (stepped, default) {
            (Err(rejection), _) => {
                let error = map_rejection(rejection, self.session, &self.control());
                let _ = reply.send(Err(error));
            }
            (Ok(()), None) => {
                let _ = self.execute(effects, Some(reply)).await;
            }
            (Ok(()), Some(default)) => self.execute_then_persist(effects, default, reply).await,
        }
    }

    /// Runs one `Save::SessionAndDefault` setting, then persists it as the
    /// product default only after its settings journal append succeeded.
    ///
    /// A rejected or journal-failed operation writes nothing and forwards its
    /// outcome unchanged. A failed default write keeps the journaled session
    /// as is and replies `PersistenceFailed` with the writer's distinct
    /// wording (remote-gated or `default write failed`), never a success.
    async fn execute_then_persist(
        &mut self,
        effects: Vec<Effect>,
        default: DefaultUpdate,
        reply: ReplyTx,
    ) {
        let (local, mut outcome) = oneshot::channel();
        let _ = self.execute(effects, Some(local)).await;
        // Setting commands settle their reply inside this frame; an absent
        // outcome means the frame dropped it, and dropping `reply` here
        // reports the same closed channel the plain path would.
        let Ok(result) = outcome.try_recv() else {
            return;
        };
        if result.is_err() {
            let _ = reply.send(result);
            return;
        }
        let result = match persist_default(&self.host.shared, &default).await {
            Ok(()) => result,
            Err(error) => Err(AgentError::PersistenceFailed {
                message: error.to_string().into(),
            }),
        };
        let _ = reply.send(result);
    }

    /// Fires the turn token before the fold step touches any channel.
    fn cancel_before_step(&self, command: &Command) {
        if let Command::Cancel { scope } = command
            && let dal_core::CancelScope::Turn(turn) = scope
        {
            let _ = self.control().cancel(*turn);
        }
    }

    async fn execute(&mut self, effects: Vec<Effect>, reply: Option<ReplyTx>) -> bool {
        let mut queue: VecDeque<Effect> = effects.into();
        let mut reply = reply;
        loop {
            let mut driver_effects = Vec::new();
            let mut asks = Vec::new();
            let parked_commands = self.pending_commands.len();
            while let Some(effect) = queue.pop_front() {
                match effect {
                    Effect::Emit(emit) => {
                        if let Err(error) = self.append_observe(emit, &mut queue).await {
                            let message: Box<str> = error.to_string().into();
                            if let Some(tx) = reply.take() {
                                let _ = tx.send(Err(error));
                            }
                            if self.broken.is_none() {
                                self.broken = Some(message);
                            }
                            self.fail_parked(parked_commands);
                            self.cancel_asks(asks);
                            self.discard_pending();
                            return false;
                        }
                    }
                    Effect::Reply(result) => {
                        if let Ok(Reply::Started(job)) = &result
                            && self.flags.is_pending_compact()
                        {
                            self.shared.set_compacting(*job);
                            self.flags.clear_pending_compact();
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
                    if let Ok(sent) =
                        tokio::time::timeout(Duration::from_secs(30), &mut sending).await
                    {
                        break sent;
                    }
                    tracing::warn!(target: "dalgon.agent", session = %session, "driver batch outstanding");
                };
                if sent.is_err() {
                    self.broken = Some("the turn driver is gone.".into());
                }
            }
            if self.flags.is_hook_waiting() {
                // Finish the durable service commit and release its broker
                // waiters below; the suspended hook, not this nested
                // execute, supplies the opening's eventual Guard.
                break;
            }
            let more = {
                // A turn opening has no deadline of its own: report long
                // drives so a stalled hook chain is visible in the log.
                let session = self.session;
                let mut opening = Box::pin(self.drive_opening());
                loop {
                    if let Ok(more) =
                        tokio::time::timeout(Duration::from_secs(30), &mut opening).await
                    {
                        break more;
                    }
                    tracing::warn!(target: "dalgon.agent", session = %session, "turn opening outstanding");
                }
            };
            let Some(more) = more else { break };
            queue.extend(more);
        }
        self.deliver_pending();
        true
    }

    /// Fails command replies parked after `len`: their driver batch never
    /// sent, so no `CommandDone` can ever settle them.
    fn fail_parked(&mut self, len: usize) {
        while self.pending_commands.len() > len {
            let Some(tx) = self.pending_commands.pop_back() else {
                break;
            };
            let message = self
                .broken
                .clone()
                .unwrap_or_else(|| "journal append failed.".into());
            let _ = tx.send(Err(AgentError::Invalid(ValidationError::new(message))));
        }
    }

    /// Cancels ask waiters collected but never handed to the driver: the
    /// session is broken, so their questions can never be answered.
    fn cancel_asks(&self, asks: Vec<(Request, AnswerWait)>) {
        for (request, _) in asks {
            if let Ok(resolved) = self
                .broker
                .answer(request.id, Answer::Cancel, core_client())
            {
                self.broker.cancel(&resolved);
            }
        }
    }
    /// Delivers queued answers after their journal appends landed.
    fn deliver_pending(&mut self) {
        for resolved in self.pending_releases.drain(..) {
            self.broker.release(&resolved);
        }
    }
    /// Cancels queued answers whose journal appends never landed.
    fn discard_pending(&mut self) {
        for resolved in self.pending_releases.drain(..) {
            self.broker.cancel(&resolved);
        }
    }
    /// Bound for one opening-hook drive.
    const OPENING_HOOK_DEADLINE: Duration = Duration::from_secs(600);

    /// Runs the opening hooks for a phase awaiting its first durable record,
    /// then feeds the joined verdict as a Guard event.
    ///
    /// Prompt and follow-up openings run the input hooks before the
    /// `before_turn` hooks; wake openings skip the input hooks. The folded
    /// input content reaches the fold as an `Input` guard ahead of the
    /// `before_turn` guard, so the first `User` record and the provider
    /// request carry the same final parts. A cancelled opening keeps its
    /// admitted content: transforms from an opening that did not finish are
    /// not admitted. A cancel that lands while the hooks run wins: the fold
    /// drops a verdict whose phase moved on.
    async fn drive_opening(&mut self) -> Option<Vec<Effect>> {
        if self.settling.is_some() {
            return None;
        }
        let (turn, content, run_input) = opening_request(self.fold.phase())?;
        let services = self.services.clone()?;
        let cancel = self.control().begin_opening(turn);
        let generation = self.host.shared.generation.borrow().clone();
        let deadline = Instant::now() + Self::OPENING_HOOK_DEADLINE;
        let scope = HookScope {
            services: &services,
            session: self.session,
            parent: self.parent,
            process_env: Arc::clone(&self.host.shared.env),
            cancel: &cancel,
            turn_deadline: deadline,
            script: self.hook_script(),
        };
        let content = if run_input {
            self.fold_input_hooks(&scope, &generation, turn, content)
                .await
        } else {
            content
        };
        let event = BeforeTurn {
            turn,
            text: content_text(&content).into(),
        };
        let mut texts = Vec::new();
        for (index, extension, caller) in hook_fanout(&generation, Some(turn)) {
            let cx = scope.cx(&caller, Some(turn));
            let step = self
                .pump_opening(dispatch_before_turn(
                    extension.name(),
                    &cx,
                    generation.before_turns(index),
                    &event,
                ))
                .await;
            texts.extend(step.texts);
            for notice in step.notices {
                self.notice(turn, "hook.before_turn", &notice);
            }
        }
        if self.control().opening_shutdown(turn) {
            self.control().cancel_opening();
            return None;
        }
        self.control().end_opening(turn);
        // Digests first: `guard_input` takes `content`, and the marker below
        // must name exactly what the admitted entry carries.
        let admitted = Self::admitted_digests(&content);
        let mut effects = Vec::new();
        // Only a guard-admitted transform counts as delivered. Cancelled
        // turns still fall through below so their plain user entry lands
        // (pinned), but they must leave no marker, or every later input
        // would freeze on images no request ever carried. The refuse
        // condition is unchanged: guard refusal still ends the opening here.
        let guard_ok =
            run_input && !cancel.is_cancelled() && self.guard_input(turn, content, &mut effects);
        if run_input && !cancel.is_cancelled() && !guard_ok {
            self.control().cancel_opening();
            return None;
        }
        let add = join_before_turn(&texts).map(Into::into);
        let Ok(outcome) = HookOutcome::new(HookEvent::BeforeTurn, HookVerdict::BeforeTurn(add))
        else {
            return None;
        };
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
            if guard_ok {
                self.mark_admitted_images(&admitted, &mut effects);
            }
            return Some(effects);
        }
        self.control().cancel_opening();
        None
    }

    /// Digests of the image parts in folded input content. Text-only input
    /// yields none, so callers pay nothing unless images are actually
    /// admitted.
    fn admitted_digests(content: &[Part]) -> Vec<BlobId> {
        let mut digests = Vec::new();
        for part in content {
            match part {
                Part::Image { bytes, mime } if mime.starts_with("image/") => {
                    let bytes: &[u8] = bytes;
                    digests.push(BlobId::from_bytes(bytes));
                }
                Part::Blob { blob_id, mime, .. } if mime.starts_with("image/") => {
                    digests.push(*blob_id);
                }
                _ => {}
            }
        }
        digests
    }

    /// Journals a `letter_delivered` marker in the same batch as the admitted
    /// user entry, so the two land atomically: the marker names every image
    /// digest the entry carries, letter or otherwise, and later inputs tell
    /// delivered letters from records that never reached a request.
    ///
    /// The marker is best-effort and purely additive. Cancellations,
    /// shutdowns, and refusals return before this point, so a dropped
    /// transform leaves no marker. When the batch holds no user entry there
    /// is nothing to mark. Body shape contract, mirrored by the strict
    /// reader in extensions/dal-ext/src/letter.rs and pinned by its
    /// `letter_first_prompt` tests: `{"v":1,"digests":[<64-hex>...]}`.
    fn mark_admitted_images(&mut self, digests: &[BlobId], effects: &mut [Effect]) {
        if digests.is_empty() {
            return;
        }
        let Some(body) = Self::delivered_body(digests) else {
            return;
        };
        let Some(emit) = effects.iter_mut().find_map(|effect| match effect {
            Effect::Emit(emit)
                if emit
                    .records
                    .iter()
                    .any(|record| matches!(record, Record::User(_))) =>
            {
                Some(emit)
            }
            _ => None,
        }) else {
            return;
        };
        self.fold.fold_ext("letter", "letter_delivered", &body);
        emit.records.push(Record::Ext {
            at: Timestamp::now(),
            ext: "letter".into(),
            kind: "letter_delivered".into(),
            body,
        });
        self.shared.sync_ext(&self.fold);
    }

    /// Renders one `letter_delivered` body for `digests`. Hex strings keep
    /// the shape valid JSON without an engine-side JSON dependency; the
    /// strict reader side parses each back through `BlobId`.
    fn delivered_body(digests: &[BlobId]) -> Option<dal_core::RawJson> {
        let list = digests
            .iter()
            .map(|digest| format!("\"{digest}\""))
            .collect::<Vec<_>>()
            .join(",");
        let text = format!("{{\"v\":1,\"digests\":[{list}]}}");
        dal_core::RawJson::parse(&text).ok()
    }

    /// Folds every extension's `input` hooks over `content` in canonical
    /// order and returns the final parts. Transforms feed the next hook and
    /// the first stopping extension ends the fold. A share that would leave
    /// non-empty input without parts is refused with one notice and keeps
    /// the running content, as a failed hook does. Every hook sees the model
    /// the request will resolve (`opening_model`), so a hook can adapt its
    /// content to the model's capabilities before anything is journaled.
    async fn fold_input_hooks(
        &mut self,
        scope: &HookScope<'_>,
        generation: &Generation,
        turn: TurnId,
        content: Vec<Part>,
    ) -> Vec<Part> {
        let hooked = hook_fanout(generation, Some(turn))
            .any(|(index, ..)| !generation.inputs(index).is_empty());
        let model = if hooked {
            let shared = Arc::clone(&self.host.shared);
            let requested = self.requested_route();
            // The session's opening thinking, not the live default: a default
            // saved later reaches only sessions opened after it.
            let thinking = self.thinking;
            // Boxed: the resolution future would otherwise push this fold
            // (and its `drive_opening` caller) over the `large_future` cap.
            Box::pin(self.pump_opening(async move {
                opening_model(&shared, requested.as_ref(), thinking).await
            }))
            .await
        } else {
            None
        };
        let mut event = InputEvent { content, model };
        for (index, extension, caller) in hook_fanout(generation, Some(turn)) {
            let cx = scope.cx(&caller, Some(turn));
            let step = self
                .pump_opening(dispatch_input(
                    extension.name(),
                    &cx,
                    matches!(extension.origin(), Origin::Builtin),
                    generation.inputs(index),
                    &event,
                ))
                .await;
            for notice in step.notices {
                self.notice(turn, "hook.input", &notice);
            }
            if step.content.is_empty() && !event.content.is_empty() {
                self.notice(turn, "hook.input", &input_failed_notice(extension.name()));
            } else {
                event.content = step.content;
            }
            if step.stop {
                break;
            }
        }
        event.content
    }

    /// Feeds the folded input content to the fold as one `Input` guard.
    /// Returns `false` when the fold refuses the guard. The guard emits no
    /// record and no effect; the `before_turn` guard that follows journals
    /// the first `User` record with these parts. Empty content is only an
    /// admitted empty follow-up, which no hook replaced: nothing to send.
    fn guard_input(&mut self, turn: TurnId, content: Vec<Part>, effects: &mut Vec<Effect>) -> bool {
        if content.is_empty() {
            return true;
        }
        let verdict = HookVerdict::Input(InputVerdict::Transform(content));
        let Ok(outcome) = HookOutcome::new(HookEvent::Input, verdict) else {
            return false;
        };
        let event = Event::Guard {
            turn,
            call: None,
            extension: None,
            outcome,
        };
        self.fold.step(event, Timestamp::now(), effects).is_ok()
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
        // A `Record::Name` changes the session's visible listing metadata; host
        // subscribers must hear about it once the record is durable.
        let renamed = emit
            .records
            .iter()
            .any(|record| matches!(record, Record::Name { .. }));
        if let Err(error) = self.journal.append(emit.records).await {
            tracing::warn!(target: "dalgon.agent", session = %self.session, error = %error, "journal append failed");
            let message: Box<str> = format!("journal write failed: {error}").into();
            if breaks_session(&error) {
                self.broken = Some(message.clone());
            }
            return Err(AgentError::Invalid(ValidationError::new(message)));
        }
        for kind in updates {
            self.observe(kind, queue);
        }
        if renamed {
            self.host.publish(&crate::host::HostUpdate::SessionChanged {
                session: self.session,
            });
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
                assert!(self.settling.replace(*turn).is_none());
                self.control().end_turn(*turn);
                // The turn's terminal record is already journaled; its
                // backstop cause is `Cancelled` when the driver confirmed a
                // cancel and `Withdrawn` for any other end.
                let cause = if *stop == dal_core::Stop::Cancelled {
                    ResolutionCause::Cancelled
                } else {
                    ResolutionCause::Withdrawn
                };
                let resolved = self.broker.resolve_turn(*turn, cause);
                for item in resolved {
                    self.queue_resolved_or_cancel(&item, queue);
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

    /// Queues a resolution when durable, otherwise cancels its waiter.
    fn queue_resolved_or_cancel(&mut self, item: &Resolved, queue: &mut VecDeque<Effect>) {
        if self.queue_resolved(item, queue) {
            return;
        }
        self.broker.cancel(item);
    }

    /// Steps one broker resolution into queued effects without recursing.
    ///
    /// Returns whether the fold accepted the resolution *and* emitted its
    /// journal record: an unknown question steps to `Ok` with no `Emit`,
    /// and such a resolution must fail closed through [`Broker::cancel`]
    /// instead of waiting for a release that would carry an unjournaled
    /// answer.
    fn queue_resolved(&mut self, item: &Resolved, queue: &mut VecDeque<Effect>) -> bool {
        let by = if item.was_default {
            None
        } else {
            Some(item.by.clone())
        };
        let mut effects = Vec::new();
        let stepped = self
            .fold
            .step(
                Event::GrantResolved {
                    request: item.request.id,
                    answer: item.answer.clone(),
                    by,
                    was_default: item.was_default,
                    cause: item.cause,
                },
                Timestamp::now(),
                &mut effects,
            )
            .is_ok()
            && effects
                .iter()
                .any(|effect| matches!(effect, Effect::Emit(_)));
        if stepped {
            queue.extend(effects);
            self.pending_releases.push(item.clone());
        }
        stepped
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
        // Reserved-identity enforcement lives at the extension door
        // (`SessionServices::append_record`): the host child-start adapter
        // reaches this journal through `handle.ext_record` directly, so an
        // actor-side check cannot tell the trusted writer apart and must
        // not second-guess it.
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

    /// Journals one private-loop birth after pre-validation, and answers
    /// only after the journal receipt.
    ///
    /// The fold transition is checked BEFORE the durable append: the store
    /// performs no private validation, so a persisted invalid birth would
    /// fail the next open at replay. The transition commits only after the
    /// receipt, then the shared snapshot syncs (which publishes private
    /// terminals first) before the reply. A rejected birth appends nothing;
    /// a failed append folds/publishes nothing (irreversible store errors
    /// may partially write; the next open repairs).
    async fn on_private_loop(&mut self, req: super::PrivateLoopRequest) {
        let super::PrivateLoopRequest {
            r#loop,
            who,
            turn,
            reply,
        } = req;
        if let Some(broken) = &self.broken {
            let _ = reply.send(Err(AgentError::Invalid(ValidationError::new(
                broken.clone(),
            ))));
            return;
        }
        if let Err(conflict) = self.fold.check_private_loop(r#loop) {
            let _ = reply.send(Err(private_loop_conflict(conflict, r#loop)));
            return;
        }
        let record = Record::PrivateLoop {
            at: Timestamp::now(),
            r#loop,
            who: who.clone(),
            turn,
        };
        if let Err(error) = self.journal.append(vec![record]).await {
            let message: Box<str> = format!("journal write failed: {error}").into();
            if breaks_session(&error) {
                self.broken = Some(message.clone());
            }
            let _ = reply.send(Err(AgentError::PersistenceFailed { message }));
            return;
        }
        self.fold.fold_private_loop(r#loop, who, turn);
        self.shared.sync_ext(&self.fold);
        let _ = reply.send(Ok(()));
    }

    /// Journals one private-effect start after pre-validation, and answers
    /// only after the journal receipt.
    ///
    /// Same ordering as [`Self::on_private_loop`]. `call` and `args` ride
    /// the journal record for live-execution correlation; the fold pairing
    /// row keeps only the attribution subset (`who`, `turn`, `round`,
    /// `name`). The caller runs the effect only after this replies `Ok`.
    async fn on_private_start(&mut self, req: super::PrivateStartRequest) {
        let super::PrivateStartRequest {
            r#loop,
            token,
            who,
            turn,
            round,
            call,
            name,
            args,
            reply,
        } = req;
        if let Some(broken) = &self.broken {
            let _ = reply.send(Err(ServiceError::failed(None, broken.clone())));
            return;
        }
        if let Err(conflict) = self.fold.check_private_start(r#loop, token, &who, turn) {
            let _ = reply.send(Err(private_effect_conflict(conflict, r#loop, token)));
            return;
        }
        let record = Record::PrivateStart {
            at: Timestamp::now(),
            r#loop,
            token,
            who: who.clone(),
            turn,
            round,
            call,
            name: name.clone(),
            args,
        };
        if let Err(error) = self.journal.append(vec![record]).await {
            let message: Box<str> = format!("journal write failed: {error}").into();
            if breaks_session(&error) {
                self.broken = Some(message.clone());
            }
            let _ = reply.send(Err(ServiceError::failed(None, message)));
            return;
        }
        self.fold
            .fold_private_start(r#loop, token, &who, turn, round, &name);
        self.shared.sync_ext(&self.fold);
        let _ = reply.send(Ok(()));
    }

    /// Journals one private-effect terminal outcome after pre-validation,
    /// and answers only after the journal receipt.
    ///
    /// Same ordering as [`Self::on_private_loop`]. On `Ok` the terminal is
    /// durable, folded, and published. A rejected end appends nothing; a
    /// failed append folds/publishes nothing and leaves the row Started in
    /// memory, which the next open repairs to
    /// [`dal_core::PrivateOutcome::Indeterminate`].
    async fn on_private_end(&mut self, req: super::PrivateEndRequest) {
        let super::PrivateEndRequest {
            r#loop,
            token,
            outcome,
            reply,
        } = req;
        if let Some(broken) = &self.broken {
            let _ = reply.send(Err(ServiceError::failed(None, broken.clone())));
            return;
        }
        if let Err(conflict) = self.fold.check_private_end(r#loop, token) {
            let _ = reply.send(Err(private_effect_conflict(conflict, r#loop, token)));
            return;
        }
        let record = Record::PrivateEnd {
            at: Timestamp::now(),
            r#loop,
            token,
            outcome: outcome.clone(),
        };
        if let Err(error) = self.journal.append(vec![record]).await {
            let message: Box<str> = format!("journal write failed: {error}").into();
            if breaks_session(&error) {
                self.broken = Some(message.clone());
            }
            let _ = reply.send(Err(ServiceError::failed(None, message)));
            return;
        }
        self.fold.fold_private_end(r#loop, token, outcome);
        self.shared.sync_ext(&self.fold);
        let _ = reply.send(Ok(()));
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
        let mut queue = VecDeque::new();
        if !self.queue_resolved(&resolved, &mut queue) {
            self.broker.cancel(&resolved);
            return Err(AgentError::Invalid(ValidationError::new(format!(
                "request {id} has no open question."
            ))));
        }
        if !self.execute(queue.into_iter().collect(), None).await {
            let message = self
                .broken
                .clone()
                .unwrap_or_else(|| "journal append failed.".into());
            return Err(AgentError::Invalid(ValidationError::new(message)));
        }
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
        let mut effects = Vec::new();
        let _ = self.fold.step(
            Event::RequestOpened {
                request: request.clone(),
            },
            Timestamp::now(),
            &mut effects,
        );
        let deadline = Instant::now() + default_timeout(&request.question);
        let waiter = self.broker.track(request.clone(), deadline);
        asks.push((request.clone(), waiter));
        self.publish(UpdateKind::RequestOpened(request));
    }

    async fn sweep_expiry(&mut self) {
        // The caller computes controller availability per request class:
        // approvals and grants expire `NoController` with no approval
        // answerer attached; everything else expires `TimedOut` unless no
        // ask answerer is attached. The probe runs after the broker lock is
        // released, so it may take any attachment lock the registry needs.
        let expired = self
            .broker
            .expire(Instant::now(), |question| self.unattended(question));
        if expired.is_empty() {
            return;
        }
        let mut queue = VecDeque::new();
        for item in &expired {
            if !self.queue_resolved(item, &mut queue) {
                self.broker.cancel(item);
            }
        }
        self.execute(queue.into_iter().collect(), None).await;
    }

    /// Says whether no answerer for `question` is attached, by the class
    /// that could have resolved it.
    fn unattended(&self, question: &dal_core::Question) -> bool {
        match question {
            dal_core::Question::Approval { .. } | dal_core::Question::Grant { .. } => {
                !self.shared.attached_approval()
            }
            _ => !self.shared.attached_ask(),
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

    /// Runs one compare-and-swap state operation against the actor-owned
    /// map, persisted through the session's durable sidecar.
    async fn on_state(&mut self, req: super::StateReq) {
        let _ = req.reply.send(self.state_result(req.op).await);
    }

    /// Applies one state operation: the mutation is written to the
    /// sidecar file before the reply acknowledges it, so an acknowledged
    /// record always survives restart.
    #[expect(
        clippy::too_many_lines,
        reason = "one match arm per op keeps the CAS table readable"
    )]
    async fn state_result(&mut self, op: StateOp) -> Result<StateRecord, StateError> {
        if self.journal.sidecar().is_none() {
            return Err(StateError::Unavailable);
        }
        if self.state.is_none() {
            // A missing sidecar is an empty map; an unreadable or corrupt one
            // must surface rather than silently reset every stored key.
            let sidecar = self.journal.sidecar().ok_or(StateError::Unavailable)?;
            let (map, revisions) = load_state_map(sidecar.read(STATE_SIDECAR))?;
            self.state_rev = revisions;
            self.state = Some(map);
        }
        let Some(map) = self.state.as_mut() else {
            return Err(StateError::Unavailable);
        };
        let (ns, key) = match &op {
            StateOp::Read { ns, key }
            | StateOp::Write { ns, key, .. }
            | StateOp::Delete { ns, key, .. } => (ns, key),
        };
        let slot = (ns_label(ns), key.clone());
        // Build the prospective map and revision first, then publish them only
        // after the sidecar write succeeds: memory must never run ahead of the
        // journal (a failed write leaves the stored state untouched).
        let mut next = map.clone();
        let mut next_rev = self.state_rev;
        // An unchanged map needs no write: a read of an existing key stays
        // durable even when the sidecar directory is readable but unwritable.
        let (record, changed) = match op {
            StateOp::Read { .. } => {
                if let Some((value, at)) = map.get(&slot) {
                    (
                        StateRecord {
                            present: value.is_some(),
                            value: value.clone(),
                            revision: revision(*at),
                        },
                        false,
                    )
                } else {
                    // A missing key still mints a revision so the caller can
                    // compare-and-swap its first write.
                    next_rev += 1;
                    let minted = next_rev;
                    next.insert(slot, (None, minted));
                    (
                        StateRecord {
                            present: false,
                            value: None,
                            revision: revision(minted),
                        },
                        true,
                    )
                }
            }
            StateOp::Write {
                value, expected, ..
            } => {
                match map.get(&slot) {
                    Some((_, current)) if *current == expected.get().get() => {}
                    _ => return Err(StateError::Conflict),
                }
                next_rev += 1;
                let minted = next_rev;
                next.insert(slot, (Some(value.clone()), minted));
                (
                    StateRecord {
                        present: true,
                        value: Some(value),
                        revision: revision(minted),
                    },
                    true,
                )
            }
            StateOp::Delete { expected, .. } => {
                match map.get(&slot) {
                    Some((_, current)) if *current == expected.get().get() => {}
                    _ => return Err(StateError::Conflict),
                }
                next_rev += 1;
                let minted = next_rev;
                next.insert(slot, (None, minted));
                (
                    StateRecord {
                        present: false,
                        value: None,
                        revision: revision(minted),
                    },
                    true,
                )
            }
        };
        if !changed {
            return Ok(record);
        }
        let file = StateFile {
            format: STATE_FILE_FORMAT,
            revisions: next_rev,
            entries: next
                .iter()
                .map(|((ns, key), (value, at))| StateFileEntry {
                    ns: ns.clone(),
                    key: key.as_str().into(),
                    present: value.is_some(),
                    value: value.clone(),
                    revision: *at,
                })
                .collect(),
        };
        let Ok(bytes) = sonic_rs::to_string(&file) else {
            return Err(StateError::Unavailable);
        };
        if self.journal.is_lazy() {
            // The state file is about to be durable; the session it belongs
            // to must be recoverable first, or an acknowledged write is
            // unreachable after restart. Memory state must never be more
            // durable than the journal.
            self.journal
                .materialize()
                .await
                .map_err(|_| StateError::Unavailable)?;
        }
        let Some(sidecar) = self.journal.sidecar() else {
            self.state = None;
            return Err(StateError::Unavailable);
        };
        if sidecar.write(STATE_SIDECAR, bytes.as_bytes()).is_err() {
            // The write's outcome is indeterminate (the rename may have
            // published before the directory sync failed), so drop the
            // cached map: the next operation reloads whatever is durable
            // instead of serving a map the file may no longer match.
            self.state = None;
            return Err(StateError::Unavailable);
        }
        self.state_rev = next_rev;
        self.state = Some(next);
        Ok(record)
    }

    /// Reads or writes one extension-scoped sidecar file.
    async fn on_sidecar(&mut self, op: super::SidecarOp) {
        match op {
            super::SidecarOp::Read { ext, name, reply } => {
                let result = match self.journal.sidecar() {
                    None => Err("the session has no sidecar store".into()),
                    Some(sidecar) => {
                        let sidecar = sidecar.for_extension(&ext);
                        match sidecar.read(&name) {
                            Ok(bytes) => Ok(Some(bytes)),
                            Err(dal_store::StoreError::NotFound { .. }) => Ok(None),
                            Err(error) => Err(error.to_string().into()),
                        }
                    }
                };
                let _ = reply.send(result);
            }
            super::SidecarOp::Write {
                ext,
                name,
                bytes,
                reply,
            } => {
                let result = self.write_sidecar(&ext, &name, &bytes).await;
                let _ = reply.send(result);
            }
        }
    }

    async fn write_sidecar(
        &mut self,
        ext: &dal_core::Name,
        name: &dal_core::SidecarName,
        bytes: &[u8],
    ) -> Result<(), Box<str>> {
        if self.journal.is_lazy() {
            self.journal
                .materialize()
                .await
                .map_err(|error| error.to_string().into_boxed_str())?;
        }
        let Some(sidecar) = self.journal.sidecar() else {
            return Err("the session has no sidecar store".into());
        };
        sidecar
            .for_extension(ext)
            .write(name, bytes)
            .map_err(|error| error.to_string().into_boxed_str())
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
                Some(turn) => match self.fold.preflight_steer(turn) {
                    Ok(()) => Receipt::Delivered,
                    Err(Rejection::SteerFull) => Receipt::Full,
                    Err(_) => Receipt::Buffered,
                },
                None => self.buffered_mail_receipt(),
            },
            MailMode::Aside => Receipt::Delivered,
            MailMode::NextTurn => self.buffered_mail_receipt(),
            _ => {
                debug_assert!(false, "unmapped mail mode; add an explicit arm");
                self.buffered_mail_receipt()
            }
        };
        if receipt == Receipt::Full {
            let _ = reply.send(Ok(Some(receipt)));
            return;
        }
        let record = Record::Mail(dal_core::Mail {
            at: jiff::Timestamp::now(),
            from: mail.from,
            to: mail.to,
            mode: mail.mode,
            text: mail.text.clone(),
            reply_to: mail.reply_to.map(|entry| entry.get().to_string().into()),
        });
        if let Err(error) = self.journal.append(vec![record]).await {
            let message: Box<str> = format!("journal write failed: {error}").into();
            if breaks_session(&error) {
                self.broken = Some(message.clone());
            }
            let _ = reply.send(Err(ServiceError::failed(
                Some(dal_core::Service::Agents),
                message,
            )));
            return;
        }
        if mail.mode == MailMode::Steer
            && receipt == Receipt::Delivered
            && let Some(turn) = running
        {
            let mut effects = Vec::new();
            let stepped = self.fold.step(
                Event::Steer {
                    turn,
                    text: mail.text,
                },
                Timestamp::now(),
                &mut effects,
            );
            assert!(
                stepped.is_ok(),
                "exclusive steer preflight must remain valid"
            );
            self.execute(effects, None).await;
        }
        let _ = reply.send(Ok(Some(receipt)));
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

    /// Runs one turn operation against the control cell and fold.
    async fn on_turn(&mut self, req: super::TurnRequest) {
        let running = self.control().running();
        let idle = self.control().idle();
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
            _ => TurnOpReply::Idle(idle),
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

/// Collects the intact read views a settled outcome delivered.
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

/// Maps a loop-birth validation conflict onto the agent error contract.
///
/// The text is the fold-owned shape description plus the rejected loop
/// identity. No append is attempted on this path, so the journal stays
/// byte-identical and the reply carries the whole failure.
fn private_loop_conflict(conflict: PrivateConflict, r#loop: PrivateLoopId) -> AgentError {
    AgentError::Invalid(ValidationError::new(format!(
        "private loop {}: {}",
        r#loop,
        conflict.detail()
    )))
}

/// Maps a private-effect validation conflict onto the service failure
/// contract.
///
/// Shared by the start and end paths: both carry the same (`loop`, `token`)
/// identity and the same fold-owned shape text, and both reject before any
/// append, so one mapper covers both without duplicating the text rule.
fn private_effect_conflict(
    conflict: PrivateConflict,
    r#loop: PrivateLoopId,
    token: PrivateEffectId,
) -> ServiceError {
    ServiceError::failed(
        None,
        format!(
            "private effect {} in loop {}: {}",
            token,
            r#loop,
            conflict.detail()
        ),
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_state_map_treats_a_missing_file_as_empty() {
        let (map, revisions) = load_state_map(Err(dal_store::StoreError::NotFound {
            path: std::path::PathBuf::from("state"),
        }))
        .expect("missing is empty");
        assert!(map.is_empty() && revisions == 0);
    }

    #[test]
    fn load_state_map_surfaces_corruption_instead_of_resetting() {
        let corrupt = load_state_map(Ok(b"{not json".to_vec()));
        assert!(
            matches!(corrupt, Err(StateError::Unavailable)),
            "malformed JSON fails closed: {corrupt:?}"
        );
        let bad_key = load_state_map(Ok(
            br#"{"format":1,"revisions":3,"entries":[{"ns":"eval","key":"BAD KEY","present":false,"value":null,"revision":3}]}"#
                .to_vec(),
        ));
        assert!(
            matches!(bad_key, Err(StateError::Unavailable)),
            "an invalid stored key fails closed: {bad_key:?}"
        );
        let future = load_state_map(Ok(br#"{"format":2,"revisions":3,"entries":[]}"#.to_vec()));
        assert!(
            matches!(future, Err(StateError::Unavailable)),
            "an unknown format version fails closed: {future:?}"
        );
        let unknown = load_state_map(Ok(
            br#"{"format":1,"revisions":3,"entries":[],"extra":true}"#.to_vec(),
        ));
        assert!(
            matches!(unknown, Err(StateError::Unavailable)),
            "an unknown stored field fails closed: {unknown:?}"
        );
        let zero = load_state_map(Ok(
            br#"{"format":1,"revisions":3,"entries":[{"ns":"eval","key":"k","present":true,"value":0,"revision":0}]}"#
                .to_vec(),
        ));
        assert!(
            matches!(zero, Err(StateError::Unavailable)),
            "a zero stored revision fails closed: {zero:?}"
        );
        let ahead = load_state_map(Ok(
            br#"{"format":1,"revisions":3,"entries":[{"ns":"eval","key":"k","present":true,"value":0,"revision":4}]}"#
                .to_vec(),
        ));
        assert!(
            matches!(ahead, Err(StateError::Unavailable)),
            "a revision ahead of the counter fails closed: {ahead:?}"
        );
        let dupe = load_state_map(Ok(
            br#"{"format":1,"revisions":4,"entries":[{"ns":"eval","key":"k","present":true,"value":0,"revision":2},{"ns":"eval","key":"k","present":true,"value":1,"revision":3}]}"#
                .to_vec(),
        ));
        assert!(
            matches!(dupe, Err(StateError::Unavailable)),
            "a duplicate slot fails closed: {dupe:?}"
        );
        let reused = load_state_map(Ok(
            br#"{"format":1,"revisions":4,"entries":[{"ns":"eval","key":"a","present":true,"value":0,"revision":2},{"ns":"eval","key":"b","present":true,"value":1,"revision":2}]}"#
                .to_vec(),
        ));
        assert!(
            matches!(reused, Err(StateError::Unavailable)),
            "a revision reused across slots fails closed: {reused:?}"
        );
        let armed = load_state_map(Ok(
            br#"{"format":1,"revisions":3,"entries":[{"ns":"eval","key":"k","present":false,"value":0,"revision":2}]}"#
                .to_vec(),
        ));
        assert!(
            matches!(armed, Err(StateError::Unavailable)),
            "a tombstone carrying a payload fails closed: {armed:?}"
        );
    }

    #[test]
    fn load_state_map_preserves_an_explicit_null_value() {
        // `value: null` with `present: true` is a stored JSON null, not a
        // tombstone: serde cannot distinguish them without the flag.
        let bytes = br#"{"format":1,"revisions":2,"entries":[{"ns":"eval","key":"k","present":true,"value":null,"revision":2}]}"#
            .to_vec();
        let (map, _) = load_state_map(Ok(bytes)).expect("stored map");
        let Some((value, at)) = map.get(&("eval".into(), StateKey::parse("k").expect("key")))
        else {
            panic!("the null entry survives the load");
        };
        assert_eq!(*at, 2);
        let Some(value) = value else {
            panic!("an explicit null is present, not a tombstone");
        };
        assert_eq!(value.as_str(), "null");
    }

    #[test]
    fn load_state_map_replays_stored_entries() {
        let bytes = br#"{"format":1,"revisions":4,"entries":[{"ns":"eval","key":"counter","present":true,"value":"41","revision":2}]}"#
            .to_vec();
        let (map, revisions) = load_state_map(Ok(bytes)).expect("stored map");
        assert_eq!(revisions, 4);
        assert_eq!(map.len(), 1);
    }
}
