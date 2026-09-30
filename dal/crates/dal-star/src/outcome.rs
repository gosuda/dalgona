//! The boundary between host operation outcomes and script-visible values
//! (§R07).
//!
//! Three cases leave this module: a successful [`OpValue`] is materialized
//! into the caller's heap, an expected [`OpFailure`] raises a [`ScriptFailure`]
//! the invocation boundary records as that operation's failed outcome, and a
//! [`HostTerminal`] carries through unchanged so no script can catch it.
//!
//! [`Settled`] is the explicit-recovery value `try_call`, `Task.settle` and
//! `Scope.settle` return. `r.ok` answers the question, `r.value`/`r.error`
//! expose exactly one arm, and `r.unwrap()` re-raises the recorded failure.
#![expect(unsafe_code, reason = "starlark value derives")]

use std::fmt;
use std::sync::Arc;

use allocative::Allocative;
use dal_agent::ext::script::{HostTerminal, Invocation, OpFailure, OpOutcome, OpValue};
use dal_core::ext::{OpId, StateRecord};
use starlark::any::ProvidesStaticType;
use starlark::coerce::Coerce;
use starlark::environment::{Methods, MethodsBuilder};
use starlark::values::{
    Freeze, Heap, NoSerialize, StarlarkValue, Trace, Value, ValueLifetimeless, ValueLike,
};
use starlark::{starlark_complex_value, starlark_module};

use crate::error::api_error;
use crate::record::Revision;
use crate::value;

/// An expected operation failure raised into the evaluator (§R07).
///
/// Raised once per operation by [`project_outcome`] or `Result.unwrap`.
/// The invocation boundary downcasts the error kind back to this type and
/// records the outcome against `op`; a script can never observe the type.
#[derive(Debug)]
pub(crate) struct ScriptFailure {
    /// The operation this failure belongs to; `None` for failures outside a
    /// cataloged operation such as `ctx.adopt`.
    pub(crate) op: Option<OpId>,
    /// The recoverable failure record.
    pub(crate) failure: OpFailure,
}

impl fmt::Display for ScriptFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.op {
            Some(op) => write!(f, "{}: {}", op, self.failure),
            None => self.failure.fmt(f),
        }
    }
}

impl std::error::Error for ScriptFailure {}

/// Wraps one expected operation failure as a script failure (§R07).
pub(crate) fn script_failure(op: OpId, failure: OpFailure) -> starlark::Error {
    starlark::Error::new_other(ScriptFailure {
        op: Some(op),
        failure,
    })
}

/// Wraps one non-operation failure such as a refused `ctx.adopt` (§E06).
pub(crate) fn boundary_failure(failure: OpFailure) -> starlark::Error {
    starlark::Error::new_other(ScriptFailure { op: None, failure })
}

/// Carries a terminal host outcome through the evaluator unchanged (§R07).
///
/// The invocation boundary downcasts it back; scripts never see a catchable
/// Result for denial, cancellation, revocation, or hard limits.
pub(crate) fn terminal_error(terminal: HostTerminal) -> starlark::Error {
    starlark::Error::new_other(terminal)
}

/// Borrows a [`ScriptFailure`] a raised error carries, if any.
pub(crate) fn script_failure_of(error: &starlark::Error) -> Option<&ScriptFailure> {
    match error.kind() {
        starlark::ErrorKind::Other(source) => source.downcast_ref::<ScriptFailure>(),
        _ => None,
    }
}

/// Borrows the [`HostTerminal`] a raised error carries, if any.
pub(crate) fn terminal_of(error: &starlark::Error) -> Option<&HostTerminal> {
    match error.kind() {
        starlark::ErrorKind::Other(source) => source.downcast_ref::<HostTerminal>(),
        _ => None,
    }
}

/// Materializes one operation outcome: the value, an expected failure, or a
/// terminal error (§R07).
pub(crate) fn project_outcome<'v>(
    outcome: OpOutcome,
    inv: &Arc<Invocation>,
    heap: Heap<'v>,
) -> starlark::Result<Value<'v>> {
    match outcome {
        OpOutcome::Ok { value, .. } => project_op_value(value, inv, heap),
        OpOutcome::Failed { failure, record } => Err(script_failure(record.op, failure)),
        OpOutcome::Terminal(terminal) => Err(terminal_error(terminal)),
    }
}

/// Materializes one successful operation value (§R05 §R06).
pub(crate) fn project_op_value<'v>(
    value: OpValue,
    inv: &Arc<Invocation>,
    heap: Heap<'v>,
) -> starlark::Result<Value<'v>> {
    match value {
        OpValue::Json(json) => decode_result(json.as_str(), heap),
        OpValue::Data(data) => crate::evidence::tool_data_value(data, heap),
        OpValue::State(record) => project_state(record, inv, heap),
        OpValue::Detached(job) => {
            crate::record::Record::alloc(heap, vec![("job".into(), heap.alloc(job.to_string()))])
        }
    }
}

/// Decodes host JSON into immutable script values.
fn decode_result<'v>(json: &str, heap: Heap<'v>) -> starlark::Result<Value<'v>> {
    value::Value::decode(json)
        .and_then(|value| value.into_starlark(heap))
        .map_err(|error| api_error(format!("operation result: {error}")))
}

/// Projects a state record: `present`, `value` when present, `revision` (§R08).
fn project_state<'v>(
    record: StateRecord,
    inv: &Arc<Invocation>,
    heap: Heap<'v>,
) -> starlark::Result<Value<'v>> {
    let mut fields: Vec<(String, Value<'v>)> = Vec::with_capacity(3);
    fields.push(("present".into(), heap.alloc(record.present)));
    if let Some(raw) = record.value.filter(|_| record.present) {
        fields.push(("value".into(), decode_result(raw.as_str(), heap)?));
    }
    let revision = Revision {
        generation: inv.generation().get(),
        serial: record.revision.get().get(),
    };
    fields.push(("revision".into(), heap.alloc(revision)));
    crate::record::Record::alloc(heap, fields)
}

/// Materializes a `Result.error` record from an [`OpFailure`].
fn failure_record<'v>(heap: Heap<'v>, failure: &OpFailure) -> starlark::Result<Value<'v>> {
    let mut fields: Vec<(String, Value<'v>)> = Vec::with_capacity(3);
    fields.push(("code".into(), heap.alloc(failure.code.as_str())));
    fields.push(("message".into(), heap.alloc(failure.message.as_ref())));
    if let Some(details) = &failure.details {
        fields.push(("details".into(), decode_result(details.as_str(), heap)?));
    }
    crate::record::Record::alloc(heap, fields)
}

/// One settled native result returned by `try_call` and scope recovery (§R07 §E05).
///
/// `ok` selects the accessible field: `.value` on success, `.error` on an
/// expected failure; the other variant answers an attribute error. `unwrap`
/// returns the value or re-raises the recorded [`ScriptFailure`]. The typed
/// `op` and `failure` fields are host data carried verbatim through freeze.
#[repr(C)]
#[derive(Debug, Trace, Coerce, Freeze, ProvidesStaticType, NoSerialize, Allocative)]
pub(crate) struct SettledGen<V: ValueLifetimeless> {
    /// Whether the operation produced a value.
    ok: bool,
    /// The success value; `NoneType` on failure.
    value: V,
    /// The `error` record; `NoneType` on success.
    error: V,
    /// The operation the result belongs to.
    #[trace(unsafe_ignore)]
    #[freeze(identity)]
    #[allocative(skip)]
    op: OpId,
    /// The recoverable failure record `unwrap` re-raises; `None` on success.
    #[trace(unsafe_ignore)]
    #[freeze(identity)]
    #[allocative(skip)]
    failure: Option<OpFailure>,
}

starlark_complex_value!(pub(crate) Settled);

impl<V: ValueLifetimeless + fmt::Debug> fmt::Display for SettledGen<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let variant = if self.ok { "ok" } else { "err" };
        write!(f, "result({variant}, {})", self.op)
    }
}

#[starlark::values::starlark_value(type = "result")]
impl<'v, V: starlark::values::ValueLike<'v>> StarlarkValue<'v> for SettledGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    fn get_attr(&self, attribute: &str, heap: Heap<'v>) -> Option<Value<'v>> {
        match attribute {
            "ok" => Some(heap.alloc(self.ok)),
            "value" if self.ok => Some(self.value.to_value()),
            "error" if !self.ok => Some(self.error.to_value()),
            _ => None,
        }
    }

    fn get_methods() -> Option<&'static Methods> {
        Some(SETTLED_METHODS.methods())
    }
}

starlark::methods_static!(SETTLED_METHODS = settled_methods);

#[starlark_module]
fn settled_methods(builder: &mut MethodsBuilder) {
    /// Returns the value, or re-raises the recorded operation failure.
    fn unwrap<'v>(#[starlark(this)] this: Value<'v>) -> starlark::Result<Value<'v>> {
        let Some(result) = Settled::from_value(this) else {
            return Err(api_error("unwrap: receiver is not a result"));
        };
        if result.ok {
            return Ok(result.value.to_value());
        }
        let Some(failure) = result.failure.clone() else {
            return Err(api_error("unwrap: failed result has no failure"));
        };
        Err(script_failure(result.op.clone(), failure))
    }
}

/// Allocates a successful [`Settled`] result.
pub(crate) fn settled_ok<'v>(heap: Heap<'v>, op: &OpId, value: Value<'v>) -> Value<'v> {
    heap.alloc_complex(SettledGen {
        ok: true,
        value,
        error: Value::new_none(),
        op: op.clone(),
        failure: None,
    })
}

/// Allocates a failed [`Settled`] result.
///
/// The `error` record is materialized once here, so reads never decode.
pub(crate) fn settled_err<'v>(
    heap: Heap<'v>,
    op: &OpId,
    failure: OpFailure,
) -> starlark::Result<Value<'v>> {
    let record = failure_record(heap, &failure)?;
    Ok(heap.alloc_complex(SettledGen {
        ok: false,
        value: Value::new_none(),
        error: record,
        op: op.clone(),
        failure: Some(failure),
    }))
}

/// Allocates a [`Settled`] for one collected task outcome (§E05).
pub(crate) fn settled_outcome<'v>(
    outcome: OpOutcome,
    inv: &Arc<Invocation>,
    heap: Heap<'v>,
) -> starlark::Result<Value<'v>> {
    match outcome {
        OpOutcome::Ok { value, record } => {
            let projected = project_op_value(value, inv, heap)?;
            Ok(settled_ok(heap, &record.op, projected))
        }
        OpOutcome::Failed { failure, record } => settled_err(heap, &record.op, failure),
        OpOutcome::Terminal(terminal) => Err(terminal_error(terminal)),
    }
}
