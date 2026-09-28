//! The Anthropic Messages family: request lowering and stream decoding.
//!
//! [`build`] lowers a [`ModelRequest`] to the exact `POST <base>/v1/messages`
//! headers and body. Body members go out in one fixed order, tools keep
//! registration order, and two builds of the same input are equal byte for
//! byte. Cache breakpoints sit on exactly three places: the last system block,
//! the last tool, and the last typed content block of the last message.
//! Tool arguments and replayed thinking blocks travel as raw JSON, never
//! re-encoded. A Claude OAuth token also carries the fingerprint of
//! [`crate::claude_fingerprint`]; API keys and bearer keys never do.
//!
//! [`decode_stream`] turns the SSE events of one response into the neutral
//! [`StreamEvent`] grammar: deltas as they arrive, a [`StreamEvent::Replay`]
//! for every signed or redacted thinking block, then exactly one
//! `ToolCallsDone`, one `Usage`, and one `Stop` at `message_stop`. An `error`
//! event ends the stream with its error, and input that ends before
//! `message_stop` ends it with [`ProviderError::StreamCut`]. Decoding is
//! lenient: unknown members, event types, block types, and delta types are
//! ignored; dispatch reads the JSON `type` member, never the SSE event name.

use std::{borrow::Cow, collections::BTreeMap, collections::VecDeque, fmt, mem};

use base64::Engine as _;
use dal_core::{
    AssistantPart, ContextItem, Family, ModelRequest, Part, RawJson, ReplaySource, Usage,
};
use futures::{Stream, StreamExt, stream};
use serde::{Deserialize, Serialize, Serializer, ser::SerializeMap};
use sonic_rs::{JsonValueTrait, LazyValue};

use crate::{
    claude_fingerprint,
    error::ProviderError,
    sse::SseEvent,
    stream::{ReplayPayload, StopReason, StreamEvent, ToolArgs, ToolCall},
    thinking::{AnthropicThinking, Effort},
};

/// The endpoint path joined to the provider base URL.
pub(crate) const MESSAGES_PATH: &str = "v1/messages";

/// The `anthropic-version` header value.
pub(crate) const ANTHROPIC_VERSION: &str = "2023-06-01";

/// The `max_tokens` ceiling, and the value when `max_output` is unknown.
pub(crate) const MAX_TOKENS_CAP: u32 = 32_000;

const INTERLEAVED_THINKING_BETA: &str = "interleaved-thinking-2025-05-14";
const COMPACT_BETA: &str = "compact-2026-09-04";

/// How one request authenticates, resolved from the credential store and the
/// provider entry before lowering.
#[derive(Clone, Copy)]
pub(crate) enum AnthropicAuth<'a> {
    /// An API key, or a named provider with the default auth style:
    /// `x-api-key: <key>`.
    ApiKey(&'a str),
    /// A named provider with `auth = "bearer"`:
    /// `authorization: Bearer <key>`.
    Bearer(&'a str),
    /// A Claude OAuth sign-in: `authorization: Bearer <access_token>` plus
    /// the Claude Code fingerprint.
    ClaudeOAuth {
        /// The current access token.
        access_token: &'a str,
        /// The Claude Code version to send; the default pin is
        /// [`claude_fingerprint::CLAUDE_CODE_VERSION`].
        version: &'a str,
    },
}

impl AnthropicAuth<'_> {
    const fn is_oauth(self) -> bool {
        matches!(self, Self::ClaudeOAuth { .. })
    }
}

impl fmt::Debug for AnthropicAuth<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ApiKey(_) => f.write_str("ApiKey(<redacted>)"),
            Self::Bearer(_) => f.write_str("Bearer(<redacted>)"),
            Self::ClaudeOAuth { version, .. } => f
                .debug_struct("ClaudeOAuth")
                .field("access_token", &"<redacted>")
                .field("version", version)
                .finish(),
        }
    }
}

/// Everything one Anthropic body needs besides the credential.
///
/// `thinking`, `effort`, and `temperature` come from
/// [`crate::thinking::plan`] with [`crate::thinking::RequestLimits::max_tokens`]
/// set to [`base_max_tokens`]; the temperature is already dropped there
/// unless the model allows it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AnthropicRequest<'a> {
    /// The neutral request; its route id is the wire model id.
    pub(crate) request: &'a ModelRequest,
    /// The model's output ceiling, when the catalog knows it.
    pub(crate) max_output: Option<u32>,
    /// The `thinking` member.
    pub(crate) thinking: AnthropicThinking,
    /// `output_config.effort`; `None` omits `output_config`.
    pub(crate) effort: Option<Effort>,
    /// Whether the model supports the adaptive thinking display hint.
    pub(crate) display_supported: bool,
    /// The sampling temperature, sent only when present.
    pub(crate) temperature: Option<f32>,
    /// A compaction block bound to this family and model, sent verbatim as
    /// the first content block of the first user message.
    pub(crate) compaction: Option<&'a RawJson>,
    /// Requests a remote compaction: `"compaction":{"type":"summarize"}`,
    /// and no streaming.
    pub(crate) summarize: bool,
}

/// One lowered request: the path, the headers in send order, the
/// `user-agent` override, and the body bytes.
#[must_use]
pub(crate) struct AnthropicWire {
    /// The path joined to the base URL.
    pub(crate) path: &'static str,
    /// Headers in send order; names are lowercase.
    pub(crate) headers: Vec<(&'static str, String)>,
    /// The `user-agent` the request must send instead of the dalgon one;
    /// set only for Claude OAuth.
    pub(crate) user_agent: Option<String>,
    /// The JSON body.
    pub(crate) body: Vec<u8>,
}

impl AnthropicWire {
    /// Attaches the headers and the body to a `POST` of `url`; the caller
    /// passes the result to [`crate::http::send`] with
    /// [`AnthropicWire::user_agent`] when it is set.
    pub(crate) fn into_request(self, client: &reqwest::Client, url: url::Url) -> reqwest::RequestBuilder {
        let mut request = client.post(url);
        for (name, value) in self.headers {
            request = request.header(name, value);
        }
        request.body(self.body)
    }
}

impl fmt::Debug for AnthropicWire {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let headers: Vec<(&str, &str)> = self
            .headers
            .iter()
            .map(|(name, value)| {
                let secret = matches!(*name, "x-api-key" | "authorization");
                (*name, if secret { "<redacted>" } else { value.as_str() })
            })
            .collect();
        f.debug_struct("AnthropicWire")
            .field("path", &self.path)
            .field("headers", &headers)
            .field("user_agent", &self.user_agent)
            .field("body_len", &self.body.len())
            .finish()
    }
}

/// The body's `max_tokens` without thinking: `min(max_output, 32000)`, or
/// 32000 when `max_output` is unknown.
#[must_use]
pub(crate) fn base_max_tokens(max_output: Option<u32>) -> u32 {
    max_output.map_or(MAX_TOKENS_CAP, |cap| cap.min(MAX_TOKENS_CAP))
}

/// Lowers one request to its headers and body.
///
/// # Errors
///
/// [`ProviderError::InvalidRequest`] when a content part is still a stored
/// blob (the caller inlines blobs before lowering) or the body cannot be
/// serialized.
pub(crate) fn build(
    input: &AnthropicRequest<'_>,
    auth: AnthropicAuth<'_>,
) -> Result<AnthropicWire, ProviderError> {
    let oauth = auth.is_oauth();
    let request = input.request;
    let model = request.model.id();
    let enabled = matches!(input.thinking, AnthropicThinking::Enabled { .. });
    let compacting = input.compaction.is_some() || input.summarize;

    let mut betas: Vec<&'static str> = Vec::new();
    let mut add_beta = |beta: &'static str| {
        if !betas.contains(&beta) {
            betas.push(beta);
        }
    };
    if oauth {
        add_beta(claude_fingerprint::CLAUDE_CODE_BETA);
        add_beta(claude_fingerprint::OAUTH_BETA);
    }
    if enabled {
        add_beta(INTERLEAVED_THINKING_BETA);
    }
    if compacting {
        add_beta(COMPACT_BETA);
    }

    let mut headers = vec![("anthropic-version", ANTHROPIC_VERSION.to_owned())];
    let mut user_agent = None;
    match auth {
        AnthropicAuth::ApiKey(key) => headers.push(("x-api-key", key.to_owned())),
        AnthropicAuth::Bearer(key) => headers.push(("authorization", format!("Bearer {key}"))),
        AnthropicAuth::ClaudeOAuth {
            access_token,
            version,
        } => {
            headers.push(("authorization", format!("Bearer {access_token}")));
            user_agent = Some(claude_fingerprint::user_agent(version));
        }
    }
    if !betas.is_empty() {
        headers.push(("anthropic-beta", betas.join(",")));
    }
    if oauth {
        headers.push(("x-app", claude_fingerprint::X_APP.to_owned()));
    }

    let body = Body {
        model,
        max_tokens: match input.thinking {
            AnthropicThinking::Enabled { max_tokens, .. } => max_tokens,
            _ => base_max_tokens(input.max_output),
        },
        stream: !input.summarize,
        system: system_blocks(&request.system, oauth),
        messages: messages(request, model, input.compaction, oauth)?,
        tools: tools(request, oauth),
        tool_choice: (!request.tools.is_empty()).then_some(ToolChoice::Auto),
        thinking: match input.thinking {
            AnthropicThinking::Omit => None,
            AnthropicThinking::Disabled => Some(Thinking::Disabled),
            AnthropicThinking::Adaptive => Some(Thinking::Adaptive {
                display: input.display_supported.then_some("summarized"),
            }),
            AnthropicThinking::Enabled { budget_tokens, .. } => Some(Thinking::Enabled {
                budget_tokens,
                display: "summarized",
            }),
        },
        output_config: input.effort.map(|effort| OutputConfig {
            effort: effort.as_str(),
        }),
        temperature: input.temperature,
        compaction: input.summarize.then_some(Compaction::Summarize),
    };
    let body = sonic_rs::to_vec(&body).map_err(|error| ProviderError::InvalidRequest {
        message: format!("anthropic body did not serialize: {error}"),
    })?;
    Ok(AnthropicWire {
        path: MESSAGES_PATH,
        headers,
        user_agent,
        body,
    })
}

#[derive(Serialize)]
struct Body<'a> {
    model: &'a str,
    max_tokens: u32,
    stream: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    system: Vec<SystemBlock<'a>>,
    messages: Vec<Message<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<Tool<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<ToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<Thinking>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_config: Option<OutputConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    compaction: Option<Compaction>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CacheControl {
    Ephemeral,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ToolChoice {
    Auto,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Thinking {
    Disabled,
    Adaptive {
        #[serde(skip_serializing_if = "Option::is_none")]
        display: Option<&'static str>,
    },
    Enabled {
        budget_tokens: u32,
        display: &'static str,
    },
}

#[derive(Serialize)]
struct OutputConfig {
    effort: &'static str,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Compaction {
    Summarize,
}

#[derive(Serialize)]
struct SystemBlock<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<CacheControl>,
}

#[derive(Serialize)]
struct Tool<'a> {
    name: Cow<'a, str>,
    description: &'a str,
    input_schema: &'a RawJson,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<CacheControl>,
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'static str,
    content: Vec<Block<'a>>,
}

/// A content block: typed, or a raw provider block sent verbatim.
enum Block<'a> {
    Typed(Typed<'a>),
    Raw(&'a RawJson),
}

impl Serialize for Block<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Typed(block) => block.serialize(serializer),
            Self::Raw(raw) => raw.serialize(serializer),
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Typed<'a> {
    Text {
        text: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    Image {
        source: ImageSource<'a>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    ToolUse {
        id: &'a str,
        name: Cow<'a, str>,
        input: Input<'a>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    ToolResult {
        tool_use_id: &'a str,
        content: Vec<Typed<'a>>,
        is_error: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
}

impl Typed<'_> {
    const fn cache_slot(&mut self) -> &mut Option<CacheControl> {
        match self {
            Self::Text { cache_control, .. }
            | Self::Image { cache_control, .. }
            | Self::ToolUse { cache_control, .. }
            | Self::ToolResult { cache_control, .. } => cache_control,
        }
    }
}

#[derive(Serialize)]
struct ImageSource<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    media_type: &'a str,
    data: String,
}

/// A `tool_use` input: the recorded raw object, or `{}` when the recorded
/// arguments were not an object.
enum Input<'a> {
    Raw(&'a RawJson),
    Empty,
}

impl Serialize for Input<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Raw(raw) => raw.serialize(serializer),
            Self::Empty => serializer.serialize_map(Some(0))?.end(),
        }
    }
}

fn system_blocks(system: &str, oauth: bool) -> Vec<SystemBlock<'_>> {
    let identity = oauth.then_some(claude_fingerprint::CLAUDE_CODE_SYSTEM_INSTRUCTION);
    let text = (!system.is_empty()).then_some(system);
    let mut blocks: Vec<SystemBlock<'_>> = identity
        .into_iter()
        .chain(text)
        .map(|text| SystemBlock {
            kind: "text",
            text,
            cache_control: None,
        })
        .collect();
    if let Some(last) = blocks.last_mut() {
        last.cache_control = Some(CacheControl::Ephemeral);
    }
    blocks
}

fn tools(request: &ModelRequest, oauth: bool) -> Vec<Tool<'_>> {
    let mut tools: Vec<Tool<'_>> = request
        .tools
        .iter()
        .map(|tool| Tool {
            name: wire_tool_name(&tool.name, oauth),
            description: &tool.description,
            input_schema: &tool.parameters,
            cache_control: None,
        })
        .collect();
    if let Some(last) = tools.last_mut() {
        last.cache_control = Some(CacheControl::Ephemeral);
    }
    tools
}

fn wire_tool_name(name: &str, oauth: bool) -> Cow<'_, str> {
    if oauth {
        claude_fingerprint::encode_tool_name(name)
    } else {
        Cow::Borrowed(name)
    }
}

/// Lowers the context: adjacent items of one wire role share one message,
/// so every tool result of a turn lands in the next user message.
///
/// Anthropic accepts `tool_result` blocks only as the leading run of the
/// user message after the assistant message holding their `tool_use`, and the
/// context projection gives every call exactly one result. An unknown or
/// duplicate result, and user content or a new assistant message while a call
/// still lacks its result, are refused before any request is sent; history is
/// never repaired by fabricating or reordering items.
fn messages<'a>(
    request: &'a ModelRequest,
    model: &str,
    compaction: Option<&'a RawJson>,
    oauth: bool,
) -> Result<Vec<Message<'a>>, ProviderError> {
    let mut messages: Vec<Message<'a>> = Vec::new();
    let mut turn = ToolTurn::default();
    for item in &*request.context {
        let (role, blocks) = match item {
            ContextItem::User { parts } => {
                let blocks = user_blocks(parts)?;
                if !blocks.is_empty() {
                    turn.require_settled("user content")?;
                }
                ("user", blocks)
            }
            ContextItem::Assistant { source, parts } => {
                let continues = messages.last().is_some_and(|last| last.role == "assistant");
                turn.assistant(parts, continues)?;
                ("assistant", assistant_blocks(parts, source, model, oauth))
            }
            ContextItem::ToolResult {
                call,
                is_error,
                parts,
                ..
            } => {
                turn.result(call.as_str())?;
                (
                    "user",
                    vec![Block::Typed(Typed::ToolResult {
                        tool_use_id: call.as_str(),
                        content: typed_parts(parts)?,
                        is_error: *is_error,
                        cache_control: None,
                    })],
                )
            }
        };
        if blocks.is_empty() {
            continue;
        }
        match messages.last_mut() {
            Some(last) if last.role == role => last.content.extend(blocks),
            _ => messages.push(Message {
                role,
                content: blocks,
            }),
        }
    }
    turn.require_settled("the end of the context")?;
    if let Some(block) = compaction {
        match messages.iter_mut().find(|message| message.role == "user") {
            Some(first) => first.content.insert(0, Block::Raw(block)),
            None => messages.insert(
                0,
                Message {
                    role: "user",
                    content: vec![Block::Raw(block)],
                },
            ),
        }
    }
    if let Some(slot) = messages.last_mut().and_then(|last| {
        last.content.iter_mut().rev().find_map(|block| match block {
            Block::Typed(typed) => Some(typed.cache_slot()),
            Block::Raw(_) => None,
        })
    }) {
        *slot = Some(CacheControl::Ephemeral);
    }
    Ok(messages)
}

/// The tool calls of the current wire assistant message and those still
/// waiting for their result.
#[derive(Default)]
struct ToolTurn<'a> {
    calls: Vec<&'a str>,
    pending: Vec<&'a str>,
}

impl<'a> ToolTurn<'a> {
    /// Records the calls of one assistant item; `continues` is true when the
    /// item merges into the previous wire assistant message.
    fn assistant(
        &mut self,
        parts: &'a [AssistantPart],
        continues: bool,
    ) -> Result<(), ProviderError> {
        if !continues {
            self.require_settled("the next assistant message")?;
            self.calls.clear();
        }
        for part in parts {
            if let AssistantPart::ToolCall { call, .. } = part {
                self.calls.push(call.as_str());
                self.pending.push(call.as_str());
            }
        }
        Ok(())
    }

    /// Settles the call a result answers.
    fn result(&mut self, call: &str) -> Result<(), ProviderError> {
        if let Some(index) = self.pending.iter().position(|pending| *pending == call) {
            self.pending.remove(index);
            return Ok(());
        }
        let problem = if self.calls.contains(&call) {
            "already has a result or is separated from its tool call by user content"
        } else {
            "has no matching tool call in the preceding assistant message"
        };
        Err(ProviderError::InvalidRequest {
            message: format!("tool result for call {call} {problem}"),
        })
    }

    /// Refuses `what` while a call of the current assistant message has no
    /// result.
    fn require_settled(&self, what: &str) -> Result<(), ProviderError> {
        match self.pending.first() {
            Some(call) => Err(ProviderError::InvalidRequest {
                message: format!("tool call {call} has no result before {what}"),
            }),
            None => Ok(()),
        }
    }
}

fn user_blocks(parts: &[Part]) -> Result<Vec<Block<'_>>, ProviderError> {
    Ok(typed_parts(parts)?.into_iter().map(Block::Typed).collect())
}

/// Text and image parts as typed blocks; empty text is dropped because the
/// API rejects empty text blocks.
fn typed_parts(parts: &[Part]) -> Result<Vec<Typed<'_>>, ProviderError> {
    let mut blocks = Vec::with_capacity(parts.len());
    for part in parts {
        match part {
            Part::Text { text } if text.is_empty() => {}
            Part::Text { text } => blocks.push(Typed::Text {
                text,
                cache_control: None,
            }),
            Part::Image { mime, bytes } => blocks.push(Typed::Image {
                source: ImageSource {
                    kind: "base64",
                    media_type: mime,
                    data: base64::engine::general_purpose::STANDARD.encode(bytes),
                },
                cache_control: None,
            }),
            Part::Blob { blob_id, .. } => {
                return Err(ProviderError::InvalidRequest {
                    message: format!(
                        "content blob {blob_id} must be inlined before the anthropic request is built"
                    ),
                });
            }
        }
    }
    Ok(blocks)
}

fn assistant_blocks(
    parts: &[AssistantPart],
    source: &ReplaySource,
    model: &str,
    oauth: bool,
) -> Vec<Block<'_>> {
    let replay_ok = source.family == Family::Anthropic && source.model.as_ref() == model;
    parts
        .iter()
        .filter_map(|part| match part {
            AssistantPart::Text { text } if text.is_empty() => None,
            AssistantPart::Text { text } => Some(Block::Typed(Typed::Text {
                text,
                cache_control: None,
            })),
            AssistantPart::Thinking { replay, .. } if replay_ok => replay
                .as_ref()
                .filter(|raw| replayable(raw))
                .map(Block::Raw),
            AssistantPart::Thinking { .. } => None,
            AssistantPart::ToolCall { call, name, args } => Some(Block::Typed(Typed::ToolUse {
                id: call.as_str(),
                name: wire_tool_name(name, oauth),
                input: if args.as_str().starts_with('{') {
                    Input::Raw(args)
                } else {
                    Input::Empty
                },
                cache_control: None,
            })),
        })
        .collect()
}

/// Whether a stored replay payload is an Anthropic block the API accepts
/// back: a `redacted_thinking` block, or a `thinking` block with a non-empty
/// signature.
fn replayable(raw: &RawJson) -> bool {
    let member = |name: &str| {
        sonic_rs::get_from_str(raw.as_str(), [name])
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
    };
    match member("type").as_deref() {
        Some("redacted_thinking") => true,
        Some("thinking") => member("signature").is_some_and(|signature| !signature.is_empty()),
        _ => false,
    }
}

/// Decodes the SSE events of one Anthropic response into neutral events.
///
/// `model` binds every [`ReplayPayload`]; `oauth` strips the Claude Code
/// tool prefix from tool names. The stream yields the events of the 5.1
/// grammar, then ends after `Stop` or after one error; input that ends
/// before `message_stop` yields [`ProviderError::StreamCut`].
pub(crate) fn decode_stream<S>(
    events: S,
    model: Box<str>,
    oauth: bool,
) -> impl Stream<Item = Result<StreamEvent, ProviderError>> + Send + 'static
where
    S: Stream<Item = Result<SseEvent, ProviderError>> + Send + 'static,
{
    let state = Pump {
        events: Some(Box::pin(events)),
        decoder: Decoder::new(model, oauth),
        queue: VecDeque::new(),
    };
    stream::unfold(state, |mut pump| async move {
        let item = pump.next().await?;
        Some((item, pump))
    })
}

struct Pump<S> {
    /// `None` once a terminal was queued; nothing is read after it.
    events: Option<std::pin::Pin<Box<S>>>,
    decoder: Decoder,
    queue: VecDeque<Result<StreamEvent, ProviderError>>,
}

impl<S> Pump<S>
where
    S: Stream<Item = Result<SseEvent, ProviderError>> + Send,
{
    async fn next(&mut self) -> Option<Result<StreamEvent, ProviderError>> {
        loop {
            if let Some(item) = self.queue.pop_front() {
                return Some(item);
            }
            let events = self.events.as_mut()?;
            let fed = match events.next().await {
                None => Err(ProviderError::StreamCut),
                Some(Err(error)) => Err(error),
                Some(Ok(event)) => self.decoder.feed(&event.data),
            };
            match fed {
                Ok(Fed { events, finished }) => {
                    self.queue.extend(events.into_iter().map(Ok));
                    if finished {
                        self.events = None;
                    }
                }
                Err(error) => {
                    self.queue.push_back(Err(error));
                    self.events = None;
                }
            }
        }
    }
}

/// The events one SSE event produced, and whether it was the terminal.
struct Fed {
    events: Vec<StreamEvent>,
    finished: bool,
}

/// One open content block.
enum OpenBlock {
    Text,
    Thinking { text: String, signature: String },
    Redacted(RawJson),
    ToolUse,
    Other,
}

/// One tool call, from `content_block_start` to the end of the message.
struct CallState {
    id: String,
    name: String,
    /// The `input` of `content_block_start`, the argument when no delta came.
    start_input: Option<RawJson>,
    args: Vec<u8>,
    saw_delta: bool,
}

/// Usage members as last reported; `None` when never reported.
#[derive(Default)]
struct UsageState {
    input: Option<u64>,
    output: Option<u64>,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
    thinking: Option<u64>,
}

impl UsageState {
    fn merge(&mut self, wire: WireUsage) {
        let WireUsage {
            input_tokens,
            output_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
            output_tokens_details,
        } = wire;
        let thinking = output_tokens_details.and_then(|details| details.thinking_tokens);
        for (slot, value) in [
            (&mut self.input, input_tokens),
            (&mut self.output, output_tokens),
            (&mut self.cache_read, cache_read_input_tokens),
            (&mut self.cache_write, cache_creation_input_tokens),
            (&mut self.thinking, thinking),
        ] {
            if value.is_some() {
                *slot = value;
            }
        }
    }

    /// The core counters: input counts every prompt token, cache reads and
    /// writes included; output excludes the separately reported thinking.
    fn usage(&self) -> Usage {
        let cache_read = self.cache_read.unwrap_or(0);
        let cache_write = self.cache_write.unwrap_or(0);
        Usage {
            input_tokens: self
                .input
                .unwrap_or(0)
                .saturating_add(cache_read)
                .saturating_add(cache_write),
            cached_input_tokens: cache_read,
            output_tokens: self
                .output
                .unwrap_or(0)
                .saturating_sub(self.thinking.unwrap_or(0)),
            reasoning_tokens: self.thinking,
            cache_write_tokens: cache_write,
            cost_usd: None,
        }
    }
}

struct Decoder {
    model: Box<str>,
    oauth: bool,
    usage: UsageState,
    stop: Option<StopReason>,
    open: BTreeMap<u64, OpenBlock>,
    calls: BTreeMap<u64, CallState>,
}

#[derive(Deserialize)]
struct WireUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    output_tokens_details: Option<OutputDetails>,
}

#[derive(Deserialize)]
struct OutputDetails {
    thinking_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct MessageStart {
    message: StartMessage,
}

#[derive(Deserialize)]
struct StartMessage {
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct BlockStart<'a> {
    index: u64,
    #[serde(borrow)]
    content_block: LazyValue<'a>,
}

#[derive(Deserialize)]
struct StartFields<'a> {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<String>,
    thinking: Option<String>,
    signature: Option<String>,
    id: Option<String>,
    name: Option<String>,
    #[serde(borrow)]
    input: Option<LazyValue<'a>>,
}

#[derive(Deserialize)]
struct BlockDelta {
    index: u64,
    delta: DeltaFields,
}

#[derive(Deserialize)]
struct DeltaFields {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<String>,
    thinking: Option<String>,
    signature: Option<String>,
    partial_json: Option<String>,
}

#[derive(Deserialize)]
struct BlockStop {
    index: u64,
}

#[derive(Deserialize)]
struct MessageDelta {
    delta: Option<StopDelta>,
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct StopDelta {
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct ErrorEvent {
    error: Option<ErrorBody>,
}

#[derive(Deserialize)]
struct ErrorBody {
    #[serde(rename = "type")]
    kind: Option<String>,
    message: Option<String>,
}

#[derive(Serialize)]
struct ThinkingReplay<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    thinking: &'a str,
    signature: &'a str,
}

fn protocol(detail: impl Into<String>) -> ProviderError {
    ProviderError::Protocol {
        family: Family::Anthropic,
        detail: detail.into(),
    }
}

fn parse<'a, T: Deserialize<'a>>(data: &'a str, kind: &str) -> Result<T, ProviderError> {
    sonic_rs::from_str(data).map_err(|error| protocol(format!("malformed {kind} event: {error}")))
}

/// Maps a wire `stop_reason`.
fn stop_reason(reason: &str) -> StopReason {
    match reason {
        "end_turn" | "stop_sequence" => StopReason::EndTurn,
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        "pause_turn" => StopReason::Paused,
        "refusal" => StopReason::Refusal,
        other => StopReason::Other(other.to_owned()),
    }
}

/// Maps an in-stream `error` event to the error its HTTP twin would give.
fn stream_error(body: Option<ErrorBody>) -> ProviderError {
    let (kind, message) = body.map_or((None, None), |body| (body.kind, body.message));
    let message = message.unwrap_or_default();
    if let Some(overflow) = kind
        .as_deref()
        .and_then(|code| ProviderError::context_overflow(Family::Anthropic, code, &message))
    {
        return overflow;
    }
    let status = match kind.as_deref() {
        Some("overloaded_error") => return ProviderError::Overloaded,
        Some("invalid_request_error") => 400,
        Some("authentication_error") => 401,
        Some("billing_error") => 402,
        Some("permission_error") => 403,
        Some("not_found_error") => 404,
        Some("conflict_error") => 409,
        Some("request_too_large") => 413,
        Some("rate_limit_error") => 429,
        Some("timeout_error") => 504,
        _ => 500,
    };
    ProviderError::Status {
        family: Family::Anthropic,
        status,
        message,
    }
}

impl Decoder {
    fn new(model: Box<str>, oauth: bool) -> Self {
        Self {
            model,
            oauth,
            usage: UsageState::default(),
            stop: None,
            open: BTreeMap::new(),
            calls: BTreeMap::new(),
        }
    }

    /// Consumes the data of one SSE event.
    fn feed(&mut self, data: &str) -> Result<Fed, ProviderError> {
        let mut events = Vec::new();
        if data.trim() == "[DONE]" {
            return Ok(Fed {
                events,
                finished: false,
            });
        }
        let kind = match sonic_rs::get_from_str(data, ["type"]) {
            Ok(kind) => kind,
            // An object without `type` is an unknown event: ignored.
            Err(error) if error.is_not_found() => {
                return Ok(Fed {
                    events,
                    finished: false,
                });
            }
            Err(error) => {
                return Err(protocol(format!(
                    "event data is not a JSON object: {error}"
                )));
            }
        };
        let finished = match kind.as_str() {
            Some("message_start") => {
                let start: MessageStart = parse(data, "message_start")?;
                if let Some(usage) = start.message.usage {
                    self.usage.merge(usage);
                }
                false
            }
            Some("content_block_start") => {
                self.block_start(&parse(data, "content_block_start")?, &mut events)?;
                false
            }
            Some("content_block_delta") => {
                self.block_delta(parse(data, "content_block_delta")?, &mut events)?;
                false
            }
            Some("content_block_stop") => {
                self.block_stop(&parse::<BlockStop>(data, "content_block_stop")?, &mut events)?;
                false
            }
            Some("message_delta") => {
                let delta: MessageDelta = parse(data, "message_delta")?;
                if let Some(usage) = delta.usage {
                    self.usage.merge(usage);
                }
                if let Some(reason) = delta.delta.and_then(|delta| delta.stop_reason) {
                    self.stop = Some(stop_reason(&reason));
                }
                false
            }
            Some("message_stop") => {
                self.finish(&mut events);
                true
            }
            Some("error") => return Err(stream_error(parse::<ErrorEvent>(data, "error")?.error)),
            _ => false,
        };
        Ok(Fed { events, finished })
    }

    fn block_start(
        &mut self,
        start: &BlockStart<'_>,
        events: &mut Vec<StreamEvent>,
    ) -> Result<(), ProviderError> {
        let index = start.index;
        if self.open.contains_key(&index) || self.calls.contains_key(&index) {
            return Err(protocol(format!("content block {index} started twice")));
        }
        let raw = start.content_block.as_raw_str();
        let fields: StartFields<'_> = parse(raw, "content_block_start")?;
        let block = match fields.kind.as_deref() {
            Some("text") => {
                push_text(events, fields.text, |text| StreamEvent::TextDelta { text });
                OpenBlock::Text
            }
            Some("thinking") => {
                let text = fields.thinking.unwrap_or_default();
                if !text.is_empty() {
                    events.push(StreamEvent::ReasoningDelta { text: text.clone() });
                }
                OpenBlock::Thinking {
                    text,
                    signature: fields.signature.unwrap_or_default(),
                }
            }
            Some("redacted_thinking") => OpenBlock::Redacted(
                RawJson::parse(raw)
                    .map_err(|error| protocol(format!("malformed redacted_thinking block: {error}")))?,
            ),
            Some("tool_use") => {
                let (Some(id), Some(name)) = (fields.id, fields.name) else {
                    return Err(protocol(format!(
                        "tool_use block {index} has no id or no name"
                    )));
                };
                let name = if self.oauth {
                    claude_fingerprint::decode_tool_name(&name).to_owned()
                } else {
                    name
                };
                let start_input = fields
                    .input
                    .map(|input| RawJson::parse(input.as_raw_str()))
                    .transpose()
                    .map_err(|error| protocol(format!("malformed tool_use input: {error}")))?;
                events.push(StreamEvent::ToolCallStarted {
                    id: id.clone(),
                    name: name.clone(),
                });
                self.calls.insert(
                    index,
                    CallState {
                        id,
                        name,
                        start_input,
                        args: Vec::new(),
                        saw_delta: false,
                    },
                );
                OpenBlock::ToolUse
            }
            _ => OpenBlock::Other,
        };
        self.open.insert(index, block);
        Ok(())
    }

    fn block_delta(
        &mut self,
        delta: BlockDelta,
        events: &mut Vec<StreamEvent>,
    ) -> Result<(), ProviderError> {
        let BlockDelta { index, delta } = delta;
        match delta.kind.as_deref() {
            Some("text_delta") => match self.open.get(&index) {
                Some(OpenBlock::Text) => {
                    push_text(events, delta.text, |text| StreamEvent::TextDelta { text });
                }
                Some(OpenBlock::Other) => {}
                Some(_) => {
                    return Err(protocol(format!(
                        "text_delta for block {index}, which is not an open text block"
                    )));
                }
                None => {
                    return Err(protocol(format!(
                        "text_delta for unopened block {index}"
                    )));
                }
            },
            Some("thinking_delta") => {
                let piece = delta.thinking.unwrap_or_default();
                match self.open.get_mut(&index) {
                    Some(OpenBlock::Thinking { text, .. }) => text.push_str(&piece),
                    Some(OpenBlock::Other) => return Ok(()),
                    Some(_) => {
                        return Err(protocol(format!(
                            "thinking_delta for block {index}, which is not an open thinking block"
                        )));
                    }
                    None => {
                        return Err(protocol(format!(
                            "thinking_delta for unopened block {index}"
                        )));
                    }
                }
                push_text(events, Some(piece), |text| StreamEvent::ReasoningDelta { text });
            }
            Some("signature_delta") => match self.open.get_mut(&index) {
                Some(OpenBlock::Thinking { signature, .. }) => {
                    *signature = delta.signature.unwrap_or_default();
                }
                Some(OpenBlock::Other) => {}
                Some(_) => {
                    return Err(protocol(format!(
                        "signature_delta for block {index}, which is not an open thinking block"
                    )));
                }
                None => {
                    return Err(protocol(format!(
                        "signature_delta for unopened block {index}"
                    )));
                }
            },
            Some("input_json_delta") => {
                if matches!(self.open.get(&index), Some(OpenBlock::Other)) {
                    return Ok(());
                }
                let call = self
                    .calls
                    .get_mut(&index)
                    .filter(|_| matches!(self.open.get(&index), Some(OpenBlock::ToolUse)))
                    .ok_or_else(|| {
                        protocol(format!(
                            "input_json_delta for block {index}, which is not an open tool_use block"
                        ))
                    })?;
                let fragment = delta.partial_json.unwrap_or_default().into_bytes();
                call.saw_delta = true;
                call.args.extend_from_slice(&fragment);
                if !fragment.is_empty() {
                    events.push(StreamEvent::ToolArgsDelta {
                        id: call.id.clone(),
                        fragment,
                    });
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn block_stop(
        &mut self,
        stop: &BlockStop,
        events: &mut Vec<StreamEvent>,
    ) -> Result<(), ProviderError> {
        let item = match self.open.remove(&stop.index) {
            Some(OpenBlock::Thinking { text, signature }) if !signature.is_empty() => {
                let replay = ThinkingReplay {
                    kind: "thinking",
                    thinking: &text,
                    signature: &signature,
                };
                let json = sonic_rs::to_string(&replay)
                    .map_err(|error| protocol(format!("thinking block did not encode: {error}")))?;
                RawJson::parse(&json)
                    .map_err(|error| protocol(format!("thinking block did not encode: {error}")))?
            }
            Some(OpenBlock::Redacted(raw)) => raw,
            _ => return Ok(()),
        };
        events.push(StreamEvent::Replay {
            payload: ReplayPayload {
                family: Family::Anthropic,
                model: self.model.clone(),
                item,
            },
        });
        Ok(())
    }

    /// Emits the terminal triple at `message_stop`.
    fn finish(&mut self, events: &mut Vec<StreamEvent>) {
        let reason = self
            .stop
            .take()
            .unwrap_or_else(|| StopReason::Other("none".to_owned()));
        let truncating = reason == StopReason::MaxTokens;
        let calls = mem::take(&mut self.calls)
            .into_values()
            .map(|call| ToolCall {
                args: call_args(&call, truncating),
                id: call.id,
                name: call.name,
            })
            .collect();
        events.push(StreamEvent::ToolCallsDone { calls });
        events.push(StreamEvent::Usage {
            usage: self.usage.usage(),
        });
        events.push(StreamEvent::Stop { reason });
    }
}

/// Pushes a non-empty text piece as one event.
fn push_text(
    events: &mut Vec<StreamEvent>,
    text: Option<String>,
    event: impl FnOnce(String) -> StreamEvent,
) {
    if let Some(text) = text.filter(|text| !text.is_empty()) {
        events.push(event(text));
    }
}

/// The final arguments of one call: the concatenated deltas when any came,
/// else the start `input`; empty text is `{}`. Under `max_tokens`, every call
/// has `Truncated` arguments because the response ended before all arguments
/// could be trusted.
fn call_args(call: &CallState, truncating: bool) -> ToolArgs {
    if truncating {
        return ToolArgs::Truncated;
    }
    let bytes: &[u8] = if call.saw_delta {
        &call.args
    } else {
        match &call.start_input {
            Some(raw) => return ToolArgs::Parsed(raw.clone()),
            None => b"",
        }
    };
    let bytes = if bytes.trim_ascii().is_empty() {
        b"{}".as_slice()
    } else {
        bytes
    };
    ToolArgs::from_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use dal_core::{
        CallId, ModelRoute, ModelToolSpec, Purpose, ReplaySource, RequestParams, ThinkingLevel,
    };
    use futures::executor::block_on;

    use super::*;
    use crate::sse;

    const BASIC_TEXT_STREAM: &str = concat!(
        "event: message_start\n",
        r#"data: {"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"claude-opus-5-5","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":25,"output_tokens":1}}}"#,
        "\n\nevent: content_block_start\n",
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        "\n\nevent: ping\n",
        r#"data: {"type":"ping"}"#,
        "\n\nevent: content_block_delta\n",
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
        "\n\nevent: content_block_delta\n",
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"!"}}"#,
        "\n\nevent: content_block_stop\n",
        r#"data: {"type":"content_block_stop","index":0}"#,
        "\n\nevent: message_delta\n",
        r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":15}}"#,
        "\n\nevent: message_stop\n",
        r#"data: {"type":"message_stop"}"#,
        "\n\n",
    );

    /// Frames `data` payloads as `event: <type>` SSE events.
    fn frames(datas: &[&str]) -> String {
        let mut wire = String::new();
        for data in datas {
            let kind = sonic_rs::get_from_str(data, ["type"])
                .ok()
                .and_then(|kind| kind.as_str().map(str::to_owned))
                .unwrap_or_default();
            wire.push_str(&format!("event: {kind}\ndata: {data}\n\n"));
        }
        wire
    }

    /// Decodes wire bytes delivered in 7-byte chunks, as a network would.
    fn decode(wire: &str, oauth: bool) -> Vec<Result<StreamEvent, ProviderError>> {
        let chunks: Vec<Vec<u8>> = wire.as_bytes().chunks(7).map(<[u8]>::to_vec).collect();
        let events = sse::decode_stream(stream::iter(chunks));
        block_on(decode_stream(events, "claude-sonnet-5".into(), oauth).collect())
    }

    fn assert_protocol_failure(wire: &str) {
        assert!(decode(wire, false).iter().any(|result| matches!(
            result,
            Err(ProviderError::Protocol {
                family: Family::Anthropic,
                ..
            })
        )));
    }

    fn ok(results: Vec<Result<StreamEvent, ProviderError>>) -> Vec<StreamEvent> {
        results.into_iter().map(Result::unwrap).collect()
    }

    fn text(text: &str) -> StreamEvent {
        StreamEvent::TextDelta { text: text.into() }
    }

    fn usage(input: u64, output: u64) -> StreamEvent {
        StreamEvent::Usage {
            usage: Usage {
                input_tokens: input,
                cached_input_tokens: 0,
                output_tokens: output,
                reasoning_tokens: None,
                cache_write_tokens: 0,
                cost_usd: None,
            },
        }
    }

    fn stop(reason: StopReason) -> StreamEvent {
        StreamEvent::Stop { reason }
    }

    fn calls(results: &[StreamEvent]) -> Vec<ToolCall> {
        results
            .iter()
            .find_map(|event| match event {
                StreamEvent::ToolCallsDone { calls } => Some(calls.clone()),
                _ => None,
            })
            .expect("ToolCallsDone")
    }

    fn parsed(raw: &str) -> ToolArgs {
        ToolArgs::Parsed(RawJson::parse(raw).unwrap())
    }

    const START: &str = r#"{"type":"message_start","message":{"usage":{"input_tokens":25,"output_tokens":1}}}"#;
    const STOP: &str = r#"{"type":"message_stop"}"#;

    #[test]
    fn basic_text_stream() {
        assert_eq!(
            ok(decode(BASIC_TEXT_STREAM, false)),
            vec![
                text("Hello"),
                text("!"),
                StreamEvent::ToolCallsDone { calls: vec![] },
                usage(25, 15),
                stop(StopReason::EndTurn),
            ]
        );
    }

    #[test]
    fn thinking_stream_replays_signed_block() {
        let wire = frames(&[
            START,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"1071 = 2 × 462 + 147"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"\n462 = 3 × 147 + 21"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"EqQBCgIYAhIM"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"GCD is 21."}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null}}"#,
            STOP,
        ]);
        let replay = RawJson::parse(
            r#"{"type":"thinking","thinking":"1071 = 2 × 462 + 147\n462 = 3 × 147 + 21","signature":"EqQBCgIYAhIM"}"#,
        )
        .unwrap();
        assert_eq!(
            ok(decode(&wire, false)),
            vec![
                StreamEvent::ReasoningDelta {
                    text: "1071 = 2 × 462 + 147".into()
                },
                StreamEvent::ReasoningDelta {
                    text: "\n462 = 3 × 147 + 21".into()
                },
                StreamEvent::Replay {
                    payload: ReplayPayload {
                        family: Family::Anthropic,
                        model: "claude-sonnet-5".into(),
                        item: replay,
                    }
                },
                text("GCD is 21."),
                StreamEvent::ToolCallsDone { calls: vec![] },
                usage(25, 1),
                stop(StopReason::EndTurn),
            ]
        );
    }

    #[test]
    fn empty_signature_is_never_replayed_and_redacted_is_verbatim() {
        let redacted = r#"{"type":"redacted_thinking","data":"EmwKAhgBEgy3va3pzix/LafPsn4a"}"#;
        let redacted_start = format!(
            r#"{{"type":"content_block_start","index":1,"content_block":{redacted}}}"#
        );
        let wire = frames(&[
            START,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hm"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":""}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            &redacted_start,
            r#"{"type":"content_block_stop","index":1}"#,
            STOP,
        ]);
        let events = ok(decode(&wire, false));
        let replays: Vec<&RawJson> = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::Replay { payload } => Some(&payload.item),
                _ => None,
            })
            .collect();
        assert_eq!(replays, vec![&RawJson::parse(redacted).unwrap()]);
        assert_eq!(events.last(), Some(&stop(StopReason::Other("none".into()))));
    }

    #[test]
    fn tool_arguments_split_mid_key_assemble_once() {
        let wire = frames(&[
            START,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_01","name":"get_weather","input":{}}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":""}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"locat"}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"ion\":\"San Fra"}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"ncisco, CA\"}"}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":891}}"#,
            STOP,
        ]);
        let events = ok(decode(&wire, false));
        assert_eq!(
            events[0],
            StreamEvent::ToolCallStarted {
                id: "toolu_01".into(),
                name: "get_weather".into()
            }
        );
        let fragments: Vec<u8> = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ToolArgsDelta { fragment, .. } => Some(fragment.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(fragments, br#"{"location":"San Francisco, CA"}"#);
        assert_eq!(
            calls(&events),
            vec![ToolCall {
                id: "toolu_01".into(),
                name: "get_weather".into(),
                args: parsed(r#"{"location":"San Francisco, CA"}"#),
            }]
        );
        assert_eq!(events.last(), Some(&stop(StopReason::ToolUse)));
    }

    #[test]
    fn start_input_without_deltas_is_the_argument() {
        let wire = frames(&[
            START,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_2","name":"_read","input":{"path":"a.rs", "n":1.50}}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"}}"#,
            STOP,
        ]);
        let events = ok(decode(&wire, true));
        assert_eq!(
            calls(&events),
            vec![ToolCall {
                id: "toolu_2".into(),
                name: "read".into(),
                args: parsed(r#"{"path":"a.rs", "n":1.50}"#),
            }]
        );
        assert!(!events
            .iter()
            .any(|event| matches!(event, StreamEvent::ToolArgsDelta { .. })));
    }

    #[test]
    fn text_and_tool_calls_both_arrive() {
        let wire = frames(&[
            START,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Reading."}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"t_a","name":"read","input":{}}}"#,
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"p\":1}"}}"#,
            r#"{"type":"content_block_stop","index":2}"#,
            r#"{"type":"content_block_start","index":3,"content_block":{"type":"tool_use","id":"t_b","name":"exec","input":{}}}"#,
            r#"{"type":"content_block_stop","index":3}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":40}}"#,
            STOP,
        ]);
        let events = ok(decode(&wire, false));
        assert_eq!(events[0], text("Reading."));
        assert_eq!(
            calls(&events),
            vec![
                ToolCall {
                    id: "t_a".into(),
                    name: "read".into(),
                    args: parsed(r#"{"p":1}"#),
                },
                ToolCall {
                    id: "t_b".into(),
                    name: "exec".into(),
                    args: parsed("{}"),
                },
            ]
        );
        assert_eq!(events.last(), Some(&stop(StopReason::ToolUse)));
    }

    #[test]
    fn text_and_thinking_deltas_require_the_matching_open_block() {
        let text_on_thinking = frames(&[
            START,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"misattributed"}}"#,
        ]);
        assert_protocol_failure(&text_on_thinking);

        let thinking_on_text = frames(&[
            START,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"misattributed"}}"#,
        ]);
        assert_protocol_failure(&thinking_on_text);

        let unopened = frames(&[
            START,
            r#"{"type":"content_block_delta","index":4,"delta":{"type":"text_delta","text":"misattributed"}}"#,
        ]);
        assert_protocol_failure(&unopened);
    }

    #[test]
    fn unknown_block_deltas_are_ignored() {
        let wire = frames(&[
            START,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"server","name":"web_search","input":{}}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ignored"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"ignored"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"ignored"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"ignored"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            STOP,
        ]);
        let events = ok(decode(&wire, false));
        assert!(!events.iter().any(|event| matches!(
            event,
            StreamEvent::TextDelta { .. }
                | StreamEvent::ReasoningDelta { .. }
                | StreamEvent::ToolCallStarted { .. }
                | StreamEvent::ToolArgsDelta { .. }
        )));
    }

    #[test]
    fn max_tokens_marks_every_tool_argument_truncated() {
        let wire = frames(&[
            START,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"start","name":"write","input":{"valid":true}}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"delta","name":"write","input":{}}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"valid\":true}"}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"partial","name":"write","input":{}}}"#,
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"body\":\"ab"}}"#,
            r#"{"type":"content_block_stop","index":2}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"}}"#,
            STOP,
        ]);
        let events = ok(decode(&wire, false));
        let calls = calls(&events);
        assert_eq!(calls.len(), 3);
        assert!(
            calls
                .iter()
                .all(|call| call.args == ToolArgs::Truncated)
        );
        assert_eq!(events.last(), Some(&stop(StopReason::MaxTokens)));
    }

    #[test]
    fn overloaded_error_ends_the_stream_before_or_after_delivery() {
        let overloaded =
            r#"{"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}"#;
        let first = decode(&frames(&[overloaded, STOP]), false);
        assert!(matches!(first.as_slice(), [Err(ProviderError::Overloaded)]));

        let wire = frames(&[
            START,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#,
            overloaded,
            STOP,
        ]);
        let late = decode(&wire, false);
        assert!(matches!(
            late.as_slice(),
            [Ok(StreamEvent::TextDelta { .. }), Err(ProviderError::Overloaded)]
        ));

        let other = decode(
            &frames(&[r#"{"type":"error","error":{"type":"api_error","message":"boom"}}"#]),
            false,
        );
        assert!(matches!(
            other.as_slice(),
            [Err(ProviderError::Status { status: 500, message, .. })] if message == "boom"
        ));
    }

    #[test]
    fn usage_is_start_overwritten_by_each_delta_member() {
        let wire = frames(&[
            r#"{"type":"message_start","message":{"usage":{"input_tokens":25,"cache_read_input_tokens":100,"cache_creation_input_tokens":50,"output_tokens":1}}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":null},"usage":{"output_tokens":15}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":891,"cache_read_input_tokens":null}}"#,
            STOP,
        ]);
        let events = ok(decode(&wire, false));
        assert_eq!(
            events[1],
            StreamEvent::Usage {
                usage: Usage {
                    input_tokens: 175,
                    cached_input_tokens: 100,
                    output_tokens: 891,
                    reasoning_tokens: None,
                    cache_write_tokens: 50,
                    cost_usd: None,
                }
            }
        );
        assert_eq!(events[2], stop(StopReason::EndTurn));
    }

    #[test]
    fn stop_reason_map() {
        for (wire, reason) in [
            ("pause_turn", StopReason::Paused),
            ("refusal", StopReason::Refusal),
            ("stop_sequence", StopReason::EndTurn),
            (
                "model_context_window_exceeded",
                StopReason::Other("model_context_window_exceeded".into()),
            ),
        ] {
            let delta = format!(r#"{{"type":"message_delta","delta":{{"stop_reason":"{wire}"}}}}"#);
            let events = ok(decode(&frames(&[START, &delta, STOP]), false));
            assert_eq!(events.last(), Some(&stop(reason)));
        }
        let events = ok(decode(&frames(&[START, STOP]), false));
        assert_eq!(events[1], usage(25, 1));
        assert_eq!(events[2], stop(StopReason::Other("none".into())));
    }

    #[test]
    fn end_of_input_before_message_stop_is_a_cut() {
        let cut = BASIC_TEXT_STREAM
            .rsplit_once("event: message_stop")
            .unwrap()
            .0;
        let results = decode(cut, false);
        assert!(matches!(results.last(), Some(Err(ProviderError::StreamCut))));
        assert!(!results
            .iter()
            .any(|event| matches!(event, Ok(StreamEvent::Stop { .. }))));
    }

    fn request(context: Vec<ContextItem>, tools: Vec<ModelToolSpec>) -> ModelRequest {
        ModelRequest {
            purpose: Purpose::Turn,
            model: ModelRoute::Api {
                family: Family::Anthropic,
                model: "claude-sonnet-5".into(),
            },
            system: Arc::from("Be brief."),
            tools: Arc::from(tools),
            context: Arc::from(context),
            params: RequestParams {
                thinking: ThinkingLevel::High,
                effort: None,
                temperature: None,
            },
            cache_key: None,
        }
    }

    fn tool(name: &str) -> ModelToolSpec {
        ModelToolSpec {
            name: name.into(),
            description: "d".into(),
            parameters: RawJson::parse(r#"{"type":"object","properties":{}}"#).unwrap(),
        }
    }

    fn replay_source(family: Family, model: &str) -> ReplaySource {
        ReplaySource {
            family,
            model: model.into(),
        }
    }

    fn history() -> Vec<ContextItem> {
        vec![
            ContextItem::User {
                parts: vec![Part::Text { text: "hi".into() }],
            },
            ContextItem::Assistant {
                source: replay_source(Family::Anthropic, "claude-sonnet-5"),
                parts: vec![
                    AssistantPart::Thinking {
                        text: "t".into(),
                        replay: Some(
                            RawJson::parse(
                                r#"{ "signature" : "S", "type" : "thinking", "thinking" : "t" }"#,
                            )
                            .unwrap(),
                        ),
                    },
                    AssistantPart::Thinking {
                        text: "u".into(),
                        replay: Some(
                            RawJson::parse(r#"{"type":"thinking","thinking":"u","signature":""}"#)
                                .unwrap(),
                        ),
                    },
                    AssistantPart::Text { text: "ok".into() },
                    AssistantPart::ToolCall {
                        call: CallId::new("toolu_1"),
                        name: "read".into(),
                        args: RawJson::parse(r#"{"path": "a", "n": 1.50}"#).unwrap(),
                    },
                    AssistantPart::ToolCall {
                        call: CallId::new("toolu_2"),
                        name: "web_search".into(),
                        args: RawJson::parse("[1]").unwrap(),
                    },
                ],
            },
            ContextItem::ToolResult {
                call: CallId::new("toolu_1"),
                name: "read".into(),
                is_error: false,
                parts: vec![
                    Part::Text { text: "x".into() },
                    Part::Image {
                        mime: "image/png".into(),
                        bytes: Box::new([1, 2, 3]),
                    },
                ],
            },
            ContextItem::ToolResult {
                call: CallId::new("toolu_2"),
                name: "web_search".into(),
                is_error: true,
                parts: vec![Part::Text { text: "no".into() }],
            },
        ]
    }

    fn header<'a>(wire: &'a AnthropicWire, name: &str) -> Option<&'a str> {
        wire.headers
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.as_str())
    }

    fn replay_body(source: ReplaySource) -> String {
        let request = request(
            vec![
                ContextItem::Assistant {
                    source,
                    parts: vec![
                        AssistantPart::Thinking {
                            text: "private".into(),
                            replay: Some(
                                RawJson::parse(
                                    r#"{ "type" : "thinking", "thinking" : "private", "signature" : "signed" }"#,
                                )
                                .unwrap(),
                            ),
                        },
                        AssistantPart::Text {
                            text: "visible".into(),
                        },
                        AssistantPart::ToolCall {
                            call: CallId::new("call_1"),
                            name: "read".into(),
                            args: RawJson::parse(r#"{"path":"notes.txt"}"#).unwrap(),
                        },
                    ],
                },
                ContextItem::ToolResult {
                    call: CallId::new("call_1"),
                    name: "read".into(),
                    is_error: false,
                    parts: vec![Part::Text { text: "done".into() }],
                },
            ],
            vec![tool("read")],
        );
        let input = AnthropicRequest {
            request: &request,
            max_output: None,
            thinking: AnthropicThinking::Omit,
            effort: None,
            display_supported: false,
            temperature: None,
            compaction: None,
            summarize: false,
        };
        String::from_utf8(build(&input, AnthropicAuth::ApiKey("sk-ant")).unwrap().body).unwrap()
    }

    fn assert_replay_was_filtered(body: &str) {
        assert!(body.contains(concat!(
            r#""role":"assistant","content":[{"type":"text","text":"visible"},"#,
            r#"{"type":"tool_use","id":"call_1","name":"read","input":{"path":"notes.txt"}}]}"#
        )));
        assert!(!body.contains("private"));
        assert!(!body.contains("signature"));
        assert!(!body.contains(r#""type":"thinking""#));
    }

    #[test]
    fn foreign_family_replay_is_omitted_without_losing_text_or_tool_calls() {
        let body = replay_body(replay_source(Family::Chat, "claude-sonnet-5"));
        assert_replay_was_filtered(&body);
    }

    #[test]
    fn different_model_replay_is_omitted_without_losing_text_or_tool_calls() {
        let body = replay_body(replay_source(Family::Anthropic, "claude-opus-5"));
        assert_replay_was_filtered(&body);
    }

    #[test]
    fn adaptive_display_is_omitted_when_unsupported() {
        let request = request(vec![], vec![]);
        let input = AnthropicRequest {
            request: &request,
            max_output: None,
            thinking: AnthropicThinking::Adaptive,
            effort: None,
            display_supported: false,
            temperature: None,
            compaction: None,
            summarize: false,
        };
        let body = String::from_utf8(
            build(&input, AnthropicAuth::ApiKey("sk-ant"))
                .unwrap()
                .body,
        )
        .unwrap();
        assert!(body.contains(r#""thinking":{"type":"adaptive"}"#));
        assert!(!body.contains(r#""display""#));
    }

    #[test]
    fn api_key_adaptive_body_is_exact() {
        let request = request(history(), vec![tool("read"), tool("web_search")]);
        let input = AnthropicRequest {
            request: &request,
            max_output: Some(64_000),
            thinking: AnthropicThinking::Adaptive,
            effort: Some(Effort::High),
            display_supported: true,
            temperature: None,
            compaction: None,
            summarize: false,
        };
        let wire = build(&input, AnthropicAuth::ApiKey("sk-ant")).unwrap();
        assert_eq!(
            wire.headers,
            vec![
                ("anthropic-version", "2023-06-01".to_owned()),
                ("x-api-key", "sk-ant".to_owned()),
            ]
        );
        assert_eq!(wire.user_agent, None);
        let body = String::from_utf8(wire.body).unwrap();
        let expected = concat!(
            r#"{"model":"claude-sonnet-5","max_tokens":32000,"stream":true,"#,
            r#""system":[{"type":"text","text":"Be brief.","cache_control":{"type":"ephemeral"}}],"#,
            r#""messages":[{"role":"user","content":[{"type":"text","text":"hi"}]},"#,
            r#"{"role":"assistant","content":[{ "signature" : "S", "type" : "thinking", "thinking" : "t" },"#,
            r#"{"type":"text","text":"ok"},"#,
            r#"{"type":"tool_use","id":"toolu_1","name":"read","input":{"path": "a", "n": 1.50}},"#,
            r#"{"type":"tool_use","id":"toolu_2","name":"web_search","input":{}}]},"#,
            r#"{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":[{"type":"text","text":"x"},"#,
            r#"{"type":"image","source":{"type":"base64","media_type":"image/png","data":"AQID"}}],"is_error":false},"#,
            r#"{"type":"tool_result","tool_use_id":"toolu_2","content":[{"type":"text","text":"no"}],"is_error":true,"cache_control":{"type":"ephemeral"}}]}],"#,
            r#""tools":[{"name":"read","description":"d","input_schema":{"type":"object","properties":{}}},"#,
            r#"{"name":"web_search","description":"d","input_schema":{"type":"object","properties":{}},"cache_control":{"type":"ephemeral"}}],"#,
            r#""tool_choice":{"type":"auto"},"thinking":{"type":"adaptive","display":"summarized"},"output_config":{"effort":"high"}}"#,
        );
        assert_eq!(body, expected);
        assert_eq!(body.matches("cache_control").count(), 3);
        let again = build(&input, AnthropicAuth::ApiKey("sk-ant")).unwrap();
        assert_eq!(again.body, expected.as_bytes());
    }

    #[test]
    fn claude_oauth_carries_the_reference_fingerprint() {
        let request = request(history(), vec![tool("read"), tool("web_search")]);
        let input = AnthropicRequest {
            request: &request,
            max_output: None,
            thinking: AnthropicThinking::Adaptive,
            effort: None,
            display_supported: true,
            temperature: None,
            compaction: None,
            summarize: false,
        };
        let auth = AnthropicAuth::ClaudeOAuth {
            access_token: "sk-ant-oat",
            version: claude_fingerprint::CLAUDE_CODE_VERSION,
        };
        let wire = build(&input, auth).unwrap();
        assert_eq!(
            wire.headers,
            vec![
                ("anthropic-version", "2023-06-01".to_owned()),
                ("authorization", "Bearer sk-ant-oat".to_owned()),
                (
                    "anthropic-beta",
                    "claude-code-20250219,oauth-2025-04-20".to_owned()
                ),
                ("x-app", "cli".to_owned()),
            ]
        );
        assert_eq!(
            wire.user_agent.as_deref(),
            Some("claude-cli/2.1.280 (external, cli)")
        );
        assert!(!format!("{wire:?}").contains("sk-ant-oat"));
        let body = String::from_utf8(wire.body).unwrap();
        assert!(body.contains(concat!(
            r#""system":[{"type":"text","text":"You are Claude Code, Anthropic's official CLI for Claude."},"#,
            r#"{"type":"text","text":"Be brief.","cache_control":{"type":"ephemeral"}}]"#
        )));
        assert!(body.contains(r#""name":"_read","input":{"path": "a", "n": 1.50}"#));
        assert!(body.contains(r#""tools":[{"name":"_read","#));
        assert!(body.contains(r#"{"name":"web_search","#));
        assert!(!body.contains("_web_search"));
    }

    #[test]
    fn bearer_budget_compaction_omits_temperature_with_enabled_thinking() {
        let block = RawJson::parse(r#"{"type":"compaction","content":"summary"}"#).unwrap();
        let context = vec![
            ContextItem::Assistant {
                source: replay_source(Family::Anthropic, "claude-sonnet-5"),
                parts: vec![AssistantPart::Text { text: "a".into() }],
            },
            ContextItem::User {
                parts: vec![
                    Part::Text { text: "q".into() },
                    Part::Text { text: String::new().into() },
                ],
            },
        ];
        let request = request(context, vec![]);
        let input = AnthropicRequest {
            request: &request,
            max_output: Some(4096),
            thinking: AnthropicThinking::Enabled {
                budget_tokens: 3072,
                max_tokens: 4096,
            },
            effort: None,
            display_supported: false,
            temperature: None,
            compaction: Some(&block),
            summarize: false,
        };
        let wire = build(&input, AnthropicAuth::Bearer("zm-key")).unwrap();
        assert_eq!(header(&wire, "authorization"), Some("Bearer zm-key"));
        assert_eq!(header(&wire, "x-api-key"), None);
        assert_eq!(
            header(&wire, "anthropic-beta"),
            Some("interleaved-thinking-2025-05-14,compact-2026-09-04")
        );
        let body = String::from_utf8(wire.body).unwrap();
        assert_eq!(
            body,
            concat!(
                r#"{"model":"claude-sonnet-5","max_tokens":4096,"stream":true,"#,
                r#""system":[{"type":"text","text":"Be brief.","cache_control":{"type":"ephemeral"}}],"#,
                r#""messages":[{"role":"assistant","content":[{"type":"text","text":"a"}]},"#,
                r#"{"role":"user","content":[{"type":"compaction","content":"summary"},{"type":"text","text":"q","cache_control":{"type":"ephemeral"}}]}],"#,
                r#""thinking":{"type":"enabled","budget_tokens":3072,"display":"summarized"}}"#,
            )
        );
    }

    #[test]
    fn unresolved_blob_is_refused() {
        let blob_id = dal_core::BlobId::from_bytes(&[7; 32]);
        let context = vec![ContextItem::User {
            parts: vec![Part::Blob {
                blob_id,
                mime: "image/png".into(),
                bytes: 32,
            }],
        }];
        let request = request(context, vec![]);
        let input = AnthropicRequest {
            request: &request,
            max_output: None,
            thinking: AnthropicThinking::Omit,
            effort: None,
            display_supported: false,
            temperature: None,
            compaction: None,
            summarize: false,
        };
        let Err(ProviderError::InvalidRequest { message }) =
            build(&input, AnthropicAuth::ApiKey("k"))
        else {
            panic!("an unresolved blob must be refused as an invalid request");
        };
        assert!(message.contains(&blob_id.to_string()));
    }

    fn call_item(ids: &[&str]) -> ContextItem {
        ContextItem::Assistant {
            source: replay_source(Family::Anthropic, "claude-sonnet-5"),
            parts: ids
                .iter()
                .map(|id| AssistantPart::ToolCall {
                    call: CallId::new(*id),
                    name: "read".into(),
                    args: RawJson::parse("{}").unwrap(),
                })
                .collect(),
        }
    }

    fn result_item(id: &str) -> ContextItem {
        ContextItem::ToolResult {
            call: CallId::new(id),
            name: "read".into(),
            is_error: false,
            parts: vec![Part::Text {
                text: format!("out-{id}").into(),
            }],
        }
    }

    fn user_item(text: &str) -> ContextItem {
        ContextItem::User {
            parts: vec![Part::Text { text: text.into() }],
        }
    }

    fn build_context(context: Vec<ContextItem>) -> Result<AnthropicWire, ProviderError> {
        let request = request(context, vec![]);
        let input = AnthropicRequest {
            request: &request,
            max_output: None,
            thinking: AnthropicThinking::Omit,
            effort: None,
            display_supported: false,
            temperature: None,
            compaction: None,
            summarize: false,
        };
        build(&input, AnthropicAuth::ApiKey("k"))
    }

    fn refusal(context: Vec<ContextItem>) -> String {
        let Err(ProviderError::InvalidRequest { message }) = build_context(context) else {
            panic!("the history must be refused as an invalid request");
        };
        message
    }

    #[test]
    fn two_results_lead_the_user_message_before_later_text() {
        let wire = build_context(vec![
            user_item("go"),
            call_item(&["toolu_a", "toolu_b"]),
            result_item("toolu_b"),
            result_item("toolu_a"),
            user_item("next"),
        ])
        .unwrap();
        let body = String::from_utf8(wire.body).unwrap();
        assert!(body.contains(concat!(
            r#"{"role":"user","content":["#,
            r#"{"type":"tool_result","tool_use_id":"toolu_b","content":[{"type":"text","text":"out-toolu_b"}],"is_error":false},"#,
            r#"{"type":"tool_result","tool_use_id":"toolu_a","content":[{"type":"text","text":"out-toolu_a"}],"is_error":false},"#,
            r#"{"type":"text","text":"next","cache_control":{"type":"ephemeral"}}]}"#,
        )));
    }

    #[test]
    fn tool_result_without_preceding_call_is_refused() {
        let message = refusal(vec![result_item("toolu_orphan")]);
        assert!(message.contains("toolu_orphan"));
    }

    #[test]
    fn duplicate_tool_result_is_refused() {
        let message = refusal(vec![
            call_item(&["toolu_a"]),
            result_item("toolu_a"),
            result_item("toolu_a"),
        ]);
        assert!(message.contains("toolu_a"));
        assert!(message.contains("already has a result"));
    }

    #[test]
    fn user_content_before_pending_result_is_refused() {
        let message = refusal(vec![
            call_item(&["toolu_a"]),
            user_item("steer"),
            result_item("toolu_a"),
        ]);
        assert!(message.contains("tool call toolu_a has no result before user content"));
    }

    #[test]
    fn adjacent_assistant_items_share_pending_calls() {
        let wire = build_context(vec![
            call_item(&["toolu_a"]),
            ContextItem::Assistant {
                source: replay_source(Family::Anthropic, "claude-sonnet-5"),
                parts: vec![AssistantPart::Text {
                    text: "more".into(),
                }],
            },
            result_item("toolu_a"),
        ]);
        assert!(wire.is_ok());
    }

    #[test]
    fn tool_call_without_result_at_end_is_refused() {
        let message = refusal(vec![
            user_item("go"),
            call_item(&["toolu_a", "toolu_b"]),
            result_item("toolu_a"),
        ]);
        assert!(message.contains("tool call toolu_b has no result before the end of the context"));
    }
}
