// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! The battery's constants: every value is an alias of the owner constant
//! in the module that enforces it (no twin literals). The
//! `dalgona://orchestration` page renders this table and the gate test
//! reads both sides from here, so page and behavior cannot drift apart.

/// Loop-guard records kept per session.
pub(crate) const LOOP_GUARD_WINDOW: usize = super::stuck::RECORD_CAPACITY;
/// Identical calls in a row that admit a notice.
pub(crate) const IDENTICAL_RUN: usize = super::stuck::IDENTICAL_RUN;
/// Admitted notices before a block arms.
pub(crate) const NOTICES_BEFORE_BLOCK: usize = super::stuck::NOTICES_BEFORE_BLOCK;
/// Blocked calls before a hard stop.
pub(crate) const BLOCKED_BEFORE_HARD_STOP: u32 = super::stuck::BLOCKED_BEFORE_HARD_STOP;
/// Near-identical calls in a row that admit a notice.
pub(crate) const SIMILAR_RUN: usize = super::stuck::SIMILAR_RUN;
/// Mean adjacent bigram Dice that counts as similar.
pub(crate) const SIMILARITY_THRESHOLD: f64 = super::stuck::SIMILARITY_THRESHOLD;
/// Cycle periods the cycle detector admits.
pub(crate) const CYCLE_PERIODS: std::ops::RangeInclusive<usize> = super::stuck::CYCLE_PERIODS;
/// Full cycle repetitions before a cycle notice.
pub(crate) const CYCLE_REPETITIONS: usize = super::stuck::CYCLE_REPETITIONS;
/// How much a notice count must grow before the gate readsmits it.
pub(crate) const NOTICE_ESCALATION_FACTOR: u32 = super::stuck::NOTICE_ESCALATION_FACTOR;
/// Quiet seconds before a background job reports as silent.
pub(crate) const SILENCE_THRESHOLD_S: u64 = super::stuck::SILENCE_THRESHOLD_S;
/// A bare or compound sleep that detaches.
pub(crate) const SLEEP_WAIT_THRESHOLD_S: f64 = super::stuck::sleep::SLEEP_WAIT_THRESHOLD_S;
/// A sleep inside a loop that detaches.
pub(crate) const SLEEP_WAIT_LOOP_THRESHOLD_S: f64 =
    super::stuck::sleep::SLEEP_WAIT_LOOP_THRESHOLD_S;
/// Shell wrappers removed before classification.
pub(crate) const WRAPPER_UNWRAP_DEPTH: usize = super::stuck::sleep::WRAPPER_UNWRAP_DEPTH;
/// Foreground window a classified sleep is rewritten to.
pub(crate) const SLEEP_FOREGROUND_WINDOW_S: u64 = super::stuck::sleep::FOREGROUND_WINDOW_S;
/// Monitor lines flushed per injection.
pub(crate) const MONITOR_LINES: usize = super::monitor::state::DEFAULT_MAX_LINES;
/// Monitor characters buffered per injection.
pub(crate) const MONITOR_CHARS: usize = super::monitor::state::DEFAULT_MAX_CHARS;
/// Extra characters a monitor line may spend over the cap.
pub(crate) const MONITOR_QUEUE_OVERHEAD: u64 = super::monitor::state::QUEUE_OVERHEAD;
/// Monitor-only wakes between user turns.
pub(crate) const MONITOR_WAKE_BUDGET: u16 = super::monitor::state::DEFAULT_WAKE_BUDGET;
/// Live monitors one session may hold.
pub(crate) const LIVE_MONITORS: usize = super::monitor::state::MAX_LIVE_MONITORS;
/// Monitor fires allowed per 24 hours before the auto-mute.
pub(crate) const MONITOR_FIRE_BUDGET: usize = super::monitor::state::FIRE_BUDGET;
/// Characters of one output line kept for matching.
pub(crate) const MONITOR_LINE_CAP: usize = super::monitor::state::MAX_RETAINED_LINE;
/// Consecutive continuations on one signature before the cap blocks.
pub(crate) const GOAL_CONSECUTIVE_CAP: u32 = super::goal::policy::CAP_TURNS;
/// Tool-less goal turns before the stall notice rides a continuation.
pub(crate) const GOAL_STALL_TURNS: u32 = super::goal::policy::STALL_TURNS;
/// Repeated output hashes before the repetition block.
pub(crate) const GOAL_REPETITION_STREAK: usize = super::goal::policy::REPETITION_STREAK;
/// Length recoveries on one signature before truncation blocks.
pub(crate) const GOAL_LENGTH_RECOVERY_LIMIT: u32 = super::goal::policy::LENGTH_RECOVERY_LIMIT;
/// Continuations without a user message before the unattended block.
pub(crate) const GOAL_UNATTENDED_LIMIT: u32 = super::goal::policy::UNATTENDED_TURNS;
/// Grace after a user turn before an automatic continuation fires.
pub(crate) const GOAL_USER_GRACE_MS: u64 = super::goal::policy::CONTINUATION_DELAY_MS;
/// Unicode scalar values one objective may hold.
pub(crate) const GOAL_OBJECTIVE_LIMIT: usize = super::goal::ops::MAX_OBJECTIVE;
/// Goal turns before the goal refuses to mark blocked.
pub(crate) const GOAL_TURNS_BEFORE_BLOCKED: u32 = super::goal::ops::BLOCK_MIN_TURNS;
/// Recent output hashes kept for repetition detection.
pub(crate) const GOAL_RECENT_HASHES: usize = super::goal::policy::RECENT_HASHES;
/// Characters of the goal objective the status payload carries.
pub(crate) const STATUS_OBJECTIVE_PREVIEW: usize = super::monitor::status::OBJECTIVE_PREVIEW;
/// Quiet seconds after the last activity before a wake may fire.
pub(crate) const ARBITER_QUIET_AFTER_S: u64 = super::arbiter::QUIET_AFTER.as_secs();
/// Waiting job reports that wake the session on their own.
pub(crate) const ARBITER_WAITING_REPORTS: usize = super::arbiter::WAITING_REPORTS;
/// Age in seconds of the oldest waiting report that wakes the session.
pub(crate) const ARBITER_OLDEST_REPORT_S: u64 = super::arbiter::OLDEST_REPORT_S;
/// Bytes one injection may hold.
pub(crate) const INJECTION_BUDGET: usize = super::arbiter::INJECTION_BUDGET;
/// Status updates per second the battery emits at most.
pub(crate) const STATUS_UPDATE_RATE: u32 = super::monitor::status::UPDATE_RATE;
/// Messages one battery mailbox holds.
pub(crate) const BATTERY_MAILBOX: usize = 256;
/// Steps one workflow may declare.
pub(crate) const WORKFLOW_STEPS: usize = super::workflow::WORKFLOW_STEPS;
/// Unicode scalar values one step prompt may hold.
pub(crate) const STEP_PROMPT_LIMIT: usize = super::workflow::STEP_PROMPT_LIMIT;
/// Items one pool may hold.
pub(crate) const POOL_ITEMS: usize = super::workflow::POOL_ITEMS;
/// Characters one pool item may hold.
pub(crate) const POOL_ITEM_LIMIT: usize = super::workflow::POOL_ITEM_LIMIT;
/// Concurrent subagents one pool runs at most.
pub(crate) const POOL_WORKERS_MAX: usize = super::workflow::POOL_WORKERS_MAX;
/// Concurrent subagents one pool runs by default.
pub(crate) const POOL_WORKERS_DEFAULT: u8 = super::workflow::POOL_WORKERS_DEFAULT;
/// Names one step's tool list may hold.
pub(crate) const STEP_TOOLS_MAX: usize = super::workflow::STEP_TOOLS_MAX;
/// Characters one child report may hold.
pub(crate) const REPORT_LIMIT: usize = super::agents_tool::REPORT_LIMIT;
/// Default tool rounds one child turn may spend.
pub(crate) const CHILD_MAX_STEPS_DEFAULT: u32 = super::admission::CHILD_MAX_STEPS_DEFAULT;
/// Tool rounds one child turn may spend at most.
pub(crate) const CHILD_MAX_STEPS_MAX: u32 = super::admission::CHILD_MAX_STEPS_MAX;
/// Default minutes one child may run.
pub(crate) const CHILD_MAX_MINUTES_DEFAULT: u32 = super::admission::CHILD_MAX_MINUTES_DEFAULT;
/// Minutes one child may run at most.
pub(crate) const CHILD_MAX_MINUTES_MAX: u32 = super::admission::CHILD_MAX_MINUTES_MAX;
/// Default runs one session holds at once.
pub(crate) const MAX_RUNS_DEFAULT: u32 = super::admission::MAX_RUNS_DEFAULT;
/// Runs one session holds at once at most.
pub(crate) const MAX_RUNS_MAX: u32 = super::admission::MAX_RUNS_MAX;
/// Seconds before a silent child gets its one grace turn.
pub(crate) const GRACE_TURN_S: f64 = super::pool::GRACE_SECONDS;
/// Bytes one run notice may hold.
pub(crate) const RUN_NOTICE_LIMIT: usize = super::delivery::RUN_NOTICE_LIMIT;
/// Changed paths shown per task before folding the remainder.
pub(crate) const CHANGED_PATHS_SHOWN: usize = super::delivery::CHANGED_SHOWN;
/// Default seconds one `wait` may block.
pub(crate) const WAIT_TIMEOUT_DEFAULT_S: u16 = super::agents_tool::WAIT_TIMEOUT_DEFAULT_S;
/// Seconds one `wait` may block at most.
pub(crate) const WAIT_TIMEOUT_MAX_S: u16 = super::agents_tool::WAIT_TIMEOUT_MAX_S;
/// Runs one `list` shows.
pub(crate) const LIST_RUNS: usize = super::agents_tool::LIST_RUNS_LIMIT;
/// Task lines one `list` shows per run.
pub(crate) const LIST_TASK_LINES: usize = super::agents_tool::LIST_TASK_LINES;
/// Minimum git version the worktree path accepts.
pub(crate) const MIN_GIT_VERSION: (u32, u32) = super::worktree::MIN_GIT_VERSION;

/// One constant row of the truth table: name and rendered value.
pub(crate) struct ConstantRow {
    pub(crate) name: &'static str,
    pub(crate) value: String,
}

fn guard_rows(rows: &mut Vec<ConstantRow>) {
    rows.push(ConstantRow {
        name: "Loop guard window",
        value: format!("{LOOP_GUARD_WINDOW} records"),
    });
    rows.push(ConstantRow {
        name: "Identical run threshold",
        value: IDENTICAL_RUN.to_string(),
    });
    rows.push(ConstantRow {
        name: "Notices before block",
        value: NOTICES_BEFORE_BLOCK.to_string(),
    });
    rows.push(ConstantRow {
        name: "Blocked calls before hard stop",
        value: BLOCKED_BEFORE_HARD_STOP.to_string(),
    });
    rows.push(ConstantRow {
        name: "Similar run threshold",
        value: SIMILAR_RUN.to_string(),
    });
    rows.push(ConstantRow {
        name: "Similarity threshold",
        value: SIMILARITY_THRESHOLD.to_string(),
    });
    rows.push(ConstantRow {
        name: "Cycle periods",
        value: format!("{}..={}", CYCLE_PERIODS.start(), CYCLE_PERIODS.end()),
    });
    rows.push(ConstantRow {
        name: "Cycle repetitions",
        value: CYCLE_REPETITIONS.to_string(),
    });
    rows.push(ConstantRow {
        name: "Notice escalation factor",
        value: NOTICE_ESCALATION_FACTOR.to_string(),
    });
    rows.push(ConstantRow {
        name: "Silence threshold",
        value: format!("{SILENCE_THRESHOLD_S} s"),
    });
    rows.push(ConstantRow {
        name: "Sleep-wait threshold",
        value: format!("{SLEEP_WAIT_THRESHOLD_S} s"),
    });
    rows.push(ConstantRow {
        name: "Sleep-wait loop threshold",
        value: format!("{SLEEP_WAIT_LOOP_THRESHOLD_S} s"),
    });
    rows.push(ConstantRow {
        name: "Wrapper unwrap depth",
        value: WRAPPER_UNWRAP_DEPTH.to_string(),
    });
    rows.push(ConstantRow {
        name: "Sleep-wait foreground window",
        value: format!("{SLEEP_FOREGROUND_WINDOW_S} s"),
    });
}
fn monitor_rows(rows: &mut Vec<ConstantRow>) {
    const MONITOR_LINE_CLAMP: &str = "clamp 1..=200";
    const MONITOR_CHAR_CLAMP: &str = "clamp 512..=16384";
    const MONITOR_WAKE_CLAMP: &str = "clamp 1..=100";
    const MONITOR_COALESCE_CLAMP: &str = "clamp 1..=60000";
    const MONITOR_RATE_CLAMP: &str = "clamp 1..=3600000";
    rows.push(ConstantRow {
        name: "Monitor coalesce",
        value: format!(
            "{} ms, {}",
            super::monitor::state::DEFAULT_COALESCE_MS,
            MONITOR_COALESCE_CLAMP
        ),
    });
    rows.push(ConstantRow {
        name: "Monitor rate limit",
        value: format!(
            "{} ms, {}",
            super::monitor::state::DEFAULT_RATE_LIMIT_MS,
            MONITOR_RATE_CLAMP
        ),
    });
    rows.push(ConstantRow {
        name: "Monitor lines per injection",
        value: format!("{MONITOR_LINES}, {MONITOR_LINE_CLAMP}"),
    });
    rows.push(ConstantRow {
        name: "Monitor characters per injection",
        value: format!("{MONITOR_CHARS}, {MONITOR_CHAR_CLAMP}"),
    });
    rows.push(ConstantRow {
        name: "Monitor queue overhead",
        value: format!("{MONITOR_QUEUE_OVERHEAD} chars"),
    });
    rows.push(ConstantRow {
        name: "Monitor wake budget",
        value: format!("{MONITOR_WAKE_BUDGET}, {MONITOR_WAKE_CLAMP}"),
    });
    rows.push(ConstantRow {
        name: "Monitor fire budget",
        value: format!("{MONITOR_FIRE_BUDGET} per 24 h"),
    });
    rows.push(ConstantRow {
        name: "Monitor line cap",
        value: format!("{MONITOR_LINE_CAP} chars"),
    });
    rows.push(ConstantRow {
        name: "Live monitors per session",
        value: LIVE_MONITORS.to_string(),
    });
}
fn goal_rows(rows: &mut Vec<ConstantRow>) {
    rows.push(ConstantRow {
        name: "Goal consecutive cap",
        value: GOAL_CONSECUTIVE_CAP.to_string(),
    });
    rows.push(ConstantRow {
        name: "Goal stall toolless threshold",
        value: GOAL_STALL_TURNS.to_string(),
    });
    rows.push(ConstantRow {
        name: "Goal repetition hash streak",
        value: GOAL_REPETITION_STREAK.to_string(),
    });
    rows.push(ConstantRow {
        name: "Goal length recovery limit",
        value: GOAL_LENGTH_RECOVERY_LIMIT.to_string(),
    });
    rows.push(ConstantRow {
        name: "Goal unattended limit",
        value: GOAL_UNATTENDED_LIMIT.to_string(),
    });
    rows.push(ConstantRow {
        name: "Goal user grace",
        value: format!("{GOAL_USER_GRACE_MS} ms"),
    });
    rows.push(ConstantRow {
        name: "Goal objective limit",
        value: format!("{GOAL_OBJECTIVE_LIMIT} chars"),
    });
    rows.push(ConstantRow {
        name: "Goal turns before blocked",
        value: GOAL_TURNS_BEFORE_BLOCKED.to_string(),
    });
    rows.push(ConstantRow {
        name: "Goal recent hashes kept",
        value: GOAL_RECENT_HASHES.to_string(),
    });
    rows.push(ConstantRow {
        name: "Status objective preview",
        value: format!("{STATUS_OBJECTIVE_PREVIEW} chars"),
    });
    rows.push(ConstantRow {
        name: "Arbiter quiet after the last job end",
        value: format!("{ARBITER_QUIET_AFTER_S} s"),
    });
    rows.push(ConstantRow {
        name: "Arbiter waiting-reports trigger",
        value: ARBITER_WAITING_REPORTS.to_string(),
    });
    rows.push(ConstantRow {
        name: "Arbiter oldest-report trigger",
        value: format!("{ARBITER_OLDEST_REPORT_S} s"),
    });
    rows.push(ConstantRow {
        name: "Injection budget",
        value: format!("{INJECTION_BUDGET} bytes"),
    });
    rows.push(ConstantRow {
        name: "Status update rate",
        value: format!("{STATUS_UPDATE_RATE} per second"),
    });
}
fn workflow_rows(rows: &mut Vec<ConstantRow>) {
    rows.push(ConstantRow {
        name: "Battery mailbox",
        value: BATTERY_MAILBOX.to_string(),
    });
    rows.push(ConstantRow {
        name: "Workflow steps",
        value: format!("1..={WORKFLOW_STEPS}"),
    });
    rows.push(ConstantRow {
        name: "Prompt length",
        value: format!("1..={STEP_PROMPT_LIMIT} chars"),
    });
    rows.push(ConstantRow {
        name: "Pool items",
        value: format!("1..={POOL_ITEMS}, each 1..={POOL_ITEM_LIMIT} chars"),
    });
    rows.push(ConstantRow {
        name: "Pool workers",
        value: format!("1..={POOL_WORKERS_MAX}, default {POOL_WORKERS_DEFAULT}"),
    });
    rows.push(ConstantRow {
        name: "Step tools",
        value: format!("at most {STEP_TOOLS_MAX}"),
    });
    rows.push(ConstantRow {
        name: "Report length",
        value: format!("1..={REPORT_LIMIT}"),
    });
    rows.push(ConstantRow {
        name: "child_max_steps",
        value: format!("{CHILD_MAX_STEPS_DEFAULT}, 1..={CHILD_MAX_STEPS_MAX}"),
    });
    rows.push(ConstantRow {
        name: "child_max_minutes",
        value: format!("{CHILD_MAX_MINUTES_DEFAULT}, 1..={CHILD_MAX_MINUTES_MAX}"),
    });
    rows.push(ConstantRow {
        name: "max_runs",
        value: format!("{MAX_RUNS_DEFAULT}, 1..={MAX_RUNS_MAX}"),
    });
    rows.push(ConstantRow {
        name: "Grace turn",
        value: format!("{GRACE_TURN_S} s"),
    });
    rows.push(ConstantRow {
        name: "Run notice",
        value: format!("at most {RUN_NOTICE_LIMIT} bytes"),
    });
    rows.push(ConstantRow {
        name: "Changed paths shown",
        value: format!("{CHANGED_PATHS_SHOWN}, then `(+<k> more)`"),
    });
    rows.push(ConstantRow {
        name: "`wait` timeout",
        value: format!("default {WAIT_TIMEOUT_DEFAULT_S} s, at most {WAIT_TIMEOUT_MAX_S} s"),
    });
    rows.push(ConstantRow {
        name: "`list` runs",
        value: format!("{LIST_RUNS}; {LIST_TASK_LINES} task lines per run"),
    });
    rows.push(ConstantRow {
        name: "Minimum git",
        value: format!("{}.{}", MIN_GIT_VERSION.0, MIN_GIT_VERSION.1),
    });
}
/// Builds the constants table in the contract's order. The page and the
/// gate read this table, so both show one truth.
pub(crate) fn constant_rows() -> Vec<ConstantRow> {
    let mut rows: Vec<ConstantRow> = Vec::new();
    guard_rows(&mut rows);
    monitor_rows(&mut rows);
    goal_rows(&mut rows);
    workflow_rows(&mut rows);
    rows
}

/// The constants table rendered into the served page.
pub(crate) fn constants_table() -> String {
    let mut text = String::from("\n## Constants\n\n| Constant | Value |\n|---|---|\n");
    for row in constant_rows() {
        text.push_str("| ");
        text.push_str(row.name);
        text.push_str(" | ");
        text.push_str(&row.value);
        text.push_str(" |\n");
    }
    text
}
