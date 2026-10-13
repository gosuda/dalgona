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
fn redirects_follow_https_and_loopback_only_from_loopback() {
    let remote = [url("https://api.openai.com/v1/responses")];
    let local = [url("http://127.0.0.1:4000/v1/responses")];
    assert!(follow(&url("https://cdn.openai.com/x"), &remote).is_ok());
    assert!(follow(&url("http://[::1]:4000/y"), &local).is_ok());
    assert!(follow(&url("https://example.com/y"), &local).is_ok());
    assert!(matches!(
        follow(&url("http://example.com/x"), &remote),
        Err(RefusedRedirect::PlainHttp { host }) if host == "example.com"
    ));
    assert!(matches!(
        follow(&url("http://example.com/x"), &local),
        Err(RefusedRedirect::PlainHttp { .. })
    ));
    assert!(matches!(
        follow(&url("http://127.0.0.1:22/"), &remote),
        Err(RefusedRedirect::Target { .. })
    ));
    assert!(matches!(
        follow(&url("ftp://example.com/"), &remote),
        Err(RefusedRedirect::Target { .. })
    ));
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
