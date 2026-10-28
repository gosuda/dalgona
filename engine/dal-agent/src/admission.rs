use std::{future::Future, sync::Arc, time::Duration};

use dal_core::{CallId, DenyReason};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::error::{AdmissionLimit, ToolError};
use crate::ext::script::WorkerPermit;
use crate::jobs::{JobTable, ProcessSlots, SlotGuard};
use crate::proc::{PROCESS_FD_COST, ProcessLease, ProgressPort, SnapshotCoordinator};

const DEFAULT_PROCESSES: usize = 256;
const DEFAULT_ADMISSION_WAIT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub(crate) processes: usize,
    pub(crate) admission_wait: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            processes: DEFAULT_PROCESSES,
            admission_wait: DEFAULT_ADMISSION_WAIT,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Admission {
    processes: Arc<Semaphore>,
    fds: Arc<Semaphore>,
    wait: Duration,
}

/// One admission operation's deadline: `limits.admission_wait` from the
/// moment the operation began, on the monotonic clock.
///
/// Every hop of the operation (the session table lookup, the session slot
/// queue, the host process permit, the fd charge) waits against this same
/// instant, so the whole operation is bounded by the one existing limit
/// instead of each hop starting its own clock.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Deadline(tokio::time::Instant);

impl Deadline {
    /// Resolves when the deadline passes.
    pub(crate) async fn elapsed(self) {
        tokio::time::sleep_until(self.0).await;
    }

    /// Whether the deadline has already passed.
    pub(crate) fn is_past(self) -> bool {
        tokio::time::Instant::now() >= self.0
    }

    /// Waits for `acquire` until `cancel` fires or the deadline passes.
    ///
    /// Cancellation wins ties, and no hop starts once the deadline has
    /// passed, even when its permit happens to be free. Dropping the losing
    /// future releases anything it held, so a cancelled or expired wait
    /// leaves nothing behind.
    ///
    /// # Errors
    /// Returns [`ToolError::Cancelled`] when `cancel` fires first and
    /// [`ToolError::Admission`] naming `limit` when the deadline passes first.
    pub(crate) async fn wait<T, F>(
        self,
        limit: AdmissionLimit,
        cancel: &CancellationToken,
        acquire: F,
    ) -> Result<T, ToolError>
    where
        F: Future<Output = Result<T, ToolError>>,
    {
        if cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        if self.is_past() {
            return Err(ToolError::Admission { limit });
        }
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(ToolError::Cancelled),
            result = tokio::time::timeout_at(self.0, acquire) => match result {
                Ok(result) => result,
                Err(_) => Err(ToolError::Admission { limit }),
            },
        }
    }
}

impl Admission {
    /// Starts one admission operation: its deadline is `limits.admission_wait`
    /// from now.
    pub(crate) fn begin(&self) -> Deadline {
        Deadline(tokio::time::Instant::now() + self.wait)
    }

    pub(crate) fn new(limits: Limits, fd_soft_limit: u64) -> Self {
        let fd_budget = fd_soft_limit / 5 * 4 + fd_soft_limit % 5 * 4 / 5;
        let fd_budget = usize::try_from(fd_budget)
            .unwrap_or(usize::MAX)
            .min(Semaphore::MAX_PERMITS);
        Self {
            processes: Arc::new(Semaphore::new(limits.processes.min(Semaphore::MAX_PERMITS))),
            fds: Arc::new(Semaphore::new(fd_budget)),
            wait: limits.admission_wait,
        }
    }

    /// Admits one child process: the session slot (queueing in arrival order
    /// behind the session cap), then the host process permit, then the fd
    /// charge, all against `deadline`.
    ///
    /// The two caps stay independent: a call queued behind the session cap
    /// holds no host capacity. A refusal at any hop drops everything taken at
    /// the earlier hops, so a timed-out or cancelled call holds nothing and
    /// no process starts.
    ///
    /// # Errors
    /// Returns [`ToolError::Cancelled`], [`ToolError::Admission`] when
    /// `deadline` passes at any hop, or [`ToolError::AdmissionQueueFull`]
    /// when the session's waiting queue is full.
    pub(crate) async fn admit_process(
        &self,
        table: &tokio::sync::Mutex<JobTable>,
        call: &CallId,
        port: &dyn ProgressPort,
        cancel: &CancellationToken,
        deadline: Deadline,
    ) -> Result<ProcessAdmission, ToolError> {
        let slot = ProcessSlots::reserve(table, call, port, cancel, deadline).await?;
        let process = self.acquire_process(cancel, deadline).await?;
        let fds = self.charge_fds(PROCESS_FD_COST, cancel, deadline).await?;
        Ok(ProcessAdmission { process, fds, slot })
    }

    pub(crate) async fn acquire_process(
        &self,
        cancel: &CancellationToken,
        deadline: Deadline,
    ) -> Result<OwnedSemaphorePermit, ToolError> {
        let semaphore = Arc::clone(&self.processes);
        deadline
            .wait(AdmissionLimit::Processes, cancel, async move {
                semaphore
                    .acquire_owned()
                    .await
                    .map_err(|_| ToolError::Cancelled)
            })
            .await
    }

    pub(crate) async fn charge_fds(
        &self,
        cost: usize,
        cancel: &CancellationToken,
        deadline: Deadline,
    ) -> Result<FdPermit, ToolError> {
        if cost == 0 {
            return Ok(FdPermit { _permit: None });
        }
        let Ok(permits) = u32::try_from(cost) else {
            return Err(ToolError::Admission {
                limit: AdmissionLimit::Fds,
            });
        };
        let semaphore = Arc::clone(&self.fds);
        let permit = deadline
            .wait(AdmissionLimit::Fds, cancel, async move {
                semaphore
                    .acquire_many_owned(permits)
                    .await
                    .map_err(|_| ToolError::Cancelled)
            })
            .await?;
        Ok(FdPermit {
            _permit: Some(permit),
        })
    }
}

/// An fd charge; the permit returns its budget on drop.
#[must_use = "an fd charge releases its budget on drop"]
#[derive(Debug)]
pub(crate) struct FdPermit {
    _permit: Option<OwnedSemaphorePermit>,
}

/// The capacity one child process holds from admission to reap: its host
/// process permit, its fd charge, and its session slot.
///
/// An approval carries it until the spawn door moves it into the child's
/// [`ProcessLease`]; dropping it at any point frees all three, so a refused
/// spawn, an approval dropped unused, and a reaped child release exactly once.
#[must_use = "dropping an admission releases its permits and session slot"]
#[derive(Debug)]
pub(crate) struct ProcessAdmission {
    /// The host-wide process permit.
    pub(crate) process: OwnedSemaphorePermit,
    /// The fd charge.
    pub(crate) fds: FdPermit,
    /// The session slot; released last, after the host capacity, so the
    /// next queued call finds the host permit free.
    pub(crate) slot: SlotGuard,
}

impl ProcessAdmission {
    /// Moves the admission into the lease the spawned child owns.
    pub(crate) fn lease(self, snapshots: Arc<SnapshotCoordinator>) -> ProcessLease {
        let Self { process, fds, slot } = self;
        ProcessLease::admit(process, Some(fds), slot, snapshots)
    }
}

/// The model-visible refusal for an admission that did not end in a process.
pub(crate) fn admission_denial(error: &ToolError) -> DenyReason {
    let what: Box<str> = match error {
        ToolError::Cancelled => "turn cancelled".into(),
        ToolError::Admission { limit } => format!(
            "no free {} slot within limits.admission_wait",
            limit.as_str()
        )
        .into(),
        ToolError::AdmissionQueueFull { .. } => "the session process queue is full".into(),
        _ => "process unavailable".into(),
    };
    DenyReason::Unavailable { what }
}

/// The host-wide interpreter worker cap, including the callback worker and
/// every dependency-reserved worker.
const MAX_WORKERS: usize = 8;

/// The dedicated hook-callback capacity inside [`MAX_WORKERS`].
const CALLBACK_WORKERS: usize = 1;

/// Host-wide admission for the synchronous interpreter workers.
///
/// The host runs at most [`MAX_WORKERS`] workers: one callback worker
/// reserved for observer, lifecycle, and non-nested guard hooks, root
/// workers admitted from the remaining unreserved capacity, and workers
/// currently reserved for accepted dependencies. A reservation is a
/// permit held outside the root pool, so unrelated roots can never borrow
/// it. Admission never waits: a queued dependency could deadlock against
/// ancestors holding every permit, so each refusal is an immediate
/// recoverable [`Busy`].
#[derive(Debug)]
pub(crate) struct Interpreters {
    callback: Arc<Semaphore>,
    live: Arc<Semaphore>,
}

impl Default for Interpreters {
    fn default() -> Self {
        Self::new()
    }
}

impl Interpreters {
    /// Builds the interpreter pool at the provisional limits.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            callback: Arc::new(Semaphore::new(CALLBACK_WORKERS)),
            live: Arc::new(Semaphore::new(MAX_WORKERS - CALLBACK_WORKERS)),
        }
    }

    /// Admits one root interpreter worker from unreserved capacity.
    ///
    /// Dependency reservations and the callback slot are never offered to
    /// roots.
    ///
    /// # Errors
    ///
    /// Returns [`Busy::Workers`] while every unreserved worker is in use;
    /// the caller refuses execution and may retry once capacity frees.
    pub(crate) fn root(&self) -> Result<WorkerPermit, Busy> {
        Arc::clone(&self.live)
            .try_acquire_owned()
            .map(WorkerPermit::new)
            .map_err(|_| Busy::Workers)
    }

    /// Admits the dedicated callback worker for one hook dispatch.
    ///
    /// The slot stays reserved while idle, so roots can never starve a
    /// hook.
    ///
    /// # Errors
    ///
    /// Returns [`Busy::Callback`] while another hook occupies the worker.
    pub(crate) fn callback(&self) -> Result<WorkerPermit, Busy> {
        Arc::clone(&self.callback)
            .try_acquire_owned()
            .map(WorkerPermit::new)
            .map_err(|_| Busy::Callback)
    }
}

/// An interpreter admission refusal; recoverable once capacity frees.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum Busy {
    /// Every unreserved interpreter worker is in use.
    #[error("all interpreter workers are busy")]
    Workers,
    /// Another hook occupies the callback worker.
    #[error("the interpreter callback worker is busy")]
    Callback,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The unreserved capacity left while `held` workers are in use.
    fn available_after(held: usize) -> usize {
        MAX_WORKERS - CALLBACK_WORKERS - held
    }

    #[tokio::test]
    async fn cancelled_process_wait_releases_no_slot() {
        let admission = Arc::new(Admission::new(
            Limits {
                processes: 1,
                admission_wait: Duration::from_secs(5),
            },
            100,
        ));
        let first = admission
            .acquire_process(&CancellationToken::new(), admission.begin())
            .await;
        assert!(first.is_ok());
        let token = CancellationToken::new();
        let waiter_admission = Arc::clone(&admission);
        let waiter_token = token.clone();
        let mut waiter = tokio::task::JoinSet::new();
        waiter.spawn(async move {
            waiter_admission
                .acquire_process(&waiter_token, waiter_admission.begin())
                .await
        });
        tokio::task::yield_now().await;
        token.cancel();
        let result = waiter.join_next().await;
        assert!(matches!(result, Some(Ok(Err(ToolError::Cancelled)))));
        drop(first);
        assert!(
            admission
                .acquire_process(&CancellationToken::new(), admission.begin())
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn fd_budget_waits_for_the_previous_process() {
        let admission = Arc::new(Admission::new(
            Limits {
                processes: 2,
                admission_wait: Duration::from_secs(1),
            },
            5,
        ));
        let first = admission
            .charge_fds(4, &CancellationToken::new(), admission.begin())
            .await
            .expect("first process receives the whole test budget");
        let waiter_admission = Arc::clone(&admission);
        let mut waiter = tokio::task::JoinSet::new();
        waiter.spawn(async move {
            waiter_admission
                .charge_fds(4, &CancellationToken::new(), waiter_admission.begin())
                .await
        });
        tokio::task::yield_now().await;
        assert_ne!(waiter.len(), 0);
        drop(first);
        let second = waiter
            .join_next()
            .await
            .expect("fd waiter settles")
            .expect("fd waiter task succeeds")
            .expect("fd budget is released");
        drop(second);
    }

    /// A progress port that records nothing.
    struct Quiet;

    impl ProgressPort for Quiet {
        fn publish_progress(&self, _call: &CallId, _tail: Box<str>) {}
    }

    fn paused_admission(processes: usize) -> Admission {
        Admission::new(
            Limits {
                processes,
                admission_wait: Duration::from_secs(30),
            },
            1000,
        )
    }

    #[tokio::test(start_paused = true)]
    async fn held_session_slots_time_out_without_taking_host_capacity() {
        let admission = paused_admission(4);
        let table = tokio::sync::Mutex::new(JobTable::new());
        let slots = table.lock().await.process_slots();
        let held: Vec<SlotGuard> = (0..crate::proc::SESSION_CHILD_LIMIT)
            .map(|_| slots.claim_for_test())
            .collect();
        let cancel = CancellationToken::new();
        let call = CallId::new("waiting");
        let deadline = admission.begin();
        let outcome = tokio::time::timeout(
            Duration::from_secs(31),
            admission.admit_process(&table, &call, &Quiet, &cancel, deadline),
        )
        .await;
        assert!(
            matches!(
                outcome,
                Ok(Err(ToolError::Admission {
                    limit: AdmissionLimit::SessionProcesses
                }))
            ),
            "the session wait ends at limits.admission_wait: {outcome:?}"
        );
        assert_eq!(slots.queued(), 0, "the timed-out call left the queue");
        assert_eq!(
            admission.processes.available_permits(),
            4,
            "a call queued behind the session cap held no host permit"
        );
        assert_eq!(slots.live(), crate::proc::SESSION_CHILD_LIMIT);
        drop(held);
        assert_eq!(slots.live(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn one_deadline_bounds_every_hop_and_a_timeout_releases_everything() {
        let admission = paused_admission(1);
        let table = tokio::sync::Mutex::new(JobTable::new());
        let slots = table.lock().await.process_slots();
        let cancel = CancellationToken::new();
        let call = CallId::new("waiting");
        let deadline = admission.begin();
        let held = admission
            .acquire_process(&cancel, admission.begin())
            .await
            .expect("the only host process permit");
        tokio::time::advance(Duration::from_secs(20)).await;
        // The session slot is free at once; the host hop has the 10 s that
        // remain of the one deadline, not a fresh 30 s.
        let outcome = tokio::time::timeout(
            Duration::from_secs(11),
            admission.admit_process(&table, &call, &Quiet, &cancel, deadline),
        )
        .await;
        assert!(
            matches!(
                outcome,
                Ok(Err(ToolError::Admission {
                    limit: AdmissionLimit::Processes
                }))
            ),
            "the host hop shares the deadline the earlier hops used: {outcome:?}"
        );
        assert_eq!(
            slots.live(),
            0,
            "the timed-out call gave its session slot back"
        );
        assert_eq!(slots.queued(), 0);
        drop(held);
        assert_eq!(admission.processes.available_permits(), 1);
        assert_eq!(admission.fds.available_permits(), 800);
    }

    #[tokio::test(start_paused = true)]
    async fn a_cancel_during_the_session_wait_is_prompt_and_holds_nothing() {
        let admission = paused_admission(1);
        let table = tokio::sync::Mutex::new(JobTable::new());
        let slots = table.lock().await.process_slots();
        let held: Vec<SlotGuard> = (0..crate::proc::SESSION_CHILD_LIMIT)
            .map(|_| slots.claim_for_test())
            .collect();
        let cancel = CancellationToken::new();
        let call = CallId::new("waiting");
        let deadline = admission.begin();
        let mut waiting =
            Box::pin(admission.admit_process(&table, &call, &Quiet, &cancel, deadline));
        assert!(futures::poll!(waiting.as_mut()).is_pending());
        assert_eq!(slots.queued(), 1);
        cancel.cancel();
        assert!(matches!(
            futures::poll!(waiting.as_mut()),
            std::task::Poll::Ready(Err(ToolError::Cancelled))
        ));
        assert_eq!(slots.queued(), 0);
        assert_eq!(admission.processes.available_permits(), 1);
        drop(held);
    }

    #[test]
    fn a_denial_names_the_reason() {
        let text = |error: ToolError| match admission_denial(&error) {
            DenyReason::Unavailable { what } => what.to_string(),
            other => format!("{other:?}"),
        };
        assert_eq!(text(ToolError::Cancelled), "turn cancelled");
        assert_eq!(
            text(ToolError::Admission {
                limit: AdmissionLimit::SessionProcesses
            }),
            "no free session processes slot within limits.admission_wait"
        );
        assert_eq!(
            text(ToolError::AdmissionQueueFull {
                limit: AdmissionLimit::SessionProcesses
            }),
            "the session process queue is full"
        );
        assert_eq!(
            text(ToolError::Failed(Box::new(std::io::Error::other("x")))),
            "process unavailable"
        );
    }

    #[test]
    fn roots_admit_only_unreserved_capacity() {
        let interpreters = Interpreters::new();
        let roots: Vec<_> = (0..MAX_WORKERS - CALLBACK_WORKERS)
            .map(|_| interpreters.root().expect("fresh pool has root capacity"))
            .collect();
        assert_eq!(roots.len(), MAX_WORKERS - CALLBACK_WORKERS);
        assert_eq!(interpreters.root().unwrap_err(), Busy::Workers);
        assert_eq!(
            interpreters.root().unwrap_err().to_string(),
            "all interpreter workers are busy"
        );
        assert_eq!(interpreters.live.available_permits(), available_after(7));
    }

    #[test]
    fn callback_capacity_is_one_and_dedicated() {
        let interpreters = Interpreters::new();
        let hook = interpreters.callback().expect("idle callback worker");
        let denied = interpreters.callback().unwrap_err();
        assert_eq!(denied, Busy::Callback);
        assert_eq!(
            denied.to_string(),
            "the interpreter callback worker is busy"
        );
        let roots: Vec<_> = (0..MAX_WORKERS - CALLBACK_WORKERS)
            .map(|_| {
                interpreters
                    .root()
                    .expect("callback slot never starves roots")
            })
            .collect();
        assert_eq!(roots.len() + CALLBACK_WORKERS, MAX_WORKERS);
        drop(hook);
        assert!(interpreters.callback().is_ok());
    }
}
