#![expect(
    clippy::disallowed_methods,
    reason = "tests bind loopback servers and spawn concurrent flight tasks"
)]
//! Duo Chat proxy tests: routes, grants, bodies, and the proxy lifecycle
//! against a loopback server.

use dal_core::{
    AssistantPart, ContextItem, Family, ModelRequest, ModelRoute, Part, Purpose, RawJson,
    ReplaySource, RequestParams, SessionId, ThinkingLevel,
};
use futures::executor::block_on;
use sonic_rs::{JsonContainerTrait, JsonValueTrait};

use super::*;
use crate::{
    auth::credential::{Credential, OAuthCredential, SecretString},
    claude_fingerprint::{CLAUDE_CODE_BETA, CLAUDE_CODE_SYSTEM_INSTRUCTION, OAUTH_BETA},
    error::ProviderError,
    sse::decode_stream,
    stream::StopReason,
    thinking::{AnthropicThinking, ThinkingPlan, WireThinking},
};

fn raw(text: &str) -> RawJson {
    RawJson::parse(text).unwrap()
}

fn request(model: &str, context: Vec<ContextItem>) -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::GitLab,
            model: model.into(),
        },
        system: "Be terse.".into(),
        tools: vec![dal_core::ModelToolSpec {
            name: "read".into(),
            description: "reads".into(),
            parameters: raw(r#"{"type":"object"}"#),
            grammar: None,
            defer_loading: false,
        }]
        .into(),
        context: context.into(),
        params: RequestParams {
            thinking: ThinkingLevel::Medium,
            effort: None,
            temperature: None,
            max_output_tokens: None,
        },
        cache_key: None,
    }
}

fn user(text: &str) -> ContextItem {
    ContextItem::User {
        parts: vec![Part::Text { text: text.into() }],
    }
}

fn open_plan() -> ThinkingPlan {
    ThinkingPlan {
        level: ThinkingLevel::Medium,
        wire: WireThinking::OpenAi {
            effort: Some("medium"),
        },
        temperature: None,
        notices: Vec::new(),
    }
}

fn enabled_plan() -> ThinkingPlan {
    ThinkingPlan {
        level: ThinkingLevel::Medium,
        wire: WireThinking::Anthropic {
            thinking: AnthropicThinking::Enabled {
                budget_tokens: 2048,
                max_tokens: 8192,
            },
            effort: None,
        },
        temperature: None,
        notices: Vec::new(),
    }
}

fn grant() -> DirectAccess {
    DirectAccess {
        token: "da-token".into(),
        headers: vec![(String::from("x-test-gateway"), String::from("1"))],
    }
}

fn proxy_request<'a>(
    request: &'a ModelRequest,
    thinking: &'a ThinkingPlan,
    grant: &'a DirectAccess,
    session: SessionId,
) -> DuoProxyRequest<'a> {
    DuoProxyRequest {
        request,
        thinking,
        grant,
        max_output: Some(16_000),
        reasoning_summary: false,
        display_supported: false,
        session,
    }
}

#[test]
fn route_table_maps_aliases_prefixes_and_default() {
    assert_eq!(IDENTITIES.len(), 10);
    assert_eq!(upstream_model("duo-chat-opus-4-6"), "claude-opus-4-6");
    assert_eq!(upstream_model("duo-chat-gpt-5-codex"), "gpt-5-codex");
    assert_eq!(upstream_model("custom"), "custom");
    assert_eq!(proxy_route("duo-chat-opus-4-6"), ProxyRoute::Anthropic);
    assert_eq!(
        proxy_route("claude-sonnet-4-5-20250929"),
        ProxyRoute::Anthropic
    );
    assert_eq!(proxy_route("duo-chat-gpt-5-codex"), ProxyRoute::Responses);
    assert_eq!(proxy_route("gpt-5.2-codex"), ProxyRoute::Responses);
    assert_eq!(proxy_route("duo-chat-gpt-5-1"), ProxyRoute::Completions);
    assert_eq!(proxy_route("duo-chat-gpt-5-2"), ProxyRoute::Completions);
}

#[test]
fn anthropic_route_uses_the_oauth_wire_shape() {
    let request = request("duo-chat-sonnet-4-6", vec![user("hi")]);
    let thinking = enabled_plan();
    let grant = grant();
    let input = proxy_request(&request, &thinking, &grant, SessionId::new_v7());
    let wire = build_proxy(&input).unwrap();
    assert_eq!(wire.route, ProxyRoute::Anthropic);
    assert_eq!(
        wire.headers[0],
        ("authorization", String::from("Bearer da-token"))
    );
    assert_eq!(
        wire.headers
            .iter()
            .filter(|(name, _)| *name == "authorization")
            .count(),
        1,
        "the grant bearer is the only authorization header"
    );
    assert!(!wire.headers.iter().any(|(name, _)| *name == "x-api-key"));
    let betas = wire
        .headers
        .iter()
        .find(|(name, _)| *name == "anthropic-beta")
        .map(|(_, value)| value.as_str())
        .unwrap();
    assert!(betas.contains(CLAUDE_CODE_BETA) && betas.contains(OAUTH_BETA));
    assert!(
        wire.user_agent
            .as_deref()
            .unwrap_or_default()
            .starts_with("claude-cli/")
    );
    assert!(wire.replay_prefix.is_some());
    assert!(
        wire.grant_headers
            .iter()
            .any(|(name, _)| name == "x-test-gateway")
    );
    let body: sonic_rs::Value = sonic_rs::from_slice(&wire.body).unwrap();
    assert_eq!(body["model"].as_str().unwrap(), "claude-sonnet-4-6");
    // The OAuth wire shape leads with the Claude Code identity; the request's
    // own system text follows it.
    assert_eq!(
        body["system"][0]["text"].as_str().unwrap(),
        CLAUDE_CODE_SYSTEM_INSTRUCTION
    );
    assert_eq!(body["system"][1]["text"].as_str().unwrap(), "Be terse.");
    assert_eq!(body["tools"][0]["name"].as_str().unwrap(), "_read");
}

#[test]
fn responses_route_keys_cache_to_the_session() {
    let request = request("duo-chat-gpt-5-codex", vec![user("hi")]);
    let thinking = open_plan();
    let session = SessionId::new_v7();
    let grant = grant();
    let input = proxy_request(&request, &thinking, &grant, session);
    let wire = build_proxy(&input).unwrap();
    assert_eq!(wire.route, ProxyRoute::Responses);
    assert!(wire.user_agent.is_none());
    let body: sonic_rs::Value = sonic_rs::from_slice(&wire.body).unwrap();
    assert_eq!(body["model"].as_str().unwrap(), "gpt-5-codex");
    assert_eq!(
        body["prompt_cache_key"].as_str().unwrap(),
        session.to_string()
    );
}

#[test]
fn completions_route_rejects_foreign_thinking_and_routes() {
    let request = request("duo-chat-gpt-5-1", vec![user("hi")]);
    let thinking = open_plan();
    let grant = grant();
    let input = proxy_request(&request, &thinking, &grant, SessionId::new_v7());
    let wire = build_proxy(&input).unwrap();
    assert_eq!(wire.route, ProxyRoute::Completions);
    let body: sonic_rs::Value = sonic_rs::from_slice(&wire.body).unwrap();
    // Every route sends the identity's upstream id, never the alias.
    assert_eq!(body["model"].as_str().unwrap(), "gpt-5.1-2025-11-13");
    let anthropic_thinking = ThinkingPlan {
        level: ThinkingLevel::Medium,
        wire: WireThinking::Anthropic {
            thinking: AnthropicThinking::Disabled,
            effort: None,
        },
        temperature: None,
        notices: Vec::new(),
    };
    let input = proxy_request(&request, &anthropic_thinking, &grant, SessionId::new_v7());
    assert!(build_proxy(&input).is_err());
}

/// An assistant message the journal recorded for the alias route: the source
/// is `Family::GitLab` with the alias, while the stored block is the
/// encoder's own payload.
fn journaled(alias: &str, replay: &str) -> ContextItem {
    ContextItem::Assistant {
        source: ReplaySource {
            family: Family::GitLab,
            model: alias.into(),
        },
        parts: vec![AssistantPart::Thinking {
            text: "plan".into(),
            replay: Some(raw(replay)),
        }],
        replay: None,
    }
}

#[test]
fn gitlab_assistant_replays_on_its_route() {
    let thinking = enabled_plan();
    let grant = grant();
    // The prefix binding a producing request would have recorded.
    let producer = request("duo-chat-sonnet-4-6", vec![user("hi")]);
    let prefix = build_proxy(&proxy_request(
        &producer,
        &thinking,
        &grant,
        SessionId::new_v7(),
    ))
    .unwrap()
    .replay_prefix
    .unwrap();
    let block = |signature: &str| {
        format!(
            r#"{{"type":"thinking","thinking":"plan","signature":"{signature}","dal_prefix":"{prefix}"}}"#
        )
    };
    let request = request(
        "duo-chat-sonnet-4-6",
        vec![
            user("hi"),
            journaled("duo-chat-sonnet-4-6", &block("sig-2")),
            user("again"),
            journaled("duo-chat-opus-4-6", &block("sig-other")),
            user("more"),
        ],
    );
    let input = proxy_request(&request, &thinking, &grant, SessionId::new_v7());
    let wire = build_proxy(&input).unwrap();
    let text = String::from_utf8(wire.body).unwrap();
    let body: sonic_rs::Value = sonic_rs::from_str(&text).unwrap();
    // The block produced under this alias replays, rebuilt without its
    // storage-only binding; a block another model signed never does.
    assert_eq!(
        body["messages"][1]["content"][0]["signature"].as_str(),
        Some("sig-2")
    );
    assert!(!text.contains("sig-other"));
    assert!(!text.contains("dal_prefix"));
}

#[test]
fn gitlab_assistant_replays_on_the_responses_route() {
    let thinking = open_plan();
    let grant = grant();
    let reasoning = |id: &str| format!(r#"{{"type":"reasoning","id":"{id}","summary":[]}}"#);
    let request = request(
        "duo-chat-gpt-5-codex",
        vec![
            user("hi"),
            journaled("duo-chat-gpt-5-codex", &reasoning("rs-1")),
            user("again"),
            journaled("duo-chat-gpt-5-2-codex", &reasoning("rs-other")),
            user("more"),
        ],
    );
    let input = proxy_request(&request, &thinking, &grant, SessionId::new_v7());
    let wire = build_proxy(&input).unwrap();
    let text = String::from_utf8(wire.body).unwrap();
    let body: sonic_rs::Value = sonic_rs::from_str(&text).unwrap();
    assert_eq!(body["model"].as_str(), Some("gpt-5-codex"));
    assert_eq!(body["input"][1]["id"].as_str(), Some("rs-1"));
    assert!(!text.contains("rs-other"));
}

#[test]
fn grant_parsing_validates_token_and_headers() {
    let body = br#"{"token":"t","headers":{"x-a":"b"}}"#;
    let grant = parse_direct_access(body).unwrap();
    assert_eq!(grant.token.as_ref(), "t");
    assert_eq!(
        grant.headers,
        vec![(String::from("x-a"), String::from("b"))]
    );
    assert!(parse_direct_access(br#"{"headers":{}}"#).is_err());
    assert!(parse_direct_access(br#"{"token":"t","headers":{"x-a":1}}"#).is_err());
    assert!(parse_direct_access(br"not json").is_err());
}

#[test]
fn token_gate_accepts_keys_and_sign_ins() {
    let key = Credential::ApiKey {
        key: SecretString::from("gl-pat"),
    };
    assert_eq!(require_token(&key).unwrap(), "gl-pat");
    let oauth = Credential::OAuth(OAuthCredential {
        access_token: SecretString::from("tok"),
        refresh_token: SecretString::from(""),
        expires_at: None,
        id_token: None,
        account_id: None,
    });
    assert_eq!(require_token(&oauth).unwrap(), "tok");
    assert!(require_token(&Credential::None).is_err());
}

fn sse(datas: &[&str]) -> String {
    use std::fmt::Write as _;
    datas.iter().fold(String::new(), |mut wire, data| {
        let _ = write!(wire, "data: {data}\n\n");
        wire
    })
}

async fn serve_once(
    listener: tokio::net::TcpListener,
    status: &str,
    body: Vec<u8>,
) -> (String, Vec<u8>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (mut tcp, _) = listener.accept().await.unwrap();
    let mut head = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        AsyncReadExt::read_exact(&mut tcp, &mut byte).await.unwrap();
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let head_text = String::from_utf8_lossy(&head).into_owned();
    let len = head_text
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if !name.trim().eq_ignore_ascii_case("content-length") {
                return None;
            }
            value.trim().parse::<usize>().ok()
        })
        .unwrap_or(0);
    let mut request_body = vec![0u8; len];
    AsyncReadExt::read_exact(&mut tcp, &mut request_body)
        .await
        .unwrap();
    let reply = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    AsyncWriteExt::write_all(&mut tcp, reply.as_bytes())
        .await
        .unwrap();
    AsyncWriteExt::write_all(&mut tcp, &body).await.unwrap();
    (head_text, request_body)
}

#[tokio::test]
async fn direct_access_cache_reuses_its_grant() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let cache = DirectAccessCache::default();
    let client = crate::http::build_client();
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(serve_once(
        listener,
        "200 OK",
        br#"{"token":"t-1","headers":{"x-a":"b"}}"#.to_vec(),
    ));
    let first = cache
        .ensure(&client, &base, "gl-pat", "dalgon/test")
        .await
        .unwrap();
    assert_eq!(first.token.as_ref(), "t-1");
    let second = cache
        .ensure(&client, &base, "gl-pat", "dalgon/test")
        .await
        .unwrap();
    assert_eq!(second.token.as_ref(), "t-1");
    servers.abort_all();
}

#[tokio::test]
async fn direct_access_denied_names_the_fix() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let cache = DirectAccessCache::default();
    let client = crate::http::build_client();
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(serve_once(listener, "403 Forbidden", b"{}".to_vec()));
    let error = cache
        .ensure(&client, &base, "gl-pat", "dalgon/test")
        .await
        .err()
        .expect("denied direct access should return an error");
    assert!(matches!(error, ProviderError::Status { status: 403, .. }));
    let text = error.to_string();
    assert!(text.contains("access denied"));
    servers.abort_all();
}

#[tokio::test]
async fn proxy_open_posts_grant_and_decodes_chat() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let response = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1}}\n\n";
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(serve_once(listener, "200 OK", response.as_bytes().to_vec()));
    let request = request("duo-chat-gpt-5-1", vec![user("hi")]);
    let thinking = open_plan();
    let grant = grant();
    let input = proxy_request(&request, &thinking, &grant, SessionId::new_v7());
    let mut wire = build_proxy(&input).unwrap();
    // Point the proxy at the loopback server; the test asserts the bearer
    // and gateway headers ride the request.
    wire.url = crate::http::endpoint(Family::GitLab, &base, "chat/completions").unwrap();
    let mut stream = open(&crate::http::build_client(), wire, "dalgon/test")
        .await
        .unwrap();
    let mut saw_text = false;
    let mut saw_stop = false;
    while let Some(item) = stream.next().await {
        match item.unwrap() {
            StreamEvent::TextDelta { text } => saw_text = saw_text || text == "hi",
            StreamEvent::Stop { reason } => {
                saw_stop = reason == StopReason::EndTurn;
            }
            _ => {}
        }
    }
    assert!(saw_text && saw_stop);
    let (head, _) = servers.join_next().await.unwrap().unwrap();
    assert!(head.contains("authorization: Bearer da-token"));
    assert!(head.contains("x-test-gateway: 1"));
    servers.abort_all();
}

#[test]
fn responses_wire_decodes_through_the_shared_decoder() {
    let wire = sse(&[
        r#"{"type":"response.output_text.delta","delta":"hi"}"#,
        r#"{"type":"response.completed","response":{"output":[],"usage":null}}"#,
    ]);
    let chunks: Vec<Vec<u8>> = wire.as_bytes().chunks(7).map(<[u8]>::to_vec).collect();
    let decoded = super::decode_proxy(
        ProxyRoute::Responses,
        decode_stream(futures::stream::iter(chunks)),
        Box::from("duo-chat-gpt-5-codex"),
        None,
    );
    let items: Vec<_> = block_on(decoded.collect());
    assert!(items.iter().any(|item| matches!(
        item,
        Ok(StreamEvent::TextDelta { text }) if text == "hi"
    )));
}

/// One 200 JSON reply carrying a `direct_access` answer.
fn grant_reply(body: &[u8]) -> Vec<u8> {
    let mut reply = format!(
        concat!(
            "HTTP/1.1 200 OK\r\n",
            "content-type: application/json\r\n",
            "content-length: {}\r\n",
            "connection: close\r\n\r\n",
        ),
        body.len()
    )
    .into_bytes();
    reply.extend_from_slice(body);
    reply
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "direct access fixture tests first-fetch invalidation race in place"
)]
async fn direct_access_first_fetch_invalidated_during_flight_fails_closed() {
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let cache = Arc::new(DirectAccessCache::default());
    let client = crate::http::build_client();

    // No prior grant: the fetch below is the slot's first. The server reads
    // the request, invalidates the token while the fetch is still in flight,
    // and only then answers 200: the stale grant must never be served.
    let flight = {
        let cache = Arc::clone(&cache);
        let client = client.clone();
        let base = base.clone();
        tokio::spawn(async move { cache.ensure(&client, &base, "gl-pat", "dalgon/test").await })
    };
    let (mut tcp, _) = listener.accept().await.unwrap();
    let mut head = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        AsyncReadExt::read_exact(&mut tcp, &mut byte).await.unwrap();
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let head_text = String::from_utf8_lossy(&head).into_owned();
    let len = head_text
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if !name.trim().eq_ignore_ascii_case("content-length") {
                return None;
            }
            value.trim().parse::<usize>().ok()
        })
        .unwrap_or(0);
    let mut request_body = vec![0u8; len];
    AsyncReadExt::read_exact(&mut tcp, &mut request_body)
        .await
        .unwrap();
    cache.remove("gl-pat");
    let stale = br#"{"token":"stale","headers":{"x-a":"b"}}"#;
    AsyncWriteExt::write_all(&mut tcp, &grant_reply(stale))
        .await
        .unwrap();
    drop(tcp);

    let error = flight
        .await
        .unwrap()
        .err()
        .expect("an invalidated first fetch must fail");
    assert!(
        matches!(
            &error,
            ProviderError::Transport { family: Family::GitLab, reason }
                if reason == "gitlab grant invalidated during fetch"
        ),
        "{error:?}"
    );
    for text in [format!("{error}"), format!("{error:?}")] {
        assert!(!text.contains("gl-pat"), "{text:?} leaks the token");
        assert!(!text.contains("stale"), "{text:?} leaks the grant");
    }

    // The stale grant was never cached: the next call on the same base
    // refetches fresh. The server task proves the wire was hit; a stale
    // cache hit would leave it pending and trip the timeout instead.
    let server = tokio::spawn(async move {
        let (mut tcp, _) = listener.accept().await.unwrap();
        let mut head = Vec::new();
        loop {
            let mut byte = [0u8; 1];
            AsyncReadExt::read_exact(&mut tcp, &mut byte).await.unwrap();
            head.push(byte[0]);
            if head.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let head_text = String::from_utf8_lossy(&head).into_owned();
        let len = head_text
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                if !name.trim().eq_ignore_ascii_case("content-length") {
                    return None;
                }
                value.trim().parse::<usize>().ok()
            })
            .unwrap_or(0);
        let mut request_body = vec![0u8; len];
        AsyncReadExt::read_exact(&mut tcp, &mut request_body)
            .await
            .unwrap();
        let fresh = br#"{"token":"fresh","headers":{"x-a":"b"}}"#;
        AsyncWriteExt::write_all(&mut tcp, &grant_reply(fresh))
            .await
            .unwrap();
    });
    let grant = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        cache.ensure(&client, &base, "gl-pat", "dalgon/test"),
    )
    .await
    .expect("the refetch answers")
    .expect("the refetch succeeds");
    assert_eq!(grant.token.as_ref(), "fresh");
    tokio::time::timeout(std::time::Duration::from_secs(10), server)
        .await
        .expect("the refetch hit the wire")
        .unwrap();
}

#[tokio::test]
async fn direct_access_cache_isolates_case_sensitive_bases_and_shares_trailing_slash() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let lower = format!("http://127.0.0.1:{port}/gitlab");
    let upper = format!("http://127.0.0.1:{port}/GitLab");
    let slashed = format!("{lower}/");
    let cache = DirectAccessCache::default();
    let client = crate::http::build_client();
    let hits = Arc::new(AtomicUsize::new(0));
    let server = {
        let hits = Arc::clone(&hits);
        tokio::spawn(async move {
            loop {
                let (mut tcp, _) = listener.accept().await.unwrap();
                let mut head = Vec::new();
                loop {
                    let mut byte = [0u8; 1];
                    if AsyncReadExt::read_exact(&mut tcp, &mut byte).await.is_err() {
                        break;
                    }
                    head.push(byte[0]);
                    if head.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                let head_text = String::from_utf8_lossy(&head).into_owned();
                let len = head_text
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        if !name.trim().eq_ignore_ascii_case("content-length") {
                            return None;
                        }
                        value.trim().parse::<usize>().ok()
                    })
                    .unwrap_or(0);
                let mut body = vec![0u8; len];
                let _ = AsyncReadExt::read_exact(&mut tcp, &mut body).await;
                hits.fetch_add(1, Ordering::SeqCst);
                let grant = br#"{"token":"t","headers":{"x-a":"b"}}"#;
                let _ = AsyncWriteExt::write_all(&mut tcp, &grant_reply(grant)).await;
            }
        })
    };

    cache
        .ensure(&client, &lower, "gl-pat", "dalgon/test")
        .await
        .unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    // Path case differs: a distinct key, so the wire is hit again. The base
    // is never lowercased: path segments are case-sensitive.
    cache
        .ensure(&client, &upper, "gl-pat", "dalgon/test")
        .await
        .unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    // Both spellings are cached now.
    cache
        .ensure(&client, &lower, "gl-pat", "dalgon/test")
        .await
        .unwrap();
    cache
        .ensure(&client, &upper, "gl-pat", "dalgon/test")
        .await
        .unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    // A trailing slash is the same configured base: shared, no new fetch.
    cache
        .ensure(&client, &slashed, "gl-pat", "dalgon/test")
        .await
        .unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    // One remove kills every base: both spellings refetch.
    cache.remove("gl-pat");
    cache
        .ensure(&client, &lower, "gl-pat", "dalgon/test")
        .await
        .unwrap();
    cache
        .ensure(&client, &upper, "gl-pat", "dalgon/test")
        .await
        .unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 4);
    server.abort();
}

#[tokio::test]
async fn clear_expired_keeps_active_in_flight_epochs() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let cache = Arc::new(DirectAccessCache::default());
    let client = crate::http::build_client();
    let hits = Arc::new(AtomicUsize::new(0));
    let kept = Arc::new(AtomicUsize::new(0));
    let flight = {
        let cache = Arc::clone(&cache);
        let client = client.clone();
        let base = base.clone();
        tokio::spawn(async move { cache.ensure(&client, &base, "gl-pat", "dalgon/test").await })
    };
    let server = {
        let hits = Arc::clone(&hits);
        let kept = Arc::clone(&kept);
        let cache = Arc::clone(&cache);
        tokio::spawn(async move {
            loop {
                let (mut tcp, _) = listener.accept().await.unwrap();
                let mut head = Vec::new();
                loop {
                    let mut byte = [0u8; 1];
                    if AsyncReadExt::read_exact(&mut tcp, &mut byte).await.is_err() {
                        break;
                    }
                    head.push(byte[0]);
                    if head.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                let head_text = String::from_utf8_lossy(&head).into_owned();
                let len = head_text
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        if !name.trim().eq_ignore_ascii_case("content-length") {
                            return None;
                        }
                        value.trim().parse::<usize>().ok()
                    })
                    .unwrap_or(0);
                let mut body = vec![0u8; len];
                let _ = AsyncReadExt::read_exact(&mut tcp, &mut body).await;
                if hits.fetch_add(1, Ordering::SeqCst) == 0 {
                    // First request: the fetch is in flight and its slot is
                    // empty. Pruning now must keep the active epoch.
                    cache.clear_expired();
                    kept.store(cache.slot_count(), Ordering::SeqCst);
                }
                let grant = br#"{"token":"t","headers":{"x-a":"b"}}"#;
                let _ = AsyncWriteExt::write_all(&mut tcp, &grant_reply(grant)).await;
            }
        })
    };
    let grant = flight
        .await
        .unwrap()
        .expect("prune must not kill a live flight");
    assert_eq!(grant.token.as_ref(), "t");
    assert_eq!(kept.load(Ordering::SeqCst), 1);
    cache
        .ensure(&client, &base, "gl-pat", "dalgon/test")
        .await
        .unwrap();
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "the live flight published; no refetch"
    );
    server.abort();
}

#[tokio::test]
async fn clear_expired_reaps_inactive_empty_and_expired_slots() {
    use std::time::{Duration, Instant};

    let cache = DirectAccessCache::default();
    // A failed fetch leaves an empty slot with no live fetcher.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let client = crate::http::build_client();
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(serve_once(
        listener,
        "500 Internal Server Error",
        b"{}".to_vec(),
    ));
    let Err(error) = cache.ensure(&client, &base, "gl-pat", "dalgon/test").await else {
        panic!("a 500 fails the fetch");
    };
    assert!(
        matches!(&error, ProviderError::Status { status: 500, .. }),
        "{error:?}"
    );
    assert_eq!(cache.slot_count(), 1);
    cache.clear_expired();
    assert_eq!(cache.slot_count(), 0, "dead empty slots are reaped");
    servers.abort_all();

    // An expired grant with no live fetcher is reaped too, while a fresh
    // grant survives pruning.
    let key = cache_key(&base, "gl-pat");
    lock(&cache.state).slots.insert(
        key.clone(),
        TokenSlot {
            epoch: std::sync::Arc::new(TokenEpoch),
            grant: Some(CachedGrant {
                grant: DirectAccess {
                    token: "t".into(),
                    headers: Vec::new(),
                },
                expires: Instant::now().checked_sub(Duration::from_secs(1)).unwrap(),
            }),
        },
    );
    assert_eq!(cache.slot_count(), 1);
    cache.clear_expired();
    assert_eq!(
        cache.slot_count(),
        0,
        "expired grants with no live fetcher are reaped"
    );
    lock(&cache.state).slots.insert(
        key,
        TokenSlot {
            epoch: std::sync::Arc::new(TokenEpoch),
            grant: Some(CachedGrant {
                grant: DirectAccess {
                    token: "t".into(),
                    headers: Vec::new(),
                },
                expires: Instant::now() + Duration::from_secs(60),
            }),
        },
    );
    cache.clear_expired();
    assert_eq!(cache.slot_count(), 1, "fresh grants survive pruning");
}

#[tokio::test]
async fn entry_prune_collects_dead_slots_left_by_failed_fetches() {
    let cache = DirectAccessCache::default();
    // A dead empty slot left by an earlier failed fetch (never served).
    lock(&cache.state).slots.insert(
        cache_key("http://127.0.0.1:9/stale", "old-pat"),
        TokenSlot {
            epoch: std::sync::Arc::new(TokenEpoch),
            grant: None,
        },
    );
    assert_eq!(cache.slot_count(), 1);
    // The next ensure prunes on entry: the dead slot goes, and only its own
    // failed slot remains instead of accumulating.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let client = crate::http::build_client();
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(serve_once(
        listener,
        "500 Internal Server Error",
        b"{}".to_vec(),
    ));
    let Err(error) = cache.ensure(&client, &base, "gl-pat", "dalgon/test").await else {
        panic!("a 500 fails the fetch");
    };
    assert!(
        matches!(&error, ProviderError::Status { status: 500, .. }),
        "{error:?}"
    );
    assert_eq!(cache.slot_count(), 1);
    servers.abort_all();
}

#[test]
fn grant_fresh_splits_on_the_ttl_edge() {
    let grant = |expires: Instant| CachedGrant {
        grant: DirectAccess {
            token: "t".into(),
            headers: Vec::new(),
        },
        expires,
    };
    assert!(grant_fresh(&grant(
        Instant::now() + Duration::from_secs(60)
    )));
    assert!(!grant_fresh(&grant(
        Instant::now().checked_sub(Duration::from_secs(1)).unwrap()
    )));
}

#[tokio::test]
async fn direct_access_expired_grant_reexchanges() {
    use std::sync::Arc;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let cache = DirectAccessCache::default();
    // A stale grant for this base+token: the entry prune clears it and the
    // reuse gate would refuse it, so the next ensure must fetch instead of
    // serving it.
    lock(&cache.state).slots.insert(
        cache_key(&base, "gl-pat"),
        TokenSlot {
            epoch: Arc::new(TokenEpoch),
            grant: Some(CachedGrant {
                grant: DirectAccess {
                    token: "stale".into(),
                    headers: Vec::new(),
                },
                expires: Instant::now().checked_sub(Duration::from_secs(1)).unwrap(),
            }),
        },
    );
    let client = crate::http::build_client();
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(serve_once(
        listener,
        "200 OK",
        br#"{"token":"t-2","headers":{"x-a":"b"}}"#.to_vec(),
    ));
    let grant = cache
        .ensure(&client, &base, "gl-pat", "dalgon/test")
        .await
        .unwrap();
    assert_eq!(grant.token.as_ref(), "t-2");
    // The published grant carries the 25-minute TTL, not a stale or
    // zeroed expiry.
    let expires = lock(&cache.state)
        .slots
        .get(&cache_key(&base, "gl-pat"))
        .expect("re-fetched grant publishes")
        .grant
        .as_ref()
        .expect("published grant is present")
        .expires;
    let remaining = expires
        .checked_duration_since(Instant::now())
        .expect("published grant is fresh");
    assert!(
        remaining > Duration::from_mins(24),
        "TTL is minutes, not seconds: {remaining:?}"
    );
    assert!(
        remaining <= Duration::from_mins(25) + Duration::from_secs(5),
        "TTL is the pinned 25 minutes: {remaining:?}"
    );
    servers.abort_all();
    // The re-fetched grant publishes: a second ensure serves it with no
    // further fetch (the server is gone).
    let again = cache
        .ensure(&client, &base, "gl-pat", "dalgon/test")
        .await
        .unwrap();
    assert_eq!(again.token.as_ref(), "t-2");
}

#[test]
fn anthropic_replay_round_trips_through_decode_and_build() {
    let thinking = enabled_plan();
    let grant = grant();
    let producer = request("duo-chat-sonnet-4-6", vec![user("hi")]);
    let prefix = build_proxy(&proxy_request(
        &producer,
        &thinking,
        &grant,
        SessionId::new_v7(),
    ))
    .unwrap()
    .replay_prefix
    .unwrap();
    // Decode a signed thinking turn the way open() does, then rebind it to
    // the journal family.
    let wire = sse(&[
        r#"{"type":"message_start","message":{"usage":{"input_tokens":25,"output_tokens":1}}}"#,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"why"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"EqQBCgIYAhIM"}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null}}"#,
        r#"{"type":"message_stop"}"#,
    ]);
    let chunks: Vec<Vec<u8>> = wire.as_bytes().chunks(7).map(<[u8]>::to_vec).collect();
    let decoded = block_on(
        decode_proxy(
            ProxyRoute::Anthropic,
            decode_stream(futures::stream::iter(chunks)),
            Box::from("duo-chat-sonnet-4-6"),
            Some(&prefix),
        )
        .collect::<Vec<_>>(),
    );
    let stored = decoded
        .into_iter()
        .map(|item| rebind_replay(item.unwrap()))
        .find_map(|event| match event {
            StreamEvent::Replay { payload } => Some(payload),
            _ => None,
        })
        .expect("the signed thinking block replays");
    assert_eq!(stored.family, Family::GitLab);
    // Feeding the emitted payload back as the journal entry sends the
    // signature on the next wire without its storage-only binding.
    let request = request(
        "duo-chat-sonnet-4-6",
        vec![
            user("hi"),
            journaled("duo-chat-sonnet-4-6", stored.item.as_str()),
            user("again"),
        ],
    );
    let input = proxy_request(&request, &thinking, &grant, SessionId::new_v7());
    let built = build_proxy(&input).unwrap();
    let text = String::from_utf8(built.body).unwrap();
    let body: sonic_rs::Value = sonic_rs::from_str(&text).unwrap();
    assert_eq!(
        body["messages"][1]["content"][0]["signature"].as_str(),
        Some("EqQBCgIYAhIM")
    );
    assert!(!text.contains("dal_prefix"));
}

#[test]
fn completions_route_drops_thinking_replay_without_leaking_signatures() {
    let thinking = open_plan();
    let grant = grant();
    // The chat encoder carries no replay channel: a journaled thinking block
    // renders nothing, and its storage-only binding never reaches the wire.
    let request = request(
        "duo-chat-gpt-5-1",
        vec![
            user("hi"),
            journaled(
                "duo-chat-gpt-5-1",
                r#"{"type":"thinking","thinking":"plan","signature":"sig-9","dal_prefix":"prefix-x"}"#,
            ),
            user("again"),
        ],
    );
    let input = proxy_request(&request, &thinking, &grant, SessionId::new_v7());
    let wire = build_proxy(&input).unwrap();
    let text = String::from_utf8(wire.body).unwrap();
    assert!(!text.contains("sig-9"));
    assert!(!text.contains("dal_prefix"));
    let body: sonic_rs::Value = sonic_rs::from_str(&text).unwrap();
    assert_eq!(body["messages"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn proxy_open_rebinds_anthropic_replay_to_gitlab() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let response = sse(&[
        r#"{"type":"message_start","message":{"usage":{"input_tokens":25,"output_tokens":1}}}"#,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"why"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"EqQBCgIYAhIM"}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null}}"#,
        r#"{"type":"message_stop"}"#,
    ]);
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(serve_once(listener, "200 OK", response.as_bytes().to_vec()));
    let request = request("duo-chat-sonnet-4-6", vec![user("hi")]);
    let thinking = enabled_plan();
    let grant = grant();
    let input = proxy_request(&request, &thinking, &grant, SessionId::new_v7());
    let mut wire = build_proxy(&input).unwrap();
    assert!(
        wire.replay_prefix.is_some(),
        "the producing request binds the replay it will decode"
    );
    // Point the proxy at the loopback server.
    wire.url = crate::http::endpoint(Family::GitLab, &base, "chat/completions").unwrap();
    let mut stream = open(&crate::http::build_client(), wire, "dalgon/test")
        .await
        .unwrap();
    let mut replay = None;
    while let Some(item) = stream.next().await {
        if let StreamEvent::Replay { payload } = item.unwrap() {
            replay = Some(payload);
        }
    }
    let replay = replay.expect("the signed thinking block replays");
    // The shared decoder names its own family; the proxy lifecycle rebinds
    // the payload to the family the turn is journaled under, keeping the
    // alias route the journal records (the wire serves the upstream id).
    assert_eq!(replay.family, Family::GitLab);
    assert_eq!(replay.model.as_ref(), "duo-chat-sonnet-4-6");
    assert!(replay.item.as_str().contains("EqQBCgIYAhIM"));
    assert!(replay.item.as_str().contains("dal_prefix"));
    servers.abort_all();
}
