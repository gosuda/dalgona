// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Loop-guard reducer: canonical tool records, notice gates, and the
//! identical-call escalation episode.

use std::collections::{HashMap, VecDeque};

use dal_core::RawJson;
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};

use super::detect::{Detection, detect, read_path};
use super::{
    BLOCKED_RECOVERY, GuardError, LOOP_HARD_STOP_REASON, LOOP_HARD_STOP_WARNING, LOOP_P1_RECOVERY,
    POLL_RECOVERY, RECORD_CAPACITY, render_template,
};

/// Which detector produced a notice; each kind gates independently.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum DetectorKind {
    Identical,
    Cycle,
    Similar,
}

/// Notice admission state for one detector fingerprint.
#[derive(Clone, Debug)]
pub(crate) struct GateEntry {
    pub(crate) last_admitted: u32,
    pub(crate) saturation_admitted: bool,
}

/// One canonical tool call kept in the 64-record window.
#[derive(Clone, Debug)]
pub(crate) struct ToolCallRecord {
    pub(crate) tool: Box<str>,
    pub(crate) canonical: Box<str>,
    pub(crate) signature: Box<str>,
}

/// Escalation state for one repeated signature.
#[derive(Clone, Debug)]
pub(crate) struct Episode {
    pub(crate) fingerprint: Box<str>,
    pub(crate) tool: Box<str>,
    pub(crate) notices: u8,
    pub(crate) arm_after_this: bool,
    pub(crate) block_active: bool,
    pub(crate) blocked: u32,
    pub(crate) hard_stopped: u8,
}

/// Mutable guard state owned by the orchestration core's one task.
#[derive(Debug)]
pub(crate) struct GuardState {
    pub(crate) records: VecDeque<ToolCallRecord>,
    pub(crate) gates: HashMap<DetectorKind, HashMap<Box<str>, GateEntry>>,
    pub(crate) episode: Option<Episode>,
    pub(super) pending_attempts: VecDeque<ToolCallRecord>,
}

impl Default for GuardState {
    fn default() -> Self {
        Self {
            records: VecDeque::with_capacity(RECORD_CAPACITY),
            gates: HashMap::new(),
            episode: None,
            pending_attempts: VecDeque::with_capacity(RECORD_CAPACITY),
        }
    }
}

/// Whether the current call may proceed.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum GuardVerdict {
    Allow,
    Block { reason: Box<str> },
}

/// Everything the core needs after one tool call: the verdict plus at most
/// one steer, one warning, one P1 recovery, and one pause reason.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GuardEffects {
    pub verdict: GuardVerdict,
    pub steer: Option<Box<str>>,
    pub cancel_turn: bool,
    pub warning: Option<Box<str>>,
    pub p1_recovery: Option<Box<str>>,
    pub pause_reason: Option<&'static str>,
}

impl GuardEffects {
    pub(super) fn allow() -> Self {
        Self {
            verdict: GuardVerdict::Allow,
            steer: None,
            cancel_turn: false,
            warning: None,
            p1_recovery: None,
            pause_reason: None,
        }
    }
}

/// Serializes one JSON value with object keys sorted by UTF-8 byte order,
/// array order kept, and standard string escaping.
fn append_canonical(value: &Value, output: &mut String) -> Result<(), GuardError> {
    if let Some(object) = value.as_object() {
        let mut members: Vec<_> = object.iter().collect();
        members.sort_by(|(left, _), (right, _)| left.as_bytes().cmp(right.as_bytes()));
        output.push('{');
        for (index, (key, member)) in members.into_iter().enumerate() {
            if index != 0 {
                output.push(',');
            }
            let quoted_key = sonic_rs::to_string(key).map_err(GuardError::json)?;
            output.push_str(&quoted_key);
            output.push(':');
            append_canonical(member, output)?;
        }
        output.push('}');
    } else if let Some(array) = value.as_array() {
        output.push('[');
        for (index, item) in array.iter().enumerate() {
            if index != 0 {
                output.push(',');
            }
            append_canonical(item, output)?;
        }
        output.push(']');
    } else {
        output.push_str(&sonic_rs::to_string(value).map_err(GuardError::json)?);
    }
    Ok(())
}

/// Parses raw tool arguments once; every consumer below reuses the value.
pub(super) fn parse_args(args: &RawJson) -> Result<Value, GuardError> {
    sonic_rs::from_str(args.as_str()).map_err(GuardError::json)
}

/// Canonicalizes one parsed value; a top-level JSON `null` maps to `{}`.
pub(crate) fn canonical_args(value: &Value) -> Result<Box<str>, GuardError> {
    if value.is_null() {
        return Ok("{}".into());
    }
    let mut canonical = String::new();
    append_canonical(value, &mut canonical)?;
    Ok(canonical.into_boxed_str())
}

/// Records one tool call and returns its guard effects. Blocked calls stay
/// counted; a fresh signature ends the escalation episode.
pub(crate) fn on_tool_call(
    state: &mut GuardState,
    tool: &str,
    args: &RawJson,
) -> Result<GuardEffects, GuardError> {
    let parsed = parse_args(args)?;
    let canonical = canonical_args(&parsed)?;
    let mut signature = String::with_capacity(tool.len() + canonical.len() + 1);
    signature.push_str(tool);
    signature.push('\0');
    signature.push_str(&canonical);
    let record = ToolCallRecord {
        tool: tool.into(),
        canonical,
        signature: signature.clone().into_boxed_str(),
    };
    push_record(&mut state.records, record.clone());
    push_record(&mut state.pending_attempts, record);

    if state
        .episode
        .as_ref()
        .is_some_and(|episode| episode.fingerprint.as_ref() != signature.as_str())
    {
        state.episode = None;
    }

    if state.episode.as_ref().is_some_and(|episode| {
        episode.block_active && episode.fingerprint.as_ref() == signature.as_str()
    }) {
        return Ok(block_call(state, tool, &parsed));
    }

    let detection = detect(&state.records, &parsed)?;
    let mut active = HashMap::<DetectorKind, Box<str>>::new();
    if let Some(detection) = detection {
        active.insert(detection.kind(), detection.fingerprint().into());
        let admitted = admit_detection(state, &detection);
        let mut effects = GuardEffects::allow();
        if admitted {
            effects.steer = Some(detection.notice());
            if let Detection::Identical {
                fingerprint, tool, ..
            } = detection
            {
                advance_episode(state, fingerprint, tool);
            }
        }
        prune_gates(state, &active);
        arm_after_current_call(state);
        return Ok(effects);
    }
    prune_gates(state, &active);
    arm_after_current_call(state);
    Ok(GuardEffects::allow())
}

/// Keeps only the newest 64 records.
fn push_record(records: &mut VecDeque<ToolCallRecord>, record: ToolCallRecord) {
    if records.len() == RECORD_CAPACITY {
        records.pop_front();
    }
    records.push_back(record);
}

/// Blocks one same-signature call inside an armed episode. The third blocked
/// call cancels the turn with one warning and one P1 recovery; a further
/// blocked call in the same episode pauses instead and emits no warning.
fn block_call(state: &mut GuardState, tool: &str, args: &Value) -> GuardEffects {
    let polling_job = (tool == "agents"
        && args
            .as_object()
            .and_then(|object| object.get(&"action"))
            .and_then(|value| value.as_str())
            == Some("wait"))
        || (tool == "read" && read_path(args).is_some_and(|path| path.starts_with("job://")));
    let recovery = if polling_job {
        POLL_RECOVERY
    } else {
        BLOCKED_RECOVERY
    };
    let Some(episode) = state.episode.as_mut() else {
        return GuardEffects::allow();
    };
    episode.blocked = episode.blocked.saturating_add(1);
    let blocked = episode.blocked;
    let tool_name = episode.tool.as_ref();
    let reason = format!(
        "Loop guard blocked repeated call {blocked} to `{tool_name}` with arguments that already triggered two identical-call warnings. {recovery}"
    );
    let mut effects = GuardEffects {
        verdict: GuardVerdict::Block {
            reason: reason.into_boxed_str(),
        },
        steer: None,
        cancel_turn: false,
        warning: None,
        p1_recovery: None,
        pause_reason: None,
    };
    if blocked >= 3 {
        effects.cancel_turn = true;
        if episode.hard_stopped == 0 {
            let blocked_text = blocked.to_string().into_boxed_str();
            effects.warning = Some(render_template(
                LOOP_HARD_STOP_WARNING,
                &[("<k>", &blocked_text), ("<tool>", &episode.tool)],
            ));
            effects.p1_recovery = Some(
                LOOP_P1_RECOVERY
                    .replace("<tool>", &episode.tool)
                    .into_boxed_str(),
            );
        } else {
            effects.pause_reason = Some(LOOP_HARD_STOP_REASON);
        }
        episode.hard_stopped = episode.hard_stopped.saturating_add(1);
    }
    effects
}

/// Admits a detection when its fingerprint is new, when its count is at
/// least twice the last admitted count, or once at window saturation.
fn admit_detection(state: &mut GuardState, detection: &Detection) -> bool {
    let gates = state.gates.entry(detection.kind()).or_default();
    let count = detection.count();
    let saturation = detection.saturation();
    match gates.get_mut(detection.fingerprint()) {
        None => {
            gates.insert(
                detection.fingerprint().into(),
                GateEntry {
                    last_admitted: count,
                    saturation_admitted: count >= saturation,
                },
            );
            true
        }
        Some(entry) if count >= entry.last_admitted.saturating_mul(2) => {
            entry.last_admitted = count;
            if count >= saturation {
                entry.saturation_admitted = true;
            }
            true
        }
        Some(entry) if count >= saturation && !entry.saturation_admitted => {
            entry.last_admitted = count;
            entry.saturation_admitted = true;
            true
        }
        Some(_) => false,
    }
}

/// Forgets every gate entry whose fingerprint left the active set.
fn prune_gates(state: &mut GuardState, active: &HashMap<DetectorKind, Box<str>>) {
    for (kind, gates) in &mut state.gates {
        if let Some(fingerprint) = active.get(kind) {
            gates.retain(|known, _| known == fingerprint);
        } else {
            gates.clear();
        }
    }
}

/// Tracks identical-notice escalation: the first notice counts as notice one,
/// the second arms the block after the current call, which remains allowed.
fn advance_episode(state: &mut GuardState, fingerprint: Box<str>, tool: Box<str>) {
    match state.episode.as_mut() {
        Some(episode) if episode.fingerprint == fingerprint => {
            episode.notices = episode.notices.saturating_add(1);
            if episode.notices >= 2 && !episode.block_active {
                episode.arm_after_this = true;
            }
        }
        _ => {
            state.episode = Some(Episode {
                fingerprint,
                tool,
                notices: 1,
                arm_after_this: false,
                block_active: false,
                blocked: 0,
                hard_stopped: 0,
            });
        }
    }
}

/// Activates an armed block after the current allowed call settles.
fn arm_after_current_call(state: &mut GuardState) {
    if let Some(episode) = state.episode.as_mut()
        && episode.arm_after_this
    {
        episode.block_active = true;
        episode.arm_after_this = false;
    }
}

/// Resets records, gates, episode, and pending attempts on `session_start`,
/// `session_end`, and person-sourced `input`.
pub(crate) fn reset(state: &mut GuardState) {
    state.records.clear();
    state.gates.clear();
    state.episode = None;
    state.pending_attempts.clear();
}

/// Clears only pending attempts on `turn_end`.
pub(crate) fn clear_pending_attempts(state: &mut GuardState) {
    state.pending_attempts.clear();
}
