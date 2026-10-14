// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Orchestration battery: one owner task per session, strict session-start config.

pub(crate) mod admission;
pub(crate) mod agents_tool;
pub(crate) mod arbiter;
mod commands;
pub(crate) mod constants;
pub(crate) mod delivery;
pub mod docs;
pub(crate) mod goal;
pub(crate) mod live;
pub(crate) mod monitor;
pub(crate) mod pool;
mod runtime;
pub(crate) mod stuck;
#[cfg(test)]
mod tests;
mod tools;
pub(crate) mod types;
pub(crate) mod workflow;
pub(crate) mod worktree;

pub(crate) use types::{ControllerMode, GoalStatus, JobsView, StopKind};

pub use live::{GoalView, InflightCounts, View};

use std::sync::Arc;

use dal_agent::ext::{Extension, ExtensionBuilder};
use dal_core::{Origin, RegistrationError, ServiceSet};

/// Fixed honesty preamble delivered with orchestration reports.
pub const CLAIM_HONESTY: &str = "Reports are claims, not proof. Before you rely on one:\n1. Rebuild the task's scope from its prompt: every file, change, and check it owed.\n2. Read the changed files and run the checks yourself. A summary proves nothing.\n3. Check both ways: nothing owed is missing, and nothing outside the scope changed.\nIf a check fails, start a new run with exact instructions, or fix it yourself.";

/// Manual page body registered under `dalgona://orchestration`. The served
/// page appends [`constants_table`] (see `dalgona/dalgona/src/docs.rs`).
pub const ORCHESTRATION_DOC: &str = concat!(
    "# Orchestration\n\n",
    "The orchestration extension coordinates background jobs, monitored output, goals, and child-agent workflows. ",
    "Use `agents` to run, wait for, cancel, or list workflows. ",
    "Use `monitor` to watch matching output from a background job. ",
    "Use `create_goal`, `update_goal`, and `get_goal` with `/goal` to manage a durable session goal. ",
    "`/continuation` controls automatic turns, and `/abort` cancels active orchestration work. ",
    "Automatic reminders are delivered only when the session is ready; child reports are claims, not proof.\n\n",
    "## Agents, jobs, cancel, and shutdown\n\n",
    "Each `agents` run and each background job belongs to one session and uses a bounded queue. ",
    "`cancel` names a job or a turn and stops it and its descendants through one cancellation tree. ",
    "Host shutdown closes every open session and stops timers, child sessions, processes, and background jobs before the process exits.\n\n",
    "When a workflow exceeds its shutdown grace period, dalgona leaves its existing worktrees in place ",
    "and records their recovery paths in each task's `retained.json`. Session close waits for this recovery step. ",
    "If saving a recovery record fails, dalgona reports the failure and leaves the worktree on disk.\n\n",
    "Admission checks active runs and the remaining session quota before starting children. ",
    "`max_runs` defaults to 16 under `[plugin.orchestration.agents]`; `agents_per_session` has a fixed ceiling of 1024. ",
    "When a limit is reached, wait for a run to end, cancel one, or start a smaller run.\n\n",
    "Subagents cannot start subagents. They see only the explicitly listed tools (default `read` and `search`) and cannot ask the user. ",
    "Modifying tools require worktree isolation: `patch` under approval mode `edits` or `all`, and `exec` under `all`. ",
    "Without a configured host data root, worktree steps refuse to start; they do not run in the main checkout.\n\n",
    "Each child calls `orchestration__report` once with status `done`, `blocked`, or `failed` and 1 to 16,000 characters of report text. ",
    "Empty and duplicate reports are refused. Rebuild the promised scope, inspect the changed files, and run the checks before trusting a report.\n\n",
    "## Wakes\n\n",
    "A wake starts a turn without a user prompt. A session accepts 20 wake-started turns in a row; ",
    "the 21st wake is refused and journaled, and a user prompt resets the count. ",
    "A wake that reached the session is never repeated: when the host does not confirm the job reports it carried, ",
    "the owner sees a delivery notice and only the acknowledgement is retried. ",
    "Other delivery-poll failures are reported once per cause.\n\n",
    "## Scope, budget, and deadline\n\n",
    "A scope caps concurrent model handles with a limit; handles past the limit wait in FIFO order. ",
    "Its budget bounds requests, tokens, wall time, and cost; usage rolls up through nested scopes to the parent ledger. ",
    "Admission fails when the budget is exhausted or a USD-budgeted model has no known price. ",
    "An `on_error` policy decides what one failed handle does to its siblings. ",
    "A scope created inside a hook is cancelled when the hook deadline passes; there is no separate timeout setting.\n\n",
    "## Mailbox\n\n",
    "Sessions in one tree exchange messages through a journal-backed mailbox with per-pair FIFO order and cursor-based reads. Each message carries a mode: ",
    "`aside` delivers without steering the current turn, `steer` steers the running turn, and `next_turn` queues for the recipient's next turn. ",
    "A send returns a receipt naming the outcome: delivered, buffered, full, or gone. ",
    "Reading the mailbox does not consume messages; a full waiting queue returns `Full`, and an out-of-tree or finished recipient returns `Gone`.\n\n",
    "## Synthetic models\n\n",
    "A synthetic model is a plugin-defined model id whose handler emits a normal event stream and can forward to another model. ",
    "Its usage is journaled against the synthetic route. A handler may bind private tools that exist only for that call. ",
    "Only the handler may forward, and only once: a second forward is refused. Private tool calls run inside the handler and never reach the session, a private tool cannot shadow a session tool, a cycle is refused, and a chain of more than four routes is refused.\n",
);

/// The constants truth table rendered into the page from the same named
/// constants the code enforces, so the page cannot drift from behavior.
/// The full served page: static body plus the generated constants table.
/// `dalgona/src/docs.rs` registers this; the `dalgona/docs/orchestration.md`
/// mirror must equal it byte for byte.
#[must_use]
pub fn orchestration_served_page() -> String {
    format!("{ORCHESTRATION_DOC}{}", constants::constants_table())
}

/// The checked constant rows as public `(name, value)` pairs: the truth
/// source for the docs-parity gate, read from the code and not the page.
#[must_use]
pub fn constant_pairs() -> Vec<(String, String)> {
    constants::constant_rows()
        .into_iter()
        .map(|row| (row.name.to_owned(), row.value))
        .collect()
}

/// The generated constants table appended to the orchestration page.
#[must_use]
pub fn constants_table() -> String {
    constants::constants_table()
}

/// Enables or disables one orchestration sub-battery.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BatteryConfig {
    /// Whether this sub-battery is registered.
    pub enabled: bool,
}

impl Default for BatteryConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Configuration for orchestration job monitoring.
#[derive(Clone, Debug)]
pub struct OrchestrationMonitorConfig {
    /// Whether monitoring is enabled.
    pub enabled: bool,
    /// Output coalescing interval in milliseconds.
    pub coalesce_ms: u64,
    /// Minimum interval between monitor batches in milliseconds.
    pub rate_limit_ms: u64,
    /// Maximum queued output lines.
    pub max_lines: usize,
    /// Maximum queued output characters.
    pub max_chars: usize,
    /// Monitor-only wake limit.
    pub wake_budget: u16,
}

impl Default for OrchestrationMonitorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            coalesce_ms: 2_000,
            rate_limit_ms: 5_000,
            max_lines: 50,
            max_chars: 4_096,
            wake_budget: 5,
        }
    }
}

/// Configuration for child-agent workflows.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OrchestrationAgentsConfig {
    /// Whether child-agent workflows are enabled.
    pub enabled: bool,
    /// Maximum tool rounds per child turn.
    pub child_max_steps: u32,
    /// Maximum child run time in minutes.
    pub child_max_minutes: u32,
    /// Maximum active workflow runs per session.
    pub max_runs: u32,
}

impl Default for OrchestrationAgentsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            child_max_steps: 50,
            child_max_minutes: 30,
            max_runs: 16,
        }
    }
}

/// Strict configuration for `[plugin.orchestration]`.
#[derive(Clone, Debug, Default)]
pub struct OrchestrationConfig {
    /// Loop guard sub-battery.
    pub loop_guard: BatteryConfig,
    /// Sleep-wait rewrite sub-battery.
    pub sleep: BatteryConfig,
    /// Job monitor configuration.
    pub monitor: OrchestrationMonitorConfig,
    /// Inflight status sub-battery.
    pub inflight: BatteryConfig,
    /// Durable goal sub-battery.
    pub goal: BatteryConfig,
    /// Automatic-turn arbiter sub-battery.
    pub arbiter: BatteryConfig,
    /// Child-agent workflow configuration.
    pub agents: OrchestrationAgentsConfig,
    /// Worktree-isolation sub-battery. The worktree execution it owns is
    /// already part of `agents`; the key exists so its config surface is
    /// declared, not an unknown key.
    pub isolation: BatteryConfig,
    /// Optional named saved workflows.
    pub workflows: Option<toml::Value>,
    /// Host data root for isolated task worktrees and artifacts. The entry
    /// supplies it at assembly; without it a worktree step refuses the run
    /// before any job starts, because the battery never falls back to the
    /// real checkout.
    pub data_root: Option<std::path::PathBuf>,
}

/// Configuration decode error for the orchestration battery.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct OrchestrationConfigError(ConfigErrorKind);

#[derive(Debug, thiserror::Error)]
enum ConfigErrorKind {
    /// Configuration could not be decoded from TOML.
    #[error("plugin.orchestration: {0}")]
    Decode(#[from] toml::de::Error),
    /// A run setting was outside its accepted range.
    #[error("{0}")]
    Agents(String),
    /// A sub-battery was disabled while a dependent sub-battery is on.
    #[error("{0}")]
    Coherence(String),
    /// The monitor sub-battery configuration was invalid.
    #[error(transparent)]
    Monitor(#[from] monitor::state::MonitorConfigError),
}

impl From<toml::de::Error> for OrchestrationConfigError {
    fn from(error: toml::de::Error) -> Self {
        Self(ConfigErrorKind::Decode(error))
    }
}

impl From<monitor::state::MonitorConfigError> for OrchestrationConfigError {
    fn from(error: monitor::state::MonitorConfigError) -> Self {
        Self(ConfigErrorKind::Monitor(error))
    }
}

/// Parses the strict `[plugin.orchestration]` table.
///
/// # Errors
/// Returns the first strict-decode failure.
pub fn parse_config(
    section: Option<&toml::Value>,
) -> Result<OrchestrationConfig, OrchestrationConfigError> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct RawConfig {
        #[serde(default)]
        loop_guard: BatteryConfig,
        #[serde(default)]
        sleep: BatteryConfig,
        #[serde(default)]
        monitor: Option<toml::Value>,
        #[serde(default)]
        inflight: BatteryConfig,
        #[serde(default)]
        goal: BatteryConfig,
        #[serde(default)]
        arbiter: BatteryConfig,
        #[serde(default)]
        agents: OrchestrationAgentsConfig,
        #[serde(default)]
        isolation: BatteryConfig,
        #[serde(default)]
        workflows: Option<toml::Value>,
    }

    let Some(section) = section else {
        return Ok(OrchestrationConfig::default());
    };
    let raw: RawConfig = section.clone().try_into()?;
    let monitor = monitor::state::parse_config(raw.monitor.as_ref())?;
    let agents = decode_agents_settings(&raw.agents)?;
    let config = OrchestrationConfig {
        loop_guard: raw.loop_guard,
        sleep: raw.sleep,
        monitor: OrchestrationMonitorConfig {
            enabled: monitor.enabled,
            coalesce_ms: monitor.coalesce_ms,
            rate_limit_ms: monitor.rate_limit_ms,
            max_lines: monitor.max_lines,
            max_chars: monitor.max_chars,
            wake_budget: monitor.wake_budget,
        },
        inflight: raw.inflight,
        goal: raw.goal,
        arbiter: raw.arbiter,
        agents,
        isolation: raw.isolation,
        workflows: raw.workflows,
        data_root: None,
    };
    if let Some(text) = coherence_refusal(&config) {
        return Err(OrchestrationConfigError(ConfigErrorKind::Coherence(text)));
    }
    Ok(config)
}

/// Decodes and validates the run settings table through the admission
/// module, which owns the exact range texts.
fn decode_agents_settings(
    table: &OrchestrationAgentsConfig,
) -> Result<OrchestrationAgentsConfig, OrchestrationConfigError> {
    let encoded = format!(
        "{{\"child_max_steps\":{},\"child_max_minutes\":{},\"max_runs\":{}}}",
        table.child_max_steps, table.child_max_minutes, table.max_runs
    );
    let raw = dal_core::RawJson::parse(&encoded)
        .map_err(|error| OrchestrationConfigError(ConfigErrorKind::Agents(error.to_string())))?;
    admission::decode_settings(&raw)
        .map_err(|text| OrchestrationConfigError(ConfigErrorKind::Agents(text)))?;
    Ok(table.clone())
}

/// Names the first cross-table coherence refusal, if any: `arbiter` and
/// `inflight` cannot be off while `goal`, `monitor`, or `agents` is on.
fn coherence_refusal(config: &OrchestrationConfig) -> Option<String> {
    let on = [
        ("goal", config.goal.enabled),
        ("monitor", config.monitor.enabled),
        ("agents", config.agents.enabled),
    ]
    .into_iter()
    .find_map(|(name, on)| on.then_some(name))?;
    if !config.arbiter.enabled {
        return Some(monitor::state::coherence_error("arbiter", on));
    }
    if !config.inflight.enabled {
        return Some(monitor::state::coherence_error("inflight", on));
    }
    None
}

/// The built orchestration battery: its extension registration and the
/// read-only view of its sessions, handed to the batteries that read it.
pub struct OrchestrationParts {
    /// The hooks, tools, commands, and status kind registered with dal.
    pub extension: Extension,
    /// The read-only view of every session this battery instance owns.
    pub view: View,
}

/// Builds the orchestration extension registration with its read-only view.
/// Effects begin only after the host starts a session.
///
/// # Errors
///
/// Returns [`RegistrationError`] when the fixed service set or extension
/// registration is rejected.
pub fn orchestration(config: OrchestrationConfig) -> Result<OrchestrationParts, RegistrationError> {
    let needs_owner = config.loop_guard.enabled
        || config.sleep.enabled
        || config.monitor.enabled
        || config.inflight.enabled
        || config.goal.enabled
        || config.arbiter.enabled
        || config.agents.enabled;
    let input_enabled = config.loop_guard.enabled || config.goal.enabled || config.arbiter.enabled;
    let tool_hook_enabled = config.loop_guard.enabled
        || config.sleep.enabled
        || config.goal.enabled
        || config.monitor.enabled
        || config.arbiter.enabled
        || config.agents.enabled;
    let status_enabled = config.inflight.enabled;
    let turn_end_enabled = config.loop_guard.enabled
        || config.goal.enabled
        || config.monitor.enabled
        || config.arbiter.enabled
        || status_enabled;
    let settled_enabled = config.goal.enabled
        || config.monitor.enabled
        || config.arbiter.enabled
        || config.agents.enabled;
    let tool_result_enabled = config.goal.enabled || config.monitor.enabled;
    let runtime = runtime::Runtime::new(config)?;
    let inject = ServiceSet::from_names(["agents", "jobs", "turn", "sidecar", "run", "ask"])?;
    let mut builder =
        ExtensionBuilder::new("orchestration", "0.1.0", inject)?.with_origin(Origin::Bundled, None);
    builder = tools::register(builder, &runtime)?;
    builder = commands::register(builder, &runtime)?;
    if needs_owner {
        builder = builder
            .on_session_start_lossless(runtime::SessionStartHook(runtime.clone()))
            .on_session_end_lossless(runtime::SessionEndHook(runtime.clone()));
    }
    if input_enabled {
        builder = builder.on_input(runtime::InputHook(runtime.clone()));
    }
    if status_enabled {
        builder = builder.on_before_turn(runtime::BeforeTurnHook(runtime.clone()));
    }
    if tool_hook_enabled {
        builder = builder.on_tool_call(runtime::ToolCallHook(runtime.clone()));
    }
    if tool_result_enabled {
        builder = builder.on_tool_result_lossless(runtime::ToolResultHook(runtime.clone()));
    }
    if turn_end_enabled {
        builder = builder.on_turn_end_lossless(runtime::TurnEndHook(runtime.clone()));
    }
    if settled_enabled {
        builder = builder.on_settled_lossless(runtime::SettledHook(runtime.clone()));
    }
    let view = runtime.view();
    if status_enabled {
        builder = builder.status_kind("orchestration.status", Arc::new(runtime::Status(runtime)));
    }
    Ok(OrchestrationParts {
        extension: builder.build()?,
        view,
    })
}
