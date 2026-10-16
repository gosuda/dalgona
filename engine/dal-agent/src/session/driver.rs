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
    CallId, CompactLimits, CompactionExtRecord, ContextItem, EntryKind, EntryView, Family, Gen,
    InferFailure, Inference, JournalPart, ModelInfo, ModelRequest, ModelRoute, Name, PageReq, Part,
    Purpose, RawJson, RequestParams, ResolvedCall, SessionId, Settled, SettledOutcome, Stop,
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
use crate::host::HostState;
use crate::jobs::JobTable;
use crate::session::actor::{DriverPorts, TurnBatch, TurnWork, WatchOutcome};
use crate::session::backend::Backend;
use crate::session::context::{
    DeferredTool, LEAF_WINDOW, StreamCall, ToolBoundaries, api_family, cache_key, compaction_count,
    context_entries, context_items, model_info_for, recent_leaf, resolve_calls, search_surface,
    system_prompt, tool_list,
};
use crate::session::control::ControlCell;
use crate::session::dispatch::{
    DispatchCtx, GrantLedger, ReadyCall, echo_resolved, plan_units, run_unit,
};
use crate::session::projection::SnapshotArgs;
use crate::session::shared::{ChildPromptPolicy, Shared};
use crate::session::tasks::SessionTasks;
use crate::session::turn::{RequestDeps, StreamConverter, infer_stream};
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
                return;
            }
            self.batch(batch).await;
        }
    }

    /// Handles one actor batch: effects in order, then ask waiters.
    async fn batch(&mut self, mut batch: TurnBatch) {
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
                    self.infer(plan.turn, plan.params, &batch).await;
                }
                dal_core::Effect::Dispatch { turn, units } => {
                    self.dispatch(turn, units, &batch).await;
                }
                dal_core::Effect::Compact { turn, first_kept } => {
                    if let Some(turn) = turn {
                        self.bind_turn_step_cap(turn);
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
                generation,
                tools,
                deferred_search: Arc::from([]),
                model: None,
                provider: None,
                image_profile: None,
                context_window: None,
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
        self.report(TurnWork::Cancelled { turn, stop }).await;
    }

    /// Runs one provider request for `turn` and steps its lifecycle.
    async fn infer(&mut self, turn: TurnId, params: RequestParams, batch: &TurnBatch) {
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

    /// Resolves the route for `turn`, caches the resolved row, and reports
    /// the request opening; `None` after the failure is already reported.
    async fn resolve_request(
        &mut self,
        turn: TurnId,
        batch: &TurnBatch,
    ) -> Option<ResolvedRequest> {
        if let Some(route) = batch.model.clone() {
            let family = batch
                .family
                .or_else(|| api_family(&route))
                .unwrap_or(Family::Chat);
            let state = self.turn(turn);
            if state
                .model
                .as_ref()
                .is_some_and(|(current, _)| current != &route)
            {
                state.provider = None;
            }
            state.model = Some((route, family));
        }
        let resolved = match self.resolve_model(turn).await {
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
        let deps = RequestDeps {
            session,
            host,
            script: Some(script),
        };
        let stream = infer_stream(&deps, request, &cancel).await;
        (stream, cancel)
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
        if self.settle_watch(turn, event, delivery, &cancel).await && resolve {
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

impl Driver {
    /// Resolves the request route through the catalog, caching it for commands.
    async fn resolve_model(&self, turn: TurnId) -> Result<ResolvedRequest, Box<str>> {
        let Some(route) = self.turn_model(turn) else {
            return self.resolve_default_model().await;
        };
        let provider = self
            .turns
            .get(&turn)
            .and_then(|state| state.provider.as_deref());
        match (&route, provider) {
            (ModelRoute::Api { model, .. }, Some(provider)) => {
                self.resolve_reference(&format!("{provider}/{model}"), None)
                    .await
            }
            _ => {
                self.resolve_reference(&crate::host::ops::request_reference(&route), Some(&route))
                    .await
            }
        }
    }

    async fn resolve_default_model(&self) -> Result<ResolvedRequest, Box<str>> {
        let reference = self
            .deps
            .host
            .shared
            .config
            .model()
            .unwrap_or("")
            .to_owned();
        self.resolve_reference(&reference, None).await
    }

    /// Resolves `reference`, or the model `route` names when one is given.
    async fn resolve_reference(
        &self,
        reference: &str,
        route: Option<&ModelRoute>,
    ) -> Result<ResolvedRequest, Box<str>> {
        if reference.is_empty() {
            return Err("no model configured: set dal.toml [models] default.".into());
        }
        if let Some(found) = self.synthetic(reference) {
            return Ok(ResolvedRequest {
                family: Family::Chat,
                provider: found.provider(),
                entry: found.entry(),
                route: found.route(),
            });
        }
        let providers = &self.deps.host.shared.providers;
        let catalog = providers
            .catalog()
            .await
            .map_err(|error| format!("provider catalog unavailable: {error}").into_boxed_str())?;
        self.deps.host.shared.cache_catalog(catalog.clone());
        let aliases: Vec<(Box<str>, Box<str>)> = self
            .deps
            .host
            .shared
            .config
            .aliases()
            .iter()
            .map(|(name, target)| (name.clone(), target.clone()))
            .collect();
        let resolved = match route {
            Some(route) => dal_provider::resolve_route(&catalog, &aliases, route),
            None => dal_provider::resolve(&catalog, &aliases, reference),
        }
        .map_err(|error| format!("model {reference} did not resolve: {error}").into_boxed_str())?;
        Ok(ResolvedRequest {
            family: api_family(&resolved.route).unwrap_or(Family::Chat),
            provider: resolved.provider.clone(),
            entry: resolved.entry.clone(),
            route: resolved.route,
        })
    }

    /// Resolves a registered synthetic model; harness ids never match.
    fn synthetic(&self, reference: &str) -> Option<crate::ext::synthetic::Found> {
        if reference.starts_with("dalgon/") {
            return None;
        }
        crate::ext::synthetic::find(&self.deps.host.shared, reference)
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
        let info = model_info_for(&resolved.entry, &resolved.route, thinking);
        let model_id = cache_model_id(&resolved.provider, &resolved.route);
        let mur = self.turn(turn).round;
        self.turn(turn).round = mur + 1;
        let params = self
            .hook_params(turn, &generation, &info, params, mur)
            .await;
        let system = self.system(turn, &generation, &turn_tools, mode, &info);
        let surface = search_surface(&resolved.provider, &resolved.route);
        let (model_tools, deferred_search) =
            tool_list(&generation, &turn_tools, &info, &model_id, mode, surface);
        self.turn(turn).deferred_search = deferred_search;
        let tools: Arc<[dal_core::ModelToolSpec]> = model_tools.into();
        let visibility = self.tool_boundaries.observe(&tools);
        let entries = self.deps.shared.leaf_entries();
        let cache_key = cache_key(self.deps.session, compaction_count(&entries), visibility);
        let context: Arc<[ContextItem]> = context_items(recent_leaf(&entries)).into();
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
    async fn hook_params(
        &self,
        turn: TurnId,
        generation: &Generation,
        info: &ModelInfo,
        params: RequestParams,
        round: u32,
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
            cancel: &self.deps.cancel,
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

    fn bounded_preview(text: &str) -> Box<str> {
        const MAX_BYTES: usize = 4096;
        if text.len() <= MAX_BYTES {
            return text.into();
        }
        let end = text
            .char_indices()
            .map(|(index, _)| index)
            .take_while(|index| *index <= MAX_BYTES)
            .last()
            .unwrap_or(0);
        text[..end].into()
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
            Some((first, last)) => {
                let context = self.compact_model_context(turn).await;
                self.compact_span(match context {
                    Ok((route, image_profile, window)) => CompactedSpan {
                        turn,
                        covered: &covered,
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
        let resolved = self.resolve_default_model().await?;
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
                        compacted.covered,
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

/// Renders one prompt section through its registered body.
fn render_section(
    deps: &DriverDeps,
    owner: &crate::ext::Extension,
    section: &PromptSection,
    turn: TurnId,
    tools: &[ToolDescription],
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
    covered: &[crate::ext::compact::CoveredEntry],
    first_kept: Option<dal_core::EntryId>,
    compaction: &crate::ext::compact::Compaction,
) -> dal_core::CompactionSummary {
    let covered_tokens: u64 = covered.iter().map(|entry| entry.estimated_tokens).sum();
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
        covered_tokens,
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
    covered_tokens: u64,
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
        tokens_after: total.saturating_sub(covered_tokens) + replacement_tokens,
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
}
