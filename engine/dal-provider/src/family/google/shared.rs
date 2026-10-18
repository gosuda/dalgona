//! `GenerateContent` lowering and stream decoding shared by the Google HTTP
//! surfaces: direct Gemini API, Vertex AI, and Antigravity's Cloud Code
//! Assist envelope.
//!
//! The request side follows oh-my-pi `google-shared.ts`: `convertMessages`
//! (:161), `convertTools` (:354), `buildGoogleGenerateContentParams`, and
//! `paramsToWireBody` — one [`request_body`] per wire dialect. The decoder
//! side follows `consumeGoogleStream`: candidate index 0 only, text and
//! thought parts fold into deltas, each `functionCall` part opens one tool
//! call, `usageMetadata` maps onto the core counters (`mapGoogleUsage`), and
//! the last `finishReason` decides the terminal (`mapStopReason` /
//! `mapStopReasonString`).
//!
//! Thought signatures are the Google reasoning-replay surface: a signature
//! rides text, thought, and functionCall parts alike, and all of them replay.
//! One response emits at most one [`StreamEvent::Replay`], before its
//! `ToolCallsDone`, carrying every signature of the response in one carrier:
//! `calls` maps each signed call id to its signature, and `parts` lists the
//! signed text and thought blocks. The session folds a replay event into a
//! thinking part, the only per-part replay channel a stream has, so a block
//! is named by its place and size rather than by position in the journal: the
//! n-th visible block of its kind with the same byte length (`GoogleTextBlocks`
//! keeps the same blocks in the reference). A signature replays only onto a
//! block that still matches, and only into a request for the family and model
//! that produced it.
//!
//! Per-surface wire policy that the pin derives from model and provider
//! rules (`supportsFunctionPartId`, `requiresSkipThoughtSignature*`,
//! `multimodalFunctionResponse`, Gemini 3 generation gating) lives in
//! [`Compat`]; each surface module builds the policy for its model id.

use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
    sync::{Arc, Mutex},
};

use dal_core::{
    AssistantPart, BlobId, CallId, ContextItem, Family, ModelRequest, ModelRoute, Part, RawJson,
    Usage,
};
use futures::{Stream, StreamExt, stream};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::schema::{Spelling, project};
use crate::{
    error::{ProviderError, capped_message_line, family_label},
    http,
    retry::{RequestState, RetryDecision, classify},
    sse::SseEvent,
    stream::{ReplayPayload, StopReason, StreamEvent, ToolArgs, ToolCall},
    stream_budget::StreamBudget,
    thinking::{AnthropicThinking, Effort, ThinkingPlan, WireThinking},
    tool_names::{WIRE_NAME_MAX, wire_name},
};

/// The bypass sentinel the public Gemini API requires on an unsigned
/// functionCall part, and CCA requires on the first unsigned call of a
/// message (`SKIP_THOUGHT_SIGNATURE` in the reference).
pub(crate) const SKIP_THOUGHT_SIGNATURE: &str = "skip_thought_signature_validator";

/// The longest tool-call id kept on the wire; the `normalizeToolCallId`
/// helper in the reference caps provider ids at 64 characters.
const CALL_ID_MAX: usize = 64;

/// The text a text-only model receives in place of every image of a turn
/// (`NON_VISION_IMAGE_PLACEHOLDER` in the reference).
const NON_VISION_IMAGE_PLACEHOLDER: &str = "[image omitted: model does not support vision]";

/// One request that cannot be lowered to a Google body.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum GoogleBodyError {
    /// The request route is not a model route of the calling family.
    #[error("a {want} request needs a {want} model route, not `{route}`")]
    NotGoogle {
        /// The family label the request was lowered for.
        want: &'static str,
        /// The route id of the request.
        route: Box<str>,
    },
    /// A content part still refers to stored content; the caller resolves
    /// blobs to inline bytes before lowering.
    #[error("content blob {blob_id} must be read into the request before a Google body is built")]
    UnresolvedBlob {
        /// The unresolved blob.
        blob_id: BlobId,
    },
    /// The model id cannot be one path segment of a request URL.
    #[error("model id {model:?} is not a plain provider model id")]
    ModelId {
        /// The rejected id.
        model: Box<str>,
    },
    /// The JSON writer failed.
    #[error("google body encoding failed: {message}")]
    Encode {
        /// The writer message.
        message: String,
    },
}

impl From<GoogleBodyError> for ProviderError {
    fn from(error: GoogleBodyError) -> Self {
        Self::InvalidRequest {
            message: error.to_string(),
        }
    }
}

/// Returns the provider-native model id of `request` when its route belongs
/// to `family`.
///
/// # Errors
///
/// [`GoogleBodyError::NotGoogle`] for any other route.
pub(crate) fn route_model(request: &ModelRequest, family: Family) -> Result<&str, GoogleBodyError> {
    match &request.model {
        ModelRoute::Api {
            family: route_family,
            model,
        } if *route_family == family => Ok(model.as_ref()),
        _ => Err(GoogleBodyError::NotGoogle {
            want: family_label(family),
            route: request.model.id().into(),
        }),
    }
}

/// Whether the model id is safe to interpolate into a request path: provider
/// ids over `[A-Za-z0-9._-]`, without tuned-model path segments or query
/// characters.
#[must_use]
pub(crate) fn plain_model_id(model: &str) -> bool {
    !model.is_empty()
        && model
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

// ---------------------------------------------------------------------------
// Per-surface wire policy
// ---------------------------------------------------------------------------

/// How one surface treats unsigned tool calls: the
/// `requiresSkipThoughtSignature*` compat axes in the pin.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SignaturePolicy {
    /// Every unsigned call carries the bypass sentinel (direct Gemini API,
    /// Gemini 3+).
    Always,
    /// Only the first unsigned call of the message carries the sentinel
    /// (Cloud Code Assist Gemini routes).
    FirstCall,
    /// No sentinel ever (Vertex AI rejects it; pre-3 models carry neither
    /// sentinel nor ids).
    Never,
}

/// Whether `toolConfig` constrains tool choice on the surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ToolMode {
    /// The wire default (`AUTO`); the member is omitted.
    Auto,
    /// Antigravity's default tool mode, verified for Gemini and Claude.
    Validated,
}

/// The per-surface wire policy: which id fields travel, when the signature
/// bypass applies, and which schema spelling tools use.
#[derive(Clone, Copy, Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "per-surface wire capability switches for GenerateContent variants"
)]
pub(crate) struct Compat {
    /// `functionCall.id` / `functionResponse.id` ride both parts.
    pub(crate) part_ids: bool,
    /// Which unsigned calls carry the bypass sentinel.
    pub(crate) skip: SignaturePolicy,
    /// Tool-result images nest inside `functionResponse.parts`.
    pub(crate) nested_tool_images: bool,
    /// Claude legacy tool schema: `parameters` instead of
    /// `parametersJsonSchema`.
    pub(crate) legacy_parameters: bool,
    /// Thinking blocks replay through the thought slot.
    pub(crate) replay_thinking: bool,
    /// The surface's `toolConfig` mode.
    pub(crate) tool_mode: ToolMode,
}

/// A Gemini generation number parsed from a model id for wire policy.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct Generation(pub(crate) Option<u32>);

impl Generation {
    /// Whether the id names Gemini 3 or later, the generation that takes
    /// `thinkingLevel`, function-part ids, and multimodal responses.
    #[must_use]
    pub(crate) fn is_three_or_later(self) -> bool {
        self.0.is_some_and(|generation| generation >= 3)
    }
}

/// Parses the generation out of a Gemini model id (`gemini-3-pro` →
/// `Some(3)`); anything else is `None`, which sorts below every known
/// generation so unknown ids take the conservative path.
#[must_use]
pub(crate) fn generation_of(model: &str) -> Generation {
    let rest = model
        .strip_prefix("gemini-")
        .or_else(|| model.strip_prefix("models/gemini-"));
    let Some(rest) = rest else {
        return Generation(None);
    };
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    Generation((!digits.is_empty()).then(|| digits.parse().ok()).flatten())
}

/// Whether the model id routes Claude content (the pin's
/// `model.identity.class === "anthropic"` check, by wire-id convention).
#[must_use]
pub(crate) fn is_claude_model(model: &str) -> bool {
    model.starts_with("claude-")
}

// ---------------------------------------------------------------------------
// Request lowering
// ---------------------------------------------------------------------------

/// One conversation turn of the `contents` array.
#[derive(Serialize)]
struct Content<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<&'static str>,
    parts: Vec<PartWire<'a>>,
}

/// One piece of a turn. The wire discriminates by the member that is present;
/// `thought` marks reasoning text, and `thoughtSignature` rides any part kind.
#[derive(Serialize, Default)]
struct PartWire<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<Cow<'a, str>>,
    #[serde(skip_serializing_if = "is_false")]
    thought: bool,
    #[serde(rename = "thoughtSignature", skip_serializing_if = "Option::is_none")]
    thought_signature: Option<Cow<'a, str>>,
    #[serde(rename = "inlineData", skip_serializing_if = "Option::is_none")]
    inline_data: Option<InlineData<'a>>,
    #[serde(rename = "functionCall", skip_serializing_if = "Option::is_none")]
    function_call: Option<FunctionCall<'a>>,
    #[serde(rename = "functionResponse", skip_serializing_if = "Option::is_none")]
    function_response: Option<FunctionResponse<'a>>,
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde skip_serializing_if requires a reference predicate"
)]
fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Serialize)]
struct InlineData<'a> {
    #[serde(rename = "mimeType")]
    mime_type: &'a str,
    data: Cow<'a, str>,
}

#[derive(Serialize)]
struct FunctionCall<'a> {
    name: Cow<'a, str>,
    /// Raw argument JSON, written verbatim.
    args: &'a RawJson,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Cow<'a, str>>,
}

#[derive(Serialize)]
struct FunctionResponse<'a> {
    name: Cow<'a, str>,
    /// `{"output": <text>}` on success, `{"error": <text>}` on failure,
    /// per the SDK's documented keys.
    response: ResponseValue<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parts: Option<Vec<PartWire<'a>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Cow<'a, str>>,
}

#[derive(Serialize)]
struct ResponseValue<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<Cow<'a, str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Cow<'a, str>>,
}

/// A function declaration inside `tools[].functionDeclarations`.
#[derive(Serialize)]
struct Declaration<'a> {
    name: Cow<'a, str>,
    description: &'a str,
    /// The legacy spelling Antigravity requires; mutually exclusive with
    /// `parametersJsonSchema`.
    #[serde(skip_serializing_if = "Option::is_none")]
    parameters: Option<Cow<'a, RawJson>>,
    /// The tool's schema projected onto what the wire accepts.
    #[serde(
        rename = "parametersJsonSchema",
        skip_serializing_if = "Option::is_none"
    )]
    parameters_json_schema: Option<Cow<'a, RawJson>>,
}

#[derive(Serialize)]
struct ToolGroup<'a> {
    #[serde(rename = "functionDeclarations")]
    function_declarations: Vec<Declaration<'a>>,
}

/// The `thinkingConfig` member. Gemini 3 carries the level by name; a budget
/// is the 2.x spelling and is never sent when the plan names a level.
#[derive(Serialize)]
struct ThinkingConfig {
    #[serde(rename = "includeThoughts")]
    include_thoughts: bool,
    #[serde(rename = "thinkingLevel", skip_serializing_if = "Option::is_none")]
    thinking_level: Option<&'static str>,
    #[serde(rename = "thinkingBudget", skip_serializing_if = "Option::is_none")]
    thinking_budget: Option<u32>,
}

/// The sampling members shared by both body shapes.
#[derive(Serialize, Default)]
struct GenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(rename = "maxOutputTokens", skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
    #[serde(rename = "thinkingConfig", skip_serializing_if = "Option::is_none")]
    thinking_config: Option<ThinkingConfig>,
}

impl GenerationConfig {
    fn is_empty(&self) -> bool {
        self.temperature.is_none()
            && self.max_output_tokens.is_none()
            && self.thinking_config.is_none()
    }
}

/// The flat `GenerateContent` body (direct Gemini API and Vertex AI).
///
/// Member order mirrors `paramsToWireBody`: root members first, then the
/// `generationConfig` object with `thinkingConfig` last inside it.
#[derive(Serialize)]
struct Body<'a> {
    contents: Vec<Content<'a>>,
    #[serde(rename = "systemInstruction", skip_serializing_if = "Option::is_none")]
    system_instruction: Option<Content<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolGroup<'a>>>,
    #[serde(rename = "toolConfig", skip_serializing_if = "Option::is_none")]
    tool_config: Option<ToolConfig>,
    #[serde(rename = "safetySettings", skip_serializing_if = "Option::is_none")]
    safety_settings: Option<&'static [SafetySetting]>,
    #[serde(rename = "generationConfig", skip_serializing_if = "Option::is_none")]
    generation_config: Option<GenerationConfig>,
}

#[derive(Serialize)]
struct SafetySetting {
    category: &'static str,
    threshold: &'static str,
}

/// The Vertex AI safety defaults: all four harm categories off, matching the
/// reference `prepare()` exactly.
const SAFETY_SETTINGS_OFF: &[SafetySetting] = &[
    SafetySetting {
        category: "HARM_CATEGORY_HATE_SPEECH",
        threshold: "OFF",
    },
    SafetySetting {
        category: "HARM_CATEGORY_DANGEROUS_CONTENT",
        threshold: "OFF",
    },
    SafetySetting {
        category: "HARM_CATEGORY_SEXUALLY_EXPLICIT",
        threshold: "OFF",
    },
    SafetySetting {
        category: "HARM_CATEGORY_HARASSMENT",
        threshold: "OFF",
    },
];

#[derive(Serialize)]
struct ToolConfig {
    #[serde(rename = "functionCallingConfig")]
    function_calling_config: CallingConfig,
}

#[derive(Serialize)]
struct CallingConfig {
    mode: &'static str,
}

/// The Cloud Code Assist envelope (Antigravity).
///
/// Member order mirrors the reference `buildRequest`: `request` holds
/// `contents`, `systemInstruction`, `tools`, `toolConfig`, `labels`,
/// `generationConfig`, then `sessionId`; the envelope reads `project`,
/// `requestId`, `request`, `model`, `userAgent`, `requestType`.
#[derive(Serialize)]
struct CcaBody<'a> {
    project: &'a str,
    #[serde(rename = "requestId")]
    request_id: &'a str,
    request: CcaRequest<'a>,
    model: &'a str,
    #[serde(rename = "userAgent")]
    user_agent: &'static str,
    #[serde(rename = "requestType")]
    request_type: &'static str,
}

#[derive(Serialize)]
struct CcaRequest<'a> {
    contents: Vec<Content<'a>>,
    #[serde(rename = "systemInstruction", skip_serializing_if = "Option::is_none")]
    system_instruction: Option<Content<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolGroup<'a>>>,
    #[serde(rename = "toolConfig", skip_serializing_if = "Option::is_none")]
    tool_config: Option<ToolConfig>,
    labels: Labels<'a>,
    #[serde(rename = "generationConfig", skip_serializing_if = "Option::is_none")]
    generation_config: Option<GenerationConfig>,
    #[serde(rename = "sessionId")]
    session_id: &'a str,
}

/// The CCA `labels` map, serialized as a JSON object in insertion order.
struct Labels<'a>(&'a [(String, String)]);

impl Serialize for Labels<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

/// Everything one request body needs besides the neutral request itself.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GoogleRequest<'a> {
    /// The family the request lowers for; names errors and the replay gate.
    pub(crate) family: Family,
    /// The neutral request; its route id is the wire model name.
    pub(crate) request: &'a ModelRequest,
    /// The thinking fragment from the composed plan.
    pub(crate) thinking: &'a ThinkingPlan,
    /// The surface's wire policy for its model id.
    pub(crate) compat: Compat,
    /// Whether the model accepts image input (the catalog entry's
    /// `image_input`). A text-only model gets
    /// [`NON_VISION_IMAGE_PLACEHOLDER`] text in place of every image.
    pub(crate) image_input: bool,
    /// The wire dialect to lower to.
    pub(crate) dialect: Dialect<'a>,
}

/// Which wire dialect one request lowers to.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Dialect<'a> {
    /// Flat `generateContent` body: the direct Gemini API and Vertex AI.
    Flat {
        /// Send the four `HARM_CATEGORY_*` safety settings with threshold
        /// `OFF` (Vertex AI only).
        safety_off: bool,
    },
    /// The Cloud Code Assist envelope (Antigravity).
    CloudCodeAssist {
        /// The Google Cloud project id the request names.
        project: &'a str,
        /// The CCA `sessionId` member.
        session_id: &'a str,
        /// The envelope `requestId` member.
        request_id: &'a str,
        /// The envelope `userAgent` member (`"antigravity"`).
        user_agent: &'static str,
        /// The `labels` attached inside `request`, in insertion order.
        labels: &'a [(String, String)],
        /// The `generationConfig.maxOutputTokens` override (the
        /// per-wire-id Antigravity profile); `None` falls back to the
        /// request's `max_output_tokens`.
        max_output_tokens: Option<u32>,
    },
}

/// Lowers one request to the JSON body its dialect's transport POSTs.
///
/// The system text becomes `systemInstruction` (a `user`-role turn on the CCA
/// envelope, role-free elsewhere, per the reference). User messages carry
/// text and inline-data parts; assistant messages bound to this family and
/// model carry text, thought, and function-call parts with their stored
/// signatures; foreign thinking is never sent. Tool results become
/// `functionResponse` turns merged when consecutive, with their images
/// following in one user turn (nested in the response on Gemini 3+). Empty
/// turns are dropped. The plan's effort becomes the Gemini 3 `thinkingLevel`
/// spelling, or the 2.x `thinkingBudget` when the plan carries a budget; the
/// plan's temperature rides the sampling config.
///
/// # Errors
///
/// [`GoogleBodyError::NotGoogle`] for a route of another family,
/// [`GoogleBodyError::ModelId`] for a model id that is not one path segment
/// of a flat-dialect request URL, [`GoogleBodyError::UnresolvedBlob`] for a
/// blob part, and [`GoogleBodyError::Encode`] when the writer fails.
pub(crate) fn request_body(input: &GoogleRequest<'_>) -> Result<Vec<u8>, GoogleBodyError> {
    let model = route_model(input.request, input.family)?;
    // The flat dialect carries the model id in the request path.
    if matches!(input.dialect, Dialect::Flat { .. }) && !plain_model_id(model) {
        return Err(GoogleBodyError::ModelId {
            model: model.into(),
        });
    }
    let contents = contents(input, model)?;
    let generation = GenerationConfig {
        temperature: input.thinking.temperature,
        max_output_tokens: match input.dialect {
            Dialect::Flat { .. } => input.request.params.max_output_tokens,
            Dialect::CloudCodeAssist {
                max_output_tokens, ..
            } => max_output_tokens.or(input.request.params.max_output_tokens),
        },
        thinking_config: thinking_config(input),
    };
    let generation = (!generation.is_empty()).then_some(generation);
    let tools = tools(input);
    let tool_config = tool_config(input.compat.tool_mode);
    let body = match input.dialect {
        Dialect::Flat { safety_off } => sonic_rs::to_vec(&Body {
            contents,
            system_instruction: system_instruction(input, None),
            tools,
            tool_config,
            safety_settings: safety_off.then_some(SAFETY_SETTINGS_OFF),
            generation_config: generation,
        })
        .map_err(encode)?,
        Dialect::CloudCodeAssist {
            project,
            session_id,
            request_id,
            user_agent,
            labels,
            ..
        } => sonic_rs::to_vec(&CcaBody {
            project,
            request_id,
            request: CcaRequest {
                contents,
                system_instruction: system_instruction(input, Some("user")),
                tools,
                tool_config,
                labels: Labels(labels),
                generation_config: generation,
                session_id,
            },
            model,
            user_agent,
            request_type: "agent",
        })
        .map_err(encode)?,
    };
    Ok(body)
}

#[expect(clippy::needless_pass_by_value, reason = "convenience map_err adapter")]
fn encode(error: sonic_rs::Error) -> GoogleBodyError {
    GoogleBodyError::Encode {
        message: error.to_string(),
    }
}

/// Maps the composed plan onto `thinkingConfig`. An OpenAI-cap plan names
/// the Gemini 3 `thinkingLevel` spelling; a budget plan (Gemini 2.x, or
/// Claude behind Antigravity) names `thinkingBudget`. `off` omits the
/// member: the server default then applies, and models that cannot disable
/// thinking are clamped upstream by the capability row.
fn thinking_config(input: &GoogleRequest<'_>) -> Option<ThinkingConfig> {
    match input.thinking.wire {
        WireThinking::OpenAi { effort } => match effort {
            Some("minimal") => Some(level("MINIMAL")),
            Some("low") => Some(level("LOW")),
            Some("medium") => Some(level("MEDIUM")),
            Some("high" | "xhigh" | "max") => Some(level("HIGH")),
            _ => None,
        },
        WireThinking::Anthropic { thinking, effort } => match thinking {
            AnthropicThinking::Omit => None,
            AnthropicThinking::Disabled => Some(ThinkingConfig {
                include_thoughts: false,
                thinking_level: None,
                thinking_budget: None,
            }),
            AnthropicThinking::Adaptive => Some(ThinkingConfig {
                include_thoughts: true,
                thinking_level: Some(match effort {
                    None | Some(Effort::Low) => "LOW",
                    Some(Effort::Medium) => "MEDIUM",
                    Some(Effort::High | Effort::Xhigh | Effort::Max) => "HIGH",
                }),
                thinking_budget: None,
            }),
            AnthropicThinking::Enabled { budget_tokens, .. } => Some(ThinkingConfig {
                include_thoughts: true,
                thinking_level: None,
                thinking_budget: Some(budget_tokens),
            }),
        },
    }
}

fn level(name: &'static str) -> ThinkingConfig {
    ThinkingConfig {
        include_thoughts: true,
        thinking_level: Some(name),
        thinking_budget: None,
    }
}

fn system_instruction<'a>(
    input: &GoogleRequest<'a>,
    cca_role: Option<&'static str>,
) -> Option<Content<'a>> {
    let system = input.request.system.as_ref();
    if system.is_empty() {
        return None;
    }
    Some(Content {
        role: cca_role,
        parts: vec![PartWire {
            text: Some(Cow::Borrowed(system)),
            ..PartWire::default()
        }],
    })
}

fn tools<'a>(input: &GoogleRequest<'a>) -> Option<Vec<ToolGroup<'a>>> {
    if input.request.tools.is_empty() {
        return None;
    }
    let spelling = if input.compat.legacy_parameters {
        Spelling::Legacy
    } else {
        Spelling::Modern
    };
    let declarations = input
        .request
        .tools
        .iter()
        .map(|tool| {
            let projected = project(&tool.parameters, spelling);
            let (parameters, parameters_json_schema) = match spelling {
                Spelling::Legacy => (Some(projected), None),
                Spelling::Modern => (None, Some(projected)),
            };
            Declaration {
                name: wire_name(&tool.name, WIRE_NAME_MAX),
                description: &tool.description,
                parameters,
                parameters_json_schema,
            }
        })
        .collect();
    Some(vec![ToolGroup {
        function_declarations: declarations,
    }])
}

fn tool_config(mode: ToolMode) -> Option<ToolConfig> {
    match mode {
        ToolMode::Auto => None,
        ToolMode::Validated => Some(ToolConfig {
            function_calling_config: CallingConfig { mode: "VALIDATED" },
        }),
    }
}

fn contents<'a>(
    input: &GoogleRequest<'a>,
    model: &'a str,
) -> Result<Vec<Content<'a>>, GoogleBodyError> {
    let mut contents: Vec<Content<'a>> = Vec::with_capacity(input.request.context.len() + 1);
    // The wire name of every call of the turn, for `functionResponse` name
    // resolution (`emittedToolCallNames` in the reference).
    let mut calls: BTreeMap<&'a str, Cow<'a, str>> = BTreeMap::new();
    // Tool-result images follow the merged functionResponse turn in one user
    // turn, the Gemini < 3 shape (`pendingToolImageParts` in the reference).
    let mut tool_images: Vec<PartWire<'a>> = Vec::new();
    let mut ids = CallIds::default();
    for item in input.request.context.iter() {
        if !matches!(item, ContextItem::ToolResult { .. }) {
            flush_tool_images(&mut contents, &mut tool_images);
        }
        match item {
            ContextItem::User { parts } => {
                let parts = user_parts(parts, input.image_input)?;
                if !parts.is_empty() {
                    contents.push(Content {
                        role: Some("user"),
                        parts,
                    });
                }
            }
            ContextItem::Assistant { source, parts, .. } => {
                // Only the model's own parts replay: stored signatures are
                // bound to the producing family and model
                // (`resolveThoughtSignature` in the reference).
                let bound = source.family == input.family && source.model.as_ref() == model;
                let batch = assistant_parts(parts, input, bound, &mut calls, &mut ids);
                if !batch.is_empty() {
                    contents.push(Content {
                        role: Some("model"),
                        parts: batch,
                    });
                }
            }
            ContextItem::ToolResult {
                call,
                name,
                is_error,
                parts,
            } => {
                let (part, mut images) =
                    tool_result(call, name, *is_error, parts, &calls, &mut ids, input)?;
                // All function responses of one run merge into one user turn:
                // the backend rejects a response count that differs from the
                // call count of the turn.
                match contents.last_mut() {
                    Some(Content {
                        role: Some("user"),
                        parts,
                    }) if parts.iter().any(|part| part.function_response.is_some()) => {
                        parts.push(part);
                    }
                    _ => contents.push(Content {
                        role: Some("user"),
                        parts: vec![part],
                    }),
                }
                if !images.is_empty() {
                    tool_images.push(PartWire {
                        text: Some(Cow::Borrowed("Tool result image:")),
                        ..PartWire::default()
                    });
                    tool_images.append(&mut images);
                }
            }
        }
    }
    flush_tool_images(&mut contents, &mut tool_images);
    Ok(contents)
}

/// One inline image part.
fn inline_image<'a>(mime: &'a str, bytes: &[u8]) -> PartWire<'a> {
    PartWire {
        inline_data: Some(InlineData {
            mime_type: mime,
            data: Cow::Owned(base64_encode(bytes)),
        }),
        ..PartWire::default()
    }
}

/// The user parts of one turn. A model without vision gets one placeholder
/// text after the turn's other parts in place of every image.
fn user_parts(parts: &[Part], image_input: bool) -> Result<Vec<PartWire<'_>>, GoogleBodyError> {
    let mut wire = Vec::with_capacity(parts.len());
    let mut omitted = false;
    for part in parts {
        match part {
            // Empty user turns draw Google 400s; skip them.
            Part::Text { text } if text.trim().is_empty() => {}
            Part::Text { text } => wire.push(PartWire {
                text: Some(Cow::Borrowed(text)),
                ..PartWire::default()
            }),
            Part::Image { .. } if !image_input => omitted = true,
            Part::Image { mime, bytes } => wire.push(inline_image(mime, bytes)),
            Part::Blob { blob_id, .. } => {
                return Err(GoogleBodyError::UnresolvedBlob { blob_id: *blob_id });
            }
        }
    }
    if omitted {
        wire.push(PartWire {
            text: Some(Cow::Borrowed(NON_VISION_IMAGE_PLACEHOLDER)),
            ..PartWire::default()
        });
    }
    Ok(wire)
}

fn assistant_parts<'a>(
    parts: &'a [AssistantPart],
    input: &GoogleRequest<'a>,
    bound: bool,
    calls: &mut BTreeMap<&'a str, Cow<'a, str>>,
    ids: &mut CallIds<'a>,
) -> Vec<PartWire<'a>> {
    let mut wire = Vec::with_capacity(parts.len());
    // Stored signatures replay only onto the model's own parts: an unbound
    // message has an empty carrier.
    let mut carrier = carrier_of(parts, bound);
    // The reference requires the bypass sentinel on the first unsigned call
    // of the message on CCA routes (`isFirstToolCall`).
    let mut first = true;
    // The place of the next visible block of each kind, as the decoder counted.
    let (mut text_index, mut thought_index) = (0, 0);
    for part in parts {
        match part {
            AssistantPart::Text { text, .. } => {
                if text.trim().is_empty() {
                    continue;
                }
                let signature = carrier.take_block(Kind::Text, text_index, text.len());
                text_index += 1;
                wire.push(PartWire {
                    text: Some(Cow::Borrowed(text)),
                    thought_signature: signature.map(Cow::Owned),
                    ..PartWire::default()
                });
            }
            AssistantPart::Thinking { text, .. } => {
                if text.trim().is_empty() {
                    continue;
                }
                let signature = carrier.take_block(Kind::Thought, thought_index, text.len());
                thought_index += 1;
                // The model's own thinking replays in the thought slot even
                // unsigned; foreign thinking is never sent.
                if input.compat.replay_thinking && bound {
                    wire.push(PartWire {
                        text: Some(Cow::Borrowed(text)),
                        thought: true,
                        thought_signature: signature.map(Cow::Owned),
                        ..PartWire::default()
                    });
                }
            }
            AssistantPart::ToolCall {
                call, name, args, ..
            } => {
                let wire_name = wire_name(name, WIRE_NAME_MAX);
                calls.insert(call.as_str(), wire_name.clone());
                let id = input
                    .compat
                    .part_ids
                    .then(|| Cow::Owned(ids.call(call.as_str())));
                wire.push(PartWire {
                    function_call: Some(FunctionCall {
                        name: wire_name,
                        args,
                        id,
                    }),
                    thought_signature: effective_signature(
                        input.compat.skip,
                        &mut carrier,
                        call.as_str(),
                        first,
                    )
                    .map(Cow::Owned),
                    ..PartWire::default()
                });
                first = false;
            }
        }
    }
    wire
}

/// The replay carriers of one assistant message merged into one, or an empty
/// carrier when the message is not bound to this family and model.
fn carrier_of(parts: &[AssistantPart], bound: bool) -> Carrier {
    let mut carrier = Carrier::default();
    if !bound {
        return carrier;
    }
    for part in parts {
        if let AssistantPart::Thinking {
            replay: Some(raw), ..
        } = part
            && let Ok(parsed) = sonic_rs::from_str::<Carrier>(raw.as_str())
        {
            carrier.calls.extend(parsed.calls);
            carrier.parts.extend(parsed.parts);
        }
    }
    carrier
}

/// The signature one functionCall part carries: the stored signature when it
/// is valid base64, else the bypass sentinel per the surface policy.
fn effective_signature(
    policy: SignaturePolicy,
    carrier: &mut Carrier,
    call: &str,
    first: bool,
) -> Option<String> {
    if let Some(signature) = carrier.take_call(call) {
        return Some(signature);
    }
    match policy {
        SignaturePolicy::Always => Some(SKIP_THOUGHT_SIGNATURE.to_owned()),
        SignaturePolicy::FirstCall if first => Some(SKIP_THOUGHT_SIGNATURE.to_owned()),
        SignaturePolicy::FirstCall | SignaturePolicy::Never => None,
    }
}

/// Whether the signature is well-formed base64 (`isValidThoughtSignature` in
/// the reference: non-empty, length a multiple of 4, base64 alphabet).
fn is_valid_signature(signature: &str) -> bool {
    !signature.is_empty()
        && signature.len().is_multiple_of(4)
        && signature
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
}

/// One tool result as a `functionResponse` part, and the images that follow it
/// in their own turn when the surface does not nest them. A model without
/// vision gets the placeholder text in the response and no image.
fn tool_result<'a>(
    call: &CallId,
    name: &'a str,
    is_error: bool,
    parts: &'a [Part],
    calls: &BTreeMap<&str, Cow<'a, str>>,
    ids: &mut CallIds<'_>,
    input: &GoogleRequest<'_>,
) -> Result<(PartWire<'a>, Vec<PartWire<'a>>), GoogleBodyError> {
    let compat = input.compat;
    let mut texts = Vec::new();
    let mut images = Vec::new();
    let mut omitted = false;
    for part in parts {
        match part {
            Part::Text { text } => texts.push(text.as_ref()),
            Part::Image { .. } if !input.image_input => omitted = true,
            Part::Image { mime, bytes } => images.push(inline_image(mime, bytes)),
            Part::Blob { blob_id, .. } => {
                return Err(GoogleBodyError::UnresolvedBlob { blob_id: *blob_id });
            }
        }
    }
    let mut text = texts.join("\n");
    if omitted {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(NON_VISION_IMAGE_PLACEHOLDER);
    } else if text.is_empty() && !images.is_empty() {
        text = String::from("(see attached image)");
    }
    let (nested, rest) = if compat.nested_tool_images && !images.is_empty() {
        (Some(images), Vec::new())
    } else {
        (None, images)
    };
    let response_value = if is_error {
        ResponseValue {
            output: None,
            error: Some(Cow::Owned(text)),
        }
    } else {
        ResponseValue {
            output: Some(Cow::Owned(text)),
            error: None,
        }
    };
    let response = PartWire {
        function_response: Some(FunctionResponse {
            name: calls
                .get(call.as_str())
                .cloned()
                .unwrap_or_else(|| wire_name(name, WIRE_NAME_MAX).into_owned().into()),
            response: response_value,
            parts: nested,
            id: compat
                .part_ids
                .then(|| Cow::Owned(ids.result(call.as_str()))),
        }),
        ..PartWire::default()
    };
    Ok((response, rest))
}

fn flush_tool_images<'a>(contents: &mut Vec<Content<'a>>, tool_images: &mut Vec<PartWire<'a>>) {
    if !tool_images.is_empty() {
        contents.push(Content {
            role: Some("user"),
            parts: std::mem::take(tool_images),
        });
    }
}

/// Standard-alphabet base64, matching Google's `TYPE_BYTES` inline encoding.
fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Names a tool call id over the wire alphabet, capped at 64 characters
/// (`normalizeToolCallId` in the reference).
fn sanitize_call_id(id: &str) -> String {
    let mut out = String::with_capacity(id.len().min(CALL_ID_MAX));
    for character in id.chars() {
        if out.len() >= CALL_ID_MAX {
            break;
        }
        out.push(
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '_'
            },
        );
    }
    out
}

/// The wire id of every tool call of one request.
///
/// Ids from other providers carry characters the wire rejects and can collide
/// once cleaned. Each call gets a clean id that no other call of the request
/// has, and its result reads the same id back.
#[derive(Default)]
struct CallIds<'a> {
    /// The wire ids handed out so far.
    taken: BTreeSet<String>,
    /// The wire ids of the calls whose result has not been read yet, per raw
    /// id, oldest first.
    pending: BTreeMap<&'a str, VecDeque<String>>,
}

impl<'a> CallIds<'a> {
    /// The wire id of the call `raw`.
    fn call(&mut self, raw: &'a str) -> String {
        let id = self.unique(sanitize_call_id(raw));
        self.pending.entry(raw).or_default().push_back(id.clone());
        id
    }

    /// The wire id of the call a result answers: that of the oldest call of
    /// the same raw id still waiting for one. A result with no such call gets
    /// an id of its own.
    fn result(&mut self, raw: &str) -> String {
        if let Some(id) = self.pending.get_mut(raw).and_then(VecDeque::pop_front) {
            return id;
        }
        self.unique(sanitize_call_id(raw))
    }

    /// `base`, or its first `_dupN` variant not taken yet, cut so the id stays
    /// within the length cap.
    fn unique(&mut self, base: String) -> String {
        if !self.taken.contains(&base) {
            self.taken.insert(base.clone());
            return base;
        }
        let mut copy = 1_usize;
        loop {
            let suffix = format!("_dup{copy}");
            let stem: String = base
                .chars()
                .take(CALL_ID_MAX.saturating_sub(suffix.len()))
                .collect();
            let candidate = format!("{stem}{suffix}");
            if self.taken.insert(candidate.clone()) {
                return candidate;
            }
            copy += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// Stream decoding
// ---------------------------------------------------------------------------

/// One SSE chunk: either a bare `GenerateContent` response (direct API and
/// Vertex AI) or the Cloud Code Assist envelope wrapping one (Antigravity).
/// The envelope carries no `candidates` of its own, so a chunk with a
/// `response` member unwraps exactly one level; deeper nesting is ignored.
#[derive(Deserialize)]
struct Chunk {
    #[serde(default)]
    candidates: Vec<Candidate>,
    #[serde(rename = "usageMetadata")]
    usage_metadata: Option<UsageMetadata>,
    #[serde(rename = "promptFeedback")]
    prompt_feedback: Option<PromptFeedback>,
    #[serde(rename = "responseId")]
    response_id: Option<String>,
    error: Option<ErrorDetail>,
    response: Option<Box<ChunkBody>>,
}

#[derive(Deserialize)]
struct ChunkBody {
    #[serde(default)]
    candidates: Vec<Candidate>,
    #[serde(rename = "usageMetadata")]
    usage_metadata: Option<UsageMetadata>,
    #[serde(rename = "promptFeedback")]
    prompt_feedback: Option<PromptFeedback>,
    #[serde(rename = "responseId")]
    response_id: Option<String>,
    error: Option<ErrorDetail>,
}

#[derive(Deserialize)]
struct Candidate {
    content: Option<CandidateContent>,
    #[serde(rename = "finishReason")]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct CandidateContent {
    #[serde(default)]
    parts: Vec<ChunkPart>,
}

#[derive(Deserialize)]
struct ChunkPart {
    #[serde(default)]
    text: String,
    #[serde(default)]
    thought: bool,
    #[serde(rename = "thoughtSignature")]
    thought_signature: Option<String>,
    #[serde(rename = "functionCall")]
    function_call: Option<FunctionCallChunk>,
}

#[derive(Deserialize)]
struct FunctionCallChunk {
    #[serde(default)]
    name: String,
    /// Raw argument JSON, captured verbatim as one value.
    #[serde(default)]
    args: Option<RawJson>,
    #[serde(default)]
    id: Option<String>,
}

#[derive(Deserialize)]
#[expect(
    clippy::struct_field_names,
    reason = "mirrors Gemini wire usage field names"
)]
struct UsageMetadata {
    #[serde(rename = "promptTokenCount")]
    prompt_token_count: Option<u64>,
    #[serde(rename = "candidatesTokenCount")]
    candidates_token_count: Option<u64>,
    #[serde(rename = "thoughtsTokenCount")]
    thoughts_token_count: Option<u64>,
    #[serde(rename = "totalTokenCount")]
    total_token_count: Option<u64>,
    #[serde(rename = "cachedContentTokenCount")]
    cached_content_token_count: Option<u64>,
}

#[derive(Deserialize)]
struct PromptFeedback {
    #[serde(rename = "blockReason")]
    block_reason: Option<String>,
    #[serde(rename = "blockReasonMessage")]
    block_reason_message: Option<String>,
}

/// Which kind of block a signature belongs to.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    /// Visible answer text.
    Text,
    /// A thought summary.
    Thought,
}

/// The signature of one text or thought block, named by its place among the
/// visible blocks of its kind and by its byte length.
#[derive(Debug, Deserialize, Serialize)]
struct PartSignature {
    kind: Kind,
    index: usize,
    bytes: usize,
    signature: String,
}

/// What a response's replay event carries: the signature of each signed call
/// by call id, and the signed text and thought blocks. Both sides share the
/// shape: [`GeminiDecoder`] emits it below and [`carrier_of`] parses it back.
/// A flat map would not parse here, and the signatures would silently never
/// replay.
#[derive(Deserialize, Default)]
struct Carrier {
    #[serde(default)]
    calls: BTreeMap<String, String>,
    #[serde(default)]
    parts: Vec<PartSignature>,
}

impl Carrier {
    /// Takes the stored signature of `call`, when it is well formed.
    fn take_call(&mut self, call: &str) -> Option<String> {
        self.calls
            .remove(call)
            .filter(|signature| is_valid_signature(signature))
    }

    /// Takes the stored signature of the `index`-th visible block of `kind`
    /// when the block still has `bytes` bytes and the signature is well
    /// formed.
    fn take_block(&mut self, kind: Kind, index: usize, bytes: usize) -> Option<String> {
        let at = self
            .parts
            .iter()
            .position(|part| part.kind == kind && part.index == index && part.bytes == bytes)?;
        let part = self.parts.swap_remove(at);
        is_valid_signature(&part.signature).then_some(part.signature)
    }
}

/// The carrier as emitted; an empty half is left out.
#[derive(Serialize)]
struct CarrierOut<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    calls: Option<&'a BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parts: Option<&'a [PartSignature]>,
}

/// Maps Gemini `usageMetadata` onto the core counters: `promptTokenCount`
/// already includes `cachedContentTokenCount`, so input is the prompt and
/// the cache count is its subset; output excludes thoughts, which are
/// retained as the reasoning split (`mapGoogleUsage` in the reference, under
/// the crate's input-includes-cached convention).
fn normalize(metadata: &UsageMetadata) -> Usage {
    let candidates = metadata.candidates_token_count.unwrap_or(0);
    let thoughts = metadata.thoughts_token_count.unwrap_or(0);
    let total = metadata.total_token_count.unwrap_or(0);
    let prompt = metadata
        .prompt_token_count
        .unwrap_or_else(|| total.saturating_sub(candidates + thoughts));
    let cached = metadata.cached_content_token_count.unwrap_or(0).min(prompt);
    Usage {
        input_tokens: prompt,
        cached_input_tokens: cached,
        output_tokens: candidates,
        reasoning_tokens: Some(thoughts),
        cache_write_tokens: 0,
        premium_requests: None,
        cost_usd: None,
    }
}

/// The terminal the last `finishReason` decides: a clean stop, or a protocol
/// error for a failure the neutral grammar cannot name (`mapStopReason` in
/// the reference maps these to its `error`).
#[derive(Debug)]
enum Terminal {
    Stop(StopReason),
    Error(String),
}

fn terminal_of(reason: &str) -> Terminal {
    match reason {
        "STOP" => Terminal::Stop(StopReason::EndTurn),
        "MAX_TOKENS" => Terminal::Stop(StopReason::MaxTokens),
        "SAFETY"
        | "BLOCKLIST"
        | "PROHIBITED_CONTENT"
        | "SPII"
        | "RECITATION"
        | "LANGUAGE"
        | "IMAGE_SAFETY"
        | "IMAGE_PROHIBITED_CONTENT"
        | "IMAGE_RECITATION"
        | "IMAGE_OTHER"
        | "NO_IMAGE" => Terminal::Stop(StopReason::Refusal),
        // A malformed or unexpected call, an unnamed reason, or a missing
        // image the request required: the stream failed, even when earlier
        // chunks carried valid calls.
        other => Terminal::Error(format!("generation failed with finish reason: {other}")),
    }
}

fn protocol(family: Family, detail: impl Into<String>) -> ProviderError {
    ProviderError::Protocol {
        family,
        detail: detail.into(),
    }
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

/// The slot an Antigravity session observes the response id through, for
/// the next request's `labels.last_execution_id`.
pub(crate) type ResponseIdSlot = Arc<Mutex<Option<String>>>;

/// The incremental decoder of one `GenerateContent` SSE stream, shared by the
/// direct API, Vertex AI, and the CCA envelope surface.
///
/// Feed it the data of every SSE event in order with [`GeminiDecoder::feed`],
/// then call [`GeminiDecoder::end`] at end of input. Deltas emit as they
/// arrive, except whitespace-only text, which waits until the response shows
/// it is not empty; the terminal group is one `Replay` (only when a call or
/// block was signed), one `ToolCallsDone`, one `Usage`, and one `Stop` — or
/// one error instead, including for a clean stop with no call and no visible
/// answer. Only candidate index 0 is read; a `promptFeedback.blockReason`
/// with no candidates ends the turn as a refusal that names the block
/// (`consumeGoogleStream` throws `content-blocked` there).
#[derive(Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "stream decoder tracks four independent response latches"
)]
pub(crate) struct GeminiDecoder {
    family: Family,
    provider: Box<str>,
    model: Box<str>,
    budget: StreamBudget,
    /// Open calls in first-seen order.
    calls: Vec<OpenCall>,
    /// The thought signature of each signed call, by call id.
    signed: BTreeMap<String, String>,
    /// The text and thought blocks of the response in order. Adjacent parts
    /// of one kind form one block even across a function call, as the session
    /// fold groups the deltas.
    blocks: Vec<Block>,
    /// Whether a function call has closed the open block: a signature-only
    /// part after it signs nothing.
    sealed: bool,
    /// Whitespace-only text held back while the response may still be empty.
    held: String,
    /// Whether the response has produced a call or a visible answer.
    meaningful: bool,
    /// The last non-null usage.
    usage: Option<Usage>,
    /// The last non-null `finishReason`, mapped.
    stop: Option<Terminal>,
    /// The salt disambiguating generated call ids of this response.
    salt: String,
    /// The generated-id counter of this response.
    next_id: u64,
    response_id: Option<ResponseIdSlot>,
    emitted: bool,
    ended: bool,
}

/// The state of one assembling tool call.
#[derive(Debug)]
struct OpenCall {
    id: String,
    name: String,
    args: Vec<u8>,
}

/// One text or thought block of a response.
#[derive(Debug)]
struct Block {
    kind: Kind,
    /// The bytes of text the block holds.
    bytes: usize,
    /// Whether any of its text is not whitespace.
    visible: bool,
    /// The newest signature a part of the block carried.
    signature: Option<String>,
}

impl GeminiDecoder {
    /// A decoder for one response of `model` on the provider id `provider`;
    /// both name the request in mapped in-band errors. `response_id`
    /// receives the response id when a chunk carries one.
    #[must_use]
    pub(crate) fn new(
        family: Family,
        provider: &str,
        model: &str,
        response_id: Option<ResponseIdSlot>,
    ) -> Self {
        let salt = Uuid::new_v4().simple().to_string();
        let salt = salt[..12].to_owned();
        Self {
            family,
            provider: provider.into(),
            model: model.into(),
            budget: StreamBudget::default(),
            calls: Vec::new(),
            signed: BTreeMap::new(),
            blocks: Vec::new(),
            sealed: false,
            held: String::new(),
            meaningful: false,
            usage: None,
            stop: None,
            salt,
            next_id: 0,
            response_id,
            emitted: false,
            ended: false,
        }
    }

    /// Decodes the data of one SSE event, appending events to `out`.
    ///
    /// The event name is ignored; each event's data is one chunk. A non-null
    /// `usageMetadata` is kept. Every part of candidate 0 becomes a delta:
    /// a thought part emits `ReasoningDelta` and plain text `TextDelta`, and
    /// each `functionCall` part opens one call (which needs a non-empty
    /// name), emitting `ToolArgsDelta` with its raw argument text.
    ///
    /// # Errors
    ///
    /// [`ProviderError::Protocol`] for data that is not a chunk or a
    /// `functionCall` part without a name; the mapped server error for an
    /// in-band `error` frame. The decoder is ended after an error.
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

    /// Ends the input: a response with a `finishReason` finishes, even
    /// without usage; an ended decoder emits nothing.
    ///
    /// # Errors
    ///
    /// [`ProviderError::StreamCut`] when no `finishReason` arrived.
    pub(crate) fn end(&mut self, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        if std::mem::replace(&mut self.ended, true) {
            return Ok(());
        }
        if self.stop.is_none() {
            return Err(ProviderError::StreamCut);
        }
        self.finish(out)
    }

    fn step(&mut self, data: &str, out: &mut Vec<StreamEvent>) -> Result<Progress, ProviderError> {
        let chunk: Chunk = sonic_rs::from_str(data).map_err(|error| {
            protocol(self.family, format!("google chunk is not valid: {error}"))
        })?;
        // The CCA envelope nests the response one level deep.
        let inner = chunk.response.as_deref();
        if let Some(error) = chunk
            .error
            .as_ref()
            .or(inner.and_then(|inner| inner.error.as_ref()))
        {
            return Err(self.server_error(error));
        }
        let (candidates, usage, feedback, response_id) = match inner {
            Some(inner) => (
                &inner.candidates,
                &inner.usage_metadata,
                &inner.prompt_feedback,
                &inner.response_id,
            ),
            None => (
                &chunk.candidates,
                &chunk.usage_metadata,
                &chunk.prompt_feedback,
                &chunk.response_id,
            ),
        };
        if let Some(metadata) = usage {
            self.usage = Some(normalize(metadata));
        }
        self.observe_response_id(response_id.as_deref());
        if candidates.is_empty() {
            let blocked = feedback.as_ref().and_then(|feedback| {
                let reason = feedback.block_reason.as_deref()?;
                Some((reason, feedback.block_reason_message.as_deref()))
            });
            if let Some((reason, detail)) = blocked {
                self.blocked_prompt(reason, detail, out)?;
                return Ok(Progress::Ended);
            }
            return Ok(Progress::Open);
        }
        let candidate = &candidates[0];
        if let Some(reason) = candidate.finish_reason.as_deref() {
            self.stop = Some(terminal_of(reason));
        }
        let parts: &[ChunkPart] = candidate
            .content
            .as_ref()
            .map_or(&[], |content| &content.parts);
        for part in parts {
            self.part(part, out)?;
        }
        Ok(Progress::Open)
    }

    fn observe_response_id(&self, response_id: Option<&str>) {
        if let Some(slot) = &self.response_id
            && let Some(id) = response_id
            && let Ok(mut slot) = slot.lock()
        {
            *slot = Some(id.to_owned());
        }
    }

    /// A blocked prompt with no candidates ends the turn as a refusal that
    /// names the block reason and its message, mirroring the reference
    /// `content-blocked` throw as neutral refusal text.
    fn blocked_prompt(
        &mut self,
        reason: &str,
        detail: Option<&str>,
        out: &mut Vec<StreamEvent>,
    ) -> Result<(), ProviderError> {
        let text = match detail.filter(|detail| !detail.is_empty()) {
            Some(detail) => format!("Request blocked by Google ({reason}): {detail}"),
            None => format!("Request blocked by Google ({reason})"),
        };
        out.push(StreamEvent::TextDelta { text });
        self.meaningful = true;
        self.stop = Some(Terminal::Stop(StopReason::Refusal));
        self.finish(out)
    }

    fn part(&mut self, part: &ChunkPart, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        if let Some(call) = &part.function_call {
            self.function_call(call, part.thought_signature.as_deref(), out)?;
            return Ok(());
        }
        let kind = if part.thought {
            Kind::Thought
        } else {
            Kind::Text
        };
        let signature = part
            .thought_signature
            .as_deref()
            .filter(|signature| !signature.is_empty());
        if part.text.is_empty() {
            // A signature-only part signs the block that is open.
            if let Some(signature) = signature {
                self.sign_open(kind, signature);
            }
            return Ok(());
        }
        self.extend_block(kind, &part.text);
        if let Some(signature) = signature {
            self.sign_open(kind, signature);
        }
        self.deliver(kind, &part.text, out);
        Ok(())
    }

    /// Adds `text` to the open block of `kind`, or opens one.
    fn extend_block(&mut self, kind: Kind, text: &str) {
        self.sealed = false;
        let visible = !text.trim().is_empty();
        match self.blocks.last_mut() {
            Some(block) if block.kind == kind => {
                block.bytes += text.len();
                block.visible |= visible;
            }
            _ => self.blocks.push(Block {
                kind,
                bytes: text.len(),
                visible,
                signature: None,
            }),
        }
    }

    /// Keeps `signature` on the open block. A thought part signs only an open
    /// thought block; an unmarked part signs whichever block is open. Nothing
    /// is open after a function call.
    fn sign_open(&mut self, kind: Kind, signature: &str) {
        if self.sealed {
            return;
        }
        if let Some(block) = self.blocks.last_mut()
            && (kind == Kind::Text || block.kind == Kind::Thought)
        {
            block.signature = Some(signature.to_owned());
        }
    }

    /// Emits one part's text as a delta. Whitespace-only text is held back
    /// while the response may still turn out empty, and released in order
    /// before anything else is emitted.
    fn deliver(&mut self, kind: Kind, text: &str, out: &mut Vec<StreamEvent>) {
        let visible = !text.trim().is_empty();
        if kind == Kind::Text && !visible && !self.meaningful {
            self.held.push_str(text);
            return;
        }
        self.release_held(out);
        match kind {
            Kind::Thought => out.push(StreamEvent::ReasoningDelta {
                text: text.to_owned(),
            }),
            Kind::Text => {
                self.meaningful |= visible;
                out.push(StreamEvent::TextDelta {
                    text: text.to_owned(),
                });
            }
        }
    }

    /// Emits the held whitespace.
    fn release_held(&mut self, out: &mut Vec<StreamEvent>) {
        if !self.held.is_empty() {
            out.push(StreamEvent::TextDelta {
                text: std::mem::take(&mut self.held),
            });
        }
    }

    fn function_call(
        &mut self,
        call: &FunctionCallChunk,
        signature: Option<&str>,
        out: &mut Vec<StreamEvent>,
    ) -> Result<(), ProviderError> {
        if call.name.is_empty() {
            return Err(protocol(
                self.family,
                "google functionCall part has no name",
            ));
        }
        // The provider id is optional and may repeat: an absent, empty, or
        // reused id gets a fresh one (`nextToolCallId` in the reference).
        let provided = sanitize_call_id(call.id.as_deref().unwrap_or(""));
        let id = if provided.is_empty() || self.calls.iter().any(|open| open.id == provided) {
            self.fresh_id(&call.name)
        } else {
            provided
        };
        self.budget.admit_item()?;
        self.release_held(out);
        self.meaningful = true;
        self.sealed = true;
        out.push(StreamEvent::ToolCallStarted {
            id: id.clone(),
            name: call.name.clone(),
        });
        let mut args = Vec::new();
        if let Some(raw) = call.args.as_ref().filter(|raw| {
            let text = raw.as_str();
            !text.is_empty() && text != "null"
        }) {
            let text = raw.as_str();
            args.extend_from_slice(text.as_bytes());
            out.push(StreamEvent::ToolArgsDelta {
                id: id.clone(),
                fragment: text.as_bytes().to_vec(),
            });
        }
        if let Some(signature) = signature.filter(|signature| !signature.is_empty()) {
            self.signed.insert(id.clone(), signature.to_owned());
        }
        self.calls.push(OpenCall {
            id,
            name: call.name.clone(),
            args,
        });
        Ok(())
    }

    /// A generated call id, unique to this response.
    fn fresh_id(&mut self, name: &str) -> String {
        let stem: String = name
            .chars()
            .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
            .take(24)
            .collect();
        let stem = if stem.is_empty() { "call" } else { &stem };
        self.next_id += 1;
        format!("{stem}_{}_{}", self.salt, self.next_id)
    }

    /// Emits the terminal group: the held whitespace, the signature carrier
    /// when a call or block was signed, then `ToolCallsDone`, `Usage`, and
    /// `Stop`. A response that called tools upgrades a clean `EndTurn` or
    /// `MaxTokens` to `ToolUse` (`consumeGoogleStream` in the reference); a
    /// failure finish reason ends the stream with that error, and so does a
    /// clean stop that produced neither a call nor a visible answer, before
    /// anything terminal is emitted so the lifecycle can ask again. Absent
    /// arguments decode as `{}`.
    fn finish(&mut self, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        let terminal = self.stop.take().unwrap_or(if self.calls.is_empty() {
            Terminal::Stop(StopReason::EndTurn)
        } else {
            Terminal::Stop(StopReason::ToolUse)
        });
        let reason = match terminal {
            Terminal::Error(detail) => return Err(protocol(self.family, detail)),
            Terminal::Stop(reason) => match reason {
                StopReason::EndTurn | StopReason::MaxTokens if !self.calls.is_empty() => {
                    StopReason::ToolUse
                }
                reason => reason,
            },
        };
        if reason == StopReason::EndTurn && !self.meaningful {
            return Err(self.empty_response());
        }
        self.release_held(out);
        if let Some(item) = self.carrier()? {
            out.push(StreamEvent::Replay {
                payload: ReplayPayload {
                    family: self.family,
                    model: self.model.clone(),
                    item,
                },
            });
        }
        let calls = std::mem::take(&mut self.calls)
            .into_iter()
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
        out.push(StreamEvent::Stop { reason });
        Ok(())
    }

    /// The failure of a clean stop with nothing to act on: an empty response,
    /// or one that only thought.
    fn empty_response(&self) -> ProviderError {
        let thought = self
            .blocks
            .iter()
            .any(|block| block.kind == Kind::Thought && block.visible);
        let reason = if thought {
            "google returned a thought-only response (finishReason STOP with reasoning and no answer)"
        } else {
            "google returned an empty response (finishReason STOP with no content)"
        };
        ProviderError::Transport {
            family: self.family,
            reason: String::from(reason),
        }
    }

    /// The replay carrier of the response as the item of a replay event, or
    /// `None` when nothing was signed.
    fn carrier(&mut self) -> Result<Option<RawJson>, ProviderError> {
        let parts = self.signed_blocks();
        if self.signed.is_empty() && parts.is_empty() {
            return Ok(None);
        }
        let invalid = |error: &dyn fmt::Display| {
            protocol(
                self.family,
                format!("google replay carrier is not valid: {error}"),
            )
        };
        let carrier = sonic_rs::to_string(&CarrierOut {
            calls: (!self.signed.is_empty()).then_some(&self.signed),
            parts: (!parts.is_empty()).then_some(parts.as_slice()),
        })
        .map_err(|error| invalid(&error))?;
        RawJson::parse(&carrier)
            .map(Some)
            .map_err(|error| invalid(&error))
    }

    /// The signed text and thought blocks, each named by its place among the
    /// visible blocks of its kind.
    fn signed_blocks(&mut self) -> Vec<PartSignature> {
        let (mut texts, mut thoughts) = (0, 0);
        let mut signed = Vec::new();
        for block in std::mem::take(&mut self.blocks)
            .into_iter()
            .filter(|block| block.visible)
        {
            let counter = match block.kind {
                Kind::Text => &mut texts,
                Kind::Thought => &mut thoughts,
            };
            let index = *counter;
            *counter += 1;
            if let Some(signature) = block.signature {
                signed.push(PartSignature {
                    kind: block.kind,
                    index,
                    bytes: block.bytes,
                    signature,
                });
            }
        }
        signed
    }

    /// Maps an in-band `error` frame through the retry table. The numeric
    /// `code` is the HTTP status; without one the RPC `status` maps to its
    /// canonical HTTP status. The table sees only the code and the message,
    /// so a throttle it leaves retryable is judged once more against the
    /// frame's structured details, as the HTTP join does for an error body.
    /// The decoder returns only the error value; the request lifecycle owns
    /// retries.
    fn server_error(&self, error: &ErrorDetail) -> ProviderError {
        let message = error.message.as_deref().unwrap_or_default();
        let (status, code) = rpc_status(error);
        let request = RequestState {
            family: self.family,
            provider: &self.provider,
            model: &self.model,
            oauth: false,
            refreshed: false,
            delivered: self.emitted,
        };
        let failure = match classify(status, code.as_deref(), message, &request) {
            RetryDecision::Retry(error) | RetryDecision::Fail(error) => error,
            // Unreachable without a refreshable credential on this stream;
            // the lifecycle refreshes OAuth before the first byte.
            RetryDecision::RefreshOnce => {
                return ProviderError::SignInExpired {
                    provider: String::from(request.provider),
                };
            }
        };
        let throttled = status == Some(429) || code.as_deref() == Some("RESOURCE_EXHAUSTED");
        if throttled && matches!(failure, ProviderError::RateLimited { .. }) && is_quota(message, Some(error)) {
            return ProviderError::Quota {
                message: String::from(message),
            };
        }
        failure
    }
}

/// The canonical HTTP status of an in-band RPC error: its numeric `code`
/// when one fits, else the RPC `status` spelling.
fn rpc_status(error: &ErrorDetail) -> (Option<u16>, Option<String>) {
    if let Some(code) = error.code.and_then(|code| u16::try_from(code).ok()) {
        return (Some(code), error.status.clone());
    }
    let status = match error.status.as_deref() {
        Some("INVALID_ARGUMENT" | "FAILED_PRECONDITION" | "OUT_OF_RANGE") => Some(400),
        Some("UNAUTHENTICATED") => Some(401),
        Some("PERMISSION_DENIED") => Some(403),
        Some("NOT_FOUND") => Some(404),
        Some("ABORTED" | "ALREADY_EXISTS" | "CONFLICT") => Some(409),
        Some("RESOURCE_EXHAUSTED") => Some(429),
        Some("CANCELLED") => Some(499),
        Some("DATA_LOSS" | "UNKNOWN" | "INTERNAL") => Some(500),
        Some("UNIMPLEMENTED") => Some(501),
        Some("UNAVAILABLE") => Some(503),
        Some("DEADLINE_EXCEEDED") => Some(504),
        _ => None,
    };
    (status, error.status.clone())
}

const NO_USAGE: Usage = Usage {
    input_tokens: 0,
    cached_input_tokens: 0,
    output_tokens: 0,
    reasoning_tokens: None,
    cache_write_tokens: 0,
    premium_requests: None,
    cost_usd: None,
};

// ---------------------------------------------------------------------------
// HTTP error bodies
// ---------------------------------------------------------------------------

/// A Google HTTP error body:
/// `{"error":{"code":<http>,"message":<m>,"status":<rpc>,"details":[...]}}`.
#[derive(Deserialize, Default)]
struct HttpError {
    #[serde(default)]
    error: Option<ErrorDetail>,
}

#[derive(Deserialize, Default)]
struct ErrorDetail {
    #[serde(default)]
    code: Option<i64>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    details: Vec<ErrorDetailEntry>,
}

#[derive(Deserialize, Default)]
struct ErrorDetailEntry {
    #[serde(rename = "@type", default)]
    kind: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(rename = "retryDelay", default)]
    retry_delay: Option<String>,
}

/// Extracts the `(code, message)` of a Google HTTP error body for the join's
/// `status_failure`: the RPC `status` spelling is the code, and the message
/// is the server text. A body outside the envelope falls back to its first
/// line, cut at 300 bytes.
pub(crate) fn error_fields(body: &str) -> (Option<String>, String) {
    let parsed = sonic_rs::from_str::<HttpError>(body).ok();
    let detail = parsed.as_ref().and_then(|parsed| parsed.error.as_ref());
    let code = detail.and_then(|detail| detail.status.clone());
    let message = detail
        .and_then(|detail| detail.message.clone())
        .unwrap_or_else(|| capped_message_line(body).to_owned());
    (code, message)
}

/// Classifies a Google HTTP error response for the join's `status_failure`.
/// A 429 or `RESOURCE_EXHAUSTED` carrying an account-exhaustion signal
/// (the `google.rpc.ErrorInfo` reasons the reference reads in
/// `parseGoogleRpcRateLimitReason`, the "quota will reset" wording, or a
/// retry hint at or over five minutes) is terminal quota; everything else
/// runs the shared retry table.
pub(crate) fn classify_http(status: u16, body: &str, request: &RequestState<'_>) -> ProviderError {
    let parsed = sonic_rs::from_str::<HttpError>(body).ok();
    let detail = parsed.as_ref().and_then(|parsed| parsed.error.as_ref());
    let message = detail
        .and_then(|detail| detail.message.clone())
        .unwrap_or_else(|| capped_message_line(body).to_owned());
    let code = detail.and_then(|detail| detail.status.clone());
    // The envelope's numeric code repeats the HTTP status; prefer it when
    // the transport status is a generic proxy refusal.
    let status = detail
        .and_then(|detail| detail.code)
        .and_then(|code| u16::try_from(code).ok())
        .unwrap_or(status);
    if (status == 429 || code.as_deref() == Some("RESOURCE_EXHAUSTED"))
        && is_quota(&message, detail)
    {
        return ProviderError::Quota { message };
    }
    match classify(Some(status), code.as_deref(), &message, request) {
        RetryDecision::Retry(error) | RetryDecision::Fail(error) => error,
        RetryDecision::RefreshOnce => ProviderError::SignInExpired {
            provider: String::from(request.provider),
        },
    }
}

fn is_quota(message: &str, detail: Option<&ErrorDetail>) -> bool {
    const ERROR_INFO: &str = "type.googleapis.com/google.rpc.ErrorInfo";
    const RETRY_INFO: &str = "type.googleapis.com/google.rpc.RetryInfo";
    if let Some(detail) = detail {
        for entry in &detail.details {
            match entry.kind.as_deref() {
                Some(ERROR_INFO) => match entry.reason.as_deref() {
                    Some("QUOTA_EXHAUSTED" | "INSUFFICIENT_G1_CREDITS_BALANCE") => {
                        return true;
                    }
                    _ => {}
                },
                Some(RETRY_INFO)
                    if entry
                        .retry_delay
                        .as_deref()
                        .is_some_and(|delay| delay_secs(delay) >= 300.0) =>
                {
                    return true;
                }
                _ => {}
            }
        }
    }
    message.contains("quota will reset") || message.contains("exhausted your capacity")
}

fn delay_secs(delay: &str) -> f64 {
    delay
        .strip_suffix('s')
        .unwrap_or(delay)
        .parse()
        .unwrap_or(0.0)
}

// ---------------------------------------------------------------------------
// Transport glue
// ---------------------------------------------------------------------------

/// One lowered Google request: the path, the headers, and the body the
/// transport sends. The join maps it onto `build_stream_request` (path via
/// [`http::endpoint`], headers, body) and [`decode_events`] for the reply.
#[must_use]
pub(crate) struct GoogleWire {
    /// The path joined to the provider base URL, query included.
    pub(crate) path: String,
    /// A complete base URL overriding the provider's configured base
    /// (Vertex location hosts, CCA endpoints); `None` uses the configured
    /// base.
    pub(crate) base_override: Option<String>,
    /// Headers in send order; names are lowercase.
    pub(crate) headers: Vec<(&'static str, String)>,
    /// The `user-agent` the request must send instead of the dalgon one;
    /// set only for Antigravity.
    pub(crate) user_agent: Option<String>,
    /// The JSON body.
    pub(crate) body: Vec<u8>,
    /// Retry the request once against this target when the first attempt
    /// answers 404 (a regional Vertex endpoint serving a global-only model).
    pub(crate) fallback: Option<FallbackTarget>,
}

/// The one-shot retry target of [`GoogleWire::fallback`].
#[derive(Clone, Debug)]
pub(crate) struct FallbackTarget {
    /// A complete base URL overriding the provider's configured base.
    pub(crate) base_override: Option<String>,
    /// The path joined to the base URL, query included.
    pub(crate) path: String,
}

impl fmt::Debug for GoogleWire {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let headers: Vec<(&str, &str)> = self
            .headers
            .iter()
            .map(|(name, value)| {
                let secret = matches!(*name, "x-goog-api-key" | "authorization");
                (*name, if secret { "<redacted>" } else { value.as_str() })
            })
            .collect();
        formatter
            .debug_struct("GoogleWire")
            .field("path", &self.path)
            .field("base_override", &self.base_override)
            .field("headers", &headers)
            .field("user_agent", &self.user_agent)
            .field("body_len", &self.body.len())
            .field("fallback", &self.fallback)
            .finish()
    }
}

fn endpoint_for(
    family: Family,
    base_url: &str,
    base_override: Option<&str>,
    path: &str,
) -> Result<url::Url, ProviderError> {
    let base = base_override.unwrap_or(base_url);
    http::endpoint(family, base, path)
}

/// Sends one Google wire request over the shared HTTP stack, retrying once
/// against [`GoogleWire::fallback`] when the first attempt answers 404.
///
/// Only pre-first-byte failover lives here: once the transport accepts the
/// response, the decoder and the request lifecycle own every later outcome.
pub(crate) async fn send(
    family: Family,
    client: &reqwest::Client,
    wire: GoogleWire,
    base_url: &str,
    user_agent: &str,
) -> Result<reqwest::Response, ProviderError> {
    let url = endpoint_for(family, base_url, wire.base_override.as_deref(), &wire.path)?;
    let agent = wire.user_agent.as_deref().unwrap_or(user_agent);
    let (headers, body) = (wire.headers, wire.body);
    let attempt = |url: url::Url| {
        let mut request = client.post(url);
        for (name, value) in &headers {
            request = request.header(*name, value.clone());
        }
        http::send(
            family,
            request.body(body.clone()),
            agent,
            http::Exchange::Stream,
            tokio::time::sleep,
        )
    };
    let response = attempt(url).await?;
    if response.status().as_u16() == 404
        && let Some(fallback) = &wire.fallback
    {
        let url = endpoint_for(
            family,
            base_url,
            fallback.base_override.as_deref(),
            &fallback.path,
        )?;
        return attempt(url).await;
    }
    Ok(response)
}

/// Drives a [`GeminiDecoder`] over an SSE event stream into neutral events.
///
/// The join feeds this the transport's SSE event stream (which already owns
/// byte-read failures and secret redaction); this function owns only the
/// Google grammar on top of it.
///
/// Event names are ignored; each event's data goes to the decoder. An SSE
/// error ends the stream with that error, and end of input ends it through
/// [`GeminiDecoder::end`]. Nothing is read after the terminal.
pub(crate) fn decode_events<S>(
    family: Family,
    provider: &str,
    model: &str,
    events: S,
    response_id: Option<ResponseIdSlot>,
) -> impl Stream<Item = Result<StreamEvent, ProviderError>> + Send + 'static
where
    S: Stream<Item = Result<SseEvent, ProviderError>> + Send + 'static,
{
    let state = (
        Box::pin(events),
        GeminiDecoder::new(family, provider, model, response_id),
        VecDeque::new(),
    );
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
mod tests;
