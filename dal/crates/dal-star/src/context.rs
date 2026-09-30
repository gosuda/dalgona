//! The runtime context one invocation exposes to Starlark (§R02, §R03).
//!
//! A [`Frame`] is the invocation's execution state: the minted
//! [`Invocation`], the host seam, the captured catalog, and the validated
//! plugin descriptor for export resolution. `Evaluator::extra` borrows it for
//! the duration of one evaluation, so adapters can prove a call came from
//! the invocation it claims.
//!
//! [`ContextValue`] is the `ctx` value scripts hold. It is a projection
//! only: it owns no scheduler, grant store, or capability beyond
//! `frame.host`. Facade groups mint [`OpAdapter`](crate::adapter::OpAdapter)
//! values on lookup, so unknown attributes fail as ordinary Starlark
//! attribute errors.
#![expect(unsafe_code, reason = "starlark value derives")]

use std::fmt;
use std::sync::Arc;

use dal_agent::ext::script::{Invocation, InvocationId, ScriptCx, ScriptHost};
use dal_core::Budget;
use dal_core::ext::{ExportId, ExportKind, NativeOp, OnError, OpId, Phase, ScopeSpec};
use starlark::any::ProvidesStaticType;
use starlark::environment::{Methods, MethodsBuilder};
use starlark::eval::{Arguments, Evaluator};
use starlark::starlark_module;
use starlark::values::{Heap, NoSerialize, StarlarkValue, Trace, Value, ValueLike};

use crate::adapter::{OpAdapter, adapter_of};
use crate::error::api_error;
use crate::scope::{ScopeShared, ScopeValue};
use crate::value;

/// The execution frame shared by every value of one invocation.
///
/// `Evaluator::extra` borrows this during evaluation; adapter invoke checks
/// the frame's invocation id matches its own, so a handle from another
/// invocation is an API error, not a capability leak (§R02).
#[derive(ProvidesStaticType)]
pub(crate) struct Frame {
    /// The minted invocation identity and authority sets.
    pub(crate) inv: Arc<Invocation>,
    /// The host seam every operation crosses.
    pub(crate) host: Arc<dyn ScriptHost>,
    /// The validated plugin, when the entry is an export. Commands resolve
    /// their bound tool descriptor and callable through this owner.
    pub(crate) loaded: Option<Arc<crate::validate::LoadedPlugin>>,
    /// The model runtime context, when this entry is a scripted model.
    pub(crate) script: Option<ScriptCx>,
    /// The tokio runtime the synchronous call path blocks on.
    pub(crate) runtime: tokio::runtime::Handle,
}

/// A `ctx` value: one invocation's runtime context (§R03).
///
/// Every field is host-owned and therefore `unsafe_ignore`d for tracing:
/// nothing inside a `Context` holds a Starlark value. The captured config is
/// materialized into the caller's heap on access; it was decoded under the
/// §R10 bounds, so materialization cannot fail.
#[derive(ProvidesStaticType, Trace, NoSerialize, allocative::Allocative)]
#[repr(C)]
pub(crate) struct ContextValue {
    /// The owning invocation.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    pub(crate) inv: Arc<Invocation>,
    /// The host seam.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    pub(crate) host: Arc<dyn ScriptHost>,
    /// The validated plugin for export entries.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    pub(crate) loaded: Option<Arc<crate::validate::LoadedPlugin>>,
    /// The model runtime context, when this entry is a scripted model.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    pub(crate) script: Option<ScriptCx>,
    /// The validated config object, materialized on access.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    pub(crate) config: value::Value,
    /// The parent observation cutoff for `adopt`.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    pub(crate) cutoff: Option<u64>,
    /// The tokio runtime the synchronous call path blocks on.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    pub(crate) runtime: tokio::runtime::Handle,
}

starlark::starlark_simple_value!(ContextValue);

impl fmt::Debug for ContextValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ctx")
    }
}

impl fmt::Display for ContextValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ctx(inv={})", self.inv.id().get())
    }
}

impl Clone for ContextValue {
    fn clone(&self) -> Self {
        Self {
            inv: Arc::clone(&self.inv),
            host: Arc::clone(&self.host),
            loaded: self.loaded.clone(),
            script: self.script.clone(),
            config: self.config.clone(),
            cutoff: self.cutoff,
            runtime: self.runtime.clone(),
        }
    }
}

#[starlark::values::starlark_value(type = "ctx")]
impl<'v> StarlarkValue<'v> for ContextValue {
    #[expect(
        clippy::expect_used,
        reason = "config values are decoded within the §R10 depth bounds"
    )]
    fn get_attr(&self, attribute: &str, heap: Heap<'v>) -> Option<Value<'v>> {
        let group = match attribute {
            "tools" => Some(FacadeGroup::Tools),
            "models" => Some(FacadeGroup::Models),
            "net" => Some(FacadeGroup::Net),
            "ask" => Some(FacadeGroup::Ask),
            "state" => Some(FacadeGroup::State),
            "agents" => Some(FacadeGroup::Agents),
            "jobs" => Some(FacadeGroup::Jobs),
            "turn" => Some(FacadeGroup::Turn),
            "env" => Some(FacadeGroup::Env),
            "mcp" => Some(FacadeGroup::Mcp),
            _ => None,
        };
        if let Some(group) = group {
            return Some(facade(
                heap,
                group,
                &self.inv,
                &self.host,
                self.loaded.as_ref(),
                self.script.as_ref(),
                self.runtime.clone(),
                None,
            ));
        }
        if attribute == "config" {
            return Some(
                self.config
                    .clone()
                    .into_starlark(heap)
                    .expect("config was decoded under the §R10 bounds"),
            );
        }
        None
    }

    fn has_attr(&self, attribute: &str, heap: Heap<'v>) -> bool {
        self.get_attr(attribute, heap).is_some()
    }

    fn dir_attr(&self) -> Vec<String> {
        FACADE_NAMES
            .iter()
            .chain(["config"].iter())
            .map(|name| (*name).to_string())
            .collect()
    }

    fn get_methods() -> Option<&'static Methods> {
        Some(CTX_METHODS.methods())
    }
}

starlark::methods_static!(CTX_METHODS = ctx_methods);

/// The facade attribute names, in `dir` order.
const FACADE_NAMES: [&str; 10] = [
    "tools", "models", "net", "ask", "state", "agents", "jobs", "turn", "env", "mcp",
];

/// The facade groups a `ctx` exposes (§R03 catalog table).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FacadeGroup {
    /// `ctx.tools` and eval's `tools` alias.
    Tools,
    /// `ctx.models`.
    Models,
    /// `ctx.net`.
    Net,
    /// `ctx.ask`.
    Ask,
    /// `ctx.state`.
    State,
    /// `ctx.agents`.
    Agents,
    /// `ctx.jobs`.
    Jobs,
    /// `ctx.turn`.
    Turn,
    /// `ctx.env`.
    Env,
    /// `ctx.mcp`.
    Mcp,
}

impl FacadeGroup {
    /// The attribute names this facade serves, each mapping to one
    /// [`NativeOp`]. Unknown names produce an attribute error (§R03).
    pub(crate) fn operation(self, attr: &str) -> Option<NativeOp> {
        let op = match (self, attr) {
            (Self::Tools, "read") => NativeOp::ToolsRead,
            (Self::Tools, "search") => NativeOp::ToolsSearch,
            (Self::Tools, "patch") => NativeOp::ToolsPatch,
            (Self::Tools, "exec") => NativeOp::ToolsExec,
            (Self::Models, "infer") => NativeOp::ModelsInfer,
            (Self::Models, "forward") => NativeOp::ModelsForward,
            (Self::Net, "fetch") => NativeOp::NetFetch,
            (Self::Ask, "confirm") => NativeOp::AskConfirm,
            (Self::Ask, "select") => NativeOp::AskSelect,
            (Self::Ask, "text") => NativeOp::AskText,
            (Self::State, "read") => NativeOp::StateRead,
            (Self::State, "write") => NativeOp::StateWrite,
            (Self::State, "delete") => NativeOp::StateDelete,
            (Self::Agents, "start") => NativeOp::AgentsStart,
            (Self::Agents, "wait") => NativeOp::AgentsWait,
            (Self::Agents, "cancel") => NativeOp::AgentsCancel,
            (Self::Agents, "list") => NativeOp::AgentsList,
            (Self::Jobs, "start") => NativeOp::JobsStart,
            (Self::Jobs, "wait") => NativeOp::JobsWait,
            (Self::Jobs, "cancel") => NativeOp::JobsCancel,
            (Self::Jobs, "list") => NativeOp::JobsList,
            (Self::Jobs, "text") => NativeOp::JobsText,
            (Self::Turn, "cancel") => NativeOp::TurnCancel,
            (Self::Turn, "steer") => NativeOp::TurnSteer,
            (Self::Turn, "wake") => NativeOp::TurnWake,
            (Self::Turn, "is_idle") => NativeOp::TurnIsIdle,
            (Self::Env, "read") => NativeOp::EnvRead,
            (Self::Mcp, "call") => NativeOp::McpCall,
            _ => return None,
        };
        Some(op)
    }

    /// The facade name for diagnostics.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Tools => "tools",
            Self::Models => "models",
            Self::Net => "net",
            Self::Ask => "ask",
            Self::State => "state",
            Self::Agents => "agents",
            Self::Jobs => "jobs",
            Self::Turn => "turn",
            Self::Env => "env",
            Self::Mcp => "mcp",
        }
    }
}

/// A facade value such as `ctx.tools` or `scope.tools` (§R03, §E05).
///
/// Attribute access mints an [`OpAdapter`] for a native operation; indexing
/// `tools["plugin.name"]` mints a composite-export adapter — synchronous
/// `ctx` facades only, since scope adapters cannot schedule exports.
#[derive(ProvidesStaticType, Trace, NoSerialize, allocative::Allocative)]
#[repr(C)]
pub(crate) struct FacadeValue {
    /// The owning invocation id.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    pub(crate) inv: InvocationId,
    /// The facade group.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    pub(crate) group: FacadeGroup,
    /// The owning invocation for calls.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    pub(crate) invocation: Arc<Invocation>,
    /// The host seam.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    pub(crate) host: Arc<dyn ScriptHost>,
    /// The scope these adapters submit into; `None` is synchronous.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    pub(crate) scheduled: Option<Arc<ScopeShared>>,
    /// The loaded plugin for export resolution.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    pub(crate) loaded: Option<Arc<crate::validate::LoadedPlugin>>,
    /// The model runtime context, when this entry is a scripted model.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    pub(crate) script: Option<ScriptCx>,
    /// The tokio runtime the synchronous call path blocks on.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    pub(crate) runtime: tokio::runtime::Handle,
}

starlark::starlark_simple_value!(FacadeValue);

impl fmt::Debug for FacadeValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for FacadeValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.scheduled {
            Some(_) => write!(f, "scope.{}", self.group.name()),
            None => write!(f, "ctx.{}", self.group.name()),
        }
    }
}

#[starlark::values::starlark_value(type = "facade")]
impl<'v> StarlarkValue<'v> for FacadeValue {
    fn get_attr(&self, attribute: &str, heap: Heap<'v>) -> Option<Value<'v>> {
        let op = self.group.operation(attribute)?;
        Some(heap.alloc(self.adapter(OpId::Native(op))))
    }

    fn at(&self, index: Value<'v>, heap: Heap<'v>) -> starlark::Result<Value<'v>> {
        let Some(key) = index.unpack_str() else {
            return Err(api_error(format!(
                "{}: export keys must be strings like `quality.todos`",
                self.group.name()
            )));
        };
        if self.scheduled.is_some() {
            return Err(api_error(format!(
                "scope.{} cannot schedule scripted exports; call it through ctx",
                self.group.name()
            )));
        }
        let op = parse_export_key(self.group, key)?;
        Ok(heap.alloc(self.adapter(op)))
    }
}

impl FacadeValue {
    /// Mints one adapter carrying this facade's scope binding.
    fn adapter(&self, op: OpId) -> OpAdapter {
        OpAdapter {
            inv: self.inv,
            op,
            invocation: Arc::clone(&self.invocation),
            host: Arc::clone(&self.host),
            scheduled: self.scheduled.clone(),
            loaded: self.loaded.clone(),
            script: self.script.clone(),
            runtime: self.runtime.clone(),
        }
    }
}

/// Parses `tools["quality.todos"]` into a composite `OpId::Export`.
///
/// The facade supplies the kind; the key spells `<plugin>.<name>` with no
/// prefix of its own (§R03).
fn parse_export_key(group: FacadeGroup, key: &str) -> starlark::Result<OpId> {
    let plural = group.name();
    let kind = match group {
        FacadeGroup::Tools => ExportKind::Tool,
        FacadeGroup::Models => ExportKind::Model,
        _ => {
            return Err(api_error(format!(
                "{plural}[\"{key}\"]: only tools/models facades serve exports"
            )));
        }
    };
    let mut parts = key.split('.');
    let (Some(plugin), Some(local), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(api_error(format!(
            "{plural}[\"{key}\"]: export ids spell <plugin>.<name>"
        )));
    };
    Ok(OpId::Export(ExportId {
        plugin: dal_core::Name::parse(plugin)
            .map_err(|error| api_error(format!("{plural}[\"{key}\"]: {error}")))?,
        kind,
        local: dal_core::Name::parse(local)
            .map_err(|error| api_error(format!("{plural}[\"{key}\"]: {error}")))?,
    }))
}

/// Allocates one facade value bound to `inv`; `scope` makes it scheduled.
pub(crate) fn facade<'v>(
    heap: Heap<'v>,
    group: FacadeGroup,
    inv: &Arc<Invocation>,
    host: &Arc<dyn ScriptHost>,
    loaded: Option<&Arc<crate::validate::LoadedPlugin>>,
    script: Option<&ScriptCx>,
    runtime: tokio::runtime::Handle,
    scope: Option<Arc<ScopeShared>>,
) -> Value<'v> {
    heap.alloc(FacadeValue {
        inv: inv.id(),
        group,
        invocation: Arc::clone(inv),
        host: Arc::clone(host),
        scheduled: scope,
        loaded: loaded.cloned(),
        script: script.cloned(),
        runtime,
    })
}

/// Reads the [`Frame`] an evaluator carries; adapters require it.
pub(crate) fn frame_of<'a>(eval: &Evaluator<'_, 'a, '_>) -> Option<&'a Frame> {
    eval.extra.and_then(|e| e.downcast_ref::<Frame>())
}

/// Builds a `ctx` value bound to `frame` for a fresh heap.
pub(crate) fn alloc_ctx<'v>(heap: Heap<'v>, frame: &Frame, config: &value::Value) -> Value<'v> {
    heap.alloc(ContextValue {
        inv: Arc::clone(&frame.inv),
        host: Arc::clone(&frame.host),
        loaded: frame.loaded.clone(),
        script: frame.script.clone(),
        config: config.clone(),
        cutoff: frame.inv.cutoff(),
        runtime: frame.runtime.clone(),
    })
}

/// Extracts the [`ContextValue`] behind a `ctx` value, frozen or not.
pub(crate) fn context_of(value: Value<'_>) -> Option<ContextValue> {
    let borrowed = if let Some(frozen) = value.unpack_frozen() {
        ValueLike::downcast_ref::<ContextValue>(frozen.to_value())
    } else {
        ValueLike::downcast_ref::<ContextValue>(value)
    };
    borrowed.cloned()
}

/// The `ctx` methods (§R03 §E03 §E05 §E06 §R07).
#[starlark_module]
fn ctx_methods(builder: &mut MethodsBuilder) {
    /// Reports whether the ceiling permits one operation id (§E03).
    fn can<'v>(#[starlark(this)] this: Value<'v>, op: &str) -> starlark::Result<bool> {
        let Some(ctx) = context_of(this) else {
            return Err(api_error("ctx.can: receiver is not ctx"));
        };
        let Ok(op_id) = OpId::parse(op) else {
            return Ok(false);
        };
        Ok(ctx.inv.allows(&op_id))
    }

    /// Describes one operation or the captured ceiling (§E08).
    fn describe<'v>(
        #[starlark(this)] this: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
        op: Option<String>,
    ) -> starlark::Result<Value<'v>> {
        let Some(ctx) = context_of(this) else {
            return Err(api_error("ctx.describe: receiver is not ctx"));
        };
        let heap = eval.heap();
        match op {
            Some(name) => describe_op(&ctx, &name, heap),
            None => describe_invocation(&ctx, heap),
        }
    }

    /// Invokes one synchronous native adapter and returns a `Result` for
    /// deliberate recovery (§R07).
    fn try_call<'v>(
        #[starlark(this)] this: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
        args: &Arguments<'v, '_>,
    ) -> starlark::Result<Value<'v>> {
        if context_of(this).is_none() {
            return Err(api_error("ctx.try_call: receiver is not ctx"));
        }
        let heap = eval.heap();
        let mut positions = args.positions(heap)?;
        let Some(first) = positions.next() else {
            return Err(api_error("ctx.try_call: pass the operation adapter first"));
        };
        let Some(adapter) = adapter_of(first) else {
            return Err(api_error(
                "ctx.try_call: the first argument must be a host-issued operation adapter",
            ));
        };
        let rest: Vec<Value<'v>> = positions.collect();
        let names = args.names_map()?;
        adapter.try_call(eval, &rest, &names, heap)
    }

    /// Opens a scope with a required concurrency limit in 1..=64 (§E05).
    fn scope<'v>(
        #[starlark(this)] this: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
        limit: i32,
        #[starlark(require = named)] on_error: Option<&str>,
        #[starlark(require = named)] usd: Option<Value<'v>>,
        #[starlark(require = named)] budget: Option<Value<'v>>,
    ) -> starlark::Result<Value<'v>> {
        let Some(ctx) = context_of(this) else {
            return Err(api_error("ctx.scope: receiver is not ctx"));
        };
        let limit = u16::try_from(limit)
            .ok()
            .filter(|limit| *limit >= 1)
            .ok_or_else(|| api_error("ctx.scope: limit must be in 1..=64"))?;
        let on_error = match on_error.unwrap_or("cancel") {
            "cancel" => OnError::Cancel,
            "settle" => OnError::Settle,
            other => {
                return Err(api_error(format!(
                    "ctx.scope: unknown on_error policy `{other}`"
                )));
            }
        };
        let budget = scope_budget(budget, usd)?;
        ScopeValue::open(
            eval.heap(),
            &ctx.inv,
            &ctx.host,
            ctx.loaded.as_ref(),
            ctx.script.as_ref(),
            ctx.runtime.clone(),
            ScopeSpec {
                limit,
                on_error,
                budget,
            },
        )
    }

    /// Adopts an observation reference eligible to the parent consumer (§E06).
    fn adopt<'v>(
        #[starlark(this)] this: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
        reference: &str,
    ) -> starlark::Result<Value<'v>> {
        let Some(ctx) = context_of(this) else {
            return Err(api_error("ctx.adopt: receiver is not ctx"));
        };
        crate::evidence::adopt(eval.heap(), &ctx.inv, &ctx.host, reference)
    }

    /// Wraps an intact host-issued view as an output descriptor (§R06).
    fn show<'v>(
        #[starlark(this)] this: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
        view: Value<'v>,
    ) -> starlark::Result<Value<'v>> {
        if context_of(this).is_none() {
            return Err(api_error("ctx.show: receiver is not ctx"));
        }
        crate::evidence::show(eval.heap(), view)
    }
}

/// Strictly decodes Starlark scope budget fields and the `usd` shorthand.
fn scope_budget(fields: Option<Value<'_>>, usd: Option<Value<'_>>) -> starlark::Result<Budget> {
    let mut budget = Budget::default();
    if let Some(fields) = fields {
        let fields = value::Value::from_starlark(fields)
            .map_err(|error| api_error(format!("ctx.scope: invalid budget: {error}")))?;
        let value::Value::Object(fields) = fields else {
            return Err(api_error("ctx.scope: budget must be an object"));
        };
        for (name, value) in fields.iter() {
            match name.as_ref() {
                "requests" => budget.requests = Some(scope_budget_count(value, name)?),
                "input_tokens" => budget.input_tokens = Some(scope_budget_count(value, name)?),
                "output_tokens" => budget.output_tokens = Some(scope_budget_count(value, name)?),
                "wall" => budget.wall = Some(scope_budget_duration(value)?),
                "usd" => {
                    if usd.is_some() {
                        return Err(api_error(
                            "ctx.scope: usd was supplied both directly and in budget",
                        ));
                    }
                    budget.usd = Some(scope_budget_usd(value)?);
                }
                _ => {
                    return Err(api_error(format!(
                        "ctx.scope: unknown budget field `{name}`"
                    )));
                }
            }
        }
    }
    if let Some(usd) = usd {
        if budget.usd.is_some() {
            return Err(api_error(
                "ctx.scope: usd was supplied both directly and in budget",
            ));
        }
        let usd = value::Value::from_starlark(usd)
            .map_err(|error| api_error(format!("ctx.scope: invalid usd budget: {error}")))?;
        budget.usd = Some(scope_budget_usd(&usd)?);
    }
    Ok(budget)
}

fn scope_budget_count(value: &value::Value, field: &str) -> starlark::Result<u64> {
    let value::Value::Int(value) = value else {
        return Err(api_error(format!(
            "ctx.scope: `{field}` must be an integer"
        )));
    };
    u64::try_from(*value)
        .map_err(|_| api_error(format!("ctx.scope: `{field}` must be non-negative")))
}

fn scope_budget_usd(value: &value::Value) -> starlark::Result<f64> {
    let number = match value {
        value::Value::Int(value) => *value as f64,
        value::Value::Num(value) => *value,
        _ => return Err(api_error("ctx.scope: `usd` must be a number")),
    };
    if !number.is_finite() || number <= 0.0 {
        return Err(api_error("ctx.scope: `usd` must be positive and finite"));
    }
    Ok(number)
}

fn scope_budget_duration(value: &value::Value) -> starlark::Result<std::time::Duration> {
    let seconds = match value {
        value::Value::Int(value) => *value as f64,
        value::Value::Num(value) => *value,
        _ => return Err(api_error("ctx.scope: `wall` must be a number of seconds")),
    };
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err(api_error("ctx.scope: `wall` must be positive and finite"));
    }
    std::time::Duration::try_from_secs_f64(seconds)
        .map_err(|_| api_error("ctx.scope: `wall` is out of range"))
}

/// Builds the zero-argument `ctx.describe()` record: ceiling and phase (§E08).
fn describe_invocation<'v>(ctx: &ContextValue, heap: Heap<'v>) -> starlark::Result<Value<'v>> {
    let ceiling: Vec<Value<'v>> = ctx
        .inv
        .ceiling()
        .iter()
        .map(|op| heap.alloc(op.to_string()))
        .collect();
    crate::record::Record::alloc(
        heap,
        vec![
            ("ceiling".into(), crate::record::Array::alloc(heap, ceiling)),
            ("phase".into(), heap.alloc(phase_name(ctx.inv.phase()))),
        ],
    )
}

/// Builds `ctx.describe(op)` for one operation id (§E08).
fn describe_op<'v>(ctx: &ContextValue, name: &str, heap: Heap<'v>) -> starlark::Result<Value<'v>> {
    let Ok(op) = OpId::parse(name) else {
        return Err(api_error(format!(
            "ctx.describe: unknown operation `{name}`"
        )));
    };
    let kind = match &op {
        OpId::Native(_) => "native",
        OpId::Export(_) => "export",
    };
    crate::record::Record::alloc(
        heap,
        vec![
            ("id".into(), heap.alloc(op.to_string())),
            ("type".into(), heap.alloc(kind)),
            ("allowed".into(), heap.alloc(ctx.inv.allows(&op))),
            ("available".into(), heap.alloc(catalog_available(ctx, &op))),
        ],
    )
}

/// Whether the captured catalog can serve `op` right now.
fn catalog_available(ctx: &ContextValue, op: &OpId) -> bool {
    match op {
        OpId::Native(native) => ctx.host.catalog().native(*native).available,
        OpId::Export(id) => ctx.host.catalog().export(id).is_some(),
    }
}

/// The lowercase phase name for `describe`.
fn phase_name(phase: Phase) -> &'static str {
    match phase {
        Phase::Tool => "tool",
        Phase::Command => "command",
        Phase::Eval => "eval",
        Phase::Model => "model",
        Phase::Hook(_) => "hook",
    }
}
