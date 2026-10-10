// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Pure workflow validation and prompt rendering.

use sonic_rs::Value;

const STEP_FIELDS: [&str; 11] = [
    "name",
    "prompt",
    "items",
    "items_from",
    "workers",
    "after",
    "tools",
    "model",
    "role",
    "system",
    "isolation",
];
pub(crate) const FORBIDDEN_TOOLS: [&str; 7] = [
    "agents",
    "report",
    super::agents_tool::REPORT_TOOL_NAME,
    "create_goal",
    "update_goal",
    "get_goal",
    "monitor",
];
const POOL_REPORT_LIMIT: usize = 16_384;

/// Whether a workflow step writes in its own git worktree or in the shared checkout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Isolation {
    /// The child writes in its own worktree.
    Worktree,
    /// The child writes in the real checkout.
    Shared,
}

/// The item source for one step.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Items {
    /// Start one child for the task.
    Task,
    /// Start one child per literal item.
    Literal(Vec<String>),
    /// Use the report lines of an earlier task step.
    From(String),
}

/// One validated DAG step.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Step {
    /// Stable step name.
    pub name: String,
    /// Prompt template.
    pub prompt: String,
    /// Task or pool source.
    pub items: Items,
    /// Maximum concurrent children for a pool.
    pub workers: u8,
    /// Dependency step indexes in declaration order.
    pub after: Vec<usize>,
    /// Child tool names.
    pub tools: Vec<String>,
    /// Optional model override.
    pub model: Option<String>,
    /// Optional role preset.
    pub role: Option<String>,
    /// Optional system prompt replacement.
    pub system: Option<String>,
    /// Child workspace policy.
    pub isolation: Isolation,
}

/// A validated workflow ready for admission.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Workflow {
    /// Run label shown in job reports.
    pub label: String,
    /// Steps in declaration order.
    pub steps: Vec<Step>,
}

/// Completed report data available to downstream `{{step:name}}` templates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StepResult {
    /// Step name.
    pub name: String,
    /// Full report of a task step, when present.
    pub task_report: Option<Box<str>>,
    /// Full item results for a pool step. `None` denotes a task.
    pub pool_items: Option<Vec<PoolItemResult>>,
}

/// One completed pool item used to render a downstream dependency.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PoolItemResult {
    /// Original item text.
    pub item: Box<str>,
    /// State word.
    pub state: Box<str>,
    /// Short report summary.
    pub summary: Box<str>,
}

/// A deterministic workflow validation or rendering error.
#[derive(Debug, thiserror::Error, Eq, PartialEq)]
#[error("{0}")]
pub(crate) struct WorkflowError(Box<str>);

impl WorkflowError {
    pub(crate) fn new(message: impl Into<Box<str>>) -> Self {
        Self(message.into())
    }
}

/// Field names are checked against [`STEP_FIELDS`] by the decoder loop;
/// a derived `deny_unknown_fields` cannot apply because the raw [`Value`]
/// keeps document order for stable duplicate detection.
struct RawStep {
    index: usize,
    name: String,
    prompt: String,
    items: Option<Value>,
    items_from: Option<Value>,
    workers: Option<Value>,
    after: Option<Value>,
    tools: Option<Value>,
    model: Option<Value>,
    role: Option<Value>,
    system: Option<Value>,
    isolation: Option<Value>,
}

pub(crate) mod decode;
pub(crate) mod render;
pub(crate) mod saved;

pub(crate) use decode::decode_steps;
pub(crate) use saved::{find_saved, saved_names};
impl Workflow {
    /// Counts the statically known child sessions planned by the workflow.
    #[must_use]
    pub(crate) fn planned(&self) -> usize {
        self.steps
            .iter()
            .map(|step| match &step.items {
                Items::Task => 1,
                Items::Literal(items) => items.len(),
                Items::From(_) => 0,
            })
            .sum()
    }
}

#[cfg(test)]
mod tests;
