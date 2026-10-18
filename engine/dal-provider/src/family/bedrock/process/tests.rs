//! Tests for the Bedrock `credential_process` runner.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::time::{Instant, sleep};
use tokio_util::sync::CancellationToken;

use super::{
    CredentialProcessRequest, is_windows_batch_script, run_credential_process,
    split_credential_process_argv, tokenize_credential_process_command, tokenize_posix,
    tokenize_windows,
};
use crate::error::{LimitError, ProviderError};
use crate::http::BODY_LIMIT;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_cwd(tag: &str) -> PathBuf {
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("dal-aws-process-{}-{n}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("test temp dir creates");
    dir
}

fn boxed(args: &[&str]) -> Vec<Box<str>> {
    args.iter().map(|a| (*a).into()).collect()
}

fn tokens_text(tokens: &[Box<str>]) -> Vec<&str> {
    tokens.iter().map(AsRef::as_ref).collect()
}

fn invalid_request_message(error: &ProviderError) -> &str {
    match error {
        ProviderError::InvalidRequest { message } => message,
        other => panic!("expected InvalidRequest, got {other:?}"),
    }
}

fn transport_reason(error: &ProviderError) -> &str {
    match error {
        ProviderError::Transport { reason, .. } => reason,
        other => panic!("expected Transport, got {other:?}"),
    }
}

#[cfg(unix)]
fn proc_cmdline_contains(marker: &str) -> bool {
    let entries = std::fs::read_dir("/proc").expect("read /proc");
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let path = format!("/proc/{name}/cmdline");
        let bytes = std::fs::read(&path).unwrap_or_default();
        if bytes.is_empty() {
            continue;
        }
        if String::from_utf8_lossy(&bytes).contains(marker) {
            return true;
        }
    }
    false
}

#[cfg(not(unix))]
fn proc_cmdline_contains(_marker: &str) -> bool {
    false
}

#[test]
fn request_debug_redacts_argv_and_env_values() {
    let dir = temp_cwd("debug");
    let secret = "fake-secret-debug-7c3a";
    let args = boxed(&[secret]);
    let env: &[(&str, &str)] = &[("PATH", "/usr/bin:/bin"), ("AWS_SECRET_ACCESS_KEY", secret)];
    let req = CredentialProcessRequest {
        program: "/bin/echo",
        args: &args,
        cwd: &dir,
        env,
        profile: "test-debug",
    };
    let text = format!("{req:?}");
    assert!(!text.contains(secret), "debug redacts values: {text}");
    assert!(text.contains("test-debug"), "debug keeps profile: {text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn posix_space_tab_newline_cr_delimit() {
    let tokens = tokenize_posix("a b\tc\nd\re").expect("posix splits");
    assert_eq!(tokens_text(&tokens), vec!["a", "b", "c", "d", "e"]);
}

#[test]
fn posix_single_quotes_are_literal() {
    let tokens = tokenize_posix("a 'b c' d").expect("posix single quotes");
    assert_eq!(tokens_text(&tokens), vec!["a", "b c", "d"]);
    let tokens = tokenize_posix("a 'b\\$c'").expect("single keeps backslash");
    assert_eq!(tokens_text(&tokens), vec!["a", "b\\$c"]);
}

#[test]
fn posix_double_quotes_escape_only_dollar_backtick_quote_backslash() {
    let tokens = tokenize_posix("a \"b\\$c\\`d\\\"e\\\\f\"").expect("posix double escapes");
    assert_eq!(tokens_text(&tokens), vec!["a", "b$c`d\"e\\f"]);
    let tokens = tokenize_posix("a \"b\\nc\"").expect("other backslash stays");
    assert_eq!(tokens_text(&tokens), vec!["a", "b\\nc"]);
}

#[test]
fn posix_unquoted_backslash_escapes_next_and_trailing_stays() {
    let tokens = tokenize_posix("a\\ b c").expect("escaped space");
    assert_eq!(tokens_text(&tokens), vec!["a b", "c"]);
    let tokens = tokenize_posix("ab\\").expect("trailing backslash literal");
    assert_eq!(tokens_text(&tokens), vec!["ab\\"]);
}

#[test]
fn posix_empty_quoted_span_yields_empty_token() {
    let tokens = tokenize_posix("\"\"").expect("empty double yields token");
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens_text(&tokens), vec![""]);
    let tokens = tokenize_posix("a '' b").expect("empty single yields token");
    assert_eq!(tokens_text(&tokens), vec!["a", "", "b"]);
}

#[test]
fn posix_unterminated_single_quote_is_invalid_request() {
    let error = tokenize_posix("echo 'abc").expect_err("single must fail");
    assert_eq!(
        invalid_request_message(&error),
        "AWS credential_process command has an unterminated quote."
    );
}

#[test]
fn posix_unterminated_double_quote_is_invalid_request() {
    let error = tokenize_posix("echo \"abc").expect_err("double must fail");
    assert_eq!(
        invalid_request_message(&error),
        "AWS credential_process command has an unterminated quote."
    );
}

#[test]
fn windows_only_space_tab_delimit_newline_cr_literal() {
    let tokens = tokenize_windows("a b\tc").expect("windows splits");
    assert_eq!(tokens_text(&tokens), vec!["a", "b", "c"]);
    let tokens = tokenize_windows("a\nb\rc").expect("newline is text on windows");
    assert_eq!(tokens_text(&tokens), vec!["a\nb\rc"]);
}

#[test]
fn windows_single_quotes_do_not_group() {
    let tokens = tokenize_windows("a 'b c'").expect("single is text");
    assert_eq!(tokens_text(&tokens), vec!["a", "'b", "c'"]);
    let tokens = tokenize_windows("a \"b c\" d").expect("double groups");
    assert_eq!(tokens_text(&tokens), vec!["a", "b c", "d"]);
}

#[test]
fn windows_backslash_runs_before_quote() {
    let tokens = tokenize_windows("a \"b\\\"c\"").expect("odd escapes quote");
    assert_eq!(tokens_text(&tokens), vec!["a", "b\"c"]);
    let tokens = tokenize_windows("a \"b\\\\\" c").expect("even keeps grouping");
    assert_eq!(tokens_text(&tokens), vec!["a", "b\\", "c"]);
}

#[test]
fn windows_paths_preserve_backslashes() {
    let tokens = tokenize_windows("C:\\tools\\aws_helper.exe --flag").expect("path intact");
    assert_eq!(
        tokens_text(&tokens),
        vec!["C:\\tools\\aws_helper.exe", "--flag"]
    );
    let tokens = tokenize_windows("\\\\server\\share\\helper.exe arg").expect("unc intact");
    assert_eq!(
        tokens_text(&tokens),
        vec!["\\\\server\\share\\helper.exe", "arg"]
    );
}

#[test]
fn windows_unterminated_double_quote_is_invalid_request() {
    let error = tokenize_windows("helper.exe \"abc").expect_err("must fail");
    assert_eq!(
        invalid_request_message(&error),
        "AWS credential_process command has an unterminated quote."
    );
}

#[test]
fn windows_trailing_backslashes_flushed() {
    let tokens = tokenize_windows("helper.exe C:\\path\\").expect("trailing kept");
    assert_eq!(tokens_text(&tokens), vec!["helper.exe", "C:\\path\\"]);
}

#[test]
fn dispatch_selects_platform_grammar() {
    let tokens = tokenize_credential_process_command("a b").expect("dispatch splits");
    assert_eq!(tokens_text(&tokens), vec!["a", "b"]);
    #[cfg(not(windows))]
    {
        let tokens = tokenize_credential_process_command("a\nb").expect("posix newline splits");
        assert_eq!(tokens_text(&tokens), vec!["a", "b"]);
    }
    #[cfg(windows)]
    {
        let tokens = tokenize_credential_process_command("a\nb").expect("windows keeps newline");
        assert_eq!(tokens_text(&tokens), vec!["a\nb"]);
    }
}

#[test]
fn split_empty_command_reports_profile_without_echoing_input() {
    let error = split_credential_process_argv("   \t  ", "dev").expect_err("empty must fail");
    let message = invalid_request_message(&error);
    assert!(message.contains("dev"), "names profile: {message}");
    assert!(!message.contains("  "), "echoes no input: {message}");
}

#[test]
fn batch_classifier_is_case_insensitive_cmd_bat() {
    assert!(is_windows_batch_script("helper.CMD"));
    assert!(is_windows_batch_script("C:\\tools\\run.Bat"));
    assert!(!is_windows_batch_script("helper.exe"));
    assert!(!is_windows_batch_script("helper.com"));
    assert!(!is_windows_batch_script("helper.cmdx"));
}

#[test]
fn batch_classifier_allows_exe_and_com() {
    assert!(!is_windows_batch_script("C:\\tools\\cred.exe"));
    assert!(!is_windows_batch_script("cred.com"));
}

#[tokio::test]
async fn runner_success_returns_exact_stdout() {
    let dir = temp_cwd("success");
    let env: &[(&str, &str)] = &[("PATH", "/usr/bin:/bin")];
    let args = boxed(&["hello"]);
    let req = CredentialProcessRequest {
        program: "/bin/echo",
        args: &args,
        cwd: &dir,
        env,
        profile: "test-success",
    };
    let out = run_credential_process(
        &req,
        &CancellationToken::new(),
        Instant::now() + Duration::from_secs(30),
    )
    .await
    .expect("echo succeeds");
    assert_eq!(out.as_ref(), b"hello\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn runner_environment_isolated_to_snapshot() {
    let dir = temp_cwd("env");
    let env: &[(&str, &str)] = &[("PATH", "/usr/bin:/bin"), ("HOME", "/tmp")];
    let args = boxed(&[]);
    let req = CredentialProcessRequest {
        program: "/usr/bin/env",
        args: &args,
        cwd: &dir,
        env,
        profile: "test-env",
    };
    let out = run_credential_process(
        &req,
        &CancellationToken::new(),
        Instant::now() + Duration::from_secs(30),
    )
    .await
    .expect("env succeeds");
    let text = String::from_utf8_lossy(&out);
    let mut got: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line.split_once('=').expect("env line has =");
        got.push((name.to_owned(), value.to_owned()));
    }
    got.sort();
    let mut want: Vec<(String, String)> = env
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    want.sort();
    assert_eq!(got, want, "child env equals snapshot exactly");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn runner_cwd_is_edge_captured_value() {
    let dir = temp_cwd("cwd");
    let env: &[(&str, &str)] = &[("PATH", "/usr/bin:/bin")];
    let args = boxed(&[]);
    let req = CredentialProcessRequest {
        program: "/bin/pwd",
        args: &args,
        cwd: &dir,
        env,
        profile: "test-cwd",
    };
    let out = run_credential_process(
        &req,
        &CancellationToken::new(),
        Instant::now() + Duration::from_secs(30),
    )
    .await
    .expect("pwd succeeds");
    let text = String::from_utf8_lossy(&out);
    let canonical = dir.canonicalize().expect("canonical cwd");
    assert_eq!(text.trim(), canonical.to_string_lossy().trim());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn runner_nonzero_exit_is_secret_free_transport() {
    let secret = "fake-secret-9f27c1-nonzero";
    let dir = temp_cwd("nonzero");
    let env: &[(&str, &str)] = &[("PATH", "/usr/bin:/bin")];
    let script = format!("echo {secret}; echo {secret} >&2; exit 1");
    let args = boxed(&["-c", &script]);
    let req = CredentialProcessRequest {
        program: "/bin/sh",
        args: &args,
        cwd: &dir,
        env,
        profile: "test-nonzero",
    };
    let error = run_credential_process(
        &req,
        &CancellationToken::new(),
        Instant::now() + Duration::from_secs(30),
    )
    .await
    .expect_err("nonzero must fail");
    let text = transport_reason(&error);
    assert!(!text.contains(secret), "no stdout in error: {text}");
    assert!(text.contains("test-nonzero"), "names profile: {text}");
    assert!(text.contains("status"), "names status only: {text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn runner_stdout_beyond_body_limit_returns_limit_without_truncated_bytes() {
    let dir = temp_cwd("limit");
    let env: &[(&str, &str)] = &[("PATH", "/usr/bin:/bin")];
    let want = BODY_LIMIT.saturating_add(1);
    let script = format!("head -c {want} /dev/zero | tr '\\0' 'A'; exit 0");
    let args = boxed(&["-c", &script]);
    let req = CredentialProcessRequest {
        program: "/bin/sh",
        args: &args,
        cwd: &dir,
        env,
        profile: "test-limit",
    };
    let error = run_credential_process(
        &req,
        &CancellationToken::new(),
        Instant::now() + Duration::from_secs(30),
    )
    .await
    .expect_err("over-limit must fail");
    match error {
        ProviderError::Limit(LimitError::Body) => {}
        other => panic!("expected Limit(Body), got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn runner_explicit_cancel_kills_and_reaps() {
    let dir = temp_cwd("cancel");
    let env: &[(&str, &str)] = &[("PATH", "/usr/bin:/bin")];
    let args = boxed(&["30"]);
    let req = CredentialProcessRequest {
        program: "/bin/sleep",
        args: &args,
        cwd: &dir,
        env,
        profile: "test-cancel",
    };
    let cancel = CancellationToken::new();
    let child_cancel = cancel.clone();
    #[expect(
        clippy::disallowed_methods,
        reason = "test edge: cancellation timer runs concurrently with process"
    )]
    let _stopper = tokio::spawn(async move {
        sleep(Duration::from_millis(200)).await;
        child_cancel.cancel();
    });
    let error = run_credential_process(&req, &cancel, Instant::now() + Duration::from_secs(30))
        .await
        .expect_err("cancel must fail");
    let text = transport_reason(&error);
    assert!(text.contains("test-cancel"), "names profile: {text}");
    assert!(text.contains("cancel"), "names cancel: {text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn runner_absolute_deadline_timeout_kills_and_reaps() {
    let dir = temp_cwd("deadline");
    let env: &[(&str, &str)] = &[("PATH", "/usr/bin:/bin")];
    let args = boxed(&["30"]);
    let req = CredentialProcessRequest {
        program: "/bin/sleep",
        args: &args,
        cwd: &dir,
        env,
        profile: "test-deadline",
    };
    let error = run_credential_process(
        &req,
        &CancellationToken::new(),
        Instant::now() + Duration::from_millis(300),
    )
    .await
    .expect_err("deadline must fail");
    let text = transport_reason(&error);
    assert!(text.contains("test-deadline"), "names profile: {text}");
    assert!(text.contains("deadline"), "names deadline: {text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn runner_leader_exit_with_descendant_holding_pipe_drains_promptly() {
    let dir = temp_cwd("descendant");
    let env: &[(&str, &str)] = &[("PATH", "/usr/bin:/bin")];
    let args = boxed(&["-c", "sleep 30 & echo hello"]);
    let req = CredentialProcessRequest {
        program: "/bin/sh",
        args: &args,
        cwd: &dir,
        env,
        profile: "test-descendant",
    };
    let out = run_credential_process(
        &req,
        &CancellationToken::new(),
        Instant::now() + Duration::from_secs(30),
    )
    .await
    .expect("leader exit drains after group kill");
    assert_eq!(out.as_ref(), b"hello\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn runner_future_drop_sends_termination_only_and_releases_slot() {
    let dir = temp_cwd("drop");
    let env: &[(&str, &str)] = &[("PATH", "/usr/bin:/bin")];
    let marker = format!(
        "DAL_AWS_PROCESS_DROP_{}_{}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let script = format!("echo {marker}; sleep 30");
    let args = boxed(&["-c", &script]);
    let req = CredentialProcessRequest {
        program: "/bin/sh",
        args: &args,
        cwd: &dir,
        env,
        profile: "test-drop",
    };
    let outcome = tokio::time::timeout(
        Duration::from_millis(500),
        run_credential_process(
            &req,
            &CancellationToken::new(),
            Instant::now() + Duration::from_secs(30),
        ),
    )
    .await;
    assert!(outcome.is_err(), "long fixture times out via drop");
    let mut gone = false;
    for _ in 0..50 {
        if !proc_cmdline_contains(&marker) {
            gone = true;
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    assert!(gone, "dropped future terminated the child");
    let args = boxed(&["0"]);
    let req = CredentialProcessRequest {
        program: "/bin/sleep",
        args: &args,
        cwd: &dir,
        env,
        profile: "test-drop",
    };
    run_credential_process(
        &req,
        &CancellationToken::new(),
        Instant::now() + Duration::from_secs(30),
    )
    .await
    .expect("slot released after drop");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(windows)]
#[tokio::test]
async fn windows_direct_exe_runs_without_shell() {
    let dir = temp_cwd("win-exe");
    let env: &[(&str, &str)] = &[("PATH", "C:\\Windows\\System32")];
    let args = boxed(&[]);
    let req = CredentialProcessRequest {
        program: "C:\\Windows\\System32\\whoami.exe",
        args: &args,
        cwd: &dir,
        env,
        profile: "test-win-exe",
    };
    let out = run_credential_process(
        &req,
        &CancellationToken::new(),
        Instant::now() + Duration::from_secs(30),
    )
    .await
    .expect("whoami.exe succeeds without shell");
    assert!(!out.is_empty(), "direct exe returns output");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(windows)]
#[tokio::test]
async fn windows_batch_refusal_names_shell_prerequisite() {
    let dir = temp_cwd("win-batch");
    let env: &[(&str, &str)] = &[("PATH", "C:\\Windows\\System32")];
    let args = boxed(&["--flag"]);
    let req = CredentialProcessRequest {
        program: "helper.cmd",
        args: &args,
        cwd: &dir,
        env,
        profile: "test-batch",
    };
    let error = run_credential_process(
        &req,
        &CancellationToken::new(),
        Instant::now() + Duration::from_secs(30),
    )
    .await
    .expect_err("batch must be refused");
    let text = transport_reason(&error);
    assert!(text.contains("test-batch"), "names profile: {text}");
    assert!(text.contains("cmd.exe"), "names prerequisite: {text}");
    let _ = std::fs::remove_dir_all(&dir);
}
