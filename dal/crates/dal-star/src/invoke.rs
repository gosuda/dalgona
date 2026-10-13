//! One synchronous Starlark evaluation inside an invocation (§R09 §E07).
//!
//! The evaluator runs on a blocking worker because every operation a script
//! issues may block on `host.call`. The worker installs the [`Frame`] in
//! `Evaluator::extra`, captures `print` into a bounded buffer, applies the
//! cell limits, and reports a classified [`InvokeFailure`] so each caller
//! (tools, commands, hooks, eval) maps exactly one terminal outcome.
//!
//! The returned future is `Send` and owns every input; the worker is one of
//! the host's reserved interpreter threads.

use std::fmt;
use std::sync::Mutex;

use std::sync::Arc;
use std::time::Duration;

use dal_agent::ext::script::{
    Cleanup, Entry, FailureCode, HostTerminal, OpFailure, Parent, ScriptCx,
};
use dal_core::RawJson;
use dal_core::ext::{ExportId, OpId, Phase, ViewNode};
use starlark::PrintHandler;
use starlark::environment::Module;
use starlark::eval::Evaluator;
use starlark::syntax::AstModule;
use starlark::values::{FrozenValue, Heap, Value};
use tokio::time::Instant;

use crate::context::{Frame, alloc_ctx};
use crate::descriptor::{DomainErr, DomainOk, OutputValue};
use crate::engine::{CELL_WALL, Limits, globals};
use crate::error::{ApiError, api_error};
use crate::evidence::view_node_from;
use crate::hooks::{Event, EventValue, Minted, minted_of};
use crate::outcome::{script_failure_of, terminal_of};
use crate::schema::Schema;
use crate::validate::LoadedPlugin;
use crate::value;

/// What one evaluation produced.
pub(crate) struct InvokeOutput {
    /// The return value in transport form.
    pub(crate) value: value::Value,
    /// The `dal.output` display tree, if one was returned.
    pub(crate) view: Option<ViewNode>,
    /// The hook verdict, if one was returned.
    pub(crate) verdict: Option<Minted>,
    /// The intact host views the result tree carries (§R06).
    pub(crate) views: Box<[dal_core::ext::ReadView]>,
    /// Captured `print` lines in source order.
    pub(crate) prints: Box<[Box<str>]>,
    /// Whether the print buffer hit the §R10 cap and truncated.
    pub(crate) prints_truncated: bool,
}

/// The single terminal classification a caller maps (§R07 §E07).
#[derive(Debug)]
pub(crate) enum InvokeFailure {
    /// An expected operation failure: a failed native call or a scripted
    /// `dal.err`. Recorded as the invocation's failed outcome.
    Script {
        /// The operation, if the failure names one.
        op: Option<OpId>,
        /// The recoverable failure record.
        failure: OpFailure,
    },
    /// A terminal host outcome propagated unchanged.
    Terminal(HostTerminal),
    /// Script API misuse: foreign handles, wrong argument shapes, bad calls.
    Api(Box<str>),
    /// A language or output-serialization failure.
    Eval(Box<str>),
    /// Cancellation arrived before the call settled.
    Cancelled,
    /// The wall deadline passed before the call settled.
    Timeout,
    /// The worker thread failed to run to completion.
    Worker,
}

impl fmt::Display for InvokeFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Script {
                op: Some(op),
                failure,
            } => write!(f, "{op}: {}: {}", failure.code.as_str(), failure.message),
            Self::Script { op: None, failure } => {
                write!(f, "{}: {}", failure.code.as_str(), failure.message)
            }
            Self::Terminal(terminal) => write!(f, "{terminal}"),
            Self::Api(message) | Self::Eval(message) => f.write_str(message),
            Self::Cancelled => f.write_str("cancelled"),
            Self::Timeout => {
                f.write_str("plugin handler timed out; narrow the request or raise the limit")
            }
            Self::Worker => f.write_str("worker failed"),
        }
    }
}

/// A bounded `print` sink (§R10): at most `MAX_PRINTS` bytes, then one
/// truncation marker.
struct PrintCollector {
    /// Captured lines.
    lines: Vec<Box<str>>,
    /// Bytes captured so far.
    bytes: usize,
    /// Whether output was truncated.
    truncated: bool,
}

/// The `PrintHandler` wrapper starlark requires (the trait cannot be
/// implemented on `Mutex<T>` directly).
struct PrintSink(Mutex<PrintCollector>);

impl PrintHandler for PrintSink {
    fn println(&self, text: &str) -> starlark::Result<()> {
        let mut guard = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.bytes + text.len() > crate::engine::MAX_PRINTS {
            if !guard.truncated {
                guard.truncated = true;
                guard.lines.push("… prints truncated at 64 KiB".into());
            }
            return Ok(());
        }
        guard.bytes += text.len();
        guard.lines.push(text.into());
        Ok(())
    }
}

/// Applies the §R10 evaluation limits to one evaluator.
fn apply_limits(evaluator: &mut Evaluator<'_, '_, '_>, limits: &Limits) -> starlark::Result<()> {
    let heap = usize::try_from(limits.heap_bytes).unwrap_or(usize::MAX);
    let depth = usize::try_from(limits.stack_depth).unwrap_or(usize::MAX);
    evaluator.set_max_tick_count(limits.ticks)?;
    evaluator.set_max_heap_size(heap)?;
    evaluator.set_max_callstack_size(depth)?;
    Ok(())
}

/// Classifies a raised `starlark::Error` into the caller-facing failure.
fn classify(error: &starlark::Error, cancelled: bool) -> InvokeFailure {
    if cancelled {
        return InvokeFailure::Cancelled;
    }
    if let Some(failure) = script_failure_of(error) {
        return InvokeFailure::Script {
            op: failure.op.clone(),
            failure: failure.failure.clone(),
        };
    }
    if let Some(terminal) = terminal_of(error) {
        return InvokeFailure::Terminal(terminal.clone());
    }
    if let starlark::ErrorKind::Other(source) = error.kind()
        && let Some(api) = source.downcast_ref::<ApiError>()
    {
        return InvokeFailure::Api(api.0.to_string().into());
    }
    InvokeFailure::Eval(error.to_string().into())
}

/// What one evaluation runs: a frozen handler or a cell module.
#[expect(
    clippy::large_enum_variant,
    reason = "boxing the parsed AST adds a heap allocation solely to shrink this one-shot evaluation state"
)]
enum Body {
    /// `run(ctx, args)` on a frozen callable.
    Run {
        /// The frozen handler.
        callable: FrozenValue,
        /// The handler argument.
        args: Arg,
    },
    /// A cell module that sees `ctx`, `tools`, and `data`.
    Cell {
        /// The parsed cell.
        ast: AstModule,
        /// Caller data in transport form.
        data: value::Value,
    },
}

impl Body {
    /// Evaluates the body with `ctx` bound; the result lives on the module
    /// heap.
    fn evaluate<'v>(
        self,
        module: &Module<'v>,
        evaluator: &mut Evaluator<'v, '_, '_>,
        ctx: Value<'v>,
    ) -> starlark::Result<Value<'v>> {
        let heap = module.heap();
        match self {
            Self::Run { callable, args } => {
                let args = args.alloc(heap)?;
                evaluator.eval_function(callable.to_value(), &[ctx, args], &[])
            }
            Self::Cell { ast, data } => {
                let data = data
                    .into_starlark(heap)
                    .map_err(|error| api_error(format!("data: {error}")))?;
                let tools = ctx.get_attr_error("tools", heap)?;
                module.set("ctx", ctx);
                module.set("tools", tools);
                module.set("data", data);
                evaluator.eval_module(ast, &globals())
            }
        }
    }
}

/// Runs one evaluation on a worker with the frame installed (§R09).
///
/// The worker owns limits, cancellation, the deadline, prints, and
/// classification. Cancellation and the deadline both stop the evaluator,
/// so a timed-out script releases its interpreter thread.
async fn evaluate_on_worker(
    frame: Frame,
    config: value::Value,
    limits: Limits,
    deadline: Instant,
    body: Body,
) -> Result<InvokeOutput, InvokeFailure> {
    let cancel = frame.inv.cancel().clone();
    if cancel.is_cancelled() {
        return Err(InvokeFailure::Cancelled);
    }
    let wall = deadline.into_std();
    let worker = tokio::task::spawn_blocking(move || {
        let cancelled = cancel.clone();
        Module::with_temp_heap(|module| {
            let prints = PrintSink(Mutex::new(PrintCollector {
                lines: Vec::new(),
                bytes: 0,
                truncated: false,
            }));
            let mut evaluator = Evaluator::new(&module);
            evaluator.set_print_handler(&prints);
            evaluator.set_check_cancelled(Box::new(move || {
                cancelled.is_cancelled() || std::time::Instant::now() >= wall
            }));
            evaluator.extra = Some(&frame);
            apply_limits(&mut evaluator, &limits)
                .map_err(|error| InvokeFailure::Eval(error.to_string().into()))?;
            let ctx = alloc_ctx(module.heap(), &frame, &config);
            let result = body.evaluate(&module, &mut evaluator, ctx);
            drop(evaluator);
            if std::time::Instant::now() >= wall && result.is_err() {
                return Err(InvokeFailure::Timeout);
            }
            let collector = prints
                .0
                .into_inner()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let output = match result {
                Err(error) => return Err(classify(&error, cancel.is_cancelled())),
                Ok(output) => output,
            };
            let returned = unwrap_returned(output)?;
            let mut views = Vec::new();
            crate::evidence::collect_views(returned.value, &mut views);
            Ok(InvokeOutput {
                value: encode(returned.value)?,
                view: returned.view,
                verdict: returned.verdict,
                views: views.into_boxed_slice(),
                prints: collector.lines.into_boxed_slice(),
                prints_truncated: collector.truncated,
            })
        })
    });
    let remaining = deadline.saturating_duration_since(Instant::now());
    match tokio::time::timeout(remaining, worker).await {
        Err(_) => Err(InvokeFailure::Timeout),
        Ok(Err(_)) => Err(InvokeFailure::Worker),
        Ok(Ok(outcome)) => outcome,
    }
}

/// Encodes one returned Starlark value into transport form.
fn encode(value: Value<'_>) -> Result<value::Value, InvokeFailure> {
    value::Value::from_starlark(value)
        .map_err(|error| InvokeFailure::Eval(error.to_string().into()))
}

/// Unwraps an explicit domain outcome or presentation exactly once at the
/// invocation boundary (§R07 §P06).
///
/// `dal.ok(v)` yields `v`; `dal.err(...)` becomes the invocation's failed
/// outcome; `dal.output(value, view)` yields the value and its decoded
/// display tree. Any other value is plain data, so a dictionary with
/// `ok = False` stays data.
fn unwrap_returned(value: Value<'_>) -> Result<Returned<'_>, InvokeFailure> {
    if let Some(ok) = DomainOk::from_value(value) {
        return Ok(Returned::data(ok.value));
    }
    if let Some(err) = DomainErr::from_value(value) {
        return Err(domain_failure(err)?);
    }
    if let Some(verdict) = minted_of(value) {
        return Ok(Returned {
            verdict: Some(verdict),
            ..Returned::data(Value::new_none())
        });
    }
    let Some(output) = OutputValue::from_value(value) else {
        return Ok(Returned::data(value));
    };
    let view = view_node_from(output.view)
        .map_err(|error| InvokeFailure::Eval(format!("view: {error}").into()))?;
    Ok(Returned {
        view: Some(view),
        ..Returned::data(output.value)
    })
}

/// A returned value after the boundary unwrap.
struct Returned<'v> {
    /// The semantic value.
    value: Value<'v>,
    /// The display tree from `dal.output`.
    view: Option<ViewNode>,
    /// The hook verdict.
    verdict: Option<Minted>,
}

impl<'v> Returned<'v> {
    /// Plain data with no display tree or verdict.
    fn data(value: Value<'v>) -> Self {
        Self {
            value,
            view: None,
            verdict: None,
        }
    }
}

/// The argument a handler receives after `ctx`.
pub(crate) enum Arg {
    /// Validated data in transport form.
    Data(value::Value),
    /// A hook event.
    Event(Event),
}

impl Arg {
    /// Allocates the argument onto `heap`.
    fn alloc(self, heap: Heap<'_>) -> starlark::Result<Value<'_>> {
        match self {
            Self::Data(data) => data
                .into_starlark(heap)
                .map_err(|error| api_error(format!("arguments: {error}"))),
            Self::Event(event) => Ok(EventValue::alloc(heap, event)),
        }
    }
}

/// Builds the failed outcome a returned `dal.err` records.
fn domain_failure(err: &DomainErr<'_>) -> Result<InvokeFailure, InvokeFailure> {
    let details = if err.details.is_none() {
        None
    } else {
        Some(
            RawJson::parse(&encode(err.details)?.to_json())
                .map_err(|error| InvokeFailure::Eval(format!("details: {error}").into()))?,
        )
    };
    Ok(InvokeFailure::Script {
        op: None,
        failure: OpFailure {
            code: FailureCode::Domain(err.code.as_str().into()),
            message: err.message.as_str().into(),
            details,
        },
    })
}

/// Evaluates one cell module and returns its final expression value (§E02).
///
/// The cell sees `ctx`, `tools` (the same facade as `ctx.tools`), and
/// `data`; no `load` is needed for ordinary work.
pub(crate) async fn cell(
    ast: AstModule,
    frame: Frame,
    config: value::Value,
    data: value::Value,
    limits: Limits,
    deadline: Instant,
) -> Result<InvokeOutput, InvokeFailure> {
    evaluate_on_worker(frame, config, limits, deadline, Body::Cell { ast, data }).await
}

/// One exported handler to enter: its plugin, identity, phase, callable, and
/// phase wall cap.
pub(crate) struct Handler<'a> {
    /// The validated plugin that owns the callable and its config.
    pub(crate) plugin: &'a Arc<LoadedPlugin>,
    /// The entry identity the host admits.
    pub(crate) id: &'a ExportId,
    /// The phase the handler runs in.
    pub(crate) phase: Phase,
    /// The frozen `run` callable.
    pub(crate) run: FrozenValue,
    /// The phase wall cap; the invocation deadline may be earlier.
    pub(crate) cap: Duration,
}

/// What entering a handler produced: its one outcome and the host's cleanup
/// report for the invocation (§E07).
pub(crate) struct Finished {
    /// The handler outcome.
    pub(crate) outcome: Result<InvokeOutput, InvokeFailure>,
    /// Whether every owned operation settled, and what did not.
    pub(crate) cleanup: Cleanup,
}

/// Begins an invocation for `handler`, runs it with `args`, and finishes the
/// invocation (§R04 §R09 §E07).
///
/// The host is the only minting door: a refused entry becomes a terminal
/// outcome and runs nothing. Every begun invocation is finished exactly once,
/// whatever the handler outcome.
pub(crate) async fn enter(
    script: &ScriptCx,
    handler: Handler<'_>,
    args: Arg,
) -> Result<Finished, InvokeFailure> {
    let parent = match &script.parent {
        Some(parent) => Parent::Of(parent),
        None => Parent::Root,
    };
    let entry = Entry::Export {
        id: handler.id.clone(),
        phase: handler.phase,
    };
    let inv = script
        .host
        .begin(parent, entry)
        .map_err(InvokeFailure::Terminal)?;
    let frame = Frame {
        inv: Arc::clone(&inv),
        host: Arc::clone(&script.host),
        loaded: Some(Arc::clone(handler.plugin)),
        script: Some(script.clone()),
        runtime: tokio::runtime::Handle::current(),
    };
    let deadline = deadline_for(&frame, handler.cap);
    let config = handler.plugin.config_value.clone();
    let body = Body::Run {
        callable: handler.run,
        args,
    };
    let outcome = evaluate_on_worker(frame, config, handler.plugin.limits, deadline, body).await;
    let cleanup = script.host.finish(&inv).await;
    Ok(Finished { outcome, cleanup })
}

/// The text a handler shows when the host supplies no script seam.
pub(crate) const NO_SCRIPT_HOST: &str =
    "this host does not run plugin handlers; start dal with plugin support enabled";

/// Reduces an entered handler to its validated output (§P06 §E07).
///
/// Prints go to the diagnostic channel only. An unsettled cleanup is logged
/// with the exact outstanding calls; it never changes the handler outcome.
/// A present output schema validates the value, not the display tree.
pub(crate) fn settle(
    entered: Result<Finished, InvokeFailure>,
    output: Option<&Schema>,
    id: &ExportId,
) -> Result<InvokeOutput, InvokeFailure> {
    let Finished { outcome, cleanup } = entered?;
    let entry = OpId::Export(id.clone());
    if !cleanup.complete {
        tracing::warn!(%entry, outstanding = ?cleanup.outstanding, "plugin invocation left unsettled calls");
    }
    let mut settled = outcome?;
    for line in &settled.prints {
        tracing::info!(%entry, %line, "plugin print");
    }
    let Some(schema) = output else {
        return Ok(settled);
    };
    settled.value = schema
        .validate(&settled.value, "output")
        .map_err(|error| InvokeFailure::Eval(error.to_string().into()))?;
    Ok(settled)
}

/// The deadline for one handler or cell: the earlier of the invocation wall
/// and the phase cap.
pub(crate) fn deadline_for(frame: &Frame, phase_cap: std::time::Duration) -> Instant {
    let capped = Instant::now() + phase_cap.min(CELL_WALL);
    capped.min(frame.inv.deadline())
}
