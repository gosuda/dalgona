//! Anthropic Messages SSE: `message_start`, then content blocks one at a
//! time (`content_block_start`, deltas, `content_block_stop`),
//! `message_delta`, and `message_stop`; failures end with an `error` event.
//!
//! Only one block is open at a time. A tool call that starts while another
//! tool block streams waits in a queue with its arguments buffered, and
//! opens when the streaming call completes.

use std::collections::VecDeque;

use sonic_rs::{JsonValueTrait, Value};

use super::super::harness::{HarnessEvent, HarnessStop, UsageSum};
use super::{event_line, json, messages_reason};

/// The kind of the open content block.
#[derive(PartialEq, Eq)]
enum Open {
    Text,
    Tool(String),
}

/// A tool call waiting for its block.
struct Queued {
    id: String,
    name: String,
    args: String,
    done: bool,
}

/// A Messages SSE encoder.
pub(crate) struct MessagesEncoder {
    /// The message id, bound on the start event.
    id: String,
    /// The echoed model id.
    model: String,
    /// Encoded lines not yet sent.
    pub lines: Vec<String>,
    /// The open block with its index.
    open: Option<(usize, Open)>,
    /// Tool calls waiting for their block, in start order.
    queued: VecDeque<Queued>,
    /// The next block index.
    next_block: usize,
}

impl MessagesEncoder {
    /// Creates one encoder; the id binds and `message_start` emits on start.
    pub(crate) fn new(model: &str) -> Self {
        Self {
            id: String::new(),
            model: model.to_owned(),
            lines: Vec::new(),
            open: None,
            queued: VecDeque::new(),
            next_block: 0,
        }
    }

    /// Feeds one turn event with the running usage totals.
    pub(crate) fn feed(&mut self, event: &HarnessEvent, usage: &UsageSum) {
        match event {
            HarnessEvent::Started(ids) => {
                self.id.clone_from(&ids.messages);
                self.push(&sonic_rs::json!({
                    "type": "message_start",
                    "message": {"id": self.id, "type": "message", "role": "assistant",
                                "model": self.model, "content": [], "stop_reason": null,
                                "usage": {"input_tokens": usage.input, "output_tokens": 0}},
                }));
            }
            HarnessEvent::Text(delta) => self.text(delta),
            HarnessEvent::ToolCallStarted { id, name } => self.tool_started(id, name),
            HarnessEvent::ToolArgs { id, fragment } => self.tool_args(id, fragment),
            HarnessEvent::ToolCallDone { id, args, .. } => self.tool_done(id, args),
            HarnessEvent::Replay(item) => {
                self.close();
                let index = self.start(item);
                self.stop(index);
            }
            HarnessEvent::Stop(stop) => self.finish(stop, usage),
            HarnessEvent::Reasoning(_) | HarnessEvent::Usage(_) => {}
        }
    }

    /// Streams text into the open text block, opening one when needed.
    fn text(&mut self, delta: &str) {
        let index = if let Some((index, Open::Text)) = &self.open {
            *index
        } else {
            self.close();
            let index = self.start(&sonic_rs::json!({"type": "text", "text": ""}));
            self.open = Some((index, Open::Text));
            index
        };
        self.delta(
            index,
            &sonic_rs::json!({"type": "text_delta", "text": delta}),
        );
    }

    /// Opens a tool block, or queues the call while another tool streams.
    fn tool_started(&mut self, id: &str, name: &str) {
        if matches!(self.open, Some((_, Open::Tool(_)))) {
            self.queued.push_back(Queued {
                id: id.to_owned(),
                name: name.to_owned(),
                args: String::new(),
                done: false,
            });
            return;
        }
        self.close();
        self.open_tool(id, name);
    }

    /// Streams arguments of the open tool, or buffers them for a queued one.
    fn tool_args(&mut self, id: &str, fragment: &str) {
        if let Some((index, Open::Tool(open))) = &self.open
            && open == id
        {
            let index = *index;
            self.delta(
                index,
                &sonic_rs::json!({"type": "input_json_delta", "partial_json": fragment}),
            );
            return;
        }
        if let Some(queued) = self.queued.iter_mut().find(|queued| queued.id == id) {
            queued.args.push_str(fragment);
        }
    }

    /// Closes a completed open tool and opens the queued calls in order.
    fn tool_done(&mut self, id: &str, args: &str) {
        if let Some(queued) = self.queued.iter_mut().find(|queued| queued.id == id) {
            args.clone_into(&mut queued.args);
            queued.done = true;
            return;
        }
        if matches!(&self.open, Some((_, Open::Tool(open))) if open == id) {
            self.close();
            self.drain_done();
        }
    }

    /// Opens queued calls in order; a call not yet done stays open.
    fn drain_done(&mut self) {
        while let Some(queued) = self.queued.pop_front() {
            let done = queued.done;
            self.open_queued(&queued);
            if !done {
                return;
            }
            self.close();
        }
    }

    /// Opens and closes every queued call in order.
    fn flush_queue(&mut self) {
        while let Some(queued) = self.queued.pop_front() {
            self.open_queued(&queued);
            self.close();
        }
    }

    /// Opens one queued call's block with its buffered arguments.
    fn open_queued(&mut self, queued: &Queued) {
        let index = self.open_tool(&queued.id, &queued.name);
        if !queued.args.is_empty() {
            self.delta(
                index,
                &sonic_rs::json!({"type": "input_json_delta", "partial_json": queued.args}),
            );
        }
    }

    /// Starts one tool block and marks it open.
    fn open_tool(&mut self, id: &str, name: &str) -> usize {
        let index =
            self.start(&sonic_rs::json!({"type": "tool_use", "id": id, "name": name, "input": {}}));
        self.open = Some((index, Open::Tool(id.to_owned())));
        index
    }

    /// Pushes one event line named by its `type`.
    fn push(&mut self, payload: &Value) {
        let name = payload
            .get("type")
            .and_then(JsonValueTrait::as_str)
            .unwrap_or("");
        self.lines.push(event_line(name, &json(payload)));
    }

    /// Starts one content block and returns its index.
    fn start(&mut self, block: &Value) -> usize {
        let index = self.next_block;
        self.next_block += 1;
        self.push(&sonic_rs::json!({
            "type": "content_block_start",
            "index": index,
            "content_block": block,
        }));
        index
    }

    /// Emits one delta into the open block.
    fn delta(&mut self, index: usize, delta: &Value) {
        self.push(&sonic_rs::json!({
            "type": "content_block_delta",
            "index": index,
            "delta": delta,
        }));
    }

    /// Stops one block.
    fn stop(&mut self, index: usize) {
        self.push(&sonic_rs::json!({"type": "content_block_stop", "index": index}));
    }

    /// Stops the open block, when there is one.
    fn close(&mut self) {
        if let Some((index, _)) = self.open.take() {
            self.stop(index);
        }
    }

    /// Emits the terminal events for one stop.
    fn finish(&mut self, stop: &HarnessStop, usage: &UsageSum) {
        let message = match stop {
            HarnessStop::Cancelled => "the turn was cancelled",
            HarnessStop::Failed(message) => message.as_str(),
            _ => return self.complete(messages_reason(stop), usage),
        };
        self.push(&sonic_rs::json!({
            "type": "error",
            "error": {"type": "api_error", "message": message},
        }));
    }

    /// Stops the open block, flushes queued calls, then emits
    /// `message_delta` and `message_stop`.
    fn complete(&mut self, reason: &str, usage: &UsageSum) {
        self.close();
        self.flush_queue();
        self.push(&sonic_rs::json!({
            "type": "message_delta",
            "delta": {"stop_reason": reason, "stop_sequence": null},
            "usage": {"output_tokens": usage.output},
        }));
        self.push(&sonic_rs::json!({"type": "message_stop"}));
    }
}
