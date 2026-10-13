//! Server-sent event and response-object encoding for router families.
//!
//! Chat emits `OpenAI` SSE (`role` chunk first, deltas, tool-call chunks,
//! finish reason, optional usage chunk, `[DONE]`); Responses emits its ordered
//! event families with `sequence_number` from 1; Messages emits Anthropic SSE.
//! Reasoning never appears in Chat output; harness Messages carry no thinking
//! block. Harness turns and pass-through relays feed the same encoders.

use sonic_rs::Value;

use super::harness::{HarnessStop, HarnessTurn, UsageSum};

mod chat;
mod messages;
mod relay;
mod responses;

pub(crate) use chat::ChatEncoder;
pub(crate) use messages::MessagesEncoder;
pub(crate) use relay::{relay_chat_object, relay_message_object, relay_responses_object};
pub(crate) use responses::ResponsesEncoder;

/// The family wire ids of one turn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TurnIds {
    /// The Chat completion id.
    pub chat: String,
    /// The Responses response id.
    pub responses: String,
    /// The Messages message id.
    pub messages: String,
}

impl TurnIds {
    /// Builds the ids of one harness turn.
    pub(crate) fn harness(session: dal_core::SessionId, turn: dal_core::TurnId) -> Self {
        Self {
            chat: format!("chatcmpl-dal-{turn}"),
            responses: format!("resp_{session}.{turn}"),
            messages: format!("msg_dal_{turn}"),
        }
    }

    /// Builds the ids of one relay from a fresh time-ordered suffix.
    pub(crate) fn relay() -> Self {
        let suffix = uuid::Uuid::now_v7();
        Self {
            chat: format!("chatcmpl-dal-{suffix}"),
            responses: format!("resp_{suffix}"),
            messages: format!("msg_dal_{suffix}"),
        }
    }
}

/// The wire ids and echoed model of one non-stream reply.
pub(crate) struct StreamIds {
    /// The family wire ids.
    pub turn: TurnIds,
    /// The model id echoed in responses.
    pub model: String,
}

/// Builds the wire ids for one harness turn.
pub(crate) fn stream_ids(
    model: &str,
    session: dal_core::SessionId,
    turn: dal_core::TurnId,
) -> StreamIds {
    StreamIds {
        turn: TurnIds::harness(session, turn),
        model: model.to_owned(),
    }
}

/// Encodes one SSE data line.
fn data_line(payload: &str) -> String {
    format!("data: {payload}\n\n")
}

/// Encodes one SSE event line.
fn event_line(event: &str, payload: &str) -> String {
    format!("event: {event}\ndata: {payload}\n\n")
}

/// Encodes the JSON body of one line with byte-sorted keys and no
/// whitespace, so the bytes are deterministic.
fn json(value: &Value) -> String {
    super::canonical_json(value)
}

/// Builds the `OpenAI` Chat usage block from one sum.
fn usage_block(usage: &UsageSum) -> Value {
    sonic_rs::json!({
        "prompt_tokens": usage.input,
        "completion_tokens": usage.output,
        "total_tokens": usage.input + usage.output,
    })
}

/// Builds the Responses usage block from one sum.
fn responses_usage(usage: &UsageSum) -> Value {
    sonic_rs::json!({
        "input_tokens": usage.input,
        "output_tokens": usage.output,
        "total_tokens": usage.input + usage.output,
    })
}

/// Returns the Chat finish reason of a successful stop.
const fn chat_reason(stop: &HarnessStop) -> &'static str {
    match stop {
        HarnessStop::EndTurn | HarnessStop::Cancelled | HarnessStop::Failed(_) => "stop",
        HarnessStop::ToolUse => "tool_calls",
        HarnessStop::MaxTokens | HarnessStop::MaxTurnRequests => "length",
        HarnessStop::Refusal => "content_filter",
    }
}

/// Returns the Anthropic stop reason of a successful stop.
const fn messages_reason(stop: &HarnessStop) -> &'static str {
    match stop {
        HarnessStop::EndTurn | HarnessStop::Cancelled | HarnessStop::Failed(_) => "end_turn",
        HarnessStop::ToolUse => "tool_use",
        HarnessStop::MaxTokens | HarnessStop::MaxTurnRequests => "max_tokens",
        HarnessStop::Refusal => "refusal",
    }
}

/// Returns the Responses status of a stop.
const fn responses_status(stop: &HarnessStop) -> &'static str {
    match stop {
        HarnessStop::EndTurn | HarnessStop::ToolUse => "completed",
        HarnessStop::MaxTokens | HarnessStop::MaxTurnRequests => "incomplete",
        HarnessStop::Refusal | HarnessStop::Cancelled | HarnessStop::Failed(_) => "failed",
    }
}

/// Builds one non-stream chat completion object.
pub(crate) fn chat_object(ids: &StreamIds, turn: &HarnessTurn) -> Value {
    sonic_rs::json!({
        "id": ids.turn.chat,
        "object": "chat.completion",
        "created": 0,
        "model": ids.model,
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": turn.text},
            "finish_reason": chat_reason(&turn.stop),
        }],
        "usage": usage_block(&turn.usage),
    })
}

/// Builds one non-stream responses object.
pub(crate) fn responses_object(ids: &StreamIds, turn: &HarnessTurn) -> Value {
    let mut output = Vec::new();
    if !turn.reasoning.is_empty() {
        output.push(sonic_rs::json!({
            "type": "reasoning", "id": "reasoning_0",
            "summary": [{"type": "summary_text", "text": turn.reasoning}],
        }));
    }
    output.push(sonic_rs::json!({
        "type": "message", "id": "msg_0", "role": "assistant",
        "content": [{"type": "output_text", "text": turn.text}],
    }));
    sonic_rs::json!({
        "id": ids.turn.responses,
        "object": "response",
        "status": responses_status(&turn.stop),
        "model": ids.model,
        "output": output,
        "usage": responses_usage(&turn.usage),
    })
}

/// Builds one non-stream Anthropic message object.
pub(crate) fn message_object(ids: &StreamIds, turn: &HarnessTurn) -> Value {
    sonic_rs::json!({
        "id": ids.turn.messages,
        "type": "message",
        "role": "assistant",
        "model": ids.model,
        "content": [{"type": "text", "text": turn.text}],
        "stop_reason": messages_reason(&turn.stop),
        "usage": {"input_tokens": turn.usage.input, "output_tokens": turn.usage.output},
    })
}
