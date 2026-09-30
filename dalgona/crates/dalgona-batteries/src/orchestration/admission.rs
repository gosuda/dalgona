// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Run admission: live-run, quota, and child-spec checks before any job starts.
//! No run starts on a failed check and no job is recorded on failure.

use super::workflow::{Step, Workflow};
use sonic_rs::{JsonContainerTrait, JsonValueTrait};

#[cfg(test)]
mod tests;

/// Session limits decoded from `[agents]` and `[plugin.orchestration.agents]`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Limits {
    /// Maximum live workflow runs per session.
    pub max_runs: usize,
    /// Maximum child sessions per session.
    pub agents_per_session: u32,
    /// Child sessions already started in this session.
    pub session_used: u32,
}

/// A failed admission check, rendered with the exact model-facing text.
#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub(crate) enum AdmissionError {
    #[error(
        "agents: {live} runs are active; the limit is plugin.orchestration.max_runs = {max}. Wait for a run to end, or cancel one."
    )]
    TooManyRuns { live: usize, max: usize },
    #[error(
        "agents: this run needs {need} subagents, but the session has {left} left of agents_per_session = {limit}. Start a smaller run."
    )]
    Quota { need: usize, left: u32, limit: u32 },
    #[error("agents: step {step}: {reason}")]
    Spec { step: String, reason: String },
}

/// Checks live runs, remaining quota for statically planned members, then
/// every child spec in declaration order. Returns the planned member count.
pub(crate) fn admit(
    workflow: &Workflow,
    live_runs: usize,
    limits: &Limits,
    spec_check: impl Fn(&Step) -> Result<(), String>,
) -> Result<usize, AdmissionError> {
    if live_runs >= limits.max_runs {
        return Err(AdmissionError::TooManyRuns {
            live: live_runs,
            max: limits.max_runs,
        });
    }
    let planned = workflow.planned();
    let left = limits
        .agents_per_session
        .saturating_sub(limits.session_used);
    if u32::try_from(planned).map_or(true, |need| need > left) {
        return Err(AdmissionError::Quota {
            need: planned,
            left,
            limit: limits.agents_per_session,
        });
    }
    for step in &workflow.steps {
        if let Err(reason) = spec_check(step) {
            return Err(AdmissionError::Spec {
                step: step.name.clone(),
                reason,
            });
        }
    }
    Ok(planned)
}

/// Validated `[plugin.orchestration]` run settings. Defaults match the
/// dalgona product defaults; the entry part owns the table source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Settings {
    /// Tool rounds per subagent turn.
    pub child_max_steps: u32,
    /// Minutes a subagent may run before the last turn.
    pub child_max_minutes: u32,
    /// Runs alive at once in one session.
    pub max_runs: u32,
}

/// Reads one integer setting: missing or mistyped members fall back to the
/// default, out-of-range values fail with the exact text.
fn setting(
    table: &sonic_rs::Value,
    key: &str,
    default: u32,
    min: u32,
    max: u32,
) -> Result<u32, String> {
    let Some(value) = table.get(key) else {
        return Ok(default);
    };
    let Some(number) = value.as_u64() else {
        return Ok(default);
    };
    let Ok(number) = u32::try_from(number) else {
        return Err(format!(
            "plugin.orchestration.{key} must be an integer from {min} to {max}."
        ));
    };
    if !(min..=max).contains(&number) {
        return Err(format!(
            "plugin.orchestration.{key} must be an integer from {min} to {max}."
        ));
    }
    Ok(number)
}

/// Decodes the run settings from the orchestration configuration table.
pub(crate) fn decode_settings(table: &dal_core::RawJson) -> Result<Settings, String> {
    let value: sonic_rs::Value = sonic_rs::from_str(table.as_str())
        .map_err(|_| "the table must be an object.".to_owned())?;
    if value.as_object().is_none() {
        return Err("the table must be an object.".to_owned());
    }
    Ok(Settings {
        child_max_steps: setting(&value, "child_max_steps", 50, 1, 1000)?,
        child_max_minutes: setting(&value, "child_max_minutes", 30, 1, 600)?,
        max_runs: setting(&value, "max_runs", 16, 1, 64)?,
    })
}
