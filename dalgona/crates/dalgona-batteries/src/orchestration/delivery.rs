// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Completed run snapshots: bounded notices, report URIs, and the single
//! honesty suffix appended once per injection that carries run reports.
//! Builders take timing and live-record data as explicit parameters so the
//! rendered result remains bounded and deterministic.

use std::path::{Path, PathBuf};

use dal_core::JobId;

use super::CLAIM_HONESTY;
use super::pool::{TaskResult, TaskState};
use super::worktree::IsolationOutcome;

#[cfg(test)]
mod tests;

/// Default bound of one run notice.
pub(crate) const RUN_NOTICE_LIMIT: usize = 12_288;

/// Maximum changed paths shown per task before folding the remainder.
pub(crate) const CHANGED_SHOWN: usize = 3;

/// Bytes reserved past the last shown pool task so the overflow line always
/// fits: `(<n> more tasks: read job://<id>)` stays under this bound.
const OVERFLOW_RESERVE: usize = 96;

/// One completed run ready for its single top-level job report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RunResult {
    pub id: JobId,
    pub label: String,
    pub tasks: Vec<TaskResult>,
}

impl RunResult {
    /// Counts tasks that did not finish done.
    pub(crate) fn unfinished(&self) -> usize {
        self.tasks
            .iter()
            .filter(|task| !task.state.is_done())
            .count()
    }

    /// Whether every task ended done.
    pub(crate) fn is_done(&self) -> bool {
        !self.tasks.is_empty() && self.unfinished() == 0
    }
}

/// Builds a `job://` URI for a run or task report.
pub(crate) fn report_uri(id: JobId) -> String {
    format!("job://{id}")
}

/// Renders the task tally: `done`, `failed`, and `cancelled` in order;
/// zeros omitted; `blocked` counts as failed and `skipped` never counts.
pub(crate) fn counts_line<'a>(states: impl IntoIterator<Item = &'a TaskState>) -> String {
    let mut done = 0;
    let mut failed = 0;
    let mut cancelled = 0;
    for state in states {
        match state {
            TaskState::Done(_) => done += 1,
            TaskState::Blocked(_) | TaskState::Failed(_) => failed += 1,
            TaskState::Cancelled => cancelled += 1,
            TaskState::Skipped(_) => {}
        }
    }
    let mut parts = Vec::with_capacity(3);
    if done > 0 {
        parts.push(format!("{done} done"));
    }
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    if cancelled > 0 {
        parts.push(format!("{cancelled} cancelled"));
    }
    parts.join(", ")
}

/// The preview bound for a run with `total` tasks:
/// `max 160 (min 1200 (8192 / tasks))`.
pub(crate) fn preview_limit(total_tasks: usize) -> usize {
    let share = 8192 / total_tasks.max(1);
    160.max(1200.min(share))
}

/// Cuts a preview at a UTF-8 boundary and marks the cut with `...`.
pub(crate) fn preview(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &text[..end])
}

/// Renders changed paths: at most three, then `(+<k> more)`; `none` empty.
pub(crate) fn changed_list(changed: &[PathBuf]) -> String {
    if changed.is_empty() {
        return "none".to_owned();
    }
    let names = changed
        .iter()
        .take(CHANGED_SHOWN)
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    if changed.len() > CHANGED_SHOWN {
        format!("{names} (+{} more)", changed.len() - CHANGED_SHOWN)
    } else {
        names
    }
}

/// Renders every changed path for a task record; `none` when empty.
pub(crate) fn changed_all(changed: &[PathBuf]) -> String {
    if changed.is_empty() {
        return "none".to_owned();
    }
    changed
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn isolation_suffix(outcome: Option<&IsolationOutcome>) -> &'static str {
    match outcome {
        None => "",
        Some(IsolationOutcome::Clean) => " · isolation: clean",
        Some(IsolationOutcome::Merged) => " · isolation: merged",
        Some(IsolationOutcome::Kept { .. }) => " · isolation: kept",
        Some(IsolationOutcome::Retained { .. }) => " · isolation: retained",
    }
}

/// One task as shown in a pool: `- <id> "<label>": <word> · changed: ...`.
pub(crate) fn task_line(
    id: JobId,
    label: &str,
    state: &TaskState,
    changed: &[PathBuf],
    suffix: &str,
) -> String {
    format!(
        "- {id} \"{label}\": {} · changed: {}{suffix}",
        state.word(),
        changed_list(changed)
    )
}

/// One task step line: `step <name>: <state> · <task id> · changed: ...`.
pub(crate) fn step_task_line(
    step: &str,
    state: &TaskState,
    id: JobId,
    changed: &[PathBuf],
    suffix: &str,
) -> String {
    format!(
        "step {step}: {} · {id} · changed: {}{suffix}",
        state.word(),
        changed_list(changed)
    )
}

/// One pool step line: `step <name>: pool of <n> · <counts>`.
pub(crate) fn step_pool_line(step: &str, states: &[&TaskState]) -> String {
    format!(
        "step {step}: pool of {} · {}",
        states.len(),
        counts_line(states.iter().copied())
    )
}

/// One skipped step line: `step <name>: skipped · <reason>`.
pub(crate) fn step_skipped_line(step: &str, reason: &str) -> String {
    format!("step {step}: skipped · {reason}")
}

/// The overflow line hiding `hidden` tasks of one run.
pub(crate) fn more_tasks_line(hidden: usize, run: JobId) -> String {
    format!("({hidden} more tasks: read {})", report_uri(run))
}

/// One task inside a notice section.
#[derive(Clone)]
pub(crate) struct TaskNotice<'a> {
    pub id: JobId,
    pub label: &'a str,
    pub state: &'a TaskState,
    pub changed: &'a [PathBuf],
    pub preview_text: &'a str,
    pub suffix: &'a str,
}

/// One notice section: a task step (one task), a pool step (many tasks),
/// or a skipped step (a reason, no tasks).
pub(crate) struct StepNotice<'a> {
    pub name: &'a str,
    pub tasks: Vec<TaskNotice<'a>>,
    /// Set for pool steps; task steps carry exactly one task.
    pub pool: bool,
    /// Set for skipped steps; tasks must then be empty.
    pub skipped: Option<&'a str>,
}

/// Problem severity for problems-first pool ordering.
fn severity(state: &TaskState) -> u8 {
    match state {
        TaskState::Failed(_) => 0,
        TaskState::Blocked(_) => 1,
        TaskState::Cancelled => 2,
        TaskState::Done(_) => 3,
        TaskState::Skipped(_) => 4,
    }
}

/// Whether notice lines carry report previews.
#[derive(Clone, Copy)]
enum Preview {
    /// Cut previews at the bound.
    Show(usize),
    /// Step lines only.
    Hide,
}

/// Renders one section without any budget. Pool tasks come problems-first,
/// then done in item order.
fn section_lines(section: &StepNotice, preview_mode: Preview) -> Vec<String> {
    if let Some(reason) = section.skipped {
        return vec![step_skipped_line(section.name, reason)];
    }
    if !section.pool {
        let task = &section.tasks[0];
        let mut lines = vec![step_task_line(
            section.name,
            task.state,
            task.id,
            task.changed,
            task.suffix,
        )];
        if let Preview::Show(limit) = preview_mode
            && !task.preview_text.is_empty()
        {
            lines.push(format!("  {}", preview(task.preview_text, limit)));
        }
        return lines;
    }
    let states: Vec<&TaskState> = section.tasks.iter().map(|task| task.state).collect();
    let mut lines = vec![step_pool_line(section.name, &states)];
    let mut order: Vec<usize> = (0..section.tasks.len()).collect();
    order.sort_by_key(|index| severity(section.tasks[*index].state));
    for index in order {
        let task = &section.tasks[index];
        lines.push(task_line(
            task.id,
            task.label,
            task.state,
            task.changed,
            task.suffix,
        ));
        if let Preview::Show(limit) = preview_mode
            && !task.preview_text.is_empty()
        {
            lines.push(format!("    {}", preview(task.preview_text, limit)));
        }
    }
    lines
}

/// The honesty tail: the report URI line plus its verification reminder.
fn honesty_tail(run: JobId) -> String {
    format!(
        "Read {} for the full report of the run or of one task.\n{CLAIM_HONESTY}",
        report_uri(run)
    )
}

/// Builds the notice body section by section until the budget is spent. A
/// pool section that does not fit contributes its fitting prefix plus the
/// overflow line; anything after a dropped section is dropped too.
fn notice_body(run: JobId, sections: &[StepNotice], preview_mode: Preview, room: usize) -> String {
    let mut body = String::new();
    let mut rest = room;
    for section in sections {
        if let Some(reason) = section.skipped {
            let line = step_skipped_line(section.name, reason);
            if rest < line.len() + 1 {
                break;
            }
            rest -= line.len() + 1;
            body.push('\n');
            body.push_str(&line);
            continue;
        }
        if !section.pool {
            let lines = section_lines(section, preview_mode);
            let block: String = lines.join("\n");
            if rest < block.len() + 1 {
                break;
            }
            rest -= block.len() + 1;
            body.push('\n');
            body.push_str(&block);
            continue;
        }
        let states: Vec<&TaskState> = section.tasks.iter().map(|task| task.state).collect();
        let header = step_pool_line(section.name, &states);
        if rest < header.len() + 1 {
            break;
        }
        // The overflow line always fits: task chunks stop one reserve early.
        let reserve = OVERFLOW_RESERVE;
        let mut order: Vec<usize> = (0..section.tasks.len()).collect();
        order.sort_by_key(|index| severity(section.tasks[*index].state));
        let mut shown = 0;
        let mut left = rest - (header.len() + 1);
        let mut block = header;
        for index in order {
            let task = &section.tasks[index];
            let mut lines = vec![task_line(
                task.id,
                task.label,
                task.state,
                task.changed,
                task.suffix,
            )];
            if let Preview::Show(limit) = preview_mode
                && !task.preview_text.is_empty()
            {
                lines.push(format!("    {}", preview(task.preview_text, limit)));
            }
            let chunk: String = lines.join("\n");
            if left < chunk.len() + 1 + reserve {
                break;
            }
            left -= chunk.len() + 1;
            block.push('\n');
            block.push_str(&chunk);
            shown += 1;
        }
        let hidden = section.tasks.len() - shown;
        if hidden > 0 {
            let overflow = more_tasks_line(hidden, run);
            if left < overflow.len() + 1 {
                break;
            }
            block.push('\n');
            block.push_str(&overflow);
            left -= overflow.len() + 1;
        }
        rest = left;
        body.push('\n');
        body.push_str(&block);
    }
    body
}

/// Renders the bounded run notice: header, step lines with indented
/// previews, the report URI tail, and one honesty suffix.
pub(crate) fn run_notice(
    id: JobId,
    label: &str,
    state_word: &str,
    duration: &str,
    sections: &[StepNotice],
    byte_limit: usize,
) -> String {
    let total: usize = sections.iter().map(|section| section.tasks.len()).sum();
    let states: Vec<&TaskState> = sections
        .iter()
        .flat_map(|section| section.tasks.iter().map(|task| task.state))
        .collect();
    let header = format!(
        "run {id} \"{label}\" {state_word} in {duration} · {total} tasks: {}",
        counts_line(states)
    );
    let tail = honesty_tail(id);
    let room = byte_limit.saturating_sub(header.len() + tail.len() + 2);
    let mut notice = header;
    notice.push_str(&notice_body(
        id,
        sections,
        Preview::Show(preview_limit(total)),
        room,
    ));
    notice.push('\n');
    notice.push_str(&tail);
    notice
}

/// Renders the cancel summary: like the notice, but the run line names the
/// cancellation and no previews are shown.
pub(crate) fn cancel_summary(
    id: JobId,
    label: &str,
    duration: &str,
    sections: &[StepNotice],
    byte_limit: usize,
) -> String {
    let total: usize = sections.iter().map(|section| section.tasks.len()).sum();
    let states: Vec<&TaskState> = sections
        .iter()
        .flat_map(|section| section.tasks.iter().map(|task| task.state))
        .collect();
    let header = format!(
        "run {id} \"{label}\" cancelled after {duration} · {total} tasks: {}",
        counts_line(states)
    );
    let tail = honesty_tail(id);
    let room = byte_limit.saturating_sub(header.len() + tail.len() + 2);
    let mut notice = header;
    notice.push_str(&notice_body(id, sections, Preview::Hide, room));
    notice.push('\n');
    notice.push_str(&tail);
    notice
}

/// Renders one task record: `task <id> "<label>" of run <run>: <word> in
/// <duration>`, every changed path, a blank line, and the full body.
pub(crate) fn task_text(
    id: JobId,
    label: &str,
    run: JobId,
    word: &str,
    duration: &str,
    changed: &[PathBuf],
    body: &str,
) -> String {
    format!(
        "task {id} \"{label}\" of run {run}: {word} in {duration}\nchanged: {}\n\n{body}",
        changed_all(changed)
    )
}

/// Confirms every claimed changed path parses as a relative path.
pub(crate) fn changed_paths_valid(changed: &[PathBuf]) -> bool {
    changed.iter().all(|path| {
        let path: &Path = path.as_ref();
        !path.as_os_str().is_empty() && path.is_relative()
    })
}
