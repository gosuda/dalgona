// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! The read-only view other batteries consume: live-work counts, the durable
//! goal, and the status payload of every session the orchestration battery
//! owns. The owner task publishes into a per-session cell at every status
//! publish, so a consumer never reaches into the runtime and never blocks its
//! message loop. One [`View`] belongs to one battery instance; the composition
//! root hands clones to the batteries that read it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use dal_core::SessionId;

use super::goal::sidecar::{Goal, goal_status_wire};
use super::monitor::status::StatusPayload;

/// The five inflight sources, read-only. Ask counts gate on the source
/// that owns them, so a consumer can weigh open questions separately from
/// background work.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InflightCounts {
    /// Queued or running top-level jobs, workflow runs included.
    pub jobs: usize,
    /// Live monitors that are not paused.
    pub monitors: usize,
    /// Open questions waiting for the person.
    pub asks: usize,
    /// Whether a goal continuation timer is scheduled.
    pub goal_timer: bool,
    /// Whether a loop-guard recovery arm is pending.
    pub loop_guard: bool,
}

impl InflightCounts {
    /// Whether any work is live: a battery that must not end while work
    /// runs waits until this is false.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.jobs > 0 || self.monitors > 0 || self.asks > 0 || self.goal_timer || self.loop_guard
    }

    /// Names the live work that is not an open question, such as
    /// `2 jobs, 1 monitor`, or `None` when only questions (or nothing) are
    /// open. A consumer that must not end while background work runs uses
    /// this text in its refusal; an open ask is the person's turn and
    /// never counts here.
    #[must_use]
    pub fn work_summary(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if self.jobs > 0 {
            let noun = if self.jobs == 1 { "job" } else { "jobs" };
            parts.push(format!("{} {noun}", self.jobs));
        }
        if self.monitors > 0 {
            let noun = if self.monitors == 1 {
                "monitor"
            } else {
                "monitors"
            };
            parts.push(format!("{} {noun}", self.monitors));
        }
        if self.goal_timer {
            parts.push("a scheduled goal continuation".to_owned());
        }
        if self.loop_guard {
            parts.push("a loop-guard recovery".to_owned());
        }
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

/// A read-only copy of one session's durable goal, or `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GoalView {
    /// Session-local id `g<n>`.
    pub id: String,
    /// Lifecycle state wire word: `active`, `paused`, `blocked`, or
    /// `complete`.
    pub status: String,
    /// The concrete objective.
    pub objective: String,
}

impl GoalView {
    pub(crate) fn from_goal(goal: &Goal) -> Self {
        Self {
            id: goal.id.to_string(),
            status: goal_status_wire(goal.status).to_owned(),
            objective: goal.objective.to_string(),
        }
    }
}

/// Everything one owner task publishes in one step, so a reader never pairs
/// counts from one moment with a goal from another.
#[derive(Clone)]
pub(crate) struct Published {
    pub(crate) inflight: InflightCounts,
    pub(crate) goal: Option<GoalView>,
    pub(crate) status: StatusPayload,
}

/// One session's published state, written by its owner task and read
/// through the [`View`].
#[derive(Clone, Default)]
pub(crate) struct Cell(Arc<Mutex<Option<Published>>>);

impl Cell {
    pub(crate) fn publish(&self, published: Published) {
        *lock(&self.0) = Some(published);
    }

    pub(crate) fn clear(&self) {
        *lock(&self.0) = None;
    }

    fn read(&self) -> Option<Published> {
        lock(&self.0).clone()
    }
}

/// A cloneable read-only handle on every live session of one orchestration
/// battery. Reads return `None` for a session the battery does not own, has
/// not yet published, or has closed.
#[derive(Clone, Default)]
pub struct View {
    sessions: Arc<Mutex<HashMap<SessionId, Cell>>>,
}

impl View {
    /// The session's live-work counts: what a battery that must not end
    /// while work is live reads.
    #[must_use]
    pub fn inflight(&self, session: SessionId) -> Option<InflightCounts> {
        Some(self.published(session)?.inflight)
    }

    /// The session's durable goal, or `None` when it has no goal or no live
    /// owner. Never mutates state.
    #[must_use]
    pub fn goal(&self, session: SessionId) -> Option<GoalView> {
        self.published(session)?.goal
    }

    /// The session's `orchestration.status` payload as JSON: `mode`,
    /// `paused_reason`, `quiet`, `inflight`, `silent_jobs`, and a nullable
    /// `goal` whose objective is cut to the preview length.
    #[must_use]
    pub fn status(&self, session: SessionId) -> Option<String> {
        sonic_rs::to_string(&self.published(session)?.status).ok()
    }

    pub(crate) fn open(&self, session: SessionId) -> Cell {
        let cell = Cell::default();
        lock(&self.sessions).insert(session, cell.clone());
        cell
    }

    pub(crate) fn close(&self, session: SessionId) {
        if let Some(cell) = lock(&self.sessions).remove(&session) {
            cell.clear();
        }
    }

    fn published(&self, session: SessionId) -> Option<Published> {
        let cell = lock(&self.sessions).get(&session).cloned()?;
        cell.read()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
