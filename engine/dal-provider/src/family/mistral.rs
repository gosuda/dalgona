//! The native Mistral conversations family (`v1/conversations`): request
//! lowering and SSE decoding.
//!
//! [`request_body`] lowers one [`ModelRequest`] and its composed
//! [`ThinkingPlan`] to the exact `POST <base>/conversations` body with
//! `stream: true`. This is the conversations endpoint, not the chat
//! completions one: turns arrive as typed `inputs` entries instead of
//! messages, the system text travels as top-level `instructions`, sampling
//! travels as `completion_args`, and the response is a conversation with a
//! server-assigned id and an `outputs` array. The member order is fixed and
//! two builds of the same request are equal byte for byte. Tool schemas and
//! tool-call arguments are written from their [`RawJson`] text, never
//! re-encoded.
//!
//! Wire sources: with the pinned senpi tree implementing its
//! `mistral-conversations` api on top of `v1/chat/completions` (a shape the
//! ENG-05 contract forbids reusing), everything here derives from Mistral's
//! public API instead and is marked [INFERENCE]: the official generated
//! `client-ts` (`models/components/conversationevents.ts` and siblings,
//! read from a scratch clone) fixes the SSE event names, the entry shapes,
//! `completion_args`, the thinking chunk with its replay `signature`, and
//! the `ConversationUsageInfo` shape; the endpoint paths and response
//! envelope come from the public conversations reference. The one exception
//! is tool-call identity, which follows the pinned senpi normalizer:
//! Mistral call ids are nine alphanumeric characters; ids dalgon replays
//! are projected into that alphabet, hashed only when they do not already
//! fit, exactly like the pinned `deriveMistralToolCallId` (the hash itself
//! is SHA-256 hex rather than the pinned `shortHash`, an [INFERENCE]
//! divergence that keeps replay deterministic with the workspace's existing
//! `sha2` dependency).
//!
//! [`MistralDecoder`] turns the data of each SSE event into neutral
//! [`StreamEvent`]s with the grammar of [`crate::stream`]: deltas as they
//! arrive, then one `ToolCallsDone`, one `Usage`, and one `Stop` at the
//! `conversation.response.done` event, or one error instead. Dispatch reads
//! the JSON `type` member; the SSE event name is ignored, like every other
//! family decoder. `conversation.response.started` records the conversation
//! id the server assigned (kept, since the neutral grammar has no slot for
//! it); `message.output.delta` content becomes text and reasoning deltas;
//! `function.call.delta` opens one call per `output_index` with the native
//! `tool_call_id` and streams its argument fragments; a delta without an
//! `output_index` resumes the open call its id names, opens the next slot
//! for an unseen id, or continues the call that last spoke (the chat
//! decoder's unkeyed rule); server-side
//! `tool.execution.*` and `agent.handoff.*` events and any unknown type are
//! ignored; requests dalgon builds stay strict.
//!
//! The done event carries no finish reason, so the stop reason is derived:
//! calls present give `ToolUse`, otherwise `EndTurn`. Usage maps
//! `prompt_tokens` to `input_tokens` and `completion_tokens` to
//! `output_tokens`; the conversations usage shape reports no cache split.
//! Cancellation matches the family pattern: the decoder is pure, the
//! terminal is emitted exactly once, input after it is ignored, end of input
//! before the done event is [`ProviderError::StreamCut`], and dropping the
//! stream drops the byte source, which cancels the underlying transfer.

use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap, HashSet, VecDeque, btree_map::Entry as MapEntry},
    mem,
};

use base64::{Engine, engine::general_purpose::STANDARD};
use dal_core::{
    AssistantPart, BlobId, ContextItem, Family, ModelRequest, ModelRoute, Part, RawJson, Usage,
};
use futures::{Stream, StreamExt, stream};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sonic_rs::JsonValueTrait;

use crate::{
    error::ProviderError,
    retry::{RequestState, RetryDecision, classify},
    sse::SseEvent,
    stream::{StopReason, StreamEvent, ToolArgs, ToolCall},
    stream_budget::StreamBudget,
    thinking::{ThinkingPlan, WireThinking},
    tool_names::{WIRE_NAME_MAX, wire_name},
};

/// The endpoint path of the family, relative to the provider base URL.
/// The default base is `https://api.mistral.ai/v1`; the `#stream` operation
/// suffix of the SDK docs never goes on the wire.
pub(crate) const ENDPOINT_PATH: &str = "conversations";

/// The wire length of a Mistral tool-call id.
const TOOL_CALL_ID_LENGTH: usize = 9;

/// The HTTP status of the stream response that carries an in-stream error.
const IN_STREAM_STATUS: u16 = 200;

const NO_USAGE: Usage = Usage {
    input_tokens: 0,
    cached_input_tokens: 0,
    output_tokens: 0,
    reasoning_tokens: None,
    cache_write_tokens: 0,
    premium_requests: None,
    cost_usd: None,
};

/// A request that cannot be lowered to a conversations body.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum MistralBodyError {
    /// The request route is not a `mistral` model route.
    #[error("a mistral request needs a mistral model route, not `{route}`")]
    NotMistral {
        /// The route id of the request.
        route: Box<str>,
    },
    /// A content part still refers to stored content; the caller resolves
    /// blobs to inline bytes before lowering.
    #[error("content blob {blob_id} must be read into the request before a mistral body is built")]
    UnresolvedBlob {
        /// The unresolved blob.
        blob_id: BlobId,
    },
    /// The thinking plan was composed for the Anthropic family.
    #[error("the thinking plan carries an Anthropic fragment, not a mistral reasoning effort")]
    ForeignThinking,
    /// The JSON writer failed.
    #[error("mistral body encoding failed: {message}")]
    Encode {
        /// The writer message.
        message: String,
    },
}

#[derive(Serialize)]
struct Body<'a> {
    model: &'a str,
    inputs: Vec<Entry<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<WireTool<'a>>,
    stream: bool,
    store: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    completion_args: Option<CompletionArgs>,
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum Entry<'a> {
    #[serde(rename = "message.input")]
    MessageInput { role: Role, content: Vec<Chunk<'a>> },
    #[serde(rename = "message.output")]
    MessageOutput { role: Role, content: Vec<Chunk<'a>> },
    #[serde(rename = "function.call")]
    FunctionCall {
        tool_call_id: String,
        name: Cow<'a, str>,
        arguments: &'a RawJson,
    },
    #[serde(rename = "function.result")]
    FunctionResult {
        tool_call_id: String,
        result: String,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum Role {
    User,
    Assistant,
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum Chunk<'a> {
    #[serde(rename = "text")]
    Text { text: Cow<'a, str> },
    #[serde(rename = "thinking")]
    Thinking { thinking: Vec<ThinkingText<'a>> },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: String },
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ThinkingText<'a> {
    Text { text: Cow<'a, str> },
}

#[derive(Serialize)]
struct WireTool<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    function: WireToolFunction<'a>,
}

#[derive(Serialize)]
struct WireToolFunction<'a> {
    name: Cow<'a, str>,
    description: &'a str,
    parameters: &'a RawJson,
}

#[derive(Serialize)]
struct CompletionArgs {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'static str>,
}

/// Lowers `request` to the JSON body of `POST <base>/conversations`.
///
/// `thinking` is the plan composed from `request.params`, the hooks, and the
/// model row. The shared [`WireThinking::OpenAi`] fragment is the only
/// non-Anthropic fragment the planner produces; its effort spells
/// `reasoning_effort`, clamped down to the conversations ladder
/// (`minimal` to `low`, `xhigh` to `high`) with `none` kept as the
/// thinking-off form and unknown spellings omitted. `request.cache_key` is
/// not sent: every turn opens a fresh server conversation with the full
/// inputs (`store: false`), so history stays dalgon-owned like every other
/// family, which never resends provider ids. The system text becomes
/// `instructions` unless empty. User parts become one `message.input` with
/// typed chunks; consecutive user entries coalesce because Mistral's
/// role-order validation is not documented to accept two in a row.
/// Assistant text and reasoning become one `message.output` with the
/// thinking chunks first (Mistral takes reasoning back as a `thinking` part
/// at the front of the content), each tool call becomes its own
/// `function.call` with the nine-character wire id, and each tool result
/// becomes a `function.result`; the images of a tool result follow in one
/// user message, each run's images after the text part `Images from tool
/// call <call_id>.`. Without tools, `tools` and `tool_choice` are omitted.
///
/// # Errors
///
/// [`MistralBodyError::NotMistral`] for a route other than a `mistral` API
/// route, [`MistralBodyError::UnresolvedBlob`] for a blob part,
/// [`MistralBodyError::ForeignThinking`] for an Anthropic thinking
/// fragment, and [`MistralBodyError::Encode`] when the writer fails.
pub(crate) fn request_body(
    request: &ModelRequest,
    thinking: &ThinkingPlan,
) -> Result<Vec<u8>, MistralBodyError> {
    let ModelRoute::Api {
        family: Family::Mistral,
        model,
    } = &request.model
    else {
        return Err(MistralBodyError::NotMistral {
            route: request.model.id().into(),
        });
    };
    let WireThinking::OpenAi { effort } = thinking.wire else {
        return Err(MistralBodyError::ForeignThinking);
    };
    let has_tools = !request.tools.is_empty();
    let reasoning_effort = effort.and_then(|effort| match effort {
        "none" => Some("none"),
        "minimal" | "low" => Some("low"),
        "medium" => Some("medium"),
        "high" | "xhigh" => Some("high"),
        "max" => Some("max"),
        _ => None,
    });
    let completion_args = (thinking.temperature.is_some()
        || request.params.max_output_tokens.is_some()
        || has_tools
        || reasoning_effort.is_some())
    .then(|| CompletionArgs {
        temperature: thinking.temperature,
        max_tokens: request.params.max_output_tokens,
        tool_choice: has_tools.then_some("auto"),
        reasoning_effort,
    });
    // Reasoning replays only while it is on: `reasoning_effort: none` is the
    // thinking-off wire form and a request with no effort sends no reasoning
    // control, so neither takes a prior `thinking` chunk back.
    let history = if reasoning_effort.is_some_and(|effort| effort != "none") {
        ThinkingHistory::Replay
    } else {
        ThinkingHistory::Omit
    };
    let body = Body {
        model,
        inputs: entries(request, history)?,
        instructions: (!request.system.is_empty()).then(|| request.system.as_ref()),
        tools: request
            .tools
            .iter()
            .map(|tool| WireTool {
                kind: "function",
                function: WireToolFunction {
                    name: wire_name(&tool.name, WIRE_NAME_MAX),
                    description: &tool.description,
                    parameters: &tool.parameters,
                },
            })
            .collect(),
        stream: true,
        store: false,
        completion_args,
    };
    sonic_rs::to_vec(&body).map_err(|error| MistralBodyError::Encode {
        message: error.to_string(),
    })
}

/// Whether prior assistant reasoning goes back to the server as `thinking`
/// chunks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ThinkingHistory {
    Replay,
    Omit,
}

fn entries(
    request: &ModelRequest,
    history: ThinkingHistory,
) -> Result<Vec<Entry<'_>>, MistralBodyError> {
    let mut normalizer = CallIdNormalizer::default();
    let mut inputs: Vec<Entry<'_>> = Vec::with_capacity(request.context.len() + 1);
    // Images of the current run of tool results, sent after the run.
    let mut tool_images: Vec<Chunk<'_>> = Vec::new();
    for item in request.context.iter() {
        if !matches!(item, ContextItem::ToolResult { .. }) {
            flush_tool_images(&mut inputs, &mut tool_images);
        }
        match item {
            ContextItem::User { parts } => {
                let chunks = user_chunks(parts)?;
                if !chunks.is_empty() {
                    push_user(&mut inputs, chunks);
                }
            }
            ContextItem::Assistant { parts, .. } => {
                append_assistant(&mut inputs, parts, &mut normalizer, history);
            }
            ContextItem::ToolResult { call, parts, .. } => {
                let (text, mut images) = split_tool_result(parts)?;
                let wire = normalizer.normalize(call.as_str());
                inputs.push(Entry::FunctionResult {
                    tool_call_id: wire.clone(),
                    result: text,
                });
                if !images.is_empty() {
                    tool_images.push(Chunk::Text {
                        text: Cow::Owned(format!("Images from tool call {wire}.")),
                    });
                    tool_images.append(&mut images);
                }
            }
        }
    }
    flush_tool_images(&mut inputs, &mut tool_images);
    Ok(inputs)
}

/// Appends a user entry, coalescing with a preceding user entry so the
/// request never carries two user entries in a row.
fn push_user<'a>(inputs: &mut Vec<Entry<'a>>, chunks: Vec<Chunk<'a>>) {
    if let Some(Entry::MessageInput {
        role: Role::User,
        content,
    }) = inputs.last_mut()
    {
        content.extend(chunks);
    } else {
        inputs.push(Entry::MessageInput {
            role: Role::User,
            content: chunks,
        });
    }
}

fn flush_tool_images<'a>(inputs: &mut Vec<Entry<'a>>, images: &mut Vec<Chunk<'a>>) {
    if !images.is_empty() {
        push_user(inputs, mem::take(images));
    }
}

fn user_chunks(parts: &[Part]) -> Result<Vec<Chunk<'_>>, MistralBodyError> {
    let mut chunks = Vec::new();
    for part in parts {
        match part {
            Part::Text { text } => chunks.push(Chunk::Text {
                text: Cow::Borrowed(text),
            }),
            Part::Image { mime, bytes } => chunks.push(Chunk::ImageUrl {
                image_url: format!("data:{mime};base64,{}", STANDARD.encode(bytes)),
            }),
            Part::Blob { blob_id, .. } => {
                return Err(MistralBodyError::UnresolvedBlob { blob_id: *blob_id });
            }
        }
    }
    Ok(chunks)
}

/// Splits tool-result parts into the LF-joined text and the image chunks.
fn split_tool_result(parts: &[Part]) -> Result<(String, Vec<Chunk<'_>>), MistralBodyError> {
    let mut texts = Vec::new();
    let mut images = Vec::new();
    for part in parts {
        match part {
            Part::Text { text } => texts.push(text.to_string()),
            Part::Image { mime, bytes } => images.push(Chunk::ImageUrl {
                image_url: format!("data:{mime};base64,{}", STANDARD.encode(bytes)),
            }),
            Part::Blob { blob_id, .. } => {
                return Err(MistralBodyError::UnresolvedBlob { blob_id: *blob_id });
            }
        }
    }
    Ok((texts.join("\n"), images))
}

fn append_assistant<'a>(
    inputs: &mut Vec<Entry<'a>>,
    parts: &'a [AssistantPart],
    normalizer: &mut CallIdNormalizer,
    history: ThinkingHistory,
) {
    let mut content: Vec<Chunk<'_>> = Vec::new();
    for part in parts {
        if history == ThinkingHistory::Replay
            && let AssistantPart::Thinking { text, .. } = part
            && !text.is_empty()
        {
            content.push(Chunk::Thinking {
                thinking: vec![ThinkingText::Text {
                    text: Cow::Borrowed(text),
                }],
            });
        }
    }
    let texts: Vec<&str> = parts
        .iter()
        .filter_map(|part| match part {
            AssistantPart::Text { text, .. } => Some(text.as_ref()),
            _ => None,
        })
        .collect();
    if !texts.is_empty() {
        content.push(Chunk::Text {
            text: Cow::Owned(texts.join("\n")),
        });
    }
    let mut calls = Vec::new();
    for part in parts {
        if let AssistantPart::ToolCall {
            call, name, args, ..
        } = part
        {
            let id = normalizer.normalize(call.as_str());
            let entry = Entry::FunctionCall {
                tool_call_id: id,
                name: wire_name(name, WIRE_NAME_MAX),
                arguments: args,
            };
            calls.push(entry);
        }
    }
    if content.is_empty() && calls.is_empty() {
        return;
    }
    if !content.is_empty() {
        inputs.push(Entry::MessageOutput {
            role: Role::Assistant,
            content,
        });
    }
    inputs.extend(calls);
}

/// Projects one internal call id into the nine-character Mistral wire
/// alphabet, following the pinned normalizer: an id that already is nine
/// alphanumeric characters passes through, anything else hashes (SHA-256
/// hex here, the pinned `shortHash` there) with a counter suffix on
/// collision. Deterministic within one request build.
#[derive(Default)]
struct CallIdNormalizer {
    known: HashMap<Box<str>, Box<str>>,
    taken: HashSet<Box<str>>,
}

impl CallIdNormalizer {
    fn normalize(&mut self, id: &str) -> String {
        if let Some(wire) = self.known.get(id) {
            return wire.to_string();
        }
        let mut attempt = 0_usize;
        loop {
            let candidate = derive_call_id(id, attempt);
            if self.taken.insert(candidate.clone().into_boxed_str()) {
                self.known
                    .insert(id.into(), candidate.clone().into_boxed_str());
                return candidate;
            }
            attempt += 1;
        }
    }
}

fn derive_call_id(id: &str, attempt: usize) -> String {
    let alnum: String = id.chars().filter(char::is_ascii_alphanumeric).collect();
    if attempt == 0 && alnum.chars().count() == TOOL_CALL_ID_LENGTH {
        return alnum;
    }
    let base = if alnum.is_empty() {
        id.to_owned()
    } else {
        alnum
    };
    let seed = if attempt == 0 {
        base
    } else {
        format!("{base}:{attempt}")
    };
    let digest = Sha256::digest(seed.as_bytes());
    let mut wire = String::with_capacity(TOOL_CALL_ID_LENGTH);
    for byte in &digest {
        for digit in format!("{byte:02x}").chars() {
            wire.push(digit);
            if wire.len() == TOOL_CALL_ID_LENGTH {
                return wire;
            }
        }
    }
    wire
}

#[derive(Deserialize)]
struct Started {
    conversation_id: Option<String>,
}

#[derive(Deserialize)]
struct OutputDelta {
    content: Option<DeltaContent>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum DeltaContent {
    Text(String),
    Chunks(Vec<DeltaChunk>),
}

#[derive(Deserialize)]
struct DeltaChunk {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<String>,
    thinking: Option<Vec<ThinkPart>>,
}

#[derive(Deserialize)]
struct ThinkPart {
    text: Option<String>,
}

#[derive(Deserialize)]
struct FunctionDelta {
    output_index: Option<u64>,
    name: Option<String>,
    tool_call_id: Option<String>,
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct Done {
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct WireUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
}

/// Whether the decoder can take more input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Progress {
    /// The response is still open.
    Open,
    /// The terminal was emitted, or an error was returned; later input is
    /// ignored.
    Ended,
}

#[derive(Debug)]
struct OpenCall {
    id: String,
    name: String,
    args: Vec<u8>,
}

/// The incremental decoder of one conversations response.
///
/// Feed it the data of every SSE event in order with
/// [`MistralDecoder::feed`], then call [`MistralDecoder::end`] at end of
/// input. It emits at most one terminal: `ToolCallsDone`, `Usage`, and
/// `Stop` together, or one error. Once ended, it ignores all further input.
#[derive(Debug)]
pub(crate) struct MistralDecoder {
    provider: Box<str>,
    model: Box<str>,
    budget: StreamBudget,
    /// Open calls by slot (`output_index`, or the resolved slot of a delta
    /// without one); iteration order is index order.
    calls: BTreeMap<u64, OpenCall>,
    /// The slot of the call that last spoke on the stream; the resume
    /// target of a call delta without an index or an id.
    spoken: Option<u64>,
    /// The last non-null usage.
    usage: Option<Usage>,
    /// The conversation id the server assigned to this response.
    conversation_id: Option<String>,
    /// Some event was emitted.
    emitted: bool,
    ended: bool,
}

impl MistralDecoder {
    /// A decoder for one response of `model` on the provider id `provider`;
    /// both only name the request in mapped in-stream errors.
    #[must_use]
    pub(crate) fn new(provider: &str, model: &str) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            budget: StreamBudget::default(),
            calls: BTreeMap::new(),
            spoken: None,
            usage: None,
            conversation_id: None,
            emitted: false,
            ended: false,
        }
    }

    /// Decodes the data of one SSE event, appending events to `out`.
    ///
    /// `[DONE]` finishes the response. An event dispatches on its JSON
    /// `type`: `conversation.response.started` records the conversation id,
    /// `message.output.delta` content becomes text and reasoning deltas,
    /// `function.call.delta` opens its `output_index` (or the slot the
    /// unkeyed rule resolves for a delta without one) with its
    /// `tool_call_id` and name (later ones for an open index are ignored)
    /// and appends its argument fragments as raw bytes, and
    /// `conversation.response.done` finishes with its usage. Unknown event
    /// types and connector or handoff events are ignored.
    ///
    /// # Errors
    ///
    /// [`ProviderError::Protocol`] for data that is not a JSON object or a
    /// call delta that opens an index without a `tool_call_id` and a name;
    /// the mapped server error for a `conversation.response.error` event.
    /// The decoder is ended after an error.
    pub(crate) fn feed(
        &mut self,
        data: &str,
        out: &mut Vec<StreamEvent>,
    ) -> Result<Progress, ProviderError> {
        if self.ended {
            return Ok(Progress::Ended);
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

    /// Ends the input: a response without a done event is a cut; an ended
    /// decoder emits nothing.
    ///
    /// # Errors
    ///
    /// [`ProviderError::StreamCut`] when no done event arrived.
    pub(crate) fn end(&mut self, _out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        if mem::replace(&mut self.ended, true) {
            return Ok(());
        }
        Err(ProviderError::StreamCut)
    }

    fn step(&mut self, data: &str, out: &mut Vec<StreamEvent>) -> Result<Progress, ProviderError> {
        if data.trim() == "[DONE]" {
            self.finish(out);
            return Ok(Progress::Ended);
        }
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
            Some("conversation.response.started") => {
                let started: Started = parse(data, "conversation.response.started")?;
                if self.conversation_id.is_none() {
                    self.conversation_id = started.conversation_id;
                }
            }
            Some("message.output.delta") => {
                let delta: OutputDelta = parse(data, "message.output.delta")?;
                Self::output_delta(delta, out);
            }
            Some("function.call.delta") => {
                let delta: FunctionDelta = parse(data, "function.call.delta")?;
                self.function_delta(delta, out)?;
            }
            Some("conversation.response.done") => {
                let event: Done = parse(data, "conversation.response.done")?;
                if let Some(usage) = event.usage.as_ref().map(normalize) {
                    self.usage = Some(usage);
                }
                self.finish(out);
                return Ok(Progress::Ended);
            }
            Some("conversation.response.error") => {
                let event: ErrorEvent = parse(data, "conversation.response.error")?;
                return Err(self.server_error(event));
            }
            // `tool.execution.*`, `agent.handoff.*`, and anything else the
            // API adds arrive on agent runs dalgon never starts; ignored.
            _ => {}
        }
        Ok(Progress::Open)
    }

    fn output_delta(delta: OutputDelta, out: &mut Vec<StreamEvent>) {
        let non_empty = |text: &String| !text.is_empty();
        match delta.content {
            Some(DeltaContent::Text(text)) => {
                if !text.is_empty() {
                    out.push(StreamEvent::TextDelta { text });
                }
            }
            Some(DeltaContent::Chunks(chunks)) => {
                for chunk in chunks {
                    match chunk.kind.as_deref() {
                        Some("text") => {
                            if let Some(text) = chunk.text.filter(non_empty) {
                                out.push(StreamEvent::TextDelta { text });
                            }
                        }
                        Some("thinking") => {
                            let joined = chunk
                                .thinking
                                .into_iter()
                                .flatten()
                                .filter_map(|part| part.text)
                                .filter(|text| !text.is_empty())
                                .collect::<String>();
                            if !joined.is_empty() {
                                out.push(StreamEvent::ReasoningDelta { text: joined });
                            }
                        }
                        _ => {}
                    }
                }
            }
            None => {}
        }
    }

    fn function_delta(
        &mut self,
        delta: FunctionDelta,
        out: &mut Vec<StreamEvent>,
    ) -> Result<(), ProviderError> {
        let index = match delta.output_index {
            Some(index) => index,
            None => self.unkeyed_index(delta.tool_call_id.as_deref()),
        };
        self.spoken = Some(index);
        let call = match self.calls.entry(index) {
            MapEntry::Occupied(open) => open.into_mut(),
            MapEntry::Vacant(slot) => {
                self.budget.admit_item()?;
                let id = delta.tool_call_id.filter(|id| !id.is_empty());
                let name = delta.name.filter(|name| !name.is_empty());
                let (Some(id), Some(name)) = (id, name) else {
                    return Err(protocol(format!(
                        "function call output {index} opened without a tool_call_id and a name"
                    )));
                };
                out.push(StreamEvent::ToolCallStarted {
                    id: id.clone(),
                    name: name.clone(),
                });
                slot.insert(OpenCall {
                    id,
                    name,
                    args: Vec::new(),
                })
            }
        };
        if let Some(fragment) = delta.arguments.filter(|fragment| !fragment.is_empty()) {
            call.args.extend_from_slice(fragment.as_bytes());
            out.push(StreamEvent::ToolArgsDelta {
                id: call.id.clone(),
                fragment: fragment.into_bytes(),
            });
        }
        Ok(())
    }

    /// Resolves the slot of a call delta that arrived without an
    /// `output_index`: a known id resumes its own call wherever it was
    /// opened, an unseen id opens the slot after the highest open one, and
    /// a delta with neither continues the call that last spoke; with
    /// nothing open that is slot 0, where opening refuses a keyless delta
    /// exactly as it does for a keyed one. This mirrors the chat decoder's
    /// `unkeyed_index` rule; the chat slot type is private to its module,
    /// so the rule is restated here rather than shared.
    fn unkeyed_index(&self, id: Option<&str>) -> u64 {
        let id = id.filter(|id| !id.is_empty());
        if let Some(id) = id
            && let Some((index, _)) = self.calls.iter().find(|(_, call)| call.id.as_str() == id)
        {
            return *index;
        }
        if id.is_some() {
            return self.calls.keys().next_back().map_or(0, |last| *last + 1);
        }
        self.spoken.unwrap_or(0)
    }

    /// Emits the terminal group. Calls close in `output_index` order; empty
    /// argument text is `{}`. The stop reason is derived: calls give
    /// `ToolUse`, otherwise `EndTurn`.
    fn finish(&mut self, out: &mut Vec<StreamEvent>) {
        let has_calls = !self.calls.is_empty();
        let calls = mem::take(&mut self.calls)
            .into_values()
            .map(|call| ToolCall {
                args: if call.args.is_empty() {
                    ToolArgs::from_bytes(b"{}")
                } else {
                    ToolArgs::from_bytes(&call.args)
                },
                id: call.id,
                name: call.name,
            })
            .collect();
        out.push(StreamEvent::ToolCallsDone { calls });
        out.push(StreamEvent::Usage {
            usage: self.usage.take().unwrap_or(NO_USAGE),
        });
        out.push(StreamEvent::Stop {
            reason: if has_calls {
                StopReason::ToolUse
            } else {
                StopReason::EndTurn
            },
        });
    }

    /// Maps a `conversation.response.error` event to the error its HTTP twin
    /// would give: a numeric code is an HTTP status and a string code keeps
    /// its spelling for the retry table. An event without a usable code is
    /// still a server error, never a transport failure: it ends as the
    /// status of the stream response that carried it. The decoder returns
    /// only the error value; the request lifecycle owns retries.
    fn server_error(&self, event: ErrorEvent) -> ProviderError {
        let ErrorEvent { message, code } = event;
        let message = message.unwrap_or_default();
        let status = code
            .as_ref()
            .and_then(JsonValueTrait::as_u64)
            .and_then(|status| u16::try_from(status).ok());
        let code = code.as_ref().and_then(JsonValueTrait::as_str);
        if status.is_none() && code.is_none() {
            return ProviderError::Status {
                family: Family::Mistral,
                status: IN_STREAM_STATUS,
                message,
            };
        }
        let request = RequestState {
            family: Family::Mistral,
            provider: &self.provider,
            model: &self.model,
            oauth: false,
            refreshed: false,
            delivered: self.emitted,
        };
        match classify(status, code, &message, &request) {
            RetryDecision::Retry(error) | RetryDecision::Fail(error) => error,
            // Unreachable without an HTTP status; kept total.
            RetryDecision::RefreshOnce => ProviderError::SignInExpired {
                provider: String::from(request.provider),
            },
        }
    }
}

/// One `conversation.response.error` event: a message and a numeric or string code.
#[derive(Deserialize)]
struct ErrorEvent {
    message: Option<String>,
    code: Option<sonic_rs::Value>,
}

fn parse<'a, T: Deserialize<'a>>(data: &'a str, kind: &str) -> Result<T, ProviderError> {
    sonic_rs::from_str(data).map_err(|error| protocol(format!("malformed {kind} event: {error}")))
}

/// Maps conversations usage onto the core counters.
fn normalize(usage: &WireUsage) -> Usage {
    Usage {
        input_tokens: usage.prompt_tokens.unwrap_or(0),
        cached_input_tokens: 0,
        output_tokens: usage.completion_tokens.unwrap_or(0),
        reasoning_tokens: None,
        cache_write_tokens: 0,
        premium_requests: None,
        cost_usd: None,
    }
}

fn protocol(detail: String) -> ProviderError {
    ProviderError::Protocol {
        family: Family::Mistral,
        detail,
    }
}

/// Decodes one conversations SSE event stream into neutral events.
///
/// Event names are ignored; each event's data goes to `decoder`. An SSE
/// error ends the stream with that error, and end of input ends it through
/// [`MistralDecoder::end`]. Nothing is read after the terminal.
pub(crate) fn decode_events<S>(
    events: S,
    decoder: MistralDecoder,
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
