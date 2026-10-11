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
    for base in [
        "ftp://example.com",
        "ws://127.0.0.1",
        "not a url",
        "https://user:secret@example.com/v1",
        "https://example.com/v1?key=1",
        "https://example.com/v1#top",
    ] {
        transport_reason(check_base_url(Family::Chat, base).unwrap_err());
    }
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
fn redirect_chain_is_capped() {
    let hops: Vec<Url> = (0..=MAX_REDIRECTS)
        .map(|n| url(&format!("https://h.example/{n}")))
        .collect();
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
    append_capped(&mut body, &vec![b'a'; BODY_LIMIT - 1]).unwrap();
    append_capped(&mut body, b"b").unwrap();
    assert_eq!(body.len(), BODY_LIMIT);
    let error = append_capped(&mut body, b"c").unwrap_err();
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
