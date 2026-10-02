use std::{
    collections::VecDeque,
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};

use dal_core::{CallId, JobId, JobOutcome, Workspace};
use process_wrap::tokio::{ChildWrapper, CommandWrap};
#[cfg(unix)]
use process_wrap::tokio::{KillOnDrop, ProcessGroup};
use tokio::{
    task::JoinSet,
    time::{self, Instant as TokioInstant},
};
use tokio_util::sync::CancellationToken;

use crate::{
    admission::FdPermit,
    error::{DenyReason, ToolError},
    ext::Approved,
};

mod capture;
pub(crate) mod sandbox;
mod stop;
#[cfg(test)]
mod tests;
#[cfg(windows)]
mod windows;

use capture::{CaptureResult, run_capture};
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

/// A process-progress callback owned by the session actor.
pub(crate) type ProgressFn = Arc<dyn Fn(Box<str>) + Send + Sync + 'static>;

type Permit = tokio::sync::OwnedSemaphorePermit;

type CaptureSet = JoinSet<Result<CaptureResult, io::Error>>;

/// One launched child and its bounded capture task.
pub struct Proc {
    child: Box<dyn ChildWrapper>,
    call: CallId,
    job: JobId,
    log_path: PathBuf,
    launcher_profile: Option<PathBuf>,
    process_permit: Option<Permit>,
    fd_permit: Option<FdPermit>,
    capture: Option<CaptureSet>,
    live_tail: tokio::sync::watch::Receiver<VecDeque<u8>>,
    leader_pid: Option<u32>,
    timeout: Option<Duration>,
    started_at: Instant,
    result: Option<ProcResult>,
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
    /// Waits for the child or applies its timeout/cancellation stop ladder.
    ///
    /// # Errors
    /// Returns an error if waiting for the child or draining captured output fails.
    pub async fn wait(&mut self, cancel: &CancellationToken) -> Result<ProcResult, ToolError> {
        if let Some(result) = &self.result {
            return Ok(result.clone());
        }

        let status = if let Some(timeout) = self.timeout {
            let deadline = TokioInstant::from_std(self.started_at + timeout);
            tokio::select! {
                biased;
                status = await_leader(&mut self.child) => {
                    process_exit_status(status?)
                }
                () = time::sleep_until(deadline) => {
                    return self.stop(StopReason::Timeout).await;
                }
                () = cancel.cancelled() => {
                    return self.stop(StopReason::Cancelled).await;
                }
            }
        } else {
            tokio::select! {
                biased;
                status = await_leader(&mut self.child) => {
                    process_exit_status(status?)
                }
                () = cancel.cancelled() => {
                    return self.stop(StopReason::Cancelled).await;
                }
            }
        };
        // A surviving descendant holding a pipe write end would stall
        // capture at EOF below; stop what the command left running first.
        self.sweep_after_exit().await;
        self.finish(status).await
    }

    /// Stops a live child using the platform group/job-object ladder.
    ///
    /// # Errors
    /// Returns an error if stopping the child or finalizing captured output fails.
    pub async fn stop(&mut self, reason: StopReason) -> Result<ProcResult, ToolError> {
        if let Some(result) = &self.result {
            return Ok(result.clone());
        }

        if let Some(status) = self
            .child
            .try_wait()
            .map_err(|error| process_failure(&error))?
        {
            self.sweep_after_exit().await;
            return self.finish(process_exit_status(status)).await;
        }

        // Snapshot the lineage before signaling: a `setsid` grandchild
        // escapes the process group, and reparents to init the instant its
        // bridge exits — the post-exit `/proc` sweep can no longer find it,
        // so the only reachable point for detached descendants is now.
        let doomed = self.recorded_descendants().await;
        soft_kill(&mut self.child)?;
        let status = if let Ok(waited) = time::timeout(KILL_GRACE, self.child.wait()).await {
            let _ = waited.map_err(|error| process_failure(&error))?;
            self.sweep_after_exit().await;
            status_for(reason)
        } else {
            hard_kill(&mut self.child)?;
            let _ = self
                .child
                .wait()
                .await
                .map_err(|error| process_failure(&error))?;
            self.sweep_after_exit().await;
            status_for(reason)
        };
        sweep_recorded(&doomed);
        self.finish(status).await
    }

    /// Records live descendants for the post-exit `setsid` sweep.
    #[cfg_attr(
        not(target_os = "linux"),
        expect(
            clippy::unused_async_trait_impl,
            reason = "the /proc walk await is linux-only"
        )
    )]
    async fn recorded_descendants(&mut self) -> Vec<u32> {
        #[cfg(target_os = "linux")]
        if let Some(pid) = self.leader_pid {
            return tokio::task::spawn_blocking(move || stop::proc_descendants(pid))
                .await
                .unwrap_or_default();
        }
        Vec::new()
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
        self.process_permit.take();
        self.fd_permit.take();
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

    #[cfg_attr(
        not(target_os = "linux"),
        expect(
            clippy::unused_async_trait_impl,
            reason = "the /proc walk await is linux-only"
        )
    )]
    async fn sweep_after_exit(&mut self) {
        #[cfg(windows)]
        {
            // TerminateJobObject at once: the leader already exited, so this
            // only reaps descendants still holding pipes open.
            let _ = self.child.start_kill();
            return;
        }
        let Some(pid) = self.leader_pid else {
            return;
        };
        sweep_process_group(pid);
        #[cfg(target_os = "linux")]
        {
            let _ = tokio::task::spawn_blocking(move || stop::sweep_proc_descendants(pid)).await;
        }
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        // JoinSet aborts its tasks on drop; no detached capture survives the session.
        self.capture.take();
        if let Some(profile) = self.launcher_profile.take() {
            let _ = fs::remove_file(profile);
        }
    }
}

/// Starts a checked process with ordinary output capture.
#[expect(
    clippy::too_many_arguments,
    reason = "plan-fixed spawn door carries preview digest, workspace, session jobs dir, env, launcher, and permit"
)]
pub(crate) fn spawn_process(
    argv: &[OsString],
    call: CallId,
    opts: SpawnOpts,
    approved: &Approved,
    preview_digest: Option<[u8; 32]>,
    workspace: &Workspace,
    jobs_dir: &Path,
    env_snapshot: &[(OsString, OsString)],
    launcher: &Launcher,
    process_permit: Permit,
    fd_permit: Option<FdPermit>,
) -> Result<Proc, ToolError> {
    // The durable witness lives under the session jobs directory (plan 3644),
    // never in the user project tree; the caller supplies the session dir.
    let log_path = jobs_dir.join(format!("{}.log", call.as_str()));
    spawn_process_with_capture(
        argv,
        call,
        JobId::new_v7(),
        opts,
        approved,
        preview_digest,
        workspace,
        env_snapshot,
        launcher,
        process_permit,
        fd_permit,
        log_path,
        0,
        None,
    )
}

/// Starts a checked process with an optional bounded stdout-prefix request.
#[expect(
    clippy::too_many_arguments,
    reason = "plan-fixed spawn signature carries preview digest, workspace, env, launcher, permit, log, prefix, and progress"
)]
pub(crate) fn spawn_process_with_capture(
    argv: &[OsString],
    call: CallId,
    job: JobId,
    opts: SpawnOpts,
    approved: &Approved,
    preview_digest: Option<[u8; 32]>,
    workspace: &Workspace,
    env_snapshot: &[(OsString, OsString)],
    launcher: &Launcher,
    process_permit: Permit,
    fd_permit: Option<FdPermit>,
    log_path: PathBuf,
    stdout_prefix_limit: usize,
    progress: Option<ProgressFn>,
) -> Result<Proc, ToolError> {
    if argv.is_empty() {
        return Err(ToolError::Spawn {
            path: PathBuf::new(),
            source: io::Error::new(io::ErrorKind::InvalidInput, "empty argv"),
        });
    }
    if stdout_prefix_limit > MAX_STDOUT_PREFIX_BYTES {
        return Err(ToolError::Failed(Box::new(io::Error::new(
            io::ErrorKind::InvalidInput,
            "stdout prefix limit exceeds 262145 bytes",
        ))));
    }
    if approved.call() != &call
        || approved.digest() != preview_digest
        || !cwd_in_roots(&opts.cwd, approved.roots())
    {
        return Err(ToolError::Denied(DenyReason::out_of_scope(format!(
            "call {}",
            call.as_str()
        ))));
    }
    launch(
        argv,
        call,
        job,
        opts,
        workspace,
        env_snapshot,
        launcher,
        process_permit,
        fd_permit,
        log_path,
        stdout_prefix_limit,
        progress,
    )
}

/// Testable launch core without the approval proof.
#[expect(
    clippy::too_many_arguments,
    reason = "launch carries the same plan-fixed spawn inputs for direct testing"
)]
fn launch(
    argv: &[OsString],
    call: CallId,
    job: JobId,
    opts: SpawnOpts,
    workspace: &Workspace,
    env_snapshot: &[(OsString, OsString)],
    launcher: &Launcher,
    process_permit: Permit,
    fd_permit: Option<FdPermit>,
    log_path: PathBuf,
    stdout_prefix_limit: usize,
    progress: Option<ProgressFn>,
) -> Result<Proc, ToolError> {
    // The workspace anchors future sandbox-root resolution; the child cwd stays explicit.
    let _ = workspace;
    if argv.is_empty() {
        return Err(ToolError::Spawn {
            path: PathBuf::new(),
            source: io::Error::new(io::ErrorKind::InvalidInput, "empty argv"),
        });
    }
    if stdout_prefix_limit > MAX_STDOUT_PREFIX_BYTES {
        return Err(ToolError::Failed(Box::new(io::Error::new(
            io::ErrorKind::InvalidInput,
            "stdout prefix limit exceeds 262145 bytes",
        ))));
    }
    let (program, command_args, launcher_profile) = launcher_argv(argv, &job, launcher)?;
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
    let mut child = command.spawn().map_err(|source| ToolError::Spawn {
        path: PathBuf::from(&program),
        source,
    })?;
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
        log_path.clone(),
        stdout_prefix_limit,
        tail_tx,
        progress,
    ));

    Ok(Proc {
        child,
        call,
        job,
        log_path,
        launcher_profile,
        process_permit: Some(process_permit),
        fd_permit,
        capture: Some(capture),
        live_tail: tail_rx,
        leader_pid,
        timeout,
        started_at: Instant::now(),
        result: None,
    })
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
        Launcher::Direct => Ok((target, target_args.to_vec(), None)),
        Launcher::Sandbox { helper, roots } => {
            sandbox::sandbox_argv(&target, target_args, job, helper.as_deref(), roots)
        }
    }
}

#[cfg(test)]
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

/// Renders the final `n` output lines from a tail snapshot.
#[must_use]
pub(crate) fn last_lines(bytes: &[u8], n: usize) -> String {
    let lines: Vec<&[u8]> = bytes.split(|byte| *byte == b'\n').collect();
    let start = lines.len().saturating_sub(n);
    lines[start..]
        .iter()
        .map(|line| String::from_utf8_lossy(line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Waits for the leader exit without hanging on surviving descendants.
///
/// Unix `child.wait` returns when the leader is reaped; the group sweep
/// after it stops pipe-holding descendants. On Windows the job-object wait
/// blocks until the whole job empties, so the leader is detected with
/// `try_wait` and the job is terminated at once instead.
async fn await_leader(
    child: &mut Box<dyn ChildWrapper>,
) -> Result<std::process::ExitStatus, ToolError> {
    #[cfg(windows)]
    {
        stop::wait_leader_exit(child).await
    }
    #[cfg(not(windows))]
    {
        child.wait().await.map_err(|error| process_failure(&error))
    }
}
