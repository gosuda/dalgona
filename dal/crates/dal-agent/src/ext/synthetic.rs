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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use dal_core::{
    CallId, ContextItem, InferFailure, Inference, InferredPurpose, ModelId, ModelInfo, ModelPrice,
    ModelRequest, ModelRoute, ModelToolSpec, Owner, Part, RawJson, ScopeSpec, ThinkingLevel, Usage,
    Workspace, check_synthetic_chain,
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
        let id = self.record.id.as_str();
        id.split_once('/')
            .map_or(id, |(namespace, _)| namespace)
            .into()
    }
}

fn entry_for(record: &ModelRecord) -> CatalogEntry {
    let caps = &record.caps;
    let id = record.id.as_str();
    let (provider, name) = id.split_once('/').unwrap_or((id, id));
    CatalogEntry {
        provider: provider.into(),
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

fn origin_text(origin: dal_core::Origin) -> &'static str {
    match origin {
        dal_core::Origin::Builtin => "builtin",
        dal_core::Origin::Bundled => "bundled",
        dal_core::Origin::User => "user",
        _ => "unknown",
    }
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
    let who = Owner::Extension {
        name: extension.name().into(),
        origin: origin_text(extension.origin()).into(),
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
        Caller::new(
            name.clone(),
            extension.origin(),
            extension.inject(),
            kind,
            turn,
        )
    };
    let token = cancel.child_token();
    let guard = token.clone().drop_guard();
    let run = Arc::new(Run {
        deps: deps.clone(),
        caller: mint(CallerKind::Tool),
        handler: mint(CallerKind::Handler),
        lineage: next.clone(),
        services,
        script_services: session.as_ref().and_then(|parts| parts.script.clone()),
        workspace,
        model_export: found.record.export.clone(),
        forwarded: AtomicBool::new(false),
        info: ModelInfo {
            route: found.route(),
            name: found.record.id.as_str().into(),
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

fn lookup_price(shared: &HostShared, key: &str) -> Option<ModelPrice> {
    shared
        .config
        .price_for_model(key)
        .copied()
        .or_else(|| dal_provider::compiled_price(key))
}

pub(crate) fn price_of(shared: &HostShared, route: &ModelRoute) -> Option<ModelPrice> {
    let reference = request_reference(route);
    if let Some(price) = lookup_price(shared, &reference) {
        return Some(price);
    }
    let catalog = shared.cached_catalog()?;
    let aliases: Vec<(Box<str>, Box<str>)> = shared
        .config
        .aliases()
        .iter()
        .map(|(name, target)| (name.clone(), target.clone()))
        .collect();
    let resolved = dal_provider::resolve(&catalog, &aliases, &reference).ok()?;
    lookup_price(
        shared,
        &format!("{}/{}", resolved.provider, resolved.entry.id),
    )
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
            Ok(private_loop(self, request, private, forward))
        })
    }
}

struct Round {
    text: String,
    replays: Vec<RawJson>,
    absorbed: HashSet<String>,
    private: Vec<dal_provider::ToolCall>,
    session: Vec<dal_provider::ToolCall>,
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
            done: false,
        }
    }
}

struct Loop {
    deps: RequestDeps,
    cancel: CancellationToken,
    lineage: Lineage,
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

fn private_loop(
    run: &Run,
    mut request: ModelRequest,
    private: &[PrivateTool],
    lineage: Lineage,
) -> EventStream {
    let tools: Vec<Arc<dyn Tool>> = private.iter().map(|tool| Arc::clone(&tool.0)).collect();
    let declared: std::collections::HashSet<Box<str>> =
        request.tools.iter().map(|spec| spec.name.clone()).collect();
    // The adapter binds an export as private exactly when `request.tools`
    // names its wire name, so a private spec already named there is bound
    // by the handler, not a shadow. A private tool never named in
    // `request.tools` would silently intercept a session call, so that
    // collision fails closed (R04).
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
        }
    }));
    request.tools = specs.into();
    let state = Loop {
        deps: run.deps.clone(),
        cancel: run.cancel.clone(),
        lineage,
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
                let (private, session) = calls
                    .into_iter()
                    .partition(|call| self.is_private(&call.name));
                self.round.private = private;
                self.round.session = session;
                self.round.done = true;
            }
            ProviderEvent::Usage { usage } => self.add_usage(usage),
            ProviderEvent::Stop { reason } => self.stop(reason).await,
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
                cost_usd: total
                    .cost_usd
                    .zip(next.cost_usd)
                    .map(|(left, right)| left + right),
            },
        });
    }

    async fn stop(&mut self, reason: dal_provider::StopReason) {
        if self.round.private.is_empty() {
            self.emit_final(reason);
            return;
        }
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
        self.run_private().await;
        self.current = None;
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

    async fn run_private(&mut self) {
        let calls = std::mem::take(&mut self.round.private);
        let source = dal_core::ReplaySource {
            family: family_of(&self.request.model),
            model: request_reference(&self.request.model).into(),
        };
        let mut parts = Vec::new();
        if !self.round.text.is_empty() {
            parts.push(dal_core::AssistantPart::Text {
                text: std::mem::take(&mut self.round.text).into(),
            });
        }
        for replay in std::mem::take(&mut self.round.replays) {
            parts.push(dal_core::AssistantPart::Thinking {
                text: "".into(),
                replay: Some(replay),
            });
        }
        let mut results = Vec::new();
        for call in calls {
            let args = match &call.args {
                ToolArgs::Parsed(raw) => raw.clone(),
                _ => RawJson::null(),
            };
            parts.push(dal_core::AssistantPart::ToolCall {
                call: CallId::new(call.id.as_str()),
                name: call.name.as_str().into(),
                args: args.clone(),
            });
            let (is_error, text) =
                call_private(&self.tools, &self.private_cx, &self.workspace, &call, &args).await;
            results.push(ContextItem::ToolResult {
                call: CallId::new(call.id.as_str()),
                name: call.name.as_str().into(),
                is_error,
                parts: vec![Part::Text { text: text.into() }],
            });
        }
        let mut context: Vec<ContextItem> = self.request.context.iter().cloned().collect();
        context.push(ContextItem::Assistant { source, parts });
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
    match tool.run(tool_call, cx).await {
        ToolOutcome::Ok(output) => (false, output.to_string()),
        ToolOutcome::Err(error) => (true, error.to_string()),
        ToolOutcome::Interrupted => (true, "Tool call interrupted by user.".into()),
        ToolOutcome::Detached(job) => (true, format!("tool detached as job {job}")),
    }
}

fn family_of(route: &ModelRoute) -> dal_core::Family {
    match route {
        ModelRoute::Api { family, .. } => *family,
        ModelRoute::Synthetic { .. } | ModelRoute::Harness { .. } => dal_core::Family::Chat,
    }
}
