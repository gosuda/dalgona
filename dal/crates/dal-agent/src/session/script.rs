//! The per-session script host: the one [`ScriptHost`] implementation
//! (R02 R03 R04 R07 R09 E01 E05 E06 E07).
//!
//! One [`SessionScriptHost`] serves one session behind the host trait. It
//! composes the existing owners instead of replacing them: operations run
//! through the same checked tool dispatch a model call uses, scopes run on
//! the invocation-owned [`ScopeTable`], interpreter workers come from the
//! host-wide [`Interpreters`] pool, and evidence flows through the
//! generation's [`Evidence`] owner. Minting is the only authority door:
//! [`ScriptHost::begin`] fixes `U`, `D`, the phase, the consumer, the
//! deadline, and cancellation before any script runs (R02 R04).

use std::collections::{BTreeSet, HashMap};
use std::sync::{
    Arc, Mutex as StdMutex, OnceLock, RwLock as StdRwLock, Weak,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use std::time::Duration;

use dal_core::ext::{
    Consumer, ExportId, ExportKind, OpId, OpSet, Phase, ReadView, ToolData, ViewNode,
};
use dal_core::{
    CallId, DenyReason, ModelRequest, Name, Origin, RawJson, ScopeSpec, ServiceSet, SessionId,
    TurnId,
};
use tokio::sync::Mutex;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::admission::Interpreters;
use crate::error::ServiceError;
use crate::ext::generation::{
    Generation,
    catalog::{Catalog, wire_name},
};
use crate::ext::script::{
    CancelTarget, Cleanup, Collect, Collected, EffectStatus, Entry, EvalEnvironment, FailureCode,
    HostTerminal, Invocation, InvocationId, InvocationParts, ObserverError, OpFailure, OpOutcome,
    OpRecord, OpRequest, OpValue, Parent, ScopeId, ScriptCx, ScriptHost, Submit, TaskId,
};
use crate::ext::{BoxFuture, Caller, CallerKind};
use crate::session::backend::Backend;
use crate::session::tasks::ScopeTable;

#[cfg(test)]
mod tests;

/// The wall deadline minted into every root invocation (R10).
const SCRIPT_WALL: Duration = Duration::from_secs(60);

/// The bounded cleanup drain before unresolved calls are reported (E07 R09).
const CLEANUP_GRACE: Duration = Duration::from_secs(5);

/// The recorded observer-failure cap per root (R07 P05).
const MAX_OBSERVER_ERRORS: usize = 64;

/// The generation-bound half of one capture: environment plus its catalog.
#[derive(Clone)]
struct Captured {
    /// The host-captured eval environment (E01).
    env: Arc<EvalEnvironment>,
    /// The generation the environment was captured against (R02).
    generation: Arc<Generation>,
    /// The turn whose inference may request script services.
    turn: Option<TurnId>,
    /// The turn cancellation bounding cells of this capture (R09).
    turn_cancel: Option<CancellationToken>,
}

/// One invocation's scope executor state (E05).
#[derive(Clone, Default)]
struct Runner {
    /// In-flight scope tasks, driven by the runtime so collectors only await.
    inflight: Arc<tokio::sync::Mutex<tokio::task::JoinSet<ScopedResult>>>,
    /// Tasks handed to the executor whose native outcome has not landed.
    pending: Arc<tokio::sync::Mutex<BTreeSet<TaskId>>>,
}

/// One completed scope task: its id, operation, outcome, and encoded size.
type ScopedResult = (TaskId, OpId, OpOutcome, usize);

/// The session script host (R02 R03 R04 R07 E05 E06 E07).
pub(crate) struct SessionScriptHost {
    /// The owning session.
    session: SessionId,
    /// The session data-plane the nested calls run through.
    backend: Arc<Backend>,
    /// The host-wide interpreter worker pool (R09).
    interpreters: Arc<Interpreters>,
    /// The latest capture, or the session-start capture before one exists.
    captured: StdRwLock<Captured>,
    /// The session-start catalog behind [`ScriptHost::catalog`] (R03).
    catalog: Catalog,
    /// Per-invocation scope tables (E05).
    tables: StdMutex<HashMap<InvocationId, ScopeTable>>,
    /// Per-invocation scope executors.
    runners: Mutex<HashMap<InvocationId, Runner>>,
    /// Per-root recorded observer failures (R07 P05).
    observers: StdMutex<HashMap<InvocationId, Vec<ObserverError>>>,
    /// The host-wide retained-result budget (E05 R10).
    host_bytes: Arc<AtomicUsize>,
    /// The session runtime that owns spawned model work (E05 R09).
    runtime: tokio::runtime::Handle,
    /// The nested-call id counter.
    next_call: AtomicU64,
    /// The `Arc` self-reference that arms [`ScriptCx::host`].
    host_cell: OnceLock<Weak<SessionScriptHost>>,
}

/// How a cancelled native op maps onto the outcome split (R07 E05).
#[derive(Clone, Copy)]
enum CancelScope {
    /// The root was cancelled: the terminal outcome.
    Root,
    /// The owner cancelled one task: the recoverable failure.
    Owned,
}

impl SessionScriptHost {
    /// Mints one host bound to its own `Arc`, against `generation` (R10).
    ///
    /// The session script surface is minted per boundary — turn, hook
    /// chain, command run — so the catalog behind [`ScriptHost::catalog`]
    /// and the generation authority resolution use always share one
    /// identity (R02).
    pub(crate) fn for_generation(
        session: SessionId,
        backend: &Arc<Backend>,
        interpreters: Arc<Interpreters>,
        generation: Arc<Generation>,
    ) -> Arc<Self> {
        let host = Arc::new(Self::new(
            session,
            Arc::clone(backend),
            interpreters,
            generation,
        ));
        host.bind();
        host
    }

    /// Builds the host for one session against the session-start generation.
    pub(crate) fn new(
        session: SessionId,
        backend: Arc<Backend>,
        interpreters: Arc<Interpreters>,
        generation: Arc<Generation>,
    ) -> Self {
        let catalog = generation.catalog().clone();
        #[expect(
            clippy::expect_used,
            reason = "the empty authority set can never exceed the capture ceiling"
        )]
        let empty = EvalEnvironment::capture(generation.id, OpSet::EMPTY, None, [0; 32])
            .expect("empty authority set captures");
        let captured = Captured {
            env: Arc::new(empty),
            generation,
            turn: None,
            turn_cancel: None,
        };
        Self {
            session,
            backend,
            interpreters,
            captured: StdRwLock::new(captured),
            catalog,
            tables: StdMutex::new(HashMap::new()),
            runners: Mutex::new(HashMap::new()),
            observers: StdMutex::new(HashMap::new()),
            host_bytes: Arc::new(AtomicUsize::new(0)),
            runtime: tokio::runtime::Handle::current(),
            next_call: AtomicU64::new(1),
            host_cell: OnceLock::new(),
        }
    }

    /// Arms the host's self-reference; call once right after `Arc::new`.
    pub(crate) fn bind(self: &Arc<Self>) {
        let _ = self.host_cell.set(Arc::downgrade(self));
    }

    /// Captures the eval environment of one decision request (E01) and
    /// returns it for the batch's [`ScriptCx`].
    ///
    /// `None` means the capture was refused (an oversized authority set);
    /// the batch then runs without a script seam, which fails closed.
    pub(crate) fn capture(
        &self,
        generation: Arc<Generation>,
        allowed: OpSet,
        cutoff: Option<u64>,
        fingerprint: [u8; 32],
        turn: Option<TurnId>,
        turn_cancel: CancellationToken,
    ) -> Option<Arc<EvalEnvironment>> {
        let env =
            Arc::new(EvalEnvironment::capture(generation.id, allowed, cutoff, fingerprint).ok()?);
        let mut captured = self
            .captured
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *captured = Captured {
            env: Arc::clone(&env),
            generation,
            turn,
            turn_cancel: Some(turn_cancel),
        };
        Some(env)
    }

    /// Builds the script context one call attaches, with `parent` as the
    /// enclosing invocation (E06).
    pub(crate) fn attach(&self, parent: Option<Arc<Invocation>>) -> Option<ScriptCx> {
        let host: Arc<dyn ScriptHost> = self.me()?;
        let captured = self.snapshot();
        Some(ScriptCx::new(host, captured.env, parent))
    }

    /// Records one observer-hook failure against the invocation's root (R07).
    pub(crate) fn record_observer(&self, inv: &Arc<Invocation>, error: ObserverError) {
        let mut observers = self
            .observers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = observers.entry(inv.root()).or_default();
        if root.len() < MAX_OBSERVER_ERRORS {
            root.push(error);
        }
    }

    /// Returns the fingerprint of the policy one capture freezes (E01).
    pub(crate) fn policy_fingerprint(policy: &dal_core::Policy, allowed: &OpSet) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"dal-eval-env-v1");
        hasher.update(approval_spelling(policy.mode).as_bytes());
        hasher.update(&[0][..]);
        for name in &policy.allow_always {
            hasher.update(name.as_str().as_bytes());
            hasher.update(&[0][..]);
        }
        for op in allowed.iter() {
            hasher.update(op.to_string().as_bytes());
            hasher.update(&[0][..]);
        }
        *hasher.finalize().as_bytes()
    }

    /// Returns the captured generation snapshot.
    fn snapshot(&self) -> Captured {
        self.captured
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Returns the turn whose inference may run scripted services (E05).
    pub(crate) fn captured_turn(&self) -> Option<TurnId> {
        self.snapshot().turn
    }

    /// Returns the live generation for authority resolution (R02).
    fn generation(&self) -> Arc<Generation> {
        self.snapshot().generation
    }

    /// Returns the `Arc` self-reference, absent once the session is gone.
    fn me(&self) -> Option<Arc<SessionScriptHost>> {
        self.host_cell.get().and_then(Weak::upgrade)
    }

    /// Mints one nested-call identity (R02).
    fn mint_call(&self) -> CallId {
        let n = self.next_call.fetch_add(1, Ordering::Relaxed);
        CallId::new(format!("script-{n}"))
    }

    /// Admits one interpreter worker for the entry's phase (R09).
    fn admit(&self, phase: Phase) -> Result<crate::ext::script::WorkerPermit, HostTerminal> {
        let permit = match phase {
            Phase::Hook(_) => self.interpreters.callback(),
            _ => self.interpreters.root(),
        };
        permit.map_err(|_| HostTerminal::LimitExceeded {
            what: "interpreter workers",
        })
    }

    /// Builds the caller attribution of one export entry (R02).
    fn export_caller(generation: &Generation, id: &ExportId, turn: Option<TurnId>) -> Caller {
        let extension = generation
            .extensions
            .iter()
            .find(|ext| ext.name() == id.plugin.as_str());
        let origin = extension.map_or(Origin::User, crate::ext::Extension::origin);
        let inject = extension.map_or(ServiceSet::EMPTY, crate::ext::Extension::inject);
        Caller::new(id.plugin.clone(), origin, inject, CallerKind::Handler, turn)
    }

    /// The catalog declaration of one export, denied when unknown (R03).
    fn export_spec(
        generation: &Generation,
        id: &ExportId,
    ) -> Result<crate::ext::generation::catalog::ExportSpec, HostTerminal> {
        generation
            .catalog()
            .export(id)
            .cloned()
            .ok_or_else(|| HostTerminal::Denied {
                reason: DenyReason::OutOfScope {
                    what: OpId::Export(id.clone()).to_string().into(),
                },
            })
    }

    fn declared_uses(generation: &Generation, id: &ExportId) -> Result<OpSet, HostTerminal> {
        if id.kind == ExportKind::Model {
            return generation
                .catalog()
                .model_export(id)
                .map(|model| model.uses.clone())
                .ok_or_else(|| HostTerminal::Denied {
                    reason: DenyReason::OutOfScope {
                        what: OpId::Export(id.clone()).to_string().into(),
                    },
                });
        }
        Self::export_spec(generation, id).map(|spec| spec.uses)
    }

    /// Mints the invocation for one admitted entry (R02 R04 E01).
    fn begin_inner(
        &self,
        parent: Parent<'_>,
        entry: Entry,
    ) -> Result<Arc<Invocation>, HostTerminal> {
        let captured = self.snapshot();
        let parts = match parent {
            Parent::Of(inv) => match entry {
                Entry::Eval { .. } => return Err(HostTerminal::NestedEval),
                Entry::Export { id, phase } => {
                    if inv.export().is_some() {
                        return Err(HostTerminal::NestedScriptExport);
                    }
                    let uses = Self::declared_uses(&captured.generation, &id)?;
                    let missing = ops_outside(&uses, inv.ceiling())?;
                    if !missing.is_empty() {
                        return Err(HostTerminal::IncompleteScope { missing });
                    }
                    InvocationParts {
                        parent: Some(Arc::clone(inv)),
                        session: self.session,
                        generation: captured.generation.id,
                        caller: Self::export_caller(&captured.generation, &id, captured.turn),
                        export: Some(id),
                        phase,
                        consumer: None,
                        deadline: Instant::now() + SCRIPT_WALL,
                        ceiling: uses.clone(),
                        declared: uses.clone(),
                        cutoff: inv.cutoff(),
                        cancel: CancellationToken::new(),
                        permit: Some(self.admit(phase)?),
                    }
                }
            },
            Parent::Root => {
                let (ceiling, phase, export, inject) = match entry {
                    Entry::Eval { uses } => {
                        let ceiling = captured.env.resolve(uses.as_ref())?;
                        let inject = ceiling.services();
                        (ceiling, Phase::Eval, None, inject)
                    }
                    Entry::Export { id, phase } => {
                        let uses = Self::declared_uses(&captured.generation, &id)?;
                        (uses, phase, Some(id), ServiceSet::EMPTY)
                    }
                };
                let caller = match &export {
                    Some(id) => Self::export_caller(&captured.generation, id, captured.turn),
                    None => Caller::new(
                        Name::parse("eval")
                            .map_err(|_| HostTerminal::LimitExceeded { what: "identities" })?,
                        Origin::Builtin,
                        inject,
                        CallerKind::Cell { approved: true },
                        captured.turn,
                    ),
                };
                InvocationParts {
                    parent: None,
                    session: self.session,
                    generation: captured.generation.id,
                    caller,
                    export,
                    phase,
                    consumer: None,
                    deadline: Instant::now() + SCRIPT_WALL,
                    ceiling: ceiling.clone(),
                    declared: ceiling,
                    cutoff: captured.env.cutoff(),
                    cancel: captured
                        .turn_cancel
                        .as_ref()
                        .map_or_else(CancellationToken::new, CancellationToken::child_token),
                    permit: Some(self.admit(phase)?),
                }
            }
        };
        let inv = Invocation::mint(parts)?;
        let mut tables = self
            .tables
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let shared = Arc::clone(&self.backend.host_state().shared);
        let price: crate::ext::scope::PriceFn =
            Arc::new(move |route| crate::ext::synthetic::price_of(&shared, route));
        tables.insert(
            inv.id(),
            ScopeTable::new(Arc::clone(&self.host_bytes), price),
        );
        Ok(inv)
    }

    /// Runs one synchronous operation through the checked dispatch path (R03 R04 R07).
    async fn call_inner(self: Arc<Self>, inv: &Arc<Invocation>, req: OpRequest) -> OpOutcome {
        if let Some(terminal) = sealed_gate(inv) {
            return OpOutcome::Terminal(terminal);
        }
        if !inv.allows(&req.op) {
            return OpOutcome::Terminal(HostTerminal::Denied {
                reason: DenyReason::OutOfScope {
                    what: req.op.to_string().into(),
                },
            });
        }
        let generation = self.generation();
        if let OpId::Export(id) = &req.op {
            let Ok(spec) = Self::export_spec(&generation, id) else {
                return OpOutcome::Terminal(HostTerminal::Denied {
                    reason: DenyReason::OutOfScope {
                        what: OpId::Export(id.clone()).to_string().into(),
                    },
                });
            };
            let missing = ops_outside(&spec.uses, inv.ceiling()).unwrap_or_default();
            if !missing.is_empty() {
                return OpOutcome::Terminal(HostTerminal::IncompleteScope { missing });
            }
        }
        let call = self.mint_call();
        self.run_op(inv, req, inv.cutoff(), call, inv.cancel().clone())
            .await
            .1
    }

    /// Resolves and runs one operation, returning its operation, outcome,
    /// and encoded size.
    ///
    /// The issued-operation charge lands here, once per started operation
    /// (R10); an operation without a backend fails with `unavailable` and
    /// charges nothing because it never starts (R03).
    async fn run_op(
        self: &Arc<Self>,
        inv: &Arc<Invocation>,
        req: OpRequest,
        cutoff: Option<u64>,
        call: CallId,
        cancel: CancellationToken,
    ) -> (OpId, OpOutcome, usize) {
        let op = req.op.clone();
        if matches!(
            &op,
            OpId::Native(
                dal_core::ext::NativeOp::ModelsInfer | dal_core::ext::NativeOp::ModelsForward
            )
        ) {
            return self.run_model_op(inv, req, call, cancel).await;
        }
        let generation = self.generation();
        let name = match &op {
            OpId::Native(native) => generation.catalog().native_tool_name(*native),
            OpId::Export(id) => generation
                .catalog()
                .export(id)
                .and_then(|_| Name::parse(&wire_name(id)).ok()),
        };
        let Some(name) = name else {
            let message = format!("operation {op} has no backend in this generation");
            let outcome = unavailable(&call, &op, &message);
            return (op, outcome, 0);
        };
        if inv.issue().is_err() {
            let terminal = OpOutcome::Terminal(HostTerminal::LimitExceeded {
                what: "issued operations",
            });
            return (op, terminal, 0);
        }
        // The caller is the plugin identity of the calling invocation for
        // native ops, and the owning plugin for an export entry: the
        // plugin's own grant applies either way (R04).
        let caller = match &op {
            OpId::Export(id) => Self::export_caller(&generation, id, inv.caller().turn()),
            OpId::Native(_) => inv.caller().clone(),
        };
        let script = self.attach(Some(Arc::clone(inv)));
        let seed = crate::session::dispatch::NestedCall {
            name,
            args: req.args,
            call: call.clone(),
            caller,
            cancel,
            cutoff,
            script,
            turn: inv.caller().turn().or_else(|| self.captured_turn()),
            broker: None,
            shared: None,
            attached: None,
        };
        let outcome = crate::session::dispatch::direct_call_seeded(&self.backend, seed).await;
        let scope = if inv.cancel().is_cancelled() {
            CancelScope::Root
        } else {
            CancelScope::Owned
        };
        let (outcome, bytes) = convert_outcome(outcome, &call, &op, scope);
        (op, outcome, bytes)
    }

    /// Validates a model operation and decodes its request payload.
    fn model_op_request(
        req: &OpRequest,
    ) -> Option<(
        OpId,
        bool,
        Arc<dyn crate::ext::ModelCxRuntime>,
        ModelRequest,
    )> {
        let op = req.op.clone();
        let is_infer = matches!(&op, OpId::Native(dal_core::ext::NativeOp::ModelsInfer));
        let is_forward = matches!(&op, OpId::Native(dal_core::ext::NativeOp::ModelsForward));
        if !is_infer && !is_forward {
            return None;
        }
        let runtime = req.model_runtime.clone()?;
        let request = req.args.decode_as::<ModelRequest>().ok()?;
        Some((op, is_infer, runtime, request))
    }

    /// Rejects a model operation that failed validation.
    fn model_op_rejected(req: &OpRequest, call: &CallId) -> (OpId, OpOutcome, usize) {
        let op = req.op.clone();
        let is_infer = matches!(&op, OpId::Native(dal_core::ext::NativeOp::ModelsInfer));
        let is_forward = matches!(&op, OpId::Native(dal_core::ext::NativeOp::ModelsForward));
        if !is_infer && !is_forward {
            return (
                op.clone(),
                unavailable(call, &op, "unsupported model operation"),
                0,
            );
        }
        if req.model_runtime.is_none() {
            return (
                op.clone(),
                unavailable(call, &op, "model operation has no model runtime context"),
                0,
            );
        }
        if let Err(error) = req.args.decode_as::<ModelRequest>() {
            return (
                op.clone(),
                failed_operation(call, &op, error.to_string().into()),
                0,
            );
        }
        (
            op,
            unavailable(call, &req.op, "model operation failed validation"),
            0,
        )
    }

    async fn run_model_op(
        self: &Arc<Self>,
        inv: &Arc<Invocation>,
        req: OpRequest,
        call: CallId,
        cancel: CancellationToken,
    ) -> (OpId, OpOutcome, usize) {
        let Some((op, is_infer, runtime, request)) = Self::model_op_request(&req) else {
            return Self::model_op_rejected(&req, &call);
        };
        if inv.issue().is_err() {
            return (
                op,
                OpOutcome::Terminal(HostTerminal::LimitExceeded {
                    what: "issued operations",
                }),
                0,
            );
        }
        let private_tools = req.private_tools;
        let caller = inv.caller().clone();
        let script = Arc::clone(self);
        let runtime_cancel = cancel.clone();
        // The model op runs on the session runtime rather than the calling
        // interpreter thread, so a scope task never stalls the turn driver
        // while the Starlark evaluator blocks on this call (E05 R09).
        let output = self.runtime.spawn(async move {
            let inference = if is_infer {
                runtime
                    .infer(&caller, request, script, runtime_cancel)
                    .await?
            } else {
                let stream = runtime
                    .forward(request, &private_tools)
                    .await
                    .map_err(|error| ServiceError::failed(None, error.to_string()))?;
                crate::ext::synthetic::collect(stream)
                    .await
                    .map_err(|error| ServiceError::failed(None, error.to_string()))?
            };
            let encoded = sonic_rs::to_string(&inference)
                .map_err(|error| ServiceError::failed(None, error.to_string()))?;
            RawJson::parse(&encoded).map_err(|error| ServiceError::failed(None, error.to_string()))
        });
        let output = async {
            match output.await {
                Ok(value) => value,
                Err(error) => Err(ServiceError::failed(None, error.to_string())),
            }
        };
        let value = tokio::select! {
            biased;
            () = inv.cancel().cancelled() => {
                return (op, OpOutcome::Terminal(HostTerminal::Cancelled), 0);
            }
            () = cancel.cancelled() => {
                return (op.clone(), owner_cancelled(&call, &op), 0);
            }
            value = output => value,
        };
        let value = match value {
            Ok(value) => value,
            Err(ServiceError::Denied(reason)) => {
                return (op, OpOutcome::Terminal(HostTerminal::Denied { reason }), 0);
            }
            Err(ServiceError::Cancelled) => {
                return (op, OpOutcome::Terminal(HostTerminal::Cancelled), 0);
            }
            Err(error) => {
                return (
                    op.clone(),
                    failed_operation(&call, &op, error.to_string().into()),
                    0,
                );
            }
        };
        let bytes = value.as_str().len();
        (
            op.clone(),
            OpOutcome::Ok {
                value: OpValue::Json(value),
                record: OpRecord {
                    call,
                    op,
                    status: EffectStatus::Completed,
                },
            },
            bytes,
        )
    }

    /// Issues runnable scope tasks to the executor, honouring scope bounds (E05).
    async fn pump(self: &Arc<Self>, inv: &Arc<Invocation>) {
        let issued = {
            let mut tables = self
                .tables
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match tables.get_mut(&inv.id()) {
                Some(table) => table.runnable(),
                None => return,
            }
        };
        if issued.is_empty() {
            return;
        }
        let calls: Vec<CallId> = {
            let tables = self
                .tables
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            issued
                .iter()
                .map(|(task, ..)| {
                    tables
                        .get(&inv.id())
                        .and_then(|table| table.call_id(*task))
                        .unwrap_or_else(|| self.mint_call())
                })
                .collect()
        };
        let mut runners = self.runners.lock().await;
        let runner = runners.entry(inv.id()).or_default();
        for ((task, req, cutoff, cancel), call) in issued.into_iter().zip(calls) {
            runner.pending.lock().await.insert(task);
            let this = Arc::clone(self);
            let inv = Arc::clone(inv);
            runner.inflight.lock().await.spawn(async move {
                let (op, outcome, bytes) = this.run_op(&inv, req, cutoff, call, cancel).await;
                (task, op, outcome, bytes)
            });
        }
    }

    /// Reports whether the invocation still has queued or running work (E05).
    async fn has_inflight(&self, inv: &Arc<Invocation>) -> bool {
        let pending = {
            let runners = self.runners.lock().await;
            runners.get(&inv.id()).map(|runner| Arc::clone(&runner.pending))
        };
        let Some(pending) = pending else {
            return false;
        };
        !pending.lock().await.is_empty()
    }

    /// Waits for one in-flight completion and records it; `false` when the
    /// invocation has nothing in flight (E05).
    async fn wait_one(&self, inv: &Arc<Invocation>) -> bool {
        let inflight = {
            let runners = self.runners.lock().await;
            runners
                .get(&inv.id())
                .map(|runner| Arc::clone(&runner.inflight))
        };
        let Some(inflight) = inflight else {
            return false;
        };
        let landing = inflight.lock().await.join_next().await;
        let Some(Ok((task, op, outcome, bytes))) = landing else {
            return false;
        };
        let runners = self.runners.lock().await;
        if let Some(runner) = runners.get(&inv.id()) {
            runner.pending.lock().await.remove(&task);
        }
        drop(runners);
        let mut tables = self
            .tables
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(table) = tables.get_mut(&inv.id())
            && let Err(_) = table.complete(task, outcome, bytes)
        {
            // The retained-result budget refused the result (E05 R10): the
            // effect stands in the ledger under its call id; the task
            // settles with a reference-sized failure.
            let call = table
                .call_id(task)
                .unwrap_or_else(|| CallId::new("script-unknown"));
            let _ = table.complete(
                task,
                OpOutcome::Failed {
                    failure: OpFailure {
                        code: FailureCode::Failed,
                        message: format!(
                            "result of {op} exceeds the retained-result budget; the effect is recorded under {}",
                            call.as_str()
                        )
                        .into(),
                        details: None,
                    },
                    record: OpRecord {
                        call,
                        op,
                        status: EffectStatus::Completed,
                    },
                },
                0,
            );
        }
        true
    }

    /// Seals a scope or names one task, then awaits the members in
    /// submission order (E05).
    async fn collect_inner(
        self: Arc<Self>,
        inv: &Arc<Invocation>,
        which: Collect,
    ) -> Result<Collected, HostTerminal> {
        let members: Vec<TaskId> = match which {
            Collect::Task(task) => vec![task],
            Collect::Seal(scope) => {
                let mut tables = self
                    .tables
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let table = tables.get_mut(&inv.id()).ok_or_else(unknown_invocation)?;
                table.seal(scope).map_err(scope_refused)?.into_vec()
            }
        };
        {
            let tables = self
                .tables
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(table) = tables.get(&inv.id()) else {
                return Err(unknown_invocation());
            };
            if members.iter().any(|task| table.call_id(*task).is_none()) {
                return Err(HostTerminal::Denied {
                    reason: DenyReason::OutOfScope {
                        what: "task of another invocation".into(),
                    },
                });
            }
        }
        loop {
            if let Some(terminal) = sealed_gate(inv) {
                return Err(terminal);
            }
            self.pump(inv).await;
            let (settled, scope_deadline) = {
                let tables = self
                    .tables
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let Some(table) = tables.get(&inv.id()) else {
                    return Err(unknown_invocation());
                };
                (
                    members.iter().all(|task| table.settled(*task)),
                    table.next_deadline(),
                )
            };
            if settled {
                let mut tables = self
                    .tables
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let table = tables.get_mut(&inv.id()).ok_or_else(unknown_invocation)?;
                let mut collected = Vec::with_capacity(members.len());
                for task in members {
                    let outcome = table.deliver(task).ok_or_else(unknown_invocation)?;
                    if let OpOutcome::Terminal(terminal) = &outcome {
                        return Err(terminal.clone());
                    }
                    collected.push((task, outcome));
                }
                return Ok(collected.into());
            }
            tokio::select! {
                biased;
                () = inv.cancel().cancelled() => return Err(HostTerminal::Cancelled),
                () = tokio::time::sleep_until(inv.deadline()) => return Err(HostTerminal::Cancelled),
                () = async {
                    match scope_deadline {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending::<()>().await,
                    }
                } => {
                    let mut tables = self
                        .tables
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if let Some(table) = tables.get_mut(&inv.id()) {
                        table.expire();
                    }
                }
                landed = self.wait_one(inv) => {
                    if !landed && !self.has_inflight(inv).await {
                        // Nothing in flight and nothing queued: the member
                        // set is inconsistent; refuse rather than spin.
                        return Err(unknown_invocation());
                    }
                }
            }
        }
    }

    /// Cancels owned scopes, drains in-flight work under a bounded grace,
    /// and reports cleanup (E07).
    async fn finish_inner(self: Arc<Self>, inv: &Arc<Invocation>) -> Cleanup {
        // The root's effect gate guards the whole cell's authority, so only
        // a root finish closes it; a child shares that gate and must leave
        // it open for the parent's remaining operations (R04 E07).
        if inv.parent().is_none() {
            inv.gate().close();
        }
        inv.cancel().cancel();
        let mut child_errors = Vec::new();
        let observer = {
            let mut observers = self
                .observers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match inv.parent() {
                None => observers.remove(&inv.root()).unwrap_or_default(),
                // A child reports its slice without emptying the root's
                // accumulated list, which the root's own finish reports (E07).
                Some(_) => {
                    let root = observers.entry(inv.root()).or_default();
                    let root_list = &mut *root;
                    let child_start = root_list
                        .iter()
                        .rposition(|error| child_errors.contains(error))
                        .map_or(0, |position| position + 1);
                    let reported = root_list.drain(child_start..).collect::<Vec<_>>();
                    child_errors.extend(reported.iter().cloned());
                    reported
                }
            }
        };
        let remaining = inv.deadline().saturating_duration_since(Instant::now());
        let drain = async { while self.wait_one(inv).await {} };
        let _ = tokio::time::timeout(remaining.min(CLEANUP_GRACE), drain).await;
        let pending = {
            let mut runners = self.runners.lock().await;
            match runners.remove(&inv.id()) {
                Some(runner) => {
                    let mut pending = runner.pending.lock().await;
                    std::mem::take(&mut *pending)
                }
                None => BTreeSet::new(),
            }
        };
        let mut tables = self
            .tables
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(mut table) = tables.remove(&inv.id()) else {
            return Cleanup {
                complete: true,
                outstanding: Box::new([]),
                observer_errors: observer.into(),
            };
        };
        let outstanding: Box<[CallId]> = pending
            .iter()
            .filter_map(|task| table.call_id(*task))
            .collect();
        let _ = table.cleanup();
        Cleanup {
            complete: outstanding.is_empty(),
            outstanding,
            observer_errors: observer.into(),
        }
    }
}

/// The gate and cancellation state of an invocation ready to act (R07 R09).
fn sealed_gate(inv: &Invocation) -> Option<HostTerminal> {
    if !inv.gate().is_open() || inv.cancel().is_cancelled() {
        return Some(HostTerminal::Cancelled);
    }
    if Instant::now() >= inv.deadline() {
        return Some(HostTerminal::Cancelled);
    }
    None
}

/// The operations of `set` missing from `ceiling` (R04).
fn ops_outside(set: &OpSet, ceiling: &OpSet) -> Result<OpSet, HostTerminal> {
    let missing: Vec<String> = set
        .iter()
        .filter(|op| !ceiling.contains(op))
        .map(|op| op.to_string())
        .collect();
    OpSet::parse(missing.iter().map(String::as_str)).map_err(|_| HostTerminal::LimitExceeded {
        what: "operation ids",
    })
}

/// The refusal for a scope handle outside the calling invocation (E05).
fn scope_refused(_: crate::session::tasks::ScopeError) -> HostTerminal {
    HostTerminal::Denied {
        reason: DenyReason::OutOfScope {
            what: "scope".into(),
        },
    }
}

/// The refusal for an invocation that holds no script state here.
fn unknown_invocation() -> HostTerminal {
    HostTerminal::Denied {
        reason: DenyReason::OutOfScope {
            what: "invocation".into(),
        },
    }
}

/// Builds the `unavailable` failure of a backendless operation (R03).
fn unavailable(call: &CallId, op: &OpId, message: &str) -> OpOutcome {
    OpOutcome::Failed {
        failure: OpFailure {
            code: FailureCode::Unavailable,
            message: Box::from(message),
            details: None,
        },
        record: OpRecord {
            call: call.clone(),
            op: op.clone(),
            status: EffectStatus::Failed,
        },
    }
}

fn failed_operation(call: &CallId, op: &OpId, message: Box<str>) -> OpOutcome {
    OpOutcome::Failed {
        failure: OpFailure {
            code: FailureCode::Failed,
            message,
            details: None,
        },
        record: OpRecord {
            call: call.clone(),
            op: op.clone(),
            status: EffectStatus::Failed,
        },
    }
}

fn owner_cancelled(call: &CallId, op: &OpId) -> OpOutcome {
    OpOutcome::Failed {
        failure: OpFailure {
            code: FailureCode::Cancelled,
            message: "the task was cancelled".into(),
            details: None,
        },
        record: OpRecord {
            call: call.clone(),
            op: op.clone(),
            status: EffectStatus::Cancelled,
        },
    }
}

/// Encodes the byte size of one typed view for retention accounting (E05).
fn data_bytes(data: &ToolData) -> usize {
    match data {
        ToolData::Read(view) => {
            view.path.len()
                + view.header.len()
                + view
                    .rows
                    .iter()
                    .map(|row| row.text.len() + 8)
                    .sum::<usize>()
        }
        ToolData::Search(page) => page
            .matches
            .iter()
            .map(|hit| {
                data_bytes(&ToolData::Read(hit.source.clone())) + hit.text.len() + hit.path.len()
            })
            .sum(),
        ToolData::Find(page) => page
            .entries
            .iter()
            .map(|entry| entry.path.len() + entry.kind.len())
            .sum(),
        ToolData::Symbols(page) => page
            .hits
            .iter()
            .map(|hit| hit.path.len() + hit.name.len() + hit.kind.len() + 16)
            .sum(),
        ToolData::Views(views) => views
            .iter()
            .map(|view| data_bytes(&ToolData::Read(view.clone())))
            .sum(),
        ToolData::Display(node) => display_bytes(node),
    }
}

/// Encodes the byte size of one display node (E05).
fn display_bytes(node: &ViewNode) -> usize {
    match node {
        ViewNode::Text(text) => text.len(),
        ViewNode::Table { columns, rows } => {
            columns
                .iter()
                .map(|column| column.as_ref().len())
                .sum::<usize>()
                + rows
                    .iter()
                    .map(|row| row.iter().map(|cell| cell.as_str().len()).sum::<usize>())
                    .sum::<usize>()
        }
        ViewNode::Source(view) => data_bytes(&ToolData::Read(view.clone())),
        ViewNode::Group(nodes) => nodes.iter().map(display_bytes).sum(),
    }
}

/// Wraps `text` as the JSON string value of a plain tool result (R05).
fn json_text(text: &str) -> RawJson {
    let encoded = sonic_rs::to_string(text);
    match encoded {
        Ok(encoded) =>
        {
            #[expect(
                clippy::expect_used,
                reason = "sonic-rs encodes any &str as a valid JSON string"
            )]
            RawJson::parse(&encoded).expect("encoded text is a JSON string")
        }
        Err(_) => RawJson::null(),
    }
}

/// Converts one terminal tool outcome into its operation outcome (R07).
///
/// Denial and root cancellation stay terminal; a task cancelled by its
/// owner is the recoverable `cancelled` failure; every other tool failure
/// is the recoverable `failed` failure with the tool's text.
fn convert_outcome(
    outcome: crate::ext::ToolOutcome,
    call: &CallId,
    op: &OpId,
    scope: CancelScope,
) -> (OpOutcome, usize) {
    let record = |status: EffectStatus| OpRecord {
        call: call.clone(),
        op: op.clone(),
        status,
    };
    let cancelled = |scope: CancelScope| match scope {
        CancelScope::Root => OpOutcome::Terminal(HostTerminal::Cancelled),
        CancelScope::Owned => OpOutcome::Failed {
            failure: OpFailure {
                code: FailureCode::Cancelled,
                message: "the task was cancelled".into(),
                details: None,
            },
            record: record(EffectStatus::Cancelled),
        },
    };
    match outcome {
        crate::ext::ToolOutcome::Ok(output) => {
            let bytes = output.to_string().len() + output.data.as_ref().map_or(0, data_bytes);
            let value = match output.data {
                Some(data) => OpValue::Data(data),
                None => OpValue::Json(json_text(&output.to_string())),
            };
            (
                OpOutcome::Ok {
                    value,
                    record: record(EffectStatus::Completed),
                },
                bytes,
            )
        }
        crate::ext::ToolOutcome::Detached(job) => (
            OpOutcome::Ok {
                value: OpValue::Detached(job),
                record: record(EffectStatus::Unresolved),
            },
            job.to_string().len(),
        ),
        crate::ext::ToolOutcome::Interrupted
        | crate::ext::ToolOutcome::Err(crate::error::ToolError::Cancelled) => (cancelled(scope), 0),
        crate::ext::ToolOutcome::Err(crate::error::ToolError::Denied(reason)) => {
            (OpOutcome::Terminal(HostTerminal::Denied { reason }), 0)
        }
        crate::ext::ToolOutcome::Err(error) => (
            OpOutcome::Failed {
                failure: OpFailure {
                    code: FailureCode::Failed,
                    message: error.to_string().into(),
                    details: None,
                },
                record: record(EffectStatus::Failed),
            },
            0,
        ),
    }
}

impl ScriptHost for SessionScriptHost {
    fn begin(&self, parent: Parent<'_>, entry: Entry) -> Result<Arc<Invocation>, HostTerminal> {
        self.begin_inner(parent, entry)
    }

    fn call(&self, inv: &Arc<Invocation>, req: OpRequest) -> BoxFuture<'static, OpOutcome> {
        let Some(this) = self.me() else {
            return Box::pin(async { OpOutcome::Terminal(HostTerminal::Cancelled) });
        };
        let inv = Arc::clone(inv);
        Box::pin(async move { this.call_inner(&inv, req).await })
    }

    fn open_scope(&self, inv: &Arc<Invocation>, spec: ScopeSpec) -> Result<ScopeId, HostTerminal> {
        if let Some(terminal) = sealed_gate(inv) {
            return Err(terminal);
        }
        let mut tables = self
            .tables
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let table = tables.get_mut(&inv.id()).ok_or_else(unknown_invocation)?;
        table.open(spec, inv.cancel().child_token())
    }

    fn submit(&self, inv: &Arc<Invocation>, scope: ScopeId, req: OpRequest) -> Submit {
        if let Some(terminal) = sealed_gate(inv) {
            return Submit::Terminal(terminal);
        }
        if !inv.allows(&req.op) {
            return Submit::Terminal(HostTerminal::Denied {
                reason: DenyReason::OutOfScope {
                    what: req.op.to_string().into(),
                },
            });
        }
        let mut tables = self
            .tables
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(table) = tables.get_mut(&inv.id()) else {
            return Submit::Terminal(unknown_invocation());
        };
        table
            .submit(scope, req, inv.cutoff())
            .unwrap_or_else(|error| Submit::Terminal(scope_refused(error)))
    }

    fn collect(
        &self,
        inv: &Arc<Invocation>,
        which: Collect,
    ) -> BoxFuture<'static, Result<Collected, HostTerminal>> {
        let Some(this) = self.me() else {
            return Box::pin(async { Err(HostTerminal::Cancelled) });
        };
        let inv = Arc::clone(inv);
        Box::pin(async move { this.collect_inner(&inv, which).await })
    }

    fn cancel(&self, inv: &Arc<Invocation>, target: CancelTarget) {
        let mut tables = self
            .tables
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(table) = tables.get_mut(&inv.id()) {
            let _ = table.cancel(target);
        }
    }

    fn adopt(&self, inv: &Arc<Invocation>, reference: &str) -> Result<ReadView, OpFailure> {
        let unavailable = || OpFailure {
            code: FailureCode::ObservationUnavailable,
            message: "the referenced read is not available to this invocation; read the file again"
                .into(),
            details: None,
        };
        let generation = self.generation();
        let Some(evidence) = generation.evidence() else {
            return Err(unavailable());
        };
        let parent_cutoff = inv.cutoff().unwrap_or_default();
        evidence
            .adopt(
                inv.session(),
                reference,
                Consumer::Model,
                parent_cutoff,
                inv.consumer(),
            )
            .map_err(|_| unavailable())
    }

    fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    fn finish(&self, inv: &Arc<Invocation>) -> BoxFuture<'static, Cleanup> {
        let Some(this) = self.me() else {
            return Box::pin(async { Cleanup::default() });
        };
        let inv = Arc::clone(inv);
        Box::pin(async move { this.finish_inner(&inv).await })
    }

    fn observe_failed(&self, inv: &Arc<Invocation>, error: ObserverError) {
        self.record_observer(inv, error);
    }
}

/// The stable approval-mode spelling inside the policy fingerprint (E01).
fn approval_spelling(mode: dal_core::ApprovalMode) -> &'static str {
    match mode {
        dal_core::ApprovalMode::Ask => "ask",
        dal_core::ApprovalMode::Edits => "edits",
        dal_core::ApprovalMode::All => "all",
    }
}
