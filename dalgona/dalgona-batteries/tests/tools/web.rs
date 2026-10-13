// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use std::time::Duration;

use super::common::{Host, Net, Received, TestResult, locked, run_tool, serve};
use dal_core::FetchResponse;
use dalgona_batteries::web::{WebConfig, web};
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};

fn config(timeout_secs: u32) -> WebConfig {
    WebConfig {
        enabled: true,
        provider: "brave".into(),
        api_key_env: "BRAVE_API_KEY".into(),
        search_depth: None,
        timeout_secs,
        max_redirects: 10,
        max_body_bytes: 2_097_152,
        max_markdown_bytes: 131_072,
    }
}

fn tavily_config() -> WebConfig {
    WebConfig {
        provider: "tavily".into(),
        api_key_env: "TAVILY_API_KEY".into(),
        ..config(30)
    }
}

fn reply(status: u16, content_type: &str, body: &[u8]) -> FetchResponse {
    FetchResponse {
        status,
        headers: vec![("Content-Type".into(), content_type.into())],
        body: body.to_vec(),
    }
}

fn redirect(location: &str) -> FetchResponse {
    FetchResponse {
        status: 302,
        headers: vec![("Location".into(), location.into())],
        body: Vec::new(),
    }
}

fn scripted(responses: impl IntoIterator<Item = FetchResponse>) -> std::sync::Arc<Host> {
    let host = Host::answering([]);
    *locked(&host.net) = Net::Script;
    locked(&host.net_script).extend(responses);
    host
}

fn requested_urls(host: &Host) -> Vec<String> {
    locked(&host.net_requests)
        .iter()
        .map(|request| request.url.to_string())
        .collect()
}

fn parse(text: &str) -> Result<Value, Box<dyn std::error::Error>> {
    Ok(sonic_rs::from_str(text)?)
}

#[tokio::test]
async fn a_fetch_that_outlives_the_deadline_times_out_naming_the_url() -> TestResult {
    let host = Host::answering([]);
    *locked(&host.net) = Net::Hang;
    let extension = web(config(1))?;
    let text = run_tool(
        &extension,
        &host,
        "web_fetch",
        r#"{"url":"http://1.1.1.1/slow"}"#,
    )
    .await?;
    assert_eq!(text, "web_fetch timed out after 1s: http://1.1.1.1/slow");
    Ok(())
}

#[tokio::test]
async fn an_unreachable_net_service_is_a_network_error_not_a_timeout() -> TestResult {
    let host = Host::answering([]);
    let extension = web(config(30))?;
    let text = run_tool(
        &extension,
        &host,
        "web_fetch",
        r#"{"url":"http://1.1.1.1/"}"#,
    )
    .await?;
    assert!(text.starts_with("web_fetch failed: "), "{text}");
    Ok(())
}

#[tokio::test]
async fn network_cancellation_interrupts_the_tool() -> TestResult {
    let host = Host::answering([]);
    *locked(&host.net) = Net::Cancel;
    let extension = web(config(30))?;
    let text = run_tool(
        &extension,
        &host,
        "web_fetch",
        r#"{"url":"http://1.1.1.1/"}"#,
    )
    .await?;
    assert_eq!(text, "interrupted");
    Ok(())
}

#[tokio::test]
async fn non_http_schemes_never_reach_the_net_service() -> TestResult {
    let host = Host::answering([]);
    *locked(&host.net) = Net::Hang;
    let extension = web(config(1))?;
    let text = run_tool(
        &extension,
        &host,
        "web_fetch",
        r#"{"url":"ftp://example.test/x"}"#,
    )
    .await?;
    assert_eq!(
        text,
        "unsupported URL scheme \"ftp\": only http and https are allowed"
    );
    Ok(())
}

// --- the redirect cap -------------------------------------------------

#[tokio::test]
async fn a_four_hop_chain_exceeds_a_two_redirect_cap_after_exactly_three_requests() -> TestResult {
    let host = scripted([
        redirect("/h1"),
        redirect("/h2"),
        redirect("/h3"),
        redirect("/h4"),
    ]);
    let extension = web(WebConfig {
        max_redirects: 2,
        ..config(30)
    })?;
    let text = run_tool(
        &extension,
        &host,
        "web_fetch",
        r#"{"url":"http://1.1.1.1/start"}"#,
    )
    .await?;
    assert_eq!(text, "web_fetch exceeded 2 redirects");
    assert_eq!(
        requested_urls(&host),
        [
            "http://1.1.1.1/start",
            "http://1.1.1.1/h1",
            "http://1.1.1.1/h2"
        ],
        "the initial request plus exactly two followed redirects"
    );
    Ok(())
}

#[tokio::test]
async fn a_chain_exactly_at_the_cap_still_lands() -> TestResult {
    let host = scripted([
        redirect("/h1"),
        redirect("/h2"),
        reply(200, "text/plain", b"done"),
    ]);
    let extension = web(WebConfig {
        max_redirects: 2,
        ..config(30)
    })?;
    let text = run_tool(
        &extension,
        &host,
        "web_fetch",
        r#"{"url":"http://1.1.1.1/start"}"#,
    )
    .await?;
    let result = parse(&text)?;
    assert_eq!(result["final_url"].as_str(), Some("http://1.1.1.1/h2"));
    assert_eq!(result["markdown"].as_str(), Some("done"));
    assert_eq!(requested_urls(&host).len(), 3);
    Ok(())
}

#[tokio::test]
async fn a_zero_redirect_cap_refuses_the_first_redirect() -> TestResult {
    let host = scripted([
        redirect("/elsewhere"),
        reply(200, "text/plain", b"unreached"),
    ]);
    let extension = web(WebConfig {
        max_redirects: 0,
        ..config(30)
    })?;
    let text = run_tool(
        &extension,
        &host,
        "web_fetch",
        r#"{"url":"http://1.1.1.1/start"}"#,
    )
    .await?;
    assert_eq!(text, "web_fetch exceeded 0 redirects");
    assert_eq!(requested_urls(&host).len(), 1);
    Ok(())
}

// --- the body cap -----------------------------------------------------

#[tokio::test]
async fn a_body_over_the_read_cap_is_cut_to_the_prefix_and_reports_the_bytes_read() -> TestResult {
    // The origin declares 5242880 bytes and sends 3145728: 2097152 of `a`,
    // then 1048576 of `b`. Only the prefix may be converted.
    let mut body = vec![b'a'; 2_097_152];
    body.extend(std::iter::repeat_n(b'b', 1_048_576));
    let mut oversized = reply(200, "text/plain", &body);
    oversized
        .headers
        .push(("Content-Length".into(), "5242880".into()));
    let host = scripted([oversized]);
    let extension = web(WebConfig {
        max_markdown_bytes: 4_194_304,
        ..config(30)
    })?;
    let text = run_tool(
        &extension,
        &host,
        "web_fetch",
        r#"{"url":"http://1.1.1.1/big"}"#,
    )
    .await?;
    let result = parse(&text)?;
    assert_eq!(result["raw_bytes"].as_u64(), Some(2_097_152));
    assert_eq!(result["truncated"].as_bool(), Some(false));
    let markdown = result["markdown"].as_str().unwrap_or_default();
    assert_eq!(markdown.len(), 2_097_152);
    assert!(
        markdown.bytes().all(|byte| byte == b'a'),
        "the unread tail must not reach the conversion"
    );
    Ok(())
}

#[tokio::test]
async fn a_requested_cap_below_the_ceiling_cuts_earlier() -> TestResult {
    let host = scripted([reply(200, "text/plain", &vec![b'x'; 5000])]);
    let extension = web(config(30))?;
    let text = run_tool(
        &extension,
        &host,
        "web_fetch",
        r#"{"url":"http://1.1.1.1/","max_bytes":2048}"#,
    )
    .await?;
    let result = parse(&text)?;
    assert_eq!(result["raw_bytes"].as_u64(), Some(2048));
    Ok(())
}

// --- private addresses ---------------------------------------------

#[tokio::test]
async fn private_targets_never_reach_the_net_service() -> TestResult {
    let extension = web(config(30))?;
    let cases = [
        ("http://127.0.0.1:8080/", "127.0.0.1"),
        ("http://[::1]/", "[::1]"),
        (
            "http://169.254.169.254/latest/meta-data/",
            "169.254.169.254",
        ),
        ("http://10.0.0.1/", "10.0.0.1"),
        ("http://172.16.0.1/", "172.16.0.1"),
        ("http://192.168.0.1/", "192.168.0.1"),
        ("http://0.0.0.0/", "0.0.0.0"),
        ("http://[fc00::1]/", "[fc00::1]"),
        ("http://[fe80::1]/", "[fe80::1]"),
        ("https://[::ffff:10.0.0.1]/", "[::ffff:a00:1]"),
        ("http://2130706433/", "127.0.0.1"),
        ("http://localhost:9/", "localhost"),
    ];
    for (target, host_name) in cases {
        let host = scripted([reply(200, "text/plain", b"secret")]);
        let args = format!(r#"{{"url":"{target}"}}"#);
        let text = run_tool(&extension, &host, "web_fetch", &args).await?;
        assert_eq!(
            text,
            format!("web_fetch: {host_name} is a private address; fetch public URLs only."),
            "{target}"
        );
        assert!(
            requested_urls(&host).is_empty(),
            "{target} reached the net service"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_live_loopback_listener_sees_no_connection() -> TestResult {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let host = Host::answering([]);
    *locked(&host.net) = Net::Live { base: None };
    let extension = web(config(30))?;
    let args = format!(r#"{{"url":"http://127.0.0.1:{port}/admin"}}"#);

    let (text, connected) = tokio::join!(
        run_tool(&extension, &host, "web_fetch", &args),
        tokio::time::timeout(Duration::from_millis(400), listener.accept()),
    );
    assert_eq!(
        text?,
        "web_fetch: 127.0.0.1 is a private address; fetch public URLs only."
    );
    assert!(
        connected.is_err(),
        "the loopback service must see no connection"
    );
    assert_eq!(requested_urls(&host), Vec::<String>::new());
    Ok(())
}

#[tokio::test]
async fn a_redirect_into_a_private_address_is_refused_before_it_is_requested() -> TestResult {
    let host = scripted([
        redirect("http://169.254.169.254/latest/meta-data/"),
        reply(200, "text/plain", b"iam-credentials"),
    ]);
    let extension = web(config(30))?;
    let text = run_tool(
        &extension,
        &host,
        "web_fetch",
        r#"{"url":"http://1.1.1.1/landing"}"#,
    )
    .await?;
    assert_eq!(
        text,
        "web_fetch: 169.254.169.254 is a private address; fetch public URLs only."
    );
    assert_eq!(requested_urls(&host), ["http://1.1.1.1/landing"]);
    Ok(())
}

#[tokio::test]
async fn a_global_literal_is_requested_unchanged_without_a_host_override() -> TestResult {
    let host = scripted([reply(200, "text/plain", b"ok")]);
    let extension = web(config(30))?;
    run_tool(
        &extension,
        &host,
        "web_fetch",
        r#"{"url":"http://1.1.1.1:8080/p?q=1"}"#,
    )
    .await?;
    let requests = locked(&host.net_requests);
    assert_eq!(requests[0].url.as_ref(), "http://1.1.1.1:8080/p?q=1");
    assert!(
        !requests[0]
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("Host"))
    );
    Ok(())
}

#[tokio::test]
async fn a_transport_failure_is_a_fetch_network_error() -> TestResult {
    // A closed port: the host answers status 0 when the connection fails.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    drop(listener);
    let host = Host::answering([]);
    *locked(&host.net) = Net::Live {
        base: Some(format!("http://127.0.0.1:{port}")),
    };
    let extension = web(config(30))?;
    let text = run_tool(
        &extension,
        &host,
        "web_fetch",
        r#"{"url":"http://1.1.1.1/page"}"#,
    )
    .await?;
    assert_eq!(
        text,
        "web_fetch failed: no response from http://1.1.1.1/page"
    );
    Ok(())
}

// --- search without a key, search to a closed port -----------

const NOT_CONFIGURED_BRAVE: &str =
    "web_search is not configured: set the BRAVE_API_KEY environment variable";

#[tokio::test]
async fn an_unset_key_env_reports_not_configured_with_zero_requests() -> TestResult {
    let host = Host::answering([]);
    let extension = web(config(30))?;
    let text = run_tool(&extension, &host, "web_search", r#"{"query":"parse rust"}"#).await?;
    assert_eq!(text, NOT_CONFIGURED_BRAVE);
    assert_eq!(requested_urls(&host), Vec::<String>::new());
    Ok(())
}

#[tokio::test]
async fn an_empty_key_env_reports_not_configured_too() -> TestResult {
    let host = Host::answering([]);
    locked(&host.env_vars).insert("BRAVE_API_KEY".to_owned(), String::new());
    let extension = web(config(30))?;
    let text = run_tool(&extension, &host, "web_search", r#"{"query":"parse rust"}"#).await?;
    assert_eq!(text, NOT_CONFIGURED_BRAVE);
    assert_eq!(requested_urls(&host), Vec::<String>::new());
    Ok(())
}

#[tokio::test]
async fn a_tavily_config_left_on_the_brave_key_name_reads_the_tavily_variable() -> TestResult {
    // The product defaults carry `api_key_env = "BRAVE_API_KEY"`; a user who
    // only sets `provider = "tavily"` must be sent to `TAVILY_API_KEY`.
    let host = Host::answering([]);
    let extension = web(WebConfig {
        provider: "tavily".into(),
        ..config(30)
    })?;
    let text = run_tool(&extension, &host, "web_search", r#"{"query":"parse rust"}"#).await?;
    assert_eq!(
        text,
        "web_search is not configured: set the TAVILY_API_KEY environment variable"
    );
    assert_eq!(requested_urls(&host), Vec::<String>::new());
    Ok(())
}

#[tokio::test]
async fn a_search_to_a_closed_port_is_a_network_error_without_the_key() -> TestResult {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    drop(listener);
    let host = Host::answering([]);
    locked(&host.env_vars).insert("BRAVE_API_KEY".to_owned(), "BSA-closed-port-key".to_owned());
    *locked(&host.net) = Net::Live {
        base: Some(format!("http://127.0.0.1:{port}")),
    };
    let extension = web(config(30))?;
    let text = run_tool(&extension, &host, "web_search", r#"{"query":"parse rust"}"#).await?;
    assert_eq!(
        text,
        "web_search failed: no response from https://api.search.brave.com/res/v1/web/search?q=parse+rust&count=5"
    );
    assert!(!text.contains("BSA-closed-port-key"));
    Ok(())
}

// --- the providers over a real socket ----------

const BRAVE_KEY: &str = "BSA-0123456789abcdefghij";
const TAVILY_KEY: &str = "tvly-0123456789abcdefghij";

/// Runs `web_search` against a loopback listener that answers once.
async fn search_through_listener(
    config: WebConfig,
    key_var: &str,
    key: &str,
    args: &str,
    status: u16,
    body: &[u8],
) -> Result<(String, Received), Box<dyn std::error::Error>> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let host = Host::answering([]);
    locked(&host.env_vars).insert(key_var.to_owned(), key.to_owned());
    *locked(&host.net) = Net::Live {
        base: Some(format!("http://127.0.0.1:{port}")),
    };
    let extension = web(config)?;
    let (text, received) = tokio::join!(
        run_tool(&extension, &host, "web_search", args),
        serve(&listener, status, "application/json", body),
    );
    Ok((text?, received))
}

#[tokio::test]
async fn brave_sends_a_get_with_the_subscription_token_and_hides_the_key() -> TestResult {
    let body = br#"{"web":{"results":[
        {"title":"Parse","url":"https://a.test/","description":"snippet a","age":"1 day","extra":{"x":1}},
        {"title":"No url"}
    ]},"query":{"original":"parse rust"},"future":true}"#;
    let (text, received) = search_through_listener(
        config(30),
        "BRAVE_API_KEY",
        BRAVE_KEY,
        r#"{"query":"parse rust","count":2}"#,
        200,
        body,
    )
    .await?;
    assert_eq!(received.method, "GET");
    assert_eq!(received.target, "/res/v1/web/search?q=parse+rust&count=2");
    assert_eq!(received.header("X-Subscription-Token"), Some(BRAVE_KEY));
    let result = parse(&text)?;
    assert_eq!(result["provider"].as_str(), Some("brave"));
    assert_eq!(
        result["results"].as_array().map(sonic_rs::Array::len),
        Some(1)
    );
    assert_eq!(result["results"][0]["snippet"].as_str(), Some("snippet a"));
    assert!(!text.contains("published_date"), "{text}");
    assert!(!text.contains(BRAVE_KEY));
    Ok(())
}

#[tokio::test]
async fn tavily_posts_the_exact_body_and_passes_the_published_date_through() -> TestResult {
    let body = br#"{"query":"parse rust","answer":null,"response_time":0.4,"results":[
        {"title":"Parse","url":"https://a.test/","content":"snippet a","score":0.93,"raw_content":null,"published_date":"2026-01-02","favicon":"x"},
        {"title":"Undated","url":"https://b.test/","content":"snippet b"},
        {"title":"No url","content":"dropped"}
    ],"request_id":"r-1","future":{"nested":[1]}}"#;
    let (text, received) = search_through_listener(
        WebConfig {
            search_depth: Some("advanced".into()),
            ..tavily_config()
        },
        "TAVILY_API_KEY",
        TAVILY_KEY,
        r#"{"query":"parse rust","count":2}"#,
        200,
        body,
    )
    .await?;
    assert_eq!(received.method, "POST");
    assert_eq!(received.target, "/search");
    assert_eq!(
        received.header("Authorization"),
        Some(format!("Bearer {TAVILY_KEY}").as_str())
    );
    assert_eq!(received.header("Content-Type"), Some("application/json"));
    let sent = parse(std::str::from_utf8(&received.body)?)?;
    let expected = parse(
        r#"{"query":"parse rust","max_results":2,"search_depth":"advanced","topic":"general","include_answer":false,"include_raw_content":false}"#,
    )?;
    assert_eq!(sent, expected);

    let result = parse(&text)?;
    assert_eq!(result["provider"].as_str(), Some("tavily"));
    let hits = result["results"].as_array().expect("results");
    assert_eq!(hits.len(), 2, "the hit without a url is dropped");
    assert_eq!(hits[0]["title"].as_str(), Some("Parse"));
    assert_eq!(hits[0]["snippet"].as_str(), Some("snippet a"));
    assert_eq!(hits[0]["published_date"].as_str(), Some("2026-01-02"));
    assert!(hits[1].get("published_date").is_none(), "{text}");
    for unknown in [
        "score",
        "raw_content",
        "favicon",
        "request_id",
        "answer",
        "future",
    ] {
        assert!(!text.contains(unknown), "{unknown} must be dropped: {text}");
    }
    assert!(!text.contains(TAVILY_KEY));
    Ok(())
}

#[tokio::test]
async fn tavily_defaults_to_the_basic_search_depth() -> TestResult {
    let (_, received) = search_through_listener(
        tavily_config(),
        "TAVILY_API_KEY",
        TAVILY_KEY,
        r#"{"query":"q"}"#,
        200,
        br#"{"results":[]}"#,
    )
    .await?;
    let sent = parse(std::str::from_utf8(&received.body)?)?;
    assert_eq!(sent["search_depth"].as_str(), Some("basic"));
    assert_eq!(sent["max_results"].as_u64(), Some(5));
    Ok(())
}

#[tokio::test]
async fn a_provider_that_echoes_the_key_never_reaches_the_model() -> TestResult {
    let body = format!(
        r#"{{"results":[{{"title":"{TAVILY_KEY}","url":"https://a.test/?k={TAVILY_KEY}","content":"Authorization: Bearer {TAVILY_KEY}"}}]}}"#
    );
    let (text, _) = search_through_listener(
        tavily_config(),
        "TAVILY_API_KEY",
        TAVILY_KEY,
        r#"{"query":"q"}"#,
        200,
        body.as_bytes(),
    )
    .await?;
    assert!(!text.contains(TAVILY_KEY), "{text}");
    assert!(text.contains("Bearer ***"), "{text}");
    Ok(())
}

#[tokio::test]
async fn a_failure_cause_that_carries_the_key_is_scrubbed() -> TestResult {
    let host = Host::answering([]);
    locked(&host.env_vars).insert("TAVILY_API_KEY".to_owned(), TAVILY_KEY.to_owned());
    *locked(&host.net) = Net::Fail {
        cause: format!("proxy refused the request: Authorization: Bearer {TAVILY_KEY}"),
    };
    let extension = web(tavily_config())?;
    let text = run_tool(&extension, &host, "web_search", r#"{"query":"q"}"#).await?;
    assert!(text.starts_with("web_search failed: "), "{text}");
    assert!(text.contains("Bearer ***"), "{text}");
    assert!(!text.contains(TAVILY_KEY), "{text}");
    Ok(())
}

#[tokio::test]
async fn a_rate_limited_search_keeps_its_exact_status_form() -> TestResult {
    let host = scripted([reply(429, "application/json", br#"{"error":"slow down"}"#)]);
    locked(&host.env_vars).insert("BRAVE_API_KEY".to_owned(), BRAVE_KEY.to_owned());
    let extension = web(config(30))?;
    let text = run_tool(&extension, &host, "web_search", r#"{"query":"parse rust"}"#).await?;
    assert_eq!(
        text,
        "web_search got HTTP 429 for https://api.search.brave.com/res/v1/web/search?q=parse+rust&count=5"
    );
    Ok(())
}
