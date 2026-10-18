//! Droid adapter tests: route table, headers, bodies, decoders, and the proxy
//! lifecycle against a loopback server. Wire bytes below are captured shapes,
//! never model text.

use dal_core::{
    AssistantPart, CallId, ContextItem, Family, ModelRequest, ModelRoute, Part, Purpose, RawJson,
    ReplaySource, RequestParams, SessionId, ThinkingLevel,
};
use futures::{StreamExt, executor::block_on, stream};
use sonic_rs::{JsonContainerTrait, JsonValueTrait};

use super::*;
use crate::{
    auth::credential::{Credential, OAuthCredential, SecretString},
    sse::decode_stream,
    stream::StopReason,
    thinking::{AnthropicThinking, ThinkingPlan, WireThinking},
};

fn raw(text: &str) -> RawJson {
    RawJson::parse(text).unwrap()
}

fn tool(name: &str) -> dal_core::ModelToolSpec {
    dal_core::ModelToolSpec {
        name: name.into(),
        description: "a tool".into(),
        parameters: raw(r#"{"type":"object","properties":{"q":{"type":"string"}}}"#),
        grammar: None,
        defer_loading: false,
    }
}

fn text_part(text: &str) -> Part {
    Part::Text { text: text.into() }
}

fn user(text: &str) -> ContextItem {
    ContextItem::User {
        parts: vec![text_part(text)],
    }
}

fn request(model: &str, context: Vec<ContextItem>) -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::Droid,
            model: model.into(),
        },
        system: "You are terse.".into(),
        tools: vec![tool("read")].into(),
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

fn anthropic_plan() -> ThinkingPlan {
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

static POLICY: RoutePolicy<'static> = RoutePolicy {
    reasoning_summary: false,
    display_supported: false,
    anthropic_tool_streaming_beta: false,
    anthropic_effort_beta: false,
    openai_platform: None,
    responses_cache_24h: false,
    responses_safety_identifier: false,
    responses_verbosity: None,
    responses_service_tier: None,
    gemini_medium: false,
    routing_locked: false,
    max_output: Some(16_000),
};

fn droid_request<'a>(
    request: &'a ModelRequest,
    thinking: &'a ThinkingPlan,
    session: SessionId,
) -> DroidRequest<'a> {
    DroidRequest {
        request,
        thinking,
        base_url: "https://api.factory.ai",
        upstream: "fireworks",
        session,
        request_id: "req-1",
        bearer: "workos-token",
        policy: &POLICY,
    }
}

fn session() -> SessionId {
    SessionId::new_v7()
}

fn wire(body: &DroidWire) -> sonic_rs::Value {
    sonic_rs::from_slice(&body.body).unwrap()
}

#[test]
fn route_table_covers_the_pinned_rows_and_defaults_unknown() {
    assert_eq!(ROUTES.len(), 62);
    assert_eq!(wire_for("claude-sonnet-5"), Wire::Anthropic);
    assert_eq!(wire_for("atlas-07-21"), Wire::Anthropic);
    assert_eq!(wire_for("gpt-5.6-luna"), Wire::Responses);
    assert_eq!(wire_for("grok-4.5"), Wire::Responses);
    assert_eq!(wire_for("gemini-3.8-flash"), Wire::Gemini);
    assert_eq!(wire_for("kimi-k3"), Wire::Completions);
    assert_eq!(wire_for("something-new"), Wire::Completions);
}

#[test]
fn bearer_gate_refuses_api_keys_and_empty_tokens() {
    let key = Credential::ApiKey {
        key: SecretString::from("fk-key"),
    };
    assert!(matches!(
        require_bearer(&key),
        Err(ProviderError::InvalidRequest { .. })
    ));
    assert!(matches!(
        require_bearer(&Credential::None),
        Err(ProviderError::NoCredentials { .. })
    ));
    let oauth = Credential::OAuth(OAuthCredential {
        access_token: SecretString::from("tok"),
        refresh_token: SecretString::from(""),
        expires_at: None,
        id_token: None,
        account_id: None,
    });
    assert_eq!(require_bearer(&oauth).unwrap(), "tok");
}

#[test]
fn session_uuid_is_a_stable_v4_shape() {
    let first = session();
    let again = session_uuid(first);
    assert_eq!(again, session_uuid(first));
    let parsed = uuid::Uuid::parse_str(&again).unwrap();
    assert_eq!(parsed.get_version_num(), 4);
    assert_ne!(again, session_uuid(session()));
}

#[test]
fn org_claim_reads_external_org_id() {
    use base64::Engine as _;
    let claims =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"external_org_id":"org-9"}"#);
    let token = format!("aaa.{claims}.bbb");
    assert_eq!(org_id_from_token(&token).as_deref(), Some("org-9"));
    assert_eq!(org_id_from_token("opaque"), None);
}

#[test]
fn identity_headers_carry_routing_session_and_secret_first() {
    let request = request("kimi-k3", vec![user("hi")]);
    let plan = open_plan();
    let session = session();
    let input = droid_request(&request, &plan, session);
    let built = build_wire(&input).unwrap();
    assert_eq!(built.headers[0].0, "authorization");
    assert_eq!(built.headers[0].1, "Bearer workos-token");
    assert_eq!(built.secret.as_ref(), input.bearer);
    let routing = built
        .headers
        .iter()
        .find(|(name, _)| *name == "x-provider-routing-source")
        .unwrap();
    assert_eq!(routing.1, "registry_default");
    let session_header = built
        .headers
        .iter()
        .find(|(name, _)| *name == "x-session-id")
        .unwrap();
    assert_eq!(session_header.1, session_uuid(session));
    assert!(built.user_agent.starts_with("factory-cli/"));
}

#[test]
fn served_session_locks_the_upstream() {
    let replay = ContextItem::Assistant {
        source: ReplaySource {
            family: Family::Droid,
            model: "kimi-k3".into(),
        },
        parts: vec![AssistantPart::Text {
            text: "done".into(),
            replay: None,
        }],
        replay: None,
    };
    let request = request("kimi-k3", vec![user("hi"), replay, user("more")]);
    let plan = open_plan();
    let input = droid_request(&request, &plan, session());
    let built = build_wire(&input).unwrap();
    let routing = built
        .headers
        .iter()
        .find(|(name, _)| *name == "x-provider-routing-source")
        .unwrap();
    assert_eq!(routing.1, "session_lock");
}

#[test]
fn completions_body_prefixes_system_and_pins_temperature() {
    let request = request("kimi-k3", vec![user("hi")]);
    let plan = open_plan();
    let input = droid_request(&request, &plan, session());
    let built = build_wire(&input).unwrap();
    let body = wire(&built);
    assert_eq!(
        body["messages"][0]["content"].as_str().unwrap(),
        "You are Droid, an AI software engineering agent built by Factory.\nYou are terse."
    );
    assert_eq!(body["temperature"].as_f64().unwrap(), 1.0);
    assert_eq!(body["reasoning_effort"].as_str().unwrap(), "medium");
    assert_eq!(
        body["prompt_cache_key"].as_str().unwrap(),
        built
            .headers
            .iter()
            .find(|(name, _)| *name == "x-session-id")
            .unwrap()
            .1
    );
    assert!(body.get("chat_template_args").is_none());
}

#[test]
fn completions_opt_in_splices_toggle_without_effort() {
    let request = request("nemotron-3-ultra", vec![user("hi")]);
    let plan = open_plan();
    let mut input = droid_request(&request, &plan, session());
    input.upstream = "baseten";
    assert_eq!(wire_for("nemotron-3-ultra"), Wire::Completions);
    let built = build_wire(&input).unwrap();
    let body = wire(&built);
    assert!(body.get("reasoning_effort").is_none());
    assert!(
        body["chat_template_args"]["enable_thinking"]
            .as_bool()
            .unwrap()
    );
}

#[test]
fn completions_forced_on_maps_off_to_low() {
    let request = request("deepseek-v4-pro", vec![user("hi")]);
    let quiet = ThinkingPlan {
        level: ThinkingLevel::Off,
        wire: WireThinking::OpenAi { effort: None },
        temperature: None,
        notices: Vec::new(),
    };
    let mut input = droid_request(&request, &quiet, session());
    input.upstream = "baseten";
    assert_eq!(wire_for("deepseek-v4-pro"), Wire::Completions);
    let built = build_wire(&input).unwrap();
    let body = wire(&built);
    assert_eq!(body["reasoning_effort"].as_str().unwrap(), "low");
    assert!(body.get("chat_template_args").is_none());
}

#[test]
fn responses_body_keys_cache_and_declares_non_strict_tools() {
    let request = request("gpt-5.4", vec![user("hi")]);
    let plan = open_plan();
    let input = droid_request(&request, &plan, session());
    let built = build_wire(&input).unwrap();
    let body = wire(&built);
    assert_eq!(
        body["prompt_cache_key"].as_str().unwrap(),
        built
            .headers
            .iter()
            .find(|(name, _)| *name == "x-session-id")
            .unwrap()
            .1
    );
    assert!(!body["tools"][0]["strict"].as_bool().unwrap());
    assert_eq!(
        body["instructions"]
            .as_str()
            .unwrap()
            .lines()
            .next()
            .unwrap(),
        DROID_SYSTEM_PREFIX
    );
}

#[test]
fn responses_tools_with_quoted_text_still_declare_non_strict() {
    let quoted = dal_core::ModelToolSpec {
        name: "read".into(),
        description: "reads the file the user quoted as \"main\"".into(),
        parameters: raw(
            r#"{"type":"object","properties":{"q":{"type":"string","description":"a \"quoted\" value"}}}"#,
        ),
        grammar: None,
        defer_loading: false,
    };
    let request = ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::Droid,
            model: "gpt-5.4".into(),
        },
        system: "You are terse.".into(),
        tools: vec![quoted].into(),
        context: vec![user("hi")].into(),
        params: RequestParams {
            thinking: ThinkingLevel::Medium,
            effort: None,
            temperature: None,
            max_output_tokens: None,
        },
        cache_key: None,
    };
    let plan = open_plan();
    let input = droid_request(&request, &plan, session());
    let built = build_wire(&input).unwrap();
    let text = String::from_utf8(built.body).unwrap();
    let body: sonic_rs::Value = sonic_rs::from_str(&text).unwrap();
    assert!(!body["tools"][0]["strict"].as_bool().unwrap());
    assert_eq!(
        body["tools"][0]["description"].as_str(),
        Some("reads the file the user quoted as \"main\"")
    );
}

#[test]
fn anthropic_body_keeps_one_bearer_and_merges_betas() {
    let request = request("claude-sonnet-5", vec![user("hi")]);
    let plan = anthropic_plan();
    let input = droid_request(&request, &plan, session());
    let built = build_wire(&input).unwrap();
    let authorizations = built
        .headers
        .iter()
        .filter(|(name, _)| *name == "authorization")
        .count();
    assert_eq!(authorizations, 1);
    assert!(
        built
            .headers
            .iter()
            .any(|(name, value)| *name == "x-api-key" && value == "placeholder")
    );
    assert!(built.replay_prefix.is_some());
    let body = wire(&built);
    assert!(
        body["system"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with(DROID_SYSTEM_PREFIX)
    );
}

#[test]
fn unknown_upstream_and_foreign_thinking_fail() {
    let request = request("kimi-k3", vec![user("hi")]);
    let plan = open_plan();
    let mut input = droid_request(&request, &plan, session());
    input.upstream = "unknown-router";
    assert!(build_wire(&input).is_err());
    let unknown = anthropic_plan();
    let input = droid_request(&request, &unknown, session());
    assert!(build_wire(&input).is_err());
}

fn gemini_request(context: Vec<ContextItem>) -> (ModelRequest, ThinkingPlan, SessionId) {
    (request("gemini-3.8-flash", context), open_plan(), session())
}

#[test]
fn gemini_body_orders_members_and_pins_defaults() {
    let (request, plan, session) = gemini_request(vec![user("hi")]);
    let input = droid_request(&request, &plan, session);
    let built = build_wire(&input).unwrap();
    let text = String::from_utf8(built.body.clone()).unwrap();
    let order = [
        "\"model\"",
        "\"contents\"",
        "\"systemInstruction\"",
        "\"generationConfig\"",
        "\"tools\"",
    ];
    let mut at = 0;
    for member in order {
        let found = text[at..].find(member).unwrap();
        at += found + member.len();
    }
    let body = wire(&built);
    assert_eq!(
        body["generationConfig"]["temperature"].as_f64().unwrap(),
        1.0
    );
    assert_eq!(body["generationConfig"]["topP"].as_f64().unwrap(), 0.95);
    assert_eq!(
        body["generationConfig"]["thinkingConfig"]["thinkingLevel"]
            .as_str()
            .unwrap(),
        "HIGH"
    );
    assert!(
        body["systemInstruction"]["parts"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with(DROID_SYSTEM_PREFIX)
    );
    assert!(
        !built
            .headers
            .iter()
            .any(|(name, _)| name.starts_with("x-stainless"))
    );
}

#[test]
fn gemini_tool_schema_projects_refs_and_unions() {
    let mut request = request("gemini-3.8-flash", vec![user("hi")]);
    request.tools = vec![dal_core::ModelToolSpec {
        name: "run".into(),
        description: "runs".into(),
        parameters: raw(
            r##"{"type":"object","required":["x"],"properties":{"x":{"$ref":"#/$defs/X"},"y":{"anyOf":[{"type":"string"},{"type":"null"}]}},"$defs":{"X":{"type":"string","description":"d"}}}"##,
        ),
        grammar: None,
        defer_loading: false,
    }]
    .into();
    let plan = open_plan();
    let input = droid_request(&request, &plan, session());
    let built = build_wire(&input).unwrap();
    let body = wire(&built);
    let params = &body["tools"][0]["functionDeclarations"][0]["parameters"];
    assert_eq!(
        params["properties"]["x"]["description"].as_str().unwrap(),
        "d"
    );
    assert_eq!(
        params["properties"]["y"]["type"].as_str().unwrap(),
        "string"
    );
    assert!(params["properties"]["y"]["nullable"].as_bool().unwrap());
}

#[test]
fn gemini_tool_results_group_and_tail_calls_skip_validation() {
    let result = |text: &str| ContextItem::ToolResult {
        call: CallId::new("c1"),
        name: "read".into(),
        is_error: false,
        parts: vec![text_part(text)],
    };
    let call = ContextItem::Assistant {
        source: ReplaySource {
            family: Family::Droid,
            model: "gemini-3.8-flash".into(),
        },
        parts: vec![AssistantPart::ToolCall {
            call: CallId::new("c1"),
            name: "read".into(),
            args: raw(r#"{"q":"x"}"#),
            replay: None,
        }],
        replay: None,
    };
    let (request, plan, session) = gemini_request(vec![user("go"), call, result("a"), result("b")]);
    let input = droid_request(&request, &plan, session);
    let built = build_wire(&input).unwrap();
    let body = wire(&built);
    let contents = body["contents"].as_array().unwrap();
    let responses: Vec<_> = contents
        .iter()
        .filter(|content| content["role"].as_str().unwrap() == "user")
        .collect();
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[1]["parts"].as_array().unwrap().len(), 2);
    assert_eq!(
        body["contents"][1]["parts"][0]["thoughtSignature"]
            .as_str()
            .unwrap(),
        gemini::SKIP_SENTINEL
    );
}

fn sse(datas: &[&str]) -> String {
    use std::fmt::Write as _;
    datas.iter().fold(String::new(), |mut wire, data| {
        let _ = write!(wire, "data: {data}\n\n");
        wire
    })
}

fn run_gemini(wire: &str, split: usize) -> (Vec<StreamEvent>, Option<ProviderError>) {
    let chunks: Vec<Vec<u8>> = wire.as_bytes().chunks(split).map(<[u8]>::to_vec).collect();
    let decoded = gemini::decode(decode_stream(stream::iter(chunks)), "gemini-3.8-flash");
    let items: Vec<_> = block_on(decoded.collect());
    let mut events = Vec::new();
    let mut error = None;
    for item in items {
        assert!(error.is_none(), "an item followed the terminal error");
        match item {
            Ok(event) => events.push(event),
            Err(failure) => error = Some(failure),
        }
    }
    (events, error)
}

#[test]
fn gemini_text_turn_decodes_over_any_split() {
    let wire = sse(&[
        r#"{"candidates":[{"content":{"parts":[{"text":"Hello"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":19,"candidatesTokenCount":10,"thoughtsTokenCount":3,"cachedContentTokenCount":4}}"#,
    ]);
    for split in [1, 7, 4096] {
        let (events, error) = run_gemini(&wire, split);
        assert!(error.is_none());
        assert!(
            events
                .iter()
                .any(|event| matches!(event, StreamEvent::TextDelta { text } if text == "Hello"))
        );
        let Some(StreamEvent::Stop { reason }) = events.last() else {
            panic!("no terminal stop");
        };
        assert_eq!(*reason, StopReason::EndTurn);
        let Some(StreamEvent::Usage { usage }) = events
            .iter()
            .find(|event| matches!(event, StreamEvent::Usage { .. }))
        else {
            panic!("no usage");
        };
        assert_eq!(usage.input_tokens, 15);
        assert_eq!(usage.cached_input_tokens, 4);
        assert_eq!(usage.output_tokens, 13);
        assert_eq!(usage.reasoning_tokens, Some(3));
    }
}

#[test]
fn gemini_thought_and_signature_emit_reasoning_and_replay() {
    let wire = sse(&[
        r#"{"candidates":[{"content":{"parts":[{"text":"because","thought":true,"thoughtSignature":"sig-1"}]},"finishReason":"STOP"}]}"#,
    ]);
    let (events, error) = run_gemini(&wire, 4096);
    assert!(error.is_none());
    assert!(
        events.iter().any(
            |event| matches!(event, StreamEvent::ReasoningDelta { text } if text == "because")
        )
    );
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::Replay { payload } if payload.family == Family::Droid
            && payload.model.as_ref() == "gemini-3.8-flash"
    )));
}

#[test]
fn gemini_function_call_gets_a_generated_id_and_closes_as_tool_use() {
    let wire = sse(&[
        r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"read","args":{"q":"x"}}}]},"finishReason":"STOP"}]}"#,
    ]);
    let (events, error) = run_gemini(&wire, 4096);
    assert!(error.is_none());
    let started = events
        .iter()
        .find_map(|event| match event {
            StreamEvent::ToolCallStarted { id, name } => Some((id.clone(), name.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(started.1, "read");
    assert!(
        events.iter().any(
            |event| matches!(event, StreamEvent::ToolArgsDelta { id, .. } if id == &started.0)
        )
    );
    let Some(StreamEvent::Stop { reason }) = events.last() else {
        panic!("no terminal stop");
    };
    assert_eq!(*reason, StopReason::ToolUse);
}

#[test]
fn gemini_truncation_without_a_reason_is_stream_cut() {
    let (events, error) = run_gemini(
        &sse(&[r#"{"candidates":[{"content":{"parts":[{"text":"half"}]}}]}"#]),
        4096,
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::TextDelta { .. }))
    );
    assert!(matches!(error, Some(ProviderError::StreamCut)));
}

#[test]
fn gemini_max_tokens_without_calls_is_max_tokens() {
    let (events, error) = run_gemini(
        &sse(&[r#"{"candidates":[{"content":{"parts":[]},"finishReason":"MAX_TOKENS"}]}"#]),
        4096,
    );
    assert!(error.is_none());
    assert!(matches!(
        events.last(),
        Some(StreamEvent::Stop {
            reason: StopReason::MaxTokens
        })
    ));
}

#[test]
fn gemini_block_refuses_and_malformed_errors() {
    let (events, error) = run_gemini(
        &sse(&[r#"{"promptFeedback":{"blockReason":"SAFETY"}}"#]),
        4096,
    );
    assert!(error.is_none());
    assert!(matches!(
        events.last(),
        Some(StreamEvent::Stop {
            reason: StopReason::Refusal
        })
    ));
    let (_, error) = run_gemini(
        &sse(&[
            r#"{"candidates":[{"content":{"parts":[]},"finishReason":"MALFORMED_FUNCTION_CALL"}]}"#,
        ]),
        4096,
    );
    assert!(matches!(error, Some(ProviderError::Protocol { .. })));
}

#[test]
fn gemini_malformed_json_and_error_frames_fail() {
    let (_, error) = run_gemini(&sse(&["{not json"]), 4096);
    assert!(matches!(error, Some(ProviderError::Protocol { .. })));
    let (_, error) = run_gemini(
        &sse(&[r#"{"error":{"code":429,"message":"slow","status":"RESOURCE_EXHAUSTED"}}"#]),
        4096,
    );
    assert!(matches!(error, Some(ProviderError::RateLimited { .. })));
}

#[test]
fn gemini_terminal_is_exactly_one() {
    // Intermediate reasons are benign: the reference keeps every chunk and
    // lets the last chunk's reason stand, so both texts arrive under one
    // terminal stop.
    let wire = sse(&[
        r#"{"candidates":[{"content":{"parts":[{"text":"x"}]},"finishReason":"STOP"}]}"#,
        r#"{"candidates":[{"content":{"parts":[{"text":"late"}]},"finishReason":"MAX_TOKENS"}]}"#,
    ]);
    let (events, error) = run_gemini(&wire, 4096);
    assert!(error.is_none());
    assert!(
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::TextDelta { text } if text == "late"))
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, StreamEvent::Stop { .. }))
            .count(),
        1
    );
    assert!(matches!(
        events.last(),
        Some(StreamEvent::Stop {
            reason: StopReason::MaxTokens
        })
    ));
}

#[test]
fn droid_assistant_replays_on_its_route_but_not_foreign_sources() {
    let thinking = ContextItem::Assistant {
        source: ReplaySource {
            family: Family::Droid,
            model: "gemini-3.8-flash".into(),
        },
        parts: vec![AssistantPart::Thinking {
            text: "plan".into(),
            replay: Some(raw(r#"{"text":"plan","thoughtSignature":"sig-9"}"#)),
        }],
        replay: None,
    };
    let (request, plan, session) = gemini_request(vec![user("hi"), thinking]);
    let input = droid_request(&request, &plan, session);
    let built = build_wire(&input).unwrap();
    let text = String::from_utf8(built.body).unwrap();
    assert!(text.contains("sig-9"));
    let foreign = ContextItem::Assistant {
        source: ReplaySource {
            family: Family::Anthropic,
            model: "gemini-3.8-flash".into(),
        },
        parts: vec![AssistantPart::Thinking {
            text: "plan".into(),
            replay: Some(raw(r#"{"text":"plan","thoughtSignature":"sig-9"}"#)),
        }],
        replay: None,
    };
    let (request, plan, session) = gemini_request(vec![user("hi"), foreign]);
    let input = droid_request(&request, &plan, session);
    let built = build_wire(&input).unwrap();
    let text = String::from_utf8(built.body).unwrap();
    assert!(!text.contains("sig-9"));
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
        "HTTP/1.1 {status}\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    AsyncWriteExt::write_all(&mut tcp, reply.as_bytes())
        .await
        .unwrap();
    AsyncWriteExt::write_all(&mut tcp, &body).await.unwrap();
    (head_text, request_body)
}

async fn drain(mut stream: EventStream) -> Vec<StreamEvent> {
    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        events.push(item.unwrap());
    }
    events
}

#[tokio::test]
async fn open_posts_identity_and_decodes_chat() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let response = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1}}\n\n";
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(serve_once(listener, "200 OK", response.as_bytes().to_vec()));
    let request = request("kimi-k3", vec![user("hi")]);
    let plan = open_plan();
    let input = DroidRequest {
        request: &request,
        thinking: &plan,
        base_url: &base,
        upstream: "fireworks",
        session: session(),
        request_id: "req-1",
        bearer: "workos-token",
        policy: &POLICY,
    };
    let wire = build_wire(&input).unwrap();
    assert!(wire.url.as_str().starts_with(&base));
    assert!(
        wire.url
            .as_str()
            .ends_with("/api/llm/o/v1/chat/completions")
    );
    let stream = open(&crate::http::build_client(), wire).await.unwrap();
    let events = drain(stream).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::TextDelta { text } if text == "hi"))
    );
    assert!(matches!(
        events.last(),
        Some(StreamEvent::Stop {
            reason: StopReason::EndTurn
        })
    ));
    let (head, body) = servers.join_next().await.unwrap().unwrap();
    assert!(head.contains("authorization: Bearer workos-token"));
    assert!(head.contains("x-api-provider: fireworks"));
    let body = String::from_utf8(body).unwrap();
    assert!(body.contains(DROID_SYSTEM_PREFIX));
    servers.abort_all();
}

#[tokio::test]
async fn open_maps_region_refusal_without_retry() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let response = r#"{"error":{"code":"region","message":"not available in this region"}}"#;
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(serve_once(
        listener,
        "403 Forbidden",
        response.as_bytes().to_vec(),
    ));
    let request = request("kimi-k3", vec![user("hi")]);
    let plan = open_plan();
    let input = DroidRequest {
        request: &request,
        thinking: &plan,
        base_url: &base,
        upstream: "fireworks",
        session: session(),
        request_id: "req-1",
        bearer: "workos-token",
        policy: &POLICY,
    };
    let wire = build_wire(&input).unwrap();
    let failure = open(&crate::http::build_client(), wire).await.unwrap_err();
    assert!(matches!(
        failure,
        crate::lifecycle::AttemptFailure::Provider(ProviderError::InvalidRequest { message })
            if message.contains("not served from your network's region")
    ));
    servers.abort_all();
}

#[tokio::test]
async fn open_reports_plain_failures_with_status() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let response = r#"{"error":{"message":"boom"}}"#;
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(serve_once(
        listener,
        "500 Internal Server Error",
        response.as_bytes().to_vec(),
    ));
    let request = request("kimi-k3", vec![user("hi")]);
    let plan = open_plan();
    let input = DroidRequest {
        request: &request,
        thinking: &plan,
        base_url: &base,
        upstream: "fireworks",
        session: session(),
        request_id: "req-1",
        bearer: "workos-token",
        policy: &POLICY,
    };
    let wire = build_wire(&input).unwrap();
    let failure = open(&crate::http::build_client(), wire).await.unwrap_err();
    assert!(matches!(
        failure,
        crate::lifecycle::AttemptFailure::Response { status: 500, .. }
    ));
    servers.abort_all();
}

/// A bearer carrying an `external_org_id` claim, the shape
/// [`org_id_from_token`] reads.
fn org_token(org: &str) -> String {
    use base64::Engine as _;
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(format!(r#"{{"external_org_id":"{org}"}}"#));
    format!("aaa.{claims}.bbb")
}

/// Serves one 500 that reflects the request's `authorization` value in its
/// body, the hostile shape the fixed error texts must not echo.
async fn serve_reflected_bearer(listener: tokio::net::TcpListener) -> String {
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
    let bearer = head_text
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("authorization")
                .then(|| value.trim().to_owned())
        })
        .unwrap_or_default();
    let body = format!(r#"{{"error":"denied for {bearer}"}}"#);
    let reply = format!(
        "HTTP/1.1 500 Internal Server Error\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    AsyncWriteExt::write_all(&mut tcp, reply.as_bytes())
        .await
        .unwrap();
    head_text
}

/// Accepts one `whoami` request and never answers, so the test can prove
/// cancellation wins over a hung server.
async fn serve_hung_whoami(listener: tokio::net::TcpListener) {
    use tokio::io::AsyncReadExt;
    let (mut tcp, _) = listener.accept().await.unwrap();
    let mut head = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        if AsyncReadExt::read_exact(&mut tcp, &mut byte).await.is_err() {
            return;
        }
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    std::future::pending::<()>().await;
}

#[tokio::test]
async fn verify_session_sends_identity_headers_and_accepts_a_resolved_org() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let token = org_token("org-1");
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(serve_once(
        listener,
        "200 OK",
        br#"{"orgId":"org-1","region":"global","inferenceRegion":"global"}"#.to_vec(),
    ));
    let cancel = tokio_util::sync::CancellationToken::new();
    verify_session(&crate::http::build_client(), &base, &token, &cancel)
        .await
        .expect("a 2xx answer with an org verifies");
    let (head, _) = servers.join_next().await.unwrap().unwrap();
    assert!(
        head.starts_with("GET /api/cli/whoami "),
        "unexpected request line: {head}"
    );
    assert!(head.contains(&format!("authorization: Bearer {token}")));
    assert!(head.contains("accept: application/json"));
    assert!(head.contains("x-factory-org-id: org-1"));
    servers.abort_all();
}

#[tokio::test]
async fn verify_session_falls_back_to_the_token_claim() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let token = org_token("org-2");
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(serve_once(
        listener,
        "200 OK",
        br#"{"region":"global"}"#.to_vec(),
    ));
    let cancel = tokio_util::sync::CancellationToken::new();
    verify_session(&crate::http::build_client(), &base, &token, &cancel)
        .await
        .expect("a record without orgId still verifies through the claim");
    let (head, _) = servers.join_next().await.unwrap().unwrap();
    assert!(head.contains("x-factory-org-id: org-2"));
    servers.abort_all();
}

#[tokio::test]
async fn verify_session_rejects_an_unresolved_org() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(serve_once(listener, "200 OK", b"{}".to_vec()));
    let cancel = tokio_util::sync::CancellationToken::new();
    let result = verify_session(&crate::http::build_client(), &base, "opaque-token", &cancel).await;
    let (head, _) = servers.join_next().await.unwrap().unwrap();
    assert!(
        !head.contains("x-factory-org-id"),
        "a token without a claim sends no org header: {head}"
    );
    assert!(
        matches!(
            &result,
            Err(ProviderError::TokenExchange { status: 200, message })
                if message == "Factory login did not resolve an organization."
        ),
        "a 2xx answer without an org is a fixed exchange failure: {result:?}"
    );
    servers.abort_all();
}

#[tokio::test]
async fn verify_session_treats_an_unreadable_body_like_a_missing_org() {
    let client = crate::http::build_client();
    let garbage = b"not json".to_vec();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(serve_once(listener, "200 OK", b"not json".to_vec()));
    let cancel = tokio_util::sync::CancellationToken::new();
    let result = verify_session(&client, &base, "opaque-token", &cancel).await;
    servers.join_next().await.unwrap().unwrap();
    assert!(
        matches!(
            &result,
            Err(ProviderError::TokenExchange { status: 200, message })
                if message == "Factory login did not resolve an organization."
                    && !message.contains("not json")
        ),
        "a malformed body without a claim fails without echo: {result:?}"
    );
    servers.abort_all();
    // The reference rethrows the parse failure on login, so a malformed
    // body fails even when the token carries a claim.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(serve_once(listener, "200 OK", garbage));
    let token = org_token("org-3");
    let result = verify_session(&client, &base, &token, &cancel).await;
    servers.join_next().await.unwrap().unwrap();
    assert!(
        matches!(
            result,
            Err(ProviderError::TokenExchange { status: 200, .. })
        ),
        "a malformed body fails despite the claim: {result:?}"
    );
    servers.abort_all();
    // A body that parses but is not a record still falls back to the claim.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(serve_once(listener, "200 OK", b"[1,2]".to_vec()));
    verify_session(&client, &base, &token, &cancel)
        .await
        .expect("a non-record body still verifies through the claim");
    servers.abort_all();
}

#[tokio::test]
async fn verify_session_maps_denials_to_a_fixed_status_error() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(serve_once(
        listener,
        "403 Forbidden",
        br#"{"error":"denied-marker-xyz"}"#.to_vec(),
    ));
    let cancel = tokio_util::sync::CancellationToken::new();
    let result = verify_session(&crate::http::build_client(), &base, "opaque-token", &cancel).await;
    servers.join_next().await.unwrap().unwrap();
    assert!(
        matches!(
            &result,
            Err(ProviderError::Status {
                status: 403,
                message,
                ..
            })
                if message
                    == "Factory sign-in verification failed; the Droid service refused the session."
                    && !message.contains("denied-marker-xyz")
        ),
        "a denial is a fixed status failure without the body: {result:?}"
    );
    servers.abort_all();
}

#[tokio::test]
async fn verify_session_never_echoes_the_bearer_or_org() {
    let client = crate::http::build_client();
    let token = org_token("org-secret-9");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(serve_reflected_bearer(listener));
    let cancel = tokio_util::sync::CancellationToken::new();
    let result = verify_session(&client, &base, &token, &cancel).await;
    servers.join_next().await.unwrap().unwrap();
    assert!(
        matches!(
            &result,
            Err(ProviderError::Status { status: 500, message, .. })
                if !message.contains(&token) && !message.contains("org-secret-9")
        ),
        "a hostile body must not reach the status failure: {result:?}"
    );
    servers.abort_all();
    // A malformed body fails even with the claim token, so both secrets are
    // in play and neither may reach the failure.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(serve_once(listener, "200 OK", b"not json".to_vec()));
    let result = verify_session(&client, &base, &token, &cancel).await;
    servers.join_next().await.unwrap().unwrap();
    assert!(
        matches!(
            &result,
            Err(ProviderError::TokenExchange { message, .. })
                if !message.contains(&token) && !message.contains("org-secret-9")
        ),
        "the exchange failure carries no credential: {result:?}"
    );
    servers.abort_all();
}

#[test]
fn transport_failure_that_races_cancellation_reports_cancel() {
    let cancel = tokio_util::sync::CancellationToken::new();
    cancel.cancel();
    let failure = ProviderError::Transport {
        family: Family::Droid,
        reason: String::from("boom"),
    };
    assert!(matches!(
        cancel_or(failure, &cancel, "bearer"),
        ProviderError::LoginCancelled
    ));
}

#[test]
fn transport_failure_without_cancellation_is_redacted_not_cancelled() {
    let cancel = tokio_util::sync::CancellationToken::new();
    let bearer = "bearer-secret-xyz";
    let failure = ProviderError::Transport {
        family: Family::Droid,
        reason: format!("denied for {bearer}"),
    };
    match cancel_or(failure, &cancel, bearer) {
        ProviderError::Transport { reason, .. } => {
            assert!(!reason.contains(bearer));
            assert!(reason.contains("<redacted>"));
        }
        other => panic!("an uncancelled failure keeps its variant redacted: {other:?}"),
    }
}

#[tokio::test]
async fn verify_session_honours_cancellation_before_and_during_the_call() {
    let client = crate::http::build_client();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let cancel = tokio_util::sync::CancellationToken::new();
    cancel.cancel();
    let result = verify_session(&client, &base, "opaque-token", &cancel).await;
    assert!(
        matches!(result, Err(ProviderError::LoginCancelled)),
        "a cancelled call fails before touching the network: {result:?}"
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(200), listener.accept())
            .await
            .is_err(),
        "a pre-cancelled verify must not connect"
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(serve_hung_whoami(listener));
    let token = org_token("org-7");
    let cancel = tokio_util::sync::CancellationToken::new();
    let (result, ()) = tokio::join!(verify_session(&client, &base, &token, &cancel), async {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        cancel.cancel();
    },);
    assert!(
        matches!(result, Err(ProviderError::LoginCancelled)),
        "cancellation wins over a hung server: {result:?}"
    );
    servers.abort_all();
}
/// An assistant entry journaled under [`Family::Droid`], carrying the stored
/// replay payload the producing request recorded.
fn journaled(alias: &str, replay: &str) -> ContextItem {
    ContextItem::Assistant {
        source: ReplaySource {
            family: Family::Droid,
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
fn droid_assistant_replays_thinking_on_the_anthropic_route() {
    let thinking = anthropic_plan();
    // The prefix binding a producing request would have recorded.
    let producer = request("claude-sonnet-5", vec![user("hi")]);
    let prefix = build_wire(&droid_request(&producer, &thinking, session()))
        .unwrap()
        .replay_prefix
        .unwrap();
    let block = |signature: &str| {
        format!(
            r#"{{"type":"thinking","thinking":"plan","signature":"{signature}","dal_prefix":"{prefix}"}}"#
        )
    };
    let request = request(
        "claude-sonnet-5",
        vec![
            user("hi"),
            journaled("claude-sonnet-5", &block("sig-own")),
            user("again"),
            journaled("atlas-07-21", &block("sig-other")),
            user("more"),
        ],
    );
    let input = droid_request(&request, &thinking, session());
    let built = build_wire(&input).unwrap();
    let text = String::from_utf8(built.body).unwrap();
    let body: sonic_rs::Value = sonic_rs::from_str(&text).unwrap();
    // The block produced under this route model replays, rebuilt without its
    // storage-only binding; a block another model signed never does.
    assert_eq!(
        body["messages"][1]["content"][0]["signature"].as_str(),
        Some("sig-own")
    );
    assert!(!text.contains("sig-other"));
    assert!(!text.contains("dal_prefix"));
}

#[test]
fn droid_assistant_replays_reasoning_on_the_responses_route() {
    let thinking = open_plan();
    let reasoning = |id: &str| format!(r#"{{"type":"reasoning","id":"{id}","summary":[]}}"#);
    let request = request(
        "gpt-5.6-luna",
        vec![
            user("hi"),
            journaled("gpt-5.6-luna", &reasoning("rs-own")),
            user("again"),
            journaled("grok-4.5", &reasoning("rs-other")),
            user("more"),
        ],
    );
    let input = droid_request(&request, &thinking, session());
    let built = build_wire(&input).unwrap();
    let text = String::from_utf8(built.body).unwrap();
    let body: sonic_rs::Value = sonic_rs::from_str(&text).unwrap();
    assert_eq!(body["model"].as_str(), Some("gpt-5.6-luna"));
    assert_eq!(body["input"][1]["id"].as_str(), Some("rs-own"));
    assert!(!text.contains("rs-other"));
}

#[test]
fn anthropic_replay_round_trips_through_decode_and_build() {
    let thinking = anthropic_plan();
    let producer = request("claude-sonnet-5", vec![user("hi")]);
    let prefix = build_wire(&droid_request(&producer, &thinking, session()))
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
        decode_route(
            Wire::Anthropic,
            decode_stream(stream::iter(chunks)),
            "claude-sonnet-5",
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
    assert_eq!(stored.family, Family::Droid);
    // Feeding the emitted payload back as the journal entry sends the
    // signature on the next wire without its storage-only binding.
    let request = request(
        "claude-sonnet-5",
        vec![
            user("hi"),
            journaled("claude-sonnet-5", stored.item.as_str()),
            user("again"),
        ],
    );
    let built = build_wire(&droid_request(&request, &thinking, session())).unwrap();
    let text = String::from_utf8(built.body).unwrap();
    let body: sonic_rs::Value = sonic_rs::from_str(&text).unwrap();
    assert_eq!(
        body["messages"][1]["content"][0]["signature"].as_str(),
        Some("EqQBCgIYAhIM")
    );
    assert!(!text.contains("dal_prefix"));
}

#[tokio::test]
async fn open_rebinds_shared_anthropic_replay_to_droid() {
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
    let request = request("claude-sonnet-5", vec![user("hi")]);
    let plan = anthropic_plan();
    let input = DroidRequest {
        request: &request,
        thinking: &plan,
        base_url: &base,
        upstream: "fireworks",
        session: session(),
        request_id: "req-9",
        bearer: "workos-token",
        policy: &POLICY,
    };
    let wire = build_wire(&input).unwrap();
    assert!(
        wire.replay_prefix.is_some(),
        "the producing request binds the replay it will decode"
    );
    let stream = open(&crate::http::build_client(), wire).await.unwrap();
    let events = drain(stream).await;
    let replay = events
        .iter()
        .find_map(|event| match event {
            StreamEvent::Replay { payload } => Some(payload),
            _ => None,
        })
        .expect("the signed thinking block replays");
    // The shared decoder names its own family; the proxy lifecycle rebinds
    // the payload to the family the turn is journaled under.
    assert_eq!(replay.family, Family::Droid);
    assert_eq!(replay.model.as_ref(), "claude-sonnet-5");
    assert!(replay.item.as_str().contains("EqQBCgIYAhIM"));
    assert!(replay.item.as_str().contains("dal_prefix"));
    servers.abort_all();
}
