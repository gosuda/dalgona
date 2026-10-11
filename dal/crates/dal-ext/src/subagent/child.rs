use std::path::PathBuf;

use dal_core::{AgentStart, AgentsOp, CallId, Name, SessionId, Workspace};

use super::DEFAULT_TOOLS;
use super::action::AgentInputError;

/// Builds validated child tool names; omitted means exactly read and search.
pub(crate) fn child_tool_names(tools: Option<Vec<String>>) -> Result<Box<[Name]>, AgentInputError> {
    let mut names = Vec::with_capacity(tools.as_ref().map_or(DEFAULT_TOOLS.len(), Vec::len));
    match tools {
        Some(tools) => {
            for tool in tools {
                names.push(
                    Name::parse_mapped_tool(&tool)
                        .map_err(|_| AgentInputError::UnknownTool(tool.into_boxed_str()))?,
                );
            }
        }
        None => {
            for tool in DEFAULT_TOOLS {
                names.push(
                    Name::parse(tool).map_err(|_| AgentInputError::UnknownTool(tool.into()))?,
                );
            }
        }
    }
    Ok(names.into_boxed_slice())
}

/// Converts a supplied workspace string to an absolute workspace.
pub(crate) fn child_workspace(
    workspace: Option<String>,
) -> Result<Option<Workspace>, AgentInputError> {
    workspace
        .map(|path| Workspace::new(PathBuf::from(path)).map_err(AgentInputError::Workspace))
        .transpose()
}

/// Builds one parent-scoped start operation; no parent grant is copied.
pub(crate) fn start_child(
    call: CallId,
    name: &str,
    prompt: String,
    model: Option<String>,
    role: Option<String>,
    system: Option<String>,
    tools: Option<Vec<String>>,
    workspace: Option<String>,
) -> Result<AgentsOp, AgentInputError> {
    let start = AgentStart {
        call,
        name: name.into(),
        prompt: prompt.into_boxed_str(),
        model: model.map(String::into_boxed_str),
        role: role.map(String::into_boxed_str),
        system: system.map(String::into_boxed_str),
        tools: Some(child_tool_names(tools)?),
        workspace: child_workspace(workspace)?,
    };
    start
        .validate()
        .map_err(|_| AgentInputError::RoleAndSystem)?;
    Ok(AgentsOp::Start(start))
}

/// Parses a child session id; unparsable ids use the unknown-child text.
pub(crate) fn child_id(id: &str) -> Result<SessionId, AgentInputError> {
    SessionId::parse(id).map_err(|_| AgentInputError::UnknownChild(id.into()))
}
// weave: run 'weave explain dal/crates/dal-ext/src/subagent/child.rs' for per-hunk detail, 'weave check' to verify your resolution
