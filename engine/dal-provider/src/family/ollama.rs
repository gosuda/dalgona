//! The native Ollama family (`api/chat` NDJSON and `api/tags`): request
//! lowering and record decoding.
//!
//! [`request_body`] lowers one [`ModelRequest`] and its composed
//! [`ThinkingPlan`] to the exact `POST <base>/api/chat` body. Ollama is not an
//! OpenAI-compatible surface: message content is one plain string plus a
//! separate `images` array, assistant history replays tool calls as
//! `{type,function:{name,arguments}}` with no call ids, reasoning travels as
//! the string `think` member, and sampling lives under `options`. The member
//! order is fixed and two builds of the same request are equal byte for byte.
//! Tool schemas and tool-call arguments are written from their [`RawJson`]
//! text, never re-encoded. Wire sources: the pinned oh-my-pi reference
//! (`packages/ai/src/providers/ollama.ts` at `a4dc3302`), read on disk.
//!
//! Two behaviors depend on which deployment answers, not on the request, and
//! arrive as a [`WirePolicy`]: Ollama Cloud rejects `num_predict` above
//! 65,536 and rejects assistant `thinking` in replayed history for every
//! model but GLM 5.2, while a self-hosted server takes both. The [`show`]
//! module holds the `api/show` capability probe that discovery joins to
//! `api/tags`.
//!
//! [`OllamaDecoder`] turns the data of one NDJSON record line into neutral
//! [`StreamEvent`]s with the grammar of [`crate::stream`]: deltas as they
//! arrive, then one `ToolCallsDone`, one `Usage`, and one `Stop` at the
//! `done` record, or one error instead. The stream carries no SSE framing;
//! [`decode_stream`] splits raw bytes into lines itself. Every call id is
//! synthesized as `ollama:<ordinal>:<name>` because Ollama provides none.
//! `done_reason` maps per the pinned behavior: `length` to `MaxTokens`,
//! `tool_calls` to `ToolUse`, `stop` to `EndTurn`, a bare `load` (model
//! loaded, nothing generated) to a `Status` error, a missing reason with
//! calls to `ToolUse`, and a `length` response without any visible content
//! to a `Status` error naming the context window. Usage maps
//! `prompt_eval_count` (total prompt tokens) to `input_tokens`,
//! `prompt_eval_cached_count` to `cached_input_tokens`, and `eval_count` to
//! `output_tokens`. Decoding is lenient: unknown members and
//! blank lines are ignored; requests dalgon builds stay strict.
//!
//! Cancellation matches the family pattern: the decoder is pure, the
//! terminal is emitted exactly once, input after it is ignored, end of input
//! before a `done` record is [`ProviderError::StreamCut`], and dropping the
//! stream drops the byte source, which cancels the underlying transfer.

use std::{borrow::Cow, mem};

use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::BytesMut;
use dal_core::{
    AssistantPart, BlobId, ContextItem, Family, ModelRequest, ModelRoute, Part, RawJson, Usage,
};
use futures::{Stream, StreamExt, stream};
use serde::{Deserialize, Serialize};
use sonic_rs::{JsonValueTrait, LazyValue};

use crate::{
    error::ProviderError,
    sse::SSE_LINE_LIMIT,
    stream::{StopReason, StreamEvent, ToolArgs, ToolCall},
    stream_budget::StreamBudget,
    thinking::{ThinkingPlan, ThinkingSupport, WireThinking},
    tool_names::{WIRE_NAME_MAX, wire_name},
};

#[cfg(test)]
pub(crate) mod live_fixtures;
mod show;

#[cfg(test)]
pub(crate) use show::BOOLEAN_ON;

pub(crate) use show::{
    CLOUD_NUM_PREDICT_CAP, Deployment, OllamaShow, SHOW_PATH, cloud_output_capped, decode_show,
    is_boolean_ladder, show_body, thinking_ladder,
};

/// The endpoint path of the family, relative to the provider base URL.
pub(crate) const ENDPOINT_PATH: &str = "api/chat";

/// The model-discovery endpoint path, relative to the provider base URL.
pub(crate) const TAGS_PATH: &str = "api/tags";

/// The deployment-dependent parts of a request body.
///
/// `replay_thinking` writes assistant reasoning back into the history as
/// `thinking`; `num_predict_cap` bounds `options.num_predict`, and `None`
/// leaves the caller's limit alone; `boolean_think` spells any reasoning level
/// as `think: true`, for a model whose server lists no levels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct WirePolicy {
    pub(crate) replay_thinking: bool,
    pub(crate) num_predict_cap: Option<u32>,
    pub(crate) boolean_think: bool,
}

impl WirePolicy {
    /// A local or self-hosted server: reasoning replays, no output ceiling,
    /// named levels.
    pub(crate) const SELF_HOSTED: Self = Self {
        replay_thinking: true,
        num_predict_cap: None,
        boolean_think: false,
    };

    /// The policy of `deployment` for `model`, whose catalog row carries
    /// `support`: the cloud caps `num_predict` at [`CLOUD_NUM_PREDICT_CAP`]
    /// and replays reasoning only for the model verified to accept it, and a
    /// boolean-only ladder spells reasoning `true`.
    #[must_use]
    pub(crate) fn for_model(
        deployment: Deployment,
        model: &str,
        support: &ThinkingSupport,
    ) -> Self {
        let boolean_think = is_boolean_ladder(support);
        match deployment {
            Deployment::SelfHosted => Self {
                boolean_think,
                ..Self::SELF_HOSTED
            },
            Deployment::Cloud => Self {
                replay_thinking: show::cloud_replays_thinking(model),
                num_predict_cap: Some(CLOUD_NUM_PREDICT_CAP),
                boolean_think,
            },
        }
    }
}

/// The HTTP status carried by an in-stream Ollama error.
const IN_STREAM_STATUS: u16 = 200;

/// The pinned message for a `length` response that generated nothing.
const EMPTY_OLLAMA_LENGTH_MESSAGE: &str = "Model returned no content: prompt filled the context window; raise Ollama num_ctx or shorten the prompt.";

/// The pinned message for a `load` response that generated nothing.
const EMPTY_OLLAMA_LOAD_MESSAGE: &str = "Ollama loaded the model but generated nothing (done_reason: load): the request contained no user-role message.";

/// A request that cannot be lowered to an Ollama body.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum OllamaBodyError {
    /// The request route is not an `ollama` model route.
    #[error("an ollama request needs an ollama model route, not `{route}`")]
    NotOllama {
        /// The route id of the request.
        route: Box<str>,
    },
    /// A content part still refers to stored content; the caller resolves
    /// blobs to inline bytes before lowering.
    #[error("content blob {blob_id} must be read into the request before an ollama body is built")]
    UnresolvedBlob {
        /// The unresolved blob.
        blob_id: BlobId,
    },
    /// The thinking plan was composed for the Anthropic family.
    #[error("the thinking plan carries an Anthropic fragment, not an ollama think level")]
    ForeignThinking,
    /// The JSON writer failed.
    #[error("ollama body encoding failed: {message}")]
    Encode {
        /// The writer message.
        message: String,
    },
}

#[derive(Serialize)]
struct Body<'a> {
    model: &'a str,
    messages: Vec<Message<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<WireTool<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    think: Option<OllamaThink>,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<Options>,
    stream: bool,
}

/// The `think` member: a boolean, or a named effort.
#[derive(Serialize)]
#[serde(untagged)]
enum OllamaThink {
    /// `think: false` turns reasoning off; `think: true` turns it on for a
    /// model without levels.
    Switch(bool),
    /// A named effort: `low`, `medium`, `high`, `xhigh`, or `max`.
    Effort(&'static str),
}

#[derive(Serialize)]
struct Options {
    #[serde(skip_serializing_if = "Option::is_none")]
    num_predict: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Serialize)]
#[serde(tag = "role", rename_all = "lowercase")]
enum Message<'a> {
    System {
        content: String,
    },
    User {
        content: String,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        images: Vec<String>,
    },
    Assistant {
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        thinking: Option<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<WireCall<'a>>,
    },
    Tool {
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_name: Option<&'a str>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        images: Vec<String>,
    },
}

#[derive(Serialize)]
struct WireCall<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    function: WireCallFunction<'a>,
}

#[derive(Serialize)]
struct WireCallFunction<'a> {
    name: Cow<'a, str>,
    arguments: &'a RawJson,
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

/// Strips the trailing `/` and `/api` of a configured Ollama base URL.
///
/// The pinned client accepts bases spelled with or without the `/api`
/// suffix; `http::endpoint` joins paths to the base, so the suffix must go
/// before joining or every request doubles the segment.
#[must_use]
pub(crate) fn trim_api_suffix(base: &str) -> &str {
    let trimmed = base.trim_end_matches('/');
    trimmed.strip_suffix("/api").unwrap_or(trimmed)
}

/// Lowers `request` to the JSON body of `POST <base>/api/chat`.
///
/// `thinking` is the plan composed from `request.params`, the hooks, and the
/// model row. The shared [`WireThinking::OpenAi`] fragment is the only
/// non-Anthropic fragment the planner produces; its effort spells Ollama's
/// `think` member: `none` becomes `false`, `minimal` and `low` become
/// `"low"`, `xhigh` stays `"xhigh"` (the planner only produces it for a model
/// whose server lists it), and unknown spellings omit the member; under
/// `policy.boolean_think` every reasoning level becomes `true`.
/// `request.params.max_output_tokens` becomes `options.num_predict`, held to
/// `policy.num_predict_cap`, and the plan temperature `options.temperature`,
/// each omitted when absent. The
/// system text is a leading system message unless empty. User messages carry
/// their text parts LF-joined as `content` and their images as bare base64
/// in `images`. Assistant messages carry their text, their reasoning text,
/// and their calls with raw argument text; one with none of the three is
/// omitted; its reasoning is written only when `policy.replay_thinking` is set,
/// so a reasoning-only message under a policy that drops reasoning is omitted
/// too. A tool result becomes one `tool` message with `tool_name` and
/// its own `images`. Without tools, `tools` is omitted.
///
/// # Errors
///
/// [`OllamaBodyError::NotOllama`] for a route other than an `ollama` API
/// route, [`OllamaBodyError::UnresolvedBlob`] for a blob part,
/// [`OllamaBodyError::ForeignThinking`] for an Anthropic thinking fragment,
/// and [`OllamaBodyError::Encode`] when the writer fails.
pub(crate) fn request_body(
    request: &ModelRequest,
    thinking: &ThinkingPlan,
    policy: WirePolicy,
) -> Result<Vec<u8>, OllamaBodyError> {
    let ModelRoute::Api {
        family: Family::Ollama,
        model,
        ..
    } = &request.model
    else {
        return Err(OllamaBodyError::NotOllama {
            route: request.model.id().into(),
        });
    };
    let WireThinking::OpenAi { effort } = thinking.wire else {
        return Err(OllamaBodyError::ForeignThinking);
    };
    let think = effort.and_then(|effort| match effort {
        "none" => Some(OllamaThink::Switch(false)),
        "minimal" | "low" | "medium" | "high" | "xhigh" | "max" if policy.boolean_think => {
            Some(OllamaThink::Switch(true))
        }
        "minimal" | "low" => Some(OllamaThink::Effort("low")),
        "medium" => Some(OllamaThink::Effort("medium")),
        "high" => Some(OllamaThink::Effort("high")),
        "xhigh" => Some(OllamaThink::Effort("xhigh")),
        "max" => Some(OllamaThink::Effort("max")),
        _ => None,
    });
    let num_predict = request
        .params
        .max_output_tokens
        .map(|limit| policy.num_predict_cap.map_or(limit, |cap| limit.min(cap)));
    let options = (num_predict.is_some() || thinking.temperature.is_some()).then_some(Options {
        num_predict,
        temperature: thinking.temperature,
    });
    let body = Body {
        model,
        messages: messages(request, policy)?,
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
        think,
        options,
        stream: true,
    };
    sonic_rs::to_vec(&body).map_err(|error| OllamaBodyError::Encode {
        message: error.to_string(),
    })
}

fn messages(
    request: &ModelRequest,
    policy: WirePolicy,
) -> Result<Vec<Message<'_>>, OllamaBodyError> {
    let mut messages = Vec::with_capacity(request.context.len() + 1);
    if !request.system.is_empty() {
        messages.push(Message::System {
            content: request.system.to_string(),
        });
    }
    for item in request.context.iter() {
        match item {
            ContextItem::User { parts } => {
                let (content, images) = plain_parts(parts)?;
                messages.push(Message::User { content, images });
            }
            ContextItem::Assistant { parts, .. } => {
                if let Some(message) = assistant(parts, policy) {
                    messages.push(message);
                }
            }
            ContextItem::ToolResult { name, parts, .. } => {
                let (content, images) = plain_parts(parts)?;
                messages.push(Message::Tool {
                    content,
                    tool_name: Some(name),
                    images,
                });
            }
        }
    }
    Ok(messages)
}

/// Splits content parts into the LF-joined text and the bare base64 images.
fn plain_parts(parts: &[Part]) -> Result<(String, Vec<String>), OllamaBodyError> {
    let mut texts = Vec::new();
    let mut images = Vec::new();
    for part in parts {
        match part {
            Part::Text { text } => texts.push(text.to_string()),
            Part::Image { bytes, .. } => images.push(STANDARD.encode(bytes)),
            Part::Blob { blob_id, .. } => {
                return Err(OllamaBodyError::UnresolvedBlob { blob_id: *blob_id });
            }
        }
    }
    Ok((texts.join("\n"), images))
}

fn assistant(parts: &[AssistantPart], policy: WirePolicy) -> Option<Message<'_>> {
    let mut texts: Vec<&str> = Vec::new();
    let mut thinking: Vec<&str> = Vec::new();
    let mut tool_calls = Vec::new();
    for part in parts {
        match part {
            AssistantPart::Text { text, .. } => texts.push(text),
            AssistantPart::Thinking { text, .. } if policy.replay_thinking => thinking.push(text),
            AssistantPart::Thinking { .. } => {}
            AssistantPart::ToolCall { name, args, .. } => tool_calls.push(WireCall {
                kind: "function",
                function: WireCallFunction {
                    name: wire_name(name, WIRE_NAME_MAX),
                    arguments: args,
                },
            }),
        }
    }
    if texts.is_empty() && thinking.is_empty() && tool_calls.is_empty() {
        return None;
    }
    Some(Message::Assistant {
        content: texts.join("\n"),
        thinking: (!thinking.is_empty()).then(|| thinking.join("\n")),
        tool_calls,
    })
}

#[derive(Deserialize)]
struct Record<'a> {
    #[serde(borrow)]
    message: Option<RecordMessage<'a>>,
    done: Option<bool>,
    done_reason: Option<String>,
    prompt_eval_count: Option<u64>,
    prompt_eval_cached_count: Option<u64>,
    eval_count: Option<u64>,
    error: Option<sonic_rs::Value>,
}

#[derive(Deserialize)]
struct RecordMessage<'a> {
    content: Option<String>,
    thinking: Option<String>,
    #[serde(borrow)]
    tool_calls: Option<Vec<RecordCall<'a>>>,
}

#[derive(Deserialize)]
struct RecordCall<'a> {
    #[serde(borrow)]
    function: Option<RecordFunction<'a>>,
}

#[derive(Deserialize)]
struct RecordFunction<'a> {
    name: Option<String>,
    #[serde(borrow)]
    arguments: Option<LazyValue<'a>>,
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

/// The incremental decoder of one Ollama chat response.
///
/// Feed it every NDJSON record line in order with [`OllamaDecoder::feed`],
/// then call [`OllamaDecoder::end`] at end of input. It emits at most one
/// terminal: `ToolCallsDone`, `Usage`, and `Stop` together, or one error.
/// Once ended, it ignores all further input.
#[derive(Debug, Default)]
pub(crate) struct OllamaDecoder {
    budget: StreamBudget,
    calls: Vec<OpenCall>,
    /// Whether any text, reasoning, or call was emitted.
    visible: bool,
    ended: bool,
}

impl OllamaDecoder {
    /// A decoder for one chat response.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Decodes one NDJSON record line, appending events to `out`.
    ///
    /// Blank lines are ignored. A record's `thinking` and `content` become
    /// reasoning and text deltas, its `tool_calls` open complete calls with
    /// synthesized ids, and a record with `done: true` finishes the response
    /// with its usage. A `done_reason` of `load`, or `length` without any
    /// visible content, is an error instead of a terminal.
    ///
    /// # Errors
    ///
    /// [`ProviderError::Protocol`] for data that is not a JSON record;
    /// [`ProviderError::Status`] for the pinned empty-completion guards and
    /// for a record carrying `error`. The decoder is ended after an error.
    pub(crate) fn feed(
        &mut self,
        data: &str,
        out: &mut Vec<StreamEvent>,
    ) -> Result<Progress, ProviderError> {
        if self.ended {
            return Ok(Progress::Ended);
        }
        if data.trim().is_empty() {
            return Ok(Progress::Open);
        }
        let result = self
            .budget
            .admit_event(data.len())
            .and_then(|()| self.step(data, out));
        if !matches!(result, Ok(Progress::Open)) {
            self.ended = true;
        }
        result
    }

    /// Ends the input: a response without a `done` record is a cut; an ended
    /// decoder emits nothing.
    ///
    /// # Errors
    ///
    /// [`ProviderError::StreamCut`] when no `done` record arrived.
    pub(crate) fn end(&mut self) -> Result<(), ProviderError> {
        if mem::replace(&mut self.ended, true) {
            return Ok(());
        }
        Err(ProviderError::StreamCut)
    }

    fn step(&mut self, data: &str, out: &mut Vec<StreamEvent>) -> Result<Progress, ProviderError> {
        let record: Record<'_> = sonic_rs::from_str(data)
            .map_err(|error| protocol(format!("ollama record is not valid JSON: {error}")))?;
        if let Some(error) = record.error {
            return Err(status(
                error
                    .as_str()
                    .unwrap_or("ollama returned an in-stream error"),
            ));
        }
        if let Some(message) = record.message {
            if let Some(thinking) = message.thinking.filter(|text| !text.is_empty()) {
                self.visible = true;
                out.push(StreamEvent::ReasoningDelta { text: thinking });
            }
            if let Some(content) = message.content.filter(|text| !text.is_empty()) {
                self.visible = true;
                out.push(StreamEvent::TextDelta { text: content });
            }
            if let Some(calls) = message.tool_calls {
                for call in &calls {
                    self.tool_call(call, out)?;
                }
            }
        }
        if !record.done.unwrap_or(false) {
            return Ok(Progress::Open);
        }
        let done_reason = record.done_reason.unwrap_or_default();
        if done_reason == "load" {
            return Err(status(EMPTY_OLLAMA_LOAD_MESSAGE));
        }
        if done_reason == "length" && !self.visible {
            return Err(status(EMPTY_OLLAMA_LENGTH_MESSAGE));
        }
        let total = record.prompt_eval_count.unwrap_or(0);
        let usage = Usage {
            input_tokens: total,
            cached_input_tokens: record.prompt_eval_cached_count.unwrap_or(0).min(total),
            output_tokens: record.eval_count.unwrap_or(0),
            reasoning_tokens: None,
            cache_write_tokens: 0,
            premium_requests: None,
            cost_usd: None,
        };
        self.finish(&done_reason, usage, out);
        Ok(Progress::Ended)
    }

    fn tool_call(
        &mut self,
        call: &RecordCall<'_>,
        out: &mut Vec<StreamEvent>,
    ) -> std::result::Result<(), ProviderError> {
        self.budget.admit_item()?;
        let name = call
            .function
            .as_ref()
            .and_then(|function| function.name.as_deref())
            .filter(|name| !name.is_empty())
            .unwrap_or("unknown_tool");
        let id = format!("ollama:{}:{name}", self.calls.len());
        let args = arguments_bytes(call.function.as_ref().and_then(|f| f.arguments.as_ref()));
        self.visible = true;
        out.push(StreamEvent::ToolCallStarted {
            id: id.clone(),
            name: name.to_owned(),
        });
        out.push(StreamEvent::ToolArgsDelta {
            id: id.clone(),
            fragment: args.clone(),
        });
        self.calls.push(OpenCall {
            id,
            name: name.to_owned(),
            args,
        });
        Ok(())
    }

    /// Emits the terminal group. Calls close in arrival order; empty
    /// argument text is `{}`, and every call of a `MaxTokens` response is
    /// truncated. A natural stop with calls promotes to `ToolUse`, and a
    /// missing reason with calls is `ToolUse`.
    fn finish(&mut self, done_reason: &str, usage: Usage, out: &mut Vec<StreamEvent>) {
        let has_calls = !self.calls.is_empty();
        let reason = match done_reason {
            "length" => StopReason::MaxTokens,
            "tool_calls" => StopReason::ToolUse,
            "stop" | "" => {
                if has_calls {
                    StopReason::ToolUse
                } else {
                    StopReason::EndTurn
                }
            }
            other => StopReason::Other(other.to_owned()),
        };
        let truncated = reason == StopReason::MaxTokens;
        let calls = mem::take(&mut self.calls)
            .into_iter()
            .map(|call| ToolCall {
                args: if truncated {
                    ToolArgs::Truncated
                } else if call.args.is_empty() {
                    ToolArgs::from_bytes(b"{}")
                } else {
                    ToolArgs::from_bytes(&call.args)
                },
                id: call.id,
                name: call.name,
            })
            .collect();
        out.push(StreamEvent::ToolCallsDone { calls });
        out.push(StreamEvent::Usage { usage });
        out.push(StreamEvent::Stop { reason });
    }
}

/// An in-stream failure: the stream response itself was `200`, so the error
/// ends in `Status` with that status and the server text, never retried
/// by the decoder (the request lifecycle owns every retry).
fn status(message: &str) -> ProviderError {
    ProviderError::Status {
        family: Family::Ollama,
        status: IN_STREAM_STATUS,
        message: message.to_owned(),
    }
}

/// Extracts the argument text of one tool call. An object keeps its raw
/// text; a JSON string carries the text as its content; absent or null is
/// `{}`.
fn arguments_bytes(arguments: Option<&LazyValue<'_>>) -> Vec<u8> {
    let Some(raw) = arguments else {
        return b"{}".to_vec();
    };
    let text = raw.as_raw_str().trim();
    if text.is_empty() || text == "null" {
        return b"{}".to_vec();
    }
    if text.starts_with('"') {
        return sonic_rs::from_str::<String>(text)
            .map_or_else(|_| text.as_bytes().to_vec(), String::into_bytes);
    }
    text.as_bytes().to_vec()
}

fn protocol(detail: String) -> ProviderError {
    ProviderError::Protocol {
        family: Family::Ollama,
        detail,
    }
}

/// Splits a raw byte stream into NDJSON record lines.
///
/// A record ends at LF, with one trailing CR removed; the final record may
/// arrive without its newline. Only the unterminated tail is bounded: once
/// it passes [`crate::sse::SSE_LINE_LIMIT`] with no record boundary in the
/// buffer, the stream ends with a protocol error. An oversized record is
/// unparseable NDJSON input, not an SSE event, so it never reports an SSE
/// limit variant.
fn lines<S>(bytes: S) -> impl Stream<Item = Result<String, ProviderError>> + Send + 'static
where
    S: Stream<Item = Vec<u8>> + Send + 'static,
{
    stream::unfold(
        (Box::pin(bytes.fuse()), Framer::default()),
        |(mut bytes, mut frame)| {
            async move {
                loop {
                    if let Some(record) = frame.next_record() {
                        return Some((Ok(record), (bytes, frame)));
                    }
                    let Some(chunk) = bytes.next().await else {
                        if frame.len() > SSE_LINE_LIMIT {
                            return Some((
                                Err(protocol(format!(
                                    "ollama record exceeds {SSE_LINE_LIMIT} bytes"
                                ))),
                                (bytes, frame),
                            ));
                        }
                        return frame.take_tail().map(|record| (Ok(record), (bytes, frame)));
                    };
                    frame.extend(&chunk);
                    // Complete records drain through `next_record`; the bound
                    // applies only to the tail still waiting for its newline.
                    if frame.has_terminator() {
                        continue;
                    }
                    if frame.len() > SSE_LINE_LIMIT {
                        return Some((
                            Err(protocol(format!(
                                "ollama record exceeds {SSE_LINE_LIMIT} bytes"
                            ))),
                            (bytes, frame),
                        ));
                    }
                }
            }
        },
    )
}

/// One NDJSON record reassembler: a byte buffer with a scan cursor.
///
/// The cursor never moves backwards, so each arriving byte is scanned for
/// the LF once. Complete records split off the front in O(1).
#[derive(Default)]
struct Framer {
    buffer: BytesMut,
    scanned: usize,
}

impl Framer {
    /// The length of the tail still waiting for its newline.
    fn len(&self) -> usize {
        self.buffer.len() - self.scanned
    }

    /// Whether the tail holds a record boundary.
    fn has_terminator(&self) -> bool {
        self.buffer[self.scanned..].contains(&b'\n')
    }

    /// Appends `chunk` to the buffer.
    fn extend(&mut self, chunk: &[u8]) {
        self.buffer.extend_from_slice(chunk);
    }

    /// Removes one complete record through its newline, or `None`.
    fn next_record(&mut self) -> Option<String> {
        let tail = &self.buffer[self.scanned..];
        let at = tail.iter().position(|byte| *byte == b'\n')?;
        let mut end = at;
        if end > 0 && tail[end - 1] == b'\r' {
            end -= 1;
        }
        let record = String::from_utf8_lossy(&tail[..end]).into_owned();
        self.scanned += at + 1;
        if self.scanned == self.buffer.len() {
            self.buffer.clear();
            self.scanned = 0;
        }
        Some(record)
    }

    /// Removes the final record that arrived without its newline.
    fn take_tail(&mut self) -> Option<String> {
        let tail = &self.buffer[self.scanned..];
        if tail.is_empty() {
            return None;
        }
        let record = String::from_utf8_lossy(tail).into_owned();
        self.buffer.clear();
        self.scanned = 0;
        Some(record)
    }
}

/// Decodes an Ollama NDJSON byte stream into neutral events.
///
/// Records feed `decoder` in order. An oversized line ends the stream with
/// its limit error, end of input ends it through [`OllamaDecoder::end`].
/// Nothing is read after the terminal; dropping the returned stream drops
/// the byte source, which cancels the underlying transfer.
pub(crate) fn decode_stream<S>(
    bytes: S,
    decoder: OllamaDecoder,
) -> impl Stream<Item = Result<StreamEvent, ProviderError>> + Send + 'static
where
    S: Stream<Item = Vec<u8>> + Send + 'static,
{
    let state = (
        Box::pin(lines(bytes)),
        decoder,
        std::collections::VecDeque::new(),
    );
    stream::unfold(state, |(mut records, mut decoder, mut queue)| async move {
        loop {
            if let Some(item) = queue.pop_front() {
                return Some((item, (records, decoder, queue)));
            }
            if decoder.ended {
                return None;
            }
            let mut out = Vec::new();
            let result = match records.next().await {
                Some(Ok(record)) => decoder.feed(&record, &mut out).map(drop),
                Some(Err(error)) => {
                    decoder.ended = true;
                    Err(error)
                }
                None => decoder.end().map(drop),
            };
            queue.extend(out.into_iter().map(Ok));
            if let Err(error) = result {
                queue.push_back(Err(error));
            }
        }
    })
}

/// One model row of the `api/tags` discovery body: the two id spellings.
/// The join maps rows to catalog entries from [`OllamaModel::wire_id`]; the
/// extra `details` members stay unread here and are not decoded.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct OllamaModel {
    /// The legacy tag spelling of the model id.
    #[serde(default)]
    pub(crate) name: Option<String>,
    /// The preferred wire model id.
    #[serde(default)]
    pub(crate) model: Option<String>,
}

impl OllamaModel {
    /// The wire model id of the row, preferring `model` over `name`.
    #[must_use]
    pub(crate) fn wire_id(&self) -> Option<&str> {
        self.model.as_deref().or(self.name.as_deref())
    }
}

#[derive(Deserialize)]
struct Tags {
    #[serde(default)]
    models: Vec<OllamaModel>,
}

/// Decodes a complete `GET api/tags` discovery body.
///
/// # Errors
///
/// [`ProviderError::Protocol`] when the body is not one JSON object.
pub(crate) fn decode_tags(bytes: &[u8]) -> Result<Vec<OllamaModel>, ProviderError> {
    let tags: Tags = sonic_rs::from_slice(bytes)
        .map_err(|error| protocol(format!("ollama tags body is not valid JSON: {error}")))?;
    Ok(tags.models)
}

#[cfg(test)]
mod tests;
