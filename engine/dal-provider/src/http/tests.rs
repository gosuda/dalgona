#![expect(
    clippy::disallowed_methods,
    reason = "tests bind real listeners; the joined handle bounds each helper task's lifetime"
)]

use std::cell::Cell;

use futures::executor::block_on;
use futures::future::{pending, ready};

use super::*;

fn url(text: &str) -> Url {
    Url::parse(text).unwrap()
}

#[test]
fn websocket_tls_connector_builds_once_and_names_the_fix() {
    let first = crate::http::websocket_connector();
    let second = crate::http::websocket_connector();
    match (first, second) {
        (Ok(first), Ok(second)) => match (first, second) {
            (tokio_tungstenite::Connector::Rustls(a), tokio_tungstenite::Connector::Rustls(b)) => {
                assert!(std::sync::Arc::ptr_eq(&a, &b));
            }
            _ => panic!("expected two Rustls connectors"),
        },
        (Err(first), Err(second)) => {
            assert!(first.to_string().contains("system CA certificates"));
            assert_eq!(first.to_string(), second.to_string());
        }
        _ => panic!("connector must settle the same way twice"),
    }
}

fn transport_reason(error: ProviderError) -> String {
    match error {
        ProviderError::Transport {
            family: Family::Chat,
            reason,
        } => reason,
        other => panic!("expected Transport, got {other:?}"),
    }
}

fn plain_http_host(error: ProviderError) -> String {
    match error {
        ProviderError::PlainHttp { host } => host,
        other => panic!("expected PlainHttp, got {other:?}"),
    }
}

#[test]
fn base_url_admits_https_and_literal_loopback_http() {
    for base in [
        "https://api.openai.com/v1",
        "https://api.anthropic.com/",
        "http://localhost:8080",
        "http://LOCALHOST/",
        "http://127.0.0.1:9/v1",
        "http://[::1]:9/v1/",
    ] {
        assert!(check_base_url(Family::Chat, base).is_ok(), "{base}");
    }
}

#[test]
fn host_confusion_never_reads_as_loopback() {
    for (base, host) in [
        ("http://localhost@evil.example/v1", "evil.example"),
        ("http://127.0.0.1@evil.example/v1", "evil.example"),
        ("http://[::1]@example.com", "example.com"),
        ("http://localhost.evil.example", "localhost.evil.example"),
        ("http://127.0.0.1.evil.example", "127.0.0.1.evil.example"),
        ("http://localhost%2eevil.example", "localhost.evil.example"),
        (
            "http://localhost\u{ff0e}evil.example",
            "localhost.evil.example",
        ),
        ("http://evil.example\\@127.0.0.1/v1", "evil.example"),
        ("HTTP://example.com", "example.com"),
        ("HtTp://EXAMPLE.com/v1", "example.com"),
    ] {
        let error = check_base_url(Family::Chat, base).expect_err(base);
        assert!(
            matches!(&error, ProviderError::PlainHttp { host: seen } if seen == host),
            "{base}: {error:?}"
        );
    }
}

#[test]
fn addresses_next_to_the_loopback_literals_are_plain_http() {
    let mut problems = Vec::new();
    for (base, host) in [
        ("http://127.1.0.1/v1", "127.1.0.1"),
        ("http://[::]/v1", "[::]"),
        ("http://[::2]/v1", "[::2]"),
        ("http://[::7f00:1]/v1", "[::7f00:1]"),
    ] {
        match check_base_url(Family::Chat, base) {
            Err(ProviderError::PlainHttp { host: seen }) if seen == host => {}
            other => problems.push(format!("{base}: {other:?}")),
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn base_url_credentials_are_refused_in_every_spelling_and_never_echoed() {
    for (base, host) in [
        ("https://user:hunter2@example.com/v1", "example.com"),
        ("https://user@example.com/v1", "example.com"),
        ("https://:hunter2@example.com/v1", "example.com"),
        ("http://user:hunter2@127.0.0.1:8080/v1", "127.0.0.1"),
        ("http://hunter2@localhost/v1", "localhost"),
    ] {
        assert_eq!(
            transport_reason(check_base_url(Family::Chat, base).expect_err(base)),
            format!("base URL for {host} must not carry credentials"),
            "{base}"
        );
    }
}

#[test]
fn base_url_query_and_fragment_are_refused_even_when_empty_and_never_echoed() {
    for (base, origin) in [
        (
            "https://example.com/v1?api_key=hunter2",
            "https://example.com",
        ),
        ("https://example.com/v1#hunter2", "https://example.com"),
        ("https://example.com/v1?", "https://example.com"),
        ("https://example.com/v1#", "https://example.com"),
        ("http://127.0.0.1:8080/v1?x=1", "http://127.0.0.1:8080"),
    ] {
        assert_eq!(
            transport_reason(check_base_url(Family::Chat, base).expect_err(base)),
            format!("base URL for {origin} must not carry a query or fragment"),
            "{base}"
        );
    }
}

#[test]
fn plain_http_elsewhere_fails_before_any_connection() {
    for (base, host) in [
        ("http://example.com", "example.com"),
        ("http://localhost.:8080", "localhost."),
        ("http://127.0.0.2", "127.0.0.2"),
        ("http://[::ffff:127.0.0.1]", "[::ffff:7f00:1]"),
        ("http://[2001:db8::1]/v1", "[2001:db8::1]"),
        ("http://0.0.0.0", "0.0.0.0"),
    ] {
        assert_eq!(
            plain_http_host(check_base_url(Family::Chat, base).unwrap_err()),
            host
        );
    }
    let error = check_base_url(Family::Chat, "http://example.com").unwrap_err();
    assert_eq!(
        error.to_string(),
        "refusing plain http for non-loopback host example.com."
    );
}

#[test]
fn other_base_url_defects_are_transport() {
    for (base, reason) in [
        ("ftp://example.com", "URL scheme \"ftp\" is not https"),
        ("ws://127.0.0.1", "URL scheme \"ws\" is not https"),
        (
            "https://user:secret@example.com/v1",
            "base URL for example.com must not carry credentials",
        ),
        (
            "https://example.com/v1?key=1",
            "base URL for https://example.com must not carry a query or fragment",
        ),
        (
            "https://example.com/v1#top",
            "base URL for https://example.com must not carry a query or fragment",
        ),
    ] {
        assert_eq!(
            transport_reason(check_base_url(Family::Chat, base).unwrap_err()),
            reason,
            "{base}"
        );
    }
    let unparsable = transport_reason(check_base_url(Family::Chat, "not a url").unwrap_err());
    assert!(
        unparsable.starts_with("base URL is not a URL: "),
        "{unparsable}"
    );
}

#[test]
fn endpoint_joins_with_exactly_one_slash() {
    for (base, path, expected) in [
        (
            "https://api.openai.com/v1/",
            "responses",
            "https://api.openai.com/v1/responses",
        ),
        (
            "https://api.openai.com/v1",
            "responses",
            "https://api.openai.com/v1/responses",
        ),
        (
            "https://api.openai.com/v1//",
            "/responses",
            "https://api.openai.com/v1/responses",
        ),
        (
            "https://api.anthropic.com",
            "v1/messages",
            "https://api.anthropic.com/v1/messages",
        ),
        (
            "https://api.anthropic.com/",
            "/v1/messages",
            "https://api.anthropic.com/v1/messages",
        ),
        (
            "http://127.0.0.1:4000/",
            "v1/chat/completions",
            "http://127.0.0.1:4000/v1/chat/completions",
        ),
        (
            "http://[::1]:4000",
            "usage?window=7d",
            "http://[::1]:4000/usage?window=7d",
        ),
    ] {
        assert_eq!(
            endpoint(Family::Chat, base, path).unwrap().as_str(),
            expected
        );
    }
}

#[test]
fn endpoint_refuses_encoded_dot_segments_that_climb_out_of_the_base_prefix() {
    for path in [
        "%2e%2e/admin",
        "%2E%2E/admin",
        ".%2e/admin",
        "%2e./admin",
        "a/%2e%2e/%2e%2e/admin",
    ] {
        assert_eq!(
            transport_reason(endpoint(Family::Chat, "https://h.example/v1", path).unwrap_err()),
            format!("endpoint path {path:?} escapes the base URL"),
            "{path}"
        );
    }
}

#[test]
fn endpoint_rejects_paths_that_escape_or_are_empty() {
    for path in [
        "",
        "/",
        "?x=1",
        "a//b",
        "//evil.example/x",
        "../admin",
        "v1/./x",
        "a\\..\\x",
        "a#frag",
    ] {
        transport_reason(endpoint(Family::Chat, "https://h.example/v1", path).unwrap_err());
    }
    let host = plain_http_host(endpoint(Family::Chat, "http://example.com", "v1").unwrap_err());
    assert_eq!(host, "example.com");
}

#[test]
fn redirects_stay_within_the_origin_and_never_reach_plaintext() {
    let remote = [url("https://api.openai.com/v1/responses")];
    let local = [url("http://127.0.0.1:4000/v1/responses")];
    assert!(follow(&url("https://api.openai.com/v2/responses"), &remote).is_ok());
    assert!(follow(&url("http://127.0.0.1:4000/y"), &local).is_ok());
    for (next, previous) in [
        ("https://cdn.openai.com/x", &remote),
        ("https://api.openai.com:8443/x", &remote),
        ("https://example.com/y", &local),
        ("http://[::1]:4000/y", &local),
        ("http://127.0.0.1:22/", &remote),
        ("http://127.0.0.1:4001/", &local),
        ("ftp://example.com/", &remote),
    ] {
        assert!(
            matches!(
                follow(&url(next), previous),
                Err(RefusedRedirect::Target { .. })
            ),
            "{next}"
        );
    }
    for previous in [&remote, &local] {
        assert!(matches!(
            follow(&url("http://example.com/x"), previous),
            Err(RefusedRedirect::PlainHttp { host }) if host == "example.com"
        ));
    }
}

#[test]
fn redirects_never_leave_the_hop_origin_in_disguise() {
    let remote = [url("https://api.openai.com/v1/responses")];
    let local = [url("http://127.0.0.1:4000/v1/responses")];
    for next in ["https://api.openai.com:443/x", "HTTPS://API.OPENAI.COM/x"] {
        assert!(follow(&url(next), &remote).is_ok(), "{next}");
    }
    for (next, previous) in [
        ("https://api.openai.com@evil.example/x", &remote),
        ("https://api.openai.com.evil.example/x", &remote),
        ("https://api.openai.com%2eevil.example/x", &remote),
        ("https://127.0.0.1:4000/y", &local),
        ("https://127.0.0.1:4000@evil.example/y", &local),
    ] {
        assert!(
            matches!(
                follow(&url(next), previous),
                Err(RefusedRedirect::Target { .. })
            ),
            "{next}"
        );
    }
    for next in [
        "http://localhost@evil.example/x",
        "http://127.0.0.1.evil.example/x",
        "http://localhost.evil.example/x",
    ] {
        assert!(
            matches!(
                follow(&url(next), &local),
                Err(RefusedRedirect::PlainHttp { host }) if host.ends_with("evil.example")
            ),
            "{next}"
        );
    }
}

#[test]
fn a_redirect_hop_with_no_previous_origin_fails_closed() {
    assert!(matches!(
        follow(&url("https://api.openai.com/x"), &[]),
        Err(RefusedRedirect::Target { .. })
    ));
}

#[test]
fn redirect_chain_is_capped() {
    let hops: Vec<Url> = (0..=MAX_REDIRECTS)
        .map(|n| url(&format!("https://h.example/{n}")))
        .collect();
    assert!(follow(&url("https://h.example/next"), &hops[..MAX_REDIRECTS - 1]).is_ok());
    assert!(follow(&url("https://h.example/next"), &hops[..MAX_REDIRECTS]).is_ok());
    assert!(matches!(
        follow(&url("https://h.example/next"), &hops),
        Err(RefusedRedirect::TooMany)
    ));
}

#[test]
fn prepare_sets_the_request_contract() {
    let client = build_client();
    let agent = user_agent("0.1.0", "linux", "7.0", "x86_64");
    assert_eq!(agent, "dalgon/0.1.0 (linux 7.0; x86_64)");

    let json = client.post("https://h.example/v1/responses").body("{}");
    let (_, request) = prepare(
        Family::Responses,
        json,
        &agent,
        Exchange::Json {
            total: NON_STREAM_TOTAL_TIMEOUT,
        },
    )
    .unwrap();
    assert_eq!(request.headers()[USER_AGENT], agent.as_str());
    assert_eq!(request.headers()[ACCEPT], "application/json");
    assert_eq!(request.headers()[CONTENT_TYPE], "application/json");
    assert_eq!(request.timeout(), Some(&NON_STREAM_TOTAL_TIMEOUT));

    let form = client
        .post("http://localhost:1455/oauth/token")
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body("grant_type=refresh_token");
    let (_, request) = prepare(
        Family::Codex,
        form,
        &agent,
        Exchange::Json {
            total: OAUTH_TIMEOUT,
        },
    )
    .unwrap();
    assert_eq!(
        request.headers()[CONTENT_TYPE],
        "application/x-www-form-urlencoded"
    );

    let stream = client
        .post("https://h.example/v1/messages")
        .timeout(COMPACT_TIMEOUT)
        .body("{}");
    let (_, request) = prepare(Family::Anthropic, stream, &agent, Exchange::Stream).unwrap();
    assert_eq!(request.headers()[ACCEPT], "text/event-stream");
    assert_eq!(request.timeout(), None);

    let get = client.get("https://h.example/usage");
    let (_, request) = prepare(
        Family::Codex,
        get,
        &agent,
        Exchange::Json {
            total: USAGE_TIMEOUT,
        },
    )
    .unwrap();
    assert!(!request.headers().contains_key(CONTENT_TYPE));
}

#[test]
fn prepare_sends_a_bytes_accept_and_no_total_when_it_carries_none() {
    let client = build_client();
    let agent = user_agent("0.1.0", "linux", "7.0", "x86_64");

    let get = client.get("https://h.example/video/1");
    let (_, request) = prepare(
        Family::Chat,
        get,
        &agent,
        Exchange::Bytes {
            accept: "video/*",
            total: None,
        },
    )
    .unwrap();
    assert_eq!(request.headers()[ACCEPT], "video/*");
    assert_eq!(request.timeout(), None);
    assert!(!request.headers().contains_key(CONTENT_TYPE));

    let bounded = client.get("https://h.example/audio/1");
    let (_, request) = prepare(
        Family::Chat,
        bounded,
        &agent,
        Exchange::Bytes {
            accept: "audio/*",
            total: Some(NON_STREAM_TOTAL_TIMEOUT),
        },
    )
    .unwrap();
    assert_eq!(request.headers()[ACCEPT], "audio/*");
    assert_eq!(request.timeout(), Some(&NON_STREAM_TOTAL_TIMEOUT));
}

#[test]
fn prepare_refuses_an_invalid_bytes_accept_before_any_connection() {
    let client = build_client();
    let request = client.get("https://h.example/video/1");
    let error = prepare(
        Family::Chat,
        request,
        "dalgon/0",
        Exchange::Bytes {
            accept: "video/*\r\nx-evil: 1",
            total: None,
        },
    )
    .unwrap_err();
    assert_eq!(
        transport_reason(error),
        "accept \"video/*\\r\\nx-evil: 1\" is not a header value"
    );
}

#[test]
fn prepare_refuses_plain_http_and_bad_agents() {
    let client = build_client();
    let exchange = Exchange::Json {
        total: NON_STREAM_TOTAL_TIMEOUT,
    };
    let plain = client.get("http://example.com/v1/models");
    assert_eq!(
        plain_http_host(prepare(Family::Chat, plain, "dalgon/0", exchange).unwrap_err()),
        "example.com"
    );
    let bad_agent = client.get("https://h.example/v1/models");
    transport_reason(prepare(Family::Chat, bad_agent, "dalgon\n0", exchange).unwrap_err());
}

#[test]
fn send_refuses_plain_http_without_starting_the_clock() {
    let started = Cell::new(false);
    let request = build_client().get("http://example.com/v1/models");
    let error = block_on(send(
        Family::Chat,
        request,
        "dalgon/0",
        Exchange::Stream,
        |_| {
            started.set(true);
            pending::<()>()
        },
    ))
    .unwrap_err();
    assert_eq!(plain_http_host(error), "example.com");
    assert!(!started.get());
}

#[test]
fn send_drops_the_request_when_the_header_deadline_has_passed() {
    let asked = Cell::new(None);
    let request = build_client().get("https://127.0.0.1:9/v1/models");
    let error = block_on(send(
        Family::Chat,
        request,
        "dalgon/0",
        Exchange::Stream,
        |duration| {
            asked.set(Some(duration));
            ready(())
        },
    ))
    .unwrap_err();
    assert_eq!(transport_reason(error), "no response headers within 60 s");
    assert_eq!(asked.get(), Some(RESPONSE_HEADER_TIMEOUT));
}

#[test]
fn body_cap_admits_the_limit_and_refuses_one_byte_more() {
    let mut body = Vec::new();
    append_capped(
        &mut body,
        &vec![b'a'; BODY_LIMIT - 1],
        BODY_LIMIT,
        LimitError::Body,
    )
    .unwrap();
    append_capped(&mut body, b"b", BODY_LIMIT, LimitError::Body).unwrap();
    assert_eq!(body.len(), BODY_LIMIT);
    let error = append_capped(&mut body, b"c", BODY_LIMIT, LimitError::Body).unwrap_err();
    assert!(matches!(error, ProviderError::Limit(LimitError::Body)));
    assert_eq!(body.len(), BODY_LIMIT);
}

fn streamed(chunks: Vec<Result<Vec<u8>, std::io::Error>>) -> reqwest::Response {
    reqwest::Response::from(hyper::http::Response::new(reqwest::Body::wrap_stream(
        futures::stream::iter(chunks),
    )))
}

#[test]
fn read_body_admits_the_limit_and_refuses_one_byte_more() {
    const MIB: usize = 1 << 20;
    let at_limit = streamed(
        std::iter::repeat_with(|| Ok(vec![b'a'; MIB]))
            .take(BODY_LIMIT / MIB)
            .collect(),
    );
    assert_eq!(
        block_on(read_body(Family::Chat, at_limit)).unwrap().len(),
        BODY_LIMIT
    );

    let mut over: Vec<Result<Vec<u8>, std::io::Error>> =
        std::iter::repeat_with(|| Ok(vec![b'a'; MIB]))
            .take(BODY_LIMIT / MIB)
            .collect();
    over.push(Ok(vec![b'b']));
    assert!(matches!(
        block_on(read_body(Family::Chat, streamed(over))),
        Err(ProviderError::Limit(LimitError::Body))
    ));

    let declared = reqwest::Response::from(hyper::http::Response::new(reqwest::Body::from(vec![
            b'a';
            BODY_LIMIT
                + 1
        ])));
    assert!(matches!(
        block_on(read_body(Family::Chat, declared)),
        Err(ProviderError::Limit(LimitError::Body))
    ));
}

#[test]
fn read_body_limited_reports_the_limit_it_applied() {
    const LIMIT: usize = 8;
    let under = streamed(vec![Ok(b"1234567".to_vec())]);
    assert_eq!(
        block_on(read_body_limited(Family::Chat, under, LIMIT)).unwrap(),
        b"1234567"
    );

    let at_limit = streamed(vec![Ok(b"1234567".to_vec()), Ok(vec![b'8'])]);
    assert_eq!(
        block_on(read_body_limited(Family::Chat, at_limit, LIMIT)).unwrap(),
        b"12345678"
    );

    let one_over = streamed(vec![Ok(b"12345678".to_vec()), Ok(vec![b'9'])]);
    assert!(matches!(
        block_on(read_body_limited(Family::Chat, one_over, LIMIT)),
        Err(ProviderError::Limit(LimitError::Payload { limit: 8 }))
    ));

    let declared = reqwest::Response::from(hyper::http::Response::new(reqwest::Body::from(vec![
        b'a';
        LIMIT + 1
    ])));
    assert!(matches!(
        block_on(read_body_limited(Family::Chat, declared, LIMIT)),
        Err(ProviderError::Limit(LimitError::Payload { limit: 8 }))
    ));
}

#[test]
fn a_body_cut_mid_read_is_a_transport_error_never_a_partial_ok() {
    let cut = streamed(vec![
        Ok(b"{\"error\":".to_vec()),
        Err(std::io::Error::other("connection reset")),
    ]);
    transport_reason(block_on(read_body(Family::Chat, cut)).unwrap_err());
}

#[test]
fn lazy_client_builds_on_first_use_and_clones_share_one_build() {
    let lazy = LazyClient::default();
    let clone = lazy.clone();
    assert!(lazy.0.get().is_none());
    let shared = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|index| {
                let client = if index % 2 == 0 { &lazy } else { &clone };
                scope.spawn(move || client.get())
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert!(shared.iter().all(|client| std::ptr::eq(*client, shared[0])));
    assert!(lazy.0.get().is_some());

    let prebuilt = LazyClient::from(build_client());
    assert!(prebuilt.0.get().is_some());
}

#[tokio::test]
async fn a_cross_origin_redirect_never_carries_provider_credentials() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target_port = target.local_addr().unwrap().port();
    let hop = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let hop_port = hop.local_addr().unwrap().port();
    let leaked = tokio::spawn(async move {
        let (mut socket, _) = target.accept().await.unwrap();
        let mut buffer = vec![0; 8192];
        let length = socket.read(&mut buffer).await.unwrap();
        socket
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();
        buffer.truncate(length);
        String::from_utf8_lossy(&buffer).into_owned()
    });
    tokio::spawn(async move {
        let (mut socket, _) = hop.accept().await.unwrap();
        let mut buffer = vec![0; 8192];
        let _ = socket.read(&mut buffer).await.unwrap();
        let reply = format!(
            "HTTP/1.1 307 Temporary Redirect\r\nlocation: http://127.0.0.1:{target_port}/leak\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        );
        socket.write_all(reply.as_bytes()).await.unwrap();
    });

    let request = build_client()
        .post(format!("http://127.0.0.1:{hop_port}/v1/messages"))
        .header("x-api-key", "sk-secret")
        .header("chatgpt-account-id", "acct-secret")
        .body("{}");
    let error = send(
        Family::Anthropic,
        request,
        "dalgon/0",
        Exchange::Json {
            total: Duration::from_secs(5),
        },
        tokio::time::sleep,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(error, ProviderError::Transport { .. }),
        "{error:?}"
    );
    let seen = tokio::time::timeout(Duration::from_millis(300), leaked).await;
    assert!(seen.is_err(), "the redirect target received {seen:?}");
}

#[tokio::test]
async fn the_no_redirect_client_surfaces_each_hop_to_its_caller() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = server.local_addr().unwrap().port();
    tokio::spawn(async move {
        // Three connections: the no-redirect request, then the default
        // client's initial request and its same-origin follow.
        for _ in 0..3 {
            let Ok((mut socket, _)) = server.accept().await else {
                return;
            };
            let mut buffer = vec![0; 8192];
            let Ok(length) = socket.read(&mut buffer).await else {
                return;
            };
            let request = String::from_utf8_lossy(&buffer[..length]).into_owned();
            let reply = if request.starts_with("GET /b") {
                "HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok"
            } else {
                "HTTP/1.1 307 Temporary Redirect\r\nlocation: /b\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            };
            if socket.write_all(reply.as_bytes()).await.is_err() {
                return;
            }
        }
    });

    let hop = build_client_no_redirect()
        .get(format!("http://127.0.0.1:{port}/a"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        hop.status(),
        reqwest::StatusCode::TEMPORARY_REDIRECT,
        "the hop is returned for the caller to validate"
    );

    let followed = build_client()
        .get(format!("http://127.0.0.1:{port}/a"))
        .send()
        .await
        .unwrap();
    assert_eq!(followed.status(), reqwest::StatusCode::OK);
    assert_eq!(followed.text().await.unwrap(), "ok");
}

#[tokio::test]
async fn bytes_exchange_sends_its_accept_and_reads_the_body() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = server.local_addr().unwrap().port();
    let seen = tokio::spawn(async move {
        let (mut socket, _) = server.accept().await.unwrap();
        let mut buffer = vec![0; 8192];
        let length = socket.read(&mut buffer).await.unwrap();
        socket
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 4\r\nconnection: close\r\n\r\nRIFF")
            .await
            .unwrap();
        buffer.truncate(length);
        String::from_utf8_lossy(&buffer).into_owned()
    });

    let request = build_client().get(format!("http://127.0.0.1:{port}/audio/1"));
    let response = send(
        Family::Chat,
        request,
        "dalgon/0",
        Exchange::Bytes {
            accept: "audio/*",
            total: Some(Duration::from_secs(5)),
        },
        tokio::time::sleep,
    )
    .await
    .unwrap();
    let body = read_body_limited(Family::Chat, response, 1 << 20)
        .await
        .unwrap();
    assert_eq!(body, b"RIFF");
    let wire = tokio::time::timeout(Duration::from_secs(5), seen)
        .await
        .unwrap()
        .unwrap();
    assert!(
        wire.contains("accept: audio/*\r\n"),
        "the accept header reached the server, got {wire:?}"
    );
}

#[tokio::test]
async fn a_body_held_past_its_total_is_a_transport_error_never_a_partial_ok() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = server.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut socket, _) = server.accept().await.unwrap();
        let mut buffer = vec![0; 8192];
        if socket.read(&mut buffer).await.is_err() {
            return;
        }
        let head = b"HTTP/1.1 200 OK\r\ncontent-length: 64\r\nconnection: close\r\n\r\n";
        if socket.write_all(head).await.is_err() {
            return;
        }
        // Hold the body open past every deadline: the half-open socket keeps
        // the read pending instead of ending it.
        let _ = socket.read(&mut buffer).await;
    });

    let request = build_client().get(format!("http://127.0.0.1:{port}/video/1"));
    let response = send(
        Family::Chat,
        request,
        "dalgon/0",
        Exchange::Bytes {
            accept: "video/*",
            total: Some(Duration::from_millis(200)),
        },
        tokio::time::sleep,
    )
    .await
    .unwrap();
    transport_reason(
        read_body_limited(Family::Chat, response, 1 << 20)
            .await
            .unwrap_err(),
    );
}

#[tokio::test]
async fn loopback_bounds_apply_to_declared_and_streamed_bodies() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = server.local_addr().unwrap().port();
    tokio::spawn(async move {
        for _ in 0..3 {
            let Ok((mut socket, _)) = server.accept().await else {
                return;
            };
            let mut buffer = vec![0; 8192];
            let Ok(length) = socket.read(&mut buffer).await else {
                return;
            };
            let request = String::from_utf8_lossy(&buffer[..length]).into_owned();
            let reply = if request.starts_with("GET /declared ") {
                "HTTP/1.1 200 OK\r\ncontent-length: 9\r\nconnection: close\r\n\r\n123456789"
                    .to_owned()
            } else if request.starts_with("GET /streamed ") {
                "HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n9\r\n123456789\r\n0\r\n\r\n"
                    .to_owned()
            } else {
                "HTTP/1.1 200 OK\r\ncontent-length: 8\r\nconnection: close\r\n\r\n12345678"
                    .to_owned()
            };
            let _ = socket.write_all(reply.as_bytes()).await;
        }
    });

    for (path, want) in [
        ("/declared", None),
        ("/streamed", None),
        ("/exact", Some(b"12345678".as_slice())),
    ] {
        let request = build_client().get(format!("http://127.0.0.1:{port}{path}"));
        let response = send(
            Family::Chat,
            request,
            "dalgon/0",
            Exchange::Bytes {
                accept: "application/octet-stream",
                total: Some(Duration::from_secs(5)),
            },
            tokio::time::sleep,
        )
        .await
        .unwrap();
        match want {
            Some(text) => assert_eq!(
                read_body_limited(Family::Chat, response, 8).await.unwrap(),
                text
            ),
            None => assert!(matches!(
                read_body_limited(Family::Chat, response, 8).await,
                Err(ProviderError::Limit(LimitError::Payload { limit: 8 }))
            )),
        }
    }
}
