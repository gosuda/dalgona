use std::{
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

use dal_core::{
    ContextItem, Family, ModelRequest, ModelRoute, Part, Purpose, RequestParams, SessionId,
    ThinkingLevel,
};

use futures::SinkExt;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::{Duration, timeout},
};
use tokio_tungstenite::{
    accept_async,
    tungstenite::{
        Message,
        handshake::server::{Request, Response},
        protocol::{CloseFrame, frame::coding::CloseCode},
    },
};

use super::*;
use crate::{
    auth::credential::{Credential, SecretString},
    family::{codex::CodexWire, responses::ResponsesRequest},
    provider::AuthStyle,
    stream::{NoticeSink, StreamEvent},
    thinking::WireThinking,
};

const COMPLETE: &str =
    r#"{"type":"response.completed","sequence_number":2,"response":{"output":[],"usage":null}}"#;
const TEXT_DELTA: &str = r#"{"type":"response.output_text.delta","sequence_number":1,"delta":"x"}"#;

#[test]
fn retry_jitter_maps_uniform_samples_to_the_full_allowed_range() {
    assert!((jitter_for_sample(0) - 0.9).abs() < 1.0e-9);
    assert!((jitter_for_sample(u32::MAX / 2) - 1.0).abs() < 1.0e-9);
    assert!((jitter_for_sample(u32::MAX) - 1.1).abs() < f64::EPSILON);
}

fn wire(model: &str) -> CodexWire {
    wire_for_session(model, SessionId::new_v7())
}

fn wire_for_session(model: &str, session_id: SessionId) -> CodexWire {
    let session = session_id.to_string();
    CodexWire {
            headers: vec![
                ("authorization", String::from("Bearer test-token")),
                ("chatgpt-account-id", String::from("acct-7f3a9c")),
                ("originator", String::from(crate::auth::oauth::CODEX_ORIGINATOR)),
                ("session-id", session.clone()),
                ("thread-id", session.clone()),
                ("x-client-request-id", session.clone()),
            ],
            body: format!(
                r#"{{"model":"{model}","input":[],"store":false,"stream":true,"include":["reasoning.encrypted_content"],"prompt_cache_key":"{session}"}}"#
            )
            .into_bytes(),
            model: model.into(),
            session_id,
            user_agent: String::from("dalgon/test (Linux test; x86_64)"),
        }
}

fn responses_wire(
    model: &str,
    session_id: SessionId,
    auth: AuthStyle,
) -> crate::family::responses::ResponsesWire {
    let request = ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::Responses,
            model: model.into(),
        },
        system: Arc::from("Test instructions"),
        tools: Vec::new().into(),
        context: Arc::from([ContextItem::User {
            parts: vec![Part::Text {
                text: "full user history".into(),
            }],
        }]),
        params: RequestParams {
            thinking: ThinkingLevel::High,
            effort: None,
            temperature: None,
            max_output_tokens: None,
        },
        cache_key: Some(format!("{session_id}:1").into_boxed_str()),
    };
    let credential = Credential::ApiKey {
        key: SecretString::from("responses-test-key"),
    };
    crate::family::responses::wire(&ResponsesRequest {
        request: &request,
        thinking: WireThinking::OpenAi {
            effort: Some("high"),
        },
        reasoning_summary: false,
        auth,
        credential: &credential,
        session_id,
        user_agent: "dalgon/test (Linux test; x86_64)",
    })
    .expect("Responses request has a valid route and credential")
}

fn ws_request<'a>(
    provider: &'a str,
    session_id: SessionId,
    base_url: &'a str,
    wire: WsWire<'a>,
    notices: &'a NoticeSink,
    cancel: &'a CancellationToken,
) -> WsRequest<'a> {
    WsRequest {
        provider,
        session_id,
        base_url,
        wire,
        // The shared retry budget for every test turn: five retries after
        // the first attempt, without a prior OAuth refresh.
        stream_max_retries: 5,
        refreshed: false,
        notices,
        cancel,
    }
}

fn fast_sessions(offset: Arc<AtomicU64>) -> WsSessions {
    let origin = std::time::Instant::now();
    let clock: Clock =
        Arc::new(move || origin + Duration::from_secs(offset.load(Ordering::SeqCst)));
    let sleeper: Sleeper = Arc::new(|_| Box::pin(async {}));
    WsSessions::with_clock_and_sleeper(clock, sleeper)
}

fn notices() -> (NoticeSink, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let target = Arc::clone(&seen);
    (Arc::new(move |notice| lock(&target).push(notice)), seen)
}

async fn drain(mut stream: EventStream) -> Result<Vec<StreamEvent>, ProviderError> {
    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        events.push(item?);
    }
    Ok(events)
}

async fn server_replies(listener: TcpListener, handshakes: Arc<AtomicUsize>, requests: usize) {
    for _ in 0..requests {
        let (tcp, _) = listener.accept().await.expect("client connects");
        let mut socket = accept_async(tcp).await.expect("client handshake is valid");
        handshakes.fetch_add(1, Ordering::SeqCst);
        while let Some(Ok(Message::Text(_))) = socket.next().await {
            socket
                .send(Message::Text(COMPLETE.into()))
                .await
                .expect("response frame reaches client");
        }
    }
}

async fn open_stream(
    sessions: &WsSessions,
    base: &str,
    wire: &CodexWire,
    notices: &NoticeSink,
) -> EventStream {
    let cancel = CancellationToken::new();
    match sessions
        .open(ws_request(
            "openai-codex",
            wire.session_id,
            base,
            WsWire::Codex(wire),
            notices,
            &cancel,
        ))
        .await
        .expect("WebSocket turn opens")
    {
        WsTurn::Stream(stream) => stream,
        WsTurn::HttpsFallback => panic!("loopback WebSocket must not fall back"),
    }
}

#[tokio::test]
async fn websocket_reuses_session_socket_within_idle_window() -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}/backend-api/codex", listener.local_addr()?);
    let handshakes = Arc::new(AtomicUsize::new(0));
    let server_count = Arc::clone(&handshakes);
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(server_replies(listener, server_count, 1));
    let offset = Arc::new(AtomicU64::new(0));
    let sessions = fast_sessions(Arc::clone(&offset));
    let wire = wire("gpt-6-luna");
    let (notice, _) = notices();

    assert!(
        drain(open_stream(&sessions, &base, &wire, &notice).await)
            .await
            .is_ok()
    );
    offset.store(60, Ordering::SeqCst);
    assert!(
        drain(open_stream(&sessions, &base, &wire, &notice).await)
            .await
            .is_ok()
    );
    assert_eq!(handshakes.load(Ordering::SeqCst), 1);
    servers.abort_all();
    Ok(())
}

#[tokio::test]
async fn websocket_idle_expiry_reconnects_after_six_minutes() -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}/backend-api/codex", listener.local_addr()?);
    let handshakes = Arc::new(AtomicUsize::new(0));
    let server_count = Arc::clone(&handshakes);
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(server_replies(listener, server_count, 2));
    let offset = Arc::new(AtomicU64::new(0));
    let sessions = fast_sessions(Arc::clone(&offset));
    let wire = wire("gpt-6-luna");
    let (notice, _) = notices();

    assert!(
        drain(open_stream(&sessions, &base, &wire, &notice).await)
            .await
            .is_ok()
    );
    offset.store(6 * 60, Ordering::SeqCst);
    assert!(
        drain(open_stream(&sessions, &base, &wire, &notice).await)
            .await
            .is_ok()
    );

    assert_eq!(handshakes.load(Ordering::SeqCst), 2);
    servers.abort_all();
    Ok(())
}

#[tokio::test]
async fn six_failed_handshakes_enable_https_until_the_model_changes() -> Result<(), Box<dyn Error>>
{
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}/backend-api/codex", listener.local_addr()?);
    let accepted = Arc::new(AtomicUsize::new(0));
    let server_accepted = Arc::clone(&accepted);
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(async move {
        for _ in 0..12 {
            if let Ok((tcp, _)) = listener.accept().await {
                server_accepted.fetch_add(1, Ordering::SeqCst);
                drop(tcp);
            }
        }
    });
    let sessions = fast_sessions(Arc::new(AtomicU64::new(0)));
    let session = SessionId::new_v7();
    let first_wire = wire_for_session("gpt-6-luna", session);
    let second_wire = wire_for_session("gpt-5.6-luna", session);
    let (notice, seen) = notices();
    let cancel = CancellationToken::new();

    let first = sessions
        .open(ws_request(
            "openai-codex",
            session,
            &base,
            WsWire::Codex(&first_wire),
            &notice,
            &cancel,
        ))
        .await?;
    assert_eq!(accepted.load(Ordering::SeqCst), 6);
    assert!(matches!(first, WsTurn::HttpsFallback));
    assert_eq!(
        lock(&seen)
            .iter()
            .filter(|notice| notice.starts_with(FALLBACK_NOTICE))
            .count(),
        1
    );
    assert!(
        lock(&seen)
            .iter()
            .all(|notice| !notice.contains("test-token"))
    );

    let second = sessions
        .open(ws_request(
            "openai-codex",
            session,
            &base,
            WsWire::Codex(&first_wire),
            &notice,
            &cancel,
        ))
        .await?;
    assert!(matches!(second, WsTurn::HttpsFallback));
    assert_eq!(accepted.load(Ordering::SeqCst), 6);

    let changed = sessions
        .open(ws_request(
            "openai-codex",
            session,
            &base,
            WsWire::Codex(&second_wire),
            &notice,
            &cancel,
        ))
        .await?;
    assert!(matches!(changed, WsTurn::HttpsFallback));
    assert_eq!(accepted.load(Ordering::SeqCst), 12);
    assert_eq!(
        lock(&seen)
            .iter()
            .filter(|notice| notice.starts_with(FALLBACK_NOTICE))
            .count(),
        2
    );
    servers.abort_all();
    Ok(())
}

#[tokio::test]
async fn websocket_connection_limit_before_first_event_reconnects() -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}/backend-api/codex", listener.local_addr()?);
    let handshakes = Arc::new(AtomicUsize::new(0));
    let server_count = Arc::clone(&handshakes);
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(async move {
            for attempt in 0..2 {
                let (tcp, _) = listener.accept().await.expect("client connects");
                let mut socket = accept_async(tcp).await.expect("handshake succeeds");
                server_count.fetch_add(1, Ordering::SeqCst);
                let _ = socket.next().await;
                let response = if attempt == 0 {
                    r#"{"type":"error","error":{"type":"server_error","code":"websocket_connection_limit_reached","message":"limit"}}"#
                } else {
                    COMPLETE
                };
                socket
                    .send(Message::Text(response.into()))
                    .await
                    .expect("event reaches client");
            }
        });
    let sessions = fast_sessions(Arc::new(AtomicU64::new(0)));
    let (notice, _) = notices();
    let wire = wire("gpt-6-luna");
    let stream = open_stream(&sessions, &base, &wire, &notice).await;

    assert!(drain(stream).await.is_ok());
    assert_eq!(handshakes.load(Ordering::SeqCst), 2);
    servers.abort_all();
    Ok(())
}

#[tokio::test]
async fn websocket_close_frame_preserves_code_and_reason() -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}/backend-api/codex", listener.local_addr()?);
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(async move {
        let (tcp, _) = listener.accept().await.expect("client connects");
        let mut socket = accept_async(tcp).await.expect("handshake succeeds");
        let _ = socket.next().await;
        socket
            .send(Message::Text(TEXT_DELTA.into()))
            .await
            .expect("first event reaches client");
        socket
            .send(Message::Close(Some(CloseFrame {
                code: CloseCode::Error,
                reason: "busy".into(),
            })))
            .await
            .expect("close reaches client");
    });
    let sessions = fast_sessions(Arc::new(AtomicU64::new(0)));
    let (notice, _) = notices();
    let wire = wire("gpt-6-luna");
    let mut stream = open_stream(&sessions, &base, &wire, &notice).await;
    assert!(matches!(
        stream.next().await,
        Some(Ok(StreamEvent::TextDelta { text })) if text == "x"
    ));
    let error = stream
        .next()
        .await
        .expect("close is an error")
        .expect_err("close before terminal fails");
    assert_eq!(
        error.to_string(),
        "websocket closed by server before response.completed. (code 1011: busy)"
    );
    servers.abort_all();
    Ok(())
}

#[tokio::test]
async fn codex_websocket_errors_redact_the_bearer_token_and_account_id()
-> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}/backend-api/codex", listener.local_addr()?);
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(async move {
        let (tcp, _) = listener.accept().await.expect("client connects");
        let mut socket = accept_async(tcp).await.expect("handshake succeeds");
        let _ = socket.next().await;
        socket
            .send(Message::Text(TEXT_DELTA.into()))
            .await
            .expect("first event reaches client");
        socket
            .send(Message::Text(
                r#"{"type":"error","status":400,"error":{"type":"invalid_request_error","message":"echo test-token / acct-7f3a9c"}}"#
                    .into(),
            ))
            .await
            .expect("error event reaches client");
    });
    let sessions = fast_sessions(Arc::new(AtomicU64::new(0)));
    let (notice, _) = notices();
    let wire = wire("gpt-6-luna");
    let mut stream = open_stream(&sessions, &base, &wire, &notice).await;
    assert!(matches!(
        stream.next().await,
        Some(Ok(StreamEvent::TextDelta { text })) if text == "x"
    ));
    let error = stream
        .next()
        .await
        .expect("error frame is delivered")
        .expect_err("error frame ends the stream");
    assert!(matches!(
        &error,
        ProviderError::Status { status: 400, message, .. }
            if message == "echo <redacted> / <redacted>"
    ));
    let shown = format!("{error:?} {error}");
    assert!(
        !shown.contains("test-token") && !shown.contains("acct-7f3a9c"),
        "{shown}"
    );
    servers.abort_all();
    Ok(())
}

#[tokio::test]
async fn dropping_after_first_delta_releases_the_socket_within_250ms() -> Result<(), Box<dyn Error>>
{
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}/backend-api/codex", listener.local_addr()?);
    let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(async move {
        let (tcp, _) = listener.accept().await.expect("client connects");
        let mut socket = accept_async(tcp).await.expect("handshake succeeds");
        let _ = socket.next().await;
        socket
            .send(Message::Text(TEXT_DELTA.into()))
            .await
            .expect("delta reaches client");
        let closed = timeout(Duration::from_millis(250), socket.next()).await;
        let _ = closed_tx.send(matches!(closed, Ok(None | Some(Err(_)))));
    });
    let sessions = fast_sessions(Arc::new(AtomicU64::new(0)));
    let (notice, _) = notices();
    let wire = wire("gpt-6-luna");
    let mut stream = open_stream(&sessions, &base, &wire, &notice).await;
    assert!(matches!(
        stream.next().await,
        Some(Ok(StreamEvent::TextDelta { text })) if text == "x"
    ));
    drop(stream);
    assert!(closed_rx.await?);
    servers.abort_all();
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::result_large_err,
    reason = "handshake callback Err type is fixed by tungstenite's Callback trait; the test only returns Ok"
)]
async fn websocket_frame_uses_beta_header_and_success_stream_terminates()
-> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}/backend-api/codex", listener.local_addr()?);
    let wire = wire("gpt-6-luna");
    let expected_session = wire.session_id.to_string();
    let expected_frame = format!(
        r#"{{"type":"response.create","model":"gpt-6-luna","input":[],"store":false,"include":["reasoning.encrypted_content"],"prompt_cache_key":"{expected_session}"}}"#
    );
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(async move {
        let (tcp, _) = listener.accept().await.expect("client connects");
        let mut socket =
            tokio_tungstenite::accept_hdr_async(tcp, |request: &Request, response: Response| {
                let headers = request.headers();
                assert_eq!(
                    headers
                        .get("openai-beta")
                        .and_then(|value| value.to_str().ok()),
                    Some("responses_websockets=2026-02-06")
                );
                assert_eq!(
                    headers
                        .get("originator")
                        .and_then(|value| value.to_str().ok()),
                    Some(crate::auth::oauth::CODEX_ORIGINATOR)
                );
                assert_eq!(
                    headers
                        .get("user-agent")
                        .and_then(|value| value.to_str().ok()),
                    Some("dalgon/test (Linux test; x86_64)")
                );
                assert!(headers.get("authorization").is_some());
                assert!(headers.get("chatgpt-account-id").is_some());
                assert_eq!(
                    headers
                        .get("session-id")
                        .and_then(|value| value.to_str().ok()),
                    Some(expected_session.as_str())
                );
                assert_eq!(headers.get("session-id"), headers.get("thread-id"));
                assert_eq!(
                    headers.get("session-id"),
                    headers.get("x-client-request-id")
                );
                assert!(headers.get("accept").is_none());
                Ok(response)
            })
            .await
            .expect("Codex headers pass the handshake");
        let expected = expected_frame;
        assert!(matches!(
            socket.next().await,
            Some(Ok(Message::Text(frame))) if frame.as_str() == expected
        ));
        socket
            .send(Message::Text(COMPLETE.into()))
            .await
            .expect("terminal event reaches client");
    });
    let sessions = fast_sessions(Arc::new(AtomicU64::new(0)));
    let (notice, _) = notices();
    let events = drain(open_stream(&sessions, &base, &wire, &notice).await).await?;
    assert!(matches!(events.last(), Some(StreamEvent::Stop { .. })));
    servers.abort_all();
    Ok(())
}

fn assert_responses_frame(body: &[u8], session: SessionId) -> Result<String, Box<dyn Error>> {
    let frame = crate::family::codex::websocket_frame(body)?;
    let frame = String::from_utf8(frame)?;
    let frame_json = sonic_rs::from_str::<sonic_rs::Value>(&frame)?;
    assert_eq!(
        frame_json.get("type").and_then(JsonValueTrait::as_str),
        Some("response.create")
    );
    assert!(frame_json.get("stream").is_none());
    assert!(frame_json.get("previous_response_id").is_none());
    let expected_cache_key = format!("{session}:1");
    assert_eq!(
        frame_json
            .get("prompt_cache_key")
            .and_then(JsonValueTrait::as_str),
        Some(expected_cache_key.as_str())
    );
    assert!(frame.contains("full user history"));
    assert!(!frame.contains("responses-test-key"));
    Ok(frame)
}

fn assert_responses_handshake(request: &Request, auth: AuthStyle) {
    let headers = request.headers();
    assert_eq!(
        headers
            .get("openai-beta")
            .and_then(|value| value.to_str().ok()),
        Some(BETA_HEADER)
    );
    assert_eq!(
        headers
            .get("user-agent")
            .and_then(|value| value.to_str().ok()),
        Some("dalgon/test (Linux test; x86_64)")
    );
    match auth {
        AuthStyle::Bearer => {
            assert_eq!(
                headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok()),
                Some("Bearer responses-test-key")
            );
            assert!(headers.get("x-api-key").is_none());
        }
        AuthStyle::XApiKey => {
            assert_eq!(
                headers
                    .get("x-api-key")
                    .and_then(|value| value.to_str().ok()),
                Some("responses-test-key")
            );
            assert!(headers.get("authorization").is_none());
        }
    }
    for name in [
        "chatgpt-account-id",
        "originator",
        "session-id",
        "thread-id",
        "x-client-request-id",
    ] {
        assert!(headers.get(name).is_none(), "unexpected header {name}");
    }
    assert!(headers.get("accept").is_none());
}

#[expect(
    clippy::result_large_err,
    reason = "handshake callback Err type is fixed by tungstenite's Callback trait; the test only returns Ok"
)]
async fn serve_responses_reuse(
    listener: TcpListener,
    server_frame: String,
    auth: AuthStyle,
    handshakes: Arc<AtomicUsize>,
) {
    let (tcp, _) = listener.accept().await.expect("client connects");
    let mut socket =
        tokio_tungstenite::accept_hdr_async(tcp, |request: &Request, response: Response| {
            assert_responses_handshake(request, auth);
            Ok(response)
        })
        .await
        .expect("Responses headers pass the handshake");
    handshakes.fetch_add(1, Ordering::SeqCst);
    for _ in 0..2 {
        assert!(matches!(
            socket.next().await,
            Some(Ok(Message::Text(actual))) if actual.as_str() == server_frame
        ));
        socket
            .send(Message::Text(COMPLETE.into()))
            .await
            .expect("terminal event reaches client");
    }
}

#[tokio::test]
async fn responses_api_key_handshake_frame_and_reuse() -> Result<(), Box<dyn Error>> {
    for auth in [AuthStyle::Bearer, AuthStyle::XApiKey] {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}/v1", listener.local_addr()?);
        let session = SessionId::new_v7();
        let wire = responses_wire("gpt-5", session, auth);
        let frame = assert_responses_frame(&wire.body, session)?;
        let handshakes = Arc::new(AtomicUsize::new(0));
        let mut server = tokio::task::JoinSet::new();
        server.spawn(serve_responses_reuse(
            listener,
            frame,
            auth,
            Arc::clone(&handshakes),
        ));
        let sessions = fast_sessions(Arc::new(AtomicU64::new(0)));
        let (notice, _) = notices();
        let cancel = CancellationToken::new();
        for _ in 0..2 {
            let stream = match sessions
                .open(ws_request(
                    "openai-responses",
                    session,
                    &base,
                    WsWire::Responses(&wire),
                    &notice,
                    &cancel,
                ))
                .await?
            {
                WsTurn::Stream(stream) => stream,
                WsTurn::HttpsFallback => {
                    return Err("Responses loopback unexpectedly fell back".into());
                }
            };
            let events = drain(stream).await?;
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, StreamEvent::Stop { .. }))
                    .count(),
                1
            );
        }
        assert_eq!(handshakes.load(Ordering::SeqCst), 1);
        server.join_next().await.expect("server completes")?;
    }
    Ok(())
}

#[tokio::test]
async fn responses_first_frame_401_falls_back_without_latching() -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}/v1", listener.local_addr()?);
    let session = SessionId::new_v7();
    let wire = responses_wire("gpt-5", session, AuthStyle::Bearer);
    let handshakes = Arc::new(AtomicUsize::new(0));
    let server_count = Arc::clone(&handshakes);
    let mut server = tokio::task::JoinSet::new();
    server.spawn(async move {
            let (tcp, _) = listener.accept().await.expect("first client connects");
            let mut first = accept_async(tcp).await.expect("first handshake succeeds");
            server_count.fetch_add(1, Ordering::SeqCst);
            let _ = first.next().await;
            first
                .send(Message::Text(
                    r#"{"type":"error","status":401,"error":{"code":"invalid_api_key","message":"denied"}}"#
                        .into(),
                ))
                .await
                .expect("401 error frame reaches client");
            assert!(matches!(first.next().await, Some(Ok(Message::Close(_)))));

            let (tcp, _) = listener.accept().await.expect("second client connects");
            let mut second = accept_async(tcp).await.expect("second handshake succeeds");
            server_count.fetch_add(1, Ordering::SeqCst);
            assert!(matches!(second.next().await, Some(Ok(Message::Text(_)))));
            second
                .send(Message::Text(COMPLETE.into()))
                .await
                .expect("second response reaches client");
        });
    let sessions = fast_sessions(Arc::new(AtomicU64::new(0)));
    let (notice, seen) = notices();
    let cancel = CancellationToken::new();
    let first = sessions
        .open(ws_request(
            "openai-responses",
            session,
            &base,
            WsWire::Responses(&wire),
            &notice,
            &cancel,
        ))
        .await?;
    assert!(matches!(first, WsTurn::HttpsFallback));
    assert!(
        lock(&seen)
            .iter()
            .all(|notice| !notice.starts_with(FALLBACK_NOTICE))
    );

    let second = sessions
        .open(ws_request(
            "openai-responses",
            session,
            &base,
            WsWire::Responses(&wire),
            &notice,
            &cancel,
        ))
        .await?;
    let WsTurn::Stream(stream) = second else {
        return Err("401 unexpectedly latched WebSocket fallback".into());
    };
    assert!(drain(stream).await.is_ok());
    assert_eq!(handshakes.load(Ordering::SeqCst), 2);
    server.join_next().await.expect("server completes")?;
    Ok(())
}

#[tokio::test]
async fn responses_handshake_401_falls_back_to_lifecycle_immediately() -> Result<(), Box<dyn Error>>
{
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}/v1", listener.local_addr()?);
    let session = SessionId::new_v7();
    let wire = responses_wire("gpt-5", session, AuthStyle::Bearer);
    let attempts = Arc::new(AtomicUsize::new(0));
    let server_attempts = Arc::clone(&attempts);
    let mut server = tokio::task::JoinSet::new();
    server.spawn(async move {
        let (mut tcp, _) = listener.accept().await.expect("first client connects");
        server_attempts.fetch_add(1, Ordering::SeqCst);
        let mut request = Vec::new();
        let mut buffer = [0_u8; 256];
        loop {
            let count = tcp.read(&mut buffer).await.expect("request bytes arrive");
            request.extend_from_slice(&buffer[..count]);
            if count == 0 || request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        tcp.write_all(
            b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )
        .await
        .expect("401 response reaches client");

        let (tcp, _) = listener.accept().await.expect("second client connects");
        server_attempts.fetch_add(1, Ordering::SeqCst);
        let mut socket = accept_async(tcp).await.expect("second handshake succeeds");
        assert!(matches!(socket.next().await, Some(Ok(Message::Text(_)))));
        socket
            .send(Message::Text(COMPLETE.into()))
            .await
            .expect("second response reaches client");
    });
    let sessions = fast_sessions(Arc::new(AtomicU64::new(0)));
    let (notice, seen) = notices();
    let cancel = CancellationToken::new();
    let first = sessions
        .open(ws_request(
            "openai-responses",
            session,
            &base,
            WsWire::Responses(&wire),
            &notice,
            &cancel,
        ))
        .await?;
    assert!(matches!(first, WsTurn::HttpsFallback));
    assert!(
        lock(&seen)
            .iter()
            .all(|notice| !notice.starts_with(FALLBACK_NOTICE))
    );

    let second = sessions
        .open(ws_request(
            "openai-responses",
            session,
            &base,
            WsWire::Responses(&wire),
            &notice,
            &cancel,
        ))
        .await?;
    let WsTurn::Stream(stream) = second else {
        return Err("401 unexpectedly latched WebSocket fallback".into());
    };
    assert!(drain(stream).await.is_ok());
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    server.join_next().await.expect("server completes")?;
    Ok(())
}

#[tokio::test]
async fn responses_pre_event_overload_retries_through_retry_policy() -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}/v1", listener.local_addr()?);
    let session = SessionId::new_v7();
    let wire = responses_wire("gpt-5", session, AuthStyle::Bearer);
    let handshakes = Arc::new(AtomicUsize::new(0));
    let server_count = Arc::clone(&handshakes);
    let mut server = tokio::task::JoinSet::new();
    server.spawn(async move {
            for attempt in 0..2 {
                let (tcp, _) = listener.accept().await.expect("client connects");
                let mut socket = accept_async(tcp).await.expect("handshake succeeds");
                server_count.fetch_add(1, Ordering::SeqCst);
                let _ = socket.next().await;
                let frame = if attempt == 0 {
                    r#"{"type":"error","status":503,"error":{"type":"server_is_overloaded","message":"busy"}}"#
                } else {
                    COMPLETE
                };
                socket
                    .send(Message::Text(frame.into()))
                    .await
                    .expect("server frame reaches client");
            }
        });
    let sessions = fast_sessions(Arc::new(AtomicU64::new(0)));
    let (notice, seen) = notices();
    let cancel = CancellationToken::new();
    let turn = sessions
        .open(ws_request(
            "openai-responses",
            session,
            &base,
            WsWire::Responses(&wire),
            &notice,
            &cancel,
        ))
        .await?;
    let WsTurn::Stream(stream) = turn else {
        return Err("overload retries unexpectedly fell back".into());
    };
    assert!(drain(stream).await.is_ok());
    assert_eq!(handshakes.load(Ordering::SeqCst), 2);
    assert!(
        lock(&seen)
            .iter()
            .any(|notice| notice.starts_with("Retrying in "))
    );
    server.join_next().await.expect("server completes")?;
    Ok(())
}

#[tokio::test]
async fn responses_errors_redact_api_key_without_scrubbing_success_events()
-> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}/v1", listener.local_addr()?);
    let mut server = tokio::task::JoinSet::new();
    server.spawn(async move {
            let (tcp, _) = listener.accept().await.expect("client connects");
            let mut socket = accept_async(tcp).await.expect("handshake succeeds");
            let _ = socket.next().await;
            socket
                .send(Message::Text(TEXT_DELTA.into()))
                .await
                .expect("success event reaches client");
            socket
                .send(Message::Text(
                    r#"{"type":"error","status":400,"error":{"type":"invalid_request_error","message":"rejected responses-test-key"}}"#
                        .into(),
                ))
                .await
                .expect("error event reaches client");
        });
    let session = SessionId::new_v7();
    let wire = responses_wire("gpt-5", session, AuthStyle::XApiKey);
    let sessions = fast_sessions(Arc::new(AtomicU64::new(0)));
    let (notice, _) = notices();
    let cancel = CancellationToken::new();
    let turn = sessions
        .open(ws_request(
            "named-openai",
            session,
            &base,
            WsWire::Responses(&wire),
            &notice,
            &cancel,
        ))
        .await?;
    let WsTurn::Stream(mut stream) = turn else {
        return Err("Responses loopback unexpectedly fell back".into());
    };
    assert!(matches!(
        stream.next().await,
        Some(Ok(StreamEvent::TextDelta { text })) if text == "x"
    ));
    let error = stream
        .next()
        .await
        .expect("error frame is delivered")
        .expect_err("error frame ends the stream");
    let message = error.to_string();
    assert!(message.contains("<redacted>"));
    assert!(!message.contains("responses-test-key"));
    server.join_next().await.expect("server completes")?;
    Ok(())
}
#[test]
fn websocket_errors_redact_every_request_secret() {
    let secrets = [Box::from("tok-secret"), Box::from("acct-secret")];
    let error = websocket_error(
        r#"{"type":"error","status":400,"error":{"type":"bad","message":"tok-secret / acct-secret"}}"#,
        Family::Codex,
        "gpt-test",
        &secrets,
    )
    .expect("error frame maps to a typed error");
    let shown = error.to_string();
    assert!(shown.contains("<redacted> / <redacted>"), "{shown}");
    assert!(
        !shown.contains("tok-secret") && !shown.contains("acct-secret"),
        "{shown}"
    );
}

#[tokio::test]
async fn responses_six_failed_handshakes_fall_back_once() -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}/v1", listener.local_addr()?);
    let accepted = Arc::new(AtomicUsize::new(0));
    let server_accepted = Arc::clone(&accepted);
    let mut server = tokio::task::JoinSet::new();
    server.spawn(async move {
        for _ in 0..6 {
            let (tcp, _) = listener.accept().await.expect("client connects");
            server_accepted.fetch_add(1, Ordering::SeqCst);
            drop(tcp);
        }
    });
    let session = SessionId::new_v7();
    let wire = responses_wire("gpt-5", session, AuthStyle::XApiKey);
    let sessions = fast_sessions(Arc::new(AtomicU64::new(0)));
    let (notice, seen) = notices();
    let cancel = CancellationToken::new();
    let turn = sessions
        .open(ws_request(
            "named-openai",
            session,
            &base,
            WsWire::Responses(&wire),
            &notice,
            &cancel,
        ))
        .await?;
    assert!(matches!(turn, WsTurn::HttpsFallback));
    assert_eq!(accepted.load(Ordering::SeqCst), 6);
    assert_eq!(
        lock(&seen)
            .iter()
            .filter(|notice| notice.starts_with(FALLBACK_NOTICE))
            .count(),
        1
    );
    assert!(
        lock(&seen)
            .iter()
            .all(|notice| !notice.contains("responses-test-key"))
    );

    let repeated = sessions
        .open(ws_request(
            "named-openai",
            session,
            &base,
            WsWire::Responses(&wire),
            &notice,
            &cancel,
        ))
        .await?;
    assert!(matches!(repeated, WsTurn::HttpsFallback));
    assert_eq!(accepted.load(Ordering::SeqCst), 6);
    assert_eq!(
        lock(&seen)
            .iter()
            .filter(|notice| notice.starts_with(FALLBACK_NOTICE))
            .count(),
        1
    );
    server.join_next().await.expect("server completes")?;
    Ok(())
}

#[tokio::test]
async fn responses_cancellation_sends_a_close_frame() -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}/v1", listener.local_addr()?);
    let (close_tx, close_rx) = tokio::sync::oneshot::channel();
    let mut server = tokio::task::JoinSet::new();
    server.spawn(async move {
        let (tcp, _) = listener.accept().await.expect("client connects");
        let mut socket = accept_async(tcp).await.expect("handshake succeeds");
        let _ = socket.next().await;
        socket
            .send(Message::Text(TEXT_DELTA.into()))
            .await
            .expect("delta reaches client");
        let closed = matches!(socket.next().await, Some(Ok(Message::Close(_))));
        let _ = close_tx.send(closed);
    });
    let session = SessionId::new_v7();
    let wire = responses_wire("gpt-5", session, AuthStyle::Bearer);
    let sessions = fast_sessions(Arc::new(AtomicU64::new(0)));
    let (notice, _) = notices();
    let cancel = CancellationToken::new();
    let turn = sessions
        .open(ws_request(
            "openai-responses",
            session,
            &base,
            WsWire::Responses(&wire),
            &notice,
            &cancel,
        ))
        .await?;
    let WsTurn::Stream(mut stream) = turn else {
        return Err("Responses loopback unexpectedly fell back".into());
    };
    assert!(matches!(
        stream.next().await,
        Some(Ok(StreamEvent::TextDelta { text })) if text == "x"
    ));
    cancel.cancel();
    assert!(matches!(
        stream.next().await,
        Some(Err(ProviderError::Transport {
            family: Family::Responses,
            ..
        }))
    ));
    assert!(close_rx.await?);
    server.join_next().await.expect("server completes")?;
    Ok(())
}

#[tokio::test]
async fn websocket_rejects_a_message_above_the_shared_limit() -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}/backend-api/codex", listener.local_addr()?);
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(async move {
        let (tcp, _) = listener.accept().await.expect("client connects");
        let mut socket = accept_async(tcp).await.expect("handshake succeeds");
        let _ = socket.next().await;
        let payload =
            String::from_utf8(vec![b'x'; WS_MESSAGE_LIMIT + 1]).expect("ASCII frame is UTF-8");
        let _ = socket.send(Message::Text(payload.into())).await;
    });
    let sessions = fast_sessions(Arc::new(AtomicU64::new(0)));
    let (notice, _) = notices();
    let wire = wire("gpt-6-luna");
    let cancel = CancellationToken::new();
    let result = sessions
        .open(ws_request(
            "openai-codex",
            wire.session_id,
            &base,
            WsWire::Codex(&wire),
            &notice,
            &cancel,
        ))
        .await;

    assert!(matches!(
        result,
        Err(ProviderError::Limit(LimitError::WsMessage))
    ));
    servers.abort_all();
    Ok(())
}
