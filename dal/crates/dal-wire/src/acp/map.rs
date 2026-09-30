//! ACP update, request, stop, and content mapping.
//!
//! Every core update becomes zero or more `session/update` bodies per the
//! version tables. Request questions become `session/request_permission` (or
//! `elicitation/create`) server-initiated requests; client answers map back
//! to core [`Answer`] values. Extension status updates become
//! `_dal/notice` bodies of kind `status`; unknown update variants map to
//! nothing.

use dal_core::{SessionId, Stop, StreamChannel, TurnCause, TurnId, Update, UpdateKind};
use sonic_rs::{JsonValueMutTrait, JsonValueTrait, Value};

use super::AcpVersion;

pub(crate) mod pump;
mod request;

pub(crate) use pump::{PromptEnd, PumpCtx, prompt_pump};
pub(crate) use request::{
    cancel_one, cancel_unaskable, may_elicit, permission_flow, resolved_elsewhere,
};

/// Maps one update to its `session/update` bodies (no side effects).
pub(crate) fn map_update(
    version: AcpVersion,
    session: SessionId,
    update: &Update,
    prompt_turn: Option<TurnId>,
) -> Vec<Value> {
    match &update.kind {
        UpdateKind::TurnStarted { turn, cause } => turn_started(version, session, *turn, *cause),
        UpdateKind::TurnEnded { turn, stop } => {
            if Some(*turn) == prompt_turn {
                vec![Value::from("prompt-turn")]
            } else {
                turn_ended_notice(version, session, *turn, *stop)
            }
        }
        UpdateKind::Delta {
            turn,
            channel,
            text,
        } => delta_body(*turn, channel, text),
        UpdateKind::ToolStarted { .. }
        | UpdateKind::ToolProgress { .. }
        | UpdateKind::ToolSettled { .. } => tool_update(version, &update.kind),
        UpdateKind::ExtStatus(status) => {
            vec![notice_body(session, "status", &status.to_string())]
        }
        _ => session_update(session, &update.kind),
    }
}

/// Maps a turn start: v1 notes non-user causes, v2 reports running.
fn turn_started(
    version: AcpVersion,
    session: SessionId,
    turn: TurnId,
    cause: TurnCause,
) -> Vec<Value> {
    if version == AcpVersion::V2 {
        return vec![state_update("running", None)];
    }
    match cause {
        TurnCause::User => Vec::new(),
        TurnCause::Wake => vec![notice_body(
            session,
            "info",
            &format!("turn {turn} started by an extension"),
        )],
        TurnCause::FollowUp => vec![notice_body(
            session,
            "info",
            &format!("turn {turn} started as a follow-up"),
        )],
    }
}

/// Maps one streamed text or thinking delta.
fn delta_body(turn: TurnId, channel: &StreamChannel, text: &str) -> Vec<Value> {
    let (kind, suffix) = match channel {
        StreamChannel::Text => ("agent_message_chunk", "agent"),
        StreamChannel::Thinking => ("agent_thought_chunk", "thought"),
        StreamChannel::ToolArgs { .. } => return Vec::new(),
    };
    vec![sonic_rs::json!({
        "sessionUpdate": kind,
        "content": {"type": "text", "text": text},
        "messageId": format!("{turn}:{suffix}"),
    })]
}

/// Maps tool start, progress, and settle updates.
fn tool_update(version: AcpVersion, kind: &UpdateKind) -> Vec<Value> {
    match kind {
        UpdateKind::ToolStarted { call, tool, args } => {
            let input = args.as_str();
            let update = match version {
                AcpVersion::V1 => "tool_call",
                AcpVersion::V2 => "tool_call_update",
            };
            vec![sonic_rs::json!({
                "sessionUpdate": update,
                "toolCallId": call.as_str(),
                "title": tool_title(tool, input),
                "kind": tool_kind(tool),
                "status": "pending",
                "rawInput": input,
                "locations": tool_locations(input),
            })]
        }
        UpdateKind::ToolProgress { call, tail } => {
            let text: &str = tail.as_ref();
            match version {
                AcpVersion::V1 => vec![sonic_rs::json!({
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": call.as_str(),
                    "status": "in_progress",
                    "content": [{"type": "content", "content": {"type": "text", "text": text_tail(text, 4096)}}],
                })],
                AcpVersion::V2 => vec![sonic_rs::json!({
                    "sessionUpdate": "tool_call_content_chunk",
                    "toolCallId": call.as_str(),
                    "content": {"type": "content", "content": {"type": "text", "text": text}},
                })],
            }
        }
        UpdateKind::ToolSettled { call, outcome } => tool_settled_body(
            version,
            call.as_str(),
            outcome.is_error,
            outcome.text.as_ref(),
        ),
        _ => Vec::new(),
    }
}

/// Maps session-level updates: notices, jobs, settings, and usage.
fn session_update(session: SessionId, kind: &UpdateKind) -> Vec<Value> {
    match kind {
        UpdateKind::RuleFired { rule, .. } => vec![notice_body(
            session,
            "rule_fired",
            &format!("rule {rule} fired"),
        )],
        UpdateKind::Notice(notice) => {
            vec![notice_body(
                session,
                notice_kind(&notice.kind),
                &notice.text,
            )]
        }
        UpdateKind::JobStarted { job } => {
            vec![notice_body(session, "info", &format!("job {job} started"))]
        }
        UpdateKind::JobSettled { job, .. } => {
            vec![notice_body(session, "info", &format!("job {job} settled"))]
        }
        UpdateKind::Settings(settings) => settings
            .name
            .as_deref()
            .map(|name| sonic_rs::json!({"sessionUpdate": "session_info_update", "title": name}))
            .into_iter()
            .collect(),
        UpdateKind::Usage(usage) => {
            let mut body = sonic_rs::json!({
                "sessionUpdate": "usage_update",
                "used": usage.context_tokens,
                "size": usage.context_window,
            });
            if let Some(amount) = usage.usage.cost_usd
                && let Some(object) = body.as_object_mut()
            {
                object.insert(
                    "cost",
                    sonic_rs::json!({"amount": amount, "currency": "USD"}),
                );
            }
            vec![body]
        }
        _ => Vec::new(),
    }
}

/// Builds one `_dal/notice` body.
pub(super) fn notice_body(session: SessionId, kind: &str, text: &str) -> Value {
    sonic_rs::json!({
        "_dal/notice": {"sessionId": session.to_string(), "kind": kind, "text": text},
    })
}

/// Maps a core notice kind to its wire kind.
fn notice_kind(kind: &str) -> &str {
    match kind {
        "warning" | "warn" => "warning",
        "status" => "status",
        _ => "info",
    }
}

/// Builds one v2 running/idle state update body.
pub(super) fn state_update(state: &str, stop: Option<Value>) -> Value {
    let mut body = sonic_rs::json!({
        "sessionUpdate": "state_update",
        "state": state,
    });
    if let Some(stop) = stop
        && let Some(object) = body.as_object_mut()
    {
        object.insert("stopReason", stop);
    }
    body
}

/// Builds one v2 idle `state_update` body with an optional stop reason.
pub(crate) fn state_update_idle(stop: Option<Value>) -> Value {
    state_update("idle", stop)
}

/// Maps a non-prompt turn end to its notice or state update.
fn turn_ended_notice(
    version: AcpVersion,
    session: SessionId,
    turn: TurnId,
    stop: Stop,
) -> Vec<Value> {
    match version {
        AcpVersion::V1 => {
            let kind = if matches!(stop, Stop::Failed) {
                "warning"
            } else {
                "info"
            };
            let text = format!("turn {turn} ended: {}", stop_text(stop));
            vec![notice_body(session, kind, &text)]
        }
        AcpVersion::V2 => vec![state_update("idle", Some(stop_literal(stop)))],
    }
}

/// Renders the wire stop text for notices.
pub(super) fn stop_text(stop: Stop) -> &'static str {
    match stop {
        Stop::EndTurn => "end_turn",
        Stop::Length => "max_tokens",
        Stop::MaxSteps => "max_turn_requests",
        Stop::Filter => "refusal",
        Stop::Cancelled => "cancelled",
        Stop::Failed => "failed",
    }
}

/// Renders one turn stop as its ACP literal: a stop-reason string, or a
/// `failed` object carrying the message and hint for the v1 error shape.
pub(crate) fn stop_literal(stop: Stop) -> Value {
    if matches!(stop, Stop::Failed) {
        sonic_rs::json!({"failed": {"message": "the turn failed", "hint": "report this"}})
    } else {
        Value::from(stop_text(stop))
    }
}
fn tool_settled_body(version: AcpVersion, call: &str, is_error: bool, text: &str) -> Vec<Value> {
    let status = if is_error { "failed" } else { "completed" };
    match version {
        AcpVersion::V1 => vec![sonic_rs::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": call,
            "status": status,
            "content": [{"type": "content", "content": {"type": "text", "text": text}}],
            "rawOutput": {"text": text},
        })],
        AcpVersion::V2 => vec![sonic_rs::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": call,
            "status": status,
            "content": [{"type": "content", "content": {"type": "text", "text": text}}],
        })],
    }
}
/// Builds a tool call title: the tool name plus the first string member
/// among `path`, `command`, `pattern`, and `code` (first line only).
fn tool_title(tool: &str, input: &str) -> String {
    let value: Value = sonic_rs::from_str(input).unwrap_or(Value::from(false));
    for member in ["path", "command", "pattern", "code"] {
        if let Some(text) = value.get(member).and_then(|value| value.as_str())
            && !text.is_empty()
        {
            let first = text.lines().next().unwrap_or("");
            return format!("{tool} {first}");
        }
    }
    tool.to_owned()
}

/// Maps a tool name to its ACP kind.
fn tool_kind(tool: &str) -> &'static str {
    match tool {
        "read" => "read",
        "search" => "search",
        "patch" => "edit",
        "exec" | "eval" => "execute",
        _ => "other",
    }
}

/// Builds the locations array from a string `path` input member.
fn tool_locations(input: &str) -> Value {
    let value: Value = sonic_rs::from_str(input).unwrap_or(Value::from(false));
    match value.get("path").and_then(|value| value.as_str()) {
        Some(path) => sonic_rs::json!([{"path": path}]),
        None => sonic_rs::json!([]),
    }
}

/// Returns the last `limit` bytes of tail text.
fn text_tail(text: &str, limit: usize) -> &str {
    if text.len() <= limit {
        text
    } else {
        let mut start = text.len() - limit;
        while !text.is_char_boundary(start) {
            start += 1;
        }
        &text[start..]
    }
}
