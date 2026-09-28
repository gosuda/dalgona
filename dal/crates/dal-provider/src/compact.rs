//! Remote history compaction for provider families that implement it.
//!
//! Each remote function performs exactly one attempt. The provider lifecycle
//! owns retry classification, credential refresh, the Codex retry cap, and the
//! request semaphore. A history is constructed only after a complete provider
//! response, so cancelling or failing an attempt cannot publish a partial one.

use std::{
    borrow::Cow,
    sync::atomic::{AtomicBool, Ordering},
};

use dal_core::{Family, RawJson};
use futures::{Stream, StreamExt};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::{
    error::ProviderError,
    family::{anthropic::AnthropicWire, codex::CodexWire, responses},
    http::{self, Exchange},
    lifecycle::AttemptFailure,
    sse::{self, SseEvent},
};

pub(crate) type AttemptResult = Result<Option<CompactedHistory>, AttemptFailure>;

/// Maximum number of Codex compaction stream retries, in addition to the first
/// attempt. The lifecycle owns and applies this cap.
pub(crate) const CODEX_STREAM_RETRIES: u32 = 2;

const CODEX_RETAINED_USER_TOKEN_LIMIT: usize = 64_000;
const CODEX_RETAINED_USER_BYTE_LIMIT: usize = CODEX_RETAINED_USER_TOKEN_LIMIT * 4;
const COMPACTION_TRIGGER: &[u8] = br#"{"type":"compaction_trigger"}"#;

/// The result of a remote compaction request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompactOutcome {
    /// Complete provider history, bound to the family and model that produced it.
    Compacted(CompactedHistory),
    /// This provider family has no remote compaction endpoint.
    Unsupported,
}

/// Provider-native raw history returned by a compaction endpoint.
///
/// The fields are private so a history cannot be rebound after it has been
/// produced. Construct it with the source identity and use
/// [`CompactedHistory::items_for`] before replaying it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompactedHistory {
    family: Family,
    model: Box<str>,
    items: Vec<RawJson>,
}

impl CompactedHistory {
    /// Binds provider-native raw items to the family and model that produced them.
    #[must_use]
    pub fn new(family: Family, model: impl Into<Box<str>>, items: Vec<RawJson>) -> Self {
        Self {
            family,
            model: model.into(),
            items,
        }
    }

    /// The provider family that produced this history.
    #[must_use]
    pub const fn family(&self) -> Family {
        self.family
    }

    /// The model id that produced this history.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The complete raw history items.
    #[must_use]
    pub fn items(&self) -> &[RawJson] {
        &self.items
    }

    /// Returns these items only when the request has the producing identity.
    ///
    /// # Errors
    /// Returns [`ProviderError::CompactionForeign`] for another family or model.
    pub fn items_for(&self, family: Family, model: &str) -> Result<&[RawJson], ProviderError> {
        if family != self.family || model != self.model.as_ref() {
            return Err(ProviderError::CompactionForeign {
                bound_family: self.family,
                bound_model: self.model.to_string(),
                family,
                model: String::from(model),
            });
        }
        Ok(&self.items)
    }
}

/// Returns `Unsupported` without accepting a client or making a request.
#[must_use]
pub(crate) const fn chat() -> CompactOutcome {
    CompactOutcome::Unsupported
}

/// Makes one OpenAI Responses `/responses/compact` request.
///
/// `request_body` is the body produced by [`responses::request_body`]. This
/// adapter preserves its `model`, `instructions`, and every raw input value,
/// while omitting the ordinary Responses-only fields. `headers` are the
/// credential headers already resolved by the provider, in send order.
pub(crate) async fn openai_responses(
    client: &reqwest::Client,
    base_url: &str,
    model: &str,
    request_body: &[u8],
    headers: &[(&'static str, String)],
    user_agent: &str,
    cancel: &CancellationToken,
) -> AttemptResult {
    let body = compact_responses_body(request_body, model).map_err(AttemptFailure::Provider)?;
    let url = http::endpoint(Family::Responses, base_url, "responses/compact")
        .map_err(AttemptFailure::Provider)?;
    let mut request = client.post(url);
    for (name, value) in headers {
        request = request.header(*name, value);
    }
    let Some(response) = send(
        Family::Responses,
        request.body(body),
        user_agent,
        Exchange::Json {
            total: http::COMPACT_TIMEOUT,
        },
        cancel,
    )
    .await?
    else {
        return Ok(None);
    };
    if !response.status().is_success() {
        let Some(failure) = status_failure(response, Family::Responses, headers, cancel).await?
        else {
            return Ok(None);
        };
        return Err(failure);
    }
    let Some(bytes) = read_body(response, Family::Responses, cancel).await? else {
        return Ok(None);
    };
    parse_responses_output(&bytes, model)
        .map(Some)
        .map_err(AttemptFailure::Provider)
}

/// Makes one Codex streaming `/responses` compaction attempt.
///
/// `wire` comes from [`crate::family::codex::build`], so the ordinary Codex
/// credential, account, originator, session, and user-agent contract is reused.
/// This function appends the trigger to that already-lowered input and scans
/// the existing Responses decoder's SSE grammar for a completed compaction
/// item. It does not retry.
pub(crate) async fn openai_codex(
    client: &reqwest::Client,
    base_url: &str,
    wire: CodexWire,
    cancel: &CancellationToken,
) -> AttemptResult {
    let (retained_users, body) =
        codex_compaction_body(&wire.body).map_err(AttemptFailure::Provider)?;
    let token = crate::family::codex::access_token(&wire).to_owned();
    let authorization = format!("Bearer {token}");
    let redactions = [("authorization", authorization.as_str())];
    let url = http::endpoint(Family::Codex, base_url, crate::family::codex::PATH)
        .map_err(AttemptFailure::Provider)?;
    let mut request = client.post(url);
    for (name, value) in &wire.headers {
        request = request.header(*name, value);
    }
    let Some(response) = send(
        Family::Codex,
        request.body(body),
        &wire.user_agent,
        Exchange::Stream,
        cancel,
    )
    .await?
    else {
        return Ok(None);
    };
    if !response.status().is_success() {
        let Some(failure) = status_failure(response, Family::Codex, &redactions, cancel).await?
        else {
            return Ok(None);
        };
        return Err(failure);
    }

    let read_failed = AtomicBool::new(false);
    let chunks = response.bytes_stream().map(|chunk| match chunk {
        Ok(bytes) => bytes.to_vec(),
        Err(_) => {
            read_failed.store(true, Ordering::Release);
            Vec::new()
        }
    });
    let events = sse::decode_stream(chunks);
    decode_codex_events(
        events,
        wire.model,
        retained_users,
        &read_failed,
        &token,
        cancel,
    )
    .await
}

/// Makes one Anthropic Messages compaction attempt using an already-built
/// summarize wire request.
///
/// `wire` must come from [`crate::family::anthropic::build`] with
/// `AnthropicRequest::summarize` set. The existing request codec supplies the
/// `compact-2026-09-04` beta and the top-level summarize directive.
pub(crate) async fn anthropic(
    client: &reqwest::Client,
    base_url: &str,
    model: &str,
    wire: AnthropicWire,
    default_user_agent: &str,
    cancel: &CancellationToken,
) -> AttemptResult {
    let actual_model =
        top_level_string(&wire.body, "model").map_err(AttemptFailure::Provider)?;
    if actual_model != model {
        return Err(AttemptFailure::Provider(ProviderError::InvalidRequest {
            message: String::from("Anthropic compaction body model does not match its request"),
        }));
    }
    let mut request = client.post(
        http::endpoint(Family::Anthropic, base_url, wire.path)
            .map_err(AttemptFailure::Provider)?,
    );
    for (name, value) in &wire.headers {
        request = request.header(*name, value);
    }
    let redactions: Vec<(&str, &str)> = wire
        .headers
        .iter()
        .filter(|(name, _)| matches!(*name, "x-api-key" | "authorization"))
        .map(|(name, value)| (*name, value.as_str()))
        .collect();
    let Some(response) = send(
        Family::Anthropic,
        request.body(wire.body),
        wire.user_agent.as_deref().unwrap_or(default_user_agent),
        Exchange::Json {
            total: http::COMPACT_TIMEOUT,
        },
        cancel,
    )
    .await?
    else {
        return Ok(None);
    };
    if !response.status().is_success() {
        let Some(failure) = status_failure(response, Family::Anthropic, &redactions, cancel).await?
        else {
            return Ok(None);
        };
        return Err(failure);
    }
    let Some(bytes) = read_body(response, Family::Anthropic, cancel).await? else {
        return Ok(None);
    };
    parse_anthropic_block(&bytes, model)
        .map(Some)
        .map_err(AttemptFailure::Provider)
}

async fn send(
    family: Family,
    request: reqwest::RequestBuilder,
    user_agent: &str,
    exchange: Exchange,
    cancel: &CancellationToken,
) -> Result<Option<reqwest::Response>, AttemptFailure> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => Ok(None),
        response = http::send(family, request, user_agent, exchange, tokio::time::sleep) => {
            response.map(Some).map_err(AttemptFailure::Provider)
        }
    }
}

async fn read_body(
    response: reqwest::Response,
    family: Family,
    cancel: &CancellationToken,
) -> Result<Option<Vec<u8>>, AttemptFailure> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => Ok(None),
        bytes = http::read_body(family, response) => {
            bytes.map(Some).map_err(AttemptFailure::Provider)
        }
    }
}

async fn status_failure(
    response: reqwest::Response,
    family: Family,
    redactions: &[(&str, &str)],
    cancel: &CancellationToken,
) -> Result<Option<AttemptFailure>, AttemptFailure> {
    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(String::from);
    let Some(bytes) = read_body(response, family, cancel).await? else {
        return Ok(None);
    };
    let decoded = decode_api_error(&bytes);
    let mut message = decoded.message.unwrap_or_default();
    redact(&mut message, redactions);
    Ok(Some(AttemptFailure::Response {
        status,
        code: decoded.code,
        message,
        retry_after,
    }))
}

fn redact(message: &mut String, secrets: &[(&str, &str)]) {
    for (name, value) in secrets {
        let secret = if *name == "authorization" {
            value.strip_prefix("Bearer ").unwrap_or(value)
        } else {
            value
        };
        if !secret.is_empty() && message.contains(secret) {
            *message = message.replace(secret, "<redacted>");
        }
    }
}

#[derive(Default, Deserialize)]
struct ApiError {
    error: Option<ApiErrorFields>,
    code: Option<String>,
    message: Option<String>,
    detail: Option<String>,
}

#[derive(Deserialize)]
struct ApiErrorFields {
    code: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    message: Option<String>,
}

fn decode_api_error(bytes: &[u8]) -> ParsedApiError {
    let decoded = sonic_rs::from_slice::<ApiError>(bytes).unwrap_or_default();
    let (code, message) = match decoded.error {
        Some(error) => (
            error.code.or(error.kind).or(decoded.code),
            error.message.or(decoded.message).or(decoded.detail),
        ),
        None => (decoded.code, decoded.message.or(decoded.detail)),
    };
    ParsedApiError { code, message }
}

#[derive(Default)]
struct ParsedApiError {
    code: Option<String>,
    message: Option<String>,
}

fn compact_responses_body(body: &[u8], expected_model: &str) -> Result<Vec<u8>, ProviderError> {
    let request = std::str::from_utf8(body).map_err(|error| invalid_json("request body", error))?;
    let mut model = None;
    let mut instructions = None;
    let mut input = None;
    for member in sonic_rs::to_object_iter(request) {
        let (name, value) = member.map_err(|error| invalid_json("request body", error))?;
        match name.as_ref() {
            "model" => model = Some(value.as_raw_cow()),
            "instructions" => instructions = Some(value.as_raw_cow()),
            "input" => input = Some(value.as_raw_cow()),
            _ => {}
        }
    }
    let model = model.ok_or_else(|| invalid("Responses body has no model"))?;
    let instructions =
        instructions.ok_or_else(|| invalid("Responses body has no instructions"))?;
    let input = input.ok_or_else(|| invalid("Responses body has no input"))?;
    let actual_model: String =
        sonic_rs::from_str(model.as_ref()).map_err(|error| invalid_json("model", error))?;
    if actual_model != expected_model {
        return Err(ProviderError::InvalidRequest {
            message: String::from("Responses compaction body model does not match its request"),
        });
    }
    let mut compact = Vec::with_capacity(model.len() + instructions.len() + input.len() + 38);
    compact.extend_from_slice(b"{\"model\":");
    compact.extend_from_slice(model.as_bytes());
    compact.extend_from_slice(b",\"instructions\":");
    compact.extend_from_slice(instructions.as_bytes());
    compact.extend_from_slice(b",\"input\":");
    compact.extend_from_slice(input.as_bytes());
    compact.push(b'}');
    Ok(compact)
}

fn codex_compaction_body(body: &[u8]) -> Result<(Vec<RawJson>, Vec<u8>), ProviderError> {
    let request = std::str::from_utf8(body).map_err(|error| invalid_json("request body", error))?;
    let mut input = None;
    for member in sonic_rs::to_object_iter(request) {
        let (name, value) = member.map_err(|error| invalid_json("request body", error))?;
        if name.as_ref() == "input" {
            input = Some(value.as_raw_cow());
            break;
        }
    }
    let input = input.ok_or_else(|| invalid("Responses body has no input"))?;
    let inputs = sonic_rs::to_array_iter(input.as_ref())
        .map(|item| {
            item.map(|item| item.as_raw_cow())
                .map_err(|error| invalid_json("input array", error))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let retained = retain_user_messages(&inputs)?;
    let request = append_input_item(body, input.as_ref(), &inputs)?;
    Ok((retained, request))
}

fn append_input_item(
    body: &[u8],
    input: &str,
    inputs: &[Cow<'_, str>],
) -> Result<Vec<u8>, ProviderError> {
    let input_start = source_offset(body, input)?;
    let insert = match inputs.last() {
        Some(last) => source_offset(body, last.as_ref())?
            .checked_add(last.len())
            .ok_or_else(|| invalid("Responses input offset overflowed"))?,
        None => input_start + 1,
    };
    let mut result = Vec::with_capacity(body.len() + COMPACTION_TRIGGER.len() + 1);
    result.extend_from_slice(&body[..insert]);
    if !inputs.is_empty() {
        result.push(b',');
    }
    result.extend_from_slice(COMPACTION_TRIGGER);
    result.extend_from_slice(&body[insert..]);
    Ok(result)
}

fn retain_user_messages(inputs: &[Cow<'_, str>]) -> Result<Vec<RawJson>, ProviderError> {
    let mut retained = Vec::new();
    let mut bytes = 0_usize;
    for item in inputs.iter().rev() {
        let item = item.as_ref();
        let role: InputRole<'_> = sonic_rs::from_str(item)
            .map_err(|error| invalid(format!("Responses input item is invalid: {error}")))?;
        if role.role != Some("user") {
            continue;
        }
        if item.len() > CODEX_RETAINED_USER_BYTE_LIMIT - bytes {
            break;
        }
        bytes += item.len();
        retained.push(
            RawJson::parse(item).map_err(|error| invalid(format!("invalid retained user message: {error}")))?,
        );
    }
    retained.reverse();
    Ok(retained)
}

#[derive(Deserialize)]
struct InputRole<'a> {
    #[serde(borrow)]
    role: Option<&'a str>,
}
fn parse_responses_output(bytes: &[u8], model: &str) -> Result<CompactedHistory, ProviderError> {
    let response: ResponsesCompactReply = sonic_rs::from_slice(bytes)
        .map_err(|error| protocol(Family::Responses, format!("malformed compact response: {error}")))?;
    Ok(CompactedHistory::new(Family::Responses, model, response.output))
}

#[derive(Deserialize)]
struct ResponsesCompactReply {
    output: Vec<RawJson>,
}

fn parse_anthropic_block(bytes: &[u8], model: &str) -> Result<CompactedHistory, ProviderError> {
    let reply: AnthropicCompactReply = sonic_rs::from_slice(bytes)
        .map_err(|error| protocol(Family::Anthropic, format!("malformed compact response: {error}")))?;
    if reply.stop_reason.as_deref() != Some("compaction") {
        return Err(missing(Family::Anthropic, "block"));
    }
    let Some(block) = reply.content.into_iter().next() else {
        return Err(missing(Family::Anthropic, "block"));
    };
    let kind: BlockKind = block
        .decode_as()
        .map_err(|error| protocol(Family::Anthropic, format!("malformed compaction block: {error}")))?;
    if kind.kind != "compaction" {
        return Err(missing(Family::Anthropic, "block"));
    }
    Ok(CompactedHistory::new(Family::Anthropic, model, vec![block]))
}

#[derive(Deserialize)]
struct AnthropicCompactReply {
    stop_reason: Option<String>,
    #[serde(default)]
    content: Vec<RawJson>,
}

#[derive(Deserialize)]
struct BlockKind {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct CompactEventHead {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct OutputItemDone {
    item: RawJson,
}

#[derive(Deserialize)]
struct OutputItemKind {
    #[serde(rename = "type")]
    kind: String,
}


async fn decode_codex_events<S>(
    events: S,
    model: Box<str>,
    retained_users: Vec<RawJson>,
    read_failed: &AtomicBool,
    token: &str,
    cancel: &CancellationToken,
) -> AttemptResult
where
    S: Stream<Item = Result<SseEvent, ProviderError>>,
{
    let mut events = Box::pin(events);
    let mut decoder = responses::Decoder::new(Family::Codex, model.clone());
    let mut compaction = None;
    let mut ignored = Vec::new();
    loop {
        let next = tokio::select! {
            biased;
            () = cancel.cancelled() => return Ok(None),
            next = events.next() => next,
        };
        let Some(event) = next else {
            if read_failed.load(Ordering::Acquire) {
                return Err(AttemptFailure::Provider(ProviderError::Transport {
                    family: Family::Codex,
                    reason: String::from("Codex response body read failed"),
                }));
            }
            return Err(AttemptFailure::Provider(ProviderError::StreamCut));
        };
        let event = event.map_err(AttemptFailure::Provider)?;
        if event.data == "[DONE]" {
            continue;
        }
        ignored.clear();
        let terminal = match decoder.feed(&event.data, &mut ignored) {
            Ok(terminal) => terminal,
            Err(error) => {
                return Err(AttemptFailure::Provider(redact_stream_error(error, token)));
            }
        };
        let head: CompactEventHead = sonic_rs::from_str(&event.data)
            .map_err(|error| AttemptFailure::Provider(protocol(Family::Codex, format!("malformed event: {error}"))))?;
        if head.kind == "response.output_item.done" {
            let done: OutputItemDone = sonic_rs::from_str(&event.data)
                .map_err(|error| AttemptFailure::Provider(protocol(Family::Codex, format!("malformed output item: {error}"))))?;
            let item: OutputItemKind = done
                .item
                .decode_as()
                .map_err(|error| AttemptFailure::Provider(protocol(Family::Codex, format!("malformed output item: {error}"))))?;
            if matches!(item.kind.as_str(), "compaction" | "compaction_summary" | "context_compaction")
                && compaction.is_none()
            {
                compaction = Some(done.item);
            }
        }
        if terminal {
            if head.kind != "response.completed" {
                if compaction.is_none() {
                    return Err(AttemptFailure::Provider(missing(Family::Codex, "item")));
                }
                return Err(AttemptFailure::Provider(protocol(
                    Family::Codex,
                    "compaction response did not complete",
                )));
            }
            let Some(compaction) = compaction else {
                return Err(AttemptFailure::Provider(missing(Family::Codex, "item")));
            };
            let mut items = retained_users;
            items.push(compaction);
            return Ok(Some(CompactedHistory::new(Family::Codex, model, items)));
        }
    }
}

fn redact_stream_error(error: ProviderError, token: &str) -> ProviderError {
    if token.is_empty() {
        return error;
    }
    let redact = |message: String| {
        if message.contains(token) {
            message.replace(token, "<redacted>")
        } else {
            message
        }
    };
    match error {
        ProviderError::Status {
            family,
            status,
            message,
        } => ProviderError::Status {
            family,
            status,
            message: redact(message),
        },
        ProviderError::ContextOverflow {
            family,
            code,
            message,
        } => ProviderError::ContextOverflow {
            family,
            code,
            message: redact(message),
        },
        ProviderError::RateLimited {
            message,
            retry_after,
        } => ProviderError::RateLimited {
            message: redact(message),
            retry_after,
        },
        ProviderError::UsageNotIncluded { message } => {
            ProviderError::UsageNotIncluded { message: redact(message) }
        }
        error => error,
    }
}


fn top_level_string(body: &[u8], key: &str) -> Result<String, ProviderError> {
    let body = std::str::from_utf8(body).map_err(|error| invalid_json("request body", error))?;
    for member in sonic_rs::to_object_iter(body) {
        let (name, value) = member.map_err(|error| invalid_json("request body", error))?;
        if name.as_ref() == key {
            let raw = value.as_raw_cow();
            return sonic_rs::from_str(raw.as_ref()).map_err(|error| invalid_json(key, error));
        }
    }
    Err(invalid(format!("provider request body has no {key} member")))
}

/// Gets the source-byte offset of a Sonic raw slice, after verifying its bytes.
fn source_offset(source: &[u8], raw: &str) -> Result<usize, ProviderError> {
    let start = (raw.as_ptr() as usize)
        .checked_sub(source.as_ptr() as usize)
        .ok_or_else(|| invalid("Sonic raw value precedes its request body"))?;
    let end = start
        .checked_add(raw.len())
        .ok_or_else(|| invalid("Sonic raw value offset overflowed"))?;
    if source.get(start..end) != Some(raw.as_bytes()) {
        return Err(invalid("Sonic raw value does not belong to its request body"));
    }
    Ok(start)
}
fn invalid(message: impl Into<String>) -> ProviderError {
    ProviderError::InvalidRequest {
        message: message.into(),
    }
}

fn invalid_json(what: &str, error: impl std::fmt::Display) -> ProviderError {
    invalid(format!("invalid {what} in provider request body: {error}"))
}

fn protocol(family: Family, detail: impl Into<String>) -> ProviderError {
    ProviderError::Protocol {
        family,
        detail: detail.into(),
    }
}

fn missing(family: Family, noun: &'static str) -> ProviderError {
    ProviderError::CompactionMissing { family, noun }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn responses_compaction_body_preserves_wire_values_byte_for_byte() {
        let request = br#"{"model":"m,1","instructions":"keep \"exact\"","input":[ {"role":"user","content":[{"text":"x","n":1.00}]} ],"tools":[],"store":false,"stream":true}"#;
        assert_eq!(
            compact_responses_body(request, "m,1").expect("valid Responses body"),
            br#"{"model":"m,1","instructions":"keep \"exact\"","input":[ {"role":"user","content":[{"text":"x","n":1.00}]} ]}"#
        );
    }

    #[test]
    fn codex_trigger_is_last_input_and_existing_items_keep_their_bytes() {
        let request = br#"{"model":"m","instructions":"i","input":[{"role":"user", "content":[{"text":"old"}]}],"store":false,"stream":true}"#;
        let (retained, body) = codex_compaction_body(request).expect("valid Codex body");
        assert_eq!(retained.len(), 1);
        assert_eq!(
            body,
            br#"{"model":"m","instructions":"i","input":[{"role":"user", "content":[{"text":"old"}]},{"type":"compaction_trigger"}],"store":false,"stream":true}"#
        );
    }

    #[test]
    fn codex_retention_keeps_newest_users_in_original_order_with_a_byte_budget() {
        let old =
            RawJson::parse(&format!("{{\"role\":\"user\",\"text\":\"{}\"}}", "a".repeat(140_000)))
                .expect("valid old message");
        let middle =
            RawJson::parse(&format!("{{\"role\":\"user\",\"text\":\"{}\"}}", "c".repeat(100_000)))
                .expect("valid middle message");
        let assistant = RawJson::parse(r#"{"role":"assistant","text":"not retained"}"#)
            .expect("valid assistant item");
        let newest =
            RawJson::parse(&format!("{{\"role\":\"user\",\"text\":\"{}\"}}", "b".repeat(120_000)))
                .expect("valid newest message");
        let input = [
            Cow::Borrowed(old.as_str()),
            Cow::Borrowed(middle.as_str()),
            Cow::Borrowed(assistant.as_str()),
            Cow::Borrowed(newest.as_str()),
        ];
        let retained = retain_user_messages(&input).expect("valid Responses input items");
        assert_eq!(
            retained.iter().map(RawJson::as_str).collect::<Vec<_>>(),
            [middle.as_str(), newest.as_str()]
        );
    }

    #[tokio::test]
    async fn codex_stream_without_compaction_item_is_typed_missing() {
        let event = SseEvent {
            name: None,
            data: String::from(r#"{"type":"response.completed","response":{"output":[]}}"#),
        };
        let read_failed = AtomicBool::new(false);
        let cancel = CancellationToken::new();
        let result = decode_codex_events(
            futures::stream::iter([Ok(event)]),
            String::from("gpt-6").into_boxed_str(),
            Vec::new(),
            &read_failed,
            "",
            &cancel,
        )
        .await;
        assert!(matches!(
            result,
            Err(AttemptFailure::Provider(ProviderError::CompactionMissing {
                family: Family::Codex,
                noun: "item",
            }))
        ));
    }

    #[tokio::test]
    async fn codex_stream_keeps_first_compaction_alias_raw() {
        let first = r#"{"type":"context_compaction", "encrypted_content":"secret", "n":1.00}"#;
        let second = r#"{"type":"compaction_summary","summary":"second"}"#;
        let events = [
            SseEvent {
                name: None,
                data: format!(r#"{{"type":"response.output_item.done","item":{first}}}"#),
            },
            SseEvent {
                name: None,
                data: format!(r#"{{"type":"response.output_item.done","item":{second}}}"#),
            },
            SseEvent {
                name: None,
                data: String::from(r#"{"type":"response.completed","response":{"output":[]}}"#),
            },
        ];
        let read_failed = AtomicBool::new(false);
        let cancel = CancellationToken::new();
        let history = decode_codex_events(
            futures::stream::iter(events.into_iter().map(Ok)),
            String::from("gpt-6").into_boxed_str(),
            Vec::new(),
            &read_failed,
            "secret",
            &cancel,
        )
        .await
        .expect("complete compaction item")
        .expect("not cancelled");
        assert_eq!(history.items()[0].as_str(), first);
    }

    #[test]
    fn codex_error_message_redacts_token_without_rewriting_output_items() {
        let error = redact_stream_error(
            ProviderError::RateLimited {
                message: String::from("secret rejected"),
                retry_after: Some(Duration::from_secs(2)),
            },
            "secret",
        );
        assert!(matches!(
            error,
            ProviderError::RateLimited { message, retry_after: Some(wait) }
                if message == "<redacted> rejected" && wait == Duration::from_secs(2)
        ));
    }

    #[test]
    fn response_output_and_anthropic_block_are_preserved_raw() {
        let responses = br#"{"output":[ {"type":"compaction", "encrypted_content":"enc-v1"} ]}"#;
        let history = parse_responses_output(responses, "gpt-6").expect("valid Responses fixture");
        assert_eq!(history.family(), Family::Responses);
        assert_eq!(history.model(), "gpt-6");
        assert_eq!(history.items()[0].as_str(), r#"{"type":"compaction", "encrypted_content":"enc-v1"}"#);

        let anthropic = br#"{"stop_reason":"compaction","content":[ {"type":"compaction", "content":"summary"} ]}"#;
        let block = parse_anthropic_block(anthropic, "claude-sonnet-5").expect("valid Anthropic fixture");
        assert_eq!(block.items()[0].as_str(), r#"{"type":"compaction", "content":"summary"}"#);
    }

    #[test]
    fn missing_anthropic_compaction_block_is_typed() {
        let error = parse_anthropic_block(
            br#"{"stop_reason":"end_turn","content":[{"type":"text","text":"no summary"}]}"#,
            "claude-sonnet-5",
        )
        .expect_err("ordinary response is not a compaction");
        assert!(matches!(error, ProviderError::CompactionMissing { family: Family::Anthropic, noun: "block" }));
    }

    #[test]
    fn compacted_history_refuses_another_family_or_model() {
        let history = CompactedHistory::new(
            Family::Responses,
            "gpt-6",
            vec![RawJson::parse(r#"{"type":"message"}"#).expect("valid item")],
        );
        assert!(history.items_for(Family::Responses, "gpt-6").is_ok());
        assert!(matches!(
            history.items_for(Family::Codex, "gpt-6"),
            Err(ProviderError::CompactionForeign { bound_family: Family::Responses, .. })
        ));
        assert!(matches!(
            history.items_for(Family::Responses, "gpt-7"),
            Err(ProviderError::CompactionForeign { bound_model, model, .. })
                if bound_model == "gpt-6" && model == "gpt-7"
        ));
    }

    #[test]
    fn chat_is_unsupported_without_a_transport_or_request_argument() {
        assert_eq!(chat(), CompactOutcome::Unsupported);
    }

    #[tokio::test]
    async fn cancelling_an_in_flight_responses_compaction_drops_the_request() {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("loopback bind");
        let address = listener.local_addr().expect("listener address");
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.expect("request accepted");
            let _ = accepted_tx.send(());
            std::future::pending::<()>().await;
        });
        let client = http::build_client();
        let cancel = CancellationToken::new();
        let body = br#"{"model":"m","instructions":"i","input":[]}"#;
        let attempt = tokio::spawn({
            let client = client.clone();
            let cancel = cancel.clone();
            async move {
                openai_responses(
                    &client,
                    &format!("http://{address}/v1"),
                    "m",
                    body,
                    &[],
                    "dalgon/test (test test; x64)",
                    &cancel,
                )
                .await
            }
        });
        accepted_rx.await.expect("request reached the server");
        cancel.cancel();
        let result = attempt.await.expect("attempt task joined");
        assert!(result.expect("cancelled attempt").is_none());
        server.abort();
    }
}
