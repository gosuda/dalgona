//! Generation feed-in: validated plugins become host extensions.
//!
//! Conversion never publishes. The host validates the whole batch and owns
//! conflict tables, so a rejection anywhere rejects the generation.

use std::sync::Arc;

use dal_agent::ext::generation::catalog::ExportSpec;
use dal_agent::ext::{Extension, ExtensionBuilder, ModelRecord, PromptOrder, PromptSection};
use dal_core::{HookEvent, Site};

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
        .map_err(|error| rejected(site, &error.to_string()))?
        .with_origin(plugin.origin, Some(site.clone()))
        .with_state_version(plugin.state_version);
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
        let event = hook.event;
        let handler = ScriptHook::new(Arc::clone(plugin), index);
        builder = match event {
            HookEvent::SessionStart => builder.on_session_start(handler),
            HookEvent::SessionEnd => builder.on_session_end(handler),
            HookEvent::Input => builder.on_input(handler),
            HookEvent::BeforeTurn => builder.on_before_turn(handler),
            HookEvent::BeforeRequest => builder.on_before_request(handler),
            HookEvent::ToolCall => builder.on_tool_call(handler),
            HookEvent::ToolResult => builder.on_tool_result(handler),
            HookEvent::TurnEnd => builder.on_turn_end(handler),
            HookEvent::Settled => builder.on_settled(handler),
            _ => {
                return Err(rejected(
                    site,
                    "plugin subscribes to a hook event this host does not dispatch",
                ));
            }
        };
    }
    for skill in &plugin.skills {
        builder = builder.skill(skill.clone());
    }
    for rule in &plugin.rules {
        builder = builder.rule(rule.clone());
    }
    for file in &plugin.rule_files {
        builder = builder.rule_file(file.clone());
    }
    if let Some(text) = &plugin.prompt {
        builder = builder.prompt_section(PromptSection::static_text(
            PromptOrder::Plugins,
            text.clone(),
        ));
    }
    builder
        .build()
        .map_err(|error| rejected(site, &error.to_string()))
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

/// A conversion rejection at the plugin site.
fn rejected(site: &Site, message: &str) -> LoadError {
    LoadError::Registration {
        path: site.path.clone(),
        line: site.line,
        col: site.col,
        message: message.into(),
    }
}
