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

use crate::{
    auth::{
        credential::{codex_identity, OAuthCredential},
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
    /// The already-clamped OpenAI thinking fragment.
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
pub(crate) fn build(input: CodexRequest<'_>) -> Result<CodexWire, ProviderError> {
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
        .or_else(|| identity.as_ref().map(|identity| identity.account_id.as_str()))
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
    let Some(marker_at) = body.windows(MARKER.len()).rposition(|window| window == MARKER) else {
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
        ProviderError::Transport { reason, .. } => redact_string(reason),
        ProviderError::Status { message, .. }
        | ProviderError::InvalidRequest { message }
        | ProviderError::RateLimited { message, .. }
        | ProviderError::RetryAfterTooLong { message, .. }
        | ProviderError::Quota { message }
        | ProviderError::UsageNotIncluded { message }
        | ProviderError::ReserveUnavailable { message, .. }
        | ProviderError::TokenExchange { message, .. }
        | ProviderError::DeviceCode { message, .. } => redact_string(message),
        ProviderError::ContextOverflow {
            code, message, ..
        } => {
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
        if let Some(error) =
            crate::usage::map_codex_error(status, body.as_ref(), &wire.model)
        {
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
    let chunks = idle_limited_chunks(response.bytes_stream(), Arc::clone(&read_failed), idle_timeout);
    let events = sse::decode_stream(chunks);
    let event_token = token;
    let decoded = responses::decode(events, Family::Codex, wire.model).map(move |event| match event {
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
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::{
        error::Error,
        sync::Arc,
        time::Duration,
    };

    use dal_core::{ContextItem, ModelToolSpec, Purpose, RequestParams, SessionId, ThinkingLevel};
    use futures::StreamExt;
    use sonic_rs::JsonValueTrait;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        time::timeout,
    };

    use super::{CODEX_ORIGINATOR, CodexRequest, build, https, https_with_idle_timeout, websocket_frame};
    use crate::{
        auth::credential::{OAuthCredential, SecretString},
        error::ProviderError,
        lifecycle::AttemptFailure,
        stream::StreamEvent,
        thinking::WireThinking,
    };

    fn wire(reasoning_summaries: bool) -> super::CodexWire {
        wire_for_model("gpt-test", reasoning_summaries)
    }

    fn wire_for_model(model: &str, reasoning_summaries: bool) -> super::CodexWire {
        let session_id = SessionId::new_v7();
        let request = dal_core::ModelRequest {
            purpose: Purpose::Turn,
            model: dal_core::ModelRoute::Api {
                family: dal_core::Family::Codex,
                model: model.into(),
            },
            system: Arc::from("system"),
            tools: Vec::<ModelToolSpec>::new().into(),
            context: Vec::<ContextItem>::new().into(),
            params: RequestParams {
                thinking: ThinkingLevel::High,
                effort: None,
                temperature: Some(0.7),
            },
            cache_key: Some(format!("{session_id}:1").into_boxed_str()),
        };
        let credential = OAuthCredential {
            access_token: SecretString::from("test-access"),
            refresh_token: SecretString::from("test-refresh"),
            expires_at: None,
            id_token: None,
            account_id: Some(String::from("account")),
        };
        build(CodexRequest {
            request: &request,
            thinking: WireThinking::OpenAi { effort: Some("high") },
            reasoning_summaries,
            credential: &credential,
            session_id,
            user_agent: "dalgon/test (Linux test; x86_64)",
        })
        .expect("Codex input has a valid route and identity")
    }

    async fn read_request(socket: &mut tokio::net::TcpStream) -> std::io::Result<()> {
        let mut headers = Vec::new();
        let mut byte = [0_u8; 1];
        loop {
            socket.read_exact(&mut byte).await?;
            headers.push(byte[0]);
            if headers.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let content_length = String::from_utf8_lossy(&headers)
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or_default();
        let mut body = vec![0; content_length];
        socket.read_exact(&mut body).await?;
        Ok(())
    }

    fn http_response(status: u16, headers: &[(&str, &str)], body: &str) -> String {
        let mut response = format!(
            "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        );
        for (name, value) in headers {
            response.push_str(name);
            response.push_str(": ");
            response.push_str(value);
            response.push_str("\r\n");
        }
        response.push_str("\r\n");
        response.push_str(body);
        response
    }

    #[tokio::test]
    async fn codex_https_keeps_successful_text_that_matches_access_token() -> Result<(), Box<dyn Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        let body = concat!(
            "data: {\"type\":\"response.output_text.delta\",\"sequence_number\":1,\"delta\":\"test-access\"}\n\n",
            "data: {\"type\":\"response.completed\",\"sequence_number\":2,\"response\":{\"output\":[],\"usage\":null}}\n\n",
        );
        let response = http_response(200, &[("Content-Type", "text/event-stream")], body);
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await?;
            read_request(&mut socket).await?;
            socket.write_all(response.as_bytes()).await
        });
        let mut events = https(&reqwest::Client::new(), &base, wire(true))
            .await
            .expect("Codex HTTPS stream opens");
        assert!(matches!(
            events.next().await,
            Some(Ok(StreamEvent::TextDelta { text })) if text == "test-access"
        ));
        let mut stops = 0;
        while let Some(event) = events.next().await {
            if matches!(event?, StreamEvent::Stop { .. }) {
                stops += 1;
            }
        }
        assert_eq!(stops, 1);
        server.await??;
        Ok(())
    }

    #[tokio::test]
    async fn codex_https_preserves_status_code_and_retry_after() -> Result<(), Box<dyn Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        let body = r#"{"error":{"type":"server_is_overloaded","message":"busy"}}"#;
        let response = http_response(503, &[("Retry-After", "7")], body);
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await?;
            read_request(&mut socket).await?;
            socket.write_all(response.as_bytes()).await
        });
        let error = match https(&reqwest::Client::new(), &base, wire(true)).await {
            Ok(_) => return Err("503 response unexpectedly opened a stream".into()),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            AttemptFailure::Response {
                status: 503,
                code: Some(code),
                message,
                retry_after: Some(retry_after),
            } if code == "server_is_overloaded" && message == "busy" && retry_after == "7"
        ));
        server.await??;
        Ok(())
    }

    #[tokio::test]
    async fn codex_https_preserves_luna_reserve_mapping() -> Result<(), Box<dyn Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        let wire = wire_for_model(crate::usage::LUNA_RESERVE_MODEL, true);
        let body = r#"{"detail":"reserve is not available"}"#;
        let response = http_response(403, &[], body);
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await?;
            read_request(&mut socket).await?;
            socket.write_all(response.as_bytes()).await
        });
        let error = match https(&reqwest::Client::new(), &base, wire).await {
            Ok(_) => return Err("403 response unexpectedly opened a stream".into()),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            AttemptFailure::Provider(ProviderError::ReserveUnavailable {
                status: 403,
                message,
            }) if message == "reserve is not available"
        ));
        server.await??;
        Ok(())
    }

    #[tokio::test]
    async fn codex_https_idle_timeout_is_stream_cut() -> Result<(), Box<dyn Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await?;
            read_request(&mut socket).await?;
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 128\r\nConnection: keep-alive\r\n\r\n",
                )
                .await?;
            tokio::time::sleep(Duration::from_millis(100)).await;
            Ok::<(), std::io::Error>(())
        });
        let mut events = https_with_idle_timeout(
            &reqwest::Client::new(),
            &base,
            wire(true),
            Duration::from_millis(20),
        )
        .await
        .expect("Codex HTTPS stream opens");
        assert!(matches!(
            timeout(Duration::from_secs(1), events.next()).await?,
            Some(Err(ProviderError::StreamCut))
        ));
        server.await??;
        Ok(())
    }

    #[test]
    fn codex_summary_auto_requires_the_resolved_capability() {
        let supported = wire(true);
        let supported_body = sonic_rs::from_slice::<sonic_rs::Value>(&supported.body)
            .expect("Codex body is JSON");
        let expected_cache_key = format!("{}:1", supported.session_id);
        assert_eq!(
            supported_body
                .get("prompt_cache_key")
                .and_then(JsonValueTrait::as_str),
            Some(expected_cache_key.as_str())
        );
        assert_eq!(
            supported_body
                .get("reasoning")
                .and_then(|reasoning| reasoning.get("summary"))
                .and_then(JsonValueTrait::as_str),
            Some("auto")
        );

        let unsupported = wire(false);
        let unsupported_body = sonic_rs::from_slice::<sonic_rs::Value>(&unsupported.body)
            .expect("Codex body is JSON");
        assert!(
            unsupported_body
                .get("reasoning")
                .and_then(|reasoning| reasoning.get("summary"))
                .is_none()
        );
    }

    #[test]
    fn codex_body_omits_sampling_and_verbosity_and_uses_session_identity_headers() {
        let wire = wire(true);
        let body = sonic_rs::from_slice::<sonic_rs::Value>(&wire.body).expect("Codex body is JSON");
        assert!(body.get("temperature").is_none());
        assert!(body.get("top_p").is_none());
        assert!(body.get("text").is_none());
        assert!(wire
            .headers
            .iter()
            .any(|(name, value)| *name == "authorization" && value.starts_with("Bearer ")));
        assert_eq!(
            wire.headers
                .iter()
                .find(|(name, _)| *name == "chatgpt-account-id")
                .map(|(_, value)| value.as_str()),
            Some("account")
        );
        assert_eq!(wire.user_agent, "dalgon/test (Linux test; x86_64)");
        assert_eq!(
            wire.headers
                .iter()
                .find(|(name, _)| *name == "originator")
                .map(|(_, value)| value.as_str()),
            Some(CODEX_ORIGINATOR)
        );
        let session = wire
            .headers
            .iter()
            .find(|(name, _)| *name == "session-id")
            .map(|(_, value)| value.as_str());
        assert!(session.is_some());
        let expected_session = wire.session_id.to_string();
        assert_eq!(session, Some(expected_session.as_str()));
        let expected_cache_key = format!("{expected_session}:1");
        assert_eq!(
            body.get("prompt_cache_key").and_then(JsonValueTrait::as_str),
            Some(expected_cache_key.as_str())
        );
        for header in ["thread-id", "x-client-request-id"] {
            assert_eq!(
                wire.headers
                    .iter()
                    .find(|(name, _)| *name == header)
                    .map(|(_, value)| value.as_str()),
                session
            );
        }
    }

    #[test]
    fn websocket_frame_prepends_create_and_removes_stream_without_reencoding() {
        let body = br#"{"model":"gpt-6-luna","input":[],"tools":[{"parameters":{"type":"object","properties":{"x":{"type":"string"}}}}],"store":false,"stream":true,"include":["reasoning.encrypted_content"],"prompt_cache_key":"session"}"#;
        let frame = websocket_frame(body).expect("typed Responses body has the WebSocket shape");
        assert_eq!(
            std::str::from_utf8(&frame).expect("JSON frame is UTF-8"),
            r#"{"type":"response.create","model":"gpt-6-luna","input":[],"tools":[{"parameters":{"type":"object","properties":{"x":{"type":"string"}}}}],"store":false,"include":["reasoning.encrypted_content"],"prompt_cache_key":"session"}"#
        );
    }

    #[test]
    fn websocket_frame_rejects_a_body_without_the_responses_stream_member() {
        assert!(websocket_frame(br#"{"model":"gpt-6-luna"}"#).is_err());
    }
}
