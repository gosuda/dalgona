// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Constants truth-table tests: every checked alias equals its owner
//! constant, and the table covers the contract's named rows.

use super::*;

#[test]
fn aliases_match_their_owners() {
    assert_eq!(LOOP_GUARD_WINDOW, super::super::stuck::RECORD_CAPACITY);
    assert_eq!(SIMILARITY_THRESHOLD, super::super::stuck::SIMILARITY_THRESHOLD);
    assert_eq!(SILENCE_THRESHOLD_S, super::super::stuck::SILENCE_THRESHOLD_S);
    assert_eq!(SLEEP_WAIT_THRESHOLD_S, super::super::stuck::sleep::SLEEP_WAIT_THRESHOLD_S);
    assert_eq!(SLEEP_WAIT_LOOP_THRESHOLD_S, super::super::stuck::sleep::SLEEP_WAIT_LOOP_THRESHOLD_S);
    assert_eq!(WRAPPER_UNWRAP_DEPTH, super::super::stuck::sleep::WRAPPER_UNWRAP_DEPTH);
    assert_eq!(SLEEP_FOREGROUND_WINDOW_S, super::super::stuck::sleep::FOREGROUND_WINDOW_S);
    assert_eq!(MONITOR_LINES, super::super::monitor::state::DEFAULT_MAX_LINES);
    assert_eq!(MONITOR_CHARS, super::super::monitor::state::DEFAULT_MAX_CHARS);
    assert_eq!(MONITOR_WAKE_BUDGET, super::super::monitor::state::DEFAULT_WAKE_BUDGET);
    assert_eq!(MONITOR_QUEUE_OVERHEAD, super::super::monitor::state::QUEUE_OVERHEAD);
    assert_eq!(MONITOR_FIRE_BUDGET, super::super::monitor::state::FIRE_BUDGET);
    assert_eq!(MONITOR_LINE_CAP, super::super::monitor::state::MAX_RETAINED_LINE);
    assert_eq!(LIVE_MONITORS, super::super::monitor::state::MAX_LIVE_MONITORS);
    assert_eq!(GOAL_CONSECUTIVE_CAP, super::super::goal::policy::CAP_TURNS);
    assert_eq!(GOAL_STALL_TURNS, super::super::goal::policy::STALL_TURNS);
    assert_eq!(GOAL_REPETITION_STREAK, super::super::goal::policy::REPETITION_STREAK);
    assert_eq!(GOAL_LENGTH_RECOVERY_LIMIT, super::super::goal::policy::LENGTH_RECOVERY_LIMIT);
    assert_eq!(GOAL_UNATTENDED_LIMIT, super::super::goal::policy::UNATTENDED_TURNS);
    assert_eq!(GOAL_USER_GRACE_MS, super::super::goal::policy::CONTINUATION_DELAY_MS);
    assert_eq!(GOAL_OBJECTIVE_LIMIT, super::super::goal::ops::MAX_OBJECTIVE);
    assert_eq!(GOAL_TURNS_BEFORE_BLOCKED, super::super::goal::ops::BLOCK_MIN_TURNS);
    assert_eq!(GOAL_RECENT_HASHES, super::super::goal::policy::RECENT_HASHES);
    assert_eq!(ARBITER_QUIET_AFTER_S, super::super::arbiter::QUIET_AFTER.as_secs());
    assert_eq!(ARBITER_WAITING_REPORTS, super::super::arbiter::WAITING_REPORTS);
    assert_eq!(ARBITER_OLDEST_REPORT_S, super::super::arbiter::OLDEST_REPORT_S);
    assert_eq!(INJECTION_BUDGET, super::super::arbiter::INJECTION_BUDGET);
    assert_eq!(
        STATUS_OBJECTIVE_PREVIEW,
        super::super::monitor::status::OBJECTIVE_PREVIEW
    );
    assert_eq!(STATUS_UPDATE_RATE, super::super::monitor::status::UPDATE_RATE);
    assert_eq!(RUN_NOTICE_LIMIT, super::super::delivery::RUN_NOTICE_LIMIT);
    assert_eq!(CHANGED_PATHS_SHOWN, super::super::delivery::CHANGED_SHOWN);
    assert_eq!(GRACE_TURN_S, super::super::pool::GRACE_SECONDS);
    assert_eq!(WAIT_TIMEOUT_DEFAULT_S, super::super::agents_tool::WAIT_TIMEOUT_DEFAULT_S);
    assert_eq!(WAIT_TIMEOUT_MAX_S, super::super::agents_tool::WAIT_TIMEOUT_MAX_S);
    assert_eq!(LIST_RUNS, super::super::agents_tool::LIST_RUNS_LIMIT);
    assert_eq!(LIST_TASK_LINES, super::super::agents_tool::LIST_TASK_LINES);
    assert_eq!(MIN_GIT_VERSION, super::super::worktree::MIN_GIT_VERSION);
    assert_eq!(CHILD_MAX_STEPS_DEFAULT, super::super::admission::CHILD_MAX_STEPS_DEFAULT);
    assert_eq!(CHILD_MAX_STEPS_MAX, super::super::admission::CHILD_MAX_STEPS_MAX);
    assert_eq!(CHILD_MAX_MINUTES_DEFAULT, super::super::admission::CHILD_MAX_MINUTES_DEFAULT);
    assert_eq!(CHILD_MAX_MINUTES_MAX, super::super::admission::CHILD_MAX_MINUTES_MAX);
    assert_eq!(MAX_RUNS_DEFAULT, super::super::admission::MAX_RUNS_DEFAULT);
    assert_eq!(MAX_RUNS_MAX, super::super::admission::MAX_RUNS_MAX);
    assert_eq!(WORKFLOW_STEPS, super::super::workflow::WORKFLOW_STEPS);
    assert_eq!(STEP_PROMPT_LIMIT, super::super::workflow::STEP_PROMPT_LIMIT);
    assert_eq!(POOL_ITEMS, super::super::workflow::POOL_ITEMS);
    assert_eq!(POOL_ITEM_LIMIT, super::super::workflow::POOL_ITEM_LIMIT);
    assert_eq!(POOL_WORKERS_MAX, super::super::workflow::POOL_WORKERS_MAX);
    assert_eq!(POOL_WORKERS_DEFAULT, super::super::workflow::POOL_WORKERS_DEFAULT);
    assert_eq!(STEP_TOOLS_MAX, super::super::workflow::STEP_TOOLS_MAX);
    assert_eq!(REPORT_LIMIT, super::super::agents_tool::REPORT_LIMIT);
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
    assert!(changed.is_some(), "Changed paths shown row missing");
    assert_eq!(
        super::super::delivery::CHANGED_SHOWN,
        super::CHANGED_PATHS_SHOWN
    );
}

#[test]
fn list_rows_state_the_page_caps() {
    let rows = constant_rows();
    let list = rows.iter().find(|row| row.name == "`list` runs");
    assert!(list.is_some(), "`list` runs row missing");
    assert_eq!(super::super::agents_tool::LIST_TASK_LINES, LIST_TASK_LINES);
}
