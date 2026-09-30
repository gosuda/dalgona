//! Generation feed-in: validated plugins become host extensions (§P02 §P03).
//!
//! Conversion never publishes. The host validates the whole batch and owns
//! conflict tables, so a rejection anywhere rejects the generation.

use std::sync::Arc;

use dal_agent::ext::generation::catalog::ExportSpec;
use dal_agent::ext::{Extension, ExtensionBuilder, ModelRecord, PromptOrder, PromptSection};
use dal_core::{HookEvent, RegistrationError, Site};

use crate::error::LoadError;
use crate::hooks::ScriptHook;
use crate::model::ScriptModelHandler;
use crate::system::PluginGeneration;
use crate::tool::{ExportCommand, ExportTool};
use crate::validate::{ExportBody, LoadedPlugin};

/// Converts one generation into host extensions in generation order:
/// bundled plugins by name, then user plugins by name.
///
/// # Errors
///
/// Returns [`LoadError::Registration`] at the offending plugin site.
pub(crate) fn convert_all(generation: &PluginGeneration) -> Result<Vec<Extension>, LoadError> {
    generation.plugins.iter().map(convert).collect()
}

/// Converts one plugin into one extension.
fn convert(plugin: &Arc<LoadedPlugin>) -> Result<Extension, LoadError> {
    let site = &plugin.site;
    let mut builder = ExtensionBuilder::new(plugin.name.as_str(), &plugin.version, plugin.inject)
        .map_err(|error| registration(site, &error))?
        .with_origin(plugin.origin, Some(site.clone()));
    for (index, export) in plugin.exports.iter().enumerate() {
        let ExportBody::Tool {
            input_json,
            description,
            visibility,
            ..
        } = &export.body;
        let spec = ExportSpec {
            id: export.id.clone(),
            uses: export.uses.clone(),
            input: input_json.clone(),
            description: description.clone(),
        };
        let tool = Arc::new(ExportTool::new(Arc::clone(plugin), index));
        builder = builder.script_tool(tool, *visibility, spec);
    }
    for (index, command) in plugin.commands.iter().enumerate() {
        let spec = ExportSpec {
            id: command.id.clone(),
            uses: command.tool.uses.clone(),
            input: command.tool.input_json.clone(),
            description: command.spec.summary.clone(),
        };
        let handler = Arc::new(ExportCommand::new(Arc::clone(plugin), index));
        builder = builder.script_command(command.spec.clone(), handler, spec);
    }
    for (index, model) in plugin.models.iter().enumerate() {
        builder = builder.model(ModelRecord {
            id: model.model_id.clone(),
            caps: model.caps.clone(),
            handler: Arc::new(ScriptModelHandler::new(Arc::clone(plugin), index)),
            export: Some(model.id.clone()),
        });
    }
    for (index, hook) in plugin.hooks.iter().enumerate() {
        builder = builder.hook_export(ExportSpec {
            id: hook.id.clone(),
            uses: hook.uses.clone(),
            input: hook_input(),
            description: hook.event.as_str().into(),
        });
        builder = subscribe(
            builder,
            hook.event,
            ScriptHook::new(Arc::clone(plugin), index),
        )
        .ok_or_else(|| {
            rejected(
                site,
                "plugin subscribes to a hook event this host does not dispatch",
            )
        })?;
    }
    for skill in &plugin.skills {
        builder = builder.skill(skill.clone());
    }
    for rule in &plugin.rules {
        builder = builder.rule(rule.clone());
    }
    if let Some(text) = &plugin.prompt {
        builder = builder.prompt_section(PromptSection::static_text(
            PromptOrder::Plugins,
            text.clone(),
        ));
    }
    builder.build().map_err(|error| registration(site, &error))
}

/// The input schema a hook declaration carries: hooks take the host event,
/// never model arguments.
#[expect(
    clippy::expect_used,
    reason = "the schema is a fixed, valid JSON literal"
)]
fn hook_input() -> dal_core::RawJson {
    dal_core::RawJson::parse(r#"{"type":"object"}"#).expect("literal object schema parses")
}

/// Subscribes `hook` to `event`; `None` for an event outside the P05 table.
fn subscribe(
    builder: ExtensionBuilder,
    event: HookEvent,
    hook: ScriptHook,
) -> Option<ExtensionBuilder> {
    Some(match event {
        HookEvent::SessionStart => builder.on_session_start(hook),
        HookEvent::SessionEnd => builder.on_session_end(hook),
        HookEvent::Input => builder.on_input(hook),
        HookEvent::BeforeTurn => builder.on_before_turn(hook),
        HookEvent::BeforeRequest => builder.on_before_request(hook),
        HookEvent::ToolCall => builder.on_tool_call(hook),
        HookEvent::ToolResult => builder.on_tool_result(hook),
        HookEvent::TurnEnd => builder.on_turn_end(hook),
        HookEvent::Settled => builder.on_settled(hook),
        _ => return None,
    })
}

/// A conversion rejection at the plugin site.
fn rejected(site: &Site, message: &str) -> LoadError {
    LoadError::Registration {
        path: site.path.clone(),
        line: site.line,
        col: site.col,
        message: message.into(),
    }
}

/// A host registration failure at the plugin site.
fn registration(site: &Site, error: &RegistrationError) -> LoadError {
    rejected(site, &error.to_string())
}
