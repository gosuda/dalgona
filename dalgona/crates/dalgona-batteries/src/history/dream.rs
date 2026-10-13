// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! Per-session auto-dream state and park policy.
use serde::{Deserialize, Serialize};
use sonic_rs::JsonContainerTrait;
use thiserror::Error;

use super::{DREAM_LETTER_THRESHOLD, IDENTICAL_PARK_THRESHOLD, TRANSIENT_PARK_THRESHOLD};

/// Decodes one `dream.json` sidecar body in field order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DreamFile {
    /// Sidecar format version, always `1`.
    pub v: u8,
    /// The session this sidecar belongs to.
    pub session: String,
    /// One-based position of the last consumed record.
    pub last_consolidated_letter: u64,
    /// Letters after the consolidated position.
    pub unreflected: u32,
    /// Failure park state.
    pub park: ParkFile,
    /// UTC RFC 3339 timestamp with milliseconds.
    pub updated_at: String,
}

/// Failure park counters carried inside `dream.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParkFile {
    /// Whether automatic consolidation is parked.
    pub parked: bool,
    /// Consecutive identical non-retryable failures.
    pub identical_streak: u8,
    /// Consecutive transient failures.
    pub transient_streak: u8,
    /// Last half-open probe timestamp, if any.
    pub last_probe_at: Option<String>,
    /// Sanitized text of the last non-retryable failure, if any.
    pub last_failure: Option<String>,
}

/// A `dream.json` body that fails version, session, or shape checks.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub(crate) enum DreamFileError {
    /// Unknown sidecar version.
    #[error("history: the dream file has version {found}; this dalgona reads version 1.")]
    Version {
        /// The rejected version.
        found: u8,
    },
    /// Sidecar belongs to another session.
    #[error(
        "history: the dream file belongs to session {other}, not this one. dalgona ignores it."
    )]
    Session {
        /// The rejected session id.
        other: String,
    },
    /// Unknown member.
    #[error("history: the dream file has an unknown key \"{key}\".")]
    UnknownKey {
        /// The rejected member.
        key: String,
    },
    /// Malformed JSON or wrong member shapes.
    #[error("history: the dream file is malformed.")]
    Malformed,
}

/// Decodes one `dream.json` sidecar body, naming unknown members.
///
/// # Errors
/// Returns `DreamFileError::Malformed` for non-UTF-8, non-JSON, non-object,
/// or mistyped bodies, and `DreamFileError::UnknownKey` for the first
/// unknown member of the file or its `park` table.
pub(crate) fn decode_dream_file(bytes: &[u8]) -> Result<DreamFile, DreamFileError> {
    let text = std::str::from_utf8(bytes).map_err(|_| DreamFileError::Malformed)?;
    if let Ok(file) = sonic_rs::from_str::<DreamFile>(text) {
        return Ok(file);
    }
    let value: sonic_rs::Value = sonic_rs::from_str(text).map_err(|_| DreamFileError::Malformed)?;
    let Some(object) = value.as_object() else {
        return Err(DreamFileError::Malformed);
    };
    for (key, member) in object {
        if !DREAM_FILE_KEYS.contains(&key) {
            return Err(DreamFileError::UnknownKey { key: key.into() });
        }
        if key == "park" {
            check_park_keys(member)?;
        }
    }
    Err(DreamFileError::Malformed)
}

fn check_park_keys(member: &sonic_rs::Value) -> Result<(), DreamFileError> {
    let Some(park) = member.as_object() else {
        return Ok(());
    };
    for (key, _) in park {
        if !PARK_FILE_KEYS.contains(&key) {
            return Err(DreamFileError::UnknownKey { key: key.into() });
        }
    }
    Ok(())
}

const DREAM_FILE_KEYS: [&str; 6] = [
    "v",
    "session",
    "last_consolidated_letter",
    "unreflected",
    "park",
    "updated_at",
];

const PARK_FILE_KEYS: [&str; 5] = [
    "parked",
    "identical_streak",
    "transient_streak",
    "last_probe_at",
    "last_failure",
];

/// Checks one decoded body's version and session against this session.
pub(crate) fn check_dream_file(file: &DreamFile, session: &str) -> Result<(), DreamFileError> {
    if file.v != 1 {
        return Err(DreamFileError::Version { found: file.v });
    }
    if file.session != session {
        return Err(DreamFileError::Session {
            other: file.session.clone(),
        });
    }
    Ok(())
}

/// Per-session dream lifecycle phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DreamPhase {
    /// Below threshold or waiting for idleness.
    Idle,
    /// Threshold met; the next `settled` may submit.
    Armed,
    /// One dream job is running.
    Running,
    /// Parked after repeated failures.
    Parked,
    /// One half-open probe is running.
    Probing,
}

/// Inputs to the pure dream state machine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum DreamEvent {
    /// A first-prompt input arrived.
    Input,
    /// An observe-only turn settled.
    Settled,
    /// The 20-minute idle timer fired.
    IdleTimer,
    /// The dream job succeeded.
    JobSucceeded,
    /// The dream job failed transiently.
    JobTransientFailure,
    /// The dream job failed non-retryably with sanitized text.
    JobIdenticalFailure(String),
    /// The dream job was cancelled; neutral, streaks unchanged.
    JobCancelled,
    /// The judge budget was exhausted; neutral, streaks unchanged.
    BudgetExhausted,
    /// The judge gate is off.
    JudgeOff,
    /// A half-open probe succeeded.
    ProbeSucceeded,
    /// A half-open probe failed.
    ProbeFailed,
    /// The session ended.
    SessionEnd,
}

/// Outputs of the pure dream state machine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum DreamAction {
    /// Reset the one 20-minute idle timer.
    ResetTimer,
    /// Submit one dream job.
    SubmitJob,
    /// Cancel the timer and any running job.
    Cancel,
    /// Emit one exact notice line.
    Notify(String),
}

/// Mutable per-session dream state owned by one session task.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DreamState {
    /// Current lifecycle phase.
    pub phase: DreamPhase,
    /// Unreflected letter count from records (source of truth).
    pub unreflected: usize,
    /// Whether the idle timer has elapsed.
    pub idle_elapsed: bool,
    /// Identical-failure park counter.
    pub identical_streak: u8,
    /// Transient-failure park counter.
    pub transient_streak: u8,
    /// Last non-retryable failure text.
    pub last_failure: Option<String>,
    /// Whether the park notice was already sent for this park.
    pub park_notified: bool,
    /// Whether a job is running.
    pub running: bool,
}

impl DreamState {
    /// Returns the initial idle state.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            phase: DreamPhase::Idle,
            unreflected: 0,
            idle_elapsed: false,
            identical_streak: 0,
            transient_streak: 0,
            last_failure: None,
            park_notified: false,
            running: false,
        }
    }
}

impl Default for DreamState {
    fn default() -> Self {
        Self::new()
    }
}

/// Returns whether a dream job may start.
#[must_use]
pub(crate) fn dream_ready(state: &DreamState) -> bool {
    state.unreflected >= DREAM_LETTER_THRESHOLD && state.idle_elapsed && !state.running
}

/// Advances pure dream state and returns the requested actions.
pub(crate) fn transition(state: &mut DreamState, event: DreamEvent) -> Vec<DreamAction> {
    match event {
        DreamEvent::Input => on_input(state),
        DreamEvent::Settled => on_settled(state),
        DreamEvent::IdleTimer => on_idle_timer(state),
        DreamEvent::JobSucceeded | DreamEvent::ProbeSucceeded => on_success(state),
        DreamEvent::JobTransientFailure => on_transient_failure(state),
        DreamEvent::JobIdenticalFailure(text) => on_identical_failure(state, text),
        DreamEvent::JobCancelled | DreamEvent::BudgetExhausted => on_neutral(state),
        DreamEvent::JudgeOff => on_judge_off(state),
        DreamEvent::ProbeFailed => on_probe_failed(state),
        DreamEvent::SessionEnd => on_session_end(state),
    }
}

/// Returns the first-park notice.
#[must_use]
pub(crate) fn park_notice() -> String {
    "Auto-dream paused after repeated failures (3 identical or 6 transient). It will retry once per 6 hours.".to_string()
}

/// Returns the exact non-parked failure notice.
#[must_use]
pub(crate) fn failed_notice() -> String {
    "Auto-dream failed; the batch remains unreflected.".to_string()
}

/// Returns the exact consolidation success notice for `count` letters.
#[must_use]
pub(crate) fn consolidated_notice(count: u32) -> String {
    format!("Auto-dream consolidated {count} letters.")
}

fn judge_off_notice() -> String {
    "Auto-dream skipped: the judge is off.".to_string()
}
fn on_judge_off(state: &mut DreamState) -> Vec<DreamAction> {
    state.running = false;
    if state.phase == DreamPhase::Running {
        state.phase = DreamPhase::Idle;
    }
    vec![DreamAction::Notify(judge_off_notice())]
}

fn on_input(state: &mut DreamState) -> Vec<DreamAction> {
    state.idle_elapsed = false;
    if state.phase != DreamPhase::Parked && state.phase != DreamPhase::Probing {
        state.phase = if state.unreflected >= DREAM_LETTER_THRESHOLD {
            DreamPhase::Armed
        } else {
            DreamPhase::Idle
        };
    }
    vec![DreamAction::ResetTimer]
}

fn on_settled(state: &mut DreamState) -> Vec<DreamAction> {
    if state.running || state.phase == DreamPhase::Parked || state.phase == DreamPhase::Probing {
        return Vec::new();
    }
    if state.unreflected < DREAM_LETTER_THRESHOLD {
        return Vec::new();
    }
    state.phase = DreamPhase::Running;
    state.running = true;
    vec![DreamAction::SubmitJob]
}

fn on_neutral(state: &mut DreamState) -> Vec<DreamAction> {
    state.running = false;
    match state.phase {
        DreamPhase::Running => state.phase = DreamPhase::Idle,
        DreamPhase::Probing => state.phase = DreamPhase::Parked,
        DreamPhase::Idle | DreamPhase::Armed | DreamPhase::Parked => {}
    }
    Vec::new()
}

fn on_idle_timer(state: &mut DreamState) -> Vec<DreamAction> {
    state.idle_elapsed = true;
    if state.phase == DreamPhase::Parked || state.phase == DreamPhase::Probing {
        return Vec::new();
    }
    if !dream_ready(state) {
        return Vec::new();
    }
    state.phase = DreamPhase::Running;
    state.running = true;
    vec![DreamAction::SubmitJob]
}

fn on_success(state: &mut DreamState) -> Vec<DreamAction> {
    state.running = false;
    state.phase = DreamPhase::Idle;
    state.identical_streak = 0;
    state.transient_streak = 0;
    state.last_failure = None;
    state.park_notified = false;
    Vec::new()
}

fn on_transient_failure(state: &mut DreamState) -> Vec<DreamAction> {
    state.running = false;
    state.transient_streak = state.transient_streak.saturating_add(1);
    state.identical_streak = 0;
    if state.transient_streak < TRANSIENT_PARK_THRESHOLD {
        state.phase = DreamPhase::Idle;
        return vec![DreamAction::Notify(failed_notice())];
    }
    state.phase = DreamPhase::Parked;
    if state.park_notified {
        return Vec::new();
    }
    state.park_notified = true;
    vec![DreamAction::Notify(park_notice())]
}

fn on_identical_failure(state: &mut DreamState, text: String) -> Vec<DreamAction> {
    state.running = false;
    if state.last_failure.as_deref() == Some(text.as_str()) {
        state.identical_streak = state.identical_streak.saturating_add(1);
    } else {
        state.identical_streak = 1;
        state.last_failure = Some(text);
    }
    state.transient_streak = 0;
    if state.identical_streak < IDENTICAL_PARK_THRESHOLD {
        state.phase = DreamPhase::Idle;
        return vec![DreamAction::Notify(failed_notice())];
    }
    state.phase = DreamPhase::Parked;
    if state.park_notified {
        return Vec::new();
    }
    state.park_notified = true;
    vec![DreamAction::Notify(park_notice())]
}

fn on_probe_failed(state: &mut DreamState) -> Vec<DreamAction> {
    state.running = false;
    state.phase = DreamPhase::Parked;
    vec![DreamAction::Notify(
        "Auto-dream remains paused; the next probe is in 6 hours.".to_string(),
    )]
}

fn on_session_end(state: &mut DreamState) -> Vec<DreamAction> {
    state.phase = DreamPhase::Idle;
    state.running = false;
    vec![DreamAction::Cancel]
}
