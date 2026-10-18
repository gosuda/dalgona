//! Devin request builders: unary bodies and the chat frame.
//!
//! Every builder here is pure: protobuf structs in, wire bytes out. The join
//! owns the transport (endpoint URLs, `http::send`, retries, the auth ladder
//! order), and calls these for bodies. [`chat_frame`] gzips the encoded
//! request and wraps it in one Connect frame with the compressed flag, which
//! is the only framing the chat RPC accepts.
//!
//! [`DevinChatInput`] carries everything one turn needs besides the
//! credential: the neutral request, the resolved wire key and JWT, the
//! cascade and execution ids the caller minted, the resolved chat model uid,
//! the assignment JWT on router turns, and the sampling inputs. Tool schemas
//! travel as their raw text, exactly like the Chat family, except on Gemini
//! routes, where `"type": [T, "null"]` arrays are scalarized with
//! `"nullable": true` because the Gemini backend rejects type arrays with an
//! opaque `invalid_argument` ([`scalarize_type_arrays`], `[INFERENCE]`-marked
//! uid heuristic below).

use dal_core::{Family, ModelRequest, ModelRoute};
use prost::Message;
use sonic_rs::{JsonContainerTrait, JsonValueMutTrait, JsonValueTrait};

use super::connect::{COMPRESSED_FLAG, ConnectError, frame_message, gzip};
use super::history::chat_prompts;
use super::proto::{
    AssignModelRequest, ChatMessagePrompt, ChatToolChoice, ChatToolDefinition,
    CompletionConfiguration, GetChatMessageRequest, GetCliModelConfigsRequest, GetUserJwtRequest,
    Metadata, PromptCacheOptions, cache_control_type, chat_message_request_type,
    conversational_planner_mode, display_option,
};
use crate::error::ProviderError;
use crate::tool_names::{WIRE_NAME_MAX, wire_name};

/// CLI product name on chat calls.
pub(crate) const IDE_NAME: &str = "devin-cli";
/// Client kind unlocking assignment and the CLI model surface.
pub(crate) const IDE_TYPE: &str = "chisel";
/// Pinned CLI version of the reference identity.
pub(crate) const IDE_VERSION: &str = "3000.11.3";
/// Extension name.
pub(crate) const EXTENSION_NAME: &str = "chisel";
/// Locale.
pub(crate) const LOCALE: &str = "en";
/// Scheme prefix the wire format requires on session tokens.
pub(crate) const SESSION_TOKEN_PREFIX: &str = "devin-session-token$";
/// Required headers on the chat call, in send order.
pub(crate) const CHAT_HEADERS: [(&str, &str); 5] = [
    ("content-type", "application/connect+proto"),
    ("connect-protocol-version", "1"),
    ("connect-content-encoding", "gzip"),
    ("accept-encoding", "identity"),
    ("connect-accept-encoding", "gzip"),
];
/// Transport user agent of the pinned reference's chat call.
pub(crate) const CONNECT_USER_AGENT: &str = "connect-go/1.18.1 (go1.26.3)";
/// Required headers on the unary calls, in send order.
pub(crate) const UNARY_HEADERS: [(&str, &str); 3] = [
    ("content-type", "application/proto"),
    ("connect-protocol-version", "1"),
    ("accept", "*/*"),
];
/// Display slots the native client advertises on discovery.
pub(crate) const DISCOVERY_DISPLAYS: [i32; 5] = [
    display_option::MODEL_ROUTER,
    display_option::QUICK_REVIEW,
    display_option::INTERNAL_DEFAULT,
    display_option::UNCLASSIFIED,
    display_option::NORMAL,
];
/// Newline ceiling of the pinned reference.
pub(crate) const MAX_NEWLINES: u64 = 400;
/// Default temperature of the pinned reference.
pub(crate) const DEFAULT_TEMPERATURE: f64 = 1.0;
/// Default top-k of the pinned reference.
pub(crate) const DEFAULT_TOP_K: u64 = 40;
/// Default top-p of the pinned reference.
pub(crate) const DEFAULT_TOP_P: f64 = 0.95;
/// Hard cap on a surfaced HTTP error detail, in UTF-16 code units.
///
/// The pinned `MAX_DEVIN_ERROR_DETAIL_CHARS` counts JavaScript string
/// length, so astral characters cost two units each.
pub(crate) const MAX_ERROR_DETAIL_UTF16_UNITS: usize = 4096;

/// Everything one Devin chat turn needs besides the credential.
///
/// `api_key` is the resolved wire key: the session-token form, or the raw
/// key when the 401 fallback selected it. `cascade_id` threads the
/// conversation server-side; `execution_id` is fresh per turn.
/// `chat_model_uid` is the resolved serving uid, never a router uid.
/// `temperature` and `top_p` fall back to the reference defaults when `None`;
/// stop sequences default to empty, which is what the pinned reference sends
/// when the caller passes none.
pub(crate) struct DevinChatInput<'a> {
    /// Provider-neutral request with inlined blobs.
    pub(crate) request: &'a ModelRequest,
    /// Resolved wire key for `Metadata.api_key`.
    pub(crate) api_key: &'a str,
    /// JWT from `GetUserJwt`.
    pub(crate) user_jwt: &'a str,
    /// Cascade thread id.
    pub(crate) cascade_id: &'a str,
    /// Per-turn execution id.
    pub(crate) execution_id: &'a str,
    /// Resolved serving model uid.
    pub(crate) chat_model_uid: &'a str,
    /// Assignment JWT on router-resolved turns.
    pub(crate) assignment_jwt: Option<&'a str>,
    /// Output token ceiling.
    pub(crate) max_tokens: u32,
    /// False unless the model supports parallel tool calls.
    pub(crate) parallel_tool_calls: bool,
    /// Sampling temperature override.
    pub(crate) temperature: Option<f64>,
    /// Top-p override.
    pub(crate) top_p: Option<f64>,
    /// Caller stop sequences.
    pub(crate) stop_sequences: &'a [String],
}

/// Session token as the wire format carries it: the scheme prefix is required.
#[must_use]
pub(crate) fn normalize_session_token(api_key: &str) -> String {
    if api_key.starts_with(SESSION_TOKEN_PREFIX) {
        String::from(api_key)
    } else {
        format!("{SESSION_TOKEN_PREFIX}{api_key}")
    }
}

/// The `Metadata.os` vocabulary of the pinned reference.
#[must_use]
pub(crate) fn devin_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    }
}

/// Released-CLI metadata with credential bytes already encoded for the wire.
///
/// `api_key` travels verbatim here; callers pass the session-token form for
/// the first attempt and the raw key for the 401 fallback.
fn wire_metadata(api_key: &str, user_jwt: &str) -> Metadata {
    Metadata {
        ide_name: String::from(IDE_NAME),
        ide_version: String::from(IDE_VERSION),
        ide_type: String::from(IDE_TYPE),
        extension_name: String::from(EXTENSION_NAME),
        extension_version: String::from(IDE_VERSION),
        api_key: String::from(api_key),
        locale: String::from(LOCALE),
        os: String::from(devin_os()),
        user_jwt: String::from(user_jwt),
        ..Default::default()
    }
}

/// Released-CLI metadata authenticated by a Devin session token.
#[must_use]
pub(crate) fn session_metadata(api_key: &str, user_jwt: &str) -> Metadata {
    wire_metadata(&normalize_session_token(api_key), user_jwt)
}

/// Dev-channel metadata for the `GetCliModelConfigs` call.
#[must_use]
pub(crate) fn discovery_metadata(api_key: &str) -> Metadata {
    Metadata {
        ide_name: String::from("chisel"),
        ide_version: String::from("0.0.0-dev"),
        extension_name: String::from("chisel"),
        extension_version: String::from("0.0.0-dev"),
        api_key: normalize_session_token(api_key),
        locale: String::from(LOCALE),
        os: String::from(devin_os()),
        supported_model_displays: DISCOVERY_DISPLAYS.to_vec(),
        ..Default::default()
    }
}

/// The `GetUserJwt` body for one session token.
///
/// # Errors
///
/// Never fails; kept fallible-free by construction.
#[must_use]
pub(crate) fn user_jwt_body(api_key: &str) -> Vec<u8> {
    GetUserJwtRequest {
        metadata: Some(session_metadata(api_key, "")),
    }
    .encode_to_vec()
}

/// The `GetUserJwt` body for the 401 fallback: the raw key, no prefix.
#[must_use]
pub(crate) fn user_jwt_fallback_body(api_key: &str) -> Vec<u8> {
    GetUserJwtRequest {
        metadata: Some(wire_metadata(api_key, "")),
    }
    .encode_to_vec()
}

/// The `AssignModel` body resolving one router uid.
///
/// `router_prompt` is the current user turn alone, built by
/// [`router_prompt`]; the router uid is never a legal chat uid, so a failed
/// assignment must fail the turn rather than fall back.
///
/// # Errors
///
/// Never fails; kept fallible-free by construction.
#[must_use]
pub(crate) fn assign_body(
    wire_key: &str,
    router_uid: &str,
    cascade_id: &str,
    router_prompt: Option<ChatMessagePrompt>,
) -> Vec<u8> {
    AssignModelRequest {
        metadata: Some(wire_metadata(wire_key, "")),
        model_router_uid: String::from(router_uid),
        cascade_id: String::from(cascade_id),
        chat_message_prompt: router_prompt,
    }
    .encode_to_vec()
}

/// The `GetCliModelConfigs` body for one session token.
///
/// # Errors
///
/// Never fails; kept fallible-free by construction.
#[must_use]
pub(crate) fn discovery_body(api_key: &str) -> Vec<u8> {
    GetCliModelConfigsRequest {
        metadata: Some(discovery_metadata(api_key)),
    }
    .encode_to_vec()
}

/// Builds the `GetChatMessageRequest` of one turn.
///
/// The route must be a Devin API route; its id is the wire model only when
/// the caller already resolved routing, otherwise `chat_model_uid` wins.
///
/// # Errors
///
/// [`ProviderError::InvalidRequest`] for a non-Devin route;
/// [`ProviderError::UnresolvedBlob`] from history lowering. Advertised tool
/// names map through `wire_name`; collisions across them are the join's
/// `ToolNames` table, restored on the `EventStream`.
pub(crate) fn chat_message(
    input: &DevinChatInput<'_>,
) -> Result<GetChatMessageRequest, ProviderError> {
    if !matches!(
        input.request.model,
        ModelRoute::Api {
            family: Family::Devin,
            ..
        }
    ) {
        return Err(ProviderError::InvalidRequest {
            message: String::from("a Devin body needs a devin model route"),
        });
    }
    let gemini_route =
        is_gemini_uid(input.request.model.id()) || is_gemini_uid(input.chat_model_uid);
    let mut tools = Vec::with_capacity(input.request.tools.len());
    for tool in input.request.tools.iter() {
        let schema = tool_schema_text(&tool.parameters, gemini_route);
        tools.push(ChatToolDefinition {
            name: wire_name(&tool.name, WIRE_NAME_MAX).into_owned(),
            description: tool.description.to_string(),
            json_schema_string: schema,
            strict: false,
        });
    }
    Ok(GetChatMessageRequest {
        metadata: Some(wire_metadata(input.api_key, input.user_jwt)),
        prompt: input.request.system.to_string(),
        chat_message_prompts: chat_prompts(input.request, input.cascade_id)?,
        chat_model_uid: String::from(input.chat_model_uid),
        request_type: chat_message_request_type::CASCADE,
        configuration: Some(CompletionConfiguration {
            num_completions: 1,
            max_tokens: u64::from(input.max_tokens),
            max_newlines: MAX_NEWLINES,
            temperature: input.temperature.unwrap_or(DEFAULT_TEMPERATURE),
            first_temperature: input.temperature.unwrap_or(DEFAULT_TEMPERATURE),
            top_k: DEFAULT_TOP_K,
            top_p: float32_round(input.top_p.unwrap_or(DEFAULT_TOP_P)),
            stop_patterns: input.stop_sequences.to_vec(),
            fim_eot_prob_threshold: 1.0,
        }),
        tools,
        disable_parallel_tool_calls: !input.parallel_tool_calls,
        tool_choice: Some(ChatToolChoice {
            option_name: Some(String::from("auto")),
            tool_name: None,
        }),
        system_prompt_cache_options: Some(PromptCacheOptions {
            r#type: cache_control_type::EPHEMERAL,
        }),
        cascade_id: String::from(input.cascade_id),
        planner_mode: conversational_planner_mode::DEFAULT,
        execution_id: String::from(input.execution_id),
        model_assignment_jwt: input.assignment_jwt.map(String::from),
    })
}

/// Encodes, gzips, and frames one chat request for the wire.
///
/// # Errors
///
/// The errors of [`chat_message`], plus [`ProviderError::Protocol`] when the
/// body does not compress.
pub(crate) fn chat_frame(input: &DevinChatInput<'_>) -> Result<Vec<u8>, ProviderError> {
    let body = chat_message(input)?.encode_to_vec();
    let gz = gzip(&body).map_err(|error| match error {
        ConnectError::Deflate { detail } => ProviderError::Protocol {
            family: Family::Devin,
            detail: format!("devin chat body did not compress: {detail}"),
        },
        other => ProviderError::Protocol {
            family: Family::Devin,
            detail: other.to_string(),
        },
    })?;
    Ok(frame_message(&gz, COMPRESSED_FLAG))
}

/// Rounds a double the way the CLI's float32 widens it.
///
/// The narrowing `as` is the point: the CLI sends top-p as a float32 widened
/// to double, so the adapter rounds identically.
#[expect(
    clippy::cast_possible_truncation,
    reason = "narrowing cast intentional: mirrors CLI float32 rounding"
)]
fn float32_round(value: f64) -> f64 {
    f64::from(value as f32)
}

/// Whether a model uid routes to the Gemini backend.
///
/// [INFERENCE]: the pinned reference classifies through its model taxonomy;
/// dal's catalog owns taxonomy, so the adapter matches only the server's own
/// enum namespace prefix. A plain `gemini` substring misroutes: any present
/// or future non-Gemini uid mentioning the word would scalarize schemas the
/// backend never asked for. Listed as a join seam: the catalog should own
/// this flag and hand it down through [`DevinChatInput`] instead of sniffing
/// the uid here.
fn is_gemini_uid(uid: &str) -> bool {
    uid.starts_with("MODEL_GOOGLE_GEMINI_")
}

/// Tool schema text, scalarizing type arrays on Gemini routes.
fn tool_schema_text(parameters: &dal_core::RawJson, gemini_route: bool) -> String {
    let text = parameters.as_str();
    if !gemini_route {
        return String::from(text);
    }
    let Ok(mut value) = sonic_rs::from_str::<sonic_rs::Value>(text) else {
        return String::from(text);
    };
    scalarize_type_arrays(&mut value);
    sonic_rs::to_string(&value).unwrap_or_else(|_| String::from(text))
}

/// Rewrites `"type": [T, "null"]` into `"type": T, "nullable": true`,
/// recursively. Any other shape passes through untouched.
fn scalarize_type_arrays(value: &mut sonic_rs::Value) {
    if let Some(object) = value.as_object_mut() {
        let keys: Vec<String> = object.iter().map(|(key, _)| String::from(key)).collect();
        for key in &keys {
            if let Some(child) = object.get_mut(key) {
                scalarize_type_arrays(child);
            }
        }
        let Some(kind) = object.get_mut(&"type") else {
            return;
        };
        let Some(items) = kind.as_array() else {
            return;
        };
        let mut scalar: Option<String> = None;
        let mut nullable = false;
        let mut multi = false;
        for item in items {
            let Some(name) = item.as_str() else {
                return;
            };
            if name == "null" {
                nullable = true;
            } else if scalar.is_none() {
                scalar = Some(String::from(name));
            } else {
                multi = true;
            }
        }
        let Some(scalar) = scalar else {
            return;
        };
        if multi {
            return;
        }
        object.insert("type", &scalar);
        if nullable {
            object.insert("nullable", true);
        }
        return;
    }
    if let Some(items) = value.as_array_mut() {
        for item in items.iter_mut() {
            scalarize_type_arrays(item);
        }
    }
}

/// Extracts a bounded human error from an HTTP error body.
///
/// Mirrors the pinned `devinErrorDetail`: strict UTF-8, no proxy HTML, JSON
/// envelope unwrapping (`error.message`, `error`, or top-level `message`),
/// whitespace collapse, control-character rejection, and a UTF-16-unit cap
/// cut at a code-point boundary that never appends a marker. Returns `None`
/// when the body carries no trustworthy diagnostic.
///
/// The production rejection path offers a bounded, already-redacted body
/// with the response's content type and renders its generic fallback when
/// this returns `None`; the returned detail is re-redacted after JSON
/// unescaping, so an escaped credential never surfaces.
#[must_use]
pub(crate) fn error_detail(body: &[u8], content_type: Option<&str>) -> Option<String> {
    let text = std::str::from_utf8(body).ok()?.trim();
    if text.is_empty() {
        return None;
    }
    if content_type.is_some_and(|kind| kind.to_lowercase().contains("text/html")) {
        return None;
    }
    let mut text = String::from(text);
    if let Ok(parsed) = sonic_rs::from_str::<sonic_rs::Value>(&text)
        && let Some(unwrapped) = envelope_message(&parsed)
    {
        text = unwrapped;
    }
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() || is_html(&collapsed) || has_controls(&collapsed) {
        return None;
    }
    Some(truncate_utf16_units(
        &collapsed,
        MAX_ERROR_DETAIL_UTF16_UNITS,
    ))
}

/// Unwraps a JSON error envelope to its message text.
fn envelope_message(parsed: &sonic_rs::Value) -> Option<String> {
    if let Some(error) = parsed.get("error") {
        if let Some(message) = error.get("message").and_then(|message| message.as_str()) {
            return Some(message.trim().to_owned());
        }
        if let Some(message) = error.as_str() {
            return Some(message.trim().to_owned());
        }
    }
    parsed
        .get("message")
        .and_then(|message| message.as_str())
        .map(|message| message.trim().to_owned())
}

/// Whether the text opens like proxied HTML.
fn is_html(text: &str) -> bool {
    let lower = text.to_lowercase();
    let trimmed = lower.trim_start();
    if !trimmed.starts_with('<') {
        return false;
    }
    let rest = &trimmed[1..];
    rest.starts_with("html") || rest.starts_with("!doctype html")
}

/// Whether the text carries control characters besides whitespace.
fn has_controls(text: &str) -> bool {
    text.chars()
        .any(|point| point.is_control() && !matches!(point, ' ' | '\t' | '\n' | '\r'))
}

/// Keeps the longest prefix of whole code points within `max_units` UTF-16
/// code units, so a cut never splits a scalar and never grows the text.
fn truncate_utf16_units(text: &str, max_units: usize) -> String {
    let mut units = 0;
    for (index, point) in text.char_indices() {
        units += point.len_utf16();
        if units > max_units {
            return String::from(&text[..index]);
        }
    }
    String::from(text)
}
