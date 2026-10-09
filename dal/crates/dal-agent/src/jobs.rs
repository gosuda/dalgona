use std::{
    collections::{HashMap, HashSet, VecDeque},
    io,
    path::PathBuf,
};

use dal_core::{
    JobCounts, JobEndEvent, JobEnds, JobId, JobLines, JobOutcome, JobReport, JobStateView,
    JobStatus, JobsError, Name, Timestamp,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

mod ledger;
mod lines;
mod ops;
mod reap;

pub(crate) use ledger::LedgerError;
pub(crate) use ops::{JobsCtx, run_jobs_op};
pub(crate) use reap::reap_detached;

use ledger::{Ledger, Line};
use lines::LineRing;

use crate::error::{AgentError, ToolError, ValidationError};
use crate::ext::Doc;

use super::proc::SESSION_CHILD_LIMIT;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JobState {
    Queued,
    Running,
    Detached,
    Done,
}

impl JobState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Detached => "detached",
            Self::Done => "done",
        }
    }
}

/// One reserved job row owned by a session actor.
#[derive(Debug)]
pub(crate) struct JobRecord {
    /// The job identity minted before spawn.
    pub(crate) id: JobId,
    /// The human-readable job label.
    pub(crate) label: Box<str>,
    /// The durable output file for the job.
    pub(crate) log_path: PathBuf,
    state: JobState,
    started: bool,
    tail: Box<[u8]>,
    parent: Option<JobId>,
    owner: Option<Name>,
    activity: Timestamp,
    lines: LineRing,
    /// Fires when the owning turn or an explicit cancel requests a stop.
    pub(crate) cancel: CancellationToken,
}

impl JobRecord {
    /// Reserves one job row in the queued state.
    pub(crate) fn new(
        id: JobId,
        label: impl Into<Box<str>>,
        log_path: PathBuf,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            id,
            label: label.into(),
            log_path,
            state: JobState::Queued,
            started: false,
            tail: Box::new([]),
            parent: None,
            owner: None,
            activity: Timestamp::now(),
            lines: LineRing::default(),
            cancel,
        }
    }

    /// Runs the job under `parent`: its cancel cascades and its end raises no
    /// top-level report.
    #[must_use]
    pub(crate) fn with_parent(mut self, parent: Option<JobId>) -> Self {
        self.parent = parent;
        self
    }

    /// Marks the job as ended by `owner`'s `Settle`, not by a process.
    #[must_use]
    pub(crate) fn owned_by(mut self, owner: Name) -> Self {
        self.owner = Some(owner);
        self
    }
}

/// One terminal job outcome retained until the next request boundary.
#[derive(Clone, Debug)]
pub(crate) struct FinishedJob {
    /// The settled job identity.
    pub(crate) id: JobId,
    /// The human-readable job label.
    pub(crate) label: Box<str>,
    /// The durable output file for the job.
    pub(crate) log_path: PathBuf,
    /// The terminal outcome.
    pub(crate) outcome: JobOutcome,
    /// The bounded output tail at settlement.
    pub(crate) tail: Box<[u8]>,
    /// When this job last produced output or changed state.
    activity: Timestamp,
    /// Whether the child started before settlement.
    #[cfg(test)]
    pub(crate) started: bool,
}

/// The queue is bounded independently from the active-child limit.
const JOB_QUEUE_LIMIT: usize = SESSION_CHILD_LIMIT;
/// The most job-end events the log keeps for slow readers.
const END_LOG_LIMIT: usize = 256;
/// The largest report text one owner may settle a job with.
pub(crate) const REPORT_TEXT_LIMIT: usize = 1 << 20;

/// One ended top-level report and whether a wake has claimed it.
#[derive(Debug)]
struct PendingReport {
    report: JobReport,
    taken: bool,
}

/// One session-owned table tracking queued, running, and finished jobs.
#[derive(Debug)]
pub(crate) struct JobTable {
    jobs: HashMap<JobId, JobRecord>,
    queued: VecDeque<JobId>,
    /// Running and detached jobs in start order.
    running: Vec<JobId>,
    finished: Vec<FinishedJob>,
    ends: VecDeque<JobEndEvent>,
    end_seq: u64,
    reports: VecDeque<PendingReport>,
    held: HashSet<JobId>,
    ledger: Ledger,
    jobs_dir: Option<PathBuf>,
    outbox: Vec<Line>,
    changed: watch::Sender<u64>,
}

impl Default for JobTable {
    fn default() -> Self {
        Self {
            jobs: HashMap::new(),
            queued: VecDeque::new(),
            running: Vec::new(),
            finished: Vec::new(),
            ends: VecDeque::new(),
            end_seq: 0,
            reports: VecDeque::new(),
            held: HashSet::new(),
            ledger: Ledger::default(),
            jobs_dir: None,
            outbox: Vec::new(),
            changed: watch::channel(0).0,
        }
    }
}

impl JobTable {
    /// Creates an empty session job table that persists nothing.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Opens the table over the session's durable ledger.
    ///
    /// Ended top-level reports no journaled wake delivered come back ready to
    /// take; `delivered` is the fold's set of ids journaled by wake records.
    ///
    /// # Errors
    /// Returns the ledger error when the file cannot be read or is corrupt.
    pub(crate) async fn open(
        dir: PathBuf,
        delivered: &HashSet<JobId>,
    ) -> Result<Self, LedgerError> {
        let (ledger, recovered) = Ledger::open(dir.clone(), delivered).await?;
        let job_log = |id: &JobId| dir.join(format!("{id:?}.log"));
        let mut table = Self {
            ledger,
            held: recovered.held,
            ..Self::default()
        };
        for report in recovered.ended {
            let activity = Timestamp::now();
            let tail: Box<[u8]> = report.text.as_bytes().into();
            let mut record = JobRecord::new(
                report.id,
                report.label.clone(),
                job_log(&report.id),
                CancellationToken::new(),
            );
            record.state = JobState::Done;
            record.started = true;
            record.tail.clone_from(&tail);
            record.activity = activity;
            record.lines.push(report.text.as_bytes());
            record.lines.finish();
            table.jobs.insert(report.id, record);
            table.finished.push(FinishedJob {
                id: report.id,
                label: report.label.clone(),
                log_path: job_log(&report.id),
                outcome: report.outcome.clone(),
                tail,
                activity,
                #[cfg(test)]
                started: true,
            });
            table.end_seq += 1;
            if table.ends.len() == END_LOG_LIMIT {
                table.ends.pop_front();
            }
            table.ends.push_back(JobEndEvent {
                seq: table.end_seq,
                id: report.id,
                label: report.label.clone(),
                outcome: report.outcome,
                top_level: true,
            });
        }
        table.reports = recovered
            .pending
            .into_iter()
            .map(|report| PendingReport {
                report,
                taken: false,
            })
            .collect();
        table.jobs_dir = Some(dir);
        Ok(table)
    }

    /// Writes the buffered ledger lines; call right after any settlement.
    ///
    /// # Errors
    /// Returns the ledger error; the lines stay buffered for the next flush.
    pub(crate) async fn flush(&mut self) -> Result<(), LedgerError> {
        self.ledger.write(&self.outbox).await?;
        self.outbox.clear();
        Ok(())
    }

    /// Subscribes to table changes: settlements and new output lines.
    pub(crate) fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    fn bump(&self) {
        self.changed.send_modify(|version| *version += 1);
    }

    /// Settles one job exactly once and writes the pending ledger lines
    /// before returning: owners that settle outside `jobs.*` ops and the
    /// reaper (the command-job task) must not leave the report buffered,
    /// or a session that ends here loses it on reopen. A failed write
    /// keeps the lines in the outbox for the next flush.
    pub(crate) async fn settle_durably(
        &mut self,
        id: JobId,
        outcome: JobOutcome,
        tail: Box<[u8]>,
    ) -> Option<FinishedJob> {
        let finished = self.settle_once(id, outcome, tail);
        let _ = self.flush().await;
        finished
    }

    /// Reserves one queued row behind the bounded FIFO.
    pub(crate) fn reserve(&mut self, record: JobRecord) -> Result<(), ToolError> {
        let id = record.id;
        if self.jobs.contains_key(&id) {
            return Err(ToolError::Failed(Box::new(io::Error::other(
                "job id is already reserved",
            ))));
        }
        if self.queued.len() >= JOB_QUEUE_LIMIT {
            return Err(ToolError::Failed(Box::new(io::Error::other(
                "job queue is full",
            ))));
        }
        self.queued.push_back(id);
        self.jobs.insert(id, record);
        Ok(())
    }

    /// Reserves a running row that no process backs; its owner settles it.
    pub(crate) fn spawn_owned(&mut self, mut record: JobRecord) -> Result<(), ToolError> {
        if self.jobs.contains_key(&record.id) {
            return Err(ToolError::Failed(Box::new(io::Error::other(
                "job id is already reserved",
            ))));
        }
        record.state = JobState::Running;
        record.started = true;
        self.jobs.insert(record.id, record);
        Ok(())
    }

    /// Moves one queued row straight to detached once its process is parked.
    pub(crate) fn adopt_detached(&mut self, id: JobId) -> Result<(), ToolError> {
        let Some(record) = self.jobs.get_mut(&id) else {
            return Err(invalid_transition());
        };
        if record.state != JobState::Queued || record.cancel.is_cancelled() {
            return Err(invalid_transition());
        }
        record.state = JobState::Detached;
        record.started = true;
        self.queued.retain(|queued| *queued != id);
        self.running.push(id);
        Ok(())
    }

    /// Appends output bytes to one live job's lines and stamps its activity.
    pub(crate) fn push_output(&mut self, id: JobId, bytes: &[u8]) {
        let Some(record) = self.jobs.get_mut(&id) else {
            return;
        };
        if record.state == JobState::Done || bytes.is_empty() {
            return;
        }
        record.lines.push(bytes);
        record.activity = Timestamp::now();
        self.bump();
    }

    /// Returns the number of jobs waiting for a session slot.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn queued_count(&self) -> usize {
        self.queued.len()
    }

    /// Returns the number of running or detached jobs holding a slot.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn active_count(&self) -> usize {
        self.running.len()
    }

    /// Returns the next queued job that may start without exceeding the cap.
    #[cfg(test)]
    pub(crate) fn next_queued(&mut self) -> Option<JobId> {
        if self.running.len() >= SESSION_CHILD_LIMIT {
            return None;
        }
        loop {
            let id = *self.queued.front()?;
            let cancelled = self
                .jobs
                .get(&id)
                .is_none_or(|record| record.cancel.is_cancelled());
            if !cancelled {
                return Some(id);
            }
            let _ = self.settle_once(id, JobOutcome::Cancelled, Box::new([]));
        }
    }

    /// Marks the head queued job running after a successful child start.
    pub(crate) fn mark_running(&mut self, id: JobId) -> Result<(), ToolError> {
        if self.running.len() >= SESSION_CHILD_LIMIT || self.queued.front() != Some(&id) {
            return Err(invalid_transition());
        }
        let Some(record) = self.jobs.get_mut(&id) else {
            return Err(invalid_transition());
        };
        if record.state != JobState::Queued || record.cancel.is_cancelled() {
            return Err(invalid_transition());
        }
        record.state = JobState::Running;
        record.started = true;
        self.queued.pop_front();
        self.running.push(id);
        Ok(())
    }

    /// Marks a running job detached after its foreground budget expires.
    #[cfg(test)]
    pub(crate) fn detach(&mut self, id: JobId) -> Result<(), ToolError> {
        let Some(record) = self.jobs.get_mut(&id) else {
            return Err(invalid_transition());
        };
        if record.state != JobState::Running {
            return Err(invalid_transition());
        }
        record.state = JobState::Detached;
        Ok(())
    }

    /// Refreshes the live tail snapshot for progress without settling.
    pub(crate) fn update_tail(&mut self, id: JobId, tail: Box<[u8]>) -> Result<(), ToolError> {
        let Some(record) = self.jobs.get_mut(&id) else {
            return Err(invalid_transition());
        };
        if record.state == JobState::Done {
            return Err(invalid_transition());
        }
        record.tail = tail;
        Ok(())
    }

    /// Settles one job exactly once and retains its completion for drain.
    pub(crate) fn settle_once(
        &mut self,
        id: JobId,
        outcome: JobOutcome,
        tail: Box<[u8]>,
    ) -> Option<FinishedJob> {
        let (was_active, top_level, label, log_path, activity) = {
            let record = self.jobs.get_mut(&id)?;
            if record.state == JobState::Done {
                return None;
            }
            let was_active = matches!(record.state, JobState::Running | JobState::Detached);
            record.state = JobState::Done;
            record.lines.finish();
            record.activity = Timestamp::now();
            (
                was_active,
                record.parent.is_none(),
                record.label.clone(),
                record.log_path.clone(),
                record.activity,
            )
        };
        #[cfg(test)]
        let started = self.jobs.get(&id).is_some_and(|record| record.started);
        if was_active {
            self.running.retain(|running| *running != id);
        } else {
            self.queued.retain(|queued| *queued != id);
        }
        let finished = FinishedJob {
            id,
            label,
            log_path,
            outcome,
            tail,
            activity,
            #[cfg(test)]
            started,
        };
        self.finished.push(finished.clone());
        self.record_end(&finished, top_level);
        Some(finished)
    }

    fn record_end(&mut self, finished: &FinishedJob, top_level: bool) {
        self.end_seq += 1;
        if self.ends.len() == END_LOG_LIMIT {
            self.ends.pop_front();
        }
        self.ends.push_back(JobEndEvent {
            seq: self.end_seq,
            id: finished.id,
            label: finished.label.clone(),
            outcome: finished.outcome.clone(),
            top_level,
        });
        if top_level {
            let report = JobReport {
                id: finished.id,
                label: finished.label.clone(),
                outcome: finished.outcome.clone(),
                text: String::from_utf8_lossy(&finished.tail)
                    .into_owned()
                    .into_boxed_str(),
            };
            self.outbox.push(Ledger::end(&report));
            self.reports.push_back(PendingReport {
                report,
                taken: false,
            });
        }
        self.bump();
    }

    /// Returns whether `id` names a queued, running, or detached job.
    #[must_use]
    pub(crate) fn is_live(&self, id: JobId) -> bool {
        self.jobs
            .get(&id)
            .is_some_and(|record| record.state != JobState::Done)
    }

    /// Fires the cancellation token of a job and of every job under it without
    /// waiting for reaping.
    pub(crate) fn cancel(&mut self, id: JobId) -> Result<(), AgentError> {
        self.cancel_tree(id)
            .map_err(|_| AgentError::Invalid(ValidationError::job_not_running(id)))
    }

    /// Cancels a job and its descendants; a row no process backs ends at once.
    ///
    /// # Errors
    /// Returns `Unknown` or `AlreadyEnded` and changes nothing.
    pub(crate) fn cancel_tree(&mut self, id: JobId) -> Result<(), JobsError> {
        match self.jobs.get(&id).map(|record| record.state) {
            None => return Err(JobsError::Unknown { id }),
            Some(JobState::Done) => return Err(JobsError::AlreadyEnded { id }),
            Some(_) => {}
        }
        let mut order = Vec::new();
        let mut stack = vec![id];
        while let Some(current) = stack.pop() {
            order.push(current);
            stack.extend(self.jobs.iter().filter_map(|(child, record)| {
                (record.parent == Some(current) && record.state != JobState::Done).then_some(*child)
            }));
        }
        for job in order {
            let Some(record) = self.jobs.get(&job) else {
                continue;
            };
            record.cancel.cancel();
            if record.owner.is_some() {
                let _ = self.settle_once(job, JobOutcome::Cancelled, Box::new([]));
            }
        }
        Ok(())
    }

    /// Ends `id` for the extension that spawned it, through `settle_once`.
    ///
    /// # Errors
    /// Returns `Unknown`, `NotSettleable`, `NotOwner`, `AlreadyEnded`, or
    /// `TooLarge`; each changes nothing and emits no end.
    pub(crate) fn settle_owned(
        &mut self,
        owner: &Name,
        id: JobId,
        outcome: JobOutcome,
        text: &str,
    ) -> Result<(), JobsError> {
        let Some(record) = self.jobs.get(&id) else {
            return Err(JobsError::Unknown { id });
        };
        match &record.owner {
            None => return Err(JobsError::NotSettleable { id }),
            Some(spawner) if spawner != owner => return Err(JobsError::NotOwner { id }),
            Some(_) => {}
        }
        if record.state == JobState::Done {
            return Err(JobsError::AlreadyEnded { id });
        }
        if text.len() > REPORT_TEXT_LIMIT {
            return Err(JobsError::TooLarge {
                limit: REPORT_TEXT_LIMIT,
            });
        }
        let _ = self.settle_once(id, outcome, text.as_bytes().into());
        Ok(())
    }

    /// Looks up one live or recently completed job.
    #[must_use]
    pub(crate) fn find(&self, id: JobId) -> Option<JobStatus> {
        self.list().into_iter().find(|status| status.id == id)
    }

    /// Reads job-end events after `after`.
    #[must_use]
    pub(crate) fn ends_after(&self, after: Option<u64>) -> JobEnds {
        let after = after.unwrap_or(0);
        let first = self
            .ends
            .front()
            .map_or(self.end_seq + 1, |event| event.seq);
        let events: Vec<JobEndEvent> = self
            .ends
            .iter()
            .filter(|event| event.seq > after)
            .cloned()
            .collect();
        let next = events.last().map_or(after, |event| event.seq);
        JobEnds {
            events,
            next,
            dropped: first.saturating_sub(after.saturating_add(1)),
        }
    }

    /// Reads one job's output lines after `after`.
    #[must_use]
    pub(crate) fn lines_after(&self, id: JobId, after: Option<u64>) -> Option<JobLines> {
        let record = self.jobs.get(&id)?;
        let read = record.lines.read(after);
        Some(JobLines {
            lines: read.lines,
            next: read.next,
            dropped: read.dropped,
            ended: record.state == JobState::Done,
        })
    }

    /// Takes up to `limit` ended top-level reports no wake has claimed.
    pub(crate) fn take(&mut self, limit: usize) -> Vec<JobReport> {
        self.reports
            .iter_mut()
            .filter(|pending| !pending.taken)
            .take(limit)
            .map(|pending| {
                pending.taken = true;
                pending.report.clone()
            })
            .collect()
    }

    /// Finishes taken reports; the ids that were not taken are not returned.
    pub(crate) fn commit(&mut self, ids: &[JobId]) -> Vec<JobId> {
        let mut changed = Vec::new();
        for id in ids {
            let Some(position) = self
                .reports
                .iter()
                .position(|pending| pending.report.id == *id && pending.taken)
            else {
                continue;
            };
            self.reports.remove(position);
            changed.push(*id);
        }
        changed
    }

    /// Returns taken reports to the queue so a later take offers them again.
    pub(crate) fn release(&mut self, ids: &[JobId]) -> Vec<JobId> {
        let mut changed = Vec::new();
        for pending in &mut self.reports {
            if pending.taken && ids.contains(&pending.report.id) {
                pending.taken = false;
                changed.push(pending.report.id);
            }
        }
        changed
    }

    /// Adds or removes one job in the held set, durably.
    ///
    /// # Errors
    /// Returns the ledger error and leaves the set unchanged.
    pub(crate) async fn set_held(
        &mut self,
        id: JobId,
        held: bool,
    ) -> Result<Vec<JobId>, LedgerError> {
        if self.held.contains(&id) != held {
            let line = if held {
                Ledger::hold(id)
            } else {
                Ledger::unhold(id)
            };
            self.ledger.write(&[line]).await?;
            if held {
                self.held.insert(id);
            } else {
                self.held.remove(&id);
            }
        }
        Ok(self.held.iter().copied().collect())
    }

    /// Counts the table's jobs by state.
    #[must_use]
    pub(crate) fn counts(&self) -> JobCounts {
        let detached = self
            .running
            .iter()
            .filter(|id| {
                self.jobs
                    .get(id)
                    .is_some_and(|record| record.state == JobState::Detached)
            })
            .count();
        JobCounts {
            queued: self.queued.len(),
            running: self.running.len() - detached,
            detached,
            done: self.finished.len(),
            held: self.held.len(),
        }
    }

    /// Drains retained completions and removes their rows.
    #[cfg(test)]
    pub(crate) fn drain_finished(&mut self) -> Vec<FinishedJob> {
        let finished = std::mem::take(&mut self.finished);
        for job in &finished {
            self.jobs.remove(&job.id);
        }
        finished
    }

    /// Returns the recorded outcome of one finished job (Q13).
    ///
    /// The outcome is retained until the next drain, so repeated waits serve
    /// the same recording; queued and running jobs have nothing to return.
    #[must_use]
    pub(crate) fn wait(&self, id: JobId) -> Option<JobOutcome> {
        self.finished
            .iter()
            .rev()
            .find(|job| job.id == id)
            .map(|job| job.outcome.clone())
    }

    /// Snapshots every known job (Q13).
    ///
    /// Queued jobs list in FIFO order, running and detached jobs in start
    /// order, and finished jobs in settlement order. `JobStateView` has no
    /// queued variant, so a queued job reports `Running`.
    #[must_use]
    pub(crate) fn list(&self) -> Vec<JobStatus> {
        let mut rows = Vec::with_capacity(self.jobs.len());
        for id in &self.queued {
            if let Some(record) = self.jobs.get(id) {
                rows.push(JobStatus {
                    id: *id,
                    label: record.label.clone(),
                    state: JobStateView::Running,
                    log: Some(record.log_path.clone()),
                    last_activity_at: record.activity,
                });
            }
        }
        for id in &self.running {
            if let Some(record) = self.jobs.get(id) {
                let state = if record.state == JobState::Detached {
                    JobStateView::Detached
                } else {
                    JobStateView::Running
                };
                rows.push(JobStatus {
                    id: *id,
                    label: record.label.clone(),
                    state,
                    log: Some(record.log_path.clone()),
                    last_activity_at: record.activity,
                });
            }
        }
        for job in &self.finished {
            rows.push(JobStatus {
                id: job.id,
                label: job.label.clone(),
                state: JobStateView::Done(job.outcome.clone()),
                log: Some(job.log_path.clone()),
                last_activity_at: job.activity,
            });
        }
        rows
    }

    /// Returns the collected output text of one job (Q13).
    ///
    /// A settled job serves its bounded final tail until the next drain; a
    /// queued or running job serves the live tail for progress reads.
    #[must_use]
    pub(crate) fn text(&self, id: JobId) -> Option<Box<str>> {
        if let Some(job) = self.finished.iter().rev().find(|job| job.id == id) {
            return Some(
                String::from_utf8_lossy(&job.tail)
                    .into_owned()
                    .into_boxed_str(),
            );
        }
        let record = self.jobs.get(&id)?;
        Some(
            String::from_utf8_lossy(&record.tail)
                .into_owned()
                .into_boxed_str(),
        )
    }

    /// Resolves `job://<id>` and `job://<id>/log` against live rows.
    pub(crate) fn read_uri(&self, uri: &str) -> Result<Doc, ToolError> {
        let rest = uri.strip_prefix("job://").ok_or_else(|| {
            ToolError::Scheme(crate::error::SchemeError::NotFound { uri: uri.into() })
        })?;
        let (id_text, route) = rest
            .split_once('/')
            .map_or((rest, ""), |(id, route)| (id, route));
        let id = JobId::parse(id_text).map_err(|_| {
            ToolError::Scheme(crate::error::SchemeError::NotFound { uri: uri.into() })
        })?;
        let record = self
            .jobs
            .get(&id)
            .ok_or_else(|| ToolError::Failed(Box::new(ValidationError::job_not_running(id))))?;
        let text = match route {
            "" => format!(
                "job {id}: {}. Full output: {}",
                record.state.as_str(),
                record.log_path.display()
            ),
            "log" => String::from_utf8_lossy(&record.tail).into_owned(),
            _ => {
                return Err(ToolError::Scheme(crate::error::SchemeError::NotFound {
                    uri: uri.into(),
                }));
            }
        };
        Ok(Doc::new(uri, text))
    }
}

fn invalid_transition() -> ToolError {
    ToolError::Failed(Box::new(io::Error::other("invalid job state transition")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(cancel: CancellationToken) -> JobRecord {
        JobRecord::new(
            JobId::new_v7(),
            "command",
            PathBuf::from("/session/jobs/call.log"),
            cancel,
        )
    }

    #[test]
    fn job_lifecycle_settles_once_and_releases_one_session_slot() -> Result<(), ToolError> {
        let mut jobs = JobTable::new();
        let record = record(CancellationToken::new());
        let id = record.id;
        jobs.reserve(record)?;
        assert_eq!(jobs.next_queued(), Some(id));
        jobs.mark_running(id)?;
        assert_eq!(jobs.active_count(), 1);
        jobs.detach(id)?;
        let Some(first) =
            jobs.settle_once(id, JobOutcome::Exited { code: 0 }, Box::from(&b"done"[..]))
        else {
            return Err(ToolError::Failed(Box::new(io::Error::other(
                "first terminal outcome was not retained",
            ))));
        };
        assert!(first.started);
        assert_eq!(jobs.active_count(), 0);
        assert!(
            jobs.settle_once(id, JobOutcome::Cancelled, Box::new([]))
                .is_none()
        );
        let drained = jobs.drain_finished();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].outcome, JobOutcome::Exited { code: 0 });
        assert_eq!(drained[0].tail.as_ref(), b"done");
        assert!(jobs.drain_finished().is_empty());
        Ok(())
    }

    #[test]
    fn session_queue_is_fifo_and_never_starts_over_the_cap() -> Result<(), ToolError> {
        let mut jobs = JobTable::new();
        let mut ids = Vec::with_capacity(SESSION_CHILD_LIMIT + 1);
        for _ in 0..SESSION_CHILD_LIMIT {
            let record = record(CancellationToken::new());
            ids.push(record.id);
            jobs.reserve(record)?;
        }
        assert_eq!(jobs.queued_count(), SESSION_CHILD_LIMIT);
        for id in ids.iter().take(SESSION_CHILD_LIMIT) {
            assert_eq!(jobs.next_queued(), Some(*id));
            jobs.mark_running(*id)?;
        }
        assert_eq!(jobs.active_count(), SESSION_CHILD_LIMIT);
        assert_eq!(jobs.next_queued(), None);
        jobs.settle_once(ids[0], JobOutcome::Exited { code: 0 }, Box::new([]));
        let extra = record(CancellationToken::new());
        let extra_id = extra.id;
        jobs.reserve(extra)?;
        assert_eq!(jobs.next_queued(), Some(extra_id));
        jobs.mark_running(extra_id)?;
        assert_eq!(jobs.active_count(), SESSION_CHILD_LIMIT);
        Ok(())
    }

    #[test]
    fn waiting_queue_rejects_overflow_with_backpressure() -> Result<(), ToolError> {
        let mut jobs = JobTable::new();
        for _ in 0..SESSION_CHILD_LIMIT {
            jobs.reserve(record(CancellationToken::new()))?;
        }
        let overflow = jobs.reserve(record(CancellationToken::new()));
        assert!(overflow.is_err());
        Ok(())
    }

    #[test]
    fn queued_cancellation_never_reserves_a_running_slot() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut jobs = JobTable::new();
        let token = CancellationToken::new();
        let record = record(token.clone());
        let id = record.id;
        jobs.reserve(record)
            .map_err(|error| Box::<dyn std::error::Error>::from(format!("{error:?}")))?;
        jobs.cancel(id)?;
        assert!(token.is_cancelled());
        assert_eq!(jobs.active_count(), 0);
        assert_eq!(jobs.next_queued(), None);
        let finished = jobs.drain_finished();
        assert_eq!(finished.len(), 1);
        assert!(!finished[0].started);
        assert_eq!(finished[0].outcome, JobOutcome::Cancelled);
        assert!(jobs.cancel(id).is_err());
        Ok(())
    }

    #[test]
    fn wait_serves_the_retained_outcome_until_the_drain() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut jobs = JobTable::new();
        let record = record(CancellationToken::new());
        let id = record.id;
        jobs.reserve(record)
            .map_err(|error| Box::<dyn std::error::Error>::from(format!("{error:?}")))?;
        assert_eq!(jobs.wait(id), None);
        jobs.settle_once(id, JobOutcome::Exited { code: 0 }, Box::from(&b"out"[..]));
        assert_eq!(jobs.wait(id), Some(JobOutcome::Exited { code: 0 }));
        assert_eq!(jobs.wait(id), Some(JobOutcome::Exited { code: 0 }));
        jobs.drain_finished();
        assert_eq!(jobs.wait(id), None);
        Ok(())
    }

    #[test]
    fn list_orders_queued_running_and_finished_rows() -> Result<(), Box<dyn std::error::Error>> {
        let mut jobs = JobTable::new();
        let mut rows = Vec::new();
        for label in ["first", "second"] {
            let mut row = record(CancellationToken::new());
            row.label = label.into();
            rows.push(row);
        }
        let first = rows[0].id;
        let second = rows[1].id;
        for row in rows {
            jobs.reserve(row)
                .map_err(|error| Box::<dyn std::error::Error>::from(format!("{error:?}")))?;
        }
        jobs.mark_running(first)?;
        jobs.detach(first)?;
        let mut third = record(CancellationToken::new());
        third.label = "third".into();
        let third_id = third.id;
        jobs.reserve(third)
            .map_err(|error| Box::<dyn std::error::Error>::from(format!("{error:?}")))?;
        jobs.settle_once(third_id, JobOutcome::Cancelled, Box::new([]));
        let listed = jobs.list();
        assert_eq!(listed.len(), 3);
        assert_eq!(listed[0].id, second);
        assert_eq!(listed[0].state, JobStateView::Running);
        assert_eq!(listed[0].label.as_ref(), "second");
        assert_eq!(listed[1].id, first);
        assert_eq!(listed[1].state, JobStateView::Detached);
        assert_eq!(listed[2].id, third_id);
        assert_eq!(listed[2].state, JobStateView::Done(JobOutcome::Cancelled));
        Ok(())
    }

    #[test]
    fn text_serves_the_live_tail_then_the_settled_tail() -> Result<(), Box<dyn std::error::Error>> {
        let mut jobs = JobTable::new();
        let record = record(CancellationToken::new());
        let id = record.id;
        jobs.reserve(record)
            .map_err(|error| Box::<dyn std::error::Error>::from(format!("{error:?}")))?;
        assert_eq!(jobs.text(id).as_deref(), Some(""));
        jobs.update_tail(id, Box::from(&b"partial"[..]))?;
        assert_eq!(jobs.text(id).as_deref(), Some("partial"));
        jobs.settle_once(id, JobOutcome::Exited { code: 3 }, Box::from(&b"final"[..]));
        assert_eq!(jobs.text(id).as_deref(), Some("final"));
        assert_eq!(jobs.wait(id), Some(JobOutcome::Exited { code: 3 }));
        Ok(())
    }
    #[tokio::test]
    async fn report_release_replays_and_committed_wake_ids_do_not() {
        let root = tempfile::tempdir().expect("temporary job report ledger");
        let mut jobs = JobTable::open(root.path().to_path_buf(), &HashSet::new())
            .await
            .expect("open ledger");
        let id = JobId::new_v7();
        let owner = Name::test();
        jobs.spawn_owned(
            JobRecord::new(
                id,
                "run",
                root.path().join("run.log"),
                CancellationToken::new(),
            )
            .owned_by(owner.clone()),
        )
        .expect("reserve owner job");
        jobs.settle_owned(&owner, id, JobOutcome::Exited { code: 0 }, "report")
            .expect("settle owner job");
        jobs.flush().await.expect("persist ended report");
        assert_eq!(jobs.take(1)[0].id, id);
        assert_eq!(jobs.release(&[id]), vec![id]);
        drop(jobs);

        let mut jobs = JobTable::open(root.path().to_path_buf(), &HashSet::new())
            .await
            .expect("reopen released report");
        assert_eq!(jobs.take(1)[0].id, id);
        assert_eq!(jobs.commit(&[id]), vec![id]);
        drop(jobs);

        let delivered = HashSet::from([id]);
        let mut jobs = JobTable::open(root.path().to_path_buf(), &delivered)
            .await
            .expect("reopen after journaled wake");
        assert!(jobs.take(1).is_empty());
    }

    #[tokio::test]
    async fn settle_durably_writes_the_report_before_returning() {
        let root = tempfile::tempdir().expect("temporary job report ledger");
        let mut jobs = JobTable::open(root.path().to_path_buf(), &HashSet::new())
            .await
            .expect("open ledger");
        let id = JobId::new_v7();
        jobs.reserve(JobRecord::new(
            id,
            "export",
            root.path().join("export.log"),
            CancellationToken::new(),
        ))
        .expect("reserve command job");
        let _ = jobs.mark_running(id);
        jobs.settle_durably(
            id,
            JobOutcome::Exited { code: 0 },
            Box::from(&b"report"[..]),
        )
        .await;
        drop(jobs);

        // No caller flushed after the settle: if the End line had stayed
        // buffered, reopening would replay nothing.
        let mut jobs = JobTable::open(root.path().to_path_buf(), &HashSet::new())
            .await
            .expect("reopen ledger");
        assert_eq!(
            jobs.take(1)[0].id,
            id,
            "the settled report is durable without a second flush"
        );
    }

    #[test]
    fn job_end_and_output_cursors_are_bounded_and_exact_once() {
        let mut jobs = JobTable::new();
        let id = JobId::new_v7();
        let owner = Name::test();
        jobs.spawn_owned(
            JobRecord::new(
                id,
                "run",
                PathBuf::from("/jobs/run.log"),
                CancellationToken::new(),
            )
            .owned_by(owner.clone()),
        )
        .expect("reserve owner job");
        for index in 0..513 {
            jobs.push_output(id, format!("line-{index}\n").as_bytes());
        }
        let before_end = jobs.lines_after(id, None).expect("line snapshot");
        assert_eq!(before_end.lines.len(), 512);
        assert_eq!(before_end.dropped, 1);
        assert_eq!(before_end.lines[0].text.as_ref(), "line-1");

        jobs.settle_owned(&owner, id, JobOutcome::Exited { code: 0 }, "report")
            .expect("settle once");
        assert_eq!(jobs.ends_after(None).events.len(), 1);
        assert!(matches!(
            jobs.settle_owned(&owner, id, JobOutcome::Cancelled, "second"),
            Err(JobsError::AlreadyEnded { id: ended }) if ended == id
        ));
        assert_eq!(jobs.ends_after(None).events.len(), 1);
    }
}
