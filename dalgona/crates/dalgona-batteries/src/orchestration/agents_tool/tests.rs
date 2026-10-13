// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Report cell and approval-class tests.

use std::path::{Path, PathBuf};

use super::*;
use dal_core::GrantSpec;
use dal_core::RawJson;

fn workflow(json: &str) -> Workflow {
    let raw = RawJson::parse(json).expect("test workflow is valid JSON");
    super::super::workflow::decode_steps(&raw, None, "test").expect("test workflow validates")
}

#[test]
fn report_first_valid_only() {
    let cell = ReportCell::default();
    assert_eq!(
        submit(&cell, ReportStatus::Done, "   "),
        (
            ReportOutcome::Empty,
            "report: the report is empty. Write what you found or changed."
        )
    );
    assert_eq!(cell.get(), None);
    assert_eq!(
        submit(
            &cell,
            ReportStatus::Blocked,
            "src/main.rs:1: stuck on input"
        ),
        (ReportOutcome::Stored, "Report received.")
    );
    assert_eq!(
        cell.get(),
        Some(Report {
            status: ReportStatus::Blocked,
            text: "src/main.rs:1: stuck on input".to_owned(),
        })
    );
    assert_eq!(
        submit(&cell, ReportStatus::Done, "a second try"),
        (
            ReportOutcome::Duplicate,
            "report: a report was already received. Your turn ends now."
        )
    );
    assert_eq!(
        cell.get().map(|report| report.status),
        Some(ReportStatus::Blocked)
    );
}

#[test]
fn admission_read_vs_exec() {
    let shared = workflow(r#"[{"name":"task","prompt":"work"}]"#);
    assert_eq!(
        approval_class(
            &AgentAction::Run {
                label: "run".to_owned(),
                workflow: shared,
                input: None,
            },
            Path::new("/ws"),
            None,
        ),
        ToolClass::Read
    );
    let worktree = workflow(r#"[{"name":"task","prompt":"work","tools":["read","patch"]}]"#);
    let run = AgentAction::Run {
        label: "run".to_owned(),
        workflow: worktree,
        input: None,
    };
    assert_eq!(
        approval_class(&run, Path::new("/ws"), None),
        ToolClass::Exec {
            read_only: false,
            grant: Some(GrantSpec {
                argv_prefix: "git".into(),
                roots: vec![PathBuf::from("/ws")],
            }),
        }
    );
    assert_eq!(
        approval_class(
            &AgentAction::List { ids: Vec::new() },
            Path::new("/ws"),
            None,
        ),
        ToolClass::Read
    );
}

#[test]
fn exec_grant_roots_add_the_shared_data_root_trees() {
    let worktree = workflow(r#"[{"name":"task","prompt":"work","isolation":"worktree"}]"#);
    let run = AgentAction::Run {
        label: "run".to_owned(),
        workflow: worktree,
        input: None,
    };
    assert_eq!(
        approval_class(&run, Path::new("/ws"), Some(Path::new("/data"))),
        ToolClass::Exec {
            read_only: false,
            grant: Some(GrantSpec {
                argv_prefix: "git".into(),
                roots: vec![
                    PathBuf::from("/ws"),
                    PathBuf::from("/data/worktrees"),
                    PathBuf::from("/data/isolation"),
                ],
            }),
        }
    );
}

fn args(json: &str) -> RawJson {
    RawJson::parse(json).expect("test args are valid JSON")
}

#[test]
fn agents_decode_run_inline_defaults_label() {
    let action = decode_action(
        &args(r#"{"action":"run","steps":[{"name":"find","prompt":"Work."}]}"#),
        None,
    )
    .expect("inline run decodes");
    let AgentAction::Run {
        label, workflow, ..
    } = action
    else {
        panic!("expected a run");
    };
    assert_eq!(label, "find");
    assert_eq!(workflow.steps.len(), 1);
}

#[test]
fn agents_decode_run_rejects_both_neither_and_input() {
    let both = decode_action(
        &args(r#"{"action":"run","steps":[{"name":"a","prompt":"W."}],"workflow":"saved"}"#),
        None,
    )
    .unwrap_err();
    assert_eq!(
        both.to_string(),
        "agents: action run needs steps or workflow, not both."
    );
    let neither = decode_action(&args(r#"{"action":"run"}"#), None).unwrap_err();
    assert_eq!(
        neither.to_string(),
        "agents: action run needs steps or workflow."
    );
    let input = decode_action(
        &args(r#"{"action":"run","steps":[{"name":"a","prompt":"W."}],"input":"x"}"#),
        None,
    )
    .unwrap_err();
    assert_eq!(
        input.to_string(),
        "agents: input applies only to a saved workflow."
    );
}

#[test]
fn agents_decode_rejects_unused_fields_and_missing_ids() {
    let unused = decode_action(
        &args(r#"{"action":"wait","ids":["j1"],"timeout":5,"steps":[]}"#),
        None,
    )
    .unwrap_err();
    assert_eq!(
        unused.to_string(),
        "agents: field steps does not apply to action wait."
    );
    let missing = decode_action(&args(r#"{"action":"cancel"}"#), None).unwrap_err();
    assert_eq!(missing.to_string(), "agents: action cancel needs ids.");
    let unknown = decode_action(&args(r#"{"action":"fly"}"#), None).unwrap_err();
    assert_eq!(
        unknown.to_string(),
        "agents: action fly is not one of run, wait, cancel, list."
    );
}

#[test]
fn agents_decode_wait_keeps_ids_and_timeout() {
    let action = decode_action(&args(r#"{"action":"wait","ids":["j3"],"timeout":5}"#), None)
        .expect("wait decodes");
    let AgentAction::Wait { ids, timeout_s } = action else {
        panic!("expected a wait");
    };
    assert_eq!(ids, ["j3"]);
    assert_eq!(timeout_s, 5);
}

#[test]
fn agents_run_result_text_matches_template() {
    assert_eq!(
        run_result_text("j1", "audit", 2, 5, &["find"]),
        "started run j1 \"audit\": 2 steps, 5 subagents planned, plus the items of step find.\nIts report arrives in one message when the run ends. Keep working; do not poll. Use agents wait only when you have nothing else to do."
    );
    assert_eq!(
        run_result_text("j2", "one", 1, 1, &[]),
        "started run j2 \"one\": 1 step, 1 subagent planned.\nIts report arrives in one message when the run ends. Keep working; do not poll. Use agents wait only when you have nothing else to do."
    );
}

#[test]
fn agents_error_texts_are_exact() {
    assert_eq!(
        unknown_id("j9"),
        "agents: no run or task j9 in this session."
    );
    assert_eq!(
        service_denied("jobs", "denied by policy"),
        "agents: the jobs service is not granted to orchestration: denied by policy."
    );
    assert_eq!(
        SUBAGENTS_OFF,
        "agents: subagents are off (agents = false in config.toml)."
    );
    assert_eq!(NO_NESTED_RUNS, "agents: a subagent cannot start subagents.");
}
