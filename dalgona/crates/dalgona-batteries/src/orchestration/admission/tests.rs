// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Run admission tests: limits, quota, and spec ordering.

use super::*;
use dal_core::RawJson;

fn workflow(json: &str) -> Workflow {
    let raw = RawJson::parse(json).expect("test workflow is valid JSON");
    super::super::workflow::decode_steps(&raw, None, "test").expect("test workflow validates")
}

fn limits() -> Limits {
    Limits {
        max_runs: 2,
        agents_per_session: 10,
        session_used: 0,
    }
}

#[test]
fn admission_second_run_at_limit_fails() {
    let workflow = workflow(r#"[{"name":"task","prompt":"work"}]"#);
    let error = admit(&workflow, 2, &limits(), |_| Ok(())).unwrap_err();
    assert_eq!(
        error.to_string(),
        "agents: 2 runs are active; the limit is plugin.orchestration.max_runs = 2. Wait for a run to end, or cancel one."
    );
}

#[test]
fn admission_quota_shortfall_names_need_left_and_limit() {
    let workflow =
        workflow(r#"[{"name":"pool","prompt":"do {{item}}","items":["a","b","c"],"workers":3}]"#);
    let limited = Limits {
        agents_per_session: 2,
        ..limits()
    };
    let error = admit(&workflow, 0, &limited, |_| Ok(())).unwrap_err();
    assert_eq!(
        error.to_string(),
        "agents: this run needs 3 subagents, but the session has 2 left of agents_per_session = 2. Start a smaller run."
    );
}

#[test]
fn admission_first_spec_error_wins_in_declaration_order() {
    let workflow = workflow(r#"[{"name":"one","prompt":"work"},{"name":"two","prompt":"work"}]"#);
    let error = admit(&workflow, 0, &limits(), |step| {
        if step.name == "one" {
            Err("model unknown".to_owned())
        } else {
            Err("tool missing".to_owned())
        }
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "agents: step one: model unknown");
}

#[test]
fn admission_success_returns_planned_count_and_records_nothing() {
    let workflow = workflow(
        r#"[{"name":"task","prompt":"work"},{"name":"pool","prompt":"do {{item}}","items":["a","b"]}]"#,
    );
    assert_eq!(admit(&workflow, 1, &limits(), |_| Ok(())), Ok(3));
}

fn settings(json: &str) -> Result<Settings, String> {
    let raw = RawJson::parse(json).expect("test settings are valid JSON");
    decode_settings(&raw)
}

#[test]
fn admission_settings_defaults_and_ranges() {
    assert_eq!(
        settings("{}"),
        Ok(Settings {
            child_max_steps: 50,
            child_max_minutes: 30,
            max_runs: 16,
        })
    );
    assert_eq!(
        settings(r#"{"max_runs": 4}"#)
            .expect("partial table keeps defaults")
            .max_runs,
        4
    );
    assert_eq!(
        settings(r#"{"max_runs": 0}"#).unwrap_err(),
        "plugin.orchestration.max_runs must be an integer from 1 to 64."
    );
    assert_eq!(
        settings(r#"{"child_max_steps": 1001}"#).unwrap_err(),
        "plugin.orchestration.child_max_steps must be an integer from 1 to 1000."
    );
    assert_eq!(
        settings(r#"{"child_max_minutes": "soon"}"#)
            .expect("mistyped member falls back")
            .child_max_minutes,
        30
    );
}
