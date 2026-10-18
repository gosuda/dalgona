//! The pi-messages gateway family: request lowering and SSE event decoding.
//!
//! [`request_body`] lowers one [`ModelRequest`] and its composed
//! [`ThinkingPlan`] to `POST <base>/messages` with JSON `{model, context, options}`.
//! System instructions and tool declarations travel in a leading system message;
//! user, assistant, and tool-result history items are lowered into senpi transcript
//! message records. Sampling temperature, output token ceiling, reasoning effort,
//! and session cache key ride under `options`.
//!
//! [`PiMessagesDecoder`] turns the data of each SSE event into neutral
//! [`StreamEvent`]s with the grammar of [`crate::stream`]: deltas as they
//! arrive, then one `ToolCallsDone`, one `Usage`, and one `Stop` at the
//! `done` event, or one typed error instead. Dispatch reads the JSON `type`
//! member; the SSE event name is ignored. The `done` event carries typed reasons
//! (`stop` -> `EndTurn`, `length` -> `MaxTokens`, `toolUse` -> `ToolUse`) plus
//! token usage. The `error` event carries in-stream errors mapped to
//! [`ProviderError::Status`]. Dropping the stream cancels the underlying transfer,
//! and input ending before a terminal event gives [`ProviderError::StreamCut`].

use std::{
    borrow::Cow,
    collections::{BTreeMap, VecDeque},
    mem,
};

use base64::{Engine, engine::general_purpose::STANDARD};
use dal_core::{
    AssistantPart, BlobId, ContextItem, Family, ModelRequest, ModelRoute, Part, RawJson, Usage,
};
use futures::{Stream, StreamExt, stream};
use serde::{Deserialize, Serialize};
use sonic_rs::JsonValueTrait;

use crate::{
    error::ProviderError,
    retry::{RequestState, RetryDecision, classify},
    sse::SseEvent,
    stream::{ReplayPayload, StopReason, StreamEvent, ToolArgs, ToolCall},
    stream_budget::StreamBudget,
    thinking::{ThinkingPlan, WireThinking},
    tool_names::{WIRE_NAME_MAX, wire_name},
};

/// The endpoint path of the family, relative to the provider base URL.
pub(crate) const ENDPOINT_PATH: &str = "messages";

/// The model discovery endpoint path on the Radius gateway, relative to its root.
pub(crate) const CONFIG_PATH: &str = "v1/config";

/// The HTTP status carried by an in-stream pi-messages error.
const IN_STREAM_STATUS: u16 = 200;

const EMPTY_USAGE: EmptyUsage = EmptyUsage {
    input: 0,
    output: 0,
    cache_read: 0,
    cache_write: 0,
    total_tokens: 0,
    cost: EmptyCost {
        input: 0.0,
        output: 0.0,
        cache_read: 0.0,
        cache_write: 0.0,
        total: 0.0,
    },
};

/// A request that cannot be lowered to a pi-messages body.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum PiMessagesBodyError {
    /// The request route is not a `pi-messages` model route.
    #[error("a pi-messages request needs a pi-messages model route, not `{route}`")]
    NotPiMessages {
        /// The route id of the request.
        route: Box<str>,
    },
    /// A content part still refers to stored content; the caller resolves
    /// blobs to inline bytes before lowering.
    #[error(
        "content blob {blob_id} must be read into the request before a pi-messages body is built"
    )]
    UnresolvedBlob {
        /// The unresolved blob.
        blob_id: BlobId,
    },
    /// The thinking plan was composed for the Anthropic family.
    #[error("the thinking plan carries an Anthropic fragment, not an OpenAI reasoning effort")]
    ForeignThinking,
    /// The JSON writer failed.
    #[error("pi-messages body encoding failed: {message}")]
    Encode {
        /// The writer message.
        message: String,
    },
}

#[derive(Serialize)]
struct Body<'a> {
    model: &'a str,
    context: ContextWrapper<'a>,
    options: Options<'a>,
}

#[derive(Serialize)]
struct ContextWrapper<'a> {
    messages: Vec<WireMessage<'a>>,
}

#[derive(Serialize)]
#[serde(tag = "role")]
enum WireMessage<'a> {
    #[serde(rename = "system")]
    System {
        content: &'a str,
        #[serde(skip_serializing_if = "Vec::is_empty", rename = "toolsAdded")]
        tools_added: Vec<WireTool<'a>>,
        timestamp: u64,
    },
    #[serde(rename = "user")]
    User {
        content: Vec<UserPart<'a>>,
        timestamp: u64,
    },
    #[serde(rename = "assistant")]
    Assistant {
        content: Vec<AssistantContent<'a>>,
        api: &'a str,
        provider: &'a str,
        model: &'a str,
        usage: EmptyUsage,
        #[serde(rename = "stopReason")]
        stop_reason: &'static str,
        timestamp: u64,
    },
    #[serde(rename = "toolResult")]
    ToolResult {
        #[serde(rename = "toolCallId")]
        tool_call_id: &'a str,
        #[serde(rename = "toolName")]
        tool_name: &'a str,
        content: Vec<UserPart<'a>>,
        #[serde(rename = "isError")]
        is_error: bool,
        timestamp: u64,
    },
}

#[derive(Clone, Copy, Serialize)]
struct EmptyUsage {
    input: u64,
    output: u64,
    #[serde(rename = "cacheRead")]
    cache_read: u64,
    #[serde(rename = "cacheWrite")]
    cache_write: u64,
    #[serde(rename = "totalTokens")]
    total_tokens: u64,
    cost: EmptyCost,
}

#[derive(Clone, Copy, Serialize)]
struct EmptyCost {
    input: f64,
    output: f64,
    #[serde(rename = "cacheRead")]
    cache_read: f64,
    #[serde(rename = "cacheWrite")]
    cache_write: f64,
    total: f64,
}

#[derive(Serialize)]
struct WireTool<'a> {
    name: Cow<'a, str>,
    description: &'a str,
    parameters: &'a RawJson,
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum UserPart<'a> {
    #[serde(rename = "text")]
    Text { text: Cow<'a, str> },
    #[serde(rename = "image")]
    Image {
        #[serde(rename = "mimeType")]
        mime_type: &'a str,
        data: String,
    },
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum AssistantContent<'a> {
    #[serde(rename = "text")]
    Text { text: &'a str },
    #[serde(rename = "thinking")]
    Thinking {
        thinking: &'a str,
        #[serde(skip_serializing_if = "Option::is_none", rename = "thinkingSignature")]
        thinking_signature: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        redacted: Option<bool>,
    },
    #[serde(rename = "toolCall")]
    ToolCall {
        id: &'a str,
        name: Cow<'a, str>,
        arguments: &'a RawJson,
    },
}

#[derive(Serialize)]
struct Options<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "maxTokens")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "sessionId")]
    session_id: Option<&'a str>,
}

/// Lowers `request` to the JSON body of `POST <base>/messages`.
pub(crate) fn request_body(
    request: &ModelRequest,
    provider: &str,
    thinking: &ThinkingPlan,
) -> Result<Vec<u8>, PiMessagesBodyError> {
    let ModelRoute::Api {
        family: Family::PiMessages,
        model,
    } = &request.model
    else {
        return Err(PiMessagesBodyError::NotPiMessages {
            route: request.model.id().into(),
        });
    };
    let WireThinking::OpenAi { effort } = thinking.wire else {
        return Err(PiMessagesBodyError::ForeignThinking);
    };
    let reasoning = effort.and_then(|e| match e {
        "none" => None,
        other => Some(other),
    });
    let body = Body {
        model,
        context: ContextWrapper {
            messages: messages(request, provider, model)?,
        },
        options: Options {
            temperature: thinking.temperature,
            max_tokens: request.params.max_output_tokens,
            reasoning,
            session_id: request.cache_key.as_deref(),
        },
    };
    sonic_rs::to_vec(&body).map_err(|error| PiMessagesBodyError::Encode {
        message: error.to_string(),
    })
}

fn messages<'a>(
    request: &'a ModelRequest,
    provider: &'a str,
    target_model: &str,
) -> Result<Vec<WireMessage<'a>>, PiMessagesBodyError> {
    let mut messages = Vec::with_capacity(request.context.len() + 1);
    let has_tools = !request.tools.is_empty();
    if !request.system.is_empty() || has_tools {
        let tools_added = request
            .tools
            .iter()
            .map(|tool| WireTool {
                name: wire_name(&tool.name, WIRE_NAME_MAX),
                description: &tool.description,
                parameters: &tool.parameters,
            })
            .collect();
        messages.push(WireMessage::System {
            content: &request.system,
            tools_added,
            timestamp: 0,
        });
    }
    for item in request.context.iter() {
        match item {
            ContextItem::User { parts } => {
                let user_parts = parts.iter().map(user_part).collect::<Result<Vec<_>, _>>()?;
                messages.push(WireMessage::User {
                    content: user_parts,
                    timestamp: 0,
                });
            }
            ContextItem::Assistant { source, parts, .. } => {
                if let Some(msg) = assistant_message(source, parts, provider, target_model) {
                    messages.push(msg);
                }
            }
            ContextItem::ToolResult {
                call,
                name,
                is_error,
                parts,
            } => {
                let tool_parts = parts.iter().map(user_part).collect::<Result<Vec<_>, _>>()?;
                messages.push(WireMessage::ToolResult {
                    tool_call_id: call.as_str(),
                    tool_name: name,
                    content: tool_parts,
                    is_error: *is_error,
                    timestamp: 0,
                });
            }
        }
    }
    Ok(messages)
}

fn user_part(part: &Part) -> Result<UserPart<'_>, PiMessagesBodyError> {
    match part {
        Part::Text { text } => Ok(UserPart::Text {
            text: Cow::Borrowed(text),
        }),
        Part::Image { mime, bytes } => Ok(UserPart::Image {
            mime_type: mime,
            data: STANDARD.encode(bytes),
        }),
        Part::Blob { blob_id, .. } => {
            Err(PiMessagesBodyError::UnresolvedBlob { blob_id: *blob_id })
        }
    }
}

#[derive(Deserialize)]
struct ThinkingReplay<'a> {
    #[serde(borrow, rename = "thinkingSignature")]
    thinking_signature: Option<&'a str>,
    redacted: Option<bool>,
}

fn decode_thinking_replay<'a>(replay: Option<&'a RawJson>) -> (Option<&'a str>, Option<bool>) {
    let Some(replay) = replay else {
        return (None, None);
    };
    let Ok(parsed) = sonic_rs::from_str::<ThinkingReplay<'a>>(replay.as_str()) else {
        return (None, None);
    };
    (parsed.thinking_signature, parsed.redacted)
}

fn assistant_message<'a>(
    source: &'a dal_core::ReplaySource,
    parts: &'a [AssistantPart],
    provider: &'a str,
    target_model: &str,
) -> Option<WireMessage<'a>> {
    let same_route = source.family == Family::PiMessages && source.model.as_ref() == target_model;
    let mut content = Vec::new();
    let mut has_tool_calls = false;
    for part in parts {
        match part {
            AssistantPart::Text { text, .. } => {
                if !text.is_empty() {
                    content.push(AssistantContent::Text { text });
                }
            }
            AssistantPart::Thinking { text, replay } => {
                let (thinking_signature, redacted) = if same_route {
                    decode_thinking_replay(replay.as_ref())
                } else {
                    (None, None)
                };
                if !text.is_empty() || redacted == Some(true) {
                    content.push(AssistantContent::Thinking {
                        thinking: text,
                        thinking_signature,
                        redacted,
                    });
                }
            }
            AssistantPart::ToolCall {
                call, name, args, ..
            } => {
                has_tool_calls = true;
                content.push(AssistantContent::ToolCall {
                    id: call.as_str(),
                    name: wire_name(name, WIRE_NAME_MAX),
                    arguments: args,
                });
            }
        }
    }
    if content.is_empty() {
        return None;
    }
    let (api, prov) = if source.family == Family::PiMessages {
        ("pi-messages", provider)
    } else {
        (crate::error::family_label(source.family), "external")
    };
    Some(WireMessage::Assistant {
        content,
        api,
        provider: prov,
        model: &source.model,
        usage: EMPTY_USAGE,
        stop_reason: if has_tool_calls { "toolUse" } else { "stop" },
        timestamp: 0,
    })
}

/// Whether the decoder can take more input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Progress {
    /// The response is still open.
    Open,
    /// The terminal was emitted, or an error was returned; later input is ignored.
    Ended,
}

#[derive(Debug)]
struct OpenCall {
    id: String,
    name: String,
    args: Vec<u8>,
    authoritative_args: Option<Vec<u8>>,
    incomplete: bool,
}

/// The incremental decoder of one pi-messages SSE stream.
#[derive(Debug)]
pub(crate) struct PiMessagesDecoder {
    provider: Box<str>,
    model: Box<str>,
    budget: StreamBudget,
    calls: BTreeMap<usize, OpenCall>,
    seen_text: std::collections::HashSet<usize>,
    seen_thinking: std::collections::HashSet<usize>,
    emitted: bool,
    ended: bool,
}

impl PiMessagesDecoder {
    /// Creates a decoder for one stream response.
    #[must_use]
    pub(crate) fn new(provider: &str, model: &str) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            budget: StreamBudget::default(),
            calls: BTreeMap::new(),
            seen_text: std::collections::HashSet::new(),
            seen_thinking: std::collections::HashSet::new(),
            emitted: false,
            ended: false,
        }
    }

    /// Feeds one SSE event data line into the decoder.
    pub(crate) fn feed(
        &mut self,
        data: &str,
        out: &mut Vec<StreamEvent>,
    ) -> Result<Progress, ProviderError> {
        if self.ended {
            return Ok(Progress::Ended);
        }
        let trimmed = data.trim();
        if trimmed.is_empty() || trimmed == "[DONE]" {
            return Ok(Progress::Open);
        }
        let start = out.len();
        let result = self
            .budget
            .admit_event(data.len())
            .and_then(|()| self.step(data, out));
        self.emitted |= out.len() > start;
        if !matches!(result, Ok(Progress::Open)) {
            self.ended = true;
        }
        result
    }

    /// Ends input: a response without a terminal event is a cut.
    pub(crate) fn end(&mut self, _out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        if mem::replace(&mut self.ended, true) {
            return Ok(());
        }
        Err(ProviderError::StreamCut)
    }

    fn step(&mut self, data: &str, out: &mut Vec<StreamEvent>) -> Result<Progress, ProviderError> {
        let kind = match sonic_rs::get_from_str(data, ["type"]) {
            Ok(kind) => kind,
            Err(error) if error.is_not_found() => return Ok(Progress::Open),
            Err(error) => {
                return Err(protocol(format!(
                    "event data is not a JSON object: {error}"
                )));
            }
        };
        match kind.as_str() {
            Some("start" | "text_start" | "thinking_start") => {}
            Some("text_delta") => {
                #[derive(Deserialize)]
                struct TextDelta {
                    #[serde(rename = "contentIndex")]
                    content_index: Option<usize>,
                    delta: Option<String>,
                }
                let event: TextDelta = parse(data, "text_delta")?;
                if let Some(index) = event.content_index {
                    self.seen_text.insert(index);
                }
                if let Some(delta) = event.delta.filter(|s| !s.is_empty()) {
                    out.push(StreamEvent::TextDelta { text: delta });
                }
            }
            Some("text_end") => {
                #[derive(Deserialize)]
                struct TextEnd {
                    #[serde(rename = "contentIndex")]
                    content_index: usize,
                    content: Option<String>,
                }
                let event: TextEnd = parse(data, "text_end")?;
                if !self.seen_text.contains(&event.content_index)
                    && let Some(content) = event.content.filter(|s| !s.is_empty())
                {
                    out.push(StreamEvent::TextDelta { text: content });
                }
            }
            Some("thinking_delta") => {
                #[derive(Deserialize)]
                struct ThinkingDelta {
                    #[serde(rename = "contentIndex")]
                    content_index: Option<usize>,
                    delta: Option<String>,
                }
                let event: ThinkingDelta = parse(data, "thinking_delta")?;
                if let Some(index) = event.content_index {
                    self.seen_thinking.insert(index);
                }
                if let Some(delta) = event.delta.filter(|s| !s.is_empty()) {
                    out.push(StreamEvent::ReasoningDelta { text: delta });
                }
            }
            Some("thinking_end") => {
                #[derive(Deserialize)]
                struct ThinkingEnd {
                    #[serde(rename = "contentIndex")]
                    content_index: usize,
                    content: Option<String>,
                    #[serde(rename = "contentSignature")]
                    content_signature: Option<String>,
                    redacted: Option<bool>,
                }
                let event: ThinkingEnd = parse(data, "thinking_end")?;
                if !self.seen_thinking.contains(&event.content_index)
                    && let Some(content) = event.content.filter(|s| !s.is_empty())
                {
                    out.push(StreamEvent::ReasoningDelta { text: content });
                }
                if event.content_signature.is_some() || event.redacted.is_some() {
                    #[derive(Serialize)]
                    struct ReplayData<'a> {
                        #[serde(
                            skip_serializing_if = "Option::is_none",
                            rename = "thinkingSignature"
                        )]
                        thinking_signature: Option<&'a str>,
                        #[serde(skip_serializing_if = "Option::is_none")]
                        redacted: Option<bool>,
                    }
                    let payload_struct = ReplayData {
                        thinking_signature: event.content_signature.as_deref(),
                        redacted: event.redacted,
                    };
                    if let Ok(json_str) = sonic_rs::to_string(&payload_struct)
                        && let Ok(raw_json) = RawJson::parse(&json_str)
                    {
                        out.push(StreamEvent::Replay {
                            payload: ReplayPayload {
                                family: Family::PiMessages,
                                model: self.model.clone(),
                                item: raw_json,
                            },
                        });
                    }
                }
            }
            Some("toolcall_start") => {
                #[derive(Deserialize)]
                struct ToolcallStart {
                    #[serde(rename = "contentIndex")]
                    content_index: usize,
                    id: Option<String>,
                    #[serde(rename = "toolName")]
                    tool_name: Option<String>,
                }
                let event: ToolcallStart = parse(data, "toolcall_start")?;
                self.budget.admit_item()?;
                let (Some(id), Some(name)) = (
                    event.id.filter(|s| !s.is_empty()),
                    event.tool_name.filter(|s| !s.is_empty()),
                ) else {
                    return Err(protocol(format!(
                        "toolcall_start contentIndex {} missing id or toolName",
                        event.content_index
                    )));
                };
                out.push(StreamEvent::ToolCallStarted {
                    id: id.clone(),
                    name: name.clone(),
                });
                self.calls.insert(
                    event.content_index,
                    OpenCall {
                        id,
                        name,
                        args: Vec::new(),
                        authoritative_args: None,
                        incomplete: false,
                    },
                );
            }
            Some("toolcall_delta") => {
                #[derive(Deserialize)]
                struct ToolcallDelta {
                    #[serde(rename = "contentIndex")]
                    content_index: usize,
                    delta: Option<String>,
                }
                let event: ToolcallDelta = parse(data, "toolcall_delta")?;
                let Some(call) = self.calls.get_mut(&event.content_index) else {
                    return Err(protocol(format!(
                        "toolcall_delta for unknown contentIndex {}",
                        event.content_index
                    )));
                };
                if let Some(fragment) = event.delta.filter(|s| !s.is_empty()) {
                    call.args.extend_from_slice(fragment.as_bytes());
                    out.push(StreamEvent::ToolArgsDelta {
                        id: call.id.clone(),
                        fragment: fragment.into_bytes(),
                    });
                }
            }
            Some("toolcall_end") => {
                #[derive(Deserialize)]
                struct ToolcallEnd {
                    #[serde(rename = "contentIndex")]
                    content_index: usize,
                    #[serde(rename = "toolCall")]
                    tool_call: Option<WireToolCall>,
                }
                #[derive(Deserialize)]
                struct WireToolCall {
                    id: Option<String>,
                    name: Option<String>,
                    arguments: Option<RawJson>,
                    incomplete: Option<bool>,
                }
                let event: ToolcallEnd = parse(data, "toolcall_end")?;
                if let Some(call) = self.calls.get_mut(&event.content_index)
                    && let Some(tc) = event.tool_call
                {
                    if let Some(id) = tc.id.filter(|s| !s.is_empty()) {
                        call.id = id;
                    }
                    if let Some(name) = tc.name.filter(|s| !s.is_empty()) {
                        call.name = name;
                    }
                    if tc.incomplete == Some(true) {
                        call.incomplete = true;
                    }
                    if let Some(args_val) = tc.arguments {
                        call.authoritative_args = Some(args_val.as_str().as_bytes().to_vec());
                    }
                }
            }
            Some("done") => {
                #[derive(Deserialize)]
                struct DoneEvent {
                    reason: Option<String>,
                    usage: Option<WireUsage>,
                }
                let event: DoneEvent = parse(data, "done")?;
                self.finish(event.reason.as_deref(), event.usage.as_ref(), out);
                return Ok(Progress::Ended);
            }
            Some("error") => {
                #[derive(Deserialize)]
                struct ErrorEvent {
                    reason: Option<String>,
                    #[serde(rename = "errorMessage")]
                    error_message: Option<String>,
                }
                let event: ErrorEvent = parse(data, "error")?;
                let message =
                    event
                        .error_message
                        .unwrap_or_else(|| match event.reason.as_deref() {
                            Some("aborted") => String::from("pi-messages stream was aborted"),
                            _ => String::from("pi-messages stream returned an error"),
                        });
                return Err(self.server_error(&message));
            }
            _ => {}
        }
        Ok(Progress::Open)
    }

    fn finish(
        &mut self,
        reason_str: Option<&str>,
        usage_wire: Option<&WireUsage>,
        out: &mut Vec<StreamEvent>,
    ) {
        let has_calls = !self.calls.is_empty();
        let reason = match reason_str {
            Some("length") => StopReason::MaxTokens,
            Some("toolUse") => StopReason::ToolUse,
            Some("stop") => {
                if has_calls {
                    StopReason::ToolUse
                } else {
                    StopReason::EndTurn
                }
            }
            Some(other) => StopReason::Other(other.to_owned()),
            None => {
                if has_calls {
                    StopReason::ToolUse
                } else {
                    StopReason::EndTurn
                }
            }
        };
        let truncated = reason == StopReason::MaxTokens;
        let calls = mem::take(&mut self.calls)
            .into_values()
            .map(|call| {
                let args = if truncated || call.incomplete {
                    ToolArgs::Truncated
                } else if let Some(bytes) = call.authoritative_args {
                    validate_tool_args(&bytes)
                } else if call.args.is_empty() {
                    ToolArgs::from_bytes(b"{}")
                } else {
                    validate_tool_args(&call.args)
                };
                ToolCall {
                    id: call.id,
                    name: call.name,
                    args,
                }
            })
            .collect();
        out.push(StreamEvent::ToolCallsDone { calls });
        out.push(StreamEvent::Usage {
            usage: normalize_usage(usage_wire),
        });
        out.push(StreamEvent::Stop { reason });
        self.ended = true;
    }

    fn server_error(&self, message: &str) -> ProviderError {
        let request = RequestState {
            family: Family::PiMessages,
            provider: &self.provider,
            model: &self.model,
            oauth: false,
            refreshed: false,
            delivered: self.emitted,
        };
        match classify(Some(IN_STREAM_STATUS), None, message, &request) {
            RetryDecision::Retry(error) | RetryDecision::Fail(error) => error,
            RetryDecision::RefreshOnce => ProviderError::SignInExpired {
                provider: String::from(request.provider),
            },
        }
    }
}

fn validate_tool_args(bytes: &[u8]) -> ToolArgs {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return ToolArgs::Invalid {
            message: "arguments are not valid UTF-8".to_string(),
        };
    };
    let trimmed = text.trim();
    if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
        return ToolArgs::Invalid {
            message: "arguments must be a JSON object".to_string(),
        };
    }
    ToolArgs::from_bytes(bytes)
}

#[derive(Deserialize)]
struct WireUsage {
    input: Option<u64>,
    output: Option<u64>,
    #[serde(rename = "cacheRead")]
    cache_read: Option<u64>,
    #[serde(rename = "cacheWrite")]
    cache_write: Option<u64>,
    reasoning: Option<u64>,
    cost: Option<WireCost>,
}

#[derive(Deserialize)]
struct WireCost {
    total: Option<f64>,
}

fn normalize_usage(wire: Option<&WireUsage>) -> Usage {
    let Some(wire) = wire else {
        return Usage {
            input_tokens: 0,
            cached_input_tokens: 0,
            output_tokens: 0,
            reasoning_tokens: None,
            cache_write_tokens: 0,
            premium_requests: None,
            cost_usd: None,
        };
    };
    let input = wire.input.unwrap_or(0);
    let cache_read = wire.cache_read.unwrap_or(0);
    let cache_write = wire.cache_write.unwrap_or(0);
    let output = wire.output.unwrap_or(0);
    let reasoning = wire.reasoning;
    let cost_usd = wire
        .cost
        .as_ref()
        .and_then(|c| c.total)
        .filter(|t| t.is_finite() && *t > 0.0);

    Usage {
        input_tokens: input.saturating_add(cache_read).saturating_add(cache_write),
        cached_input_tokens: cache_read,
        output_tokens: output.saturating_sub(reasoning.unwrap_or(0)),
        reasoning_tokens: reasoning,
        cache_write_tokens: cache_write,
        premium_requests: None,
        cost_usd,
    }
}

fn parse<'a, T: Deserialize<'a>>(data: &'a str, kind: &str) -> Result<T, ProviderError> {
    sonic_rs::from_str(data).map_err(|error| protocol(format!("malformed {kind} event: {error}")))
}

fn protocol(detail: String) -> ProviderError {
    ProviderError::Protocol {
        family: Family::PiMessages,
        detail,
    }
}

/// Decodes one pi-messages SSE event stream into neutral events.
pub(crate) fn decode_events<S>(
    events: S,
    decoder: PiMessagesDecoder,
) -> impl Stream<Item = Result<StreamEvent, ProviderError>> + Send + 'static
where
    S: Stream<Item = Result<SseEvent, ProviderError>> + Send + 'static,
{
    let state = (Box::pin(events), decoder, VecDeque::new());
    stream::unfold(state, |(mut events, mut decoder, mut queue)| async move {
        loop {
            if let Some(item) = queue.pop_front() {
                return Some((item, (events, decoder, queue)));
            }
            if decoder.ended {
                return None;
            }
            let mut out = Vec::new();
            let result = match events.next().await {
                Some(Ok(event)) => decoder.feed(&event.data, &mut out).map(drop),
                Some(Err(error)) => {
                    decoder.ended = true;
                    Err(error)
                }
                None => decoder.end(&mut out).map(drop),
            };
            queue.extend(out.into_iter().map(Ok));
            if let Err(error) = result {
                queue.push_back(Err(error));
            }
        }
    })
}

#[cfg(test)]
mod tests;
