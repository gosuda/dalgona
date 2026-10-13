//! Non-stream family objects for relayed pass-through turns.

use sonic_rs::Value;

use super::super::pass::RelayTurn;
use super::{StreamIds, chat_reason, messages_reason, responses_status};

/// Builds one non-stream chat completion object for a relayed turn.
pub(crate) fn relay_chat_object(ids: &StreamIds, turn: &RelayTurn) -> Value {
    let calls: Vec<Value> = turn
        .calls
        .iter()
        .map(|call| {
            sonic_rs::json!({
                "id": call.call,
                "type": "function",
                "function": {"name": call.name, "arguments": call.args},
            })
        })
        .collect();
    sonic_rs::json!({
        "id": ids.turn.chat,
        "object": "chat.completion",
        "created": 0,
        "model": ids.model,
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": turn.text, "tool_calls": calls},
            "finish_reason": chat_reason(&turn.stop),
        }],
        "usage": {
            "prompt_tokens": turn.input_tokens,
            "completion_tokens": turn.output_tokens,
            "total_tokens": turn.input_tokens + turn.output_tokens,
        },
    })
}

/// Builds one non-stream responses object for a relayed turn.
pub(crate) fn relay_responses_object(ids: &StreamIds, turn: &RelayTurn) -> Value {
    let mut output = turn.replay.clone();
    output.push(sonic_rs::json!({
        "type": "message", "id": "msg_0", "role": "assistant",
        "content": [{"type": "output_text", "text": turn.text}],
    }));
    for (index, call) in turn.calls.iter().enumerate() {
        output.push(sonic_rs::json!({
            "type": "function_call",
            "id": format!("fc_{index}"),
            "call_id": call.call,
            "name": call.name,
            "arguments": call.args,
            "status": "completed",
        }));
    }
    sonic_rs::json!({
        "id": ids.turn.responses,
        "object": "response",
        "status": responses_status(&turn.stop),
        "model": ids.model,
        "output": output,
        "usage": {
            "input_tokens": turn.input_tokens,
            "output_tokens": turn.output_tokens,
            "total_tokens": turn.input_tokens + turn.output_tokens,
        },
    })
}

/// Builds one non-stream Anthropic message for a relayed turn.
pub(crate) fn relay_message_object(ids: &StreamIds, turn: &RelayTurn) -> Value {
    let mut content = turn.replay.clone();
    content.push(sonic_rs::json!({"type": "text", "text": turn.text}));
    for call in &turn.calls {
        content.push(sonic_rs::json!({
            "type": "tool_use", "id": call.call, "name": call.name,
            "input": sonic_rs::from_str::<Value>(&call.args).unwrap_or_else(|_| sonic_rs::json!({})),
        }));
    }
    sonic_rs::json!({
        "id": ids.turn.messages,
        "type": "message",
        "role": "assistant",
        "model": ids.model,
        "content": content,
        "stop_reason": messages_reason(&turn.stop),
        "usage": {"input_tokens": turn.input_tokens, "output_tokens": turn.output_tokens},
    })
}
