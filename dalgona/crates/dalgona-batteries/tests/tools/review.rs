// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::common::{Host, TestResult, exited, locked, run_tool};
use dalgona_batteries::review::{ReviewConfig, review};

async fn run_review(host: &Arc<Host>, args: &str) -> Result<String, Box<dyn std::error::Error>> {
    let extension = review(ReviewConfig::default())?;
    run_tool(&extension, host, "review", args).await
}

#[tokio::test]
async fn a_failing_git_command_reports_the_missing_repository() -> TestResult {
    let host = Host::with_runs([exited(128, "", "fatal: not a git repository")]);
    assert_eq!(
        run_review(&host, "{}").await?,
        "review needs a git repository: fatal: not a git repository"
    );
    assert_eq!(host.infer_calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn a_clean_workspace_never_calls_the_reviewer() -> TestResult {
    let host = Host::with_runs([exited(0, "", ""), exited(0, "", "")]);
    assert_eq!(run_review(&host, "{}").await?, "No changes to review.");
    assert_eq!(host.infer_calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn a_reviewer_failure_is_reported_once_with_a_capped_cause() -> TestResult {
    let host = Host::with_runs([
        exited(0, "diff --git a/x b/x\n+changed\n", ""),
        exited(0, " M x\n", ""),
    ]);
    *locked(&host.infer_error) = Some("é".repeat(150));
    let text = run_review(&host, r#"{"focus":"the x change"}"#).await?;
    let cause = text
        .strip_prefix("reviewer call failed: ")
        .ok_or("the failure is not a provider error")?;
    assert!(cause.len() <= 200);
    assert!(cause.starts_with('é'));
    assert_eq!(host.infer_calls.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn unknown_arguments_are_rejected_before_git_runs() -> TestResult {
    let host = Host::with_runs([exited(0, "", "")]);
    let text = run_review(&host, r#"{"depth":3}"#).await?;
    assert!(text.starts_with("review: invalid input"));
    assert_eq!(locked(&host.runs).len(), 1);
    Ok(())
}

fn seed_capped_session(host: &Arc<Host>, max_rounds: u8) -> TestResult {
    let session = dal_core::SessionId::new_v7();
    let mut records = locked(&host.records);
    let bodies = records.entry("review".to_owned()).or_default();
    for round in 1..=max_rounds {
        bodies.push(dal_core::RawJson::parse(&format!(
            r#"{{"session":"{session}","round":{round},"verdict":"findings","new_count":1,"findings":[{{"path":"src/lib.rs","line":7,"severity":"major","title":"Unchecked index","detail":"It can panic."}}]}}"#
        ))?);
    }
    Ok(())
}

#[tokio::test]
async fn a_call_after_the_round_cap_stops_and_reports_the_outstanding_findings() -> TestResult {
    let host = Host::with_runs([
        exited(0, "diff --git a/x b/x\n+changed\n", ""),
        exited(0, " M x\n", ""),
    ]);
    seed_capped_session(&host, ReviewConfig::default().max_rounds)?;
    let text = run_review(&host, "{}").await?;
    assert!(text.contains("reached the cap of 3 rounds"), "{text}");
    assert!(text.contains("src/lib.rs"), "{text}");
    assert!(text.contains("Unchecked index"), "{text}");
    assert_eq!(host.infer_calls.load(Ordering::SeqCst), 0);
    assert_eq!(locked(&host.runs).len(), 2);
    Ok(())
}

#[tokio::test]
async fn an_explicit_restart_after_the_round_cap_starts_a_new_session() -> TestResult {
    let host = Host::with_runs([exited(0, "", ""), exited(0, "", "")]);
    seed_capped_session(&host, ReviewConfig::default().max_rounds)?;
    let text = run_review(&host, r#"{"restart":true}"#).await?;
    assert_eq!(text, "No changes to review.");
    assert_eq!(locked(&host.runs).len(), 0);
    Ok(())
}
