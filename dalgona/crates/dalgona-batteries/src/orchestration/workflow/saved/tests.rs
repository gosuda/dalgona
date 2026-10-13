// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Saved workflow lookup tests.

use super::*;
use dal_core::RawJson;

fn table(json: &str) -> RawJson {
    RawJson::parse(json).expect("test table is valid JSON")
}

fn valid_table() -> RawJson {
    table(
        r#"{
            "audit": {
                "description": "Audit routes.",
                "steps": [{"name": "find", "prompt": "List {{input}}."}]
            },
            "plain": {"steps": [{"name": "one", "prompt": "Work."}]},
            "broken": {"steps": []},
            "wordy": {
                "description": "This description runs far past the two hundred character ceiling on purpose so the saved workflow validation rejects it with the description reason instead of accepting it as a usable workflow entry in the table.",
                "steps": [{"name": "one", "prompt": "Work."}]
            }
        }"#,
    )
}

#[test]
fn saved_unknown_name_lists_available() {
    let error = find_saved(&valid_table(), "missing", None).unwrap_err();
    assert_eq!(
        error.to_string(),
        "agents: no saved workflow \"missing\". Saved workflows: audit, broken, plain, wordy."
    );
    let empty = table("{}");
    let error = find_saved(&empty, "missing", None).unwrap_err();
    assert_eq!(
        error.to_string(),
        "agents: no saved workflow \"missing\". Saved workflows: none."
    );
}

#[test]
fn saved_invalid_steps_name_reason_and_fix() {
    let error = find_saved(&valid_table(), "broken", None).unwrap_err();
    assert_eq!(
        error.to_string(),
        "agents: saved workflow \"broken\" is invalid: agents: steps must hold 1 to 32 steps. Fix [plugin.orchestration.workflows.broken] in config.toml."
    );
}

#[test]
fn saved_input_rule_uses_saved_text() {
    let error = find_saved(&valid_table(), "audit", None).unwrap_err();
    assert_eq!(
        error.to_string(),
        "agents: saved workflow \"audit\" needs input."
    );
    let workflow = find_saved(&valid_table(), "audit", Some("routes")).expect("input given");
    assert_eq!(workflow.label, "audit");
}
