//! The `eval` tool: one Starlark cell per call (§E01 §E02 §E04).
//!
//! The request is strict: `{code, uses?, data?}` and nothing else. The host
//! mints the invocation from its captured environment; `uses` can only
//! narrow it. The reply is the E04 envelope, built once the invocation
//! exists; a request rejected before minting is a plain argument error.

use std::sync::Arc;

use dal_agent::error::ToolError;
use dal_agent::ext::script::{Cleanup, Entry, HostTerminal, Parent, ScriptCx};
use dal_agent::ext::tool::{ArgError, RawValue, ToolOutput};
use dal_agent::ext::{BoxFuture, Extension, ExtensionBuilder, Tool, ToolCall, ToolCx, ToolOutcome};
use dal_core::ext::{OpSet, ReadView, ToolData, UsesError};
use dal_core::{
    ModelInfo, Name, Origin, Part, Preview, RawJson, RegistrationError, ServiceSet, ToolClass,
    ToolSpec, Visibility, Workspace,
};

use crate::context::Frame;
use crate::engine::{CELL_WALL, Limits, MAX_CODE};
use crate::invoke::{self, InvokeFailure, InvokeOutput, NO_SCRIPT_HOST};
use crate::value::{CodecError, Value};

/// The model-facing description (§E08).
const DESCRIPTION: &str = "Write Starlark code. tools is already available. Calls return their \
values directly; a failed call stops the cell. The last expression is the result. Only tools in \
the attached environment are reachable, and their normal approvals still apply. No persistent \
globals, automatic retries, imports or wrapper function are needed. Pure computation needs no \
tool permission; uses=[] explicitly removes all effects.";

/// The request schema (§E01).
const PARAMETERS: &str = r#"{"type":"object","properties":{"code":{"type":"string"},"uses":{"type":"array","items":{"type":"string"},"uniqueItems":true},"data":{}},"required":["code"],"additionalProperties":false}"#;

/// Builds the host extension that registers the `eval` tool with the cell
/// budget `limits`.
///
/// # Errors
///
/// Returns [`RegistrationError`] when a compiled-in literal fails its own
/// validation.
pub fn eval_extension(limits: Limits) -> Result<Extension, RegistrationError> {
    let spec = Arc::new(ToolSpec {
        name: Name::parse("eval")?,
        description: DESCRIPTION.into(),
        parameters: RawJson::parse(PARAMETERS).map_err(|_| RegistrationError::InvalidParameters)?,
        grammar: None,
    });
    ExtensionBuilder::new("eval", env!("CARGO_PKG_VERSION"), ServiceSet::EMPTY)?
        .with_origin(Origin::Builtin, None)
        .tool(Arc::new(EvalTool { spec, limits }), Visibility::Model)
        .build()
}

/// The `eval` tool.
struct EvalTool {
    /// The provider-facing spec.
    spec: Arc<ToolSpec>,
    /// The per-cell evaluation budget.
    limits: Limits,
}

impl Tool for EvalTool {
    fn name(&self) -> &Name {
        &self.spec.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, args: &RawValue, _ws: &Workspace) -> Result<ToolClass, ArgError> {
        let request =
            Request::decode(args.as_str()).map_err(|error| ArgError::message(error.to_string()))?;
        let pure = request.uses.as_ref().is_some_and(OpSet::is_empty);
        Ok(ToolClass::Eval { pure })
    }

    fn run<'a>(&'a self, call: ToolCall, mut cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let request = match Request::decode(call.args.as_str()) {
                Ok(request) => request,
                Err(error) => return ToolOutcome::Err(ToolError::message(error.to_string())),
            };
            let pure = request.uses.as_ref().is_some_and(OpSet::is_empty);
            if !pure {
                let preview = Preview {
                    title: "eval".into(),
                    body: request.code.clone().into(),
                    digest: None,
                };
                if let Err(reason) = cx.authorize(preview).await {
                    return ToolOutcome::Err(ToolError::Denied(reason));
                }
            }
            let ast = match crate::engine::parse("cell.star", request.code) {
                Ok(ast) => ast,
                Err(message) => return ToolOutcome::Err(ToolError::message(message)),
            };
            let Some(script) = cx.script() else {
                return ToolOutcome::Err(ToolError::message(NO_SCRIPT_HOST));
            };
            let parent = match &script.parent {
                Some(parent) => Parent::Of(parent),
                None => Parent::Root,
            };
            let inv = match script
                .host
                .begin(parent, Entry::Eval { uses: request.uses })
            {
                Ok(inv) => inv,
                Err(terminal) => return ToolOutcome::Err(ToolError::message(terminal.to_string())),
            };
            let outcome = run_cell(script, &inv, ast, request.data, self.limits).await;
            let cleanup = script.host.finish(&inv).await;
            reply(envelope(inv.id().get().get(), outcome, &cleanup))
        })
    }
}

/// Runs one parsed cell inside a minted invocation.
async fn run_cell(
    script: &ScriptCx,
    inv: &Arc<dal_agent::ext::script::Invocation>,
    ast: starlark::syntax::AstModule,
    data: Value,
    limits: Limits,
) -> Result<InvokeOutput, InvokeFailure> {
    let frame = Frame {
        inv: Arc::clone(inv),
        host: Arc::clone(&script.host),
        loaded: None,
        script: Some(script.clone()),
        runtime: tokio::runtime::Handle::current(),
    };
    let deadline = invoke::deadline_for(&frame, CELL_WALL);
    invoke::cell(ast, frame, Value::Null, data, limits, deadline).await
}

/// One validated eval request (§E01).
struct Request {
    /// The cell source.
    code: String,
    /// The explicit narrowing, or `None` to inherit the environment.
    uses: Option<OpSet>,
    /// The caller data; `null` when absent.
    data: Value,
}

/// One rejected eval request.
#[derive(Debug, thiserror::Error)]
enum RequestError {
    /// The request is not strict JSON within the transport bounds.
    #[error(transparent)]
    Codec(#[from] CodecError),
    /// The request is not an object.
    #[error("the eval request must be an object {{code, uses?, data?}}")]
    NotObject,
    /// The request names a field outside the envelope.
    #[error("unknown eval request field `{0}`; the envelope is {{code, uses?, data?}}")]
    UnknownField(Box<str>),
    /// `code` is absent or not a string.
    #[error("`code` must be a string")]
    Code,
    /// `code` exceeds the source bound.
    #[error("`code` exceeds {MAX_CODE} bytes; split the work into smaller cells")]
    CodeTooLong,
    /// `uses` is not a list of strings.
    #[error("`uses` must be a list of operation ids")]
    UsesShape,
    /// `uses` names an unknown, repeated, or excess operation.
    #[error(transparent)]
    Uses(#[from] UsesError),
}

impl Request {
    /// Decodes and validates the request before anything runs.
    fn decode(raw: &str) -> Result<Self, RequestError> {
        let Value::Object(fields) = Value::decode(raw)? else {
            return Err(RequestError::NotObject);
        };
        let mut code = None;
        let mut uses = None;
        let mut data = Value::Null;
        for (key, item) in fields.into_vec() {
            match key.as_ref() {
                "code" => code = Some(item),
                "uses" => uses = Some(parse_uses(item)?),
                "data" => data = item,
                _ => return Err(RequestError::UnknownField(key)),
            }
        }
        let Some(Value::Str(code)) = code else {
            return Err(RequestError::Code);
        };
        if code.len() > MAX_CODE {
            return Err(RequestError::CodeTooLong);
        }
        Ok(Self {
            code: code.into_string(),
            uses,
            data,
        })
    }
}

/// Parses an explicit `uses` list.
fn parse_uses(item: Value) -> Result<OpSet, RequestError> {
    let Value::List(items) = item else {
        return Err(RequestError::UsesShape);
    };
    let ids = items
        .iter()
        .map(|item| match item {
            Value::Str(id) => Ok(id.as_ref()),
            _ => Err(RequestError::UsesShape),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(OpSet::parse(ids)?)
}

/// The terminal status of one cell (§E04).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Status {
    /// The computation finished.
    Completed,
    /// An expected or scripted failure stopped the cell.
    Failed,
    /// Authority refused an operation.
    Denied,
    /// Root cancellation stopped the cell.
    Cancelled,
    /// A hard limit stopped the cell.
    LimitExceeded,
    /// The outcome cannot be established.
    Indeterminate,
}

impl Status {
    /// The wire spelling.
    fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Denied => "denied",
            Self::Cancelled => "cancelled",
            Self::LimitExceeded => "limit_exceeded",
            Self::Indeterminate => "indeterminate",
        }
    }
}

/// Builds the E04 envelope of one minted invocation and returns it with
/// its status and the intact views a completed cell carries.
fn envelope(
    invocation: u64,
    outcome: Result<InvokeOutput, InvokeFailure>,
    cleanup: &Cleanup,
) -> (Status, Value, Box<[ReadView]>) {
    let mut fields = vec![("invocation_id", Value::Str(invocation.to_string().into()))];
    let (status, views) = match outcome {
        Ok(output) => {
            fields.push(("status", Value::Str(Status::Completed.as_str().into())));
            fields.push(("value", output.value));
            fields.push((
                "prints",
                Value::object([
                    (
                        "lines",
                        Value::List(
                            output
                                .prints
                                .into_vec()
                                .into_iter()
                                .map(Value::Str)
                                .collect(),
                        ),
                    ),
                    ("truncated", Value::Bool(output.prints_truncated)),
                ]),
            ));
            (Status::Completed, output.views)
        }
        Err(failure) => {
            let (status, code) = classify(&failure);
            fields.push(("status", Value::Str(status.as_str().into())));
            fields.push((
                "error",
                Value::object([
                    ("code", Value::Str(code)),
                    ("message", Value::Str(failure.to_string().into())),
                    ("stage", Value::Str("run".into())),
                ]),
            ));
            (status, Box::default())
        }
    };
    let observer_errors = cleanup.observer_errors.iter().map(|error| {
        Value::object([
            ("ext", Value::Str(error.ext.as_str().into())),
            ("event", Value::Str(error.event.as_str().into())),
            ("message", Value::Str(error.message.clone())),
        ])
    });
    let outstanding = cleanup
        .outstanding
        .iter()
        .map(|call| Value::Str(call.as_str().into()));
    fields.push(("observer_errors", Value::List(observer_errors.collect())));
    fields.push((
        "cleanup",
        Value::object([
            ("complete", Value::Bool(cleanup.complete)),
            ("outstanding", Value::List(outstanding.collect())),
        ]),
    ));
    (status, Value::object(fields), views)
}

/// Maps a failure onto its status and error code.
fn classify(failure: &InvokeFailure) -> (Status, Box<str>) {
    let (status, code) = match failure {
        InvokeFailure::Script { failure, .. } => {
            return (Status::Failed, failure.code.as_str().into());
        }
        InvokeFailure::Terminal(terminal) => terminal_status(terminal),
        InvokeFailure::Api(_) => (Status::Failed, "api_error"),
        InvokeFailure::Eval(_) => (Status::Failed, "eval_error"),
        InvokeFailure::Cancelled => (Status::Cancelled, "cancelled"),
        InvokeFailure::Timeout => (Status::LimitExceeded, "timeout"),
        InvokeFailure::Worker => (Status::Indeterminate, "worker_failed"),
    };
    (status, code.into())
}

/// Maps a terminal host outcome onto its status and code; an unknown
/// terminal is treated as a denial.
fn terminal_status(terminal: &HostTerminal) -> (Status, &'static str) {
    match terminal {
        HostTerminal::Cancelled => (Status::Cancelled, "cancelled"),
        HostTerminal::LimitExceeded { .. } => (Status::LimitExceeded, "limit_exceeded"),
        HostTerminal::Revoked => (Status::Denied, "revoked"),
        HostTerminal::ScopeExceeded { .. } => (Status::Denied, "scope_exceeded"),
        HostTerminal::IncompleteScope { .. } => (Status::Denied, "incomplete_scope"),
        HostTerminal::NestedEval => (Status::Denied, "nested_eval"),
        HostTerminal::NestedScriptExport => (Status::Denied, "nested_script_export"),
        _ => (Status::Denied, "denied"),
    }
}

/// Maps the envelope onto one tool outcome: completed cells succeed with
/// their intact views for evidence rebinding; cancelled cells are
/// interrupted; every other status is a failed tool call carrying the
/// envelope.
fn reply((status, envelope, views): (Status, Value, Box<[ReadView]>)) -> ToolOutcome {
    let text = envelope.to_json();
    match status {
        Status::Cancelled => ToolOutcome::Interrupted,
        Status::Completed => ToolOutcome::Ok(Box::new(ToolOutput {
            parts: vec![Part::Text { text: text.into() }],
            files_changed: Vec::new(),
            data: (!views.is_empty()).then_some(ToolData::Views(views)),
        })),
        _ => ToolOutcome::Err(ToolError::message(text)),
    }
}
