//! Ordered call dispatch: hooks, runs, approvals, settlements.
//!
//! Plan units run strictly in call order; read runs hold at most
//! `parallel_reads` in flight. Tools drive approvals themselves through
//! [`ToolCx::authorize`]; the per-call runtime behind it mints the
//! move-only [`Approved`] proof per the ladder. Settlements and broker
//! resolutions report back to the actor for journaling.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dal_core::ext::ToolCallEvent;
use dal_core::{
    Answer, CallId, ClientId, GrantSpec, JobEnd, JobId, Mode, Name, Owner, Policy, Preview,
    Question, RawJson, Request, ResolvedCall, SessionId, SettledOutcome, ToolClass, TurnId, Unit,
    Workspace,
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use super::backend::Backend;
use super::context::{
    DeferredTool, is_core_tool_search, resolve_named, tool_search_query, tool_search_results,
};
use crate::broker::{Broker, Resolution, Resolved, Settled, default_timeout};
use crate::ext::generation::Generation;
use crate::ext::hooks::{HookScope, dispatch_tool_call, hook_fanout};
use crate::ext::overlay::TurnTools;
use crate::ext::tool::{
    Approved, CallSnapshot, Tool, ToolCall, ToolCx, ToolCxRuntime, ToolOutcome,
};
use crate::ext::{BoxFuture, Caller, CallerKind, Doc, ScriptCx, Services};
use crate::jobs::JobTable;
use crate::proc::{Proc, SpawnOpts, spawn_process};
use crate::session::actor::TurnWork;
use crate::session::contain::contained;
use crate::session::service_grants::{CallKey, Invocation, scope_roots};
use crate::session::tasks::SessionTasks;

/// A call ready to run with its resolved details.
#[derive(Clone, Debug)]
pub(crate) struct ReadyCall {
    /// The provider call identity.
    pub call: CallId,
    /// The checked tool name.
    pub name: Name,
    /// The final arguments.
    pub args: RawJson,
    /// The computed class.
    pub class: ToolClass,
}

/// Driver-owned dispatch inputs for one unit batch.
#[derive(Clone)]
pub(crate) struct DispatchCtx {
    /// The owning session.
    pub session: SessionId,
    /// The parent session, when this session is a subagent.
    pub parent: Option<SessionId>,
    /// Host-captured process-edge environment snapshot.
    pub process_env: Arc<crate::Env>,
    /// The owning turn.
    pub turn: TurnId,
    /// The session workspace.
    pub workspace: Workspace,
    /// The execution mode the rewritten-argument resolve path gates on.
    pub mode: Mode,
    /// The turn's generation snapshot.
    pub generation: Arc<Generation>,
    /// The turn's overlay tools, frozen at turn start.
    pub tools: TurnTools,
    /// The deferred tool catalog for the active provider request.
    pub deferred_search: Arc<[DeferredTool]>,
    /// Capability-scoped services for hook and tool contexts.
    pub services: Arc<dyn Services>,
    /// The generation id minted into tool contexts.
    pub generation_id: dal_core::GenerationId,
    /// Turn-scoped backend for runtime operations.
    pub backend: Arc<Backend>,
    /// The session broker for approval questions.
    pub broker: Arc<Broker>,
    /// The ladder policy snapshot for this batch.
    pub policy: Policy,
    /// The turn cancellation token.
    pub cancel: CancellationToken,
    /// The turn deadline bounding hook waits.
    pub turn_deadline: tokio::time::Instant,
    /// Maximum in-flight reads in one read run.
    pub parallel_reads: usize,
    /// Per-turn cutoff for evidence scoping.
    pub cutoff: Option<u64>,
    /// Turn approval grants shared by every call runtime.
    pub ledger: Arc<Mutex<GrantLedger>>,
    /// The captured script seam for model-dispatched calls, when the host
    /// captured an eval environment this request (E01 E06).
    pub script: Option<ScriptCx>,
}

/// One settled call with its actor-bound reports in run order.
pub(crate) struct SettledCall {
    /// The completed call.
    pub call: CallId,
    /// The call's terminal outcome.
    pub outcome: SettledOutcome,
    /// Milliseconds the tool ran, approval waits excluded; `None` when the
    /// call never ran.
    pub elapsed_ms: Option<u64>,
    /// Journal reports in the order the call produced them.
    pub reports: Vec<TurnWork>,
}

/// Plans dispatch units over resolved calls in provider order.
///
/// Maximal contiguous runs of successfully resolved read calls become one
/// read run; every other call dispatches alone. Resolution failures settle
/// immediately without running.
pub(crate) fn plan_units(calls: &[ResolvedCall]) -> (Vec<Unit>, Vec<(CallId, SettledOutcome)>) {
    let mut units = Vec::new();
    let mut failed = Vec::new();
    let mut reads: Vec<CallId> = Vec::new();
    for resolved in calls {
        let class = if let Ok(class) = &resolved.result {
            class.clone()
        } else {
            if !reads.is_empty() {
                units.push(Unit::Reads {
                    calls: std::mem::take(&mut reads),
                });
            }
            failed.push((resolved.call.clone(), failed_outcome(resolved)));
            continue;
        };
        if matches!(class, ToolClass::Read) {
            reads.push(resolved.call.clone());
        } else {
            if !reads.is_empty() {
                units.push(Unit::Reads {
                    calls: std::mem::take(&mut reads),
                });
            }
            units.push(Unit::Serial {
                call: resolved.call.clone(),
            });
        }
    }
    if !reads.is_empty() {
        units.push(Unit::Reads { calls: reads });
    }
    (units, failed)
}

/// Settles one resolution failure as model-visible text.
fn failed_outcome(resolved: &ResolvedCall) -> SettledOutcome {
    let text = match &resolved.result {
        Ok(_) => {
            return SettledOutcome::Err {
                text: "internal error: resolved call has no failure".into(),
            };
        }
        Err(dal_core::ResolveError::Unknown) => format!("unknown tool: {}", resolved.name),
        Err(dal_core::ResolveError::EvalOnly) => {
            format!("{} is callable only from eval cells", resolved.name)
        }
        Err(dal_core::ResolveError::InvalidArgs(detail)) => {
            format!("invalid arguments for {}: {detail}", resolved.name)
        }
        Err(dal_core::ResolveError::DuplicateCallId) => format!(
            "invalid arguments for {}: duplicate call id {}",
            resolved.name,
            resolved.call.as_str()
        ),
    };
    SettledOutcome::Err { text: text.into() }
}

/// Runs one plan unit, returning call-ordered settlements.
///
/// Reports stay buffered on each settlement; the driver forwards them to
/// the actor in plan order, so dispatch never blocks on actor backpressure.
pub(crate) async fn run_unit(
    ctx: &DispatchCtx,
    unit: &Unit,
    calls: &HashMap<CallId, ReadyCall>,
) -> Vec<SettledCall> {
    let mut settled = match unit {
        Unit::Reads { calls: ids } => run_reads(ctx, ids, calls).await,
        Unit::Serial { call } => {
            if let Some(ready) = calls.get(call) {
                vec![run_one(ctx, ready).await]
            } else {
                Vec::new()
            }
        }
        _ => {
            debug_assert!(false, "unplanned dispatch unit; add an explicit arm");
            Vec::new()
        }
    };
    for item in &mut settled {
        item.reports.push(TurnWork::Settled {
            turn: ctx.turn,
            call: item.call.clone(),
            outcome: item.outcome.clone(),
            elapsed_ms: item.elapsed_ms,
        });
    }
    settled
}

/// Runs one read run with capped in-flight calls, settling in call order.
async fn run_reads(
    ctx: &DispatchCtx,
    ids: &[CallId],
    calls: &HashMap<CallId, ReadyCall>,
) -> Vec<SettledCall> {
    use futures::StreamExt as _;
    let mut pending = Vec::with_capacity(ids.len());
    for (index, id) in ids.iter().enumerate() {
        if let Some(ready) = calls.get(id) {
            pending.push((index, run_one_owned(ctx.clone(), ready.clone())));
        }
    }
    let mut settled: Vec<(usize, SettledCall)> = futures::stream::iter(
        pending
            .into_iter()
            .map(|(index, future)| async move { (index, future.await) }),
    )
    .buffer_unordered(ctx.parallel_reads.max(1))
    .collect()
    .await;
    settled.sort_by_key(|(index, _)| *index);
    settled.into_iter().map(|(_, item)| item).collect()
}

/// Runs one ready call through hooks and the tool, buffering its start.
async fn run_one_owned(ctx: DispatchCtx, ready: ReadyCall) -> SettledCall {
    run_one(&ctx, &ready).await
}

/// Runs one call: hooks may block or rewrite, then the tool runs to one
/// terminal outcome. Approvals happen inside the run through the call
/// runtime; the ladder denial text rides the denial reason.
async fn run_one(ctx: &DispatchCtx, ready: &ReadyCall) -> SettledCall {
    let reports = Arc::new(Mutex::new(Vec::new()));
    let mut elapsed_ms = None;
    let outcome = run_one_inner(ctx, ready, &reports, &mut elapsed_ms).await;
    let buffered = std::mem::take(&mut *reports.lock().await);
    SettledCall {
        call: ready.call.clone(),
        outcome,
        elapsed_ms,
        reports: buffered,
    }
}

/// Milliseconds a tool ran since `started`, minus the `waited` time it spent
/// on approval answers. The clock is monotonic, so wall-clock steps never
/// show up in the result.
fn execution_ms(started: Instant, waited: Duration) -> u64 {
    u64::try_from(started.elapsed().saturating_sub(waited).as_millis()).unwrap_or(u64::MAX)
}

/// Answers a `tool_search` call from the deferred tool catalog.
fn tool_search_outcome(ctx: &DispatchCtx, ready: &ReadyCall) -> SettledOutcome {
    match tool_search_query(&ready.args) {
        Ok(query) => SettledOutcome::Ok {
            text: tool_search_results(&query, &ctx.deferred_search),
            data: None,
        },
        Err(error) => SettledOutcome::Err {
            text: format!("invalid arguments for tool_search: {error}").into(),
        },
    }
}

/// Runs one call body, buffering its journal reports. `elapsed_ms` is set
/// only when a tool ran: calls blocked before execution leave it unset.
async fn run_one_inner(
    ctx: &DispatchCtx,
    ready: &ReadyCall,
    reports: &Arc<Mutex<Vec<TurnWork>>>,
    elapsed_ms: &mut Option<u64>,
) -> SettledOutcome {
    if is_core_tool_search(&ctx.generation, &ctx.tools, &ready.name) {
        reports.lock().await.push(TurnWork::CallStarted {
            turn: ctx.turn,
            call: ready.call.clone(),
        });
        let started = Instant::now();
        let outcome = tool_search_outcome(ctx, ready);
        *elapsed_ms = Some(execution_ms(started, Duration::ZERO));
        return outcome;
    }
    let Some((tool, _)) = ctx.tools.tool(&ctx.generation, &ready.name) else {
        return SettledOutcome::Err {
            text: format!("unknown tool {}.", ready.name.as_str()).into(),
        };
    };
    let tool = Arc::clone(tool);
    let event = ToolCallEvent {
        turn: ctx.turn,
        call: ready.call.clone(),
        tool: ready.name.clone(),
        class: ready.class.clone(),
        args: ready.args.clone(),
    };
    reports.lock().await.push(TurnWork::CallStarted {
        turn: ctx.turn,
        call: ready.call.clone(),
    });
    if let Err(error) = ctx
        .tools
        .authorize_mcp(&ctx.generation, &ready.name, ctx.turn)
        .await
    {
        return SettledOutcome::Err {
            text: error.to_string().into(),
        };
    }
    let args = match run_hooks(ctx, &event, ready.args.clone()).await {
        HookArgs::Blocked { reason } => {
            return SettledOutcome::Err { text: reason };
        }
        HookArgs::Args(args) => args,
    };
    if args.as_str() != ready.args.as_str() {
        let (promoted, result) = resolve_named(
            &ctx.generation,
            &ctx.tools,
            &ready.name,
            &args,
            ctx.mode,
            &ctx.workspace,
        );
        if result.is_err() {
            return failed_outcome(&ResolvedCall {
                call: ready.call.clone(),
                name: ready.name.clone(),
                promoted,
                result,
            });
        }
    }
    let invocation = Invocation::next();
    let caller = tool_caller(ctx, ready, invocation);
    let approval_wait = Arc::new(AtomicU64::new(0));
    let runtime = CallRuntime::new(
        ctx,
        ready,
        caller.ext().clone(),
        invocation,
        args.clone(),
        Arc::clone(reports),
        Arc::clone(&approval_wait),
    );
    let cx = ToolCx::new(
        caller,
        ready.call.clone(),
        ctx.session,
        Some(ctx.turn),
        Arc::clone(&ctx.services),
        Arc::new(runtime),
        CallSnapshot {
            generation: ctx.generation_id,
            cutoff: ctx.cutoff,
            env: Arc::clone(&ctx.backend.host_state().shared.env),
        },
    );
    let cx = match ctx.script.clone() {
        Some(script) => cx.with_script(script),
        None => cx,
    };
    let call = ToolCall::new(ready.call.as_str(), args);
    let started = Instant::now();
    let outcome = run_contained(&*tool, call, cx).await;
    let waited = Duration::from_nanos(approval_wait.load(Ordering::Relaxed));
    *elapsed_ms = Some(execution_ms(started, waited));
    map_outcome(outcome)
}

/// Hook outcome for one call's arguments.
enum HookArgs {
    /// A hook blocked the call with model-visible text.
    Blocked {
        /// The blocking reason.
        reason: Box<str>,
    },
    /// The final arguments after rewrites.
    Args(RawJson),
}

/// Folds every extension's `tool_call` hooks over the arguments in order.
async fn run_hooks(ctx: &DispatchCtx, event: &ToolCallEvent, args: RawJson) -> HookArgs {
    let mut current = args;
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
        let step = dispatch_tool_call(
            extension.name(),
            &dispatch,
            ctx.generation.tool_calls(index),
            event,
            current,
        )
        .await;
        if let Some(reason) = step.block {
            return HookArgs::Blocked {
                reason: format!("blocked by {}: {reason}", extension.name()).into(),
            };
        }
        current = step.args;
    }
    HookArgs::Args(current)
}

/// The caller attribution for one running tool call.
///
/// A tool acts for the extension that registered it: the extension's name,
/// origin, and declared `inject` set, exactly as `run_hooks` mints a hook
/// caller. A tool no extension owns keeps its own name, builtin origin, and
/// an empty set.
fn tool_caller(ctx: &DispatchCtx, ready: &ReadyCall, invocation: Invocation) -> Caller {
    let generation_owner = ctx
        .generation
        .tools
        .entries()
        .iter()
        .find(|entry| entry.name == ready.name)
        .and_then(|entry| ctx.generation.extensions.get(entry.ext));
    let overlay_owner = ctx.tools.owner(&ready.name).and_then(|owner| {
        ctx.generation
            .extensions
            .iter()
            .find(|extension| extension.name() == owner.as_str())
    });
    let owner = generation_owner.or(overlay_owner);
    let (ext, origin, inject, state_version) = owner
        .and_then(|extension| {
            let name = extension.name().parse::<Name>().ok()?;
            Some((
                name,
                extension.origin(),
                extension.inject(),
                extension.state_version(),
            ))
        })
        .unwrap_or_else(|| {
            (
                ready.name.clone(),
                dal_core::Origin::Builtin,
                dal_core::ext::ServiceSet::EMPTY,
                std::num::NonZeroU32::MIN,
            )
        });
    Caller::new(
        ext,
        origin,
        inject,
        state_version,
        CallerKind::Tool,
        Some(ctx.turn),
    )
    .with_invocation(invocation)
}

/// Runs one tool call and turns a panic into a visible tool error.
///
/// A panic would otherwise end the session driver task and leave the turn
/// waiting for a result that never comes. The error text names the tool and
/// carries the panic message, so the model and the client both see it.
async fn run_contained(tool: &dyn Tool, call: ToolCall, cx: ToolCx<'_>) -> ToolOutcome {
    let name = tool.name().clone();
    match contained(async move { tool.run(call, cx).await }).await {
        Ok(outcome) => outcome,
        Err(panic) => ToolOutcome::Err(crate::error::ToolError::Message {
            message: format!(
                "The {name} tool crashed and did not finish: {panic}. Report this to the tool's author, or try a different approach."
            )
            .into(),
        }),
    }
}

/// Maps one terminal tool outcome to its settlement.
fn map_outcome(outcome: ToolOutcome) -> SettledOutcome {
    match outcome {
        ToolOutcome::Ok(output) => SettledOutcome::Ok {
            text: output.to_string().into(),
            data: output.data,
        },
        ToolOutcome::Err(error) => match error {
            crate::error::ToolError::Cancelled => SettledOutcome::Interrupted,
            error => SettledOutcome::Err {
                text: error.to_string().into(),
            },
        },
        ToolOutcome::Interrupted => SettledOutcome::Interrupted,
        ToolOutcome::Detached(job) => SettledOutcome::Detached { job },
    }
}

/// One session approval grant enabling byte-identical reruns without asking.
struct LiveGrant {
    /// The tool name the grant covers.
    tool: Name,
    /// The preview digest the grant binds; `None` never matches.
    digest: Option<[u8; 32]>,
    /// The argv prefix tokens the spawn must start with.
    prefix: Box<[std::ffi::OsString]>,
    /// The canonical roots spawned children must stay inside.
    roots: Box<[std::path::PathBuf]>,
    /// The detached job bounding the grant, absent for session grants.
    job: Option<JobId>,
}

/// One ask-approved grant spec awaiting its detached job binding.
struct PendingIntent {
    /// The call whose approval proposed the grant.
    call: CallId,
    /// The tool name the grant covers.
    tool: Name,
    /// The preview digest the grant binds.
    digest: Option<[u8; 32]>,
    /// The proposed argv prefix and roots.
    spec: GrantSpec,
}

/// Turn-scoped approval grants shared by every call runtime.
///
/// Digest-bound rerun grants from detached jobs only. Explicit session
/// approvals live in the fold's allowlist and arrive through the batch
/// policy; this ledger never duplicates them. The ledger dies with the
/// turn. A job-scoped grant also requires a non-terminal row in the
/// session [`JobTable`] at authorization; an unknown or terminal job
/// fails closed.
pub(crate) struct GrantLedger {
    /// Digest-bound rerun grants from detached jobs.
    live: Vec<LiveGrant>,
    /// Ask-approved specs awaiting detach binding.
    pending: Vec<PendingIntent>,
}

impl GrantLedger {
    /// An empty ledger with no live or pending grants.
    pub(crate) fn new() -> Self {
        Self {
            live: Vec::new(),
            pending: Vec::new(),
        }
    }

    /// Finds the live grant covering one byte-identical rerun.
    ///
    /// A missing digest never matches: undigested previews authorize
    /// through the ladder every call.
    fn covers(&self, tool: &Name, digest: Option<[u8; 32]>) -> Option<GrantCover> {
        let digest = digest?;
        self.live
            .iter()
            .find(|grant| grant.tool == *tool && grant.digest == Some(digest))
            .map(|grant| GrantCover {
                prefix: grant.prefix.clone(),
                roots: grant.roots.clone(),
                job: grant.job,
            })
    }
}

/// The scope a covering grant lends one proof.
struct GrantCover {
    /// The argv prefix tokens the spawn must start with.
    prefix: Box<[std::ffi::OsString]>,
    /// The canonical roots spawned children must stay inside.
    roots: Box<[std::path::PathBuf]>,
    /// The detached job bounding the grant, absent for session grants.
    job: Option<JobId>,
}

fn job_is_live(jobs: &JobTable, job: Option<JobId>) -> bool {
    job.is_none_or(|id| jobs.is_live(id))
}

/// The per-call host backing behind [`ToolCx`].
struct CallRuntime {
    /// The owning turn, absent for turn-less direct calls.
    turn: Option<TurnId>,
    /// The running call.
    call: CallId,
    /// The running tool.
    tool: Name,
    /// The extension that owns the tool; its `run` service calls ride the
    /// grant this call earns.
    ext: Name,
    invocation: Option<Invocation>,
    /// The session the call runs in.
    session: SessionId,
    /// The final arguments after hooks.
    args: RawJson,
    /// The ladder policy snapshot.
    policy: Policy,
    /// The session broker for approval questions.
    broker: Arc<Broker>,
    /// The turn's generation snapshot for tool lookup.
    generation: Arc<Generation>,
    /// The turn's overlay tools for tool lookup.
    tools: TurnTools,
    /// The session workspace spawned children resolve against.
    workspace: Workspace,
    /// The shared session snapshot used by extension-scheme contexts.
    shared: Arc<crate::session::shared::Shared>,
    /// The session-start entry snapshot used by history schemes.
    initial_entries: Arc<[dal_core::EntryView]>,
    scheme_store: Arc<dal_store::Store>,
    /// The store session `jobs/` directory for process witnesses.
    jobs_dir: PathBuf,
    /// Captured environment snapshot for spawned children.
    env_snapshot: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    /// Process launcher prepared at session start; the error side refuses
    /// every spawn with the exact sandbox setup text.
    launcher: Result<crate::proc::Launcher, crate::proc::sandbox::SandboxSetupError>,
    /// Session job table for detach parking.
    jobs: Arc<tokio::sync::Mutex<crate::jobs::JobTable>>,
    /// Host state for admission gates.
    host: Arc<crate::host::HostState>,
    /// The session handle for immediate actor reports.
    handle: crate::session::SessionHandle,
    /// Journal reports buffered for the driver to forward in order.
    reports: Arc<Mutex<Vec<TurnWork>>>,
    /// Nanoseconds this call spent waiting for approval answers, which the
    /// dispatcher subtracts from the tool's run time.
    approval_wait: Arc<AtomicU64>,
    /// Turn approval grants.
    ledger: Arc<Mutex<GrantLedger>>,
    /// The authorization proof state: the bound digest once approved.
    auth: Mutex<Option<AuthProof>>,
    /// The session owner for detached background work.
    tasks: SessionTasks,
    /// The cancellation token bounding this call.
    cancel: CancellationToken,
}

/// The bound digest of one approved call.
#[derive(Clone, Copy)]
struct AuthProof {
    /// The preview digest the spawn door must see.
    digest: Option<[u8; 32]>,
}

impl CallRuntime {
    /// Builds the runtime for one dispatched call.
    fn new(
        ctx: &DispatchCtx,
        ready: &ReadyCall,
        ext: Name,
        invocation: Invocation,
        args: RawJson,
        reports: Arc<Mutex<Vec<TurnWork>>>,
        approval_wait: Arc<AtomicU64>,
    ) -> Self {
        Self {
            turn: Some(ctx.turn),
            call: ready.call.clone(),
            tool: ready.name.clone(),
            ext,
            invocation: Some(invocation),
            session: ctx.session,
            args,
            policy: ctx.policy.clone(),
            broker: Arc::clone(&ctx.broker),
            generation: Arc::clone(&ctx.generation),
            tools: ctx.tools.clone(),
            workspace: ctx.workspace.clone(),
            shared: Arc::clone(ctx.backend.shared()),
            initial_entries: Arc::clone(ctx.backend.initial_entries()),
            scheme_store: Arc::clone(ctx.backend.scheme_store()),
            jobs_dir: ctx.backend.jobs_dir().to_path_buf(),
            env_snapshot: ctx.backend.env_snapshot().to_vec(),
            launcher: ctx.backend.launcher().clone(),
            jobs: Arc::clone(ctx.backend.jobs()),
            host: Arc::clone(ctx.backend.host_state()),
            handle: ctx.backend.handle().clone(),
            reports,
            approval_wait,
            tasks: ctx.backend.tasks().clone(),
            ledger: Arc::clone(&ctx.ledger),
            auth: Mutex::new(None),
            cancel: ctx.cancel.clone(),
        }
    }

    /// Runs the approval ladder over the tool preview, minting the proof.
    ///
    /// Allow mints an empty-prefix proof binding (call, digest) only; the
    /// spawn door checks digest and cwd. Ask opens a broker approval and
    /// mints on approval, records session grants, and denies with
    /// model-visible text otherwise.
    async fn authorize_inner(
        &self,
        call: &CallId,
        preview: Preview,
        cancel: &CancellationToken,
    ) -> Result<Approved, dal_core::DenyReason> {
        let approved = self.authorize_ladder(call, preview, cancel).await?;
        self.lend_service_grant();
        Ok(approved)
    }

    /// Lends the scoped grant a tool's classification carries to the `run`
    /// calls its extension makes for this call. Any approval earns it, an
    /// answered ask or an allow by policy alike.
    fn lend_service_grant(&self) {
        let Ok(ToolClass::Exec {
            grant: Some(spec), ..
        }) = self.approval_class()
        else {
            return;
        };
        let prefix = spec
            .argv_prefix
            .split_ascii_whitespace()
            .map(std::ffi::OsString::from)
            .collect();
        let Some(invocation) = self.invocation else {
            return;
        };
        let roots = scope_roots(
            spec.roots,
            &self.host.shared.data_root,
            self.workspace.as_path(),
            self.session,
        );
        self.shared.service_grants().register(
            CallKey::new(self.ext.clone(), invocation),
            prefix,
            roots,
        );
    }

    /// The approval ladder itself, before any grant lending.
    async fn authorize_ladder(
        &self,
        call: &CallId,
        preview: Preview,
        cancel: &CancellationToken,
    ) -> Result<Approved, dal_core::DenyReason> {
        use dal_core::DenyReason;
        if call != &self.call {
            return Err(DenyReason::out_of_scope(format!("call {}", call.as_str())));
        }
        let class = self.approval_class()?;
        if let Some(grant) = self.ledger.lock().await.covers(&self.tool, preview.digest) {
            return self
                .finish_approval(
                    preview.digest,
                    grant.prefix.clone(),
                    grant.roots.clone(),
                    grant.job,
                    &class,
                    cancel,
                )
                .await;
        }
        match self.policy.decide(&self.tool, &class) {
            dal_core::Decision::Allow => {
                self.finish_approval(
                    preview.digest,
                    Box::new([]),
                    Box::new([self.workspace.as_path().to_path_buf()]),
                    None,
                    &class,
                    cancel,
                )
                .await
            }
            dal_core::Decision::Deny { reason } => Err(match reason {
                DenyReason::NoFrontEnd => self.headless_denial(&class),
                reason => reason,
            }),
            dal_core::Decision::Ask { grant } => self.ask(call, preview, grant, cancel).await,
            _ => Err(dal_core::DenyReason::out_of_scope(self.tool.as_str())),
        }
    }

    /// Looks up the tool and classifies the final arguments for the ladder.
    fn approval_class(&self) -> Result<ToolClass, dal_core::DenyReason> {
        use dal_core::DenyReason;
        let Some((tool, _)) = self.tools.tool(&self.generation, &self.tool) else {
            return Err(DenyReason::out_of_scope(self.tool.as_str()));
        };
        tool.classify(&self.args, &self.workspace)
            .map_err(|_| DenyReason::out_of_scope(self.tool.as_str()))
    }

    /// The model-visible denial for a gated call with no one to ask. The
    /// front end owns the matching stderr note and its rerun hint names the
    /// approval rung the call needed.
    fn headless_denial(&self, class: &ToolClass) -> dal_core::DenyReason {
        dal_core::DenyReason::out_of_scope(dal_core::headless_denial_text(
            self.tool.as_str(),
            dal_core::rung(class),
        ))
    }

    async fn finish_approval(
        &self,
        digest: Option<[u8; 32]>,
        prefix: Box<[std::ffi::OsString]>,
        roots: Box<[std::path::PathBuf]>,
        job: Option<JobId>,
        class: &ToolClass,
        cancel: &CancellationToken,
    ) -> Result<Approved, dal_core::DenyReason> {
        use dal_core::DenyReason;
        let mut approved = Approved::new(self.call.clone(), digest, prefix, roots, job);
        if !job_is_live(&*self.jobs.lock().await, job) {
            return Err(DenyReason::NotGranted);
        }
        if matches!(class, ToolClass::Exec { .. }) {
            let permit = self
                .host
                .shared
                .admission
                .acquire_process(cancel)
                .await
                .map_err(|error| match error {
                    crate::error::ToolError::Cancelled => DenyReason::Unavailable {
                        what: "turn cancelled".into(),
                    },
                    crate::error::ToolError::Admission { limit } => DenyReason::Unavailable {
                        what: format!(
                            "no free {} slot within limits.admission_wait",
                            limit.as_str()
                        )
                        .into(),
                    },
                    _ => DenyReason::Unavailable {
                        what: "process unavailable".into(),
                    },
                })?;
            let fd_permit = self
                .host
                .shared
                .admission
                .charge_fds(crate::proc::PROCESS_FD_COST, cancel)
                .await
                .map_err(|error| match error {
                    crate::error::ToolError::Cancelled => DenyReason::Unavailable {
                        what: "turn cancelled".into(),
                    },
                    crate::error::ToolError::Admission { limit } => DenyReason::Unavailable {
                        what: format!(
                            "no free {} slot within limits.admission_wait",
                            limit.as_str()
                        )
                        .into(),
                    },
                    _ => DenyReason::Unavailable {
                        what: "file descriptors unavailable".into(),
                    },
                })?;
            approved = approved.with_permits(permit, fd_permit);
        }
        *self.auth.lock().await = Some(AuthProof { digest });
        Ok(approved)
    }
}

impl CallRuntime {
    /// Opens one approval question and mints the proof on approval.
    async fn ask(
        &self,
        call: &CallId,
        preview: Preview,
        grant: Option<GrantSpec>,
        cancel: &CancellationToken,
    ) -> Result<Approved, dal_core::DenyReason> {
        use dal_core::DenyReason;
        let Some(turn) = self.turn else {
            let class = self.approval_class().unwrap_or(ToolClass::Other);
            return Err(self.headless_denial(&class));
        };
        let question = Question::Approval {
            tool: self.tool.as_str().into(),
            preview: preview.clone(),
            grant: grant.clone().map(|spec| dal_core::CallGrant {
                argv_prefix: spec.argv_prefix,
                roots: spec.roots,
                until: JobEnd(JobId::new_v7()),
            }),
            call: Some(call.clone()),
        };
        let secs = default_timeout(&question).as_secs();
        let deadline = tokio::time::Instant::now() + default_timeout(&question);
        let owner = tool_owner(&self.generation, &self.tools, &self.tool);
        let (request, waiter) = self.broker.open(owner, question, turn, deadline);
        if self.report_asked(&request).await.is_err() {
            if let Ok(resolved) =
                self.broker
                    .answer(request.id, Answer::Cancel, ClientId::new("core"))
            {
                self.broker.cancel(&resolved);
            }
            return Err(dal_core::DenyReason::Unavailable {
                what: "session closed while opening approval".into(),
            });
        }
        let asked_at = Instant::now();
        let answered = tokio::select! {
            biased;
            () = cancel.cancelled() => None,
            () = self.cancel.cancelled() => None,
            outcome = waiter => Some(outcome),
        };
        let blocked = u64::try_from(asked_at.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.approval_wait.fetch_add(blocked, Ordering::Relaxed);
        let Some(Settled {
            answer,
            by,
            resolution,
        }) = answered
        else {
            return Err(DenyReason::Unavailable {
                what: "turn cancelled".into(),
            });
        };
        self.report_answered(&request, &answer, &by, resolution, false)
            .await;
        match answer {
            Answer::Approve | Answer::ApproveForSession => {
                let roots = grant.clone().map_or_else(
                    || {
                        Box::new([self.workspace.as_path().to_path_buf()])
                            as Box<[std::path::PathBuf]>
                    },
                    |spec| spec.roots.into_boxed_slice(),
                );
                if let Some(spec) = grant {
                    self.ledger.lock().await.pending.push(PendingIntent {
                        call: call.clone(),
                        tool: self.tool.clone(),
                        digest: preview.digest,
                        spec,
                    });
                }
                let class = self.approval_class()?;
                self.finish_approval(preview.digest, Box::new([]), roots, None, &class, cancel)
                    .await
            }
            Answer::Decline => Err(DenyReason::out_of_scope(match resolution {
                Resolution::Unavailable => format!(
                    "Permission denied: {} needed approval and no one answered within {secs} s.",
                    self.tool.as_str()
                ),
                Resolution::Answered | Resolution::Cancelled => format!(
                    "Permission denied: {} was declined by {}.",
                    self.tool.as_str(),
                    by.as_str()
                ),
            })),
            Answer::Cancel => Err(DenyReason::Unavailable {
                what: "approval cancelled".into(),
            }),
            _ => Err(DenyReason::out_of_scope(format!(
                "Permission denied: {}.",
                self.tool.as_str()
            ))),
        }
    }

    /// Publishes one opened approval for journaling.
    ///
    /// `Asked` goes straight to the actor: the ask blocks this call until
    /// an answer arrives, so a deferred buffer would publish the request
    /// only after the answer it is supposed to enable.
    async fn report_asked(&self, request: &Request) -> Result<(), crate::error::AgentError> {
        self.handle
            .work(TurnWork::Asked {
                request: request.clone(),
            })
            .await?;
        self.shared
            .publish(dal_core::UpdateKind::RequestOpened(request.clone()));
        Ok(())
    }

    /// Buffers one broker resolution for journaling.
    async fn report_answered(
        &self,
        request: &Request,
        answer: &Answer,
        by: &ClientId,
        resolution: Resolution,
        was_default: bool,
    ) {
        self.reports.lock().await.push(TurnWork::Answered {
            resolved: Resolved {
                request: request.clone(),
                answer: answer.clone(),
                by: by.clone(),
                resolution,
                was_default,
            },
        });
    }
}

/// The owning extension attribution for one tool approval.
fn tool_owner(generation: &Generation, tools: &TurnTools, name: &Name) -> Owner {
    let generation_owner = generation
        .tools
        .entries()
        .iter()
        .find(|entry| entry.name == *name)
        .and_then(|entry| generation.extensions.get(entry.ext));
    let overlay_owner = tools.owner(name).and_then(|owner| {
        generation
            .extensions
            .iter()
            .find(|extension| extension.name() == owner.as_str())
    });
    let origin = generation_owner
        .or(overlay_owner)
        .map_or("unknown", |extension| match extension.origin() {
            dal_core::Origin::Builtin => "builtin",
            dal_core::Origin::Bundled => "bundled",
            dal_core::Origin::User => "user",
            _ => "unknown",
        });
    Owner::Extension {
        name: name.as_str().into(),
        origin: origin.into(),
    }
}

impl Drop for CallRuntime {
    /// The call is over; the jobs it started now bound its run grant.
    fn drop(&mut self) {
        if let Some(invocation) = self.invocation {
            self.shared
                .service_grants()
                .call_ended(&CallKey::new(self.ext.clone(), invocation));
        }
    }
}

impl ToolCxRuntime for CallRuntime {
    fn decide_run(&self) -> dal_core::Decision {
        self.policy
            .decide(&super::rt::service_tool(), &super::rt::service_class())
    }

    fn authorize(
        &self,
        call: &CallId,
        preview: Preview,
        cancel: &CancellationToken,
    ) -> BoxFuture<'_, Result<Approved, dal_core::DenyReason>> {
        let call = call.clone();
        let cancel = cancel.clone();
        Box::pin(async move { self.authorize_inner(&call, preview, &cancel).await })
    }

    fn authorize_approved(
        &self,
        _call: &CallId,
        preview: Preview,
        cancel: &CancellationToken,
    ) -> BoxFuture<'_, Result<Approved, dal_core::DenyReason>> {
        let cancel = cancel.clone();
        Box::pin(async move {
            let class = self.approval_class()?;
            self.finish_approval(
                preview.digest,
                Box::new([]),
                Box::new([self.workspace.as_path().to_path_buf()]),
                None,
                &class,
                &cancel,
            )
            .await
        })
    }

    fn spawn(
        &self,
        argv: &[std::ffi::OsString],
        opts: SpawnOpts,
        mut approved: Approved,
    ) -> Result<Proc, crate::error::ToolError> {
        use crate::error::ToolError;
        if let Some(job) = approved.job() {
            let Ok(jobs) = self.jobs.try_lock() else {
                return Err(ToolError::Denied(dal_core::DenyReason::NotGranted));
            };
            if !jobs.is_live(job) {
                return Err(ToolError::Denied(dal_core::DenyReason::NotGranted));
            }
        }
        let proof = match self.auth.try_lock() {
            Ok(guard) => *guard,
            Err(_) => None,
        };
        let Some(proof) = proof else {
            return Err(ToolError::Denied(dal_core::DenyReason::NotGranted));
        };
        let bound = proof.digest;
        if !approved.prefix().is_empty() && !grant_covers(&approved, argv, &opts.cwd) {
            return Err(ToolError::Denied(dal_core::DenyReason::out_of_scope(
                self.tool.as_str(),
            )));
        }
        let permit = approved.take_permits();
        let Some((permit, fd_permit)) = permit else {
            return Err(ToolError::Denied(dal_core::DenyReason::NotGranted));
        };
        let launcher = match &self.launcher {
            Ok(launcher) => launcher,
            Err(setup) => return Err(setup.tool_error()),
        };
        spawn_process(
            argv,
            self.call.clone(),
            opts,
            &approved,
            bound,
            &self.workspace,
            &self.jobs_dir,
            &self.env_snapshot,
            launcher,
            permit,
            Some(fd_permit),
        )
    }

    fn detach(&self, proc: Proc) -> JobId {
        let job = proc.job_id();
        let park = ParkDetached {
            jobs: Arc::clone(&self.jobs),
            ledger: Arc::clone(&self.ledger),
            call: self.call.clone(),
            tool: self.tool.clone(),
            proc,
        };
        self.tasks.spawn(async move { park.run().await });
        job
    }

    fn resolve(
        &self,
        uri: &str,
        context: crate::ext::scheme::SchemeResolveContext<'_>,
    ) -> BoxFuture<'_, Result<Doc, crate::error::ToolError>> {
        let uri: Box<str> = uri.into();
        let jobs = Arc::clone(&self.jobs);
        if uri.starts_with("job://") {
            return Box::pin(async move {
                let table = jobs.lock().await;
                table.read_uri(&uri)
            });
        }
        let store = Arc::clone(&self.scheme_store);
        let shared = Arc::clone(&self.shared);
        let initial_entries = Arc::clone(&self.initial_entries);
        crate::session::rt::resolve_extension_scheme(uri, &self.generation, &context, move || {
            (store, shared, initial_entries)
        })
    }

    fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    fn cancel(&self) -> &CancellationToken {
        &self.cancel
    }
}

/// Checks one grant-bound spawn: the grant's whitespace-split prefix tokens
/// start the actual argv with exact tokens and the cwd sits below a grant
/// root. Git path selectors (`-C`, `--git-dir`, and `--work-tree`) are checked against
/// the same roots, resolving relative operands from the effective directory.
/// An empty prefix never matches here; one-shot approvals carry none.
pub(crate) fn grant_covers(approved: &Approved, argv: &[std::ffi::OsString], cwd: &Path) -> bool {
    if approved.prefix().is_empty() || argv.is_empty() {
        return false;
    }
    let mut actual = argv.iter();
    for expected in approved.prefix() {
        match actual.next() {
            Some(arg) if arg == expected => {}
            _ => return false,
        }
    }
    crate::proc::cwd_in_roots(cwd, approved.roots())
        && git_paths_in_roots(argv, cwd, approved.roots())
}

/// Checks every git path selector in the global argument portion.
///
/// Git applies relative `-C` paths successively, so each one becomes the
/// effective directory for later path operands. `--git-dir` and `--work-tree`
/// are checked against that final directory. Unknown options are skipped;
/// `--` ends option parsing. Invalid or missing operands fail closed.
fn git_paths_in_roots(argv: &[std::ffi::OsString], cwd: &Path, roots: &[PathBuf]) -> bool {
    let Some(program) = argv.first() else {
        return false;
    };
    if Path::new(program).file_stem() != Some(std::ffi::OsStr::new("git")) {
        return true;
    }
    let Some(base) = git_c_base(argv, cwd, roots) else {
        return false;
    };
    git_dirs_in_roots(argv, &base, roots)
}

fn git_c_base(argv: &[std::ffi::OsString], cwd: &Path, roots: &[PathBuf]) -> Option<PathBuf> {
    let mut base = cwd.to_path_buf();
    let mut index = 1;
    while index < argv.len() {
        let arg = &argv[index];
        if arg == std::ffi::OsStr::new("--") {
            break;
        }
        if arg == std::ffi::OsStr::new("--git-dir") || arg == std::ffi::OsStr::new("--work-tree") {
            argv.get(index + 1)?;
            index += 2;
            continue;
        }
        if arg == std::ffi::OsStr::new("-C") {
            let path = argv.get(index + 1)?;
            base = checked_git_path(&base, path, roots)?;
            index += 2;
            continue;
        }
        let text = arg.to_str()?;
        let Some(path) = text.strip_prefix("-C") else {
            index += 1;
            continue;
        };
        base = checked_git_path(&base, std::ffi::OsStr::new(path), roots)?;
        index += 1;
    }
    Some(base)
}

fn git_dirs_in_roots(argv: &[std::ffi::OsString], base: &Path, roots: &[PathBuf]) -> bool {
    let mut index = 1;
    while index < argv.len() {
        let arg = &argv[index];
        if arg == std::ffi::OsStr::new("--") {
            break;
        }
        if arg == std::ffi::OsStr::new("--git-dir") || arg == std::ffi::OsStr::new("--work-tree") {
            let Some(path) = argv.get(index + 1) else {
                return false;
            };
            if checked_git_path(base, path, roots).is_none() {
                return false;
            }
            index += 2;
            continue;
        }
        let Some(text) = arg.to_str() else {
            return false;
        };
        if let Some(path) = text
            .strip_prefix("--git-dir=")
            .or_else(|| text.strip_prefix("--work-tree="))
            && checked_git_path(base, std::ffi::OsStr::new(path), roots).is_none()
        {
            return false;
        }
        index += 1;
    }
    true
}

fn checked_git_path(base: &Path, path: &std::ffi::OsStr, roots: &[PathBuf]) -> Option<PathBuf> {
    let resolved = resolve_git_path(base, path)?;
    crate::proc::cwd_in_roots(&resolved, roots).then_some(resolved)
}

fn resolve_git_path(base: &Path, path: &std::ffi::OsStr) -> Option<PathBuf> {
    if path.is_empty() {
        return None;
    }
    let path = Path::new(path);
    Some(if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    })
}

/// Parks one detached child: binds pending grant intents, reserves the job
/// row, and retains the process for the jobs loop.
struct ParkDetached {
    /// The session job table.
    jobs: Arc<tokio::sync::Mutex<crate::jobs::JobTable>>,
    /// Turn approval grants.
    ledger: Arc<Mutex<GrantLedger>>,
    /// The detaching call.
    call: CallId,
    /// The detaching tool.
    tool: Name,
    /// The detached child.
    proc: Proc,
}

impl ParkDetached {
    /// Binds intents, reserves the row, and retains the child.
    async fn run(self) {
        let Self {
            jobs,
            ledger,
            call,
            tool,
            proc,
        } = self;
        let job = proc.job_id();
        let log_path = proc.log_path().to_path_buf();
        let mut ledger = ledger.lock().await;
        let mut index = 0;
        while index < ledger.pending.len() {
            if ledger.pending[index].call == call {
                let intent = ledger.pending.remove(index);
                ledger.live.push(LiveGrant {
                    tool: intent.tool,
                    digest: intent.digest,
                    prefix: intent
                        .spec
                        .argv_prefix
                        .split_ascii_whitespace()
                        .map(std::ffi::OsString::from)
                        .collect::<Vec<_>>()
                        .into_boxed_slice(),
                    roots: intent.spec.roots.into_boxed_slice(),
                    job: Some(job),
                });
            } else {
                index += 1;
            }
        }
        drop(ledger);
        let cancel = CancellationToken::new();
        let record = crate::jobs::JobRecord::new(job, tool.as_str(), log_path, cancel.clone());
        let mut table = jobs.lock().await;
        if table.reserve(record).is_err() || table.adopt_detached(job).is_err() {
            return;
        }
        drop(table);
        crate::jobs::reap_detached(proc, jobs, cancel).await;
    }
}

/// The per-call inputs one nested tool call runs with (R03 R04).
pub(crate) struct NestedCall {
    /// The registered tool name: a native op's product tool or an
    /// export's provider-wire name.
    pub name: Name,
    /// The canonical bound arguments.
    pub args: RawJson,
    /// The host-minted call identity.
    pub call: CallId,
    /// The calling identity: the owning plugin for export ops, the
    /// calling invocation's plugin for native ops (R04).
    pub caller: Caller,
    /// The live turn of a scripted caller, when the call runs inside one.
    pub turn: Option<TurnId>,
    /// The cancellation token bounding the call.
    pub cancel: CancellationToken,
    /// The frozen observation cutoff of the call (R06).
    pub cutoff: Option<u64>,
    /// The script context of a scripted caller, when one is active (E06).
    pub script: Option<ScriptCx>,
    /// The session broker the nested call reports through.
    pub broker: Option<Arc<crate::broker::Broker>>,
    /// The session-shared state the nested call reads.
    pub shared: Option<Arc<crate::session::shared::Shared>>,
    /// Whether the answering client is attached to the session.
    pub attached: Option<bool>,
}

/// Runs one tool-to-tool nested call without turn dispatch.
///
/// Turn-less calls cannot ask: reads and other allow-listed calls run,
/// everything else denies without a frontend. Reports forward to the
/// actor through the session port.
pub(crate) async fn direct_call(backend: &Backend, name: &str, args: RawJson) -> ToolOutcome {
    let Ok(tool_name) = Name::parse_mapped_tool(name) else {
        return ToolOutcome::Err(crate::error::ToolError::message(format!(
            "unknown tool {name}."
        )));
    };
    let caller = Caller::new(
        tool_name.clone(),
        dal_core::Origin::Builtin,
        dal_core::ext::ServiceSet::EMPTY,
        std::num::NonZeroU32::MIN,
        CallerKind::Tool,
        None,
    );
    let seed = NestedCall {
        name: tool_name,
        args,
        call: direct_call_id(),
        caller,
        turn: None,
        cancel: backend.turn_cancel().clone(),
        cutoff: None,
        script: None,
        broker: None,
        shared: None,
        attached: None,
    };
    direct_call_seeded(backend, seed).await
}

/// Finds the tool a nested call names.
///
/// Scripts and extensions call tools on the session's behalf, so they meet
/// its tool allowlist like the model does. A name outside the list finds
/// nothing, like an unknown tool.
fn nested_tool<'a>(
    backend: &Backend,
    generation: &'a Generation,
    name: &Name,
) -> Option<&'a Arc<dyn Tool>> {
    let permitted = backend
        .shared()
        .tool_allowlist()
        .is_none_or(|allowed| allowed.contains(name));
    if !permitted {
        return None;
    }
    generation.tool(name).map(|(tool, _)| tool)
}

/// Runs one seeded nested call through the checked tool path (R03 R04).
pub(crate) async fn direct_call_seeded(backend: &Backend, seed: NestedCall) -> ToolOutcome {
    use dal_core::DenyReason;
    let NestedCall {
        name,
        args,
        call,
        caller,
        turn,
        cancel,
        cutoff,
        script,
        broker,
        shared,
        attached,
    } = seed;
    let Some(services) = backend.services().get() else {
        return ToolOutcome::Err(crate::error::ToolError::Denied(DenyReason::Unavailable {
            what: "services".into(),
        }));
    };
    let generation = backend.host_state().shared.generation.borrow().clone();
    let Some(tool) = nested_tool(backend, &generation, &name) else {
        return ToolOutcome::Err(crate::error::ToolError::message(format!(
            "unknown tool {}.",
            name.as_str()
        )));
    };
    let answering = backend.answerer_context();
    let (broker, shared, attached) = answering.map_or_else(
        || {
            (
                broker.unwrap_or_else(|| Arc::clone(backend.broker())),
                shared.unwrap_or_else(|| Arc::clone(backend.shared())),
                attached.unwrap_or_else(|| backend.shared().attached_approval()),
            )
        },
        |(broker, shared)| (broker, shared, true),
    );
    let policy = nested_policy(caller.cell_approved(), attached);
    let tool = Arc::clone(tool);
    let reports = Arc::new(Mutex::new(Vec::new()));
    let runtime = CallRuntime {
        approval_wait: Arc::default(),
        turn,
        call: call.clone(),
        tool: name.clone(),
        ext: caller.ext().clone(),
        invocation: None,
        session: backend.session(),
        args: args.clone(),
        policy,
        broker,
        generation: Arc::clone(&generation),
        tools: TurnTools::empty(),
        workspace: backend.workspace().clone(),
        shared,
        initial_entries: Arc::clone(backend.initial_entries()),
        scheme_store: Arc::clone(backend.scheme_store()),
        jobs_dir: backend.jobs_dir().to_path_buf(),
        env_snapshot: backend.env_snapshot().to_vec(),
        launcher: backend.launcher().clone(),
        jobs: Arc::clone(backend.jobs()),
        host: Arc::clone(backend.host_state()),
        handle: backend.handle().clone(),
        reports: Arc::clone(&reports),
        tasks: backend.tasks().clone(),
        ledger: Arc::new(Mutex::new(GrantLedger::new())),
        auth: Mutex::new(None),
        cancel,
    };
    let cx = ToolCx::new(
        caller,
        call.clone(),
        backend.session(),
        turn,
        Arc::clone(services),
        Arc::new(runtime),
        CallSnapshot {
            generation: generation.id,
            cutoff,
            env: Arc::clone(&backend.host_state().shared.env),
        },
    );
    let cx = match script {
        Some(script) => cx.with_script(script),
        None => cx,
    };
    let outcome = run_contained(&*tool, ToolCall::new(call.as_str(), args), cx).await;
    for report in std::mem::take(&mut *reports.lock().await) {
        let _ = backend.handle().work(report).await;
    }
    outcome
}

/// The approval policy of one nested call: an approved eval cell runs
/// without asking; every other nested call asks.
fn nested_policy(approved_cell: bool, answerer_attached: bool) -> Policy {
    let mode = if approved_cell {
        dal_core::ApprovalMode::All
    } else {
        dal_core::ApprovalMode::Ask
    };
    Policy {
        mode,
        answerer_attached,
        allow_always: std::collections::BTreeSet::new(),
    }
}

/// Mints a unique call identity for one turn-less direct call.
fn direct_call_id() -> CallId {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    CallId::new(format!("direct-{n}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn execution_time_excludes_approval_waits() {
        let started = Instant::now()
            .checked_sub(Duration::from_millis(500))
            .expect("monotonic clock has run for half a second");
        let ms = execution_ms(started, Duration::from_millis(200));
        assert!((300..500).contains(&ms), "{ms}");
    }

    #[test]
    fn job_scoped_grant_requires_a_live_job() -> Result<(), crate::error::ToolError> {
        let mut jobs = JobTable::new();
        let id = JobId::new_v7();
        let record = crate::jobs::JobRecord::new(
            id,
            "grant-test",
            std::path::PathBuf::from("/tmp/grant-test.log"),
            CancellationToken::new(),
        );
        jobs.reserve(record)?;
        assert!(job_is_live(&jobs, Some(id)));
        assert!(!job_is_live(&jobs, Some(JobId::new_v7())));
        jobs.mark_running(id)?;
        assert!(job_is_live(&jobs, Some(id)));
        jobs.settle_once(id, dal_core::JobOutcome::Exited { code: 0 }, Box::new([]));
        assert!(!job_is_live(&jobs, Some(id)));
        assert!(job_is_live(&jobs, None));
        Ok(())
    }
    #[test]
    fn git_c_and_git_dir_stay_inside_a_nested_workspace() {
        let temp = tempfile::tempdir().expect("temporary grant roots");
        let outer = temp.path().join("outer-repo");
        let workspace = outer.join("agent-workspace");
        let nested = workspace.join("nested-repo");
        let workspace_git = workspace.join(".git");
        let outside_git = outer.join("outside.git");
        let inside_git = workspace.join("outside.git");
        std::fs::create_dir_all(&nested).expect("nested repository");
        std::fs::create_dir_all(&workspace_git).expect("workspace git directory");
        std::fs::create_dir_all(&inside_git).expect("nested relative git directory");
        std::fs::create_dir_all(&outside_git).expect("outside git directory");
        let approved = Approved::new(
            CallId::new("git-grant"),
            None,
            Box::new([std::ffi::OsString::from("git")]),
            Box::new([workspace.clone()]),
            None,
        );

        let nested_relative = vec![
            "git".into(),
            "-C".into(),
            "nested-repo".into(),
            "status".into(),
        ];
        assert!(grant_covers(&approved, &nested_relative, &workspace));
        let enclosing_absolute = vec![
            "git".into(),
            "-C".into(),
            outer.as_os_str().to_os_string(),
            "status".into(),
        ];
        assert!(!grant_covers(&approved, &enclosing_absolute, &workspace));

        let reordered_git_dir = vec![
            "git".into(),
            "--git-dir".into(),
            "../outside.git".into(),
            "-C".into(),
            "..".into(),
            "status".into(),
        ];
        assert!(!grant_covers(&approved, &reordered_git_dir, &nested));

        let git_dir_relative = vec![
            "git".into(),
            "--git-dir".into(),
            ".git".into(),
            "status".into(),
        ];
        assert!(grant_covers(&approved, &git_dir_relative, &workspace));
        let mut outside_git_arg = std::ffi::OsString::from("--git-dir=");
        outside_git_arg.push(outside_git.as_os_str());
        let outside_git_dir = vec!["git".into(), outside_git_arg, "status".into()];
        assert!(!grant_covers(&approved, &outside_git_dir, &workspace));
    }
    #[test]
    fn git_work_tree_stays_inside_a_nested_workspace() {
        let temp = tempfile::tempdir().expect("temporary grant roots");
        let outer = temp.path().join("outer-repo");
        let workspace = outer.join("agent-workspace");
        let nested = workspace.join("nested-repo");
        let workspace_git = workspace.join(".git");
        let outside_git = outer.join("outside.git");
        let inside_git = workspace.join("outside.git");
        let work_tree_dash_c = workspace.join("-Cnested");
        std::fs::create_dir_all(&nested).expect("nested repository");
        std::fs::create_dir_all(&workspace_git).expect("workspace git directory");
        std::fs::create_dir_all(&inside_git).expect("nested relative git directory");
        std::fs::create_dir_all(&outside_git).expect("outside git directory");
        std::fs::create_dir_all(&work_tree_dash_c).expect("dash-prefixed work tree");
        let approved = Approved::new(
            CallId::new("work-tree-grant"),
            None,
            Box::new([std::ffi::OsString::from("git")]),
            Box::new([workspace.clone()]),
            None,
        );

        let work_tree_relative = vec![
            "git".into(),
            "--git-dir".into(),
            ".git".into(),
            "--work-tree".into(),
            "nested-repo".into(),
            "status".into(),
        ];
        assert!(grant_covers(&approved, &work_tree_relative, &workspace));

        let work_tree_absolute = vec![
            "git".into(),
            "--git-dir".into(),
            ".git".into(),
            "--work-tree".into(),
            outer.as_os_str().to_os_string(),
            "status".into(),
        ];
        assert!(!grant_covers(&approved, &work_tree_absolute, &workspace));

        let mut outside_work_tree_arg = std::ffi::OsString::from("--work-tree=");
        outside_work_tree_arg.push(outer.as_os_str());
        let outside_work_tree_equals = vec![
            "git".into(),
            "--git-dir".into(),
            ".git".into(),
            outside_work_tree_arg,
            "status".into(),
        ];
        assert!(!grant_covers(
            &approved,
            &outside_work_tree_equals,
            &workspace
        ));
        let work_tree_operand_that_looks_like_c = vec![
            "git".into(),
            "--work-tree".into(),
            "-Cnested".into(),
            "--git-dir".into(),
            "../outside.git".into(),
            "status".into(),
        ];
        assert!(!grant_covers(
            &approved,
            &work_tree_operand_that_looks_like_c,
            &workspace,
        ));

        let mut nested_work_tree_arg = std::ffi::OsString::from("--work-tree=");
        nested_work_tree_arg.push(nested.as_os_str());
        let work_tree_equals = vec![
            "git".into(),
            "--git-dir".into(),
            ".git".into(),
            nested_work_tree_arg,
            "status".into(),
        ];
        assert!(grant_covers(&approved, &work_tree_equals, &workspace));
    }
}
