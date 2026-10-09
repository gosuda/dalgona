// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Report cell and approval-class tests.

use super::*;
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
    let AgentAction::Run { label, workflow } = action else {
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
