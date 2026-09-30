//! Codex request shaping and HTTPS transport.
//!
//! Codex shares the Responses wire body and decoder. Its transport identity and
//! summary capability are specific to the resolved Codex route.

use std::{
    borrow::Cow,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use dal_core::{Family, ModelRequest, ModelRoute, SessionId};
use futures::{Stream, StreamExt, stream};
use sonic_rs::JsonValueTrait;

use crate::{
    auth::{
        credential::{OAuthCredential, codex_identity},
        oauth::CODEX_ORIGINATOR,
    },
    error::ProviderError,
    family::responses,
    http::{self, Exchange},
    lifecycle::AttemptFailure,
    sse,
    stream::EventStream,
    thinking::WireThinking,
};

/// Codex's endpoint path relative to its configured base URL.
pub(crate) const PATH: &str = "responses";

/// The inputs shared by Codex HTTPS and WebSocket request construction.
pub(crate) struct CodexRequest<'a> {
    /// Provider-neutral request after the caller has inlined stored blobs.
    pub(crate) request: &'a ModelRequest,
    /// The already-clamped `OpenAI` thinking fragment.
    pub(crate) thinking: WireThinking,
    /// True only when the resolved Codex catalog row confirms summary support.
    pub(crate) reasoning_summaries: bool,
    /// The Codex OAuth credential used for this request.
    pub(crate) credential: &'a OAuthCredential,
    /// Session identity used by all three Codex session headers.
    pub(crate) session_id: SessionId,
    /// The already-rendered dalgon user-agent.
    pub(crate) user_agent: &'a str,
}

/// A Codex request ready for either transport.
///
/// This type intentionally has no `Debug` implementation because its headers
/// contain an access token.
pub(crate) struct CodexWire {
    /// Headers shared by HTTPS and WebSocket, excluding transport-specific
    /// `accept` and WebSocket beta headers.
    pub(crate) headers: Vec<(&'static str, String)>,
    /// The request body with the HTTPS Responses shape (`stream: true`).
    pub(crate) body: Vec<u8>,
    /// The resolved Codex model id.
    pub(crate) model: Box<str>,
    /// The session identity carried by the three session headers.
    pub(crate) session_id: SessionId,
    /// The user-agent required on both transports.
    pub(crate) user_agent: String,
}

/// Builds a Codex body by reusing the Responses body builder and adding the
/// Codex-only identity headers.
///
/// `reasoning.summary` is enabled only by the resolved catalog capability;
/// model names are never used to infer it. The request cache key is preserved
/// independently from the session id in `session-id`, `thread-id`, and
/// `x-client-request-id`.
pub(crate) fn build(input: &CodexRequest<'_>) -> Result<CodexWire, ProviderError> {
    let ModelRoute::Api {
        family: Family::Codex,
        model,
    } = &input.request.model
    else {
        return Err(ProviderError::InvalidRequest {
            message: String::from("a Codex body needs an openai_codex model route"),
        });
    };
    let model = model.to_string().into_boxed_str();
    let identity = input
        .credential
        .id_token
        .as_deref()
        .and_then(codex_identity);
    let account_id = input
        .credential
        .account_id
        .as_deref()
        .filter(|account_id| !account_id.trim().is_empty())
        .or_else(|| {
            identity
                .as_ref()
                .map(|identity| identity.account_id.as_str())
        })
        .ok_or_else(|| ProviderError::InvalidRequest {
            message: String::from("openai-codex credential has no ChatGPT account id"),
        })?;

    let session_id = input.session_id.to_string();
    let body = responses::request_body(input.request, input.thinking, input.reasoning_summaries)?;
    let mut bearer = String::from("Bearer ");
    bearer.push_str(input.credential.access_token.expose());
    let headers = vec![
        ("authorization", bearer),
        ("chatgpt-account-id", account_id.to_owned()),
        ("originator", String::from(CODEX_ORIGINATOR)),
        ("session-id", session_id.clone()),
        ("thread-id", session_id.clone()),
        ("x-client-request-id", session_id),
    ];
    Ok(CodexWire {
        headers,
        body,
        model,
        session_id: input.session_id,
        user_agent: String::from(input.user_agent),
    })
}

/// Returns the full-context WebSocket frame. Every turn, including one on a
/// reused socket, uses this same `response.create` shape; there is no
/// `previous_response_id` or other server-side continuation field.
pub(crate) fn websocket_frame(body: &[u8]) -> Result<Vec<u8>, ProviderError> {
    const MARKER: &[u8] = b",\"stream\":true,\"include\":";
    const STREAM_MEMBER: &[u8] = b",\"stream\":true";
    const PREFIX: &[u8] = b"{\"type\":\"response.create\",";

    if body.first() != Some(&b'{') || body.last() != Some(&b'}') {
        return Err(invalid_body());
    }
    let Some(marker_at) = body
        .windows(MARKER.len())
        .rposition(|window| window == MARKER)
    else {
        return Err(invalid_body());
    };
    let stream_end = marker_at + STREAM_MEMBER.len();
    let mut frame = Vec::with_capacity(body.len() + PREFIX.len() - 1 - STREAM_MEMBER.len());
    frame.extend_from_slice(PREFIX);
    frame.extend_from_slice(&body[1..marker_at]);
    frame.extend_from_slice(&body[stream_end..]);
    Ok(frame)
}

fn invalid_body() -> ProviderError {
    ProviderError::InvalidRequest {
        message: String::from("the Responses body cannot be used for a Codex WebSocket frame"),
    }
}

/// Borrows the bearer token from the generated request headers for redaction.
pub(crate) fn access_token(wire: &CodexWire) -> &str {
    wire.headers
        .iter()
        .find(|(name, _)| *name == "authorization")
        .and_then(|(_, value)| value.strip_prefix("Bearer "))
        .unwrap_or_default()
}

/// Replaces actual secret occurrences and borrows the unchanged case.
pub(crate) fn redact<'a>(text: &'a str, token: &str) -> Cow<'a, str> {
    if token.is_empty() || !text.contains(token) {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(text.replace(token, "<redacted>"))
    }
}

fn redact_provider_error(error: &mut ProviderError, token: &str) {
    if token.is_empty() {
        return;
    }
    let redact_string = |value: &mut String| {
        if value.contains(token) {
            *value = value.replace(token, "<redacted>");
        }
    };
    match error {
        ProviderError::Transport {
            reason: message, ..
        }
        | ProviderError::Status { message, .. }
        | ProviderError::InvalidRequest { message }
        | ProviderError::RateLimited { message, .. }
        | ProviderError::RetryAfterTooLong { message, .. }
        | ProviderError::Quota { message }
        | ProviderError::UsageNotIncluded { message }
        | ProviderError::ReserveUnavailable { message, .. }
        | ProviderError::TokenExchange { message, .. }
        | ProviderError::DeviceCode { message, .. } => redact_string(message),
        ProviderError::ContextOverflow { code, message, .. } => {
            redact_string(code);
            redact_string(message);
        }
        ProviderError::Protocol { detail, .. } => redact_string(detail),
        ProviderError::UsageLimit { model, message } => {
            redact_string(model);
            redact_string(message);
        }
        ProviderError::WsClosed {
            code: Some((_, reason)),
        } => redact_string(reason),
        _ => {}
    }
}

/// Sends and decodes one Codex HTTPS streaming request.
///
/// Non-success status bodies are read through the shared size-limited reader.
/// Dropping the returned `EventStream` drops the response body and cancels the
/// HTTP transfer.
pub(crate) async fn https(
    client: &reqwest::Client,
    base_url: &str,
    wire: CodexWire,
) -> Result<EventStream, AttemptFailure> {
    https_with_idle_timeout(client, base_url, wire, http::STREAM_IDLE_TIMEOUT).await
}

async fn https_with_idle_timeout(
    client: &reqwest::Client,
    base_url: &str,
    wire: CodexWire,
    idle_timeout: Duration,
) -> Result<EventStream, AttemptFailure> {
    let token = access_token(&wire).to_owned();
    let url = http::endpoint(Family::Codex, base_url, PATH)?;
    let mut request = client.post(url);
    for (name, value) in &wire.headers {
        request = request.header(*name, value);
    }
    let response = http::send(
        Family::Codex,
        request.body(wire.body),
        &wire.user_agent,
        Exchange::Stream,
        tokio::time::sleep,
    )
    .await?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .map(String::from);
        let body = http::read_body(Family::Codex, response).await?;
        let body = String::from_utf8_lossy(&body);
        let body = redact(&body, &token);
        if let Some(error) = crate::usage::map_codex_error(status, body.as_ref(), &wire.model) {
            return Err(AttemptFailure::Provider(error));
        }
        return Err(AttemptFailure::Response {
            status,
            code: response_code(body.as_ref()),
            message: response_message(body.as_ref()),
            retry_after,
        });
    }

    let read_failed = Arc::new(Mutex::new(false));
    let chunks = idle_limited_chunks(
        response.bytes_stream(),
        Arc::clone(&read_failed),
        idle_timeout,
    );
    let events = sse::decode_stream(chunks);
    let event_token = token;
    let decoded =
        responses::decode(events, Family::Codex, wire.model).map(move |event| match event {
            Err(ProviderError::StreamCut) if *lock(&read_failed) => {
                let mut error = ProviderError::Transport {
                    family: Family::Codex,
                    reason: String::from("Codex response body read failed"),
                };
                redact_provider_error(&mut error, &event_token);
                Err(error)
            }
            Err(mut error) => {
                redact_provider_error(&mut error, &event_token);
                Err(error)
            }
            Ok(event) => Ok(event),
        });
    Ok(EventStream::new(decoded, || {}))
}

fn response_message(body: &str) -> String {
    let value = sonic_rs::from_str::<sonic_rs::Value>(body).ok();
    value
        .as_ref()
        .and_then(|value| value.get("error"))
        .and_then(|error| error.get("message"))
        .and_then(JsonValueTrait::as_str)
        .or_else(|| {
            value
                .as_ref()
                .and_then(|value| value.get("detail"))
                .and_then(JsonValueTrait::as_str)
        })
        .map_or_else(String::new, String::from)
}

fn response_code(body: &str) -> Option<String> {
    let value = sonic_rs::from_str::<sonic_rs::Value>(body).ok()?;
    value
        .get("error")
        .and_then(|error| error.get("code").or_else(|| error.get("type")))
        .and_then(JsonValueTrait::as_str)
        .map(String::from)
}

fn idle_limited_chunks<S, B>(
    body: S,
    read_failed: Arc<Mutex<bool>>,
    idle_timeout: Duration,
) -> impl Stream<Item = Vec<u8>>
where
    S: Stream<Item = Result<B, reqwest::Error>> + Unpin + Send + 'static,
    B: AsRef<[u8]> + Send + 'static,
{
    stream::unfold(
        (body, read_failed, idle_timeout),
        |(mut body, read_failed, idle_timeout)| async move {
            match tokio::time::timeout(idle_timeout, body.next()).await {
                Ok(Some(Ok(bytes))) => {
                    Some((bytes.as_ref().to_vec(), (body, read_failed, idle_timeout)))
                }
                Ok(Some(Err(error))) => {
                    if !error.is_timeout() {
                        *lock(&read_failed) = true;
                    }
                    None
                }
                Ok(None) | Err(_) => None,
            }
        },
    )
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
