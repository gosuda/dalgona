//! The Validate stage of the plugin load pipeline (spec §P02, §P03, §R02).
//!
//! `plugin.star` evaluation produces one exported `plugin` descriptor plus a
//! frozen module. This stage walks that descriptor exactly once and produces
//! the [`LoadedPlugin`] the generation publishes: every reachable export,
//! command, hook, skill, rule and model with normalized names, parsed `uses`
//! sets, command field bindings and resolved asset bodies. Reachability is the
//! only registration path: a descriptor never attached to the `plugin` value
//! publishes nothing.
//!
//! There is no partial publish: the first rejection fails the whole candidate
//! generation, matching the `Failed` state of §P03.

use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dal_core::ext::{ExportId, ExportKind, OpId, OpSet, Phase};
use dal_core::ext::{Scope, decode_skill_mcp};
use dal_core::{
    Caps, CommandSpec, HookEvent, ModelId, Name, Origin, RawJson, RuleRecord, ServiceSet, Site,
    SkillRecord,
};
use starlark::values::{OwnedFrozenValue, Value, ValueLike};

use crate::descriptor::{
    CommandValue, HookValue, ModelValue, PluginValue, RuleValue, SkillValue, ToolValue,
};
use crate::engine::{Limits, MAX_CODE};
use crate::error::LoadError;
use crate::schema::{Presence, Schema, SchemaError};
use crate::sdk::SchemaValue;

/// One validated plugin ready for the generation.
#[derive(Debug)]
pub(crate) struct LoadedPlugin {
    /// The checked plugin name; matches its directory.
    pub(crate) name: Name,
    /// The declared `SemVer` version.
    pub(crate) version: Box<str>,
    /// Bundled or user-supplied.
    pub(crate) origin: Origin,
    /// The host services explicitly requested by the plugin.
    pub(crate) inject: ServiceSet,
    /// The state schema version; nonzero by constructor.
    #[expect(
        dead_code,
        reason = "the host state namespace has no API that accepts a version yet"
    )]
    pub(crate) state_version: NonZeroU32,
    /// The normalized config value, decoded from the host-supplied config
    /// object and validated against `config`; `Null` when absent.
    pub(crate) config_value: crate::value::Value,
    /// The `plugin.star` site, used as the diagnostic anchor.
    pub(crate) site: Site,
    /// The reachable exports in identity order.
    pub(crate) exports: Box<[Export]>,
    /// Slash commands keyed by checked command name.
    pub(crate) commands: Box<[CommandDecl]>,
    /// Lifecycle hooks in descriptor order.
    pub(crate) hooks: Box<[HookDecl]>,
    /// Resolved skill records.
    pub(crate) skills: Box<[SkillRecord]>,
    /// Rule declarations ready for the rules owner.
    pub(crate) rules: Box<[RuleRecord]>,
    /// Scripted model routes keyed by local name.
    pub(crate) models: Box<[ModelDecl]>,
    /// The optional prompt section.
    pub(crate) prompt: Option<Box<str>>,
    /// The immutable `plugin` descriptor; handlers resolve their callable
    /// through this owner, so frozen callables stay alive with their module.
    #[expect(
        dead_code,
        reason = "owns the frozen heap that every stored callable points into"
    )]
    pub(crate) plugin: OwnedFrozenValue,
    /// The per-handler evaluation budget.
    pub(crate) limits: Limits,
}

/// One reachable export entry (§R02): a tool or a scripted model route.
#[derive(Debug)]
pub(crate) struct Export {
    /// The immutable entry identity.
    pub(crate) id: ExportId,
    /// The declared operation set `D`.
    pub(crate) uses: OpSet,
    /// The executable body.
    pub(crate) body: ExportBody,
}

/// The executable half of an export.
#[derive(Debug)]
pub(crate) enum ExportBody {
    /// A model-callable tool.
    Tool {
        /// The normalized input schema.
        schema: Schema,
        /// The optional output schema.
        output: Option<Schema>,
        /// The provider JSON schema, computed once at load.
        input_json: RawJson,
        /// The model-facing description.
        description: Box<str>,
        /// The wire name `<plugin>__<local>`.
        wire: Name,
        /// The declared visibility.
        visibility: dal_core::Visibility,
        /// The `run(ctx, args)` callable, kept as a value so freezing keeps
        /// it alive with its defining module.
        run: starlark::values::FrozenValue,
    },
}

/// One slash-command declaration after positional binding checks.
#[derive(Debug)]
pub(crate) struct CommandDecl {
    /// The entry identity `command.<plugin>.<local>`.
    pub(crate) id: ExportId,
    /// The command spec (name plus summary).
    pub(crate) spec: CommandSpec,
    /// The positional schema-field names in order.
    pub(crate) positional: Box<[Box<str>]>,
    /// The bound tool descriptor; several commands may share one tool.
    pub(crate) tool: ToolDecl,
}

/// The validated data of one tool descriptor, shared by its exports and
/// command bindings.
#[derive(Debug, Clone)]
pub(crate) struct ToolDecl {
    /// The normalized input schema.
    pub(crate) schema: Schema,
    /// The optional output schema.
    pub(crate) output: Option<Schema>,
    /// The provider JSON schema.
    pub(crate) input_json: RawJson,
    /// The description.
    pub(crate) description: Box<str>,
    /// The declared operation set.
    pub(crate) uses: OpSet,
    /// The declared visibility.
    pub(crate) visibility: dal_core::Visibility,
    /// The `run(ctx, args)` callable.
    pub(crate) run: starlark::values::FrozenValue,
}

/// One lifecycle hook after its P05 phase check.
#[derive(Debug)]
pub(crate) struct HookDecl {
    /// The entry identity `hook.<plugin>.<event>-<seq>`.
    pub(crate) id: ExportId,
    /// The subscribed event.
    pub(crate) event: HookEvent,
    /// The declared operation set, already checked against the event's
    /// phase ceiling.
    pub(crate) uses: OpSet,
    /// The handler callable.
    pub(crate) run: starlark::values::FrozenValue,
    /// The order index within the plugin's `hooks` list.
    pub(crate) seq: u32,
}

/// One scripted model route after validation.
#[derive(Debug)]
pub(crate) struct ModelDecl {
    /// The route identity (local key under `models.<plugin>.`).
    pub(crate) id: ExportId,
    /// The public model identifier dispatched by the host.
    pub(crate) model_id: ModelId,
    /// The capability summary decoded to its core type.
    pub(crate) caps: Caps,
    /// The declared operation set.
    pub(crate) uses: OpSet,
    /// The inference callable.
    pub(crate) run: starlark::values::FrozenValue,
}

/// Validates one evaluated plugin module into a [`LoadedPlugin`].
///
/// `plugin_value` is the module's exported `plugin` binding; `dir` is the
/// plugin's display root for diagnostics; `files` supplies skill bodies and
/// `config_json` is the host-resolved `[plugin.<name>]` config object.
pub(crate) fn validate(
    dir: &str,
    name: &str,
    origin: Origin,
    plugin_value: &OwnedFrozenValue,
    files: &std::collections::BTreeMap<PathBuf, Vec<u8>>,
    config_json: Option<&str>,
    limits: Limits,
) -> Result<LoadedPlugin, LoadError> {
    let default_site = Site {
        path: PathBuf::from(dir).join("plugin.star"),
        line: 1,
        col: 1,
    };
    let Some(descriptor) = PluginValue::from_value(plugin_value.value()) else {
        return Err(LoadError::InvalidPlugin {
            path: default_site.path.clone(),
            message: "the `plugin` export is not a dal.plugin(...) descriptor".into(),
        });
    };
    let site = descriptor.site.as_ref().map_or_else(
        || default_site.clone(),
        |(path, line, col)| Site {
            path: PathBuf::from(path),
            line: *line,
            col: *col,
        },
    );
    let declared = Name::parse(&descriptor.name).map_err(|_| LoadError::InvalidPlugin {
        path: site.path.clone(),
        message: format!("name {:?} is not a valid plugin name", descriptor.name).into(),
    })?;
    if declared.as_str() != name {
        return Err(LoadError::NameMismatch {
            dir: name.into(),
            declared: descriptor.name.as_str().into(),
        });
    }
    let inject =
        ServiceSet::from_names(descriptor.inject.iter().map(String::as_str)).map_err(|error| {
            LoadError::Registration {
                path: site.path.clone(),
                line: site.line,
                col: site.col,
                message: format!("plugin inject: {error}").into(),
            }
        })?;
    if descriptor.state_version == 0 {
        return Err(LoadError::InvalidPlugin {
            path: site.path.clone(),
            message: "state_version must be nonzero".into(),
        });
    }
    let state_version =
        NonZeroU32::new(descriptor.state_version).ok_or_else(|| LoadError::InvalidPlugin {
            path: site.path.clone(),
            message: "state_version must be nonzero".into(),
        })?;

    let config = config_schema_of(&site, descriptor.config)?;
    let config_value = validate_config(&site, name, config.as_ref(), config_json)?;

    let exports = validate_tools(&site, &declared, &descriptor.tools)?;
    let commands = validate_commands(&site, &declared, &descriptor.commands)?;
    let hooks = validate_hooks(&site, &declared, &descriptor.hooks)?;
    let skills = validate_skills(&site, &descriptor.skills, files)?;
    let rules = validate_rules(&site, &descriptor.rules)?;
    let models = validate_models(&site, &declared, &descriptor.models)?;

    // Every operation a declaration may issue contributes its service to the
    // plugin's injection manifest: `uses` is the declaration the service
    // boundary reads, the same way an eval cell's ceiling resolves to its
    // services (R03). An explicit `inject` list only ever adds to this.
    let mut inject = inject;
    for uses in exports
        .iter()
        .map(|export| &export.uses)
        .chain(hooks.iter().map(|hook| &hook.uses))
        .chain(models.iter().map(|model| &model.uses))
        .chain(commands.iter().map(|command| &command.tool.uses))
    {
        inject = inject.union(uses.services());
    }

    let prompt = descriptor.prompt.clone().map(String::into_boxed_str);

    Ok(LoadedPlugin {
        name: declared,
        version: descriptor.version.clone().into_boxed_str(),
        origin,
        inject,
        state_version,
        config_value,
        site,
        exports: exports.into_boxed_slice(),
        commands: commands.into_boxed_slice(),
        hooks: hooks.into_boxed_slice(),
        skills: skills.into_boxed_slice(),
        rules: rules.into_boxed_slice(),
        models: models.into_boxed_slice(),
        prompt,
        plugin: plugin_value.clone(),
        limits,
    })
}

fn validate_tools(
    site: &Site,
    declared: &Name,
    entries: &starlark::collections::SmallMap<String, Value<'_>>,
) -> Result<Vec<Export>, LoadError> {
    let mut tools: Vec<(Name, ToolDecl)> = Vec::new();
    let mut seen_tool_values: Vec<starlark::values::FrozenValue> = Vec::new();
    for (local, tool_value) in entries {
        let local = check_local(site, "tool", local)?;
        let tool = tool_decl(site, *tool_value)?;
        let frozen_value = frozen(site, *tool_value)?;
        if seen_tool_values
            .iter()
            .any(|seen| same_value(*seen, frozen_value))
        {
            return Err(LoadError::InvalidPlugin {
                path: site.path.clone(),
                message: format!(
                    "tool `{}` is published under more than one tools key",
                    local.as_str()
                )
                .into(),
            });
        }
        seen_tool_values.push(frozen_value);
        tools.push((local, tool));
    }
    tools.sort_by(|left, right| left.0.cmp(&right.0));

    let mut exports = Vec::new();
    for (local, tool) in &tools {
        let id = ExportId {
            plugin: declared.clone(),
            kind: ExportKind::Tool,
            local: local.clone(),
        };
        let wire =
            Name::parse(&dal_agent::ext::generation::catalog::wire_name(&id)).map_err(|_| {
                LoadError::InvalidPlugin {
                    path: site.path.clone(),
                    message: format!(
                        "wire name for {} exceeds the tool-name grammar",
                        OpId::Export(id.clone())
                    )
                    .into(),
                }
            })?;
        exports.push(Export {
            id,
            uses: tool.uses.clone(),
            body: ExportBody::Tool {
                schema: tool.schema.clone(),
                output: tool.output.clone(),
                input_json: tool.input_json.clone(),
                description: tool.description.clone(),
                wire,
                visibility: tool.visibility,
                run: tool.run,
            },
        });
    }
    Ok(exports)
}

fn validate_commands(
    site: &Site,
    declared: &Name,
    commands: &starlark::collections::SmallMap<String, Value<'_>>,
) -> Result<Vec<CommandDecl>, LoadError> {
    let mut validated = Vec::new();
    for (local, command_value) in commands {
        let local = check_local(site, "command", local)?;
        let command =
            CommandValue::from_value(*command_value).ok_or_else(|| LoadError::InvalidPlugin {
                path: site.path.clone(),
                message: format!("command `{local}` is not a dal.command(...) descriptor").into(),
            })?;
        let tool = tool_decl(site, command.tool)?;
        check_command_bindings(site, local.as_str(), &command.positional, &tool.schema)?;
        let summary = command
            .description
            .clone()
            .unwrap_or_else(|| tool.description.to_string());
        let qualified = format!("{}:{local}", declared.as_str());
        validated.push(CommandDecl {
            id: ExportId {
                plugin: declared.clone(),
                kind: ExportKind::Command,
                local,
            },
            spec: CommandSpec {
                name: dal_core::CommandName::parse(&qualified).map_err(|_| {
                    LoadError::InvalidPlugin {
                        path: site.path.clone(),
                        message: format!("command name `{qualified}` is not a valid command name")
                            .into(),
                    }
                })?,
                summary: summary.into(),
                args_hint: None,
            },
            positional: command
                .positional
                .iter()
                .map(|p| p.as_str().into())
                .collect(),
            tool,
        });
    }
    validated.sort_by(|left, right| left.spec.name.as_str().cmp(right.spec.name.as_str()));
    Ok(validated)
}

fn validate_hooks(
    site: &Site,
    declared: &Name,
    hooks: &[Value<'_>],
) -> Result<Vec<HookDecl>, LoadError> {
    let mut validated = Vec::new();
    for (index, hook_value) in hooks.iter().enumerate() {
        let hook = HookValue::from_value(*hook_value).ok_or_else(|| LoadError::InvalidPlugin {
            path: site.path.clone(),
            message: format!("hooks[{index}] is not a dal.on(...) descriptor").into(),
        })?;
        let event = hook_event(site, &hook.event)?;
        let uses = parse_uses(site, "on", &hook.event, &hook.uses)?;
        for op in uses.iter() {
            if !Phase::Hook(event).permits(&op) {
                return Err(LoadError::Uses {
                    path: site.path.clone(),
                    line: site.line,
                    col: site.col,
                    id: op.to_string().into(),
                    message: format!("event `{}` may not request it under P05", hook.event).into(),
                });
            }
        }
        let local = check_local(site, "hook", &format!("{}-{index}", hook.event))?;
        validated.push(HookDecl {
            id: ExportId {
                plugin: declared.clone(),
                kind: ExportKind::Hook,
                local,
            },
            event,
            uses,
            run: frozen(site, hook.run)?,
            seq: u32::try_from(index).map_err(|_| LoadError::InvalidPlugin {
                path: site.path.clone(),
                message: "hooks exceed u32 range".into(),
            })?,
        });
    }
    Ok(validated)
}

fn validate_skills(
    site: &Site,
    skills: &starlark::collections::SmallMap<String, Value<'_>>,
    files: &std::collections::BTreeMap<PathBuf, Vec<u8>>,
) -> Result<Vec<SkillRecord>, LoadError> {
    let mut validated = Vec::new();
    for (local, skill_value) in skills {
        let local = check_local(site, "skill", local)?;
        let skill =
            SkillValue::from_value(*skill_value).ok_or_else(|| LoadError::InvalidPlugin {
                path: site.path.clone(),
                message: format!("skill `{local}` is not a dal.skill(...) descriptor").into(),
            })?;
        let body = read_asset(site, files, "skill", local.as_str(), &skill.path)?;
        let plugin_dir = site.path.parent().unwrap_or_else(|| Path::new(""));
        let mcp = decode_skill_mcp(&plugin_dir.join(&skill.path), &body)?;
        validated.push(SkillRecord {
            name: local,
            description: skill.description.as_str().into(),
            body: Arc::from(body),
            letter2image: skill.letter2image,
            mcp,
        });
    }
    validated.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(validated)
}

fn validate_rules(
    site: &Site,
    rules: &starlark::collections::SmallMap<String, Value<'_>>,
) -> Result<Vec<RuleRecord>, LoadError> {
    let mut validated = Vec::new();
    for (local, rule_value) in rules {
        let local = check_local(site, "rule", local)?;
        let rule = RuleValue::from_value(*rule_value).ok_or_else(|| LoadError::InvalidPlugin {
            path: site.path.clone(),
            message: format!("rule `{local}` is not a dal.rule(...) descriptor").into(),
        })?;
        let judge = judge_text(site, local.as_str(), rule.judge)?;
        let invalid = |message: String| LoadError::InvalidPlugin {
            path: site.path.clone(),
            message: format!("rule `{local}`: {message}").into(),
        };
        let scope = rule
            .scope
            .as_deref()
            .map(|tokens| rule_scope(tokens).ok_or_else(|| invalid(SCOPE_HINT.to_owned())))
            .transpose()?;
        let mode = rule.interrupt_mode.as_deref().map(|word| rule_word(word).ok_or_else(|| invalid(format!("interrupt_mode \"{word}\" is invalid; use always, prose-only, tool-only, or never")))).transpose()?;
        let repeat_mode = rule
            .repeat_mode
            .as_deref()
            .map(|word| {
                rule_word(word).ok_or_else(|| {
                    invalid(format!(
                        "repeat_mode \"{word}\" is invalid; use once or after-gap"
                    ))
                })
            })
            .transpose()?;
        let repeat_gap = rule
            .repeat_gap
            .map(|gap| {
                rule_gap(gap).ok_or_else(|| {
                    invalid(format!(
                        "repeat_gap {gap} is invalid; use a whole number from 1 to 1000"
                    ))
                })
            })
            .transpose()?;
        validated.push(RuleRecord {
            name: local,
            patterns: rule
                .pattern
                .iter()
                .map(|pattern| pattern.as_str().into())
                .collect(),
            text: rule.text.as_str().into(),
            judge,
            scope,
            globs: None,
            agents: None,
            mode,
            repeat_mode,
            repeat_gap,
            always_apply: rule.always_apply,
            report: false,
            enabled: true,
        });
    }
    validated.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(validated)
}

/// The reason given for an empty scope or an unknown scope token.
const SCOPE_HINT: &str = "scope needs one or more of text, thinking, tool, or tool:<name>";

/// Builds a rule scope from `text`, `thinking`, `tool`, and `tool:<name>`
/// tokens, or [`None`] when the list is empty or a token is unknown.
fn rule_scope(tokens: &[String]) -> Option<Scope> {
    let mut scope = Scope {
        text: false,
        thinking: false,
        tool: false,
        named_tools: Vec::new(),
    };
    let known = tokens
        .iter()
        .all(|token| add_scope_token(&mut scope, token));
    (known && !tokens.is_empty()).then_some(scope)
}

/// Adds one scope token; a named tool also selects tool arguments.
fn add_scope_token(scope: &mut Scope, token: &str) -> bool {
    match token {
        "text" => scope.text = true,
        "thinking" => scope.thinking = true,
        "tool" => scope.tool = true,
        named => {
            let Some(tool) = named
                .strip_prefix("tool:")
                .and_then(|tool| Name::parse(tool).ok())
            else {
                return false;
            };
            scope.tool = true;
            scope.named_tools.push(tool);
        }
    }
    true
}

/// Decodes one rule policy word through the core type's wire names.
fn rule_word<T: serde::de::DeserializeOwned>(word: &str) -> Option<T> {
    use serde::de::IntoDeserializer as _;
    let decoder: serde::de::value::StrDeserializer<'_, serde::de::value::Error> =
        word.into_deserializer();
    T::deserialize(decoder).ok()
}

/// Accepts a repeat gap from 1 to 1000 deltas.
fn rule_gap(gap: u32) -> Option<u16> {
    u16::try_from(gap)
        .ok()
        .filter(|gap| (1..=1000).contains(gap))
}

fn validate_models(
    site: &Site,
    declared: &Name,
    models: &starlark::collections::SmallMap<String, Value<'_>>,
) -> Result<Vec<ModelDecl>, LoadError> {
    let mut validated = Vec::new();
    for (local, model_value) in models {
        let model = ModelValue::from_value(*model_value).ok_or_else(|| {
            declaration_error(
                site,
                format!("model `{local}` is not a dal.model(...) descriptor"),
            )
        })?;
        let model_site = model.site.as_ref().map_or_else(
            || site.clone(),
            |(path, line, col)| Site {
                path: PathBuf::from(path),
                line: *line,
                col: *col,
            },
        );
        let local = check_local(&model_site, "model", local)?;
        let model_id = ModelId::parse(&model.id).map_err(|error| {
            declaration_error(&model_site, format!("model `{local}` id: {error}"))
        })?;
        let caps = decode_model_caps(&model_site, local.as_str(), model.caps)?;
        let uses = parse_uses(&model_site, "model", local.as_str(), &model.uses)?;
        validated.push(ModelDecl {
            id: ExportId {
                plugin: declared.clone(),
                kind: ExportKind::Model,
                local: local.clone(),
            },
            model_id,
            caps,
            uses,
            run: frozen(&model_site, model.run)?,
        });
    }
    validated.sort_by(|left, right| left.id.local.cmp(&right.id.local));
    Ok(validated)
}

/// Decodes a model capability record without accepting unknown local fields.
fn decode_model_caps(site: &Site, local: &str, value: Value<'_>) -> Result<Caps, LoadError> {
    let transport = crate::value::Value::from_starlark(value)
        .map_err(|error| declaration_error(site, format!("model `{local}` caps: {error}")))?;
    let crate::value::Value::Object(fields) = &transport else {
        return Err(declaration_error(
            site,
            format!("model `{local}` caps must be an object"),
        ));
    };
    for (name, _) in fields {
        if !matches!(
            name.as_ref(),
            "context_window" | "thinking" | "tool_use" | "image_input" | "custom_grammar"
        ) {
            return Err(declaration_error(
                site,
                format!("model `{local}` caps has unknown field `{name}`"),
            ));
        }
    }
    sonic_rs::from_str(&transport.to_json())
        .map_err(|error| declaration_error(site, format!("model `{local}` caps: {error}")))
}

/// Builds a source-located model declaration error.
fn declaration_error(site: &Site, message: impl Into<Box<str>>) -> LoadError {
    LoadError::Registration {
        path: site.path.clone(),
        line: site.line,
        col: site.col,
        message: message.into(),
    }
}

/// Compares two frozen descriptor values by pointer identity.
fn same_value(left: starlark::values::FrozenValue, right: starlark::values::FrozenValue) -> bool {
    left.to_value().ptr_eq(right.to_value())
}

/// Reads the frozen form of a descriptor value or reports an invalid plugin.
fn frozen(site: &Site, value: Value<'_>) -> Result<starlark::values::FrozenValue, LoadError> {
    value
        .unpack_frozen()
        .ok_or_else(|| LoadError::InvalidPlugin {
            path: site.path.clone(),
            message: "validated descriptors are frozen values".into(),
        })
}

/// Validates a map key against the local-name grammar and returns a `Name`.
fn check_local(site: &Site, kind: &str, local: &str) -> Result<Name, LoadError> {
    Name::parse(local).map_err(|_| LoadError::InvalidPlugin {
        path: site.path.clone(),
        message: format!(
            "{kind} name {local:?} is invalid; names must match [a-z][a-z0-9_-]{{0,63}}"
        )
        .into(),
    })
}

/// Extracts the `dal.schema` value behind `dal.plugin(config = …)`.
fn config_schema_of(site: &Site, value: Value<'_>) -> Result<Option<Schema>, LoadError> {
    if value.is_none() {
        return Ok(None);
    }
    let Some(schema) = ValueLike::downcast_ref::<SchemaValue>(value) else {
        return Err(LoadError::Schema {
            path: site.path.clone(),
            message: "plugin config must be a dal.schema(...) value".into(),
        });
    };
    Ok(Some(schema.schema.clone()))
}

/// Validates the host-resolved `[plugin.<name>]` config object.
///
/// The config table arrives as JSON text (`None` when the plugin configures
/// nothing). An absent schema rejects any supplied config; an absent config
/// yields the schema's own defaults or `Null`.
fn validate_config(
    site: &Site,
    name: &str,
    schema: Option<&Schema>,
    config_json: Option<&str>,
) -> Result<crate::value::Value, LoadError> {
    let object = match config_json {
        None => crate::value::Value::Object(Vec::new().into_boxed_slice()),
        Some(text) => crate::value::Value::decode(text).map_err(|error| LoadError::Schema {
            path: site.path.clone(),
            message: format!("config for `{name}`: {error}").into(),
        })?,
    };
    let Some(schema) = schema else {
        return match object {
            crate::value::Value::Null => Ok(crate::value::Value::Null),
            crate::value::Value::Object(ref fields) if fields.is_empty() => {
                Ok(crate::value::Value::Null)
            }
            _ => Err(LoadError::Schema {
                path: site.path.clone(),
                message: format!(
                    "plugin `{name}` declares no config schema but config was supplied"
                )
                .into(),
            }),
        };
    };
    let object = match object {
        crate::value::Value::Null => crate::value::Value::Object(Vec::new().into_boxed_slice()),
        crate::value::Value::Object(_) => object,
        _ => {
            return Err(LoadError::Schema {
                path: site.path.clone(),
                message: format!("config for `{name}` must be an object").into(),
            });
        }
    };
    schema
        .validate(&object, "config")
        .map_err(|error: SchemaError| LoadError::Schema {
            path: site.path.clone(),
            message: format!("config for `{name}`: {error}").into(),
        })
}

/// Validates one `dal.tool` descriptor into a [`ToolDecl`].
fn tool_decl(site: &Site, value: Value<'_>) -> Result<ToolDecl, LoadError> {
    let Some(tool) = ToolValue::from_value(value) else {
        return Err(LoadError::InvalidPlugin {
            path: site.path.clone(),
            message: "expected a dal.tool(...) descriptor".into(),
        });
    };
    let schema = schema_of(site, "tool input", tool.input)?;
    let output = if tool.output.is_none() {
        None
    } else {
        Some(schema_of(site, "tool output", tool.output)?)
    };
    let uses = parse_uses(site, "tool", &tool.description, &tool.uses)?;
    for op in uses.iter() {
        if !Phase::Tool.permits(&op) {
            return Err(LoadError::Uses {
                path: site.path.clone(),
                line: site.line,
                col: site.col,
                id: op.to_string().into(),
                message: "a tool may not request it under P05".into(),
            });
        }
    }
    let visibility = dal_core::Visibility::parse(&tool.visibility).map_err(|error| {
        LoadError::InvalidPlugin {
            path: site.path.clone(),
            message: format!("tool visibility: {error}").into(),
        }
    })?;
    let input_json = schema
        .to_json_schema()
        .map_err(|error: SchemaError| LoadError::Schema {
            path: site.path.clone(),
            message: format!("tool input schema: {error}").into(),
        })?;
    Ok(ToolDecl {
        schema,
        output,
        input_json,
        description: tool.description.as_str().into(),
        uses,
        visibility,
        run: frozen(site, tool.run)?,
    })
}

/// Extracts the normalized schema behind a `dal.schema(...)` value.
fn schema_of(site: &Site, what: &str, value: Value<'_>) -> Result<Schema, LoadError> {
    let Some(schema) = ValueLike::downcast_ref::<SchemaValue>(value) else {
        return Err(LoadError::Schema {
            path: site.path.clone(),
            message: format!("{what} must be a dal.schema(...) value").into(),
        });
    };
    Ok(schema.schema.clone())
}

/// Parses a `uses` string list into an `OpSet`.
fn parse_uses(site: &Site, kind: &str, local: &str, uses: &[String]) -> Result<OpSet, LoadError> {
    OpSet::parse(uses.iter().map(String::as_str)).map_err(|error| LoadError::Uses {
        path: site.path.clone(),
        line: site.line,
        col: site.col,
        id: format!("{kind} `{local}`").into(),
        message: error.to_string().into(),
    })
}

/// Maps a declared event string onto the core `HookEvent`.
fn hook_event(site: &Site, event: &str) -> Result<HookEvent, LoadError> {
    let parsed = match event {
        "session_start" => HookEvent::SessionStart,
        "session_end" => HookEvent::SessionEnd,
        "input" => HookEvent::Input,
        "before_turn" => HookEvent::BeforeTurn,
        "before_request" => HookEvent::BeforeRequest,
        "tool_call" => HookEvent::ToolCall,
        "tool_result" => HookEvent::ToolResult,
        "turn_end" => HookEvent::TurnEnd,
        "settled" => HookEvent::Settled,
        other => {
            return Err(LoadError::InvalidPlugin {
                path: site.path.clone(),
                message: format!("unknown hook event `{other}`").into(),
            });
        }
    };
    Ok(parsed)
}

/// Checks a command's `positional` names against its tool schema (§P04):
/// distinct, existing, scalar, and no required field after an omittable one.
fn check_command_bindings(
    site: &Site,
    command: &str,
    positional: &[String],
    schema: &Schema,
) -> Result<(), LoadError> {
    let Schema::Object(fields) = schema else {
        return Err(LoadError::InvalidPlugin {
            path: site.path.clone(),
            message: format!("command `{command}` requires an object input schema").into(),
        });
    };
    let mut seen = BTreeSet::new();
    let mut omittable = false;
    for name in positional {
        if !seen.insert(name.as_str()) {
            return Err(LoadError::InvalidPlugin {
                path: site.path.clone(),
                message: format!("command `{command}` repeats positional `{name}`").into(),
            });
        }
        let Some(field) = fields
            .iter()
            .find(|field| field.name.as_ref() == name.as_str())
        else {
            return Err(LoadError::InvalidPlugin {
                path: site.path.clone(),
                message: format!("command `{command}` positional `{name}` is not a schema field")
                    .into(),
            });
        };
        if !field.ty.is_scalar() {
            return Err(LoadError::InvalidPlugin {
                path: site.path.clone(),
                message: format!("command `{command}` positional `{name}` is not scalar").into(),
            });
        }
        let optional = !matches!(field.presence, Presence::Required | Presence::Nullable);
        if omittable && !optional {
            return Err(LoadError::InvalidPlugin {
                path: site.path.clone(),
                message: format!(
                    "command `{command}` positional `{name}` is required after an omittable field"
                )
                .into(),
            });
        }
        omittable |= optional;
    }
    Ok(())
}

/// Reads one confined plugin asset (a skill body) with the asset budget.
fn read_asset(
    site: &Site,
    files: &std::collections::BTreeMap<PathBuf, Vec<u8>>,
    kind: &str,
    local: &str,
    path: &str,
) -> Result<String, LoadError> {
    let fail = |message: String| LoadError::Asset {
        path: site.path.clone(),
        asset: format!("{kind} `{local}`: {path}").into(),
        message: message.into(),
    };
    let candidate = std::path::Path::new(path);
    if candidate.is_absolute()
        || candidate
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(fail("path escapes the plugin directory".into()));
    }
    let bytes = files
        .get(candidate)
        .ok_or_else(|| fail("file not found in the plugin directory".into()))?;
    if bytes.len() > MAX_CODE {
        return Err(fail(format!("exceeds {MAX_CODE} bytes")));
    }
    String::from_utf8(bytes.clone()).map_err(|_| LoadError::InvalidUtf8 {
        path: format!("{kind}/{path}").into(),
    })
}

/// Converts a `judge` callable or string into the record's instruction text.
///
/// v1 rules carry a judge callable that runs under the rules owner's
/// judged-mode policy. The record keeps instruction text; a callable judge is
/// the script's own handler and cannot be serialized, so it is rejected until
/// the rules owner defines its contract (Q16 covers only `private_tool`).
fn judge_text(site: &Site, local: &str, judge: Value<'_>) -> Result<Option<Box<str>>, LoadError> {
    if judge.is_none() {
        return Ok(None);
    }
    if let Some(text) = judge.unpack_str() {
        return Ok(Some(text.into()));
    }
    Err(LoadError::InvalidPlugin {
        path: site.path.clone(),
        message: format!(
            "rule `{local}` judge must be instruction text; callable judges are not a v1 surface"
        )
        .into(),
    })
}
