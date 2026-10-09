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
}
