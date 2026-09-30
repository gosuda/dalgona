// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Goal tools and commands: scope gates, `create_goal`, `update_goal`,
//! `get_goal`, `/goal`, `/continuation` replies, and the `/goal clear`
//! recovery document.

use std::fmt::Write as _;
use dal_core::Timestamp;
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};

use super::super::monitor::InflightCounts;
use super::super::{ControllerMode, GoalStatus};
use super::sidecar::{
    BlockedReason, Goal, GoalError, GoalSidecar, controller_wire, goal_status_wire,
};

/// Description for the model-visible `create_goal` tool.
pub(crate) const CREATE_GOAL_DESCRIPTION: &str = "Register a goal for work that outlives this turn: it waits on external state, or the requested outcome needs more than one verify-and-fix round. A single answer, lookup, or one-shot edit needs no goal. Objectives are limited to 4000 characters; put longer instructions in a file and name the file. Fails while an unfinished goal exists.";

/// Schema for the model-visible `create_goal` tool.
pub(crate) const CREATE_GOAL_SCHEMA: &str = "{\"type\":\"object\",\"properties\":{\"objective\":{\"type\":\"string\",\"minLength\":1,\"description\":\"The concrete objective, at most 4000 characters.\"}},\"required\":[\"objective\"],\"additionalProperties\":false}";

/// Description for the model-visible `update_goal` tool.
pub(crate) const UPDATE_GOAL_DESCRIPTION: &str = "Set the goal's status to complete or blocked. Run the completion audit or the blocked audit of the goal prompt first; only a passing audit permits the call. complete is rejected while todo tasks are open. blocked needs a reason; it is rejected while a job, monitor, scheduled continuation, or open question can still deliver, and until the goal has had three goal turns since it became active or the user last spoke. Pausing and resuming are the user's actions.";

/// Schema for the model-visible `update_goal` tool.
pub(crate) const UPDATE_GOAL_SCHEMA: &str = "{\"type\":\"object\",\"properties\":{\"status\":{\"type\":\"string\",\"enum\":[\"complete\",\"blocked\"]},\"reason\":{\"type\":\"string\"}},\"required\":[\"status\"],\"additionalProperties\":false}";

/// Description for the model-visible `get_goal` tool.
pub(crate) const GET_GOAL_DESCRIPTION: &str =
    "Show the current goal: objective, status, tokens used, and time used.";

/// Schema for the model-visible `get_goal` tool.
pub(crate) const GET_GOAL_SCHEMA: &str =
    "{\"type\":\"object\",\"properties\":{},\"additionalProperties\":false}";

/// Maximum objective length in Unicode scalar values.
pub(crate) const MAX_OBJECTIVE: usize = 4000;

/// Goal turns required before a model block.
pub(crate) const BLOCK_MIN_TURNS: u32 = 3;

/// Session facts available without filesystem access.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct GoalScope<'a> {
    /// Current session id.
    pub(crate) session: &'a str,
    /// Whether the session is saved (ephemeral sessions hold no goal).
    pub(crate) saved: bool,
    /// Session depth; children of depth greater than zero hold no goal.
    pub(crate) depth: u32,
}

/// Open-todo summary feeding the completion gate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TodoSummary {
    /// Todos in pending or in-progress state.
    pub(crate) open: usize,
    /// Total todos.
    pub(crate) total: usize,
    /// First open subjects in list order.
    pub(crate) first_titles: Vec<Box<str>>,
}

/// Update target for `update_goal`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UpdateTarget {
    /// Mark the goal complete.
    Complete,
    /// Mark the goal blocked with a reason.
    Blocked,
}

/// One `/goal` command after argument parsing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum GoalCommand {
    /// `/goal` with no argument: show the goal.
    Show,
    /// `/goal <objective>`: replace any existing goal.
    Create { objective: Box<str> },
    /// `/goal pause`.
    Pause,
    /// `/goal resume`.
    Resume,
    /// `/goal clear`.
    Clear,
}

/// Refuses an ephemeral session, a child session, or a sidecar owned by
/// another session before any tool or `/goal` action.
///
/// # Errors
///
/// Returns the exact scoping error for refused input.
pub(crate) fn check_scope(sidecar: &GoalSidecar, ctx: &GoalScope<'_>) -> Result<(), GoalError> {
    if !ctx.saved {
        return Err(GoalError::NotSaved);
    }
    if ctx.depth > 0 {
        return Err(GoalError::Child);
    }
    if sidecar.session.as_ref() != ctx.session {
        return Err(GoalError::SessionMismatch {
            other: sidecar.session.clone(),
        });
    }
    Ok(())
}

/// Formats one duration with the jobs service duration format.
#[must_use]
pub(crate) fn format_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}.0s")
    } else if secs < 3_600 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h{:02}m{:02}s", secs / 3_600, (secs / 60) % 60, secs % 60)
    }
}

/// Renders the goal success text shared by the tools and bare `/goal`.
#[must_use]
pub(crate) fn success_text(goal: &Goal) -> String {
    let mut text = format!(
        "goal {}: {}\nobjective: {}\ntime: {} · tokens: {}",
        goal.id,
        goal_status_wire(goal.status),
        goal.objective,
        format_duration(goal.time_used_s),
        goal.tokens_used,
    );
    if goal.status == GoalStatus::Blocked
        && let Some(blocked) = goal.blocked.as_ref()
    {
        let _ = write!(text, "\nblocked: {}", blocked.reason);
    }
    text
}

/// Builds one fresh active goal without touching the sidecar counter.
fn fresh_goal(id: Box<str>, objective: &str, now: Timestamp) -> Goal {
    Goal {
        id,
        objective: objective.into(),
        status: GoalStatus::Active,
        created_at: now,
        updated_at: now,
        tokens_used: 0,
        time_used_s: 0,
        consecutive: 0,
        unattended: 0,
        length_recoveries: 0,
        toolless_streak: 0,
        goal_turns: 0,
        last_signature: None,
        recent_hashes: Vec::new(),
        blocked: None,
        completed_at: None,
    }
}

/// Takes the next session-local id `g<n>` from the sidecar counter.
fn take_goal_id(sidecar: &mut GoalSidecar) -> Box<str> {
    let id: Box<str> = format!("g{}", sidecar.next_goal).into_boxed_str();
    sidecar.next_goal = sidecar.next_goal.saturating_add(1);
    id
}

/// Registers one goal; fails while an unfinished goal exists.
///
/// # Errors
///
/// Returns the exact `create_goal:` error for refused input.
pub(crate) fn create_goal(
    sidecar: &mut GoalSidecar,
    ctx: &GoalScope<'_>,
    objective: &str,
    now: Timestamp,
) -> Result<String, GoalError> {
    check_scope(sidecar, ctx)?;
    if let Some(existing) = sidecar.goal.as_ref()
        && existing.status != GoalStatus::Complete
    {
        return Err(GoalError::CreateUnfinished {
            id: existing.id.clone(),
            status: goal_status_wire(existing.status).into(),
        });
    }
    let count = objective.chars().count();
    if count == 0 {
        return Err(GoalError::CreateEmpty);
    }
    if count > MAX_OBJECTIVE {
        return Err(GoalError::CreateTooLong { count });
    }
    let id = take_goal_id(sidecar);
    let goal = fresh_goal(id, objective, now);
    let reply = success_text(&goal);
    sidecar.goal = Some(goal);
    Ok(reply)
}

/// Renders nonzero inflight sources in status order for the blocked error.
/// Asks appear only when every other source is zero.
fn inflight_parts(inflight: &InflightCounts) -> String {
    let mut parts: Vec<String> = Vec::new();
    if inflight.jobs == 1 {
        parts.push("1 job".to_owned());
    } else if inflight.jobs > 1 {
        parts.push(format!("{} jobs", inflight.jobs));
    }
    if inflight.monitors == 1 {
        parts.push("1 monitor".to_owned());
    } else if inflight.monitors > 1 {
        parts.push(format!("{} monitors", inflight.monitors));
    }
    if inflight.goal_timer > 0 {
        parts.push("goal".to_owned());
    }
    if inflight.loop_guard > 0 {
        parts.push("loop guard".to_owned());
    }
    if parts.is_empty() && inflight.asks > 0 {
        parts.push("asks".to_owned());
    }
    parts.join(" · ")
}

/// Sets the goal to complete or blocked through the ordered update gates.
///
/// # Errors
///
/// Returns the exact `update_goal:` error for refused input, evaluated in
/// order: goal missing, goal not active, status/reason mismatch, then
/// status-specific gates.
pub(crate) fn update_goal(
    sidecar: &mut GoalSidecar,
    ctx: &GoalScope<'_>,
    target: UpdateTarget,
    reason: Option<&str>,
    todos: &TodoSummary,
    inflight: &InflightCounts,
    now: Timestamp,
) -> Result<String, GoalError> {
    check_scope(sidecar, ctx)?;
    let Some(goal) = sidecar.goal.as_mut() else {
        return Err(GoalError::UpdateMissing);
    };
    if goal.status != GoalStatus::Active {
        return Err(GoalError::UpdateNotActive {
            status: goal_status_wire(goal.status).into(),
        });
    }
    match target {
        UpdateTarget::Blocked => {
            let Some(reason) = reason else {
                return Err(GoalError::UpdateNeedReason);
            };
            if reason.chars().all(char::is_whitespace) {
                return Err(GoalError::UpdateNeedReason);
            }
            if inflight.jobs > 0
                || inflight.monitors > 0
                || inflight.asks > 0
                || inflight.goal_timer > 0
                || inflight.loop_guard > 0
            {
                return Err(GoalError::UpdateInflight {
                    parts: inflight_parts(inflight).into_boxed_str(),
                });
            }
            if goal.goal_turns < BLOCK_MIN_TURNS {
                return Err(GoalError::UpdateTooEarly {
                    turns: goal.goal_turns,
                });
            }
            goal.status = GoalStatus::Blocked;
            goal.blocked = Some(BlockedReason {
                reason: reason.into(),
                at: now,
                mechanical: false,
            });
            goal.updated_at = now;
        }
        UpdateTarget::Complete => {
            if reason.is_some() {
                return Err(GoalError::UpdateNoReason);
            }
            if todos.open > 0 {
                let titles = todos
                    .first_titles
                    .iter()
                    .take(3)
                    .map(AsRef::as_ref)
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(GoalError::UpdateTodosOpen {
                    count: todos.open,
                    titles: titles.into_boxed_str(),
                });
            }
            goal.status = GoalStatus::Complete;
            goal.updated_at = now;
            goal.completed_at = Some(now);
        }
    }
    Ok(success_text(goal))
}

/// Shows the current goal.
///
/// # Errors
///
/// Returns the exact scoping error for refused input.
pub(crate) fn get_goal(sidecar: &GoalSidecar, ctx: &GoalScope<'_>) -> Result<String, GoalError> {
    check_scope(sidecar, ctx)?;
    Ok(sidecar
        .goal
        .as_ref()
        .map_or_else(|| "No goal.".to_owned(), success_text))
}

/// Parses `/goal` arguments: empty shows, bare `pause`/`resume`/`clear` run
/// commands, and every other text replaces the goal.
#[must_use]
pub(crate) fn parse_goal_command(args: &str) -> GoalCommand {
    let trimmed = args.trim();
    match trimmed {
        "" => GoalCommand::Show,
        "pause" => GoalCommand::Pause,
        "resume" => GoalCommand::Resume,
        "clear" => GoalCommand::Clear,
        objective => GoalCommand::Create {
            objective: objective.into(),
        },
    }
}

/// Runs one `/goal` command against the sidecar, returning its exact reply.
/// A replaced goal takes a new id without an archive; resume resets the
/// continuation counters and clears the block; clear removes any goal.
#[must_use]
pub(crate) fn apply_goal_command(
    sidecar: &mut GoalSidecar,
    ctx: &GoalScope<'_>,
    command: &GoalCommand,
    now: Timestamp,
) -> String {
    if let Err(error) = check_scope(sidecar, ctx) {
        return error.to_string();
    }
    match command {
        GoalCommand::Show => sidecar
            .goal
            .as_ref()
            .map_or_else(|| "No goal.".to_owned(), success_text),
        GoalCommand::Create { objective } => {
            let count = objective.chars().count();
            if count == 0 {
                return GoalError::CreateEmpty.to_string();
            }
            if count > MAX_OBJECTIVE {
                return GoalError::CreateTooLong { count }.to_string();
            }
            let id = take_goal_id(sidecar);
            let goal = fresh_goal(id, objective, now);
            let reply = success_text(&goal);
            sidecar.goal = Some(goal);
            reply
        }
        GoalCommand::Pause => {
            let Some(goal) = sidecar.goal.as_mut() else {
                return "No goal.".to_owned();
            };
            if goal.status == GoalStatus::Complete {
                return GoalError::UpdateNotActive {
                    status: goal_status_wire(goal.status).into(),
                }
                .to_string();
            }
            goal.status = GoalStatus::Paused;
            goal.updated_at = now;
            format!("goal {} paused.", goal.id)
        }
        GoalCommand::Resume => {
            let Some(goal) = sidecar.goal.as_mut() else {
                return "No goal.".to_owned();
            };
            goal.status = GoalStatus::Active;
            goal.consecutive = 0;
            goal.unattended = 0;
            goal.goal_turns = 0;
            goal.blocked = None;
            goal.completed_at = None;
            goal.updated_at = now;
            format!("goal {} is active.", goal.id)
        }
        GoalCommand::Clear => {
            let Some(goal) = sidecar.goal.take() else {
                return "No goal.".to_owned();
            };
            format!("goal {} cleared.", goal.id)
        }
    }
}

/// Replies `automatic turns: <run|paused (<reason>)|stopped>`.
#[must_use]
pub(crate) fn continuation_line(mode: &ControllerMode) -> String {
    match mode {
        ControllerMode::Run => "automatic turns: run".to_owned(),
        ControllerMode::Paused { reason } => format!("automatic turns: paused ({reason})"),
        ControllerMode::Stopped => {
            "automatic turns: stopped. Only /continuation run starts them again.".to_owned()
        }
    }
}

/// Replies to an unknown `/continuation` argument.
#[must_use]
pub(crate) fn continuation_unknown() -> String {
    "continuation: use run, pause, or stop.".to_owned()
}

/// Appends the unsaved-sidecar suffix while keeping the in-memory mode.
#[must_use]
pub(crate) fn continuation_unsaved(reply: &str, message: &str) -> String {
    format!("{reply} (not saved: {message})")
}

/// Builds the `/goal clear` recovery document: a valid empty goal document
/// with the current in-memory controller mode and next id. Arms no P4.
#[must_use]
pub(crate) fn clear_recovery_doc(
    session: &str,
    controller: ControllerMode,
    next_goal: u64,
) -> Vec<u8> {
    let sidecar = GoalSidecar {
        v: 1,
        session: session.into(),
        controller,
        next_goal,
        goal: None,
    };
    super::sidecar::encode_sidecar(&sidecar).unwrap_or_else(|_| {
        format!(
            "{{\"v\":1,\"session\":\"{session}\",\"controller\":\"{}\",\"next_goal\":{next_goal},\"goal\":null}}\n",
            controller_wire(controller)
        )
        .into_bytes()
    })
}

/// Recovers the prior next id from a damaged document, if one decodes.
#[must_use]
pub(crate) fn salvage_next_goal(bytes: &[u8]) -> Option<u64> {
    let text = core::str::from_utf8(bytes).ok()?;
    let value: Value = sonic_rs::from_str(text.strip_suffix('\n').unwrap_or(text)).ok()?;
    value.as_object()?.get(&"next_goal")?.as_u64()
}
