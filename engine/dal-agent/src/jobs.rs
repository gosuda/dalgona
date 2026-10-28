use std::{
    collections::{HashMap, HashSet, VecDeque},
    io,
    path::PathBuf,
    sync::Arc,
};

use dal_core::{
    JobCounts, JobEndEvent, JobEnds, JobId, JobLines, JobOutcome, JobReport, JobStateView,
    JobStatus, JobsError, Name, SessionId, Timestamp,
};
use dal_store::{JobLedger, JobLedgerEntry, JobLedgerError, Store};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

#[cfg(test)]
mod ledger_recovery_tests;
mod lines;
mod ops;
mod reap;
mod slots;

pub(crate) use ops::{JobsCtx, run_jobs_op};
pub(crate) use reap::reap_detached;
pub(crate) use slots::{ProcessSlots, SlotGuard};

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
///
/// A persistent report becomes `durable` only after its `End` append is
/// acknowledged. `take` offers only durable reports. Recovery suppresses an
/// id only after matching wake input is durably materialized; a wake attempt
/// alone does not suppress it.
#[derive(Debug)]
struct PendingReport {
    report: JobReport,
    taken: bool,
    durable: bool,
}

/// One session-owned table tracking queued, running, and finished jobs.
#[derive(Debug)]
pub(crate) struct JobTable {
    jobs: HashMap<JobId, JobRecord>,
    queued: VecDeque<JobId>,
    /// Running and detached jobs in start order, including owned logical jobs.
    running: Vec<JobId>,
    finished: Vec<FinishedJob>,
    ends: VecDeque<JobEndEvent>,
    end_seq: u64,
    reports: VecDeque<PendingReport>,
    held: HashSet<JobId>,
    /// The Store-owned durable ledger; `None` is a memory-only table that
    /// persists nothing.
    ledger: Option<JobLedger>,
    outbox: Vec<JobLedgerEntry>,
    changed: watch::Sender<u64>,
    /// The session's cap on live child processes and the bounded FIFO of
    /// calls waiting for a slot. It counts guards held by real children, so
    /// coordinator-owned rows (`spawn_owned`) never count against it.
    slots: Arc<ProcessSlots>,
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
            ledger: None,
            outbox: Vec::new(),
            changed: watch::channel(0).0,
            slots: ProcessSlots::new(SESSION_CHILD_LIMIT, JOB_QUEUE_LIMIT),
        }
    }
}

impl JobTable {
    /// Creates an empty session job table that persists nothing.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Opens the table over the session's Store-owned durable ledger.
    ///
    /// The ledger path and the job-log directory both derive from
    /// [`Store::session_jobs_dir`]; the Store performs every ledger read and
    /// write.
    ///
    /// `delivered` contains caller-supplied IDs for matching wake input that was
    /// durably materialized. A journaled wake record alone does not suppress a
    /// report on replay.
    ///
    /// # Errors
    /// Returns the ledger error when the file cannot be read or is corrupt.
    pub(crate) async fn open(
        store: &Store,
        session: SessionId,
        delivered: &HashSet<JobId>,
    ) -> Result<Self, JobLedgerError> {
        let (ledger, recovered) = store.job_ledger(session, delivered).await?;
        let dir = store.session_jobs_dir(session);
        let job_log = |id: &JobId| dir.join(format!("{id:?}.log"));
        let mut table = Self {
            ledger: Some(ledger),
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
                durable: true,
            })
            .collect();
        Ok(table)
    }

    /// Writes the buffered ledger lines; call right after any settlement.
    ///
    /// A memory-only table has no ledger: nothing is written, the buffered
    /// lines are discarded, and its reports are durable at once.
    ///
    /// # Errors
    /// Returns the ledger error; the lines stay buffered for the next flush.
    pub(crate) async fn flush(&mut self) -> Result<(), JobLedgerError> {
        if let Some(ledger) = &mut self.ledger {
            ledger.append(&self.outbox).await?;
        }
        for pending in &mut self.reports {
            pending.durable = true;
        }
        self.outbox.clear();
        Ok(())
    }

    /// Subscribes to table changes: settlements and new output lines.
    pub(crate) fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    /// Returns the session's process-slot ledger.
    ///
    /// Callers clone the `Arc`, release the table lock, and only then wait on
    /// the ledger, so a queued call never holds the table.
    pub(crate) fn process_slots(&self) -> Arc<ProcessSlots> {
        Arc::clone(&self.slots)
    }

    fn bump(&self) {
        self.changed.send_modify(|version| *version += 1);
    }

    /// Settles one job exactly once and writes the pending ledger lines
    /// before returning: owners that settle outside `jobs.*` ops and the
    /// reaper (the command-job task) must not leave the report buffered,
    /// or a session that ends here loses it on reopen.
    ///
    /// # Errors
    /// Returns the ledger error; the settlement stays recorded and the
    /// lines stay buffered for the next flush.
    pub(crate) async fn settle_durably(
        &mut self,
        id: JobId,
        outcome: JobOutcome,
        tail: Box<[u8]>,
    ) -> Result<Option<FinishedJob>, JobLedgerError> {
        let finished = self.settle_once(id, outcome, tail);
        self.flush().await.map(|()| finished)
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
        let id = record.id;
        self.jobs.insert(id, record);
        self.running.push(id);
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

    /// Returns the number of process-backed running or detached jobs holding a slot.
    #[must_use]
    pub(crate) fn active_count(&self) -> usize {
        self.running
            .iter()
            .filter(|id| {
                self.jobs
                    .get(id)
                    .is_some_and(|record| record.owner.is_none() && record.state != JobState::Done)
            })
            .count()
    }

    /// Returns the next queued job that may start without exceeding the cap.
    #[cfg(test)]
    pub(crate) fn next_queued(&mut self) -> Option<JobId> {
        if self.active_count() >= SESSION_CHILD_LIMIT {
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
        if self.active_count() >= SESSION_CHILD_LIMIT || self.queued.front() != Some(&id) {
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
            // Memory-only tables (`JobTable::new`, `ledger: None`) persist
            // nothing, so the report is durable at once and `take` without
            // `flush` keeps working. Ledger-backed tables gate on `flush`
            // success instead.
            let durable = self.ledger.is_none();
            self.outbox.push(JobLedger::end(&report));
            self.reports.push_back(PendingReport {
                report,
                taken: false,
                durable,
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

    /// Fires every live job's token, settles every live row as cancelled,
    /// and flushes the terminal ledger ends before returning.
    ///
    /// Session close calls this before draining tasks: detached
    /// reapers dropped by the drain would otherwise abandon their rows with
    /// no terminal ledger end. Firing first still lets a live reaper stop its
    /// child through the group ladder; settling here guarantees the end even
    /// when the reaper never wakes, since `settle_once` keeps the first end.
    ///
    /// # Errors
    /// Returns the ledger error; the cancellations stay recorded and the
    /// lines stay buffered for the next flush.
    pub(crate) async fn cancel_all(&mut self) -> Result<(), JobLedgerError> {
        let live: Vec<JobId> = self
            .jobs
            .iter()
            .filter_map(|(id, record)| (record.state != JobState::Done).then_some(*id))
            .collect();
        for id in live {
            let Some(record) = self.jobs.get(&id) else {
                continue;
            };
            record.cancel.cancel();
            let _ = self.settle_once(id, JobOutcome::Cancelled, Box::new([]));
        }
        self.flush().await
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
        let record = self.jobs.get(&id)?;
        let state = match record.state {
            JobState::Queued | JobState::Running => JobStateView::Running,
            JobState::Detached => JobStateView::Detached,
            JobState::Done => JobStateView::Done(self.wait(id)?),
        };
        Some(JobStatus {
            id,
            label: record.label.clone(),
            state,
            log: Some(record.log_path.clone()),
            last_activity_at: record.activity,
        })
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

    /// Takes up to `limit` durable ended top-level reports no wake has claimed.
    ///
    /// Reports whose `End` line has not yet flushed stay hidden until the
    /// retry makes them durable. `wait`/`find`/`ends_after` still serve the
    /// buffered settlement locally; only offering through `take` is gated.
    pub(crate) fn take(&mut self, limit: usize) -> Vec<JobReport> {
        self.reports
            .iter_mut()
            .filter(|pending| !pending.taken && pending.durable)
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
    ) -> Result<Vec<JobId>, JobLedgerError> {
        if self.held.contains(&id) != held {
            if let Some(ledger) = &mut self.ledger {
                let entry = if held {
                    JobLedger::hold(id)
                } else {
                    JobLedger::unhold(id)
                };
                ledger.append(&[entry]).await?;
            }
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

    /// One real [`Store`] over a temporary data root and one stable session
    /// id. Every open of the rig reads the jobs directory the Store derives
    /// for that session, so a reopen sees exactly what an earlier table
    /// wrote and nothing is addressed by an arbitrary path.
    pub(super) struct StoreRig {
        _root: tempfile::TempDir,
        store: Store,
        session: SessionId,
    }

    impl StoreRig {
        pub(super) fn new() -> Self {
            let root = tempfile::tempdir().expect("temporary store data root");
            let store = Store::new(
                root.path().join("data"),
                dal_core::Workspace::new(root.path().join("workspace"))
                    .expect("absolute workspace path"),
                dal_core::Product::Dal,
            );
            Self {
                _root: root,
                store,
                session: SessionId::new_v7(),
            }
        }

        /// The session `jobs/` directory the Store derives.
        pub(super) fn jobs_dir(&self) -> PathBuf {
            self.store.session_jobs_dir(self.session)
        }

        /// The job log path of `label` below the session `jobs/` directory.
        pub(super) fn log(&self, label: &str) -> PathBuf {
            self.jobs_dir().join(format!("{label}.log"))
        }

        /// Opens a table over this session's Store-owned ledger.
        pub(super) async fn open(
            &self,
            delivered: &HashSet<JobId>,
        ) -> Result<JobTable, JobLedgerError> {
            JobTable::open(&self.store, self.session, delivered).await
        }
    }

    fn record(cancel: CancellationToken) -> JobRecord {
        JobRecord::new(
            JobId::new_v7(),
            "command",
            PathBuf::from("/session/jobs/call.log"),
            cancel,
        )
    }

    fn wake_turn(value: u64) -> dal_core::TurnId {
        use std::num::NonZeroU64;
        dal_core::TurnId::new(NonZeroU64::new(value).unwrap())
    }

    fn wake_entry(value: u64) -> dal_core::EntryId {
        use std::num::NonZeroU64;
        dal_core::EntryId::new(NonZeroU64::new(value).unwrap())
    }

    fn wake_stamp() -> jiff::Timestamp {
        jiff::Timestamp::UNIX_EPOCH
    }

    fn wake_turn_start(turn: u64) -> dal_core::Record {
        dal_core::Record::TurnStart {
            at: wake_stamp(),
            turn: wake_turn(turn),
        }
    }

    fn wake_turn_end(turn: u64) -> dal_core::Record {
        dal_core::Record::TurnEnd {
            at: wake_stamp(),
            turn: wake_turn(turn),
            stop: dal_core::TurnEndStop::Done,
            usage: None,
            changes: Vec::new(),
        }
    }

    fn wake_user(entry: u64, parent: Option<dal_core::EntryId>) -> dal_core::Record {
        dal_core::Record::User(dal_core::Entry {
            id: wake_entry(entry),
            parent,
            at: wake_stamp(),
            kind: dal_core::EntryKind::User {
                parts: vec![dal_core::JournalPart::Text {
                    text: "question".into(),
                }],
            },
        })
    }

    fn wake_attempt(turn: u64, count: u32, jobs: Vec<JobId>) -> dal_core::Record {
        dal_core::Record::WakeAttempt {
            at: wake_stamp(),
            turn: wake_turn(turn),
            count,
            jobs,
        }
    }

    fn replay_delivered(journal: Vec<dal_core::Record>) -> HashSet<JobId> {
        dal_core::Session::replay_declared(journal)
            .expect("replay wake journal")
            .delivered_jobs()
            .clone()
    }

    async fn persist_one_owned(label: &str) -> (StoreRig, JobId) {
        let rig = StoreRig::new();
        let mut jobs = rig.open(&HashSet::new()).await.expect("open ledger");
        let id = JobId::new_v7();
        let owner = Name::test();
        jobs.spawn_owned(
            JobRecord::new(id, label, rig.log(label), CancellationToken::new())
                .owned_by(owner.clone()),
        )
        .expect("reserve owner job");
        jobs.settle_owned(&owner, id, JobOutcome::Exited { code: 0 }, label)
            .expect("settle owner job");
        jobs.flush().await.expect("persist ended report");
        drop(jobs);
        (rig, id)
    }

    async fn persist_two_owned() -> (StoreRig, JobId, JobId) {
        let rig = StoreRig::new();
        let mut jobs = rig.open(&HashSet::new()).await.expect("open ledger");
        let owner = Name::test();
        let first = JobId::new_v7();
        let second = JobId::new_v7();
        for (id, label) in [(first, "first"), (second, "second")] {
            jobs.spawn_owned(
                JobRecord::new(id, label, rig.log(label), CancellationToken::new())
                    .owned_by(owner.clone()),
            )
            .expect("reserve owner job");
            jobs.settle_owned(&owner, id, JobOutcome::Exited { code: 0 }, label)
                .expect("settle owner job");
        }
        jobs.flush().await.expect("persist ended reports");
        drop(jobs);
        (rig, first, second)
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
    fn owned_rows_are_observable_live_and_settle_exactly_once() -> Result<(), ToolError> {
        let mut jobs = JobTable::new();
        let owner = Name::test();
        let row = record(CancellationToken::new()).owned_by(owner.clone());
        let id = row.id;
        jobs.spawn_owned(row)?;
        assert_eq!(
            jobs.find(id).expect("live owned row").state,
            JobStateView::Running
        );
        assert_eq!(
            jobs.list().iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![id]
        );
        assert_eq!(
            jobs.counts(),
            JobCounts {
                running: 1,
                ..JobCounts::default()
            }
        );
        assert_eq!(jobs.active_count(), 0);
        assert!(
            jobs.spawn_owned(record(CancellationToken::new()).owned_by(owner.clone()))
                .is_ok()
        );
        assert!(
            jobs.spawn_owned(
                JobRecord::new(id, "duplicate", PathBuf::new(), CancellationToken::new())
                    .owned_by(owner.clone())
            )
            .is_err()
        );
        jobs.settle_owned(&owner, id, JobOutcome::Exited { code: 7 }, "report")
            .expect("settle owned row");
        assert!(matches!(
            jobs.settle_owned(&owner, id, JobOutcome::Cancelled, "duplicate"),
            Err(JobsError::AlreadyEnded { id: ended }) if ended == id
        ));
        assert_eq!(
            jobs.find(id).expect("settled owned row").state,
            JobStateView::Done(JobOutcome::Exited { code: 7 })
        );
        assert_eq!(jobs.list().iter().filter(|row| row.id == id).count(), 1);
        assert_eq!(
            jobs.counts(),
            JobCounts {
                running: 1,
                done: 1,
                ..JobCounts::default()
            }
        );
        assert_eq!(jobs.ends_after(None).events.len(), 1);
        assert_eq!(jobs.take(10).len(), 1);
        jobs.drain_finished();
        assert_eq!(jobs.find(id), None);
        Ok(())
    }

    #[test]
    fn owned_rows_do_not_consume_or_release_process_capacity() -> Result<(), ToolError> {
        let mut jobs = JobTable::new();
        let owner = Name::test();
        let mut logical_jobs = Vec::new();
        for _ in 0..=SESSION_CHILD_LIMIT {
            let row = record(CancellationToken::new()).owned_by(owner.clone());
            logical_jobs.push(row.id);
            jobs.spawn_owned(row)?;
        }
        assert_eq!(jobs.counts().running, SESSION_CHILD_LIMIT + 1);
        assert_eq!(jobs.active_count(), 0);
        let mut process = Vec::new();
        for _ in 0..SESSION_CHILD_LIMIT {
            let row = record(CancellationToken::new());
            process.push(row.id);
            jobs.reserve(row)?;
        }
        for id in &process {
            assert_eq!(jobs.next_queued(), Some(*id));
            jobs.mark_running(*id)?;
        }
        jobs.detach(process[0])?;
        let row = record(CancellationToken::new());
        let waiting = row.id;
        jobs.reserve(row)?;
        assert_eq!(jobs.active_count(), SESSION_CHILD_LIMIT);
        assert_eq!(jobs.next_queued(), None);
        assert!(jobs.mark_running(waiting).is_err());
        jobs.settle_owned(&owner, logical_jobs[0], JobOutcome::Cancelled, "cancelled")
            .expect("settle logical row at process capacity");
        assert_eq!(jobs.active_count(), SESSION_CHILD_LIMIT);
        assert_eq!(jobs.next_queued(), None);
        assert!(jobs.mark_running(waiting).is_err());
        jobs.settle_once(process[0], JobOutcome::Exited { code: 0 }, Box::new([]));
        assert_eq!(jobs.next_queued(), Some(waiting));
        jobs.mark_running(waiting)?;
        assert_eq!(jobs.active_count(), SESSION_CHILD_LIMIT);
        Ok(())
    }

    #[test]
    fn mixed_owned_and_process_rows_preserve_all_three_orders() -> Result<(), ToolError> {
        let mut jobs = JobTable::new();
        let owner = Name::test();
        let first = record(CancellationToken::new());
        let first_id = first.id;
        let second = record(CancellationToken::new());
        let second_id = second.id;
        jobs.reserve(first)?;
        jobs.reserve(second)?;
        let logical_job = record(CancellationToken::new()).owned_by(owner.clone());
        let owned_id = logical_job.id;
        jobs.spawn_owned(logical_job)?;
        jobs.mark_running(first_id)?;
        jobs.detach(first_id)?;
        let queued = record(CancellationToken::new());
        let queued_id = queued.id;
        jobs.reserve(queued)?;
        let last = record(CancellationToken::new()).owned_by(owner.clone());
        let last_id = last.id;
        jobs.spawn_owned(last)?;
        let listed = jobs.list();
        assert_eq!(
            listed.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![second_id, queued_id, owned_id, first_id, last_id]
        );
        assert_eq!(
            jobs.counts(),
            JobCounts {
                queued: 2,
                running: 2,
                detached: 1,
                ..JobCounts::default()
            }
        );
        for row in listed {
            assert_eq!(jobs.find(row.id), Some(row));
        }
        jobs.settle_owned(&owner, last_id, JobOutcome::Cancelled, "last")
            .expect("settle last starter first");
        jobs.settle_once(first_id, JobOutcome::Exited { code: 2 }, Box::new([]));
        jobs.settle_owned(&owner, owned_id, JobOutcome::Exited { code: 0 }, "first")
            .expect("settle first starter last");
        let listed = jobs.list();
        assert_eq!(
            listed.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![second_id, queued_id, last_id, first_id, owned_id]
        );
        assert_eq!(
            jobs.counts(),
            JobCounts {
                queued: 2,
                done: 3,
                ..JobCounts::default()
            }
        );
        for row in listed {
            assert_eq!(jobs.find(row.id), Some(row));
        }
        assert_eq!(jobs.next_queued(), Some(second_id));
        jobs.mark_running(second_id)?;
        assert_eq!(jobs.next_queued(), Some(queued_id));
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
        let rig = StoreRig::new();
        let mut jobs = rig.open(&HashSet::new()).await.expect("open ledger");
        let id = JobId::new_v7();
        let owner = Name::test();
        jobs.spawn_owned(
            JobRecord::new(id, "run", rig.log("run"), CancellationToken::new())
                .owned_by(owner.clone()),
        )
        .expect("reserve owner job");
        jobs.settle_owned(&owner, id, JobOutcome::Exited { code: 0 }, "report")
            .expect("settle owner job");
        jobs.flush().await.expect("persist ended report");
        assert_eq!(jobs.take(1)[0].id, id);
        assert_eq!(jobs.release(&[id]), vec![id]);
        drop(jobs);

        let mut jobs = rig
            .open(&HashSet::new())
            .await
            .expect("reopen released report");
        assert_eq!(jobs.take(1)[0].id, id);
        assert_eq!(jobs.commit(&[id]), vec![id]);
        drop(jobs);

        let delivered = replay_delivered(vec![
            wake_turn_start(1),
            wake_user(1, None),
            wake_attempt(2, 1, vec![id]),
            wake_turn_end(1),
            wake_turn_start(2),
            wake_user(2, Some(wake_entry(1))),
        ]);
        assert_eq!(
            &delivered,
            &HashSet::from([id]),
            "materialized wake input must deliver exactly its report"
        );
        let mut jobs = rig
            .open(&delivered)
            .await
            .expect("reopen after journaled wake");
        assert_eq!(jobs.take(1), []);
    }

    #[tokio::test]
    async fn settle_durably_writes_the_report_before_returning() {
        let rig = StoreRig::new();
        let mut jobs = rig.open(&HashSet::new()).await.expect("open ledger");
        let id = JobId::new_v7();
        jobs.reserve(JobRecord::new(
            id,
            "export",
            rig.log("export"),
            CancellationToken::new(),
        ))
        .expect("reserve command job");
        let _ = jobs.mark_running(id);
        jobs.settle_durably(
            id,
            JobOutcome::Exited { code: 0 },
            Box::from(&b"report"[..]),
        )
        .await
        .expect("settle_durably persists the report");
        drop(jobs);

        // No caller flushed after the settle: if the End line had stayed
        // buffered, reopening would replay nothing.
        let mut jobs = rig.open(&HashSet::new()).await.expect("reopen ledger");
        assert_eq!(
            jobs.take(1)[0].id,
            id,
            "the settled report is durable without a second flush"
        );
    }

    #[tokio::test]
    async fn queued_wake_attempt_keeps_report_pending() {
        let (rig, id) = persist_one_owned("run").await;
        let delivered = replay_delivered(vec![wake_attempt(1, 1, vec![id])]);
        assert!(
            delivered.is_empty(),
            "queued wake attempt without input must not deliver"
        );
        let mut reopened = rig
            .open(&delivered)
            .await
            .expect("reopen after queued wake");
        assert_eq!(
            reopened
                .take(1)
                .iter()
                .map(|report| report.id)
                .collect::<Vec<_>>(),
            vec![id],
            "queued wake attempt must leave the report recoverable"
        );
    }

    #[tokio::test]
    async fn turn_start_only_prefix_keeps_report_pending() {
        let (rig, id) = persist_one_owned("run").await;
        let delivered = replay_delivered(vec![wake_attempt(1, 1, vec![id]), wake_turn_start(1)]);
        assert!(
            delivered.is_empty(),
            "turn start without wake input must not deliver"
        );
        let mut reopened = rig
            .open(&delivered)
            .await
            .expect("reopen after turn-start-only prefix");
        assert_eq!(
            reopened
                .take(1)
                .iter()
                .map(|report| report.id)
                .collect::<Vec<_>>(),
            vec![id],
            "turn-start-only prefix must leave the report recoverable"
        );
    }

    #[tokio::test]
    async fn unrelated_turn_input_keeps_queued_report_pending() {
        let (rig, id) = persist_one_owned("run").await;
        let delivered = replay_delivered(vec![
            wake_turn_start(1),
            wake_user(1, None),
            wake_attempt(2, 1, vec![id]),
        ]);
        assert!(
            delivered.is_empty(),
            "unrelated turn input must not deliver the queued wake job"
        );
        let mut reopened = rig
            .open(&delivered)
            .await
            .expect("reopen after unrelated input with queued wake");
        assert_eq!(
            reopened
                .take(1)
                .iter()
                .map(|report| report.id)
                .collect::<Vec<_>>(),
            vec![id],
            "unrelated turn input must leave the report recoverable"
        );
    }

    #[tokio::test]
    async fn materialized_first_wake_input_suppresses_only_its_report() {
        let (rig, first, second) = persist_two_owned().await;
        let delivered = replay_delivered(vec![
            wake_turn_start(1),
            wake_user(1, None),
            wake_attempt(2, 1, vec![first]),
            wake_attempt(3, 2, vec![second]),
            wake_turn_end(1),
            wake_turn_start(2),
            wake_user(2, Some(wake_entry(1))),
        ]);
        assert_eq!(
            &delivered,
            &HashSet::from([first]),
            "only the materialized wake input delivers"
        );
        assert!(
            !delivered.contains(&second),
            "the still-queued second wake must not deliver"
        );

        let mut reopened = rig
            .open(&delivered)
            .await
            .expect("reopen after first wake input");
        let taken = reopened.take(10);
        assert_eq!(
            taken.len(),
            1,
            "exactly the still-queued report stays recoverable"
        );
        assert_eq!(
            taken[0].id, second,
            "materialized input suppresses only its exact report id"
        );
    }

    #[tokio::test]
    async fn lost_take_reply_replays_both_reports_with_identity_unless_wake_materialized() {
        // Adverse JobTable-level proof (not a public Services/real-Agent run):
        // a Take batch whose reply never reaches commit/release is lost on
        // crash; reopen must replay the same whole reports exactly once, and
        // only a durably materialized wake id may suppress its own report.
        let rig = StoreRig::new();
        let mut jobs = rig.open(&HashSet::new()).await.expect("open ledger");
        let owner = Name::test();
        let first = JobId::new_v7();
        let second = JobId::new_v7();
        for (id, label, text, outcome) in [
            (
                first,
                "alpha-first",
                "alpha-first-body",
                JobOutcome::Exited { code: 0 },
            ),
            (
                second,
                "beta-second",
                "beta-second-body with extra detail",
                JobOutcome::Exited { code: 3 },
            ),
        ] {
            jobs.spawn_owned(
                JobRecord::new(id, label, rig.log(label), CancellationToken::new())
                    .owned_by(owner.clone()),
            )
            .expect("reserve owner job");
            jobs.settle_owned(&owner, id, outcome, text)
                .expect("settle owner job");
        }
        jobs.flush().await.expect("persist ended reports");
        let expected_first = JobReport {
            id: first,
            label: "alpha-first".into(),
            outcome: JobOutcome::Exited { code: 0 },
            text: "alpha-first-body".into(),
        };
        let expected_second = JobReport {
            id: second,
            label: "beta-second".into(),
            outcome: JobOutcome::Exited { code: 3 },
            text: "beta-second-body with extra detail".into(),
        };

        // The consumer claims both reports, then its reply is lost: no
        // commit, no release, just a crash before any wake is journaled.
        let claimed = jobs.take(10);
        assert_eq!(
            claimed,
            vec![expected_first.clone(), expected_second.clone()],
            "take must offer both whole reports once, in settle order"
        );
        drop(jobs);

        // Unmaterialized reports recover with identity, exactly once per batch.
        let mut reopened = rig
            .open(&HashSet::new())
            .await
            .expect("reopen after lost take reply");
        let replayed = reopened.take(10);
        assert_eq!(
            replayed,
            vec![expected_first.clone(), expected_second.clone()],
            "lost take reply must replay the same whole reports, not ids alone"
        );
        assert_eq!(
            reopened.take(10),
            Vec::<JobReport>::new(),
            "a replayed batch offers each report exactly once until released"
        );
        drop(reopened);

        // Only the durably materialized wake id is suppressed on reopen.
        let delivered = replay_delivered(vec![
            wake_turn_start(1),
            wake_user(1, None),
            wake_attempt(2, 1, vec![first]),
            wake_turn_end(1),
            wake_turn_start(2),
            wake_user(2, Some(wake_entry(1))),
        ]);
        assert_eq!(
            &delivered,
            &HashSet::from([first]),
            "materialized wake input must deliver exactly its report"
        );
        let mut suppressed = rig
            .open(&delivered)
            .await
            .expect("reopen after first wake materialized");
        assert_eq!(
            suppressed.take(10),
            vec![expected_second.clone()],
            "materialized input suppresses only its exact whole report"
        );
        assert_eq!(
            suppressed.take(10),
            Vec::<JobReport>::new(),
            "the surviving report still offers exactly once"
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

    /// Attacks on durable-before-visible job reports.
    ///
    /// A report may be offered to a wake only once its `End` line is on
    /// disk. Every outage below is a real filesystem failure: a regular file
    /// takes the place of the session `jobs/` directory, so the ledger
    /// append cannot create its parent. The directory is the one the real
    /// [`Store`] derives for a stable session id. Nothing is mocked, and
    /// nothing outside the test-owned temporary root is touched.
    mod unflushed_report_visibility {
        use std::{collections::HashSet, fs, path::PathBuf, sync::Arc};

        use dal_core::{JobId, JobOutcome, JobReport, JobsOp, JobsReply, Name, RawJson};
        use dal_store::JobLedgerError;
        use tokio::sync::Mutex;
        use tokio_util::sync::CancellationToken;

        use super::super::{JobRecord, JobTable, JobsCtx, run_jobs_op};
        use super::{
            StoreRig, replay_delivered, wake_attempt, wake_entry, wake_turn_end, wake_turn_start,
            wake_user,
        };

        /// The Store-derived session `jobs/` directory path of one
        /// [`StoreRig`], which the test can block for real.
        struct Outage {
            rig: StoreRig,
            dir: PathBuf,
        }

        impl Outage {
            fn new() -> Self {
                let rig = StoreRig::new();
                let dir = rig.jobs_dir();
                Self { rig, dir }
            }

            fn aside(&self) -> PathBuf {
                self.dir.with_file_name("jobs.aside")
            }

            /// Occupies the ledger directory path with a regular file. A live
            /// directory is moved aside intact so `unblock` restores it. The
            /// session directory above the path is created when absent: the
            /// Store creates nothing before its first append.
            fn block(&self) {
                if self.dir.exists() {
                    fs::rename(&self.dir, self.aside())
                        .expect("move the live jobs directory aside");
                }
                let session_dir = self
                    .dir
                    .parent()
                    .expect("the jobs directory has a session directory");
                fs::create_dir_all(session_dir).expect("create the session directory");
                fs::write(&self.dir, b"occupied").expect("occupy the jobs directory path");
            }

            fn unblock(&self) {
                fs::remove_file(&self.dir).expect("free the jobs directory path");
                if self.aside().exists() {
                    fs::rename(self.aside(), &self.dir).expect("restore the live jobs directory");
                }
            }

            async fn open(&self) -> JobTable {
                self.reopen(&HashSet::new()).await
            }

            async fn reopen(&self, delivered: &HashSet<JobId>) -> JobTable {
                self.rig
                    .open(delivered)
                    .await
                    .expect("open the job table over the session ledger")
            }

            /// Counts every line on disk under the ledger directory.
            fn ledger_lines(&self) -> usize {
                fs::read_dir(&self.dir)
                    .expect("read the ledger directory")
                    .map(|entry| {
                        fs::read_to_string(entry.expect("ledger entry").path())
                            .expect("read the ledger file")
                            .lines()
                            .count()
                    })
                    .sum()
            }
        }

        fn report(id: JobId, label: &str, outcome: JobOutcome, text: &str) -> JobReport {
            JobReport {
                id,
                label: label.into(),
                outcome,
                text: text.into(),
            }
        }

        fn spawn_owned_row(jobs: &mut JobTable, outage: &Outage, label: &str) -> JobId {
            let id = JobId::new_v7();
            jobs.spawn_owned(
                JobRecord::new(
                    id,
                    label,
                    outage.dir.join(format!("{label}.log")),
                    CancellationToken::new(),
                )
                .owned_by(Name::test()),
            )
            .expect("reserve owner job");
            id
        }

        /// Settles one owner row and returns the report the table must keep.
        fn end_owned(jobs: &mut JobTable, outage: &Outage, label: &str, text: &str) -> JobReport {
            let id = spawn_owned_row(jobs, outage, label);
            let outcome = JobOutcome::Exited { code: 0 };
            jobs.settle_owned(&Name::test(), id, outcome.clone(), text)
                .expect("settle owner job");
            report(id, label, outcome, text)
        }

        fn assert_io_failure<T: std::fmt::Debug>(result: &Result<T, JobLedgerError>) {
            assert!(
                matches!(result, Err(JobLedgerError::Io { .. })),
                "the blocked jobs directory must fail the append with an I/O error: {result:?}"
            );
        }

        /// The delivered set a journaled and materialized wake over `jobs`
        /// leaves in the replayed journal.
        fn delivered_by_wake(jobs: &[JobId]) -> HashSet<JobId> {
            if jobs.is_empty() {
                return HashSet::new();
            }
            replay_delivered(vec![
                wake_turn_start(1),
                wake_user(1, None),
                wake_attempt(2, 1, jobs.to_vec()),
                wake_turn_end(1),
                wake_turn_start(2),
                wake_user(2, Some(wake_entry(1))),
            ])
        }

        async fn jobs_op(table: &Arc<Mutex<JobTable>>, outage: &Outage, op: JobsOp) -> JobsReply {
            run_jobs_op(
                JobsCtx {
                    table: Arc::clone(table),
                    owner: Name::test(),
                    jobs_dir: outage.dir.clone(),
                    cancel: CancellationToken::new(),
                },
                op,
            )
            .await
        }

        async fn spawn_job(table: &Arc<Mutex<JobTable>>, outage: &Outage) -> JobId {
            let reply = jobs_op(
                table,
                outage,
                JobsOp::Spawn {
                    name: Name::test(),
                    payload: RawJson::null(),
                    parent: None,
                },
            )
            .await;
            let JobsReply::Spawned { id } = reply else {
                panic!("spawn must reserve an owner row: {reply:?}");
            };
            id
        }

        #[tokio::test]
        async fn failed_flush_report_is_hidden_from_take_until_the_retry_is_durable() {
            let outage = Outage::new();
            let mut jobs = outage.open().await;
            let ended = end_owned(&mut jobs, &outage, "run", "report");
            outage.block();
            assert_io_failure(&jobs.flush().await);

            let offered = jobs.take(10);
            assert!(
                offered.is_empty(),
                "a report whose End line failed to reach disk was offered for delivery: {offered:?}"
            );

            outage.unblock();
            jobs.flush().await.expect("retry flush after the outage");
            assert_eq!(
                jobs.take(10),
                vec![ended],
                "the retried flush makes the same report, with its identity, takeable"
            );
        }

        #[tokio::test]
        async fn cancelled_report_is_hidden_from_take_until_a_flush_makes_it_durable() {
            let outage = Outage::new();
            let mut jobs = outage.open().await;
            let id = spawn_owned_row(&mut jobs, &outage, "cancelled");
            jobs.cancel(id).expect("cancel the owner row");
            assert!(
                !outage.dir.exists(),
                "no flush ran, so nothing may be on disk yet"
            );

            let offered = jobs.take(10);
            assert!(
                offered.is_empty(),
                "a cancel that never reached disk was offered for delivery: {offered:?}"
            );

            jobs.flush().await.expect("flush the cancelled report");
            assert_eq!(
                jobs.take(10),
                vec![report(id, "cancelled", JobOutcome::Cancelled, "")],
                "the flushed cancel is takeable with its identity"
            );
        }

        #[tokio::test]
        async fn a_durable_report_does_not_vouch_for_one_whose_flush_failed() {
            let outage = Outage::new();
            let mut jobs = outage.open().await;
            let durable = end_owned(&mut jobs, &outage, "durable", "kept");
            jobs.flush()
                .await
                .expect("the first report becomes durable");
            let failed = end_owned(&mut jobs, &outage, "failed", "pending");
            outage.block();
            assert_io_failure(&jobs.flush().await);

            let claimed: Vec<JobId> = jobs.take(10).iter().map(|offered| offered.id).collect();
            assert!(
                !claimed.contains(&failed.id),
                "the report whose flush failed was offered next to a durable one: {claimed:?}"
            );
            jobs.release(&claimed);

            outage.unblock();
            jobs.flush().await.expect("retry flush after the outage");
            assert_eq!(
                jobs.take(10),
                vec![durable, failed],
                "both reports keep their identity and settle order after the retry"
            );
        }

        #[tokio::test]
        async fn a_wake_never_consumes_a_report_the_ledger_cannot_reproduce() {
            let outage = Outage::new();
            let mut jobs = outage.open().await;
            let ended = end_owned(&mut jobs, &outage, "run", "report");
            outage.block();
            assert_io_failure(&jobs.flush().await);

            // The orchestrator claims whatever `take` offers and journals a
            // wake over it.
            let claimed: Vec<JobId> = jobs.take(10).iter().map(|offered| offered.id).collect();
            let delivered = delivered_by_wake(&claimed);

            // The process dies before the outage ends and before any retry.
            drop(jobs);
            outage.unblock();
            let reopened = outage.reopen(&delivered).await;
            for id in &delivered {
                assert!(
                    reopened.find(*id).is_some(),
                    "a wake consumed {ended:?} but its End line never reached disk: \
                     the completion is lost for good"
                );
            }
        }

        #[tokio::test]
        async fn unavailable_settle_reply_leaks_no_report_through_take() {
            let outage = Outage::new();
            let table = Arc::new(Mutex::new(outage.open().await));
            let id = spawn_job(&table, &outage).await;
            outage.block();

            let settled = jobs_op(
                &table,
                &outage,
                JobsOp::Settle {
                    id,
                    outcome: JobOutcome::Exited { code: 0 },
                    text: "report".into(),
                },
            )
            .await;
            assert!(
                matches!(settled, JobsReply::Unavailable { .. }),
                "the blocked ledger must fail the settle flush: {settled:?}"
            );

            let taken = jobs_op(&table, &outage, JobsOp::Take { limit: 10 }).await;
            assert_eq!(
                taken,
                JobsReply::Taken(Vec::new()),
                "an Unavailable settle left a report that Take offers as deliverable"
            );
        }

        #[tokio::test]
        async fn unavailable_cancel_reply_leaks_no_report_through_take() {
            let outage = Outage::new();
            let table = Arc::new(Mutex::new(outage.open().await));
            let id = spawn_job(&table, &outage).await;
            outage.block();

            let cancelled = jobs_op(&table, &outage, JobsOp::Cancel { id }).await;
            assert!(
                matches!(cancelled, JobsReply::Unavailable { .. }),
                "the blocked ledger must fail the cancel flush: {cancelled:?}"
            );

            let taken = jobs_op(&table, &outage, JobsOp::Take { limit: 10 }).await;
            assert_eq!(
                taken,
                JobsReply::Taken(Vec::new()),
                "an Unavailable cancel left a report that Take offers as deliverable"
            );
        }

        #[tokio::test]
        async fn retried_flush_writes_the_failed_report_exactly_once_with_its_identity() {
            let outage = Outage::new();
            let mut jobs = outage.open().await;
            let ended = end_owned(&mut jobs, &outage, "run", "report text");
            outage.block();
            assert_io_failure(&jobs.flush().await);
            assert_io_failure(&jobs.flush().await);

            outage.unblock();
            jobs.flush().await.expect("retry flush after the outage");
            jobs.flush()
                .await
                .expect("a flush with a drained outbox writes nothing");
            drop(jobs);

            assert_eq!(
                outage.ledger_lines(),
                1,
                "two failed flushes and two good ones wrote one End line"
            );
            let mut reopened = outage.open().await;
            assert_eq!(
                reopened.take(10),
                vec![ended],
                "the reopened ledger offers the same id, label, outcome and text"
            );
        }

        #[tokio::test]
        async fn failed_flushes_keep_every_buffered_report_in_settle_order() {
            let outage = Outage::new();
            let mut jobs = outage.open().await;
            let first = end_owned(&mut jobs, &outage, "first", "one");
            outage.block();
            assert_io_failure(&jobs.flush().await);
            let second = end_owned(&mut jobs, &outage, "second", "two");
            assert_io_failure(&jobs.flush().await);

            outage.unblock();
            jobs.flush().await.expect("retry flush after the outage");
            let third = end_owned(&mut jobs, &outage, "third", "three");
            jobs.flush()
                .await
                .expect("a later flush appends after the retried lines");
            drop(jobs);

            assert_eq!(outage.ledger_lines(), 3, "one End line per ended job");
            let mut reopened = outage.open().await;
            assert_eq!(
                reopened.take(10),
                vec![first, second, third],
                "every report keeps its identity, once, in settle order"
            );
        }

        #[tokio::test]
        async fn settle_durably_failure_keeps_the_outcome_and_the_retry_persists_it_once() {
            let outage = Outage::new();
            let mut jobs = outage.open().await;
            let id = JobId::new_v7();
            jobs.reserve(JobRecord::new(
                id,
                "export",
                outage.dir.join("export.log"),
                CancellationToken::new(),
            ))
            .expect("reserve command job");
            jobs.mark_running(id).expect("start command job");
            outage.block();

            let settled = jobs
                .settle_durably(
                    id,
                    JobOutcome::Exited { code: 0 },
                    Box::from(&b"report"[..]),
                )
                .await;
            assert_io_failure(&settled);
            assert_eq!(
                jobs.wait(id),
                Some(JobOutcome::Exited { code: 0 }),
                "the failed durable settle must keep the recorded outcome"
            );

            outage.unblock();
            jobs.flush().await.expect("retry flush after the outage");
            drop(jobs);

            assert_eq!(outage.ledger_lines(), 1, "the retry wrote one End line");
            let mut reopened = outage.open().await;
            assert_eq!(
                reopened.take(10),
                vec![report(
                    id,
                    "export",
                    JobOutcome::Exited { code: 0 },
                    "report"
                )],
                "the retried settle keeps the job identity across reopen"
            );
        }
    }
}
