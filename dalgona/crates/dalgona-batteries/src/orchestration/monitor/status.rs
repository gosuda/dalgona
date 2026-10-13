// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Status rendering: inflight counts, the human status line, quiet polling,
//! and `/abort`.

use super::super::ControllerMode;

/// Live per-source counts feeding status and the blocked gate.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct InflightCounts {
    pub jobs: usize,
    pub monitors: usize,
    pub asks: usize,
    pub goal_timer: u8,
    pub loop_guard: u8,
}

/// Builds inflight counts from the five sources only: queued or running
/// top-level jobs, live monitors that are not paused, open asks, a
/// zero-or-one goal timer, and a zero-or-one loop-guard recovery.
pub(crate) fn inflight_counts(
    jobs: usize,
    monitors: usize,
    asks: usize,
    goal_timer: bool,
    loop_guard: bool,
) -> InflightCounts {
    InflightCounts {
        jobs,
        monitors,
        asks,
        goal_timer: u8::from(goal_timer),
        loop_guard: u8::from(loop_guard),
    }
}
/// Upper bound of the one-line status text shown in the activity row.
pub(crate) const STATUS_LINE_LIMIT: usize = 120;

/// Renders the status forms shared by the TUI and other front ends.
pub(crate) fn status_line(
    mode: ControllerMode,
    session_idle: bool,
    inflight: InflightCounts,
) -> String {
    let mut line = if !session_idle {
        "working".to_owned()
    } else if inflight.asks > 0 {
        "waiting for you".to_owned()
    } else {
        let mut parts = Vec::with_capacity(4);
        if inflight.jobs > 0 {
            let noun = if inflight.jobs == 1 { "job" } else { "jobs" };
            parts.push(format!("{} {noun}", inflight.jobs));
        }
        if inflight.monitors > 0 {
            let noun = if inflight.monitors == 1 {
                "monitor"
            } else {
                "monitors"
            };
            parts.push(format!("{} {noun}", inflight.monitors));
        }
        if inflight.goal_timer > 0 {
            parts.push("goal".to_owned());
        }
        if inflight.loop_guard > 0 {
            parts.push("loop guard".to_owned());
        }
        if parts.is_empty() {
            "idle".to_owned()
        } else {
            format!("waiting on {}", parts.join(" · "))
        }
    };
    match mode {
        ControllerMode::Paused { .. } => line.push_str(" · paused"),
        ControllerMode::Stopped => line.push_str(" · stopped"),
        ControllerMode::Run => {}
    }
    if line.len() > STATUS_LINE_LIMIT {
        let mut end = STATUS_LINE_LIMIT - '…'.len_utf8();
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        line.truncate(end);
        line.push('…');
    }
    line
}

/// Builds the exact `/abort` reply after the core cancels the turn, cancels
/// every top-level job, stops every monitor, and drops the goal timer.
pub(crate) fn abort_reply(
    turn_was_running: bool,
    jobs_cancelled: usize,
    monitors_stopped: usize,
) -> String {
    let turn = if turn_was_running {
        "cancelled"
    } else {
        "idle"
    };
    format!(
        "aborted: turn {turn}, {jobs_cancelled} jobs cancelled, {monitors_stopped} monitors stopped. Automatic turns are paused; your next message resumes them."
    )
}

/// Replies when a controller command runs in a subagent.
pub(crate) fn subagent_reply(command: &str) -> String {
    format!("{command}: not available in a subagent.")
}
