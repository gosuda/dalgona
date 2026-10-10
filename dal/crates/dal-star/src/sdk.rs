#![expect(unsafe_code, reason = "starlark value derives")]
#![expect(
    clippy::unnecessary_wraps,
    reason = "the starlark_module macro requires registered functions to return Result"
)]

//! The `dal` SDK namespace and the virtual `@dal/v1` module (spec §P01, §P02).
//!
//! `load("@dal/v1", "dal")` binds a `DalNamespace` value built once and frozen
//! into a process-wide `FrozenModule`. Its members mint the descriptor types
//! of `descriptor.rs` and the schema field types of `schema.rs`. Constructors
//! copy their collections at call time (T-P02); the exported `plugin` value is
//! the only thing a v1 module publishes, so nothing else registers by ambient
//! side effect.
//!
//! `dal.MISSING` is the omission sentinel for `optional` fields. It lives
//! here, not in `record.rs`, because scripts name it `dal.MISSING` and
//! `args.<field>` fills absent `optional` fields with the same value.

use std::sync::OnceLock;

use allocative::Allocative;
use starlark::{
    collections::SmallMap,
    environment::{Methods, MethodsBuilder, Module},
    eval::Evaluator,
    starlark_module, starlark_simple_value,
    values::{
        NoSerialize, ProvidesStaticType, StarlarkValue, Value, ValueLike, float::UnpackFloat,
        list::UnpackList,
    },
};

use crate::{
    descriptor::{
        CommandValue, CommandValueGen, DomainErr, DomainErrGen, DomainOk, DomainOkGen, HookValue,
        HookValueGen, ModelValue, ModelValueGen, OutputValue, OutputValueGen, PluginValue,
        PluginValueGen, RuleValue, RuleValueGen, SkillValue, ToolValue, ToolValueGen,
        is_reserved_code,
    },
    record::Missing,
    schema::{Field, Presence, Schema},
    value::Value as Transport,
};

/// The canonical virtual load path for the v1 SDK.
pub(crate) const DAL_V1: &str = "@dal/v1";

/// One normalized field spec produced by `dal.string()` and friends.
///
/// `ty` is the field schema; `presence` is `Required` unless a default was
/// given or `optional`/`nullable` wrapped it. `dal.schema(**fields)` turns a
/// map of these into a `Schema::Object`.
#[derive(Debug, Clone, ProvidesStaticType, NoSerialize, Allocative)]
pub(crate) struct FieldSpecValue {
    /// The field schema.
    pub(crate) ty: Schema,
    /// The presence rule.
    pub(crate) presence: Presence,
}

starlark_simple_value!(FieldSpecValue);

impl std::fmt::Display for FieldSpecValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "dal.type(presence={})", self.presence)
    }
}

#[starlark::values::starlark_value(type = "field_spec")]
#[expect(
    clippy::elidable_lifetime_names,
    reason = "StarlarkValue requires implementations for every value lifetime"
)]
impl<'v> StarlarkValue<'v> for FieldSpecValue {}

/// A complete `dal.schema(...)` object schema as a Starlark value.
#[derive(Debug, Clone, ProvidesStaticType, NoSerialize, Allocative)]
pub(crate) struct SchemaValue {
    /// The normalized object schema.
    pub(crate) schema: Schema,
}

starlark_simple_value!(SchemaValue);

impl std::fmt::Display for SchemaValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("dal.schema(...)")
    }
}

#[starlark::values::starlark_value(type = "schema")]
#[expect(
    clippy::elidable_lifetime_names,
    reason = "StarlarkValue requires implementations for every value lifetime"
)]
impl<'v> StarlarkValue<'v> for SchemaValue {}

/// True when `value` downcasts to `SchemaValue`, frozen or not.
pub(crate) fn is_schema(value: Value<'_>) -> bool {
    if let Some(frozen) = value.unpack_frozen() {
        ValueLike::downcast_ref::<SchemaValue>(frozen.to_value()).is_some()
    } else {
        ValueLike::downcast_ref::<SchemaValue>(value).is_some()
    }
}

/// The `dal` namespace object exported by `@dal/v1`.
#[derive(Debug, Clone, Copy, ProvidesStaticType, NoSerialize, Allocative)]
pub(crate) struct DalNamespace;

starlark_simple_value!(DalNamespace);

impl std::fmt::Display for DalNamespace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("dal")
    }
}

#[starlark::values::starlark_value(type = "dal")]
#[expect(
    clippy::elidable_lifetime_names,
    reason = "StarlarkValue requires implementations for every value lifetime"
)]
impl<'v> StarlarkValue<'v> for DalNamespace {
    fn get_methods() -> Option<&'static Methods> {
        Some(DAL_METHODS.methods())
    }
}

starlark::methods_static!(DAL_METHODS = dal_methods);

use crate::error::api_error;

/// Decodes a `default =` argument into the transport form.
fn default_value(
    name: &str,
    ty: &Schema,
    default: Option<Value<'_>>,
) -> starlark::Result<Presence> {
    let Some(raw) = default else {
        return Ok(Presence::Required);
    };
    let transport = Transport::from_starlark(raw)
        .map_err(|error| api_error(format!("{name}: default is not transport data: {error}")))?;
    if ty.validate(&transport, name).is_err() {
        return Err(api_error(format!(
            "{name}: default does not satisfy its type"
        )));
    }
    Ok(Presence::Default(transport))
}

/// Reads the `(ty, presence)` pair of a field spec value.
fn field_spec_of(value: Value<'_>) -> starlark::Result<(Schema, Presence)> {
    if let Some(spec) = ValueLike::downcast_ref::<FieldSpecValue>(value) {
        return Ok((spec.ty.clone(), spec.presence.clone()));
    }
    if let Some(schema) = ValueLike::downcast_ref::<SchemaValue>(value) {
        return Ok((schema.schema.clone(), Presence::Required));
    }
    Err(api_error("expected a dal type value"))
}

/// The set of valid hook event names (P05's closed table, minus the
/// Rust-only `output_stream`).
const HOOK_EVENTS: &[&str] = &[
    "session_start",
    "session_end",
    "input",
    "before_turn",
    "before_request",
    "tool_call",
    "tool_result",
    "turn_end",
    "settled",
];

#[starlark_module]
fn dal_methods(builder: &mut MethodsBuilder) {
    /// `dal.MISSING`: the native omission sentinel for `optional` fields.
    #[starlark(attribute)]
    fn MISSING(this: &DalNamespace) -> starlark::Result<Missing> {
        Ok(Missing)
    }

    /// `dal.schema(**fields)`: compile field specs into a strict object schema.
    fn schema<'v>(
        #[starlark(this)] _this: &DalNamespace,
        #[starlark(kwargs)] fields: SmallMap<String, Value<'v>>,
    ) -> starlark::Result<SchemaValue> {
        let mut compiled = Vec::with_capacity(fields.len());
        for (name, value) in &fields {
            let (ty, presence) = field_spec_of(*value)?;
            compiled.push(Field {
                name: name.as_str().into(),
                ty,
                presence,
            });
        }
        let schema = Schema::Object(compiled.into_boxed_slice());
        schema
            .check("")
            .map_err(|error| api_error(error.to_string()))?;
        Ok(SchemaValue { schema })
    }

    /// `dal.string(min_len=..., max_len=..., default=...)`.
    fn string<'v>(
        #[starlark(this)] _this: &DalNamespace,
        #[starlark(require = named)] min_len: Option<i64>,
        #[starlark(require = named)] max_len: Option<i64>,
        #[starlark(require = named)] default: Option<Value<'v>>,
    ) -> starlark::Result<FieldSpecValue> {
        let to_len = |n: i64, role: &str| -> starlark::Result<usize> {
            usize::try_from(n).map_err(|_| api_error(format!("string: {role} {n} is negative")))
        };
        let (min_len, max_len) = match (min_len, max_len) {
            (None, None) => (None, None),
            (a, b) => (
                a.map(|n| to_len(n, "min_len")).transpose()?,
                b.map(|n| to_len(n, "max_len")).transpose()?,
            ),
        };
        let ty = Schema::Str { min_len, max_len };
        let presence = default_value("string", &ty, default)?;
        Ok(FieldSpecValue { ty, presence })
    }

    /// `dal.integer(min=..., max=..., default=...)`.
    fn integer<'v>(
        #[starlark(this)] _this: &DalNamespace,
        #[starlark(require = named)] min: Option<i64>,
        #[starlark(require = named)] max: Option<i64>,
        #[starlark(require = named)] default: Option<Value<'v>>,
    ) -> starlark::Result<FieldSpecValue> {
        let ty = Schema::Int { min, max };
        let presence = default_value("integer", &ty, default)?;
        Ok(FieldSpecValue { ty, presence })
    }

    /// `dal.number(min=..., max=..., default=...)`.
    fn number<'v>(
        #[starlark(this)] _this: &DalNamespace,
        #[starlark(require = named)] min: Option<UnpackFloat>,
        #[starlark(require = named)] max: Option<UnpackFloat>,
        #[starlark(require = named)] default: Option<Value<'v>>,
    ) -> starlark::Result<FieldSpecValue> {
        let ty = Schema::Num {
            min: min.map(|bound| bound.0),
            max: max.map(|bound| bound.0),
        };
        let presence = default_value("number", &ty, default)?;
        Ok(FieldSpecValue { ty, presence })
    }

    /// `dal.boolean(default=...)`.
    fn boolean<'v>(
        #[starlark(this)] _this: &DalNamespace,
        #[starlark(require = named)] default: Option<Value<'v>>,
    ) -> starlark::Result<FieldSpecValue> {
        let ty = Schema::Bool;
        let presence = default_value("boolean", &ty, default)?;
        Ok(FieldSpecValue { ty, presence })
    }

    /// `dal.enum(values, default=...)`: a fixed set of strings.
    fn r#enum<'v>(
        #[starlark(this)] _this: &DalNamespace,
        values: UnpackList<String>,
        #[starlark(require = named)] default: Option<Value<'v>>,
    ) -> starlark::Result<FieldSpecValue> {
        let ty = Schema::Enum(
            values
                .items
                .into_iter()
                .map(String::into_boxed_str)
                .collect(),
        );
        let presence = default_value("enum", &ty, default)?;
        Ok(FieldSpecValue { ty, presence })
    }

    /// `dal.list(item, min_len=..., max_len=...)`: a sequence of `item`.
    fn list<'v>(
        #[starlark(this)] _this: &DalNamespace,
        item: Value<'v>,
        #[starlark(require = named)] min_len: Option<i64>,
        #[starlark(require = named)] max_len: Option<i64>,
    ) -> starlark::Result<FieldSpecValue> {
        let (item_ty, presence) = field_spec_of(item)?;
        if !matches!(presence, Presence::Required) {
            return Err(api_error(
                "list: item must be a bare type (no default or optional)",
            ));
        }
        let to_len = |n: i64, role: &str| -> starlark::Result<usize> {
            usize::try_from(n).map_err(|_| api_error(format!("list: {role} {n} is negative")))
        };
        let ty = Schema::List {
            item: Box::new(item_ty),
            min_len: min_len.map(|n| to_len(n, "min_len")).transpose()?,
            max_len: max_len.map(|n| to_len(n, "max_len")).transpose()?,
        };
        Ok(FieldSpecValue {
            ty,
            presence: Presence::Required,
        })
    }

    /// `dal.optional(T)`: the field may be omitted; args carry `MISSING`.
    fn optional<'v>(
        #[starlark(this)] _this: &DalNamespace,
        ty: Value<'v>,
    ) -> starlark::Result<FieldSpecValue> {
        let (ty, presence) = field_spec_of(ty)?;
        match presence {
            Presence::Required => Ok(FieldSpecValue {
                ty,
                presence: Presence::Optional,
            }),
            Presence::Default(_) => Err(api_error(
                "optional with a default is rejected: use the type's default argument",
            )),
            _ => Err(api_error("optional: argument already has a presence rule")),
        }
    }

    /// `dal.nullable(T)`: the field is required but accepts `None`.
    fn nullable<'v>(
        #[starlark(this)] _this: &DalNamespace,
        ty: Value<'v>,
    ) -> starlark::Result<FieldSpecValue> {
        let (ty, presence) = field_spec_of(ty)?;
        match presence {
            Presence::Required => Ok(FieldSpecValue {
                ty,
                presence: Presence::Nullable,
            }),
            Presence::Default(default) => Ok(FieldSpecValue {
                ty,
                presence: Presence::NullableDefault(default),
            }),
            _ => Err(api_error("nullable: argument already has a presence rule")),
        }
    }

    /// `dal.plugin(name, version, ...)`: the published descriptor root.
    #[expect(clippy::too_many_arguments, reason = "spec P02 names ten keywords")]
    fn plugin<'v>(
        #[starlark(this)] _this: &DalNamespace,
        eval: &mut Evaluator<'v, '_, '_>,
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] version: String,
        #[starlark(require = named)] inject: Option<UnpackList<String>>,
        #[starlark(require = named)] config: Option<Value<'v>>,
        #[starlark(require = named)] state_version: Option<i64>,
        #[starlark(require = named)] tools: Option<SmallMap<String, Value<'v>>>,
        #[starlark(require = named)] commands: Option<SmallMap<String, Value<'v>>>,
        #[starlark(require = named)] hooks: Option<UnpackList<Value<'v>>>,
        #[starlark(require = named)] skills: Option<SmallMap<String, Value<'v>>>,
        #[starlark(require = named)] rules: Option<SmallMap<String, Value<'v>>>,
        #[starlark(require = named)] models: Option<SmallMap<String, Value<'v>>>,
        #[starlark(require = named)] prompt: Option<String>,
    ) -> starlark::Result<PluginValue<'v>> {
        let state_version = u32::try_from(state_version.unwrap_or(1))
            .map_err(|_| api_error("plugin: state_version must be a positive integer"))?;
        if state_version == 0 {
            return Err(api_error("plugin: state_version must be nonzero"));
        }
        let site = eval.call_stack_top_location().map(|span| {
            let position = span.as_ref().resolve_span().begin;
            (
                span.file.filename().to_owned(),
                u32::try_from(position.line + 1).unwrap_or(u32::MAX),
                u32::try_from(position.column + 1).unwrap_or(u32::MAX),
            )
        });
        Ok(PluginValueGen {
            name,
            version,
            site,
            inject: inject.unwrap_or_default().items,
            state_version,
            config: config.unwrap_or_else(Value::new_none),
            tools: tools.unwrap_or_default(),
            commands: commands.unwrap_or_default(),
            hooks: hooks.unwrap_or_default().items,
            skills: skills.unwrap_or_default(),
            rules: rules.unwrap_or_default(),
            models: models.unwrap_or_default(),
            prompt,
        })
    }

    /// `dal.tool(description, input, run, ...)`: a model-visible operation.
    fn tool<'v>(
        #[starlark(this)] _this: &DalNamespace,
        #[starlark(require = named)] description: String,
        #[starlark(require = named)] input: Value<'v>,
        #[starlark(require = named)] run: Value<'v>,
        #[starlark(require = named)] uses: Option<UnpackList<String>>,
        #[starlark(require = named)] output: Option<Value<'v>>,
        #[starlark(require = named)] visibility: Option<String>,
    ) -> starlark::Result<ToolValue<'v>> {
        if !is_schema(input) {
            return Err(api_error("tool: input must be a dal.schema value"));
        }
        let visibility = visibility.unwrap_or_else(|| "model".to_owned());
        if dal_core::Visibility::parse(&visibility).is_err() {
            return Err(api_error(
                "tool: visibility must be one of model, deferred, eval_only",
            ));
        }
        Ok(ToolValueGen {
            description,
            input,
            output: output.unwrap_or_else(Value::new_none),
            uses: uses.unwrap_or_default().items,
            visibility,
            run,
        })
    }

    /// `dal.command(tool, positional, description)`.
    fn command<'v>(
        #[starlark(this)] _this: &DalNamespace,
        #[starlark(require = named)] tool: Value<'v>,
        #[starlark(require = named)] positional: Option<UnpackList<String>>,
        #[starlark(require = named)] description: Option<String>,
    ) -> starlark::Result<CommandValue<'v>> {
        Ok(CommandValueGen {
            tool,
            positional: positional.unwrap_or_default().items,
            description,
        })
    }

    /// `dal.on(event, run, uses=[...])`: a lifecycle hook.
    fn on<'v>(
        #[starlark(this)] _this: &DalNamespace,
        event: String,
        run: Value<'v>,
        #[starlark(require = named)] uses: Option<UnpackList<String>>,
    ) -> starlark::Result<HookValue<'v>> {
        if !HOOK_EVENTS.contains(&event.as_str()) {
            return Err(api_error(format!(
                "on: unknown event `{event}`; one of {}",
                HOOK_EVENTS.join(", ")
            )));
        }
        Ok(HookValueGen {
            event,
            run,
            uses: uses.unwrap_or_default().items,
        })
    }

    /// `dal.skill(description, path, letter2image=False)`.
    fn skill<'v>(
        #[starlark(this)] _this: &DalNamespace,
        #[starlark(require = named)] description: String,
        #[starlark(require = named)] path: String,
        #[starlark(require = named)] letter2image: Option<bool>,
    ) -> starlark::Result<SkillValue> {
        Ok(SkillValue {
            description,
            path,
            letter2image: letter2image.unwrap_or(false),
        })
    }

    /// `dal.rule(text, pattern=None, judge=None, always_apply=False,
    /// scope=None, interrupt_mode=None, repeat_mode=None, repeat_gap=None)`;
    /// a rule needs a pattern unless it always applies.
    fn rule<'v>(
        #[starlark(this)] _this: &DalNamespace,
        #[starlark(require = named)] pattern: Option<String>,
        #[starlark(require = named)] text: String,
        #[starlark(require = named)] judge: Option<Value<'v>>,
        #[starlark(require = named)] always_apply: Option<bool>,
        #[starlark(require = named)] scope: Option<UnpackList<String>>,
        #[starlark(require = named)] interrupt_mode: Option<String>,
        #[starlark(require = named)] repeat_mode: Option<String>,
        #[starlark(require = named)] repeat_gap: Option<u32>,
    ) -> starlark::Result<RuleValue<'v>> {
        let always_apply = always_apply.unwrap_or(false);
        if pattern.is_none() && !always_apply {
            return Err(api_error(
                "rule: give a pattern, or set always_apply = True",
            ));
        }
        Ok(RuleValueGen {
            pattern,
            text,
            judge: judge.unwrap_or_else(Value::new_none),
            always_apply,
            scope: scope.map(|scope| scope.items),
            interrupt_mode,
            repeat_mode,
            repeat_gap,
        })
    }

    /// `dal.model(id, caps, run, uses=[...])`.
    fn model<'v>(
        #[starlark(this)] _this: &DalNamespace,
        eval: &mut Evaluator<'v, '_, '_>,
        #[starlark(require = named)] id: String,
        #[starlark(require = named)] caps: Value<'v>,
        #[starlark(require = named)] run: Value<'v>,
        #[starlark(require = named)] uses: Option<UnpackList<String>>,
    ) -> starlark::Result<ModelValue<'v>> {
        let site = eval.call_stack_top_location().map(|span| {
            let position = span.as_ref().resolve_span().begin;
            (
                span.file.filename().to_owned(),
                u32::try_from(position.line + 1).unwrap_or(u32::MAX),
                u32::try_from(position.column + 1).unwrap_or(u32::MAX),
            )
        });
        Ok(ModelValueGen {
            id,
            site,
            caps,
            run,
            uses: uses.unwrap_or_default().items,
        })
    }

    /// `dal.ok(value)`: an explicit domain success.
    fn ok<'v>(
        #[starlark(this)] _this: &DalNamespace,
        value: Value<'v>,
    ) -> starlark::Result<DomainOk<'v>> {
        Ok(DomainOkGen { value })
    }

    /// `dal.err(code, message, details)`: an explicit domain failure.
    fn err<'v>(
        #[starlark(this)] _this: &DalNamespace,
        code: String,
        message: String,
        details: Option<Value<'v>>,
    ) -> starlark::Result<DomainErr<'v>> {
        if is_reserved_code(&code) {
            return Err(api_error(format!(
                "err: `{code}` is a reserved host failure code"
            )));
        }
        Ok(DomainErrGen {
            code,
            message,
            details: details.unwrap_or_else(Value::new_none),
        })
    }

    /// `dal.output(value, view)`: payload plus a safe display tree.
    fn output<'v>(
        #[starlark(this)] _this: &DalNamespace,
        #[starlark(require = named)] value: Value<'v>,
        #[starlark(require = named)] view: Value<'v>,
    ) -> starlark::Result<OutputValue<'v>> {
        Ok(OutputValueGen { value, view })
    }
}

static DAL_V1_MODULE: OnceLock<FreezeResult<starlark::environment::FrozenModule>> = OnceLock::new();

use starlark::values::FreezeResult;

/// The frozen `@dal/v1` module: one `dal` global bound to `DalNamespace`.
///
/// Built once per process; every evaluation shares the same frozen heap, so
/// SDK descriptors created in one module cannot leak into another.
pub(crate) fn dal_v1_module()
-> Result<&'static starlark::environment::FrozenModule, crate::error::LoadError> {
    let result = DAL_V1_MODULE.get_or_init(|| {
        Module::with_temp_heap(|module| {
            let namespace = module.heap().alloc(DalNamespace);
            module.set("dal", namespace);
            module.freeze()
        })
    });
    match result {
        Ok(module) => Ok(module),
        Err(error) => Err(crate::error::LoadError::SdkModule {
            module: DAL_V1.into(),
            message: format!("{error:?}").into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use starlark::{
        environment::{FrozenModule, Module},
        eval::{Evaluator, FileLoader},
    };

    use super::dal_v1_module;
    use crate::engine::{dialect, globals};

    use starlark::syntax::AstModule;
    use starlark::values::ValueLike;

    struct SdkLoader(FrozenModule);

    impl FileLoader for SdkLoader {
        fn load(&self, path: &str) -> starlark::Result<FrozenModule> {
            if path == super::DAL_V1 {
                Ok(self.0.clone())
            } else {
                Err(starlark::Error::new_other(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("no module {path}"),
                )))
            }
        }
    }

    #[test]
    fn dal_v1_module_loads_and_constructs_descriptors() -> Result<(), Box<dyn std::error::Error>> {
        let sdk = dal_v1_module()?;
        let source = r#"
load("@dal/v1", "dal")

todos = dal.tool(
    description = "Find TODO comments under a path.",
    input = dal.schema(
        path = dal.string(default = "."),
        limit = dal.integer(default = 20, min = 1, max = 200),
    ),
    uses = ["tools.search"],
    run = lambda ctx, args: None,
)

plugin = dal.plugin(
    name = "quality",
    version = "0.1.0",
    tools = {"todos": todos},
    commands = {"todos": dal.command(tool = todos, positional = ["path"])},
)
"#;
        let ast = AstModule::parse("plugin.star", source.to_owned(), &dialect())
            .map_err(|e| e.to_string())?;
        let loader = SdkLoader(sdk.clone());
        Module::with_temp_heap(|module| {
            let mut evaluator = Evaluator::new(&module);
            evaluator.set_loader(&loader);
            evaluator
                .eval_module(ast, &globals())
                .map_err(|e| e.to_string())?;
            let plugin = module.get("plugin").expect("plugin export");
            let descriptor = ValueLike::downcast_ref::<
                super::PluginValueGen<starlark::values::Value>,
            >(plugin.to_value())
            .expect("plugin must be a PluginValue");
            assert_eq!(descriptor.name, "quality");
            assert_eq!(descriptor.version, "0.1.0");
            assert_eq!(descriptor.state_version, 1);
            assert_eq!(descriptor.tools.len(), 1);
            assert_eq!(descriptor.commands.len(), 1);
            Ok(())
        })
    }

    #[test]
    fn dal_err_rejects_reserved_host_codes() -> Result<(), Box<dyn std::error::Error>> {
        let sdk = dal_v1_module()?;
        let source = r#"
load("@dal/v1", "dal")
outcome = dal.err("unavailable", "boom", None)
"#;
        let ast = AstModule::parse("plugin.star", source.to_owned(), &dialect())
            .map_err(|e| e.to_string())?;
        let loader = SdkLoader(sdk.clone());
        let result: Result<(), starlark::Error> = Module::with_temp_heap(|module| {
            let mut evaluator = Evaluator::new(&module);
            evaluator.set_loader(&loader);
            evaluator.eval_module(ast, &globals()).map(|_| ())
        });
        let error = result
            .map_err(|error| error.to_string())
            .expect_err("reserved code must fail");
        assert!(error.contains("reserved host failure code"));
        Ok(())
    }
}
