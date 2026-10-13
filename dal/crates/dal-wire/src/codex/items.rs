//! Maps one turn's core updates to Codex item and turn notifications.
//!
//! Items map to entries: a user entry becomes a `userMessage` item and an
//! assistant entry's text and reasoning blocks become `agentMessage` and
//! `reasoning` items. A streamed agent message opens before its entry
//! exists, so it carries a turn-scoped id (`<turn>-<n>`) that the entry's
//! arrival completes.

use std::collections::HashMap;

use dal_core::{
    Block, EntryKind, EntryView, JournalPart, Request, RequestId, Stop, StreamChannel, TurnId,
    UpdateKind,
};
use sonic_rs::{JsonValueMutTrait, Value};

/// One wire action produced by a core update.
pub(crate) enum Event {
    /// A server notification with its method and params.
    Notify(&'static str, Value),
    /// A core request that needs a Codex server request.
    Ask(Box<Request>),
    /// A core request resolved; any outstanding client wait ends.
    Resolved(RequestId),
}

/// The streaming agent message that has not reached its entry yet.
struct OpenMessage {
    id: String,
    text: String,
}

/// Per-turn mapping state from core updates to Codex notifications.
pub(crate) struct TurnStream {
    thread: String,
    turn: TurnId,
    message: Option<OpenMessage>,
    messages: u32,
    calls: HashMap<String, (Box<str>, Value)>,
    done: bool,
}

impl TurnStream {
    /// Starts mapping one turn of one thread.
    pub(crate) fn new(thread: String, turn: TurnId) -> Self {
        Self {
            thread,
            turn,
            message: None,
            messages: 0,
            calls: HashMap::new(),
            done: false,
        }
    }

    /// Returns true once the turn's `turn/completed` was produced.
    pub(crate) fn is_done(&self) -> bool {
        self.done
    }

    /// Returns the thread id text.
    pub(crate) fn thread(&self) -> &str {
        &self.thread
    }

    /// Completes any open message and ends the turn with `status`.
    pub(crate) fn finish(&mut self, status: &str, error: Option<&str>) -> Vec<Event> {
        let mut out = Vec::new();
        if self.done {
            return out;
        }
        self.close_message(None, &mut out);
        out.push(Event::Notify(
            "turn/completed",
            sonic_rs::json!({
                "threadId": self.thread.as_str(),
                "turn": turn_value(&self.turn.to_string(), status, error, &[]),
            }),
        ));
        self.done = true;
        out
    }

    /// Maps one core update to its wire events, in emission order.
    pub(crate) fn apply(&mut self, kind: &UpdateKind) -> Vec<Event> {
        let mut out = Vec::new();
        if self.done {
            return out;
        }
        match kind {
            UpdateKind::Delta {
                turn,
                channel: StreamChannel::Text,
                text,
            } if *turn == self.turn => self.delta(text, &mut out),
            UpdateKind::Tree(delta) => {
                for entry in &delta.added {
                    self.entry(entry, &mut out);
                }
            }
            UpdateKind::ToolStarted { call, tool, args } => {
                let arguments = sonic_rs::from_str::<Value>(args.as_str())
                    .unwrap_or_else(|_| Value::from(args.as_str()));
                let item = tool_item(call.as_str(), tool, &arguments, "inProgress", None);
                self.calls
                    .insert(call.as_str().to_owned(), (tool.clone(), arguments));
                out.push(self.started(&item));
            }
            UpdateKind::ToolSettled { call, outcome } => {
                if let Some((tool, arguments)) = self.calls.remove(call.as_str()) {
                    let status = if outcome.is_error {
                        "failed"
                    } else {
                        "completed"
                    };
                    let result = Some((!outcome.is_error, outcome.text.as_ref()));
                    let item = tool_item(call.as_str(), &tool, &arguments, status, result);
                    out.push(self.completed(&item));
                }
            }
            UpdateKind::RequestOpened(request)
                if request.turn.is_none_or(|turn| turn == self.turn) =>
            {
                out.push(Event::Ask(Box::new(request.clone())));
            }
            UpdateKind::RequestResolved { id, .. } => out.push(Event::Resolved(*id)),
            UpdateKind::TurnEnded { turn, stop } if *turn == self.turn => {
                let (status, error) = stop_status(*stop);
                out.extend(self.finish(status, error));
            }
            _ => {}
        }
        out
    }

    /// Appends streamed text, opening the agent message item on first text.
    fn delta(&mut self, text: &str, out: &mut Vec<Event>) {
        if self.message.is_none() {
            self.messages += 1;
            let id = format!("{}-{}", self.turn, self.messages);
            out.push(self.started(&agent_message(&id, "")));
            self.message = Some(OpenMessage {
                id,
                text: String::new(),
            });
        }
        if let Some(message) = self.message.as_mut() {
            message.text.push_str(text);
            out.push(Event::Notify(
                "item/agentMessage/delta",
                sonic_rs::json!({
                    "threadId": self.thread.as_str(),
                    "turnId": self.turn.to_string(),
                    "itemId": message.id.as_str(),
                    "delta": text,
                }),
            ));
        }
    }

    /// Maps one appended entry to item notifications.
    fn entry(&mut self, entry: &EntryView, out: &mut Vec<Event>) {
        let id = entry.id.to_string();
        match &entry.kind {
            EntryKind::User { parts } => {
                let item = user_message(&id, parts);
                out.push(self.started(&item));
                out.push(self.completed(&item));
            }
            EntryKind::Assistant { content, .. } => {
                for (index, block) in content.iter().enumerate() {
                    if let Block::Reasoning { text, .. } = block {
                        let item = reasoning(&format!("{id}-{index}"), text);
                        out.push(self.started(&item));
                        out.push(self.completed(&item));
                    }
                }
                let text = assistant_text(content);
                if self.message.is_some() {
                    self.close_message(Some(text), out);
                } else if !text.is_empty() {
                    let item = agent_message(&id, &text);
                    out.push(self.started(&item));
                    out.push(self.completed(&item));
                }
            }
            _ => {}
        }
    }

    /// Completes the open streamed message with its final text.
    fn close_message(&mut self, text: Option<String>, out: &mut Vec<Event>) {
        if let Some(message) = self.message.take() {
            let text = text.filter(|text| !text.is_empty()).unwrap_or(message.text);
            out.push(self.completed(&agent_message(&message.id, &text)));
        }
    }

    /// Builds an `item/started` notification.
    fn started(&self, item: &Value) -> Event {
        Event::Notify(
            "item/started",
            sonic_rs::json!({
                "threadId": self.thread.as_str(),
                "turnId": self.turn.to_string(),
                "item": item,
                "startedAtMs": now_ms(),
            }),
        )
    }

    /// Builds an `item/completed` notification.
    fn completed(&self, item: &Value) -> Event {
        Event::Notify(
            "item/completed",
            sonic_rs::json!({
                "threadId": self.thread.as_str(),
                "turnId": self.turn.to_string(),
                "item": item,
                "completedAtMs": now_ms(),
            }),
        )
    }
}

/// Returns the current Unix time in milliseconds.
pub(crate) fn now_ms() -> i64 {
    dal_core::Timestamp::now().as_millisecond()
}

/// Maps a core stop reason to the Codex turn status and error text.
fn stop_status(stop: Stop) -> (&'static str, Option<&'static str>) {
    match stop {
        Stop::EndTurn | Stop::Length | Stop::Filter | Stop::MaxSteps => ("completed", None),
        Stop::Cancelled => ("interrupted", None),
        Stop::Failed => ("failed", Some("the turn failed")),
    }
}

/// Builds a Codex `Turn` value; `error` is set only for failed turns.
pub(crate) fn turn_value(id: &str, status: &str, error: Option<&str>, items: &[Value]) -> Value {
    let mut turn = sonic_rs::json!({"id": id, "items": items, "status": status});
    if let (Some(message), Some(object)) = (error, turn.as_object_mut()) {
        object.insert("error", sonic_rs::json!({"message": message}));
    }
    turn
}

/// Builds an `agentMessage` item.
pub(crate) fn agent_message(id: &str, text: &str) -> Value {
    sonic_rs::json!({"type": "agentMessage", "id": id, "text": text})
}

/// Builds a `reasoning` item carrying the reasoning text as content.
pub(crate) fn reasoning(id: &str, text: &str) -> Value {
    sonic_rs::json!({"type": "reasoning", "id": id, "summary": [], "content": [text]})
}

/// Builds a `userMessage` item from journal parts; stored blobs are omitted.
pub(crate) fn user_message(id: &str, parts: &[JournalPart]) -> Value {
    let content: Vec<Value> = parts
        .iter()
        .filter_map(|part| match part {
            JournalPart::Text { text } => {
                Some(sonic_rs::json!({"type": "text", "text": text.as_ref()}))
            }
            JournalPart::Image { mime, base64 } => Some(sonic_rs::json!({
                "type": "image",
                "url": format!("data:{mime};base64,{base64}"),
            })),
            _ => None,
        })
        .collect();
    sonic_rs::json!({"type": "userMessage", "id": id, "content": content})
}

/// Joins an assistant entry's visible text blocks.
pub(crate) fn assistant_text(content: &[Block]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            Block::Text { text } => Some(text.as_ref()),
            _ => None,
        })
        .collect()
}

/// Builds a `dynamicToolCall` item; `result` carries success and output text.
fn tool_item(
    id: &str,
    tool: &str,
    arguments: &Value,
    status: &str,
    result: Option<(bool, &str)>,
) -> Value {
    let mut item = sonic_rs::json!({
        "type": "dynamicToolCall",
        "id": id,
        "tool": tool,
        "arguments": arguments,
        "status": status,
    });
    if let (Some((success, text)), Some(object)) = (result, item.as_object_mut()) {
        object.insert("success", success);
        object.insert(
            "contentItems",
            sonic_rs::json!([{"type": "inputText", "text": text}]),
        );
    }
    item
}
