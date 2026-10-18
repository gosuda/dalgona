//! Devin history lowering: neutral context into Cascade prompts.
//!
//! [`chat_prompts`] maps one [`ModelRequest`] onto the `chatMessagePrompts`
//! of a `GetChatMessageRequest`, and [`router_prompt`] builds the lone user
//! prompt the `AssignModel` call scores. The mapping mirrors the pinned
//! reference's history builder (`a4dc3302`, `buildChatMessagePrompts`):
//! user turns become `USER` prompts with inline images, assistant turns
//! become `SYSTEM` prompts carrying text, native thinking plus its signature,
//! and tool calls, and tool results become `TOOL` prompts keyed by call id.
//!
//! Three dal realities shape the port. First, thinking signatures travel on
//! the thinking part's replay payload, so only native turns (same family and
//! model) keep them; foreign thinking is demoted to bare prompt text with no
//! wrapper tag, which is what the pinned history test asserts. Second, dal
//! has no response-id slot on an assistant message, so native turns reuse the
//! id stored in the message-level replay payload when the decoder captured
//! one, and fall back to the deterministic id otherwise. Third, tool-call ids
//! are remapped once, over the whole context, through one [`CallIdMap`]: ids
//! from Responses-family turns shed their `|item` half first, everything is
//! restricted to `[A-Za-z0-9_-]` and capped at 40 characters with a `_dup`
//! suffix on collision, and results resolve through the same map so calls
//! never lose their results.

use std::collections::{HashMap, HashSet};

use base64::{Engine, engine::general_purpose::STANDARD};
use dal_core::{
    AssistantPart, CallId, ContextItem, Family, ModelRequest, Part, RawJson, ReplaySource,
};
use sha2::{Digest, Sha256};

use super::proto::{ChatMessagePrompt, ChatToolCall, ImageData, chat_message_source};
use crate::error::ProviderError;
use crate::tool_names::{WIRE_NAME_MAX, wire_name};

/// Longest tool-call id the chat backends accept.
pub(crate) const TOOL_CALL_ID_MAX: usize = 40;

/// Builds the full history of one chat request.
///
/// Message ids seed from `cascade_id`, the context index, and the role, so
/// two builds of the same request are equal byte for byte. Assistant messages
/// with neither text, thinking, signature, nor calls are skipped, matching
/// the pinned builder's empty-turn rule.
///
/// # Errors
///
/// [`ProviderError::UnresolvedBlob`] for a stored-blob part; the caller
/// inlines blobs before lowering. [`ProviderError::InvalidRequest`] for a
/// tool result with an empty call id, which the server could not route.
pub(crate) fn chat_prompts(
    request: &ModelRequest,
    cascade_id: &str,
) -> Result<Vec<ChatMessagePrompt>, ProviderError> {
    let model_id = request.model.id();
    let mut ids = CallIdMap::new();
    ids.collect(&request.context);
    let mut prompts = Vec::with_capacity(request.context.len());
    for (index, item) in request.context.iter().enumerate() {
        match item {
            ContextItem::User { parts } => {
                let seed = format!("{cascade_id}\0{index}\0user");
                prompts.push(user_prompt(parts, &deterministic_id(&seed))?);
            }
            ContextItem::Assistant {
                source,
                parts,
                replay,
            } => {
                if let Some(prompt) = assistant_prompt(
                    source,
                    parts,
                    replay.as_ref(),
                    model_id,
                    cascade_id,
                    index,
                    &mut ids,
                ) {
                    prompts.push(prompt);
                }
            }
            ContextItem::ToolResult {
                call,
                parts,
                is_error,
                ..
            } => {
                prompts.push(tool_prompt(
                    call, parts, *is_error, cascade_id, index, &mut ids,
                )?);
            }
        }
    }
    Ok(prompts)
}

/// Builds the router prompt: the latest user turn alone, id left empty.
///
/// The chat request that follows mints the turn id; the router scores the
/// current turn on its own.
///
/// # Errors
///
/// [`ProviderError::UnresolvedBlob`] when the latest user turn carries a
/// stored-blob part. The error propagates instead of falling back to an
/// older turn: scoring a stale turn while [`chat_prompts`] fails the same
/// request would bill an assignment against history the chat never sends.
pub(crate) fn router_prompt(
    request: &ModelRequest,
) -> Result<Option<ChatMessagePrompt>, ProviderError> {
    request
        .context
        .iter()
        .rev()
        .find_map(|item| match item {
            ContextItem::User { parts } => Some(parts),
            ContextItem::Assistant { .. } | ContextItem::ToolResult { .. } => None,
        })
        .map(|parts| user_prompt(parts, ""))
        .transpose()
}

/// Formats the leading bytes of the SHA-256 of `seed` as a UUID-shaped id.
///
/// The pinned reference hashes with SHA-256 and lays out 8-4-4-4-12 hex
/// groups; ids stay stable across content edits and history rebuilds because
/// the seed carries position, never text.
#[must_use]
pub(crate) fn deterministic_id(seed: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(seed.as_bytes());
    let mut hex = String::with_capacity(32);
    for byte in digest.iter().take(16) {
        hex.push(HEX[(byte >> 4) as usize] as char);
        hex.push(HEX[(byte & 0x0f) as usize] as char);
    }
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Restricts one id to the backend alphabet, capped at [`TOOL_CALL_ID_MAX`].
///
/// Characters outside `[A-Za-z0-9_-]` become `_`.
fn restrict_call_id(base: &str) -> String {
    base.chars()
        .map(|point| {
            if point.is_ascii_alphanumeric() || point == '-' || point == '_' {
                point
            } else {
                '_'
            }
        })
        .take(TOOL_CALL_ID_MAX)
        .collect()
}

/// Sanitizes a Responses-family id, shedding the `|item` half first.
fn sanitize_responses_call_id(raw: &str) -> String {
    restrict_call_id(raw.split('|').next().unwrap_or(raw))
}

/// Sanitizes an opaque id, keeping every segment.
fn sanitize_opaque_call_id(raw: &str) -> String {
    restrict_call_id(raw)
}

/// One original-to-wire id table for a whole context.
///
/// The table is filled in a first pass over every assistant message, so a
/// result always resolves to the same wire id as its call no matter where
/// the two land in document order. Collisions gain a `_dup` suffix inside
/// the length cap.
#[derive(Debug, Default)]
struct CallIdMap {
    /// Original id to wire id.
    wire: HashMap<String, String>,
    /// Wire ids already handed out.
    used: HashSet<String>,
}

impl CallIdMap {
    /// An empty table.
    fn new() -> Self {
        Self::default()
    }

    /// Records every assistant tool-call id of the context.
    fn collect(&mut self, context: &[ContextItem]) {
        for item in context {
            let ContextItem::Assistant { source, parts, .. } = item else {
                continue;
            };
            let responses_origin = is_responses_family(source);
            for part in parts {
                let AssistantPart::ToolCall { call, .. } = part else {
                    continue;
                };
                if responses_origin {
                    self.assign_responses(call.as_str());
                } else {
                    self.assign_opaque(call.as_str());
                }
            }
        }
    }

    /// The wire id of one Responses-family call-site id.
    fn assign_responses(&mut self, original: &str) -> String {
        self.assign_inner(original, true)
    }

    /// The wire id of one opaque call-site id.
    fn assign_opaque(&mut self, original: &str) -> String {
        self.assign_inner(original, false)
    }

    /// The wire id of one call-site id, allocating on first sight.
    fn assign_inner(&mut self, original: &str, responses_origin: bool) -> String {
        if let Some(wire) = self.wire.get(original) {
            return wire.clone();
        }
        let base = if responses_origin {
            sanitize_responses_call_id(original)
        } else {
            sanitize_opaque_call_id(original)
        };
        let wire = dedup(base, &mut self.used);
        self.wire.insert(original.to_owned(), wire.clone());
        wire
    }

    /// The wire id of one result id: the call's id when the call was seen,
    /// else a fresh sanitized id that can never collide with one.
    fn resolve(&mut self, original: &str) -> String {
        if let Some(wire) = self.wire.get(original) {
            return wire.clone();
        }
        self.assign_opaque(original)
    }
}

/// Whether tool-call ids from `source` shed their `|item` half.
fn is_responses_family(source: &ReplaySource) -> bool {
    matches!(source.family, Family::Responses | Family::Codex)
}

/// Appends a `_dup` suffix until `base` is unused, inside the length cap.
fn dedup(base: String, used: &mut HashSet<String>) -> String {
    if !used.contains(&base) {
        used.insert(base.clone());
        return base;
    }
    let mut counter = 2_u32;
    loop {
        let suffix = format!("_dup{counter}");
        let keep = TOOL_CALL_ID_MAX.saturating_sub(suffix.len());
        let candidate: String = base.chars().take(keep).collect::<String>() + &suffix;
        if !used.contains(&candidate) {
            used.insert(candidate.clone());
            return candidate;
        }
        counter = counter.saturating_add(1);
    }
}

/// Whether `source` produced the current route's model natively.
///
/// The pin compares api, provider, and model; dal's replay source carries
/// family and model only, so those two decide.
fn is_native(source: &ReplaySource, model_id: &str) -> bool {
    source.family == Family::Devin && source.model.as_ref() == model_id
}

/// The thinking signature stored on one thinking part's replay payload.
fn replay_signature(replay: Option<&RawJson>) -> Option<String> {
    let replay = replay?;
    let parsed: ThinkingSignature = replay.decode_as().ok()?;
    parsed.signature.filter(|signature| !signature.is_empty())
}

/// The native response id stored on one message-level replay payload.
fn replay_message_id(replay: Option<&RawJson>) -> Option<String> {
    let replay = replay?;
    let parsed: MessageIdentity = replay.decode_as().ok()?;
    parsed.message_id.filter(|id| !id.is_empty())
}

/// The `{ signature }` half of a thinking replay item.
#[derive(serde::Deserialize)]
struct ThinkingSignature {
    /// The server-issued signature, when the item carries one.
    #[serde(default)]
    signature: Option<String>,
}

/// The `{ message_id }` half of a message replay item.
#[derive(serde::Deserialize)]
struct MessageIdentity {
    /// The server-issued response id, when the item carries one.
    #[serde(default)]
    message_id: Option<String>,
}

/// One user turn as a `USER` prompt with inline images.
fn user_prompt(parts: &[Part], message_id: &str) -> Result<ChatMessagePrompt, ProviderError> {
    let mut prompt = String::new();
    let mut images = Vec::new();
    for part in parts {
        match part {
            Part::Text { text } => prompt.push_str(text),
            Part::Image { mime, bytes } => images.push(ImageData {
                base64_data: STANDARD.encode(bytes),
                mime_type: mime.to_string(),
                caption: String::new(),
            }),
            Part::Blob { blob_id, .. } => {
                return Err(ProviderError::UnresolvedBlob { blob_id: *blob_id });
            }
        }
    }
    Ok(ChatMessagePrompt {
        message_id: String::from(message_id),
        source: chat_message_source::USER,
        prompt,
        images,
        ..Default::default()
    })
}

/// One assistant turn as a `SYSTEM` prompt, or `None` when the turn is empty.
///
/// Native turns keep thinking text and the first signature; foreign thinking
/// joins the prompt as bare text. Tool calls map through `ids`; calls with
/// an empty id or name are skipped, because the server rejects them.
fn assistant_prompt(
    source: &ReplaySource,
    parts: &[AssistantPart],
    replay: Option<&RawJson>,
    model_id: &str,
    cascade_id: &str,
    index: usize,
    ids: &mut CallIdMap,
) -> Option<ChatMessagePrompt> {
    let native = is_native(source, model_id);
    let responses_origin = is_responses_family(source);
    let mut prompt_text = String::new();
    let mut thinking_text = String::new();
    let mut signature = String::new();
    let mut tool_calls = Vec::new();
    for part in parts {
        match part {
            AssistantPart::Text { text, .. } => prompt_text.push_str(text),
            AssistantPart::Thinking { text, replay } => {
                if !native {
                    if text.is_empty() {
                        continue;
                    }
                    prompt_text.push_str(text);
                    prompt_text.push('\n');
                    continue;
                }
                thinking_text.push_str(text);
                if signature.is_empty()
                    && let Some(kept) = replay_signature(replay.as_ref())
                {
                    signature = kept;
                }
            }
            AssistantPart::ToolCall {
                call, name, args, ..
            } => {
                if call.as_str().is_empty() || name.is_empty() {
                    continue;
                }
                tool_calls.push(ChatToolCall {
                    id: if responses_origin {
                        ids.assign_responses(call.as_str())
                    } else {
                        ids.assign_opaque(call.as_str())
                    },
                    name: wire_name(name, WIRE_NAME_MAX).into_owned(),
                    arguments_json: String::from(args.as_str()),
                });
            }
        }
    }
    if prompt_text.is_empty()
        && thinking_text.is_empty()
        && signature.is_empty()
        && tool_calls.is_empty()
    {
        return None;
    }
    let message_id = if native {
        replay_message_id(replay)
            .unwrap_or_else(|| deterministic_id(&format!("{cascade_id}\0{index}\0assistant")))
    } else {
        deterministic_id(&format!("{cascade_id}\0{index}\0assistant"))
    };
    Some(ChatMessagePrompt {
        message_id,
        source: chat_message_source::SYSTEM,
        prompt: prompt_text,
        thinking: thinking_text,
        signature,
        tool_calls,
        ..Default::default()
    })
}

/// One tool result as a `TOOL` prompt.
///
/// Results with an empty call id abort the turn with
/// [`ProviderError::InvalidRequest`]: unlike an empty-id call, which is
/// skipped because the server rejects it and nothing is lost, dropping a
/// result would silently lose tool output and desynchronize the call/result
/// pairing the server scores. An empty id is corrupt input, so it fails
/// loudly like [`ProviderError::UnresolvedBlob`].
/// Images ride in the same message, after the text.
fn tool_prompt(
    call: &CallId,
    parts: &[Part],
    is_error: bool,
    cascade_id: &str,
    index: usize,
    ids: &mut CallIdMap,
) -> Result<ChatMessagePrompt, ProviderError> {
    if call.as_str().is_empty() {
        return Err(ProviderError::InvalidRequest {
            message: String::from("a Devin tool result needs the id of its call"),
        });
    }
    let mut prompt = String::new();
    let mut images = Vec::new();
    for part in parts {
        match part {
            Part::Text { text } => prompt.push_str(text),
            Part::Image { mime, bytes } => images.push(ImageData {
                base64_data: STANDARD.encode(bytes),
                mime_type: mime.to_string(),
                caption: String::new(),
            }),
            Part::Blob { blob_id, .. } => {
                return Err(ProviderError::UnresolvedBlob { blob_id: *blob_id });
            }
        }
    }
    let wire = ids.resolve(call.as_str());
    let seed = format!("{cascade_id}\0{index}\0tool\0{wire}");
    Ok(ChatMessagePrompt {
        message_id: deterministic_id(&seed),
        source: chat_message_source::TOOL,
        prompt,
        tool_call_id: wire,
        tool_result_is_error: is_error,
        images,
        ..Default::default()
    })
}
