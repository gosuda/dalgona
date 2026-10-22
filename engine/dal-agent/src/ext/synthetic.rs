//! Synthetic models: resolution, the id stack, the handler run, and the
//! private-tool loop behind `ModelCx::forward`.
//!
//! A registered model id resolves before API-family routing. The run's
//! lineage (the id stack, the journaling target, the enclosing budget
//! ledger) travels in a task-local so an inner `Services::infer` reaches the
//! same dispatch with the same stack; every stream the run hands out polls
//! inside that scope. A run outside a session (the router relay) has no
//! journaling target and no member sessions.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use dal_core::{
    CallId, ContextItem, InferFailure, Inference, InferredPurpose, JournalPart, ModelId, ModelInfo,
    ModelPrice, ModelRequest, ModelRoute, ModelToolSpec, Owner, Part, PrivateEffectId,
    PrivateLoopId, PrivateOutcome, RawJson, ScopeSpec, ThinkingLevel, TurnId, Usage, Workspace,
    check_synthetic_chain,
};
use dal_provider::{
    CatalogEntry, EventStream, Listing, ProviderError, StreamEvent as ProviderEvent,
    ThinkingSupport, ToolArgs, ToolSupport,
};
use futures::stream;
use tokio_util::sync::{CancellationToken, DropGuard};

use super::generation::Generation;
use super::scope::{Ledger, PriceFn, Scope, ScopeError};
use super::tool::{PrivateCx, Tool, ToolCall, ToolOutcome};
use super::{
    BoxFuture, Caller, CallerKind, ModelCx, ModelCxRuntime, ModelError, ModelRecord, PrivateTool,
    Services,
};
use crate::host::HostShared;
use crate::host::ops::request_reference;
use crate::session::SessionHandle;
use crate::session::contain::{contained, crashed_tool};
use crate::session::continuation::ContinuationSlot;
use crate::session::turn::{RequestDeps, StreamConverter, infer_stream};

mod relay;

/// Private tool rounds one forward may run.
const PRIVATE_ROUNDS: u32 = 8;

tokio::task_local! {
    static LINEAGE: Lineage;
}

/// What an inner call inherits from the synthetic run that made it.
#[derive(Clone, Default)]
pub(crate) struct Lineage {
    chain: Vec<Box<str>>,
    who: Option<Owner>,
    journal: Option<SessionHandle>,
    ledger: Option<Arc<Ledger>>,
}

impl Lineage {
    pub(crate) fn ledger(&self) -> Option<Arc<Ledger>> {
        self.ledger.clone()
    }

    pub(crate) fn with_ledger(self, ledger: Arc<Ledger>) -> Self {
        Self {
            ledger: Some(ledger),
            ..self
        }
    }
}

pub(crate) fn lineage() -> Lineage {
    LINEAGE.try_with(Clone::clone).unwrap_or_default()
}

/// Runs `future` with `lineage` as the ambient run lineage.
pub(crate) fn enter<F: std::future::Future>(
    lineage: Lineage,
    future: F,
) -> impl std::future::Future<Output = F::Output> {
    LINEAGE.scope(lineage, future)
}

/// One resolved synthetic model with the generation that published it.
pub(crate) struct Found {
    generation: Arc<Generation>,
    ext: usize,
    record: ModelRecord,
}

/// Resolves `reference` (one alias expansion first) to a registered model.
pub(crate) fn find(shared: &HostShared, reference: &str) -> Option<Found> {
    let target = shared
        .config
        .aliases()
        .get(reference)
        .map_or(reference, AsRef::as_ref);
    let generation = shared.generation.borrow().clone();
    let (ext, record) = {
        let (ext, record) = generation.model(target)?;
        (ext, record.clone())
    };
    Some(Found {
        generation,
        ext,
        record,
    })
}

/// Resolves a request route to a registered model, when it names one.
pub(crate) fn find_route(shared: &HostShared, route: &ModelRoute) -> Option<Found> {
    match route {
        ModelRoute::Synthetic { id } => find(shared, id),
        ModelRoute::Api { .. } | ModelRoute::Harness { .. } => None,
    }
}

impl Found {
    /// The synthetic route of the found model.
    pub(crate) fn route(&self) -> ModelRoute {
        ModelRoute::Synthetic {
            id: self.record.id.as_str().into(),
        }
    }

    /// The catalog row that stands for the model's declared capabilities.
    pub(crate) fn entry(&self) -> CatalogEntry {
        entry_for(&self.record)
    }

    /// The namespace part of the model id.
    pub(crate) fn provider(&self) -> Box<str> {
        namespace(self.record.id.as_str()).into()
    }
}

/// The namespace of a synthetic model id: the segment before the first `/`,
/// or the whole id when it has none. The one owner of that split: the
/// catalog row ([`entry_for`]), the found model, its listing row, and its
/// running identity all read the provider from here, so they agree by
/// construction.
fn namespace(id: &str) -> &str {
    id.split_once('/').map_or(id, |(namespace, _)| namespace)
}

fn entry_for(record: &ModelRecord) -> CatalogEntry {
    let caps = &record.caps;
    let id = record.id.as_str();
    let name = id.split_once('/').map_or(id, |(_, name)| name);
    CatalogEntry {
        provider: namespace(id).into(),
        id: name.into(),
        display: id.into(),
        listing: Listing::Listed,
        context_window: caps.context_window,
        max_output: None,
        thinking: ThinkingSupport::OpenAi {
            accepted: caps.thinking.to_vec(),
            none_supported: caps.thinking.contains(&ThinkingLevel::Off),
        },
        image_input: caps.image_input,
        image_profile: None,
        remote_compact: false,
        supports_reasoning_summaries: false,
        tool_support: if caps.tool_use {
            ToolSupport::Any
        } else {
            ToolSupport::None
        },
        temperature_allowed: false,
        display_supported: false,
        custom_grammar: caps.custom_grammar,
        premium_multiplier: None,
        price: None,
    }
}

/// Lists every registered model as display metadata.
pub(crate) fn listed(generation: &Generation) -> Vec<ModelInfo> {
    generation
        .extensions
        .iter()
        .flat_map(crate::ext::builder::Extension::models)
        .map(|record| ModelInfo {
            route: ModelRoute::Synthetic {
                id: record.id.as_str().into(),
            },
            name: record.id.as_str().into(),
            provider: namespace(record.id.as_str()).into(),
            caps: record.caps.clone(),
        })
        .collect()
}

fn check_chain(chain: &[Box<str>]) -> Result<(), ModelError> {
    let routes: Vec<ModelRoute> = chain
        .iter()
        .map(|id| ModelRoute::Synthetic { id: id.clone() })
        .collect();
    let ids = || {
        chain
            .iter()
            .filter_map(|id| ModelId::parse(id).ok())
            .collect()
    };
    match check_synthetic_chain(&routes) {
        Ok(()) => Ok(()),
        Err(InferFailure::SyntheticCycle { .. }) => {
            Err(ModelError::SyntheticCycle { chain: ids() })
        }
        Err(_) => Err(ModelError::SyntheticDepth { chain: ids() }),
    }
}

fn failure_of(error: ModelError) -> InferFailure {
    let routes = |chain: &[ModelId]| {
        chain
            .iter()
            .map(|id| ModelRoute::Synthetic {
                id: id.as_str().into(),
            })
            .collect::<Vec<_>>()
    };
    match error {
        ModelError::SyntheticCycle { chain } => InferFailure::SyntheticCycle {
            chain: routes(&chain),
        },
        ModelError::SyntheticDepth { chain } => InferFailure::SyntheticDepth {
            chain: routes(&chain),
        },
        ModelError::SecondForward | ModelError::PrivateRounds => InferFailure::Fatal {
            message: error.to_string().into(),
            fix: None,
        },
    }
}

fn failed(failure: InferFailure) -> EventStream {
    let items = vec![Err(ProviderError::Synthetic(failure))];
    EventStream::new(stream::iter(items), || {})
}

/// Journals each usage an inner call reports while a session-backed
/// synthetic run is on the stack; every other stream passes through.
pub(crate) fn observe(inner: EventStream) -> EventStream {
    let target = LINEAGE
        .try_with(|lineage| {
            Some((
                lineage.journal.clone()?,
                lineage.who.clone()?,
                lineage.chain.last()?.clone(),
            ))
        })
        .ok()
        .flatten();
    let Some((handle, who, id)) = target else {
        return inner;
    };
    let source = stream::unfold(
        (inner, handle, who, id),
        |(mut inner, handle, who, id)| async move {
            let item = inner.next().await?;
            if let Ok(ProviderEvent::Usage { usage }) = &item {
                handle
                    .inferred(
                        who.clone(),
                        InferredPurpose::Synthetic { id: id.clone() },
                        *usage,
                    )
                    .await;
            }
            Some((item, (inner, handle, who, id)))
        },
    );
    EventStream::new(source, || {})
}

fn scoped(inner: EventStream, lineage: Lineage, guard: DropGuard) -> EventStream {
    let source = stream::unfold(
        (inner, lineage, guard),
        |(mut inner, lineage, guard)| async move {
            let item = LINEAGE.scope(lineage.clone(), inner.next()).await?;
            Some((item, (inner, lineage, guard)))
        },
    );
    EventStream::new(source, || {})
}

/// Runs one registered model for `request` and returns its stream.
pub(crate) async fn open(
    deps: &RequestDeps,
    found: Found,
    request: ModelRequest,
    cancel: &CancellationToken,
) -> EventStream {
    let outer = lineage();
    let id: Box<str> = found.record.id.as_str().into();
    let mut chain = outer.chain.clone();
    chain.push(id);
    if let Err(error) = check_chain(&chain) {
        return failed(failure_of(error));
    }
    let shared = &deps.host.shared;
    let session = session_parts(deps);
    let extension = &found.generation.extensions[found.ext];
    // Share the Origin renderer with the private-outcomes read filter, so
    // owner equality cannot drift between write and read.
    let who = Owner::Extension {
        name: extension.name().into(),
        origin: crate::ext::services::origin_text_shared(extension.origin()).into(),
    };
    let next = Lineage {
        chain,
        who: Some(who),
        journal: session.as_ref().map(|parts| parts.handle.clone()),
        ledger: outer.ledger,
    };
    let workspace = match session.as_ref() {
        Some(parts) => parts.workspace.clone(),
        None => match relay::workspace(shared) {
            Ok(workspace) => workspace,
            Err(error) => {
                return failed(InferFailure::Fatal {
                    message: error.to_string().into(),
                    fix: None,
                });
            }
        },
    };
    let services: Arc<dyn Services> = match &session {
        Some(parts) => Arc::clone(&parts.services),
        None => Arc::new(relay::RelayServices::new(deps.clone())),
    };
    let name = match dal_core::Name::parse(extension.name()) {
        Ok(name) => name,
        Err(error) => {
            return failed(InferFailure::Fatal {
                message: error.to_string().into(),
                fix: None,
            });
        }
    };
    let turn = deps
        .script
        .as_ref()
        .and_then(|script| script.captured_turn());
    let mint = |kind| {
        let caller = Caller::new(
            name.clone(),
            extension.origin(),
            extension.inject(),
            extension.state_version(),
            kind,
            turn,
        )
        .with_extension(extension);
        // Carry the invocation owner's Weak (turn or relay-stream holder)
        // into every synthetic caller; minting here would fork an idle slot
        // beside the live conversation.
        match deps.continuation.clone() {
            Some(slot) => caller.with_continuation(slot),
            None => caller,
        }
    };
    let token = cancel.child_token();
    let guard = token.clone().drop_guard();
    let run = Arc::new(Run {
        deps: deps.clone(),
        caller: mint(CallerKind::Tool),
        handler: mint(CallerKind::Handler),
        turn,
        lineage: next.clone(),
        services,
        script_services: session.as_ref().and_then(|parts| parts.script.clone()),
        workspace,
        model_export: found.record.export.clone(),
        forwarded: AtomicBool::new(false),
        info: ModelInfo {
            route: found.route(),
            name: found.record.id.as_str().into(),
            provider: found.provider(),
            caps: found.record.caps.clone(),
        },
        generation: Arc::clone(&found.generation),
        cancel: token.clone(),
    });
    let model = Arc::clone(&found.record.handler);
    let cx = ModelCx::new(run);
    let started = LINEAGE.scope(next.clone(), model.run(request, cx)).await;
    match started {
        Ok(stream) => scoped(stream, next, guard),
        Err(error) => failed(failure_of(error)),
    }
}

/// The session-owned pieces a synthetic run snapshots before it opens.
struct SessionParts {
    handle: SessionHandle,
    services: Arc<dyn Services>,
    workspace: Workspace,
    script: Option<Arc<crate::ext::services::SessionServices>>,
}

/// Snapshots the caller's session handle, services, workspace, and script
/// services so the model runs against one consistent view.
fn session_parts(deps: &RequestDeps) -> Option<SessionParts> {
    let sessions = deps
        .host
        .sessions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    sessions.get(&deps.session).map(|entry| SessionParts {
        handle: entry.handle.clone(),
        services: Arc::clone(&entry.services),
        workspace: entry.workspace.clone(),
        script: entry.backend.script_services(),
    })
}

struct Run {
    deps: RequestDeps,
    caller: Caller,
    handler: Caller,
    turn: Option<TurnId>,
    lineage: Lineage,
    services: Arc<dyn Services>,
    script_services: Option<Arc<super::services::SessionServices>>,
    workspace: Workspace,
    model_export: Option<dal_core::ext::ExportId>,
    forwarded: AtomicBool,
    info: ModelInfo,
    generation: Arc<Generation>,
    cancel: CancellationToken,
}

pub(crate) fn price_of(shared: &HostShared, route: &ModelRoute) -> Option<ModelPrice> {
    let reference = request_reference(route);
    if let Some(price) = shared.config.price_for_model(&reference) {
        return Some(price.clone());
    }
    let catalog = shared.cached_catalog();
    let resolved = catalog.as_ref().and_then(|catalog| {
        let aliases: Vec<(Box<str>, Box<str>)> = shared
            .config
            .aliases()
            .iter()
            .map(|(name, target)| (name.clone(), target.clone()))
            .collect();
        dal_provider::resolve(catalog, &aliases, &reference).ok()
    });
    let canonical = resolved
        .as_ref()
        .map(|r| format!("{}/{}", r.provider, r.entry.id));
    if let Some(price) = canonical
        .as_deref()
        .and_then(|key| shared.config.price_for_model(key))
    {
        return Some(price.clone());
    }
    if let Some(price) = resolved.as_ref().and_then(|r| r.entry.price.as_ref()) {
        return Some(price.clone());
    }
    if let Some(price) = dal_provider::compiled_price(&reference) {
        return Some(price);
    }
    canonical.as_deref().and_then(dal_provider::compiled_price)
}

pub(crate) async fn collect(mut stream: EventStream) -> Result<Inference, InferFailure> {
    let mut converter = StreamConverter::new();
    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        events.extend(converter.convert(item.map_err(InferFailure::from)?));
    }
    events.extend(converter.flush());
    Ok(Inference { events })
}

impl ModelCxRuntime for Run {
    fn scope(&self, spec: ScopeSpec) -> Result<Scope, ScopeError> {
        let shared = Arc::clone(&self.deps.host.shared);
        let price: PriceFn = Arc::new(move |route| price_of(&shared, route));
        Scope::open(
            Arc::clone(&self.services),
            self.handler.clone(),
            self.cancel.clone(),
            &spec,
            Some(price),
        )
    }

    fn script_cx(&self, runtime: Arc<dyn ModelCxRuntime>) -> Option<super::ScriptCx> {
        self.generation.model_export(self.model_export.as_ref()?)?;
        let mut script = self.deps.script.as_ref()?.attach(None)?;
        script.model_runtime = Some(runtime);
        Some(script)
    }

    fn infer<'a>(
        &'a self,
        who: &'a Caller,
        request: ModelRequest,
        script: Arc<crate::session::script::SessionScriptHost>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<Inference, crate::error::ServiceError>> {
        Box::pin(async move {
            let Some(services) = &self.script_services else {
                return Err(crate::error::ServiceError::failed(
                    None,
                    "models.infer is unavailable outside a session",
                ));
            };
            services
                .script_infer_with(who, request, Some(script), cancel)
                .await
        })
    }

    fn script_forward<'a>(
        &'a self,
        who: &'a Caller,
        request: ModelRequest,
        private: &'a [PrivateTool],
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<EventStream, crate::error::ServiceError>> {
        Box::pin(async move {
            let Some(services) = &self.script_services else {
                return Err(crate::error::ServiceError::failed(
                    None,
                    "models.forward is unavailable outside a session",
                ));
            };
            services.authorize_script_infer(who, cancel).await?;
            self.forward(request, private)
                .await
                .map_err(|error| crate::error::ServiceError::failed(None, error.to_string()))
        })
    }

    fn forward<'a>(
        &'a self,
        request: ModelRequest,
        private: &'a [PrivateTool],
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            if self.forwarded.swap(true, Ordering::SeqCst) {
                return Err(ModelError::SecondForward);
            }
            if let ModelRoute::Synthetic { id } = &request.model {
                let mut chain = self.lineage.chain.clone();
                chain.push(id.clone());
                check_chain(&chain)?;
            }
            let forward = Lineage {
                journal: if matches!(&request.model, ModelRoute::Synthetic { .. }) {
                    self.lineage.journal.clone()
                } else {
                    None
                },
                ..self.lineage.clone()
            };
            if private.is_empty() {
                let deps = self.deps.clone();
                let cancel = self.cancel.clone();
                return Ok(LINEAGE
                    .scope(forward, infer_stream(&deps, request, &cancel))
                    .await);
            }
            // Birth the private loop before its first round on the outer
            // session journal (session-backed Cursor forwards only). The
            // forward-scoped lineage above drops the journal for
            // non-Synthetic inner models, so it cannot serve here. A failed
            // birth ends the forward before any round opens or effect runs.
            let ploop = PrivateLoopId::new_v7();
            if is_cursor_request(&request.model)
                && let (Some(handle), Some(who)) =
                    (self.lineage.journal.clone(), self.lineage.who.clone())
                && let Err(error) = handle.private_loop_born(ploop, who, self.turn).await
            {
                return Ok(failed(InferFailure::Fatal {
                    message: error.to_string().into(),
                    fix: None,
                }));
            }
            Ok(private_loop(self, request, private, forward, ploop))
        })
    }
}

/// Whether a forward request addresses the Cursor family: the only family
/// with a run conversation behind `open_step`. Every other family keeps the
/// one-shot path byte-identical and journals no private records.
fn is_cursor_request(route: &ModelRoute) -> bool {
    matches!(route, ModelRoute::Api { family, .. } if *family == dal_core::Family::Cursor)
}

/// A terminal request failure naming a continuation misuse. The parked run
/// is left for the owner to release; only this round ends here.
fn invalid_request(message: &str) -> ProviderError {
    ProviderError::InvalidRequest {
        message: message.into(),
    }
}

/// The in-memory error of a private call whose start never became durable.
/// The effect never runs; nothing is stashed and nothing is responded.
const CLOSED_PRIVATE_CALL: &str = "session closed before private call";

struct Round {
    text: String,
    replays: Vec<RawJson>,
    absorbed: HashSet<String>,
    private: Vec<dal_provider::ToolCall>,
    session: Vec<dal_provider::ToolCall>,
    /// The step's full (id, name) cover in decoded order, parked verbatim.
    cover: Vec<(CallId, Box<str>)>,
    done: bool,
}

impl Round {
    fn new() -> Self {
        Self {
            text: String::new(),
            replays: Vec::new(),
            absorbed: HashSet::new(),
            private: Vec::new(),
            session: Vec::new(),
            cover: Vec::new(),
            done: false,
        }
    }
}

struct Loop {
    deps: RequestDeps,
    cancel: CancellationToken,
    lineage: Lineage,
    /// The journal-stable loop identity, birthed in `forward` before the
    /// first round. Distinct from every wire call id.
    ploop: PrivateLoopId,
    /// The owning turn for private records, when the loop runs inside one.
    turn: Option<TurnId>,
    /// The durability target for this round's private records: the outer
    /// session journal with its owner, fixed at construction for Cursor
    /// loops. `None` on memory-only legs (relay, non-Cursor families,
    /// owner-less loops).
    journal: Option<(SessionHandle, Owner)>,
    request: ModelRequest,
    tools: Vec<Arc<dyn Tool>>,
    private_cx: PrivateCx,
    workspace: Workspace,
    rounds: u32,
    pending_session: Vec<dal_provider::ToolCall>,
    current: Option<EventStream>,
    queue: VecDeque<Result<ProviderEvent, ProviderError>>,
    round: Round,
    usage: Option<Usage>,
    finished: bool,
}

/// The three sinks one private call writes to: the in-memory assistant
/// parts, the in-memory tool results, and the parked slot stash.
struct PrivateCallSinks<'a> {
    parts: &'a mut Vec<dal_core::AssistantPart>,
    results: &'a mut Vec<ContextItem>,
    slot: Option<&'a ContinuationSlot>,
}

fn private_loop(
    run: &Run,
    mut request: ModelRequest,
    private: &[PrivateTool],
    lineage: Lineage,
    ploop: PrivateLoopId,
) -> EventStream {
    let tools: Vec<Arc<dyn Tool>> = private.iter().map(|tool| Arc::clone(&tool.0)).collect();
    let declared: std::collections::HashSet<Box<str>> =
        request.tools.iter().map(|spec| spec.name.clone()).collect();
    // The adapter binds an export as private exactly when `request.tools`
    // names its wire name, so a private spec already named there is bound
    // by the handler, not a shadow. A private tool never named in
    // `request.tools` would silently intercept a session call, so that
    // collision fails closed.
    let unbound = tools
        .iter()
        .filter(|tool| !declared.contains(tool.name().as_str()))
        .collect::<Vec<_>>();
    let registered = run.deps.host.shared.generation.borrow().clone();
    let shadows_session = unbound
        .iter()
        .any(|tool| registered.tool(tool.name()).is_some());
    if shadows_session {
        let items = vec![Err(ProviderError::InvalidRequest {
            message: "a private tool may not shadow a session tool".into(),
        })];
        return EventStream::new(stream::iter(items), || {});
    }
    let mut specs: Vec<ModelToolSpec> = request.tools.iter().cloned().collect();
    specs.extend(unbound.iter().map(|tool| {
        let spec = tool.spec(&run.info);
        ModelToolSpec {
            name: spec.name.as_str().into(),
            description: spec.description.clone(),
            parameters: spec.parameters.clone(),
            grammar: spec.grammar.clone(),
            defer_loading: false,
        }
    }));
    request.tools = specs.into();
    // Durability target, fixed for the loop's lifetime: the outer session
    // journal with its owner, Cursor-family only. Read from the run's
    // lineage, never the forward-scoped one (which drops the journal for
    // non-Synthetic inner models): a Cursor-directed forward still describes
    // the session turn's loop.
    let journal = if is_cursor_request(&request.model) {
        match (&run.lineage.journal, &run.lineage.who) {
            (Some(handle), Some(who)) => Some((handle.clone(), who.clone())),
            _ => None,
        }
    } else {
        None
    };
    let state = Loop {
        deps: run.deps.clone(),
        cancel: run.cancel.clone(),
        lineage,
        ploop,
        turn: run.turn,
        journal,
        request,
        tools,
        private_cx: PrivateCx {
            caller: run.caller.clone(),
            session: run.deps.session,
            services: Arc::clone(&run.services),
            workspace: run.workspace.clone(),
            env: Arc::clone(&run.deps.host.shared.env),
            generation: run.generation.id,
            cancel: run.cancel.clone(),
        },
        workspace: run.workspace.clone(),
        rounds: 0,
        pending_session: Vec::new(),
        current: None,
        queue: VecDeque::new(),
        round: Round::new(),
        usage: None,
        finished: false,
    };
    let source = stream::unfold(state, |mut state| async move {
        let item = state.next().await?;
        Some((item, state))
    });
    EventStream::new(source, || {})
}

impl Loop {
    async fn next(&mut self) -> Option<Result<ProviderEvent, ProviderError>> {
        loop {
            if let Some(item) = self.queue.pop_front() {
                return Some(item);
            }
            if self.finished {
                return None;
            }
            let Some(current) = self.current.as_mut() else {
                self.open_round().await;
                continue;
            };
            match current.next().await {
                None => self.finished = true,
                Some(Err(error)) => {
                    self.finished = true;
                    self.queue.push_back(Err(error));
                }
                Some(Ok(event)) => self.absorb(event).await,
            }
        }
    }

    async fn open_round(&mut self) {
        let request = self.request.clone();
        let stream = LINEAGE
            .scope(
                self.lineage.clone(),
                infer_stream(&self.deps, request, &self.cancel),
            )
            .await;
        self.current = Some(stream);
        self.round = Round::new();
    }

    fn is_private(&self, name: &str) -> bool {
        self.tools.iter().any(|tool| tool.name().as_str() == name)
    }

    async fn absorb(&mut self, event: ProviderEvent) {
        match event {
            ProviderEvent::TextDelta { text } => {
                self.round.text.push_str(&text);
                self.queue.push_back(Ok(ProviderEvent::TextDelta { text }));
            }
            ProviderEvent::ToolCallStarted { id, name } => {
                if self.is_private(&name) {
                    self.round.absorbed.insert(id);
                } else {
                    self.queue
                        .push_back(Ok(ProviderEvent::ToolCallStarted { id, name }));
                }
            }
            ProviderEvent::ToolArgsDelta { id, fragment } => {
                if !self.round.absorbed.contains(&id) {
                    self.queue
                        .push_back(Ok(ProviderEvent::ToolArgsDelta { id, fragment }));
                }
            }
            ProviderEvent::Replay { payload } => self.round.replays.push(payload.item),
            ProviderEvent::ToolCallsDone { calls } => {
                // Retain the decoded cover order for parking: the partition
                // below must not reorder what the step announced.
                self.round.cover = calls
                    .iter()
                    .map(|call| (CallId::new(call.id.as_str()), call.name.as_str().into()))
                    .collect();
                let (private, session) = calls
                    .into_iter()
                    .partition(|call| self.is_private(&call.name));
                self.round.private = private;
                self.round.session = session;
                self.round.done = true;
            }
            ProviderEvent::Usage { usage } => self.add_usage(usage),
            ProviderEvent::Stop { reason } => self.stop(reason).await,
            // An inner provider's `MessageReplay` capture belongs to the
            // inner entry only. The relay has no single closing response to
            // bind it to: forwarding it would emit the capture ahead of the
            // relay's deferred `Replay`/`ToolCallsDone`/`Usage` and journal
            // it on the synthetic route's entry, and a second round's
            // capture would trip the outer stream's at-most-once guard. Drop
            // it here; the relay's own `Replay` emission is unchanged.
            ProviderEvent::MessageReplay { .. } => {}
            other => self.queue.push_back(Ok(other)),
        }
    }

    fn add_usage(&mut self, next: Usage) {
        self.usage = Some(match self.usage {
            None => next,
            Some(total) => Usage {
                input_tokens: total.input_tokens.saturating_add(next.input_tokens),
                cached_input_tokens: total
                    .cached_input_tokens
                    .saturating_add(next.cached_input_tokens),
                output_tokens: total.output_tokens.saturating_add(next.output_tokens),
                reasoning_tokens: match (total.reasoning_tokens, next.reasoning_tokens) {
                    (None, None) => None,
                    (left, right) => Some(left.unwrap_or(0).saturating_add(right.unwrap_or(0))),
                },
                cache_write_tokens: total
                    .cache_write_tokens
                    .saturating_add(next.cache_write_tokens),
                premium_requests: total
                    .premium_requests
                    .zip(next.premium_requests)
                    .map(|(left, right)| left + right),
                cost_usd: total
                    .cost_usd
                    .zip(next.cost_usd)
                    .map(|(left, right)| left + right),
            },
        });
    }

    async fn stop(&mut self, reason: dal_provider::StopReason) {
        #[cfg(test)]
        let mut witness = decision_witness::Pending::new(self.private_cx.session);
        // The slot that `open_step` installed this step's conversation into,
        // when this round addresses the Cursor family through a live owner.
        // `None` on every other path (other families, service-only
        // inference, a released owner): the one-shot shape below, untouched.
        let slot = if is_cursor_request(&self.request.model) {
            self.deps.continuation.as_ref().and_then(Weak::upgrade)
        } else {
            None
        };
        if self.round.private.is_empty() {
            // Session-only (or call-free) round: park the session cover so a
            // later respond finds exactly the set this round's emission
            // dispatches. The driver responds before its next generation
            // step; the loop never responds on this path.
            if matches!(reason, dal_provider::StopReason::ToolUse)
                && !self.round.session.is_empty()
                && let Some(slot) = &slot
                && let Err(error) = slot.park(std::mem::take(&mut self.round.cover))
            {
                self.queue
                    .push_back(Err(invalid_request(&error.to_string())));
                self.finished = true;
                return;
            }
            self.emit_final(reason);
            return;
        }
        // Private-carrying round below this point.
        let parked: Option<Arc<ContinuationSlot>> =
            if matches!(reason, dal_provider::StopReason::ToolUse) {
                slot
            } else {
                None
            };
        let Some(slot) = parked else {
            // One-shot shape: service-only Cursor, a dead owner, a foreign
            // family, or a stop the run cannot park on. Durability still
            // applies (journaled starts/ends, in-memory results); nothing is
            // stashed or responded, and the next `open_step` rules on the
            // run's honest state. Today's path, untouched.
            self.pending_session.append(&mut self.round.session);
            if self.rounds >= PRIVATE_ROUNDS {
                self.finished = true;
                self.queue
                    .push_back(Err(ProviderError::Synthetic(failure_of(
                        ModelError::PrivateRounds,
                    ))));
                return;
            }
            self.rounds += 1;
            self.run_private(
                None,
                #[cfg(test)]
                &mut witness,
            )
            .await;
            self.current = None;
            return;
        };
        // Slotted ToolUse round: park the full step cover in decoded order
        // before any effect runs. Any failure below leaves the run for the
        // owner; this round ends here.
        if let Err(error) = slot.park(std::mem::take(&mut self.round.cover)) {
            self.queue
                .push_back(Err(invalid_request(&error.to_string())));
            self.finished = true;
            return;
        }
        if self.rounds >= PRIVATE_ROUNDS {
            self.finished = true;
            self.queue
                .push_back(Err(ProviderError::Synthetic(failure_of(
                    ModelError::PrivateRounds,
                ))));
            return;
        }
        self.rounds += 1;
        self.run_private(
            Some(slot.as_ref()),
            #[cfg(test)]
            &mut witness,
        )
        .await;
        if self.finished {
            // A stash desync failed the round inside `run_private`; its error
            // is already queued. The parked cover stays for the owner.
            return;
        }
        if self.round.session.is_empty() {
            // All-private round: the stash holds the exact cover, so resume
            // the same parked run instead of opening a second connection.
            // The staged stream returns through the next `open_round`; only
            // the error surfaces here. No retry: this round's inputs are
            // deterministic, so a failed cover fails identically on
            // re-supply, and a fired cancel refires; the driver's
            // pre-next-step respond fails the turn closed on whatever cover
            // remains.
            if let Err(error) = slot.respond(Vec::new(), &self.cancel).await {
                self.queue.push_back(Err(error));
                self.finished = true;
                return;
            }
            self.current = None;
            return;
        }
        // Mixed round: the private stash stays in the slot and the session
        // calls emit once for the outer actor, which dispatches, journals,
        // and responds before its next generation step. This round ends
        // here; nothing is responded in the loop and no result is donated to
        // a new loop. `pending_session` is empty on this path (every
        // session-carrying round finishes), so the emission below carries
        // exactly the round's session set, once.
        self.emit_final(reason);
    }

    fn emit_final(&mut self, reason: dal_provider::StopReason) {
        self.pending_session.append(&mut self.round.session);
        for payload in std::mem::take(&mut self.round.replays) {
            let item = dal_provider::ReplayPayload {
                family: family_of(&self.request.model),
                model: request_reference(&self.request.model).into(),
                item: payload,
            };
            self.queue
                .push_back(Ok(ProviderEvent::Replay { payload: item }));
        }
        let calls = std::mem::take(&mut self.pending_session);
        self.queue
            .push_back(Ok(ProviderEvent::ToolCallsDone { calls }));
        if let Some(usage) = self.usage {
            self.queue.push_back(Ok(ProviderEvent::Usage { usage }));
        }
        self.queue.push_back(Ok(ProviderEvent::Stop { reason }));
        self.finished = true;
    }

    /// Executes one round's private calls with durable-first ordering.
    ///
    /// `slot` is `Some` only on a parked slotted round: every settled end is
    /// stashed before the loop responds. `None` keeps every result in memory
    /// (one-shot shape, relay memory-only legs, or a stop the run cannot
    /// park on). A stashing desync queues the error and finishes the loop;
    /// the parked cover stays for the owner.
    /// Runs one private call with durable-first ordering: journal the start,
    /// run the effect unless the start was rejected, journal the end, then
    /// stash into the slot.
    ///
    /// Returns true when a stash desync finished the loop; the error is
    /// already queued and the parked cover stays for the owner.
    async fn run_one_private_call(
        &mut self,
        call: dal_provider::ToolCall,
        journal: Option<&(SessionHandle, Owner)>,
        sinks: PrivateCallSinks<'_>,
        #[cfg(test)] witness: &mut decision_witness::Pending,
    ) -> bool {
        let PrivateCallSinks {
            parts,
            results,
            slot,
        } = sinks;
        let args = match &call.args {
            ToolArgs::Parsed(raw) => raw.clone(),
            _ => RawJson::null(),
        };
        let wire = CallId::new(call.id.as_str());
        let name: Box<str> = call.name.as_str().into();
        parts.push(dal_core::AssistantPart::ToolCall {
            call: wire.clone(),
            name: name.clone(),
            args: args.clone(),
            replay: None,
        });
        let token = PrivateEffectId::new_v7();
        let started = match journal {
            Some((handle, who)) => Some(
                handle
                    .private_start(
                        self.ploop,
                        token,
                        who.clone(),
                        self.turn,
                        self.rounds,
                        wire.clone(),
                        name.clone(),
                        args.clone(),
                    )
                    .await,
            ),
            None => None,
        };
        let started_ok = matches!(started, Some(Ok(())));
        let (is_error, text) = match started {
            // The start is not durable, so the effect never runs: an
            // honest in-memory error, never stashed, never responded.
            // The parked cover stays open for the owner to release.
            Some(Err(_)) => (true, CLOSED_PRIVATE_CALL.to_owned()),
            _ => call_private(&self.tools, &self.private_cx, &self.workspace, &call, &args).await,
        };
        // One shared text for the three sinks (in-memory result, durable
        // end, slot stash): two small copies on slotted paths only, so
        // no sink aliases another.
        let text: Box<str> = text.into();
        results.push(ContextItem::ToolResult {
            call: wire.clone(),
            name: name.clone(),
            is_error,
            parts: vec![Part::Text { text: text.clone() }],
        });
        // End-then-stash, in that order, and only on a durable start:
        // the actor rejects an end without its start, so attempting one
        // wastes a roundtrip. A failed end keeps the row Started
        // (repaired Indeterminate at the next open) and the result out
        // of the stash; the cover fails closed downstream.
        if let Some((handle, _)) = journal {
            if !started_ok {
                #[cfg(test)]
                witness.note(
                    wire.as_str(),
                    decision_witness::PrivateDecision::StartRejected,
                );
                return false;
            }
            let end = PrivateOutcome::Settled {
                is_error,
                parts: vec![JournalPart::Text { text: text.clone() }],
            };
            if handle.private_end(self.ploop, token, end).await.is_err() {
                #[cfg(test)]
                witness.note(
                    wire.as_str(),
                    decision_witness::PrivateDecision::EndRejected,
                );
                return false;
            }
        }
        if let Some(slot) = slot
            && let Err(error) = slot.stash(wire, name, is_error, vec![Part::Text { text }])
        {
            self.queue
                .push_back(Err(invalid_request(&error.to_string())));
            self.finished = true;
            return true;
        }
        false
    }

    async fn run_private(
        &mut self,
        slot: Option<&ContinuationSlot>,
        #[cfg(test)] witness: &mut decision_witness::Pending,
    ) {
        let calls = std::mem::take(&mut self.round.private);
        let source = dal_core::ReplaySource {
            family: family_of(&self.request.model),
            model: request_reference(&self.request.model).into(),
        };
        let mut parts = Vec::new();
        if !self.round.text.is_empty() {
            parts.push(dal_core::AssistantPart::Text {
                text: std::mem::take(&mut self.round.text).into(),
                replay: None,
            });
        }
        for replay in std::mem::take(&mut self.round.replays) {
            parts.push(dal_core::AssistantPart::Thinking {
                text: "".into(),
                replay: Some(replay),
            });
        }
        // Durability target fixed at construction (`None` on memory-only
        // legs): cloned once per round, so no lineage borrow crosses the
        // effect awaits below.
        let journal = self.journal.clone();
        let mut results = Vec::new();
        for call in calls {
            let sinks = PrivateCallSinks {
                parts: &mut parts,
                results: &mut results,
                slot,
            };
            if self
                .run_one_private_call(
                    call,
                    journal.as_ref(),
                    sinks,
                    #[cfg(test)]
                    &mut *witness,
                )
                .await
            {
                return;
            }
        }
        let mut context: Vec<ContextItem> = self.request.context.iter().cloned().collect();
        context.push(ContextItem::Assistant {
            source,
            parts,
            replay: None,
        });
        context.extend(results);
        self.request.context = context.into();
    }
}

async fn call_private(
    tools: &[Arc<dyn Tool>],
    private_cx: &PrivateCx,
    workspace: &Workspace,
    call: &dal_provider::ToolCall,
    args: &RawJson,
) -> (bool, String) {
    let Some(tool) = tools.iter().find(|tool| tool.name().as_str() == call.name) else {
        return (true, format!("unknown tool: {}", call.name));
    };
    if let Err(error) = tool.classify(args, workspace) {
        return (true, error.to_string());
    }
    let cx = private_cx.mint(CallId::new(call.id.as_str()));
    let tool_call = ToolCall {
        id: CallId::new(call.id.as_str()),
        args: args.clone(),
    };
    match contained(async move { tool.run(tool_call, cx).await }).await {
        Ok(ToolOutcome::Ok(output)) => (false, output.to_string()),
        Ok(ToolOutcome::Err(error)) => (true, error.to_string()),
        Ok(ToolOutcome::Interrupted) => (true, "Tool call interrupted by user.".into()),
        Ok(ToolOutcome::Detached(job)) => (true, format!("tool detached as job {job}")),
        Err(panic) => (true, crashed_tool(tool.name(), &panic)),
    }
}

fn family_of(route: &ModelRoute) -> dal_core::Family {
    match route {
        ModelRoute::Api { family, .. } => *family,
        ModelRoute::Synthetic { .. } | ModelRoute::Harness { .. } => dal_core::Family::Chat,
    }
}

/// Test-only observation of the loop's branch decision for one private call.
///
/// The loop records a decision only when a receipt was rejected and the call
/// leaves its branch without stash or respond. The record is keyed by session
/// and call. It never touches the journal, the wire, or any effect.
#[cfg(test)]
pub(crate) mod decision_witness {
    use std::{
        collections::HashMap,
        sync::{LazyLock, Mutex, MutexGuard, PoisonError},
    };

    use dal_core::SessionId;

    /// Why a private call's branch ended without stash or respond.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum PrivateDecision {
        /// The start receipt was rejected, so the effect never ran.
        StartRejected,
        /// The effect ran, but its settled end was rejected; nothing was stashed.
        EndRejected,
    }

    static DECISIONS: LazyLock<Mutex<HashMap<(SessionId, String), PrivateDecision>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    fn decisions() -> MutexGuard<'static, HashMap<(SessionId, String), PrivateDecision>> {
        DECISIONS.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The decisions one round step reached, published when the step ends.
    pub(crate) struct Pending {
        session: SessionId,
        decisions: Vec<(String, PrivateDecision)>,
    }

    impl Pending {
        pub(crate) fn new(session: SessionId) -> Self {
            Self {
                session,
                decisions: Vec::new(),
            }
        }

        pub(crate) fn note(&mut self, call: &str, decision: PrivateDecision) {
            self.decisions.push((call.to_owned(), decision));
        }
    }

    impl Drop for Pending {
        fn drop(&mut self) {
            let mut table = decisions();
            for (call, decision) in self.decisions.drain(..) {
                table.insert((self.session, call), decision);
            }
        }
    }

    /// Takes the decision recorded for `call` in `session`, if one was recorded.
    #[cfg(test)]
    pub(crate) fn take(session: SessionId, call: &str) -> Option<PrivateDecision> {
        decisions().remove(&(session, call.to_owned()))
    }
}
