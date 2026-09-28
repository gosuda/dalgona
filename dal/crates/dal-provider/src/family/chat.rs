//! The Chat Completions family (`openai_chat`): request body lowering and stream
//! decoding.
//!
//! [`request_body`] lowers one [`ModelRequest`] and its composed
//! [`ThinkingPlan`] to the exact `POST <base>/chat/completions` body. The
//! members appear in one fixed order, tools keep registration order and never
//! carry `strict`, and two builds of the same request are equal byte for byte.
//! Tool schemas and tool-call arguments are written from their [`RawJson`]
//! text, never re-encoded.
//!
//! [`ChatDecoder`] turns the data of each SSE event into neutral
//! [`StreamEvent`]s with the grammar of [`crate::stream`]: deltas, then one
//! `ToolCallsDone`, one `Usage`, and one `Stop`, or one error instead. Only
//! choice index 0 is read. Tool calls are keyed by their delta `index`; the
//! first id and name of an index win, argument text is kept as raw bytes and
//! validated once when the response finishes. [`decode_events`] drives the
//! decoder over an SSE event stream.

use std::{
    borrow::Cow,
    collections::{BTreeMap, VecDeque, btree_map::Entry},
    mem,
};

use dal_core::{
    AssistantPart, BlobId, ContextItem, Family, ModelRequest, ModelRoute, Part, RawJson, Usage,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures::{Stream, StreamExt, stream};
use serde::{Deserialize, Serialize};
use sonic_rs::JsonValueTrait;

use crate::{
    error::ProviderError,
    retry::{RequestState, RetryDecision, classify},
    sse::SseEvent,
    stream::{StopReason, StreamEvent, ToolArgs, ToolCall},
    thinking::{ThinkingPlan, WireThinking},
};

/// The endpoint path of the family, relative to the provider base URL.
pub(crate) const ENDPOINT_PATH: &str = "chat/completions";

/// The SSE data that ends a Chat stream.
const DONE: &str = "[DONE]";

/// The HTTP status of the stream response that carries an in-stream error.
const IN_STREAM_STATUS: u16 = 200;

const NO_USAGE: Usage = Usage {
    input_tokens: 0,
    cached_input_tokens: 0,
    output_tokens: 0,
    reasoning_tokens: None,
    cache_write_tokens: 0,
    cost_usd: None,
};

/// A request that cannot be lowered to a Chat body.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum ChatBodyError {
    /// The request route is not an `openai_chat` model route.
    #[error("a chat request needs an openai_chat model route, not `{route}`")]
    NotChat {
        /// The route id of the request.
        route: Box<str>,
    },
    /// A content part still refers to stored content; the caller resolves
    /// blobs to inline bytes before lowering.
    #[error("content blob {blob_id} must be read into the request before a chat body is built")]
    UnresolvedBlob {
        /// The unresolved blob.
        blob_id: BlobId,
    },
    /// The thinking plan was composed for the Anthropic family.
    #[error("the thinking plan carries an Anthropic fragment, not a chat reasoning effort")]
    ForeignThinking,
    /// The JSON writer failed.
    #[error("chat body encoding failed: {message}")]
    Encode {
        /// The writer message.
        message: String,
    },
}

#[derive(Serialize)]
struct Body<'a> {
    model: &'a str,
    messages: Vec<Message<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<WireTool<'a>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parallel_tool_calls: Option<bool>,
    stream: bool,
    stream_options: StreamOptions,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<&'a str>,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Serialize)]
#[serde(tag = "role", rename_all = "lowercase")]
enum Message<'a> {
    System {
        content: &'a str,
    },
    User {
        content: Vec<ContentPart<'a>>,
    },
    Assistant {
        content: Option<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<WireCall<'a>>,
    },
    Tool {
        tool_call_id: &'a str,
        content: String,
    },
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentPart<'a> {
    Text { text: Cow<'a, str> },
    ImageUrl { image_url: ImageUrl },
}

#[derive(Serialize)]
struct ImageUrl {
    url: String,
}

#[derive(Serialize)]
struct WireCall<'a> {
    id: &'a str,
    #[serde(rename = "type")]
    kind: &'static str,
    function: WireCallFunction<'a>,
}

#[derive(Serialize)]
struct WireCallFunction<'a> {
    name: &'a str,
    arguments: &'a str,
}

#[derive(Serialize)]
struct WireTool<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    function: WireToolFunction<'a>,
}

#[derive(Serialize)]
struct WireToolFunction<'a> {
    name: &'a str,
    description: &'a str,
    parameters: &'a RawJson,
}

/// Lowers `request` to the JSON body of `POST <base>/chat/completions`.
///
/// `thinking` is the plan composed from `request.params`, the hooks, and the
/// model row; its effort becomes `reasoning_effort` and its temperature
/// `temperature`, each omitted when the plan has none. `request.cache_key`
/// becomes `prompt_cache_key`. The system text is a leading system message
/// unless empty. User messages carry text and `image_url` parts; assistant
/// messages carry their text parts joined by LF as `content` (`null` without
/// text) and their calls as `tool_calls` with the raw argument text; reasoning
/// parts are never sent, and an assistant message with neither text nor calls
/// is omitted. A tool result becomes one `tool` message with its text parts
/// joined by LF; the images of a run of consecutive tool results follow in one
/// user message, each call's images after the text part
/// `Images from tool call <call_id>.`. Without tools, `tools`, `tool_choice`,
/// and `parallel_tool_calls` are omitted.
///
/// # Errors
///
/// [`ChatBodyError::NotChat`] for a route other than an `openai_chat` API
/// route, [`ChatBodyError::UnresolvedBlob`] for a blob part,
/// [`ChatBodyError::ForeignThinking`] for an Anthropic thinking fragment, and
/// [`ChatBodyError::Encode`] when the writer fails.
pub(crate) fn request_body(
    request: &ModelRequest,
    thinking: &ThinkingPlan,
) -> Result<Vec<u8>, ChatBodyError> {
    let ModelRoute::Api {
        family: Family::Chat,
        model,
    } = &request.model
    else {
        return Err(ChatBodyError::NotChat {
            route: request.model.id().into(),
        });
    };
    let WireThinking::OpenAi { effort } = thinking.wire else {
        return Err(ChatBodyError::ForeignThinking);
    };
    let has_tools = !request.tools.is_empty();
    let tools = has_tools.then(|| {
        request
            .tools
            .iter()
            .map(|tool| WireTool {
                kind: "function",
                function: WireToolFunction {
                    name: &tool.name,
                    description: &tool.description,
                    parameters: &tool.parameters,
                },
            })
            .collect()
    });
    let body = Body {
        model,
        messages: messages(request)?,
        tools,
        tool_choice: has_tools.then_some("auto"),
        parallel_tool_calls: has_tools.then_some(true),
        stream: true,
        stream_options: StreamOptions {
            include_usage: true,
        },
        reasoning_effort: effort,
        temperature: thinking.temperature,
        prompt_cache_key: request.cache_key.as_deref(),
    };
    sonic_rs::to_vec(&body).map_err(|error| ChatBodyError::Encode {
        message: error.to_string(),
    })
}

fn messages(request: &ModelRequest) -> Result<Vec<Message<'_>>, ChatBodyError> {
    let mut messages = Vec::with_capacity(request.context.len() + 1);
    if !request.system.is_empty() {
        messages.push(Message::System {
            content: &request.system,
        });
    }
    // Images of the current run of tool results, sent after the run.
    let mut tool_images: Vec<ContentPart<'_>> = Vec::new();
    for item in request.context.iter() {
        if !matches!(item, ContextItem::ToolResult { .. }) {
            flush_tool_images(&mut messages, &mut tool_images);
        }
        match item {
            ContextItem::User { parts } => messages.push(Message::User {
                content: parts.iter().map(content_part).collect::<Result<_, _>>()?,
            }),
            ContextItem::Assistant { source: _, parts } => {
                if let Some(message) = assistant(parts) {
                    messages.push(message);
                }
            }
            ContextItem::ToolResult { call, parts, .. } => {
                let (content, mut images) = split_tool_result(parts)?;
                messages.push(Message::Tool {
                    tool_call_id: call.as_str(),
                    content,
                });
                if !images.is_empty() {
                    tool_images.push(ContentPart::Text {
                        text: Cow::Owned(format!("Images from tool call {}.", call.as_str())),
                    });
                    tool_images.append(&mut images);
                }
            }
        }
    }
    flush_tool_images(&mut messages, &mut tool_images);
    Ok(messages)
}

/// Splits tool-result parts into the LF-joined text and the image parts.
fn split_tool_result(parts: &[Part]) -> Result<(String, Vec<ContentPart<'_>>), ChatBodyError> {
    let mut texts = Vec::new();
    let mut images = Vec::new();
    for part in parts {
        match content_part(part)? {
            ContentPart::Text { text } => texts.push(text),
            image @ ContentPart::ImageUrl { .. } => images.push(image),
        }
    }
    Ok((texts.join("\n"), images))
}

fn flush_tool_images<'a>(messages: &mut Vec<Message<'a>>, images: &mut Vec<ContentPart<'a>>) {
    if !images.is_empty() {
        messages.push(Message::User {
            content: mem::take(images),
        });
    }
}

fn content_part(part: &Part) -> Result<ContentPart<'_>, ChatBodyError> {
    match part {
        Part::Text { text } => Ok(ContentPart::Text {
            text: Cow::Borrowed(text),
        }),
        Part::Image { mime, bytes } => Ok(ContentPart::ImageUrl {
            image_url: ImageUrl {
                url: format!("data:{mime};base64,{}", STANDARD.encode(bytes)),
            },
        }),
        Part::Blob { blob_id, .. } => Err(ChatBodyError::UnresolvedBlob { blob_id: *blob_id }),
    }
}

fn assistant(parts: &[AssistantPart]) -> Option<Message<'_>> {
    let mut texts: Vec<&str> = Vec::new();
    let mut tool_calls = Vec::new();
    for part in parts {
        match part {
            AssistantPart::Text { text } => texts.push(text),
            AssistantPart::Thinking { .. } => {}
            AssistantPart::ToolCall { call, name, args } => tool_calls.push(WireCall {
                id: call.as_str(),
                kind: "function",
                function: WireCallFunction {
                    name,
                    arguments: args.as_str(),
                },
            }),
        }
    }
    if texts.is_empty() && tool_calls.is_empty() {
        return None;
    }
    Some(Message::Assistant {
        content: (!texts.is_empty()).then(|| texts.join("\n")),
        tool_calls,
    })
}

#[derive(Deserialize)]
struct Chunk {
    choices: Option<Vec<Choice>>,
    usage: Option<WireUsage>,
    error: Option<WireError>,
}

#[derive(Deserialize)]
struct Choice {
    #[serde(default)]
    index: u32,
    delta: Option<Delta>,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct Delta {
    content: Option<String>,
    refusal: Option<String>,
    // The reasoning spelling of `reasoning_content` servers.
    reasoning_content: Option<String>,
    // The reasoning spelling of `reasoning` servers.
    reasoning: Option<String>,
    tool_calls: Option<Vec<ToolDelta>>,
}

#[derive(Deserialize)]
struct ToolDelta {
    index: u32,
    id: Option<String>,
    function: Option<FunctionDelta>,
}

#[derive(Deserialize)]
struct FunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct WireUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    prompt_tokens_details: Option<PromptDetails>,
    completion_tokens_details: Option<CompletionDetails>,
}

#[derive(Deserialize)]
struct PromptDetails {
    cached_tokens: Option<u64>,
    cache_write_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct CompletionDetails {
    reasoning_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct WireError {
    message: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    // A string on the reference API; some compatible servers send a number.
    code: Option<sonic_rs::Value>,
}

/// Maps Chat usage onto the core counters: `input_tokens` includes cached
/// input, and `output_tokens` excludes the separately reported reasoning.
fn normalize(usage: &WireUsage) -> Usage {
    let prompt = usage.prompt_tokens_details.as_ref();
    let reasoning = usage
        .completion_tokens_details
        .as_ref()
        .and_then(|details| details.reasoning_tokens);
    Usage {
        input_tokens: usage.prompt_tokens.unwrap_or(0),
        cached_input_tokens: prompt.and_then(|p| p.cached_tokens).unwrap_or(0),
        output_tokens: usage
            .completion_tokens
            .unwrap_or(0)
            .saturating_sub(reasoning.unwrap_or(0)),
        reasoning_tokens: reasoning,
        cache_write_tokens: prompt.and_then(|p| p.cache_write_tokens).unwrap_or(0),
        cost_usd: None,
    }
}

fn stop_reason(reason: String) -> StopReason {
    let known = match reason.as_str() {
        "stop" => Some(StopReason::EndTurn),
        "tool_calls" => Some(StopReason::ToolUse),
        "length" => Some(StopReason::MaxTokens),
        "content_filter" => Some(StopReason::Refusal),
        _ => None,
    };
    known.unwrap_or_else(|| StopReason::Other(reason))
}

fn protocol(detail: String) -> ProviderError {
    ProviderError::Protocol {
        family: Family::Chat,
        detail,
    }
}

/// Opens the call of a new delta index, announcing it on `out`; an index
/// needs a non-empty id and name on its first delta.
fn open_call(
    index: u32,
    id: Option<String>,
    name: Option<String>,
    out: &mut Vec<StreamEvent>,
) -> Result<OpenCall, ProviderError> {
    let id = id.filter(|id| !id.is_empty());
    let name = name.filter(|name| !name.is_empty());
    let (Some(id), Some(name)) = (id, name) else {
        return Err(protocol(format!(
            "chat tool call {index} opened without an id and a name"
        )));
    };
    out.push(StreamEvent::ToolCallStarted {
        id: id.clone(),
        name: name.clone(),
    });
    Ok(OpenCall {
        id,
        name,
        args: Vec::new(),
    })
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

/// The incremental decoder of one Chat completion stream.
///
/// Feed it the data of every SSE event in order with [`ChatDecoder::feed`],
/// then call [`ChatDecoder::end`] at end of input. It emits at most one
/// terminal: `ToolCallsDone`, `Usage`, and `Stop` together, or one error.
/// Once ended, it ignores all further input.
#[derive(Debug)]
pub(crate) struct ChatDecoder {
    provider: Box<str>,
    model: Box<str>,
    /// Open calls by delta index; iteration order is index order.
    calls: BTreeMap<u32, OpenCall>,
    /// The last non-null usage.
    usage: Option<Usage>,
    /// The first non-null `finish_reason` of choice 0, mapped.
    stop: Option<StopReason>,
    /// Some event was emitted.
    emitted: bool,
    ended: bool,
}

impl ChatDecoder {
    /// A decoder for one response of `model` on the provider id `provider`;
    /// both only name the request in mapped in-stream errors.
    #[must_use]
    pub(crate) fn new(provider: &str, model: &str) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            calls: BTreeMap::new(),
            usage: None,
            stop: None,
            emitted: false,
            ended: false,
        }
    }

    /// Decodes the data of one SSE event, appending events to `out`.
    ///
    /// `[DONE]` finishes the response. Any other data is one chunk: a
    /// non-null `usage` is kept, only choice index 0 is read, non-empty
    /// reasoning, `content`, and `refusal` text become deltas, a tool delta
    /// opens its index with its id and name (later ones are ignored) and its
    /// non-empty argument text is appended as raw bytes, and the first
    /// non-null `finish_reason` is kept.
    ///
    /// # Errors
    ///
    /// [`ProviderError::Protocol`] for data that is not a chunk or a tool
    /// delta that opens an index without an id and a name; the mapped server
    /// error for an `error` chunk. The decoder is ended after an error.
    pub(crate) fn feed(
        &mut self,
        data: &str,
        out: &mut Vec<StreamEvent>,
    ) -> Result<Progress, ProviderError> {
        if self.ended {
            return Ok(Progress::Ended);
        }
        let start = out.len();
        let result = self.step(data, out);
        self.emitted |= out.len() > start;
        if !matches!(result, Ok(Progress::Open)) {
            self.ended = true;
        }
        result
    }

    /// Ends the input: a response with a `finish_reason` finishes, even
    /// without `[DONE]` or usage; an ended decoder emits nothing.
    ///
    /// # Errors
    ///
    /// [`ProviderError::StreamCut`] when no `finish_reason` arrived.
    pub(crate) fn end(&mut self, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        if mem::replace(&mut self.ended, true) {
            return Ok(());
        }
        if self.stop.is_none() {
            return Err(ProviderError::StreamCut);
        }
        self.finish(out);
        Ok(())
    }

    fn step(&mut self, data: &str, out: &mut Vec<StreamEvent>) -> Result<Progress, ProviderError> {
        if data == DONE {
            self.finish(out);
            return Ok(Progress::Ended);
        }
        let chunk: Chunk = sonic_rs::from_str(data)
            .map_err(|error| protocol(format!("chat chunk is not valid: {error}")))?;
        if let Some(error) = chunk.error {
            return Err(self.server_error(error));
        }
        if let Some(usage) = &chunk.usage {
            self.usage = Some(normalize(usage));
        }
        let Some(choice) = chunk
            .choices
            .into_iter()
            .flatten()
            .find(|choice| choice.index == 0)
        else {
            return Ok(Progress::Open);
        };
        if let Some(delta) = choice.delta {
            self.delta(delta, out)?;
        }
        if self.stop.is_none() {
            self.stop = choice.finish_reason.map(stop_reason);
        }
        Ok(Progress::Open)
    }

    fn delta(&mut self, delta: Delta, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        let non_empty = |text: &String| !text.is_empty();
        if let Some(text) = delta
            .reasoning_content
            .filter(non_empty)
            .or_else(|| delta.reasoning.filter(non_empty))
        {
            out.push(StreamEvent::ReasoningDelta { text });
        }
        for text in [delta.content, delta.refusal].into_iter().flatten() {
            if !text.is_empty() {
                out.push(StreamEvent::TextDelta { text });
            }
        }
        for tool in delta.tool_calls.into_iter().flatten() {
            let mut function = tool.function;
            let call = match self.calls.entry(tool.index) {
                Entry::Occupied(open) => open.into_mut(),
                Entry::Vacant(slot) => {
                    let name = function.as_mut().and_then(|function| function.name.take());
                    slot.insert(open_call(tool.index, tool.id, name, out)?)
                }
            };
            if let Some(fragment) = function
                .and_then(|function| function.arguments)
                .filter(|fragment| !fragment.is_empty())
            {
                call.args.extend_from_slice(fragment.as_bytes());
                out.push(StreamEvent::ToolArgsDelta {
                    id: call.id.clone(),
                    fragment: fragment.into_bytes(),
                });
            }
        }
        Ok(())
    }

    /// Emits the terminal group. Calls close in index order; empty argument
    /// text is `{}`, and every call of a `MaxTokens` response is truncated.
    /// Without a `finish_reason`, open calls stop as `ToolUse`, else `EndTurn`.
    fn finish(&mut self, out: &mut Vec<StreamEvent>) {
        let reason = self.stop.take().unwrap_or(if self.calls.is_empty() {
            StopReason::EndTurn
        } else {
            StopReason::ToolUse
        });
        let truncated = reason == StopReason::MaxTokens;
        let calls = mem::take(&mut self.calls)
            .into_values()
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
        out.push(StreamEvent::Usage {
            usage: self.usage.take().unwrap_or(NO_USAGE),
        });
        out.push(StreamEvent::Stop { reason });
    }

    /// Maps an in-stream `error` chunk through the retry table. The decoder
    /// returns only the error value; the request lifecycle owns retries.
    fn server_error(&self, error: WireError) -> ProviderError {
        let message = error.message.unwrap_or_default();
        let code = error
            .code
            .as_ref()
            .and_then(JsonValueTrait::as_str)
            .or(error.kind.as_deref());
        let Some(code) = code else {
            return ProviderError::Status {
                family: Family::Chat,
                status: IN_STREAM_STATUS,
                message,
            };
        };
        let request = RequestState {
            family: Family::Chat,
            provider: &self.provider,
            model: &self.model,
            oauth: false,
            refreshed: false,
            delivered: self.emitted,
        };
        match classify(None, Some(code), &message, &request) {
            RetryDecision::Retry(error) | RetryDecision::Fail(error) => error,
            // Unreachable without an HTTP status; kept total.
            RetryDecision::RefreshOnce => ProviderError::SignInExpired {
                provider: String::from(request.provider),
            },
        }
    }
}

/// Decodes a Chat SSE event stream into neutral events.
///
/// Event names are ignored; each event's data goes to `decoder`. An SSE
/// error ends the stream with that error, and end of input ends it through
/// [`ChatDecoder::end`]. Nothing is read after the terminal.
pub(crate) fn decode_events<S>(
    events: S,
    decoder: ChatDecoder,
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
                None => decoder.end(&mut out),
            };
            queue.extend(out.into_iter().map(Ok));
            if let Err(error) = result {
                queue.push_back(Err(error));
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use dal_core::{CallId, ModelToolSpec, Purpose, ReplaySource, RequestParams, ThinkingLevel};
    use futures::executor::block_on;

    use super::*;
    use crate::sse::decode_stream;

    fn raw(text: &str) -> RawJson {
        RawJson::parse(text).unwrap()
    }

    fn replay_source() -> ReplaySource {
        ReplaySource {
            family: Family::Chat,
            model: "gpt-6-astra".into(),
        }
    }

    fn png() -> Part {
        Part::Image {
            mime: "image/png".into(),
            bytes: b"\x89PNG".as_slice().into(),
        }
    }

    fn text(value: &str) -> Part {
        Part::Text { text: value.into() }
    }

    fn plan(effort: Option<&'static str>, temperature: Option<f32>) -> ThinkingPlan {
        ThinkingPlan {
            level: ThinkingLevel::High,
            wire: WireThinking::OpenAi { effort },
            temperature,
            notices: Vec::new(),
        }
    }

    fn request(tools: Vec<ModelToolSpec>, context: Vec<ContextItem>) -> ModelRequest {
        ModelRequest {
            purpose: Purpose::Turn,
            model: ModelRoute::Api {
                family: Family::Chat,
                model: "gpt-6-astra".into(),
            },
            system: "You are terse.".into(),
            tools: tools.into(),
            context: context.into(),
            params: RequestParams {
                thinking: ThinkingLevel::High,
                effort: None,
                temperature: None,
            },
            cache_key: Some("0190-session".into()),
        }
    }

    fn body_text(request: &ModelRequest, thinking: &ThinkingPlan) -> String {
        String::from_utf8(request_body(request, thinking).unwrap()).unwrap()
    }

    #[test]
    fn request_body_matches_the_member_order_byte_for_byte() {
        let request = request(
            vec![
                ModelToolSpec {
                    name: "read".into(),
                    description: "Read a file.".into(),
                    parameters: raw(r#"{"type":"object","properties":{"path":{"type":"string"}}}"#),
                },
                ModelToolSpec {
                    name: "list".into(),
                    description: "List files.".into(),
                    parameters: raw(r#"{"type": "object"}"#),
                },
            ],
            vec![
                ContextItem::User {
                    parts: vec![text("Describe"), png()],
                },
                ContextItem::Assistant {
                    source: replay_source(),
                    parts: vec![
                        AssistantPart::Thinking {
                            text: "hidden".into(),
                            replay: None,
                        },
                        AssistantPart::Text {
                            text: "Checking.".into(),
                        },
                        AssistantPart::ToolCall {
                            call: CallId::new("call_1"),
                            name: "read".into(),
                            args: raw(r#"{"path": "a.txt" ,"n":1.50}"#),
                        },
                    ],
                },
                ContextItem::ToolResult {
                    call: CallId::new("call_1"),
                    name: "read".into(),
                    is_error: false,
                    parts: vec![text("line1"), text("line2"), png()],
                },
            ],
        );
        let expected = concat!(
            r#"{"model":"gpt-6-astra","messages":["#,
            r#"{"role":"system","content":"You are terse."},"#,
            r#"{"role":"user","content":[{"type":"text","text":"Describe"},"#,
            r#"{"type":"image_url","image_url":{"url":"data:image/png;base64,iVBORw=="}}]},"#,
            r#"{"role":"assistant","content":"Checking.","tool_calls":[{"id":"call_1","type":"function","#,
            r#""function":{"name":"read","arguments":"{\"path\": \"a.txt\" ,\"n\":1.50}"}}]},"#,
            r#"{"role":"tool","tool_call_id":"call_1","content":"line1\nline2"},"#,
            r#"{"role":"user","content":[{"type":"text","text":"Images from tool call call_1."},"#,
            r#"{"type":"image_url","image_url":{"url":"data:image/png;base64,iVBORw=="}}]}],"#,
            r#""tools":[{"type":"function","function":{"name":"read","description":"Read a file.","#,
            r#""parameters":{"type":"object","properties":{"path":{"type":"string"}}}}},"#,
            r#"{"type":"function","function":{"name":"list","description":"List files.","#,
            r#""parameters":{"type": "object"}}}],"#,
            r#""tool_choice":"auto","parallel_tool_calls":true,"stream":true,"#,
            r#""stream_options":{"include_usage":true},"reasoning_effort":"high","#,
            r#""prompt_cache_key":"0190-session"}"#,
        );
        assert_eq!(body_text(&request, &plan(Some("high"), None)), expected);
    }

    #[test]
    fn request_body_without_tools_omits_tool_members_and_unsent_parameters() {
        let mut request = request(
            Vec::new(),
            vec![
                ContextItem::User {
                    parts: vec![text("hi")],
                },
                ContextItem::Assistant {
                    source: replay_source(),
                    parts: vec![AssistantPart::Thinking {
                        text: "only reasoning".into(),
                        replay: Some(raw("{}")),
                    }],
                },
            ],
        );
        request.cache_key = None;
        assert_eq!(
            body_text(&request, &plan(None, Some(0.5))),
            concat!(
                r#"{"model":"gpt-6-astra","messages":[{"role":"system","content":"You are terse."},"#,
                r#"{"role":"user","content":[{"type":"text","text":"hi"}]}],"stream":true,"#,
                r#""stream_options":{"include_usage":true},"temperature":0.5}"#,
            )
        );
    }

    #[test]
    fn request_body_rejects_what_chat_cannot_carry() {
        let blob_id = BlobId::from_bytes(b"stored");
        let blob = request(
            Vec::new(),
            vec![ContextItem::User {
                parts: vec![Part::Blob {
                    blob_id,
                    mime: "image/png".into(),
                    bytes: 6,
                }],
            }],
        );
        assert_eq!(
            request_body(&blob, &plan(None, None)),
            Err(ChatBodyError::UnresolvedBlob { blob_id })
        );

        let mut other = request(Vec::new(), Vec::new());
        other.model = ModelRoute::Api {
            family: Family::Responses,
            model: "gpt-6-astra".into(),
        };
        assert_eq!(
            request_body(&other, &plan(None, None)),
            Err(ChatBodyError::NotChat {
                route: "gpt-6-astra".into()
            })
        );

        let anthropic = ThinkingPlan {
            wire: WireThinking::Anthropic {
                thinking: crate::thinking::AnthropicThinking::Adaptive,
                effort: None,
            },
            ..plan(None, None)
        };
        assert_eq!(
            request_body(&request(Vec::new(), Vec::new()), &anthropic),
            Err(ChatBodyError::ForeignThinking)
        );
    }

    fn sse(datas: &[&str]) -> String {
        datas.iter().map(|data| format!("data: {data}\n\n")).collect()
    }

    /// Runs `wire` through the SSE framing and the Chat decoder, delivered
    /// in network chunks of `split` bytes.
    fn run(wire: &str, split: usize) -> (Vec<StreamEvent>, Option<ProviderError>) {
        let chunks: Vec<Vec<u8>> = wire.as_bytes().chunks(split).map(<[u8]>::to_vec).collect();
        let decoded = decode_events(
            decode_stream(stream::iter(chunks)),
            ChatDecoder::new("zenmux", "openai/gpt-5.6-luna"),
        );
        let items: Vec<_> = block_on(decoded.collect());
        let mut events = Vec::new();
        let mut error = None;
        for item in items {
            assert!(error.is_none(), "an item followed the terminal error");
            match item {
                Ok(event) => events.push(event),
                Err(failure) => error = Some(failure),
            }
        }
        (events, error)
    }

    fn done(calls: Vec<ToolCall>, usage: Usage, reason: StopReason) -> Vec<StreamEvent> {
        vec![
            StreamEvent::ToolCallsDone { calls },
            StreamEvent::Usage { usage },
            StreamEvent::Stop { reason },
        ]
    }

    const TEXT_TURN: [&str; 5] = [
        r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","created":1694268190,"model":"gpt-6-astra", "system_fingerprint": "fp_44709d6fcb", "choices":[{"index":0,"delta":{"role":"assistant","content":""},"logprobs":null,"finish_reason":null}],"obfuscation":"r4N7vQ2m","usage":null}"#,
        r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","created":1694268190,"model":"gpt-6-astra", "system_fingerprint": "fp_44709d6fcb", "choices":[{"index":0,"delta":{"content":"Hello"},"logprobs":null,"finish_reason":null}],"obfuscation":"p9K3xT6w","usage":null}"#,
        r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","created":1694268190,"model":"gpt-6-astra", "system_fingerprint": "fp_44709d6fcb", "choices":[{"index":0,"delta":{},"logprobs":null,"finish_reason":"stop"}],"obfuscation":"","usage":null}"#,
        r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","created":1694268190,"model":"gpt-6-astra","choices":[],"usage":{"prompt_tokens":19,"completion_tokens":10,"total_tokens":29,"prompt_tokens_details":{"cached_tokens":4,"audio_tokens":0},"completion_tokens_details":{"reasoning_tokens":3,"audio_tokens":0}}}"#,
        "[DONE]",
    ];

    #[test]
    fn text_turn_decodes_over_any_network_split() {
        let wire = sse(&TEXT_TURN);
        let mut expected = vec![StreamEvent::TextDelta {
            text: "Hello".into(),
        }];
        expected.extend(done(
            Vec::new(),
            Usage {
                input_tokens: 19,
                cached_input_tokens: 4,
                output_tokens: 7,
                reasoning_tokens: Some(3),
                cache_write_tokens: 0,
                cost_usd: None,
            },
            StopReason::EndTurn,
        ));
        for split in [1, 2, 7, 64, wire.len()] {
            let (events, error) = run(&wire, split);
            assert!(error.is_none(), "split {split}: {error:?}");
            assert_eq!(events, expected, "split {split}");
        }
    }

    #[test]
    fn tool_turn_keeps_raw_argument_bytes_and_the_first_id() {
        let wire = sse(&[
            r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":null,"tool_calls":[{"index":0,"id":"call_abc123","type":"function","function":{"name":"get_current_weather","arguments":""}}]},"finish_reason":null}],"usage":null}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\n\""}}]},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"location"}}]},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_late","function":{"name":"other","arguments":"\": \"Bos"}}]},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"ton, MA\""}}]},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\n}"}}]},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],"usage":null}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":82,"completion_tokens":17,"total_tokens":99}}"#,
            "[DONE]",
        ]);
        let id = String::from("call_abc123");
        let mut expected = vec![StreamEvent::ToolCallStarted {
            id: id.clone(),
            name: "get_current_weather".into(),
        }];
        for fragment in ["{\n\"", "location", "\": \"Bos", "ton, MA\"", "\n}"] {
            expected.push(StreamEvent::ToolArgsDelta {
                id: id.clone(),
                fragment: fragment.as_bytes().to_vec(),
            });
        }
        expected.extend(done(
            vec![ToolCall {
                id,
                name: "get_current_weather".into(),
                args: ToolArgs::Parsed(raw("{\n\"location\": \"Boston, MA\"\n}")),
            }],
            Usage {
                input_tokens: 82,
                output_tokens: 17,
                ..NO_USAGE
            },
            StopReason::ToolUse,
        ));
        for split in [3, wire.len()] {
            let (events, error) = run(&wire, split);
            assert!(error.is_none(), "{error:?}");
            assert_eq!(events, expected);
        }
    }

    #[test]
    fn parallel_calls_close_in_index_order() {
        let wire = sse(&[
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"call_b","type":"function","function":{"name":"b","arguments":""}}]},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"a","arguments":"{\"x\":"}}]},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"arguments":"{\"y\":2}"}}]},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"1}"}}]},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
            "[DONE]",
        ]);
        let (events, error) = run(&wire, wire.len());
        assert!(error.is_none(), "{error:?}");
        assert_eq!(
            events[events.len() - 3..],
            done(
                vec![
                    ToolCall {
                        id: "call_a".into(),
                        name: "a".into(),
                        args: ToolArgs::Parsed(raw(r#"{"x":1}"#)),
                    },
                    ToolCall {
                        id: "call_b".into(),
                        name: "b".into(),
                        args: ToolArgs::Parsed(raw(r#"{"y":2}"#)),
                    },
                ],
                NO_USAGE,
                StopReason::ToolUse,
            )
        );
    }

    #[test]
    fn end_of_input_after_a_finish_reason_stops_with_zero_usage() {
        let wire = sse(&TEXT_TURN[..3]);
        let (events, error) = run(&wire, 5);
        assert!(error.is_none(), "{error:?}");
        let mut expected = vec![StreamEvent::TextDelta {
            text: "Hello".into(),
        }];
        expected.extend(done(Vec::new(), NO_USAGE, StopReason::EndTurn));
        assert_eq!(events, expected);
    }

    #[test]
    fn end_of_input_before_a_finish_reason_is_a_cut() {
        let (events, error) = run(&sse(&TEXT_TURN[..2]), 9);
        assert_eq!(
            events,
            vec![StreamEvent::TextDelta {
                text: "Hello".into()
            }]
        );
        let error = error.unwrap();
        assert!(matches!(error, ProviderError::StreamCut));
        assert_eq!(error.to_string(), "stream cut off before completion.");
    }

    fn single_call(arguments: &str, finish: &str) -> ToolArgs {
        let open = format!(
            r#"{{"choices":[{{"index":0,"delta":{{"tool_calls":[{{"index":0,"id":"call_1","function":{{"name":"f","arguments":{arguments}}}}}]}},"finish_reason":null}}]}}"#
        );
        let finish = format!(r#"{{"choices":[{{"index":0,"delta":{{}},"finish_reason":"{finish}"}}]}}"#);
        let (events, error) = run(&sse(&[&open, &finish, "[DONE]"]), 11);
        assert!(error.is_none(), "{error:?}");
        let Some(StreamEvent::ToolCallsDone { calls }) = events.iter().rev().nth(2) else {
            panic!("no ToolCallsDone in {events:?}");
        };
        assert_eq!(calls.len(), 1);
        calls[0].args.clone()
    }

    #[test]
    fn final_arguments_parse_once_by_the_stop_reason() {
        let ToolArgs::Invalid { message } = single_call(r#""{\"a\":""#, "tool_calls") else {
            panic!("unparsable arguments must be Invalid");
        };
        assert!(!message.is_empty());
        assert_eq!(single_call(r#""""#, "tool_calls"), ToolArgs::Parsed(raw("{}")));
        assert_eq!(single_call("null", "tool_calls"), ToolArgs::Parsed(raw("{}")));
        assert_eq!(single_call(r#""{\"a\":1}""#, "length"), ToolArgs::Truncated);
    }

    #[test]
    fn only_choice_zero_reasoning_and_refusal_reach_the_stream() {
        let wire = sse(&[
            r#"{"choices":[{"index":1,"delta":{"content":"B"},"finish_reason":"stop"},{"index":0,"delta":{"reasoning_content":"think","content":"A"},"finish_reason":null}],"usage":null}"#,
            r#"{"choices":[{"index":0,"delta":{"reasoning":"more","refusal":"I can't help with that."},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{},"finish_reason":"content_filter"}]}"#,
            r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
            "[DONE]",
        ]);
        let (events, error) = run(&wire, wire.len());
        assert!(error.is_none(), "{error:?}");
        let mut expected = vec![
            StreamEvent::ReasoningDelta {
                text: "think".into(),
            },
            StreamEvent::TextDelta { text: "A".into() },
            StreamEvent::ReasoningDelta {
                text: "more".into(),
            },
            StreamEvent::TextDelta {
                text: "I can't help with that.".into(),
            },
        ];
        expected.extend(done(Vec::new(), NO_USAGE, StopReason::Refusal));
        assert_eq!(events, expected);
    }

    #[test]
    fn in_stream_errors_and_malformed_chunks_end_the_stream() {
        let (events, error) = run(
            &sse(&[
                TEXT_TURN[1],
                r#"{"error":{"message":"Rate limit reached","type":"requests","code":"rate_limit_exceeded"}}"#,
                TEXT_TURN[2],
            ]),
            13,
        );
        assert_eq!(events.len(), 1);
        assert!(matches!(
            error,
            Some(ProviderError::RateLimited { ref message, retry_after: None }) if message == "Rate limit reached"
        ));

        let (_, error) = run(&sse(&[r#"{"error":{"message":"busy","code":503}}"#]), 64);
        assert!(matches!(
            error,
            Some(ProviderError::Status { family: Family::Chat, status: 200, ref message }) if message == "busy"
        ));

        let (events, error) = run(&sse(&["not json", "[DONE]"]), 64);
        assert!(events.is_empty());
        assert!(matches!(
            error,
            Some(ProviderError::Protocol {
                family: Family::Chat,
                ..
            })
        ));

        let (_, error) = run(
            &sse(&[r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{}"}}]},"finish_reason":null}]}"#]),
            64,
        );
        assert!(matches!(error, Some(ProviderError::Protocol { .. })));
    }

    #[test]
    fn done_without_a_finish_reason_keeps_open_calls_as_tool_use() {
        let mut decoder = ChatDecoder::new("openai", "gpt-6-astra");
        let mut out = Vec::new();
        let open = r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"f","arguments":"{}"}}]},"finish_reason":null}]}"#;
        assert_eq!(decoder.feed(open, &mut out).unwrap(), Progress::Open);
        assert_eq!(decoder.feed("[DONE]", &mut out).unwrap(), Progress::Ended);
        assert_eq!(
            out.last(),
            Some(&StreamEvent::Stop {
                reason: StopReason::ToolUse
            })
        );
    }

    #[test]
    fn the_decoder_emits_one_terminal_and_ignores_later_input() {
        let mut decoder = ChatDecoder::new("openai", "gpt-6-astra");
        let mut out = Vec::new();
        assert_eq!(decoder.feed(TEXT_TURN[1], &mut out).unwrap(), Progress::Open);
        assert_eq!(decoder.feed("[DONE]", &mut out).unwrap(), Progress::Ended);
        assert_eq!(out.len(), 4);
        assert_eq!(decoder.feed(TEXT_TURN[1], &mut out).unwrap(), Progress::Ended);
        assert_eq!(decoder.feed("[DONE]", &mut out).unwrap(), Progress::Ended);
        decoder.end(&mut out).unwrap();
        assert_eq!(out.len(), 4);
        assert_eq!(
            out.last(),
            Some(&StreamEvent::Stop {
                reason: StopReason::EndTurn
            })
        );
    }

    #[test]
    fn the_full_path_runs_through_the_event_stream_guard() {
        let chunks = vec![sse(&TEXT_TURN).into_bytes()];
        let mut events = crate::stream::EventStream::new(
            decode_events(
                decode_stream(stream::iter(chunks)),
                ChatDecoder::new("openai", "gpt-6-astra"),
            ),
            || panic!("a clean Stop must not cancel"),
        );
        let seen: Vec<_> = block_on(async {
            let mut seen = Vec::new();
            while let Some(item) = events.next().await {
                seen.push(item.unwrap());
            }
            seen
        });
        assert_eq!(seen.len(), 4);
        assert!(matches!(seen.last(), Some(StreamEvent::Stop { .. })));
    }
}
