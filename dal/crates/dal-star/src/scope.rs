//! Invocation-owned scopes and the tasks submitted through them (§E05).
//!
//! `ctx.scope(limit)` mints a [`ScopeValue`]: a facade owner whose scheduled
//! adapters submit work through `host.submit` and return [`TaskValue`]
//! handles. `all` and `settle` seal the scope, await every task through
//! `host.collect`, and answer in submission order; `all` raises the first
//! expected failure while `settle` returns one [`Settled`] per task.
//!
//! A scope is unfrozen host state. Once sealed, minted adapters refuse new
//! submissions, and a dropped scope asks the host to cancel any owned work
//! the cell never collected.
#![expect(unsafe_code, reason = "starlark value derives")]

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use dal_agent::ext::script::{
    CancelTarget, Collect, FailureCode, HostTerminal, Invocation, InvocationId, OpFailure,
    OpOutcome, ScopeId, ScriptCx, ScriptHost, TaskId,
};
use dal_core::{
    ModelRoute, ScopeSpec,
    ext::{NativeOp, OpId},
};
use starlark::any::ProvidesStaticType;
use starlark::environment::{Methods, MethodsBuilder};
use starlark::eval::Evaluator;
use starlark::starlark_module;
use starlark::starlark_simple_value;
use starlark::values::{Heap, NoSerialize, StarlarkValue, Trace, Value, ValueLike};

use crate::adapter::OpAdapter;
use crate::context::{FacadeDeps, FacadeGroup, facade, frame_of};
use crate::error::api_error;
use crate::outcome::{boundary_failure, project_outcome, settled_outcome, terminal_error};

/// The shared scope state adapters minted through a scope facade check.
///
/// `sealed` flips when `all`, `settle`, or drop closes the scope; a closed
/// scope refuses new submissions as a script API error (§E05).
#[derive(Debug)]
pub(crate) struct ScopeShared {
    /// The host-minted scope identity.
    pub(crate) id: ScopeId,
    /// Whether the scope still accepts submissions.
    sealed: AtomicBool,
    /// The model route scheduled per task, so collectors can project the
    /// typed model result the scripted handler expects.
    pub(crate) model_routes: Mutex<HashMap<TaskId, ModelRoute>>,
}

impl ScopeShared {
    /// True while the scope accepts new tasks.
    pub(crate) fn is_open(&self) -> bool {
        !self.sealed.load(Ordering::Acquire)
    }

    /// Seals the scope; closing only removes submission authority.
    fn seal(&self) {
        self.sealed.store(true, Ordering::Release);
    }
}

/// A `scope` value: the owner of one invocation's scheduled tasks (§E05).
///
/// `s.tools`, `s.models`, and `s.net` mint schedulable facades; `all` and
/// `settle` collect. Everything else is an ordinary attribute error.
#[derive(ProvidesStaticType, Trace, NoSerialize, allocative::Allocative)]
#[repr(C)]
pub(crate) struct ScopeValue {
    /// The owning invocation identity.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    inv: InvocationId,
    /// The owning invocation for collect and cancel.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    invocation: Arc<Invocation>,
    /// The host seam.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    host: Arc<dyn ScriptHost>,
    /// The shared scope state.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    shared: Arc<ScopeShared>,
    /// The loaded plugin; scope facades never resolve exports, kept for the
    /// facade constructor shape.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    loaded: Option<Arc<crate::validate::LoadedPlugin>>,
    /// The model runtime context, when this scope belongs to a scripted model.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    script: Option<ScriptCx>,
    /// The tokio runtime the collect path blocks on.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    runtime: tokio::runtime::Handle,
}

starlark_simple_value!(ScopeValue);

impl fmt::Debug for ScopeValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for ScopeValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "scope({})", self.shared.id.get())
    }
}

#[starlark::values::starlark_value(type = "scope")]
impl<'v> StarlarkValue<'v> for ScopeValue {
    fn get_attr(&self, attribute: &str, heap: Heap<'v>) -> Option<Value<'v>> {
        let group = match attribute {
            "tools" => FacadeGroup::Tools,
            "models" => FacadeGroup::Models,
            "net" => FacadeGroup::Net,
            _ => return None,
        };
        Some(facade(
            heap,
            group,
            &FacadeDeps {
                invocation: &self.invocation,
                host: &self.host,
                loaded: self.loaded.as_ref(),
                script: self.script.as_ref(),
                runtime: &self.runtime,
                scope: Some(&self.shared),
            },
        ))
    }

    fn get_methods() -> Option<&'static Methods> {
        Some(SCOPE_METHODS.methods())
    }
}

starlark::methods_static!(SCOPE_METHODS = scope_methods);

impl Drop for ScopeValue {
    fn drop(&mut self) {
        if self.shared.is_open() {
            self.shared.seal();
            self.host
                .cancel(&self.invocation, CancelTarget::Scope(self.shared.id));
        }
    }
}

impl ScopeValue {
    /// Opens a scope under `inv` with `limit` concurrent operations (§E05).
    ///
    /// The limit is required and in 1..=64; the host enforces the live-scope
    /// cap.
    pub(crate) fn open<'v>(
        heap: Heap<'v>,
        inv: &Arc<Invocation>,
        host: &Arc<dyn ScriptHost>,
        loaded: Option<&Arc<crate::validate::LoadedPlugin>>,
        script: Option<&ScriptCx>,
        runtime: tokio::runtime::Handle,
        spec: ScopeSpec,
    ) -> starlark::Result<Value<'v>> {
        let id = host.open_scope(inv, spec).map_err(terminal_error)?;
        Ok(heap.alloc(Self {
            inv: inv.id(),
            invocation: Arc::clone(inv),
            host: Arc::clone(host),
            shared: Arc::new(ScopeShared {
                id,
                sealed: AtomicBool::new(false),
                model_routes: Mutex::new(HashMap::new()),
            }),
            loaded: loaded.cloned(),
            script: script.cloned(),
            runtime,
        }))
    }

    /// Schedules one model inference directly through the scope's model context.
    fn infer<'v>(
        &self,
        eval: &Evaluator<'v, '_, '_>,
        request: Value<'v>,
    ) -> starlark::Result<Value<'v>> {
        check_frame(eval, self.inv, "scope.infer")?;
        let adapter = OpAdapter {
            inv: self.inv,
            op: OpId::Native(NativeOp::ModelsInfer),
            invocation: Arc::clone(&self.invocation),
            host: Arc::clone(&self.host),
            scheduled: Some(Arc::clone(&self.shared)),
            loaded: self.loaded.clone(),
            script: self.script.clone(),
            runtime: self.runtime.clone(),
        };
        adapter.schedule_model_request(request, eval.heap())
    }

    /// Verifies the caller frame owns this scope, then seals and collects.
    fn collect_all(
        &self,
        eval: &Evaluator<'_, '_, '_>,
    ) -> starlark::Result<Vec<(TaskId, OpOutcome)>> {
        check_frame(eval, self.inv, "scope")?;
        self.shared.seal();
        let host = Arc::clone(&self.host);
        let invocation = Arc::clone(&self.invocation);
        let collect = Collect::Seal(self.shared.id);
        let task = self
            .runtime
            .spawn(async move { host.collect(&invocation, collect).await });
        let collected = self
            .runtime
            .block_on(task)
            .map_err(|error| collect_join_failure(&error))?
            .map_err(terminal_error)?;
        Ok(collected.into_vec())
    }
}

/// Maps a failed collect task: a host panic is an indeterminate failure the
/// script can see, and only a cancelled task reads as cancellation.
fn collect_join_failure(error: &tokio::task::JoinError) -> starlark::Error {
    if error.is_panic() {
        return boundary_failure(OpFailure {
            code: FailureCode::Indeterminate,
            message: "the host panicked while collecting scope results".into(),
            details: None,
        });
    }
    terminal_error(HostTerminal::Cancelled)
}

#[starlark_module]
fn scope_methods(builder: &mut MethodsBuilder) {
    /// Schedules one typed model inference through this scope.
    fn infer<'v>(
        #[starlark(this)] this: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
        request: Value<'v>,
    ) -> starlark::Result<Value<'v>> {
        let Some(scope) = scope_of(this) else {
            return Err(api_error("scope.infer: receiver is not a scope"));
        };
        scope.infer(eval, request)
    }

    /// Seals the scope, awaits every task, and returns the successful values
    /// in submission order; the first expected failure raises (§E05).
    fn all<'v>(
        #[starlark(this)] this: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let Some(scope) = scope_of(this) else {
            return Err(api_error("scope.all: receiver is not a scope"));
        };
        let heap = eval.heap();
        let collected = scope.collect_all(eval)?;
        let mut values = Vec::with_capacity(collected.len());
        for (task, outcome) in collected {
            let route = scope
                .shared
                .model_routes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&task);
            let outcome = crate::adapter::wrap_model_outcome(outcome, route)?;
            values.push(project_outcome(outcome, &scope.invocation, heap)?);
        }
        Ok(crate::record::Array::alloc(heap, values))
    }

    /// Seals the scope, awaits every task, and returns one `Result` per task
    /// in submission order for deliberate recovery (§E05).
    fn settle<'v>(
        #[starlark(this)] this: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let Some(scope) = scope_of(this) else {
            return Err(api_error("scope.settle: receiver is not a scope"));
        };
        let heap = eval.heap();
        let collected = scope.collect_all(eval)?;
        let mut results = Vec::with_capacity(collected.len());
        for (task, outcome) in collected {
            let route = scope
                .shared
                .model_routes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&task);
            let outcome = crate::adapter::wrap_model_outcome(outcome, route)?;
            results.push(settled_outcome(outcome, &scope.invocation, heap)?);
        }
        Ok(crate::record::Array::alloc(heap, results))
    }
}

/// Borrows the [`ScopeValue`] inside `value`.
fn scope_of(value: Value<'_>) -> Option<&ScopeValue> {
    ValueLike::downcast_ref::<ScopeValue>(value)
}

/// A `task` value: one submitted scope operation's handle (§E05).
///
/// `wait` delivers the value or raises the expected failure; `settle`
/// returns the `Result` instead; `cancel` requests cancellation.
#[derive(ProvidesStaticType, Trace, NoSerialize, allocative::Allocative)]
#[repr(C)]
pub(crate) struct TaskValue {
    /// The owning invocation identity.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    inv: InvocationId,
    /// The host-minted task identity.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    task: TaskId,
    /// The owning invocation for collect and cancel.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    invocation: Arc<Invocation>,
    /// The host seam.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    host: Arc<dyn ScriptHost>,
    /// The shared scope state holding the task's scheduled model route.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    shared: Arc<ScopeShared>,
    /// The tokio runtime the collect path blocks on.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    runtime: tokio::runtime::Handle,
}

starlark_simple_value!(TaskValue);

impl fmt::Debug for TaskValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for TaskValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "task({})", self.task.get())
    }
}

#[starlark::values::starlark_value(type = "task")]
#[expect(
    clippy::elidable_lifetime_names,
    reason = "StarlarkValue requires a lifetime-generic implementation"
)]
impl<'v> StarlarkValue<'v> for TaskValue {
    fn get_methods() -> Option<&'static Methods> {
        Some(TASK_METHODS.methods())
    }
}

starlark::methods_static!(TASK_METHODS = task_methods);

impl TaskValue {
    /// Allocates a handle for one submitted task.
    pub(crate) fn new(
        invocation: &Arc<Invocation>,
        task: TaskId,
        host: &Arc<dyn ScriptHost>,
        shared: &Arc<ScopeShared>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            inv: invocation.id(),
            task,
            invocation: Arc::clone(invocation),
            host: Arc::clone(host),
            shared: Arc::clone(shared),
            runtime,
        }
    }

    /// Awaits this task's recorded outcome once.
    fn collect_one(&self, eval: &Evaluator<'_, '_, '_>, what: &str) -> starlark::Result<OpOutcome> {
        check_frame(eval, self.inv, what)?;
        let host = Arc::clone(&self.host);
        let invocation = Arc::clone(&self.invocation);
        let collect = Collect::Task(self.task);
        let task = self
            .runtime
            .spawn(async move { host.collect(&invocation, collect).await });
        let collected = self
            .runtime
            .block_on(task)
            .map_err(|error| collect_join_failure(&error))?
            .map_err(terminal_error)?;
        collected
            .into_vec()
            .into_iter()
            .next()
            .map(|(_, outcome)| outcome)
            .ok_or_else(|| api_error(format!("{what}: the host returned no outcome")))
    }
}

#[starlark_module]
fn task_methods(builder: &mut MethodsBuilder) {
    /// Delivers the task's successful value; an expected failure raises (§E05).
    fn wait<'v>(
        #[starlark(this)] this: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let Some(task) = task_of(this) else {
            return Err(api_error("task.wait: receiver is not a task"));
        };
        let outcome = task.collect_one(eval, "task.wait")?;
        let model_route = task
            .shared
            .model_routes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&task.task);
        let outcome = crate::adapter::wrap_model_outcome(outcome, model_route)?;
        project_outcome(outcome, &task.invocation, eval.heap())
    }

    /// Delivers the task's `Result` for deliberate recovery (§E05).
    fn settle<'v>(
        #[starlark(this)] this: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let Some(task) = task_of(this) else {
            return Err(api_error("task.settle: receiver is not a task"));
        };
        let outcome = task.collect_one(eval, "task.settle")?;
        let model_route = task
            .shared
            .model_routes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&task.task);
        let outcome = crate::adapter::wrap_model_outcome(outcome, model_route)?;
        settled_outcome(outcome, &task.invocation, eval.heap())
    }

    /// Requests cancellation; idempotent (§E05).
    fn cancel<'v>(
        #[starlark(this)] this: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let Some(task) = task_of(this) else {
            return Err(api_error("task.cancel: receiver is not a task"));
        };
        check_frame(eval, task.inv, "task.cancel")?;
        task.host
            .cancel(&task.invocation, CancelTarget::Task(task.task));
        Ok(Value::new_none())
    }
}

/// Borrows the [`TaskValue`] inside `value`.
fn task_of(value: Value<'_>) -> Option<&TaskValue> {
    ValueLike::downcast_ref::<TaskValue>(value)
}

/// Proves the method runs inside the invocation it claims (§R02).
fn check_frame(
    eval: &Evaluator<'_, '_, '_>,
    inv: InvocationId,
    what: &str,
) -> starlark::Result<()> {
    let Some(frame) = frame_of(eval) else {
        return Err(api_error(format!("{what}: no live invocation")));
    };
    if frame.inv.id() != inv {
        return Err(api_error(format!(
            "{what}: handle belongs to another invocation"
        )));
    }
    Ok(())
}
