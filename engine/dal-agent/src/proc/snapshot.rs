//! Coalesces concurrent pre-signal descendant snapshots into one global
//! `/proc` parent-graph scan per admission batch.
//!
//! One coordinator lives on the shared process-spawn owner
//! ([`crate::host::HostShared`]); there is no process-global static, no
//! completed-graph cache, and no optional fallback path. The coordinator
//! owns a [`SessionTasks`] tracker clone: every census worker is spawned
//! through it, so the host task owner tracks the workers and host
//! shutdown drains them; workers hold only a [`Weak`] back-reference, so
//! there is no `Arc` cycle and the coordinator lives exactly as long as
//! its owner.
//!
//! Batch semantics: admission happens under the state lock. A request
//! that observes the idle phase opens the next batch — its membership is
//! sealed at the transition to scanning, and the census worker is spawned
//! immediately after the lock is released. Requests that observe a scan
//! in flight are sealed into the next batch instead, which the completing
//! worker chains, and can never be served by the graph being walked. The
//! graph lives only in the worker's stack and is dropped with it.
//!
//! Boundedness: every waiter is a live admitted process holding its
//! process permit, so concurrent registrations are structurally bounded
//! by the existing process admission. The coordinator restates that bound
//! on the pending queue, prunes already-closed callers before checking
//! it, and sheds overflow to a fresh tracked direct walk instead of
//! growing the queue.
//!
//! Failure semantics: a panicking or failed census walk fails its batch
//! loudly ([`ToolError::Failed`]); it never reads as an empty descendant
//! set, and the phase still advances so the coordinator cannot strand.

#[cfg(target_os = "linux")]
use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::io;
use std::sync::Arc;
#[cfg(all(test, target_os = "linux"))]
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(target_os = "linux")]
use std::sync::{Mutex, Weak};

#[cfg(target_os = "linux")]
use tokio::sync::oneshot;

use crate::admission::FdPermit;
use crate::error::ToolError;
use crate::jobs::SlotGuard;
use crate::session::tasks::SessionTasks;

use super::Permit;

/// The global parent-to-children map one `/proc` census walk produces.
#[cfg(target_os = "linux")]
pub(crate) type ParentGraph = HashMap<u32, Vec<u32>>;

/// The injectable global census walk; production passes
/// [`super::stop::proc_parent_graph`], tests pass a deterministic fake.
/// A lost census is a typed error, never an empty graph.
#[cfg(target_os = "linux")]
pub(crate) type ScanFn = Arc<dyn Fn() -> Result<ParentGraph, ToolError> + Send + Sync>;

/// One registered pre-signal snapshot request. The sender travels inside
/// the batch worker; a dropped caller only closes the channel, which the
/// worker and every admission path prune without ceremony.
#[cfg(target_os = "linux")]
#[derive(Debug)]
struct Waiter {
    leader: u32,
    tx: oneshot::Sender<Result<Vec<u32>, ToolError>>,
}

/// The coordinator phase. The census worker itself lives on the host task
/// tracker; no join handle is stored in shared state. The out batch is
/// identified by `State.epoch` while the phase is scanning; the epoch only
/// advances on walk completion.
#[cfg(target_os = "linux")]
#[derive(Debug)]
enum Phase {
    Idle,
    Scanning,
}

/// Membership and sequencing state of the coordinator.
#[cfg(target_os = "linux")]
#[derive(Debug)]
struct State {
    /// Identity of the batch currently out or next to run, advanced only
    /// by walk completion.
    epoch: u64,
    phase: Phase,
    /// Requests sealed into the next batch. They were admitted while a
    /// scan was out, so they are never served by the graph being walked.
    pending: Vec<Waiter>,
    /// Set by [`SnapshotCoordinator::shutdown`]; new admissions refuse,
    /// and the worker in flight drains `pending` with failures instead of
    /// chaining.
    shutting_down: bool,
}

/// Bounded coalescing coordinator for pre-signal descendant snapshots.
///
/// Cloned into every admitted [`ProcessLease`] as an `Arc`; stop paths
/// await [`SnapshotCoordinator::snapshot`] exactly where the direct scan
/// ran, so each leader stays alive and unsignaled until its own batch
/// result returns.
pub(crate) struct SnapshotCoordinator {
    #[cfg(target_os = "linux")]
    state: Mutex<State>,
    /// The host process-admission capacity, restated as the pending-queue
    /// bound. Every pending waiter holds one process permit, so this is
    /// never the binding constraint; overflow sheds to a direct walk.
    #[cfg(target_os = "linux")]
    bound: usize,
    /// The host runtime; census workers spawn onto it through `tasks`.
    #[cfg(target_os = "linux")]
    runtime: tokio::runtime::Handle,
    /// The host task owner tracking every census worker; shutdown drains
    /// it.
    #[cfg(target_os = "linux")]
    tasks: SessionTasks,
    #[cfg(target_os = "linux")]
    scan: ScanFn,
    #[cfg(all(test, target_os = "linux"))]
    registrations: AtomicUsize,
}

impl SnapshotCoordinator {
    /// Builds the production coordinator: one global census walk per
    /// batch, workers tracked by `tasks`. The bound must be the same
    /// process-admission capacity [`crate::admission::Admission`] was
    /// sized with.
    pub(crate) fn host(bound: usize, tasks: SessionTasks) -> Arc<Self> {
        #[cfg(target_os = "linux")]
        {
            let scan: ScanFn = Arc::new(super::stop::proc_parent_graph);
            Self::with_scan(bound, scan, tasks)
        }
        #[cfg(not(target_os = "linux"))]
        {
            // No /proc descendant source: the coordinator is a valid owner
            // type whose snapshot reads as no descendants, matching the
            // legacy non-Linux behavior.
            let _ = (bound, tasks);
            Arc::new(Self {})
        }
    }

    /// Builds a coordinator over an explicit scan source.
    #[cfg(target_os = "linux")]
    pub(crate) fn with_scan(bound: usize, scan: ScanFn, tasks: SessionTasks) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                epoch: 0,
                phase: Phase::Idle,
                pending: Vec::new(),
                shutting_down: false,
            }),
            bound: bound.max(1),
            runtime: tokio::runtime::Handle::current(),
            tasks,
            scan,
            #[cfg(test)]
            registrations: AtomicUsize::new(0),
        })
    }

    /// Returns the live descendants of `leader` from the batch this
    /// request was admitted to.
    ///
    /// Admission is the lock acquisition: a request that observes the idle
    /// phase is sealed into the batch it opens; a request that observes a
    /// scan in flight waits for its own later fresh walk and is never
    /// served by the graph being walked. Dropping the returned future
    /// mid-batch closes only this caller's channel; the worker and every
    /// later admission prune it, and the child's cleanup authority stays
    /// with [`super::Proc`]. A census failure surfaces as
    /// [`ToolError::Failed`] instead of an empty descendant set.
    #[cfg_attr(
        not(target_os = "linux"),
        expect(
            clippy::unused_async,
            clippy::unused_async_trait_impl,
            reason = "without a /proc source the no-op arm holds the shared async shape"
        )
    )]
    #[cfg_attr(
        not(target_os = "linux"),
        expect(dead_code, reason = "the census runs only where /proc exists")
    )]
    pub(crate) async fn snapshot(self: Arc<Self>, leader: u32) -> Result<Vec<u32>, ToolError> {
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (&self, leader);
            Ok(Vec::new())
        }

        #[cfg(target_os = "linux")]
        {
            enum Admitted {
                Batch {
                    epoch: u64,
                    waiter: Waiter,
                },
                Queued,
                Shed {
                    tx: oneshot::Sender<Result<Vec<u32>, ToolError>>,
                },
            }
            let (rx, admitted) = {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if state.shutting_down {
                    return Err(ToolError::Cancelled);
                }
                let (tx, rx) = oneshot::channel();
                let admitted = match &mut state.phase {
                    Phase::Idle => {
                        // Sealed into the batch it opens; the worker spawns
                        // right after the lock is released.
                        let epoch = state.epoch;
                        state.phase = Phase::Scanning;
                        #[cfg(test)]
                        self.registrations.fetch_add(1, Ordering::SeqCst);
                        Admitted::Batch {
                            epoch,
                            waiter: Waiter { leader, tx },
                        }
                    }
                    Phase::Scanning => {
                        // Closed callers release their queue slots first, so
                        // abandoned waiters never consume capacity.
                        state.pending.retain(|waiter| !waiter.tx.is_closed());
                        #[cfg(test)]
                        self.registrations.fetch_add(1, Ordering::SeqCst);
                        if state.pending.len() < self.bound {
                            // Sealed into the next batch: never served by
                            // the graph being walked.
                            state.pending.push(Waiter { leader, tx });
                            Admitted::Queued
                        } else {
                            // Structurally unreachable: every waiter is a
                            // live admitted process holding a process permit.
                            // The shed keeps the queue hard-bounded with the
                            // same walk semantics, tracked on the same owner.
                            Admitted::Shed { tx }
                        }
                    }
                };
                (rx, admitted)
            };
            match admitted {
                Admitted::Batch { epoch, waiter } => self.spawn_batch(epoch, vec![waiter]),
                Admitted::Queued => {}
                Admitted::Shed { tx } => {
                    let scan = Arc::clone(&self.scan);
                    self.tasks.spawn_cleanup(&self.runtime, async move {
                        let walked = run_walk(&scan).await;
                        let _ =
                            tx.send(walked.map(|graph| super::stop::graph_reach(&graph, leader)));
                    });
                }
            }
            match rx.await {
                Ok(result) => result,
                // The sender only vanishes if the coordinator itself is
                // gone; that is a failure, never an empty descendant set.
                Err(_) => Err(failed(
                    "cancellation snapshot batch ended without delivering a result".to_string(),
                )),
            }
        }
    }

    /// Spawns one batch worker on the host task tracker. The worker holds
    /// a weak back-reference only, so it can never keep the coordinator
    /// (or its owner) alive.
    #[cfg(target_os = "linux")]
    fn spawn_batch(self: &Arc<Self>, epoch: u64, waiters: Vec<Waiter>) {
        let coordinator = Arc::downgrade(self);
        let scan = Arc::clone(&self.scan);
        self.tasks.spawn_cleanup(&self.runtime, async move {
            complete_batch(&coordinator, &scan, epoch, waiters).await;
        });
    }

    /// Cancels further admissions and drains the tracked workers. An
    /// in-flight batch still runs to its own completion and fails its
    /// queued callers loudly instead of leaving them waiting.
    #[cfg(target_os = "linux")]
    pub(crate) async fn shutdown(&self) {
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.shutting_down = true;
        }
        self.tasks.stop().await;
    }

    /// No census workers exist without a `/proc` descendant source.
    #[cfg(not(target_os = "linux"))]
    #[expect(
        clippy::unused_async,
        reason = "the empty arm keeps one shutdown call shape across platforms"
    )]
    pub(crate) async fn shutdown(&self) {}

    /// The epoch of the walk currently out, if any.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn scanning_epoch(&self) -> Option<u64> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &state.phase {
            Phase::Scanning => Some(state.epoch),
            Phase::Idle => None,
        }
    }

    /// Total registrations admitted so far (batch members, queued, shed).
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn registered(&self) -> usize {
        self.registrations.load(Ordering::SeqCst)
    }

    /// Requests waiting for the next fresh batch.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn pending_len(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
            .len()
    }
}

/// Runs one batch to completion: exactly one global census walk, then
/// per-waiter extraction, the epoch advance, and the chained next batch.
///
/// The graph lives only in this worker's stack and is dropped with it.
/// Every terminal path sends each caller a result or a failure, so no
/// caller reads a lost walk as an empty descendant set.
#[cfg(target_os = "linux")]
async fn complete_batch(
    coordinator: &Weak<SnapshotCoordinator>,
    scan: &ScanFn,
    epoch: u64,
    waiters: Vec<Waiter>,
) {
    let walked = run_walk(scan).await;
    let Some(coordinator) = coordinator.upgrade() else {
        // The owner vanished mid-walk: fail this batch loudly and leave.
        for waiter in waiters {
            let _ = waiter.tx.send(Err(failed(
                "cancellation snapshot coordinator is gone".to_string(),
            )));
        }
        return;
    };
    match walked {
        Ok(graph) => {
            for waiter in waiters {
                let _ = waiter
                    .tx
                    .send(Ok(super::stop::graph_reach(&graph, waiter.leader)));
            }
        }
        Err(error) => {
            let text = error.to_string();
            for waiter in waiters {
                let _ = waiter
                    .tx
                    .send(Err(failed(format!("cancellation census failed: {text}"))));
            }
        }
    }
    let mut state = coordinator
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.epoch = epoch + 1;
    if state.shutting_down {
        for waiter in state.pending.drain(..) {
            let _ = waiter.tx.send(Err(ToolError::Cancelled));
        }
        state.phase = Phase::Idle;
        return;
    }
    // Closed callers never trigger a walk of their own.
    state.pending.retain(|waiter| !waiter.tx.is_closed());
    if state.pending.is_empty() {
        state.phase = Phase::Idle;
        return;
    }
    // Callers sealed into `pending` during the finished walk get their own
    // fresh walk; they were never registered against the graph above. The
    // lock stays held across the transition, so no concurrent admission
    // can observe an idle gap.
    let pending = std::mem::take(&mut state.pending);
    let next_epoch = state.epoch;
    state.phase = Phase::Scanning;
    drop(state);
    coordinator.spawn_batch(next_epoch, pending);
}

/// One tracked census walk: the global scan on the blocking pool, panics
/// mapped to a failure instead of an empty graph. A typed census error
/// propagates unchanged.
#[cfg(target_os = "linux")]
async fn run_walk(scan: &ScanFn) -> Result<ParentGraph, ToolError> {
    let scan = Arc::clone(scan);
    match tokio::task::spawn_blocking(move || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || scan()))
    })
    .await
    {
        Ok(Ok(Ok(graph))) => Ok(graph),
        Ok(Ok(Err(error))) => Err(error),
        Ok(Err(panic)) => Err(failed(format!(
            "cancellation census walk panicked: {panic:?}"
        ))),
        Err(join) => Err(failed(format!(
            "cancellation census walk task failed: {join}"
        ))),
    }
}

#[cfg(target_os = "linux")]
fn failed(message: String) -> ToolError {
    ToolError::Failed(Box::new(io::Error::other(message)))
}

/// The admitted process capacity one spawn consumes: the process and fd
/// permits, the session slot, plus the snapshot coordinator, minted together
/// so a spawned [`super::Proc`] can never exist without its census owner. The
/// fields are private; every construction path goes through
/// [`ProcessLease::admit`].
pub(crate) struct ProcessLease {
    /// Released by value when the lease drops, refusals included.
    _process_permit: Permit,
    /// Released by value when the lease drops, refusals included.
    _fd_permit: Option<FdPermit>,
    /// The child's session process slot, released by value after the host
    /// permits when the lease drops. Every real admission carries one; only
    /// the standalone test lease has none.
    _session_slot: Option<SlotGuard>,
    /// The type owner stays valid on every platform; without a `/proc`
    /// descendant source the coordinator it carries simply reads as no
    /// descendants.
    #[cfg_attr(
        not(target_os = "linux"),
        expect(
            dead_code,
            reason = "the coordinator carries no descendants without /proc"
        )
    )]
    snapshots: Arc<SnapshotCoordinator>,
}

impl ProcessLease {
    /// Bundles the admission permits and the session slot with the owner's
    /// coordinator.
    pub(crate) fn admit(
        process_permit: Permit,
        fd_permit: Option<FdPermit>,
        session_slot: SlotGuard,
        snapshots: Arc<SnapshotCoordinator>,
    ) -> Self {
        Self {
            _process_permit: process_permit,
            _fd_permit: fd_permit,
            _session_slot: Some(session_slot),
            snapshots,
        }
    }

    /// A fresh owner for standalone process tests: same code path as the
    /// host, its own coordinator and tracker.
    #[cfg(test)]
    pub(crate) fn for_test(process_permit: Permit, fd_permit: Option<FdPermit>) -> Self {
        let snapshots = SnapshotCoordinator::host(
            crate::admission::Limits::default().processes,
            SessionTasks::new(),
        );
        Self {
            _process_permit: process_permit,
            _fd_permit: fd_permit,
            _session_slot: None,
            snapshots,
        }
    }

    /// The coordinator this lease was admitted with.
    #[cfg(target_os = "linux")]
    pub(crate) fn snapshots(&self) -> &Arc<SnapshotCoordinator> {
        &self.snapshots
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::{ParentGraph, ScanFn, SessionTasks, SnapshotCoordinator};
    use crate::error::ToolError;
    use std::collections::VecDeque;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    };
    use tokio::sync::Notify;
    use tokio::task::JoinSet;

    /// Lost-wakeup-free gate: `open` sets the flag then notifies; waiters
    /// recheck the flag in a loop, so a missed notify still makes progress.
    #[derive(Default)]
    struct Gate {
        open: AtomicBool,
        notify: Notify,
    }

    impl Gate {
        fn open(&self) {
            self.open.store(true, Ordering::SeqCst);
            self.notify.notify_waiters();
        }
    }

    /// Opens the fake gate when the test ends, so a failed assert or an
    /// early return cannot leave a census worker spinning on a closed gate.
    struct OpenOnDrop(Arc<FakeScan>);

    impl Drop for OpenOnDrop {
        fn drop(&mut self) {
            self.0.gate.open();
        }
    }

    /// Deterministic fake census: yields one prepared graph per walk in
    /// walk order, counting every walk. Walks block on the gate until it
    /// opens; a planned panic makes the matching walk fail after the gate
    /// releases it.
    struct FakeScan {
        graphs: Mutex<VecDeque<ParentGraph>>,
        walks: AtomicU64,
        gate: Gate,
        panic_walks: Mutex<VecDeque<bool>>,
    }

    impl FakeScan {
        fn new(graphs: Vec<ParentGraph>) -> Arc<Self> {
            let count = graphs.len();
            Arc::new(Self {
                graphs: Mutex::new(graphs.into()),
                walks: AtomicU64::new(0),
                gate: Gate::default(),
                panic_walks: Mutex::new(vec![false; count].into()),
            })
        }

        fn with_panic_plan(graphs: Vec<ParentGraph>, panic_walks: Vec<bool>) -> Arc<Self> {
            Arc::new(Self {
                graphs: Mutex::new(graphs.into()),
                walks: AtomicU64::new(0),
                gate: Gate::default(),
                panic_walks: Mutex::new(panic_walks.into()),
            })
        }

        fn walks(&self) -> u64 {
            self.walks.load(Ordering::SeqCst)
        }

        fn scan_fn(self: &Arc<Self>) -> ScanFn {
            let scan = Arc::clone(self);
            Arc::new(move || {
                let graph = scan
                    .graphs
                    .lock()
                    .map_err(|error| super::failed(format!("fake census lock poisoned: {error}")))?
                    .pop_front()
                    .unwrap_or_default();
                let panicked = scan
                    .panic_walks
                    .lock()
                    .map_err(|error| {
                        super::failed(format!("fake census panic-plan lock poisoned: {error}"))
                    })?
                    .pop_front()
                    .unwrap_or(false);
                scan.walks.fetch_add(1, Ordering::SeqCst);
                while !scan.gate.open.load(Ordering::SeqCst) {
                    std::thread::yield_now();
                }
                assert!(!panicked, "planned census walk failure");
                Ok(graph)
            })
        }
    }

    fn graph(edges: &[(u32, &[u32])]) -> ParentGraph {
        edges
            .iter()
            .map(|(parent, children)| (*parent, children.to_vec()))
            .collect()
    }

    /// Spins the scheduler until `pred` holds; bounded so a logic bug fails
    /// the test instead of hanging. No wall clock is involved.
    async fn until(pred: impl Fn() -> bool) -> Result<(), ToolError> {
        for _ in 0..10_000 {
            if pred() {
                return Ok(());
            }
            tokio::task::yield_now().await;
        }
        Err(super::failed("condition never became true".to_string()))
    }

    /// Joins one owned test task, mapping a missing slot or a join failure
    /// to a loud test error. Snapshot failures stay inside the returned
    /// value so negative tests can still assert on them.
    async fn join_one<T>(tasks: &mut JoinSet<T>) -> Result<T, ToolError>
    where
        T: Send + 'static,
    {
        tasks
            .join_next()
            .await
            .ok_or_else(|| super::failed("owned test task missing".to_string()))?
            .map_err(|join| super::failed(format!("owned test task join failed: {join}")))
    }

    #[tokio::test]
    async fn late_admissions_share_one_chained_scan() -> Result<(), ToolError> {
        // Walk 1 serves A alone; B and C, sealed into the next batch while
        // walk 1 is out, both share walk 2.
        let fake = FakeScan::new(vec![graph(&[(1, &[11])]), graph(&[(2, &[21]), (3, &[31])])]);
        let coord = SnapshotCoordinator::with_scan(256, fake.scan_fn(), SessionTasks::new());
        let _open_on_drop = OpenOnDrop(Arc::clone(&fake));
        let mut a_tasks = JoinSet::new();
        a_tasks.spawn({
            let coord = Arc::clone(&coord);
            async move { coord.snapshot(1).await }
        });
        until(|| fake.walks() == 1).await?;
        let mut b_tasks = JoinSet::new();
        b_tasks.spawn({
            let coord = Arc::clone(&coord);
            async move { coord.snapshot(2).await }
        });
        let mut c_tasks = JoinSet::new();
        c_tasks.spawn({
            let coord = Arc::clone(&coord);
            async move { coord.snapshot(3).await }
        });
        until(|| coord.pending_len() == 2).await?;
        // The gate is still closed, so no second walk can have started:
        // the walk count here is race-free.
        assert_eq!(fake.walks(), 1);
        fake.gate.open();
        assert_eq!(
            join_one(&mut a_tasks).await??,
            vec![11],
            "A served by its own walk"
        );
        until(|| fake.walks() == 2).await?;
        fake.gate.open();
        assert_eq!(
            join_one(&mut b_tasks).await??,
            vec![21],
            "B served by the shared chained walk"
        );
        assert_eq!(
            join_one(&mut c_tasks).await??,
            vec![31],
            "C served by the shared chained walk"
        );
        assert_eq!(fake.walks(), 2, "two late admissions cost one extra walk");
        until(|| coord.scanning_epoch().is_none()).await?;
        assert!(
            coord.scanning_epoch().is_none(),
            "phase idle after the batch"
        );
        Ok(())
    }

    #[tokio::test]
    async fn late_arrival_never_sees_the_older_snapshot() -> Result<(), ToolError> {
        // Walk 1: leader 1 has child 11. Walk 2: child 22 instead. The
        // request sealed during walk 1 must observe only walk 2.
        let fake = FakeScan::new(vec![graph(&[(1, &[11])]), graph(&[(1, &[22])])]);
        let coord = SnapshotCoordinator::with_scan(256, fake.scan_fn(), SessionTasks::new());
        let _open_on_drop = OpenOnDrop(Arc::clone(&fake));
        let mut a_tasks = JoinSet::new();
        a_tasks.spawn({
            let coord = Arc::clone(&coord);
            async move { coord.snapshot(1).await }
        });
        until(|| fake.walks() == 1).await?;
        let mut b_tasks = JoinSet::new();
        b_tasks.spawn({
            let coord = Arc::clone(&coord);
            async move { coord.snapshot(1).await }
        });
        until(|| coord.pending_len() == 1).await?;
        fake.gate.open();
        assert_eq!(join_one(&mut a_tasks).await??, vec![11]);
        until(|| fake.walks() == 2).await?;
        assert_eq!(coord.scanning_epoch(), Some(1));
        fake.gate.open();
        assert_eq!(join_one(&mut b_tasks).await??, vec![22]);
        assert_eq!(fake.walks(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn batch_results_stay_per_leader() -> Result<(), ToolError> {
        let fake = FakeScan::new(vec![
            graph(&[(0, &[10])]),
            graph(&[(1, &[11, 12]), (2, &[21]), (11, &[111])]),
        ]);
        let coord = SnapshotCoordinator::with_scan(256, fake.scan_fn(), SessionTasks::new());
        let _open_on_drop = OpenOnDrop(Arc::clone(&fake));
        let mut a_tasks = JoinSet::new();
        a_tasks.spawn({
            let coord = Arc::clone(&coord);
            async move { coord.snapshot(0).await }
        });
        until(|| fake.walks() == 1).await?;
        let mut b_tasks = JoinSet::new();
        b_tasks.spawn({
            let coord = Arc::clone(&coord);
            async move { coord.snapshot(1).await }
        });
        let mut c_tasks = JoinSet::new();
        c_tasks.spawn({
            let coord = Arc::clone(&coord);
            async move { coord.snapshot(2).await }
        });
        until(|| coord.pending_len() == 2).await?;
        fake.gate.open();
        assert_eq!(join_one(&mut a_tasks).await??, vec![10]);
        until(|| fake.walks() == 2).await?;
        fake.gate.open();
        let b = join_one(&mut b_tasks).await??;
        let c = join_one(&mut c_tasks).await??;
        // `graph_reach` returns the reachable set without ordering, exactly
        // like the legacy `proc_descendants` HashSet collect.
        assert_eq!(
            {
                let mut reached = b;
                reached.sort_unstable();
                reached
            },
            vec![11, 12, 111],
            "leader 1's reachable subtree only"
        );
        assert_eq!(c, vec![21], "leader 2's reachable subtree only");
        Ok(())
    }

    #[tokio::test]
    async fn dropped_caller_neither_strands_nor_suppresses() -> Result<(), ToolError> {
        let fake = FakeScan::new(vec![
            graph(&[(0, &[10])]),
            graph(&[(9, &[91])]),
            graph(&[(1, &[11])]),
        ]);
        let coord = SnapshotCoordinator::with_scan(256, fake.scan_fn(), SessionTasks::new());
        let _open_on_drop = OpenOnDrop(Arc::clone(&fake));
        let mut a_tasks = JoinSet::new();
        a_tasks.spawn({
            let coord = Arc::clone(&coord);
            async move { coord.snapshot(0).await }
        });
        until(|| fake.walks() == 1).await?;
        let mut b_tasks = JoinSet::new();
        let b_handle = b_tasks.spawn({
            let coord = Arc::clone(&coord);
            async move { coord.snapshot(1).await }
        });
        until(|| coord.registered() == 2).await?;
        // B's caller disappears mid-batch; the worker ignores the failed
        // send and the coordinator stays usable.
        b_handle.abort();
        let _ = b_tasks.join_next().await;
        fake.gate.open();
        assert_eq!(join_one(&mut a_tasks).await??, vec![10]);
        // The chained batch for the already-dead B still runs to Idle: the
        // coordinator owns it, and B's absence strands nothing.
        until(|| coord.scanning_epoch().is_none()).await?;
        assert!(coord.scanning_epoch().is_none());
        let mut d_tasks = JoinSet::new();
        d_tasks.spawn({
            let coord = Arc::clone(&coord);
            async move { coord.snapshot(9).await }
        });
        until(|| fake.walks() == 2).await?;
        fake.gate.open();
        assert_eq!(join_one(&mut d_tasks).await??, vec![91]);
        Ok(())
    }

    #[tokio::test]
    async fn dropped_batch_opener_still_serves_coalesced_callers() -> Result<(), ToolError> {
        let fake = FakeScan::new(vec![graph(&[(0, &[10])]), graph(&[(1, &[11]), (2, &[21])])]);
        let coord = SnapshotCoordinator::with_scan(256, fake.scan_fn(), SessionTasks::new());
        let _open_on_drop = OpenOnDrop(Arc::clone(&fake));
        let mut a_tasks = JoinSet::new();
        let a_handle = a_tasks.spawn({
            let coord = Arc::clone(&coord);
            async move { coord.snapshot(0).await }
        });
        until(|| fake.walks() == 1).await?;
        let mut b_tasks = JoinSet::new();
        b_tasks.spawn({
            let coord = Arc::clone(&coord);
            async move { coord.snapshot(1).await }
        });
        let mut c_tasks = JoinSet::new();
        c_tasks.spawn({
            let coord = Arc::clone(&coord);
            async move { coord.snapshot(2).await }
        });
        until(|| coord.pending_len() == 2).await?;
        // The batch opener's future dies mid-scan; the coordinator-owned
        // worker completes, so the chained batch still serves B and C.
        a_handle.abort();
        let _ = a_tasks.join_next().await;
        fake.gate.open();
        until(|| fake.walks() == 2).await?;
        fake.gate.open();
        assert_eq!(join_one(&mut b_tasks).await??, vec![11]);
        assert_eq!(join_one(&mut c_tasks).await??, vec![21]);
        until(|| coord.scanning_epoch().is_none()).await?;
        assert!(coord.scanning_epoch().is_none());
        Ok(())
    }

    #[tokio::test]
    async fn census_failure_yields_empty_like_the_legacy_path() -> Result<(), ToolError> {
        let fake = FakeScan::new(vec![ParentGraph::new()]);
        let coord = SnapshotCoordinator::with_scan(256, fake.scan_fn(), SessionTasks::new());
        let _open_on_drop = OpenOnDrop(Arc::clone(&fake));
        let mut a_tasks = JoinSet::new();
        a_tasks.spawn({
            let coord = Arc::clone(&coord);
            async move { coord.snapshot(1).await }
        });
        until(|| fake.walks() == 1).await?;
        fake.gate.open();
        assert_eq!(join_one(&mut a_tasks).await??, Vec::<u32>::new());
        Ok(())
    }

    #[tokio::test]
    async fn walking_failure_fails_loudly_instead_of_reading_empty() -> Result<(), ToolError> {
        // Walk 1 panics after the gate releases it; A and the queued B must
        // both observe a failure, never an empty descendant set, and the
        // phase must still advance.
        let fake = FakeScan::with_panic_plan(
            vec![ParentGraph::new(), ParentGraph::new()],
            vec![true, true],
        );
        let coord = SnapshotCoordinator::with_scan(256, fake.scan_fn(), SessionTasks::new());
        let _open_on_drop = OpenOnDrop(Arc::clone(&fake));
        let mut a_tasks = JoinSet::new();
        a_tasks.spawn({
            let coord = Arc::clone(&coord);
            async move { coord.snapshot(1).await }
        });
        until(|| fake.walks() == 1).await?;
        let mut b_tasks = JoinSet::new();
        b_tasks.spawn({
            let coord = Arc::clone(&coord);
            async move { coord.snapshot(1).await }
        });
        until(|| coord.pending_len() == 1).await?;
        fake.gate.open();
        assert!(
            join_one(&mut a_tasks).await?.is_err(),
            "panicked walk must fail, not read empty"
        );
        // B is a strict later arrival: its own fresh walk runs and fails
        // too, never the older graph, and never an empty set.
        assert!(
            join_one(&mut b_tasks).await?.is_err(),
            "queued caller of a panicked walk must fail"
        );
        until(|| coord.scanning_epoch().is_none()).await?;
        assert!(
            coord.scanning_epoch().is_none(),
            "phase advances past the failure"
        );
        assert_eq!(fake.walks(), 2, "each batch paid exactly one failing walk");
        Ok(())
    }

    #[tokio::test]
    async fn closed_waiters_are_pruned_before_the_bound() -> Result<(), ToolError> {
        // bound=1: after B is abandoned, C must reuse its queue slot instead
        // of shedding, so the abandoned waiter never consumes capacity.
        let fake = FakeScan::new(vec![
            graph(&[(0, &[10])]),
            graph(&[(2, &[21])]),
            graph(&[(3, &[31])]),
        ]);
        let coord = SnapshotCoordinator::with_scan(1, fake.scan_fn(), SessionTasks::new());
        let _open_on_drop = OpenOnDrop(Arc::clone(&fake));
        let mut a_tasks = JoinSet::new();
        a_tasks.spawn({
            let coord = Arc::clone(&coord);
            async move { coord.snapshot(0).await }
        });
        until(|| fake.walks() == 1).await?;
        let mut b_tasks = JoinSet::new();
        let b_handle = b_tasks.spawn({
            let coord = Arc::clone(&coord);
            async move { coord.snapshot(1).await }
        });
        until(|| coord.pending_len() == 1).await?;
        b_handle.abort();
        let _ = b_tasks.join_next().await;
        let mut c_tasks = JoinSet::new();
        c_tasks.spawn({
            let coord = Arc::clone(&coord);
            async move { coord.snapshot(2).await }
        });
        until(|| coord.pending_len() == 1).await?;
        fake.gate.open();
        assert_eq!(join_one(&mut a_tasks).await??, vec![10]);
        // The chained batch carries only the live C: exactly two walks, no
        // shed, and C is served by the fresh second graph.
        until(|| fake.walks() == 2).await?;
        assert_eq!(join_one(&mut c_tasks).await??, vec![21]);
        assert_eq!(fake.walks(), 2, "pruning reclaimed the abandoned slot");
        until(|| coord.scanning_epoch().is_none()).await?;
        assert!(coord.scanning_epoch().is_none());
        Ok(())
    }

    #[tokio::test]
    async fn waiter_queue_is_hard_bounded_by_admission_capacity() -> Result<(), ToolError> {
        // bound=1 with live waiters: the first late admission fills
        // `pending`; the second must shed to a fresh tracked direct walk
        // instead of queueing.
        let fake = FakeScan::new(vec![
            graph(&[(0, &[10])]),
            graph(&[(3, &[31])]),
            graph(&[(2, &[21])]),
            graph(&[(1, &[11])]),
        ]);
        let coord = SnapshotCoordinator::with_scan(1, fake.scan_fn(), SessionTasks::new());
        let _open_on_drop = OpenOnDrop(Arc::clone(&fake));
        let mut a_tasks = JoinSet::new();
        a_tasks.spawn({
            let coord = Arc::clone(&coord);
            async move { coord.snapshot(0).await }
        });
        until(|| fake.walks() == 1).await?;
        let mut b_tasks = JoinSet::new();
        b_tasks.spawn({
            let coord = Arc::clone(&coord);
            async move { coord.snapshot(1).await }
        });
        until(|| coord.pending_len() == 1).await?;
        let mut c_tasks = JoinSet::new();
        c_tasks.spawn({
            let coord = Arc::clone(&coord);
            async move { coord.snapshot(2).await }
        });
        let mut d_tasks = JoinSet::new();
        d_tasks.spawn({
            let coord = Arc::clone(&coord);
            async move { coord.snapshot(3).await }
        });
        // C or D shed to the direct path: its walk runs while walk 1 is
        // still held.
        until(|| fake.walks() == 2).await?;
        fake.gate.open();
        assert_eq!(join_one(&mut a_tasks).await??, vec![10]);
        // Walk 1 chains the batch for the queued waiter; the shed walk runs
        // beside it: exactly four walks total.
        until(|| fake.walks() == 4).await?;
        fake.gate.open();
        // Per-waiter graph content beyond A is walk-order dependent because
        // the shed walk and the chained walk both consume the fake queue;
        // the deterministic claims are the walk count, completion, and the
        // idle phase.
        assert!(join_one(&mut b_tasks).await?.is_ok());
        assert!(join_one(&mut c_tasks).await?.is_ok());
        assert!(join_one(&mut d_tasks).await?.is_ok());
        assert_eq!(fake.walks(), 4);
        until(|| coord.scanning_epoch().is_none()).await?;
        assert!(coord.scanning_epoch().is_none());
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_drains_the_tracked_workers_and_refuses_new_admissions()
    -> Result<(), ToolError> {
        let fake = FakeScan::new(vec![graph(&[(0, &[10])]), graph(&[(1, &[11])])]);
        let coord = SnapshotCoordinator::with_scan(256, fake.scan_fn(), SessionTasks::new());
        let _open_on_drop = OpenOnDrop(Arc::clone(&fake));
        let mut a_tasks = JoinSet::new();
        a_tasks.spawn({
            let coord = Arc::clone(&coord);
            async move { coord.snapshot(0).await }
        });
        until(|| fake.walks() == 1).await?;
        let mut b_tasks = JoinSet::new();
        b_tasks.spawn({
            let coord = Arc::clone(&coord);
            async move { coord.snapshot(1).await }
        });
        until(|| coord.pending_len() == 1).await?;
        // Shutdown flags refusals before it drains: the gate is still held,
        // so the in-flight walk cannot finish while the flag is observed.
        let mut shutdown_tasks = JoinSet::new();
        shutdown_tasks.spawn({
            let coord = Arc::clone(&coord);
            async move { coord.shutdown().await }
        });
        let mut probe_tasks = JoinSet::new();
        let probe_handle = probe_tasks.spawn({
            let coord = Arc::clone(&coord);
            async move { coord.snapshot(9).await }
        });
        until(|| probe_handle.is_finished()).await?;
        assert!(
            join_one(&mut probe_tasks).await?.is_err(),
            "admission during shutdown must refuse"
        );
        fake.gate.open();
        join_one(&mut shutdown_tasks).await?;
        // The in-flight opener still receives its real subtree.
        assert_eq!(join_one(&mut a_tasks).await??, vec![10]);
        // The queued caller is failed loudly, never left waiting.
        assert!(join_one(&mut b_tasks).await?.is_err());
        // New admissions refuse instead of queueing on a closed owner.
        let Err(err) = coord.clone().snapshot(2).await else {
            return Err(super::failed(
                "admission after shutdown must refuse".to_string(),
            ));
        };
        assert!(matches!(err, crate::error::ToolError::Cancelled));
        Ok(())
    }
}
