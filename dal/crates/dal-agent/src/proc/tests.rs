use std::{ffi::OsString, path::PathBuf, time::Duration};

#[cfg(unix)]
use dal_core::JobId;
use dal_core::{CallId, Workspace};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

#[cfg(unix)]
use super::launch;
#[cfg(unix)]
use super::{COMPLETION_TAIL_BYTES, PREVIEW_BYTES, last_lines, tail_preview};
use super::{Launcher, SpawnOpts, spawn_process};
use super::{MAX_STDOUT_PREFIX_BYTES, ProcStatus, StopReason};
use crate::error::{DenyReason, ToolError};
use crate::ext::tool::Approved;

async fn test_permit() -> tokio::sync::OwnedSemaphorePermit {
    static SEMAPHORE: std::sync::LazyLock<std::sync::Arc<Semaphore>> =
        std::sync::LazyLock::new(|| std::sync::Arc::new(Semaphore::new(256)));
    SEMAPHORE
        .clone()
        .acquire_owned()
        .await
        .expect("test permit")
}

fn test_workspace(temp: &tempfile::TempDir) -> Workspace {
    Workspace::new(temp.path().to_path_buf()).expect("absolute temp workspace")
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
        &test_workspace(temp),
        &temp.path().join("jobs"),
        &child_env(),
        &Launcher::Direct,
        test_permit().await,
        None,
    )
}

// The lifecycle tests below spawn real processes through a POSIX shell and
// rely on Unix process-group semantics (stop-ladder signals, grandchild
// sweeps). The Windows job-object launcher is covered by the
// `windows_*` tests at the end of this module.
#[cfg(unix)]
#[tokio::test]
async fn proc_spawns_captures_output_and_reports_bounded_env() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let workspace = test_workspace(&temp);
    let call = CallId::new("test-call-lifetime");
    let job = JobId::new_v7();
    let log_path = temp.path().join("jobs").join("test-call-lifetime.log");
    let opts = SpawnOpts {
        cwd: temp.path().to_path_buf(),
        timeout: None,
        env: Vec::new(),
        stdout_prefix_limit: 0,
    };
    let mut proc = launch(
        &shell_argv("printf 'a\\nb\\nc\\n'; printf \"NO_COLOR=$NO_COLOR TERM=$TERM PAGER=$PAGER DAL_NESTED=$DAL_NESTED\"; read line || true"),
        call,
        job,
        opts,
        &workspace,
        &[],
        &Launcher::Direct,
        test_permit().await,
        None,
        log_path.clone(),
        0,
        None,
    )
    .expect("spawn test shell");
    let cancel = CancellationToken::new();
    let result = proc.wait(&cancel).await.expect("wait test shell");
    assert_eq!(result.status, ProcStatus::Exited { code: 0 });
    assert_eq!(result.outcome, dal_core::JobOutcome::Exited { code: 0 });
    assert_eq!(result.log_path, log_path);
    assert!(PathBuf::from(&result.log_path).exists());
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
    assert!(!tail.is_empty());
    let preview_from_file = tail_preview(&log_path, PREVIEW_BYTES).expect("tail preview");
    assert_eq!(preview_from_file.as_ref(), result.preview.as_ref());
    assert_eq!(last_lines(b"a\nb\nc", 2), "b\nc");
}

#[cfg(unix)]
#[tokio::test]
async fn proc_timeout_uses_stop_ladder_and_reports_timed_out() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let workspace = test_workspace(&temp);
    let call = CallId::new("test-call-timeout");
    let job = JobId::new_v7();
    let log_path = temp.path().join("jobs").join("test-call-timeout.log");
    let opts = SpawnOpts {
        cwd: temp.path().to_path_buf(),
        timeout: Some(Duration::from_secs(1)),
        env: Vec::new(),
        stdout_prefix_limit: 0,
    };
    let mut proc = launch(
        &shell_argv("sleep 30"),
        call,
        job,
        opts,
        &workspace,
        &[],
        &Launcher::Direct,
        test_permit().await,
        None,
        log_path,
        0,
        None,
    )
    .expect("spawn sleep");
    let cancel = CancellationToken::new();
    let result = proc.wait(&cancel).await.expect("wait with timeout");
    assert_eq!(result.status, ProcStatus::TimedOut);
}

#[cfg(unix)]
#[tokio::test]
async fn proc_cancellation_kills_tree_and_settles_once() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let workspace = test_workspace(&temp);
    let call = CallId::new("test-call-cancel");
    let job = JobId::new_v7();
    let log_path = temp.path().join("jobs").join("test-call-cancel.log");
    let opts = SpawnOpts {
        cwd: temp.path().to_path_buf(),
        timeout: None,
        env: Vec::new(),
        stdout_prefix_limit: 0,
    };
    let mut proc = launch(
        &shell_argv("sleep 30 & wait"),
        call,
        job,
        opts,
        &workspace,
        &[],
        &Launcher::Direct,
        test_permit().await,
        None,
        log_path,
        0,
        None,
    )
    .expect("spawn sleep tree");
    let leader = proc.tail(0);
    let _ = leader;
    let result = proc.stop(StopReason::Cancelled).await.expect("stop tree");
    assert_eq!(result.status, ProcStatus::Cancelled);
    // A second terminal call returns the same cached outcome without reaping again.
    let again = proc.stop(StopReason::Cancelled).await.expect("repeat stop");
    assert_eq!(again.status, result.status);
    assert_eq!(again.preview.as_ref(), result.preview.as_ref());
}

#[cfg(unix)]
#[tokio::test]
async fn proc_wait_sweeps_grandchild_holding_pipe_before_capture_join() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let workspace = test_workspace(&temp);
    let call = CallId::new("test-call-wait-sweep");
    let job = JobId::new_v7();
    let log_path = temp.path().join("jobs").join("test-call-wait-sweep.log");
    let opts = SpawnOpts {
        cwd: temp.path().to_path_buf(),
        timeout: None,
        env: Vec::new(),
        stdout_prefix_limit: 0,
    };
    // The background sleeper inherits the pipe write ends while the leader
    // exits 0 at once. Without the wait-path sweep, capture would hold EOF
    // open until the sleeper exits, and the bounded wait below would time out.
    let mut proc = launch(
        &shell_argv("sleep 30 & exit 0"),
        call,
        job,
        opts,
        &workspace,
        &[],
        &Launcher::Direct,
        test_permit().await,
        None,
        log_path,
        0,
        None,
    )
    .expect("spawn leader with background grandchild");
    let cancel = CancellationToken::new();
    let result = tokio::time::timeout(Duration::from_secs(10), proc.wait(&cancel))
        .await
        .expect("wait settles despite surviving grandchild")
        .expect("wait test shell");
    assert_eq!(result.status, ProcStatus::Exited { code: 0 });
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
    let workspace = test_workspace(&temp);
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
        &workspace,
        &temp.path().join("jobs"),
        &[],
        &Launcher::Direct,
        test_permit().await,
        None,
    )
    .expect("in-scope spawn starts");
    let cancel = CancellationToken::new();
    let result = proc.wait(&cancel).await.expect("wait in-scope child");
    assert_eq!(result.status, ProcStatus::Exited { code: 0 });
    assert_eq!(
        result.log_path,
        temp.path().join("jobs").join("test-approved-allow.log")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn spawn_keeps_the_requested_stdout_prefix_and_flags_only_real_overflow() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let workspace = test_workspace(&temp);
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
            &workspace,
            &temp.path().join("jobs"),
            &[],
            &Launcher::Direct,
            test_permit().await,
            None,
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
    let workspace = test_workspace(&temp);
    let call = CallId::new("test-approved-digest");
    let approved = approved_for(&call, Some([7_u8; 32]), vec![temp.path().to_path_buf()]);
    let log_path = temp.path().join("jobs").join("test-approved-digest.log");
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
        &workspace,
        &temp.path().join("jobs"),
        &[],
        &Launcher::Direct,
        test_permit().await,
        None,
    );
    assert!(denied_out_of_scope(&result));
    assert!(!log_path.exists());
}

#[tokio::test]
async fn approved_scope_denies_foreign_call_without_spawning() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let workspace = test_workspace(&temp);
    let approved = approved_for(
        &CallId::new("test-approved-other"),
        Some([7_u8; 32]),
        vec![temp.path().to_path_buf()],
    );
    let log_path = temp.path().join("jobs").join("test-approved-call.log");
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
        &workspace,
        &temp.path().join("jobs"),
        &[],
        &Launcher::Direct,
        test_permit().await,
        None,
    );
    assert!(denied_out_of_scope(&result));
    assert!(!log_path.exists());
}

#[tokio::test]
async fn approved_scope_denies_cwd_outside_roots_without_spawning() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let elsewhere = tempfile::tempdir().expect("other roots");
    let workspace = test_workspace(&temp);
    let call = CallId::new("test-approved-roots");
    let digest = Some([7_u8; 32]);
    let approved = approved_for(&call, digest, vec![elsewhere.path().to_path_buf()]);
    let log_path = temp.path().join("jobs").join("test-approved-roots.log");
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
        &workspace,
        &temp.path().join("jobs"),
        &[],
        &Launcher::Direct,
        test_permit().await,
        None,
    );
    assert!(denied_out_of_scope(&result));
    assert!(!log_path.exists());
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
        let call = format!("test-prefix-{name}");
        let result = spawn_approved(&temp, &call, &echo_argv("ok"), None, limit).await;
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
        assert!(!jobs.join(format!("{call}.log")).exists());
    }
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
