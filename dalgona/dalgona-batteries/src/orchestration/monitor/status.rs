// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Status rendering: inflight counts, the human status line, quiet polling,
//! and `/abort`.

use std::time::{Duration, Instant};

use dal_agent::ext::StatusSnapshot;

use super::super::ControllerMode;
use super::super::live::GoalView;

/// Live per-source counts feeding status and the blocked gate.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize)]
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

/// Characters of the goal objective the status payload carries.
pub(crate) const OBJECTIVE_PREVIEW: usize = 32;

/// The `orchestration.status` payload: mode, pause reason, quiet flag,
/// inflight counts, silent job count, and the goal with its objective cut to
/// [`OBJECTIVE_PREVIEW`] characters.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub(crate) struct StatusPayload {
    mode: &'static str,
    paused_reason: Option<&'static str>,
    quiet: bool,
    inflight: InflightCounts,
    silent_jobs: usize,
    goal: Option<StatusGoal>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
struct StatusGoal {
    id: String,
    status: String,
    objective: String,
}

impl StatusPayload {
    pub(crate) fn new(
        mode: ControllerMode,
        quiet: bool,
        inflight: InflightCounts,
        silent_jobs: usize,
        goal: Option<&GoalView>,
    ) -> Self {
        let (mode, paused_reason) = match mode {
            ControllerMode::Run => ("run", None),
            ControllerMode::Paused { reason } => ("paused", Some(reason)),
            ControllerMode::Stopped => ("stopped", None),
        };
        Self {
            mode,
            paused_reason,
            quiet,
            inflight,
            silent_jobs,
            goal: goal.map(|goal| StatusGoal {
                id: goal.id.clone(),
                status: goal.status.clone(),
                objective: goal.objective.chars().take(OBJECTIVE_PREVIEW).collect(),
            }),
        }
    }
}

/// Status updates the battery emits at most per second.
pub(crate) const UPDATE_RATE: u32 = 2;

/// Releases the owner's latest status to the host poll only when it changed,
/// and at most [`UPDATE_RATE`] times per second. The host sweeps far more
/// often than that, so the gate decides what each sweep sees.
pub(crate) struct StatusGate {
    latest: StatusSnapshot,
    emitted: StatusSnapshot,
    emitted_at: Option<Instant>,
}

impl StatusGate {
    pub(crate) fn new() -> Self {
        let quiet = StatusSnapshot {
            quiet: true,
            text: None,
        };
        Self {
            latest: quiet.clone(),
            emitted: quiet,
            emitted_at: None,
        }
    }

    pub(crate) fn publish(&mut self, snapshot: StatusSnapshot) {
        self.latest = snapshot;
    }

    #[cfg(test)]
    pub(crate) fn latest(&self) -> &StatusSnapshot {
        &self.latest
    }

    pub(crate) fn emit(&mut self, now: Instant) -> StatusSnapshot {
        let due = self.emitted_at.is_none_or(|at| {
            now.saturating_duration_since(at) >= Duration::from_secs(1) / UPDATE_RATE
        });
        if due && self.latest != self.emitted {
            self.emitted = self.latest.clone();
            self.emitted_at = Some(now);
        }
        self.emitted.clone()
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
