// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Model-facing `agents` actions and the child-only `report` result cell.
//! Tool registration wires these pure items once the runtime tool group lands.

use std::fmt::Write as _;
use std::sync::{Arc, OnceLock};

use dal_core::{RawJson, ToolClass};
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};

use super::workflow::{Isolation, Workflow, find_saved};

#[cfg(test)]
mod tests;

pub(crate) const REPORT_DESCRIPTION: &str = "Send your final report to the agent that started you. Call it exactly once, when the task is done, blocked, or failed. Your turn ends after this call.";

pub(crate) const REPORT_SCHEMA: &str = "{\"type\":\"object\",\"properties\":{\"status\":{\"type\":\"string\",\"enum\":[\"done\",\"blocked\",\"failed\"],\"description\":\"done: the task is complete. blocked: you cannot go on without something you do not have. failed: you tried, and the task cannot be done.\"},\"report\":{\"type\":\"string\",\"minLength\":1,\"maxLength\":16000,\"description\":\"What you found or changed, with file paths and line numbers. The agent that started you reads only this text.\"}},\"required\":[\"status\",\"report\"],\"additionalProperties\":false}";

pub(crate) const AGENTS_DESCRIPTION: &str = "Run subagents in the background. action run starts a workflow and returns at once. A workflow is a list of steps. A step without items is one subagent. A step with items is a pool: one subagent per item, at most workers at a time. A step with after starts when those steps end, and {{step:<name>}} in its prompt becomes their reports. Each subagent ends by calling its own report tool. When the whole run ends, you get one message with the reports. Do not poll; use wait only when you have nothing else to do. Subagents see only the tools you list, by default read and search, and they cannot ask the user: patch works only in approval modes edits and all, and exec only in all. A step that lists patch or exec runs in its own git worktree by default, and its changes merge back when it ends done. Saved workflows are listed by action list.";

pub(crate) const AGENTS_SCHEMA: &str = "{\"type\":\"object\",\"properties\":{\"action\":{\"type\":\"string\",\"enum\":[\"run\",\"wait\",\"cancel\",\"list\"],\"description\":\"run starts a workflow. wait blocks until runs or tasks end. cancel stops runs or tasks. list shows runs, providers, and saved workflows.\"},\"steps\":{\"type\":\"array\",\"minItems\":1,\"maxItems\":32,\"description\":\"run: the steps of an inline workflow.\",\"items\":{\"type\":\"object\",\"properties\":{\"name\":{\"type\":\"string\",\"pattern\":\"^[a-z][a-z0-9-]{0,31}$\",\"description\":\"Step name, unique in the run.\"},\"prompt\":{\"type\":\"string\",\"minLength\":1,\"maxLength\":32000,\"description\":\"The task. {{item}} is the item of a pool. {{step:<name>}} is the reports of a step listed in after. {{input}} is the input of a saved workflow.\"},\"items\":{\"type\":\"array\",\"minItems\":1,\"maxItems\":1024,\"items\":{\"type\":\"string\",\"minLength\":1,\"maxLength\":2000},\"description\":\"Makes the step a pool with one subagent per item.\"},\"items_from\":{\"type\":\"string\",\"description\":\"Makes the step a pool over the non-empty lines of the report of this earlier step, which must be in after.\"},\"workers\":{\"type\":\"integer\",\"minimum\":1,\"maximum\":64,\"description\":\"Pool only: subagents that run at the same time. Default 4.\"},\"after\":{\"type\":\"array\",\"maxItems\":31,\"items\":{\"type\":\"string\"},\"description\":\"Steps that must end before this step starts.\"},\"tools\":{\"type\":\"array\",\"maxItems\":16,\"items\":{\"type\":\"string\"},\"description\":\"Tools of the subagents. Default [\\\"read\\\",\\\"search\\\"].\"},\"model\":{\"type\":\"string\",\"description\":\"Model id for the subagents. Default: your model.\"},\"isolation\":{\"type\":\"string\",\"enum\":[\"worktree\",\"shared\"],\"description\":\"worktree: the subagent works in its own git worktree and its changes merge back when it ends done. shared: it works in the real checkout. Default: worktree when tools include patch or exec.\"}},\"required\":[\"name\",\"prompt\"],\"additionalProperties\":false}},\"workflow\":{\"type\":\"string\",\"description\":\"run: the name of a saved workflow instead of steps.\"},\"input\":{\"type\":\"string\",\"maxLength\":32000,\"description\":\"run: text for {{input}} in a saved workflow.\"},\"name\":{\"type\":\"string\",\"maxLength\":80,\"description\":\"run: a short label for the run. Default: the workflow name or the first step name.\"},\"ids\":{\"type\":\"array\",\"minItems\":1,\"maxItems\":64,\"items\":{\"type\":\"string\",\"pattern\":\"^j[0-9]+$\"},\"description\":\"wait, cancel, list: run or task ids.\"},\"timeout\":{\"type\":\"integer\",\"minimum\":1,\"maximum\":600,\"description\":\"wait: seconds to wait at most. Default 60.\"}},\"required\":[\"action\"],\"additionalProperties\":false}";

/// Tools that never write outside the child's granted roots. The plan's
/// approval class derives from each tool's spec at admission; this is the
/// fallback for names the registry does not know.
const READ_ONLY_TOOLS: [&str; 2] = ["read", "search"];

/// Subagents are switched off for the session.
pub(crate) const SUBAGENTS_OFF: &str = "agents: subagents are off (agents = false in config.toml).";

/// A child tried to start its own subagents.
pub(crate) const NO_NESTED_RUNS: &str = "agents: a subagent cannot start subagents.";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReportStatus {
    Done,
    Blocked,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Report {
    pub status: ReportStatus,
    pub text: String,
}

/// One result cell bound to one child. The first valid report wins; a
/// duplicate cannot replace it.
#[derive(Clone, Debug)]
pub(crate) struct ReportCell(Arc<OnceLock<Report>>);

impl Default for ReportCell {
    fn default() -> Self {
        Self(Arc::new(OnceLock::new()))
    }
}

impl ReportCell {
    /// Returns the stored report, when a valid call has filled the cell.
    pub(crate) fn get(&self) -> Option<Report> {
        self.0.get().cloned()
    }
}

/// The outcome of one `report` call: the verdict plus the exact reply text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReportOutcome {
    Empty,
    Stored,
    Duplicate,
}

/// Records one `report` call. Empty text never fills the cell.
pub(crate) fn submit(
    cell: &ReportCell,
    status: ReportStatus,
    text: &str,
) -> (ReportOutcome, &'static str) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return (
            ReportOutcome::Empty,
            "report: the report is empty. Write what you found or changed.",
        );
    }
    let report = Report {
        status,
        text: trimmed.to_owned(),
    };
    match cell.0.set(report) {
        Ok(()) => (ReportOutcome::Stored, "Report received."),
        Err(_) => (
            ReportOutcome::Duplicate,
            "report: a report was already received. Your turn ends now.",
        ),
    }
}

/// The four model-facing `agents` actions. Run and task ids are resolved by
/// the session owner and never cross session boundaries.
#[derive(Clone, Debug)]
pub(crate) enum AgentAction {
    Run { label: String, workflow: Workflow },
    Wait { ids: Vec<String>, timeout_s: u16 },
    Cancel { ids: Vec<String> },
    List { ids: Vec<String> },
}

/// Classifies one action for the approval ladder. A run is read-only only
/// when every step is shared and lists read-only tools; waiting, cancelling,
/// and listing never mutate.
pub(crate) fn approval_class(action: &AgentAction) -> ToolClass {
    match action {
        AgentAction::Run { workflow, .. } => {
            let read_only = workflow.steps.iter().all(|step| {
                step.isolation == Isolation::Shared
                    && step
                        .tools
                        .iter()
                        .all(|tool| READ_ONLY_TOOLS.contains(&tool.as_str()))
            });
            if read_only {
                ToolClass::Read
            } else {
                ToolClass::Exec {
                    read_only: false,
                    grant: None,
                }
            }
        }
        AgentAction::Wait { .. } | AgentAction::Cancel { .. } | AgentAction::List { .. } => {
            ToolClass::Read
        }
    }
}

/// A rejected `agents` call, carrying the exact model-facing text.
#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub(crate) enum ActionError {
    #[error("agents: action {0} is not one of run, wait, cancel, list.")]
    UnknownAction(String),
    #[error("agents: field {field} does not apply to action {action}.")]
    FieldUnused { field: String, action: &'static str },
    #[error("agents: action run needs steps or workflow, not both.")]
    StepsAndWorkflow,
    #[error("agents: action run needs steps or workflow.")]
    StepsOrWorkflow,
    #[error("agents: input applies only to a saved workflow.")]
    InputWithoutWorkflow,
    #[error("agents: action {0} needs ids.")]
    NeedsIds(&'static str),
    #[error("{0}")]
    Workflow(#[from] super::workflow::WorkflowError),
}

/// Renders the unknown-id error for one display id.
pub(crate) fn unknown_id(id: &str) -> String {
    format!("agents: no run or task {id} in this session.")
}

/// Renders the missing-service error naming the denial.
pub(crate) fn service_denied(service: &str, denied: &str) -> String {
    format!("agents: the {service} service is not granted to orchestration: {denied}.")
}

/// Renders the exact `run` result line.
pub(crate) fn run_result_text(
    id: &str,
    label: &str,
    steps: usize,
    planned: usize,
    items_from: &[&str],
) -> String {
    let step_word = if steps == 1 { "step" } else { "steps" };
    let agent_word = if planned == 1 {
        "subagent"
    } else {
        "subagents"
    };
    let mut text = format!(
        "started run {id} \"{label}\": {steps} {step_word}, {planned} {agent_word} planned"
    );
    for step in items_from {
        let _ = write!(text, ", plus the items of step {step}");
    }
    text.push('.');
    text.push_str(
        "\nIts report arrives in one message when the run ends. Keep working; do not poll. Use agents wait only when you have nothing else to do.",
    );
    text
}

/// Reads one optional string member; JSON null counts as absent.
fn opt_string(args: &Value, field: &str) -> Option<String> {
    args.as_object().and_then(|object| object.get(&field))
        .and_then(JsonValueTrait::as_str)
        .map(str::to_owned)
}

/// Reads the raw `steps` member back into owned JSON for the decoder.
fn raw_steps(args: &Value) -> Option<RawJson> {
    let steps = args.as_object().and_then(|object| object.get(&"steps"))?;
    if steps.is_null() {
        return None;
    }
    sonic_rs::to_string(steps)
        .ok()
        .and_then(|text| RawJson::parse(&text).ok())
}

/// Reads the `ids` member as display ids. Non-string entries cannot arrive
/// behind the tool schema; they are skipped rather than rejected.
fn read_ids(args: &Value) -> Vec<String> {
    args.as_object().and_then(|object| object.get(&"ids"))
        .and_then(|ids| ids.as_array())
        .map(|ids| {
            ids.iter()
                .filter_map(JsonValueTrait::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Rejects the first member the action does not use, in schema order.
fn reject_unused(args: &Value, action: &'static str, used: &[&str]) -> Result<(), ActionError> {
    for field in ["steps", "workflow", "input", "name", "ids", "timeout"] {
        if used.contains(&field) {
            continue;
        }
        let present = args.as_object().and_then(|object| object.get(&field)).is_some_and(|value| !value.is_null());
        if present {
            return Err(ActionError::FieldUnused {
                field: field.to_owned(),
                action,
            });
        }
    }
    Ok(())
}

/// Decodes one model-facing `agents` call. Every reachable-behind-schema
/// failure carries the exact O4 text; the tool schema itself rejects
/// mistyped members before this runs.
pub(crate) fn decode_action(
    args: &RawJson,
    saved: Option<&RawJson>,
) -> Result<AgentAction, ActionError> {
    let parsed: Value = sonic_rs::from_str(args.as_str())
        .map_err(|_| ActionError::UnknownAction("missing".to_owned()))?;
    let action = opt_string(&parsed, "action").unwrap_or_default();
    match action.as_str() {
        "run" => {
            reject_unused(&parsed, "run", &["steps", "workflow", "input", "name"])?;
            let steps = raw_steps(&parsed);
            let workflow_name = opt_string(&parsed, "workflow");
            let input = opt_string(&parsed, "input");
            let workflow = match (steps, workflow_name) {
                (Some(_), Some(_)) => return Err(ActionError::StepsAndWorkflow),
                (None, None) => return Err(ActionError::StepsOrWorkflow),
                (Some(raw), None) => {
                    if input.is_some() {
                        return Err(ActionError::InputWithoutWorkflow);
                    }
                    super::workflow::decode_steps(&raw, None, "run")?
                }
                (None, Some(name)) => {
                    let fallback = RawJson::null();
                    let table = saved.unwrap_or(&fallback);
                    find_saved(table, &name, input.as_deref())?
                }
            };
            let label = match opt_string(&parsed, "name") {
                Some(name) => name,
                None if parsed.as_object().and_then(|object| object.get(&"workflow")).is_some_and(|name| !name.is_null()) => {
                    workflow.label.clone()
                }
                None => workflow
                    .steps
                    .first()
                    .map(|step| step.name.clone())
                    .unwrap_or_default(),
            };
            Ok(AgentAction::Run { label, workflow })
        }
        "wait" => {
            reject_unused(&parsed, "wait", &["ids", "timeout"])?;
            let ids = read_ids(&parsed);
            if ids.is_empty() {
                return Err(ActionError::NeedsIds("wait"));
            }
            let timeout_s = parsed
                .as_object().and_then(|object| object.get(&"timeout"))
                .and_then(sonic_rs::JsonValueTrait::as_u64)
                .and_then(|timeout| u16::try_from(timeout).ok())
                .unwrap_or(60);
            Ok(AgentAction::Wait { ids, timeout_s })
        }
        "cancel" => {
            reject_unused(&parsed, "cancel", &["ids"])?;
            let ids = read_ids(&parsed);
            if ids.is_empty() {
                return Err(ActionError::NeedsIds("cancel"));
            }
            Ok(AgentAction::Cancel { ids })
        }
        "list" => {
            reject_unused(&parsed, "list", &["ids"])?;
            Ok(AgentAction::List {
                ids: read_ids(&parsed),
            })
        }
        _ => Err(ActionError::UnknownAction(action)),
    }
}
