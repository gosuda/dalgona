// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use std::{
    future::Future,
    path::PathBuf,
    sync::{Arc, atomic::AtomicU64},
};

use reqwest::{
    Url,
    header::{HeaderName, HeaderValue},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};
use tokio_util::sync::CancellationToken;

use super::{
    CallDeadline, HttpTransport, auth as token_auth,
    protocol::{PROTOCOL_VERSION, outbound_headers, request_body},
};
use crate::{
    mcp::{Budgets, tools::Key},
    work::support::FakeServices,
};

async fn request_parts(stream: TcpStream) -> (String, String, BufReader<TcpStream>) {
    let mut reader = BufReader::new(stream);
    let mut headers = String::new();
    loop {
        let mut line = String::new();
        let count = reader.read_line(&mut line).await.expect("request headers");
        if count == 0 {
            break;
        }
        headers.push_str(&line);
        if line == "\r\n" {
            break;
        }
    }
    let length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or_default();
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await.expect("request body");
    (
        headers,
        String::from_utf8(body).expect("UTF-8 request body"),
        reader,
    )
}

async fn fixture_response(response: String) -> (Url, impl Future<Output = (String, String)>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener");
    let address = listener.local_addr().expect("listener address");
    let url = Url::parse(&format!("http://{address}/mcp")).expect("fixture URL");
    let server = async move {
        let (stream, _) = listener.accept().await.expect("fixture request");
        let (headers, body, mut reader) = request_parts(stream).await;
        reader
            .get_mut()
            .write_all(response.as_bytes())
            .await
            .expect("fixture response");
        (headers, body)
    };
    (url, server)
}

fn test_transport(url: Url) -> HttpTransport {
    HttpTransport::new(
        Key {
            session: dal_core::SessionId::new_v7(),
            skill: "test-skill".to_owned(),
            server: "test-server".to_owned(),
        },
        url,
        PathBuf::from("/tmp/dalgona-http-fixture-tokens.json"),
        "0.1.0".to_owned(),
        &Budgets::default(),
    )
    .expect("transport")
}

#[tokio::test]
async fn posts_mcp_headers_and_correlates_a_retried_id() {
    let body = "{\"jsonrpc\":\"2.0\",\"id\":9,\"result\":{\"ok\":true}}";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nMcp-Session-Id: fixture-session\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let (url, server) = fixture_response(response).await;
    let transport = test_transport(url);
    let request = request_body(
        9,
        "tools/call",
        "{\"name\":\"echo\"}",
        PROTOCOL_VERSION,
        "0.1.0",
    )
    .expect("request envelope");
    let extra = [(
        HeaderName::from_static("mcp-param-x-trace"),
        HeaderValue::from_static("trace-1"),
    )];
    let request_headers = outbound_headers(
        PROTOCOL_VERSION,
        Some("tools/call"),
        Some("echo"),
        &extra,
        None,
    )
    .expect("MCP headers");
    let deadline = CallDeadline::new(
        std::time::Duration::from_secs(5),
        std::time::Duration::from_secs(5),
    );
    let cancel = CancellationToken::new();
    let send_cx = super::SendCx {
        version: PROTOCOL_VERSION,
        token: None,
        cancel: &cancel,
        deadline: &deadline,
    };
    let (response, (headers, sent_body)) = tokio::join!(
        transport.send_once(
            request.as_str(),
            Some("tools/call"),
            Some("echo"),
            &extra,
            &send_cx,
        ),
        server,
    );
    let response = response.expect("HTTP response");
    assert!(
        headers
            .to_ascii_lowercase()
            .contains("mcp-method: tools/call")
    );
    assert!(
        headers
            .to_ascii_lowercase()
            .contains("mcp-protocol-version: 2026-07-28")
    );
    assert!(headers.to_ascii_lowercase().contains("mcp-name: echo"));
    assert!(
        headers
            .to_ascii_lowercase()
            .contains("mcp-param-x-trace: trace-1")
    );
    assert!(sent_body.contains("\"progressToken\":\"t-9\""));
    assert!(request_headers.contains_key("mcp-protocol-version"));
    transport
        .capture_session(&response)
        .await
        .expect("session header");
    let bytes = transport
        .read_body(
            response,
            super::RESPONSE_MAX,
            &CancellationToken::new(),
            &deadline,
        )
        .await
        .expect("bounded JSON response");
    let text = std::str::from_utf8(&bytes).expect("JSON UTF-8");
    let normalized = super::response_for_id(text, 9, 4).expect("correlated response");
    assert!(normalized.as_str().contains("\"id\":4"));
    assert_eq!(
        transport.session_id.lock().await.as_deref(),
        Some("fixture-session")
    );
}

#[tokio::test]
async fn consumes_request_scoped_sse_incrementally_and_extends_on_progress() {
    let body = concat!(
        "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{\"_meta\":{\"progressToken\":\"t-9\"}}}\n\n",
        "data: {\"jsonrpc\":\"2.0\",\"id\":9,\"result\":{\"ok\":true}}\n\n"
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let (url, server) = fixture_response(response).await;
    let transport = test_transport(url);
    let (response, _) = tokio::join!(
        async {
            let headers = outbound_headers(PROTOCOL_VERSION, Some("tools/call"), None, &[], None)
                .expect("headers");
            transport
                .client
                .post(transport.url.clone())
                .headers(headers)
                .body("{}".to_owned())
                .send()
                .await
                .expect("SSE response")
        },
        server,
    );
    let mut deadline = CallDeadline::new(
        std::time::Duration::from_secs(2),
        std::time::Duration::from_secs(4),
    );
    let cancel = CancellationToken::new();
    let initial_deadline = deadline.expires;
    let response = transport
        .read_event_stream(
            response,
            &mut super::StreamCx {
                request_id: 9,
                original_id: 4,
                version: PROTOCOL_VERSION,
                token: None,
                cancel: &cancel,
                deadline: &mut deadline,
            },
        )
        .await
        .expect("SSE reply");
    assert!(response.as_str().contains("\"id\":4"));
    assert!(deadline.expires > initial_deadline);
}

#[test]
fn validates_http_loopback_policy() {
    let loopback = Url::parse("http://127.0.0.1:9000/mcp").expect("loopback URL");
    let remote = Url::parse("http://example.com/mcp").expect("remote URL");
    assert!(super::validate_endpoint(&loopback).is_ok());
    assert!(super::validate_endpoint(&remote).is_err());
    assert!(
        super::validate_endpoint(&Url::parse("https://example.com/mcp").expect("HTTPS URL"))
            .is_ok()
    );
}

fn json_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// One seen request: the request line, the Authorization header, and the body.
struct SeenRequest {
    line: String,
    authorization: Option<String>,
    body: String,
}

/// Serves the OAuth discovery documents on `issuer` and replays `mcp_replies`
/// to `POST /mcp` in order. The future finishes once the last scripted reply is
/// answered; every request seen is returned, in order.
async fn auth_server(
    listener: TcpListener,
    issuer: String,
    resource: String,
    mcp_replies: Vec<String>,
) -> Vec<SeenRequest> {
    let mut seen = Vec::new();
    let mut mcp_replies = mcp_replies.into_iter().peekable();
    loop {
        let (stream, _) = listener.accept().await.expect("scripted request");
        let (headers, body, mut reader) = request_parts(stream).await;
        let line = headers.lines().next().unwrap_or_default().to_owned();
        let authorization = headers
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
            .map(|(_, value)| value.trim().to_owned());
        seen.push(SeenRequest {
            line: line.clone(),
            authorization,
            body,
        });
        let response = if line.starts_with("POST /mcp") {
            mcp_replies.next().expect("scripted /mcp reply")
        } else if line.starts_with("GET /.well-known/oauth-protected-resource") {
            json_response(&format!(
                "{{\"resource\":\"{resource}\",\"authorization_servers\":[\"{issuer}\"]}}"
            ))
        } else if line.starts_with("GET /.well-known/oauth-authorization-server") {
            json_response(&format!(
                "{{\"issuer\":\"{issuer}\",\"authorization_endpoint\":\"{issuer}/authorize\",\"token_endpoint\":\"{issuer}/token\"}}"
            ))
        } else if line.starts_with("POST /token") {
            json_response(
                "{\"access_token\":\"fresh-token\",\"token_type\":\"Bearer\",\"refresh_token\":\"rt-2\"}",
            )
        } else {
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned()
        };
        reader
            .get_mut()
            .write_all(response.as_bytes())
            .await
            .expect("scripted response");
        if line.starts_with("POST /mcp") && mcp_replies.peek().is_none() {
            return seen;
        }
    }
}

fn notify_transport(url: Url, tokens_path: PathBuf) -> HttpTransport {
    HttpTransport::new(
        Key {
            session: dal_core::SessionId::new_v7(),
            skill: "test-skill".to_owned(),
            server: "test-server".to_owned(),
        },
        url,
        tokens_path,
        "0.1.0".to_owned(),
        &Budgets::default(),
    )
    .expect("transport")
}

fn notify_call<'a>(
    cx: &'a dal_agent::ext::HookCx,
    cancel: &'a CancellationToken,
) -> super::CallCx<'a> {
    super::CallCx {
        method: "notifications/initialized",
        version: PROTOCOL_VERSION,
        services: cx.services.as_ref(),
        who: &cx.caller,
        cancel,
    }
}

async fn notify_fixture(
    record: token_auth::TokenRecord,
    mcp_replies: Vec<String>,
) -> (
    HttpTransport,
    impl Future<Output = Vec<SeenRequest>>,
    String,
    PathBuf,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener");
    let issuer = format!("http://{}", listener.local_addr().expect("address"));
    let url = Url::parse(&format!("{issuer}/mcp")).expect("fixture URL");
    let resource = token_auth::canonical_resource(&url);
    let tokens_path = std::env::temp_dir().join(format!(
        "dalgona-mcp-notify-tokens-{}.json",
        uuid::Uuid::new_v4()
    ));
    token_auth::persist_token(&tokens_path, &issuer, &resource, record).expect("seed tokens");
    let server = auth_server(listener, issuer.clone(), resource, mcp_replies);
    (
        notify_transport(url, tokens_path.clone()),
        server,
        issuer,
        tokens_path,
    )
}

fn http_response(status: u16, reason: &str, headers: &str) -> String {
    format!("HTTP/1.1 {status} {reason}\r\n{headers}Content-Length: 0\r\nConnection: close\r\n\r\n")
}

#[tokio::test]
async fn notify_retries_once_with_the_stored_token() {
    let (transport, server, _issuer, tokens_path) = notify_fixture(
        token_auth::TokenRecord {
            client_id: "fixture".to_owned(),
            access_token: "stored-token".to_owned(),
            refresh_token: None,
            scopes: Vec::new(),
        },
        vec![
            http_response(401, "Unauthorized", "WWW-Authenticate: Bearer\r\n"),
            http_response(202, "Accepted", ""),
        ],
    )
    .await;
    let cx = dal_agent::ext::HookCx::for_test(
        Arc::new(FakeServices::default()),
        dal_core::SessionId::new_v7(),
        None,
    );
    let cancel = CancellationToken::new();
    let call = notify_call(&cx, &cancel);
    let ids = AtomicU64::new(9);
    let (result, seen) = tokio::join!(transport.notify(&ids, &call), server);
    result.expect("notify retried with the stored token");
    let mcp_posts: Vec<&SeenRequest> = seen
        .iter()
        .filter(|request| request.line.starts_with("POST /mcp"))
        .collect();
    assert_eq!(mcp_posts.len(), 2);
    assert_eq!(mcp_posts[0].authorization, None);
    assert_eq!(
        mcp_posts[1].authorization.as_deref(),
        Some("Bearer stored-token")
    );
    assert!(seen.iter().any(|request| {
        request
            .line
            .starts_with("GET /.well-known/oauth-protected-resource")
    }));
    assert!(seen.iter().any(|request| {
        request
            .line
            .starts_with("GET /.well-known/oauth-authorization-server")
    }));
    let _ = std::fs::remove_file(tokens_path);
}

#[tokio::test]
async fn notify_refreshes_a_rejected_token_and_persists() {
    let (transport, server, issuer, tokens_path) = notify_fixture(
        token_auth::TokenRecord {
            client_id: "fixture".to_owned(),
            access_token: "stale-token".to_owned(),
            refresh_token: Some("rt-1".to_owned()),
            scopes: Vec::new(),
        },
        vec![
            http_response(401, "Unauthorized", ""),
            http_response(401, "Unauthorized", ""),
            http_response(202, "Accepted", ""),
        ],
    )
    .await;
    let cx = dal_agent::ext::HookCx::for_test(
        Arc::new(FakeServices::default()),
        dal_core::SessionId::new_v7(),
        None,
    );
    let cancel = CancellationToken::new();
    let call = notify_call(&cx, &cancel);
    let ids = AtomicU64::new(9);
    let (result, seen) = tokio::join!(transport.notify(&ids, &call), server);
    result.expect("notify refreshed the rejected token");
    let mcp_posts: Vec<&SeenRequest> = seen
        .iter()
        .filter(|request| request.line.starts_with("POST /mcp"))
        .collect();
    assert_eq!(mcp_posts.len(), 3);
    assert_eq!(
        mcp_posts[1].authorization.as_deref(),
        Some("Bearer stale-token")
    );
    assert_eq!(
        mcp_posts[2].authorization.as_deref(),
        Some("Bearer fresh-token")
    );
    let refresh = seen
        .iter()
        .find(|request| request.line.starts_with("POST /token"))
        .expect("refresh request");
    assert!(refresh.body.contains("grant_type=refresh_token"));
    assert!(refresh.body.contains("refresh_token=rt-1"));
    let tokens = token_auth::read_tokens(&tokens_path);
    let stored = token_auth::record_for(
        &tokens,
        &issuer,
        &token_auth::canonical_resource(&transport.url),
    )
    .expect("persisted record");
    assert_eq!(stored.access_token, "fresh-token");
    assert_eq!(stored.refresh_token.as_deref(), Some("rt-2"));
    let _ = std::fs::remove_file(tokens_path);
}

#[tokio::test]
async fn notify_fails_closed_on_a_plain_forbidden() {
    let (transport, server, _issuer, tokens_path) = notify_fixture(
        token_auth::TokenRecord {
            client_id: "fixture".to_owned(),
            access_token: "stored-token".to_owned(),
            refresh_token: None,
            scopes: Vec::new(),
        },
        vec![http_response(403, "Forbidden", "")],
    )
    .await;
    let cx = dal_agent::ext::HookCx::for_test(
        Arc::new(FakeServices::default()),
        dal_core::SessionId::new_v7(),
        None,
    );
    let cancel = CancellationToken::new();
    let call = notify_call(&cx, &cancel);
    let ids = AtomicU64::new(9);
    let (result, seen) = tokio::join!(transport.notify(&ids, &call), server);
    match result {
        Err(super::TransportError::Mcp(crate::mcp::McpError::HttpAuth { code, .. })) => {
            assert_eq!(code, 403);
        }
        other => panic!("a plain 403 must fail closed, got {other:?}"),
    }
    assert!(
        !seen
            .iter()
            .any(|request| request.line.starts_with("POST /token"))
    );
    let _ = std::fs::remove_file(tokens_path);
}
