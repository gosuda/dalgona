use std::{ffi::OsString, path::PathBuf, time::Duration};

use dal_core::CallId;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

#[cfg(unix)]
use std::{os::unix::fs::PermissionsExt as _, path::Path};

#[cfg(unix)]
use super::{COMPLETION_TAIL_BYTES, PREVIEW_BYTES, tail_preview};
use super::{Launcher, ProcessLease, SpawnOpts, spawn_process};
use super::{MAX_STDOUT_PREFIX_BYTES, ProcStatus, StopReason};
use crate::error::{DenyReason, ToolError};
use crate::ext::tool::Approved;

#[cfg(unix)]
use process_wrap::tokio::ChildWrapper;
#[cfg(unix)]
use std::{
    future::Future,
    io,
    pin::Pin,
    process::ExitStatus,
    sync::{Arc, atomic::Ordering as AtomicOrdering},
};
#[cfg(unix)]
use tokio::process::{ChildStderr, ChildStdin, ChildStdout};
#[cfg(unix)]
use tokio::{sync::Notify, task::JoinSet};

async fn test_permit() -> tokio::sync::OwnedSemaphorePermit {
    static SEMAPHORE: std::sync::LazyLock<std::sync::Arc<Semaphore>> =
        std::sync::LazyLock::new(|| std::sync::Arc::new(Semaphore::new(256)));
    SEMAPHORE
        .clone()
        .acquire_owned()
        .await
        .expect("test permit")
}

fn shell_argv(command: &str) -> Vec<OsString> {
    vec![
        OsString::from("/bin/sh"),
        OsString::from("-c"),
        OsString::from(command),
    ]
}

#[cfg(windows)]
fn cmd_argv(script: &str) -> Vec<OsString> {
    vec![
        OsString::from("cmd.exe"),
        OsString::from("/D"),
        OsString::from("/C"),
        OsString::from(script),
    ]
}

/// A child that prints `text` first, then exits 0, on either platform.
#[cfg(unix)]
fn echo_argv(text: &str) -> Vec<OsString> {
    shell_argv(&format!("printf '{text}'"))
}

#[cfg(windows)]
fn echo_argv(text: &str) -> Vec<OsString> {
    cmd_argv(&format!("echo {text}"))
}

/// The host variables a Windows child needs to find `cmd.exe` tools; the
/// launcher clears the environment, so nothing else reaches the child.
#[cfg(windows)]
#[expect(
    clippy::disallowed_methods,
    reason = "test fixture reads the four host variables cmd.exe needs; the production launcher receives the session env snapshot instead"
)]
fn child_env() -> Vec<(OsString, OsString)> {
    ["SystemRoot", "PATH", "PATHEXT", "ComSpec"]
        .into_iter()
        .filter_map(|key| std::env::var_os(key).map(|value| (OsString::from(key), value)))
        .collect()
}

#[cfg(not(windows))]
fn child_env() -> Vec<(OsString, OsString)> {
    Vec::new()
}

/// Spawns `argv` through the checked door with an approval covering the temp root.
async fn spawn_approved(
    temp: &tempfile::TempDir,
    name: &str,
    argv: &[OsString],
    timeout: Option<Duration>,
    stdout_prefix_limit: usize,
) -> Result<super::Proc, ToolError> {
    let call = CallId::new(name);
    let digest = Some([5_u8; 32]);
    let approved = approved_for(&call, digest, vec![temp.path().to_path_buf()]);
    let opts = SpawnOpts {
        cwd: temp.path().to_path_buf(),
        timeout,
        env: Vec::new(),
        stdout_prefix_limit,
    };
    spawn_process(
        argv,
        call,
        opts,
        &approved,
        digest,
        &temp.path().join("jobs"),
        &child_env(),
        &Launcher::Direct,
        ProcessLease::for_test(test_permit().await, None),
    )
}

// The lifecycle tests below spawn real processes through a POSIX shell and
// rely on Unix process-group semantics (stop-ladder signals, grandchild
// sweeps). The Windows job-object launcher is covered by the
// `windows_*` tests at the end of this module.

/// Every byte the lifetime test's child writes, in order: three lines, then
/// the four variables the launcher forces on each child.
#[cfg(unix)]
const LIFETIME_OUTPUT: &[u8] = b"a\nb\nc\nNO_COLOR=1 TERM=dumb PAGER=cat DAL_NESTED=1";

#[cfg(unix)]
#[tokio::test]
async fn proc_spawns_captures_output_and_reports_bounded_env() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let mut proc = spawn_approved(
        &temp,
        "test-call-lifetime",
        &shell_argv("printf 'a\\nb\\nc\\n'; printf \"NO_COLOR=$NO_COLOR TERM=$TERM PAGER=$PAGER DAL_NESTED=$DAL_NESTED\"; read line || true"),
        None,
        0,
    )
    .await
    .expect("spawn test shell");
    let cancel = CancellationToken::new();
    let result = proc.wait(&cancel).await.expect("wait test shell");
    assert_eq!(result.status, ProcStatus::Exited { code: 0 });
    assert_eq!(result.outcome, dal_core::JobOutcome::Exited { code: 0 });
    assert!(
        result.log_path.starts_with(temp.path().join("jobs")),
        "the durable log lives in the jobs directory: {:?}",
        result.log_path
    );
    assert_eq!(
        std::fs::read(&result.log_path).expect("durable log bytes"),
        LIFETIME_OUTPUT,
        "the durable log holds every byte the child wrote, in order"
    );
    assert_eq!(result.preview.as_ref(), LIFETIME_OUTPUT);
    let preview = String::from_utf8_lossy(&result.preview);
    assert!(
        preview.contains("NO_COLOR=1"),
        "preview carries bounded env: {preview}"
    );
    assert!(
        preview.contains("TERM=dumb"),
        "preview carries bounded env: {preview}"
    );
    assert!(
        preview.contains("PAGER=cat"),
        "preview carries bounded env: {preview}"
    );
    assert!(
        preview.contains("DAL_NESTED=1"),
        "preview carries bounded env: {preview}"
    );
    assert!(result.preview.len() <= PREVIEW_BYTES);
    assert!(result.completion_tail.len() <= COMPLETION_TAIL_BYTES);
    assert!(!result.denial_seen);
    let tail = proc.tail(64);
    assert_eq!(
        tail.as_ref(),
        LIFETIME_OUTPUT,
        "a window wider than the output returns all of it"
    );
    let preview_from_file = tail_preview(&result.log_path, PREVIEW_BYTES).expect("tail preview");
    assert_eq!(preview_from_file.as_ref(), result.preview.as_ref());
}

#[cfg(unix)]
#[tokio::test]
async fn proc_timeout_uses_stop_ladder_and_reports_timed_out() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let mut proc = spawn_approved(
        &temp,
        "test-call-timeout",
        &shell_argv("sleep 30"),
        Some(Duration::from_secs(1)),
        0,
    )
    .await
    .expect("spawn sleep");
    let cancel = CancellationToken::new();
    let result = proc.wait(&cancel).await.expect("wait with timeout");
    assert_eq!(result.status, ProcStatus::TimedOut);
    assert_eq!(
        result.outcome,
        dal_core::JobOutcome::Failed {
            message: "Command timed out".into()
        },
        "a timeout settles as a failed job, never as an exit"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn proc_cancellation_kills_tree_and_settles_once() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let mut proc = spawn_approved(
        &temp,
        "test-call-cancel",
        &shell_argv("sleep 30 & wait"),
        None,
        0,
    )
    .await
    .expect("spawn sleep tree");
    let result = proc.stop(StopReason::Cancelled).await.expect("stop tree");
    assert_eq!(result.status, ProcStatus::Cancelled);
    assert_eq!(result.outcome, dal_core::JobOutcome::Cancelled);
    // A second terminal call returns the same cached outcome without reaping again.
    let again = proc.stop(StopReason::Cancelled).await.expect("repeat stop");
    assert_eq!(again.status, result.status);
    assert_eq!(again.preview.as_ref(), result.preview.as_ref());
}

#[cfg(unix)]
#[tokio::test]
async fn proc_wait_sweeps_grandchild_holding_pipe_before_capture_join() {
    let temp = tempfile::tempdir().expect("temp workspace");
    // The background sleeper inherits the pipe write ends while the leader
    // exits 0 at once. Without the wait-path sweep, capture would hold EOF
    // open until the sleeper exits, and the bounded wait below would time out.
    let mut proc = spawn_approved(
        &temp,
        "test-call-wait-sweep",
        &shell_argv("sleep 30 & exit 0"),
        None,
        0,
    )
    .await
    .expect("spawn leader with background grandchild");
    let cancel = CancellationToken::new();
    let result = tokio::time::timeout(Duration::from_secs(10), proc.wait(&cancel))
        .await
        .expect("wait settles despite surviving grandchild")
        .expect("wait test shell");
    assert_eq!(result.status, ProcStatus::Exited { code: 0 });
}

/// The post-exit sweep owns the leader identity: once the native leader is
/// reaped, the group is swept synchronously and the outer wrapper wait reaps
/// remaining group zombies before capture finalization. `Drop` must not signal
/// the pid again, even when capture failure leaves `result` unset. This closes
/// the new Drop suspension window, not every historical numeric-PID reuse risk.
#[cfg(unix)]
#[tokio::test]
async fn proc_wait_consumes_leader_identity_after_reap_and_sweep() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let mut proc = spawn_approved(&temp, "test-call-drop-identity", &echo_argv("hi"), None, 0)
        .await
        .expect("spawn echo");
    assert!(
        proc.leader_pid.is_some(),
        "a live child keeps its owned pid for the close-abort sweep"
    );
    let cancel = CancellationToken::new();
    let result = proc.wait(&cancel).await.expect("wait echo");
    assert_eq!(result.status, ProcStatus::Exited { code: 0 });
    assert!(
        proc.leader_pid.is_none(),
        "reap plus sweep consumes the owned pid so Drop cannot re-signal it"
    );
}

/// The stop ladder ends in the same reap-plus-sweep transition, so a settled
/// stop must also disarm the `Drop` sweep.
#[cfg(unix)]
#[tokio::test]
async fn proc_stop_consumes_leader_identity_after_ladder() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let mut proc = spawn_approved(
        &temp,
        "test-call-stop-identity",
        &shell_argv("sleep 30"),
        None,
        0,
    )
    .await
    .expect("spawn sleep");
    assert!(
        proc.leader_pid.is_some(),
        "a live child keeps its owned pid for the close-abort sweep"
    );
    let result = proc.stop(StopReason::Cancelled).await.expect("stop sleep");
    assert_eq!(result.status, ProcStatus::Cancelled);
    assert!(
        proc.leader_pid.is_none(),
        "stop-ladder reap plus sweep consumes the owned pid"
    );
}

/// A capture fault after native leader reap, synchronous group sweep, and
/// retained outer reaping leaves `result` unset; `Drop` must not re-signal the
/// consumed pid.
#[cfg(unix)]
#[tokio::test]
async fn proc_capture_fault_after_reap_disarms_drop_sweep() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let mut proc = spawn_approved(
        &temp,
        "test-call-capture-fault-identity",
        &echo_argv("hi"),
        None,
        0,
    )
    .await
    .expect("spawn echo");
    drop(proc.capture.take());
    let cancel = CancellationToken::new();
    let result = proc.wait(&cancel).await;
    assert!(result.is_err(), "missing capture fails the wait");
    assert!(
        proc.leader_pid.is_none(),
        "reap plus sweep consumes the owned pid even when capture fails"
    );
}

/// Polls the live tail until it holds `marker`, so a signal sent next finds
/// the child's handlers already installed.
#[cfg(unix)]
async fn wait_for_output(proc: &super::Proc, marker: &[u8]) {
    for _ in 0..250 {
        if proc
            .tail(4_096)
            .windows(marker.len())
            .any(|window| window == marker)
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the child never printed {marker:?}");
}

/// `Proc::tail` serves the newest bytes of the retained output and never an
/// older window, at every width around the output length.
#[cfg(unix)]
#[tokio::test]
async fn tail_serves_exactly_the_newest_requested_bytes() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let mut proc = spawn_approved(
        &temp,
        "test-tail-window",
        &shell_argv("printf '0123456789abcdef'"),
        None,
        0,
    )
    .await
    .expect("spawn printf");
    proc.wait(&CancellationToken::new())
        .await
        .expect("wait printf");
    let output: &[u8] = b"0123456789abcdef";
    assert_eq!(proc.tail(0).as_ref(), b"", "a zero window is empty");
    assert_eq!(proc.tail(1).as_ref(), b"f", "one byte is the newest byte");
    assert_eq!(proc.tail(6).as_ref(), b"abcdef");
    assert_eq!(
        proc.tail(15).as_ref(),
        &output[1..],
        "one under the length drops only the oldest byte"
    );
    assert_eq!(
        proc.tail(16).as_ref(),
        output,
        "the exact length is the whole output"
    );
    assert_eq!(
        proc.tail(17).as_ref(),
        output,
        "one over the length invents no bytes"
    );
    assert_eq!(proc.tail(usize::MAX).as_ref(), output);
}

/// The launcher forces the four bounded variables last, so no snapshot or
/// override can switch them off, and the child sees nothing else from the
/// host: its environment is exactly snapshot, then overrides, then forced.
#[cfg(unix)]
#[tokio::test]
async fn child_environment_is_exactly_snapshot_then_overrides_then_the_bounded_variables() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let call = CallId::new("test-env-exact");
    let digest = Some([4_u8; 32]);
    let approved = approved_for(&call, digest, vec![temp.path().to_path_buf()]);
    let pair = |key: &str, value: &str| (OsString::from(key), OsString::from(value));
    let snapshot = [
        pair("KEEP", "snapshot"),
        pair("ONLY_SNAPSHOT", "s"),
        pair("NO_COLOR", "0"),
        pair("TERM", "xterm-256color"),
    ];
    let opts = SpawnOpts {
        cwd: temp.path().to_path_buf(),
        timeout: None,
        env: vec![
            pair("KEEP", "override"),
            pair("PAGER", "less"),
            pair("DAL_NESTED", "0"),
        ],
        stdout_prefix_limit: 0,
    };
    let mut proc = spawn_process(
        &[OsString::from("/usr/bin/env")],
        call,
        opts,
        &approved,
        digest,
        &temp.path().join("jobs"),
        &snapshot,
        &Launcher::Direct,
        ProcessLease::for_test(test_permit().await, None),
    )
    .expect("spawn env");
    let result = proc
        .wait(&CancellationToken::new())
        .await
        .expect("wait env");
    assert_eq!(result.status, ProcStatus::Exited { code: 0 });
    let mut lines: Vec<String> = String::from_utf8_lossy(&result.preview)
        .lines()
        .map(str::to_owned)
        .collect();
    lines.sort();
    assert_eq!(
        lines,
        [
            "DAL_NESTED=1",
            "KEEP=override",
            "NO_COLOR=1",
            "ONLY_SNAPSHOT=s",
            "PAGER=cat",
            "TERM=dumb",
        ]
    );
}

/// A non-zero exit and a signal death are different terminal states with
/// their own durable outcomes; neither may collapse into a clean exit.
#[cfg(unix)]
#[tokio::test]
async fn non_zero_exits_and_signal_deaths_settle_with_their_own_status_and_outcome() {
    use dal_core::JobOutcome;

    let temp = tempfile::tempdir().expect("temp workspace");
    for (index, (script, status, outcome)) in [
        (
            "exit 7",
            ProcStatus::Exited { code: 7 },
            JobOutcome::Exited { code: 7 },
        ),
        (
            "kill -9 $$",
            ProcStatus::Signaled { signal: 9 },
            JobOutcome::Failed {
                message: "Command was killed by signal 9".into(),
            },
        ),
        (
            "kill -TERM $$",
            ProcStatus::Signaled { signal: 15 },
            JobOutcome::Failed {
                message: "Command was killed by signal 15".into(),
            },
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let result = run_with_call_id(&temp, &CallId::new(format!("test-status-{index}")), script)
            .await
            .unwrap_or_else(|error| panic!("{script} settles: {error}"));
        assert_eq!(result.status, status, "status of {script}");
        assert_eq!(result.outcome, outcome, "outcome of {script}");
    }
}

/// The stop ladder sends the soft signal first: a handler gets to run and
/// write before the hard kill could ever land.
#[cfg(unix)]
#[tokio::test]
async fn stop_sends_the_soft_signal_first_and_lets_a_handler_finish() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let mut proc = spawn_approved(
        &temp,
        "test-soft-first",
        &shell_argv("trap 'printf graceful; exit 0' TERM; printf ready; sleep 30 & wait"),
        None,
        0,
    )
    .await
    .expect("spawn trapping shell");
    wait_for_output(&proc, b"ready").await;
    let result = proc.stop(StopReason::Cancelled).await.expect("stop");
    assert_eq!(result.status, ProcStatus::Cancelled);
    assert_eq!(
        result.preview.as_ref(),
        b"readygraceful",
        "the TERM handler ran before the group died"
    );
}

/// A child that ignores the soft signal still dies: the ladder waits out
/// the grace window, then kills the whole group.
#[cfg(unix)]
#[tokio::test]
async fn a_child_that_ignores_the_soft_signal_is_killed_after_the_grace_window() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let mut proc = spawn_approved(
        &temp,
        "test-hard-kill",
        &shell_argv("trap '' TERM; printf ready; sleep 30 & wait"),
        None,
        0,
    )
    .await
    .expect("spawn TERM-ignoring shell");
    wait_for_output(&proc, b"ready").await;
    let started = std::time::Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(20), proc.stop(StopReason::Timeout))
        .await
        .expect("the hard kill reaps a child that ignored the soft signal")
        .expect("stop");
    assert_eq!(result.status, ProcStatus::TimedOut);
    assert!(
        started.elapsed() + Duration::from_millis(100) >= super::KILL_GRACE,
        "the soft signal gets the whole grace window first: {:?}",
        started.elapsed()
    );
    assert_eq!(
        result.preview.as_ref(),
        b"ready",
        "the ignored soft signal produced no output"
    );
}

/// Asserts `path` is the jobs directory itself holding a host-minted
/// `UUIDv7` log name, proving call text never became the filename.
#[cfg(unix)]
fn assert_job_owned_log(jobs: &Path, path: &Path) {
    assert_eq!(
        path.parent(),
        Some(jobs),
        "the durable log lives in the jobs directory: {path:?}"
    );
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .expect("uuid log name");
    dal_core::JobId::parse(stem).expect("log name is a host-minted uuid");
}

/// Runs one child with the given call identity through the checked door and
/// returns its settled result.
#[cfg(unix)]
async fn run_with_call_id(
    temp: &tempfile::TempDir,
    call: &CallId,
    script: &str,
) -> Result<super::ProcResult, ToolError> {
    let digest = Some([9_u8; 32]);
    let approved = approved_for(call, digest, vec![temp.path().to_path_buf()]);
    let opts = SpawnOpts {
        cwd: temp.path().to_path_buf(),
        timeout: None,
        env: Vec::new(),
        stdout_prefix_limit: 0,
    };
    let mut proc = spawn_process(
        &shell_argv(script),
        call.clone(),
        opts,
        &approved,
        digest,
        &temp.path().join("jobs"),
        &child_env(),
        &Launcher::Direct,
        ProcessLease::for_test(test_permit().await, None),
    )
    .unwrap_or_else(|error| panic!("call id still spawns: {error}"));
    proc.wait(&CancellationToken::new()).await
}

/// Every path a provider call may carry — relative traversal, absolute
/// targets, 8 KiB of text — must stay data: the log lands only under the
/// minted UUID name inside the jobs directory and nothing appears outside.
#[cfg(unix)]
#[tokio::test]
async fn path_shaped_call_ids_cannot_escape_the_jobs_dir() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let outside = tempfile::tempdir().expect("outside store");
    std::fs::write(outside.path().join("sentinel"), b"guard").expect("sentinel");

    let jobs = temp.path().join("jobs");
    for (label, call) in [
        ("traversal", CallId::new("../../sentinel")),
        (
            "absolute",
            CallId::new(format!("{}/abs-target", outside.path().display())),
        ),
        ("oversized", CallId::new("x".repeat(8192))),
    ] {
        let result = run_with_call_id(&temp, &call, "printf 'out'")
            .await
            .unwrap_or_else(|error| panic!("{label} call id does not run: {error}"));
        assert_eq!(result.status, ProcStatus::Exited { code: 0 });
        assert_job_owned_log(&jobs, &result.log_path);
        assert_eq!(
            std::fs::read(&result.log_path).expect("log bytes"),
            b"out",
            "{label} call id captured into the owned log"
        );
    }

    let sentinel = outside.path().join("sentinel");
    assert_eq!(
        std::fs::read(&sentinel).expect("sentinel bytes"),
        b"guard",
        "the sentinel content is untouched"
    );
    let outside_entries: Vec<_> = std::fs::read_dir(outside.path())
        .expect("outside store")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    assert_eq!(outside_entries.len(), 1, "no log escaped the jobs dir");
    assert_eq!(outside_entries[0], "sentinel");
}

/// Repeating one provider call identity must open a second distinct owned
/// log and leave the first run's durable output intact.
#[cfg(unix)]
#[tokio::test]
async fn repeated_call_id_writes_distinct_logs_without_truncation() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let call = CallId::new("test-repeat");

    let first = run_with_call_id(&temp, &call, "printf 'first'")
        .await
        .expect("first repeat runs");
    let second = run_with_call_id(&temp, &call, "printf 'second'")
        .await
        .expect("second repeat runs");

    assert_ne!(first.log_path, second.log_path, "runs get distinct logs");
    assert_eq!(
        std::fs::read(&first.log_path).expect("first log bytes"),
        b"first",
        "the first run's output survives the second run"
    );
    assert_eq!(
        std::fs::read(&second.log_path).expect("second log bytes"),
        b"second"
    );
}

/// The capture log is created owner-private and never follows a planted
/// symlink or truncates a preexisting file at its own name.
#[cfg(unix)]
#[test]
fn capture_log_open_refuses_existing_and_symlinked_paths() {
    let temp = tempfile::tempdir().expect("temp workspace");

    let existing = temp.path().join("existing.log");
    std::fs::write(&existing, b"keep").expect("preexisting file");
    let error = super::open_capture_log(&existing).expect_err("preexisting name refused");
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(
        std::fs::read(&existing).expect("preexisting bytes"),
        b"keep",
        "the preexisting file is not truncated"
    );

    let sentinel = temp.path().join("sentinel");
    std::fs::write(&sentinel, b"guard").expect("sentinel file");
    let link = temp.path().join("link.log");
    std::os::unix::fs::symlink(&sentinel, &link).expect("planted symlink");
    let error = super::open_capture_log(&link).expect_err("symlinked name refused");
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(
        std::fs::read(&sentinel).expect("sentinel bytes"),
        b"guard",
        "the symlink target is never written through"
    );

    let fresh = temp.path().join("fresh.log");
    super::open_capture_log(&fresh).expect("fresh name opens");
    let mode = std::fs::metadata(&fresh)
        .expect("fresh log metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600, "the log is owner-private");
}

// The escape probe must reach setsid(), setpgid(), and posix_spawn's
// POSIX_SPAWN_SETPGROUP attribute through their literal syscall surfaces;
// neither Rust nor the workspace offers them without unsafe. The probe is
// compiled at test time by the system cc that building this crate already
// requires on macOS.
#[cfg(target_os = "macos")]
const MACOS_ESCAPE_PROBE_C: &str = r#"
#include <signal.h>
#include <spawn.h>
#include <string.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

extern char **environ;

// Exit 0 when the sandbox denied the escape, 6x when it went through.
int main(int argc, char **argv) {
    if (argc < 2) {
        return 64;
    }
    if (strcmp(argv[1], "setsid") == 0 || strcmp(argv[1], "setpgid") == 0) {
        pid_t pid = fork();
        if (pid < 0) {
            return 65;
        }
        if (pid == 0) {
            int escaped = strcmp(argv[1], "setsid") == 0
                ? setsid() != (pid_t)-1
                : setpgid(0, 0) == 0;
            _exit(escaped ? 61 : 0);
        }
        int status = 0;
        if (waitpid(pid, &status, 0) < 0) {
            return 65;
        }
        return WIFEXITED(status) ? WEXITSTATUS(status) : 65;
    }
    if (strcmp(argv[1], "spawnpgroup") == 0) {
        posix_spawnattr_t attr;
        pid_t pid = 0;
        char *child_argv[] = { "sleep", "30", 0 };
        int rc;
        if (posix_spawnattr_init(&attr) != 0) {
            return 65;
        }
        posix_spawnattr_setflags(&attr, POSIX_SPAWN_SETPGROUP);
        posix_spawnattr_setpgroup(&attr, 0);
        rc = posix_spawn(&pid, "/bin/sleep", 0, &attr, child_argv, environ);
        posix_spawnattr_destroy(&attr);
        if (rc != 0) {
            return 0;
        }
        for (int poll = 0; poll < 20; poll++) {
            int status = 0;
            struct timespec delay = { 0, 100 * 1000 * 1000 };
            if (waitpid(pid, &status, WNOHANG) == pid) {
                return WIFEXITED(status) && WEXITSTATUS(status) == 127 ? 0 : 63;
            }
            nanosleep(&delay, 0);
        }
        kill(pid, SIGKILL);
        waitpid(pid, 0, 0);
        return 63;
    }
    return 64;
}
"#;

/// Compiles the escape probe with the system compiler through the checked
/// spawn door; the compile itself runs under the lifetime policy on macOS.
#[cfg(target_os = "macos")]
async fn compile_escape_probe(temp: &tempfile::TempDir) -> PathBuf {
    let source = temp.path().join("escape-probe.c");
    let binary = temp.path().join("escape-probe");
    std::fs::write(&source, MACOS_ESCAPE_PROBE_C).expect("write probe source");
    let mut proc = spawn_approved(
        temp,
        "test-macos-probe-cc",
        &[
            OsString::from("/usr/bin/cc"),
            OsString::from("-o"),
            binary.as_os_str().to_owned(),
            source.as_os_str().to_owned(),
        ],
        Some(Duration::from_secs(60)),
        4_096,
    )
    .await
    .expect("spawn cc for the escape probe");
    let result = proc
        .wait(&CancellationToken::new())
        .await
        .expect("wait for the probe compile");
    assert_eq!(
        result.status,
        ProcStatus::Exited { code: 0 },
        "cc could not build the escape probe: {}",
        String::from_utf8_lossy(&result.preview)
    );
    binary
}

// macOS keeps a mandatory lifetime-only Seatbelt policy even when the
// filesystem sandbox is off: a descendant must not leave the group the
// host owns, whether it calls setsid() or setpgid() after a fork or asks
// posix_spawn for a new group through its spawn attributes.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn macos_descendants_cannot_escape_the_owned_process_group() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let probe = compile_escape_probe(&temp).await;
    for mode in ["setsid", "setpgid", "spawnpgroup"] {
        let mut proc = spawn_approved(
            &temp,
            &format!("test-macos-escape-{mode}"),
            &[probe.as_os_str().to_owned(), OsString::from(mode)],
            None,
            0,
        )
        .await
        .expect("spawn the escape probe");
        let result = tokio::time::timeout(
            Duration::from_secs(15),
            proc.wait(&CancellationToken::new()),
        )
        .await
        .unwrap_or_else(|_| panic!("{mode} probe did not settle"))
        .expect("wait for the escape probe");
        assert_eq!(
            result.status,
            ProcStatus::Exited { code: 0 },
            "{mode} escape must be denied by the mandatory lifetime policy"
        );
    }
}

fn approved_for(call: &CallId, digest: Option<[u8; 32]>, roots: Vec<PathBuf>) -> Approved {
    Approved::new(
        call.clone(),
        digest,
        Box::default(),
        roots.into_boxed_slice(),
        None,
    )
}

fn denied_out_of_scope(result: &Result<super::Proc, ToolError>) -> bool {
    matches!(
        result,
        Err(ToolError::Denied(DenyReason::OutOfScope { .. }))
    )
}

#[cfg(unix)]
#[tokio::test]
async fn approved_scope_allows_matching_call_digest_and_roots() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let call = CallId::new("test-approved-allow");
    let digest = Some([7_u8; 32]);
    let approved = approved_for(&call, digest, vec![temp.path().to_path_buf()]);
    let opts = SpawnOpts {
        cwd: temp.path().to_path_buf(),
        timeout: None,
        env: Vec::new(),
        stdout_prefix_limit: 0,
    };
    let mut proc = spawn_process(
        &shell_argv("true"),
        call,
        opts,
        &approved,
        digest,
        &temp.path().join("jobs"),
        &[],
        &Launcher::Direct,
        ProcessLease::for_test(test_permit().await, None),
    )
    .expect("in-scope spawn starts");
    let cancel = CancellationToken::new();
    let result = proc.wait(&cancel).await.expect("wait in-scope child");
    assert_eq!(result.status, ProcStatus::Exited { code: 0 });
    assert!(
        result.log_path.starts_with(temp.path().join("jobs")),
        "the durable log lives in the jobs directory: {:?}",
        result.log_path
    );
}

#[cfg(unix)]
#[tokio::test]
async fn spawn_keeps_the_requested_stdout_prefix_and_flags_only_real_overflow() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let mut results = Vec::new();
    for (name, limit) in [("fits", 64), ("overflows", 4)] {
        let call = CallId::new(format!("test-prefix-{name}"));
        let digest = Some([3_u8; 32]);
        let approved = approved_for(&call, digest, vec![temp.path().to_path_buf()]);
        let opts = SpawnOpts {
            cwd: temp.path().to_path_buf(),
            timeout: None,
            env: Vec::new(),
            stdout_prefix_limit: limit,
        };
        let mut proc = spawn_process(
            &shell_argv("printf ' M a.rs\\n'"),
            call,
            opts,
            &approved,
            digest,
            &temp.path().join("jobs"),
            &[],
            &Launcher::Direct,
            ProcessLease::for_test(test_permit().await, None),
        )
        .expect("spawn with a prefix limit starts");
        results.push(proc.wait(&CancellationToken::new()).await.expect("wait"));
    }
    assert_eq!(results[0].stdout_prefix.as_ref(), b" M a.rs\n");
    assert!(!results[0].stdout_prefix_overflowed);
    assert_eq!(results[1].stdout_prefix.as_ref(), b" M a");
    assert!(results[1].stdout_prefix_overflowed);
}

#[tokio::test]
async fn approved_scope_denies_digest_mismatch_without_spawning() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let call = CallId::new("test-approved-digest");
    let approved = approved_for(&call, Some([7_u8; 32]), vec![temp.path().to_path_buf()]);
    let opts = SpawnOpts {
        cwd: temp.path().to_path_buf(),
        timeout: None,
        env: Vec::new(),
        stdout_prefix_limit: 0,
    };
    let result = spawn_process(
        &shell_argv("true"),
        call,
        opts,
        &approved,
        Some([8_u8; 32]),
        &temp.path().join("jobs"),
        &[],
        &Launcher::Direct,
        ProcessLease::for_test(test_permit().await, None),
    );
    assert!(denied_out_of_scope(&result));
    assert!(
        !temp.path().join("jobs").exists(),
        "the digest refusal created no jobs directory"
    );
}

#[tokio::test]
async fn approved_scope_denies_foreign_call_without_spawning() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let approved = approved_for(
        &CallId::new("test-approved-other"),
        Some([7_u8; 32]),
        vec![temp.path().to_path_buf()],
    );
    let opts = SpawnOpts {
        cwd: temp.path().to_path_buf(),
        timeout: None,
        env: Vec::new(),
        stdout_prefix_limit: 0,
    };
    let result = spawn_process(
        &shell_argv("true"),
        CallId::new("test-approved-call"),
        opts,
        &approved,
        Some([7_u8; 32]),
        &temp.path().join("jobs"),
        &[],
        &Launcher::Direct,
        ProcessLease::for_test(test_permit().await, None),
    );
    assert!(denied_out_of_scope(&result));
    assert!(
        !temp.path().join("jobs").exists(),
        "the foreign-call refusal created no jobs directory"
    );
}

#[tokio::test]
async fn approved_scope_denies_cwd_outside_roots_without_spawning() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let elsewhere = tempfile::tempdir().expect("other roots");
    let call = CallId::new("test-approved-roots");
    let digest = Some([7_u8; 32]);
    let approved = approved_for(&call, digest, vec![elsewhere.path().to_path_buf()]);
    let opts = SpawnOpts {
        cwd: temp.path().to_path_buf(),
        timeout: None,
        env: Vec::new(),
        stdout_prefix_limit: 0,
    };
    let result = spawn_process(
        &shell_argv("true"),
        call,
        opts,
        &approved,
        digest,
        &temp.path().join("jobs"),
        &[],
        &Launcher::Direct,
        ProcessLease::for_test(test_permit().await, None),
    );
    assert!(denied_out_of_scope(&result));
    assert!(
        !temp.path().join("jobs").exists(),
        "the out-of-roots refusal created no jobs directory"
    );
}

#[tokio::test]
async fn spawn_accepts_the_maximum_stdout_prefix_limit() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let mut proc = spawn_approved(
        &temp,
        "test-prefix-max",
        &echo_argv("ok"),
        None,
        MAX_STDOUT_PREFIX_BYTES,
    )
    .await
    .expect("the maximum prefix limit is accepted");
    let result = proc
        .wait(&CancellationToken::new())
        .await
        .expect("wait at the maximum limit");
    assert_eq!(result.status, ProcStatus::Exited { code: 0 });
    assert!(result.stdout_prefix.starts_with(b"ok"));
    assert!(!result.stdout_prefix_overflowed);
}

#[tokio::test]
async fn spawn_refuses_stdout_prefix_limits_above_the_maximum_before_launching() {
    let protocol_max = usize::try_from(u32::MAX).unwrap_or(usize::MAX);
    for (name, limit) in [
        ("one-over", MAX_STDOUT_PREFIX_BYTES + 1),
        ("protocol-max", protocol_max),
    ] {
        let temp = tempfile::tempdir().expect("temp workspace");
        let result = spawn_approved(
            &temp,
            &format!("test-prefix-{name}"),
            &echo_argv("ok"),
            None,
            limit,
        )
        .await;
        let Err(ToolError::Failed(error)) = result else {
            panic!("limit {limit} must fail as a tool failure, got {result:?}");
        };
        let io_error = error
            .downcast_ref::<std::io::Error>()
            .expect("the refusal is an io error");
        assert_eq!(io_error.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(
            io_error.to_string(),
            "stdout prefix limit exceeds 262145 bytes"
        );
        let jobs = temp.path().join("jobs");
        assert!(!jobs.exists(), "limit {limit} created a jobs directory");
    }
}

#[tokio::test]
async fn spawn_rejects_empty_argv_before_prefix_and_approval() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let approved = approved_for(
        &CallId::new("test-invalid-argv-other"),
        Some([7_u8; 32]),
        vec![temp.path().to_path_buf()],
    );
    let opts = SpawnOpts {
        cwd: temp.path().to_path_buf(),
        timeout: None,
        env: Vec::new(),
        stdout_prefix_limit: MAX_STDOUT_PREFIX_BYTES + 1,
    };
    let result = spawn_process(
        &[],
        CallId::new("test-invalid-argv"),
        opts,
        &approved,
        Some([7_u8; 32]),
        &temp.path().join("jobs"),
        &[],
        &Launcher::Direct,
        ProcessLease::for_test(test_permit().await, None),
    );
    let Err(ToolError::Spawn { path, source }) = &result else {
        panic!("empty argv must fail as Spawn before approval, got {result:?}");
    };
    assert_eq!(*path, PathBuf::new());
    assert_eq!(source.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(source.to_string(), "empty argv");
    assert!(
        !temp.path().join("jobs").exists(),
        "the empty-argv refusal created a jobs directory"
    );
}

#[tokio::test]
async fn spawn_rejects_oversized_prefix_before_approval() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let approved = approved_for(
        &CallId::new("test-invalid-prefix-other"),
        Some([7_u8; 32]),
        vec![temp.path().to_path_buf()],
    );
    let opts = SpawnOpts {
        cwd: temp.path().to_path_buf(),
        timeout: None,
        env: Vec::new(),
        stdout_prefix_limit: MAX_STDOUT_PREFIX_BYTES + 1,
    };
    let result = spawn_process(
        &shell_argv("true"),
        CallId::new("test-invalid-prefix"),
        opts,
        &approved,
        Some([7_u8; 32]),
        &temp.path().join("jobs"),
        &[],
        &Launcher::Direct,
        ProcessLease::for_test(test_permit().await, None),
    );
    let Err(ToolError::Failed(error)) = &result else {
        panic!("an oversized prefix must fail before approval, got {result:?}");
    };
    let io_error = error
        .downcast_ref::<std::io::Error>()
        .expect("the refusal is an io error");
    assert_eq!(io_error.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(
        io_error.to_string(),
        "stdout prefix limit exceeds 262145 bytes"
    );
    assert!(
        !temp.path().join("jobs").exists(),
        "the prefix refusal created a jobs directory"
    );
}

#[tokio::test]
async fn spawn_refusal_releases_process_and_fd_permits() {
    use crate::admission::{Admission, Limits};

    let temp = tempfile::tempdir().expect("temp workspace");
    let admission = Admission::new(
        Limits {
            processes: 1,
            admission_wait: Duration::from_millis(100),
        },
        5,
    );
    let cancel = CancellationToken::new();
    let permit = admission
        .acquire_process(&cancel, admission.begin())
        .await
        .expect("process slot");
    let fd_permit = admission
        .charge_fds(super::PROCESS_FD_COST, &cancel, admission.begin())
        .await
        .expect("fd budget");
    let approved = approved_for(
        &CallId::new("test-permit-release-other"),
        Some([7_u8; 32]),
        vec![temp.path().to_path_buf()],
    );
    let opts = SpawnOpts {
        cwd: temp.path().to_path_buf(),
        timeout: None,
        env: Vec::new(),
        stdout_prefix_limit: MAX_STDOUT_PREFIX_BYTES + 1,
    };
    let result = spawn_process(
        &shell_argv("true"),
        CallId::new("test-permit-release"),
        opts,
        &approved,
        Some([7_u8; 32]),
        &temp.path().join("jobs"),
        &[],
        &Launcher::Direct,
        ProcessLease::for_test(permit, Some(fd_permit)),
    );
    assert!(matches!(result, Err(ToolError::Failed(_))));
    let _process = admission
        .acquire_process(&cancel, admission.begin())
        .await
        .expect("refusal releases the process slot");
    let _fds = admission
        .charge_fds(super::PROCESS_FD_COST, &cancel, admission.begin())
        .await
        .expect("refusal releases the fd budget");
    assert!(!temp.path().join("jobs").exists());
}

#[cfg(windows)]
#[tokio::test]
async fn windows_proc_captures_output_and_reports_bounded_env() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let mut proc = spawn_approved(
        &temp,
        "test-win-lifetime",
        &cmd_argv("echo NO_COLOR=%NO_COLOR% TERM=%TERM% PAGER=%PAGER% DAL_NESTED=%DAL_NESTED%"),
        None,
        0,
    )
    .await
    .expect("spawn cmd.exe");
    let result = proc
        .wait(&CancellationToken::new())
        .await
        .expect("wait cmd.exe");
    assert_eq!(result.status, ProcStatus::Exited { code: 0 });
    assert_eq!(result.outcome, dal_core::JobOutcome::Exited { code: 0 });
    assert!(result.log_path.exists());
    let preview = String::from_utf8_lossy(&result.preview);
    assert!(
        preview.contains("NO_COLOR=1 TERM=dumb PAGER=cat DAL_NESTED=1"),
        "preview carries bounded env: {preview}"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn windows_spawn_keeps_the_requested_stdout_prefix_and_flags_only_real_overflow() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let mut results = Vec::new();
    for (name, limit) in [("fits", 64), ("overflows", 4)] {
        let mut proc = spawn_approved(
            &temp,
            &format!("test-win-prefix-{name}"),
            &echo_argv("abcdefgh"),
            None,
            limit,
        )
        .await
        .expect("spawn with a prefix limit");
        results.push(
            proc.wait(&CancellationToken::new())
                .await
                .expect("wait for echo"),
        );
    }
    assert_eq!(results[0].stdout_prefix.as_ref(), b"abcdefgh\r\n");
    assert!(!results[0].stdout_prefix_overflowed);
    assert_eq!(results[1].stdout_prefix.as_ref(), b"abcd");
    assert!(results[1].stdout_prefix_overflowed);
}

#[cfg(windows)]
#[tokio::test]
async fn windows_timeout_terminates_the_job_and_reports_timed_out() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let mut proc = spawn_approved(
        &temp,
        "test-win-timeout",
        &cmd_argv("ping -n 31 127.0.0.1"),
        Some(Duration::from_secs(1)),
        0,
    )
    .await
    .expect("spawn ping tree");
    let cancel = CancellationToken::new();
    let result = tokio::time::timeout(Duration::from_secs(20), proc.wait(&cancel))
        .await
        .expect("the timeout ladder settles before the ping tree would exit")
        .expect("wait with timeout");
    assert_eq!(result.status, ProcStatus::TimedOut);
}

#[cfg(windows)]
#[tokio::test]
async fn windows_cancellation_kills_tree_and_settles_once() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let mut proc = spawn_approved(
        &temp,
        "test-win-cancel",
        &cmd_argv("ping -n 31 127.0.0.1"),
        None,
        0,
    )
    .await
    .expect("spawn ping tree");
    // The capture join inside `stop` needs EOF, which only arrives once the
    // ping grandchild holding the stdout pipe is gone with its job.
    let result = tokio::time::timeout(Duration::from_secs(20), proc.stop(StopReason::Cancelled))
        .await
        .expect("stop reaps the whole job tree")
        .expect("stop tree");
    assert_eq!(result.status, ProcStatus::Cancelled);
    let again = proc.stop(StopReason::Cancelled).await.expect("repeat stop");
    assert_eq!(again.status, result.status);
    assert_eq!(again.preview.as_ref(), result.preview.as_ref());
}

#[cfg(windows)]
#[tokio::test]
async fn windows_wait_terminates_grandchild_holding_pipe_before_capture_join() {
    let temp = tempfile::tempdir().expect("temp workspace");
    // The leader exits at once while `start /B` leaves ping holding the
    // inherited pipe; without the job termination in the wait path capture
    // would hold EOF open until ping exits and the bounded wait would time out.
    let mut proc = spawn_approved(
        &temp,
        "test-win-wait-sweep",
        &cmd_argv("start /B ping -n 31 127.0.0.1"),
        None,
        0,
    )
    .await
    .expect("spawn leader with background grandchild");
    let cancel = CancellationToken::new();
    let result = tokio::time::timeout(Duration::from_secs(10), proc.wait(&cancel))
        .await
        .expect("wait settles despite surviving grandchild")
        .expect("wait for leader");
    assert_eq!(result.status, ProcStatus::Exited { code: 0 });
}

/// No-OS-resource placeholder used only synchronously inside
/// `wrap_proc_for_ordering` to move the real child out without `unsafe`.
///
/// It never lives across an await, is never waited on, and reports no pid,
/// so no fabricated identity can reach real signaling or cleanup.
#[cfg(unix)]
#[derive(Debug)]
struct EmptyChild {
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
}

#[cfg(unix)]
impl EmptyChild {
    fn new() -> Self {
        Self {
            stdin: None,
            stdout: None,
            stderr: None,
        }
    }
}

#[cfg(unix)]
impl ChildWrapper for EmptyChild {
    fn inner(&self) -> &dyn ChildWrapper {
        self
    }
    fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
        self
    }
    fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
        self
    }
    fn id(&self) -> Option<u32> {
        None
    }
    fn stdin(&mut self) -> &mut Option<ChildStdin> {
        &mut self.stdin
    }
    fn stdout(&mut self) -> &mut Option<ChildStdout> {
        &mut self.stdout
    }
    fn stderr(&mut self) -> &mut Option<ChildStderr> {
        &mut self.stderr
    }
    fn start_kill(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        Ok(None)
    }
    fn wait(&mut self) -> Pin<Box<dyn Future<Output = io::Result<ExitStatus>> + Send + '_>> {
        Box::pin(std::future::pending())
    }
    fn signal(&self, _sig: i32) -> io::Result<()> {
        Ok(())
    }
}

/// Gated native-leader observer wrapping the real process-group child.
///
/// `wait` records entry, holds the test gate, then delegates to the real
/// native child (`Real.inner_mut().wait()`), preserving the real pid, real
/// exit status, and real pipes. `try_wait` and signals delegate straight
/// through so stop fast paths and group kills keep production semantics.
#[cfg(unix)]
#[derive(Debug)]
struct GatedInner {
    inner: Box<dyn ChildWrapper>,
    ordering: Arc<super::TestOrdering>,
}

#[cfg(unix)]
impl ChildWrapper for GatedInner {
    fn inner(&self) -> &dyn ChildWrapper {
        self.inner.as_ref()
    }
    fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
        self.inner.as_mut()
    }
    fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
        self.inner
    }
    fn id(&self) -> Option<u32> {
        self.inner.id()
    }
    fn stdin(&mut self) -> &mut Option<ChildStdin> {
        self.inner.stdin()
    }
    fn stdout(&mut self) -> &mut Option<ChildStdout> {
        self.inner.stdout()
    }
    fn stderr(&mut self) -> &mut Option<ChildStderr> {
        self.inner.stderr()
    }
    fn start_kill(&mut self) -> io::Result<()> {
        self.inner.start_kill()
    }
    fn signal(&self, sig: i32) -> io::Result<()> {
        self.inner.signal(sig)
    }
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.inner.inner_mut().try_wait()
    }
    fn wait(&mut self) -> Pin<Box<dyn Future<Output = io::Result<ExitStatus>> + Send + '_>> {
        let ordering = self.ordering.clone();
        Box::pin(async move {
            ordering.inner_entered.fetch_add(1, AtomicOrdering::SeqCst);
            ordering.got_inner.notify_one();
            ordering.allow_inner.notified().await;
            self.inner.inner_mut().wait().await
        })
    }
}

/// Separately held outer group-reap wrapper.
///
/// `wait` records entry, holds its own gate, then delegates to the real outer
/// group wait (`Real.wait()`), so group zombies are still really reaped once
/// released. Signals are counted then delegated to the real group wrapper.
#[cfg(unix)]
#[derive(Debug)]
struct GatedOuter {
    inner: Box<dyn ChildWrapper>,
    ordering: Arc<super::TestOrdering>,
}

#[cfg(unix)]
impl ChildWrapper for GatedOuter {
    fn inner(&self) -> &dyn ChildWrapper {
        self.inner.as_ref()
    }
    fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
        self.inner.as_mut()
    }
    fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
        self.inner
    }
    fn id(&self) -> Option<u32> {
        self.inner.id()
    }
    fn stdin(&mut self) -> &mut Option<ChildStdin> {
        self.inner.stdin()
    }
    fn stdout(&mut self) -> &mut Option<ChildStdout> {
        self.inner.stdout()
    }
    fn stderr(&mut self) -> &mut Option<ChildStderr> {
        self.inner.stderr()
    }
    fn start_kill(&mut self) -> io::Result<()> {
        self.inner.start_kill()
    }
    fn signal(&self, sig: i32) -> io::Result<()> {
        if sig == 15 {
            self.ordering.sig_term.fetch_add(1, AtomicOrdering::SeqCst);
        } else if sig == 9 {
            self.ordering.sig_kill.fetch_add(1, AtomicOrdering::SeqCst);
        }
        self.ordering.did_signal.notify_one();
        self.inner.signal(sig)
    }
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.inner.inner_mut().try_wait()
    }
    fn wait(&mut self) -> Pin<Box<dyn Future<Output = io::Result<ExitStatus>> + Send + '_>> {
        let ordering = self.ordering.clone();
        Box::pin(async move {
            ordering.outer_entered.fetch_add(1, AtomicOrdering::SeqCst);
            ordering.got_outer.notify_one();
            if ordering.fail_outer.load(AtomicOrdering::SeqCst) {
                return Err(io::Error::other("gated outer reap failure"));
            }
            ordering.allow_outer.notified().await;
            let status = self.inner.inner_mut().wait().await?;
            ordering
                .outer_completed
                .fetch_add(1, AtomicOrdering::SeqCst);
            Ok(status)
        })
    }
}

/// Installs the gated pair around the real spawned child.
///
/// The real child, pid, pipes, and capture stay intact; only observation is
/// gated. The transient placeholder never crosses an await and is never
/// signaled, so all cleanup still targets the real spawned group.
#[cfg(unix)]
fn wrap_proc_for_ordering(proc: &mut super::Proc, ordering: Arc<super::TestOrdering>) {
    let real = std::mem::replace(
        &mut proc.child,
        Box::new(EmptyChild::new()) as Box<dyn ChildWrapper>,
    );
    let inner = Box::new(GatedInner {
        inner: real,
        ordering: ordering.clone(),
    }) as Box<dyn ChildWrapper>;
    proc.child = Box::new(GatedOuter {
        inner,
        ordering: ordering.clone(),
    }) as Box<dyn ChildWrapper>;
    proc.test_ordering = Some(ordering);
}

#[cfg(unix)]
async fn await_notify(notify: &Notify, what: &str) {
    tokio::time::timeout(Duration::from_secs(10), notify.notified())
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

/// Normal wait: native leader completion synchronously disarms `Drop`
/// authority before the separately held outer group reap.
///
/// Holds the outer wait open to prove the sweep already ran while `result`
/// is still unset, then releases it to prove the outer wait is really
/// invoked exactly once for group-zombie reaping.
#[cfg(unix)]
#[tokio::test]
async fn proc_wait_disarms_before_outer_reap() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let mut proc = spawn_approved(&temp, "test-call-wait-ordering", &echo_argv("hi"), None, 0)
        .await
        .expect("spawn echo");
    let ordering = super::TestOrdering::new();
    wrap_proc_for_ordering(&mut proc, ordering.clone());
    assert!(proc.leader_pid.is_some());
    let cancel = CancellationToken::new();
    let cancel_wait = cancel.clone();
    let mut tasks = JoinSet::new();
    tasks.spawn(async move {
        let result = proc.wait(&cancel_wait).await;
        (proc, result)
    });
    await_notify(&ordering.got_inner, "native leader wait entry").await;
    assert_eq!(
        ordering.sweep.load(AtomicOrdering::SeqCst),
        0,
        "no group sweep before native leader completion"
    );
    ordering.allow_inner.notify_one();
    await_notify(&ordering.did_sweep, "synchronous group sweep").await;
    assert_eq!(
        ordering.sweep.load(AtomicOrdering::SeqCst),
        1,
        "exactly one disarming sweep after leader completion"
    );
    await_notify(&ordering.got_outer, "retained outer reap entry").await;
    assert!(
        tasks.try_join_next().is_none(),
        "wait stays pending while the outer group reap is held"
    );
    ordering.allow_outer.notify_one();
    let (proc, result) = tokio::time::timeout(Duration::from_secs(10), tasks.join_next())
        .await
        .expect("wait settles after outer release")
        .expect("wait task remains owned")
        .expect("wait task joins");
    let result = result.expect("wait echo");
    assert_eq!(result.status, ProcStatus::Exited { code: 0 });
    assert_eq!(
        ordering.outer_entered.load(AtomicOrdering::SeqCst),
        1,
        "retained outer wait is entered exactly once"
    );
    assert_eq!(
        ordering.outer_completed.load(AtomicOrdering::SeqCst),
        1,
        "retained outer wait really completes instead of being dropped"
    );
    assert_eq!(ordering.sweep.load(AtomicOrdering::SeqCst), 1);
    assert!(proc.leader_pid.is_none());
    assert!(proc.result.is_some());
}

/// Stop grace path: soft group signal, native leader observation, synchronous
/// disarm, then the separately held outer reap.
///
/// Asserts the soft signal goes through the outer wrapper, the hard kill is
/// not needed, and the sweep precedes outer entry while the stop stays
/// pending.
#[cfg(unix)]
#[tokio::test]
async fn proc_stop_grace_disarms_before_outer_reap() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let mut proc = spawn_approved(
        &temp,
        "test-call-stop-ordering",
        &shell_argv("sleep 30"),
        None,
        0,
    )
    .await
    .expect("spawn sleep");
    let ordering = super::TestOrdering::new();
    wrap_proc_for_ordering(&mut proc, ordering.clone());
    let mut tasks = JoinSet::new();
    tasks.spawn(async move {
        let result = proc.stop(StopReason::Cancelled).await;
        (proc, result)
    });
    await_notify(&ordering.did_signal, "soft group signal").await;
    await_notify(&ordering.got_inner, "grace native leader wait entry").await;
    assert_eq!(
        ordering.sig_term.load(AtomicOrdering::SeqCst),
        1,
        "grace sends one outer group SIGTERM"
    );
    assert_eq!(ordering.sweep.load(AtomicOrdering::SeqCst), 0);
    ordering.allow_inner.notify_one();
    await_notify(&ordering.did_sweep, "stop-path synchronous sweep").await;
    assert_eq!(ordering.sweep.load(AtomicOrdering::SeqCst), 1);
    await_notify(&ordering.got_outer, "stop-path outer reap entry").await;
    assert!(
        tasks.try_join_next().is_none(),
        "stop stays pending while the outer group reap is held"
    );
    ordering.allow_outer.notify_one();
    let (proc, result) = tokio::time::timeout(Duration::from_secs(10), tasks.join_next())
        .await
        .expect("stop settles after outer release")
        .expect("stop task remains owned")
        .expect("stop task joins");
    let result = result.expect("stop sleep");
    assert_eq!(result.status, ProcStatus::Cancelled);
    assert_eq!(
        ordering.sig_kill.load(AtomicOrdering::SeqCst),
        0,
        "grace observation avoids the hard kill"
    );
    assert_eq!(
        ordering.outer_completed.load(AtomicOrdering::SeqCst),
        1,
        "stop retains the outer group reap"
    );
    assert_eq!(ordering.sweep.load(AtomicOrdering::SeqCst), 1);
    assert!(proc.leader_pid.is_none());
}

/// Initial pending wait keeps `Drop` armed: dropping before native leader
/// completion must still run the close-abort group sweep exactly once.
#[cfg(unix)]
#[tokio::test]
async fn proc_initial_pending_drop_stays_armed() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let mut proc = spawn_approved(&temp, "test-call-initial-drop", &echo_argv("hi"), None, 0)
        .await
        .expect("spawn echo");
    let ordering = super::TestOrdering::new();
    wrap_proc_for_ordering(&mut proc, ordering.clone());
    let cancel = CancellationToken::new();
    let cancel_wait = cancel.clone();
    let mut tasks = JoinSet::new();
    tasks.spawn(async move {
        let _ = proc.wait(&cancel_wait).await;
    });
    await_notify(&ordering.got_inner, "native leader wait entry").await;
    assert_eq!(ordering.sweep.load(AtomicOrdering::SeqCst), 0);
    assert!(
        tasks.try_join_next().is_none(),
        "wait is pending before native leader completion"
    );
    tasks.abort_all();
    assert!(
        tasks
            .join_next()
            .await
            .expect("aborted wait task remains owned")
            .is_err(),
        "pending wait task is aborted and joined before checking Drop"
    );
    assert_eq!(
        ordering.sweep.load(AtomicOrdering::SeqCst),
        1,
        "armed Drop runs the close-abort sweep once"
    );
    assert_eq!(
        ordering.outer_entered.load(AtomicOrdering::SeqCst),
        0,
        "no outer reap runs when the leader was never observed"
    );
}

/// Held outer reap is already disarmed: dropping while suspended after the
/// synchronous sweep must not run a second group cleanup.
#[cfg(unix)]
#[tokio::test]
async fn proc_held_outer_drop_does_not_resweep() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let mut proc = spawn_approved(&temp, "test-call-held-drop", &echo_argv("hi"), None, 0)
        .await
        .expect("spawn echo");
    let ordering = super::TestOrdering::new();
    wrap_proc_for_ordering(&mut proc, ordering.clone());
    let cancel = CancellationToken::new();
    let cancel_wait = cancel.clone();
    let mut tasks = JoinSet::new();
    tasks.spawn(async move {
        let _ = proc.wait(&cancel_wait).await;
    });
    await_notify(&ordering.got_inner, "native leader wait entry").await;
    ordering.allow_inner.notify_one();
    await_notify(&ordering.did_sweep, "synchronous group sweep").await;
    await_notify(&ordering.got_outer, "retained outer reap entry").await;
    assert!(tasks.try_join_next().is_none());
    tasks.abort_all();
    assert!(
        tasks
            .join_next()
            .await
            .expect("held outer task remains owned")
            .is_err(),
        "held outer wait task is aborted and joined before checking Drop"
    );
    assert_eq!(
        ordering.sweep.load(AtomicOrdering::SeqCst),
        1,
        "disarmed Drop does not re-signal after the sweep"
    );
    assert_eq!(
        ordering.outer_completed.load(AtomicOrdering::SeqCst),
        0,
        "aborted outer reap never completes"
    );
}

/// Capture fault after the retained reap stays disarmed.
///
/// The missing capture fails the wait, but the native observation, synchronous
/// sweep, and outer reap already ran, so `Drop` must not re-signal.
#[cfg(unix)]
#[tokio::test]
async fn proc_capture_fault_after_reap_still_reaps_outer() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let mut proc = spawn_approved(
        &temp,
        "test-call-capture-ordering",
        &echo_argv("hi"),
        None,
        0,
    )
    .await
    .expect("spawn echo");
    let ordering = super::TestOrdering::new();
    wrap_proc_for_ordering(&mut proc, ordering.clone());
    drop(proc.capture.take());
    let cancel = CancellationToken::new();
    let cancel_wait = cancel.clone();
    let mut tasks = JoinSet::new();
    tasks.spawn(async move {
        let result = proc.wait(&cancel_wait).await;
        (proc, result)
    });
    await_notify(&ordering.got_inner, "native leader wait entry").await;
    ordering.allow_inner.notify_one();
    await_notify(&ordering.did_sweep, "synchronous group sweep").await;
    await_notify(&ordering.got_outer, "retained outer reap entry").await;
    ordering.allow_outer.notify_one();
    let (proc, result) = tokio::time::timeout(Duration::from_secs(10), tasks.join_next())
        .await
        .expect("capture-fault wait settles")
        .expect("capture-fault task remains owned")
        .expect("wait task joins");
    assert!(result.is_err(), "missing capture fails the wait");
    assert_eq!(
        ordering.outer_completed.load(AtomicOrdering::SeqCst),
        1,
        "outer reap runs even when capture later faults"
    );
    assert_eq!(ordering.sweep.load(AtomicOrdering::SeqCst), 1);
    assert!(proc.leader_pid.is_none());
    assert!(proc.result.is_none());
}

#[cfg(target_os = "linux")]
struct EscapeeCleanup {
    pid_file: PathBuf,
    leader_pid: u32,
    armed: bool,
}

#[cfg(target_os = "linux")]
impl Drop for EscapeeCleanup {
    fn drop(&mut self) {
        if self.armed {
            kill_escapee_from_pid_file(&self.pid_file, self.leader_pid);
        }
    }
}

#[cfg(target_os = "linux")]
fn kill_escapee_from_pid_file(pid_file: &Path, leader_pid: u32) {
    use rustix::process::{Pid, Signal, kill_process};

    if let Ok(contents) = std::fs::read_to_string(pid_file)
        && let Ok(pid) = contents.trim().parse::<u32>()
        && pid > 1
        && let Ok(raw) = i32::try_from(pid)
        && let Some(target) = Pid::from_raw(raw)
    {
        let _ = kill_process(target, Signal::KILL);
    }
    let descendants = super::stop::proc_descendants(leader_pid);
    super::stop::sweep_recorded(&descendants);
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
struct LinuxProcessStat {
    state: char,
    process_group: u32,
    session: u32,
}

#[cfg(target_os = "linux")]
fn linux_process_stat(pid: u32) -> Option<LinuxProcessStat> {
    let line = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let mut fields = line.rsplit_once(')')?.1.split_whitespace();
    let state = fields.next()?.chars().next()?;
    fields.next()?;
    Some(LinuxProcessStat {
        process_group: fields.next()?.parse().ok()?,
        session: fields.next()?.parse().ok()?,
        state,
    })
}

#[cfg(target_os = "linux")]
async fn wait_for_escapee_ready(leader_pid: u32, pid_file: &Path) -> u32 {
    let escapee_pid = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(contents) = std::fs::read_to_string(pid_file)
                && let Ok(pid) = contents.trim().parse::<u32>()
                && pid > 1
            {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("escapee writes its actual pid");

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let (Some(leader), Some(escapee)) = (
                linux_process_stat(leader_pid),
                linux_process_stat(escapee_pid),
            ) && leader.state != 'Z'
                && escapee.state != 'Z'
                && leader.process_group == leader_pid
                && escapee.process_group != leader_pid
                && escapee.session != leader.session
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("leader and live setsid escapee are verified distinct");
    escapee_pid
}

#[cfg(target_os = "linux")]
async fn wait_for_escapee_death(pid: u32) -> bool {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if linux_process_stat(pid).is_none_or(|stat| stat.state == 'Z') {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .is_ok()
}
/// A controlled outer-reap failure must still sweep a recorded real escapee.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn proc_stop_outer_error_still_sweeps_recorded_descendants() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let pid_file = temp.path().join("escapee.pid");
    let pid_file_arg = format!(
        "'{}'",
        pid_file.display().to_string().replace('\'', "'\\''")
    );
    let script = format!(
        "setsid /bin/sh -c 'echo $$ > \"$1\"; exec /bin/sleep 30' escapee {pid_file_arg} & /bin/sleep 30"
    );
    let mut proc = spawn_approved(
        &temp,
        "test-call-stop-outer-error",
        &shell_argv(&script),
        None,
        0,
    )
    .await
    .expect("spawn real leader and setsid escapee");
    let leader_pid = proc.leader_pid.expect("real leader pid stays owned");
    let mut cleanup = EscapeeCleanup {
        pid_file: pid_file.clone(),
        leader_pid,
        armed: true,
    };
    let ordering = super::TestOrdering::new();
    wrap_proc_for_ordering(&mut proc, ordering.clone());

    let escapee_pid = wait_for_escapee_ready(leader_pid, &pid_file).await;

    ordering.fail_outer.store(true, AtomicOrdering::SeqCst);
    let stop = proc.stop(StopReason::Cancelled);
    tokio::pin!(stop);
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::select! {
            result = &mut stop => result,
            () = ordering.got_inner.notified() => {
                ordering.allow_inner.notify_one();
                stop.await
            }
        }
    })
    .await
    .expect("stop settles");
    let error = result.expect_err("typed outer error propagates");
    assert!(
        error.to_string().contains("gated outer reap failure"),
        "original outer error: {error}"
    );

    assert!(
        wait_for_escapee_death(escapee_pid).await,
        "recorded real escapee died before Proc was dropped"
    );
    cleanup.armed = false;
}
