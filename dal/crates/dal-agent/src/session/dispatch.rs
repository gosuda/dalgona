//! Ordered call dispatch: hooks, runs, approvals, settlements.
//!
//! Plan units run strictly in call order; read runs hold at most
//! `parallel_reads` in flight. Tools drive approvals themselves through
//! [`ToolCx::authorize`]; the per-call runtime behind it mints the
//! move-only [`Approved`] proof per the ladder. Settlements and broker
//! resolutions report back to the actor for journaling.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use dal_core::ext::ToolCallEvent;
use dal_core::{
    Answer, CallId, ClientId, GrantSpec, JobEnd, JobId, Name, Owner, Policy, Preview, Question,
    RawJson, Request, ResolvedCall, SessionId, SettledOutcome, ToolClass, TurnId, Unit, Workspace,
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use super::backend::Backend;
use super::context::{DeferredTool, is_core_tool_search, tool_search_query, tool_search_results};
use crate::broker::{Broker, Resolved, default_timeout};
use crate::ext::generation::Generation;
use crate::ext::hooks::{DispatchCx, dispatch_tool_call};
use crate::ext::overlay::TurnTools;
use crate::ext::tool::{Approved, CallSnapshot, ToolCall, ToolCx, ToolCxRuntime, ToolOutcome};
use crate::ext::{BoxFuture, Caller, CallerKind, Doc, ScriptCx, Services};
use crate::jobs::JobTable;
use crate::proc::{Proc, SpawnOpts, spawn_process};
use crate::session::actor::TurnWork;
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
    let outcome = run_one_inner(ctx, ready, &reports).await;
    let buffered = std::mem::take(&mut *reports.lock().await);
    SettledCall {
        call: ready.call.clone(),
        outcome,
        reports: buffered,
    }
}

/// Runs one call body, buffering its journal reports.
async fn run_one_inner(
    ctx: &DispatchCtx,
    ready: &ReadyCall,
    reports: &Arc<Mutex<Vec<TurnWork>>>,
) -> SettledOutcome {
    if is_core_tool_search(&ctx.generation, &ctx.tools, &ready.name) {
        reports.lock().await.push(TurnWork::CallStarted {
            turn: ctx.turn,
            call: ready.call.clone(),
        });
        return match tool_search_query(&ready.args) {
            Ok(query) => SettledOutcome::Ok {
                text: tool_search_results(&query, &ctx.deferred_search),
                data: None,
            },
            Err(error) => SettledOutcome::Err {
                text: format!("invalid arguments for tool_search: {error}").into(),
            },
        };
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
    let caller = tool_caller(ctx, ready);
    let runtime = CallRuntime::new(ctx, ready, args.clone(), Arc::clone(reports));
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
    let outcome = tool.run(call, cx).await;
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
    for (index, extension) in ctx.generation.extensions.iter().enumerate() {
        let Ok(ext) = extension.name().parse::<Name>() else {
            continue;
        };
        let caller = Caller::new(
            ext,
            extension.origin(),
            extension.inject(),
            CallerKind::Hook,
            Some(ctx.turn),
        );
        let dispatch = DispatchCx {
            parent: ctx.parent,
            process_env: Arc::clone(&ctx.process_env),
            caller: &caller,
            services: &ctx.services,
            session: ctx.session,
            turn: Some(ctx.turn),
            cancel: &ctx.cancel,
            turn_deadline: ctx.turn_deadline,
            script: ctx.script.clone(),
        };
        let step = dispatch_tool_call(
            extension.name(),
            &dispatch,
            ctx.generation.tool_calls(index),
            event,
            current,
        )
        .await;
        if let Some(reason) = step.block {
            return HookArgs::Blocked { reason };
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
fn tool_caller(ctx: &DispatchCtx, ready: &ReadyCall) -> Caller {
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
    let (ext, origin, inject) = owner
        .and_then(|extension| {
            let name = extension.name().parse::<Name>().ok()?;
            Some((name, extension.origin(), extension.inject()))
        })
        .unwrap_or_else(|| {
            (
                ready.name.clone(),
                dal_core::Origin::Builtin,
                dal_core::ext::ServiceSet::EMPTY,
            )
        });
    Caller::new(ext, origin, inject, CallerKind::Tool, Some(ctx.turn))
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
    /// Turn approval grants.
    ledger: Arc<Mutex<GrantLedger>>,
    /// The authorization proof state: the bound digest once approved.
    auth: Mutex<Option<AuthProof>>,
    /// The pre-acquired process and fd permits for exec spawns.
    permit: Mutex<
        Option<(
            tokio::sync::OwnedSemaphorePermit,
            crate::admission::FdPermit,
        )>,
    >,
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
        args: RawJson,
        reports: Arc<Mutex<Vec<TurnWork>>>,
    ) -> Self {
        Self {
            turn: Some(ctx.turn),
            call: ready.call.clone(),
            tool: ready.name.clone(),
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
            tasks: ctx.backend.tasks().clone(),
            ledger: Arc::clone(&ctx.ledger),
            auth: Mutex::new(None),
            permit: Mutex::new(None),
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
        use dal_core::DenyReason;
        if call != &self.call {
            return Err(DenyReason::OutOfScope {
                what: format!("call {}", call.as_str()).into(),
            });
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
            dal_core::Decision::Deny { reason } => Err(reason),
            dal_core::Decision::Ask { grant } => self.ask(call, preview, grant, cancel).await,
            _ => Err(dal_core::DenyReason::OutOfScope {
                what: self.tool.as_str().into(),
            }),
        }
    }

    /// Looks up the tool and classifies the final arguments for the ladder.
    fn approval_class(&self) -> Result<ToolClass, dal_core::DenyReason> {
        use dal_core::DenyReason;
        let Some((tool, _)) = self.tools.tool(&self.generation, &self.tool) else {
            return Err(DenyReason::OutOfScope {
                what: self.tool.as_str().into(),
            });
        };
        tool.classify(&self.args, &self.workspace)
            .map_err(|_| DenyReason::OutOfScope {
                what: self.tool.as_str().into(),
            })
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
            *self.permit.lock().await = Some((permit, fd_permit));
        }
        *self.auth.lock().await = Some(AuthProof { digest });
        Ok(Approved::new(self.call.clone(), digest, prefix, roots, job))
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
            return Err(DenyReason::NoFrontEnd);
        };
        let question = Question::Approval {
            tool: self.tool.as_str().into(),
            preview: preview.clone(),
            grant: grant.clone().map(|spec| dal_core::CallGrant {
                argv_prefix: spec.argv_prefix,
                roots: spec.roots,
                until: JobEnd(JobId::new_v7()),
            }),
        };
        let secs = default_timeout(&question).as_secs();
        let deadline = tokio::time::Instant::now() + default_timeout(&question);
        let owner = tool_owner(&self.generation, &self.tools, &self.tool);
        let (request, waiter) = self.broker.open(owner, question, turn, deadline);
        self.report_asked(&request).await;
        let answered = tokio::select! {
            biased;
            () = cancel.cancelled() => None,
            () = self.cancel.cancelled() => None,
            outcome = waiter => Some(outcome),
        };
        let Some((answer, by)) = answered else {
            return Err(DenyReason::Unavailable {
                what: "turn cancelled".into(),
            });
        };
        self.report_answered(&request, &answer, &by, false).await;
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
            Answer::Decline => Err(DenyReason::OutOfScope {
                what: if by.as_str() == "core" {
                    format!(
                        "Permission denied {} needed approval and no one answered within {secs} s.",
                        self.tool.as_str()
                    )
                    .into()
                } else {
                    format!(
                        "Permission denied: {} was declined by {}.",
                        self.tool.as_str(),
                        by.as_str()
                    )
                    .into()
                },
            }),
            Answer::Cancel => Err(DenyReason::Unavailable {
                what: "approval cancelled".into(),
            }),
            _ => Err(DenyReason::OutOfScope {
                what: format!("Permission denied: {}.", self.tool.as_str()).into(),
            }),
        }
    }

    /// Publishes one opened approval for journaling.
    ///
    /// `Asked` goes straight to the actor: the ask blocks this call until
    /// an answer arrives, so a deferred buffer would publish the request
    /// only after the answer it is supposed to enable.
    async fn report_asked(&self, request: &Request) {
        self.shared
            .publish(dal_core::UpdateKind::RequestOpened(request.clone()));
        let _ = self
            .handle
            .work(TurnWork::Asked {
                request: request.clone(),
            })
            .await;
    }

    /// Buffers one broker resolution for journaling.
    async fn report_answered(
        &self,
        request: &Request,
        answer: &Answer,
        by: &ClientId,
        was_default: bool,
    ) {
        self.reports.lock().await.push(TurnWork::Answered {
            resolved: Resolved {
                request: request.clone(),
                answer: answer.clone(),
                by: by.clone(),
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

impl ToolCxRuntime for CallRuntime {
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

    fn spawn(
        &self,
        argv: &[std::ffi::OsString],
        opts: SpawnOpts,
        approved: Approved,
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
            return Err(ToolError::Denied(dal_core::DenyReason::OutOfScope {
                what: self.tool.as_str().into(),
            }));
        }
        let permit = match self.permit.try_lock() {
            Ok(mut guard) => guard.take(),
            Err(_) => None,
        };
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
/// root. An empty prefix never matches here; one-shot approvals carry none.
pub(crate) fn grant_covers(
    approved: &Approved,
    argv: &[std::ffi::OsString],
    cwd: &std::path::Path,
) -> bool {
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

/// Runs one seeded nested call through the checked tool path (R03 R04).
pub(crate) async fn direct_call_seeded(backend: &Backend, seed: NestedCall) -> ToolOutcome {
    use dal_core::{ApprovalMode, DenyReason};
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
    let Some((tool, _)) = generation.tool(&name) else {
        return ToolOutcome::Err(crate::error::ToolError::message(format!(
            "unknown tool {}.",
            name.as_str()
        )));
    };
    let answering = backend.answerer_context();
    let (broker, shared, attached) = answering
        .map(|(broker, shared)| (broker, shared, true))
        .unwrap_or_else(|| {
            (
                broker.unwrap_or_else(|| Arc::clone(backend.broker())),
                shared.unwrap_or_else(|| Arc::clone(backend.shared())),
                attached.unwrap_or_else(|| backend.shared().attached()),
            )
        });
    let approved_cell = caller.cell_approved();
    let tool = Arc::clone(tool);
    let reports = Arc::new(Mutex::new(Vec::new()));
    let runtime = CallRuntime {
        turn,
        call: call.clone(),
        tool: name.clone(),
        args: args.clone(),
        policy: Policy {
            mode: if approved_cell {
                ApprovalMode::All
            } else {
                ApprovalMode::Ask
            },
            answerer_attached: attached,
            allow_always: std::collections::BTreeSet::new(),
        },
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
        permit: Mutex::new(None),
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
    let outcome = tool.run(ToolCall::new(call.as_str(), args), cx).await;
    for report in std::mem::take(&mut *reports.lock().await) {
        let _ = backend.handle().work(report).await;
    }
    outcome
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
}
