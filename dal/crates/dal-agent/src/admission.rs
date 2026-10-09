use std::{future::Future, sync::Arc, time::Duration};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::error::{AdmissionLimit, ToolError};
use crate::ext::script::WorkerPermit;

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

impl Admission {
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

    pub(crate) async fn acquire_process(
        &self,
        cancel: &CancellationToken,
    ) -> Result<OwnedSemaphorePermit, ToolError> {
        let semaphore = Arc::clone(&self.processes);
        self.wait_for(AdmissionLimit::Processes, cancel, async move {
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
        let permit = self
            .wait_for(AdmissionLimit::Fds, cancel, async move {
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

    async fn wait_for<T, F>(
        &self,
        limit: AdmissionLimit,
        cancel: &CancellationToken,
        acquire: F,
    ) -> Result<T, ToolError>
    where
        F: Future<Output = Result<T, ToolError>>,
    {
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(ToolError::Cancelled),
            result = tokio::time::timeout(self.wait, acquire) => match result {
                Ok(result) => result,
                Err(_) => Err(ToolError::Admission { limit }),
            },
        }
    }
}

/// An fd charge; the permit returns its budget on drop.
#[must_use = "an fd charge releases its budget on drop"]
#[derive(Debug)]
pub(crate) struct FdPermit {
    _permit: Option<OwnedSemaphorePermit>,
}

/// The host-wide interpreter worker cap, including the callback worker and
/// every dependency-reserved worker (R09).
const MAX_WORKERS: usize = 8;

/// The dedicated hook-callback capacity inside [`MAX_WORKERS`] (R09).
const CALLBACK_WORKERS: usize = 1;

/// Host-wide admission for the synchronous interpreter workers (R09).
///
/// The host runs at most [`MAX_WORKERS`] workers: one callback worker
/// reserved for observer, lifecycle, and non-nested guard hooks, root
/// workers admitted from the remaining unreserved capacity, and workers
/// currently reserved for accepted dependencies. A reservation is a
/// permit held outside the root pool, so unrelated roots can never borrow
/// it. Admission never waits: a queued dependency could deadlock against
/// ancestors holding every permit, so each refusal is an immediate
/// recoverable [`Busy`] (T-S05 T-S06).
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
    /// Builds the interpreter pool at the provisional R09 limits.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            callback: Arc::new(Semaphore::new(CALLBACK_WORKERS)),
            live: Arc::new(Semaphore::new(MAX_WORKERS - CALLBACK_WORKERS)),
        }
    }

    /// Admits one root interpreter worker from unreserved capacity (R09).
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

    /// Admits the dedicated callback worker for one hook dispatch (R09).
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

/// An interpreter admission refusal; recoverable once capacity frees
/// (R09).
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
        let first = admission.acquire_process(&CancellationToken::new()).await;
        assert!(first.is_ok());
        let token = CancellationToken::new();
        let waiter_admission = Arc::clone(&admission);
        let waiter_token = token.clone();
        let mut waiter = tokio::task::JoinSet::new();
        waiter.spawn(async move { waiter_admission.acquire_process(&waiter_token).await });
        tokio::task::yield_now().await;
        token.cancel();
        let result = waiter.join_next().await;
        assert!(matches!(result, Some(Ok(Err(ToolError::Cancelled)))));
        drop(first);
        assert!(
            admission
                .acquire_process(&CancellationToken::new())
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
            .charge_fds(4, &CancellationToken::new())
            .await
            .expect("first process receives the whole test budget");
        let waiter_admission = Arc::clone(&admission);
        let mut waiter = tokio::task::JoinSet::new();
        waiter.spawn(async move {
            waiter_admission
                .charge_fds(4, &CancellationToken::new())
                .await
        });
        tokio::task::yield_now().await;
        assert!(!waiter.is_empty());
        drop(first);
        let second = waiter
            .join_next()
            .await
            .expect("fd waiter settles")
            .expect("fd waiter task succeeds")
            .expect("fd budget is released");
        drop(second);
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
