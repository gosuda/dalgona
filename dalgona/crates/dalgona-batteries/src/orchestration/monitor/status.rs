// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Status rendering: inflight counts, the `orchestration.status` payload,
//! quiet polling, and `/abort`.

use super::super::{ControllerMode, GoalStatus};

/// Live per-source counts feeding status and the blocked gate.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct InflightCounts {
    pub jobs: usize,
    pub monitors: usize,
    pub asks: usize,
    pub goal_timer: u8,
    pub loop_guard: u8,
}

/// One status emission for the `orchestration.status` update kind.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StatusPayload {
    pub mode: ControllerMode,
    pub paused_reason: Option<Box<str>>,
    pub quiet: bool,
    pub inflight: InflightCounts,
    pub silent_jobs: usize,
    pub goal: Option<GoalPreview>,
}

/// Session-local goal preview rendered into status.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GoalPreview {
    pub id: Box<str>,
    pub status: GoalStatus,
    pub objective: Box<str>,
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

/// Builds one status payload; the core emits it only when it changes and no
/// more than twice per second.
pub(crate) fn status_payload(
    mode: ControllerMode,
    quiet: bool,
    inflight: InflightCounts,
    silent_jobs: usize,
    goal: Option<GoalPreview>,
) -> StatusPayload {
    let paused_reason = match mode {
        ControllerMode::Paused { reason } => Some(reason.into()),
        ControllerMode::Run | ControllerMode::Stopped => None,
    };
    StatusPayload {
        mode,
        paused_reason,
        quiet,
        inflight,
        silent_jobs,
        goal,
    }
}

/// Serializes the exact status shape; `goal` is `null` with no goal.
/// Key order follows the struct declaration for docs truth.
pub(crate) fn status_json(payload: &StatusPayload) -> String {
    let mode = match payload.mode {
        ControllerMode::Run => "run",
        ControllerMode::Paused { .. } => "paused",
        ControllerMode::Stopped => "stopped",
    };
    let paused_reason = payload
        .paused_reason
        .as_deref()
        .map(|reason| sonic_rs::to_string(reason).unwrap_or_default())
        .unwrap_or_default();
    let paused_reason = if payload.paused_reason.is_some() {
        paused_reason
    } else {
        "null".to_owned()
    };
    let goal = payload
        .goal
        .as_ref()
        .map_or_else(
            || "null".to_owned(),
            |goal| {
                let status = match goal.status {
                    GoalStatus::Active => "active",
                    GoalStatus::Paused => "paused",
                    GoalStatus::Blocked => "blocked",
                    GoalStatus::Complete => "complete",
                };
                let id = sonic_rs::to_string(goal.id.as_ref()).unwrap_or_default();
                let objective = sonic_rs::to_string(goal.objective.as_ref()).unwrap_or_default();
                format!("{{\"id\":{id},\"status\":\"{status}\",\"objective\":{objective}}}")
            },
        );
    let inflight = payload.inflight;
    format!(
        "{{\"mode\":\"{mode}\",\"paused_reason\":{paused_reason},\"quiet\":{},\"inflight\":{{\"jobs\":{},\"monitors\":{},\"asks\":{},\"goal_timer\":{},\"loop_guard\":{}}},\"silent_jobs\":{},\"goal\":{goal}}}",
        payload.quiet,
        inflight.jobs,
        inflight.monitors,
        inflight.asks,
        inflight.goal_timer,
        inflight.loop_guard,
        payload.silent_jobs,
    )
}

/// Renders the exact status line: `working` while a turn runs, `waiting for
/// you` when idle with at least one open ask, `waiting on <parts>` when idle
/// with another nonzero count, otherwise `idle`. Appends ` · paused` or
/// ` · stopped` for those controller modes.
pub(crate) fn status_line(payload: &StatusPayload, session_idle: bool) -> String {
    let mut parts: Vec<String> = Vec::new();
    if payload.inflight.jobs == 1 {
        parts.push("1 job".to_owned());
    } else if payload.inflight.jobs > 1 {
        parts.push(format!("{} jobs", payload.inflight.jobs));
    }
    if payload.inflight.monitors == 1 {
        parts.push("1 monitor".to_owned());
    } else if payload.inflight.monitors > 1 {
        parts.push(format!("{} monitors", payload.inflight.monitors));
    }
    if payload.goal.is_some() {
        parts.push("goal".to_owned());
    }
    if payload.inflight.loop_guard > 0 {
        parts.push("loop guard".to_owned());
    }
    let mut line = if !session_idle {
        "working".to_owned()
    } else if payload.inflight.asks > 0 {
        "waiting for you".to_owned()
    } else if parts.is_empty() {
        "idle".to_owned()
    } else {
        format!("waiting on {}", parts.join(" · "))
    };
    match payload.mode {
        ControllerMode::Run => {}
        ControllerMode::Paused { .. } => line.push_str(" · paused"),
        ControllerMode::Stopped => line.push_str(" · stopped"),
    }
    line
}

/// Quiet holds only when the session is idle, every inflight source except
/// `asks` is zero, no arbiter item is ready, and nothing relevant changed
/// for 2000 ms. An open ask alone never enters the count predicate, but it
/// keeps the session non-idle through its caller.
pub(crate) fn quiet(
    session_idle: bool,
    counts: &InflightCounts,
    ready_items: usize,
    unchanged_for_ms: u64,
) -> bool {
    session_idle
        && counts.jobs == 0
        && counts.monitors == 0
        && counts.goal_timer == 0
        && counts.loop_guard == 0
        && ready_items == 0
        && unchanged_for_ms >= 2000
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
