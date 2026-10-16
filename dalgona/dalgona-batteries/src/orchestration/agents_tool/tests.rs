// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Report cell, schema, and approval-class tests.
//!
//! The child-receipt / reject-before-launch regression lives in
//! `runtime/tests.rs` (the scripted `Host` records every `AgentsOp::Start`);
//! this file holds the schema-constant and decoder-level checks. The schema
//! tests read `AGENTS_SCHEMA` itself; they do not go through tool
//! registration, approval classification, or the provider wire.

use std::path::{Path, PathBuf};

use super::*;
use dal_core::GrantSpec;
use dal_core::RawJson;

fn workflow(json: &str) -> Workflow {
    let raw = RawJson::parse(json).expect("test workflow is valid JSON");
    super::super::workflow::decode_steps(&raw, None, "test").expect("test workflow validates")
}

/// Decodes one `agents` call, returning the run's workflow when the call is
/// a run; the assertion failure carries the rejection text.
fn run_workflow(json: &str) -> Result<Workflow, ActionError> {
    match decode_action(&args(json), None)? {
        AgentAction::Run { workflow, .. } => Ok(workflow),
        other => panic!("expected a run, got {other:?}"),
    }
}

/// Asserts one call rejects with exactly `expected`.
fn reject(json: &str, expected: &str) {
    let error = decode_action(&args(json), None)
        .err()
        .unwrap_or_else(|| panic!("expected rejection, decoded: {json}"));
    assert_eq!(error.to_string(), expected, "call: {json}");
}

/// Parses the `AGENTS_SCHEMA` constant; a parse failure fails the test.
fn schema_constant() -> sonic_rs::Value {
    sonic_rs::from_str(AGENTS_SCHEMA).expect("AGENTS_SCHEMA is valid JSON")
}

/// The schema object of one inline step (`steps.items`).
///
/// `Value::as_object` yields a `sonic_rs::Object`, whose inherent
/// `get<Q: AsRef<str>>(&self, key: &Q)` takes a reference to the key, so
/// the keys below are passed as `&"…"`. `Value::get` (the `JsonValueTrait`
/// method) takes the index by value instead.
fn step_schema(schema: &sonic_rs::Value) -> &sonic_rs::Object {
    let object = schema.as_object().expect("AGENTS_SCHEMA is an object");
    let top_level = object
        .get(&"properties")
        .and_then(|properties| properties.as_object())
        .expect("AGENTS_SCHEMA declares its top-level properties");
    let steps = top_level
        .get(&"steps")
        .and_then(|steps| steps.as_object())
        .expect("AGENTS_SCHEMA declares steps");
    steps
        .get(&"items")
        .and_then(|items| items.as_object())
        .expect("steps declares its items schema")
}

/// The `properties` object of one inline step.
fn step_properties(schema: &sonic_rs::Value) -> &sonic_rs::Object {
    step_schema(schema)
        .get(&"properties")
        .and_then(|properties| properties.as_object())
        .expect("the step schema declares its properties")
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
        "agents: subagents are off (agents = false in dal.toml)."
    );
    assert_eq!(NO_NESTED_RUNS, "agents: a subagent cannot start subagents.");
}

/// The decoder accepts a string `role` or `system`, so the schema constant
/// must declare both as plain strings (a missing member or a narrower type
/// would hide a decodable field from the model). Member order and
/// description text are not part of that contract.
#[test]
fn agents_schema_declares_role_and_system_as_strings() {
    let schema = schema_constant();
    let properties = step_properties(&schema);
    for member in ["role", "system"] {
        let declared = properties
            .get(&member)
            .unwrap_or_else(|| panic!("AGENTS_SCHEMA step has no {member} property"));
        assert_eq!(
            declared.get("type").and_then(JsonValueTrait::as_str),
            Some("string"),
            "{member} is declared as a string"
        );
    }
}

/// Every member the step schema declares must be a field the decoder knows:
/// the decoder refuses an unknown field before it reads any value, so a null
/// probes only the name. The control proves the probe can tell the two apart.
/// This covers the schema-to-decoder direction only; the decoder's accepted
/// set is private to the workflow module.
#[test]
fn agents_schema_step_members_are_fields_the_decoder_knows() {
    let rejected_as_unknown = |member: &str| {
        let json =
            format!(r#"{{"action":"run","steps":[{{"name":"a","prompt":"w","{member}":null}}]}}"#);
        decode_action(&args(&json), None)
            .err()
            .is_some_and(|error| {
                error.to_string() == format!("agents: step a: unknown field \"{member}\".")
            })
    };
    assert!(
        rejected_as_unknown("no_such_member"),
        "the probe must detect a field the decoder does not know"
    );
    let schema = schema_constant();
    for (member, _) in step_properties(&schema) {
        if member == "name" || member == "prompt" {
            continue;
        }
        assert!(
            !rejected_as_unknown(member),
            "the schema declares {member}, which the decoder rejects as unknown"
        );
    }
}

/// The step schema stays closed and still requires exactly a name and a
/// prompt, whatever order either list is written in.
#[test]
fn agents_schema_keeps_the_step_restrictions() {
    let schema = schema_constant();
    let shape = step_schema(&schema);
    let listed = shape
        .get(&"required")
        .and_then(|required| required.as_array())
        .expect("the step schema keeps its required list");
    let mut required: Vec<&str> = listed.iter().filter_map(JsonValueTrait::as_str).collect();
    required.sort_unstable();
    assert_eq!(required, ["name", "prompt"]);
    assert_eq!(
        shape
            .get(&"additionalProperties")
            .and_then(JsonValueTrait::as_bool),
        Some(false),
        "steps stay closed to unknown members"
    );
}

#[test]
fn agents_decode_run_accepts_role_only() {
    let workflow = run_workflow(
        r#"{"action":"run","steps":[{"name":"review","prompt":"work","role":"reviewer","isolation":"shared"}]}"#,
    )
    .expect("a role-only run decodes");
    assert_eq!(workflow.steps[0].role.as_deref(), Some("reviewer"));
    assert_eq!(workflow.steps[0].system, None);
}

#[test]
fn agents_decode_run_accepts_system_only() {
    let workflow = run_workflow(
        r#"{"action":"run","steps":[{"name":"audit","prompt":"work","system":"Be concise.","isolation":"shared"}]}"#,
    )
    .expect("a system-only run decodes");
    assert_eq!(workflow.steps[0].system.as_deref(), Some("Be concise."));
    assert_eq!(workflow.steps[0].role, None);
}

/// The schema declares `role` and `system` as strings, and the decoder
/// enforces that: a value of another type is refused, not skipped.
#[test]
fn agents_decode_run_rejects_non_string_role_and_system() {
    for (member, value) in [
        ("role", "7"),
        ("role", "null"),
        ("system", "true"),
        ("system", "null"),
    ] {
        let json = format!(
            r#"{{"action":"run","steps":[{{"name":"step","prompt":"work","{member}":{value}}}]}}"#
        );
        reject(
            &json,
            &format!("agents: step step: {member} must be a string."),
        );
    }
}

#[test]
fn agents_decode_run_rejects_role_and_system_together() {
    reject(
        r#"{"action":"run","steps":[{"name":"step","prompt":"work","role":"reviewer","system":"be concise"}]}"#,
        "agents: step step: role and system cannot both be set.",
    );
}
