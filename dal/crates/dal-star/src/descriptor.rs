#![expect(unsafe_code, reason = "starlark custom-value derives emit unsafe impl for Trace/Freeze/ProvidesStaticType; required by spec R05/P02")]

//! v1 descriptor values: the objects `dal.*` constructors return (spec §P01,
//! §P02, §R02).
//!
//! Every descriptor copies its collections into Rust-owned data at
//! construction — a mutated source dictionary or list cannot change a
//! descriptor afterwards (T-P02). Callables stay as `V`, so freezing a
//! module keeps the handler alive with the heap that defined it (T-P03).
//! `dal.err` mints only domain codes: the reserved host `FailureCode` names
//! cannot be minted from script.
//!
//! Absent optional inputs flatten to `None` at the Starlark boundary rather
//! than `Option<V>`: `Option<FrozenValue>` has no `Coerce` impl, so a
//! nullable slot is a `V` that reads `Value::new_none()`. `validate.rs` and
//! the SDK constructors interpret `is_none()` as "not supplied".

use allocative::Allocative;
use starlark::{
    collections::SmallMap,
    coerce::Coerce,
    starlark_complex_value, starlark_simple_value,
    values::{
        Freeze, NoSerialize, ProvidesStaticType, StarlarkValue, Trace,
        ValueLifetimeless, ValueLike,
    },
};

/// Reserved `FailureCode` names a script may not mint through `dal.err`.
/// Mirrors `dal_agent::ext::script::outcome::FailureCode::as_str` (the
/// canonical list lives above this crate's dependency direction); the
/// `reserved_codes` parity test below pins each spelling.
const RESERVED_CODES: &[&str] = &[
    "failed",
    "exit_nonzero",
    "unavailable",
    "conflict",
    "busy",
    "cancelled",
    "observation_unavailable",
    "invocation_mismatch",
    "indeterminate",
];

/// True when `code` names a host-reserved failure code.
pub(crate) fn is_reserved_code(code: &str) -> bool {
    RESERVED_CODES.contains(&code)
}

/// `dal.plugin(...)`: the published descriptor root.
#[repr(C)]
#[derive(Debug, Trace, Coerce, Freeze, ProvidesStaticType, NoSerialize, Allocative)]
pub(crate) struct PluginValueGen<V: ValueLifetimeless> {
    /// Plugin name (`[a-z][a-z0-9_-]{0,63}`).
    pub(crate) name: String,
    /// SemVer string.
    pub(crate) version: String,
    /// State schema version; nonzero.
    pub(crate) state_version: u32,
    /// The `dal.schema(...)` value for `[plugin.<name>]`, or `None`.
    pub(crate) config: V,
    /// Tool map, `local name -> ToolValue`.
    pub(crate) tools: SmallMap<String, V>,
    /// Command map, `command name -> CommandValue`.
    pub(crate) commands: SmallMap<String, V>,
    /// Hooks in descriptor order.
    pub(crate) hooks: Vec<V>,
    /// Skill map.
    pub(crate) skills: SmallMap<String, V>,
    /// Rule map.
    pub(crate) rules: SmallMap<String, V>,
    /// Model map.
    pub(crate) models: SmallMap<String, V>,
    /// The optional prompt section.
    pub(crate) prompt: Option<String>,
}

starlark_complex_value!(pub(crate) PluginValue);

impl<'v, V: ValueLike<'v>> std::fmt::Display for PluginValueGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "dal.plugin(name={:?}, version={:?})", self.name, self.version)
    }
}

#[starlark::values::starlark_value(type = "plugin")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for PluginValueGen<V>
where
    Self: ProvidesStaticType<'v>,
{
}

/// `dal.tool(...)`: a model-visible operation.
#[repr(C)]
#[derive(Debug, Trace, Coerce, Freeze, ProvidesStaticType, NoSerialize, Allocative)]
pub(crate) struct ToolValueGen<V: ValueLifetimeless> {
    /// One-line description shown to the model.
    pub(crate) description: String,
    /// The `dal.schema(...)` value for `run`'s `args` record.
    pub(crate) input: V,
    /// The optional output schema value, or `None`.
    pub(crate) output: V,
    /// Declared operation ids (`uses`).
    pub(crate) uses: Vec<String>,
    /// Visibility: `"model"` or a restricted tier.
    pub(crate) visibility: String,
    /// The `run(ctx, args)` callable.
    pub(crate) run: V,
}

starlark_complex_value!(pub(crate) ToolValue);

impl<'v, V: ValueLike<'v>> std::fmt::Display for ToolValueGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "dal.tool(description={:?})", self.description)
    }
}

#[starlark::values::starlark_value(type = "tool")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for ToolValueGen<V>
where
    Self: ProvidesStaticType<'v>,
{
}

/// `dal.command(...)`: a slash-command binding over a tool descriptor.
#[repr(C)]
#[derive(Debug, Trace, Coerce, Freeze, ProvidesStaticType, NoSerialize, Allocative)]
pub(crate) struct CommandValueGen<V: ValueLifetimeless> {
    /// The bound tool descriptor.
    pub(crate) tool: V,
    /// Positional schema-field names, in order.
    pub(crate) positional: Vec<String>,
    /// Optional override description (defaults to the tool's).
    pub(crate) description: Option<String>,
}

starlark_complex_value!(pub(crate) CommandValue);

impl<'v, V: ValueLike<'v>> std::fmt::Display for CommandValueGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("dal.command(...)")
    }
}

#[starlark::values::starlark_value(type = "command")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for CommandValueGen<V>
where
    Self: ProvidesStaticType<'v>,
{
}

/// `dal.on(event, run, uses=[...])`: a lifecycle hook.
#[repr(C)]
#[derive(Debug, Trace, Coerce, Freeze, ProvidesStaticType, NoSerialize, Allocative)]
pub(crate) struct HookValueGen<V: ValueLifetimeless> {
    /// One of the nine Starlark-visible events.
    pub(crate) event: String,
    /// The handler callable.
    pub(crate) run: V,
    /// Declared operation ids for non-pure hooks.
    pub(crate) uses: Vec<String>,
}

starlark_complex_value!(pub(crate) HookValue);

impl<'v, V: ValueLike<'v>> std::fmt::Display for HookValueGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "dal.on({:?}, ...)", self.event)
    }
}

#[starlark::values::starlark_value(type = "hook")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for HookValueGen<V>
where
    Self: ProvidesStaticType<'v>,
{
}

/// `dal.skill(description, path, letter2image=False)`.
#[derive(Debug, Clone, ProvidesStaticType, NoSerialize, Allocative)]
pub(crate) struct SkillValue {
    /// One-line description.
    pub(crate) description: String,
    /// Plugin-relative `SKILL.md` path.
    pub(crate) path: String,
    /// Whether the skill may carry the image-letter form.
    pub(crate) letter2image: bool,
}

starlark_simple_value!(SkillValue);

impl std::fmt::Display for SkillValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "dal.skill(path={:?})", self.path)
    }
}

#[starlark::values::starlark_value(type = "skill")]
impl<'v> StarlarkValue<'v> for SkillValue {}

/// `dal.rule(pattern, text, judge=None)`.
#[repr(C)]
#[derive(Debug, Trace, Coerce, Freeze, ProvidesStaticType, NoSerialize, Allocative)]
pub(crate) struct RuleValueGen<V: ValueLifetimeless> {
    /// The compiled trigger pattern source.
    pub(crate) pattern: String,
    /// The rule text.
    pub(crate) text: String,
    /// Optional judge callable, or `None`.
    pub(crate) judge: V,
}

starlark_complex_value!(pub(crate) RuleValue);

impl<'v, V: ValueLike<'v>> std::fmt::Display for RuleValueGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "dal.rule(pattern={:?})", self.pattern)
    }
}

#[starlark::values::starlark_value(type = "rule")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for RuleValueGen<V>
where
    Self: ProvidesStaticType<'v>,
{
}

/// `dal.model(caps, run, uses=[...])`.
#[repr(C)]
#[derive(Debug, Trace, Coerce, Freeze, ProvidesStaticType, NoSerialize, Allocative)]
pub(crate) struct ModelValueGen<V: ValueLifetimeless> {
    /// Model capabilities snapshot (the core record value).
    pub(crate) caps: V,
    /// The inference callable.
    pub(crate) run: V,
    /// Declared operation ids.
    pub(crate) uses: Vec<String>,
}

starlark_complex_value!(pub(crate) ModelValue);

impl<'v, V: ValueLike<'v>> std::fmt::Display for ModelValueGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("dal.model(...)")
    }
}

#[starlark::values::starlark_value(type = "model_descriptor")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for ModelValueGen<V>
where
    Self: ProvidesStaticType<'v>,
{
}

/// `dal.ok(value)` / `dal.err(code, message, details)` marker.
#[repr(C)]
#[derive(Debug, Trace, Coerce, Freeze, ProvidesStaticType, NoSerialize, Allocative)]
pub(crate) struct DomainOutcomeGen<V: ValueLifetimeless> {
    /// `true` for `dal.ok`, `false` for `dal.err`.
    pub(crate) ok: bool,
    /// The payload value for `dal.ok`; unused for `dal.err`.
    pub(crate) value: V,
    /// `dal.err` code (domain namespace only; host codes are rejected).
    pub(crate) code: Option<String>,
    /// `dal.err` message.
    pub(crate) message: Option<String>,
    /// `dal.err` structured details, or `None`.
    pub(crate) details: V,
}

starlark_complex_value!(pub(crate) DomainOutcome);

impl<'v, V: ValueLike<'v>> std::fmt::Display for DomainOutcomeGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.ok {
            f.write_str("dal.ok(...)")
        } else {
            write!(f, "dal.err({:?})", self.code.as_deref().unwrap_or(""))
        }
    }
}

#[starlark::values::starlark_value(type = "outcome")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for DomainOutcomeGen<V>
where
    Self: ProvidesStaticType<'v>,
{
}

/// `dal.output(value, view)` — a value plus a safe display tree.
#[repr(C)]
#[derive(Debug, Trace, Coerce, Freeze, ProvidesStaticType, NoSerialize, Allocative)]
pub(crate) struct OutputValueGen<V: ValueLifetimeless> {
    /// The semantic payload.
    pub(crate) value: V,
    /// The display tree root.
    pub(crate) view: V,
}

starlark_complex_value!(pub(crate) OutputValue);

impl<'v, V: ValueLike<'v>> std::fmt::Display for OutputValueGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("dal.output(...)")
    }
}

#[starlark::values::starlark_value(type = "output")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for OutputValueGen<V>
where
    Self: ProvidesStaticType<'v>,
{
}


#[cfg(test)]
mod tests {
    use dal_agent::ext::script::FailureCode;

    use super::is_reserved_code;

    #[test]
    fn reserved_codes_match_the_host_failure_codes() {
        for code in [
            FailureCode::Failed,
            FailureCode::ExitNonZero,
            FailureCode::Unavailable,
            FailureCode::Conflict,
            FailureCode::Busy,
            FailureCode::Cancelled,
            FailureCode::ObservationUnavailable,
            FailureCode::InvocationMismatch,
            FailureCode::Indeterminate,
        ] {
            assert!(
                is_reserved_code(code.as_str()),
                "host code `{}` must be reserved",
                code.as_str()
            );
        }
        assert!(
            !is_reserved_code(FailureCode::Domain("custom".into()).as_str()),
            "domain codes are the script-mintable set"
        );
    }
}
