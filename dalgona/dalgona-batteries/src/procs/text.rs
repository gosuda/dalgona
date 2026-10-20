// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Pure render side of the `procs` tool: ages, list rows, and grep text.

use std::fmt::Write as _;

use dal_core::ext::JobStateView;
use dal_core::{JobOutcome, JobStatus, Timestamp};
use regex_automata::meta::Regex;

/// Terminal rows shown after the live rows.
pub(crate) const TERMINAL_ROWS: usize = 10;

/// Characters kept of the list command text.
pub(crate) const COMMAND_CHARS: usize = 80;

/// Bytes the whole grep result may reach, elision tail included.
pub(crate) const RESULT_BYTES: usize = 16_384;

/// One list row status word: `running`, `done`, `cancelled`, or `failed`.
/// A job that exits nonzero, fails, or is lost is `failed`.
pub(crate) fn status_word(state: &JobStateView) -> &'static str {
    match state {
        JobStateView::Running | JobStateView::Detached => "running",
        JobStateView::Done(JobOutcome::Exited { code: 0 }) => "done",
        JobStateView::Done(JobOutcome::Cancelled) => "cancelled",
        _ => "failed",
    }
}

/// Whether `status` names the phase of `state`.
pub(crate) fn state_matches(state: &JobStateView, status: &str) -> bool {
    status_word(state) == status
}

/// Formats one age with the jobs duration format (`48.0s`, `4m12s`).
pub(crate) fn format_age(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}.0s")
    } else if secs < 3_600 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h{:02}m{:02}s", secs / 3_600, (secs / 60) % 60, secs % 60)
    }
}

/// Whole seconds from `earlier` to `now`, saturating skew below zero.
fn secs_since(now: Timestamp, earlier: Timestamp) -> u64 {
    if now < earlier {
        return 0;
    }
    let seconds = now.as_second().saturating_sub(earlier.as_second());
    let nanos =
        i64::from(now.subsec_nanosecond()).saturating_sub(i64::from(earlier.subsec_nanosecond()));
    let total = i128::from(seconds) * 1_000_000_000 + i128::from(nanos);
    u64::try_from(total.div_euclid(1_000_000_000)).unwrap_or(u64::MAX)
}

/// The command text one row shows: the starter's command when the job
/// carries it, otherwise the label, collapsed to one line and cut at
/// [`COMMAND_CHARS`] characters. Only the shown prefix is built, so a long
/// command is never copied whole.
pub(crate) fn command_text(job: &JobStatus) -> String {
    collapsed_prefix(job.command.as_deref().unwrap_or(&job.label), COMMAND_CHARS)
}

/// `text` with every whitespace run collapsed to one space, cut at `max`
/// characters; reads only as much of `text` as the cut needs.
fn collapsed_prefix(text: &str, max: usize) -> String {
    let mut collapsed = String::new();
    let mut chars = 0;
    for (index, word) in text.split_whitespace().enumerate() {
        if index > 0 {
            if chars == max {
                break;
            }
            collapsed.push(' ');
            chars += 1;
        }
        for character in word.chars() {
            if chars == max {
                return collapsed;
            }
            collapsed.push(character);
            chars += 1;
        }
    }
    collapsed
}

/// One list line: `<id> <status> <age> <label>: <command>`, the command
/// collapsed to one line and cut at [`COMMAND_CHARS`] characters.
pub(crate) fn list_line(job: &JobStatus, now: Timestamp, command: &str) -> String {
    format!(
        "{} {} {} {}: {}",
        job.id,
        status_word(&job.state),
        format_age(secs_since(now, job.last_activity_at)),
        job.label,
        cut_chars(command, COMMAND_CHARS),
    )
}

/// Cuts a string to at most `max` characters.
fn cut_chars(value: &str, max: usize) -> &str {
    match value.char_indices().nth(max) {
        Some((index, _)) => &value[..index],
        None => value,
    }
}

/// The escaped form of the first `max` bytes of `line`.
///
/// Control characters render `\t`, `\r`, `\n`, and `\u{XX}` for the rest;
/// the byte cut never splits a UTF-8 sequence.
pub(crate) fn escaped_prefix(line: &str, max: usize) -> String {
    let cut = crate::text::byte_prefix(line, max).len();
    let mut escaped = String::with_capacity(cut);
    for character in line[..cut].chars() {
        match character {
            '\t' => escaped.push_str("\\t"),
            '\r' => escaped.push_str("\\r"),
            '\n' => escaped.push_str("\\n"),
            control if u32::from(control) < 0x20 || u32::from(control) == 0x7f => {
                let _ = write!(escaped, "\\u{{{:02x}}}", u32::from(control));
            }
            other => escaped.push(other),
        }
    }
    escaped
}

/// The matching lines of `text`, each with its one-based line number.
pub(crate) fn matching_lines<'a>(
    regex: &'a Regex,
    text: &'a str,
) -> impl Iterator<Item = (usize, &'a str)> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| regex.is_match(line.as_bytes()))
        .map(|(index, line)| (index + 1, line))
}

/// The elision tail for `elided` matches cut by the caps.
fn elision_tail(elided: usize) -> String {
    format!("\n({elided} more matches elided; narrow query or raise max_matches.)")
}

/// Joins `rendered` into the grep result of at most [`RESULT_BYTES`] bytes.
///
/// `seen_total` counts every match the scan saw. Lines that do not fit are
/// dropped from the end, and every match not shown, dropped or never kept,
/// is counted in the elision tail. The first line always stays.
pub(crate) fn finish_grep(mut rendered: Vec<String>, seen_total: usize) -> String {
    let mut length =
        rendered.iter().map(String::len).sum::<usize>() + rendered.len().saturating_sub(1);
    while rendered.len() > 1 {
        let elided = seen_total - rendered.len();
        let tail = if elided == 0 {
            0
        } else {
            elision_tail(elided).len()
        };
        if length + tail <= RESULT_BYTES {
            break;
        }
        if let Some(dropped) = rendered.pop() {
            length -= dropped.len() + 1;
        }
    }
    let mut text = rendered.join("\n");
    let elided = seen_total - rendered.len();
    if elided > 0 {
        text.push_str(&elision_tail(elided));
    }
    text
}
