use std::fmt::Write as _;

use dal_core::{AgentInfo, AgentState, Stop};

use super::action::AgentInputError;

/// Renders a terminal reason as its progress word.
pub(crate) fn stop_word(stop: Stop) -> &'static str {
    match stop {
        Stop::EndTurn => "end_turn",
        Stop::Length => "length",
        Stop::Filter => "filter",
        Stop::MaxSteps => "max_steps",
        Stop::Cancelled => "cancelled",
        Stop::Failed => "failed",
    }
}

/// Renders a child lifecycle state; settled error and cancelled stops fail.
pub(crate) fn state_word(state: &AgentState) -> &'static str {
    match state {
        AgentState::Queued => "queued",
        AgentState::Running => "running",
        AgentState::Done(Stop::Cancelled | Stop::Failed) => "failed",
        AgentState::Done(_) => "done",
        _ => "failed",
    }
}

/// Renders children in dispatch order, one `<id> <state> <name>` row each.
pub(crate) fn list_children(children: &[AgentInfo]) -> Result<String, AgentInputError> {
    if children.is_empty() {
        return Ok("No child sessions.".into());
    }

    let mut output = String::new();
    for (index, child) in children.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        write!(
            output,
            "{} {} {}",
            child.id,
            state_word(&child.state),
            child.name
        )?;
    }
    Ok(output)
}

/// Renders the structured completion notice for one finished child.
pub(crate) fn completion_notice(name: &str, stop: Stop) -> String {
    format!("child {name} finished ({})", stop_word(stop))
}
