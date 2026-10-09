// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Pool fan-out: one child per item, results stored by item index rather
//! than finish order. Everything here is pure: prompts, item labels, and
//! the `items_from` split.

use std::path::PathBuf;

use dal_core::JobId;

use super::agents_tool::Report;

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
}

/// One settled task with its recorded paths and isolation outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TaskResult {
    pub id: JobId,
    pub state: TaskState,
    pub changed: Vec<PathBuf>,
}

/// Builds the exact subagent preamble for one task label.
pub(crate) fn preamble(label: &str, rendered: &str) -> String {
    format!(
        "You are a subagent. Another agent started you for one task. The user does not see your messages; only your report reaches the agent that started you.\nWork only on this task. When you finish, or when you cannot go on, call report exactly once. The report must name every file you changed, the commands you ran, what you found with file paths and line numbers, and anything the other agent must still do.\n\nTask \"{label}\":\n{rendered}"
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

/// The `items_from` failure text when the source reports too many lines.
pub(crate) fn too_many_items(step: &str, count: usize) -> String {
    format!("step {step} reported {count} items; the limit is {ITEM_LINES_LIMIT}")
}
