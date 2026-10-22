// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Constants truth-table tests: every checked alias equals its owner
//! constant, and the table covers the contract's named rows.

use super::*;

#[test]
fn every_constant_aliases_its_owner_except_the_one_documented_literal() {
    const SOURCE: &str = include_str!("../constants.rs");
    let mut consts: Vec<(String, String)> = Vec::new();
    let mut open: Option<(String, String)> = None;
    for line in SOURCE.lines() {
        if let Some((name, value)) = open.as_mut() {
            value.push_str(line.trim());
            if line.trim_end().ends_with(';') {
                consts.push((std::mem::take(name), std::mem::take(value)));
                open = None;
            }
            continue;
        }
        let Some(rest) = line.strip_prefix("pub(crate) const ") else {
            continue;
        };
        let (name, tail) = rest.split_once(':').expect("a typed const");
        let value = tail.split_once('=').expect("an initialized const").1.trim();
        if line.trim_end().ends_with(';') {
            consts.push((name.to_owned(), value.to_owned()));
        } else {
            open = Some((name.to_owned(), value.to_owned()));
        }
    }
    assert!(
        consts.len() >= 50,
        "the scan must see the table's constants, saw {}",
        consts.len()
    );
    let literals: Vec<&str> = consts
        .iter()
        .filter(|(_, value)| !value.starts_with("super::"))
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(
        literals,
        ["BATTERY_MAILBOX"],
        "every other constant must alias the module that enforces it; a twin literal can drift from its owner"
    );
}

#[test]
fn the_mailbox_literal_is_the_documented_capacity() {
    assert_eq!(BATTERY_MAILBOX, 256);
}

#[test]
fn checked_table_covers_the_owner_bound_rows() {
    const NAMES: [&str; 52] = [
        "Loop guard window",
        "Identical run threshold",
        "Notices before block",
        "Blocked calls before hard stop",
        "Similar run threshold",
        "Similarity threshold",
        "Cycle periods",
        "Cycle repetitions",
        "Notice escalation factor",
        "Silence threshold",
        "Sleep-wait threshold",
        "Sleep-wait loop threshold",
        "Wrapper unwrap depth",
        "Sleep-wait foreground window",
        "Monitor coalesce",
        "Monitor rate limit",
        "Monitor lines per injection",
        "Monitor characters per injection",
        "Monitor queue overhead",
        "Monitor wake budget",
        "Monitor fire budget",
        "Monitor line cap",
        "Live monitors per session",
        "Goal consecutive cap",
        "Goal stall toolless threshold",
        "Goal repetition hash streak",
        "Goal length recovery limit",
        "Goal unattended limit",
        "Goal user grace",
        "Goal objective limit",
        "Goal turns before blocked",
        "Goal recent hashes kept",
        "Status objective preview",
        "Arbiter quiet after the last job end",
        "Arbiter waiting-reports trigger",
        "Arbiter oldest-report trigger",
        "Injection budget",
        "Status update rate",
        "Battery mailbox",
        "Workflow steps",
        "Prompt length",
        "Pool items",
        "Pool workers",
        "Step tools",
        "Report length",
        "child_max_steps",
        "child_max_minutes",
        "max_runs",
        "Grace turn",
        "Run notice",
        "`wait` timeout",
        "Minimum git",
    ];
    let rows = constant_rows();
    for name in NAMES {
        assert!(
            rows.iter().any(|row| row.name == name),
            "checked table is missing constant {name:?}"
        );
    }
    assert!(rows.iter().all(|row| !row.value.is_empty()));
}

#[test]
fn formerly_cited_rows_are_checked() {
    let rows = constant_rows();
    for name in [
        "Status objective preview",
        "Status update rate",
        "Arbiter waiting-reports trigger",
        "Arbiter oldest-report trigger",
    ] {
        assert!(
            rows.iter().any(|row| row.name == name),
            "checked table is missing constant {name:?}"
        );
    }
}

#[test]
fn changed_paths_row_lists_the_fold() {
    let rows = constant_rows();
    let changed = rows.iter().find(|row| row.name == "Changed paths shown");
    assert_eq!(
        changed.map(|row| row.value.as_str()),
        Some(
            format!(
                "{}, then `(+<k> more)`",
                super::super::delivery::CHANGED_SHOWN
            )
            .as_str()
        )
    );
}

#[test]
fn list_rows_state_the_page_caps() {
    let rows = constant_rows();
    let list = rows.iter().find(|row| row.name == "`list` runs");
    assert_eq!(
        list.map(|row| row.value.as_str()),
        Some(
            format!(
                "{}; {} task lines per run",
                super::super::agents_tool::LIST_RUNS_LIMIT,
                super::super::agents_tool::LIST_TASK_LINES
            )
            .as_str()
        )
    );
}
