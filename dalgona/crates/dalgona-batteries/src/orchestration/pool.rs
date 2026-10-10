// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Pool fan-out: one child per item, results stored by item index rather
//! than finish order. Scope execution wires in once the runtime group lands;
//! everything here is pure: prompts, grace texts, ending decisions, and the
//! `items_from` split.

use std::path::PathBuf;

use dal_core::JobId;

use super::agents_tool::{Report, ReportStatus};
use super::worktree::IsolationOutcome;
#[cfg(test)]
mod tests;

/// Grace-turn wait in seconds before the second interrupt.
pub(crate) const GRACE_SECONDS: f64 = 60.0;

/// Bytes of the last assistant message shown when no report arrives.
pub(crate) const LAST_MESSAGE_LIMIT: usize = 600;

/// Characters per `items_from` line.
pub(crate) const ITEM_LINE_LIMIT: usize = 2000;

/// Maximum `items_from` lines before the step fails.
pub(crate) const ITEM_LINES_LIMIT: usize = 1024;

/// Bytes of the item shown in a pool item label.
pub(crate) const ITEM_LABEL_LIMIT: usize = 40;

/// How one task ended. Every task contributes a final result; one failure
/// never erases sibling reports.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TaskState {
    Done(Report),
    Blocked(Report),
    Failed(String),
    Cancelled,
    Skipped(String),
}

impl TaskState {
    /// The state word rendered into run notices and pool reports.
    pub(crate) fn word(&self) -> &'static str {
        match self {
            TaskState::Done(_) => "done",
            TaskState::Blocked(_) => "blocked",
            TaskState::Failed(_) => "failed",
            TaskState::Cancelled => "cancelled",
            TaskState::Skipped(_) => "skipped",
        }
    }

    /// Whether the task counts as finished work for the run tally.
    pub(crate) fn is_done(&self) -> bool {
        matches!(self, TaskState::Done(_))
    }
}

/// One settled task with its recorded paths, isolation outcome, report
/// body, and item label.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TaskResult {
    pub id: JobId,
    pub state: TaskState,
    pub changed: Vec<PathBuf>,
    pub isolation: Option<IsolationOutcome>,
    /// The task's report body, kept for previews and downstream steps.
    pub body: Box<str>,
    /// The item label shown in notices and downstream pool reports.
    pub item: Box<str>,
}

/// Collects pool results by item index. Workers finish in any order; the
/// step resolves with items in declaration order.
#[derive(Clone, Debug)]
pub(crate) struct IndexCollector {
    slots: Vec<Option<TaskResult>>,
}

impl IndexCollector {
    /// Creates an empty collector for `len` items.
    pub(crate) fn new(len: usize) -> Self {
        let mut slots = Vec::with_capacity(len);
        slots.resize_with(len, || None);
        Self { slots }
    }

    /// Stores one finished item. A late result never replaces the settled
    /// one: every task keeps exactly one final state.
    pub(crate) fn insert(&mut self, index: usize, result: TaskResult) {
        if let Some(slot) = self.slots.get_mut(index)
            && slot.is_none()
        {
            *slot = Some(result);
        }
    }

    /// Whether every item has a result.
    pub(crate) fn is_complete(&self) -> bool {
        self.slots.iter().all(Option::is_some)
    }

    /// Drains results in item index order. Empty when incomplete.
    pub(crate) fn into_ordered(self) -> Vec<TaskResult> {
        if !self.is_complete() {
            return Vec::new();
        }
        self.slots.into_iter().flatten().collect()
    }
}

/// Builds the exact subagent preamble for one task label.
pub(crate) fn preamble(label: &str, rendered: &str) -> String {
    format!(
        "You are a subagent. Another agent started you for one task. The user does not see your messages; only your report reaches the agent that started you.\nWork only on this task. When you finish, or when you cannot go on, call {} exactly once. The report must name every file you changed, the commands you ran, what you found with file paths and line numbers, and anything the other agent must still do.\n\nTask \"{label}\":\n{rendered}",
        super::agents_tool::REPORT_TOOL_NAME
    )
}

/// Labels one pool item: `<step> <i>: <item, first 40 bytes>`, 1-based.
pub(crate) fn item_label(step: &str, index: usize, item: &str) -> String {
    let mut end = ITEM_LABEL_LIMIT.min(item.len());
    while !item.is_char_boundary(end) {
        end -= 1;
    }
    format!("{step} {}: {}", index + 1, &item[..end])
}

/// Why a child earned one last turn.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GraceCause {
    /// The turn ended without a report.
    NoReport,
    /// The turn used every tool round (`n`).
    ToolRounds(u32),
    /// The deadline fired after `m` minutes.
    TimeLimit(u32),
    /// The reply hit the output token limit.
    OutputLimit,
}

impl GraceCause {
    /// The reason word recorded with a late report.
    pub(crate) fn word(self) -> &'static str {
        match self {
            GraceCause::NoReport => "no report",
            GraceCause::ToolRounds(_) => "tool round limit",
            GraceCause::TimeLimit(_) => "time limit",
            GraceCause::OutputLimit => "output limit",
        }
    }

    /// The exact reason sentence opening the grace text.
    pub(crate) fn reason(self) -> String {
        match self {
            GraceCause::NoReport => format!(
                "You ended your turn without calling {}.",
                super::agents_tool::REPORT_TOOL_NAME
            ),
            GraceCause::ToolRounds(rounds) => {
                format!("You used all {rounds} tool rounds of this turn.")
            }
            GraceCause::TimeLimit(minutes) => {
                format!("You reached the time limit of {minutes} minutes.")
            }
            GraceCause::OutputLimit => "Your reply hit the output token limit.".to_owned(),
        }
    }
}

/// Builds the exact grace prompt for one cause.
pub(crate) fn grace_text(cause: GraceCause) -> String {
    format!(
        "{} You have one last turn. Call {} now with your best answer. Use status done only if the task is complete; otherwise use blocked or failed, and say in the report that your work was cut short. Do not call any other tool.",
        cause.reason(),
        super::agents_tool::REPORT_TOOL_NAME
    )
}

/// How the child's turn stopped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum StopReason {
    EndTurn,
    MaxSteps,
    Length,
    Cancelled,
    Error(String),
    Filter,
}

/// One finished child turn awaiting its verdict.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ChildEnd {
    /// The stored report, when the child called `report` with valid text.
    pub report: Option<Report>,
    /// How the turn stopped.
    pub stop: StopReason,
    /// Whether the deadline fired before the stop.
    pub deadline_hit: bool,
    /// Tool rounds allowed this turn, for the grace reason.
    pub max_rounds: u32,
    /// Minutes allowed this turn, for the grace reason.
    pub max_minutes: u32,
    /// Last assistant text, shown when no report ever arrives.
    pub last_text: String,
}

/// One settled task plus the late-report note. The note travels with the
/// verdict; the Change-8 record writer attaches it to the task outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Settled {
    pub state: TaskState,
    pub note: Option<String>,
}

/// The verdict for one finished child turn: settle now or grant one grace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ChildDecision {
    Settle(Settled),
    Grace(GraceCause),
}

/// Decides one finished child turn. A valid report always settles;
/// cancellation or provider failure without one never earns grace.
pub(crate) fn decide(end: &ChildEnd) -> ChildDecision {
    if let Some(report) = &end.report {
        let state = match report.status {
            ReportStatus::Done => TaskState::Done(report.clone()),
            ReportStatus::Blocked => TaskState::Blocked(report.clone()),
            ReportStatus::Failed => TaskState::Failed(report.text.clone()),
        };
        return ChildDecision::Settle(Settled { state, note: None });
    }
    match &end.stop {
        StopReason::Cancelled if !end.deadline_hit => ChildDecision::Settle(Settled {
            state: TaskState::Cancelled,
            note: None,
        }),
        StopReason::Error(message) => ChildDecision::Settle(Settled {
            state: TaskState::Failed(message.clone()),
            note: None,
        }),
        StopReason::Filter => ChildDecision::Settle(Settled {
            state: TaskState::Failed("the provider filtered the reply".to_owned()),
            note: None,
        }),
        StopReason::EndTurn => ChildDecision::Grace(GraceCause::NoReport),
        StopReason::MaxSteps => ChildDecision::Grace(GraceCause::ToolRounds(end.max_rounds)),
        StopReason::Length => ChildDecision::Grace(GraceCause::OutputLimit),
        StopReason::Cancelled => ChildDecision::Grace(GraceCause::TimeLimit(end.max_minutes)),
    }
}

/// Decides the grace turn. A report settles with the `reported after` note;
/// silence fails with `no report after the last turn`.
pub(crate) fn decide_grace(report: Option<&Report>, cause: GraceCause) -> Settled {
    match report {
        Some(found) => {
            let state = match found.status {
                ReportStatus::Done => TaskState::Done(found.clone()),
                ReportStatus::Blocked => TaskState::Blocked(found.clone()),
                ReportStatus::Failed => TaskState::Failed(found.text.clone()),
            };
            Settled {
                state,
                note: Some(format!("reported after {}", cause.word())),
            }
        }
        None => Settled {
            state: TaskState::Failed("no report after the last turn".to_owned()),
            note: None,
        },
    }
}

/// Renders the last assistant text shown with a no-report failure: the
/// first 600 bytes, cut on a character boundary.
pub(crate) fn last_message_text(text: &str) -> String {
    let mut end = LAST_MESSAGE_LIMIT.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("last message (not a report): {}", &text[..end])
}

/// Splits an `items_from` source report into pool items: the non-empty
/// trimmed lines, each cut to 2000 characters.
pub(crate) fn split_items(report: &str) -> Vec<String> {
    report
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let mut end = ITEM_LINE_LIMIT.min(line.len());
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            line[..end].to_owned()
        })
        .collect()
}

/// A step whose dependency produced no result never starts anything.
pub(crate) fn unresolved_skip(step: &str) -> TaskState {
    TaskState::Skipped(format!("step {step} produced no result"))
}

/// An `items_from` source with no usable lines skips the pool.
pub(crate) fn no_items_skip(step: &str) -> TaskState {
    TaskState::Skipped(format!("step {step} reported no items"))
}

/// The `items_from` failure text when the source reports too many lines.
pub(crate) fn too_many_items(step: &str, count: usize) -> String {
    format!("step {step} reported {count} items; the limit is {ITEM_LINES_LIMIT}")
}

/// The pool failure text when the session budget cannot cover the items.
pub(crate) fn pool_budget_short(step: &str, need: usize, left: u32) -> String {
    format!("step {step} needs {need} subagents, but the session has {left} left")
}

/// The run failure text when tasks did not all finish done.
pub(crate) fn unfinished_text(unfinished: usize, total: usize) -> Option<String> {
    if unfinished == 0 {
        None
    } else {
        Some(format!("{unfinished} of {total} tasks did not finish"))
    }
}
