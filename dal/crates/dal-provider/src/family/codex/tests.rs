use std::{error::Error, sync::Arc, time::Duration};

use dal_core::{ContextItem, ModelToolSpec, Purpose, RequestParams, SessionId, ThinkingLevel};
use sonic_rs::JsonValueTrait;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::timeout,
};

use super::{
    CODEX_ORIGINATOR, CodexRequest, build, https, https_with_idle_timeout, websocket_frame,
};
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
            max_output_tokens: None,
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
    build(&CodexRequest {
        request: &request,
        thinking: WireThinking::OpenAi {
            effort: Some("high"),
        },
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
async fn codex_https_keeps_successful_text_that_matches_access_token() -> Result<(), Box<dyn Error>>
{
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let body = concat!(
        "data: {\"type\":\"response.output_text.delta\",\"sequence_number\":1,\"delta\":\"test-access\"}\n\n",
        "data: {\"type\":\"response.completed\",\"sequence_number\":2,\"response\":{\"output\":[],\"usage\":null}}\n\n",
    );
    let response = http_response(200, &[("Content-Type", "text/event-stream")], body);
    let mut server = tokio::task::JoinSet::new();
    server.spawn(async move {
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
    server.join_next().await.expect("server completes")??;
    Ok(())
}

#[tokio::test]
async fn codex_https_preserves_status_code_and_retry_after() -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let body = r#"{"error":{"type":"server_is_overloaded","message":"busy"}}"#;
    let response = http_response(503, &[("Retry-After", "7")], body);
    let mut server = tokio::task::JoinSet::new();
    server.spawn(async move {
        let (mut socket, _) = listener.accept().await?;
        read_request(&mut socket).await?;
        socket.write_all(response.as_bytes()).await
    });
    let Err(error) = https(&reqwest::Client::new(), &base, wire(true)).await else {
        return Err("503 response unexpectedly opened a stream".into());
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
    server.join_next().await.expect("server completes")??;
    Ok(())
}

#[tokio::test]
async fn codex_https_preserves_luna_reserve_mapping() -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let wire = wire_for_model(crate::usage::LUNA_RESERVE_MODEL, true);
    let body = r#"{"detail":"reserve is not available"}"#;
    let response = http_response(403, &[], body);
    let mut server = tokio::task::JoinSet::new();
    server.spawn(async move {
        let (mut socket, _) = listener.accept().await?;
        read_request(&mut socket).await?;
        socket.write_all(response.as_bytes()).await
    });
    let Err(error) = https(&reqwest::Client::new(), &base, wire).await else {
        return Err("403 response unexpectedly opened a stream".into());
    };
    assert!(matches!(
        error,
        AttemptFailure::Provider(ProviderError::ReserveUnavailable {
            status: 403,
            message,
        }) if message == "reserve is not available"
    ));
    server.join_next().await.expect("server completes")??;
    Ok(())
}

#[tokio::test]
async fn codex_https_idle_timeout_is_stream_cut() -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let mut server = tokio::task::JoinSet::new();
    server.spawn(async move {
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
    server.join_next().await.expect("server completes")??;
    Ok(())
}

#[tokio::test]
async fn codex_https_body_cut_mid_frame_is_transport_not_protocol() -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let mut server = tokio::task::JoinSet::new();
    server.spawn(async move {
        let (mut socket, _) = listener.accept().await?;
        read_request(&mut socket).await?;
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 500\r\nConnection: close\r\n\r\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"a",
            )
            .await?;
        Ok::<(), std::io::Error>(())
    });
    let mut events = https_with_idle_timeout(
        &reqwest::Client::new(),
        &base,
        wire(true),
        Duration::from_secs(5),
    )
    .await
    .expect("Codex HTTPS stream opens");
    assert!(matches!(
        timeout(Duration::from_secs(2), events.next()).await?,
        Some(Err(ProviderError::Transport { .. }))
    ));
    assert!(events.next().await.is_none());
    server.join_next().await.expect("server completes")??;
    Ok(())
}

#[test]
fn codex_summary_auto_requires_the_resolved_capability() {
    let supported = wire(true);
    let supported_body =
        sonic_rs::from_slice::<sonic_rs::Value>(&supported.body).expect("Codex body is JSON");
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
    let unsupported_body =
        sonic_rs::from_slice::<sonic_rs::Value>(&unsupported.body).expect("Codex body is JSON");
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
    assert!(
        wire.headers
            .iter()
            .any(|(name, value)| *name == "authorization" && value.starts_with("Bearer "))
    );
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
        body.get("prompt_cache_key")
            .and_then(JsonValueTrait::as_str),
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
