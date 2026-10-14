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
    /// captured an eval environment this request.
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
/// read run; every other call dispatches alone. A tool named in dal-core's
/// [`dal_core::EXCLUSIVE_TOOLS`] never joins a read run even when its class
/// is read-only: it blocks on a human and must not stall parallel reads.
/// Resolution failures settle immediately without running.
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
        let exclusive = dal_core::EXCLUSIVE_TOOLS.contains(&resolved.name.as_str());
        if !exclusive && matches!(class, ToolClass::Read) {
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

/// Settles hook-rewritten arguments that fail re-resolution or escape a
/// read-only classification; `None` lets the call run.
///
/// Re-resolution reuses the streamed-argument checks, so a rewrite meets the
/// same gates. The caller keeps hook, authorization, report, elapsed-time,
/// and error ordering: this runs after `run_hooks` and authorization, before
/// execution, with no report or clock side effects.
fn hook_rewrite_outcome(
    ctx: &DispatchCtx,
    ready: &ReadyCall,
    args: &RawJson,
) -> Option<SettledOutcome> {
    if args.as_str() == ready.args.as_str() {
        return None;
    }
    let (promoted, result) = resolve_named(
        &ctx.generation,
        &ctx.tools,
        &ready.name,
        args,
        ctx.mode,
        &ctx.workspace,
    );
    if result.is_err() {
        return Some(failed_outcome(&ResolvedCall {
            call: ready.call.clone(),
            name: ready.name.clone(),
            promoted,
            result,
        }));
    }
    if let Ok(new_class) = &result
        && matches!(ready.class, ToolClass::Read)
        && !matches!(new_class, ToolClass::Read)
    {
        return Some(SettledOutcome::Err {
            text: format!(
                "hook rewrote arguments for {} changing its classification from read to {new_class:?}; parallel read calls cannot change classification. Update the hook to keep the rewrite read-only or make the mutating call separately.",
                ready.name.as_str()
            )
            .into(),
        });
    }
    None
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
    if let Some(outcome) = hook_rewrite_outcome(ctx, ready, &args) {
        return outcome;
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
    .with_optional_extension(owner)
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
        // The prefix match authenticates scoped grants; the git checks below
        // apply to every approval shape, including empty-prefix one-shot and
        // allow-all grants whose roots still bind. Only the request overrides
        // are vetted here: the host snapshot is operator-controlled ambient
        // authority, and denying on it would turn operator environments into
        // per-call grant failures. Snapshot-borne `GIT_*` belongs to a
        // capture-time scrub instead.
        let prefix_ok = approved.prefix().is_empty() || grant_covers(&approved, argv, &opts.cwd);
        if !prefix_ok || !git_spawn_in_roots(argv, &opts.env, &opts.cwd, approved.roots()) {
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
/// root. Git repository selectors (`-C`, `--git-dir`, `--work-tree`,
/// `--namespace`, `--bare`, `--exec-path`, `--super-prefix`, and `-c` /
/// `--config` / `--config-env` assignments) are checked against the same
/// roots, resolving relative operands from the effective directory.
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

/// Reports whether an argv program name invokes git.
///
/// The stem match is ASCII case-insensitive because Windows executes
/// `GIT.EXE` for `git`; an undecodable stem matches nothing and skips the
/// git checks like any other foreign program.
fn is_git_program(program: &std::ffi::OsStr) -> bool {
    Path::new(program)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.eq_ignore_ascii_case("git"))
}

/// Checks every git repository selector in the argument list.
///
/// Only the global portion is scanned: git applies wrapper selectors solely
/// before the subcommand, so the first non-dash token ends the scan and
/// later tokens are the subcommand's own flags (e.g. `switch -c`). Git
/// applies relative `-C` paths successively, so each one becomes
/// the effective directory for later path operands. `--git-dir`,
/// `--work-tree`, `--super-prefix`, and `--exec-path=<path>` are confined to
/// the roots from that final directory; `--namespace` (any form), bare
/// `--bare`, and `-c` / `--config` assignments to `core.bare` (unless
/// explicitly false), `core.hooksPath` outside the roots, or `core.worktree`
/// outside the roots fail closed, as do `--config-env` assignments and
/// config includes (their values arrive elsewhere, invisible here). Other
/// `-c` keys (e.g. `receive.denyCurrentBranch`) carry no repository redirect
/// and are allowed, as are the pager, `[--no-]-replace-objects`, and
/// path-printing flags. `--` ends option parsing. Invalid or missing
/// operands fail closed.
fn git_paths_in_roots(argv: &[std::ffi::OsString], cwd: &Path, roots: &[PathBuf]) -> bool {
    let Some(program) = argv.first() else {
        return false;
    };
    if !is_git_program(program) {
        return true;
    }
    let Some(base) = git_c_base(argv, cwd, roots) else {
        return false;
    };
    git_selectors_in_roots(argv, &base, roots)
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GitLongSelector {
    GitDir,
    WorkTree,
    SuperPrefix,
    ExecPath,
    Namespace,
    Config,
    ConfigEnv,
    Bare,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GitLongOption<'a> {
    selector: GitLongSelector,
    value: Option<&'a str>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GitOperand<'a> {
    Separate,
    Inline(&'a str),
}

/// Parses a global Git selector, including unique long-option prefixes.
///
/// Git's option parser accepts abbreviations where they are unambiguous.
/// Treating those spellings as ordinary flags would let a path-bearing
/// selector skip the root check.
fn git_long_selector(text: &str) -> Option<GitLongOption<'_>> {
    let text = text.strip_prefix("--")?;
    let (name, value) = text
        .split_once('=')
        .map_or((text, None), |(name, value)| (name, Some(value)));
    let exact = match name {
        "git-dir" => Some(GitLongSelector::GitDir),
        "work-tree" => Some(GitLongSelector::WorkTree),
        "super-prefix" => Some(GitLongSelector::SuperPrefix),
        "exec-path" => Some(GitLongSelector::ExecPath),
        "namespace" => Some(GitLongSelector::Namespace),
        "config" => Some(GitLongSelector::Config),
        "config-env" => Some(GitLongSelector::ConfigEnv),
        "bare" => Some(GitLongSelector::Bare),
        _ => None,
    };
    let selector = exact.or_else(|| {
        let mut matched = None;
        for (candidate, selector) in [
            ("git-dir", GitLongSelector::GitDir),
            ("work-tree", GitLongSelector::WorkTree),
            ("super-prefix", GitLongSelector::SuperPrefix),
            ("exec-path", GitLongSelector::ExecPath),
            ("namespace", GitLongSelector::Namespace),
            ("config", GitLongSelector::Config),
            ("config-env", GitLongSelector::ConfigEnv),
            ("bare", GitLongSelector::Bare),
        ] {
            if candidate.starts_with(name) {
                if matched.is_some() {
                    return None;
                }
                matched = Some(selector);
            }
        }
        matched
    })?;
    Some(GitLongOption { selector, value })
}

fn git_selector_takes_operand(selector: GitLongSelector) -> bool {
    matches!(
        selector,
        GitLongSelector::GitDir
            | GitLongSelector::WorkTree
            | GitLongSelector::SuperPrefix
            | GitLongSelector::Config
            | GitLongSelector::ConfigEnv
            | GitLongSelector::Namespace
    )
}

fn git_subcommand_in_roots(
    argv: &[std::ffi::OsString],
    command_index: usize,
    base: &Path,
    roots: &[PathBuf],
) -> bool {
    let Some(command) = argv.get(command_index).and_then(|arg| arg.to_str()) else {
        return false;
    };
    let rest = argv.get(command_index + 1..).unwrap_or_default();
    match command {
        "config" => git_config_subcommand_in_roots(rest, base, roots),
        "clone" => git_clone_subcommand_in_roots(rest, base, roots),
        "init" => git_init_subcommand_in_roots(rest, base, roots),
        "worktree" => git_worktree_subcommand_in_roots(rest, base, roots),
        "archive" => git_archive_subcommand_in_roots(rest, base, roots),
        "bundle" => git_bundle_subcommand_in_roots(rest, base, roots),
        "submodule" => git_submodule_subcommand_in_roots(rest, base, roots),
        _ => git_generic_subcommand_in_roots(rest, base, roots),
    }
}

/// Checks `git config`'s file option, which belongs to the subcommand rather
/// than to Git's global option prefix.
fn git_config_subcommand_in_roots(
    args: &[std::ffi::OsString],
    base: &Path,
    roots: &[PathBuf],
) -> bool {
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == std::ffi::OsStr::new("--") {
            return git_config_positionals_in_roots(&args[index + 1..], base, roots);
        }
        if arg == std::ffi::OsStr::new("-f") {
            let Some(path) = args.get(index + 1) else {
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
        if git_config_escapes_roots(text) {
            return false;
        }
        if let Some(path) = text.strip_prefix("-f")
            && !path.is_empty()
        {
            let path = path.strip_prefix('=').unwrap_or(path);
            if path.is_empty()
                || checked_git_path(base, std::ffi::OsStr::new(path), roots).is_none()
            {
                return false;
            }
            index += 1;
            continue;
        }
        if let Some(operand) = git_config_file_option(text) {
            let path = match operand {
                GitOperand::Inline(path) => Some(path),
                GitOperand::Separate => args.get(index + 1).and_then(|arg| arg.to_str()),
            };
            let Some(path) = path else {
                return false;
            };
            if checked_git_path(base, std::ffi::OsStr::new(path), roots).is_none() {
                return false;
            }
            index += match operand {
                GitOperand::Inline(_) => 1,
                GitOperand::Separate => 2,
            };
            continue;
        }
        if text.starts_with('-') {
            index += 1;
            continue;
        }
        if git_config_subcommand_key_is_sensitive(text) {
            let Some(value) = args.get(index + 1).and_then(|arg| arg.to_str()) else {
                index += 1;
                continue;
            };
            let mut kv = String::with_capacity(text.len() + value.len() + 1);
            kv.push_str(text);
            kv.push('=');
            kv.push_str(value);
            if !git_config_in_roots(std::ffi::OsStr::new(&kv), base, &[], roots) {
                return false;
            }
            index += 2;
            continue;
        }
        index += 1;
    }
    true
}

/// Reports whether a `git config` positional key selects a path or mode that
/// must stay inside the grant roots.
///
/// Only these keys carry repository redirects (`core.worktree`,
/// `core.hooksPath`, `core.bare`, and config includes). Every other key
/// (e.g. `user.name`) carries an opaque string value and needs no confinement,
/// so absolute-looking values for those keys stay allowed.
fn git_config_subcommand_key_is_sensitive(key: &str) -> bool {
    key.eq_ignore_ascii_case("core.worktree")
        || key.eq_ignore_ascii_case("core.hookspath")
        || key.eq_ignore_ascii_case("core.bare")
        || is_include_path_key(key)
}

/// Checks `git config` positionals after `--`, where leading dashes no longer
/// mark options.
///
/// The same sensitive-key rule applies: only path-bearing keys confine their
/// following value, so `user.name` values that happen to look like absolute
/// paths stay allowed.
fn git_config_positionals_in_roots(
    args: &[std::ffi::OsString],
    base: &Path,
    roots: &[PathBuf],
) -> bool {
    let mut index = 0;
    while index < args.len() {
        let Some(text) = args[index].to_str() else {
            return false;
        };
        if git_config_subcommand_key_is_sensitive(text) {
            let Some(value) = args.get(index + 1).and_then(|arg| arg.to_str()) else {
                index += 1;
                continue;
            };
            let mut kv = String::with_capacity(text.len() + value.len() + 1);
            kv.push_str(text);
            kv.push('=');
            kv.push_str(value);
            if !git_config_in_roots(std::ffi::OsStr::new(&kv), base, &[], roots) {
                return false;
            }
            index += 2;
            continue;
        }
        index += 1;
    }
    true
}

fn git_config_file_option(text: &str) -> Option<GitOperand<'_>> {
    let text = text.strip_prefix("--")?;
    let (name, value) = text
        .split_once('=')
        .map_or((text, None), |(name, value)| (name, Some(value)));
    let is_file = name == "file" || (name.len() >= 3 && "file".starts_with(name));
    is_file.then(|| value.map_or(GitOperand::Separate, GitOperand::Inline))
}
fn git_config_escapes_roots(text: &str) -> bool {
    let Some(text) = text.strip_prefix("--") else {
        return false;
    };
    let name = text.split_once('=').map_or(text, |(name, _)| name);
    if name.starts_with("no-") {
        return false;
    }
    (name == "global" || (name.len() >= 2 && "global".starts_with(name)))
        || (name == "system" || (name.len() >= 2 && "system".starts_with(name)))
        || (name == "includes" || (name.len() >= 3 && "includes".starts_with(name)))
}

fn git_clone_config_option(text: &str) -> Option<GitOperand<'_>> {
    let text = text.strip_prefix("--")?;
    let (name, value) = text
        .split_once('=')
        .map_or((text, None), |(name, value)| (name, Some(value)));
    let is_config = name == "config" || (name.len() >= 2 && "config".starts_with(name));
    is_config.then(|| value.map_or(GitOperand::Separate, GitOperand::Inline))
}

/// `git clone` accepts `-c`/`--config` after the subcommand. Check those
/// assignments as well as the global forms before the command.
fn git_clone_subcommand_in_roots(
    args: &[std::ffi::OsString],
    base: &Path,
    roots: &[PathBuf],
) -> bool {
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == std::ffi::OsStr::new("--") {
            return git_subcommand_positionals_in_roots(&args[index + 1..], base, roots);
        }
        if arg == std::ffi::OsStr::new("-c") {
            let Some(kv) = args.get(index + 1) else {
                return false;
            };
            if !git_config_in_roots(kv, base, &[], roots) {
                return false;
            }
            index += 2;
            continue;
        }
        let Some(text) = arg.to_str() else {
            return false;
        };
        if let Some(kv) = text.strip_prefix("-c")
            && !kv.is_empty()
        {
            let kv = kv.strip_prefix('=').unwrap_or(kv);
            if !git_config_in_roots(std::ffi::OsStr::new(kv), base, &[], roots) {
                return false;
            }
            index += 1;
            continue;
        }
        if let Some(operand) = git_clone_config_option(text) {
            let kv = match operand {
                GitOperand::Inline(kv) => Some(kv),
                GitOperand::Separate => args.get(index + 1).and_then(|arg| arg.to_str()),
            };
            let Some(kv) = kv else {
                return false;
            };
            if !git_config_in_roots(std::ffi::OsStr::new(kv), base, &[], roots) {
                return false;
            }
            index += match operand {
                GitOperand::Inline(_) => 1,
                GitOperand::Separate => 2,
            };
            continue;
        }
        if git_long_selector(text)
            .is_some_and(|option| option.selector == GitLongSelector::ConfigEnv)
        {
            return false;
        }
        match git_generic_long_in_roots(text, args, index, base, roots) {
            GitSubcommandPath::NotPath => {}
            GitSubcommandPath::Escapes => return false,
            GitSubcommandPath::Confined(next) => {
                index = next;
                continue;
            }
        }
        match git_generic_short_in_roots(text, args, index, base, roots) {
            GitSubcommandPath::NotPath => {}
            GitSubcommandPath::Escapes => return false,
            GitSubcommandPath::Confined(next) => {
                index = next;
                continue;
            }
        }
        if text.starts_with('-') {
            index += 1;
            continue;
        }
        if checked_git_path(base, std::ffi::OsStr::new(text), roots).is_none() {
            return false;
        }
        index += 1;
    }
    true
}

/// Matches one path-bearing subcommand long option, including abbreviations.
///
/// Any prefix of length two or more counts, mirroring git's unique-prefix rule
/// from the caller's side. Single-letter prefixes stay unmatched here; git
/// itself rejects them as ambiguous, so allowing them changes nothing.
/// `no-` negations carry no operand and never match.
fn git_subcommand_path_option<'a>(text: &'a str, candidate: &str) -> Option<GitOperand<'a>> {
    let body = text.strip_prefix("--")?;
    let (name, value) = body
        .split_once('=')
        .map_or((body, None), |(name, value)| (name, Some(value)));
    if name.starts_with("no-") || name.is_empty() {
        return None;
    }
    let matches = name == candidate || (name.len() >= 2 && candidate.starts_with(name));
    matches.then(|| value.map_or(GitOperand::Separate, GitOperand::Inline))
}

/// Confines one subcommand path operand to the grant roots.
///
/// Empty operands fail closed. Returns the next argument index on success.
fn git_subcommand_operand_in_roots(
    option: GitOperand<'_>,
    args: &[std::ffi::OsString],
    index: usize,
    base: &Path,
    roots: &[PathBuf],
) -> Option<usize> {
    let path = match option {
        GitOperand::Inline(path) => Some(path),
        GitOperand::Separate => args.get(index + 1).and_then(|arg| arg.to_str()),
    };
    let path = path?;
    if path.is_empty() {
        return None;
    }
    checked_git_path(base, std::ffi::OsStr::new(path), roots)?;
    Some(index + usize::from(matches!(option, GitOperand::Separate)) + 1)
}

/// Verdict for one subcommand path-selector probe.
///
/// A single enum keeps `clippy::option_option` (pedantic, denied in CI) out of
/// the gate: `NotPath` means the token is not a path selector, `Escapes`
/// means it selects outside the grant roots, and `Confined` carries the next
/// argument index when the operand stays inside.
enum GitSubcommandPath {
    NotPath,
    Escapes,
    Confined(usize),
}

/// Checks one generic long path selector, if the token is one.
///
/// The candidate set covers every subcommand output selector observed in the
/// installed git: `init`/`clone` `--separate-git-dir` and `--template`,
/// `clone`/`submodule` `--reference` and `--reference-if-able`, `archive`
/// `--output`, `archive` `--remote` and `--add-file`, and `clone`
/// `--upload-pack`. That list drives operand indexing for separate operands;
/// every attached `=value` is confined by the fall-through below regardless of
/// the name, `--exec` included.
fn git_generic_long_in_roots(
    text: &str,
    args: &[std::ffi::OsString],
    index: usize,
    base: &Path,
    roots: &[PathBuf],
) -> GitSubcommandPath {
    for candidate in [
        "separate-git-dir",
        "template",
        "reference",
        "reference-if-able",
        "output",
        "remote",
        "add-file",
        "upload-pack",
    ] {
        let Some(option) = git_subcommand_path_option(text, candidate) else {
            continue;
        };
        let Some(next) = git_subcommand_operand_in_roots(option, args, index, base, roots) else {
            return GitSubcommandPath::Escapes;
        };
        return GitSubcommandPath::Confined(next);
    }
    // An unlisted long option with an attached `=value` may still select a
    // filesystem location Git reads or writes (`format-patch
    // --output-directory=<dir>` writes there). The value is confined exactly
    // like an operand: relative values resolve inside, absolute values must
    // stay in the grant roots. An empty value selects nothing, and Git accepts
    // it for optional-argument options (`log --pretty=`), so it stays inert;
    // known path options above still fail closed on empty operands. A bare
    // `--name` without `=` carries no value and stays inert.
    if let Some((name, value)) = text
        .strip_prefix("--")
        .and_then(|body| body.split_once('='))
        && !name.starts_with("no-")
        && !name.is_empty()
        && !value.is_empty()
    {
        if checked_git_path(base, std::ffi::OsStr::new(value), roots).is_none() {
            return GitSubcommandPath::Escapes;
        }
        return GitSubcommandPath::Confined(index + 1);
    }
    GitSubcommandPath::NotPath
}

/// Checks one short output selector (`-o`/`-u`), if the token is one.
///
/// `-o` writes `archive --output`; `-u` runs `clone --upload-pack`. Letters
/// glued ahead of a trailing `o`/`u` attach the operand the same way
/// (`format-patch -qo<dir>`), so the first `o`/`u` after the dash letters is
/// treated as the selector and its remainder confined. A leading value-taking
/// short (`-S`, `-G`, `-L`) owns its whole remainder as a string argument, so
/// its token stays inert and the next argument is the ordinary non-dash
/// position the scanner already checks; an empty remainder likewise carries no
/// operand. Other shorts (`-b`
/// branch, `-o` origin for `clone`, `-c` count for `grep`) carry branch names
/// or counts, which as relative paths stay inside and pass, so treating an
/// absolute-looking operand as a path fails closed safely.
fn git_generic_short_in_roots(
    text: &str,
    args: &[std::ffi::OsString],
    index: usize,
    base: &Path,
    roots: &[PathBuf],
) -> GitSubcommandPath {
    for short in ["-o", "-u"] {
        if text == short {
            let Some(path) = args.get(index + 1) else {
                return GitSubcommandPath::Escapes;
            };
            let Some(path) = path.to_str() else {
                return GitSubcommandPath::Escapes;
            };
            if path.is_empty() {
                return GitSubcommandPath::Escapes;
            }
            if checked_git_path(base, std::ffi::OsStr::new(path), roots).is_none() {
                return GitSubcommandPath::Escapes;
            }
            return GitSubcommandPath::Confined(index + 2);
        }
        let Some(rest) = text.strip_prefix(short) else {
            continue;
        };
        if rest.is_empty() {
            continue;
        }
        let rest = rest.strip_prefix('=').unwrap_or(rest);
        if rest.is_empty() {
            return GitSubcommandPath::Escapes;
        }
        if checked_git_path(base, std::ffi::OsStr::new(rest), roots).is_none() {
            return GitSubcommandPath::Escapes;
        }
        return GitSubcommandPath::Confined(index + 1);
    }
    // Cluster tail: letters glued ahead of `o`/`u` attach the operand, so
    // `-qo<value>` confines `<value>` like `-o <value>`. A leading value-taking
    // short (`-S`, `-G`, `-L`) parses its whole remainder as its string
    // argument, so any `o`/`u` inside belongs to that argument, not to an
    // option cluster: such a token stays inert and its next argument is the
    // ordinary non-dash position the scanner already checks. Without such a
    // letter, the first `o`/`u` is the selector and its remainder is confined;
    // an empty remainder carries no operand and also stays inert.
    if let Some(letters) = text.strip_prefix('-')
        && let Some(position) = letters.find(['o', 'u'])
        && !letters[..position].contains(['S', 'G', 'L'])
    {
        let rest = &letters[position + 1..];
        if !rest.is_empty() {
            let rest = rest.strip_prefix('=').unwrap_or(rest);
            if rest.is_empty() {
                return GitSubcommandPath::Escapes;
            }
            if checked_git_path(base, std::ffi::OsStr::new(rest), roots).is_none() {
                return GitSubcommandPath::Escapes;
            }
            return GitSubcommandPath::Confined(index + 1);
        }
    }
    GitSubcommandPath::NotPath
}

/// Confines bare positionals after `--` to the grant roots.
///
/// Branch names, revisions, and URLs read as relative paths and pass; absolute
/// outside paths fail. Undecodable tokens fail closed.
fn git_subcommand_positionals_in_roots(
    args: &[std::ffi::OsString],
    base: &Path,
    roots: &[PathBuf],
) -> bool {
    for arg in args {
        let Some(text) = arg.to_str() else {
            return false;
        };
        if checked_git_path(base, std::ffi::OsStr::new(text), roots).is_none() {
            return false;
        }
    }
    true
}

/// Confines every subcommand filesystem token to the grant roots.
///
/// Long output selectors confine their operands; `-o`/`-u` shorts do the same.
/// Every other dash-led token is inert unless it carries an attached value:
/// an unlisted `--name=value` confines the value, and a short cluster's first
/// `o`/`u` confines its remainder, because either can select a filesystem
/// location Git reads or writes. Every non-dash token is either
/// a filesystem position (target dirs, worktree paths, bundle files) or a
/// branch, revision, or URL, which as a relative path stays inside and passes.
/// `--` ends option parsing; the remainder are all positions. This one gate
/// absorbs `init --separate-git-dir`, `clone` target dirs, `worktree` paths,
/// `archive` outputs, `bundle` files, and `submodule` paths without one
/// special case per subcommand.
fn git_generic_subcommand_in_roots(
    args: &[std::ffi::OsString],
    base: &Path,
    roots: &[PathBuf],
) -> bool {
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == std::ffi::OsStr::new("--") {
            return git_subcommand_positionals_in_roots(&args[index + 1..], base, roots);
        }
        let Some(text) = arg.to_str() else {
            return false;
        };
        match git_generic_long_in_roots(text, args, index, base, roots) {
            GitSubcommandPath::NotPath => {}
            GitSubcommandPath::Escapes => return false,
            GitSubcommandPath::Confined(next) => {
                index = next;
                continue;
            }
        }
        match git_generic_short_in_roots(text, args, index, base, roots) {
            GitSubcommandPath::NotPath => {}
            GitSubcommandPath::Escapes => return false,
            GitSubcommandPath::Confined(next) => {
                index = next;
                continue;
            }
        }
        if text.starts_with('-') {
            index += 1;
            continue;
        }
        if checked_git_path(base, std::ffi::OsStr::new(text), roots).is_none() {
            return false;
        }
        index += 1;
    }
    true
}

/// Confines `init` output paths to the grant roots.
///
/// `init --separate-git-dir` and `--template` plus the optional directory
/// positional all write outside when absolute outside. The shared gate above
/// already covers every case; this wrapper names the subcommand for future
/// divergence.
fn git_init_subcommand_in_roots(
    args: &[std::ffi::OsString],
    base: &Path,
    roots: &[PathBuf],
) -> bool {
    git_generic_subcommand_in_roots(args, base, roots)
}

/// Confines `worktree` paths to the grant roots.
///
/// `add <path>`, `move <old> <new>`, `remove`/`lock`/`unlock`/`repair` paths,
/// and `list`/`prune` flags all flow through the shared gate: positions stay
/// inside, branch names pass as relative paths.
fn git_worktree_subcommand_in_roots(
    args: &[std::ffi::OsString],
    base: &Path,
    roots: &[PathBuf],
) -> bool {
    git_generic_subcommand_in_roots(args, base, roots)
}

/// Confines `archive` outputs to the grant roots.
///
/// `-o`/`--output` writes the archive file; `--remote` and `--add-file` read
/// outside when absolute outside and fail closed for the same reason as
/// `--git-dir`. Tree-ish and pathspec positions pass as relative paths.
fn git_archive_subcommand_in_roots(
    args: &[std::ffi::OsString],
    base: &Path,
    roots: &[PathBuf],
) -> bool {
    git_generic_subcommand_in_roots(args, base, roots)
}

/// Confines `bundle` files to the grant roots.
///
/// `create`/`verify`/`list-heads`/`unbundle <file>` writes or reads the bundle
/// file; revisions after it pass as relative paths.
fn git_bundle_subcommand_in_roots(
    args: &[std::ffi::OsString],
    base: &Path,
    roots: &[PathBuf],
) -> bool {
    git_generic_subcommand_in_roots(args, base, roots)
}

/// Confines `submodule` paths to the grant roots.
///
/// `--reference` plus `add <repo> [<path>]` and per-verb `[<path>...]` all
/// flow through the shared gate; repository URLs pass as relative paths.
fn git_submodule_subcommand_in_roots(
    args: &[std::ffi::OsString],
    base: &Path,
    roots: &[PathBuf],
) -> bool {
    git_generic_subcommand_in_roots(args, base, roots)
}
fn git_c_base(argv: &[std::ffi::OsString], cwd: &Path, roots: &[PathBuf]) -> Option<PathBuf> {
    let mut base = cwd.to_path_buf();
    let mut index = 1;
    while index < argv.len() {
        let arg = &argv[index];
        if arg == std::ffi::OsStr::new("--") {
            break;
        }
        if arg == std::ffi::OsStr::new("-C") {
            let path = argv.get(index + 1)?;
            base = checked_git_path(&base, path, roots)?;
            index += 2;
            continue;
        }
        if arg == std::ffi::OsStr::new("-c") {
            argv.get(index + 1)?;
            index += 2;
            continue;
        }
        let text = arg.to_str()?;
        if let Some(option) = git_long_selector(text) {
            if option.value.is_none() && git_selector_takes_operand(option.selector) {
                argv.get(index + 1)?;
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if !text.starts_with('-') {
            break;
        }
        if let Some(path) = text.strip_prefix("-C") {
            base = checked_git_path(&base, std::ffi::OsStr::new(path), roots)?;
        }
        index += 1;
    }
    Some(base)
}

fn git_sensitive_long_prefix(text: &str) -> bool {
    let Some(text) = text.strip_prefix("--") else {
        return false;
    };
    let name = text.split_once('=').map_or(text, |(name, _)| name);
    [
        "git-dir",
        "work-tree",
        "super-prefix",
        "exec-path",
        "namespace",
        "config",
        "config-env",
        "bare",
    ]
    .into_iter()
    .any(|candidate| candidate.starts_with(name))
}

/// Applies one global long selector, returning the next argument index.
///
/// `None` denies the spawn: a path leaves the roots, a redirect-capable key
/// cannot be verified, or the selector carries no confinable path.
fn git_global_long_in_roots(
    option: GitLongOption<'_>,
    argv: &[std::ffi::OsString],
    index: usize,
    base: &Path,
    git_dirs: &[PathBuf],
    roots: &[PathBuf],
) -> Option<usize> {
    match option.selector {
        GitLongSelector::GitDir
        | GitLongSelector::WorkTree
        | GitLongSelector::SuperPrefix
        | GitLongSelector::ExecPath => {
            if option.selector == GitLongSelector::ExecPath && option.value.is_none() {
                return Some(index + 1);
            }
            let path = option
                .value
                .or_else(|| argv.get(index + 1).and_then(|arg| arg.to_str()))?;
            checked_git_path(base, std::ffi::OsStr::new(path), roots)?;
            Some(index + usize::from(option.value.is_none()) + 1)
        }
        GitLongSelector::Config => {
            let kv = option
                .value
                .or_else(|| argv.get(index + 1).and_then(|arg| arg.to_str()))?;
            git_config_in_roots(std::ffi::OsStr::new(kv), base, git_dirs, roots)
                .then(|| index + usize::from(option.value.is_none()) + 1)
        }
        GitLongSelector::ConfigEnv => {
            let kv = option
                .value
                .or_else(|| argv.get(index + 1).and_then(|arg| arg.to_str()))?;
            git_config_env_in_roots(std::ffi::OsStr::new(kv))
                .then(|| index + usize::from(option.value.is_none()) + 1)
        }
        GitLongSelector::Namespace | GitLongSelector::Bare => None,
    }
}

fn git_selectors_in_roots(argv: &[std::ffi::OsString], base: &Path, roots: &[PathBuf]) -> bool {
    // Relative `core.worktree` values resolve against the git dir rather than
    // the working directory, so gather the explicit `--git-dir` anchors here;
    // the `-C` base below covers the discovered-repo case.
    let git_dirs = git_dir_operands(argv, base);
    let mut index = 1;
    while index < argv.len() {
        let arg = &argv[index];
        if arg == std::ffi::OsStr::new("--") {
            return git_subcommand_in_roots(argv, index + 1, base, roots);
        }
        if arg == std::ffi::OsStr::new("-C") {
            if argv.get(index + 1).is_none() {
                return false;
            }
            index += 2;
            continue;
        }
        if arg == std::ffi::OsStr::new("-c") {
            let Some(kv) = argv.get(index + 1) else {
                return false;
            };
            if !git_config_in_roots(kv, base, &git_dirs, roots) {
                return false;
            }
            index += 2;
            continue;
        }
        let Some(text) = arg.to_str() else {
            return false;
        };
        if let Some(path) = text.strip_prefix("-C")
            && !path.is_empty()
        {
            index += 1;
            continue;
        }
        if text == "-C" {
            return false;
        }
        if let Some(kv) = text.strip_prefix("-c")
            && !kv.is_empty()
        {
            if !git_config_in_roots(
                std::ffi::OsStr::new(kv.strip_prefix('=').unwrap_or(kv)),
                base,
                &git_dirs,
                roots,
            ) {
                return false;
            }
            index += 1;
            continue;
        }
        if text == "-c" {
            return false;
        }
        if let Some(option) = git_long_selector(text) {
            let Some(next) = git_global_long_in_roots(option, argv, index, base, &git_dirs, roots)
            else {
                return false;
            };
            index = next;
            continue;
        }
        if !text.starts_with('-') {
            return git_subcommand_in_roots(argv, index, base, roots);
        }
        if text.starts_with("--") && git_sensitive_long_prefix(text) {
            return false;
        }
        index += 1;
    }
    true
}

/// Collects the explicit `--git-dir` operands as resolved paths.
///
/// These anchor relative `core.worktree` values. Collection itself never
/// fails: unresolvable operands are skipped here and denied by the main scan,
/// whose verdict conjoins with every anchor check below.
fn git_dir_operands(argv: &[std::ffi::OsString], base: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut index = 1;
    while index < argv.len() {
        let arg = &argv[index];
        if arg == std::ffi::OsStr::new("--") {
            break;
        }
        if arg == std::ffi::OsStr::new("-C") || arg == std::ffi::OsStr::new("-c") {
            index += 2;
            continue;
        }
        let Some(text) = arg.to_str() else {
            index += 1;
            continue;
        };
        if !text.starts_with('-') {
            break;
        }
        if let Some(option) = git_long_selector(text) {
            if option.selector == GitLongSelector::GitDir {
                if let Some(path) = option.value {
                    dirs.extend(resolve_git_path(base, std::ffi::OsStr::new(path)));
                } else if let Some(path) = argv.get(index + 1) {
                    dirs.extend(resolve_git_path(base, path));
                }
            }
            index += if option.value.is_some() || !git_selector_takes_operand(option.selector) {
                1
            } else {
                2
            };
            continue;
        }
        if text.strip_prefix("-C").is_some() || text.strip_prefix("-c").is_some() {
            index += 1;
            continue;
        }
        index += 1;
    }
    dirs
}

/// Checks one `-c` / `--config` assignment against the roots.
///
/// `core.worktree` carries a work-tree path: absolute values confine
/// directly, relative ones resolve against the git dir, so they require
/// explicit `--git-dir` anchors and must stay inside the roots from the `-C`
/// base and from every anchor. Without an explicit `--git-dir` the anchor is
/// whichever repository git discovers upward, which argv cannot name: fail
/// closed. `core.hooksPath` selects executed hook programs: absolute values
/// confine to the roots (in-roots hooks are baseline trust, like the repo's
/// own hooks), relative values carry no verifiable anchor and fail closed.
/// `include.path` and `includeIf.*.path` pull in whole config files whose
/// content cannot be verified here, so they fail closed. `core.bare` flips
/// repository semantics without carrying a path, so only an explicit false
/// passes. Any other key carries no repository redirect and is allowed.
fn git_config_in_roots(
    kv: &std::ffi::OsStr,
    base: &Path,
    git_dirs: &[PathBuf],
    roots: &[PathBuf],
) -> bool {
    let Some(kv) = kv.to_str() else {
        return false;
    };
    let Some((key, value)) = kv.split_once('=') else {
        return false;
    };
    if key.eq_ignore_ascii_case("core.worktree") {
        if value.is_empty() {
            return false;
        }
        let value = std::ffi::OsStr::new(value);
        if Path::new(value).is_absolute() {
            return checked_git_path(base, value, roots).is_some();
        }
        if git_dirs.is_empty() {
            return false;
        }
        return checked_git_path(base, value, roots).is_some()
            && git_dirs
                .iter()
                .all(|dir| checked_git_path(dir, value, roots).is_some());
    }
    if key.eq_ignore_ascii_case("core.hookspath") {
        if value.is_empty() {
            return false;
        }
        let value = std::ffi::OsStr::new(value);
        if !Path::new(value).is_absolute() {
            return false;
        }
        return checked_git_path(base, value, roots).is_some();
    }
    if key.eq_ignore_ascii_case("core.bare") {
        return is_falsy_git_bool(value);
    }
    if is_include_path_key(key) {
        return false;
    }
    true
}

/// Reports whether a `-c` key pulls in another config file.
///
/// The pulled file's content cannot be verified from argv (it can itself set
/// `core.worktree` or `core.hooksPath`), so these keys fail closed. Matching
/// is ASCII case-insensitive on the section and key; the `includeIf`
/// condition keeps its case but needs no parsing here.
fn is_include_path_key(key: &str) -> bool {
    const PREFIX_LEN: usize = "includeif.".len();
    const SUFFIX_LEN: usize = ".path".len();
    if key.eq_ignore_ascii_case("include.path") {
        return true;
    }
    if key.len() <= PREFIX_LEN + SUFFIX_LEN {
        return false;
    }
    key.get(..PREFIX_LEN)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("includeif."))
        && key
            .get(key.len() - SUFFIX_LEN..)
            .is_some_and(|suffix| suffix.eq_ignore_ascii_case(".path"))
}

/// Checks one `--config-env` assignment against the roots.
///
/// The value arrives via the environment, so redirect-capable keys
/// (`core.worktree`, `core.hooksPath`, `core.bare`, and config includes)
/// cannot be verified from argv and fail closed; every other key is allowed.
fn git_config_env_in_roots(kv: &std::ffi::OsStr) -> bool {
    let Some(kv) = kv.to_str() else {
        return false;
    };
    let Some((name, _envvar)) = kv.split_once('=') else {
        return false;
    };
    !(name.eq_ignore_ascii_case("core.worktree")
        || name.eq_ignore_ascii_case("core.hookspath")
        || name.eq_ignore_ascii_case("core.bare")
        || is_include_path_key(name))
}

/// Reports whether a git boolean spelling disables an option.
fn is_falsy_git_bool(value: &str) -> bool {
    value.is_empty()
        || value.eq_ignore_ascii_case("false")
        || value.eq_ignore_ascii_case("no")
        || value.eq_ignore_ascii_case("off")
        || value.eq_ignore_ascii_case("0")
}

/// Checks git's repository-selecting environment overrides.
///
/// `GIT_DIR`, `GIT_WORK_TREE`, `GIT_COMMON_DIR`, `GIT_EXEC_PATH`,
/// `GIT_OBJECT_DIRECTORY`, and `GIT_INDEX_FILE` carry paths: empty values
/// behave as unset, anything else must resolve inside the roots from the
/// spawn cwd (environment work-tree paths anchor on the startup directory,
/// unlike `--work-tree`). `GIT_CONFIG` is the path used by `git config` and
/// is confined the same way. `GIT_NAMESPACE` carries no path
/// and cannot be confined, so any non-empty value fails closed. The same
/// holds for `GIT_CONFIG_PARAMETERS` (git's internal `-c` relay channel,
/// never set legitimately on a top-level spawn yet honored when present, as
/// verified against the installed git) and for `GIT_CONFIG_GLOBAL` /
/// `GIT_CONFIG_SYSTEM` (alternate config files, loaded before argv config
/// and able to carry the same redirects). The `GIT_CONFIG_COUNT` /
/// `GIT_CONFIG_KEY_n` / `GIT_CONFIG_VALUE_n` scheme applies env-borne `-c`
/// assignments the same way and is classified per pair below. Other programs
/// ignore these variables and skip the check. Only request overrides are
/// vetted here; the host snapshot is scrubbed of the same names at capture.
pub(crate) fn git_env_in_roots(
    argv: &[std::ffi::OsString],
    env: &[(std::ffi::OsString, std::ffi::OsString)],
    cwd: &Path,
    roots: &[PathBuf],
) -> bool {
    let Some(program) = argv.first() else {
        return false;
    };
    if !is_git_program(program) {
        return true;
    }
    for (key, value) in env {
        let Some(name) = key.to_str() else {
            continue;
        };
        // Environment names resolve case-insensitively on Windows, so
        // `git_dir` reaches git as `GIT_DIR` there: match ASCII
        // case-insensitively everywhere.
        let denied = name.eq_ignore_ascii_case("GIT_NAMESPACE")
            || name.eq_ignore_ascii_case("GIT_CONFIG_PARAMETERS")
            || name.eq_ignore_ascii_case("GIT_CONFIG_GLOBAL")
            || name.eq_ignore_ascii_case("GIT_CONFIG_SYSTEM")
            || name.eq_ignore_ascii_case("GIT_ALTERNATE_OBJECT_DIRECTORIES");
        // Object store and index locations resolve from the spawn cwd like
        // `GIT_DIR`; the alternate list cannot be confined element-wise, so
        // it denies instead.
        let confined = name.eq_ignore_ascii_case("GIT_DIR")
            || name.eq_ignore_ascii_case("GIT_WORK_TREE")
            || name.eq_ignore_ascii_case("GIT_COMMON_DIR")
            || name.eq_ignore_ascii_case("GIT_EXEC_PATH")
            || name.eq_ignore_ascii_case("GIT_OBJECT_DIRECTORY")
            || name.eq_ignore_ascii_case("GIT_INDEX_FILE")
            || name.eq_ignore_ascii_case("GIT_CONFIG");
        if !denied && !confined {
            continue;
        }
        if value.is_empty() {
            continue;
        }
        if denied || checked_git_path(cwd, value, roots).is_none() {
            return false;
        }
    }
    git_env_config_in_roots(env, cwd, roots)
}

/// Checks env-borne `-c` assignments against the roots.
///
/// Each `GIT_CONFIG_KEY_n` / `GIT_CONFIG_VALUE_n` pair classifies exactly
/// like an argv `-c` assignment, except the git-dir anchor set is empty:
/// relative `core.worktree` and `core.hooksPath` values fail closed, absolute
/// ones confine from the spawn cwd. A missing count behaves as unset; a
/// malformed count or an absurd pair budget fails closed rather than looping
/// unboundedly.
fn git_env_config_in_roots(
    env: &[(std::ffi::OsString, std::ffi::OsString)],
    cwd: &Path,
    roots: &[PathBuf],
) -> bool {
    // Later duplicates win in the spawned child (`Command::envs` overwrites),
    // so validation reads the last match, never the first.
    let lookup = |name: &str| {
        env.iter()
            .rfind(|(key, _)| {
                key.to_str()
                    .is_some_and(|key| key.eq_ignore_ascii_case(name))
            })
            .map(|(_, value)| value)
    };
    let Some(count) = lookup("GIT_CONFIG_COUNT") else {
        return true;
    };
    let Some(count) = count.to_str() else {
        return false;
    };
    if count.is_empty() {
        return true;
    }
    let Ok(count) = count.parse::<usize>() else {
        return false;
    };
    if count > 64 {
        return false;
    }
    for index in 0..count {
        let key_name = format!("GIT_CONFIG_KEY_{index}");
        let value_name = format!("GIT_CONFIG_VALUE_{index}");
        let Some(key) = lookup(&key_name) else {
            continue;
        };
        let mut kv = key.clone();
        kv.push("=");
        if let Some(value) = lookup(&value_name) {
            kv.push(value);
        }
        if !git_config_in_roots(&kv, cwd, &[], roots) {
            return false;
        }
    }
    true
}

/// Checks every git escape hatch on one spawn, regardless of approval shape.
///
/// Empty-prefix approvals (one-shot and allow-all grants) skip the argv
/// prefix match by design, but their roots still bind: an auto-approved
/// `git --git-dir=/outside` or a request `GIT_DIR` outside the roots must
/// fail the same way a scoped grant would. Non-git programs pass through.
pub(crate) fn git_spawn_in_roots(
    argv: &[std::ffi::OsString],
    env: &[(std::ffi::OsString, std::ffi::OsString)],
    cwd: &Path,
    roots: &[PathBuf],
) -> bool {
    git_paths_in_roots(argv, cwd, roots) && git_env_in_roots(argv, env, cwd, roots)
}

fn checked_git_path(base: &Path, path: &std::ffi::OsStr, roots: &[PathBuf]) -> Option<PathBuf> {
    let joined = resolve_git_path(base, path)?;
    let resolved = crate::session::backend::canonicalize_existing_prefix(&joined)?;
    roots
        .iter()
        .any(|root| {
            std::fs::canonicalize(root)
                .is_ok_and(|canonical_root| resolved.starts_with(canonical_root))
        })
        .then_some(resolved)
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

/// The per-call inputs one nested tool call runs with.
pub(crate) struct NestedCall {
    /// The registered tool name: a native op's product tool or an
    /// export's provider-wire name.
    pub name: Name,
    /// The canonical bound arguments.
    pub args: RawJson,
    /// The host-minted call identity.
    pub call: CallId,
    /// The calling identity: the owning plugin for export ops, the
    /// calling invocation's plugin for native ops.
    pub caller: Caller,
    /// The live turn of a scripted caller, when the call runs inside one.
    pub turn: Option<TurnId>,
    /// The cancellation token bounding the call.
    pub cancel: CancellationToken,
    /// The frozen observation cutoff of the call.
    pub cutoff: Option<u64>,
    /// The script context of a scripted caller, when one is active.
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

/// Runs one seeded nested call through the checked tool path.
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
    let Some(services) = backend.services() else {
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
    let policy = nested_policy(&caller, attached);
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
        services,
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
fn nested_policy(caller: &Caller, answerer_attached: bool) -> Policy {
    let mode = if caller.cell_approved() {
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
    #[test]
    fn git_namespace_and_bare_are_denied() {
        let temp = tempfile::tempdir().expect("temporary grant roots");
        let workspace = temp.path().join("agent-workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let approved = Approved::new(
            CallId::new("namespace-grant"),
            None,
            Box::new([std::ffi::OsString::from("git")]),
            Box::new([workspace.clone()]),
            None,
        );

        let baseline: Vec<std::ffi::OsString> = vec!["git".into(), "status".into()];
        assert!(grant_covers(&approved, &baseline, &workspace));

        let mut namespaced_equals = std::ffi::OsString::from("--namespace=");
        namespaced_equals.push("agent");
        for argv in [
            vec!["git".into(), namespaced_equals, "status".into()],
            vec![
                "git".into(),
                "--namespace".into(),
                "agent".into(),
                "status".into(),
            ],
            vec!["git".into(), "--namespace=".into(), "status".into()],
            vec!["git".into(), "--namespace".into()],
            vec!["git".into(), "--bare".into(), "status".into()],
        ] {
            assert!(!grant_covers(&approved, &argv, &workspace));
        }
    }
    #[test]
    fn git_config_worktree_stays_inside_roots() {
        fn config_argv(flag: &str, kv: &str) -> Vec<std::ffi::OsString> {
            vec!["git".into(), flag.into(), kv.into(), "status".into()]
        }
        let temp = tempfile::tempdir().expect("temporary grant roots");
        let outer = temp.path().join("outer-repo");
        let workspace = outer.join("agent-workspace");
        let nested = workspace.join("nested-repo");
        let workspace_git = workspace.join(".git");
        let git_nested = workspace_git.join("nested-repo");
        let outside = outer.join("outside");
        for dir in [&nested, &workspace_git, &git_nested, &outside] {
            std::fs::create_dir_all(dir).expect("grant fixture directory");
        }
        let approved = Approved::new(
            CallId::new("config-grant"),
            None,
            Box::new([std::ffi::OsString::from("git")]),
            Box::new([workspace.clone()]),
            None,
        );

        // A relative value without an explicit `--git-dir` resolves against
        // whichever repository git discovers upward, which argv cannot name:
        // fail closed even when the `-C`-anchored join stays inside.
        assert!(!grant_covers(
            &approved,
            &config_argv("-c", "core.worktree=nested-repo"),
            &workspace
        ));
        let absolute_inside =
            std::ffi::OsString::from(format!("core.worktree={}", nested.display()));
        let mut absolute_argv: Vec<std::ffi::OsString> =
            vec!["git".into(), "-c".into(), absolute_inside, "status".into()];
        assert!(grant_covers(&approved, &absolute_argv, &workspace));

        let absolute_outside =
            std::ffi::OsString::from(format!("core.worktree={}", outside.display()));
        absolute_argv[2] = absolute_outside.clone();
        assert!(!grant_covers(&approved, &absolute_argv, &workspace));
        assert!(!grant_covers(
            &approved,
            &config_argv("--config", &absolute_outside.to_string_lossy()),
            &workspace
        ));
        let mut stuck = std::ffi::OsString::from("-c");
        stuck.push(&absolute_outside);
        assert!(!grant_covers(
            &approved,
            &["git".into(), stuck, "status".into()],
            &workspace
        ));
        assert!(!grant_covers(
            &approved,
            &config_argv("-c", "core.worktree=../outside"),
            &workspace
        ));
        assert!(!grant_covers(
            &approved,
            &config_argv("-c", "core.worktree="),
            &workspace
        ));
        assert!(!grant_covers(
            &approved,
            &config_argv("-c", "core.bare"),
            &workspace
        ));
        assert!(!grant_covers(
            &approved,
            &["git".into(), "-c".into()],
            &workspace
        ));

        // Relative values must also hold from each explicit `--git-dir` anchor.
        let anchored_inside: Vec<std::ffi::OsString> = vec![
            "git".into(),
            "--git-dir".into(),
            ".git".into(),
            "-c".into(),
            "core.worktree=nested-repo".into(),
            "status".into(),
        ];
        assert!(grant_covers(&approved, &anchored_inside, &workspace));
        let anchored_outside: Vec<std::ffi::OsString> = vec![
            "git".into(),
            "--git-dir".into(),
            ".git".into(),
            "-c".into(),
            "core.worktree=../outside".into(),
            "status".into(),
        ];
        assert!(!grant_covers(&approved, &anchored_outside, &workspace));
    }
    #[test]
    fn git_exec_path_and_super_prefix_stay_inside_roots() {
        let temp = tempfile::tempdir().expect("temporary grant roots");
        let outer = temp.path().join("outer-repo");
        let workspace = outer.join("agent-workspace");
        let nested = workspace.join("nested-repo");
        let bin = workspace.join("bin");
        for dir in [&nested, &bin] {
            std::fs::create_dir_all(dir).expect("grant fixture directory");
        }
        let approved = Approved::new(
            CallId::new("exec-path-grant"),
            None,
            Box::new([std::ffi::OsString::from("git")]),
            Box::new([workspace.clone()]),
            None,
        );

        let mut exec_inside = std::ffi::OsString::from("--exec-path=");
        exec_inside.push(bin.as_os_str());
        let exec_argv: [std::ffi::OsString; 3] = ["git".into(), exec_inside, "status".into()];
        assert!(grant_covers(&approved, &exec_argv, &workspace));
        let mut exec_outside = std::ffi::OsString::from("--exec-path=");
        exec_outside.push(outer.as_os_str());
        let exec_outside_argv: [std::ffi::OsString; 3] =
            ["git".into(), exec_outside, "status".into()];
        assert!(!grant_covers(&approved, &exec_outside_argv, &workspace));
        // Bare `--exec-path` only prints the exec path and exits.
        let bare_exec: [std::ffi::OsString; 3] =
            ["git".into(), "--exec-path".into(), "status".into()];
        assert!(grant_covers(&approved, &bare_exec, &workspace));

        let super_inside: [std::ffi::OsString; 4] = [
            "git".into(),
            "--super-prefix".into(),
            "nested-repo".into(),
            "status".into(),
        ];
        assert!(grant_covers(&approved, &super_inside, &workspace));
        let mut super_outside = std::ffi::OsString::from("--super-prefix=");
        super_outside.push(outer.as_os_str());
        let super_outside_argv: [std::ffi::OsString; 3] =
            ["git".into(), super_outside, "status".into()];
        assert!(!grant_covers(&approved, &super_outside_argv, &workspace));

        // Pager flags never redirect the repository.
        for flag in ["-P", "--no-pager", "-p", "--paginate"] {
            let argv: Vec<std::ffi::OsString> = vec!["git".into(), flag.into(), "status".into()];
            assert!(grant_covers(&approved, &argv, &workspace));
        }
    }
    #[test]
    fn git_env_redirects_stay_inside_roots() {
        let temp = tempfile::tempdir().expect("temporary grant roots");
        let outer = temp.path().join("outer-repo");
        let workspace = outer.join("agent-workspace");
        let nested = workspace.join("nested-repo");
        let workspace_git = workspace.join(".git");
        for dir in [&nested, &workspace_git] {
            std::fs::create_dir_all(dir).expect("grant fixture directory");
        }
        let roots = [workspace.clone()];
        let git: Vec<std::ffi::OsString> = vec!["git".into(), "status".into()];
        let pair = |key: &str, value: &str| {
            (
                std::ffi::OsString::from(key),
                std::ffi::OsString::from(value),
            )
        };
        let absolute_outside = outer.as_os_str().to_os_string();

        assert!(git_env_in_roots(&git, &[], &workspace, &roots));
        assert!(git_env_in_roots(
            &git,
            &[pair("GIT_DIR", ".git")],
            &workspace,
            &roots
        ));
        assert!(git_env_in_roots(
            &git,
            &[pair("GIT_DIR", "")],
            &workspace,
            &roots
        ));
        assert!(!git_env_in_roots(
            &git,
            &[(
                std::ffi::OsString::from("GIT_DIR"),
                absolute_outside.clone()
            )],
            &workspace,
            &roots
        ));
        assert!(git_env_in_roots(
            &git,
            &[pair("GIT_WORK_TREE", "nested-repo")],
            &workspace,
            &roots
        ));
        assert!(!git_env_in_roots(
            &git,
            &[(
                std::ffi::OsString::from("GIT_WORK_TREE"),
                absolute_outside.clone()
            )],
            &workspace,
            &roots
        ));
        assert!(!git_env_in_roots(
            &git,
            &[pair("GIT_NAMESPACE", "agent")],
            &workspace,
            &roots
        ));
        assert!(git_env_in_roots(
            &git,
            &[pair("GIT_NAMESPACE", "")],
            &workspace,
            &roots
        ));
        assert!(!git_env_in_roots(
            &git,
            &[(
                std::ffi::OsString::from("GIT_COMMON_DIR"),
                absolute_outside.clone()
            )],
            &workspace,
            &roots
        ));
        assert!(!git_env_in_roots(
            &git,
            &[(
                std::ffi::OsString::from("GIT_EXEC_PATH"),
                absolute_outside.clone()
            )],
            &workspace,
            &roots
        ));
        // Other programs ignore these variables; unrelated keys pass through.
        let other: Vec<std::ffi::OsString> = vec!["ls".into(), "status".into()];
        assert!(git_env_in_roots(
            &other,
            &[(
                std::ffi::OsString::from("GIT_DIR"),
                absolute_outside.clone()
            )],
            &workspace,
            &roots
        ));
        assert!(git_env_in_roots(
            &git,
            &[(std::ffi::OsString::from("PAGER"), absolute_outside.clone())],
            &workspace,
            &roots
        ));
    }
    #[test]
    fn git_env_git_config_stays_inside_roots() {
        let temp = tempfile::tempdir().expect("temporary grant roots");
        let outer = temp.path().join("outer-repo");
        let workspace = outer.join("agent-workspace");
        let workspace_git = workspace.join(".git");
        std::fs::create_dir_all(&workspace_git).expect("grant fixture directory");
        let roots = [workspace.clone()];
        let git: Vec<std::ffi::OsString> = vec!["git".into(), "status".into()];
        let pair = |key: &str, value: &str| {
            (
                std::ffi::OsString::from(key),
                std::ffi::OsString::from(value),
            )
        };
        let absolute_outside = outer.as_os_str().to_os_string();
        assert!(git_env_in_roots(
            &git,
            &[pair("GIT_CONFIG", ".git")],
            &workspace,
            &roots
        ));
        assert!(git_env_in_roots(
            &git,
            &[pair("GIT_CONFIG", "")],
            &workspace,
            &roots
        ));
        assert!(!git_env_in_roots(
            &git,
            &[(std::ffi::OsString::from("GIT_CONFIG"), absolute_outside,)],
            &workspace,
            &roots
        ));
        assert!(!git_env_in_roots(
            &git,
            &[pair("git_config", "/outside/repo")],
            &workspace,
            &roots
        ));
    }
    #[test]
    fn git_env_config_schemes_stay_inside_roots() {
        let temp = tempfile::tempdir().expect("temporary grant roots");
        let outer = temp.path().join("outer-repo");
        let workspace = outer.join("agent-workspace");
        let nested = workspace.join("nested-repo");
        std::fs::create_dir_all(&nested).expect("grant fixture directory");
        let roots = [workspace.clone()];
        let git: Vec<std::ffi::OsString> = vec!["git".into(), "status".into()];
        let pair = |key: &str, value: &str| {
            (
                std::ffi::OsString::from(key),
                std::ffi::OsString::from(value),
            )
        };
        let config = |count: &str, entries: &[(&str, &str)]| {
            let mut env = vec![pair("GIT_CONFIG_COUNT", count)];
            for (index, (key, value)) in entries.iter().enumerate() {
                env.push(pair(&format!("GIT_CONFIG_KEY_{index}"), key));
                env.push(pair(&format!("GIT_CONFIG_VALUE_{index}"), value));
            }
            env
        };
        let outside = outer.join("outside");
        std::fs::create_dir_all(&outside).expect("outside directory");

        // No count behaves as unset, even with stray pairs present.
        assert!(git_env_in_roots(
            &git,
            &[pair("GIT_CONFIG_KEY_0", "core.bare")],
            &workspace,
            &roots
        ));
        // Benign keys pass through the scheme untouched.
        assert!(git_env_in_roots(
            &git,
            &config("1", &[("user.name", "agent")]),
            &workspace,
            &roots
        ));
        // Absolute redirect-capable values confine from the spawn cwd.
        assert!(git_env_in_roots(
            &git,
            &config("1", &[("core.worktree", &nested.to_string_lossy())]),
            &workspace,
            &roots
        ));
        assert!(!git_env_in_roots(
            &git,
            &config("1", &[("core.worktree", &outside.to_string_lossy())]),
            &workspace,
            &roots
        ));
        // Relative values carry no argv-visible anchor: fail closed.
        assert!(!git_env_in_roots(
            &git,
            &config("1", &[("core.worktree", "nested-repo")]),
            &workspace,
            &roots
        ));
        assert!(!git_env_in_roots(
            &git,
            &config("1", &[("core.hooksPath", &outside.to_string_lossy())]),
            &workspace,
            &roots
        ));
        assert!(!git_env_in_roots(
            &git,
            &config("2", &[("user.name", "agent"), ("core.bare", "true")]),
            &workspace,
            &roots
        ));
        // Malformed counts and absurd budgets fail closed, not unbounded.
        assert!(!git_env_in_roots(
            &git,
            &[pair("GIT_CONFIG_COUNT", "many")],
            &workspace,
            &roots
        ));
        assert!(!git_env_in_roots(
            &git,
            &[pair("GIT_CONFIG_COUNT", "1000000")],
            &workspace,
            &roots
        ));
        assert!(git_env_in_roots(
            &git,
            &[pair("GIT_CONFIG_COUNT", "")],
            &workspace,
            &roots
        ));
        assert!(git_env_in_roots(
            &git,
            &[pair("GIT_CONFIG_COUNT", "0")],
            &workspace,
            &roots
        ));
    }
    #[test]
    fn git_config_bare_needs_an_explicit_false() {
        fn config_argv(flag: &str, kv: &str) -> Vec<std::ffi::OsString> {
            vec!["git".into(), flag.into(), kv.into(), "status".into()]
        }
        let temp = tempfile::tempdir().expect("temporary grant roots");
        let workspace = temp.path().join("agent-workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let approved = Approved::new(
            CallId::new("bare-grant"),
            None,
            Box::new([std::ffi::OsString::from("git")]),
            Box::new([workspace.clone()]),
            None,
        );
        for truthy in ["true", "yes", "1", "on"] {
            assert!(
                !grant_covers(
                    &approved,
                    &config_argv("-c", &format!("core.bare={truthy}")),
                    &workspace
                ),
                "core.bare={truthy} must fail closed"
            );
        }
        for falsy in ["false", "no", "off", "0"] {
            assert!(
                grant_covers(
                    &approved,
                    &config_argv("-c", &format!("core.bare={falsy}")),
                    &workspace
                ),
                "core.bare={falsy} redirects nothing"
            );
        }
        assert!(grant_covers(
            &approved,
            &config_argv("-c", "receive.denyCurrentBranch=false"),
            &workspace
        ));
        assert!(grant_covers(
            &approved,
            &config_argv("-c", "advice.detachedHead=false"),
            &workspace
        ));
    }
    #[test]
    fn git_config_hookspath_and_includes_stay_inside_roots() {
        fn config_argv(flag: &str, kv: &str) -> Vec<std::ffi::OsString> {
            vec!["git".into(), flag.into(), kv.into(), "status".into()]
        }
        let temp = tempfile::tempdir().expect("temporary grant roots");
        let outer = temp.path().join("outer-repo");
        let workspace = outer.join("agent-workspace");
        let nested = workspace.join("nested-repo");
        let hooks = workspace.join("hooks");
        let outside = outer.join("outside");
        for dir in [&nested, &hooks, &outside] {
            std::fs::create_dir_all(dir).expect("grant fixture directory");
        }
        let approved = Approved::new(
            CallId::new("hooks-grant"),
            None,
            Box::new([std::ffi::OsString::from("git")]),
            Box::new([workspace.clone()]),
            None,
        );
        // Verified live: an outside hooks dir runs its hooks on commit.
        assert!(grant_covers(
            &approved,
            &config_argv("-c", &format!("core.hooksPath={}", hooks.display())),
            &workspace
        ));
        assert!(!grant_covers(
            &approved,
            &config_argv("-c", &format!("core.hooksPath={}", outside.display())),
            &workspace
        ));
        assert!(!grant_covers(
            &approved,
            &config_argv("-c", "core.hooksPath=hooks"),
            &workspace
        ));
        assert!(!grant_covers(
            &approved,
            &config_argv("-c", &format!("include.path={}", nested.display())),
            &workspace
        ));
        assert!(!grant_covers(
            &approved,
            &config_argv("-c", "includeIf.gitdir:/work/.path=/tmp/inc"),
            &workspace
        ));
    }
    #[test]
    fn git_config_env_keys_fail_closed() {
        fn config_argv(flag: &str, kv: &str) -> Vec<std::ffi::OsString> {
            vec!["git".into(), flag.into(), kv.into(), "status".into()]
        }
        let temp = tempfile::tempdir().expect("temporary grant roots");
        let workspace = temp.path().join("agent-workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let approved = Approved::new(
            CallId::new("config-env-grant"),
            None,
            Box::new([std::ffi::OsString::from("git")]),
            Box::new([workspace.clone()]),
            None,
        );
        assert!(!grant_covers(
            &approved,
            &config_argv("--config-env", "core.worktree=HOME"),
            &workspace
        ));
        let mut config_env_equals = std::ffi::OsString::from("--config-env=");
        config_env_equals.push("core.worktree=HOME");
        assert!(!grant_covers(
            &approved,
            &["git".into(), config_env_equals, "status".into()],
            &workspace
        ));
        assert!(!grant_covers(
            &approved,
            &config_argv("--config-env", "core.bare=FLAG"),
            &workspace
        ));
        assert!(grant_covers(
            &approved,
            &config_argv("--config-env", "advice.detachedHead=FLAG"),
            &workspace
        ));
        // The env value is just a name here, so redirect-capable keys fail
        // closed however the variable is spelled.
        assert!(!grant_covers(
            &approved,
            &config_argv("--config-env", "core.hooksPath=E"),
            &workspace
        ));
        assert!(!grant_covers(
            &approved,
            &config_argv("--config-env", "include.path=E"),
            &workspace
        ));
    }
    #[test]
    fn git_env_relay_and_files_fail_closed() {
        let temp = tempfile::tempdir().expect("temporary grant roots");
        let workspace = temp.path().join("agent-workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let roots = [workspace.clone()];
        let git: Vec<std::ffi::OsString> = vec!["git".into(), "status".into()];
        let pair = |key: &str, value: &str| {
            (
                std::ffi::OsString::from(key),
                std::ffi::OsString::from(value),
            )
        };
        // The internal `-c` relay channel is never legitimate top-level.
        assert!(!git_env_in_roots(
            &git,
            &[pair("GIT_CONFIG_PARAMETERS", "'user.name=agent'")],
            &workspace,
            &roots
        ));
        assert!(git_env_in_roots(
            &git,
            &[pair("GIT_CONFIG_PARAMETERS", "")],
            &workspace,
            &roots
        ));
        // Alternate config files load before argv config: fail closed.
        assert!(!git_env_in_roots(
            &git,
            &[pair("GIT_CONFIG_GLOBAL", "/outside/gitconfig")],
            &workspace,
            &roots
        ));
        assert!(!git_env_in_roots(
            &git,
            &[pair("GIT_CONFIG_SYSTEM", "/outside/gitconfig")],
            &workspace,
            &roots
        ));
    }
    #[test]
    fn git_spawn_gate_ignores_approval_shape() {
        // Both spawn doors apply this helper with no prefix input: scoped,
        // one-shot, and allow-all approvals all deny the same escapes.
        let temp = tempfile::tempdir().expect("temporary grant roots");
        let outer = temp.path().join("outer-repo");
        let workspace = outer.join("agent-workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let roots = [workspace.clone()];
        let outside = outer.as_os_str().to_os_string();
        let mut git_dir = std::ffi::OsString::from("--git-dir=");
        git_dir.push(&outside);
        let argv: Vec<std::ffi::OsString> = vec!["git".into(), git_dir, "status".into()];
        assert!(!git_spawn_in_roots(&argv, &[], &workspace, &roots));
        let git: Vec<std::ffi::OsString> = vec!["git".into(), "status".into()];
        assert!(git_spawn_in_roots(&git, &[], &workspace, &roots));
        assert!(!git_spawn_in_roots(
            &git,
            &[(std::ffi::OsString::from("GIT_DIR"), outside.clone())],
            &workspace,
            &roots
        ));
    }
    #[test]
    fn git_env_duplicate_keys_use_the_last_match() {
        // Later duplicates win in the child (`Command::envs` overwrites), so
        // a benign first count cannot mask a hostile later one, and vice
        // versa. The per-variable loop already denies on any hostile entry.
        let temp = tempfile::tempdir().expect("temporary grant roots");
        let outer = temp.path().join("outer-repo");
        let workspace = outer.join("agent-workspace");
        let outside = outer.join("outside");
        for dir in [&workspace, &outside] {
            std::fs::create_dir_all(dir).expect("grant fixture directory");
        }
        let roots = [workspace.clone()];
        let git: Vec<std::ffi::OsString> = vec!["git".into(), "status".into()];
        let pair = |key: &str, value: &str| {
            (
                std::ffi::OsString::from(key),
                std::ffi::OsString::from(value),
            )
        };
        assert!(!git_env_in_roots(
            &git,
            &[
                pair("GIT_CONFIG_COUNT", "1"),
                pair("GIT_CONFIG_KEY_0", "user.name"),
                pair("GIT_CONFIG_VALUE_0", "agent"),
                pair("GIT_CONFIG_COUNT", "2"),
                pair("GIT_CONFIG_KEY_1", "core.bare"),
                pair("GIT_CONFIG_VALUE_1", "true"),
            ],
            &workspace,
            &roots
        ));
        assert!(git_env_in_roots(
            &git,
            &[
                pair("GIT_CONFIG_COUNT", "1"),
                pair("GIT_CONFIG_KEY_0", "core.bare"),
                pair("GIT_CONFIG_VALUE_0", "true"),
                pair("GIT_CONFIG_COUNT", "1"),
                pair("GIT_CONFIG_KEY_0", "user.name"),
                pair("GIT_CONFIG_VALUE_0", "agent"),
            ],
            &workspace,
            &roots
        ));
        assert!(!git_env_in_roots(
            &git,
            &[
                pair("GIT_DIR", ".git"),
                (
                    std::ffi::OsString::from("GIT_DIR"),
                    outside.as_os_str().to_os_string()
                ),
            ],
            &workspace,
            &roots
        ));
    }
    #[test]
    fn git_config_file_options_stay_inside_roots() {
        let temp = tempfile::tempdir().expect("temporary grant roots");
        let outer = temp.path().join("outer-repo");
        let workspace = outer.join("agent-workspace");
        let inside = workspace.join("config");
        let outside = outer.join("outside-config");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::File::create(&inside).expect("in-root config");
        std::fs::File::create(&outside).expect("outside config");
        let approved = Approved::new(
            CallId::new("config-file-grant"),
            None,
            Box::new([std::ffi::OsString::from("git")]),
            Box::new([workspace.clone()]),
            None,
        );
        let config_arg = |flag: &str, path: &str| {
            vec![
                "git".into(),
                "config".into(),
                flag.into(),
                path.into(),
                "user.name".into(),
            ]
        };
        let config_equals = |flag: &str, path: &str| {
            vec![
                "git".into(),
                "config".into(),
                format!("{flag}={path}").into(),
                "user.name".into(),
            ]
        };
        let inside_path = inside.to_string_lossy();
        for argv in [
            config_arg("--file", "config"),
            config_arg("-f", "config"),
            config_arg("--fil", "config"),
            config_equals("--file", &inside_path),
            config_equals("-f", &inside_path),
            vec![
                "git".into(),
                "config".into(),
                "get".into(),
                "--file".into(),
                "config".into(),
                "user.name".into(),
            ],
        ] {
            assert!(
                grant_covers(&approved, &argv, &workspace),
                "in-root git config option was denied: {argv:?}"
            );
        }
        let outside_path = outside.to_string_lossy();
        for argv in [
            config_arg("--file", &outside_path),
            config_arg("-f", &outside_path),
            config_equals("--fil", &outside_path),
            config_equals("-f", &outside_path),
            config_arg("--global", "user.name"),
            config_arg("--system", "user.name"),
            config_arg("--includes", "user.name"),
        ] {
            assert!(
                !grant_covers(&approved, &argv, &workspace),
                "outside git config option was allowed: {argv:?}"
            );
        }
        assert!(!grant_covers(
            &approved,
            &["git".into(), "config".into(), "--file".into()],
            &workspace
        ));
    }

    #[test]
    fn git_selector_abbreviations_and_clone_config_stay_inside_roots() {
        let temp = tempfile::tempdir().expect("temporary grant roots");
        let outer = temp.path().join("outer-repo");
        let workspace = outer.join("agent-workspace");
        let nested = workspace.join("nested");
        let workspace_git = workspace.join(".git");
        std::fs::create_dir_all(&nested).expect("nested workspace");
        std::fs::create_dir_all(&workspace_git).expect("workspace git directory");
        let approved = Approved::new(
            CallId::new("selector-abbrev-grant"),
            None,
            Box::new([std::ffi::OsString::from("git")]),
            Box::new([workspace.clone()]),
            None,
        );
        let outside = outer.to_string_lossy();
        let outside_git: std::ffi::OsString = format!("--git-d={outside}").into();
        let outside_work_tree: std::ffi::OsString = format!("--work-t={outside}").into();
        let outside_super_prefix: std::ffi::OsString = format!("--super-p={outside}").into();
        let outside_exec_path: std::ffi::OsString = format!("--exec-p={outside}").into();
        for flag in [
            outside_git,
            outside_work_tree,
            outside_super_prefix,
            outside_exec_path,
        ] {
            let argv: Vec<std::ffi::OsString> = vec!["git".into(), flag, "status".into()];
            assert!(
                !grant_covers(&approved, &argv, &workspace),
                "abbreviated selector escaped roots: {argv:?}"
            );
        }
        assert!(grant_covers(
            &approved,
            &[
                "git".into(),
                "--git-d".into(),
                ".git".into(),
                "status".into()
            ],
            &workspace
        ));
        assert!(!grant_covers(
            &approved,
            &["git".into(), "--bar".into(), "status".into()],
            &workspace
        ));
        assert!(!grant_covers(
            &approved,
            &[
                "git".into(),
                "--con".into(),
                outer.as_os_str().to_os_string(),
                "status".into(),
            ],
            &workspace
        ));
        assert!(!grant_covers(
            &approved,
            &[
                "git".into(),
                "-C".into(),
                "nested".into(),
                "-c".into(),
                format!("core.worktree={outside}").into(),
                "status".into(),
            ],
            &workspace
        ));

        assert!(!grant_covers(
            &approved,
            &[
                "git".into(),
                "-C".into(),
                "nested".into(),
                "-c".into(),
                format!("core.worktree={outside}").into(),
                "status".into(),
            ],
            &workspace
        ));
    }

    #[test]
    fn git_clone_config_stays_inside_roots() {
        let temp = tempfile::tempdir().expect("temporary grant roots");
        let outer = temp.path().join("outer-repo");
        let workspace = outer.join("agent-workspace");
        let nested = workspace.join("nested");
        std::fs::create_dir_all(&nested).expect("nested workspace");
        let approved = Approved::new(
            CallId::new("clone-config-grant"),
            None,
            Box::new([std::ffi::OsString::from("git")]),
            Box::new([workspace.clone()]),
            None,
        );
        let outside = outer.to_string_lossy();
        let clone = |flag: &str, kv: &str| {
            vec![
                "git".into(),
                "clone".into(),
                flag.into(),
                kv.into(),
                "source".into(),
                "destination".into(),
            ]
        };
        let inside_work_tree = format!("core.worktree={}", nested.display());
        let outside_work_tree = format!("core.worktree={outside}");
        assert!(grant_covers(
            &approved,
            &clone("-c", &inside_work_tree),
            &workspace
        ));
        assert!(grant_covers(
            &approved,
            &clone("--config", &inside_work_tree),
            &workspace
        ));
        assert!(!grant_covers(
            &approved,
            &clone("-c", &outside_work_tree),
            &workspace
        ));
        assert!(!grant_covers(
            &approved,
            &clone("--confi", &outside_work_tree),
            &workspace
        ));
    }

    fn git_subcommand_init_outputs_stay_inside_roots(
        approved: &Approved,
        workspace: &Path,
        outside: &str,
        inside_arg: &str,
    ) {
        assert!(!grant_covers(
            approved,
            &[
                "git".into(),
                "init".into(),
                format!("--separate-git-dir={outside}").into(),
                "repo".into(),
            ],
            workspace
        ));
        assert!(grant_covers(
            approved,
            &[
                "git".into(),
                "init".into(),
                format!("--separate-git-dir={inside_arg}").into(),
                "repo".into(),
            ],
            workspace
        ));
        assert!(!grant_covers(
            approved,
            &[
                "git".into(),
                "init".into(),
                format!("--separate-git-d={outside}").into(),
                "repo".into(),
            ],
            workspace
        ));
        assert!(!grant_covers(
            approved,
            &["git".into(), "init".into(), outside.into()],
            workspace
        ));
        assert!(grant_covers(
            approved,
            &["git".into(), "init".into(), inside_arg.into()],
            workspace
        ));
    }

    fn git_subcommand_clone_outputs_stay_inside_roots(
        approved: &Approved,
        workspace: &Path,
        outside: &str,
        inside_arg: &str,
    ) {
        assert!(!grant_covers(
            approved,
            &[
                "git".into(),
                "clone".into(),
                "https://example.invalid/repo".into(),
                outside.into(),
            ],
            workspace
        ));
        assert!(grant_covers(
            approved,
            &[
                "git".into(),
                "clone".into(),
                "https://example.invalid/repo".into(),
                inside_arg.into(),
            ],
            workspace
        ));
        assert!(!grant_covers(
            approved,
            &[
                "git".into(),
                "clone".into(),
                format!("--separate-git-dir={outside}").into(),
                "src".into(),
                "dest".into(),
            ],
            workspace
        ));
    }

    fn git_subcommand_worktree_outputs_stay_inside_roots(
        approved: &Approved,
        workspace: &Path,
        outside: &str,
        inside_arg: &str,
    ) {
        assert!(!grant_covers(
            approved,
            &[
                "git".into(),
                "worktree".into(),
                "add".into(),
                outside.into(),
            ],
            workspace
        ));
        assert!(grant_covers(
            approved,
            &[
                "git".into(),
                "worktree".into(),
                "add".into(),
                inside_arg.into(),
            ],
            workspace
        ));
    }

    fn git_subcommand_archive_and_bundle_outputs_stay_inside_roots(
        approved: &Approved,
        workspace: &Path,
        outside: &str,
        inside_arg: &str,
    ) {
        assert!(!grant_covers(
            approved,
            &[
                "git".into(),
                "archive".into(),
                "-o".into(),
                outside.into(),
                "HEAD".into(),
            ],
            workspace
        ));
        assert!(grant_covers(
            approved,
            &[
                "git".into(),
                "archive".into(),
                "-o".into(),
                inside_arg.into(),
                "HEAD".into(),
            ],
            workspace
        ));
        assert!(!grant_covers(
            approved,
            &[
                "git".into(),
                "bundle".into(),
                "create".into(),
                outside.into(),
                "HEAD".into(),
            ],
            workspace
        ));
    }

    fn git_subcommand_config_output_and_status_stay_inside_roots(
        approved: &Approved,
        workspace: &Path,
        outside: &str,
        inside_arg: &str,
    ) {
        assert!(!grant_covers(
            approved,
            &[
                "git".into(),
                "config".into(),
                "core.worktree".into(),
                outside.into(),
            ],
            workspace
        ));
        assert!(grant_covers(
            approved,
            &[
                "git".into(),
                "config".into(),
                "core.worktree".into(),
                inside_arg.into(),
            ],
            workspace
        ));
        assert!(grant_covers(
            approved,
            &["git".into(), "status".into()],
            workspace
        ));
    }

    #[test]
    fn git_subcommand_outputs_stay_inside_roots() {
        let temp = tempfile::tempdir().expect("temporary grant roots");
        let outer = temp.path().join("outer-repo");
        let workspace = outer.join("agent-workspace");
        let inside = workspace.join("inside");
        std::fs::create_dir_all(&inside).expect("workspace");
        let approved = Approved::new(
            CallId::new("subcommand-output-grant"),
            None,
            Box::new([std::ffi::OsString::from("git")]),
            Box::new([workspace.clone()]),
            None,
        );
        let outside = outer.to_string_lossy().into_owned();
        let inside_arg = inside.to_string_lossy().into_owned();
        git_subcommand_init_outputs_stay_inside_roots(&approved, &workspace, &outside, &inside_arg);
        git_subcommand_clone_outputs_stay_inside_roots(
            &approved,
            &workspace,
            &outside,
            &inside_arg,
        );
        git_subcommand_worktree_outputs_stay_inside_roots(
            &approved,
            &workspace,
            &outside,
            &inside_arg,
        );
        git_subcommand_archive_and_bundle_outputs_stay_inside_roots(
            &approved,
            &workspace,
            &outside,
            &inside_arg,
        );
        git_subcommand_config_output_and_status_stay_inside_roots(
            &approved,
            &workspace,
            &outside,
            &inside_arg,
        );
    }

    #[test]
    fn git_subcommand_attached_long_value_stays_inside_roots() {
        let temp = tempfile::tempdir().expect("temporary grant roots");
        let outer = temp.path().join("outer-repo");
        let workspace = outer.join("agent-workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let approved = Approved::new(
            CallId::new("attached-long-grant"),
            None,
            Box::new([std::ffi::OsString::from("git")]),
            Box::new([workspace.clone()]),
            None,
        );
        let outside = outer.to_string_lossy().into_owned();
        let inside_arg = workspace.to_string_lossy().into_owned();
        assert!(grant_covers(
            &approved,
            &[
                "git".into(),
                "format-patch".into(),
                format!("--output-directory={inside_arg}").into(),
                "HEAD".into(),
            ],
            &workspace
        ));
        assert!(!grant_covers(
            &approved,
            &[
                "git".into(),
                "format-patch".into(),
                format!("--output-directory={outside}").into(),
                "HEAD".into(),
            ],
            &workspace
        ));
        // An unlisted option whose value is a plain string stays allowed, and
        // a negated form carries no operand: neither confuses the attached
        // value confinement with git's real option grammar. An empty attached
        // value selects nothing and Git accepts it for optional-argument
        // options (`log --pretty=`), so it stays allowed too.
        assert!(grant_covers(
            &approved,
            &[
                "git".into(),
                "format-patch".into(),
                "--cover-from-description=subject".into(),
                "HEAD".into(),
            ],
            &workspace
        ));
        assert!(grant_covers(
            &approved,
            &[
                "git".into(),
                "log".into(),
                "--pretty=".into(),
                "HEAD".into()
            ],
            &workspace
        ));
        assert!(grant_covers(
            &approved,
            &[
                "git".into(),
                "format-patch".into(),
                "--no-attach".into(),
                "HEAD".into(),
            ],
            &workspace
        ));
    }

    #[test]
    fn git_subcommand_short_cluster_value_stays_inside_roots() {
        let temp = tempfile::tempdir().expect("temporary grant roots");
        let outer = temp.path().join("outer-repo");
        let workspace = outer.join("agent-workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let approved = Approved::new(
            CallId::new("short-cluster-grant"),
            None,
            Box::new([std::ffi::OsString::from("git")]),
            Box::new([workspace.clone()]),
            None,
        );
        let outside = outer.to_string_lossy().into_owned();
        let inside_arg = workspace.to_string_lossy().into_owned();
        assert!(grant_covers(
            &approved,
            &[
                "git".into(),
                "format-patch".into(),
                format!("-qo{inside_arg}").into(),
                "HEAD".into(),
            ],
            &workspace
        ));
        assert!(!grant_covers(
            &approved,
            &[
                "git".into(),
                "format-patch".into(),
                format!("-qo{outside}").into(),
                "HEAD".into(),
            ],
            &workspace
        ));
        // A leading value-taking short owns its whole remainder as a string
        // argument, so an `o`/`u` inside it is not a selector: `-Sdo/bar`
        // and `-Gdo/bar` stay allowed even though a naive first-`o`/`u` read
        // would parse `/bar` as an outside path. A no-selector cluster and a
        // selector-tail cluster carry no glued operand, so `-Sxyz` and bare
        // `-qo` stay inert and the next argument remains the ordinary
        // position the non-dash check already confines.
        assert!(grant_covers(
            &approved,
            &["git".into(), "log".into(), "-Sfoo".into(), "HEAD".into()],
            &workspace
        ));
        assert!(grant_covers(
            &approved,
            &["git".into(), "log".into(), "-Sdo/bar".into(), "HEAD".into(),],
            &workspace
        ));
        assert!(grant_covers(
            &approved,
            &["git".into(), "log".into(), "-Gdo/bar".into(), "HEAD".into(),],
            &workspace
        ));
        assert!(grant_covers(
            &approved,
            &["git".into(), "log".into(), "-Sxyz".into(), "HEAD".into()],
            &workspace
        ));
        assert!(grant_covers(
            &approved,
            &[
                "git".into(),
                "format-patch".into(),
                "-qo".into(),
                inside_arg.clone().into(),
            ],
            &workspace
        ));
    }

    #[test]
    fn git_subcommand_flags_are_not_global_selectors() {
        // Git applies wrapper selectors only before the subcommand: later
        // tokens are the subcommand's own flags, so ordinary in-root
        // workflows (`switch -c`, `grep -c`) keep working. Post-command
        // wrapper spellings are rejected by git itself, hence benign.
        let temp = tempfile::tempdir().expect("temporary grant roots");
        let outer = temp.path().join("outer-repo");
        let workspace = outer.join("agent-workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let approved = Approved::new(
            CallId::new("subcommand-grant"),
            None,
            Box::new([std::ffi::OsString::from("git")]),
            Box::new([workspace.clone()]),
            None,
        );
        for argv in [
            vec!["git".into(), "switch".into(), "-c".into(), "feature".into()],
            vec!["git".into(), "grep".into(), "-c".into(), "pattern".into()],
            vec!["git".into(), "grep".into(), "-C3".into(), "pattern".into()],
            vec![
                "git".into(),
                "status".into(),
                "--git-dir".into(),
                "x".into(),
            ],
        ] {
            assert!(grant_covers(&approved, &argv, &workspace));
        }
        // Pre-command selectors still gate: the stop only narrows the scan.
        let mut outside_git_dir = std::ffi::OsString::from("--git-dir=");
        outside_git_dir.push(outer.as_os_str());
        assert!(!grant_covers(
            &approved,
            &["git".into(), outside_git_dir, "status".into()],
            &workspace
        ));
    }
    #[test]
    fn git_env_names_match_case_insensitively() {
        // Windows resolves environment names case-insensitively, so lowercase
        // spellings reach git under the canonical names there.
        let temp = tempfile::tempdir().expect("temporary grant roots");
        let outer = temp.path().join("outer-repo");
        let workspace = outer.join("agent-workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let roots = [workspace.clone()];
        let git: Vec<std::ffi::OsString> = vec!["git".into(), "status".into()];
        let pair = |key: &str, value: &str| {
            (
                std::ffi::OsString::from(key),
                std::ffi::OsString::from(value),
            )
        };
        assert!(!git_env_in_roots(
            &git,
            &[pair("git_dir", "/outside/repo")],
            &workspace,
            &roots
        ));
        assert!(!git_env_in_roots(
            &git,
            &[pair("Git_Namespace", "agent")],
            &workspace,
            &roots
        ));
        assert!(!git_env_in_roots(
            &git,
            &[
                pair("git_config_count", "1"),
                pair("git_config_key_0", "core.bare"),
                pair("git_config_value_0", "true"),
            ],
            &workspace,
            &roots
        ));
    }
    #[test]
    fn git_program_and_object_env_close_remnants() {
        // `GIT.EXE` invokes git on Windows: the gates cannot key on exact
        // `git`. Object store and index locations select repository
        // components, so they confine like `GIT_DIR`; the alternates list
        // cannot be confined element-wise and denies instead.
        let temp = tempfile::tempdir().expect("temporary grant roots");
        let outer = temp.path().join("outer-repo");
        let workspace = outer.join("agent-workspace");
        let objects = workspace.join("objects");
        std::fs::create_dir_all(&objects).expect("objects directory");
        let roots = [workspace.clone()];
        let git: Vec<std::ffi::OsString> = vec!["git".into(), "status".into()];
        let pair = |key: &str, value: &str| {
            (
                std::ffi::OsString::from(key),
                std::ffi::OsString::from(value),
            )
        };
        let outside = outer.join("outside");
        std::fs::create_dir_all(&outside).expect("outside directory");
        let git_exe: Vec<std::ffi::OsString> =
            vec!["GIT.EXE".into(), "--namespace".into(), "x".into()];
        assert!(!git_spawn_in_roots(&git_exe, &[], &workspace, &roots));
        let git_exe_ok: Vec<std::ffi::OsString> = vec!["GIT.EXE".into(), "status".into()];
        assert!(git_spawn_in_roots(&git_exe_ok, &[], &workspace, &roots));
        assert!(git_env_in_roots(
            &git,
            &[pair("GIT_OBJECT_DIRECTORY", "objects")],
            &workspace,
            &roots
        ));
        assert!(!git_env_in_roots(
            &git,
            &[pair(
                "GIT_OBJECT_DIRECTORY",
                outside.to_string_lossy().as_ref()
            )],
            &workspace,
            &roots
        ));
        assert!(!git_env_in_roots(
            &git,
            &[pair(
                "GIT_ALTERNATE_OBJECT_DIRECTORIES",
                outside.to_string_lossy().as_ref()
            )],
            &workspace,
            &roots
        ));
    }

    #[test]
    fn ask_never_joins_a_read_run() {
        let resolved = |id: &str, name: &str, class: dal_core::ToolClass| dal_core::ResolvedCall {
            call: CallId::new(id),
            name: name.parse().expect("tool name"),
            promoted: false,
            result: Ok(class),
        };
        let calls = vec![
            resolved("a", "ask", dal_core::ToolClass::Read),
            resolved("b", "read", dal_core::ToolClass::Read),
        ];
        let (units, failed) = plan_units(&calls);
        assert_eq!(failed, []);
        assert!(
            matches!(
                units.as_slice(),
                [Unit::Serial { call: first }, Unit::Reads { calls: rest }]
                    if first.as_str() == "a" && rest.len() == 1 && rest[0].as_str() == "b"
            ),
            "ask dispatches alone even beside reads: {units:?}"
        );
        let calls = vec![
            resolved("a", "read", dal_core::ToolClass::Read),
            resolved("b", "read", dal_core::ToolClass::Read),
        ];
        let (units, failed) = plan_units(&calls);
        assert_eq!(failed, []);
        assert!(
            matches!(units.as_slice(), [Unit::Reads { calls }] if calls.len() == 2),
            "plain reads still batch: {units:?}"
        );
    }

    #[test]
    fn cancellation_maps_to_the_interrupted_settlement() {
        assert!(matches!(
            map_outcome(ToolOutcome::Interrupted),
            SettledOutcome::Interrupted
        ));
        assert!(matches!(
            map_outcome(ToolOutcome::Err(crate::error::ToolError::Cancelled)),
            SettledOutcome::Interrupted
        ));
    }
}
