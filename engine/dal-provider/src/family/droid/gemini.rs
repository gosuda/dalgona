//! The Droid Gemini route: the proxy's native `generateContent` surface.
//!
//! `POST /api/llm/g/v1/generate` speaks Google's `generateContent` SSE, not
//! one of the shared wires, so this module owns both directions. The body
//! follows the pinned CLI traffic (`model`, `contents`, `systemInstruction`,
//! `generationConfig` with the temperature pin and `thinkingConfig`, and
//! `tools` with projected schemas); the decoder reads candidate 0 of every
//! chunk, folds `thought` parts into reasoning deltas, assembles
//! `functionCall` parts into tool calls with generated ids, and maps the
//! candidate `finishReason`.
//!
//! Two deliberate deviations from the pinned TypeScript, both documented at
//! the use site: content-filter blocks surface as the neutral `Refusal` stop
//! (the crate's grammar has it; the sibling Google decoder agrees) instead of
//! an error event, and a `functionCall` `thoughtSignature` has no dal channel
//! and is dropped, with the validator-skip sentinel covering the tail. Tool
//! schemas project through [`super::schema`].

use std::{collections::VecDeque, pin::Pin};

use base64::Engine as _;
use dal_core::{AssistantPart, ContextItem, Family, ModelRequest, Part, RawJson, ReplaySource};
use futures::{Stream, StreamExt, stream};
use serde::{Deserialize, Serialize};

use crate::{
    error::ProviderError,
    retry::{RequestState, RetryDecision, classify},
    sse::SseEvent,
    stream::{StopReason, StreamEvent, ToolArgs, ToolCall},
    stream_budget::StreamBudget,
    thinking::WireThinking,
    tool_names::{WIRE_NAME_MAX, wire_name},
};

use super::{DroidRequest, PROVIDER_ID, schema};

/// The signature that skips the validator on unsigned calls, verbatim from
/// the pinned `SKIP_THOUGHT_SIGNATURE`.
pub(crate) const SKIP_SENTINEL: &str = "skip_thought_signature_validator";

/// Narrows the request temperature to the body's `f32`.
///
/// The wire reads an `f32`; the request keeps an `f64`. The narrowing is the
/// wanted behavior: sampling precision beyond `f32` never reaches the model.
#[expect(clippy::cast_possible_truncation, reason = "the wire reads f32")]
fn narrow_f32(sample: f64) -> f32 {
    sample as f32
}

fn invalid(message: impl Into<String>) -> ProviderError {
    ProviderError::InvalidRequest {
        message: message.into(),
    }
}

// --- Request body -------------------------------------------------------

/// Lowers one request to the proxy's `generateContent` body.
///
/// # Errors
///
/// [`ProviderError::InvalidRequest`] for a non-OpenAI thinking plan, an
/// unread blob part, an unprojectable tool schema, or a body the JSON writer
/// rejects.
pub(crate) fn body(input: &DroidRequest<'_>) -> Result<Vec<u8>, ProviderError> {
    let WireThinking::OpenAi { effort } = input.thinking.wire else {
        return Err(invalid(
            "the thinking plan does not belong to the Droid route",
        ));
    };
    let request = input.request;
    let disabled = matches!(effort, None | Some("none"));
    let system = droid_system_text(request);
    let wire = GeminiBody {
        model: request.model.id(),
        contents: contents(request)?,
        system_instruction: Some(SystemInstruction {
            parts: vec![SystemPart { text: &system }],
        }),
        generation_config: GenerationConfig {
            temperature: request
                .params
                .temperature
                .map_or(super::COMPLETIONS_TEMPERATURE, narrow_f32),
            top_p: 0.95,
            top_k: 64,
            thinking_config: ThinkingConfig {
                include_thoughts: !disabled,
                thinking_level: if disabled {
                    None
                } else {
                    Some(level(effort, input.policy.gemini_medium))
                },
            },
        },
        tools: declarations(request)?,
    };
    sonic_rs::to_vec(&wire).map_err(|error| invalid(format!("droid gemini body failed: {error}")))
}

/// The Droid system text of the derived request; the prefix is always
/// present, so the instruction member never omits.
fn droid_system_text(request: &ModelRequest) -> String {
    super::droid_system(&request.system)
}

/// The `thinkingLevel` of one effort: `low` and `minimal` read `LOW`,
/// `medium` reads `MEDIUM` where the route accepts it and `HIGH` elsewhere,
/// and every higher rung reads `HIGH`.
fn level(effort: Option<&str>, medium: bool) -> &'static str {
    match effort {
        Some("low" | "minimal") => "LOW",
        Some("medium") if medium => "MEDIUM",
        _ => "HIGH",
    }
}

#[derive(Serialize)]
struct GeminiBody<'a> {
    model: &'a str,
    contents: Vec<Content<'a>>,
    #[serde(rename = "systemInstruction")]
    system_instruction: Option<SystemInstruction<'a>>,
    #[serde(rename = "generationConfig")]
    generation_config: GenerationConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<[Tools<'a>; 1]>,
}

#[derive(Serialize)]
struct GenerationConfig {
    temperature: f32,
    #[serde(rename = "topP")]
    top_p: f32,
    #[serde(rename = "topK")]
    top_k: u32,
    #[serde(rename = "thinkingConfig")]
    thinking_config: ThinkingConfig,
}

#[derive(Serialize)]
struct ThinkingConfig {
    #[serde(rename = "includeThoughts")]
    include_thoughts: bool,
    #[serde(rename = "thinkingLevel", skip_serializing_if = "Option::is_none")]
    thinking_level: Option<&'static str>,
}

#[derive(Serialize)]
struct Content<'a> {
    role: &'static str,
    parts: Vec<PartOut<'a>>,
}

#[derive(Serialize)]
struct SystemInstruction<'a> {
    parts: Vec<SystemPart<'a>>,
}

#[derive(Serialize)]
struct SystemPart<'a> {
    text: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PartOut<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    function_call: Option<FnCall<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    function_response: Option<FnResponse<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    inline_data: Option<InlineData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thought_signature: Option<std::borrow::Cow<'a, str>>,
}

#[derive(Serialize)]
struct FnCall<'a> {
    name: std::borrow::Cow<'a, str>,
    args: &'a RawJson,
}

#[derive(Serialize)]
struct FnResponse<'a> {
    name: std::borrow::Cow<'a, str>,
    response: FnResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    parts: Option<Vec<InlineData>>,
}

#[derive(Serialize)]
struct FnResult {
    result: String,
}

#[derive(Serialize)]
struct InlineData {
    #[serde(rename = "mimeType")]
    mime_type: String,
    data: String,
}

#[derive(Serialize)]
struct Tools<'a> {
    #[serde(rename = "functionDeclarations")]
    function_declarations: Vec<Declaration<'a>>,
}

#[derive(Serialize)]
struct Declaration<'a> {
    name: std::borrow::Cow<'a, str>,
    description: &'a str,
    parameters: schema::Node,
}

/// Renders the neutral context as Gemini history.
///
/// User turns carry text and image parts; assistant turns carry text,
/// signed thinking, and function calls; tool results group into one user
/// content per consecutive run, because the proxy rejects a call turn whose
/// response part count mismatches. Unsigned calls at or after the latest
/// user turn carry the validator-skip sentinel.
fn contents(request: &ModelRequest) -> Result<Vec<Content<'_>>, ProviderError> {
    let mut out: Vec<Content<'_>> = Vec::with_capacity(request.context.len());
    for item in request.context.iter() {
        match item {
            ContextItem::User { parts } => {
                if let Some(content) = user_content(parts)? {
                    out.push(content);
                }
            }
            ContextItem::Assistant { source, parts, .. } => {
                if let Some(content) = assistant_content(source, request.model.id(), parts) {
                    out.push(content);
                }
            }
            ContextItem::ToolResult { name, parts, .. } => {
                push_result(&mut out, name, parts)?;
            }
        }
    }
    skip_unsigned(&mut out);
    Ok(out)
}

fn user_content(parts: &[Part]) -> Result<Option<Content<'_>>, ProviderError> {
    let mut wire: Vec<PartOut<'_>> = Vec::new();
    for part in parts {
        match part {
            Part::Text { text } if !text.is_empty() => wire.push(PartOut {
                text: Some(text),
                function_call: None,
                function_response: None,
                inline_data: None,
                thought_signature: None,
            }),
            Part::Image { mime, bytes } => wire.push(PartOut {
                text: None,
                function_call: None,
                function_response: None,
                inline_data: Some(inline_data(mime, bytes)),
                thought_signature: None,
            }),
            Part::Text { .. } => {}
            Part::Blob { blob_id, .. } => {
                return Err(ProviderError::UnresolvedBlob { blob_id: *blob_id });
            }
        }
    }
    Ok((!wire.is_empty()).then_some(Content {
        role: "user",
        parts: wire,
    }))
}

fn assistant_content<'a>(
    source: &ReplaySource,
    model: &str,
    parts: &'a [AssistantPart],
) -> Option<Content<'a>> {
    let replay_ok = source.family == Family::Droid && source.model.as_ref() == model;
    let mut wire: Vec<PartOut<'a>> = Vec::new();
    for part in parts {
        match part {
            AssistantPart::Text { text, .. } if !text.is_empty() => wire.push(PartOut {
                text: Some(text),
                function_call: None,
                function_response: None,
                inline_data: None,
                thought_signature: None,
            }),
            AssistantPart::Thinking { text, replay } => {
                if text.is_empty() {
                    continue;
                }
                wire.push(PartOut {
                    text: Some(text),
                    function_call: None,
                    function_response: None,
                    inline_data: None,
                    thought_signature: replay_signature(replay.as_ref(), replay_ok),
                });
            }
            AssistantPart::ToolCall {
                name,
                args,
                replay,
                call: _,
            } => wire.push(PartOut {
                text: None,
                function_call: Some(FnCall {
                    name: wire_name(name, WIRE_NAME_MAX),
                    args,
                }),
                function_response: None,
                inline_data: None,
                thought_signature: replay_signature(replay.as_ref(), replay_ok),
            }),
            AssistantPart::Text { .. } => {}
        }
    }
    (!wire.is_empty()).then_some(Content {
        role: "model",
        parts: wire,
    })
}

/// The replayed signature of one thinking or call part, when the source
/// binding allows it.
fn replay_signature(
    replay: Option<&RawJson>,
    replay_ok: bool,
) -> Option<std::borrow::Cow<'_, str>> {
    let raw = replay_ok.then_some(replay).flatten()?;
    let signed: ThoughtSignature = sonic_rs::from_str(raw.as_str()).ok()?;
    signed
        .thought_signature
        .filter(|signature| !signature.is_empty())
        .map(std::borrow::Cow::Owned)
}

#[derive(Deserialize)]
struct ThoughtSignature {
    #[serde(default, rename = "thoughtSignature")]
    thought_signature: Option<String>,
}

fn push_result<'a>(
    contents: &mut Vec<Content<'a>>,
    name: &'a str,
    parts: &[Part],
) -> Result<(), ProviderError> {
    let mut texts: Vec<&str> = Vec::new();
    let mut images: Vec<InlineData> = Vec::new();
    for part in parts {
        match part {
            Part::Text { text } if !text.is_empty() => texts.push(text),
            Part::Image { mime, bytes } => images.push(inline_data(mime, bytes)),
            Part::Text { .. } => {}
            Part::Blob { blob_id, .. } => {
                return Err(ProviderError::UnresolvedBlob { blob_id: *blob_id });
            }
        }
    }
    let result = if texts.is_empty() {
        match images.len() {
            0 => String::from("Tool execution succeeded."),
            count => format!("Binary content provided ({count} item(s))."),
        }
    } else {
        texts.join("\n")
    };
    let response = PartOut {
        text: None,
        function_call: None,
        function_response: Some(FnResponse {
            name: wire_name(name, WIRE_NAME_MAX),
            response: FnResult { result },
            parts: (!images.is_empty()).then_some(images),
        }),
        inline_data: None,
        thought_signature: None,
    };
    match contents.last_mut() {
        Some(current)
            if current.role == "user"
                && !current.parts.is_empty()
                && current
                    .parts
                    .iter()
                    .all(|part| part.function_response.is_some()) =>
        {
            current.parts.push(response);
        }
        _ => contents.push(Content {
            role: "user",
            parts: vec![response],
        }),
    }
    Ok(())
}

fn inline_data(mime: &str, bytes: &[u8]) -> InlineData {
    InlineData {
        mime_type: mime.to_owned(),
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
    }
}

/// Marks unsigned calls at or after the latest user turn with the
/// validator-skip sentinel. The validator only checks the current tail, so
/// earlier calls keep whatever signature they replayed.
fn skip_unsigned(contents: &mut [Content]) {
    let tail = contents
        .iter()
        .rposition(|content| {
            content.role == "user"
                && content
                    .parts
                    .iter()
                    .any(|part| part.function_response.is_none())
        })
        .map_or(0, |index| index + 1);
    for content in &mut contents[tail..] {
        if content.role != "model" {
            continue;
        }
        for part in &mut content.parts {
            let unsigned = part.function_call.is_some()
                && part
                    .thought_signature
                    .as_ref()
                    .is_none_or(|signature| signature.is_empty());
            if unsigned {
                part.thought_signature = Some(std::borrow::Cow::Borrowed(SKIP_SENTINEL));
            }
        }
    }
}

fn declarations(request: &ModelRequest) -> Result<Option<[Tools<'_>; 1]>, ProviderError> {
    if request.tools.is_empty() {
        return Ok(None);
    }
    let mut function_declarations = Vec::with_capacity(request.tools.len());
    for tool in request.tools.iter() {
        let parameters = schema::project(&tool.parameters).map_err(|error| {
            invalid(format!(
                "tool `{}` parameters do not project: {error}",
                tool.name
            ))
        })?;
        function_declarations.push(Declaration {
            name: wire_name(&tool.name, WIRE_NAME_MAX),
            description: tool.description.as_ref(),
            parameters,
        });
    }
    Ok(Some([Tools {
        function_declarations,
    }]))
}

// --- Stream decoding ----------------------------------------------------

/// Decodes a Gemini `generateContent` SSE stream into neutral events.
///
/// Candidate 0 only. Text parts become text deltas, `thought` parts become
/// reasoning deltas, and a part carrying a `thoughtSignature` also yields a
/// replay event bound to this family and model. `functionCall` parts open a
/// tool call under a generated id with their raw arguments; a call without
/// arguments closes as `{}`. A stream that ends with neither a `finishReason`
/// nor a block reason ends as [`ProviderError::StreamCut`].
pub(crate) fn decode<S>(
    events: S,
    model: &str,
) -> impl Stream<Item = Result<StreamEvent, ProviderError>> + Send + 'static
where
    S: Stream<Item = Result<SseEvent, ProviderError>> + Send + 'static,
{
    let pump = Pump {
        events: Box::pin(events),
        decoder: Decoder::new(model),
        queue: VecDeque::new(),
    };
    stream::unfold(pump, |mut pump| async move {
        let item = pump.next().await?;
        Some((item, pump))
    })
}

#[derive(Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "stream decoder tracks four independent terminal latches"
)]
struct Decoder {
    model: Box<str>,
    tag: String,
    budget: StreamBudget,
    calls: Vec<OpenCall>,
    finish_reason: Option<String>,
    blocked: bool,
    usage: Option<dal_core::Usage>,
    emitted: bool,
    ended: bool,
    finished: bool,
}

#[derive(Debug)]
struct OpenCall {
    id: String,
    name: String,
    args: Vec<u8>,
}

const NO_USAGE: dal_core::Usage = dal_core::Usage {
    input_tokens: 0,
    cached_input_tokens: 0,
    output_tokens: 0,
    reasoning_tokens: None,
    cache_write_tokens: 0,
    cost_usd: None,
    premium_requests: None,
};

#[derive(Deserialize)]
struct Chunk {
    #[serde(default)]
    candidates: Vec<Candidate>,
    #[serde(default, rename = "usageMetadata")]
    usage_metadata: Option<UsageMeta>,
    #[serde(default, rename = "promptFeedback")]
    prompt_feedback: Option<PromptFeedback>,
    #[serde(default)]
    error: Option<WireError>,
}

#[derive(Deserialize)]
struct Candidate {
    #[serde(default)]
    content: Option<ChunkContent>,
    #[serde(default, rename = "finishReason")]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChunkContent {
    #[serde(default)]
    parts: Vec<ChunkPart>,
}

#[derive(Deserialize)]
struct ChunkPart {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    thought: Option<bool>,
    #[serde(default, rename = "functionCall")]
    function_call: Option<FnCallChunk>,
    #[serde(default, rename = "thoughtSignature")]
    thought_signature: Option<String>,
}

#[derive(Deserialize)]
struct FnCallChunk {
    #[serde(default)]
    name: String,
    #[serde(default)]
    args: Option<RawJson>,
}

#[derive(Deserialize)]
#[expect(
    clippy::struct_field_names,
    reason = "mirrors Gemini wire usage fields"
)]
struct UsageMeta {
    #[serde(default, rename = "promptTokenCount")]
    prompt_token_count: Option<u64>,
    #[serde(default, rename = "candidatesTokenCount")]
    candidates_token_count: Option<u64>,
    #[serde(default, rename = "thoughtsTokenCount")]
    thoughts_token_count: Option<u64>,
    #[serde(default, rename = "totalTokenCount")]
    total_token_count: Option<u64>,
    #[serde(default, rename = "cachedContentTokenCount")]
    cached_content_token_count: Option<u64>,
}

#[derive(Deserialize)]
struct PromptFeedback {
    #[serde(default, rename = "blockReason")]
    block_reason: Option<String>,
}

#[derive(Deserialize)]
struct WireError {
    #[serde(default)]
    code: Option<u32>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

impl Decoder {
    fn new(model: &str) -> Self {
        Self {
            model: Box::from(model),
            tag: response_tag(),
            budget: StreamBudget::default(),
            calls: Vec::new(),
            finish_reason: None,
            blocked: false,
            usage: None,
            emitted: false,
            ended: false,
            finished: false,
        }
    }

    fn feed(&mut self, data: &str, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        if self.ended {
            return Ok(());
        }
        let start = out.len();
        let result = self
            .budget
            .admit_event(data.len())
            .and_then(|()| self.step(data, out));
        self.emitted |= out.len() > start;
        if result.is_err() {
            self.ended = true;
        }
        result
    }

    /// Ends the input: a response with a `finishReason` or a block reason
    /// finishes; anything else was cut.
    ///
    /// # Errors
    ///
    /// [`ProviderError::StreamCut`] when no `finishReason` arrived.
    fn end(&mut self, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        if self.finished {
            return Ok(());
        }
        if self.finish_reason.is_none() && !self.blocked {
            return Err(ProviderError::StreamCut);
        }
        let result = self.finish(out);
        if result.is_ok() {
            self.finished = true;
        }
        result
    }

    fn step(&mut self, data: &str, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        let chunk: Chunk = sonic_rs::from_str(data)
            .map_err(|error| protocol(format!("droid gemini chunk is not valid: {error}")))?;
        if let Some(error) = chunk.error {
            return Err(self.server_error(error));
        }
        if let Some(usage) = chunk.usage_metadata.as_ref() {
            self.usage = Some(normalize_usage(usage));
        }
        if chunk
            .prompt_feedback
            .is_some_and(|feedback| feedback.block_reason.is_some())
        {
            self.blocked = true;
        }
        let mut candidate = chunk.candidates.into_iter().next();
        if let Some(reason) = candidate
            .as_mut()
            .and_then(|candidate| candidate.finish_reason.take())
        {
            self.finish_reason = Some(reason);
        }
        let parts = candidate
            .and_then(|candidate| candidate.content)
            .map(|content| content.parts)
            .unwrap_or_default();
        for part in parts {
            self.part(part, out)?;
        }
        Ok(())
    }

    fn part(&mut self, part: ChunkPart, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        if let Some(call) = part.function_call {
            self.function_call(call, out)?;
        }
        let text = part.text.unwrap_or_default();
        if let Some(signature) = part
            .thought_signature
            .filter(|signature| !signature.is_empty())
        {
            push_replay(&self.model, &text, &signature, out);
        }
        if text.is_empty() {
            return Ok(());
        }
        if part.thought.unwrap_or(false) {
            out.push(StreamEvent::ReasoningDelta { text });
        } else {
            out.push(StreamEvent::TextDelta { text });
        }
        Ok(())
    }

    fn function_call(
        &mut self,
        call: FnCallChunk,
        out: &mut Vec<StreamEvent>,
    ) -> Result<(), ProviderError> {
        let name = wire_name(&call.name, WIRE_NAME_MAX).into_owned();
        let args = call
            .args
            .map(|args| args.as_str().as_bytes().to_vec())
            .unwrap_or_default();
        let id = format!("{}-{}", self.tag, self.calls.len());
        self.budget.admit_item()?;
        out.push(StreamEvent::ToolCallStarted {
            id: id.clone(),
            name: name.clone(),
        });
        if !args.is_empty() {
            out.push(StreamEvent::ToolArgsDelta {
                id: id.clone(),
                fragment: args.clone(),
            });
        }
        self.calls.push(OpenCall { id, name, args });
        Ok(())
    }

    /// Emits the terminal group. Any tool call wins over the finish reason;
    /// otherwise a block reason refuses, and the mapped `finishReason`
    /// decides. A `MALFORMED_FUNCTION_CALL` is a protocol violation.
    fn finish(&mut self, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        if !self.calls.is_empty() {
            self.close_calls(crate::stream::StopReason::ToolUse, out);
            return Ok(());
        }
        if self.blocked {
            self.close_calls(crate::stream::StopReason::Refusal, out);
            return Ok(());
        }
        let reason = match self.finish_reason.take().as_deref() {
            Some("STOP") | None => StopReason::EndTurn,
            Some("MAX_TOKENS") => StopReason::MaxTokens,
            Some(
                "SAFETY"
                | "BLOCKLIST"
                | "PROHIBITED_CONTENT"
                | "SPII"
                | "IMAGE_SAFETY"
                | "IMAGE_PROHIBITED_CONTENT",
            ) => StopReason::Refusal,
            Some("MALFORMED_FUNCTION_CALL") => {
                return Err(protocol(
                    "generation failed with finish reason: MALFORMED_FUNCTION_CALL",
                ));
            }
            Some(other) => StopReason::Other(String::from(other)),
        };
        self.close_calls(reason, out);
        Ok(())
    }

    fn close_calls(&mut self, reason: StopReason, out: &mut Vec<StreamEvent>) {
        let calls = std::mem::take(&mut self.calls)
            .into_iter()
            .map(|call| ToolCall {
                id: call.id,
                name: call.name,
                args: if call.args.is_empty() {
                    ToolArgs::from_bytes(b"{}")
                } else {
                    ToolArgs::from_bytes(&call.args)
                },
            })
            .collect();
        out.push(StreamEvent::ToolCallsDone { calls });
        out.push(StreamEvent::Usage {
            usage: self.usage.take().unwrap_or(NO_USAGE),
        });
        out.push(StreamEvent::Stop { reason });
    }

    /// Maps an in-band `error` frame through the retry table. The decoder
    /// returns only the error value; the request lifecycle owns retries.
    fn server_error(&self, error: WireError) -> ProviderError {
        let message = error.message.unwrap_or_default();
        let status = error.code.and_then(|code| u16::try_from(code).ok());
        let code = error.status.as_deref();
        let request = RequestState {
            family: Family::Droid,
            provider: PROVIDER_ID,
            model: &self.model,
            oauth: true,
            refreshed: false,
            delivered: self.emitted,
        };
        match classify(status, code, &message, &request) {
            RetryDecision::Retry(error) | RetryDecision::Fail(error) => error,
            RetryDecision::RefreshOnce => ProviderError::SignInExpired {
                provider: String::from(PROVIDER_ID),
            },
        }
    }
}

/// The usage counters of one chunk, mapped onto the core counters the way
/// the pinned `mapGoogleUsage` does: the prompt count falls back to the
/// total minus candidates and thinking, and cached input is bounded by the
/// prompt.
fn normalize_usage(meta: &UsageMeta) -> dal_core::Usage {
    let candidates = meta.candidates_token_count.unwrap_or(0);
    let thoughts = meta.thoughts_token_count.unwrap_or(0);
    let total = meta.total_token_count.unwrap_or(0);
    let prompt = meta
        .prompt_token_count
        .unwrap_or_else(|| total.saturating_sub(candidates + thoughts));
    let cached = meta.cached_content_token_count.unwrap_or(0).min(prompt);
    dal_core::Usage {
        input_tokens: prompt - cached,
        cached_input_tokens: cached,
        output_tokens: candidates + thoughts,
        reasoning_tokens: (thoughts > 0).then_some(thoughts),
        cache_write_tokens: 0,
        cost_usd: None,
        premium_requests: None,
    }
}

/// One signature-bearing part, kept verbatim for replay: the text it signs
/// and the signature, bound to this family and model by the caller.
fn push_replay(model: &str, text: &str, signature: &str, out: &mut Vec<StreamEvent>) {
    let item = sonic_rs::to_vec(&ThoughtReplay {
        text,
        thought_signature: signature,
    })
    .ok()
    .and_then(|bytes| String::from_utf8(bytes).ok())
    .and_then(|json| RawJson::parse(&json).ok());
    if let Some(item) = item {
        out.push(StreamEvent::Replay {
            payload: crate::stream::ReplayPayload {
                family: Family::Droid,
                model: Box::from(model),
                item,
            },
        });
    }
}

#[derive(Serialize)]
struct ThoughtReplay<'a> {
    text: &'a str,
    #[serde(rename = "thoughtSignature")]
    thought_signature: &'a str,
}

/// The per-response call-id tag: twelve hex digits, unique per response so
/// generated call ids never collide across turns.
fn response_tag() -> String {
    uuid::Uuid::new_v4()
        .as_simple()
        .to_string()
        .chars()
        .take(12)
        .collect()
}

fn protocol(detail: impl Into<String>) -> ProviderError {
    ProviderError::Protocol {
        family: Family::Droid,
        detail: detail.into(),
    }
}

struct Pump<S> {
    events: Pin<Box<S>>,
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
            if self.decoder.ended {
                return None;
            }
            let mut out = Vec::new();
            let result = match self.events.next().await {
                Some(Ok(event)) => self.decoder.feed(&event.data, &mut out),
                Some(Err(error)) => {
                    self.decoder.ended = true;
                    Err(error)
                }
                None => {
                    self.decoder.ended = true;
                    self.decoder.end(&mut out)
                }
            };
            self.queue.extend(out.into_iter().map(Ok));
            if let Err(error) = result {
                self.queue.push_back(Err(error));
            }
        }
    }
}
