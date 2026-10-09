// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Orchestration battery: one owner task per session, strict session-start config.

pub(crate) mod admission;
pub(crate) mod agents_tool;
pub(crate) mod arbiter;
mod commands;
pub(crate) mod delivery;
pub(crate) mod goal;
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

use std::sync::Arc;

use dal_agent::ext::{Extension, ExtensionBuilder};
use dal_core::{Origin, RegistrationError, ServiceSet};

/// Fixed honesty preamble delivered with orchestration reports.
pub const CLAIM_HONESTY: &str = "Reports are claims, not proof. Before you rely on one:\n1. Rebuild the task's scope from its prompt: every file, change, and check it owed.\n2. Read the changed files and run the checks yourself. A summary proves nothing.\n3. Check both ways: nothing owed is missing, and nothing outside the scope changed.\nIf a check fails, start a new run with exact instructions, or fix it yourself.";

/// Manual page text registered under `dalgona://orchestration`.
pub const ORCHESTRATION_DOC: &str = "# Orchestration\n\nThe orchestration extension coordinates background jobs, monitored output, goals, and child-agent workflows. Use `agents` to run, wait for, cancel, or list workflows. Use `monitor` to watch matching output from a background job. Use `create_goal`, `update_goal`, and `get_goal` with `/goal` to manage a durable session goal. `/continuation` controls automatic turns, and `/abort` cancels active orchestration work. Automatic reminders are delivered only when the session is ready; child reports are claims, not proof.\n\n## Jobs and cancellation\n\nJobs run under one owner task per session with a bounded queue. `cancel` names a job or a turn and ends it through one cancellation tree; cascade cancellation reaches every descendant. Host shutdown closes every open session and stops timers, child sessions, processes, and background jobs before the process exits.\n\n## Agents, scopes, and budgets\n\nChild agents are admitted in FIFO order into a scope. A scope carries a budget of requests, tokens, and usd, and usage rolls up through nested scopes to the parent ledger. Admission fails when the budget is exhausted or a usd-budgeted model has no known price. A scope opened inside a hook dies at that hook's deadline. Reports from children are claims, not proof: rebuild the promised scope, inspect the changed files, and run the checks before trusting one.\n\n## Wake and turns\n\nA turn started by a wake rather than a user prompt counts against a limit of 20 consecutive wake-started turns. The twenty-first wake is refused and the refusal is journaled; a user prompt resets the count.\n\n## Mailbox\n\nAgent-to-agent mail travels through the journal-backed mailbox with per-pair FIFO order and cursor-based reads. The delivery mode is `aside`, `steer`, or `next_turn`: an aside is delivered without steering the current turn, steer joins the running turn, and next_turn queues for the following one. A send returns a receipt naming the outcome: delivered, woken, buffered, full, or gone.\n\n## Synthetic models and private tools\n\nA synthetic model is a model route whose handler emits a normal event stream; usage inside it is journaled against the run that made it. A handler may bind private tools that exist only for that call; a private tool that is not declared in the request may not shadow a session tool, and the collision fails closed.\n";

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
#[derive(Clone, Debug)]
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
    /// Worktree isolation sub-battery.
    pub isolation: BatteryConfig,
    /// Optional named saved workflows.
    pub workflows: Option<toml::Value>,
}

impl Default for OrchestrationConfig {
    fn default() -> Self {
        Self {
            loop_guard: BatteryConfig::default(),
            sleep: BatteryConfig::default(),
            monitor: OrchestrationMonitorConfig::default(),
            inflight: BatteryConfig::default(),
            goal: BatteryConfig::default(),
            arbiter: BatteryConfig::default(),
            agents: OrchestrationAgentsConfig::default(),
            isolation: BatteryConfig::default(),
            workflows: None,
        }
    }
}

/// Configuration decode error for the orchestration battery.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct OrchestrationConfigError(ConfigErrorKind);

#[derive(Debug, thiserror::Error)]
enum ConfigErrorKind {
    #[error("plugin.orchestration: {0}")]
    Decode(#[from] toml::de::Error),
    #[error("plugin.orchestration.{key} must be an integer from {min} to {max}")]
    Range {
        key: &'static str,
        min: u32,
        max: u32,
    },
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
    let agents = &raw.agents;
    if !(1..=1_000).contains(&agents.child_max_steps) {
        return Err(OrchestrationConfigError(ConfigErrorKind::Range {
            key: "agents.child_max_steps",
            min: 1,
            max: 1_000,
        }));
    }
    if !(1..=600).contains(&agents.child_max_minutes) {
        return Err(OrchestrationConfigError(ConfigErrorKind::Range {
            key: "agents.child_max_minutes",
            min: 1,
            max: 600,
        }));
    }
    if !(1..=64).contains(&agents.max_runs) {
        return Err(OrchestrationConfigError(ConfigErrorKind::Range {
            key: "agents.max_runs",
            min: 1,
            max: 64,
        }));
    }
    Ok(OrchestrationConfig {
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
        agents: raw.agents,
        isolation: raw.isolation,
        workflows: raw.workflows,
    })
}

/// Builds the orchestration extension registration. Effects begin only after
/// the host starts a session.
///
/// # Errors
///
/// Returns [`RegistrationError`] when the fixed service set or extension
/// registration is rejected.
pub fn orchestration(config: OrchestrationConfig) -> Result<Extension, RegistrationError> {
    let needs_owner = config.loop_guard.enabled
        || config.sleep.enabled
        || config.monitor.enabled
        || config.inflight.enabled
        || config.goal.enabled
        || config.arbiter.enabled
        || config.agents.enabled
        || config.isolation.enabled;
    let input_enabled = config.loop_guard.enabled || config.goal.enabled || config.arbiter.enabled;
    let tool_hook_enabled = config.loop_guard.enabled
        || config.sleep.enabled
        || config.goal.enabled
        || config.monitor.enabled
        || config.arbiter.enabled
        || config.agents.enabled;
    let turn_end_enabled = config.loop_guard.enabled
        || config.goal.enabled
        || config.monitor.enabled
        || config.arbiter.enabled;
    let settled_enabled = config.goal.enabled
        || config.monitor.enabled
        || config.arbiter.enabled
        || config.agents.enabled;
    let status_enabled = config.inflight.enabled;
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
    if status_enabled {
        builder = builder.status_kind("orchestration.status", Arc::new(runtime::Status(runtime)));
    }
    builder.build()
}
