//! The `OpAdapter`: the only value that carries an invocation's operations
//! (§R04, §R05).
//!
//! Attribute access on a facade mints an adapter bound to the invocation id
//! and the resolved `OpId`. Calling it re-verifies the frame (the call came
//! from this invocation's evaluator), binds `Arguments` to one transport
//! object, then either blocks on `host.call` or schedules through
//! `host.submit` when minted inside `scope(...)`.
//!
//! The adapter is a handle, not a grant: the host computes authority
//! `U ∩ D ∩ P` at request time, so adapters minted inside scopes, stored in
//! values, or passed across functions carry no residual authority (§R07).
#![expect(unsafe_code, reason = "starlark value derives")]

use std::sync::Arc;

use allocative::Allocative;
use dal_agent::ext::script::{Invocation, InvocationId, ScriptHost};
use dal_agent::ext::script::{OpOutcome, OpRequest, OpValue, ScriptCx, Submit};
use dal_agent::ext::{PrivateTool, Tool};
use dal_core::{
    ModelRequest, ModelRoute, RawJson,
    ext::{NativeOp, OpId},
};
use starlark::any::ProvidesStaticType;
use starlark::collections::SmallMap;
use starlark::eval::{Arguments, Evaluator};
use starlark::starlark_simple_value;
use starlark::values::{Heap, NoSerialize, StarlarkValue, StringValue, Trace, Value, ValueLike};

use crate::context::frame_of;
use crate::error::api_error;
use crate::scope::{ScopeShared, TaskValue};
use crate::tool::ExportTool;
use crate::validate::ExportBody;
use crate::value;

/// A minted operation handle (§R04).
///
/// `scheduled` distinguishes a synchronous `ctx` adapter (`None`) from one
/// minted inside `scope(...)`: scheduled adapters submit and return a
/// `Task`; synchronous adapters block the Starlark thread on `host.call`
/// and return the outcome projection.
#[derive(ProvidesStaticType, Trace, NoSerialize, Allocative)]
#[repr(C)]
pub(crate) struct OpAdapter {
    /// The invocation this handle belongs to.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    pub(crate) inv: InvocationId,
    /// The operation this call requests.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    pub(crate) op: OpId,
    /// The invocation for `call`/`submit`.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    pub(crate) invocation: Arc<Invocation>,
    /// The host seam.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    pub(crate) host: Arc<dyn ScriptHost>,
    /// The scope a scheduled adapter submits into.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    pub(crate) scheduled: Option<Arc<ScopeShared>>,
    /// The loaded plugin for export argument binding.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    pub(crate) loaded: Option<Arc<crate::validate::LoadedPlugin>>,
    /// The model runtime context, when this entry is a scripted model.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    pub(crate) script: Option<ScriptCx>,
    /// The tokio runtime handle the synchronous path blocks on.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    pub(crate) runtime: tokio::runtime::Handle,
}

starlark_simple_value!(OpAdapter);

impl std::fmt::Debug for OpAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl std::fmt::Display for OpAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<op {}>", self.op)
    }
}

#[starlark::values::starlark_value(type = "operation")]
impl<'v> StarlarkValue<'v> for OpAdapter {
    fn invoke(
        &self,
        me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let _ = me;
        self.check_frame(eval)?;
        let bound = self.bind_args(args, eval.heap())?;
        let (request, model_route) = self.request(&bound)?;
        if let Some(shared) = &self.scheduled {
            self.scheduled_call(shared, request, model_route.as_ref(), eval.heap())
        } else {
            let outcome = self.dispatch(request);
            let outcome = wrap_model_outcome(outcome, model_route)?;
            crate::outcome::project_outcome(outcome, &self.invocation, eval.heap())
        }
    }
}

impl OpAdapter {
    /// Proves the call came from this invocation's evaluator (§R02).
    fn check_frame(&self, eval: &Evaluator<'_, '_, '_>) -> starlark::Result<()> {
        let Some(frame) = frame_of(eval) else {
            return Err(api_error(format!(
                "{}: operations need a live invocation",
                self.op
            )));
        };
        if frame.inv.id() != self.inv {
            return Err(api_error(format!(
                "{}: handle belongs to another invocation",
                self.op
            )));
        }
        Ok(())
    }

    /// Encodes `Arguments` into one transport object (§R05).
    fn bind_args<'v>(
        &self,
        args: &Arguments<'v, '_>,
        heap: Heap<'v>,
    ) -> starlark::Result<value::Value> {
        let positions: Vec<Value<'v>> = args.positions(heap)?.collect();
        let names = args.names_map()?;
        self.bind_parts(&positions, &names)
    }

    /// Binds collected positional and named arguments (§R03 §R05).
    ///
    /// Named arguments keep their call-site names. A native operation with a
    /// declared convenience field accepts one positional item; a command
    /// export's positionals cross as `args`, a list bound against the
    /// command's declared positional schema at the script-tool layer.
    fn bind_parts<'v>(
        &self,
        positions: &[Value<'v>],
        names: &SmallMap<StringValue<'v>, Value<'v>>,
    ) -> starlark::Result<value::Value> {
        let mut map: Vec<(Box<str>, value::Value)> = Vec::new();
        for (name, item) in names {
            map.push((
                name.as_str().into(),
                value::Value::from_starlark(*item)
                    .map_err(|e| api_error(format!("{}: {e}", self.op)))?,
            ));
        }
        match &self.op {
            OpId::Native(native) => self.bind_native(*native, positions, &mut map)?,
            OpId::Export(_) if !positions.is_empty() => {
                let items = positions
                    .iter()
                    .copied()
                    .map(|item| {
                        value::Value::from_starlark(item)
                            .map_err(|e| api_error(format!("{}: {e}", self.op)))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                map.push(("args".into(), value::Value::List(items.into_boxed_slice())));
            }
            OpId::Export(_) => {}
        }
        Ok(value::Value::Object(map.into_boxed_slice()))
    }

    /// Applies the native positional-convenience rule (§R03).
    fn bind_native(
        &self,
        native: dal_core::ext::NativeOp,
        positions: &[Value<'_>],
        map: &mut Vec<(Box<str>, value::Value)>,
    ) -> starlark::Result<()> {
        if matches!(native, NativeOp::ModelsInfer | NativeOp::ModelsForward) {
            match positions.len() {
                0 => {}
                1 if !map.iter().any(|(name, _)| name.as_ref() == "request") => {
                    let item = value::Value::from_starlark(positions[0])
                        .map_err(|error| api_error(format!("{}: {error}", self.op)))?;
                    map.push(("request".into(), item));
                }
                1 => {
                    return Err(api_error(format!(
                        "{}: request was supplied both positionally and by name",
                        self.op
                    )));
                }
                _ => {
                    return Err(api_error(format!(
                        "{}: at most one positional request",
                        self.op
                    )));
                }
            }
        } else {
            match positions.len() {
                0 => {}
                1 => {
                    let Some(field) = native.positional() else {
                        return Err(api_error(format!(
                            "{}: this operation takes named arguments only",
                            self.op
                        )));
                    };
                    let item = value::Value::from_starlark(positions[0])
                        .map_err(|error| api_error(format!("{}: {error}", self.op)))?;
                    map.push((field.into(), item));
                }
                _ => {
                    return Err(api_error(format!(
                        "{}: at most one positional argument",
                        self.op
                    )));
                }
            }
        }
        for (key, literal) in native.sdk_defaults() {
            if map.iter().any(|(name, _)| name.as_ref() == *key) {
                continue;
            }
            let default = value::Value::decode(literal)
                .unwrap_or_else(|_| value::Value::Str((*literal).into()));
            map.push(((*key).into(), default));
        }
        Ok(())
    }

    /// Wraps bound arguments into the host request envelope.
    fn request(&self, args: &value::Value) -> starlark::Result<(OpRequest, Option<ModelRoute>)> {
        let is_model_operation = matches!(
            &self.op,
            OpId::Native(NativeOp::ModelsInfer | NativeOp::ModelsForward)
        );
        let (raw, model_route, private_tools) = if is_model_operation {
            let request = Self::decode_model_request(args)
                .map_err(|error| api_error(format!("{}: {error}", self.op)))?;
            let route = request.model.clone();
            let tools = self.private_tools(&request);
            let raw = sonic_rs::to_string(&request)
                .map_err(|error| api_error(format!("{}: {error}", self.op)))?;
            (raw, Some(route), tools)
        } else {
            (args.to_json(), None, Vec::new())
        };
        let args =
            RawJson::parse(&raw).map_err(|error| api_error(format!("{}: {error}", self.op)))?;
        let request = OpRequest::new(self.op.clone(), args);
        if !is_model_operation {
            return Ok((request, model_route));
        }
        let request = if let Some(script) = &self.script {
            request.with_model_context(script)
        } else {
            request
        };
        Ok((request.with_private_tools(private_tools), model_route))
    }

    /// Wraps callable exports as opaque tools for the forwarded request.
    fn private_tools(&self, request: &ModelRequest) -> Vec<PrivateTool> {
        let Some(plugin) = &self.loaded else {
            return Vec::new();
        };
        plugin
            .exports
            .iter()
            .enumerate()
            .filter_map(|(index, export)| {
                let ExportBody::Tool { wire, .. } = &export.body;
                request
                    .tools
                    .iter()
                    .any(|spec| spec.name.as_ref() == wire.as_str())
                    .then(|| {
                        let tool: Arc<dyn Tool> =
                            Arc::new(ExportTool::new(Arc::clone(plugin), index));
                        PrivateTool(tool)
                    })
            })
            .collect()
    }

    /// Schedules one typed model request through this scope facade.
    pub(crate) fn schedule_model_request<'v>(
        &self,
        request: Value<'v>,
        heap: Heap<'v>,
    ) -> starlark::Result<Value<'v>> {
        let shared = self
            .scheduled
            .as_ref()
            .ok_or_else(|| api_error(format!("{}: this operation needs a scope", self.op)))?;
        let request = value::Value::from_starlark(request)
            .map_err(|error| api_error(format!("{}: {error}", self.op)))?;
        let args = value::Value::Object(vec![("request".into(), request)].into_boxed_slice());
        let (request, model_route) = self.request(&args)?;
        self.scheduled_call(shared, request, model_route.as_ref(), heap)
    }

    /// Blocks the Starlark thread on `host.call` (§R09).
    ///
    /// The future is spawned on the owning runtime so nested host work runs
    /// on a live task instead of inside this blocking bridge.
    fn dispatch(&self, request: OpRequest) -> OpOutcome {
        let host = Arc::clone(&self.host);
        let inv = Arc::clone(&self.invocation);
        let task = self
            .runtime
            .spawn(async move { host.call(&inv, request).await });
        self.runtime
            .block_on(task)
            .unwrap_or_else(|error| OpOutcome::Failed {
                failure: dal_agent::ext::script::OpFailure {
                    code: dal_agent::ext::script::FailureCode::Indeterminate,
                    message: format!("host call failed: {error}").into(),
                    details: None,
                },
                record: dal_agent::ext::script::OpRecord {
                    call: dal_core::CallId::new("interpreter"),
                    op: self.op.clone(),
                    status: dal_agent::ext::script::EffectStatus::Failed,
                },
            })
    }

    /// Decodes the strict provider-neutral request schema used by model operations.
    fn decode_model_request(args: &value::Value) -> Result<ModelRequest, Box<str>> {
        let request = match args {
            value::Value::Object(fields)
                if fields.len() == 1 && fields[0].0.as_ref() == "request" =>
            {
                &fields[0].1
            }
            other => other,
        };
        validate_model_request_fields(request)?;
        sonic_rs::from_str(&request.to_json())
            .map_err(|error| format!("invalid request: {error}").into())
    }

    /// Scheduled call inside `scope(...)`: submits and returns a `Task` (§E05).
    fn scheduled_call<'v>(
        &self,
        shared: &Arc<ScopeShared>,
        request: OpRequest,
        model_route: Option<&ModelRoute>,
        heap: Heap<'v>,
    ) -> starlark::Result<Value<'v>> {
        if !shared.is_open() {
            return Err(api_error(format!(
                "{}: the scope is sealed; no new task may be submitted",
                self.op
            )));
        }
        match &self.op {
            OpId::Native(native) if native.schedulable() => {}
            OpId::Native(_) => {
                return Err(api_error(format!(
                    "{}: this operation cannot be scheduled in a scope",
                    self.op
                )));
            }
            OpId::Export(_) => {
                return Err(api_error(format!(
                    "{}: scripted exports cannot be scheduled; call it through ctx",
                    self.op
                )));
            }
        }
        match self.host.submit(&self.invocation, shared.id, request) {
            Submit::Terminal(terminal) => Err(crate::outcome::terminal_error(terminal)),
            Submit::Refused(error) => Err(api_error(error.to_string())),
            Submit::Queued(task) | Submit::Settled(task) => {
                if let Some(route) = model_route {
                    shared
                        .model_routes
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .insert(task, route.clone());
                }
                Ok(heap.alloc(TaskValue::new(
                    &self.invocation,
                    task,
                    &self.host,
                    shared,
                    self.runtime.clone(),
                )))
            }
        }
    }

    /// `ctx.try_call` on this adapter (§R07).
    ///
    /// Only a synchronous native adapter is accepted; denial, cancellation,
    /// and hard limits remain terminal.
    pub(crate) fn try_call<'v>(
        &self,
        eval: &Evaluator<'v, '_, '_>,
        positions: &[Value<'v>],
        names: &SmallMap<StringValue<'v>, Value<'v>>,
        heap: Heap<'v>,
    ) -> starlark::Result<Value<'v>> {
        if self.scheduled.is_some() {
            return Err(api_error(format!(
                "{}: scheduled scope adapters cannot be retried through try_call",
                self.op
            )));
        }
        if !matches!(self.op, OpId::Native(_)) {
            return Err(api_error(format!(
                "{}: try_call accepts native operation adapters only",
                self.op
            )));
        }
        self.check_frame(eval)?;
        let bound = self.bind_parts(positions, names)?;
        let (request, model_route) = self.request(&bound)?;
        let outcome = self.dispatch(request);
        let outcome = wrap_model_outcome(outcome, model_route)?;
        crate::outcome::settled_outcome(outcome, &self.invocation, heap)
    }
}

fn validate_model_request_fields(value: &value::Value) -> Result<(), Box<str>> {
    let fields = strict_object(
        value,
        "request",
        &[
            "purpose",
            "model",
            "system",
            "tools",
            "context",
            "params",
            "cache_key",
        ],
    )?;
    if let Some(model) = get_field(fields, "model") {
        let kind = object_tag(model, "kind", "request.model")?;
        let allowed = match kind {
            "api" => &["kind", "family", "model"][..],
            "synthetic" | "harness" => &["kind", "id"][..],
            _ => return Err(format!("request.model has unknown kind `{kind}`").into()),
        };
        strict_object(model, "request.model", allowed)?;
    }
    if let Some(params) = get_field(fields, "params") {
        strict_object(
            params,
            "request.params",
            &["thinking", "effort", "temperature", "max_output_tokens"],
        )?;
    }
    if let Some(tools) = get_field(fields, "tools") {
        let value::Value::List(tools) = tools else {
            return Err("request.tools must be a list".into());
        };
        for tool in tools {
            strict_object(
                tool,
                "request.tools[]",
                &["name", "description", "parameters", "grammar"],
            )?;
        }
    }
    if let Some(context) = get_field(fields, "context") {
        let value::Value::List(items) = context else {
            return Err("request.context must be a list".into());
        };
        for item in items {
            validate_context_item(item)?;
        }
    }
    Ok(())
}

fn validate_context_item(value: &value::Value) -> Result<(), Box<str>> {
    let role = object_tag(value, "role", "request.context[]")?;
    let fields = match role {
        "user" => strict_object(value, "request.context[]", &["role", "parts"])?,
        "assistant" => strict_object(value, "request.context[]", &["role", "source", "parts"])?,
        "tool_result" => strict_object(
            value,
            "request.context[]",
            &["role", "call", "name", "is_error", "parts"],
        )?,
        _ => return Err(format!("request.context[] has unknown role `{role}`").into()),
    };
    if role == "assistant" {
        if let Some(source) = get_field(fields, "source") {
            strict_object(source, "request.context[].source", &["family", "model"])?;
        }
        if let Some(parts) = get_field(fields, "parts") {
            validate_parts(parts, true)?;
        }
    } else if let Some(parts) = get_field(fields, "parts") {
        validate_parts(parts, false)?;
    }
    Ok(())
}

fn validate_parts(value: &value::Value, assistant: bool) -> Result<(), Box<str>> {
    let value::Value::List(parts) = value else {
        return Err("request.context[].parts must be a list".into());
    };
    for part in parts {
        let kind = object_tag(part, "type", "request.context[].parts[]")?;
        let allowed = match (assistant, kind) {
            // `text` is the one part kind both roles share; `unnested_or_patterns`
            // rejects the merged arm and `match_same_arms` rejects two.
            (false, "text") if !assistant => &["type", "text"][..],
            (true, "text") => &["type", "text"][..],
            (false, "image") => &["type", "mime", "bytes"][..],
            (false, "blob") => &["type", "blob_id", "mime", "bytes"][..],
            (true, "thinking") => &["type", "text", "replay"][..],
            (true, "tool_call") => &["type", "call", "name", "args"][..],
            _ => return Err(format!("request.context[].parts[] has unknown type `{kind}`").into()),
        };
        strict_object(part, "request.context[].parts[]", allowed)?;
    }
    Ok(())
}

fn object_tag<'a>(value: &'a value::Value, field: &str, label: &str) -> Result<&'a str, Box<str>> {
    let value::Value::Object(fields) = value else {
        return Err(format!("{label} must be an object").into());
    };
    match get_field(fields, field) {
        Some(value::Value::Str(value)) => Ok(value),
        _ => Err(format!("{label}.{field} must be a string").into()),
    }
}

fn strict_object<'a>(
    value: &'a value::Value,
    label: &str,
    allowed: &[&str],
) -> Result<&'a [(Box<str>, value::Value)], Box<str>> {
    let value::Value::Object(fields) = value else {
        return Err(format!("{label} must be an object").into());
    };
    for (name, _) in fields {
        if !allowed.contains(&name.as_ref()) {
            return Err(format!("{label} has unknown field `{name}`").into());
        }
    }
    Ok(fields)
}

fn get_field<'a>(fields: &'a [(Box<str>, value::Value)], name: &str) -> Option<&'a value::Value> {
    fields
        .iter()
        .find(|(field, _)| field.as_ref() == name)
        .map(|(_, value)| value)
}

pub(crate) fn wrap_model_outcome(
    outcome: OpOutcome,
    model: Option<ModelRoute>,
) -> starlark::Result<OpOutcome> {
    let Some(model) = model else {
        return Ok(outcome);
    };
    let OpOutcome::Ok {
        value: OpValue::Json(json),
        record,
    } = outcome
    else {
        return Ok(outcome);
    };
    let inference: dal_core::Inference = sonic_rs::from_str(json.as_str())
        .map_err(|error| api_error(format!("model inference result: {error}")))?;
    let encoded = sonic_rs::to_string(&crate::model::ModelForward {
        model,
        events: inference.events,
    })
    .map_err(|error| api_error(format!("model inference result: {error}")))?;
    let value = RawJson::parse(&encoded)
        .map_err(|error| api_error(format!("model inference result: {error}")))?;
    Ok(OpOutcome::Ok {
        value: OpValue::Json(value),
        record,
    })
}

/// Borrows the [`OpAdapter`] inside `value`; adapters are never frozen.
pub(crate) fn adapter_of(value: Value<'_>) -> Option<&OpAdapter> {
    ValueLike::downcast_ref::<OpAdapter>(value)
}

#[cfg(test)]
mod tests {
    use super::OpAdapter;
    use crate::value::Value;

    #[test]
    fn model_request_decodes_the_canonical_typed_fields() {
        let request = Value::decode(
            r#"{"purpose":"turn","model":{"kind":"api","family":"openai_chat","model":"gpt-6"},"system":"system","tools":[],"context":[{"role":"user","parts":[{"type":"text","text":"hello"}]}],"params":{"thinking":"off","effort":null,"temperature":null,"max_output_tokens":null},"cache_key":null}"#,
        )
        .expect("request transport value");
        let request = OpAdapter::decode_model_request(&request).expect("typed model request");
        assert_eq!(request.model.id(), "gpt-6");
        assert_eq!(request.context.len(), 1);
    }

    #[test]
    fn model_request_rejects_unknown_nested_fields() {
        let request = Value::decode(
            r#"{"purpose":"turn","model":{"kind":"api","family":"openai_chat","model":"gpt-6"},"system":"system","tools":[],"context":[],"params":{"thinking":"off","unsupported":true},"cache_key":null}"#,
        )
        .expect("request transport value");
        let error = OpAdapter::decode_model_request(&request).expect_err("unknown field");
        assert!(
            error.contains("request.params has unknown field `unsupported`"),
            "{error}"
        );
    }

    #[test]
    fn model_request_rejects_unknown_content_fields() {
        let request = Value::decode(
            r#"{"purpose":"turn","model":{"kind":"api","family":"openai_chat","model":"gpt-6"},"system":"system","tools":[],"context":[{"role":"user","parts":[{"type":"text","text":"hello","extra":false}]}],"params":{"thinking":"off","effort":null,"temperature":null,"max_output_tokens":null},"cache_key":null}"#,
        )
        .expect("request transport value");
        let error = OpAdapter::decode_model_request(&request).expect_err("unknown field");
        assert!(
            error.contains("request.context[].parts[] has unknown field `extra`"),
            "{error}"
        );
    }
}
