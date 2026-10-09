// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Automatic-turn arbitration: source priority, byte budget, and exactly-once
//! job delivery. The runtime `wake` call wires in once the turn service seam
//! lands; everything here is pure and clock-injected.

use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};

use dal_core::JobId;

use super::CLAIM_HONESTY;
use super::types::ControllerMode;

#[cfg(test)]
mod tests;

/// Maximum bytes of one injected reminder.
pub(crate) const INJECTION_BUDGET: usize = 16_384;

/// Idle time with no activity before the session counts as quiet.
pub(crate) const QUIET_AFTER: Duration = Duration::from_secs(2);

/// One ended top-level job report awaiting delivery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct JobReport {
    pub id: JobId,
    /// Report body.
    pub text: String,
    /// Whether the report comes from a subagent run.
    pub from_run: bool,
}

/// Ready automatic-turn sources in admission order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Ready {
    Recovery(String),
    Jobs(Vec<JobReport>),
    Monitor(Vec<String>),
    Goal(String),
}

/// The per-session automatic-turn controller. This owner is the sole caller
/// of `turn.wake` for the battery once the service seam lands.
#[derive(Clone, Debug)]
pub(crate) struct Arbiter {
    mode: ControllerMode,
    recovery: Option<String>,
    monitor: VecDeque<String>,
    goal: Option<String>,
    committed: HashSet<JobId>,
    last_activity: Option<Instant>,
}

impl Arbiter {
    /// Opens paused; a persisted stop is restored through [`Arbiter::stop`].
    pub(crate) fn new() -> Self {
        Self {
            mode: ControllerMode::Paused {
                reason: "session opened",
            },
            recovery: None,
            monitor: VecDeque::new(),
            goal: None,
            committed: HashSet::new(),
            last_activity: None,
        }
    }

    /// Current controller mode.
    pub(crate) fn mode(&self) -> ControllerMode {
        self.mode
    }

    /// Admits loop-guard recovery text for the next injection.
    pub(crate) fn admit_recovery(&mut self, text: String, now: Instant) {
        self.recovery = Some(text);
        self.last_activity = Some(now);
    }

    /// Admits one coalesced monitor batch.
    pub(crate) fn push_monitor(&mut self, batch: String, now: Instant) {
        self.monitor.push_back(batch);
        self.last_activity = Some(now);
    }

    /// Admits goal continuation text.
    pub(crate) fn admit_goal(&mut self, text: String, now: Instant) {
        self.goal = Some(text);
        self.last_activity = Some(now);
    }

    /// Whether a goal continuation text waits for a wake.
    pub(crate) fn goal_pending(&self) -> bool {
        self.goal.is_some()
    }

    /// Whether the next wake carries a source other than the goal: the
    /// recovery text, taken job reports, or a monitor batch. The goal
    /// verdict runs on the Idle path only at such a wake.
    pub(crate) fn wake_has_other_sources(&self, jobs: &[JobReport]) -> bool {
        self.recovery.is_some() || !jobs.is_empty() || !self.monitor.is_empty()
    }

    /// A user prompt resumes paused mode but never stopped.
    pub(crate) fn on_user_prompt(&mut self) {
        if matches!(self.mode, ControllerMode::Paused { .. }) {
            self.mode = ControllerMode::Run;
        }
    }

    /// User cancellation pauses automatic turns.
    pub(crate) fn on_user_cancel(&mut self) {
        self.mode = ControllerMode::Paused {
            reason: "cancelled by the user",
        };
    }
    /// Pauses automatic turns with a fixed owner-selected reason.
    pub(crate) fn pause(&mut self, reason: &'static str) {
        self.mode = ControllerMode::Paused { reason };
    }

    /// `/abort` pauses with its fixed reason.
    pub(crate) fn on_abort(&mut self) {
        self.mode = ControllerMode::Paused {
            reason: "aborted by the user",
        };
    }

    /// `/continuation run` resumes from any mode; only it resumes stopped.
    pub(crate) fn on_continuation_run(&mut self) {
        self.mode = ControllerMode::Run;
    }

    /// `/continuation stop` halts automatic turns until resumed.
    pub(crate) fn stop(&mut self) {
        self.mode = ControllerMode::Stopped;
    }

    pub(crate) fn collect(&mut self, jobs: Vec<JobReport>) -> Vec<Ready> {
        let mut items = Vec::new();
        let has_recovery = self.recovery.take().is_some_and(|recovery| {
            items.push(Ready::Recovery(recovery));
            true
        });
        if !jobs.is_empty() {
            items.push(Ready::Jobs(jobs));
        }
        if !self.monitor.is_empty() {
            items.push(Ready::Monitor(self.monitor.drain(..).collect()));
        }
        if !has_recovery && let Some(goal) = self.goal.take() {
            items.push(Ready::Goal(goal));
        }
        items
    }

    /// Composes one reminder within `max_bytes`. Recovery and goal text are
    /// never cut; overflowing job reports and monitor batches stay ready for
    /// another wake. Returns the text, the source names in order, and the job
    /// ids actually included (the only ids the caller may commit).
    pub(crate) fn compose(
        &self,
        items: &[Ready],
        max_bytes: usize,
    ) -> (String, Vec<&'static str>, Vec<JobId>, usize) {
        let mut text = String::new();
        let mut sources = Vec::new();
        let mut included = Vec::new();
        let mut run_report_included = false;
        let mut monitor_batches = 0;
        for item in items {
            let (source, body, ids) = match item {
                Ready::Recovery(body) => ("loop_guard", body.clone(), Vec::new()),
                Ready::Jobs(reports) => {
                    let mut body = String::new();
                    let mut ids = Vec::new();
                    for report in reports {
                        let line = format!("\n{}", report.text);
                        if text.len() + body.len() + line.len() + 1 > max_bytes {
                            break;
                        }
                        if self.committed.contains(&report.id) || ids.contains(&report.id) {
                            continue;
                        }
                        body.push_str(&line);
                        ids.push(report.id);
                        run_report_included |= report.from_run;
                    }
                    if ids.is_empty() {
                        continue;
                    }
                    ("jobs", body, ids)
                }
                Ready::Monitor(batches) => {
                    let take = Self::fit_monitor(batches, text.len(), max_bytes);
                    if take == 0 {
                        continue;
                    }
                    monitor_batches += take;
                    let mut body = String::new();
                    for batch in &batches[..take] {
                        body.push('\n');
                        body.push_str(batch);
                    }
                    ("monitor", body, Vec::new())
                }
                Ready::Goal(body) => ("goal", format!("\n{body}"), Vec::new()),
            };
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&body);
            sources.push(source);
            included.extend(ids);
        }
        if let Some(stripped) = text.strip_prefix('\n') {
            text = stripped.to_owned();
        }
        if run_report_included {
            text.push('\n');
            text.push_str(CLAIM_HONESTY);
        }
        (text, sources, included, monitor_batches)
    }

    /// Counts how many leading monitor batches fit in the remaining budget
    /// after `used` bytes. The Change-8 wake uses the same count to requeue
    /// the batches an injection leaves out, so no batch is lost to the cut.
    pub(crate) fn fit_monitor(batches: &[String], used: usize, max_bytes: usize) -> usize {
        let mut taken = 0;
        let mut count = 0;
        for batch in batches {
            if used + taken + batch.len() + 1 + 1 > max_bytes {
                break;
            }
            taken += batch.len() + 1;
            count += 1;
        }
        count
    }

    /// Commits included job ids after a successful wake. An id is never
    /// committed twice.
    pub(crate) fn commit(&mut self, ids: &[JobId]) {
        self.committed.extend(ids.iter().copied());
    }

    /// Releases ids after a failed wake so the next event redelivers them.
    pub(crate) fn release(&mut self, ids: &[JobId]) {
        for id in ids {
            self.committed.remove(id);
        }
    }

    /// Whether the id already reached a wake reminder.
    pub(crate) fn is_committed(&self, id: &JobId) -> bool {
        self.committed.contains(id)
    }

    /// Quiet holds after two seconds idle with no activity. Asks keep the
    /// session non-idle elsewhere; only other inflight counts gate here.
    pub(crate) fn quiet(
        &self,
        session_idle: bool,
        inflight_without_asks: usize,
        ready_items: usize,
        now: Instant,
    ) -> bool {
        session_idle
            && inflight_without_asks == 0
            && ready_items == 0
            && self
                .last_activity
                .is_none_or(|at| now.duration_since(at) >= QUIET_AFTER)
    }
}

impl Default for Arbiter {
    fn default() -> Self {
        Self::new()
    }
}
