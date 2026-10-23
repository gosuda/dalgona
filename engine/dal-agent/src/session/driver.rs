//! Turn driver: inference, dispatch, compaction, and commands.
//!
//! One driver task per session consumes [`DriverPorts`]. Inference streams
//! through [`turn`](crate::session::turn), dispatch runs through
//! [`dispatch`](crate::session::dispatch), and every fold-bound report goes
//! back through the session handle. Command effects run through
//! [`commands`](crate::session::commands).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use dal_core::ext::{BeforeRequest, Channel, TurnEnd};
use dal_core::{
    BlobId, CallId, CompactLimits, CompactionExtRecord, ContextItem, EntryKind, EntryView, Family,
    Gen, InferFailure, Inference, JournalPart, ModelInfo, ModelRequest, ModelRoute, Name, PageReq,
    Part, Purpose, RawJson, RequestParams, ResolvedCall, SessionId, Settled, SettledOutcome, Stop,
    StreamChannel, StreamEvent, ToolResultEvent, TurnId, Usage, Visibility, Workspace,
};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::broker::Broker;
use crate::ext::EventStream;
use crate::ext::generation::Generation;
use crate::ext::hooks::{
    HookScope, ObserverReport, StreamFire, StreamFireAction, StreamVerdict, StreamWatch, TurnInfo,
    dispatch_before_request, dispatch_settled, dispatch_tool_result, dispatch_turn_end,
    hook_fanout,
};
use crate::ext::overlay::{Overlay, TurnTools};
use crate::ext::prompt::{PromptOrder, PromptSection, SectionCx};
use crate::ext::services::SessionBackend;
use crate::ext::{Caller, CallerKind, Services, ToolDescription};
use crate::host::{HostShared, HostState};
use crate::jobs::JobTable;
use crate::session::actor::{DriverPorts, TurnBatch, TurnWork, WatchOutcome};
use crate::session::backend::Backend;
use crate::session::context::{
    DeferredTool, LEAF_WINDOW, StreamCall, ToolBoundaries, api_family, cache_key, compaction_count,
    context_entries, context_items, model_info_for, recent_leaf, resolve_calls, search_surface,
    system_prompt, tool_list,
};
use crate::session::continuation::{ContinuationOwner, ParkError};
use crate::session::control::ControlCell;
use crate::session::dispatch::{
    DispatchCtx, GrantLedger, ReadyCall, echo_resolved, plan_units, run_unit,
};
use crate::session::projection::SnapshotArgs;
use crate::session::shared::{ChildPromptPolicy, Shared};
use crate::session::tasks::SessionTasks;
use crate::session::turn::{RequestDeps, StreamConverter, hydrate_part, infer_stream};
use crate::session::{SessionHandle, commands};

/// Upper bound on concurrent reads in one read run.
const PARALLEL_READS: usize = 4;

/// Bound for hook waits inside one turn.
const TURN_DEADLINE: Duration = Duration::from_secs(600);

/// A provider stream this quiet is a stall, not a slow model.
const STREAM_IDLE_REPORT: Duration = Duration::from_secs(30);

const COMPACTION_MIN_TOKENS: u64 = 1;

const COMPACTION_KEEP_TOKENS: u64 = 20_000;

pub(super) fn compact_limits(threshold: f64, generation: &Generation) -> CompactLimits {
    CompactLimits {
        threshold,
        min_tokens: COMPACTION_MIN_TOKENS,
        keep_tokens: COMPACTION_KEEP_TOKENS,
        enabled: true,
        compactor_available: !generation.compactors.entries().is_empty(),
    }
}

pub(super) struct OwnedWatcher {
    pub(super) owner: Option<Box<str>>,
    pub(super) watcher: Box<dyn StreamWatch>,
}

pub(crate) struct WatchDelivery {
    pub(super) watchers: Vec<OwnedWatcher>,
    pub(super) fires: Vec<(usize, StreamFire)>,
    pub(super) interrupt: Option<usize>,
    pub(super) allowed: Option<usize>,
    pub(super) stopped_by: Option<usize>,
    legacy_interrupt: bool,
    suppressed: Vec<StreamVerdict>,
}

impl WatchDelivery {
    fn feed(&mut self, event: &StreamEvent) -> Option<(usize, StreamVerdict)> {
        let StreamEvent::Delta { channel, text } = event else {
            return None;
        };
        let channel = match channel {
            StreamChannel::Text => Channel::Text,
            StreamChannel::Thinking => Channel::Thinking,
            StreamChannel::ToolArgs { tool } => Channel::ToolArgs {
                tool: Name::parse_mapped_tool(tool).ok()?,
            },
        };
        self.collect_verdicts(|watcher| watcher.feed(channel.clone(), text))
    }

    fn finish(&mut self) -> Option<(usize, StreamVerdict)> {
        self.collect_verdicts(|watcher| watcher.finish())
    }

    fn collect_verdicts(
        &mut self,
        poll: impl Fn(&mut dyn StreamWatch) -> StreamVerdict,
    ) -> Option<(usize, StreamVerdict)> {
        for (index, owned) in self.watchers.iter_mut().enumerate() {
            let verdict = poll(owned.watcher.as_mut());
            self.fires.extend(
                owned
                    .watcher
                    .take_fires()
                    .into_iter()
                    .map(|fire| (index, fire)),
            );
            let StreamVerdict::Interrupt { rule, .. } = &verdict else {
                continue;
            };
            if self.legacy_interrupt
                || self.fires.iter().any(|(owner, fire)| {
                    *owner == index
                        && &fire.rule == rule
                        && fire.action == StreamFireAction::Interrupt
                })
            {
                return Some((index, verdict));
            }
            self.suppressed.push(verdict);
        }
        None
    }
}

/// Construction inputs for one session driver task.
pub(crate) struct DriverDeps {
    /// The owning session.
    pub session: SessionId,
    /// The parent session, when this session is a subagent.
    pub parent: Option<SessionId>,
    /// The session workspace.
    pub workspace: Workspace,
    /// The host state for providers, config, and generation.
    pub host: Arc<HostState>,
    /// The turn's data-plane.
    pub backend: Arc<Backend>,
    /// Capability-scoped services for hooks and tools.
    pub services: Arc<dyn Services>,
    /// The session request broker for approval questions.
    pub broker: Arc<Broker>,
    /// The session job table shared with the actor.
    pub jobs: Arc<Mutex<JobTable>>,
    /// The shared snapshot for prompt assembly.
    pub shared: Arc<Shared>,
    /// The session tool overlay, snapshotted when each turn starts.
    pub overlay: Arc<Overlay>,
    /// The session journal generation for snapshots.
    pub generation: Gen,
    /// The actor port for fold-bound reports.
    pub handle: SessionHandle,
    /// The session cancellation token.
    pub cancel: CancellationToken,
    /// The owner for all background work in this session.
    pub tasks: SessionTasks,
    /// Whether the journal is in-memory.
    pub ephemeral: bool,
}

/// Per-turn driver state.
struct TurnState {
    /// The turn cancellation token.
    cancel: CancellationToken,
    /// The turn's script host, minted against the turn-start generation
    /// so catalog answers and authority resolution share one identity.
    script: Arc<crate::session::script::SessionScriptHost>,
    /// The turn's continuation owner, minted from the session tasks on first
    /// touch. Held strongly for the whole turn; requests carry only its
    /// `Weak` in `RequestDeps`. Present on every turn (Cursor or not): a
    /// non-Cursor turn never parks, so its slot stays idle until this owner
    /// ends it. The `Drop` backstop is cleanup only; all terminals end here
    /// explicitly.
    owner: ContinuationOwner,
    /// The generation snapshot taken at turn start.
    generation: Arc<Generation>,
    /// The overlay tools and promotions frozen at turn start.
    tools: TurnTools,
    /// Deferred tool descriptions from the active provider request.
    deferred_search: Arc<[DeferredTool]>,
    /// The active model route and family, when known.
    model: Option<(ModelRoute, Family)>,
    /// The resolved provider used to qualify continuation requests.
    provider: Option<Box<str>>,
    image_profile: Option<dal_provider::ImageProfile>,
    context_window: Option<u64>,
    /// The identity the opening's input hooks saw, until the turn's first
    /// request or a changed requested route consumes it.
    opening: Option<OpeningModel>,
    /// The most tool rounds this turn may run, when a prompt bounded it.
    step_cap: Option<std::num::NonZeroU32>,
    /// The request round within the turn.
    round: u32,
    /// Streamed tool calls in response order.
    calls: Vec<StreamCall>,
    /// Classification stepped into the fold.
    resolved: Vec<ResolvedCall>,
    /// Call arguments by identity for dispatch.
    args: HashMap<CallId, RawJson>,
    /// Turn approval grants shared by every dispatch batch of the turn.
    ledger: Arc<Mutex<GrantLedger>>,
    /// Last provider-reported request usage, when the stream carried one.
    usage: Option<Usage>,
}

/// One session driver task.
struct Driver {
    deps: DriverDeps,
    turns: HashMap<TurnId, TurnState>,
    scoped: Option<Vec<Box<str>>>,
    last_model: Option<(ModelRoute, Family)>,
    last_catalog_entry: Option<dal_provider::CatalogEntry>,
    /// Counts the request boundaries at which the tool list changed; the
    /// prompt-cache key carries the count.
    tool_boundaries: ToolBoundaries,
    /// The actor's turn-bypass cell shared over `DriverPorts`; `cancel` there
    /// preempts a live `infer` stream — a queued `Effect::Stop` can't.
    control: Arc<std::sync::Mutex<ControlCell>>,
}

/// Spawns the session driver task consuming `ports`.
pub(crate) fn spawn(ports: DriverPorts, deps: DriverDeps) -> JoinHandle<()> {
    let control = ports.control.clone();
    #[expect(
        clippy::disallowed_methods,
        reason = "session-owned driver task: the host stores the handle and aborts it on close"
    )]
    tokio::spawn(
        Driver {
            deps,
            turns: HashMap::new(),
            scoped: None,
            last_model: None,
            last_catalog_entry: None,
            tool_boundaries: ToolBoundaries::default(),
            control,
        }
        .run(ports),
    )
}

impl Driver {
    /// Consumes effect batches until the channel closes or the session ends.
    async fn run(mut self, mut ports: DriverPorts) {
        while let Some(batch) = ports.ops_rx.recv().await {
            if self.deps.cancel.is_cancelled() {
                self.end_all_owners();
                return;
            }
            self.batch(batch).await;
        }
        self.end_all_owners();
    }

    /// Handles one actor batch: effects in order, then ask waiters.
    async fn batch(&mut self, mut batch: TurnBatch) {
        let mut opening = batch.opening.take();
        if batch.model.is_some() {
            let family = batch
                .family
                .or_else(|| batch.model.clone().and_then(|route| api_family(&route)));
            if let (Some(route), Some(family)) = (batch.model.clone(), family) {
                self.last_model = Some((route, family));
            }
        }
        for effect in std::mem::take(&mut batch.effects) {
            match effect {
                dal_core::Effect::Infer(plan) => {
                    self.bind_turn_step_cap(plan.turn);
                    self.adopt_opening(plan.turn, &mut opening, batch.model.as_ref());
                    self.infer(plan.turn, plan.params, &batch).await;
                }
                dal_core::Effect::Dispatch { turn, units } => {
                    self.dispatch(turn, units, &batch).await;
                }
                dal_core::Effect::Compact { turn, first_kept } => {
                    if let Some(turn) = turn {
                        self.bind_turn_step_cap(turn);
                        self.adopt_opening(turn, &mut opening, batch.model.as_ref());
                    }
                    self.compact(turn, first_kept).await;
                }
                dal_core::Effect::Command { cmd, by } => {
                    let route = self.last_model.clone().map(|(route, _)| route);
                    commands::run_command(&self.deps, &mut self.scoped, route, cmd, by).await;
                }
                dal_core::Effect::Stop {
                    turn,
                    stop,
                    overflowed,
                } => {
                    self.observe_turn_end(turn, stop, overflowed).await;
                    self.observe_settled(turn).await;
                    self.stop(turn, stop).await;
                }
                effect => {
                    self.report(TurnWork::TaskFailed {
                        turn: None,
                        message: format!("driver received actor-bound effect {effect:?}").into(),
                    })
                    .await;
                }
            }
        }
        for (request, waiter) in std::mem::take(&mut batch.asks) {
            let handle = self.deps.handle.clone();
            self.deps.tasks.spawn(async move {
                let crate::broker::Settled {
                    answer,
                    by,
                    resolution,
                    cause,
                } = waiter.await;
                let Some(resolved) = echo_resolved(&request, &answer, &by, resolution, cause)
                else {
                    return;
                };
                let _ = handle.work(TurnWork::Answered { resolved }).await;
            });
        }
    }

    /// Mints the script host of one hook chain: a fresh host against the
    /// current generation, whose uncaptured environment is the empty
    /// authority, so hook-phase entries stay fail-closed.
    fn hook_script(&self) -> Option<crate::ext::ScriptCx> {
        let generation = self.deps.host.shared.generation.borrow().clone();
        let script = crate::session::script::SessionScriptHost::for_generation(
            self.deps.session,
            &self.deps.backend,
            Arc::clone(&self.deps.host.shared.interpreters),
            generation,
        );
        script.attach(None)
    }

    /// Reports one fold-bound item through the session handle.
    async fn report(&self, work: TurnWork) {
        let _ = self.deps.handle.work(work).await;
    }

    /// Returns the turn state, creating it on first touch of the turn.
    ///
    /// The turn binds the bypass-cell token when the actor already opened
    /// it (`TurnStarted` is journaled before any driver effect), in either
    /// phase: opening hooks run before the journal sees `TurnStart`, so an
    /// opening touch binds the opening token `cancel` fires. A turn with no
    /// live token is stale or still replaying; it binds an inert token no
    /// cancel fires, never the session token, so a stop cannot leak across
    /// turns.
    fn turn(&mut self, turn: TurnId) -> &mut TurnState {
        let deps = &self.deps;
        let cancel = self
            .control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .token_for(turn)
            .unwrap_or_default();
        self.turns.entry(turn).or_insert_with(|| {
            let generation = deps.host.shared.generation.borrow().clone();
            let tools = deps
                .overlay
                .publish(&generation, deps.shared.promoted())
                .restricted_to(deps.shared.tool_allowlist());
            let script = crate::session::script::SessionScriptHost::for_generation(
                deps.session,
                &deps.backend,
                Arc::clone(&deps.host.shared.interpreters),
                Arc::clone(&generation),
            );
            TurnState {
                cancel,
                script,
                owner: ContinuationOwner::new(deps.tasks.clone()),
                generation,
                tools,
                deferred_search: Arc::from([]),
                model: None,
                provider: None,
                image_profile: None,
                context_window: None,
                opening: None,
                step_cap: None,
                round: 0,
                calls: Vec::new(),
                resolved: Vec::new(),
                args: HashMap::new(),
                ledger: Arc::new(Mutex::new(GrantLedger::new())),
                usage: None,
            }
        })
    }

    /// Binds a pending prompt bound when the driver receives the active
    /// turn's first effect, rather than when a lazy state lookup happens to
    /// materialize it. Stale effects queued for an earlier turn cannot claim
    /// the bound after the actor has opened the new turn.
    fn bind_turn_step_cap(&mut self, turn: TurnId) {
        if self.turns.contains_key(&turn) {
            return;
        }
        let active = self
            .control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .running();
        if active != Some(turn) {
            return;
        }
        let step_cap = self.deps.shared.take_next_turn_step_cap();
        self.turn(turn).step_cap = step_cap;
    }

    /// Binds the identity the opening's input hooks saw to `turn`'s first
    /// request, and to a compaction that runs before it.
    ///
    /// The carry holds only while the requested route still equals the one
    /// the opening resolved: a changed route (an explicit model selection)
    /// drops it, and the request resolves as it always did. Run after
    /// `bind_turn_step_cap`, which returns early once the turn state exists.
    fn adopt_opening(
        &mut self,
        turn: TurnId,
        opening: &mut Option<OpeningModel>,
        requested: Option<&ModelRoute>,
    ) {
        let Some(carried) = opening.take_if(|carried| carried.turn == turn) else {
            return;
        };
        if carried.requested.as_ref() != requested {
            return;
        }
        let state = self.turn(turn);
        state.model = Some((carried.resolved.route.clone(), carried.resolved.family));
        state.provider = Some(carried.resolved.provider.clone());
        state.image_profile = carried.resolved.entry.image_profile;
        state.context_window = carried.resolved.entry.context_window.map(u64::from);
        state.opening = Some(carried);
    }

    /// Stops one turn: cancels its token and confirms through the fold.
    ///
    /// The batch queue runs effects in order, so by the time a stop effect
    /// arrives every in-flight call settle — including a cancelled process's
    /// kill ladder — has completed; the reported confirmation is the point
    /// at which `TurnEnded` may reach subscribers.
    async fn stop(&mut self, turn: TurnId, stop: Stop) {
        if let Some(state) = self.turns.get(&turn) {
            state.cancel.cancel();
        }
        self.end_owner(turn);
        self.report(TurnWork::Cancelled { turn, stop }).await;
    }

    /// Runs one provider request for `turn` and steps its lifecycle.
    ///
    /// A token cancelled before the first effect (a cancel that landed
    /// during the opening still begins the turn through `Guard`) ends the
    /// turn through the pending cancel fold: opening the stream here would
    /// spend a provider request — consuming a scripted response — for a
    /// dead turn and poison the next prompt after resume.
    async fn infer(&mut self, turn: TurnId, params: RequestParams, batch: &TurnBatch) {
        if self.turn(turn).cancel.is_cancelled() {
            return;
        }
        for notice in self.deps.shared.take_setup_notices() {
            self.deps.backend.notify(notice);
        }
        let mut mark = std::time::Instant::now();
        let Some(resolved) = self.resolve_request(turn, batch).await else {
            return;
        };
        lap(self.deps.session, turn, "resolve", &mut mark);
        let (stream, cancel) = self.open_stream(turn, &resolved, params, batch).await;
        lap(self.deps.session, turn, "stream-open", &mut mark);
        self.consume_stream(turn, resolved, stream, cancel).await;
    }

    /// Takes the opening's identity for this request when the requested
    /// route still equals the one the opening resolved. A carry the route
    /// moved past is dropped, never reused.
    fn carried_resolution(&mut self, turn: TurnId, batch: &TurnBatch) -> Option<ResolvedRequest> {
        let carried = self.turns.get_mut(&turn)?.opening.take()?;
        (carried.requested == batch.model).then_some(carried.resolved)
    }

    /// Records the requested route snapshot as the turn's active route.
    fn bind_requested(&mut self, turn: TurnId, batch: &TurnBatch) {
        let Some(route) = batch.model.clone() else {
            return;
        };
        let family = batch
            .family
            .or_else(|| api_family(&route))
            .unwrap_or(Family::Chat);
        let state = self.turn(turn);
        if !provider_pin_holds(state.model.as_ref().map(|(current, _)| current), &route) {
            state.provider = None;
        }
        state.model = Some((route, family));
    }

    /// Resolves the route for `turn`, caches the resolved row, and reports
    /// the request opening; `None` after the failure is already reported.
    ///
    /// The turn's first request uses the identity its opening's input hooks
    /// saw when one is carried (see `adopt_opening`), so the catalog is not
    /// fetched again for it.
    async fn resolve_request(
        &mut self,
        turn: TurnId,
        batch: &TurnBatch,
    ) -> Option<ResolvedRequest> {
        let resolution = if let Some(carried) = self.carried_resolution(turn, batch) {
            Ok(carried)
        } else {
            self.bind_requested(turn, batch);
            self.resolve_model(turn).await
        };
        let resolved = match resolution {
            Ok(resolved) => resolved,
            Err(message) => {
                if let Some((model, family)) =
                    self.turns.get(&turn).and_then(|state| state.model.clone())
                {
                    self.report(TurnWork::StreamEnded {
                        turn,
                        model,
                        family,
                        result: Err(InferFailure::Fatal { message, fix: None }),
                        partial: None,
                    })
                    .await;
                } else {
                    self.report(TurnWork::TaskFailed {
                        turn: Some(turn),
                        message,
                    })
                    .await;
                }
                return None;
            }
        };
        {
            let state = self.turn(turn);
            state.model = Some((resolved.route.clone(), resolved.family));
            state.provider = Some(resolved.provider.clone());
            state.image_profile = resolved.entry.image_profile;
            state.context_window = resolved.entry.context_window.map(u64::from);
        }
        self.last_model = Some((resolved.route.clone(), resolved.family));
        self.last_catalog_entry = Some(resolved.entry.clone());
        let compact = compact_limits(
            self.deps.host.shared.config.compact_ratio(),
            &self.turn(turn).generation,
        );
        let max_steps = self
            .turn(turn)
            .step_cap
            .map_or(0, std::num::NonZeroU32::get);
        self.report(TurnWork::RequestStarted {
            turn,
            model: resolved.route.clone(),
            family: resolved.family,
            window: resolved.entry.context_window.map_or(0, u64::from),
            max_steps,
            compact,
        })
        .await;
        Some(resolved)
    }

    /// Captures the decision boundary and opens the provider stream.
    async fn open_stream(
        &mut self,
        turn: TurnId,
        resolved: &ResolvedRequest,
        params: RequestParams,
        batch: &TurnBatch,
    ) -> (EventStream, CancellationToken) {
        let mut mark = std::time::Instant::now();
        // The decision-request boundary: one environment capture per
        // provider request freezes the authority, the cutoff, and the
        // policy fingerprint for every cell of this round.
        let allowed = self.deps.host.shared.config.eval_uses().clone();
        let fingerprint =
            crate::session::script::SessionScriptHost::policy_fingerprint(&batch.policy, &allowed);
        let cutoff = Some(self.deps.shared.cursor());
        let session = self.deps.session;
        let host = Arc::clone(&self.deps.host);
        let (script, cancel) = {
            let state = self.turn(turn);
            let cancel = state.cancel.clone();
            state.script.capture(
                Arc::clone(&state.generation),
                allowed,
                cutoff,
                fingerprint,
                Some(turn),
                cancel.clone(),
            );
            (Arc::clone(&state.script), cancel)
        };
        let request = self.request(turn, resolved, params).await;
        lap(self.deps.session, turn, "request", &mut mark);
        // A cancel that landed while the before-request hooks ran has already
        // released them; sending the request anyway would spend a provider
        // call, and consume a scripted response, for a dead turn. The stream
        // is empty: `consume_stream` returns on the cancelled token and the
        // pending cancel fold ends the turn.
        if cancel.is_cancelled() {
            return (EventStream::new(futures::stream::empty(), || {}), cancel);
        }
        // Continuation resume: when the previous round parked on a slotted
        // step, the next generation step re-enters through the owned run with
        // the exact cover of the durable journal. This runs for every family
        // with no gate: direct Cursor rounds resume here, and synthetic turns
        // whose inner run parked on this same turn slot resume here too (the
        // Loop never mints owners, so its inner runs share the incoming
        // Weak). An idle slot — every flow that never parked — makes this a
        // no-op by construction. The cover error ends the turn here; no
        // second server run is opened beside the parked conversation, and the
        // owner ends in the failure branch below (idempotent with the turn
        // terminal, which repeats it harmlessly).
        let slot = self
            .turns
            .get(&turn)
            .map(|state| state.owner.downgrade())
            .and_then(|weak| weak.upgrade());
        let abandoned = has_active_input(&request)
            && matches!(&resolved.route, ModelRoute::Api { family, .. } if *family == Family::Cursor)
            && slot.as_ref().is_some_and(|slot| slot.abandon());
        let route_abandoned = matches!(&resolved.route, ModelRoute::Api { .. })
            && slot.as_ref().is_some_and(|slot| {
                slot.abandon_if_route_changed(&resolved.route, &resolved.provider)
            });
        if !abandoned
            && !route_abandoned
            && let Some(error) = self.respond_if_awaiting(turn, &cancel).await
        {
            // Cover failure: the turn ends here without opening a second
            // server run beside the parked conversation. The owner ends now
            // (idempotent; the turn terminal repeats it harmlessly), and the
            // failed stream below carries the single terminal through
            // `consume_stream` — nothing is reported here, so one request
            // yields exactly one terminal event.
            self.end_owner(turn);
            let stream = EventStream::new(futures::stream::iter(vec![Err(error)]), || {});
            return (stream, cancel);
        }
        // `continuation` is the `RequestDeps` Weak-handle field: the strong
        // owner above outlives the full invocation; only this Weak crosses.
        let continuation = self.turns.get(&turn).map(|state| state.owner.downgrade());
        let deps = RequestDeps {
            session,
            host,
            script: Some(script),
            continuation,
        };
        let stream = infer_stream(&deps, request, &cancel).await;
        (stream, cancel)
    }

    /// Ends the turn's continuation owner explicitly: the deterministic
    /// owner-end path for every turn terminal and close. Idempotent; the
    /// owner `Drop` backstop only catches missed paths. Never called per
    /// request, and never stops the session tasks the owner was minted from:
    /// a live parked conversation survives request errors until its turn
    /// ends, and session background work outlives every turn.
    fn end_owner(&mut self, turn: TurnId) {
        if let Some(state) = self.turns.get(&turn) {
            state.owner.release_if_live();
        }
    }

    /// Ends every turn owner: the session-close path of `run`. Releases only;
    /// session tasks are stopped by the session shutdown itself, never here.
    fn end_all_owners(&mut self) {
        for state in self.turns.values() {
            state.owner.release_if_live();
        }
    }

    /// Re-announces the round's session tool calls before their results
    /// dispatch.
    ///
    /// The offered set is the exact session `ToolCallsDone` order the
    /// converter observed on this round — never the classified `resolved`
    /// rows — re-parked through L5 `park_session_calls`: a Loop-preparked
    /// mixed round keeps its full private-plus-session cover (the subset is
    /// validated id-present, name-equal, no-dupe without mutating it), while
    /// a direct round with no cover yet validates full exactly like `park`,
    /// so the direct cover is never weakened. Family-agnostic: synthetic
    /// turns re-announce Loop-emitted session sets on the shared turn slot,
    /// and turns with no slot conversation (Idle — every non-slotted flow)
    /// skip silently and dispatch as today. Call-free rounds never park
    /// (Boundary path, as today). Returns the park failure, if any; the
    /// caller ends the turn on it without dispatching.
    fn park_if_tool_use(&self, turn: TurnId) -> Option<dal_provider::ProviderError> {
        let pending: Vec<(CallId, Box<str>)> = self
            .turns
            .get(&turn)?
            .calls
            .iter()
            .map(|call| (call.call.clone(), call.name.clone()))
            .collect();
        if pending.is_empty() {
            return None;
        }
        let slot = self.turns.get(&turn)?.owner.downgrade().upgrade()?;
        match slot.park_session_calls(pending) {
            // No slot conversation (Idle: every non-slotted flow) — dispatch
            // as today. Any other park failure ends the turn without
            // dispatching unresumable results.
            Ok(()) | Err(ParkError::NoConversation) => None,
            Err(error) => Some(dal_provider::ProviderError::InvalidRequest {
                message: format!("cannot park tool-use stop: {error}"),
            }),
        }
    }

    /// Maps one journal part to provider content.
    ///
    /// Total over the recorded variants: `None` only for a part that cannot
    /// be returned to the provider (an unparseable digest or undecodable
    /// bytes), in which case the caller fails the step instead of resuming
    /// with silently dropped content.
    fn journal_part_to_part(part: &JournalPart) -> Option<Part> {
        match part {
            JournalPart::Text { text } => Some(Part::Text { text: text.clone() }),
            JournalPart::TextBlob { blob, bytes } => Some(Part::Blob {
                blob_id: dal_core::BlobId::parse(blob).ok()?,
                mime: "text/plain".into(),
                bytes: *bytes,
            }),
            JournalPart::Image { mime, base64 } => Some(Part::Image {
                mime: mime.clone(),
                bytes: {
                    use base64::Engine as _;
                    base64::engine::general_purpose::STANDARD
                        .decode(base64.as_bytes())
                        .ok()?
                        .into()
                },
            }),
            JournalPart::ImageBlob { mime, blob, bytes }
            | JournalPart::Blob { mime, blob, bytes } => Some(Part::Blob {
                blob_id: dal_core::BlobId::parse(blob).ok()?,
                mime: mime.clone(),
                bytes: *bytes,
            }),
        }
    }

    /// Collects the authoritative session results for one parked round.
    ///
    /// The durable fold rows the actor published (`Shared::leaf_entries`)
    /// for this round's call ids, in round order. Dispatch memory — success
    /// subsets, in-flight outcomes — is never consulted; error and refused
    /// rows count exactly like ok rows. Rows with no journal row are skipped
    /// so the slot cover itself reports the gap (missing stays missing;
    /// nothing is donated from an extra row). A matched row holding a part
    /// that cannot be returned to the provider fails the whole step instead
    /// of resuming with silently dropped content, mirroring the
    /// blob-hydration refusal precedent. Offered verbatim to the
    /// synthetic-join lane (L6), which implements the same shape for
    /// loop-owned rounds.
    fn journaled_results_for_round(
        entries: &[EntryView],
        order: &[CallId],
    ) -> Option<Vec<dal_provider::HostToolResult>> {
        let mut by_call: HashMap<&CallId, &EntryView> = HashMap::new();
        for entry in entries {
            let EntryKind::ToolResult { call, .. } = &entry.kind else {
                continue;
            };
            by_call.entry(call).or_insert(entry);
        }
        // Missing rows stay missing: they are skipped here and the slot cover
        // reports them, so the turn dies with an accurate Missing diagnostic
        // instead of a false unmappable-part message. Only an unmappable part
        // fails the set (`None` below is exclusively that case).
        let mut out = Vec::with_capacity(order.len());
        for call in order {
            let Some(entry) = by_call.get(call) else {
                continue;
            };
            let EntryKind::ToolResult {
                name, error, parts, ..
            } = &entry.kind
            else {
                continue;
            };
            // Total mapping: one unmappable part fails the step (the
            // collected `Option` short-circuits), never a quiet truncation
            // of the provider's result batch.
            let mapped = parts
                .iter()
                .map(Self::journal_part_to_part)
                .collect::<Option<Vec<Part>>>()?;
            out.push(dal_provider::HostToolResult {
                call: call.clone(),
                name: name.clone(),
                is_error: *error,
                parts: mapped,
            });
        }
        Some(out)
    }

    /// Resumes the turn's parked conversation before the next generation
    /// step, when one is awaiting host results. Returns the cover failure
    /// without releasing: the caller ends the turn on it and the owner end
    /// releases. Uses only settled continuation API (`is_awaiting`,
    /// `respond`); never touches a slot guard across the await.
    async fn respond_if_awaiting(
        &self,
        turn: TurnId,
        cancel: &CancellationToken,
    ) -> Option<dal_provider::ProviderError> {
        let slot = self.turns.get(&turn)?.owner.downgrade().upgrade()?;
        if !slot.is_awaiting() {
            return None;
        }
        // Round identity is the streamed classification stepped into the fold
        // (`Resolved` order from `ToolCallsDone`), never the dispatch success
        // subset. Prior rounds carry disjoint call ids, so the leaf scoping
        // below cannot pick up a stale row for a live id; the slot cover
        // re-verifies every (id, name) against its parked pending either way.
        let order: Vec<CallId> = self
            .turns
            .get(&turn)?
            .resolved
            .iter()
            .map(|resolved| resolved.call.clone())
            .collect();
        let entries = self.deps.shared.leaf_entries();
        let Some(mut session_results) = Self::journaled_results_for_round(&entries, &order) else {
            return Some(dal_provider::ProviderError::InvalidRequest {
                message: "this round's journaled tool result holds a part that cannot be returned to the provider; refusing to resume the parked run.".into(),
            });
        };
        for result in &mut session_results {
            for part in &mut result.parts {
                if let Err(error) = hydrate_part(&self.deps.handle, part, cancel).await {
                    return Some(error);
                }
            }
        }
        slot.respond(session_results, cancel).await.err()
    }

    fn watch_delivery(&mut self, turn: TurnId, model: &ModelRoute) -> WatchDelivery {
        let leaf = self.leaf();
        let visible: HashSet<_> = context_entries(&leaf)
            .filter(|entry| matches!(&entry.kind, EntryKind::Reminder { source, .. } if source.starts_with("rule:")))
            .map(|entry| entry.id)
            .collect();
        let seeds: Vec<_> = self
            .deps
            .shared
            .rule_seeds()
            .iter()
            .filter(|seed| visible.contains(&seed.entry))
            .cloned()
            .collect();
        let turn_info = TurnInfo {
            session: self.deps.session,
            turn,
            model: Some(model),
            gate_seeds: Some(&seeds),
        };
        let generation = self.turn(turn).generation.clone();
        WatchDelivery {
            watchers: self.start_watchers(&generation, &turn_info),
            fires: Vec::new(),
            interrupt: None,
            allowed: None,
            stopped_by: None,
            legacy_interrupt: self.deps.shared.can_interrupt_stream(),
            suppressed: Vec::new(),
        }
    }

    /// Releases the provider before judgment, then transfers the finished watch.
    async fn consume_stream(
        &mut self,
        turn: TurnId,
        resolved: ResolvedRequest,
        mut stream: EventStream,
        cancel: CancellationToken,
    ) {
        let mut delivery = self.watch_delivery(turn, &resolved.route);
        let mut converter = StreamConverter::new();
        let mut events = Vec::new();
        let mut failed = None;
        let mut stopped = None;
        'stream: loop {
            let item = tokio::select! {
                biased;
                () = cancel.cancelled() => return,
                item = stream.next() => item,
                () = tokio::time::sleep(STREAM_IDLE_REPORT) => {
                    tracing::warn!(target: "dalgon.agent", session = %self.deps.session, turn = %turn, idle = ?STREAM_IDLE_REPORT, "provider stream idle");
                    continue;
                }
            };
            let Some(item) = item else { break };
            let event = match item {
                Ok(event) => event,
                Err(error) => {
                    failed = Some(self.failure(error));
                    break;
                }
            };
            for core in converter.convert(event) {
                let stop = delivery.feed(&core);
                self.report_suppressed(turn, &mut delivery).await;
                if let Some(stop) = stop {
                    stopped = Some(stop);
                    break 'stream;
                }
                self.note_call(turn, &core);
                if let StreamEvent::Usage(usage) = &core {
                    self.turn(turn).usage = Some(*usage);
                }
                events.push(core.clone());
                self.report(TurnWork::Streamed { turn, event: core }).await;
            }
        }
        drop(stream);
        if cancel.is_cancelled() {
            return;
        }
        if stopped.is_none() {
            for core in converter.flush() {
                let stop = delivery.feed(&core);
                self.report_suppressed(turn, &mut delivery).await;
                if let Some(stop) = stop {
                    stopped = Some(stop);
                    break;
                }
                events.push(core.clone());
                self.report(TurnWork::Streamed { turn, event: core }).await;
            }
        }
        if stopped.is_none() {
            stopped = delivery.finish();
            self.report_suppressed(turn, &mut delivery).await;
        }
        let resolve = stopped.is_none() && failed.is_none();
        let event = match stopped {
            Some(stop) => {
                self.turn(turn).calls.clear();
                self.turn(turn).args.clear();
                self.turn(turn).resolved.clear();
                let Some(event) = self
                    .resolve_watch_stop(turn, &mut delivery, stop, &cancel)
                    .await
                else {
                    return;
                };
                event
            }
            None => WatchOutcome::Ended {
                model: resolved.route,
                family: resolved.family,
                result: match failed {
                    Some(failure) => Err(failure),
                    None => Ok(Inference { events }),
                },
            },
        };
        self.settle_consumed_stream(turn, event, delivery, &cancel, resolve)
            .await;
    }

    /// Settles one consumed stream's watch outcome, then parks and resolves.
    ///
    /// The slotted Cursor tool-use stop parks ahead of classification and
    /// dispatch (dispatch itself is a later actor batch, so this precedes
    /// every tool execution). A park failure ends the turn here without
    /// dispatch: results that cannot resume must not execute, and without
    /// a parked pending set the next cover would fail anyway.
    async fn settle_consumed_stream(
        &mut self,
        turn: TurnId,
        event: WatchOutcome,
        delivery: WatchDelivery,
        cancel: &CancellationToken,
        resolve: bool,
    ) {
        if self.settle_watch(turn, event, delivery, cancel).await && resolve {
            if let Some(error) = self.park_if_tool_use(turn) {
                // The turn ends here: end the owner now (idempotent) and
                // report once, without dispatching unresumable results.
                self.end_owner(turn);
                if let Some((model, family)) =
                    self.turns.get(&turn).and_then(|state| state.model.clone())
                {
                    self.report(TurnWork::StreamEnded {
                        turn,
                        model,
                        family,
                        result: Err(self.failure(error)),
                        partial: None,
                    })
                    .await;
                } else {
                    self.report(TurnWork::TaskFailed {
                        turn: Some(turn),
                        message: error.to_string().into(),
                    })
                    .await;
                }
                return;
            }
            self.resolve(turn).await;
        }
    }
}

/// Logs an infer step that exceeded 250 ms.
fn lap(session: SessionId, turn: TurnId, step: &str, mark: &mut std::time::Instant) {
    let taken = mark.elapsed();
    if taken > std::time::Duration::from_millis(250) {
        tracing::debug!(target: "dalgon.agent", session = %session, turn = %turn, step, taken = ?taken, "infer step took long");
    }
    *mark = std::time::Instant::now();
}

struct ResolvedRequest {
    route: ModelRoute,
    family: Family,
    provider: Box<str>,
    entry: dal_provider::CatalogEntry,
}

/// Whether the provider an earlier round of this turn resolved still
/// qualifies the route the next batch requests.
///
/// The turn keeps the route its last round resolved, and that route names
/// its provider, while the batch re-sends the requested route, which may
/// not. They name the same model when family and native id agree and no
/// two providers disagree, so a provider-less request keeps the pin; a
/// different model, family, or provider drops it. Exact equality would drop
/// the pin on every round for a provider-less request.
fn provider_pin_holds(current: Option<&ModelRoute>, requested: &ModelRoute) -> bool {
    current.is_none_or(|current| current.same_model(requested))
}
fn has_active_input(request: &ModelRequest) -> bool {
    let Some(ContextItem::User { parts }) = request.context.last() else {
        return false;
    };
    let mut texts = Vec::new();
    for part in parts {
        match part {
            Part::Text { text } => texts.push(text.as_ref()),
            Part::Image { .. } | Part::Blob { .. } => return true,
        }
    }
    !texts.join("\n").trim().is_empty()
}

/// The model one turn opening resolved for its input hooks: the requested
/// route snapshot it resolved from and the resolution itself. The turn's first
/// request reuses it, so the hooks and the request share one identity.
pub(crate) struct OpeningModel {
    turn: TurnId,
    requested: Option<ModelRoute>,
    resolved: ResolvedRequest,
}

impl OpeningModel {
    /// The model as `before_request` hooks see it, built by the function the
    /// request itself uses.
    pub(crate) fn info(&self, thinking: dal_core::ThinkingLevel) -> ModelInfo {
        model_info_for(
            &self.resolved.entry,
            &self.resolved.route,
            &self.resolved.provider,
            thinking,
        )
    }
}

/// Resolves a requested route through the catalog, caching it for commands.
///
/// A route that names its provider resolves exactly that provider's row and
/// never consults the synthetic registry: a provider-native id that spells a
/// registered synthetic id is still the provider's model. `provider` is the
/// provider an earlier round of the same turn resolved; it only qualifies a
/// route that does not name one.
async fn resolve_requested(
    shared: &HostShared,
    route: &ModelRoute,
    provider: Option<&str>,
) -> Result<ResolvedRequest, Box<str>> {
    match (route, provider) {
        (
            ModelRoute::Api {
                provider: Some(_), ..
            },
            _,
        ) => {
            resolve_catalog(
                shared,
                &crate::host::ops::request_reference(route),
                Some(route),
            )
            .await
        }
        (ModelRoute::Api { model, .. }, Some(provider)) => {
            resolve_reference(shared, &format!("{provider}/{model}"), None).await
        }
        _ => {
            resolve_reference(
                shared,
                &crate::host::ops::request_reference(route),
                Some(route),
            )
            .await
        }
    }
}

/// Resolves the configured default model.
async fn resolve_default(shared: &HostShared) -> Result<ResolvedRequest, Box<str>> {
    resolve_reference(shared, shared.config.model().unwrap_or(""), None).await
}

/// Resolves `reference`, or the model `route` names when one is given.
async fn resolve_reference(
    shared: &HostShared,
    reference: &str,
    route: Option<&ModelRoute>,
) -> Result<ResolvedRequest, Box<str>> {
    if reference.is_empty() {
        return Err("no model configured: set dal.toml [models] default.".into());
    }
    if let Some(found) = synthetic(shared, reference) {
        return Ok(ResolvedRequest {
            family: Family::Chat,
            provider: found.provider(),
            entry: found.entry(),
            route: found.route(),
        });
    }
    resolve_catalog(shared, reference, route).await
}

/// Resolves `reference`, or the model `route` names, through the provider
/// catalog alone; the synthetic registry is not consulted.
async fn resolve_catalog(
    shared: &HostShared,
    reference: &str,
    route: Option<&ModelRoute>,
) -> Result<ResolvedRequest, Box<str>> {
    let catalog = shared
        .providers
        .catalog()
        .await
        .map_err(|error| format!("provider catalog unavailable: {error}").into_boxed_str())?;
    shared.cache_catalog(catalog.clone());
    let aliases: Vec<(Box<str>, Box<str>)> = shared
        .config
        .aliases()
        .iter()
        .map(|(name, target)| (name.clone(), target.clone()))
        .collect();
    let resolved = match route {
        Some(route) => dal_provider::resolve_route(&catalog, &aliases, route),
        None => dal_provider::resolve(&catalog, &aliases, reference),
    }
    .map_err(|error| {
        let shown = route.and_then(ModelRoute::provider).map_or_else(
            || reference.to_owned(),
            |provider| format!("{provider}/{reference}"),
        );
        format!("model {shown} did not resolve: {error}").into_boxed_str()
    })?;
    Ok(ResolvedRequest {
        family: api_family(&resolved.route).unwrap_or(Family::Chat),
        provider: resolved.provider.clone(),
        entry: resolved.entry.clone(),
        route: resolved.route,
    })
}

/// Resolves a registered synthetic model; harness ids never match.
fn synthetic(shared: &HostShared, reference: &str) -> Option<crate::ext::synthetic::Found> {
    if reference.starts_with("dalgon/") {
        return None;
    }
    crate::ext::synthetic::find(shared, reference)
}

/// The model a turn opening will request: the route the session selected,
/// else the configured default, resolved by the same functions the request
/// itself uses. `None` when the route does not resolve.
///
/// The caller hands the value to the driver, which reuses it for the turn's
/// first request while `requested` still names the requested route.
pub(crate) async fn opening_model(
    shared: &HostShared,
    turn: TurnId,
    requested: Option<ModelRoute>,
) -> Option<OpeningModel> {
    let resolved = match requested.as_ref() {
        Some(route) => resolve_requested(shared, route, None).await,
        None => resolve_default(shared).await,
    }
    .ok()?;
    Some(OpeningModel {
        turn,
        requested,
        resolved,
    })
}

impl Driver {
    /// Resolves the request route through the catalog, caching it for commands.
    async fn resolve_model(&self, turn: TurnId) -> Result<ResolvedRequest, Box<str>> {
        let Some(route) = self.turn_model(turn) else {
            return resolve_default(&self.deps.host.shared).await;
        };
        let provider = self
            .turns
            .get(&turn)
            .and_then(|state| state.provider.as_deref());
        resolve_requested(&self.deps.host.shared, &route, provider).await
    }

    /// Returns the turn's active route, when the actor snapshotted one.
    fn turn_model(&self, turn: TurnId) -> Option<ModelRoute> {
        self.turns
            .get(&turn)?
            .model
            .as_ref()
            .map(|(route, _)| route.clone())
    }

    /// Builds one provider request: hooks, prompt, tools, and context.
    async fn request(
        &mut self,
        turn: TurnId,
        resolved: &ResolvedRequest,
        params: RequestParams,
    ) -> ModelRequest {
        let mode = self.deps.host.shared.config.mode();
        let thinking = self.deps.host.shared.config.thinking();
        let generation = self.turn(turn).generation.clone();
        let turn_tools = self.turn(turn).tools.clone();
        let info = model_info_for(
            &resolved.entry,
            &resolved.route,
            &resolved.provider,
            thinking,
        );
        let model_id = cache_model_id(&resolved.provider, &resolved.route);
        let mur = self.turn(turn).round;
        let cancel = {
            let state = self.turn(turn);
            state.round = mur + 1;
            state.cancel.clone()
        };
        let params = self
            .hook_params(turn, &generation, &info, params, mur, &cancel)
            .await;
        let entries = self.deps.shared.leaf_entries();
        let context = context_items(recent_leaf(&entries));
        // Replayed history can carry letter images admitted for an earlier
        // image-capable turn; a model that cannot read images must never
        // receive those bytes, so the context degrades to words first.
        let context = if resolved.entry.image_input {
            context
        } else {
            strip_images_for_text_model(context)
        };
        let context: Arc<[ContextItem]> = context.into();
        let delivered = context_image_digests(&context);
        let system = self.system(turn, &generation, &turn_tools, mode, &info, &delivered);
        let surface = search_surface(&resolved.provider, &resolved.route);
        let (model_tools, deferred_search) =
            tool_list(&generation, &turn_tools, &info, &model_id, mode, surface);
        self.turn(turn).deferred_search = deferred_search;
        let tools: Arc<[dal_core::ModelToolSpec]> = model_tools.into();
        let visibility = self.tool_boundaries.observe(&tools);
        let cache_key = cache_key(self.deps.session, compaction_count(&entries), visibility);
        ModelRequest {
            purpose: Purpose::Turn,
            model: resolved.route.clone(),
            system: system.into(),
            tools,
            context,
            params,
            cache_key: Some(cache_key),
        }
    }

    /// Runs before-request hooks over the tuning parameters with a cap clamp.
    ///
    /// `cancel` is the turn's own token, the one a user cancel fires, so a
    /// held hook ends with its turn. The session token would leave the hook
    /// running to its deadline while the driver, which handles the cancel
    /// batch only after this returns, waits behind it.
    async fn hook_params(
        &self,
        turn: TurnId,
        generation: &Generation,
        info: &ModelInfo,
        params: RequestParams,
        round: u32,
        cancel: &CancellationToken,
    ) -> RequestParams {
        let event = BeforeRequest {
            turn,
            round,
            model: info.clone(),
            caps: info.caps.clone(),
            params,
            thinking_explicit: self.deps.shared.has_explicit_thinking(),
        };
        let deadline = tokio::time::Instant::now() + TURN_DEADLINE;
        let scope = HookScope {
            services: &self.deps.services,
            session: self.deps.session,
            parent: self.deps.parent,
            process_env: Arc::clone(&self.deps.host.shared.env),
            cancel,
            turn_deadline: deadline,
            script: self.hook_script(),
        };
        let mut current = event.params.clone();
        for (index, extension, caller) in hook_fanout(generation, Some(turn)) {
            let dispatch = scope.cx(&caller, Some(turn));
            let step = dispatch_before_request(
                extension.name(),
                &dispatch,
                generation.before_requests(index),
                &event,
                current,
            )
            .await;
            current = step.params;
            for notice in step.notices {
                self.notice(turn, "hook.before_request", &notice);
            }
        }
        crate::ext::hooks::clamp_params(current, &info.caps)
    }

    /// Renders the system prompt for one provider request.
    ///
    /// A child prompt policy reshapes the product-owned base sections: an
    /// explicit system takes the place of the identity and D2 sections, and
    /// a role appends one section after the assembled base so siblings on
    /// one model share the cached prefix. Extension-contributed sections
    /// keep rendering either way, so a child start cannot silently drop
    /// required extension guidance.
    fn system(
        &mut self,
        turn: TurnId,
        generation: &Generation,
        turn_tools: &TurnTools,
        mode: dal_core::Mode,
        info: &ModelInfo,
        delivered: &[BlobId],
    ) -> String {
        let policy = self.deps.shared.child_prompt_policy();
        let tools = descriptions(generation, turn_tools, info);
        let deps = &self.deps;
        let rendered = |owner: usize, section: &PromptSection| {
            render_section(
                deps,
                generation.extensions.get(owner)?,
                section,
                turn,
                &tools,
                info,
                delivered,
            )
        };
        match policy {
            Some(ChildPromptPolicy::System(system)) => {
                let base = base_without_product_sections(generation, mode, rendered);
                if base.is_empty() {
                    system.to_string()
                } else {
                    format!("{system}\n\n{base}")
                }
            }
            Some(ChildPromptPolicy::Role(role)) => {
                let base = system_prompt(generation, mode, &rendered);
                if base.is_empty() {
                    role.to_string()
                } else {
                    format!("{base}\n\n{role}")
                }
            }
            None => system_prompt(generation, mode, &rendered),
        }
    }

    /// Publishes one hook notice without journaling.
    fn notice(&self, turn: TurnId, kind: &str, text: &str) {
        self.deps.backend.notify(dal_core::Notice {
            turn: Some(turn),
            kind: kind.into(),
            text: text.into(),
        });
    }

    /// Reads the current leaf entries for prompt and compaction assembly.
    fn leaf(&self) -> Vec<EntryView> {
        let mut entries = self.deps.shared.leaf_entries();
        if entries.len() > LEAF_WINDOW {
            entries.drain(..entries.len() - LEAF_WINDOW);
        }
        entries
    }

    async fn report_suppressed(&self, turn: TurnId, delivery: &mut WatchDelivery) {
        for verdict in delivery.suppressed.drain(..) {
            self.report(TurnWork::WatcherVerdict { turn, verdict })
                .await;
        }
    }

    async fn resolve_watch_stop(
        &self,
        turn: TurnId,
        delivery: &mut WatchDelivery,
        stop: (usize, StreamVerdict),
        cancel: &CancellationToken,
    ) -> Option<WatchOutcome> {
        let (watcher, verdict) = stop;
        delivery.stopped_by = Some(watcher);
        let StreamVerdict::Interrupt { rule, inject } = verdict else {
            unreachable!("only interrupt verdicts stop the stream")
        };
        let selected = delivery.fires.iter().position(|(owner, fire)| {
            *owner == watcher && fire.rule == rule && fire.action == StreamFireAction::Interrupt
        });
        let Some(selected) = selected else {
            return Some(WatchOutcome::Verdict {
                verdict: StreamVerdict::Interrupt { rule, inject },
            });
        };
        let fire = &delivery.fires[selected].1;
        let accepted = if fire.judged {
            let result = tokio::select! {
                biased;
                () = cancel.cancelled() => return None,
                result = delivery.watchers[watcher].watcher.resolve_judged(fire.index, cancel.clone()) => result,
            };
            match result {
                Ok(accepted) => accepted,
                Err(crate::ext::HookError::Cancelled) => return None,
                Err(crate::ext::HookError::Failed { .. }) => {
                    if cancel.is_cancelled() {
                        return None;
                    }
                    self.notice(
                        turn,
                        "rules.judge",
                        "A rule judgment failed; retrying without a reminder.",
                    );
                    false
                }
            }
        } else {
            true
        };
        if cancel.is_cancelled() {
            return None;
        }
        if accepted {
            delivery.interrupt = Some(selected);
        } else {
            delivery.allowed = Some(selected);
        }
        Some(WatchOutcome::Retry {
            reminder: accepted.then_some((rule, inject)),
        })
    }

    async fn settle_watch(
        &self,
        turn: TurnId,
        event: WatchOutcome,
        delivery: WatchDelivery,
        cancel: &CancellationToken,
    ) -> bool {
        let (reply, receipt) = oneshot::channel();
        let work = TurnWork::Watched {
            turn,
            event,
            delivery,
            reply,
        };
        let sent = tokio::select! {
            biased;
            () = cancel.cancelled() => return false,
            result = self.deps.handle.work(work) => result,
        };
        if sent.is_err() {
            return false;
        }
        tokio::select! {
            biased;
            () = cancel.cancelled() => false,
            result = receipt => matches!(result, Ok(true)),
        }
    }

    /// Retains one streamed tool call for resolution and dispatch.
    fn note_call(&mut self, turn: TurnId, core: &StreamEvent) {
        let StreamEvent::ToolCall { call, name, args } = core else {
            return;
        };
        self.turn(turn).calls.push(StreamCall {
            call: call.clone(),
            name: name.clone(),
            args: args.clone(),
        });
    }

    /// Maps a transport error onto the classified failure surface.
    ///
    /// The provider's own classification is kept for overflow (a context
    /// window error or an HTTP 413) so the fold can compact and retry. A
    /// retryable failure that reaches this point has already used its whole
    /// retry budget, and the fold ignores `Retryable`, so it ends the turn as
    /// `Fatal`.
    fn failure(&self, error: dal_provider::ProviderError) -> InferFailure {
        if self.deps.cancel.is_cancelled() {
            return InferFailure::Cancelled;
        }
        match InferFailure::from(error) {
            InferFailure::Retryable { message, .. } => InferFailure::Fatal { message, fix: None },
            classified => classified,
        }
    }

    /// Classifies the turn's streamed calls and steps them into the fold.
    ///
    /// A response with no streamed calls steps `Boundary` instead of an
    /// empty `Resolved`: the fold only runs the end-after-boundary path
    /// from a boundary event, and an empty resolved is dropped outside
    /// the resolving stage.
    async fn resolve(&mut self, turn: TurnId) {
        let mode = self.deps.host.shared.config.mode();
        let workspace = self.deps.workspace.clone();
        let generation = self.turn(turn).generation.clone();
        let turn_tools = self.turn(turn).tools.clone();
        let calls = std::mem::take(&mut self.turn(turn).calls);
        if calls.is_empty() {
            self.report(TurnWork::Boundary { turn }).await;
            return;
        }
        let resolved = resolve_calls(&generation, &turn_tools, &calls, mode, &workspace);
        let mut args = HashMap::new();
        for call in &calls {
            args.insert(call.call.clone(), call.args.clone());
        }
        self.turn(turn).args = args;
        self.turn(turn).resolved.clone_from(&resolved);
        self.report(TurnWork::Resolved {
            turn,
            calls: resolved,
            answerer_attached: self.deps.shared.attached_approval(),
        })
        .await;
    }

    /// Dispatches one fold-planned batch through the tool dispatcher.
    async fn dispatch(&mut self, turn: TurnId, units: Vec<dal_core::Unit>, batch: &TurnBatch) {
        let Some(state) = self.turns.get(&turn) else {
            self.report(TurnWork::TaskFailed {
                turn: Some(turn),
                message: "dispatch arrived for a turn with no streamed calls.".into(),
            })
            .await;
            return;
        };
        let mut ready = HashMap::new();
        for resolved in &state.resolved {
            let Ok(class) = &resolved.result else {
                continue;
            };
            let Some(args) = state.args.get(&resolved.call) else {
                continue;
            };
            ready.insert(
                resolved.call.clone(),
                ReadyCall {
                    call: resolved.call.clone(),
                    name: resolved.name.clone(),
                    args: args.clone(),
                    class: class.clone(),
                },
            );
        }
        let (planned, _) = plan_units(&state.resolved);
        debug_assert_eq!(planned, units);
        let ctx = DispatchCtx {
            session: self.deps.session,
            parent: self.deps.parent,
            process_env: Arc::clone(&self.deps.host.shared.env),
            turn,
            workspace: self.deps.workspace.clone(),
            mode: self.deps.host.shared.config.mode(),
            generation: Arc::clone(&state.generation),
            tools: state.tools.clone(),
            deferred_search: Arc::clone(&state.deferred_search),
            services: Arc::clone(&self.deps.services),
            generation_id: state.generation.id,
            backend: Arc::clone(&self.deps.backend),
            broker: Arc::clone(&self.deps.broker),
            policy: batch.policy.clone(),
            cancel: state.cancel.child_token(),
            turn_deadline: tokio::time::Instant::now() + TURN_DEADLINE,
            parallel_reads: PARALLEL_READS,
            cutoff: None,
            ledger: Arc::clone(&state.ledger),
            script: state.script.attach(None),
        };
        for unit in &planned {
            for settled in run_unit(&ctx, unit, &ready).await {
                let event = state
                    .resolved
                    .iter()
                    .find(|resolved| resolved.call == settled.call)
                    .map(|resolved| {
                        Self::tool_result_event(
                            turn,
                            &settled.call,
                            &resolved.name,
                            &settled.outcome,
                        )
                    });
                for rep in settled.reports {
                    self.report(rep).await;
                }
                if let Some(event) = event {
                    self.observe_tool_result(&ctx, &event).await;
                }
            }
        }
    }

    async fn observe_tool_result(&self, ctx: &DispatchCtx, event: &ToolResultEvent) {
        let mut report = ObserverReport::default();
        let scope = HookScope {
            services: &ctx.services,
            session: ctx.session,
            parent: ctx.parent,
            process_env: Arc::clone(&ctx.process_env),
            cancel: &ctx.cancel,
            turn_deadline: ctx.turn_deadline,
            script: ctx.script.clone(),
        };
        for (index, extension, caller) in hook_fanout(&ctx.generation, Some(ctx.turn)) {
            let dispatch = scope.cx(&caller, Some(ctx.turn));
            dispatch_tool_result(
                extension.name(),
                &dispatch,
                ctx.generation.tool_results(index),
                event,
                &mut report,
            )
            .await;
            dispatch_tool_result(
                extension.name(),
                &dispatch,
                ctx.generation.tool_results_lossless(index),
                event,
                &mut report,
            )
            .await;
        }
    }

    async fn observe_turn_end(&self, turn: TurnId, stop: Stop, overflowed: bool) {
        let Some(state) = self.turns.get(&turn) else {
            return;
        };
        let generation = Arc::clone(&state.generation);
        let cancel = state.cancel.clone();
        let script = state.script.attach(None);
        let event = TurnEnd {
            turn,
            stop,
            overflowed,
        };
        let mut report = ObserverReport::default();
        let scope = HookScope {
            services: &self.deps.services,
            session: self.deps.session,
            parent: self.deps.parent,
            process_env: Arc::clone(&self.deps.host.shared.env),
            cancel: &cancel,
            turn_deadline: tokio::time::Instant::now() + TURN_DEADLINE,
            script,
        };
        for (index, extension, caller) in hook_fanout(&generation, Some(turn)) {
            let dispatch = scope.cx(&caller, Some(turn));
            dispatch_turn_end(
                extension.name(),
                &dispatch,
                generation.turn_ends(index),
                &event,
                &mut report,
            )
            .await;
            dispatch_turn_end(
                extension.name(),
                &dispatch,
                generation.turn_ends_lossless(index),
                &event,
                &mut report,
            )
            .await;
        }
    }

    fn start_watchers(&self, generation: &Generation, turn: &TurnInfo<'_>) -> Vec<OwnedWatcher> {
        let setup = self.deps.shared.setup_watches();
        let capacity = setup.len()
            + generation
                .extensions
                .iter()
                .enumerate()
                .map(|(index, _)| generation.watches(index).len())
                .sum::<usize>();
        let mut watchers = Vec::with_capacity(capacity);
        for (index, extension) in generation.extensions.iter().enumerate() {
            for factory in generation.watches(index) {
                let owner = generation.watcher_owner(factory).and_then(|owner| {
                    let name = if owner == index {
                        extension.name()
                    } else {
                        generation.extensions.get(owner)?.name()
                    };
                    Some(name.into())
                });
                let Some(watcher) = factory.start(turn) else {
                    continue;
                };
                watchers.push(OwnedWatcher { owner, watcher });
            }
        }
        for (owner, factory) in setup.iter() {
            if let Some(watcher) = factory.start(turn) {
                watchers.push(OwnedWatcher {
                    owner: Some(owner.as_str().into()),
                    watcher,
                });
            }
        }
        watchers
    }

    async fn observe_settled(&self, turn: TurnId) {
        let Some(state) = self.turns.get(&turn) else {
            return;
        };
        let generation = Arc::clone(&state.generation);
        let cancel = state.cancel.clone();
        let script = state.script.attach(None);
        let view = self.deps.shared.snapshot(SnapshotArgs {
            generation: self.deps.generation,
            id: self.deps.session,
            workspace: self.deps.workspace.clone(),
            open: self.deps.broker.open_requests(),
            updated_at: dal_core::Timestamp::now(),
            created_at: None,
            archived: None,
            page: full_page(),
        });
        let reply_text = settled_reply_text(&view.entries.items);
        let event = Settled { turn, reply_text };
        let mut report = ObserverReport::default();
        let scope = HookScope {
            services: &self.deps.services,
            session: self.deps.session,
            parent: self.deps.parent,
            process_env: Arc::clone(&self.deps.host.shared.env),
            cancel: &cancel,
            turn_deadline: tokio::time::Instant::now() + TURN_DEADLINE,
            script,
        };
        for (index, extension, caller) in hook_fanout(&generation, Some(turn)) {
            let dispatch = scope.cx(&caller, Some(turn));
            dispatch_settled(
                extension.name(),
                &dispatch,
                generation.settleds(index),
                &event,
                &mut report,
            )
            .await;
            dispatch_settled(
                extension.name(),
                &dispatch,
                generation.settleds_lossless(index),
                &event,
                &mut report,
            )
            .await;
        }
    }

    fn tool_result_event(
        turn: TurnId,
        call: &CallId,
        tool: &Name,
        outcome: &SettledOutcome,
    ) -> ToolResultEvent {
        let (ok, preview) = match outcome {
            SettledOutcome::Ok { text, .. } => (true, Self::bounded_preview(text)),
            SettledOutcome::Err { text } => (false, Self::bounded_preview(text)),
            SettledOutcome::Interrupted | SettledOutcome::Detached { .. } => (false, "".into()),
        };
        ToolResultEvent {
            turn,
            call: call.clone(),
            tool: tool.clone(),
            ok,
            preview,
        }
    }

    /// Cuts a settled tool text to the 4096 bytes a hook sees. A tool that
    /// ends its text with a `Full output: <path>` line names the file that
    /// holds what the cut drops, so a cut that would lose that line shrinks
    /// the content to keep it inside the same bound instead.
    fn bounded_preview(text: &str) -> Box<str> {
        const MAX_BYTES: usize = 4096;
        if text.len() <= MAX_BYTES {
            return text.into();
        }
        let end = Self::char_floor(text, MAX_BYTES);
        let Some((start, footer)) = Self::full_output_footer(text) else {
            return text[..end].into();
        };
        let room = MAX_BYTES.saturating_sub(footer.len() + 1);
        if start + footer.len() <= end || room == 0 {
            return text[..end].into();
        }
        let content = &text[..Self::char_floor(&text[..start], room)];
        let mut preview = String::with_capacity(MAX_BYTES);
        preview.push_str(content);
        if !content.is_empty() && !content.ends_with('\n') {
            preview.push('\n');
        }
        preview.push_str(footer);
        preview.into()
    }

    /// The largest char boundary of `text` at or below `max`.
    fn char_floor(text: &str, max: usize) -> usize {
        let mut end = max.min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        end
    }

    /// The last line of `text` that starts with the full-output prefix, with
    /// its byte offset. The tool appends that line after everything a command
    /// printed, so an earlier line of the same shape is command output.
    fn full_output_footer(text: &str) -> Option<(usize, &str)> {
        let start = text
            .rmatch_indices(crate::FULL_OUTPUT_PREFIX)
            .map(|(index, _)| index)
            .find(|index| *index == 0 || text[..*index].ends_with('\n'))?;
        let line = text[start..].lines().next()?;
        Some((start, line))
    }

    /// Runs the registered compactor chain over the current leaf entries.
    async fn compact(&mut self, turn: Option<TurnId>, first_kept: Option<dal_core::EntryId>) {
        let view = self.deps.shared.snapshot(SnapshotArgs {
            generation: self.deps.generation,
            id: self.deps.session,
            workspace: self.deps.workspace.clone(),
            open: self.deps.broker.open_requests(),
            updated_at: dal_core::Timestamp::now(),
            created_at: None,
            archived: None,
            page: full_page(),
        });
        let total = view.usage.context_tokens;
        // An all-zero input count is an unmeasured report, not a zero-token
        // context: it falls back to the leaf estimate like the fold does, or
        // a shrinking replacement can never read as shrinking.
        let measured = turn
            .and_then(|turn| self.turns.get(&turn))
            .and_then(|state| state.usage)
            .filter(|usage| usage.input_tokens > 0)
            .map(|usage| {
                usage.input_tokens + usage.output_tokens + usage.reasoning_tokens.unwrap_or(0)
            });
        let entries = self.leaf();
        let cut = first_kept
            .and_then(|first| entries.iter().position(|entry| entry.id == first))
            .unwrap_or(entries.len());
        let covered = covered_entries(&entries[..cut]);
        let replaced_tokens = replaced_context_tokens(&entries, cut, &covered);
        let retained = entries[cut..]
            .iter()
            .map(retained_images)
            .fold(RetainedImages::default(), RetainedImages::add);
        let carried = entries.iter().rev().find_map(|entry| match &entry.kind {
            dal_core::EntryKind::Compaction {
                summary: Some(summary),
                ..
            } => Some(summary.clone()),
            _ => None,
        });
        let has_context = covered.iter().any(|entry| entry.note.is_none());
        let outcome = match covered.first().zip(covered.last()).filter(|_| has_context) {
            Some(_) if replaced_tokens == 0 => {
                Err("nothing to compact: the context before the cut was already replaced.".into())
            }
            Some((first, last)) => {
                let context = self.compact_model_context(turn).await;
                self.compact_span(match context {
                    Ok((route, image_profile, window)) => CompactedSpan {
                        turn,
                        covered: &covered,
                        replaced_tokens,
                        span: (first.entry, last.entry),
                        first_kept,
                        total,
                        measured,
                        retained,
                        carried,
                        route,
                        image_profile,
                        window,
                    },
                    Err(error) => {
                        return self
                            .report(TurnWork::CompactionSettled {
                                turn,
                                outcome: Err(error),
                            })
                            .await;
                    }
                })
                .await
            }
            None => Err("nothing to compact: no conversation entries.".into()),
        };
        self.report(TurnWork::CompactionSettled { turn, outcome })
            .await;
    }

    /// Clones the live cancellation token for a turn-scoped compaction.
    ///
    /// A turn-scoped pass without driver state is not manual: when the
    /// control cell still holds that turn's live token (a threshold
    /// compaction arriving before any infer materialized the state) the
    /// chain stays bound to it, so a user cancel still stops the chain. A
    /// turn-scoped pass with no live token at all is stale or replaying; it
    /// fails closed instead of running an uncancellable chain no stop can
    /// reach.
    fn compaction_cancel(&self, turn: TurnId) -> Result<CancellationToken, Box<str>> {
        if let Some(state) = self.turns.get(&turn) {
            return Ok(state.cancel.clone());
        }
        if let Some(token) = self
            .control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .token_for(turn)
        {
            return Ok(token);
        }
        Err("compaction cancelled.".into())
    }

    /// Runs compactors in canonical order until one commits a replacement.
    async fn compact_span(
        &self,
        span: CompactedSpan<'_>,
    ) -> Result<dal_core::CompactionSummary, Box<str>> {
        self.run_compactors(span).await
    }

    /// Resolves the route, image profile, and context window for compaction.
    async fn compact_model_context(
        &self,
        turn: Option<TurnId>,
    ) -> Result<(ModelRoute, Option<dal_provider::ImageProfile>, Option<u64>), Box<str>> {
        let selected =
            turn.and_then(|turn| self.turns.get(&turn))
                .and_then(|state| {
                    state.model.as_ref().map(|(route, _)| {
                        (route.clone(), state.image_profile, state.context_window)
                    })
                });
        if let Some(selected) = selected {
            return Ok(selected);
        }
        if let (Some((route, _)), Some(entry)) = (&self.last_model, &self.last_catalog_entry) {
            return Ok((
                route.clone(),
                entry.image_profile,
                entry.context_window.map(u64::from),
            ));
        }
        let resolved = resolve_default(&self.deps.host.shared).await?;
        Ok((
            resolved.route,
            resolved.entry.image_profile,
            resolved.entry.context_window.map(u64::from),
        ))
    }

    /// Runs compactors in canonical order until one commits a shrinking
    /// replacement.
    ///
    /// A committed replacement counts only when it strictly shrinks the
    /// context under the same token accounting the journal entry reports
    /// (`tokens_after < tokens_before`). Non-shrinking output earns a
    /// neutral `compaction_not_shrunk` notice and the chain moves on to the
    /// next registered compactor. When no compactor shrinks, the last
    /// non-shrinking summary is still returned: the core fold owns the
    /// final verdict (`compaction_not_shrunk` on the journal side), so the
    /// existing outcome for an all-failed run is preserved rather than
    /// turned into a turn failure.
    async fn run_compactors(
        &self,
        compacted: CompactedSpan<'_>,
    ) -> Result<dal_core::CompactionSummary, Box<str>> {
        let generation = self.deps.host.shared.generation.borrow().clone();
        // Bind this turn's live token once for the whole chain, through
        // `compaction_cancel`: a token re-read after the fold ends the turn
        // cannot see the fired token, and a stale turn with no live token
        // fails closed instead of running an uncancellable chain.
        let cancel: CancellationToken = match compacted.turn {
            None => CancellationToken::new(),
            Some(turn) => self.compaction_cancel(turn)?,
        };
        let params = RequestParams {
            thinking: self.deps.host.shared.config.thinking(),
            ..RequestParams::default()
        };
        let mut refused: Option<Box<str>> = None;
        let mut nonshrunk: Option<dal_core::CompactionSummary> = None;
        for entry in generation.compactors.entries() {
            // A cancelled turn never starts the next link: the check covers
            // the link already parked, and a result landing after the
            // cancellation is dropped, not classified.
            if cancel.is_cancelled() {
                return Err("compaction cancelled.".into());
            }
            let Some(compactor) = generation.compactor(entry.name.as_ref()) else {
                continue;
            };
            let Some(caller) = compactor_caller(&generation, entry, compacted.turn) else {
                continue;
            };
            let input = crate::ext::compact::CompactInput {
                caller: &caller,
                model: compacted.route.clone(),
                session: self.deps.session,
                instructions: "",
                covered: compacted.covered,
                span: compacted.span,
                first_kept: compacted.first_kept,
                context_window: compacted.window,
                image_profile: compacted.image_profile,
                images_elsewhere: compacted.retained.count,
                image_bytes_elsewhere: compacted.retained.bytes,
                carried: compacted.carried.clone(),
                total_tokens: compacted.total,
                params: params.clone(),
            };
            // Race the compaction against cancellation: the losing future
            // is dropped at the await point (the mechanism of the other
            // session service calls); `biased` makes cancellation win a
            // poll tie.
            let outcome = tokio::select! {
                biased;
                () = cancel.cancelled() => Err(crate::ext::CompactError::Service(
                    crate::error::ServiceError::Cancelled,
                )),
                outcome = compactor
                    .compact(input, Arc::clone(&self.deps.services)) => outcome,
            };
            match outcome {
                Ok(Some(compaction)) => {
                    if compaction_has_reserved_policy_record(&compaction) {
                        refused = Some(
                            "compactor attempted to write the host-owned child policy record."
                                .into(),
                        );
                        continue;
                    }
                    let summary = summarize(
                        &entry.name,
                        compacted.measured.unwrap_or(compacted.total),
                        compacted.replaced_tokens,
                        compacted.first_kept,
                        &compaction,
                    );
                    if summary.tokens_after >= summary.tokens_before {
                        let text = not_shrunk_text(
                            &entry.name,
                            summary.tokens_before,
                            summary.tokens_after,
                        );
                        if let Some(turn) = compacted.turn {
                            self.notice(turn, "compaction_not_shrunk", &text);
                        }
                        nonshrunk = Some(summary);
                        continue;
                    }
                    return Ok(summary);
                }
                Ok(None) => {}
                Err(error) if error.is_cancelled() => {
                    return Err("compaction cancelled.".into());
                }
                Err(error) => {
                    refused = Some(error.to_string().into());
                }
            }
        }
        if let Some(summary) = nonshrunk {
            return Ok(summary);
        }
        Err(refused.unwrap_or_else(|| "no compactor registered.".into()))
    }
}

/// Mints the host caller for one compactor link from the live generation.
///
/// The admitted extension's immutable code identity is captured here, so an
/// in-flight call cannot borrow a later generation's grant. A missing owner
/// or an unparsable extension name skips the link, matching the chain's
/// prior per-link `continue` behavior.
fn compactor_caller(
    generation: &Generation,
    entry: &crate::ext::generation::NamedEntry,
    turn: Option<TurnId>,
) -> Option<Caller> {
    let owner = generation.extensions.get(entry.ext)?;
    let ext = owner.name().parse::<Name>().ok()?;
    Some(
        Caller::new(
            ext,
            owner.origin(),
            owner.inject(),
            owner.state_version(),
            CallerKind::Handler,
            turn,
        )
        .with_extension(owner),
    )
}

/// The notice text for one compactor's non-shrinking output.
fn not_shrunk_text(name: &str, tokens_before: u64, tokens_after: u64) -> String {
    format!(
        "Compactor {name} did not shrink the context ({tokens_before} to {tokens_after} tokens)."
    )
}

fn compaction_has_reserved_policy_record(compaction: &crate::ext::compact::Compaction) -> bool {
    let crate::ext::compact::Replacement::Parts { letters, .. } = &compaction.replacement else {
        return false;
    };
    letters.iter().any(|letter| {
        crate::host::is_child_policy_record(letter.ext.as_str(), letter.kind.as_ref())
    })
}

fn settled_reply_text(entries: &[EntryView]) -> Box<str> {
    for entry in entries.iter().rev() {
        let dal_core::EntryKind::Assistant { content, .. } = &entry.kind else {
            continue;
        };
        let capacity = content
            .iter()
            .map(|block| match block {
                dal_core::Block::Text { text, .. } => text.len(),
                _ => 0,
            })
            .sum();
        let mut text = String::with_capacity(capacity);
        for block in content {
            if let dal_core::Block::Text { text: chunk, .. } = block {
                text.push_str(chunk);
            }
        }
        return text.into_boxed_str();
    }
    "".into()
}

/// One compaction pass: the covered span plus the resolved model context.
struct CompactedSpan<'a> {
    turn: Option<TurnId>,
    covered: &'a [crate::ext::compact::CoveredEntry],
    /// Estimated tokens the cut takes out of the live context; see
    /// [`replaced_context_tokens`].
    replaced_tokens: u64,
    span: (dal_core::EntryId, dal_core::EntryId),
    first_kept: Option<dal_core::EntryId>,
    total: u64,
    measured: Option<u64>,
    retained: RetainedImages,
    carried: Option<Box<str>>,
    route: ModelRoute,
    image_profile: Option<dal_provider::ImageProfile>,
    window: Option<u64>,
}

/// Builds a full-leaf page request.
fn full_page() -> PageReq {
    PageReq {
        limit: std::num::NonZeroU32::new(PageReq::MAX_LIMIT).unwrap_or(std::num::NonZeroU32::MIN),
        before: None,
    }
}

/// Projects leaf entries to covered compaction entries in journal order.
fn covered_entries(items: &[EntryView]) -> Vec<crate::ext::compact::CoveredEntry> {
    let mut out = Vec::new();
    let mut user_open = false;
    for item in items {
        if matches!(item.kind, dal_core::EntryKind::Compaction { .. }) {
            user_open = false;
            continue;
        }
        if let dal_core::EntryKind::Reminder { source, text } = &item.kind
            && source.as_ref() != dal_core::BEFORE_TURN_SOURCE
        {
            let mut entry = crate::ext::compact::CoveredEntry::new(
                item.id,
                false,
                ContextItem::User { parts: Vec::new() },
            );
            entry.note = Some(text.clone());
            entry.estimated_tokens = 0;
            user_open = false;
            out.push(entry);
            continue;
        }
        let Some(content) = crate::session::context::context_items(std::slice::from_ref(item))
            .into_iter()
            .next()
        else {
            user_open = false;
            continue;
        };
        let starts = matches!(content, ContextItem::User { .. }) && !user_open;
        user_open = matches!(content, ContextItem::User { .. });
        out.push(crate::ext::compact::CoveredEntry::new(
            item.id, starts, content,
        ));
    }
    out
}

/// Estimated tokens a cut takes out of the live context.
///
/// `covered` runs from the root of the path to the cut, so it also holds the
/// entries an earlier compaction already replaced. The live context no longer
/// carries those, and the measured total never counted them. It carries that
/// compaction's own replacement in their place, which the new cut supersedes
/// once it passes the compaction entry. Each live entry before the cut counts
/// once: a conversation entry at its covered estimate, a compaction entry at
/// the cost of its replacement (the stored parts bill, or the summary
/// estimate). An earlier compaction after the cut stays in the context and
/// does not count.
fn replaced_context_tokens(
    entries: &[EntryView],
    cut: usize,
    covered: &[crate::ext::compact::CoveredEntry],
) -> u64 {
    let live: HashSet<dal_core::EntryId> = context_entries(entries).map(|entry| entry.id).collect();
    let estimates: HashMap<dal_core::EntryId, u64> = covered
        .iter()
        .map(|entry| (entry.entry, entry.estimated_tokens))
        .collect();
    entries[..cut]
        .iter()
        .filter(|entry| live.contains(&entry.id))
        .map(|entry| match &entry.kind {
            dal_core::EntryKind::Compaction {
                parts,
                parts_tokens,
                ..
            } if !parts.is_empty() => *parts_tokens,
            dal_core::EntryKind::Compaction { summary, .. } => {
                summary.as_deref().map_or(0, dal_core::estimate_text_tokens)
            }
            _ => estimates.get(&entry.id).copied().unwrap_or(0),
        })
        .fold(0, u64::saturating_add)
}

/// The images that stay in the request after a compaction cut.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RetainedImages {
    /// Number of images.
    count: usize,
    /// Bytes the images occupy: decoded length of inline images, stored
    /// length of blobs.
    bytes: u64,
}

impl RetainedImages {
    fn add(self, other: Self) -> Self {
        Self {
            count: self.count.saturating_add(other.count),
            bytes: self.bytes.saturating_add(other.bytes),
        }
    }
}

/// Counts one entry's images and sums their bytes.
fn retained_images(entry: &EntryView) -> RetainedImages {
    let (dal_core::EntryKind::User { parts }
    | dal_core::EntryKind::ToolResult { parts, .. }
    | dal_core::EntryKind::Compaction { parts, .. }) = &entry.kind
    else {
        return RetainedImages::default();
    };
    parts
        .iter()
        .filter_map(|part| match part {
            JournalPart::Image { base64, .. } => Some(decoded_base64_len(base64)),
            JournalPart::ImageBlob { bytes, .. } => Some(*bytes),
            JournalPart::Blob { mime, bytes, .. } if mime.starts_with("image/") => Some(*bytes),
            JournalPart::Blob { .. } | JournalPart::Text { .. } | JournalPart::TextBlob { .. } => {
                None
            }
        })
        .map(|bytes| RetainedImages { count: 1, bytes })
        .fold(RetainedImages::default(), RetainedImages::add)
}

/// The decoded length of a base64 string, without decoding it.
fn decoded_base64_len(base64: &str) -> u64 {
    let padding = base64
        .bytes()
        .rev()
        .take_while(|byte| *byte == b'=')
        .count();
    let length = u64::try_from(base64.len()).unwrap_or(u64::MAX);
    let padding = u64::try_from(padding).unwrap_or(u64::MAX);
    (length / 4)
        .saturating_mul(3)
        .saturating_add((length % 4).saturating_sub(1))
        .saturating_sub(padding.min(2))
}

/// Builds model-visible tool descriptions for section rendering.
fn descriptions(
    generation: &Generation,
    turn_tools: &TurnTools,
    info: &ModelInfo,
) -> Vec<ToolDescription> {
    let overlay = turn_tools.entries().iter().filter(|entry| {
        turn_tools.effective(entry.tool.name(), entry.visibility) == Visibility::Model
    });
    generation
        .tools
        .entries()
        .iter()
        .filter(|entry| turn_tools.permits(&entry.name))
        .filter_map(|entry| generation.tool(&entry.name))
        .map(|(tool, _)| tool.identity(info))
        .chain(overlay.map(|entry| entry.tool.identity(info)))
        .collect()
}

/// Joins the visible prompt sections minus the product-owned base orders,
/// byte-stable with the default assembly: same visibility gate, same
/// order, same separator. An explicit child system prompt takes the place
/// of exactly these sections; every extension section still renders.
fn base_without_product_sections(
    generation: &Generation,
    mode: dal_core::Mode,
    render: impl Fn(usize, &PromptSection) -> Option<String>,
) -> String {
    let mut sections: Vec<(u8, String)> = Vec::new();
    for (owner, section) in generation.prompt_sections.iter() {
        if !section_reaches_model(section.visibility(), mode) {
            continue;
        }
        if matches!(section.order(), PromptOrder::Identity | PromptOrder::D2) {
            continue;
        }
        let text = match section {
            PromptSection::Static { text, .. } => Some(text.to_string()),
            PromptSection::Session { .. } => render(*owner, section),
        };
        if let Some(text) = text {
            sections.push((section.order().position(), text));
        }
    }
    sections.sort_by_key(|(order, _)| *order);
    sections
        .into_iter()
        .map(|(_, text)| text)
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Whether a section visibility reaches the model in this mode; mirrors
/// the default request assembler's gate.
fn section_reaches_model(visibility: Visibility, mode: dal_core::Mode) -> bool {
    matches!(
        (visibility, mode),
        (Visibility::Model, _)
            | (
                Visibility::EvalOnly,
                dal_core::Mode::EvalFirst | dal_core::Mode::EvalOnly
            )
    )
}

/// Drops every image part from one request context for a model that cannot
/// read images.
///
/// Letters degrade to the text descriptions the prompt already shows such a
/// model (their source stays readable at `letter://`), so the turn proceeds
/// on words instead of failing at the provider. Only `image/` mimes go, in
/// both user messages and tool results; text and text blobs stay, and
/// messages left with no parts stay in place for the adapters' existing
/// empty-message handling.
fn strip_images_for_text_model(context: Vec<ContextItem>) -> Vec<ContextItem> {
    context
        .into_iter()
        .map(|item| match item {
            ContextItem::User { parts } => ContextItem::User {
                parts: without_images(parts),
            },
            ContextItem::ToolResult {
                call,
                name,
                is_error,
                parts,
            } => ContextItem::ToolResult {
                call,
                name,
                is_error,
                parts: without_images(parts),
            },
            item @ ContextItem::Assistant { .. } => item,
        })
        .collect()
}

/// Drops the image parts of one message, keeping text and text blobs.
fn without_images(parts: Vec<Part>) -> Vec<Part> {
    parts
        .into_iter()
        .filter(|part| match part {
            Part::Image { mime, .. } | Part::Blob { mime, .. } => !mime.starts_with("image/"),
            Part::Text { .. } => true,
        })
        .collect()
}

/// Digests of the image parts in the request context, history and current
/// input alike. Sections hide a description behind a name-only line only for
/// digests among these. Text spills stay out: only `image/` mimes count.
fn context_image_digests(context: &[ContextItem]) -> Vec<BlobId> {
    let mut digests = Vec::new();
    for item in context {
        let ContextItem::User { parts } = item else {
            continue;
        };
        for part in parts {
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
    }
    digests
}

/// Renders one prompt section through its registered body.
fn render_section(
    deps: &DriverDeps,
    owner: &crate::ext::Extension,
    section: &PromptSection,
    turn: TurnId,
    tools: &[ToolDescription],
    model: &ModelInfo,
    delivered: &[BlobId],
) -> Option<String> {
    let PromptSection::Session { section, .. } = section else {
        return None;
    };
    let name = Name::parse(owner.name()).ok()?;
    let state = deps.shared.owner_state(&name);
    let caller = Caller::new(
        name,
        owner.origin(),
        dal_core::ext::ServiceSet::EMPTY,
        owner.state_version(),
        CallerKind::Handler,
        Some(turn),
    )
    .with_extension(owner);
    let cx = SectionCx {
        state: state.as_deref(),
        services: Arc::clone(&deps.services),
        caller: &caller,
        session: deps.session,
        turn: Some(turn),
        snapshot: deps.generation,
        tools,
        model: Some(model),
        delivered_images: delivered,
        instructions: None,
        system_md: None,
    };
    section.render(&cx)
}

/// Derives the spec-cache model id from the resolved provider and route.
#[expect(
    clippy::expect_used,
    reason = "fallback identifiers contain only validated or sanitized components"
)]
fn cache_model_id(provider: &str, route: &ModelRoute) -> dal_core::ModelId {
    let candidate = match route {
        ModelRoute::Api { model, .. } => format!("{provider}/{model}"),
        ModelRoute::Synthetic { id } | ModelRoute::Harness { id } => id.to_string(),
    };
    if let Ok(id) = dal_core::ModelId::parse(&candidate) {
        return id;
    }
    let clean = |part: &str| {
        let mut text: String = part
            .bytes()
            .map(|byte| {
                if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-') {
                    byte as char
                } else {
                    '-'
                }
            })
            .collect();
        text.truncate(64);
        let trimmed = text.trim_matches('-').to_owned();
        if trimmed.is_empty() {
            String::from("unknown")
        } else {
            trimmed
        }
    };
    let candidate = match route {
        ModelRoute::Api { model, .. } => format!("{}/{}", clean(provider), clean(model)),
        ModelRoute::Synthetic { id } | ModelRoute::Harness { id } => id.to_string(),
    };
    dal_core::ModelId::parse(&candidate).expect("sanitized model id parses")
}

/// Summarizes one committed compaction for the journal.
fn summarize(
    compactor: &str,
    total: u64,
    replaced_tokens: u64,
    first_kept: Option<dal_core::EntryId>,
    compaction: &crate::ext::compact::Compaction,
) -> dal_core::CompactionSummary {
    let summary = compaction.summary_text().map(str::to_owned);
    let summary_tokens = summary.as_deref().map_or(0, dal_core::estimate_text_tokens);
    let replay = match compaction.history() {
        Some(history) => {
            let items: Vec<&str> = history
                .items
                .iter()
                .map(dal_core::RawJson::as_str)
                .collect();
            let raw = format!("[{}]", items.join(","));
            dal_core::RawJson::parse(&raw).ok()
        }
        None => None,
    };
    let (parts, parts_tokens, letters) = match &compaction.replacement {
        crate::ext::compact::Replacement::Parts {
            parts,
            letters,
            parts_tokens,
        } => (
            journal_parts(parts),
            *parts_tokens,
            letters
                .iter()
                .map(|letter| CompactionExtRecord {
                    ext: letter.ext.clone(),
                    kind: letter.kind.clone(),
                    body: letter.body.clone(),
                })
                .collect(),
        ),
        crate::ext::compact::Replacement::Text(_) | crate::ext::compact::Replacement::Native(_) => {
            (Vec::new(), 0, Vec::new())
        }
    };
    let replacement_tokens = if parts.is_empty() {
        summary_tokens
    } else {
        parts_tokens
    };
    compactor_name(
        compactor,
        total,
        replaced_tokens,
        replacement_tokens,
        CompactionInputs {
            summary,
            replay,
            parts,
            parts_tokens,
            letters,
            first_kept,
            usage: compaction.usage,
        },
    )
}

fn journal_parts(parts: &[Part]) -> Vec<JournalPart> {
    use base64::Engine as _;

    parts
        .iter()
        .map(|part| match part {
            Part::Text { text } => JournalPart::Text { text: text.clone() },
            Part::Image { mime, bytes } => JournalPart::Image {
                mime: mime.clone(),
                base64: base64::engine::general_purpose::STANDARD
                    .encode(bytes)
                    .into(),
            },
            Part::Blob {
                blob_id,
                mime,
                bytes,
            } => JournalPart::Blob {
                mime: mime.clone(),
                blob: blob_id.to_string().into(),
                bytes: *bytes,
            },
        })
        .collect()
}

/// Replacement payload assembled from one committed compaction.
struct CompactionInputs {
    summary: Option<String>,
    replay: Option<RawJson>,
    parts: Vec<JournalPart>,
    parts_tokens: u64,
    letters: Vec<CompactionExtRecord>,
    first_kept: Option<dal_core::EntryId>,
    usage: Option<Usage>,
}

/// Builds the journal summary with the caller's compactor attribution.
#[expect(
    clippy::expect_used,
    reason = "the compactor name came from the validated generation registry"
)]
fn compactor_name(
    compactor: &str,
    total: u64,
    replaced_tokens: u64,
    replacement_tokens: u64,
    inputs: CompactionInputs,
) -> dal_core::CompactionSummary {
    let CompactionInputs {
        summary,
        replay,
        parts,
        parts_tokens,
        letters,
        first_kept,
        usage,
    } = inputs;
    dal_core::CompactionSummary {
        compactor: Name::parse(compactor).expect("registered compactor name parses"),
        tokens_before: total,
        tokens_after: total.saturating_sub(replaced_tokens) + replacement_tokens,
        summary: summary.map(Into::into),
        first_kept,
        replay,
        usage,
        parts,
        parts_tokens,
        letters,
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use super::*;
    use crate::ext::ExtRecord;
    use dal_core::EntryId;

    fn letter(kind: &str) -> ExtRecord {
        ExtRecord {
            ext: Name::parse(crate::host::CHILD_POLICY_EXT).expect("reserved name parses"),
            kind: kind.into(),
            body: RawJson::parse("{}").expect("record body parses"),
        }
    }

    fn parts(letter: ExtRecord) -> crate::ext::compact::Compaction {
        let entry = EntryId::new(NonZeroU64::MIN);
        crate::ext::compact::Compaction {
            span: (entry, entry),
            replacement: crate::ext::compact::Replacement::Parts {
                parts: Vec::new(),
                letters: vec![letter],
                parts_tokens: 0,
            },
            usage: None,
        }
    }

    #[test]
    fn compaction_rejects_host_policy_identity_but_allows_ordinary_record_kind() {
        assert!(compaction_has_reserved_policy_record(&parts(letter(
            crate::host::CHILD_POLICY_KIND,
        ))));
        assert!(!compaction_has_reserved_policy_record(&parts(letter(
            "ordinary",
        ))));
    }

    fn chat(model: &str, provider: Option<&str>) -> ModelRoute {
        ModelRoute::Api {
            family: Family::Chat,
            model: model.into(),
            provider: provider.map(Into::into),
        }
    }

    #[test]
    fn a_provider_less_request_keeps_the_pin_its_resolved_route_carries() {
        // Round one resolves a provider-less request to a route that names
        // its provider; the next batch re-sends the provider-less request.
        let resolved = chat("gpt-4o", Some("acme"));
        assert_ne!(resolved, chat("gpt-4o", None), "exact equality differs");
        assert!(provider_pin_holds(Some(&resolved), &chat("gpt-4o", None)));
        assert!(provider_pin_holds(None, &chat("gpt-4o", None)));
        assert!(provider_pin_holds(
            Some(&resolved),
            &chat("gpt-4o", Some("acme"))
        ));
    }

    #[test]
    fn a_different_model_family_or_provider_drops_the_pin() {
        let resolved = chat("gpt-4o", Some("acme"));
        assert!(!provider_pin_holds(
            Some(&resolved),
            &chat("gpt-4o", Some("beta"))
        ));
        assert!(!provider_pin_holds(
            Some(&resolved),
            &chat("gpt-4o-mini", None)
        ));
        let responses = ModelRoute::Api {
            family: Family::Responses,
            model: "gpt-4o".into(),
            provider: None,
        };
        assert!(!provider_pin_holds(Some(&resolved), &responses));
        let synthetic = ModelRoute::synthetic("acme/gpt-4o").expect("synthetic route");
        assert!(!provider_pin_holds(Some(&resolved), &synthetic));
    }

    const FOOTER: &str =
        "Full output: /data/sessions/w/s/jobs/0190aaaa-bbbb-7ccc-8ddd-eeeeffff0000.log";

    #[test]
    fn a_preview_keeps_a_short_text_whole() {
        let text = format!("short\n{FOOTER}");
        assert_eq!(&*Driver::bounded_preview(&text), text);
    }

    #[test]
    fn a_preview_without_a_footer_is_the_head_cut_at_a_char_boundary() {
        let preview = Driver::bounded_preview(&"x".repeat(5000));
        assert_eq!(preview.len(), 4096);
        let preview = Driver::bounded_preview(&"é".repeat(3000));
        assert_eq!(preview.len(), 4096);
        let preview = Driver::bounded_preview(&format!("a{}", "é".repeat(3000)));
        assert_eq!(preview.len(), 4095, "never splits a char to reach the cap");
    }

    #[test]
    fn a_footer_the_cut_would_drop_is_carried_inside_the_bound() {
        // A full tail of command output, then the tool's footer and status.
        let text = format!("{}\n{FOOTER}\nCommand exited with code 1", "x".repeat(4096));
        let preview = Driver::bounded_preview(&text);
        assert!(preview.len() <= 4096, "{}", preview.len());
        assert!(preview.starts_with("xxxx"));
        assert_eq!(preview.lines().last(), Some(FOOTER));
        assert!(!preview.contains("Command exited"), "{preview}");
    }

    #[test]
    fn a_carried_footer_never_splits_a_char_of_the_content() {
        let text = format!("{}\n{FOOTER}", "é".repeat(2500));
        let preview = Driver::bounded_preview(&text);
        assert!(preview.len() <= 4096, "{}", preview.len());
        assert_eq!(preview.lines().last(), Some(FOOTER));
        assert!(preview.starts_with("éé"));
    }

    #[test]
    fn command_output_that_looks_like_a_footer_never_replaces_the_real_one() {
        let text = format!(
            "Full output: /forged.log\n{}\n{FOOTER}\nCommand exited with code 2",
            "x".repeat(5000)
        );
        let preview = Driver::bounded_preview(&text);
        assert!(preview.len() <= 4096, "{}", preview.len());
        assert_eq!(preview.lines().last(), Some(FOOTER));
    }

    #[test]
    fn a_footer_already_inside_the_cut_is_left_where_it_is() {
        let text = format!("{FOOTER}\n{}", "y".repeat(5000));
        let preview = Driver::bounded_preview(&text);
        assert_eq!(&*preview, &text[..4096]);
    }

    fn id(value: u64) -> EntryId {
        EntryId::new(NonZeroU64::new(value).expect("entry id is nonzero"))
    }

    fn view(value: u64, kind: dal_core::EntryKind) -> EntryView {
        EntryView {
            id: id(value),
            parent: value
                .checked_sub(1)
                .and_then(NonZeroU64::new)
                .map(EntryId::new),
            kind,
        }
    }

    /// A user entry whose estimate grows with `length`.
    fn user(value: u64, length: usize) -> EntryView {
        view(
            value,
            dal_core::EntryKind::User {
                parts: vec![JournalPart::Text {
                    text: "x".repeat(length).into(),
                }],
            },
        )
    }

    /// A compaction that replaces everything before `first_kept`.
    fn compaction(
        value: u64,
        first_kept: u64,
        summary: Option<Box<str>>,
        parts: Vec<JournalPart>,
        parts_tokens: u64,
    ) -> EntryView {
        view(
            value,
            dal_core::EntryKind::Compaction {
                summary,
                first_kept: Some(id(first_kept)),
                tokens_before: 0,
                replay: None,
                usage: None,
                parts,
                parts_tokens,
            },
        )
    }

    /// A compaction whose replacement is the text `summary`.
    fn summarized(value: u64, first_kept: u64, summary: &str) -> EntryView {
        compaction(value, first_kept, Some(summary.into()), Vec::new(), 0)
    }

    /// A compaction whose replacement is one part billed at `parts_tokens`.
    fn paged(value: u64, first_kept: u64, parts_tokens: u64) -> EntryView {
        let page = vec![JournalPart::Text {
            text: "page".into(),
        }];
        compaction(value, first_kept, None, page, parts_tokens)
    }

    /// The covered estimate of one entry.
    fn estimate(covered: &[crate::ext::compact::CoveredEntry], value: u64) -> u64 {
        covered
            .iter()
            .find(|entry| entry.entry == id(value))
            .map(|entry| entry.estimated_tokens)
            .expect("the entry is covered")
    }

    /// The tokens the cut at entry `kept` takes out of the live context.
    fn replaced(entries: &[EntryView], kept: u64) -> (u64, Vec<crate::ext::compact::CoveredEntry>) {
        let cut = entries
            .iter()
            .position(|entry| entry.id == id(kept))
            .expect("the cut entry exists");
        let covered = covered_entries(&entries[..cut]);
        (replaced_context_tokens(entries, cut, &covered), covered)
    }

    #[test]
    fn a_cut_over_a_path_without_compaction_replaces_every_covered_estimate() {
        let entries = [user(1, 700), user(2, 1_400), user(3, 2_100)];
        let (tokens, covered) = replaced(&entries, 3);
        assert_eq!(tokens, estimate(&covered, 1) + estimate(&covered, 2));
        assert!(tokens > 0);
    }

    #[test]
    fn a_cut_past_an_earlier_compaction_replaces_its_parts_and_not_what_it_replaced() {
        let entries = [
            user(1, 700),
            summarized(2, 1, &"s".repeat(700)),
            user(3, 1_400),
            paged(4, 3, 1_000),
            user(5, 2_100),
            user(6, 700),
        ];
        let (tokens, covered) = replaced(&entries, 6);
        assert_eq!(
            tokens,
            estimate(&covered, 3) + 1_000 + estimate(&covered, 5),
            "the live compaction counts by its parts bill, once; the entry it replaced and the \
             superseded compaction before it are out of the context"
        );
        let summed: u64 = covered.iter().map(|entry| entry.estimated_tokens).sum();
        assert_ne!(
            tokens, summed,
            "the covered estimates alone are not the cost"
        );
    }

    #[test]
    fn a_cut_past_an_earlier_summary_compaction_replaces_its_summary_estimate() {
        let summary = "s".repeat(700);
        let entries = [
            user(1, 700),
            user(2, 1_400),
            summarized(3, 2, &summary),
            user(4, 2_100),
            user(5, 700),
        ];
        let (tokens, covered) = replaced(&entries, 5);
        let summary_tokens = dal_core::estimate_text_tokens(&summary);
        assert_eq!(
            tokens,
            estimate(&covered, 2) + summary_tokens + estimate(&covered, 4)
        );
    }

    #[test]
    fn a_cut_before_an_earlier_compaction_replaces_nothing_it_already_replaced() {
        let entries = [
            user(1, 2_100),
            user(2, 1_400),
            paged(3, 2, 1_000),
            user(4, 700),
        ];
        let (tokens, covered) = replaced(&entries, 2);
        assert_eq!(
            covered.len(),
            1,
            "the cut still covers the path from the root"
        );
        assert!(estimate(&covered, 1) > 0);
        assert_eq!(
            tokens, 0,
            "the compaction stays in the context and the covered entry is already out of it"
        );
    }

    #[test]
    fn an_earlier_compaction_kept_before_the_cut_replaces_once_with_its_covered_entries() {
        let summary = "s".repeat(700);
        let entries = [
            user(1, 700),
            summarized(2, 1, &summary),
            user(3, 1_400),
            user(4, 2_100),
            paged(5, 1, 1_000),
            user(6, 700),
        ];
        let (tokens, covered) = replaced(&entries, 4);
        let summary_tokens = dal_core::estimate_text_tokens(&summary);
        assert_eq!(
            tokens,
            estimate(&covered, 1) + summary_tokens + estimate(&covered, 3),
            "the older compaction is still in the context, and the newer one is past the cut"
        );
    }

    #[test]
    fn a_committed_parts_replacement_is_billed_at_its_parts_tokens() {
        let compaction = crate::ext::compact::Compaction {
            span: (id(1), id(2)),
            replacement: crate::ext::compact::Replacement::Parts {
                parts: vec![Part::Text {
                    text: "page".into(),
                }],
                letters: Vec::new(),
                parts_tokens: 700,
            },
            usage: None,
        };
        let summary = summarize("capture", 10_000, 4_000, None, &compaction);
        assert_eq!(summary.tokens_before, 10_000);
        assert_eq!(summary.tokens_after, 6_000 + 700);
        let past_the_total = summarize("capture", 3_000, 4_000, None, &compaction);
        assert_eq!(past_the_total.tokens_after, 700);
    }
}
