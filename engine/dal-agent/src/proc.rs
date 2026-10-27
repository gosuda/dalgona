use std::{
    collections::VecDeque,
    ffi::OsString,
    fs,
    future::Future,
    io,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};

use dal_core::{CallId, JobId, JobOutcome};
use process_wrap::tokio::{ChildWrapper, CommandWrap};
#[cfg(unix)]
use process_wrap::tokio::{KillOnDrop, ProcessGroup};
use tokio::{
    task::JoinSet,
    time::{self, Instant as TokioInstant},
};
use tokio_util::sync::CancellationToken;

use crate::{
    error::{DenyReason, ToolError},
    ext::Approved,
};

mod capture;
pub(crate) mod sandbox;
mod snapshot;
mod stop;
#[cfg(test)]
mod tests;
#[cfg(windows)]
mod windows;

use capture::{CaptureResult, run_capture};
pub(crate) use snapshot::{ProcessLease, SnapshotCoordinator};
use stop::{
    hard_kill, outcome_for, process_exit_status, process_failure, soft_kill, status_for,
    sweep_process_group, sweep_recorded,
};

/// Maximum size of one read from a child output pipe.
pub(crate) const OUTPUT_CHUNK_BYTES: usize = 65_536;
/// Maximum number of bytes retained for a live output tail.
pub(crate) const TAIL_RING_BYTES: usize = 65_536;
/// Maximum durable output size, including the truncation marker.
pub const OUTPUT_FILE_CAP_BYTES: u64 = 16_777_216;
/// Maximum suffix returned as foreground output.
pub const PREVIEW_BYTES: usize = 4_096;
/// Maximum suffix retained for a completion notice.
pub(crate) const COMPLETION_TAIL_BYTES: usize = 2_048;
/// Number of output lines in periodic progress.
pub const PROGRESS_LINES: usize = 20;
/// Period between periodic output progress updates.
pub const PROGRESS_PERIOD: Duration = Duration::from_millis(250);
/// Grace period between the soft and hard process-group stop.
pub(crate) const KILL_GRACE: Duration = Duration::from_secs(2);
/// Maximum active child jobs for one session.
pub(crate) const SESSION_CHILD_LIMIT: usize = 200;
/// Maximum stdout prefix requested by a run service.
pub(crate) const MAX_STDOUT_PREFIX_BYTES: usize = 262_145;
/// File descriptors reserved by one child: stdin, stdout, and stderr pipes.
pub(crate) const PROCESS_FD_COST: usize = 4;
/// Prefix used when presenting the durable log path.
pub const FULL_OUTPUT_PREFIX: &str = "Full output: ";
/// Marker written as the final bytes of a capped output file.
pub const TRUNCATION_MARKER: &str = "[dalgon: output truncated at 16777216 bytes]";

/// Publishes one keyed process-progress row while a child runs.
///
/// The owning session implements this with the keyed update port that
/// replaces the unsent `UpdateKind::ToolProgress` row for the call.
/// `Proc` only renders the tail and never touches the journal.
pub(crate) trait ProgressPort: Send + Sync + 'static {
    /// Publishes `tail` as the current progress text of `call`.
    fn publish_progress(&self, call: &CallId, tail: Box<str>);
}

/// The last `n` lines of a tail snapshot, lossily decoded for display only.
///
/// The durable log keeps the raw bytes; this rendering never feeds back
/// into capture. It is the exec-owned rendering rule the door renders for
/// each 250 ms progress row.
fn last_lines(bytes: &[u8], n: usize) -> String {
    // `n == 0` keeps the old slice semantics: an empty render, not the
    // whole tail.
    if n == 0 {
        return String::new();
    }
    // The suffix starts after the `n`-th newline from the end: a `\n` byte
    // can never sit inside a multi-byte UTF-8 sequence, so one lossy
    // conversion over the borrowed suffix equals per-line lossy decoding
    // joined on the same separators, trailing empty segment included.
    let mut newlines = 0;
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate().rev() {
        if *byte == b'\n' {
            newlines += 1;
            if newlines == n {
                start = index + 1;
                break;
            }
        }
    }
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

/// How a running process was stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StopReason {
    /// The configured process timeout elapsed.
    Timeout,
    /// The owning turn or job was cancelled.
    Cancelled,
}

/// The observed terminal process status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcStatus {
    /// The process exited normally with an operating-system code.
    Exited {
        /// The operating-system exit code.
        code: i32,
    },
    /// The process was terminated by a signal.
    Signaled {
        /// The terminating signal number.
        signal: i32,
    },
    /// The process was stopped by its configured timeout.
    TimedOut,
    /// The process was stopped by its owning cancellation token.
    Cancelled,
}

/// Process launch options supplied by a validated tool call.
#[derive(Clone, Debug)]
pub struct SpawnOpts {
    /// The explicit current directory for the child.
    pub cwd: PathBuf,
    /// An optional timeout measured from successful spawn.
    pub timeout: Option<Duration>,
    /// Explicit child environment overrides, applied after the host snapshot.
    pub env: Vec<(OsString, OsString)>,
    /// Bytes of stdout kept for the caller as a prefix; `0` keeps none.
    ///
    /// Output past the limit sets the overflow flag. A limit above 262145
    /// bytes is refused.
    pub stdout_prefix_limit: usize,
}

/// Captured process result and durable-output pointers.
#[derive(Clone, Debug)]
pub struct ProcResult {
    /// The process status before it is mapped to a tool result.
    pub status: ProcStatus,
    /// The durable job outcome used by the session fold.
    pub outcome: JobOutcome,
    /// The kept output suffix, omitted by callers when empty.
    pub preview: Box<[u8]>,
    /// The durable output file path.
    pub log_path: PathBuf,
    /// A bounded stdout prefix requested by a service caller.
    pub stdout_prefix: Box<[u8]>,
    /// Whether stdout exceeded the requested prefix bound.
    pub stdout_prefix_overflowed: bool,
    /// Whether either sandbox-denial phrase appeared anywhere in output.
    pub denial_seen: bool,
    /// The bounded tail used by completion notices.
    pub completion_tail: Box<[u8]>,
}

/// A launch mode prepared by the host at session start.
#[derive(Clone, Debug)]
pub(crate) enum Launcher {
    /// Run the target directly.
    Direct,
    /// Run the target through the prepared sandbox helper.
    Sandbox {
        /// The helper executable, when the platform requires one.
        helper: Option<PathBuf>,
        /// Canonical allowed roots in deterministic order.
        roots: Box<[PathBuf]>,
    },
}

type Permit = tokio::sync::OwnedSemaphorePermit;

type CaptureSet = JoinSet<Result<CaptureResult, io::Error>>;

/// Test-only ordering observer for the Unix wait-ownership repair.
///
/// Shares one allocation between `Proc` group-sweep sites and the private
/// test wrapper pair in `proc::tests`, so ordering assertions observe the
/// production paths rather than a disconnected model. Never installed outside
/// `#[cfg(test)]`; production signaling still targets the real spawned group.
#[cfg(all(test, unix))]
#[derive(Debug)]
pub(crate) struct TestOrdering {
    /// Released by the test to let the native leader wait complete.
    pub allow_inner: tokio::sync::Notify,
    /// Released by the test to let the retained outer group-reap wait complete.
    pub allow_outer: tokio::sync::Notify,
    /// Notified when the native leader wait is entered.
    pub got_inner: tokio::sync::Notify,
    /// Notified when the retained outer wait is entered.
    pub got_outer: tokio::sync::Notify,
    /// Notified synchronously with group-sweep disarm, before the outer wait.
    pub did_sweep: tokio::sync::Notify,
    /// Notified when a group signal is sent through the outer wrapper.
    pub did_signal: tokio::sync::Notify,
    /// Native leader wait entries.
    pub inner_entered: std::sync::atomic::AtomicUsize,
    /// Retained outer wait entries.
    pub outer_entered: std::sync::atomic::AtomicUsize,
    /// Retained outer wait completions.
    pub outer_completed: std::sync::atomic::AtomicUsize,
    /// Group-sweep disarms (`sweep_after_exit` plus `Drop::abort_group`).
    pub sweep: std::sync::atomic::AtomicUsize,
    /// Outer wrapper `SIGTERM` observations.
    pub sig_term: std::sync::atomic::AtomicUsize,
    /// Outer wrapper `SIGKILL` observations.
    pub sig_kill: std::sync::atomic::AtomicUsize,
    /// When set, the retained outer wait fails with a controlled real error
    /// after recording entry, exercising the error path with production
    /// cleanup intact.
    pub fail_outer: std::sync::atomic::AtomicBool,
}

#[cfg(all(test, unix))]
impl TestOrdering {
    pub(crate) fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            allow_inner: tokio::sync::Notify::new(),
            allow_outer: tokio::sync::Notify::new(),
            got_inner: tokio::sync::Notify::new(),
            got_outer: tokio::sync::Notify::new(),
            did_sweep: tokio::sync::Notify::new(),
            did_signal: tokio::sync::Notify::new(),
            inner_entered: std::sync::atomic::AtomicUsize::new(0),
            outer_entered: std::sync::atomic::AtomicUsize::new(0),
            outer_completed: std::sync::atomic::AtomicUsize::new(0),
            sweep: std::sync::atomic::AtomicUsize::new(0),
            sig_term: std::sync::atomic::AtomicUsize::new(0),
            sig_kill: std::sync::atomic::AtomicUsize::new(0),
            fail_outer: std::sync::atomic::AtomicBool::new(false),
        })
    }

    fn note_sweep(&self) {
        self.sweep.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.did_sweep.notify_one();
    }
}

/// One launched child and its bounded capture task.
pub struct Proc {
    child: Box<dyn ChildWrapper>,
    call: CallId,
    job: JobId,
    log_path: PathBuf,
    launcher_profile: Option<PathBuf>,
    lease: Option<ProcessLease>,
    capture: Option<CaptureSet>,
    live_tail: tokio::sync::watch::Receiver<VecDeque<u8>>,
    progress: Option<Arc<dyn ProgressPort>>,
    progress_last: String,
    leader_pid: Option<u32>,
    timeout: Option<Duration>,
    started_at: Instant,
    result: Option<ProcResult>,
    #[cfg(all(test, unix))]
    test_ordering: Option<std::sync::Arc<TestOrdering>>,
}

impl std::fmt::Debug for Proc {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Proc")
            .field("call", &self.call)
            .field("job", &self.job)
            .field("log_path", &self.log_path)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl Proc {
    /// Attaches the keyed progress port published while the child runs.
    ///
    /// The caller that owns the session update port calls this at spawn;
    /// detached parks clear it again with [`Proc::without_progress`].
    pub(crate) fn with_progress(mut self, port: Arc<dyn ProgressPort>) -> Self {
        self.progress = Some(port);
        self
    }

    /// Clears the progress port before a detached park.
    ///
    /// Reaping a detached child must not publish for a call the loop
    /// already settled; detached output stays on the job table instead.
    pub(crate) fn without_progress(mut self) -> Self {
        self.progress = None;
        self
    }

    /// Kills the whole process group when an unsettled child is dropped.
    ///
    /// A settled child is already reaped and swept. A live one at drop time
    /// is the close-abort path: the wait future died before the stop ladder
    /// ran, and the `KillOnDrop` wrapper below signals the leader only. The
    /// group dies here so no sibling outlives the session close; the census
    /// treats zombies as dead, and the leader is reaped by whoever still
    /// owns its handle, if anyone does. The post-exit sweep consumes
    /// `leader_pid`, so even an unset `result` (a capture fault after a
    /// successful reap) cannot re-signal a recycled pid here; only a
    /// never-reaped child keeps its pid.
    #[cfg(unix)]
    fn abort_group(&self) {
        if self.result.is_some() {
            return;
        }
        let Some(pid) = self.leader_pid else {
            return;
        };
        #[cfg(all(test, unix))]
        if let Some(ordering) = &self.test_ordering {
            ordering.note_sweep();
        }
        sweep_process_group(pid);
        #[cfg(target_os = "linux")]
        stop::sweep_proc_descendants(pid);
    }

    /// Waits for the child or applies its timeout/cancellation stop ladder.
    ///
    /// When a [`ProgressPort`] is attached, every [`PROGRESS_PERIOD`] tick
    /// that finds new capture bytes publishes the last [`PROGRESS_LINES`]
    /// output lines as `UpdateKind::ToolProgress` under this call's
    /// identity. A tick that finds no new bytes emits nothing, and ticks
    /// stop with the terminal outcome or cancellation: the wait loop owns
    /// the only ticker, so no detached task can outlive it.
    ///
    /// # Errors
    /// Returns an error if waiting for the child or draining captured output fails.
    pub async fn wait(&mut self, cancel: &CancellationToken) -> Result<ProcResult, ToolError> {
        /// One settled wait step: either the leader exited, or the stop ladder runs.
        enum Step {
            Exited(std::process::ExitStatus),
            Stop(StopReason),
        }

        if let Some(result) = &self.result {
            return Ok(result.clone());
        }

        let step = {
            let leader = await_leader(&mut self.child);
            tokio::pin!(leader);
            let deadline = async {
                if let Some(timeout) = self.timeout {
                    time::sleep_until(TokioInstant::from_std(self.started_at + timeout)).await;
                } else {
                    std::future::pending::<()>().await;
                }
            };
            tokio::pin!(deadline);
            let mut ticker = self.progress.as_ref().map(|_| {
                let mut ticker =
                    time::interval_at(TokioInstant::now() + PROGRESS_PERIOD, PROGRESS_PERIOD);
                ticker.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
                ticker
            });
            loop {
                tokio::select! {
                    biased;
                    status = &mut leader => break Step::Exited(status?),
                    () = &mut deadline => break Step::Stop(StopReason::Timeout),
                    () = cancel.cancelled() => break Step::Stop(StopReason::Cancelled),
                    rendered = Self::tick_progress(
                        &mut ticker,
                        &mut self.live_tail,
                        &mut self.progress_last,
                    ) => {
                        if let (Some(text), Some(port)) = (rendered, &self.progress) {
                            port.publish_progress(&self.call, text);
                        }
                    }
                }
            }
        };
        match step {
            Step::Exited(status) => {
                // A surviving descendant holding a pipe write end would stall
                // capture at EOF below; stop what the command left running first.
                self.sweep_after_exit().await?;
                self.finish(process_exit_status(status)).await
            }
            Step::Stop(reason) => self.stop(reason).await,
        }
    }

    /// Renders one progress snapshot when the ticker fires and new capture
    /// bytes arrived since the previous tick.
    ///
    /// Returns `None` when no port is attached (the arm then pends forever
    /// instead of spinning), when capture produced no new bytes, when
    /// capture already ended, or when the rendered tail repeats the
    /// previous row. The tick future is cancel-safe: dropping it between
    /// ticks only delays the next render.
    async fn tick_progress(
        ticker: &mut Option<time::Interval>,
        tail: &mut tokio::sync::watch::Receiver<VecDeque<u8>>,
        last: &mut String,
    ) -> Option<Box<str>> {
        let Some(ticker) = ticker.as_mut() else {
            return std::future::pending().await;
        };
        ticker.tick().await;
        // No new capture bytes since the last tick, or capture already
        // ended while the leader wait still resolves: emit nothing.
        if !tail.has_changed().unwrap_or(false) {
            return None;
        }
        // VecDeque storage is split and never contiguous: copy the bounded
        // ring snapshot before rendering.
        let snapshot: Vec<u8> = tail.borrow_and_update().iter().copied().collect();
        let rendered = last_lines(&snapshot, PROGRESS_LINES);
        if rendered == *last {
            return None;
        }
        last.clone_from(&rendered);
        Some(rendered.into_boxed_str())
    }

    /// Stops a live child using the platform group/job-object ladder.
    ///
    /// # Errors
    /// Returns an error if stopping the child or finalizing captured output fails.
    pub async fn stop(&mut self, reason: StopReason) -> Result<ProcResult, ToolError> {
        if let Some(result) = &self.result {
            return Ok(result.clone());
        }

        if let Some(status) =
            leader_try_wait(&mut self.child).map_err(|error| process_failure(&error))?
        {
            self.sweep_after_exit().await?;
            return self.finish(process_exit_status(status)).await;
        }

        // Snapshot the lineage before signaling: a `setsid` grandchild
        // escapes the process group, and reparents to init the instant its
        // bridge exits — the post-exit `/proc` sweep can no longer find it,
        // so the only reachable point for detached descendants is now.
        let doomed = self.recorded_descendants().await?;
        soft_kill(&mut self.child)?;
        let status =
            if let Ok(waited) = time::timeout(KILL_GRACE, leader_wait(&mut self.child)).await {
                let _ = waited.map_err(|error| process_failure(&error))?;
                sweep_recorded(&doomed);
                let outcome = self.sweep_after_exit_without_descendant_scan().await;
                outcome?;
                status_for(reason)
            } else {
                hard_kill(&mut self.child)?;
                let _ = leader_wait(&mut self.child)
                    .await
                    .map_err(|error| process_failure(&error))?;
                sweep_recorded(&doomed);
                let outcome = self.sweep_after_exit_without_descendant_scan().await;
                outcome?;
                status_for(reason)
            };
        self.finish(status).await
    }

    /// Records live descendants for the post-signal `setsid` sweep.
    ///
    /// The pre-signal snapshot is the coalesced admission-batch census: a
    /// lost census is a typed failure the stop ladder propagates instead
    /// of signaling a leader whose detached descendants are unknown.
    #[cfg(target_os = "linux")]
    async fn recorded_descendants(&mut self) -> Result<Vec<u32>, ToolError> {
        let Some(lease) = self.lease.as_ref() else {
            return Err(ToolError::Failed(Box::new(io::Error::other(
                "the process lease was released before its stop ladder",
            ))));
        };
        let Some(pid) = self.leader_pid else {
            return Ok(Vec::new());
        };
        let snapshots = Arc::clone(lease.snapshots());
        snapshots.snapshot(pid).await
    }

    /// Records live descendants for the post-signal `setsid` sweep.
    #[cfg(not(target_os = "linux"))]
    fn recorded_descendants(&mut self) -> std::future::Ready<Result<Vec<u32>, ToolError>> {
        // Non-Linux platforms have no /proc descendant source; the lease
        // stays read so the field contract matches the Linux arm.
        let _ = self.lease;
        let _ = self.leader_pid;
        std::future::ready(Ok(Vec::new()))
    }

    /// Returns the currently retained tail without reading the full log.
    #[must_use]
    pub fn tail(&self, max_bytes: usize) -> Box<[u8]> {
        let tail = self.live_tail.borrow();
        let skip = tail.len().saturating_sub(max_bytes);
        tail.iter().skip(skip).copied().collect()
    }

    /// Returns the job identity minted for this process.
    #[must_use]
    pub(crate) fn job_id(&self) -> JobId {
        self.job
    }

    /// Returns the durable log path.
    #[must_use]
    pub(crate) fn log_path(&self) -> &Path {
        &self.log_path
    }

    async fn finish(&mut self, status: ProcStatus) -> Result<ProcResult, ToolError> {
        if let Some(result) = &self.result {
            return Ok(result.clone());
        }
        // The child already exited: release the session slot and schedule
        // profile cleanup before the fallible capture join, so a capture
        // fault cannot hold a slot or leak a profile file.
        self.lease.take();
        if let Some(profile) = self.launcher_profile.take() {
            let _ = tokio::task::spawn_blocking(move || fs::remove_file(profile)).await;
        }
        let mut set = self.capture.take().ok_or_else(|| {
            ToolError::Failed(Box::new(io::Error::other(
                "process capture task is missing",
            )))
        })?;
        let captured: CaptureResult = match set.join_next().await {
            Some(Ok(Ok(captured))) => captured,
            Some(Ok(Err(error))) => {
                return Err(ToolError::Failed(Box::new(error)));
            }
            Some(Err(join)) => {
                return Err(ToolError::Failed(Box::new(io::Error::other(format!(
                    "process capture task failed: {join}"
                )))));
            }
            None => {
                return Err(ToolError::Failed(Box::new(io::Error::other(
                    "process capture task is missing",
                ))));
            }
        };
        let outcome = outcome_for(status);
        let result = ProcResult {
            status,
            outcome,
            preview: captured.preview,
            log_path: self.log_path.clone(),
            stdout_prefix: captured.stdout_prefix,
            stdout_prefix_overflowed: captured.stdout_prefix_overflowed,
            denial_seen: captured.denial_seen,
            completion_tail: captured.completion_tail,
        };
        self.result = Some(result.clone());
        Ok(result)
    }

    /// Reaps descendants the exited leader may have left running.
    ///
    /// Consumes the owned leader pid: every caller has already reaped the
    /// leader, so the pid value must not survive to `Drop`, where a capture
    /// fault with `result` still unset would re-signal a possibly recycled
    /// pid (or its group). A `Proc` dropped before any reap keeps its pid,
    /// so the close-abort sweep in `abort_group` still runs.
    async fn sweep_after_exit(&mut self) -> Result<(), ToolError> {
        let Some(pid) = self.group_reap().await? else {
            return Ok(());
        };
        #[cfg(target_os = "linux")]
        let _ = tokio::task::spawn_blocking(move || stop::sweep_proc_descendants(pid)).await;
        #[cfg(not(target_os = "linux"))]
        let _ = pid;
        Ok(())
    }

    async fn sweep_after_exit_without_descendant_scan(&mut self) -> Result<(), ToolError> {
        self.group_reap().await.map(|_| ())
    }

    #[cfg(target_os = "linux")]
    async fn group_reap(&mut self) -> Result<Option<u32>, ToolError> {
        let Some(pid) = self.leader_pid.take() else {
            return Ok(None);
        };
        #[cfg(all(test, unix))]
        if let Some(ordering) = &self.test_ordering {
            ordering.note_sweep();
        }
        sweep_process_group(pid);
        self.child
            .wait()
            .await
            .map_err(|error| process_failure(&error))?;
        Ok(Some(pid))
    }

    /// Reaps descendants the exited leader may have left running.
    ///
    /// Consumes the owned leader pid for the same reason as the Linux arm:
    /// the leader is already reaped at every call site, and only a
    /// never-reaped child may keep its pid for the `Drop` sweep.
    #[cfg(not(target_os = "linux"))]
    #[cfg_attr(
        windows,
        expect(
            clippy::unused_async,
            clippy::unused_async_trait_impl,
            reason = "the Windows sweep is synchronous; the Unix arm awaits the child"
        )
    )]
    async fn group_reap(&mut self) -> Result<Option<u32>, ToolError> {
        #[cfg(windows)]
        // TerminateJobObject at once: the leader already exited, so this
        // only reaps descendants still holding pipes open.
        let _ = self.child.start_kill();
        let Some(pid) = self.leader_pid.take() else {
            return Ok(None);
        };
        #[cfg(all(test, unix))]
        if let Some(ordering) = &self.test_ordering {
            ordering.note_sweep();
        }
        sweep_process_group(pid);
        #[cfg(unix)]
        self.child
            .wait()
            .await
            .map_err(|error| process_failure(&error))?;
        Ok(Some(pid))
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        #[cfg(windows)]
        super::LIVE_PROCS.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        #[cfg(unix)]
        self.abort_group();
        // JoinSet aborts its tasks on drop; no detached capture survives the session.
        self.capture.take();
        if let Some(profile) = self.launcher_profile.take() {
            let _ = fs::remove_file(profile);
        }
    }
}

/// Checks argv, the prefix limit, and approval, in order.
///
/// Runs before any job identity is minted or log is created, so rejection
/// releases the caller's permits without side effects.
fn check_spawn_request(
    argv: &[OsString],
    opts: &SpawnOpts,
    approved: &Approved,
    call: &CallId,
    preview_digest: Option<[u8; 32]>,
) -> Result<(), ToolError> {
    if argv.is_empty() {
        return Err(ToolError::Spawn {
            path: PathBuf::new(),
            source: io::Error::new(io::ErrorKind::InvalidInput, "empty argv"),
        });
    }
    if opts.stdout_prefix_limit > MAX_STDOUT_PREFIX_BYTES {
        return Err(ToolError::Failed(Box::new(io::Error::new(
            io::ErrorKind::InvalidInput,
            "stdout prefix limit exceeds 262145 bytes",
        ))));
    }
    if approved.call() != call
        || approved.digest() != preview_digest
        || !cwd_in_roots(&opts.cwd, approved.roots())
    {
        return Err(ToolError::Denied(DenyReason::out_of_scope(format!(
            "call {}",
            call.as_str()
        ))));
    }
    Ok(())
}

/// Starts a process after checking argv, the prefix limit, and approval, in order.
///
/// Rejection releases the permits without creating a job or log. After
/// approval the door creates the jobs directory and opens the durable log
/// under the minted job identity with `create_new` and a private mode, so
/// provider call text can never steer where output lands. A failed spawn
/// closes and removes that empty log before returning.
/// The returned [`Proc`] owns the permits, capture task, and session log path.
pub(crate) fn spawn_process(
    argv: &[OsString],
    call: CallId,
    opts: SpawnOpts,
    approved: &Approved,
    preview_digest: Option<[u8; 32]>,
    jobs_dir: &Path,
    env_snapshot: &[(OsString, OsString)],
    launcher: &Launcher,
    lease: ProcessLease,
) -> Result<Proc, ToolError> {
    check_spawn_request(argv, &opts, approved, &call, preview_digest)?;
    let stdout_prefix_limit = opts.stdout_prefix_limit;
    let job = JobId::new_v7();
    // The provider call identity stays data: it keys progress and results
    // but never becomes a filesystem path. The durable log lives only under
    // the host-minted job identity inside the jobs directory.
    let log_path = jobs_dir.join(format!("{job}.log"));
    let (program, command_args, launcher_profile) = launcher_argv(argv, &job, launcher)?;
    if let Err(source) = fs::create_dir_all(jobs_dir) {
        return Err(ToolError::Failed(Box::new(io::Error::new(
            source.kind(),
            format!("process log dir {}: {source}", jobs_dir.display()),
        ))));
    }
    // Opened before the child exists: capture writes the same handle the
    // spawn door created, so no window lets output land on another name.
    let log_file = open_capture_log(&log_path).map_err(|source| {
        ToolError::Failed(Box::new(io::Error::new(
            source.kind(),
            format!("process log {}: {source}", log_path.display()),
        )))
    })?;
    let cwd = opts.cwd;
    let timeout = opts.timeout;
    let env_overrides = opts.env;
    let mut command = CommandWrap::with_new(&program, |process| {
        process.args(&command_args);
        process.current_dir(&cwd);
        process.env_clear();
        process.envs(env_snapshot.iter().map(|(key, value)| (key, value)));
        process.envs(env_overrides.iter().map(|(key, value)| (key, value)));
        process.env("NO_COLOR", "1");
        process.env("TERM", "dumb");
        process.env("PAGER", "cat");
        process.env("DAL_NESTED", "1");
        process.stdin(Stdio::null());
        process.stdout(Stdio::piped());
        process.stderr(Stdio::piped());
    });
    #[cfg(unix)]
    {
        command.wrap(ProcessGroup::leader());
        command.wrap(KillOnDrop);
    }
    #[cfg(windows)]
    windows::wrap(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(source) => {
            // The exclusive log has no reader yet: a failed spawn must not
            // leave empty residue at the job-owned name.
            drop(log_file);
            let _ = fs::remove_file(&log_path);
            return Err(ToolError::Spawn {
                path: PathBuf::from(&program),
                source,
            });
        }
    };
    let leader_pid = child.id();
    let stdout = child.stdout().take().ok_or_else(|| {
        ToolError::Failed(Box::new(io::Error::other(
            "child stdout pipe is unavailable",
        )))
    })?;
    let stderr = child.stderr().take().ok_or_else(|| {
        ToolError::Failed(Box::new(io::Error::other(
            "child stderr pipe is unavailable",
        )))
    })?;

    let (tail_tx, tail_rx) = tokio::sync::watch::channel(VecDeque::with_capacity(TAIL_RING_BYTES));
    let mut capture = JoinSet::new();
    capture.spawn(run_capture(
        stdout,
        stderr,
        tokio::fs::File::from(log_file),
        stdout_prefix_limit,
        tail_tx,
    ));

    #[cfg(windows)]
    super::LIVE_PROCS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(Proc {
        child,
        call,
        job,
        log_path,
        launcher_profile,
        lease: Some(lease),
        capture: Some(capture),
        live_tail: tail_rx,
        progress: None,
        progress_last: String::new(),
        leader_pid,
        timeout,
        started_at: Instant::now(),
        result: None,
        #[cfg(all(test, unix))]
        test_ordering: None,
    })
}

/// Opens the durable capture log as a brand-new owner-private file.
///
/// `create_new` refuses every existing directory entry, regular file or
/// symlink alike, so a preplanted name can neither be truncated nor
/// followed outside the jobs directory; Unix also pins mode 0600.
/// Fails closed: there is no fallback name.
fn open_capture_log(path: &Path) -> Result<fs::File, io::Error> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

/// Reports whether the canonical `cwd` sits below one canonical root.
///
/// Both sides canonicalize; a missing path fails closed. Shared with the
/// `CallGrant` matcher in `dispatch.rs`, whose argv-token and job-liveness
/// rules stay its own.
pub(crate) fn cwd_in_roots(cwd: &Path, roots: &[PathBuf]) -> bool {
    let Ok(canonical_cwd) = cwd.canonicalize() else {
        return false;
    };
    roots.iter().any(|root| {
        root.canonicalize()
            .is_ok_and(|canonical_root| canonical_cwd.starts_with(canonical_root))
    })
}
fn launcher_argv(
    argv: &[OsString],
    job: &JobId,
    launcher: &Launcher,
) -> Result<(OsString, Vec<OsString>, Option<PathBuf>), ToolError> {
    let target = argv[0].clone();
    let target_args = &argv[1..];
    match launcher {
        Launcher::Direct => {
            #[cfg(target_os = "macos")]
            {
                Ok(sandbox::lifetime_argv(&target, target_args))
            }
            #[cfg(not(target_os = "macos"))]
            {
                Ok((target, target_args.to_vec(), None))
            }
        }
        Launcher::Sandbox { helper, roots } => {
            sandbox::sandbox_argv(&target, target_args, job, helper.as_deref(), roots)
        }
    }
}

#[cfg(test)]
#[cfg_attr(
    windows,
    expect(dead_code, reason = "only the unix-gated spawn tests read log tails")
)]
pub(crate) fn tail_preview(log_path: &Path, max_bytes: usize) -> Result<Box<[u8]>, ToolError> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = fs::File::open(log_path).map_err(|source| ToolError::Spawn {
        path: log_path.to_path_buf(),
        source,
    })?;
    let length = file
        .seek(SeekFrom::End(0))
        .map_err(|source| ToolError::Spawn {
            path: log_path.to_path_buf(),
            source,
        })?;
    let max = u64::try_from(max_bytes).unwrap_or(u64::MAX);
    let start = length.saturating_sub(max);
    file.seek(SeekFrom::Start(start))
        .map_err(|source| ToolError::Spawn {
            path: log_path.to_path_buf(),
            source,
        })?;
    let remaining = usize::try_from(length - start).unwrap_or(usize::MAX);
    let mut bytes = Vec::with_capacity(remaining.min(max_bytes));
    file.read_to_end(&mut bytes)
        .map_err(|source| ToolError::Spawn {
            path: log_path.to_path_buf(),
            source,
        })?;
    Ok(bytes.into_boxed_slice())
}

/// Waits for the leader exit without hanging on surviving descendants.
///
/// Unix observes the native leader directly. Once it exits, the caller
/// synchronously consumes Drop signal authority and sweeps the group before
/// the retained outer wrapper wait for group-zombie reaping. On Windows,
/// job-object wait blocks until the whole job empties, so the leader
/// is detected with `try_wait` and the job is terminated at once instead.
async fn await_leader(
    child: &mut Box<dyn ChildWrapper>,
) -> Result<std::process::ExitStatus, ToolError> {
    #[cfg(windows)]
    {
        stop::wait_leader_exit(child).await
    }
    #[cfg(unix)]
    {
        child
            .inner_mut()
            .wait()
            .await
            .map_err(|error| process_failure(&error))
    }
    #[cfg(not(any(unix, windows)))]
    {
        child.wait().await.map_err(|error| process_failure(&error))
    }
}

/// Unix native-leader wait used for grace and hard-kill observation.
///
/// Signals stay on the outer group wrapper; only observation moves inward so
/// `Drop` authority is consumed before the retained outer reaping wait.
fn leader_wait(
    child: &mut Box<dyn ChildWrapper>,
) -> Pin<Box<dyn Future<Output = io::Result<std::process::ExitStatus>> + Send + '_>> {
    #[cfg(unix)]
    {
        child.inner_mut().wait()
    }
    #[cfg(not(unix))]
    {
        child.wait()
    }
}

/// Unix native-leader poll used for the stop fast path.
///
/// Observes only the leader so an already-exited leader still sweeps and
/// reaps through the retained outer wait instead of reusing the outer
/// wrapper's cached group status across its suspension window.
fn leader_try_wait(
    child: &mut Box<dyn ChildWrapper>,
) -> io::Result<Option<std::process::ExitStatus>> {
    #[cfg(unix)]
    {
        child.inner_mut().try_wait()
    }
    #[cfg(not(unix))]
    {
        child.try_wait()
    }
}
