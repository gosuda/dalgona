//! Chat Completions SSE: role chunk first, content and tool-call deltas,
//! finish reason, optional usage chunk, then `[DONE]`.

use sonic_rs::Value;

use super::super::harness::{HarnessEvent, HarnessStop, UsageSum};
use super::{chat_reason, data_line, json, usage_block};

/// A Chat SSE encoder.
pub(crate) struct ChatEncoder {
    /// The completion id, bound on the start event.
    id: String,
    /// The echoed model id.
    model: String,
    /// Whether the usage chunk is enabled.
    include_usage: bool,
    /// Whether the role chunk was sent.
    role_sent: bool,
    /// Tool call ids in stream order; the position is the wire index.
    calls: Vec<String>,
    /// Encoded lines not yet sent.
    pub lines: Vec<String>,
}

impl ChatEncoder {
    /// Creates one encoder for a completion.
    pub(crate) fn new(model: &str, include_usage: bool) -> Self {
        Self {
            id: String::new(),
            model: model.to_owned(),
            include_usage,
            role_sent: false,
            calls: Vec::new(),
            lines: Vec::new(),
        }
    }

    /// Feeds one turn event with the running usage totals.
    pub(crate) fn feed(&mut self, event: &HarnessEvent, usage: &UsageSum) {
        match event {
            HarnessEvent::Started(ids) => self.id.clone_from(&ids.chat),
            HarnessEvent::Text(delta) => self.delta(&sonic_rs::json!({"content": delta})),
            HarnessEvent::ToolCallStarted { id, name } => {
                let index = self.calls.len();
                self.calls.push(id.clone());
                self.delta(&sonic_rs::json!({"tool_calls": [{
                    "index": index,
                    "id": id,
                    "type": "function",
                    "function": {"name": name, "arguments": ""},
                }]}));
            }
            HarnessEvent::ToolArgs { id, fragment } => {
                if let Some(index) = self.calls.iter().position(|call| call == id) {
                    self.delta(&sonic_rs::json!({"tool_calls": [{
                        "index": index,
                        "function": {"arguments": fragment},
                    }]}));
                }
            }
            HarnessEvent::Stop(stop) => self.finish(stop, usage),
            HarnessEvent::Reasoning(_)
            | HarnessEvent::ToolCallDone { .. }
            | HarnessEvent::Replay(_)
            | HarnessEvent::Usage(_) => {}
        }
    }

    /// Builds one chunk line.
    fn chunk(&self, choices: &Value) -> String {
        data_line(&json(&sonic_rs::json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": 0,
            "model": self.model,
            "choices": choices,
        })))
    }

    /// Emits the role chunk once, before any other chunk.
    fn ensure_role(&mut self) {
        if self.role_sent {
            return;
        }
        self.role_sent = true;
        let line = self.chunk(&sonic_rs::json!([
            {"index": 0, "delta": {"role": "assistant"}, "finish_reason": null},
        ]));
        self.lines.push(line);
    }

    /// Emits one delta chunk.
    fn delta(&mut self, delta: &Value) {
        self.ensure_role();
        let line = self.chunk(&sonic_rs::json!([
            {"index": 0, "delta": delta, "finish_reason": null},
        ]));
        self.lines.push(line);
    }

    /// Emits the terminal chunks for one stop.
    fn finish(&mut self, stop: &HarnessStop, usage: &UsageSum) {
        match stop {
            HarnessStop::Cancelled => self.stream_error("the turn was cancelled"),
            HarnessStop::Failed(message) => self.stream_error(message),
            _ => self.terminal(chat_reason(stop), usage),
        }
    }

    /// Emits a finish-reason chunk, the optional usage chunk, and `[DONE]`.
    fn terminal(&mut self, reason: &str, usage: &UsageSum) {
        self.ensure_role();
        let line = self.chunk(&sonic_rs::json!([
            {"index": 0, "delta": {}, "finish_reason": reason},
        ]));
        self.lines.push(line);
        if self.include_usage {
            self.lines.push(data_line(&json(&sonic_rs::json!({
                "id": self.id,
                "object": "chat.completion.chunk",
                "created": 0,
                "model": self.model,
                "choices": [],
                "usage": usage_block(usage),
            }))));
        }
        self.lines.push("data: [DONE]\n\n".to_owned());
    }

    /// Emits a stream failure event and `[DONE]`.
    fn stream_error(&mut self, message: &str) {
        self.lines.push(data_line(&json(&sonic_rs::json!({
            "error": {"message": message, "type": "server_error", "param": null, "code": null},
        }))));
        self.lines.push("data: [DONE]\n\n".to_owned());
    }
}
