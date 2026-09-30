use std::{ffi::OsString, path::PathBuf, time::Duration};

use dal_core::{CallId, JobId, Workspace};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use super::{
    COMPLETION_TAIL_BYTES, Launcher, PREVIEW_BYTES, ProcStatus, SpawnOpts, StopReason, last_lines,
    launch, spawn_process, tail_preview,
};
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
