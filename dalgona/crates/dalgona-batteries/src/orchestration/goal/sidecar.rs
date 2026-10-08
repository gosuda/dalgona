// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Goal sidecar codec: strict `goal.json` decode, byte-exact encode, wire
//! names, and the read-only goal projection.

use dal_core::Timestamp;
use serde::{Deserialize, Serialize};
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};

use super::super::monitor::GoalPreview;
use super::super::{ControllerMode, GoalStatus};

/// Wire name for one goal lifecycle state.
#[must_use]
pub(crate) fn goal_status_wire(status: GoalStatus) -> &'static str {
    match status {
        GoalStatus::Active => "active",
        GoalStatus::Paused => "paused",
        GoalStatus::Blocked => "blocked",
        GoalStatus::Complete => "complete",
    }
}

/// Wire name for one controller mode.
#[must_use]
pub(crate) fn controller_wire(mode: ControllerMode) -> &'static str {
    match mode {
        ControllerMode::Run => "run",
        ControllerMode::Paused { .. } => "paused",
        ControllerMode::Stopped => "stopped",
    }
}

/// Goal lifecycle states shared from the orchestration core.
fn parse_status(text: &str) -> Option<GoalStatus> {
    match text {
        "active" => Some(GoalStatus::Active),
        "paused" => Some(GoalStatus::Paused),
        "blocked" => Some(GoalStatus::Blocked),
        "complete" => Some(GoalStatus::Complete),
        _ => None,
    }
}

/// Serde bridge for goal lifecycle states.
mod goal_status_serde {
    use super::{GoalStatus, parse_status};
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(
        status: &GoalStatus,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(super::goal_status_wire(*status))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<GoalStatus, D::Error> {
        let text = Box::<str>::deserialize(deserializer)?;
        parse_status(&text)
            .ok_or_else(|| serde::de::Error::custom(format!("invalid goal status `{text}`")))
    }
}

/// Serde bridge for controller modes. The wire carries only the mode name;
/// a persisted `paused` restores as freshly opened.
mod controller_serde {
    use super::{ControllerMode, controller_wire};
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(
        mode: &ControllerMode,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(controller_wire(*mode))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<ControllerMode, D::Error> {
        let text = Box::<str>::deserialize(deserializer)?;
        match text.as_ref() {
            "run" => Ok(ControllerMode::Run),
            "paused" => Ok(ControllerMode::Paused {
                reason: "session opened",
            }),
            "stopped" => Ok(ControllerMode::Stopped),
            _ => Err(serde::de::Error::custom(format!(
                "invalid controller mode `{text}`"
            ))),
        }
    }
}

/// Renders one timestamp as RFC 3339 UTC with exactly three fractional
/// digits and `Z`, truncating sub-millisecond precision.
fn format_millis(stamp: Timestamp) -> String {
    let text = stamp.to_string();
    let body = text.strip_suffix('Z').unwrap_or(&text);
    match body.split_once('.') {
        None => format!("{body}.000Z"),
        Some((head, frac)) => {
            let mut millis = String::with_capacity(3);
            for digit in frac.chars().take(3) {
                millis.push(digit);
            }
            while millis.len() < 3 {
                millis.push('0');
            }
            format!("{head}.{millis}Z")
        }
    }
}

/// Serde bridge for timestamps with mandatory milliseconds.
mod ts_millis {
    use super::{Timestamp, format_millis};
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(
        stamp: &Timestamp,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format_millis(*stamp))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Timestamp, D::Error> {
        let text = Box::<str>::deserialize(deserializer)?;
        text.parse::<Timestamp>()
            .map_err(|error| serde::de::Error::custom(error.to_string()))
    }

    /// Nullable variant for `last_signature`-style optional timestamps.
    pub(super) mod opt {
        use super::super::{Timestamp, format_millis};
        use serde::{Deserialize, Deserializer, Serializer};

        pub(in super::super) fn serialize<S: Serializer>(
            stamp: &Option<Timestamp>,
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            match stamp {
                None => serializer.serialize_none(),
                Some(stamp) => serializer.serialize_str(&format_millis(*stamp)),
            }
        }

        pub(in super::super) fn deserialize<'de, D: Deserializer<'de>>(
            deserializer: D,
        ) -> Result<Option<Timestamp>, D::Error> {
            let raw: Option<Box<str>> = Option::deserialize(deserializer)?;
            raw.map(|text| {
                text.parse::<Timestamp>()
                    .map_err(|error| serde::de::Error::custom(error.to_string()))
            })
            .transpose()
        }
    }
}

/// Durable goal document. Field order follows the declaration for the exact
/// persisted shape.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GoalSidecar {
    /// Document version, always `1`.
    pub(crate) v: u32,
    /// Owning session id.
    pub(crate) session: Box<str>,
    /// Controller mode at the last write.
    #[serde(with = "controller_serde")]
    pub(crate) controller: ControllerMode,
    /// Next goal number; creation takes it and increments.
    pub(crate) next_goal: u64,
    /// The current goal, or `None`.
    pub(crate) goal: Option<Goal>,
}

/// One durable goal with its continuation counters.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Goal {
    /// Session-local id `g<n>`.
    pub(crate) id: Box<str>,
    /// Concrete objective, 1 through 4000 Unicode scalar values.
    pub(crate) objective: Box<str>,
    /// Lifecycle state.
    #[serde(with = "goal_status_serde")]
    pub(crate) status: GoalStatus,
    /// Creation time.
    #[serde(with = "ts_millis")]
    pub(crate) created_at: Timestamp,
    /// Last mutation time.
    #[serde(with = "ts_millis")]
    pub(crate) updated_at: Timestamp,
    /// Summed model tokens across goal turns.
    pub(crate) tokens_used: u64,
    /// Summed wall seconds across goal turns.
    pub(crate) time_used_s: u64,
    /// Consecutive continuations on the current progress signature.
    pub(crate) consecutive: u32,
    /// Continuations delivered without a user message.
    pub(crate) unattended: u32,
    /// Minimal-prompt length recoveries on the current signature.
    pub(crate) length_recoveries: u32,
    /// Consecutive tool-less goal turns.
    pub(crate) toolless_streak: u32,
    /// Goal turns since the goal became active or the user last spoke.
    pub(crate) goal_turns: u32,
    /// Progress signature delivered with the last continuation.
    pub(crate) last_signature: Option<Box<str>>,
    /// Last three output hashes for repetition detection.
    pub(crate) recent_hashes: Vec<Box<str>>,
    /// Block reason, if blocked.
    pub(crate) blocked: Option<BlockedReason>,
    /// Completion time, if complete.
    #[serde(with = "ts_millis::opt")]
    pub(crate) completed_at: Option<Timestamp>,
}

/// Why a goal stopped with `blocked`.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BlockedReason {
    /// Human blocker description, preserved exactly.
    pub(crate) reason: Box<str>,
    /// Time of the block.
    #[serde(with = "ts_millis")]
    pub(crate) at: Timestamp,
    /// Whether a user prompt reactivates the goal.
    pub(crate) mechanical: bool,
}

/// Goal scoping, persistence, and tool failures with exact user texts.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum GoalError {
    /// The session is ephemeral.
    #[error(
        "goal: this session is not saved, so it cannot hold a goal. Start a saved session to use goals."
    )]
    NotSaved,
    /// A child session attempted goal state.
    #[error("goal: a subagent cannot hold a goal. Report to the agent that started you.")]
    Child,
    /// The sidecar belongs to another session.
    #[error("goal: the goal file belongs to session {other}, not this one. dalgona ignores it.")]
    SessionMismatch { other: Box<str> },
    /// The sidecar version is not 1.
    #[error("goal: the goal file has version {version}; this dalgona reads version 1.")]
    BadVersion { version: Box<str> },
    /// The sidecar cannot be decoded.
    #[error(
        "goal: the goal file is damaged: {error}. dalgona continues no goal until you run /goal clear."
    )]
    Damaged { error: Box<str> },
    /// A sidecar write failed.
    #[error("goal: saving the goal failed: {message}.")]
    SaveFailed { message: Box<str> },
    /// The sidecar service is unavailable.
    #[error("goal: the session store is not available: {message}.")]
    StoreUnavailable { message: Box<str> },
    /// An unfinished goal blocks creation.
    #[error(
        "create_goal: this session already has an unfinished goal ({id}, {status}). Use update_goal when it is complete."
    )]
    CreateUnfinished { id: Box<str>, status: Box<str> },
    /// The objective exceeds 4000 characters.
    #[error(
        "create_goal: the objective has {count} characters; the limit is 4000. Put the full objective in a file and name the file in the objective."
    )]
    CreateTooLong { count: usize },
    /// The objective is empty.
    #[error("create_goal: the objective is empty.")]
    CreateEmpty,
    /// No goal exists for an update.
    #[error("update_goal: no goal in this session.")]
    UpdateMissing,
    /// The goal is not active for an update.
    #[error("update_goal: the goal is {status}, not active.")]
    UpdateNotActive { status: Box<str> },
    /// A blocked update lacks a reason.
    #[error("update_goal: reason is required when status is blocked.")]
    UpdateNeedReason,
    /// A complete update carries a reason.
    #[error("update_goal: reason must not be given when status is complete.")]
    UpdateNoReason,
    /// Open todos block completion.
    #[error("update_goal: {count} todo tasks are still open: {titles}.")]
    UpdateTodosOpen { count: usize, titles: Box<str> },
    /// Live sources block a model block.
    #[error(
        "update_goal: blocked is rejected while {parts} can still deliver. End the turn and let them wake you."
    )]
    UpdateInflight { parts: Box<str> },
    /// Too few goal turns block a model block.
    #[error(
        "update_goal: blocked is rejected until the goal has had 3 goal turns since it became active or the user last spoke; it has had {turns}."
    )]
    UpdateTooEarly { turns: u32 },
}

fn damaged(error: impl core::fmt::Display) -> GoalError {
    GoalError::Damaged {
        error: error.to_string().into(),
    }
}

/// Required top-level sidecar members in document order.
const SIDECAR_KEYS: [&str; 5] = ["v", "session", "controller", "next_goal", "goal"];

/// Decodes one sidecar document strictly: rejects non-objects, missing or
/// unknown members, unknown versions, session mismatches, invalid statuses,
/// and invalid timestamps. Bad or denied reads arm no continuation.
///
/// # Errors
///
/// Returns the exact scoping or damage error for refused input.
pub(crate) fn decode_sidecar(
    bytes: &[u8],
    current_session: &str,
) -> Result<GoalSidecar, GoalError> {
    let text = core::str::from_utf8(bytes)
        .map_err(|error| damaged(format_args!("invalid UTF-8: {error}")))?;
    let stripped = text.strip_suffix('\n').unwrap_or(text);
    let value: Value = sonic_rs::from_str(stripped)
        .map_err(|error: sonic_rs::Error| damaged(format_args!("invalid JSON: {error}")))?;
    let Some(object) = value.as_object() else {
        return Err(damaged("expected one JSON object"));
    };
    for key in SIDECAR_KEYS {
        if object.get(&key).is_none() {
            return Err(damaged(format_args!("missing member `{key}`")));
        }
    }
    for (key, _) in object {
        if !SIDECAR_KEYS.contains(&key) {
            return Err(damaged(format_args!("unknown member `{key}`")));
        }
    }
    let session = object
        .get(&"session")
        .and_then(|member| member.as_str())
        .ok_or_else(|| damaged("invalid `session` member"))?;
    if session != current_session {
        return Err(GoalError::SessionMismatch {
            other: session.into(),
        });
    }
    let version = object
        .get(&"v")
        .ok_or_else(|| damaged("missing member `v`"))?;
    if version.as_u64() != Some(1) {
        let rendered = sonic_rs::to_string(version).unwrap_or_else(|_| "1".to_owned());
        return Err(GoalError::BadVersion {
            version: rendered.into_boxed_str(),
        });
    }
    let sidecar: GoalSidecar = sonic_rs::from_str(stripped)
        .map_err(|error: sonic_rs::Error| damaged(format_args!("{error}")))?;
    validate_hashes(&sidecar)?;
    Ok(sidecar)
}

/// Rejects hash lists longer than three entries or entries that are not
/// eight lowercase hex digits.
fn validate_hashes(sidecar: &GoalSidecar) -> Result<(), GoalError> {
    let Some(goal) = sidecar.goal.as_ref() else {
        return Ok(());
    };
    if goal.recent_hashes.len() > 3 {
        return Err(damaged("`recent_hashes` holds more than three entries"));
    }
    for hash in &goal.recent_hashes {
        let valid = hash.len() == 8 && hash.bytes().all(|byte| byte.is_ascii_hexdigit());
        let lower = hash
            .bytes()
            .all(|byte| !byte.is_ascii_alphabetic() || byte.is_ascii_lowercase());
        if !valid || !lower {
            return Err(damaged(format_args!(
                "`recent_hashes` entry `{hash}` is not eight lowercase hex digits"
            )));
        }
    }
    Ok(())
}

/// Encodes one sidecar document with a trailing LF. Field order follows the
/// struct declaration for the exact persisted shape.
///
/// # Errors
///
/// Returns [`GoalError::Damaged`] when serialization fails.
pub(crate) fn encode_sidecar(sidecar: &GoalSidecar) -> Result<Vec<u8>, GoalError> {
    let mut text = sonic_rs::to_string(sidecar).map_err(|error: sonic_rs::Error| damaged(error))?;
    text.push('\n');
    Ok(text.into_bytes())
}

/// Projects one goal to its read-only status preview, truncating the
/// objective to its first 32 Unicode scalar values.
#[must_use]
pub(crate) fn goal_projection(goal: Option<&Goal>) -> Option<GoalPreview> {
    goal.map(|goal| GoalPreview {
        id: goal.id.clone(),
        status: goal.status,
        objective: goal
            .objective
            .chars()
            .take(32)
            .collect::<String>()
            .into_boxed_str(),
    })
}
