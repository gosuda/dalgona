//! Four process-wide journal shards. Each shard is one standard thread with a
//! bounded FIFO; its worker owns the physical journals assigned to it.
//!
//! A session is pinned by its UUID to one shard. The actor-facing [`Lane`]
//! carries only a slot token. A queued append moves one complete batch to that
//! shard in two durability phases: the worker writes each blob temp and hands
//! the ordered publish steps to the per-shard syncer, and only once every
//! blob the batch references is durable does a FIFO control job bring the
//! batch back so the worker can write it and stage the journal sync. Journal
//! bytes therefore never precede the blobs they name, while a slow filesystem
//! still stalls only the syncer, never the worker's FIFO, so one convoyed
//! `fsync` cannot starve unrelated lanes (R-perf). One lane admits at most
//! one unacknowledged batch, so cancellation cannot lose a receipt or reorder
//! a session's appends.

use std::{
    cell::Cell,
    collections::VecDeque,
    fmt,
    future::Future,
    marker::PhantomData,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Condvar, Mutex, MutexGuard},
    task::{Context, Poll, Waker},
    thread::{self, JoinHandle},
};

use dal_core::SessionId;
use tokio::sync::oneshot;

use crate::{
    blob::{self, PendingBlob},
    error::{JournalError, StoreError},
    journal::{Journal, Receipt},
};

/// The number of process-owned journal shard threads.
pub(crate) const SHARD_COUNT: usize = 4;
const _: () = assert!(SHARD_COUNT == 4);
/// The maximum queued requests per shard, excluding the request being run.
pub(crate) const QUEUE_CAPACITY: usize = 256;
/// Queue admission bound: a saturated shard fails the append loudly instead
/// of parking a turn invisibly.
const ENQUEUE_WAIT: std::time::Duration = std::time::Duration::from_secs(60);
/// How long an admitted append may wait for its receipt before it reports.
const SETTLE_REPORT: std::time::Duration = std::time::Duration::from_secs(30);

enum Job {
    Register {
        journal: Journal,
        blob_dir: Option<PathBuf>,
        done: oneshot::Sender<Result<usize, JournalError>>,
    },
    Append {
        session: SessionId,
        slot: usize,
        batch: Vec<u8>,
        blobs: Vec<PendingBlob>,
        done: oneshot::Sender<Result<Receipt, StoreError>>,
    },
    /// Phase two of a blobbed append: every staged publish landed durable, so
    /// the worker may now write the batch. Journal bytes must never exist
    /// before the blobs they reference — a crash between the journal write
    /// and a pending publish would leave a durable record naming a blob that
    /// never reached disk.
    BlobsPublished(AppendCall),
    Retire {
        slot: usize,
        done: Option<oneshot::Sender<()>>,
    },
    /// A staged durability sync failed off-thread: the bytes it covered can
    /// no longer be rolled back in place, so the journal is marked damaged.
    Damaged { slot: usize },
    #[cfg(test)]
    Hold(Box<dyn FnOnce() + Send>),
}

/// The caller-owned half of an append, carried through both durability
/// phases.
struct AppendCall {
    session: SessionId,
    slot: usize,
    batch: Vec<u8>,
    done: oneshot::Sender<Result<Receipt, StoreError>>,
}

impl Job {
    fn is_request(&self) -> bool {
        matches!(self, Self::Register { .. } | Self::Append { .. })
    }
}

struct QueueState {
    jobs: VecDeque<Job>,
    requests: usize,
    closed: bool,
    stopped: bool,
    next_waiter: u64,
    waiters: VecDeque<(u64, Waker)>,
}

/// A bounded FIFO shared by async producers and one blocking worker.
struct Queue {
    state: Mutex<QueueState>,
    ready: Condvar,
}

impl Queue {
    fn new() -> Self {
        Self {
            state: Mutex::new(QueueState {
                jobs: VecDeque::new(),
                requests: 0,
                closed: false,
                stopped: false,
                next_waiter: 0,
                waiters: VecDeque::new(),
            }),
            ready: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, QueueState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn enqueue(self: &Arc<Self>, job: Job) -> Enqueue {
        debug_assert!(job.is_request());
        Enqueue {
            queue: Arc::clone(self),
            job: Some(job),
            waiter: None,
        }
    }

    /// Adds cleanup work to the FIFO. Each lane queues at most one `Retire`;
    /// control work does not consume the 256 request slots.
    fn push_control(&self, job: Job) -> bool {
        debug_assert!(!job.is_request());
        let mut state = self.lock();
        if state.stopped {
            return false;
        }
        state.jobs.push_back(job);
        self.ready.notify_one();
        true
    }

    fn pop(&self) -> Option<Job> {
        let mut state = self.lock();
        loop {
            if let Some(job) = state.jobs.pop_front() {
                let waker = if job.is_request() {
                    state.requests -= 1;
                    first_waiter_waker(&state)
                } else {
                    None
                };
                drop(state);
                wake_one(waker);
                return Some(job);
            }
            if state.closed {
                state.stopped = true;
                let wakers = waiter_wakers(&state);
                drop(state);
                wake_all(wakers);
                return None;
            }
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn close(&self) {
        let wakers = {
            let mut state = self.lock();
            state.closed = true;
            self.ready.notify_all();
            waiter_wakers(&state)
        };
        wake_all(wakers);
    }

    fn mark_stopped(&self) {
        let (jobs, wakers) = {
            let mut state = self.lock();
            state.stopped = true;
            state.closed = true;
            state.requests = 0;
            self.ready.notify_all();
            (std::mem::take(&mut state.jobs), waiter_wakers(&state))
        };
        drop(jobs);
        wake_all(wakers);
    }
    #[cfg(test)]
    fn request_count(&self) -> usize {
        self.lock().requests
    }
}

/// Async FIFO admission. While pending this owns the request, but dropping it
/// releases that request without changing the queue.
struct Enqueue {
    queue: Arc<Queue>,
    job: Option<Job>,
    waiter: Option<u64>,
}

impl Future for Enqueue {
    type Output = Result<(), ()>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.as_mut().get_mut();
        let mut state = this.queue.lock();
        if state.closed || state.stopped {
            if let Some(waiter) = this.waiter.take() {
                remove_waiter(&mut state, waiter);
            }
            return Poll::Ready(Err(()));
        }

        let waiter = *this.waiter.get_or_insert_with(|| {
            let waiter = state.next_waiter;
            state.next_waiter = waiter.wrapping_add(1);
            waiter
        });
        let is_turn = state
            .waiters
            .front()
            .is_none_or(|(front, _)| *front == waiter);
        if state.requests < QUEUE_CAPACITY && is_turn {
            this.waiter = None;
            remove_waiter(&mut state, waiter);
            let Some(job) = this.job.take() else {
                return Poll::Ready(Err(()));
            };
            state.requests += 1;
            state.jobs.push_back(job);
            let waker = first_waiter_waker(&state);
            this.queue.ready.notify_one();
            drop(state);
            wake_one(waker);
            return Poll::Ready(Ok(()));
        }

        if let Some((_, waker)) = state.waiters.iter_mut().find(|(id, _)| *id == waiter) {
            if !waker.will_wake(context.waker()) {
                waker.clone_from(context.waker());
            }
        } else {
            state.waiters.push_back((waiter, context.waker().clone()));
        }
        Poll::Pending
    }
}

impl Drop for Enqueue {
    fn drop(&mut self) {
        let Some(waiter) = self.waiter.take() else {
            return;
        };
        let wakers = {
            let mut state = self.queue.lock();
            let was_front = state.waiters.front().is_some_and(|(id, _)| *id == waiter);
            remove_waiter(&mut state, waiter);
            if was_front {
                first_waiter_waker(&state)
            } else {
                None
            }
        };
        wake_one(wakers);
    }
}

fn remove_waiter(state: &mut QueueState, waiter: u64) {
    if let Some(index) = state.waiters.iter().position(|(id, _)| *id == waiter) {
        state.waiters.remove(index);
    }
}

fn first_waiter_waker(state: &QueueState) -> Option<Waker> {
    state.waiters.front().map(|(_, waker)| waker.clone())
}

fn waiter_wakers(state: &QueueState) -> Vec<Waker> {
    state
        .waiters
        .iter()
        .map(|(_, waker)| waker.clone())
        .collect()
}

fn wake_one(waker: Option<Waker>) {
    if let Some(waker) = waker {
        waker.wake();
    }
}

fn wake_all(wakers: Vec<Waker>) {
    for waker in wakers {
        waker.wake();
    }
}

struct Shard {
    queue: Arc<Queue>,
    thread: Option<JoinHandle<()>>,
    syncer: Option<JoinHandle<()>>,
}

/// One staged durable operation finished on the per-shard syncer. Steps run
/// in queue order so the journal fsync lands after every blob publish its
/// batch references.
enum SyncStep {
    /// A staged blob publish: temp sync, rename, and directory sync.
    Publish(blob::StagedPublish),
    /// A dup handle to a journal whose batch already wrote on the worker.
    Journal { file: std::fs::File, path: PathBuf },
}

/// What the syncer does once the staged steps finish.
enum Then {
    /// The journal batch already wrote on the worker (its blobs were durable
    /// beforehand); resolve the receipt.
    Resolve {
        slot: usize,
        receipt: Receipt,
        done: oneshot::Sender<Result<Receipt, StoreError>>,
    },
    /// Blob publishes landed (or failed): hand the journal write back to the
    /// worker as a FIFO-ordered control job, or fail the caller without
    /// touching the journal.
    Journalize(AppendCall),
}

/// One append's staged durability work: the writes already happened on the
/// worker; `then` decides how the completion resolves. `born` feeds the
/// contention probe: a slow filesystem shows up as queue wait here first.
struct SyncJob {
    born: std::time::Instant,
    steps: Vec<SyncStep>,
    then: Then,
    queue: Arc<Queue>,
}

fn finish_steps(steps: Vec<SyncStep>) -> Result<(), StoreError> {
    // Every persist into a directory owes that directory one sync before the
    // batch resolves; paying it once per distinct directory instead of once
    // per step keeps the ordering contract while collapsing the fsync convoy
    // that stalls a shared-storage runner.
    let mut dirs = Vec::new();
    let mut first_error = None;
    for step in steps {
        let result = match step {
            SyncStep::Publish(staged) => {
                blob::finish_staged(staged, &mut dirs).map_err(StoreError::from)
            }
            SyncStep::Journal { file, path } => file.sync_all().map_err(|source| {
                StoreError::Journal(JournalError::Io {
                    op: "sync",
                    path,
                    source: Box::new(source),
                })
            }),
        };
        if let Err(error) = result
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    // Publishes that persisted before the first failure still earn their
    // directory sync; durable orphans are harmless where lost ones would lie.
    let synced = blob::sync_dirs(&mut dirs).map_err(StoreError::from);
    match (first_error, synced) {
        (Some(failure), _) | (None, Err(failure)) => Err(failure),
        (None, Ok(())) => Ok(()),
    }
}

fn run_syncer(inbox: &std::sync::mpsc::Receiver<SyncJob>) {
    while let Ok(job) = inbox.recv() {
        let mark = std::time::Instant::now();
        let result = finish_steps(job.steps);
        // Only slow batches report: an unconditional line per batch would
        // flood stderr on child processes and can stall their writers.
        let taken = mark.elapsed();
        if job.born.elapsed() > std::time::Duration::from_millis(250)
            || taken > std::time::Duration::from_millis(250)
        {
            eprintln!(
                "[dal-store] sync waited {:?} took {taken:?}",
                job.born.elapsed()
            );
        }
        match job.then {
            Then::Resolve {
                slot,
                receipt,
                done,
            } => {
                if result.is_err() {
                    // FIFO-ordered behind every append queued so far: the mark
                    // lands before later appends run, so they fail instead of
                    // writing onto bytes whose durability is no longer trusted.
                    let _ = job.queue.push_control(Job::Damaged { slot });
                }
                let _ = done.send(result.map(|()| receipt));
            }
            Then::Journalize(call) => match result {
                Ok(()) => {
                    // FIFO-ordered behind every append queued so far. The
                    // lane's one-in-flight rule keeps this journal's appends
                    // serial, so interleaving other lanes' jobs is safe. If the
                    // worker already stopped, the dropped reply resolves the
                    // caller as closed.
                    let _ = job.queue.push_control(Job::BlobsPublished(call));
                }
                // The journal never got bytes: it stays healthy and the
                // caller simply learns the publication failed.
                Err(failure) => {
                    let _ = call.done.send(Err(failure));
                }
            },
        }
    }
}
struct RegistrationReply {
    queue: Arc<Queue>,
    receiver: Option<oneshot::Receiver<Result<usize, JournalError>>>,
}

impl Drop for RegistrationReply {
    fn drop(&mut self) {
        let Some(mut receiver) = self.receiver.take() else {
            return;
        };
        receiver.close();
        if let Ok(Ok(slot)) = receiver.try_recv() {
            let _ = self.queue.push_control(Job::Retire { slot, done: None });
        }
    }
}

/// Owner of exactly four journal shard threads.
///
/// Each worker owns its assigned physical journals. Dropping this owner closes
/// admission, drains accepted requests, and joins all four threads. Lanes that
/// outlive it receive a wrapped [`JournalError::ShardClosed`].
#[must_use]
pub(crate) struct Shards {
    shards: Vec<Shard>,
}

impl Shards {
    /// Starts exactly four standard-thread journal workers.
    ///
    /// # Errors
    /// Returns [`JournalError::Io`] if the operating system refuses a worker
    /// thread. Any workers already created are closed and joined.
    pub(crate) fn start() -> Result<Self, JournalError> {
        let mut shards: Vec<Shard> = Vec::with_capacity(SHARD_COUNT);
        for index in 0..SHARD_COUNT {
            let queue = Arc::new(Queue::new());
            let (sync_outbox, sync_inbox) = std::sync::mpsc::channel::<SyncJob>();
            let worker_queue = Arc::clone(&queue);
            let spawned = (|| {
                let thread = thread::Builder::new()
                    .name(format!("dal-journal-{index}"))
                    .spawn(move || {
                        let _stopped = WorkerStopped(Arc::clone(&worker_queue));
                        run(worker_queue, sync_outbox);
                    })?;
                let syncer = thread::Builder::new()
                    .name(format!("dal-journal-sync-{index}"))
                    .spawn(move || run_syncer(&sync_inbox))?;
                Ok::<_, std::io::Error>((thread, syncer))
            })();
            let (thread, syncer) = match spawned {
                Ok(pair) => pair,
                Err(source) => {
                    for shard in &shards {
                        shard.queue.close();
                    }
                    for shard in &mut shards {
                        if let Some(thread) = shard.thread.take() {
                            drop(thread.join());
                        }
                        if let Some(syncer) = shard.syncer.take() {
                            drop(syncer.join());
                        }
                    }
                    return Err(JournalError::Io {
                        op: "open",
                        path: PathBuf::from(format!("dal-journal-{index}")),
                        source: Box::new(source),
                    });
                }
            };
            shards.push(Shard {
                queue,
                thread: Some(thread),
                syncer: Some(syncer),
            });
        }
        Ok(Self { shards })
    }

    /// Moves a file journal to the shard pinned by `session` and returns its
    /// actor-facing handle. `blob_dir` is `<session>/blobs`, which holds digest files.
    ///
    /// # Errors
    /// Returns [`JournalError::ShardClosed`] if that worker has stopped.
    pub(crate) async fn attach(
        &self,
        session: SessionId,
        journal: Journal,
        blob_dir: Option<PathBuf>,
    ) -> Result<Lane, JournalError> {
        let queue = Arc::clone(&self.shards[pin(session)].queue);
        let (done, reply) = oneshot::channel();
        tokio::time::timeout(
            ENQUEUE_WAIT,
            queue.enqueue(Job::Register {
                journal,
                blob_dir,
                done,
            }),
        )
        .await
        .map_err(|_| enqueue_timeout(session))?
        .map_err(|()| journal_closed(session))?;
        let mut registration = RegistrationReply {
            queue: Arc::clone(&queue),
            receiver: Some(reply),
        };
        let response = match registration.receiver.as_mut() {
            Some(receiver) => receiver.await,
            None => return Err(journal_closed(session)),
        };
        registration.receiver = None;
        let slot = response.map_err(|_| journal_closed(session))??;
        Ok(Lane {
            queue,
            session,
            slot,
            pending: None,
            retiring: false,
            retired: None,
            _not_sync: PhantomData,
        })
    }
    /// Holds one worker until the returned release channel is signalled.
    #[cfg(test)]
    pub(crate) fn hold_worker_for_test(
        &self,
        session: SessionId,
    ) -> Option<(std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>)> {
        let queue = &self.shards[pin(session)].queue;
        let (started, started_rx) = std::sync::mpsc::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        let held = Job::Hold(Box::new(move || {
            let _ = started.send(());
            let _ = release_rx.recv();
        }));
        queue.push_control(held).then_some((started_rx, release))
    }
}

impl Drop for Shards {
    fn drop(&mut self) {
        for shard in &self.shards {
            shard.queue.close();
        }
        for shard in &mut self.shards {
            // Joining the worker drops its sync outbox, which ends the
            // syncer's recv loop after every staged sync resolves.
            if let Some(thread) = shard.thread.take() {
                drop(thread.join());
            }
            if let Some(syncer) = shard.syncer.take() {
                drop(syncer.join());
            }
        }
    }
}
impl fmt::Debug for Shards {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Shards")
            .field("workers", &self.shards.len())
            .finish_non_exhaustive()
    }
}

/// Deterministically pins a session UUID to one shard with FNV-1a.
#[must_use]
pub(crate) fn pin(session: SessionId) -> usize {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in session.as_uuid().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    match hash % 4 {
        0 => 0,
        1 => 1,
        2 => 2,
        _ => 3,
    }
}

/// Actor-facing handle for one session's pinned journal. The worker owns the
/// file between calls; this lane is `Send` but not `Sync` or `Clone`, and its
/// mutation uses `&mut`.
#[must_use]
pub(crate) struct Lane {
    queue: Arc<Queue>,
    session: SessionId,
    slot: usize,
    pending: Option<(
        std::time::Instant,
        oneshot::Receiver<Result<Receipt, StoreError>>,
    )>,
    retiring: bool,
    retired: Option<oneshot::Receiver<()>>,
    // Cell is Send but not Sync, so the lane cannot be shared between actors.
    _not_sync: PhantomData<Cell<()>>,
}
impl fmt::Debug for Lane {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Lane")
            .field("session", &self.session)
            .field("slot", &self.slot)
            .field("pending", &self.pending.is_some())
            .field("retiring", &self.retiring)
            .finish_non_exhaustive()
    }
}

impl Lane {
    /// Appends one complete batch on the pinned worker. Prepared blobs are
    /// published before the journal append; the receipt follows file sync.
    ///
    /// A lane admits one unacknowledged request. If this future is cancelled
    /// after admission, call [`Lane::settle`] before another append; it returns
    /// the retained receipt or error exactly once. Cancelling while waiting
    /// for queue capacity enqueues nothing.
    ///
    /// # Errors
    /// Returns [`StoreError::Blob`] for blob publication failure or
    /// [`StoreError::Journal`] for write, sync, rollback, pending-batch, or
    /// closed-worker failures.
    pub(crate) async fn append(
        &mut self,
        batch: Vec<u8>,
        blobs: Vec<PendingBlob>,
    ) -> Result<Receipt, StoreError> {
        if self.pending.is_some() {
            return Err(JournalError::BatchPending {
                session: self.session,
            }
            .into());
        }
        if self.retiring {
            return Err(closed(self.session));
        }

        let (done, reply) = oneshot::channel();
        let born = std::time::Instant::now();
        tokio::time::timeout(
            ENQUEUE_WAIT,
            self.queue.enqueue(Job::Append {
                session: self.session,
                slot: self.slot,
                batch,
                blobs,
                done,
            }),
        )
        .await
        .map_err(|_| enqueue_timeout(self.session))?
        .map_err(|()| closed(self.session))?;
        self.pending = Some((born, reply));
        self.settle()
            .await
            .unwrap_or_else(|| Err(closed(self.session)))
    }

    /// Receives the result for an admitted append, including one whose
    /// `append` future was cancelled. Returns `None` if no append is pending;
    /// the pending result is consumed exactly once.
    pub(crate) async fn settle(&mut self) -> Option<Result<Receipt, StoreError>> {
        let (born, pending) = self.pending.as_mut()?;
        let result = loop {
            match tokio::time::timeout(SETTLE_REPORT, &mut *pending).await {
                Ok(result) => break result.unwrap_or_else(|_| Err(closed(self.session))),
                // A receipt this late means the admitted job is parked
                // somewhere in the worker/syncer chain; name it.
                Err(_) => eprintln!(
                    "[dal-store] session {:?} append receipt outstanding {:?}",
                    self.session,
                    born.elapsed()
                ),
            }
        };
        self.pending = None;
        Some(result)
    }

    /// Closes this lane's worker-owned journal after all append outcomes settle.
    ///
    /// # Errors
    /// Returns [`JournalError::BatchPending`] if an append receipt must first
    /// be consumed, or [`JournalError::ShardClosed`] if the worker stopped.
    pub(crate) async fn close(&mut self) -> Result<(), JournalError> {
        if self.pending.is_some() {
            return Err(JournalError::BatchPending {
                session: self.session,
            });
        }
        if !self.retiring {
            let (done, reply) = oneshot::channel();
            if !self.queue.push_control(Job::Retire {
                slot: self.slot,
                done: Some(done),
            }) {
                return Err(journal_closed(self.session));
            }
            self.retired = Some(reply);
            self.retiring = true;
        }
        let Some(retired) = self.retired.as_mut() else {
            return Ok(());
        };
        retired.await.map_err(|_| journal_closed(self.session))?;
        self.retired = None;
        Ok(())
    }
}

impl Drop for Lane {
    fn drop(&mut self) {
        if !self.retiring {
            // The worker may already be stopped; its slot table was then dropped.
            let _ = self.queue.push_control(Job::Retire {
                slot: self.slot,
                done: None,
            });
            self.retiring = true;
        }
    }
}

struct WorkerStopped(Arc<Queue>);

impl Drop for WorkerStopped {
    fn drop(&mut self) {
        self.0.mark_stopped();
    }
}

/// The worker-local slot table is the sole owner of this shard's journals.
#[expect(
    clippy::needless_pass_by_value,
    reason = "the shard thread owns its queue for its lifetime"
)]
fn run(queue: Arc<Queue>, sync: std::sync::mpsc::Sender<SyncJob>) {
    let mut journals: Vec<Option<(Journal, Option<PathBuf>)>> = Vec::new();
    let mut free = Vec::new();
    while let Some(job) = queue.pop() {
        match job {
            Job::Register {
                journal,
                blob_dir,
                done,
            } => {
                let slot = free.pop().unwrap_or_else(|| {
                    journals.push(None);
                    journals.len() - 1
                });
                journals[slot] = Some((journal, blob_dir));
                if done.send(Ok(slot)).is_err() {
                    drop(journals[slot].take());
                    free.push(slot);
                }
            }
            Job::Append {
                session,
                slot,
                batch,
                blobs,
                done,
            } => {
                let call = AppendCall {
                    session,
                    slot,
                    batch,
                    done,
                };
                if blobs.is_empty() {
                    // Nothing to publish: the batch can write immediately.
                    journalize(&mut journals, &queue, &sync, call);
                    continue;
                }
                // Phase one: write blob temps on the worker and hand the
                // ordered publish steps to the syncer. The journal write waits
                // for `BlobsPublished` so bytes can never exist before the
                // blobs they reference are durable.
                let mut steps = Vec::new();
                let staged = match journals.get_mut(slot).and_then(Option::as_mut) {
                    Some((_, blob_dir)) => publish(blobs, blob_dir.as_deref(), &mut steps),
                    None => Err(closed(call.session)),
                };
                match staged {
                    Ok(()) => {
                        let job = SyncJob {
                            born: std::time::Instant::now(),
                            steps,
                            then: Then::Journalize(call),
                            queue: Arc::clone(&queue),
                        };
                        if let Err(unsent) = sync.send(job) {
                            // The syncer died: finish the publishes inline,
                            // then the journal write, on the worker.
                            let job = unsent.0;
                            let Then::Journalize(call) = job.then else {
                                debug_assert!(false, "only Journalize jobs reach this arm");
                                continue;
                            };
                            match finish_steps(job.steps) {
                                Ok(()) => journalize(&mut journals, &queue, &sync, call),
                                Err(failure) => {
                                    let _ = call.done.send(Err(failure));
                                }
                            }
                        }
                    }
                    Err(failure) => {
                        let _ = call.done.send(Err(failure));
                    }
                }
            }
            Job::BlobsPublished(call) => journalize(&mut journals, &queue, &sync, call),
            Job::Damaged { slot } => {
                if let Some((journal, _)) = journals.get_mut(slot).and_then(Option::as_mut) {
                    journal.mark_damaged();
                }
            }
            Job::Retire { slot, done } => {
                if let Some((journal, _)) = journals.get_mut(slot).and_then(Option::take) {
                    drop(journal);
                    free.push(slot);
                }
                if let Some(done) = done {
                    let _ = done.send(());
                }
            }
            #[cfg(test)]
            Job::Hold(work) => work(),
        }
    }
}

/// Writes `batch` on the worker and stages its durability sync for the
/// syncer. Reached only once every blob the batch references is durable:
/// immediately for a blob-less append, or through [`Job::BlobsPublished`]
/// after the publish steps landed.
fn journalize(
    journals: &mut [Option<(Journal, Option<PathBuf>)>],
    queue: &Arc<Queue>,
    sync: &std::sync::mpsc::Sender<SyncJob>,
    call: AppendCall,
) {
    let AppendCall {
        session,
        slot,
        batch,
        done,
    } = call;
    let staged = match journals.get_mut(slot).and_then(Option::as_mut) {
        Some((journal, _)) => journal
            .append_unsynced(&batch)
            .map_err(StoreError::from)
            .and_then(|receipt| match journal.stage_sync(receipt) {
                Ok(staged) => Ok(staged),
                Err(failure) => {
                    // The sync never left the worker: roll the batch back
                    // exactly as `Journal::append` would have on a failed sync.
                    let repair = journal
                        .roll_back(receipt)
                        .err()
                        .map_or_else(|| StoreError::from(failure), StoreError::from);
                    Err(repair)
                }
            }),
        None => Err(closed(session)),
    };
    match staged {
        Ok((file, path, receipt)) => {
            let job = SyncJob {
                born: std::time::Instant::now(),
                steps: vec![SyncStep::Journal { file, path }],
                then: Then::Resolve {
                    slot,
                    receipt,
                    done,
                },
                queue: Arc::clone(queue),
            };
            if let Err(unsent) = sync.send(job) {
                // The syncer died: resolve the receipt inline so the lane
                // still learns the durable outcome.
                let job = unsent.0;
                let Then::Resolve {
                    slot,
                    receipt,
                    done,
                } = job.then
                else {
                    debug_assert!(false, "journalize stages only Resolve jobs");
                    return;
                };
                let result = finish_steps(job.steps).map(|()| receipt);
                if result.is_err()
                    && let Some((journal, _)) = journals.get_mut(slot).and_then(Option::as_mut)
                {
                    journal.mark_damaged();
                }
                let _ = done.send(result);
            }
        }
        Err(failure) => {
            let _ = done.send(Err(failure));
        }
    }
}

/// Writes each blob's temp on the worker and stages its finish steps. Actual
/// digest files appear only when the syncer runs the staged publishes, which
/// keeps blob syncs off the FIFO worker the same way journal syncs stay off.
fn publish(
    blobs: Vec<PendingBlob>,
    blob_dir: Option<&std::path::Path>,
    steps: &mut Vec<SyncStep>,
) -> Result<(), StoreError> {
    if blobs.is_empty() {
        return Ok(());
    }
    let Some(dir) = blob_dir else {
        return Err(StoreError::Invalid {
            reason: "prepared blobs supplied without a session blob directory".into(),
        });
    };
    let mut present = false;
    let mut wrote = false;
    for pending in blobs {
        match blob::stage_prepared(dir, pending)? {
            blob::StagedPublish::Present { .. } => present = true,
            staged @ blob::StagedPublish::Pending { .. } => {
                wrote = true;
                steps.push(SyncStep::Publish(staged));
            }
        }
    }
    // A staged publish already syncs the directory on its finish; only a set
    // where every digest already existed still owes one.
    if present && !wrote {
        steps.push(SyncStep::Publish(blob::StagedPublish::Present {
            dir: dir.to_path_buf(),
        }));
    }
    Ok(())
}

fn closed(session: SessionId) -> StoreError {
    journal_closed(session).into()
}

fn enqueue_timeout(session: SessionId) -> JournalError {
    JournalError::Io {
        op: "admit",
        path: PathBuf::from(format!("shard-queue/{session}")),
        source: std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "journal shard queue admission timed out",
        )
        .into(),
    }
}

fn journal_closed(session: SessionId) -> JournalError {
    JournalError::ShardClosed { session }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        num::NonZeroU64,
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
            mpsc as std_mpsc,
        },
        task::Poll,
    };

    use dal_core::{Entry, EntryId, EntryKind, JournalPart, Record, SessionId};
    use tokio::task::yield_now;

    use super::{Job, Lane, PendingBlob, QUEUE_CAPACITY, SHARD_COUNT, Shards, pin};
    use crate::{
        blob::INLINE_LIMIT,
        error::{BlobError, JournalError, StoreError},
        journal::{Faults, Journal},
    };

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("dal-shard-{}-{serial}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("create fresh test directory");
            Self(path)
        }

        fn file(&self, index: usize) -> PathBuf {
            self.0.join(format!("journal-{index}.jsonl"))
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn session_on(shard: usize) -> SessionId {
        loop {
            let session = SessionId::new_v7();
            if pin(session) == shard {
                return session;
            }
        }
    }

    fn journal(path: &Path, faults: &Faults) -> Journal {
        Journal::create(path, b"", faults).expect("create empty journal")
    }

    fn prepared_text_blob() -> (PendingBlob, Vec<u8>, dal_core::BlobId) {
        let text = "x".repeat(INLINE_LIMIT);
        let expected = text.as_bytes().to_vec();
        let mut record = Record::User(Entry {
            id: EntryId::new(NonZeroU64::MIN),
            parent: None,
            at: jiff::Timestamp::UNIX_EPOCH,
            kind: EntryKind::User {
                parts: vec![JournalPart::Text { text: text.into() }],
            },
        });
        let mut blobs =
            crate::blob::prepare_record(&mut record).expect("prepare threshold-sized text blob");
        let pending = blobs.pop().expect("one pending blob");
        assert!(blobs.is_empty());
        let id = pending.id();
        (pending, expected, id)
    }

    async fn attached(shards: &Shards, session: SessionId, path: &Path, faults: &Faults) -> Lane {
        shards
            .attach(session, journal(path, faults), None)
            .await
            .expect("register journal")
    }

    #[tokio::test]
    async fn shard_receipts_are_monotone_and_bytes_are_durable() {
        let temp = TempDir::new();
        let shards = Shards::start().expect("start shards");
        let session = SessionId::new_v7();
        let path = temp.file(0);
        let mut lane = attached(&shards, session, &path, &Faults::default()).await;

        let first = lane.append(b"one\n".to_vec(), Vec::new()).await.unwrap();
        let second = lane.append(b"two\n".to_vec(), Vec::new()).await.unwrap();
        assert_eq!((first.offset, first.len), (0, 4));
        assert_eq!((second.offset, second.len), (4, 4));
        lane.close().await.unwrap();
        assert_eq!(fs::read(path).unwrap(), b"one\ntwo\n");
    }

    #[tokio::test]
    async fn append_ack_follows_blob_publication() {
        let temp = TempDir::new();
        let shards = Shards::start().expect("start shards");
        let session = SessionId::new_v7();
        let path = temp.file(0);
        let (pending, expected, id) = prepared_text_blob();
        let blob_path = temp.0.join(id.to_string());
        let mut lane = shards
            .attach(
                session,
                journal(&path, &Faults::default()),
                Some(temp.0.clone()),
            )
            .await
            .expect("register journal");

        let receipt = lane
            .append(b"record\n".to_vec(), vec![pending])
            .await
            .unwrap();
        assert_eq!((receipt.offset, receipt.len), (0, 7));
        assert_eq!(fs::read(&blob_path).unwrap(), expected);
        assert_eq!(fs::read(&path).unwrap(), b"record\n");
        lane.close().await.unwrap();
    }

    #[tokio::test]
    async fn blob_publication_failure_leaves_journal_unchanged() {
        let temp = TempDir::new();
        let shards = Shards::start().expect("start shards");
        let session = SessionId::new_v7();
        let path = temp.file(0);
        let missing_blob_dir = temp.0.join("missing-blobs");
        let (pending, _, _) = prepared_text_blob();
        let mut lane = shards
            .attach(
                session,
                journal(&path, &Faults::default()),
                Some(missing_blob_dir),
            )
            .await
            .expect("register journal");

        let error = lane
            .append(b"must not be published\n".to_vec(), vec![pending])
            .await
            .unwrap_err();
        assert!(matches!(error, StoreError::Blob(BlobError::Io { .. })));
        assert!(fs::read(path).unwrap().is_empty());
        lane.close().await.unwrap();
    }

    #[tokio::test]
    async fn cancelled_attach_retires_a_registered_journal() {
        let temp = TempDir::new();
        let shards = Shards::start().expect("start shards");
        let session = session_on(0);
        let queue = Arc::clone(&shards.shards[pin(session)].queue);
        let (first_started, first_started_rx) = std_mpsc::channel();
        let (first_release, first_release_rx) = std_mpsc::channel();
        queue.push_control(Job::Hold(Box::new(move || {
            let _ = first_started.send(());
            let _ = first_release_rx.recv();
        })));
        first_started_rx.recv().unwrap();

        let mut attach =
            Box::pin(shards.attach(session, journal(&temp.file(0), &Faults::default()), None));
        std::future::poll_fn(|context| match attach.as_mut().poll(context) {
            Poll::Pending => Poll::Ready(()),
            Poll::Ready(_) => panic!("worker is held before registering"),
        })
        .await;
        let (second_started, second_started_rx) = std_mpsc::channel();
        let (second_release, second_release_rx) = std_mpsc::channel();
        queue.push_control(Job::Hold(Box::new(move || {
            let _ = second_started.send(());
            let _ = second_release_rx.recv();
        })));
        let _ = first_release.send(());
        // The worker has registered the journal and sent its ack before this hold starts.
        second_started_rx.recv().unwrap();
        drop(attach);
        let _ = second_release.send(());

        let mut lane = shards
            .attach(session, journal(&temp.file(1), &Faults::default()), None)
            .await
            .expect("register replacement journal");
        assert_eq!(lane.slot, 0);
        lane.close().await.unwrap();
        assert!(fs::read(temp.file(0)).unwrap().is_empty());
    }

    #[tokio::test]
    async fn partial_write_rolls_back_before_error_receipt() {
        let temp = TempDir::new();
        let shards = Shards::start().expect("start shards");
        let session = SessionId::new_v7();
        let path = temp.file(0);
        let mut lane = attached(
            &shards,
            session,
            &path,
            &Faults {
                write_after_bytes: Some(1),
                ..Faults::default()
            },
        )
        .await;

        let error = lane
            .append(b"partial batch\n".to_vec(), Vec::new())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            StoreError::Journal(JournalError::Io { op: "write", .. })
        ));
        assert!(fs::read(path).unwrap().is_empty());
    }

    #[tokio::test]
    async fn sync_failure_rolls_back_before_error_receipt() {
        let temp = TempDir::new();
        let shards = Shards::start().expect("start shards");
        let session = SessionId::new_v7();
        let path = temp.file(0);
        let mut journal = journal(&path, &Faults::default());
        journal.set_faults(Faults {
            sync_error: true,
            ..Faults::default()
        });
        let mut lane = shards
            .attach(session, journal, None)
            .await
            .expect("register journal");

        let error = lane
            .append(b"written but not synced\n".to_vec(), Vec::new())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            StoreError::Journal(JournalError::Io { op: "sync", .. })
        ));
        assert!(fs::read(path).unwrap().is_empty());
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the backpressure test joins every spawned append before asserting"
    )]
    #[tokio::test]
    async fn cancelled_admission_is_not_queued_and_queued_batch_can_settle() {
        let temp = TempDir::new();
        let shards = Shards::start().expect("start shards");
        let target = 0;
        let mut lanes = Vec::with_capacity(QUEUE_CAPACITY + 1);
        for index in 0..=QUEUE_CAPACITY {
            let session = session_on(target);
            lanes.push(attached(&shards, session, &temp.file(index), &Faults::default()).await);
        }
        let queue = Arc::clone(&shards.shards[target].queue);
        let (started_tx, started_rx) = std_mpsc::channel();
        let (release_tx, release_rx) = std_mpsc::channel();
        queue.push_control(Job::Hold(Box::new(move || {
            let _ = started_tx.send(());
            let _ = release_rx.recv();
        })));
        started_rx.recv().unwrap();

        let mut in_flight = Vec::with_capacity(QUEUE_CAPACITY);
        for (index, mut lane) in lanes.drain(..QUEUE_CAPACITY).enumerate() {
            in_flight.push(tokio::task::spawn(async move {
                let receipt = lane.append(b"x\n".to_vec(), Vec::new()).await.unwrap();
                (index, receipt)
            }));
        }
        for _ in 0..1_000 {
            if queue.request_count() == QUEUE_CAPACITY {
                break;
            }
            yield_now().await;
        }
        assert_eq!(queue.request_count(), QUEUE_CAPACITY);

        let mut last = lanes.pop().unwrap();
        let mut cancelled = Box::pin(last.append(b"must not land\n".to_vec(), Vec::new()));
        std::future::poll_fn(|context| match cancelled.as_mut().poll(context) {
            Poll::Pending => Poll::Ready(()),
            Poll::Ready(_) => panic!("257th request must wait"),
        })
        .await;
        drop(cancelled);
        assert_eq!(queue.request_count(), QUEUE_CAPACITY);
        assert!(last.pending.is_none());

        let mut waiting = Box::pin(last.append(b"last\n".to_vec(), Vec::new()));
        std::future::poll_fn(|context| match waiting.as_mut().poll(context) {
            Poll::Pending => Poll::Ready(()),
            Poll::Ready(_) => panic!("full queue must apply backpressure"),
        })
        .await;
        assert_eq!(queue.request_count(), QUEUE_CAPACITY);
        let _ = release_tx.send(());
        for task in in_flight {
            let (index, receipt) = task.await.unwrap();
            assert_eq!(receipt.offset, 0);
            assert_eq!(fs::read(temp.file(index)).unwrap(), b"x\n");
        }
        let receipt = waiting.await.unwrap();
        assert_eq!(receipt.offset, 0);
        assert_eq!(fs::read(temp.file(QUEUE_CAPACITY)).unwrap(), b"last\n");
        last.close().await.unwrap();
    }

    #[tokio::test]
    async fn cancelled_accepted_batch_requires_settle_before_next_append() {
        let temp = TempDir::new();
        let shards = Shards::start().expect("start shards");
        let session = SessionId::new_v7();
        let path = temp.file(0);
        let mut lane = attached(&shards, session, &path, &Faults::default()).await;
        let (started_tx, started_rx) = std_mpsc::channel();
        let (release_tx, release_rx) = std_mpsc::channel();
        shards.shards[pin(session)]
            .queue
            .push_control(Job::Hold(Box::new(move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv();
            })));
        started_rx.recv().unwrap();

        let mut append = Box::pin(lane.append(b"first\n".to_vec(), Vec::new()));
        std::future::poll_fn(|context| match append.as_mut().poll(context) {
            Poll::Pending => Poll::Ready(()),
            Poll::Ready(_) => panic!("worker is held"),
        })
        .await;
        drop(append);
        assert!(matches!(
            lane.append(b"second\n".to_vec(), Vec::new()).await,
            Err(StoreError::Journal(JournalError::BatchPending { .. }))
        ));
        let _ = release_tx.send(());

        let first = lane.settle().await.unwrap().unwrap();
        assert_eq!(first.offset, 0);
        let second = lane.append(b"second\n".to_vec(), Vec::new()).await.unwrap();
        assert_eq!(second.offset, 6);
        lane.close().await.unwrap();
        assert_eq!(fs::read(path).unwrap(), b"first\nsecond\n");
    }

    #[tokio::test]
    async fn another_shard_syncs_while_first_worker_is_held() {
        let temp = TempDir::new();
        let shards = Shards::start().expect("start shards");
        let session_a = session_on(0);
        let shard_b = (0..SHARD_COUNT)
            .find(|index| *index != pin(session_a))
            .unwrap();
        let session_b = session_on(shard_b);
        let path = temp.file(0);
        let mut lane_b = attached(&shards, session_b, &path, &Faults::default()).await;

        let (started_tx, started_rx) = std_mpsc::channel();
        let (release_tx, release_rx) = std_mpsc::channel();
        shards.shards[pin(session_a)]
            .queue
            .push_control(Job::Hold(Box::new(move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv();
            })));
        started_rx.recv().unwrap();

        let receipt = lane_b
            .append(b"parallel\n".to_vec(), Vec::new())
            .await
            .unwrap();
        assert_eq!(receipt.offset, 0);
        assert_eq!(fs::read(path).unwrap(), b"parallel\n");
        let _ = release_tx.send(());
        lane_b.close().await.unwrap();
    }

    #[tokio::test]
    async fn dropping_shards_drains_accepted_batch_and_joins_workers() {
        let temp = TempDir::new();
        let shards = Shards::start().expect("start shards");
        let session = SessionId::new_v7();
        let path = temp.file(0);
        let mut lane = attached(&shards, session, &path, &Faults::default()).await;
        let (started_tx, started_rx) = std_mpsc::channel();
        let (release_tx, release_rx) = std_mpsc::channel();
        shards.shards[pin(session)]
            .queue
            .push_control(Job::Hold(Box::new(move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv();
            })));
        started_rx.recv().unwrap();

        let mut append = Box::pin(lane.append(b"whole\n".to_vec(), Vec::new()));
        std::future::poll_fn(|context| match append.as_mut().poll(context) {
            Poll::Pending => Poll::Ready(()),
            Poll::Ready(_) => panic!("worker is held"),
        })
        .await;
        drop(append);
        let _ = release_tx.send(());
        drop(shards);

        assert_eq!(lane.settle().await.unwrap().unwrap().len, 6);
        assert_eq!(fs::read(path).unwrap(), b"whole\n");
        assert!(matches!(
            lane.append(b"after close\n".to_vec(), Vec::new()).await,
            Err(StoreError::Journal(JournalError::ShardClosed { .. }))
        ));
        assert!(matches!(
            lane.close().await,
            Err(JournalError::ShardClosed { .. })
        ));
    }

    #[test]
    fn pin_uses_stable_fnv_uuid_mapping() {
        let session = SessionId::parse("01927f3a-8c2e-7b4d-9f10-3a5b6c7d8e9f")
            .expect("valid fixed v7 session id");
        assert_eq!(pin(session), 1);
        assert!(pin(session) < SHARD_COUNT);
    }
}
