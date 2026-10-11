use super::AgentAction;

use sonic_rs::{JsonContainerTrait as _, JsonValueTrait as _};

/// Typed failure for strict `agent` argument handling.
#[derive(Debug, thiserror::Error)]
pub(crate) enum AgentInputError {
    /// The arguments are not a well-formed `agent` action.
    #[error("{0}")]
    Decode(Box<str>),
    /// Both `role` and `system` were supplied.
    #[error("agents: role and system cannot both be set.")]
    RoleAndSystem,
    /// A requested child tool name is not usable.
    #[error("unknown tool \"{0}\" in agents.tools")]
    UnknownTool(Box<str>),
    /// A child session id does not parse.
    #[error("agents: unknown child \"{0}\".")]
    UnknownChild(Box<str>),
    /// A child workspace path is not absolute.
    #[error(transparent)]
    Workspace(#[from] dal_core::WorkspaceError),
    /// Child list rendering failed.
    #[error("child list rendering failed")]
    Render(#[from] std::fmt::Error),
}

/// Decodes strict `agent` arguments; unknown fields and wrong types fail.
///
/// Serde ignores `deny_unknown_fields` on this internally tagged enum, so the key
/// set is checked by hand before the typed decode runs.
pub(crate) fn decode_action(raw: &str) -> Result<AgentAction, AgentInputError> {
    let decode_error = |error: sonic_rs::Error| AgentInputError::Decode(error.to_string().into());
    let value: sonic_rs::Value = sonic_rs::from_str(raw).map_err(decode_error)?;
    let object = value
        .as_object()
        .ok_or_else(|| AgentInputError::Decode("agent action must be an object".into()))?;
    let action = value
        .get("action")
        .and_then(|action| action.as_str())
        .ok_or_else(|| {
            AgentInputError::Decode("agent action is missing its action field".into())
        })?;
    let Some(allowed) = allowed_keys(action) else {
        return sonic_rs::from_str(raw).map_err(decode_error);
    };
    for (key, _) in object {
        if !allowed.contains(&key) {
            return Err(AgentInputError::Decode(
                format!("unknown field \"{key}\" in agent action").into(),
            ));
        }
    }
    sonic_rs::from_str(raw).map_err(decode_error)
}

/// The exact accepted key set per action tag; mirrored in `AGENT_SCHEMA`.
pub(crate) fn allowed_keys(action: &str) -> Option<&'static [&'static str]> {
    match action {
        "spawn" => Some(&[
            "action",
            "name",
            "prompt",
            "model",
            "role",
            "system",
            "tools",
            "workspace",
        ]),
        "wait" | "cancel" => Some(&["action", "id"]),
        "list" => Some(&["action"]),
        _ => None,
    }
}

/// Rejects `role` plus `system` before any service call.
pub(crate) fn validate_action(action: &AgentAction) -> Result<(), AgentInputError> {
    if matches!(
        action,
        AgentAction::Spawn {
            role: Some(_),
            system: Some(_),
            ..
        }
    ) {
        return Err(AgentInputError::RoleAndSystem);
    }
    Ok(())
}
