// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Monitor lifecycle: the `monitor` tool contract, watch validation with the
//! exact error texts, config parsing, and stop/rearm.

use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;

use dal_core::{JobId, RawJson, Timestamp};
use regex_automata::meta::Regex;
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};

use super::super::JobsView;

/// Tool description for the model-visible `monitor` tool.
pub(crate) const MONITOR_DESCRIPTION: &str = "Watch the output of one of your background exec jobs and get the lines that match filter as messages, without polling. The watch ends when the job ends; the job's own report still arrives. At most 16 watches at once.";

/// Input schema for the model-visible `monitor` tool.
pub(crate) const MONITOR_SCHEMA: &str = "{\"type\":\"object\",\"properties\":{\"action\":{\"type\":\"string\",\"enum\":[\"watch\",\"stop\",\"rearm\"]},\"job\":{\"type\":\"string\",\"description\":\"The UUIDv7 job id. watch only.\"},\"filter\":{\"type\":\"string\",\"description\":\"A regex; each matching output line is an event. watch only.\"},\"description\":{\"type\":\"string\",\"maxLength\":80},\"id\":{\"type\":\"string\",\"description\":\"The monitor id, m<n>. stop and rearm only.\"}},\"required\":[\"action\"],\"additionalProperties\":false}";

/// Maximum live monitors per session, including paused and muted ones.
pub(crate) const MAX_LIVE_MONITORS: usize = 16;

/// Monitor delivery state owned by the orchestration core's one task.
#[derive(Debug)]
pub(crate) struct MonitorState {
    pub(super) next_id: u64,
    pub(super) monitors: HashMap<MonitorId, Monitor>,
    pub(super) output: VecDeque<OutputLine>,
    pub(super) last_flush: Option<Timestamp>,
    pub(super) monitor_only_wakes: u16,
}

impl Default for MonitorState {
    fn default() -> Self {
        Self {
            next_id: 1,
            monitors: HashMap::new(),
            output: VecDeque::new(),
            last_flush: None,
            monitor_only_wakes: 0,
        }
    }
}

/// Session-scoped monitor id, rendered as `m<n>`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct MonitorId(pub(crate) u64);

impl MonitorState {
    pub(crate) fn live_count(&self) -> usize {
        self.monitors
            .values()
            .filter(|monitor| !monitor.stopped && !monitor.paused && !monitor.muted)
            .count()
    }
}

impl MonitorId {
    /// Renders `m<n>`.
    pub(crate) fn render(self) -> String {
        format!("m{}", self.0)
    }

    /// Parses `m<n>`; returns `None` for every other shape.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        let digits = value.strip_prefix('m')?;
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        digits.parse::<u64>().ok().map(Self)
    }
}

/// One job-output watch. A stopped watch is retained (it no longer counts
/// toward the live cap) so it can be rearmed while its job still runs; the
/// next flush reaps it and reports the end to the core.
#[derive(Debug)]
pub(crate) struct Monitor {
    pub(super) id: MonitorId,
    pub(super) job: JobId,
    pub(super) job_display: Box<str>,
    pub(super) filter: Regex,
    pub(super) description: Box<str>,
    pub(super) paused: bool,
    pub(super) stopped: bool,
    pub(super) muted: bool,
    pub(super) matched_at: VecDeque<Timestamp>,
    pub(super) matched_lines: u32,
    pub(super) last_batch_at: Option<Timestamp>,
    pub(super) last_batch_fingerprint: Option<Box<str>>,
    pub(super) overflow_lines: u32,
}

/// One queued matching output line.
#[derive(Clone, Debug)]
pub(crate) struct OutputLine {
    pub(super) monitor: MonitorId,
    pub(super) text: Box<str>,
    pub(super) at: Timestamp,
}

/// Monitor table configuration with plan defaults.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MonitorConfig {
    pub enabled: bool,
    pub coalesce_ms: u64,
    pub rate_limit_ms: u64,
    pub max_lines: usize,
    pub max_chars: usize,
    pub wake_budget: u16,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            coalesce_ms: 2000,
            rate_limit_ms: 5000,
            max_lines: 50,
            max_chars: 4096,
            wake_budget: 5,
        }
    }
}

/// A monitor-table decoding failure with the exact plan error text.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum MonitorConfigError {
    #[error("orchestration: [plugin.orchestration.monitor] has an unknown key \"{key}\".")]
    UnknownKey { key: Box<str> },
    #[error(
        "orchestration: [plugin.orchestration.monitor].{key} must be an integer from {low} to {high}."
    )]
    NotInteger { key: Box<str>, low: u64, high: u64 },
    #[error("orchestration: [plugin.orchestration.monitor].enabled must be true or false.")]
    EnabledType,
    #[error("orchestration: [plugin.orchestration.monitor] must be a table.")]
    NotTable,
}

impl MonitorConfigError {
    fn range(key: &str, low: u64, high: u64) -> Self {
        Self::NotInteger {
            key: key.into(),
            low,
            high,
        }
    }
}

/// Decodes the `[plugin.orchestration.monitor]` table strictly: unknown keys
/// are rejected, non-integer values report the exact range error, and
/// out-of-range integers clamp to their range. A missing table means plan
/// defaults. The caller keeps the extension registered and serves the error
/// on every monitor tool call.
pub(crate) fn parse_config(
    section: Option<&toml::Value>,
) -> Result<MonitorConfig, MonitorConfigError> {
    let mut config = MonitorConfig::default();
    let Some(section) = section else {
        return Ok(config);
    };
    let Some(table) = section.as_table() else {
        return Err(MonitorConfigError::NotTable);
    };
    for (key, value) in table {
        match key.as_str() {
            "enabled" => {
                config.enabled = value.as_bool().ok_or(MonitorConfigError::EnabledType)?;
            }
            "coalesce_ms" => {
                config.coalesce_ms = checked_integer(value, "coalesce_ms", 1, 60_000)?;
            }
            "rate_limit_ms" => {
                config.rate_limit_ms = checked_integer(value, "rate_limit_ms", 1, 3_600_000)?;
            }
            "max_lines" => {
                config.max_lines = usize::try_from(checked_integer(value, "max_lines", 1, 200)?)
                    .map_err(|_| MonitorConfigError::range("max_lines", 1, 200))?;
            }
            "max_chars" => {
                config.max_chars =
                    usize::try_from(checked_integer(value, "max_chars", 512, 16_384)?)
                        .map_err(|_| MonitorConfigError::range("max_chars", 512, 16_384))?;
            }
            "wake_budget" => {
                config.wake_budget = u16::try_from(checked_integer(value, "wake_budget", 1, 100)?)
                    .map_err(|_| MonitorConfigError::range("wake_budget", 1, 100))?;
            }
            other => {
                return Err(MonitorConfigError::UnknownKey { key: other.into() });
            }
        }
    }
    Ok(config)
}

/// Reads one integer key and rejects non-integers or out-of-range values.
fn checked_integer(
    value: &toml::Value,
    key: &str,
    low: u64,
    high: u64,
) -> Result<u64, MonitorConfigError> {
    let raw = value
        .as_integer()
        .and_then(|integer| u64::try_from(integer).ok())
        .filter(|integer| (low..=high).contains(integer))
        .ok_or_else(|| MonitorConfigError::range(key, low, high))?;
    Ok(raw)
}

/// A validated `monitor` tool call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MonitorRequest {
    pub action: MonitorAction,
}

/// The three tool actions with their applicable fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum MonitorAction {
    Watch {
        job: Box<str>,
        filter: Box<str>,
        description: Option<Box<str>>,
    },
    Stop {
        id: Box<str>,
    },
    Rearm {
        id: Box<str>,
    },
}

/// A `monitor` tool failure with the exact plan error text.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum MonitorError {
    #[error("monitor: no job {job} in this session.")]
    NoJob { job: Box<str> },
    #[error("monitor: job {job} is not a running exec job.")]
    NotRunning { job: Box<str> },
    #[error("monitor: filter is not a valid regex: {error}.")]
    BadFilter { error: Box<str> },
    #[error("monitor: 16 monitors are live; stop one first.")]
    Full,
    #[error("monitor: no monitor {id}.")]
    NoMonitor { id: Box<str> },
    #[error("monitor: field {field} does not apply to action {action}.")]
    FieldForAction { field: Box<str>, action: Box<str> },
    #[error("monitor: action {action} needs {field}.")]
    NeedsField { action: Box<str>, field: Box<str> },
}

/// A successful `monitor` tool reply.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum MonitorReply {
    Watching {
        job: Box<str>,
        monitor: MonitorId,
        description: Box<str>,
    },
    Stopped {
        monitor: MonitorId,
    },
    Rearmed {
        monitor: MonitorId,
    },
}

impl MonitorReply {
    /// Renders the exact successful reply text.
    pub(crate) fn text(&self) -> String {
        match self {
            Self::Watching {
                job,
                monitor,
                description,
            } => format!(
                "watching job {job} as {}: \"{description}\".",
                monitor.render()
            ),
            Self::Stopped { monitor } => format!("stopped {}.", monitor.render()),
            Self::Rearmed { monitor } => format!("rearmed {}.", monitor.render()),
        }
    }
}

/// One deliverable P3 batch: event lines under one header.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MonitorBatch {
    pub monitor: MonitorId,
    pub job_display: Box<str>,
    pub description: Box<str>,
    pub lines: Vec<Box<str>>,
    pub dropped: u32,
}

impl MonitorBatch {}

/// Reads one optional string member; a wrong-typed member counts as absent
/// and surfaces as the missing-field error for its key.
fn opt_string(object: &sonic_rs::Object, key: &str) -> Option<Box<str>> {
    object
        .get(&key)
        .and_then(|member| member.as_str())
        .map(str::to_owned)
        .map(String::into_boxed_str)
}

/// Parses one raw `monitor` tool object. Validates the action first, then
/// inapplicable fields in JSON key order, then missing required fields. The
/// runtime schema additionally enforces the action enum, string types, and
/// the 80-character description cap before this reducer runs.
pub(crate) fn parse_request(args: &RawJson) -> Result<MonitorRequest, MonitorError> {
    let value: Value = sonic_rs::from_str(args.as_str()).map_err(|error: sonic_rs::Error| {
        MonitorError::BadFilter {
            error: error.to_string().into(),
        }
    })?;
    let Some(object) = value.as_object() else {
        return Err(MonitorError::NeedsField {
            action: "unknown".into(),
            field: "action".into(),
        });
    };
    let action = opt_string(object, "action").ok_or_else(|| MonitorError::NeedsField {
        action: "unknown".into(),
        field: "action".into(),
    })?;
    match action.as_ref() {
        "watch" => {
            if object.get(&"id").is_some_and(|member| !member.is_null()) {
                return Err(MonitorError::FieldForAction {
                    field: "id".into(),
                    action: action.clone(),
                });
            }
            let Some(job) = opt_string(object, "job") else {
                return Err(MonitorError::NeedsField {
                    action,
                    field: "job".into(),
                });
            };
            let Some(filter) = opt_string(object, "filter") else {
                return Err(MonitorError::NeedsField {
                    action,
                    field: "filter".into(),
                });
            };
            Ok(MonitorRequest {
                action: MonitorAction::Watch {
                    job,
                    filter,
                    description: opt_string(object, "description"),
                },
            })
        }
        "stop" | "rearm" => {
            for (key, member) in object {
                if member.is_null() {
                    continue;
                }
                if matches!(key, "job" | "filter" | "description") {
                    return Err(MonitorError::FieldForAction {
                        field: key.into(),
                        action: action.clone(),
                    });
                }
            }
            let Some(id) = opt_string(object, "id") else {
                return Err(MonitorError::NeedsField {
                    action,
                    field: "id".into(),
                });
            };
            if action.as_ref() == "stop" {
                Ok(MonitorRequest {
                    action: MonitorAction::Stop { id },
                })
            } else {
                Ok(MonitorRequest {
                    action: MonitorAction::Rearm { id },
                })
            }
        }
        _ => Err(MonitorError::NeedsField {
            action: action.clone(),
            field: "action".into(),
        }),
    }
}

/// Runs one validated `monitor` tool call: watch validates the job through
/// the session jobs view, compiles the filter once, and subscribes; stop
/// retains the watch as stopped so it can be rearmed; rearm resumes a
/// stopped, muted, or paused watch while its job still runs.
pub(crate) fn watch(
    state: &mut MonitorState,
    request: &MonitorRequest,
    jobs: &dyn JobsView,
    _now: Timestamp,
    _config: &MonitorConfig,
) -> Result<MonitorReply, MonitorError> {
    match &request.action {
        MonitorAction::Watch {
            job,
            filter,
            description,
        } => {
            let Some(job_id) = jobs.resolve_job(job) else {
                return Err(MonitorError::NoJob { job: job.clone() });
            };
            if !jobs.is_live_top_level_exec(job_id) {
                return Err(MonitorError::NotRunning { job: job.clone() });
            }
            let compiled = Regex::new(filter).map_err(|error| MonitorError::BadFilter {
                error: error.to_string().into(),
            })?;
            let live = state
                .monitors
                .values()
                .filter(|monitor| !monitor.stopped)
                .count();
            if live >= MAX_LIVE_MONITORS {
                return Err(MonitorError::Full);
            }
            let id = MonitorId(state.next_id);
            state.next_id = state.next_id.saturating_add(1);
            let reply_description: Box<str> = description
                .clone()
                .unwrap_or_else(|| format!("job {job}").into_boxed_str());
            state.monitors.insert(
                id,
                Monitor {
                    id,
                    job: job_id,
                    job_display: job.clone(),
                    filter: compiled,
                    description: reply_description.clone(),
                    paused: false,
                    stopped: false,
                    muted: false,
                    matched_at: VecDeque::new(),
                    matched_lines: 0,
                    last_batch_at: None,
                    last_batch_fingerprint: None,
                    overflow_lines: 0,
                },
            );
            Ok(MonitorReply::Watching {
                job: job.clone(),
                monitor: id,
                description: reply_description,
            })
        }
        MonitorAction::Stop { id } => {
            let Some(monitor_id) = MonitorId::parse(id) else {
                return Err(MonitorError::NoMonitor { id: id.clone() });
            };
            let Some(monitor) = state.monitors.get_mut(&monitor_id) else {
                return Err(MonitorError::NoMonitor { id: id.clone() });
            };
            monitor.stopped = true;
            Ok(MonitorReply::Stopped {
                monitor: monitor_id,
            })
        }
        MonitorAction::Rearm { id } => {
            let Some(monitor_id) = MonitorId::parse(id) else {
                return Err(MonitorError::NoMonitor { id: id.clone() });
            };
            let Some(monitor) = state.monitors.get_mut(&monitor_id) else {
                return Err(MonitorError::NoMonitor { id: id.clone() });
            };
            if !jobs.is_live_top_level_exec(monitor.job) {
                return Err(MonitorError::NotRunning {
                    job: monitor.job_display.clone(),
                });
            }
            monitor.stopped = false;
            monitor.muted = false;
            monitor.paused = false;
            Ok(MonitorReply::Rearmed {
                monitor: monitor_id,
            })
        }
    }
}

/// Stops every live watch without ending it; rearms stay possible while
/// their jobs run. Returns the number of newly stopped watches.
pub(crate) fn stop_all(state: &mut MonitorState) -> usize {
    let mut stopped = 0;
    for monitor in state.monitors.values_mut() {
        if !monitor.stopped {
            monitor.stopped = true;
            stopped += 1;
        }
    }
    stopped
}
