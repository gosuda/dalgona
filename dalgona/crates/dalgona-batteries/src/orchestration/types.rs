// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Shared orchestration vocabulary: controller modes, stop kinds, job views,
//! and read-only status projections. One definition lives here; the owner
//! task, the arbiter, and the companion reducers all use these items.

/// Controller mode shared with the companion monitor and goal reducers.
/// The paused reason is observable as status text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ControllerMode {
    Run,
    Paused { reason: &'static str },
    Stopped,
}

/// Goal lifecycle states shared with the companion goal reducer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GoalStatus {
    Active,
    Paused,
    Blocked,
    Complete,
}

/// How the previous turn stopped, shared with the goal continuation verdict.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StopKind {
    Completed,
    Length,
    Filter,
    Error,
    Cancelled,
}

/// Read-only session-scoped jobs view consumed by the monitor reducer.
/// The body adapts to the dispatch part's job API once it lands; this trait
/// stays the single seam so no second view is introduced here.
pub(crate) trait JobsView {
    /// Parses a host-issued `UUIDv7` job id.
    /// Returns `None` when no job with that id exists in this session.
    fn resolve_job(&self, display: &str) -> Option<dal_core::JobId>;
    /// Reports whether the job is a live top-level exec job in this session.
    fn is_live_top_level_exec(&self, job: dal_core::JobId) -> bool;
    /// Counts queued or running top-level jobs.
    fn top_level_live_count(&self) -> usize;
}

/// Live per-session counts rendered into `orchestration.status`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Inflight {
    /// Queued or running top-level jobs.
    pub jobs: usize,
    /// Live monitors that are not paused.
    pub monitors: usize,
    /// Open ask requests.
    pub asks: usize,
    /// Whether a goal continuation timer is scheduled.
    pub goal_timer: bool,
    /// Whether loop-guard recovery is pending.
    pub loop_guard: bool,
}

/// Read-only goal projection for status rendering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GoalView {
    /// Session-local goal id (`g<n>`).
    pub id: String,
    /// Current lifecycle state.
    pub status: GoalStatus,
    /// Full objective text.
    pub objective: String,
}
