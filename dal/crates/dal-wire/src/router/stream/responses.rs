//! Responses SSE: `response.created`, `response.in_progress`, text and
//! reasoning deltas, function-call items, then `response.completed` or
//! `response.failed`, numbered by `sequence_number` from 1. Each output
//! item takes the next `output_index` when it first appears, and
//! `response.completed` lists the items in that order.

use sonic_rs::{JsonValueMutTrait, Value};

use super::super::harness::{HarnessEvent, HarnessStop, UsageSum};
use super::super::stream::TurnIds;
use super::{event_line, json, responses_status, responses_usage};

/// One function call seen on the stream.
struct Call {
    id: String,
    name: String,
    args: String,
    output: usize,
}

/// One output item, positioned by its `output_index`.
enum Item {
    /// The reasoning summary of the streamed reasoning text.
    Reasoning,
    /// A provider reasoning item replayed verbatim.
    Replay(Value),
    /// The assistant message.
    Message,
    /// The function call at this position of `calls`.
    Call(usize),
}

/// A Responses event encoder.
pub(crate) struct ResponsesEncoder {
    /// The response id, bound on the start event.
    id: String,
    /// The echoed model id.
    model: String,
    /// The next sequence number.
    sequence: u64,
    /// Encoded lines not yet sent.
    pub lines: Vec<String>,
    /// Accumulated reasoning text.
    reasoning: String,
    /// Accumulated message text.
    text: String,
    /// Output items in `output_index` order.
    items: Vec<Item>,
    /// Function calls in stream order.
    calls: Vec<Call>,
}

impl ResponsesEncoder {
    /// Creates one encoder; ids bind and initial events emit on start.
    pub(crate) fn new(model: &str) -> Self {
        Self {
            id: String::new(),
            model: model.to_owned(),
            sequence: 1,
            lines: Vec::new(),
            reasoning: String::new(),
            text: String::new(),
            items: Vec::new(),
            calls: Vec::new(),
        }
    }

    /// Feeds one turn event with the running usage totals.
    pub(crate) fn feed(&mut self, event: &HarnessEvent, usage: &UsageSum) {
        match event {
            HarnessEvent::Started(ids) => self.start(ids),
            HarnessEvent::Text(delta) => {
                self.text.push_str(delta);
                let output = self.slot(|item| matches!(item, Item::Message), Item::Message);
                self.push(
                    "response.output_text.delta",
                    sonic_rs::json!({
                        "item_id": "msg_0",
                        "output_index": output,
                        "content_index": 0,
                        "delta": delta,
                    }),
                );
            }
            HarnessEvent::Reasoning(delta) => {
                self.reasoning.push_str(delta);
                let output = self.slot(
                    |item| matches!(item, Item::Reasoning | Item::Replay(_)),
                    Item::Reasoning,
                );
                self.push(
                    "response.reasoning.delta",
                    sonic_rs::json!({
                        "item_id": "reasoning_0",
                        "output_index": output,
                        "content_index": 0,
                        "delta": delta,
                    }),
                );
            }
            HarnessEvent::ToolCallStarted { id, name } => self.call_started(id, name),
            HarnessEvent::ToolArgs { id, fragment } => self.call_args(id, fragment),
            HarnessEvent::ToolCallDone { id, args, .. } => self.call_done(id, args),
            HarnessEvent::Replay(item) => self.replay(item),
            HarnessEvent::Usage(_) => {}
            HarnessEvent::Stop(stop) => self.finish(stop, usage),
        }
    }

    /// Binds the response id and emits `created` plus `in_progress`.
    fn start(&mut self, ids: &TurnIds) {
        self.id.clone_from(&ids.responses);
        let response =
            sonic_rs::json!({"id": self.id, "object": "response", "status": "in_progress"});
        self.push(
            "response.created",
            sonic_rs::json!({"response": response.clone()}),
        );
        self.push(
            "response.in_progress",
            sonic_rs::json!({"response": response}),
        );
    }

    /// Pushes one typed event with its `type` and `sequence_number` first.
    fn push(&mut self, event: &str, mut members: Value) {
        let mut payload = sonic_rs::json!({"type": event, "sequence_number": self.sequence});
        self.sequence += 1;
        if let (Some(target), Some(source)) = (payload.as_object_mut(), members.as_object_mut()) {
            for (key, value) in source.iter_mut() {
                target.insert(key, std::mem::take(value));
            }
        }
        self.lines.push(event_line(event, &json(&payload)));
    }

    /// Returns the output index of the first item matching `find`, adding
    /// `item` at the next index when none does.
    fn slot(&mut self, find: impl Fn(&Item) -> bool, item: Item) -> usize {
        if let Some(index) = self.items.iter().position(find) {
            return index;
        }
        self.items.push(item);
        self.items.len() - 1
    }

    /// Records one replayed reasoning item. It takes the place of a streamed
    /// reasoning summary that no replay item has claimed yet.
    fn replay(&mut self, item: &Value) {
        if let Some(slot) = self
            .items
            .iter_mut()
            .find(|slot| matches!(slot, Item::Reasoning))
        {
            *slot = Item::Replay(item.clone());
            return;
        }
        self.items.push(Item::Replay(item.clone()));
    }

    /// Emits `response.output_item.added` for a new function call.
    fn call_started(&mut self, id: &str, name: &str) {
        let index = self.calls.len();
        let output = self.items.len();
        self.items.push(Item::Call(index));
        self.calls.push(Call {
            id: id.to_owned(),
            name: name.to_owned(),
            args: String::new(),
            output,
        });
        self.push(
            "response.output_item.added",
            sonic_rs::json!({"output_index": output, "item": self.call_item(index, "in_progress")}),
        );
    }

    /// Emits `response.function_call_arguments.delta` for one call.
    fn call_args(&mut self, id: &str, fragment: &str) {
        let Some(index) = self.calls.iter().position(|call| call.id == id) else {
            return;
        };
        self.calls[index].args.push_str(fragment);
        self.push(
            "response.function_call_arguments.delta",
            sonic_rs::json!({
                "item_id": format!("fc_{index}"),
                "output_index": self.calls[index].output,
                "delta": fragment,
            }),
        );
    }

    /// Emits the argument and item completion events for one call.
    fn call_done(&mut self, id: &str, args: &str) {
        let Some(index) = self.calls.iter().position(|call| call.id == id) else {
            return;
        };
        args.clone_into(&mut self.calls[index].args);
        self.push(
            "response.function_call_arguments.done",
            sonic_rs::json!({
                "item_id": format!("fc_{index}"),
                "output_index": self.calls[index].output,
                "arguments": args,
            }),
        );
        self.push(
            "response.output_item.done",
            sonic_rs::json!({"output_index": self.calls[index].output, "item": self.call_item(index, "completed")}),
        );
    }

    /// Builds the function-call item at one index.
    fn call_item(&self, index: usize, status: &str) -> Value {
        let call = &self.calls[index];
        sonic_rs::json!({
            "type": "function_call",
            "id": format!("fc_{index}"),
            "call_id": call.id,
            "name": call.name,
            "arguments": call.args,
            "status": status,
        })
    }

    /// Emits the terminal response event.
    fn finish(&mut self, stop: &HarnessStop, usage: &UsageSum) {
        match stop {
            HarnessStop::Refusal => self.failed("the model refused the request"),
            HarnessStop::Cancelled => self.failed("the turn was cancelled"),
            HarnessStop::Failed(message) => self.failed(message),
            _ => self.complete(responses_status(stop), usage),
        }
    }

    /// Emits `response.completed` with the full output.
    fn complete(&mut self, status: &str, usage: &UsageSum) {
        self.slot(|item| matches!(item, Item::Message), Item::Message);
        let output: Vec<Value> = std::mem::take(&mut self.items)
            .into_iter()
            .map(|item| match item {
                Item::Reasoning => sonic_rs::json!({
                    "type": "reasoning", "id": "reasoning_0",
                    "summary": [{"type": "summary_text", "text": self.reasoning}],
                }),
                Item::Replay(value) => value,
                Item::Message => sonic_rs::json!({
                    "type": "message", "id": "msg_0", "role": "assistant",
                    "content": [{"type": "output_text", "text": self.text}],
                }),
                Item::Call(index) => self.call_item(index, "completed"),
            })
            .collect();
        let response = sonic_rs::json!({
            "id": self.id,
            "object": "response",
            "status": status,
            "model": self.model,
            "output": output,
            "usage": responses_usage(usage),
        });
        self.push(
            "response.completed",
            sonic_rs::json!({"response": response}),
        );
    }

    /// Emits `response.failed`.
    fn failed(&mut self, message: &str) {
        let response = sonic_rs::json!({
            "id": self.id,
            "object": "response",
            "status": "failed",
            "error": {"message": message},
        });
        self.push("response.failed", sonic_rs::json!({"response": response}));
    }
}
