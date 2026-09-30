//! Model-facing `agent` tool: isolated child sessions via `Services::agents`.
//!
//! The tool is the single model door to child operations. It never opens a
//! host, retains no host handle, and never copies a parent grant: admission,
//! scope, and identity are enforced by the service behind every call.

mod action;
mod child;
mod report;
mod view;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use dal_agent::error::{ServiceError, ToolError};

use action::{decode_action, validate_action};
use child::{child_id, start_child};
use dal_agent::ext::{
    ArgError, BoxFuture, Caller, Extension, ExtensionBuilder, HookCx, HookError, ObserveHook,
    RawValue, Services, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput,
};
use dal_core::{
    AgentState, AgentsOp, AgentsReply, ModelInfo, Name, Notice, RawJson, RegistrationError,
    ServiceSet, SessionEnd, SessionId, ToolClass, ToolSpec, Visibility, Workspace,
};
use report::report_text;
use serde::Deserialize;
use view::{completion_notice, list_children};

/// Registered tool name.
pub(crate) const TOOL_NAME: &str = "agent";
/// Child tool allowlist when the caller omits `tools`.
pub(crate) const DEFAULT_TOOLS: [&str; 2] = ["read", "search"];
/// Longest child report returned inline, in Unicode scalar values.
pub(crate) const MAX_REPORT_CHARS: usize = 50_000;
/// Model-visible tool description.
const AGENT_DESCRIPTION: &str =
    "Start and manage isolated child agent sessions. Use wait to read a child's final report.";
/// Exact parameter schema: one tagged object with four action variants.
const AGENT_SCHEMA: &str = r#"{"type":"object","additionalProperties":false,"oneOf":[{"type":"object","additionalProperties":false,"required":["action","name","prompt"],"properties":{"action":{"const":"spawn"},"name":{"type":"string"},"prompt":{"type":"string"},"model":{"type":["string","null"]},"role":{"type":["string","null"]},"system":{"type":["string","null"]},"tools":{"type":["array","null"],"items":{"type":"string"}},"workspace":{"type":["string","null"]}}},{"type":"object","additionalProperties":false,"required":["action","id"],"properties":{"action":{"const":"wait"},"id":{"type":"string"}}},{"type":"object","additionalProperties":false,"required":["action","id"],"properties":{"action":{"const":"cancel"},"id":{"type":"string"}}},{"type":"object","additionalProperties":false,"required":["action"],"properties":{"action":{"const":"list"}}}]}"#;

/// Strict `agent` action arguments.
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum AgentAction {
    /// Start one child session.
    Spawn {
        /// Display name for progress and notices.
        name: String,
        /// The child's task prompt.
        prompt: String,
        /// Model override; absent inherits the parent model.
        #[serde(default)]
        model: Option<String>,
        /// Role section appended after the shared base prompt.
        #[serde(default)]
        role: Option<String>,
        /// Full system prompt replacement.
        #[serde(default)]
        system: Option<String>,
        /// Child tool allowlist; absent means read and search.
        #[serde(default)]
        tools: Option<Vec<String>>,
        /// Child workspace; absent inherits the parent workspace.
        #[serde(default)]
        workspace: Option<String>,
    },
    /// Await one child's final report.
    Wait {
        /// Child session id.
        id: String,
    },
    /// Cancel one child session; idempotent.
    Cancel {
        /// Child session id.
        id: String,
    },
    /// List this parent session's children in dispatch order.
    List,
}

/// Registers the `agent` tool with the `agents` injection and cleanup hook.
pub fn extension() -> Result<Extension, RegistrationError> {
    let inject = ServiceSet::from_names(["agents"])?;
    ExtensionBuilder::new("subagent", env!("CARGO_PKG_VERSION"), inject)?
        .tool(Arc::new(AgentTool::new()?), Visibility::Model)
        .on_session_end(CancelChildren)
        .build()
}

/// The `agent` model tool.
struct AgentTool {
    /// Byte-stable spec shared across models.
    spec: Arc<ToolSpec>,
}

impl AgentTool {
    /// Builds the tool with its exact parameter schema.
    fn new() -> Result<Self, RegistrationError> {
        let name = Name::parse(TOOL_NAME)?;
        let parameters =
            RawJson::parse(AGENT_SCHEMA).map_err(|_| RegistrationError::InvalidParameters)?;
        Ok(Self {
            spec: Arc::new(ToolSpec {
                name,
                description: AGENT_DESCRIPTION.into(),
                parameters,
                grammar: None,
            }),
        })
    }
}

impl Tool for AgentTool {
    fn name(&self) -> &Name {
        &self.spec.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, args: &RawValue, _workspace: &Workspace) -> Result<ToolClass, ArgError> {
        let action =
            decode_action(args.as_str()).map_err(|error| ArgError::message(error.to_string()))?;
        validate_action(&action).map_err(|error| ArgError::message(error.to_string()))?;
        Ok(ToolClass::Other)
    }

    fn run<'a>(&'a self, call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move { self.drive(call, cx).await })
    }
}

impl AgentTool {
    /// Decodes the action and dispatches one child operation.
    async fn drive(&self, call: ToolCall, mut cx: ToolCx<'_>) -> ToolOutcome {
        let action = match decode_action(call.args.as_str()) {
            Ok(action) => action,
            Err(error) => return ToolOutcome::Err(ToolError::message(error.to_string())),
        };
        if let Err(error) = validate_action(&action) {
            return ToolOutcome::Err(ToolError::message(error.to_string()));
        }
        let caller = cx.caller();
        let services = cx.services();
        match action {
            AgentAction::Spawn {
                name,
                prompt,
                model,
                role,
                system,
                tools,
                workspace,
            } => {
                let operation = match start_child(
                    call.id.clone(),
                    &name,
                    prompt,
                    model,
                    role,
                    system,
                    tools,
                    workspace,
                ) {
                    Ok(operation) => operation,
                    Err(error) => return ToolOutcome::Err(ToolError::message(error.to_string())),
                };
                match services.agents(caller, operation).await {
                    Ok(AgentsReply::Started { id }) => {
                        cx.output().push(&format!("child {name} started"));
                        ToolOutcome::Ok(ToolOutput::from_text(
                            format!(
                                "spawned child {name} ({id}); call agent with action=\"wait\" and id=\"{id}\" for its report."
                            )
                            .into_boxed_str(),
                        ))
                    }
                    Ok(_) => ToolOutcome::Err(ToolError::message(
                        "agents service returned an unexpected reply",
                    )),
                    Err(error) => service_failure(error, None),
                }
            }
            AgentAction::Wait { id } => {
                let child = match child_id(&id) {
                    Ok(child) => child,
                    Err(error) => return ToolOutcome::Err(ToolError::message(error.to_string())),
                };
                match services
                    .agents(
                        caller,
                        AgentsOp::Await {
                            id: child,
                            timeout: None,
                        },
                    )
                    .await
                {
                    Ok(AgentsReply::Await { report }) => {
                        let name = child_name(&services, caller, &report.session).await;
                        services.notify(
                            caller,
                            Notice {
                                turn: cx.turn(),
                                kind: "agent".into(),
                                text: completion_notice(&name, report.stop).into(),
                            },
                        );
                        cx.output().push(&format!("child {name} finished"));
                        ToolOutcome::Ok(ToolOutput::from_text(report_text(report).into_boxed_str()))
                    }
                    Ok(_) => ToolOutcome::Err(ToolError::message(
                        "agents service returned an unexpected reply",
                    )),
                    Err(error) => service_failure(error, Some(&id)),
                }
            }
            AgentAction::Cancel { id } => {
                let child = match child_id(&id) {
                    Ok(child) => child,
                    Err(error) => return ToolOutcome::Err(ToolError::message(error.to_string())),
                };
                match services
                    .agents(caller, AgentsOp::Cancel { id: child })
                    .await
                {
                    Ok(AgentsReply::Cancelled { id }) => {
                        cx.output().push(&format!("child {id} cancelled"));
                        ToolOutcome::Ok(ToolOutput::from_text(
                            format!("cancelled child \"{id}\".").into_boxed_str(),
                        ))
                    }
                    Ok(_) => ToolOutcome::Err(ToolError::message(
                        "agents service returned an unexpected reply",
                    )),
                    Err(error) => service_failure(error, Some(&id)),
                }
            }
            AgentAction::List => match services.agents(caller, AgentsOp::List).await {
                Ok(AgentsReply::Listed(children)) => match list_children(&children) {
                    Ok(text) => ToolOutcome::Ok(ToolOutput::from_text(text.into_boxed_str())),
                    Err(error) => ToolOutcome::Err(ToolError::message(error.to_string())),
                },
                Ok(_) => ToolOutcome::Err(ToolError::message(
                    "agents service returned an unexpected reply",
                )),
                Err(error) => service_failure(error, None),
            },
        }
    }
}

/// Resolves a finished child's display name; falls back to its session id.
async fn child_name(services: &Arc<dyn Services>, caller: &Caller, session: &SessionId) -> String {
    match services.agents(caller, AgentsOp::List).await {
        Ok(AgentsReply::Listed(children)) => children
            .iter()
            .find(|child| &child.id == session)
            .map_or_else(|| session.to_string(), |child| child.name.to_string()),
        Ok(_) | Err(_) => session.to_string(),
    }
}

/// Maps a service failure to the exact model-visible outcome.
fn service_failure(error: ServiceError, child: Option<&str>) -> ToolOutcome {
    match error {
        ServiceError::Denied(reason) => ToolOutcome::Err(ToolError::Denied(reason)),
        ServiceError::Cancelled => ToolOutcome::Interrupted,
        ServiceError::Failed { message, .. } => {
            let text = message.as_ref();
            if text.contains("agents.max_depth") {
                return ToolOutcome::Err(ToolError::message(depth_text(text)));
            }
            if let Some(id) = child
                && (text.contains("unknown session") || text.contains("session is gone"))
            {
                return ToolOutcome::Err(ToolError::message(format!(
                    "agents: unknown child \"{id}\"."
                )));
            }
            ToolOutcome::Err(ToolError::message(message))
        }
        other => ToolOutcome::Err(ToolError::message(other.to_string())),
    }
}

/// Renders the nested-start refusal, preserving the configured depth.
fn depth_text(service_text: &str) -> String {
    let depth = service_text
        .rsplit('=')
        .next()
        .and_then(|tail| tail.trim().trim_end_matches('.').trim().parse::<u32>().ok());
    match depth {
        Some(depth) => {
            format!("subagents cannot spawn subagents (agents.max_depth = {depth})")
        }
        None => service_text.to_owned(),
    }
}

/// Cancels queued and running children when their parent session ends.
struct CancelChildren;

impl ObserveHook<SessionEnd> for CancelChildren {
    fn call(&self, _end: SessionEnd, cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        Box::pin(async move {
            let children = match cx.services.agents(&cx.caller, AgentsOp::List).await {
                Ok(AgentsReply::Listed(children)) => children,
                Ok(_) | Err(_) => return Ok(()),
            };
            for child in children
                .iter()
                .filter(|child| matches!(child.state, AgentState::Queued | AgentState::Running))
            {
                let _ = cx
                    .services
                    .agents(&cx.caller, AgentsOp::Cancel { id: child.id })
                    .await;
            }
            Ok(())
        })
    }
}
