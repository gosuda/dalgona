//! Plugin tools and commands as host [`Tool`] and [`CommandHandler`] values
//! (§P01 §P04 §P06).
//!
//! A tool call and a command reach the same validated `ToolDecl` handler
//! through [`invoke::enter`]: arguments pass the same schema and defaults,
//! and the handler runs once under the host-admitted phase. A missing
//! script host fails closed; nothing runs without a minted invocation.

use std::sync::Arc;

use dal_agent::error::{ServiceError, ToolError};
use dal_agent::ext::script::HostTerminal;
use dal_agent::ext::tool::{ArgError, RawValue, ToolOutput};
use dal_agent::ext::{BoxFuture, CommandCx, CommandHandler, Tool, ToolCall, ToolCx, ToolOutcome};
use dal_core::command::Output;
use dal_core::ext::{Phase, ToolData, ViewNode};
use dal_core::{ModelInfo, Name, Part, Reply, ToolClass, ToolSpec, Workspace};

use crate::engine::CELL_WALL;
use crate::invoke::{self, Arg, Handler, InvokeFailure, InvokeOutput};
use crate::schema::Schema;
use crate::validate::{ExportBody, LoadedPlugin, ToolDecl};
use crate::value::Value;

/// One exported plugin tool, registered under its wire name.
pub(crate) struct ExportTool {
    /// The owning plugin; the export lives at `export` in its table.
    plugin: Arc<LoadedPlugin>,
    /// The index into `plugin.exports`.
    export: usize,
    /// The provider-facing spec, built once at conversion.
    spec: Arc<ToolSpec>,
}

impl ExportTool {
    /// Builds the adapter for `plugin.exports[export]`.
    pub(crate) fn new(plugin: Arc<LoadedPlugin>, export: usize) -> Self {
        let ExportBody::Tool {
            input_json,
            description,
            wire,
            ..
        } = &plugin.exports[export].body;
        let spec = Arc::new(ToolSpec {
            name: wire.clone(),
            description: description.clone(),
            parameters: input_json.clone(),
            grammar: None,
        });
        Self {
            plugin,
            export,
            spec,
        }
    }
}

impl Tool for ExportTool {
    fn name(&self) -> &Name {
        &self.spec.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, args: &RawValue, _ws: &Workspace) -> Result<ToolClass, ArgError> {
        Value::decode(args.as_str())
            .map(|_| ToolClass::Other)
            .map_err(|error| ArgError::message(error.to_string()))
    }

    fn run<'a>(&'a self, call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let export = &self.plugin.exports[self.export];
            let ExportBody::Tool {
                schema,
                output,
                run,
                ..
            } = &export.body;
            let args = match decode_args(call.args.as_str(), schema) {
                Ok(args) => args,
                Err(error) => return ToolOutcome::Err(ToolError::message(error.to_string())),
            };
            let Some(script) = cx.script() else {
                return ToolOutcome::Err(ToolError::message(invoke::NO_SCRIPT_HOST));
            };
            let handler = Handler {
                plugin: &self.plugin,
                id: &export.id,
                phase: Phase::Tool,
                run: *run,
                cap: CELL_WALL,
            };
            let settled = invoke::settle(
                invoke::enter(script, handler, Arg::Data(args)).await,
                output.as_ref(),
                &export.id,
            );
            tool_outcome(settled)
        })
    }
}

/// One plugin slash command bound to its tool descriptor.
pub(crate) struct ExportCommand {
    /// The owning plugin; the command lives at `command` in its table.
    plugin: Arc<LoadedPlugin>,
    /// The index into `plugin.commands`.
    command: usize,
}

impl ExportCommand {
    /// Builds the handler for `plugin.commands[command]`.
    pub(crate) fn new(plugin: Arc<LoadedPlugin>, command: usize) -> Self {
        Self { plugin, command }
    }
}

impl CommandHandler for ExportCommand {
    fn run<'a>(
        &'a self,
        args: &'a str,
        cx: CommandCx<'a>,
    ) -> BoxFuture<'a, Result<Reply, ServiceError>> {
        Box::pin(async move {
            let command = &self.plugin.commands[self.command];
            let ToolDecl {
                schema,
                output,
                run,
                ..
            } = &command.tool;
            let args = bind_command(args, &command.positional, schema)
                .map_err(|error| ServiceError::failed(None, error.to_string()))?;
            let script = cx
                .script()
                .ok_or_else(|| ServiceError::failed(None, invoke::NO_SCRIPT_HOST))?;
            let handler = Handler {
                plugin: &self.plugin,
                id: &command.id,
                phase: Phase::Command,
                run: *run,
                cap: CELL_WALL,
            };
            let settled = invoke::settle(
                invoke::enter(script, handler, Arg::Data(args)).await,
                output.as_ref(),
                &command.id,
            );
            command_reply(settled)
        })
    }
}

/// One rejected tool or command argument set, shown to the caller.
#[derive(Debug, thiserror::Error)]
enum ArgsError {
    /// The model's arguments are not strict JSON within the transport bounds.
    #[error(transparent)]
    Codec(#[from] crate::value::CodecError),
    /// The arguments fail the tool's schema.
    #[error(transparent)]
    Schema(#[from] crate::schema::SchemaError),
    /// The command tail does not lex.
    #[error(transparent)]
    Lex(#[from] dal_core::command::LexError),
    /// The command tokens do not bind to the schema.
    #[error(transparent)]
    Command(#[from] crate::command::CommandError),
}

/// Decodes model arguments and validates them through the tool's schema.
fn decode_args(raw: &str, schema: &Schema) -> Result<Value, ArgsError> {
    Ok(schema.validate(&Value::decode(raw)?, "args")?)
}

/// Lexes a command tail and binds it through the tool's schema (§P04).
fn bind_command(raw: &str, positional: &[Box<str>], schema: &Schema) -> Result<Value, ArgsError> {
    let tokens = dal_core::command::tokens(raw)?;
    Ok(crate::command::bind(&tokens, positional, schema)?)
}

/// Maps one settled handler onto exactly one tool outcome (§R07).
fn tool_outcome(settled: Result<InvokeOutput, InvokeFailure>) -> ToolOutcome {
    let output = match settled {
        Ok(output) => output,
        Err(InvokeFailure::Cancelled | InvokeFailure::Terminal(HostTerminal::Cancelled)) => {
            return ToolOutcome::Interrupted;
        }
        Err(InvokeFailure::Terminal(HostTerminal::Denied { reason })) => {
            return ToolOutcome::Err(ToolError::Denied(reason));
        }
        Err(failure) => return ToolOutcome::Err(ToolError::message(failure.to_string())),
    };
    let data = match (output.view, output.views.is_empty()) {
        (Some(view), _) => Some(ToolData::Display(view)),
        (None, false) => Some(ToolData::Views(output.views)),
        (None, true) => None,
    };
    ToolOutcome::Ok(Box::new(ToolOutput {
        parts: vec![Part::Text {
            text: value_text(&output.value),
        }],
        files_changed: Vec::new(),
        data,
    }))
}

/// Maps one settled handler onto one command reply.
///
/// A text or table display node renders as that output; any other result
/// renders its value as text.
fn command_reply(settled: Result<InvokeOutput, InvokeFailure>) -> Result<Reply, ServiceError> {
    let output = match settled {
        Ok(output) => output,
        Err(InvokeFailure::Cancelled | InvokeFailure::Terminal(HostTerminal::Cancelled)) => {
            return Err(ServiceError::Cancelled);
        }
        Err(InvokeFailure::Terminal(HostTerminal::Denied { reason })) => {
            return Err(ServiceError::Denied(reason));
        }
        Err(failure) => return Err(ServiceError::failed(None, failure.to_string())),
    };
    let shown = match output.view {
        Some(ViewNode::Text(text)) => Output::Text(text),
        Some(ViewNode::Table { columns, rows }) => Output::Table(table_rows(columns, &rows)),
        _ => Output::Text(value_text(&output.value)),
    };
    Ok(Reply::Done(shown))
}

/// Renders a display table as header plus text rows.
fn table_rows(columns: Box<[Box<str>]>, rows: &[Box<[dal_core::RawJson]>]) -> Vec<Vec<Box<str>>> {
    let cells = rows.iter().map(|row| row.iter().map(cell_text).collect());
    std::iter::once(columns.into_vec()).chain(cells).collect()
}

/// A scalar JSON cell as display text: strings unquoted, others verbatim.
fn cell_text(cell: &dal_core::RawJson) -> Box<str> {
    match Value::decode(cell.as_str()) {
        Ok(Value::Str(text)) => text,
        _ => cell.as_str().into(),
    }
}

/// A result value as text: a string verbatim, anything else as JSON.
fn value_text(value: &Value) -> Box<str> {
    match value {
        Value::Str(text) => text.clone(),
        other => other.to_json().into(),
    }
}
#[cfg(test)]
mod tests;
