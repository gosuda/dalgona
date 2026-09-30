// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use std::{future::Future, path::PathBuf};

use reqwest::{header::{HeaderName, HeaderValue}, Url};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};
use tokio_util::sync::CancellationToken;

use super::{
    protocol::{outbound_headers, request_body, PROTOCOL_VERSION},
    CallDeadline, HttpTransport,
};
use crate::mcp::{tools::Key, Budgets};

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
    (headers, String::from_utf8(body).expect("UTF-8 request body"), reader)
}

async fn fixture_response(response: String) -> (Url, impl Future<Output = (String, String)>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("loopback listener");
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
    let request = request_body(9, "tools/call", "{\"name\":\"echo\"}", PROTOCOL_VERSION, "0.1.0")
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
    let deadline = CallDeadline::new(std::time::Duration::from_secs(5), std::time::Duration::from_secs(5));
    let cancel = CancellationToken::new();
    let (response, (headers, sent_body)) = tokio::join!(
        transport.send_once(
            request.as_str(),
            Some("tools/call"),
            Some("echo"),
            &extra,
            PROTOCOL_VERSION,
            None,
            &cancel,
            &deadline,
        ),
        server,
    );
    let response = response.expect("HTTP response");
    assert!(headers.to_ascii_lowercase().contains("mcp-method: tools/call"));
    assert!(headers.to_ascii_lowercase().contains("mcp-protocol-version: 2026-07-28"));
    assert!(headers.to_ascii_lowercase().contains("mcp-name: echo"));
    assert!(headers.to_ascii_lowercase().contains("mcp-param-x-trace: trace-1"));
    assert!(sent_body.contains("\"progressToken\":\"t-9\""));
    assert!(request_headers.contains_key("mcp-protocol-version"));
    transport.capture_session(&response).await.expect("session header");
    let bytes = transport
        .read_body(response, super::RESPONSE_MAX, &CancellationToken::new(), &deadline)
        .await
        .expect("bounded JSON response");
    let text = std::str::from_utf8(&bytes).expect("JSON UTF-8");
    let normalized = super::response_for_id(text, 9, 4).expect("correlated response");
    assert!(normalized.as_str().contains("\"id\":4"));
    assert_eq!(transport.session_id.lock().await.as_deref(), Some("fixture-session"));
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
    let mut deadline = CallDeadline::new(std::time::Duration::from_secs(2), std::time::Duration::from_secs(4));
    let initial_deadline = deadline.expires;
    let response = transport
        .read_event_stream(
            response,
            9,
            4,
            &CancellationToken::new(),
            &mut deadline,
            PROTOCOL_VERSION,
            None,
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
    assert!(super::validate_endpoint(&Url::parse("https://example.com/mcp").expect("HTTPS URL")).is_ok());
}
