//! One-shot remote compaction attempts per family.
//!
//! Each function performs exactly one attempt. The provider lifecycle owns
//! retry classification, credential refresh, the Codex retry cap, and the
//! request semaphore.

use std::sync::atomic::{AtomicBool, Ordering};

use dal_core::Family;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use super::{
    AttemptResult, CompactOutcome,
    bodies::{
        codex_compaction_body, decode_codex_events, parse_anthropic_block, parse_responses_output,
    },
    support::{decode_api_error, redact, top_level_string},
};
use crate::{
    error::ProviderError,
    family::{anthropic::AnthropicWire, codex::CodexWire},
    http::{self, Exchange},
    lifecycle::AttemptFailure,
    sse,
};

/// Returns `Unsupported` without accepting a client or making a request.
#[must_use]
pub(crate) const fn chat() -> CompactOutcome {
    CompactOutcome::Unsupported
}

/// Makes one `OpenAI` Responses `/responses/compact` request.
///
/// `request_body` is the body produced by [`crate::family::responses::request_body`].
/// This adapter preserves its `model`, `instructions`, and every raw input value,
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
    let body = super::bodies::compact_responses_body(request_body, model)
        .map_err(AttemptFailure::Provider)?;
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
        let redactions: Vec<(&str, &str)> = headers
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        let Some(failure) =
            status_failure(response, Family::Responses, &redactions, cancel).await?
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
    let chunks = response.bytes_stream().map(|chunk| {
        if let Ok(bytes) = chunk {
            bytes.to_vec()
        } else {
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
    let actual_model = top_level_string(&wire.body, "model").map_err(AttemptFailure::Provider)?;
    if actual_model != model {
        return Err(AttemptFailure::Provider(ProviderError::InvalidRequest {
            message: String::from("Anthropic compaction body model does not match its request"),
        }));
    }
    let mut request = client.post(
        http::endpoint(Family::Anthropic, base_url, wire.path).map_err(AttemptFailure::Provider)?,
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
        let Some(failure) =
            status_failure(response, Family::Anthropic, &redactions, cancel).await?
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

async fn status_failure<S>(
    response: reqwest::Response,
    family: Family,
    redactions: &[(&str, S)],
    cancel: &CancellationToken,
) -> Result<Option<AttemptFailure>, AttemptFailure>
where
    S: AsRef<str>,
{
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
