// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Windows half of the keypress probe: it drives the private
//! `dalgona-pty-host` helper process and measures write-to-paint latencies.
//!
//! The parent never touches a console API and never holds an unbounded
//! wait: the helper runs inside a job object that terminates the whole
//! process tree on drop, every exchange is a bounded frame, and the parent
//! reaps the helper through bounded `try_wait` polling. Latencies are
//! helper-side stamps, so the two pipe hops stay out of the measured
//! budget, and each sample's glyph must be rendered by the application
//! inside a completed VT paint batch, a structural parse of the
//! post-keypress stream, rather than arriving as bare console echo.
//!
//! The guard is the same discrimination the Unix probe gets from the
//! kernel echo flag: a glyph counts only when the renderer addressed the
//! cell after the keypress and completed the batch with an erase, or when
//! it sits in the renderer's synchronized-update envelope. Stale escape
//! state from earlier frames never promotes echo; the paint-witness unit
//! tests pin the verdicts for the echo counterexamples.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

use process_wrap::tokio::{ChildWrapper, CommandWrap, JobObject, KillOnDrop};
use sonic_rs::JsonValueTrait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{ChildStderr, ChildStdin, ChildStdout};
use tokio::time::{sleep, timeout};

use super::frames::{FrameReader, Parsed};
use super::paint;
use super::protocol::{self, Request, Response, SpawnRequest};
use crate::support::TestResult;

/// Columns of the pseudo-console the child runs in.
const COLUMNS: u16 = 120;
/// Rows of the pseudo-console the child runs in.
const ROWS: u16 = 40;
/// First code point of the sample glyph range (CJK Unified Ideographs).
const GLYPH_BASE: u32 = 0x4E00;
/// Last code point of the sample glyph range.
const GLYPH_LAST: u32 = 0x9FFF;
/// Cap on the rolling match window retained between reads.
const WINDOW_CAP: usize = 64 * 1024;
/// Bytes kept across window trims so a glyph split by a trim still matches.
const SPLIT_KEEP: usize = 3;
/// Bound on every teardown step and on the reap of the helper.
const REAP_LIMIT: Duration = Duration::from_secs(5);
/// Poll interval while reaping the helper.
const REAP_POLL: Duration = Duration::from_millis(10);
/// Upper bound on the one-per-process build of the helper binary.
const HELPER_BUILD_LIMIT: Duration = Duration::from_mins(15);
/// Windows variables a console child needs before any timing starts.
const BASE_ENV: [&str; 6] = [
    "SystemRoot",
    "TEMP",
    "COMSPEC",
    "PSModulePath",
    "PATH",
    "USERPROFILE",
];

/// Sample count and wait budgets for one probe run.
#[derive(Clone, Copy)]
struct Tuning {
    /// Distinct glyphs to feed and measure.
    samples: usize,
    /// Quiet time that separates phases of child output.
    settle: Duration,
    /// Deadline for one exchange step and one glyph's appearance.
    limit: Duration,
}

/// Bounded rolling window of recent console output used for glyph matching.
#[derive(Default)]
struct Rolling {
    /// Output bytes, oldest first, trimmed to [`WINDOW_CAP`].
    bytes: Vec<u8>,
}

impl Rolling {
    /// Appends a chunk and trims the window to [`WINDOW_CAP`], always
    /// keeping the last [`SPLIT_KEEP`] bytes so a glyph split by a trim
    /// still matches.
    fn absorb(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
        let excess = self.bytes.len().saturating_sub(WINDOW_CAP);
        if excess > 0 {
            let droppable = self.bytes.len().saturating_sub(SPLIT_KEEP);
            self.bytes.drain(..excess.min(droppable));
        }
    }

    /// Discards everything absorbed so far. Called between samples, so the
    /// window holds only the post-keypress stream and the paint classifier
    /// can only ever see renderer work done after this sample's keypress.
    fn clear(&mut self) {
        self.bytes.clear();
    }
}

/// One absorbed frame's relevance to the running probe.
enum Kind {
    /// Console output; the value is the helper's read stamp.
    Out(u64),
    /// The write ack; the value is the helper's pre-write stamp.
    Wrote(u64),
    /// The child tree exited; the value is its exit code.
    Ended(u32),
}

/// Owns the helper process on every path through the probe.
///
/// `Drop` terminates the helper's whole job object (helper, console host,
/// and TUI tree) immediately and without waiting, so cancelling the probe
/// future is itself the cancellation path. Only an observed reap disarms
/// the drop, and a disarmed guard never signals a job that was already
/// verified empty, so no post-reap kill can land anywhere.
struct OwnerGuard {
    /// The helper child, carrying the job-object and kill-on-drop wraps.
    child: Box<dyn ChildWrapper>,
    /// Whether the drop path still terminates the job.
    armed: bool,
}

impl OwnerGuard {
    /// Marks the helper's exit as observed: the drop path no longer kills.
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for OwnerGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.child.start_kill();
        }
    }
}

/// Poison-on-first-failure owner for framed helper writes.
///
/// Mirrors `FrameReader`'s terminal refusal: a timed-out or failed
/// `write_all`/`flush` may have left a partial frame in the pipe, so a later
/// frame would append to those bytes and the helper would decode a phantom
/// request. The first write failure therefore poisons the sink; every later
/// framed write fails with the original reason without touching the pipe.
/// `teardown` observes the poison, skips the clean-shutdown exchange, and
/// takes the kill/reap path, so a poisoned stream is never reused.
struct FramedSink {
    /// The helper's stdin pipe; owned for the probe's write lifetime.
    stdin: ChildStdin,
    /// Terminal refusal: the first write failure or timeout, reused for
    /// every later framed write without touching the pipe.
    failed: Option<String>,
}

impl FramedSink {
    /// Owns `stdin` for the probe's write lifetime, unpoisoned.
    fn new(stdin: ChildStdin) -> Self {
        Self {
            stdin,
            failed: None,
        }
    }

    /// The poison reason of a prior framed write, when one poisoned the stream.
    fn poison_reason(&self) -> Option<&str> {
        self.failed.as_deref()
    }
}

/// The helper's owned pipes after a successful spawn.
///
/// The guard owns the job; the sink, reader, and stderr handle are taken
/// exactly once here so every later acquisition failure still settles the
/// job through `fail_early` instead of racing a drop kill.
struct HelperPipes {
    /// Framed stdin writes to the helper.
    sink: FramedSink,
    /// Framed stdout reads from the helper.
    frames: FrameReader<ChildStdout>,
    /// Fatal-path diagnostics the helper writes on stderr.
    stderr: ChildStderr,
    /// The helper process id used in every diagnostic.
    pid: u32,
}

/// Spawns `helper` under its job object and takes its pipes exactly once.
///
/// The guard owns the helper from the first instruction after spawn, so
/// every later acquisition failure still settles the job with an observed
/// reap instead of only a drop kill.
///
/// # Errors
/// Returns spawn and pipe-acquisition failures after settling the job; the
/// error text names the missing capability, never an unchecked `expect`.
async fn open_helper(helper: &Path) -> TestResult<(OwnerGuard, HelperPipes)> {
    let mut command = CommandWrap::with_new(helper, |process| {
        process.stdin(Stdio::piped());
        process.stdout(Stdio::piped());
        process.stderr(Stdio::piped());
    });
    command.wrap(JobObject);
    command.wrap(KillOnDrop);
    let mut guard = OwnerGuard {
        child: command
            .spawn()
            .map_err(|error| format!("spawning the {} helper failed: {error}", helper.display()))?,
        armed: true,
    };
    let Some(pid) = guard.child.id() else {
        return fail_early(
            &mut guard,
            helper,
            None,
            format!("the {} helper left no process id to own", helper.display()),
        )
        .await;
    };
    let Some(stdin) = guard.child.stdin().take() else {
        return fail_early(
            &mut guard,
            helper,
            None,
            format!("the {} helper has no stdin pipe", helper.display()),
        )
        .await;
    };
    let Some(stdout) = guard.child.stdout().take() else {
        return fail_early(
            &mut guard,
            helper,
            None,
            format!("the {} helper has no stdout pipe", helper.display()),
        )
        .await;
    };
    let Some(stderr) = guard.child.stderr().take() else {
        return fail_early(
            &mut guard,
            helper,
            None,
            format!("the {} helper has no stderr pipe", helper.display()),
        )
        .await;
    };
    Ok((
        guard,
        HelperPipes {
            sink: FramedSink::new(stdin),
            frames: FrameReader::new(stdout),
            stderr,
            pid,
        },
    ))
}

/// Starts the helper under a kill-on-close job object, runs the bounded
/// sample loop, and reaps the helper before returning.
///
/// The child runs in a 120 by 40 pseudo-console that the helper creates
/// with the system `ConPTY` backend. Kernel echo cannot be inspected through
/// safe API on Windows, so every sample's glyph must arrive behind real VT
/// sequence context rather than as bare console echo. Latencies are
/// helper-side stamps, so no cross-process clock comparison takes place.
/// The caller runs this under a tokio runtime with the I/O and time
/// drivers enabled; the pipes are driven asynchronously, so no extra
/// thread or task exists anywhere in the probe.
///
/// # Errors
/// Returns an error when `samples` is zero, the helper binary cannot be
/// resolved or built, the helper or the child misbehaves, any bounded
/// exchange overruns, a glyph arrives as bare echo or not at all, or the
/// teardown cannot be observed. Teardown errors name the cleanup as failed
/// or unverified, so a caller can never claim a reaped helper, a dead
/// child tree, or a returned latency while cleanup failed.
pub async fn keypress_echo_latencies(
    binary: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    cwd: &Path,
    samples: usize,
    settle: Duration,
    limit: Duration,
) -> TestResult<Vec<Duration>> {
    if samples == 0 {
        return Err(
            "the keypress probe needs a positive sample count; the stress caller \
             requires 100"
                .into(),
        );
    }
    let helper = helper_binary().await?;
    let env_pairs = captured_env(env)?;
    let spawn = Request::Spawn(SpawnRequest {
        program: path_string(binary, "the TUI binary")?,
        args: args.iter().map(|argument| (*argument).to_owned()).collect(),
        env: env_pairs,
        cwd: path_string(cwd, "the workspace directory")?,
        cols: COLUMNS,
        rows: ROWS,
    });
    let (mut guard, mut pipes) = open_helper(&helper).await?;
    let helper_pid = pipes.pid;
    let HelperPipes {
        ref mut sink,
        ref mut frames,
        ref mut stderr,
        ..
    } = pipes;

    if let Err(error) = write_request(sink, &spawn, limit).await {
        return fail_early(&mut guard, &helper, Some(stderr), error.to_string()).await;
    }
    let ack = match frames.read_parsed(limit).await {
        Ok(Parsed::Frame(response)) => match response {
            Response::Spawned { pid } => Ok(pid),
            other => Err(format!(
                "the {} helper (pid {helper_pid}) answered the launch with {}, expected Spawned",
                helper.display(),
                protocol::tag_name(other.tag())
            )),
        },
        Ok(Parsed::Quiet) => Err(format!(
            "the {} helper (pid {helper_pid}) did not acknowledge the launch within \
                 {limit:?}; its job is terminated on return",
            helper.display()
        )),
        Ok(Parsed::Closed) => Err(format!(
            "the {} helper (pid {helper_pid}) closed its pipes before acknowledging the launch",
            helper.display()
        )),
        Err(error) => Err(format!(
            "reading the launch ack from the {} helper (pid {helper_pid}) failed: {error}",
            helper.display()
        )),
    };
    if let Err(error) = ack {
        return fail_early(&mut guard, &helper, Some(stderr), error).await;
    }

    let mut window = Rolling::default();
    let tuning = Tuning {
        samples,
        settle,
        limit,
    };
    let outcome = run_samples(sink, frames, &mut window, binary, tuning).await;
    // The probe error takes precedence: teardown still settles the job and
    // drains diagnostics on every path, and its notes append to (never mask)
    // the probe failure.
    match (
        outcome,
        teardown(sink, frames, &mut guard, stderr, &helper).await,
    ) {
        (Ok(latencies), Ok(())) => Ok(latencies),
        (Err(probe), Ok(())) => Err(probe),
        (Ok(_), Err(cleanup)) => Err(cleanup),
        (Err(probe), Err(cleanup)) => Err(format!("{probe}; teardown: {cleanup}").into()),
    }
}

/// Runs the sample loop: startup settle, one bounded resize exercise, then one
/// unique glyph per sample with write-to-paint measurement.
async fn run_samples(
    sink: &mut FramedSink,
    frames: &mut FrameReader<ChildStdout>,
    window: &mut Rolling,
    binary: &Path,
    tuning: Tuning,
) -> TestResult<Vec<Duration>> {
    // Exercise the resize path once at startup. The helper applies it in its
    // frame loop and sends no ack for it: no frame acknowledges the resize,
    // and the requested constants prove nothing by themselves. Startup then
    // drains helper output to quiet within `limit`: `Quiet` only proves the
    // pipes went idle, an ended console fails startup, and a helper failure
    // propagates. There is no first-output requirement here; a silent child
    // fails at the first sample, where `await_paint` needs a painted glyph
    // within the limit. A hung startup surfaces as the startup deadline,
    // never as an unbounded wait.
    write_request(
        sink,
        &Request::Resize {
            cols: COLUMNS,
            rows: ROWS,
        },
        REAP_LIMIT,
    )
    .await?;
    let startup_deadline = Instant::now() + tuning.limit;
    if drain_until_quiet(frames, window, tuning.settle, startup_deadline).await? {
        return Err(format!(
            "the child tree ended during startup: the helper's console closed before \
             the first sample of {}",
            binary.display()
        )
        .into());
    }

    let mut latencies = Vec::new();
    for index in 0..tuning.samples {
        let deadline = Instant::now() + tuning.limit;
        let closed = drain_until_quiet(frames, window, tuning.settle, deadline).await?;
        if closed {
            return Err(format!("the child closed its console before sample {index}").into());
        }
        window.clear();
        let glyph = glyph_for(index)?;
        let mut encoded = [0_u8; 4];
        let needle = glyph.encode_utf8(&mut encoded).as_bytes().to_owned();
        write_request(
            sink,
            &Request::Write {
                bytes: needle.clone(),
            },
            tuning.limit,
        )
        .await?;
        latencies.push(await_paint(frames, window, &needle, glyph, tuning.limit).await?);
    }
    Ok(latencies)
}

/// Absorbs helper output until `settle` passes with nothing new or until
/// `deadline`; returns whether the console ended meanwhile.
async fn drain_until_quiet(
    frames: &mut FrameReader<ChildStdout>,
    window: &mut Rolling,
    settle: Duration,
    deadline: Instant,
) -> TestResult<bool> {
    loop {
        let wait = settle.min(deadline.saturating_duration_since(Instant::now()));
        match frames.read_parsed(wait).await? {
            Parsed::Frame(frame) => match classify(window, frame)? {
                Kind::Out(_) | Kind::Wrote(_) => {
                    if Instant::now() >= deadline {
                        return Ok(false);
                    }
                }
                Kind::Ended(_) => return Ok(true),
            },
            Parsed::Quiet => return Ok(false),
            Parsed::Closed => return Ok(true),
        }
    }
}

/// Waits for `glyph` to be rendered by the application, bounded by
/// `limit`, and returns the write-to-paint latency from helper-side
/// stamps.
///
/// The post-keypress stream is parsed structurally: an unaddressed
/// occurrence is console echo and fails the sample outright, and only an
/// occurrence inside a completed paint batch (erase in the addressed
/// region, or the synchronized-update envelope) counts. The write ack and
/// the output frames arrive from two helper threads, so either order is
/// valid; the latency computes once both are in hand.
async fn await_paint(
    frames: &mut FrameReader<ChildStdout>,
    window: &mut Rolling,
    needle: &[u8],
    glyph: char,
    limit: Duration,
) -> TestResult<Duration> {
    let deadline = Instant::now() + limit;
    let mut wrote_ns: Option<u64> = None;
    let mut painted_ns: Option<u64> = None;
    loop {
        if Instant::now() >= deadline {
            return Err(format!(
                "glyph {glyph} (U+{:04X}) did not paint within the limit",
                u32::from(glyph)
            )
            .into());
        }
        let wait = deadline.saturating_duration_since(Instant::now());
        match frames.read_parsed(wait).await? {
            Parsed::Frame(frame) => match classify(window, frame)? {
                Kind::Wrote(t_ns) => {
                    wrote_ns = Some(t_ns);
                    if let Some(painted) = painted_ns {
                        return Ok(Duration::from_nanos(painted.saturating_sub(t_ns)));
                    }
                }
                Kind::Out(t_ns) => {
                    if paint::contains_unaddressed(&window.bytes, needle) {
                        return Err(format!(
                            "glyph {glyph} (U+{:04X}) appeared as unaddressed console echo: \
                             the post-keypress stream carries no renderer cursor addressing \
                             behind it, and bare echo is never a valid sample",
                            u32::from(glyph)
                        )
                        .into());
                    }
                    if paint::contains_painted(&window.bytes, needle) {
                        painted_ns = Some(t_ns);
                        if let Some(wrote) = wrote_ns {
                            return Ok(Duration::from_nanos(t_ns.saturating_sub(wrote)));
                        }
                    }
                }
                Kind::Ended(code) => {
                    return Err(format!(
                        "glyph {glyph} (U+{:04X}) did not paint: the child tree exited with \
                         code {code:#010x}",
                        u32::from(glyph)
                    )
                    .into());
                }
            },
            Parsed::Quiet => {}
            Parsed::Closed => {
                return Err(format!(
                    "glyph {glyph} (U+{:04X}) did not paint: the console closed",
                    u32::from(glyph)
                )
                .into());
            }
        }
    }
}

/// Absorbs one helper frame and classifies it for the running probe.
///
/// # Errors
/// Returns a typed error for helper failures and protocol violations.
fn classify(window: &mut Rolling, frame: Response) -> TestResult<Kind> {
    match frame {
        Response::Out { t_ns, bytes } => {
            window.absorb(&bytes);
            Ok(Kind::Out(t_ns))
        }
        Response::Wrote { t_ns } => Ok(Kind::Wrote(t_ns)),
        Response::Exited { code } => Ok(Kind::Ended(code)),
        Response::Failed { reason } => Err(format!("the helper failed mid-probe: {reason}").into()),
        Response::Spawned { pid } => Err(format!(
            "protocol violation: the helper sent a second launch ack for pid {pid}"
        )
        .into()),
        Response::Final { unjoined } => Err(format!(
            "protocol violation: the helper ended early, reporting unjoined threads \
             {unjoined:?}"
        )
        .into()),
    }
}

/// Encodes one request frame and writes it, bounded by `bound`.
///
/// A timed-out or failed write may have left a partial frame in the pipe, so
/// it poisons `sink`: every later framed write fails with the original reason
/// without touching the pipe. An encode failure touches no bytes and leaves
/// the sink unpoisoned. A poisoned sink fails fast with the original reason,
/// so `teardown` skips the shutdown exchange and takes the kill/reap path
/// instead of reusing the stream.
///
/// # Errors
/// Returns encode-bound violations and write failures, names the bound when
/// the write overruns, and refuses poisoned streams with the original reason.
async fn write_request(
    sink: &mut FramedSink,
    request: &Request,
    bound: Duration,
) -> TestResult<()> {
    if let Some(reason) = sink.failed.clone() {
        return Err(format!(
            "refusing to write the {} frame: the stream is poisoned by an earlier write failure ({reason})",
            protocol::tag_name(request.tag())
        )
        .into());
    }
    let mut frame = Vec::new();
    request.encode(&mut frame).map_err(|error| {
        format!(
            "encoding the {} frame failed: {error}",
            protocol::tag_name(request.tag())
        )
    })?;
    let tag = protocol::tag_name(request.tag());
    let write = async {
        sink.stdin.write_all(&frame).await?;
        sink.stdin.flush().await
    };
    match timeout(bound, write).await {
        Err(_) => {
            let reason = format!("writing the {tag} frame overran {bound:?}");
            sink.failed = Some(reason.clone());
            Err(reason.into())
        }
        Ok(Err(error)) => {
            let reason = format!("writing the {tag} frame failed: {error}");
            sink.failed = Some(reason.clone());
            Err(reason.into())
        }
        Ok(Ok(())) => Ok(()),
    }
}

/// Shuts the helper down, settles its whole job tree, and verifies the
/// helper's own exit status.
///
/// Every exit path observes the reap within a bound and drains standard
/// error: a dead pipe during the shutdown exchange can no longer skip the
/// reap or lose diagnostics. The clean-shutdown exchange runs first and its
/// note is kept; reap, exit-status, and standard-error notes append to it.
/// A poisoned write stream skips the exchange instead: a partial frame may
/// already sit in the pipe, so the job settles through the kill/reap path
/// below without reusing the stream. The tree is terminated only when a reap
/// round times out, never before the first reap, so a clean exit that already
/// emitted `Final` is never raced by our own kill. An empty note list is the
/// only success.
///
/// # Errors
/// Returns the combined shutdown, reap, exit-status, and standard-error
/// notes; a missing `Final` report or a non-empty unjoined list is a
/// failure, never a silent pass.
async fn teardown(
    sink: &mut FramedSink,
    frames: &mut FrameReader<ChildStdout>,
    guard: &mut OwnerGuard,
    stderr: &mut ChildStderr,
    helper: &Path,
) -> TestResult<()> {
    let mut notes: Vec<String> = Vec::new();
    // A poisoned stream never reuses the pipe: a timed-out or failed write
    // may have left a partial frame behind, so the clean-shutdown exchange
    // is skipped and the job settles through the kill/reap path below. The
    // normal path still runs the full shutdown exchange first; its failure
    // is a note, and the job settles below regardless of pipe state.
    if let Some(reason) = sink.poison_reason() {
        notes.push(format!(
            "clean shutdown skipped for the {} helper: framed writes are poisoned ({reason}); its job is terminated instead",
            helper.display()
        ));
    } else if let Err(note) = shutdown_exchange(sink, frames, helper).await {
        notes.push(note);
    }
    // Always settle the job: first observe a normal exit within a bound, and
    // only terminate the whole tree (helper, console host, and TUI tree)
    // when the reap times out. Every kill below is idempotent: a helper that
    // already exited reaps on the first poll with no kill needed.
    let mut status = None;
    for _ in 0..2 {
        match reap_within(guard, helper, Instant::now() + REAP_LIMIT).await {
            Ok(Some(exit)) => {
                status = Some(exit);
                break;
            }
            Ok(None) => {
                let _ = guard.child.start_kill();
            }
            Err(note) => {
                notes.push(note);
                let _ = guard.child.start_kill();
            }
        }
    }
    if status.is_none() {
        notes.push(format!(
            "cleanup failed for the {} helper: still alive after two bounded kills of its \
             whole job; the reap was not observed",
            helper.display()
        ));
    }
    if let Err(note) = check_reap_notes(guard, helper, status) {
        notes.push(note);
    }
    // Always drain standard error: the helper writes it only on its fatal
    // exit path, so on the normal teardown this returns empty immediately
    // after the exit.
    match drain_bounded(stderr).await {
        Ok(diagnostic) if diagnostic.is_empty() => {}
        Ok(diagnostic) => notes.push(format!(
            "the {} helper wrote on standard error while shutting down: {diagnostic}",
            helper.display()
        )),
        Err(error) => notes.push(format!(
            "draining the {} helper standard error failed: {error}",
            helper.display()
        )),
    }
    if notes.is_empty() {
        Ok(())
    } else {
        Err(notes.join("; ").into())
    }
}

/// Records the reap outcome: a non-success exit is a note, and only an
/// observed successful reap disarms the guard.
///
/// The guard stays armed otherwise, so its drop remains the backstop.
fn check_reap_notes(
    guard: &mut OwnerGuard,
    helper: &Path,
    status: Option<ExitStatus>,
) -> Result<(), String> {
    match status {
        Some(exit) if exit.success() => {
            guard.disarm();
            Ok(())
        }
        Some(exit) => Err(format!(
            "the {} helper exited with {exit} after the shutdown request",
            helper.display()
        )),
        None => Ok(()),
    }
}

/// Asks the helper for a clean shutdown and verifies its `Final` report.
///
/// Runs inside [`teardown`], which settles the job and drains diagnostics
/// even when this exchange fails. Returns the first problem as a note.
///
/// # Errors
/// Returns shutdown-write, drain, missing-`Final`, and unjoined-thread notes.
async fn shutdown_exchange(
    sink: &mut FramedSink,
    frames: &mut FrameReader<ChildStdout>,
    helper: &Path,
) -> Result<(), String> {
    write_request(sink, &Request::Shutdown, REAP_LIMIT)
        .await
        .map_err(|error| error.to_string())?;
    let deadline = Instant::now() + REAP_LIMIT;
    let mut final_report = None;
    while Instant::now() < deadline {
        let wait = deadline.saturating_duration_since(Instant::now());
        match frames.read_parsed(wait).await? {
            Parsed::Frame(Response::Final { unjoined }) => {
                final_report = Some(unjoined);
                break;
            }
            Parsed::Frame(frame) => {
                if let Response::Failed { reason } = frame {
                    return Err(format!("the helper failed during teardown: {reason}"));
                }
            }
            Parsed::Quiet => {}
            Parsed::Closed => break,
        }
    }
    let unjoined = final_report.ok_or_else(|| {
        format!(
            "the {} helper closed its pipes without a Final report",
            helper.display()
        )
    })?;
    if unjoined.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "the helper reported threads that missed their join deadline: {unjoined:?}"
        ))
    }
}

/// Kills the helper job and observes its reap within a bound, without touching
/// the pipes, so launch failures on dead or silent pipes still settle the job.
///
/// Only an observed reap disarms the guard; otherwise the guard stays armed
/// and its drop remains the backstop. Returns `None` when the job is settled
/// and a cleanup note when the reap went unobserved.
async fn settle_without_pipes(guard: &mut OwnerGuard, helper: &Path) -> Option<String> {
    let _ = guard.child.start_kill();
    let deadline = Instant::now() + REAP_LIMIT;
    while Instant::now() < deadline {
        match guard.child.try_wait() {
            Ok(Some(_)) => {
                guard.disarm();
                return None;
            }
            Ok(None) => sleep(REAP_POLL).await,
            Err(error) => {
                return Some(format!(
                    "reaping the {} helper failed: {error}; whether it is gone is unverified",
                    helper.display()
                ));
            }
        }
    }
    Some(format!(
        "the {} helper was still alive after its bounded early kill; the reap was not observed",
        helper.display()
    ))
}

/// Returns an early launch failure after settling the helper job without pipe
/// writes, so a dead or silent helper never leaves its job tree behind.
///
/// Fatal standard-error diagnostics are preserved when a pipe is available:
/// they drain only after an observed reap, when the pipes are closed and the
/// read returns immediately instead of waiting out the bound against a
/// live-but-silent helper. Settlement trouble never masks the launch cause:
/// it is appended, and only an observed reap disarms the guard.
async fn fail_early<T>(
    guard: &mut OwnerGuard,
    helper: &Path,
    stderr: Option<&mut ChildStderr>,
    error: String,
) -> TestResult<T> {
    let settled = settle_without_pipes(guard, helper).await;
    let diagnostic = match (&settled, stderr) {
        (None, Some(pipe)) => drain_bounded(pipe).await.unwrap_or_default(),
        _ => String::new(),
    };
    let mut message = error;
    if !diagnostic.is_empty() {
        message = format!("{message}; helper stderr: {diagnostic}");
    }
    if let Some(note) = settled {
        message = format!("{message}; early cleanup: {note}");
    }
    Err(message.into())
}

/// Polls `try_wait` every [`REAP_POLL`] until the helper's exit is observed
/// or `deadline` passes. The completion-port queue drains one packet per
/// poll, and the kernel queue holds at most one packet per process event.
///
/// # Errors
/// Names the reap as failed and unverified when the poll errors.
async fn reap_within(
    guard: &mut OwnerGuard,
    helper: &Path,
    deadline: Instant,
) -> Result<Option<ExitStatus>, String> {
    while Instant::now() < deadline {
        match guard.child.try_wait() {
            Ok(Some(status)) => return Ok(Some(status)),
            Ok(None) => sleep(REAP_POLL).await,
            Err(error) => {
                return Err(format!(
                    "reaping the {} helper failed: {error}; whether it is gone is unverified",
                    helper.display()
                ));
            }
        }
    }
    Ok(None)
}

/// Drains at most 8 KiB from `stderr` with a bounded wait.
///
/// The helper writes standard error only on its fatal exit path, so on the
/// normal teardown this returns empty immediately after the exit; a read
/// error past a dead process is bounded diagnostics, not a cleanup failure.
async fn drain_bounded(stderr: &mut ChildStderr) -> TestResult<String> {
    let mut total = Vec::new();
    let mut chunk = [0_u8; 2 * 1024];
    let deadline = Instant::now() + REAP_LIMIT;
    loop {
        let wait = deadline.saturating_duration_since(Instant::now());
        if wait.is_zero() {
            break;
        }
        match timeout(wait, stderr.read(&mut chunk)).await {
            Err(_elapsed) => break,
            Ok(Ok(0) | Err(_)) => break,
            Ok(Ok(read)) => {
                total.extend_from_slice(&chunk[..read]);
                if total.len() >= 8 * 1024 {
                    break;
                }
            }
        }
    }
    Ok(String::from_utf8_lossy(&total).trim().to_owned())
}

/// Captures the complete child environment: the caller's pairs first, then
/// the base Windows variables from this process, deduplicated with the
/// caller winning.
///
/// The Unix probe runs its child behind `env_clear` with four variables;
/// copying that here would fail the child before any timing starts, because
/// Windows console children need `SystemRoot`, `TEMP`, `COMSPEC`,
/// `PSModulePath`, `PATH`, and a profile directory. A missing base variable
/// is an error, never a silent degradation.
///
/// # Errors
/// Names the first missing base variable.
fn captured_env(explicit: &[(&str, &str)]) -> TestResult<Vec<(String, String)>> {
    let mut captured = Vec::with_capacity(explicit.len() + BASE_ENV.len());
    let mut seen = BTreeSet::new();
    for (key, value) in explicit {
        seen.insert((*key).to_owned());
        captured.push(((*key).to_owned(), (*value).to_owned()));
    }
    for (key, value) in std::env::vars_os() {
        let (Ok(key), Ok(value)) = (key.into_string(), value.into_string()) else {
            continue;
        };
        if seen.contains(&key) || !BASE_ENV.contains(&key.as_str()) {
            continue;
        }
        seen.insert(key.clone());
        captured.push((key, value));
    }
    for name in BASE_ENV {
        if !seen.contains(name) {
            return Err(format!(
                "the caller environment is missing {name}; the ConPTY child needs it \
                 before any timing can start"
            )
            .into());
        }
    }
    Ok(captured)
}

/// Resolves the private `dalgona-pty-host` helper binary, building it once
/// per test process when stale.
///
/// Resolution follows the same layout logic as the product binary resolver
/// in [`crate::support`]: the profile directory comes from this test
/// executable's location relative to Cargo's build directory, canonical
/// directories come from `cargo metadata`, and the helper lands beside the
/// product binary in that profile directory. There is no fallback path and
/// no sibling guess.
///
/// # Errors
/// Returns metadata, layout, build, or missing-binary failures.
async fn helper_binary() -> TestResult<PathBuf> {
    static HELPER: tokio::sync::OnceCell<Result<PathBuf, String>> =
        tokio::sync::OnceCell::const_new();
    let resolved = HELPER.get_or_init(build_helper).await;
    resolved.clone().map_err(Into::into)
}

/// The once-per-process resolution and build of the helper.
async fn build_helper() -> Result<PathBuf, String> {
    let (target_dir, build_dir) = cargo_directories().await?;
    let exe = std::env::current_exe()
        .map_err(|error| format!("locating this test executable failed: {error}"))?;
    let exe_dir = exe
        .parent()
        .ok_or_else(|| "this test executable has no parent directory".to_owned())?;
    // Cargo places test executables in `<profile>/deps/<test>`; a binary
    // placed directly in the profile directory is tolerated, mirroring the
    // product binary resolver in `crate::support`.
    let profile_dir = if exe_dir.file_name().is_some_and(|name| name == "deps") {
        exe_dir
            .parent()
            .ok_or_else(|| "this test executable has no profile directory".to_owned())?
    } else {
        exe_dir
    };
    let profile_dir = std::fs::canonicalize(profile_dir)
        .map_err(|error| format!("canonicalizing the test profile directory failed: {error}"))?;
    let build_dir = std::fs::canonicalize(&build_dir)
        .map_err(|error| format!("canonicalizing Cargo's build directory failed: {error}"))?;
    let relative = profile_dir.strip_prefix(&build_dir).map_err(|_| {
        format!(
            "the test binary is not under Cargo's build directory {}",
            build_dir.display()
        )
    })?;
    let mut components = relative.components();
    let profile = components
        .next_back()
        .and_then(|component| component.as_os_str().to_str())
        .ok_or_else(|| "the test profile directory has no name".to_owned())?;
    let triple = components
        .next_back()
        .and_then(|component| component.as_os_str().to_str());
    ensure_helper_built(profile, triple).await?;
    let binary = target_dir
        .join(relative)
        .join(format!("dalgona-pty-host{}", std::env::consts::EXE_SUFFIX));
    if binary.is_file() {
        return Ok(binary);
    }
    Err(format!(
        "the dalgona-pty-host helper is still missing at {} after its --locked --features test-support build",
        binary.display()
    ))
}

/// Runs `cargo metadata` for this workspace, so configured target and build
/// directories resolve without guessing.
async fn cargo_directories() -> Result<(PathBuf, PathBuf), String> {
    let stdout = run_cargo(
        &["metadata", "--no-deps", "--format-version=1", "--locked"],
        HELPER_BUILD_LIMIT,
    )
    .await?;
    let metadata: sonic_rs::Value = sonic_rs::from_slice(&stdout)
        .map_err(|error| format!("parsing cargo metadata failed: {error}"))?;
    let directory = |field: &str| {
        metadata
            .get(field)
            .and_then(sonic_rs::Value::as_str)
            .map(PathBuf::from)
            .ok_or_else(|| format!("cargo metadata omitted {field}"))
    };
    let target_directory = directory("target_directory")?;
    let build_directory = directory("build_directory")?;
    Ok((target_directory, build_directory))
}

/// Builds the helper once per test process, mirroring the product binary's
/// freshness discipline: cargo's own fingerprinting keeps a current build
/// cheap, and a stale artifact from an older tree can never satisfy the
/// check. The helper bin carries `required-features = ["test-support"]`,
/// so the build passes `--features test-support` explicitly; no
/// default-features change and no `cfg(windows)` skip.
async fn ensure_helper_built(profile: &str, triple: Option<&str>) -> Result<(), String> {
    let mut args = vec![
        "build".to_owned(),
        "-p".to_owned(),
        "dalgona-gates".to_owned(),
        "--bin".to_owned(),
        "dalgona-pty-host".to_owned(),
        "--locked".to_owned(),
        "--features".to_owned(),
        "test-support".to_owned(),
    ];
    match profile {
        "debug" => {}
        "release" => args.push("--release".to_owned()),
        custom => {
            args.push("--profile".to_owned());
            args.push(custom.to_owned());
        }
    }
    if let Some(triple) = triple {
        args.push("--target".to_owned());
        args.push(triple.to_owned());
    }
    let references: Vec<&str> = args.iter().map(String::as_str).collect();
    run_cargo(&references, HELPER_BUILD_LIMIT).await.map(|_| ())
}

/// Runs one cargo command to completion under `bound`, returning its
/// captured standard output.
///
/// The child carries no job-object wraps, so its wait is the plain tokio
/// wait; the bound plus `start_kill` turns a hung Cargo into a typed
/// failure instead of a stalled gate.
async fn run_cargo(args: &[&str], bound: Duration) -> Result<Vec<u8>, String> {
    let mut command = CommandWrap::with_new(env!("CARGO"), |process| {
        process.args(args);
        process.current_dir(env!("CARGO_MANIFEST_DIR"));
        process.stdin(Stdio::null());
        process.stdout(Stdio::piped());
        process.stderr(Stdio::piped());
    });
    let mut child = command
        .spawn()
        .map_err(|error| format!("spawning {args:?} failed: {error}"))?;
    let mut stdout = child
        .stdout()
        .take()
        .ok_or_else(|| format!("{args:?} produced no standard output pipe"))?;
    let mut stderr = child
        .stderr()
        .take()
        .ok_or_else(|| format!("{args:?} produced no standard error pipe"))?;
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let waited = timeout(bound, async {
        let (_, _, status) = tokio::join!(
            stdout.read_to_end(&mut output),
            stderr.read_to_end(&mut errors),
            child.wait()
        );
        status
    })
    .await;
    match waited {
        Err(_elapsed) => {
            let _ = child.start_kill();
            let deadline = Instant::now() + REAP_LIMIT;
            let mut reaped = false;
            let mut reap_note: Option<String> = None;
            while Instant::now() < deadline {
                match child.try_wait() {
                    Ok(Some(_)) => {
                        reaped = true;
                        break;
                    }
                    Ok(None) => sleep(REAP_POLL).await,
                    Err(error) => {
                        reap_note = Some(format!(
                            "reaping {args:?} failed: {error}; whether it is gone is unverified"
                        ));
                        break;
                    }
                }
            }
            let mut message = format!("{args:?} did not finish within {bound:?}");
            if let Some(note) = reap_note {
                message = format!("{message}; {note}");
            } else if !reaped {
                message = format!("{message}; the reap was not observed within {REAP_LIMIT:?}");
            }
            Err(message)
        }
        Ok(Err(error)) => Err(format!("waiting for {args:?} failed: {error}")),
        Ok(Ok(status)) if !status.success() => Err(format!(
            "{args:?} failed with {status}: {}",
            error_tail(&errors)
        )),
        Ok(Ok(_)) => Ok(output),
    }
}

/// The bounded tail of a cargo failure, for error messages.
fn error_tail(bytes: &[u8]) -> String {
    const TAIL_CHARS: usize = 1024;
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    let offset = trimmed.chars().count().saturating_sub(TAIL_CHARS);
    trimmed.chars().skip(offset).collect()
}

/// Losslessly renders `path` as UTF-8 for the Spawn frame.
///
/// # Errors
/// Names the path when it is not UTF-8.
fn path_string(path: &Path, label: &str) -> TestResult<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("{label} {} is not UTF-8", path.display()).into())
}

/// The `index`-th distinct sample glyph, drawn from the CJK Unified
/// Ideographs block.
///
/// # Errors
/// Returns an error when the probe ran out of distinct sample glyphs.
fn glyph_for(index: usize) -> TestResult<char> {
    u32::try_from(index)
        .ok()
        .and_then(|offset| GLYPH_BASE.checked_add(offset))
        .filter(|code| *code <= GLYPH_LAST)
        .and_then(char::from_u32)
        .ok_or_else(|| "the keypress probe ran out of distinct sample glyphs".into())
}
