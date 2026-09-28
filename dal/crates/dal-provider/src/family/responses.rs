//! The `OpenAI` Responses family: the typed request body and the stream
//! decoder.
//!
//! [`request_body`] writes `POST <base>/responses` bodies whose members keep
//! one fixed order, so two builds of the same request are equal byte for
//! byte. Nothing is stored server-side: `store` is `false`, no
//! `previous_response_id` is ever sent, and reasoning continuity travels as
//! verbatim reasoning items bound to the family and model that produced them.
//!
//! [`Decoder`] maps the Responses event grammar onto the neutral
//! [`StreamEvent`] grammar: deltas and replay items in arrival order, then
//! exactly one `ToolCallsDone`, one `Usage`, and one `Stop`, or one error
//! instead. Events are dispatched by their JSON `type`; unknown event types
//! and unknown members are ignored, a `[DONE]` data line is ignored anywhere,
//! and a decreasing `sequence_number` is a protocol error. [`decode`] drives a
//! [`Decoder`] over an SSE event stream and ends with `StreamCut` when the
//! input ends before a terminal event.

use std::{borrow::Cow, collections::VecDeque, pin::Pin};

use base64::Engine as _;
use dal_core::{
    AssistantPart, CallId, ContextItem, Family, ModelRequest, ModelRoute, Part, RawJson,
    ReplaySource, SessionId, Usage,
};
use futures::{Stream, StreamExt, stream};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    auth::credential::Credential,
    error::ProviderError,
    provider::AuthStyle,
    sse::SseEvent,
    stream::{ReplayPayload, StopReason, StreamEvent, ToolArgs, ToolCall},
    thinking::WireThinking,
};

/// The endpoint path of a Responses request, relative to the provider base.
pub(crate) const PATH: &str = "/responses";

/// The data line some servers send after the terminal event; always ignored.
const DONE: &str = "[DONE]";

/// The status an in-stream failure reports: the 200 of the stream response.
const IN_STREAM_STATUS: u16 = 200;

/// A Responses request ready for HTTPS or the shared WebSocket state machine.
///
/// This type intentionally has no `Debug` implementation because it retains a
/// credential-derived header and redaction secret.
pub(crate) struct ResponsesRequest<'a> {
    /// Provider-neutral request after stored blobs are inlined.
    pub(crate) request: &'a ModelRequest,
    /// The already-clamped OpenAI thinking fragment.
    pub(crate) thinking: WireThinking,
    /// Whether the resolved model supports reasoning summaries.
    pub(crate) reasoning_summary: bool,
    /// API-key header style configured for this provider.
    pub(crate) auth: AuthStyle,
    /// Credential resolved for this request.
    pub(crate) credential: &'a Credential,
    /// Session id used by the shared WebSocket session pool.
    pub(crate) session_id: SessionId,
    /// Already-rendered provider user-agent.
    pub(crate) user_agent: &'a str,
}

/// A Responses request lowered into its body and configured authentication.
///
/// This type intentionally has no `Debug` implementation because it holds
/// credential material.
pub(crate) struct ResponsesWire {
    /// Configured credential header; `None` preserves keyless HTTPS behavior.
    pub(crate) auth_header: Option<(&'static str, String)>,
    /// Whether the credential is OAuth and may be refreshed by lifecycle.
    pub(crate) oauth: bool,
    /// Raw credential used only to redact server-provided error messages.
    pub(crate) secret: Box<str>,
    /// The exact HTTPS Responses request body.
    pub(crate) body: Vec<u8>,
    /// Resolved Responses model id.
    pub(crate) model: Box<str>,
    /// Session identity for WebSocket connection and fallback state.
    pub(crate) session_id: SessionId,
    /// The user-agent required on the HTTPS and WebSocket transports.
    pub(crate) user_agent: String,
}

#[derive(Serialize)]
struct Body<'a> {
    model: &'a str,
    instructions: &'a str,
    input: Vec<Input<'a>>,
    tools: Vec<Tool<'a>>,
    tool_choice: &'static str,
    parallel_tool_calls: bool,
    reasoning: Option<Reasoning>,
    store: bool,
    stream: bool,
    include: [&'static str; 1],
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<&'a str>,
}

#[derive(Serialize)]
struct Tool<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    name: &'a str,
    description: &'a str,
    parameters: &'a RawJson,
}

#[derive(Serialize)]
struct Reasoning {
    effort: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<&'static str>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Input<'a> {
    Message(Message<'a>),
    /// A reasoning item exactly as the model produced it.
    Reasoning(&'a RawJson),
    FunctionCall(FunctionCall<'a>),
    FunctionCallOutput(FunctionCallOutput<'a>),
}

#[derive(Serialize)]
struct Message<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    role: &'static str,
    content: Vec<Content<'a>>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Content<'a> {
    InputText { text: Cow<'a, str> },
    InputImage { detail: &'static str, image_url: String },
    OutputText { text: &'a str },
}

#[derive(Serialize)]
struct FunctionCall<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    call_id: &'a str,
    name: &'a str,
    arguments: &'a str,
}

#[derive(Serialize)]
struct FunctionCallOutput<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    call_id: &'a str,
    output: String,
}

fn invalid(message: impl Into<String>) -> ProviderError {
    ProviderError::InvalidRequest {
        message: message.into(),
    }
}

fn message<'a>(role: &'static str, content: Vec<Content<'a>>) -> Input<'a> {
    Input::Message(Message {
        kind: "message",
        role,
        content,
    })
}

fn image(mime: &str, bytes: &[u8]) -> Content<'static> {
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    Content::InputImage {
        detail: "auto",
        image_url: format!("data:{mime};base64,{data}"),
    }
}

fn blob_error() -> ProviderError {
    invalid("a stored blob part must be read into the request before a Responses body is built")
}

/// Appends the pending assistant text parts as one assistant message.
fn flush_text<'a>(text: &mut Vec<Content<'a>>, items: &mut Vec<Input<'a>>) {
    if !text.is_empty() {
        items.push(message("assistant", std::mem::take(text)));
    }
}

fn assistant_items<'a>(parts: &'a [AssistantPart], replay: bool, items: &mut Vec<Input<'a>>) {
    let mut text = Vec::new();
    for part in parts {
        match part {
            AssistantPart::Text { text: slice } => text.push(Content::OutputText { text: &**slice }),
            AssistantPart::Thinking {
                replay: Some(item), ..
            } if replay => {
                flush_text(&mut text, items);
                items.push(Input::Reasoning(item));
            }
            AssistantPart::Thinking { .. } => {}
            AssistantPart::ToolCall { call, name, args } => {
                flush_text(&mut text, items);
                items.push(Input::FunctionCall(FunctionCall {
                    kind: "function_call",
                    call_id: call.as_str(),
                    name: &**name,
                    arguments: args.as_str(),
                }));
            }
        }
    }
    flush_text(&mut text, items);
}

/// Lowers one tool result: the `function_call_output` item and, when the
/// result carries images, the user message that follows the result run.
fn tool_result<'a>(
    call: &'a CallId,
    parts: &[Part],
) -> Result<(Input<'a>, Option<Input<'a>>), ProviderError> {
    let mut output = String::new();
    let mut separator = "";
    let mut images = Vec::new();
    for part in parts {
        match part {
            Part::Text { text } => {
                output.push_str(separator);
                output.push_str(text);
                separator = "\n";
            }
            Part::Image { mime, bytes } => images.push(image(mime, bytes)),
            Part::Blob { .. } => return Err(blob_error()),
        }
    }
    let result = Input::FunctionCallOutput(FunctionCallOutput {
        kind: "function_call_output",
        call_id: call.as_str(),
        output,
    });
    if images.is_empty() {
        return Ok((result, None));
    }
    let mut content = vec![Content::InputText {
        text: Cow::Owned(format!("Images from tool call {}.", call.as_str())),
    }];
    content.append(&mut images);
    Ok((result, Some(message("user", content))))
}

/// Lowers the context in order. The image messages of a run of tool results
/// follow the whole run, one message per call, so every `function_call_output`
/// stays adjacent to its siblings.
fn input_items<'a>(
    context: &'a [ContextItem],
    family: Family,
    model: &str,
) -> Result<Vec<Input<'a>>, ProviderError> {
    let mut items = Vec::new();
    let mut result_images = Vec::new();
    for item in context {
        if !matches!(item, ContextItem::ToolResult { .. }) {
            items.append(&mut result_images);
        }
        match item {
            ContextItem::User { parts } => {
                let content = parts
                    .iter()
                    .map(|part| match part {
                        Part::Text { text } => Ok(Content::InputText {
                            text: Cow::Borrowed(text),
                        }),
                        Part::Image { mime, bytes } => Ok(image(mime, bytes)),
                        Part::Blob { .. } => Err(blob_error()),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                items.push(message("user", content));
            }
            ContextItem::Assistant { source, parts } => {
                let replay = source.family == family && source.model.as_ref() == model;
                assistant_items(parts, replay, &mut items);
            }
            ContextItem::ToolResult { call, parts, .. } => {
                let (result, images) = tool_result(call, parts)?;
                items.push(result);
                result_images.extend(images);
            }
        }
    }
    items.append(&mut result_images);
    Ok(items)
}

/// Writes the Responses body of `request`.
///
/// Members appear in this order: `model`, `instructions` (the system text),
/// `input`, `tools`, `tool_choice`, `parallel_tool_calls`, `reasoning`,
/// `store: false`, `stream: true`, `include: ["reasoning.encrypted_content"]`,
/// and `prompt_cache_key` (the request cache key, normally the session id).
/// `tools`, `tool_choice`, and `parallel_tool_calls` are always present, even
/// when there are no tools; tools keep registration order and are sent without
/// `strict`.
///
/// `thinking` is the fragment from [`crate::plan`]: an effort of
/// `"none"` sends `{"effort":"none"}`, another effort sends it with
/// `"summary":"auto"` when `reasoning_summary` is set, and no effort omits
/// `reasoning`, which is how `off` reaches a model without `none`.
///
/// Reasoning items are sent verbatim only when their attached `ReplaySource`
/// matches the request family and model; other source-bound payloads are
/// omitted. User text is `input_text`, images use their recorded MIME type in
/// `input_image` data URLs with `detail: auto`, assistant text is `output_text`,
/// calls are `function_call` items with raw argument text, and results are
/// `function_call_output` items with joined result text. Tool-result images
/// follow the outputs in a user message headed `Images from tool call <call_id>.`.
///
/// # Errors
/// Returns [`ProviderError::InvalidRequest`] when the route is not an
/// `openai_responses` or `openai_codex` API route, when `thinking` is an
/// Anthropic fragment, or when the context still holds a stored blob part.
pub(crate) fn request_body(
    request: &ModelRequest,
    thinking: WireThinking,
    reasoning_summary: bool,
) -> Result<Vec<u8>, ProviderError> {
    let ModelRoute::Api { family, model } = &request.model else {
        return Err(invalid("a Responses body needs an API model route"));
    };
    if !matches!(family, Family::Responses | Family::Codex) {
        return Err(invalid(
            "a Responses body needs an openai_responses or openai_codex route",
        ));
    }
    let WireThinking::OpenAi { effort } = thinking else {
        return Err(invalid(
            "an Anthropic thinking fragment cannot be sent in a Responses body",
        ));
    };
    let model: &str = model;
    let body = Body {
        model,
        instructions: &request.system,
        input: input_items(&request.context, *family, model)?,
        tools: request
            .tools
            .iter()
            .map(|tool| Tool {
                kind: "function",
                name: &tool.name,
                description: &tool.description,
                parameters: &tool.parameters,
            })
            .collect(),
        tool_choice: "auto",
        parallel_tool_calls: true,
        reasoning: effort.map(|effort| Reasoning {
            effort,
            summary: (reasoning_summary && effort != "none").then_some("auto"),
        }),
        store: false,
        stream: true,
        include: ["reasoning.encrypted_content"],
        prompt_cache_key: request.cache_key.as_deref(),
    };
    sonic_rs::to_vec(&body).map_err(|error| invalid(error.to_string()))
}
 
/// Builds a typed Responses wire body and auth header for both transports.
///
/// API keys use the configured provider header. OAuth credentials always use
/// bearer authentication; a keyless credential retains the existing no-auth
/// HTTPS behavior.
///
/// Returns [`ProviderError::InvalidRequest`] for a non-Responses model route
/// or invalid Responses body.
pub(crate) fn wire(input: ResponsesRequest<'_>) -> Result<ResponsesWire, ProviderError> {
    let ModelRoute::Api {
        family: Family::Responses,
        model,
    } = &input.request.model
    else {
        return Err(invalid("a Responses wire needs an openai_responses model route"));
    };
    let body = request_body(input.request, input.thinking, input.reasoning_summary)?;
    let (auth_header, oauth, secret) = match input.credential {
        Credential::ApiKey { key } => {
            let secret = key.expose().to_owned().into_boxed_str();
            let auth_header = match input.auth {
                AuthStyle::Bearer => {
                    ("authorization", format!("Bearer {}", secret.as_ref()))
                }
                AuthStyle::XApiKey => ("x-api-key", secret.to_string()),
            };
            (Some(auth_header), false, secret)
        }
        Credential::OAuth(credential) => {
            let secret = credential.access_token.expose().to_owned().into_boxed_str();
            (
                Some(("authorization", format!("Bearer {}", secret.as_ref()))),
                true,
                secret,
            )
        }
        Credential::None => (None, false, Box::from("")),
    };
    Ok(ResponsesWire {
        auth_header,
        oauth,
        secret,
        body,
        model: model.clone(),
        session_id: input.session_id,
        user_agent: String::from(input.user_agent),
    })
}

#[derive(Deserialize)]
struct Head {
    #[serde(rename = "type")]
    kind: String,
    sequence_number: Option<u64>,
}

#[derive(Deserialize)]
struct TextDelta {
    delta: String,
}

#[derive(Deserialize)]
struct ArgsDelta {
    item_id: String,
    delta: String,
}

#[derive(Deserialize)]
struct ArgsDone {
    item_id: String,
    arguments: String,
}

#[derive(Deserialize)]
struct ItemEvent {
    item: RawJson,
}

#[derive(Deserialize)]
struct ItemKind {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct FunctionItem {
    id: Option<String>,
    call_id: String,
    name: String,
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct ReasoningItem {
    id: Option<String>,
    encrypted_content: Option<String>,
}

#[derive(Deserialize)]
struct Terminal {
    response: WireResponse,
}

#[derive(Deserialize)]
struct WireResponse {
    #[serde(default)]
    output: Vec<RawJson>,
    usage: Option<WireUsage>,
    error: Option<WireError>,
    incomplete_details: Option<Incomplete>,
}

#[derive(Deserialize)]
struct WireUsage {
    input_tokens: Option<u64>,
    input_tokens_details: Option<InputDetails>,
    output_tokens: Option<u64>,
    output_tokens_details: Option<OutputDetails>,
}

#[derive(Deserialize)]
struct InputDetails {
    cached_tokens: Option<u64>,
    cache_write_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct OutputDetails {
    reasoning_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct WireError {
    code: Option<String>,
    message: Option<String>,
}

#[derive(Deserialize)]
struct Incomplete {
    reason: Option<String>,
}

/// Normalizes Responses usage: `input_tokens` already includes the cached
/// tokens, and `output_tokens` loses the separately reported reasoning tokens.
fn usage(wire: Option<WireUsage>) -> Usage {
    let wire = wire.unwrap_or(WireUsage {
        input_tokens: None,
        input_tokens_details: None,
        output_tokens: None,
        output_tokens_details: None,
    });
    let input = wire.input_tokens_details;
    let reasoning = wire.output_tokens_details.and_then(|details| details.reasoning_tokens);
    Usage {
        input_tokens: wire.input_tokens.unwrap_or(0),
        cached_input_tokens: input.as_ref().and_then(|details| details.cached_tokens).unwrap_or(0),
        output_tokens: wire
            .output_tokens
            .unwrap_or(0)
            .saturating_sub(reasoning.unwrap_or(0)),
        reasoning_tokens: reasoning,
        cache_write_tokens: input.and_then(|details| details.cache_write_tokens).unwrap_or(0),
        cost_usd: None,
    }
}

/// Final arguments: empty text is `{}`; anything else must be one JSON value.
fn final_args(text: &str) -> ToolArgs {
    let blank = text
        .trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r'))
        .is_empty();
    ToolArgs::from_bytes(if blank { b"{}" } else { text.as_bytes() })
}

#[derive(Debug)]
struct Call {
    /// The output item id the argument events refer to.
    item_id: String,
    /// The id the next request echoes back.
    call_id: String,
    name: String,
    deltas: String,
    done_args: Option<String>,
    item_args: Option<String>,
}

/// The incremental Responses stream decoder of one response.
///
/// Feed it the data of each SSE event, or of each WebSocket text frame, in
/// arrival order. It emits `TextDelta` for `response.output_text.delta`,
/// `ReasoningDelta` for `response.reasoning_summary_text.delta`,
/// `ToolCallStarted` when a `function_call` item is added, `ToolArgsDelta`
/// for `response.function_call_arguments.delta`, and `Replay` for each
/// completed reasoning item carrying `encrypted_content`. A reasoning item
/// that completes without it is backfilled from the same item id in the
/// terminal response's `output`. The final arguments of a call are the
/// `output_item.done` arguments, else the `function_call_arguments.done`
/// arguments, else the concatenated deltas.
///
/// `response.completed` and `response.incomplete` finish with the replay
/// backfill, `ToolCallsDone`, `Usage`, and `Stop`. `response.failed`, a bare
/// `error` event, malformed JSON, and a decreasing `sequence_number` are
/// errors.
#[derive(Debug)]
pub(crate) struct Decoder {
    family: Family,
    model: Box<str>,
    sequence: Option<u64>,
    calls: Vec<Call>,
    /// Ids of completed reasoning items still missing `encrypted_content`.
    backfill: Vec<String>,
    finished: bool,
}

impl Decoder {
    /// A decoder for one response of `model` on `family`; replay payloads are
    /// bound to that pair.
    #[must_use]
    pub(crate) fn new(family: Family, model: impl Into<Box<str>>) -> Self {
        Self {
            family,
            model: model.into(),
            sequence: None,
            calls: Vec::new(),
            backfill: Vec::new(),
            finished: false,
        }
    }

    /// Decodes one event's data, appending the neutral events to `out`.
    ///
    /// Returns `true` once the terminal `Stop` has been appended; later data
    /// is ignored. `[DONE]` and unknown event types append nothing.
    ///
    /// # Errors
    /// Returns [`ProviderError::Protocol`] for data that is not a JSON event
    /// of the expected shape or for a decreasing `sequence_number`, and the
    /// mapped failure for `response.failed` and `error` events. The decoder
    /// must not be fed after an error.
    pub(crate) fn feed(
        &mut self,
        data: &str,
        out: &mut Vec<StreamEvent>,
    ) -> Result<bool, ProviderError> {
        if self.finished || data == DONE {
            return Ok(self.finished);
        }
        let head: Head = self.parse(data, "event")?;
        if let Some(sequence) = head.sequence_number {
            if self.sequence.is_some_and(|last| sequence < last) {
                return Err(self.protocol("sequence_number went backwards"));
            }
            self.sequence = Some(sequence);
        }
        let kind = head.kind.as_str();
        match kind {
            "response.output_text.delta" => {
                let event: TextDelta = self.parse(data, kind)?;
                if !event.delta.is_empty() {
                    out.push(StreamEvent::TextDelta { text: event.delta });
                }
            }
            "response.reasoning_summary_text.delta" => {
                let event: TextDelta = self.parse(data, kind)?;
                if !event.delta.is_empty() {
                    out.push(StreamEvent::ReasoningDelta { text: event.delta });
                }
            }
            "response.output_item.added" => {
                let event: ItemEvent = self.parse(data, kind)?;
                if self.item_kind(&event.item)? == "function_call" {
                    let item = self.decode_item(&event.item)?;
                    self.open(item, out);
                }
            }
            "response.function_call_arguments.delta" => {
                let event: ArgsDelta = self.parse(data, kind)?;
                if let Some(call) = self.call_mut(&event.item_id)
                    && !event.delta.is_empty()
                {
                    call.deltas.push_str(&event.delta);
                    out.push(StreamEvent::ToolArgsDelta {
                        id: call.call_id.clone(),
                        fragment: event.delta.into_bytes(),
                    });
                }
            }
            "response.function_call_arguments.done" => {
                let event: ArgsDone = self.parse(data, kind)?;
                if let Some(call) = self.call_mut(&event.item_id) {
                    call.done_args = Some(event.arguments);
                }
            }
            "response.output_item.done" => {
                let event: ItemEvent = self.parse(data, kind)?;
                self.item_done(event.item, out)?;
            }
            "response.completed" | "response.incomplete" => {
                let event: Terminal = self.parse(data, kind)?;
                self.finish(kind == "response.completed", event.response, out)?;
                self.finished = true;
                return Ok(true);
            }
            "response.failed" | "error" => return Err(self.failure(kind, data)),
            _ => {}
        }
        Ok(false)
    }

    fn protocol(&self, detail: impl Into<String>) -> ProviderError {
        ProviderError::Protocol {
            family: self.family,
            detail: detail.into(),
        }
    }

    fn status(&self, message: String) -> ProviderError {
        ProviderError::Status {
            family: self.family,
            status: IN_STREAM_STATUS,
            message,
        }
    }

    /// Maps a `response.failed` or bare `error` event onto its failure.
    fn failure(&self, kind: &str, data: &str) -> ProviderError {
        let failed = kind == "response.failed";
        let error = if failed {
            self.parse::<Terminal>(data, kind).map(|event| event.response.error)
        } else {
            self.parse::<WireError>(data, kind).map(Some)
        };
        let (code, message) = match error {
            Ok(Some(error)) => (error.code, error.message.unwrap_or_default()),
            Ok(None) => (None, String::new()),
            Err(malformed) => return malformed,
        };
        match code.as_deref() {
            Some("usage_not_included") if failed => ProviderError::UsageNotIncluded { message },
            Some("rate_limit_exceeded") => ProviderError::RateLimited {
                message,
                retry_after: None,
            },
            Some(code) => ProviderError::context_overflow(self.family, code, &message)
                .unwrap_or_else(|| self.status(message)),
            None => self.status(message),
        }
    }

    fn parse<T: DeserializeOwned>(&self, data: &str, what: &str) -> Result<T, ProviderError> {
        sonic_rs::from_str(data).map_err(|error| self.protocol(format!("malformed {what}: {error}")))
    }

    fn decode_item<T: DeserializeOwned>(&self, item: &RawJson) -> Result<T, ProviderError> {
        item.decode_as()
            .map_err(|error| self.protocol(format!("malformed output item: {error}")))
    }

    fn item_kind(&self, item: &RawJson) -> Result<String, ProviderError> {
        self.decode_item::<ItemKind>(item).map(|item| item.kind)
    }

    fn call_mut(&mut self, item_id: &str) -> Option<&mut Call> {
        self.calls.iter_mut().find(|call| call.item_id == item_id)
    }

    /// Opens a call once per item id; a repeated `added` is ignored. Returns
    /// the index of the item's call.
    fn open(&mut self, item: FunctionItem, out: &mut Vec<StreamEvent>) -> usize {
        let item_id = item.id.unwrap_or_else(|| item.call_id.clone());
        if let Some(index) = self.calls.iter().position(|call| call.item_id == item_id) {
            return index;
        }
        out.push(StreamEvent::ToolCallStarted {
            id: item.call_id.clone(),
            name: item.name.clone(),
        });
        self.calls.push(Call {
            item_id,
            call_id: item.call_id,
            name: item.name,
            deltas: String::new(),
            done_args: None,
            item_args: None,
        });
        self.calls.len() - 1
    }

    fn item_done(&mut self, item: RawJson, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        match self.item_kind(&item)?.as_str() {
            "function_call" => {
                let mut function: FunctionItem = self.decode_item(&item)?;
                let arguments = function.arguments.take();
                let index = self.open(function, out);
                self.calls[index].item_args = arguments;
            }
            "reasoning" => {
                let reasoning: ReasoningItem = self.decode_item(&item)?;
                if reasoning.encrypted_content.is_some() {
                    out.push(self.replay(item));
                } else if let Some(id) = reasoning.id {
                    self.backfill.push(id);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn replay(&self, item: RawJson) -> StreamEvent {
        StreamEvent::Replay {
            payload: ReplayPayload {
                family: self.family,
                model: self.model.clone(),
                item,
            },
        }
    }

    fn finish(
        &mut self,
        completed: bool,
        response: WireResponse,
        out: &mut Vec<StreamEvent>,
    ) -> Result<(), ProviderError> {
        let mut function_output = false;
        for item in response.output {
            match self.item_kind(&item)?.as_str() {
                "function_call" => function_output = true,
                "reasoning" if !self.backfill.is_empty() => {
                    let reasoning: ReasoningItem = self.decode_item(&item)?;
                    let pending = reasoning
                        .id
                        .and_then(|id| self.backfill.iter().position(|wanted| *wanted == id));
                    if let Some(index) = pending {
                        self.backfill.swap_remove(index);
                        // Under `store: false` an item without its encrypted
                        // content refers to nothing and must not be replayed.
                        if reasoning.encrypted_content.is_some() {
                            out.push(self.replay(item));
                        }
                    }
                }
                _ => {}
            }
        }
        let reason = if completed {
            if function_output || !self.calls.is_empty() {
                StopReason::ToolUse
            } else {
                StopReason::EndTurn
            }
        } else {
            match response.incomplete_details.and_then(|details| details.reason) {
                Some(reason) if reason == "max_output_tokens" => StopReason::MaxTokens,
                Some(reason) if reason == "content_filter" => StopReason::Refusal,
                reason => StopReason::Other(reason.unwrap_or_default()),
            }
        };
        let truncated = reason == StopReason::MaxTokens;
        let calls = std::mem::take(&mut self.calls)
            .into_iter()
            .map(|call| {
                let args = if truncated {
                    ToolArgs::Truncated
                } else {
                    final_args(
                        call.item_args
                            .as_deref()
                            .or(call.done_args.as_deref())
                            .unwrap_or(&call.deltas),
                    )
                };
                ToolCall {
                    id: call.call_id,
                    name: call.name,
                    args,
                }
            })
            .collect();
        out.push(StreamEvent::ToolCallsDone { calls });
        out.push(StreamEvent::Usage {
            usage: usage(response.usage),
        });
        out.push(StreamEvent::Stop { reason });
        Ok(())
    }
}

struct Adapter<S> {
    source: Pin<Box<S>>,
    /// `None` once the terminal event or an error was queued.
    decoder: Option<Decoder>,
    ready: VecDeque<Result<StreamEvent, ProviderError>>,
    batch: Vec<StreamEvent>,
}

/// Decodes a Responses SSE stream into neutral events for `model` on
/// `family`.
///
/// The output ends after `Stop` or after the first error without reading more
/// input; an SSE framing error passes through unchanged, and input that ends
/// before a terminal event gives [`ProviderError::StreamCut`].
pub(crate) fn decode<S>(
    events: S,
    family: Family,
    model: impl Into<Box<str>>,
) -> impl Stream<Item = Result<StreamEvent, ProviderError>>
where
    S: Stream<Item = Result<SseEvent, ProviderError>>,
{
    let adapter = Adapter {
        source: Box::pin(events),
        decoder: Some(Decoder::new(family, model)),
        ready: VecDeque::new(),
        batch: Vec::new(),
    };
    stream::unfold(adapter, |mut adapter| async move {
        loop {
            if let Some(item) = adapter.ready.pop_front() {
                return Some((item, adapter));
            }
            let decoder = adapter.decoder.as_mut()?;
            let outcome = match adapter.source.next().await {
                None => Err(ProviderError::StreamCut),
                Some(Err(error)) => Err(error),
                Some(Ok(event)) => decoder.feed(&event.data, &mut adapter.batch),
            };
            adapter.ready.extend(adapter.batch.drain(..).map(Ok));
            match outcome {
                Ok(false) => {}
                Ok(true) => adapter.decoder = None,
                Err(error) => {
                    adapter.ready.push_back(Err(error));
                    adapter.decoder = None;
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use std::{error::Error, sync::Arc};

    use dal_core::{ModelToolSpec, Purpose, RequestParams, ThinkingLevel};
    use futures::executor::block_on;

    use super::*;
    use crate::sse::{decode_stream, encode};

    type TestResult = Result<(), Box<dyn Error>>;

    /// Frames `data` as SSE, splits the bytes into 7-byte chunks, and decodes.
    fn run(data: &[&str]) -> Vec<Result<StreamEvent, ProviderError>> {
        let events: Vec<SseEvent> = data
            .iter()
            .map(|data| SseEvent {
                name: None,
                data: (*data).into(),
            })
            .collect();
        let wire = encode(&events);
        let chunks: Vec<Vec<u8>> = wire.chunks(7).map(<[u8]>::to_vec).collect();
        block_on(decode(decode_stream(stream::iter(chunks)), Family::Responses, "gpt-5").collect())
    }

    fn ok(results: Vec<Result<StreamEvent, ProviderError>>) -> Result<Vec<StreamEvent>, ProviderError> {
        results.into_iter().collect()
    }

    fn calls_of(events: &[StreamEvent]) -> Vec<ToolCall> {
        events
            .iter()
            .find_map(|event| match event {
                StreamEvent::ToolCallsDone { calls } => Some(calls.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    const REASONING_DONE: &str = r#"{"id":"rs_1", "type":"reasoning","summary":[{"type":"summary_text","text":"Check the file."}],"encrypted_content":"gAAAAB","n":1e+02}"#;

    fn turn() -> Vec<String> {
        vec![
            r#"{"type":"response.created","sequence_number":0,"response":{"id":"resp_1","object":"response","status":"in_progress","output":[],"usage":null}}"#.into(),
            r#"{"type":"response.in_progress","sequence_number":1,"response":{"id":"resp_1","status":"in_progress","output":[],"usage":null}}"#.into(),
            r#"{"type":"response.output_item.added","sequence_number":2,"output_index":0,"item":{"id":"rs_1","type":"reasoning","summary":[]}}"#.into(),
            r#"{"type":"response.reasoning_summary_part.added","sequence_number":3,"item_id":"rs_1","output_index":0,"summary_index":0,"part":{"type":"summary_text","text":""}}"#.into(),
            r#"{"type":"response.reasoning_summary_text.delta","sequence_number":4,"item_id":"rs_1","output_index":0,"summary_index":0,"delta":"Check the file."}"#.into(),
            r#"{"type":"response.reasoning_summary_text.done","sequence_number":5,"item_id":"rs_1","output_index":0,"summary_index":0,"text":"Check the file."}"#.into(),
            r#"{"type":"response.reasoning_summary_part.done","sequence_number":6,"item_id":"rs_1","output_index":0,"summary_index":0,"part":{"type":"summary_text","text":"Check the file."}}"#.into(),
            format!(r#"{{"type":"response.output_item.done","sequence_number":7,"output_index":0,"item":{REASONING_DONE}}}"#),
            r#"{"type":"response.output_item.added","sequence_number":8,"output_index":1,"item":{"id":"msg_1","type":"message","role":"assistant","status":"in_progress","content":[]}}"#.into(),
            r#"{"type":"response.content_part.added","sequence_number":9,"item_id":"msg_1","output_index":1,"content_index":0,"part":{"type":"output_text","text":"","annotations":[]}}"#.into(),
            r#"{"type":"response.output_text.delta","sequence_number":10,"item_id":"msg_1","output_index":1,"content_index":0,"delta":"Reading.","logprobs":[]}"#.into(),
            r#"{"type":"response.output_text.done","sequence_number":11,"item_id":"msg_1","output_index":1,"content_index":0,"text":"Reading.","logprobs":[]}"#.into(),
            r#"{"type":"response.content_part.done","sequence_number":12,"item_id":"msg_1","output_index":1,"content_index":0,"part":{"type":"output_text","text":"Reading.","annotations":[]}}"#.into(),
            r#"{"type":"response.output_item.done","sequence_number":13,"output_index":1,"item":{"id":"msg_1","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Reading.","annotations":[]}]}}"#.into(),
            r#"{"type":"response.output_item.added","sequence_number":14,"output_index":2,"item":{"id":"fc_1","type":"function_call","status":"in_progress","call_id":"call_1","name":"read","arguments":""}}"#.into(),
            r#"{"type":"response.function_call_arguments.delta","sequence_number":15,"item_id":"fc_1","output_index":2,"delta":"{\"path\":"}"#.into(),
            r#"{"type":"response.function_call_arguments.done","sequence_number":16,"item_id":"fc_1","output_index":2,"arguments":"{\"path\":\"a.rs\"}"}"#.into(),
            r#"{"type":"response.output_item.done","sequence_number":17,"output_index":2,"item":{"id":"fc_1","type":"function_call","status":"completed","call_id":"call_1","name":"read","arguments":"{\"path\":\"a.rs\"}"}}"#.into(),
            format!(
                r#"{{"type":"response.completed","sequence_number":18,"response":{{"id":"resp_1","status":"completed","output":[{REASONING_DONE},{{"id":"fc_1","type":"function_call","call_id":"call_1","name":"read","arguments":"{{\"path\":\"a.rs\"}}"}}],"usage":{{"input_tokens":120,"input_tokens_details":{{"cached_tokens":100}},"output_tokens":50,"output_tokens_details":{{"reasoning_tokens":20}},"total_tokens":170}}}}}}"#
            ),
        ]
    }

    #[test]
    fn canonical_turn_decodes_in_causal_order_and_ignores_a_trailing_done() -> TestResult {
        let mut data = turn();
        data.push(DONE.into());
        data.push(r#"{"type":"response.output_text.delta","sequence_number":19,"delta":"late"}"#.into());
        let data: Vec<&str> = data.iter().map(String::as_str).collect();
        let events = ok(run(&data))?;
        let expected = vec![
            StreamEvent::ReasoningDelta {
                text: "Check the file.".into(),
            },
            StreamEvent::Replay {
                payload: ReplayPayload {
                    family: Family::Responses,
                    model: "gpt-5".into(),
                    item: RawJson::parse(REASONING_DONE)?,
                },
            },
            StreamEvent::TextDelta {
                text: "Reading.".into(),
            },
            StreamEvent::ToolCallStarted {
                id: "call_1".into(),
                name: "read".into(),
            },
            StreamEvent::ToolArgsDelta {
                id: "call_1".into(),
                fragment: br#"{"path":"#.to_vec(),
            },
            StreamEvent::ToolCallsDone {
                calls: vec![ToolCall {
                    id: "call_1".into(),
                    name: "read".into(),
                    args: ToolArgs::Parsed(RawJson::parse(r#"{"path":"a.rs"}"#)?),
                }],
            },
            StreamEvent::Usage {
                usage: Usage {
                    input_tokens: 120,
                    cached_input_tokens: 100,
                    output_tokens: 30,
                    reasoning_tokens: Some(20),
                    cache_write_tokens: 0,
                    cost_usd: None,
                },
            },
            StreamEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ];
        assert_eq!(events, expected);
        let StreamEvent::Replay { payload } = &events[1] else {
            return Err("second event is not a replay".into());
        };
        assert_eq!(payload.item.as_str(), REASONING_DONE);
        Ok(())
    }

    #[test]
    fn reasoning_without_encrypted_content_is_backfilled_from_the_completed_output() -> TestResult {
        let completed = r#"{"id":"rs_9","type":"reasoning","summary":[],"encrypted_content":"full"}"#;
        let terminal = format!(
            r#"{{"type":"response.completed","response":{{"output":[{completed}],"usage":null}}}}"#
        );
        let events = ok(run(&[
            r#"{"type":"response.output_item.done","item":{"id":"rs_9","type":"reasoning","summary":[]}}"#,
            r#"{"type":"response.output_text.delta","delta":"hi"}"#,
            terminal.as_str(),
        ]))?;
        assert!(matches!(&events[0], StreamEvent::TextDelta { text } if text == "hi"));
        assert!(matches!(
            &events[1],
            StreamEvent::Replay { payload } if payload.item.as_str() == completed
        ));
        assert_eq!(events.len(), 5);
        assert_eq!(
            events[4],
            StreamEvent::Stop {
                reason: StopReason::EndTurn
            }
        );
        let unrecoverable = ok(run(&[
            r#"{"type":"response.output_item.done","item":{"id":"rs_9","type":"reasoning","summary":[]}}"#,
            r#"{"type":"response.completed","response":{"output":[{"id":"rs_9","type":"reasoning","summary":[]}]}}"#,
        ]))?;
        assert!(
            !unrecoverable
                .iter()
                .any(|event| matches!(event, StreamEvent::Replay { .. })),
            "replayed a reasoning item without encrypted_content"
        );
        Ok(())
    }

    #[test]
    fn final_arguments_prefer_item_done_then_arguments_done_then_deltas() -> TestResult {
        let added = r#"{"type":"response.output_item.added","item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"f","arguments":""}}"#;
        let deltas = [
            r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"{\"a\":"}"#,
            r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"1}"}"#,
        ];
        let args_done = r#"{"type":"response.function_call_arguments.done","item_id":"fc_1","arguments":"{\"a\":13}"}"#;
        let item_done = r#"{"type":"response.output_item.done","item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"f","arguments":"{\"a\":23}"}}"#;
        let completed = r#"{"type":"response.completed","response":{"output":[{"type":"function_call","call_id":"call_1","name":"f","arguments":""}]}}"#;
        for (script, expected) in [
            (vec![added, deltas[0], deltas[1], args_done, item_done, completed], r#"{"a":23}"#),
            (vec![added, deltas[0], deltas[1], args_done, completed], r#"{"a":13}"#),
            (vec![added, deltas[0], deltas[1], completed], r#"{"a":1}"#),
        ] {
            let events = ok(run(&script))?;
            let calls = calls_of(&events);
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].args, ToolArgs::Parsed(RawJson::parse(expected)?));
            assert_eq!(
                events.last(),
                Some(&StreamEvent::Stop {
                    reason: StopReason::ToolUse
                })
            );
        }
        Ok(())
    }

    #[test]
    fn empty_final_arguments_parse_as_an_empty_object() -> TestResult {
        let events = ok(run(&[
            r#"{"type":"response.output_item.done","item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"now","arguments":""}}"#,
            r#"{"type":"response.completed","response":{"output":[{"type":"function_call","call_id":"call_1","name":"now","arguments":""}]}}"#,
        ]))?;
        assert_eq!(
            events[0],
            StreamEvent::ToolCallStarted {
                id: "call_1".into(),
                name: "now".into()
            }
        );
        assert_eq!(calls_of(&events)[0].args, ToolArgs::Parsed(RawJson::parse("{}")?));
        Ok(())
    }

    #[test]
    fn incomplete_maps_the_reason_and_truncates_every_call_under_max_tokens() -> TestResult {
        let closed = r#"{"type":"response.output_item.done","item":{"id":"fc_0","type":"function_call","call_id":"call_0","name":"f","arguments":"{\"b\":2}"}}"#;
        let added = r#"{"type":"response.output_item.added","item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"f","arguments":""}}"#;
        let delta = r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"{\"a\""}"#;
        for (reason, stop) in [
            (r#""max_output_tokens""#, StopReason::MaxTokens),
            (r#""content_filter""#, StopReason::Refusal),
            (r#""max_messages""#, StopReason::Other("max_messages".into())),
        ] {
            let terminal = format!(
                r#"{{"type":"response.incomplete","response":{{"status":"incomplete","incomplete_details":{{"reason":{reason}}},"output":[]}}}}"#
            );
            let events = ok(run(&[closed, added, delta, terminal.as_str()]))?;
            let calls = calls_of(&events);
            if stop == StopReason::MaxTokens {
                // D-13: arguments from a length-cut response are never run,
                // even for a call the server already closed.
                assert_eq!(calls[0].args, ToolArgs::Truncated);
                assert_eq!(calls[1].args, ToolArgs::Truncated);
            } else {
                assert_eq!(calls[0].args, ToolArgs::Parsed(RawJson::parse(r#"{"b":2}"#)?));
                assert!(matches!(calls[1].args, ToolArgs::Invalid { .. }));
            }
            assert_eq!(events.last(), Some(&StreamEvent::Stop { reason: stop }));
        }
        Ok(())
    }

    #[test]
    fn failures_map_by_code_and_end_the_stream_after_delivery() {
        let delta = r#"{"type":"response.output_text.delta","delta":"par"}"#;
        let failed = |code: &str| {
            format!(
                r#"{{"type":"response.failed","response":{{"status":"failed","error":{{"code":"{code}","message":"boom"}}}}}}"#
            )
        };
        let server_error = failed("server_error");
        let server = run(&[delta, server_error.as_str(), delta]);
        assert_eq!(server.len(), 2);
        assert!(matches!(&server[0], Ok(StreamEvent::TextDelta { text }) if text == "par"));
        assert!(matches!(
            &server[1],
            Err(ProviderError::Status { family: Family::Responses, status: 200, message }) if message == "boom"
        ));
        assert!(matches!(
            run(&[failed("usage_not_included").as_str()]).as_slice(),
            [Err(ProviderError::UsageNotIncluded { message })] if message == "boom"
        ));
        assert!(matches!(
            run(&[failed("rate_limit_exceeded").as_str()]).as_slice(),
            [Err(ProviderError::RateLimited { message, retry_after: None })] if message == "boom"
        ));
        assert!(matches!(
            run(&[r#"{"type":"error","code":"rate_limit_exceeded","message":"slow"}"#]).as_slice(),
            [Err(ProviderError::RateLimited { message, retry_after: None })] if message == "slow"
        ));
        assert!(matches!(
            run(&[r#"{"type":"error","code":"usage_not_included","message":"no"}"#]).as_slice(),
            [Err(ProviderError::Status { status: 200, message, .. })] if message == "no"
        ));
        for (event, code) in [
            (
                r#"{"type":"response.failed","response":{"error":{"code":"context_length_exceeded","message":"too long"}}}"#,
                "context_length_exceeded",
            ),
            (
                r#"{"type":"error","code":"context_window_exceeded","message":"too long"}"#,
                "context_window_exceeded",
            ),
        ] {
            assert!(
                matches!(
                    run(&[event]).as_slice(),
                    [Err(ProviderError::ContextOverflow { family: Family::Responses, code: c, message })]
                        if c == code && message == "too long"
                ),
                "{event} did not map to ContextOverflow"
            );
        }
        assert!(matches!(
            run(&[r#"{"type":"response.failed","response":{"error":{"code":"server_error","message":"context length exceeded"}}}"#]).as_slice(),
            [Err(ProviderError::Status { status: 200, .. })]
        ));
        assert!(matches!(
            run(&[r#"{"type":"response.failed","response":{"error":null}}"#]).as_slice(),
            [Err(ProviderError::Status { status: 200, message, .. })] if message.is_empty()
        ));
    }

    #[test]
    fn decreasing_sequence_number_is_a_protocol_error() -> TestResult {
        let results = run(&[
            r#"{"type":"response.in_progress","sequence_number":5}"#,
            r#"{"type":"response.in_progress","sequence_number":4}"#,
        ]);
        let [Err(error)] = results.as_slice() else {
            return Err(format!("expected one error, got {results:?}").into());
        };
        assert_eq!(
            error.to_string(),
            "openai sent an invalid stream: sequence_number went backwards"
        );
        Ok(())
    }

    #[test]
    fn malformed_or_missing_terminal_is_an_error() {
        assert!(matches!(
            run(&[r#"{"type":"response.output_text.delta","delta":"a"}"#]).as_slice(),
            [Ok(StreamEvent::TextDelta { .. }), Err(ProviderError::StreamCut)]
        ));
        assert!(matches!(
            run(&[DONE]).as_slice(),
            [Err(ProviderError::StreamCut)]
        ));
        for malformed in [
            r#"{"type":"response.completed","response":{"output":[{"id":"x"}]}}"#,
            r#"{"type":"response.completed","response":"done"}"#,
            r#"{"type":"response.completed""#,
            r#"{"sequence_number":1}"#,
        ] {
            assert!(
                matches!(
                    run(&[malformed]).as_slice(),
                    [Err(ProviderError::Protocol { family: Family::Responses, .. })]
                ),
                "accepted {malformed}"
            );
        }
    }

    fn request() -> Result<ModelRequest, Box<dyn Error>> {
        Ok(ModelRequest {
            purpose: Purpose::Turn,
            model: ModelRoute::Api {
                family: Family::Responses,
                model: "gpt-5".into(),
            },
            system: Arc::from("Be brief."),
            tools: Arc::from([ModelToolSpec {
                name: "read".into(),
                description: "Read a file.".into(),
                parameters: RawJson::parse(r#"{"type":"object", "properties":{}}"#)?,
            }]),
            context: Arc::from([
                ContextItem::User {
                    parts: vec![
                        Part::Text { text: "Look".into() },
                        Part::Image {
                            mime: "image/png".into(),
                            bytes: Box::from(*b"\x89PNG"),
                        },
                    ],
                },
                ContextItem::Assistant {
                    source: ReplaySource {
                        family: Family::Responses,
                        model: "gpt-5".into(),
                    },
                    parts: vec![
                        AssistantPart::Thinking {
                            text: "t".into(),
                            replay: Some(RawJson::parse(
                                r#"{"type":"reasoning","id":"rs_1","encrypted_content":"e", "n":1e+02}"#,
                            )?),
                        },
                        AssistantPart::Text {
                            text: "Reading.".into(),
                        },
                        AssistantPart::ToolCall {
                            call: CallId::new("call_1"),
                            name: "read".into(),
                            args: RawJson::parse(r#"{"path": "a.rs"}"#)?,
                        },
                    ],
                },
                ContextItem::ToolResult {
                    call: CallId::new("call_1"),
                    name: "read".into(),
                    is_error: false,
                    parts: vec![
                        Part::Text {
                            text: "fn main".into(),
                        },
                        Part::Image {
                            mime: "image/jpeg".into(),
                            bytes: Box::from(*b"img"),
                        },
                    ],
                },
                ContextItem::User {
                    parts: vec![Part::Text { text: "Next".into() }],
                },
            ]),
            params: RequestParams {
                thinking: ThinkingLevel::High,
                effort: None,
                temperature: None,
            },
            cache_key: Some("sess-1".into()),
        })
    }

    const EXPECTED_BODY: &str = concat!(
        r#"{"model":"gpt-5","instructions":"Be brief.","input":["#,
        r#"{"type":"message","role":"user","content":[{"type":"input_text","text":"Look"},{"type":"input_image","detail":"auto","image_url":"data:image/png;base64,iVBORw=="}]},"#,
        r#"{"type":"reasoning","id":"rs_1","encrypted_content":"e", "n":1e+02},"#,
        r#"{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Reading."}]},"#,
        r#"{"type":"function_call","call_id":"call_1","name":"read","arguments":"{\"path\": \"a.rs\"}"},"#,
        r#"{"type":"function_call_output","call_id":"call_1","output":"fn main"},"#,
        r#"{"type":"message","role":"user","content":[{"type":"input_text","text":"Images from tool call call_1."},{"type":"input_image","detail":"auto","image_url":"data:image/jpeg;base64,aW1n"}]},"#,
        r#"{"type":"message","role":"user","content":[{"type":"input_text","text":"Next"}]}],"#,
        r#""tools":[{"type":"function","name":"read","description":"Read a file.","parameters":{"type":"object", "properties":{}}}],"#,
        r#""tool_choice":"auto","parallel_tool_calls":true,"reasoning":{"effort":"high","summary":"auto"},"#,
        r#""store":false,"stream":true,"include":["reasoning.encrypted_content"],"prompt_cache_key":"sess-1"}"#,
    );

    #[test]
    fn body_is_byte_exact_and_replays_only_to_the_bound_route() -> TestResult {
        let request = request()?;
        let high = WireThinking::OpenAi {
            effort: Some("high"),
        };
        let body = request_body(&request, high, true)?;
        assert_eq!(String::from_utf8(body)?, EXPECTED_BODY);
        assert_eq!(request_body(&request, high, true)?, EXPECTED_BODY.as_bytes());

        for (family, model) in [
            (Family::Responses, "gpt-4.1"),
            (Family::Codex, "gpt-5"),
        ] {
            let mut foreign = request.clone();
            let mut context = foreign.context.to_vec();
            let ContextItem::Assistant { source, .. } = &mut context[1] else {
                return Err("fixture assistant item is missing".into());
            };
            source.family = family;
            source.model = model.into();
            foreign.context = Arc::from(context);
            let body = String::from_utf8(request_body(&foreign, high, true)?)?;
            assert!(!body.contains("rs_1"), "replayed to a foreign route: {body}");
            assert!(body.contains(r#"},{"type":"message","role":"assistant""#));
        }
        Ok(())
    }

    #[test]
    fn off_sends_effort_none_only_when_supported() -> TestResult {
        let request = request()?;
        let none = String::from_utf8(request_body(
            &request,
            WireThinking::OpenAi {
                effort: Some("none"),
            },
            true,
        )?)?;
        assert!(none.contains(r#""parallel_tool_calls":true,"reasoning":{"effort":"none"},"store":false"#));
        let omitted = String::from_utf8(request_body(
            &request,
            WireThinking::OpenAi { effort: None },
            true,
        )?)?;
        assert!(omitted.contains(r#""parallel_tool_calls":true,"store":false"#));
        assert!(!omitted.contains("reasoning\":"));
        assert!(!omitted.contains("previous_response_id"));
        Ok(())
    }

    #[test]
    fn body_without_tools_keeps_empty_tool_members() -> TestResult {
        let mut request = request()?;
        request.tools = Arc::from([]);
        request.cache_key = None;
        let body = String::from_utf8(request_body(
            &request,
            WireThinking::OpenAi { effort: None },
            false,
        )?)?;
        assert!(body.contains(r#""tools":[],"tool_choice":"auto","parallel_tool_calls":true"#));
        assert!(!body.contains("prompt_cache_key"));
        assert!(!body.contains("previous_response_id"));
        Ok(())
    }
}
