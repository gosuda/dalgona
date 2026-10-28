use dal_core::{CallId, ModelToolSpec, Purpose, ReplaySource, RequestParams, ThinkingLevel};
use futures::executor::block_on;
use sonic_rs::{JsonContainerTrait, JsonValueTrait};

use super::*;
use crate::sse::decode_stream;

fn raw(text: &str) -> RawJson {
    RawJson::parse(text).unwrap()
}

fn replay_source() -> ReplaySource {
    ReplaySource {
        family: Family::Chat,
        model: "gpt-6-astra".into(),
    }
}

fn png() -> Part {
    Part::Image {
        mime: "image/png".into(),
        bytes: b"\x89PNG".as_slice().into(),
    }
}

fn text(value: &str) -> Part {
    Part::Text { text: value.into() }
}

fn plan(effort: Option<&'static str>, temperature: Option<f32>) -> ThinkingPlan {
    ThinkingPlan {
        level: ThinkingLevel::High,
        wire: WireThinking::OpenAi { effort },
        temperature,
        notices: Vec::new(),
    }
}

fn request(tools: Vec<ModelToolSpec>, context: Vec<ContextItem>) -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::Chat,
            model: "gpt-6-astra".into(),
            provider: None,
        },
        system: "You are terse.".into(),
        tools: tools.into(),
        context: context.into(),
        params: RequestParams {
            thinking: ThinkingLevel::High,
            effort: None,
            temperature: None,
            max_output_tokens: None,
        },
        cache_key: Some("0190-session".into()),
    }
}

fn body_with(request: &ModelRequest, thinking: &ThinkingPlan, dialect: Dialect) -> String {
    String::from_utf8(request_body(request, thinking, dialect).unwrap()).unwrap()
}

fn body_text(request: &ModelRequest, thinking: &ThinkingPlan) -> String {
    body_with(request, thinking, Dialect::STANDARD)
}

#[test]
fn request_body_matches_the_member_order_byte_for_byte() {
    let request = request(
        vec![
            ModelToolSpec {
                name: "read".into(),
                description: "Read a file.".into(),
                parameters: raw(r#"{"type":"object","properties":{"path":{"type":"string"}}}"#),
                grammar: None,
                defer_loading: false,
            },
            ModelToolSpec {
                name: "list".into(),
                description: "List files.".into(),
                parameters: raw(r#"{"type": "object"}"#),
                grammar: None,
                defer_loading: false,
            },
        ],
        vec![
            ContextItem::User {
                parts: vec![text("Describe"), png()],
            },
            ContextItem::Assistant {
                source: replay_source(),
                replay: None,
                parts: vec![
                    AssistantPart::Thinking {
                        text: "hidden".into(),
                        replay: None,
                    },
                    AssistantPart::Text {
                        text: "Checking.".into(),
                        replay: None,
                    },
                    AssistantPart::ToolCall {
                        call: CallId::new("call_1"),
                        name: "read".into(),
                        args: raw(r#"{"path": "a.txt" ,"n":1.50}"#),
                        replay: None,
                    },
                ],
            },
            ContextItem::ToolResult {
                call: CallId::new("call_1"),
                name: "read".into(),
                is_error: false,
                parts: vec![text("line1"), text("line2"), png()],
            },
        ],
    );
    let expected = concat!(
        r#"{"model":"gpt-6-astra","messages":["#,
        r#"{"role":"system","content":"You are terse."},"#,
        r#"{"role":"user","content":[{"type":"text","text":"Describe"},"#,
        r#"{"type":"image_url","image_url":{"url":"data:image/png;base64,iVBORw=="}}]},"#,
        r#"{"role":"assistant","content":"Checking.","tool_calls":[{"id":"call_1","type":"function","#,
        r#""function":{"name":"read","arguments":"{\"path\": \"a.txt\" ,\"n\":1.50}"}}]},"#,
        r#"{"role":"tool","tool_call_id":"call_1","content":"line1\nline2"},"#,
        r#"{"role":"user","content":[{"type":"text","text":"Images from tool call call_1."},"#,
        r#"{"type":"image_url","image_url":{"url":"data:image/png;base64,iVBORw=="}}]}],"#,
        r#""tools":[{"type":"function","function":{"name":"read","description":"Read a file.","#,
        r#""parameters":{"type":"object","properties":{"path":{"type":"string"}}}}},"#,
        r#"{"type":"function","function":{"name":"list","description":"List files.","#,
        r#""parameters":{"type": "object"}}}],"#,
        r#""tool_choice":"auto","parallel_tool_calls":true,"stream":true,"#,
        r#""stream_options":{"include_usage":true},"reasoning_effort":"high","#,
        r#""prompt_cache_key":"0190-session"}"#,
    );
    assert_eq!(body_text(&request, &plan(Some("high"), None)), expected);
}

#[test]
fn request_body_without_tools_omits_tool_members_and_unsent_parameters() {
    let mut request = request(
        Vec::new(),
        vec![
            ContextItem::User {
                parts: vec![text("hi")],
            },
            ContextItem::Assistant {
                source: replay_source(),
                replay: None,
                parts: vec![AssistantPart::Thinking {
                    text: "only reasoning".into(),
                    replay: Some(raw("{}")),
                }],
            },
        ],
    );
    request.cache_key = None;
    assert_eq!(
        body_text(&request, &plan(None, Some(0.5))),
        concat!(
            r#"{"model":"gpt-6-astra","messages":[{"role":"system","content":"You are terse."},"#,
            r#"{"role":"user","content":[{"type":"text","text":"hi"}]}],"stream":true,"#,
            r#""stream_options":{"include_usage":true},"temperature":0.5}"#,
        )
    );
}

#[test]
fn request_body_rejects_what_chat_cannot_carry() {
    let blob_id = BlobId::from_bytes(b"stored");
    let blob = request(
        Vec::new(),
        vec![ContextItem::User {
            parts: vec![Part::Blob {
                blob_id,
                mime: "image/png".into(),
                bytes: 6,
            }],
        }],
    );
    assert_eq!(
        request_body(&blob, &plan(None, None), Dialect::STANDARD),
        Err(ChatBodyError::UnresolvedBlob { blob_id })
    );

    let mut other = request(Vec::new(), Vec::new());
    other.model = ModelRoute::Api {
        family: Family::Responses,
        model: "gpt-6-astra".into(),
        provider: None,
    };
    assert_eq!(
        request_body(&other, &plan(None, None), Dialect::STANDARD),
        Err(ChatBodyError::NotChat {
            route: "gpt-6-astra".into()
        })
    );

    let anthropic = ThinkingPlan {
        wire: WireThinking::Anthropic {
            thinking: crate::thinking::AnthropicThinking::Adaptive,
            effort: None,
        },
        ..plan(None, None)
    };
    assert_eq!(
        request_body(
            &request(Vec::new(), Vec::new()),
            &anthropic,
            Dialect::STANDARD
        ),
        Err(ChatBodyError::ForeignThinking)
    );
}

fn capped_request(cap: Option<u32>) -> ModelRequest {
    let mut request = request(
        Vec::new(),
        vec![ContextItem::User {
            parts: vec![text("hi")],
        }],
    );
    request.params.max_output_tokens = cap;
    request
}

#[test]
fn a_requested_cap_is_written_in_the_member_the_dialect_names() {
    let deepseek_row = Dialect::of("custom", "https://api.deepseek.com/v1", "any-model");
    for (dialect, member, other) in [
        (
            Dialect::STANDARD,
            r#""max_completion_tokens":2048"#,
            r#""max_tokens""#,
        ),
        (
            Dialect::LEGACY_TOKENS,
            r#""max_tokens":2048"#,
            r#""max_completion_tokens""#,
        ),
        (
            Dialect::of("cerebras", "", "gpt-oss-120b"),
            r#""max_completion_tokens":2048"#,
            r#""max_tokens""#,
        ),
        (
            Dialect::of("moonshot", "", "kimi-k3"),
            r#""max_tokens":2048"#,
            r#""max_completion_tokens""#,
        ),
        (
            deepseek_row,
            r#""max_tokens":2048"#,
            r#""max_completion_tokens""#,
        ),
    ] {
        let body = body_with(&capped_request(Some(2048)), &plan(None, None), dialect);
        assert!(body.contains(member), "{body}");
        assert!(!body.contains(other), "{body}");
        // A request that asks for no cap writes neither member.
        let body = body_with(&capped_request(None), &plan(None, None), dialect);
        assert!(!body.contains("max_completion_tokens"), "{body}");
        assert!(!body.contains("max_tokens"), "{body}");
    }
}

#[test]
fn cerebras_sends_no_stream_options_and_the_reference_body_keeps_them() {
    let request = capped_request(None);
    let cerebras = Dialect::of("cerebras", "", "gpt-oss-120b");
    let body = body_with(&request, &plan(None, None), cerebras);
    assert!(!body.contains("stream_options"), "{body}");
    let body = body_with(&request, &plan(None, None), Dialect::STANDARD);
    assert!(
        body.contains(r#""stream_options":{"include_usage":true}"#),
        "{body}"
    );
}

fn off_plan() -> ThinkingPlan {
    ThinkingPlan {
        level: ThinkingLevel::Off,
        ..plan(Some("none"), None)
    }
}

#[test]
fn the_dialect_spells_the_thinking_plan_in_its_own_members() {
    let request = capped_request(None);
    let on = plan(Some("high"), None);
    let silent = plan(None, None);
    let toggle = Dialect::of("xiaomi-token-plan-cn", "", "mimo-v2.6-pro");
    let always_on = Dialect::of("xiaomi-token-plan-cn", "", "mimo-v2.5-pro");

    // A toggle writes the switch and no effort; off is `disabled`, and a plan
    // that sends nothing writes nothing.
    let body = body_with(&request, &on, toggle);
    assert!(body.contains(r#""thinking":{"type":"enabled"}"#), "{body}");
    assert!(!body.contains("reasoning_effort"), "{body}");
    let body = body_with(&request, &off_plan(), toggle);
    assert!(body.contains(r#""thinking":{"type":"disabled"}"#), "{body}");
    assert!(!body.contains("reasoning_effort"), "{body}");
    let body = body_with(&request, &silent, toggle);
    assert!(!body.contains("thinking"), "{body}");

    // An always-on model writes the switch with its effort, and never
    // `disabled` or `none`, whatever the plan says at off.
    let body = body_with(&request, &on, always_on);
    assert!(
        body.contains(r#""reasoning_effort":"high","thinking":{"type":"enabled"}"#),
        "{body}"
    );
    for plan in [off_plan(), silent] {
        let body = body_with(&request, &plan, always_on);
        assert!(!body.contains("thinking"), "{body}");
        assert!(!body.contains("reasoning_effort"), "{body}");
    }

    // The reference dialect keeps the effort and no switch.
    let body = body_with(&request, &on, Dialect::STANDARD);
    assert!(body.contains(r#""reasoning_effort":"high""#), "{body}");
    assert!(!body.contains("thinking"), "{body}");
}

fn thought(value: &str) -> AssistantPart {
    AssistantPart::Thinking {
        text: value.into(),
        replay: None,
    }
}

fn said(value: &str) -> AssistantPart {
    AssistantPart::Text {
        text: value.into(),
        replay: None,
    }
}

fn called(id: &str) -> AssistantPart {
    AssistantPart::ToolCall {
        call: CallId::new(id),
        name: "read".into(),
        args: raw("{}"),
        replay: None,
    }
}

fn assistant_of(source: ReplaySource, parts: Vec<AssistantPart>) -> ContextItem {
    ContextItem::Assistant {
        source,
        replay: None,
        parts,
    }
}

fn result_of(id: &str) -> ContextItem {
    ContextItem::ToolResult {
        call: CallId::new(id),
        name: "read".into(),
        is_error: false,
        parts: vec![text("ok")],
    }
}

/// Five assistant messages: the model's own tool turn with reasoning, its own
/// tool turn without any, another model's tool turn with reasoning, its own
/// plain answer, and its own answer with reasoning.
fn reasoning_history() -> Vec<ContextItem> {
    let other = ReplaySource {
        family: Family::Chat,
        model: "another-model".into(),
    };
    vec![
        ContextItem::User {
            parts: vec![text("go")],
        },
        assistant_of(replay_source(), vec![thought("Weigh it."), called("c1")]),
        result_of("c1"),
        assistant_of(replay_source(), vec![called("c2")]),
        result_of("c2"),
        assistant_of(other, vec![thought("Foreign."), called("c3")]),
        result_of("c3"),
        assistant_of(replay_source(), vec![said("Plain.")]),
        assistant_of(replay_source(), vec![thought("Then this."), said("Last.")]),
    ]
}

/// The `reasoning_content` member of each assistant message, in order.
fn replayed(dialect: Dialect) -> Vec<Option<String>> {
    let body = body_with(
        &request(Vec::new(), reasoning_history()),
        &plan(None, None),
        dialect,
    );
    let body: sonic_rs::Value = sonic_rs::from_str(&body).unwrap();
    body.get("messages")
        .and_then(|messages| messages.as_array())
        .unwrap()
        .iter()
        .filter(|message| message.get("role").and_then(|role| role.as_str()) == Some("assistant"))
        .map(|message| {
            message
                .get("reasoning_content")
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        })
        .collect()
}

#[test]
fn reasoning_travels_back_as_the_dialect_says_and_foreign_reasoning_never_does() {
    let own = |value: &str| Some(value.to_owned());
    assert_eq!(replayed(Dialect::STANDARD), [None, None, None, None, None]);
    // Own reasoning where it exists, and an empty member on a tool turn
    // without any (own or another model's); a plain answer carries none.
    let on_calls = [own("Weigh it."), own(""), own(""), None, own("Then this.")];
    assert_eq!(replayed(Dialect::of("moonshot", "", "kimi-k3")), on_calls);
    assert_eq!(
        replayed(Dialect::of("xiaomi", "", "mimo-v2.6-flash")),
        on_calls
    );
    assert_eq!(
        replayed(Dialect::of("xiaomi-token-plan-cn", "", "mimo-v2.5")),
        on_calls
    );
    // Own reasoning only where it exists, no invented member.
    assert_eq!(
        replayed(Dialect::of("moonshot", "", "kimi-k2.6")),
        [own("Weigh it."), None, None, None, own("Then this.")]
    );
}

#[test]
fn an_unlisted_mimo_id_and_an_unlisted_host_keep_the_reference_dialect() {
    assert_eq!(
        Dialect::of("xiaomi", "", "mimo-v3-flash"),
        Dialect::STANDARD
    );
    assert_eq!(
        Dialect::of("openai", "https://api.openai.com/v1", "gpt-6-astra"),
        Dialect::STANDARD
    );
}

fn schema_request(parameters: &str) -> ModelRequest {
    request(
        vec![ModelToolSpec {
            name: "read".into(),
            description: "Reads.".into(),
            parameters: raw(parameters),
            grammar: None,
            defer_loading: false,
        }],
        Vec::new(),
    )
}

#[test]
fn moonshot_lowers_the_schema_and_keeps_every_number_and_data_name() {
    let moonshot = Dialect::of("moonshot", "", "kimi-k3");
    let schema = r#"{"type":"object","title":"T","properties":{"format":{"type":"string","format":"date"},"n":{"type":"integer","minimum":18446744073709551617,"maximum":1e3},"d":{"default":{"format":"raw"}}}}"#;
    let body = body_with(&schema_request(schema), &plan(None, None), moonshot);
    assert!(!body.contains(r#""title""#), "{body}");
    assert!(!body.contains(r#""date""#), "{body}");
    // The property named `format` stays, without its own `format` keyword.
    assert!(body.contains(r#""format":{"type":"string"}"#), "{body}");
    // Numbers keep their exact text, and `default` data is not a schema.
    assert!(body.contains("18446744073709551617"), "{body}");
    assert!(body.contains("1e3"), "{body}");
    assert!(body.contains(r#""default":{"format":"raw"}"#), "{body}");
    // The reference dialect sends the schema untouched.
    let body = body_with(
        &schema_request(schema),
        &plan(None, None),
        Dialect::STANDARD,
    );
    assert!(body.contains(schema), "{body}");
}

#[test]
fn moonshot_schemas_with_nothing_to_strip_keep_their_bytes() {
    let moonshot = Dialect::of("moonshot", "", "kimi-k3");
    let body = body_with(
        &schema_request(r#"{"type": "object"}"#),
        &plan(None, None),
        moonshot,
    );
    assert!(
        body.contains(r#""parameters":{"type": "object"}"#),
        "{body}"
    );
}

fn nested_any_of(levels: usize) -> String {
    let mut schema = String::from(r#"{"type":"string"}"#);
    for _ in 0..levels {
        schema = format!(r#"{{"anyOf":[{schema}]}}"#);
    }
    schema
}

#[test]
fn a_moonshot_schema_nested_past_the_limit_fails_naming_the_tool() {
    let moonshot = Dialect::of("moonshot", "", "kimi-k3");
    let deep = nested_any_of(70);
    let error = request_body(&schema_request(&deep), &plan(None, None), moonshot).unwrap_err();
    assert!(
        matches!(
            &error,
            ChatBodyError::Encode { message }
                if message.contains("tool `read`") && message.contains("128")
        ),
        "{error:?}"
    );
    // Within the limit with nothing to strip, the text is sent as it came,
    // and the reference dialect never refuses depth.
    let shallow = nested_any_of(30);
    let body = body_with(&schema_request(&shallow), &plan(None, None), moonshot);
    assert!(body.contains(&shallow), "{body}");
    let body = body_with(&schema_request(&deep), &plan(None, None), Dialect::STANDARD);
    assert!(body.contains(&deep), "{body}");
}

fn sse(datas: &[&str]) -> String {
    use std::fmt::Write as _;
    datas.iter().fold(String::new(), |mut wire, data| {
        let _ = write!(wire, "data: {data}\n\n");
        wire
    })
}

/// Runs `wire` through the SSE framing and the Chat decoder, delivered
/// in network chunks of `split` bytes.
fn run(wire: &str, split: usize) -> (Vec<StreamEvent>, Option<ProviderError>) {
    let chunks: Vec<Vec<u8>> = wire.as_bytes().chunks(split).map(<[u8]>::to_vec).collect();
    let decoded = decode_events(
        decode_stream(stream::iter(chunks)),
        ChatDecoder::new("zenmux", "openai/gpt-5.6-luna"),
    );
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

fn done(calls: Vec<ToolCall>, usage: Usage, reason: StopReason) -> Vec<StreamEvent> {
    vec![
        StreamEvent::ToolCallsDone { calls },
        StreamEvent::Usage { usage },
        StreamEvent::Stop { reason },
    ]
}

const TEXT_TURN: [&str; 5] = [
    r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","created":1694268190,"model":"gpt-6-astra", "system_fingerprint": "fp_44709d6fcb", "choices":[{"index":0,"delta":{"role":"assistant","content":""},"logprobs":null,"finish_reason":null}],"obfuscation":"r4N7vQ2m","usage":null}"#,
    r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","created":1694268190,"model":"gpt-6-astra", "system_fingerprint": "fp_44709d6fcb", "choices":[{"index":0,"delta":{"content":"Hello"},"logprobs":null,"finish_reason":null}],"obfuscation":"p9K3xT6w","usage":null}"#,
    r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","created":1694268190,"model":"gpt-6-astra", "system_fingerprint": "fp_44709d6fcb", "choices":[{"index":0,"delta":{},"logprobs":null,"finish_reason":"stop"}],"obfuscation":"","usage":null}"#,
    r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","created":1694268190,"model":"gpt-6-astra","choices":[],"usage":{"prompt_tokens":19,"completion_tokens":10,"total_tokens":29,"prompt_tokens_details":{"cached_tokens":4,"audio_tokens":0},"completion_tokens_details":{"reasoning_tokens":3,"audio_tokens":0}}}"#,
    "[DONE]",
];

#[test]
fn text_turn_decodes_over_any_network_split() {
    let wire = sse(&TEXT_TURN);
    let mut expected = vec![StreamEvent::TextDelta {
        text: "Hello".into(),
    }];
    expected.extend(done(
        Vec::new(),
        Usage {
            input_tokens: 19,
            cached_input_tokens: 4,
            output_tokens: 7,
            reasoning_tokens: Some(3),
            cache_write_tokens: 0,
            cost_usd: None,
            premium_requests: None,
        },
        StopReason::EndTurn,
    ));
    for split in [1, 2, 7, 64, wire.len()] {
        let (events, error) = run(&wire, split);
        assert!(error.is_none(), "split {split}: {error:?}");
        assert_eq!(events, expected, "split {split}");
    }
}

#[test]
fn tool_turn_keeps_raw_argument_bytes_and_the_first_id() {
    let wire = sse(&[
        r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":null,"tool_calls":[{"index":0,"id":"call_abc123","type":"function","function":{"name":"get_current_weather","arguments":""}}]},"finish_reason":null}],"usage":null}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\n\""}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"location"}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_late","function":{"name":"other","arguments":"\": \"Bos"}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"ton, MA\""}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\n}"}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],"usage":null}"#,
        r#"{"choices":[],"usage":{"prompt_tokens":82,"completion_tokens":17,"total_tokens":99}}"#,
        "[DONE]",
    ]);
    let id = String::from("call_abc123");
    let mut expected = vec![StreamEvent::ToolCallStarted {
        id: id.clone(),
        name: "get_current_weather".into(),
    }];
    for fragment in ["{\n\"", "location", "\": \"Bos", "ton, MA\"", "\n}"] {
        expected.push(StreamEvent::ToolArgsDelta {
            id: id.clone(),
            fragment: fragment.as_bytes().to_vec(),
        });
    }
    expected.extend(done(
        vec![ToolCall {
            id,
            name: "get_current_weather".into(),
            args: ToolArgs::Parsed(raw("{\n\"location\": \"Boston, MA\"\n}")),
        }],
        Usage {
            input_tokens: 82,
            output_tokens: 17,
            ..NO_USAGE
        },
        StopReason::ToolUse,
    ));
    for split in [3, wire.len()] {
        let (events, error) = run(&wire, split);
        assert!(error.is_none(), "{error:?}");
        assert_eq!(events, expected);
    }
}

#[test]
fn parallel_calls_close_in_index_order() {
    let wire = sse(&[
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"call_b","type":"function","function":{"name":"b","arguments":""}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"a","arguments":"{\"x\":"}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"arguments":"{\"y\":2}"}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"1}"}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        "[DONE]",
    ]);
    let (events, error) = run(&wire, wire.len());
    assert!(error.is_none(), "{error:?}");
    assert_eq!(
        events[events.len() - 3..],
        done(
            vec![
                ToolCall {
                    id: "call_a".into(),
                    name: "a".into(),
                    args: ToolArgs::Parsed(raw(r#"{"x":1}"#)),
                },
                ToolCall {
                    id: "call_b".into(),
                    name: "b".into(),
                    args: ToolArgs::Parsed(raw(r#"{"y":2}"#)),
                },
            ],
            NO_USAGE,
            StopReason::ToolUse,
        )
    );
}

#[test]
fn end_of_input_after_a_finish_reason_stops_with_zero_usage() {
    let wire = sse(&TEXT_TURN[..3]);
    let (events, error) = run(&wire, 5);
    assert!(error.is_none(), "{error:?}");
    let mut expected = vec![StreamEvent::TextDelta {
        text: "Hello".into(),
    }];
    expected.extend(done(Vec::new(), NO_USAGE, StopReason::EndTurn));
    assert_eq!(events, expected);
}

#[test]
fn end_of_input_before_a_finish_reason_is_a_cut() {
    let (events, error) = run(&sse(&TEXT_TURN[..2]), 9);
    assert_eq!(
        events,
        vec![StreamEvent::TextDelta {
            text: "Hello".into()
        }]
    );
    let error = error.unwrap();
    assert!(matches!(error, ProviderError::StreamCut));
    assert_eq!(error.to_string(), "stream cut off before completion.");
}

fn single_call(arguments: &str, finish: &str) -> ToolArgs {
    let open = format!(
        r#"{{"choices":[{{"index":0,"delta":{{"tool_calls":[{{"index":0,"id":"call_1","function":{{"name":"f","arguments":{arguments}}}}}]}},"finish_reason":null}}]}}"#
    );
    let finish =
        format!(r#"{{"choices":[{{"index":0,"delta":{{}},"finish_reason":"{finish}"}}]}}"#);
    let (events, error) = run(&sse(&[&open, &finish, "[DONE]"]), 11);
    assert!(error.is_none(), "{error:?}");
    let Some(StreamEvent::ToolCallsDone { calls }) = events.iter().rev().nth(2) else {
        panic!("no ToolCallsDone in {events:?}");
    };
    assert_eq!(calls.len(), 1);
    calls[0].args.clone()
}

#[test]
fn final_arguments_parse_once_by_the_stop_reason() {
    let ToolArgs::Invalid { message } = single_call(r#""{\"a\":""#, "tool_calls") else {
        panic!("unparsable arguments must be Invalid");
    };
    assert_ne!(message, "");
    assert_eq!(
        single_call(r#""""#, "tool_calls"),
        ToolArgs::Parsed(raw("{}"))
    );
    assert_eq!(
        single_call("null", "tool_calls"),
        ToolArgs::Parsed(raw("{}"))
    );
    assert_eq!(single_call(r#""{\"a\":1}""#, "length"), ToolArgs::Truncated);
}

#[test]
fn only_choice_zero_reasoning_and_refusal_reach_the_stream() {
    let wire = sse(&[
        r#"{"choices":[{"index":1,"delta":{"content":"B"},"finish_reason":"stop"},{"index":0,"delta":{"reasoning_content":"think","content":"A"},"finish_reason":null}],"usage":null}"#,
        r#"{"choices":[{"index":0,"delta":{"reasoning":"more","refusal":"I can't help with that."},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{},"finish_reason":"content_filter"}]}"#,
        r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        "[DONE]",
    ]);
    let (events, error) = run(&wire, wire.len());
    assert!(error.is_none(), "{error:?}");
    let mut expected = vec![
        StreamEvent::ReasoningDelta {
            text: "think".into(),
        },
        StreamEvent::TextDelta { text: "A".into() },
        StreamEvent::ReasoningDelta {
            text: "more".into(),
        },
        StreamEvent::TextDelta {
            text: "I can't help with that.".into(),
        },
    ];
    expected.extend(done(Vec::new(), NO_USAGE, StopReason::Refusal));
    assert_eq!(events, expected);
}

#[test]
fn in_stream_errors_and_malformed_chunks_end_the_stream() {
    let (events, error) = run(
        &sse(&[
            TEXT_TURN[1],
            r#"{"error":{"message":"Rate limit reached","type":"requests","code":"rate_limit_exceeded"}}"#,
            TEXT_TURN[2],
        ]),
        13,
    );
    assert_eq!(events.len(), 1);
    assert!(matches!(
        error,
        Some(ProviderError::RateLimited { ref message, retry_after: None }) if message == "Rate limit reached"
    ));

    let (_, error) = run(&sse(&[r#"{"error":{"message":"busy","code":503}}"#]), 64);
    assert!(matches!(
        error,
        Some(ProviderError::Status { family: Family::Chat, status: 200, ref message }) if message == "busy"
    ));

    let (events, error) = run(&sse(&["not json", "[DONE]"]), 64);
    assert_eq!(events, []);
    assert!(matches!(
        error,
        Some(ProviderError::Protocol {
            family: Family::Chat,
            ..
        })
    ));

    let (_, error) = run(
        &sse(&[
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{}"}}]},"finish_reason":null}]}"#,
        ]),
        64,
    );
    assert!(matches!(error, Some(ProviderError::Protocol { .. })));
}

#[test]
fn done_without_a_finish_reason_keeps_open_calls_as_tool_use() {
    let mut decoder = ChatDecoder::new("openai", "gpt-6-astra");
    let mut out = Vec::new();
    let open = r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"f","arguments":"{}"}}]},"finish_reason":null}]}"#;
    assert_eq!(decoder.feed(open, &mut out).unwrap(), Progress::Open);
    assert_eq!(decoder.feed("[DONE]", &mut out).unwrap(), Progress::Ended);
    assert_eq!(
        out.last(),
        Some(&StreamEvent::Stop {
            reason: StopReason::ToolUse
        })
    );
}

#[test]
fn the_decoder_emits_one_terminal_and_ignores_later_input() {
    let mut decoder = ChatDecoder::new("openai", "gpt-6-astra");
    let mut out = Vec::new();
    assert_eq!(
        decoder.feed(TEXT_TURN[1], &mut out).unwrap(),
        Progress::Open
    );
    assert_eq!(decoder.feed("[DONE]", &mut out).unwrap(), Progress::Ended);
    assert_eq!(out.len(), 4);
    assert_eq!(
        decoder.feed(TEXT_TURN[1], &mut out).unwrap(),
        Progress::Ended
    );
    assert_eq!(decoder.feed("[DONE]", &mut out).unwrap(), Progress::Ended);
    decoder.end(&mut out).unwrap();
    assert_eq!(out.len(), 4);
    assert_eq!(
        out.last(),
        Some(&StreamEvent::Stop {
            reason: StopReason::EndTurn
        })
    );
}

#[test]
fn request_body_maps_mapped_tool_names_for_advertisements_and_history() {
    let request = request(
        vec![ModelToolSpec {
            name: "deploy.web-x.list".into(),
            description: "Mapped tool.".into(),
            parameters: raw(r#"{"type":"object","properties":{}}"#),
            grammar: None,
            defer_loading: false,
        }],
        vec![ContextItem::Assistant {
            source: replay_source(),
            replay: None,
            parts: vec![AssistantPart::ToolCall {
                call: CallId::new("call-1"),
                name: "deploy.web-x.list".into(),
                args: raw("{}"),
                replay: None,
            }],
        }],
    );
    let body = body_text(&request, &plan(None, None));
    assert_eq!(
        body.matches(r#""name":"deploy_web-x_list_41588e56""#)
            .count(),
        2
    );
    assert!(!body.contains("deploy.web-x.list"));
}

#[test]
fn request_body_ignores_text_tool_and_message_replay() {
    let payload = || Some(raw(r#"{"foreign":"payload"}"#));
    let plain = request(
        Vec::new(),
        vec![ContextItem::Assistant {
            source: replay_source(),
            replay: None,
            parts: vec![
                AssistantPart::Text {
                    text: "Hi.".into(),
                    replay: None,
                },
                AssistantPart::ToolCall {
                    call: CallId::new("call-9"),
                    name: "read".into(),
                    args: raw("{}"),
                    replay: None,
                },
            ],
        }],
    );
    let laden = request(
        Vec::new(),
        vec![ContextItem::Assistant {
            source: replay_source(),
            replay: payload(),
            parts: vec![
                AssistantPart::Text {
                    text: "Hi.".into(),
                    replay: payload(),
                },
                AssistantPart::ToolCall {
                    call: CallId::new("call-9"),
                    name: "read".into(),
                    args: raw("{}"),
                    replay: payload(),
                },
            ],
        }],
    );
    assert_eq!(
        body_text(&laden, &plan(None, None)),
        body_text(&plain, &plan(None, None))
    );
}

#[test]
fn the_full_path_runs_through_the_event_stream_guard() {
    let chunks = vec![sse(&TEXT_TURN).into_bytes()];
    let mut events = crate::stream::EventStream::new(
        decode_events(
            decode_stream(stream::iter(chunks)),
            ChatDecoder::new("openai", "gpt-6-astra"),
        ),
        || panic!("a clean Stop must not cancel"),
    );
    let seen: Vec<_> = block_on(async {
        let mut seen = Vec::new();
        while let Some(item) = events.next().await {
            seen.push(item.unwrap());
        }
        seen
    });
    assert_eq!(seen.len(), 4);
    assert!(matches!(seen.last(), Some(StreamEvent::Stop { .. })));
}

#[test]
fn before_turn_text_follows_its_user_message_in_the_body() {
    let request = request(
        Vec::new(),
        vec![
            ContextItem::User {
                parts: vec![text("question")],
            },
            ContextItem::User {
                parts: vec![text("first\nsecond")],
            },
        ],
    );
    let body = body_text(&request, &plan(Some("high"), None));
    assert!(
        body.contains(concat!(
            r#"{"role":"user","content":[{"type":"text","text":"question"}]},"#,
            r#"{"role":"user","content":[{"type":"text","text":"first\nsecond"}]}"#,
        )),
        "{body}"
    );
}

#[test]
fn cumulative_tool_arguments_stop_at_the_stream_event_budget() {
    let mut decoder = ChatDecoder::new("untrusted", "model");
    let data = r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call","function":{"name":"run","arguments":"x"}}]}}]}"#;
    let mut out = Vec::new();
    for _ in 0..crate::stream_budget::STREAM_EVENTS_LIMIT {
        assert!(decoder.feed(data, &mut out).is_ok());
        out.clear();
    }
    assert!(matches!(
        decoder.feed(data, &mut out),
        Err(ProviderError::Limit(crate::error::LimitError::StreamEvents))
    ));
    assert_eq!(
        decoder.calls.get(&0).map(|call| call.args.len()),
        Some(crate::stream_budget::STREAM_EVENTS_LIMIT)
    );
}

#[test]
fn reasoning_text_is_a_third_reasoning_alias_in_upstream_order() {
    let wire = sse(&[
        r#"{"choices":[{"index":0,"delta":{"reasoning_text":"pondering"}}]}"#,
        r#"{"choices":[{"index":0,"delta":{"reasoning_content":"first","reasoning_text":"second"}}]}"#,
        r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        "[DONE]",
    ]);
    let (events, error) = run(&wire, wire.len());
    assert!(error.is_none(), "{error:?}");
    let mut expected = vec![
        StreamEvent::ReasoningDelta {
            text: "pondering".into(),
        },
        StreamEvent::ReasoningDelta {
            text: "first".into(),
        },
    ];
    expected.extend(done(Vec::new(), NO_USAGE, StopReason::EndTurn));
    assert_eq!(events, expected);
}

#[test]
fn unkeyed_tool_deltas_continue_the_last_spoken_call() {
    let wire = sse(&[
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"a","arguments":""}}]}}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"call_b","type":"function","function":{"name":"b","arguments":""}}]}}]}"#,
        // Argument deltas with neither index nor id fill the last-spoken slot.
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"function":{"arguments":"{\"y\":"}}]}}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"function":{"arguments":"2}"}}]}}]}"#,
        r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        "[DONE]",
    ]);
    let (events, error) = run(&wire, wire.len());
    assert!(error.is_none(), "{error:?}");
    assert_eq!(
        events[events.len() - 3..],
        done(
            vec![
                ToolCall {
                    id: "call_a".into(),
                    name: "a".into(),
                    args: ToolArgs::Parsed(raw("{}")),
                },
                ToolCall {
                    id: "call_b".into(),
                    name: "b".into(),
                    args: ToolArgs::Parsed(raw(r#"{"y":2}"#)),
                },
            ],
            NO_USAGE,
            StopReason::ToolUse,
        )
    );
}

#[test]
fn unkeyed_tool_deltas_resume_by_id_and_open_unseen_ids() {
    let wire = sse(&[
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"a","arguments":""}}]}}]}"#,
        // No index, unseen id: opens the slot after the highest open one.
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"id":"call_b","type":"function","function":{"name":"b","arguments":""}}]}}]}"#,
        // No index, no id: continues the last-spoken call.
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"function":{"arguments":"{\"b\":1}"}}]}}]}"#,
        // No index, known id: resumes that call wherever it was opened.
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"id":"call_a","function":{"arguments":"{\"a\":2}"}}]}}]}"#,
        r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        "[DONE]",
    ]);
    let (events, error) = run(&wire, wire.len());
    assert!(error.is_none(), "{error:?}");
    assert_eq!(
        events[events.len() - 3..],
        done(
            vec![
                ToolCall {
                    id: "call_a".into(),
                    name: "a".into(),
                    args: ToolArgs::Parsed(raw(r#"{"a":2}"#)),
                },
                ToolCall {
                    id: "call_b".into(),
                    name: "b".into(),
                    args: ToolArgs::Parsed(raw(r#"{"b":1}"#)),
                },
            ],
            NO_USAGE,
            StopReason::ToolUse,
        )
    );
}

#[test]
fn an_unkeyed_tool_delta_without_an_open_call_is_a_protocol_error() {
    let (events, error) = run(
        &sse(&[
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"function":{"arguments":"{}"}}]}}]}"#,
        ]),
        64,
    );
    assert_eq!(events, []);
    assert!(matches!(error, Some(ProviderError::Protocol { .. })));
}

/// The `Usage` event of the response the decoder produced for `wire`.
fn usage_of(wire: &str) -> Usage {
    let (events, error) = run(wire, wire.len());
    assert!(error.is_none(), "{error:?}");
    events
        .iter()
        .find_map(|event| match event {
            StreamEvent::Usage { usage } => Some(*usage),
            _ => None,
        })
        .expect("a finished response carries one usage")
}

#[test]
fn finish_reasons_map_to_stop_reasons_and_unknown_ones_pass_through_verbatim() {
    for (finish, expected) in [
        ("stop", StopReason::EndTurn),
        ("length", StopReason::MaxTokens),
        ("content_filter", StopReason::Refusal),
        ("function_call", StopReason::Other("function_call".into())),
        ("STOP", StopReason::Other("STOP".into())),
        ("length ", StopReason::Other("length ".into())),
    ] {
        let chunk = format!(
            r#"{{"choices":[{{"index":0,"delta":{{"content":"x"}},"finish_reason":"{finish}"}}]}}"#
        );
        let (events, error) = run(&sse(&[chunk.as_str(), "[DONE]"]), 64);
        assert!(error.is_none(), "{finish:?}: {error:?}");
        assert_eq!(
            events.last(),
            Some(&StreamEvent::Stop { reason: expected }),
            "{finish:?}"
        );
    }
}

#[test]
fn reasoning_beyond_the_completion_count_leaves_zero_output_and_cache_writes_survive() {
    // (completion, reasoning, output): below, at, and above the boundary
    // where reasoning would consume the whole completion count.
    for (completion, reasoning, output) in [(3_u64, 9_u64, 0_u64), (9, 9, 0), (10, 9, 1)] {
        let chunk = format!(
            r#"{{"choices":[{{"index":0,"delta":{{"content":"x"}},"finish_reason":"stop"}}],"usage":{{"prompt_tokens":10,"completion_tokens":{completion},"prompt_tokens_details":{{"cached_tokens":2,"cache_write_tokens":6}},"completion_tokens_details":{{"reasoning_tokens":{reasoning}}}}}}}"#
        );
        assert_eq!(
            usage_of(&sse(&[chunk.as_str(), "[DONE]"])),
            Usage {
                input_tokens: 10,
                cached_input_tokens: 2,
                output_tokens: output,
                reasoning_tokens: Some(reasoning),
                cache_write_tokens: 6,
                premium_requests: None,
                cost_usd: None,
            },
            "completion {completion}, reasoning {reasoning}"
        );
    }
    // Without a reasoning split the whole completion count is output.
    let plain = r#"{"choices":[{"index":0,"delta":{"content":"x"},"finish_reason":"stop"}],"usage":{"prompt_tokens":4,"completion_tokens":7}}"#;
    let usage = usage_of(&sse(&[plain, "[DONE]"]));
    assert_eq!((usage.output_tokens, usage.reasoning_tokens), (7, None));
}

fn plain_tool() -> ModelToolSpec {
    ModelToolSpec {
        name: "read".into(),
        description: "Reads.".into(),
        parameters: raw(r#"{"type":"object"}"#),
        grammar: None,
        defer_loading: false,
    }
}

#[test]
fn zai_rows_spell_thinking_alongside_the_effort_and_the_tool_stream() {
    let request = request(vec![plain_tool()], Vec::new());
    let glm53 = Dialect::of(
        "zai",
        "https://api.z.ai/api/coding/paas/v4",
        "glm-5.3-flash",
    );
    let body = body_with(&request, &plan(Some("high"), None), glm53);
    assert!(
        body.contains(r#""thinking":{"type":"enabled","clear_thinking":false}"#),
        "{body}"
    );
    assert!(body.contains(r#""reasoning_effort":"high""#), "{body}");
    assert!(body.contains(r#""tool_stream":true"#), "{body}");
    assert!(body.contains(r#""strict":false"#), "{body}");
    assert!(!body.contains("prompt_cache_key"), "{body}");

    // glm-5.3* cannot be switched off: the off plan still spells enabled and
    // never an effort.
    let body = body_with(&request, &off_plan(), glm53);
    assert!(
        body.contains(r#""thinking":{"type":"enabled","clear_thinking":false}"#),
        "{body}"
    );
    assert!(!body.contains("reasoning_effort"), "{body}");

    // glm-5.2* toggles and spells the effort only beside the enabled switch.
    let glm52 = Dialect::of("zai", "", "glm-5.2");
    let body = body_with(&request, &plan(Some("high"), None), glm52);
    assert!(
        body.contains(r#""thinking":{"type":"enabled","clear_thinking":false}"#),
        "{body}"
    );
    assert!(body.contains(r#""reasoning_effort":"high""#), "{body}");
    let body = body_with(&request, &off_plan(), glm52);
    assert!(body.contains(r#""thinking":{"type":"disabled"}"#), "{body}");
    assert!(!body.contains("reasoning_effort"), "{body}");

    // glm-4.7 toggles without ever spelling an effort.
    let glm47 = Dialect::of("zai-coding-cn", "", "glm-4.7");
    let body = body_with(&request, &plan(Some("high"), None), glm47);
    assert!(
        body.contains(r#""thinking":{"type":"enabled","clear_thinking":false}"#),
        "{body}"
    );
    assert!(!body.contains("reasoning_effort"), "{body}");
    let body = body_with(&request, &off_plan(), glm47);
    assert!(body.contains(r#""thinking":{"type":"disabled"}"#), "{body}");
}

#[test]
fn alibaba_rows_toggle_the_member_their_series_publishes() {
    let request = request(vec![plain_tool()], Vec::new());

    // The qwen rows toggle `enable_thinking` and never an effort member.
    let qwen = Dialect::of(
        "alibaba-token-plan",
        "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
        "qwen3.8-max",
    );
    let body = body_with(&request, &plan(Some("high"), None), qwen);
    assert!(body.contains(r#""enable_thinking":true"#), "{body}");
    assert!(!body.contains("reasoning_effort"), "{body}");
    assert!(!body.contains(r#""thinking""#), "{body}");
    let body = body_with(&request, &off_plan(), qwen);
    assert!(body.contains(r#""enable_thinking":false"#), "{body}");

    // The deepseek- and kimi-format rows toggle `thinking` without an effort.
    let deepseek = Dialect::of("alibaba-token-plan", "", "deepseek-v4-pro");
    let body = body_with(&request, &plan(Some("high"), None), deepseek);
    assert!(body.contains(r#""thinking":{"type":"enabled"}"#), "{body}");
    assert!(!body.contains("reasoning_effort"), "{body}");
    let body = body_with(&request, &off_plan(), deepseek);
    assert!(body.contains(r#""thinking":{"type":"disabled"}"#), "{body}");

    // The glm and MiniMax rows have no switch at any level.
    let glm = Dialect::of("alibaba-token-plan", "", "glm-5");
    let body = body_with(&request, &plan(Some("high"), None), glm);
    assert!(!body.contains(r#""thinking""#), "{body}");
    assert!(!body.contains("reasoning_effort"), "{body}");
    assert!(!body.contains("enable_thinking"), "{body}");
}

#[test]
fn the_s8_rows_cap_write_strict_and_cache_exactly_like_their_source() {
    let capped = capped_request(Some(1024));

    // Bai caps with the reference member and carries no cache key.
    let bai = Dialect::of("bai", "https://api.b.ai/v1", "qwen3.8-max");
    let body = body_with(&capped, &plan(Some("high"), None), bai);
    assert!(body.contains(r#""max_completion_tokens":1024"#), "{body}");
    assert!(!body.contains(r#""max_tokens""#), "{body}");
    assert!(!body.contains("prompt_cache_key"), "{body}");

    // The Cloudflare rows cap with `max_tokens` and send no thinking member
    // even for a reasoning plan.
    let gateway = Dialect::of(
        "cloudflare-ai-gateway",
        "https://gateway.ai.cloudflare.com/v1/account/gateway",
        "workers-ai/@cf/zai-org/glm-5.3",
    );
    let body = body_with(&capped, &plan(Some("high"), None), gateway);
    assert!(body.contains(r#""max_tokens":1024"#), "{body}");
    assert!(!body.contains("max_completion_tokens"), "{body}");
    assert!(!body.contains(r#""thinking""#), "{body}");
    assert!(!body.contains("reasoning_effort"), "{body}");

    // OpenCode's kimi toggle never spells an effort.
    let kimi = Dialect::of("opencode", "https://opencode.ai/zen/v1", "kimi-k2.6");
    let body = body_with(&capped, &plan(Some("high"), None), kimi);
    assert!(body.contains(r#""thinking":{"type":"enabled"}"#), "{body}");
    assert!(!body.contains("reasoning_effort"), "{body}");
    assert!(body.contains(r#""max_tokens":1024"#), "{body}");
}

#[test]
fn bai_rows_rewrite_the_schema_and_strict_while_unknown_hosts_stay_verbatim() {
    let schema = r#"{"optional":true,"anyOf":[{"type":"object","properties":{"a":{"type":"string","default":1.50}},"required":["a"]},{"type":"object","properties":{"b":{"type":"integer"}}}]}"#;
    let body = body_with(
        &schema_request(schema),
        &plan(None, None),
        Dialect::of("bai", "", "qwen3.8-max"),
    );
    // The root union merged into one object schema: both properties, the
    // branch-common `required` (none), the `optional` keyword gone, and every
    // number's exact text.
    assert!(body.contains("1.50"), "{body}");
    assert!(!body.contains("optional"), "{body}");
    let parsed: sonic_rs::Value = sonic_rs::from_str(&body).unwrap();
    let params = parsed
        .get("tools")
        .and_then(|tools| tools.as_array())
        .unwrap()[0]
        .get("function")
        .and_then(|function| function.get("parameters"))
        .unwrap()
        .clone();
    assert_eq!(
        params.get("type").and_then(|value| value.as_str()),
        Some("object")
    );
    assert!(
        params
            .get("properties")
            .and_then(|properties| properties.get("a"))
            .is_some()
    );
    assert!(
        params
            .get("properties")
            .and_then(|properties| properties.get("b"))
            .is_some()
    );
    assert!(params.get("required").is_none());
    assert!(body.contains(r#""strict":false"#), "{body}");

    // The reference dialect sends the schema byte for byte.
    let body = body_with(
        &schema_request(schema),
        &plan(None, None),
        Dialect::STANDARD,
    );
    assert!(body.contains(schema), "{body}");
}

#[test]
fn a_union_root_with_conflicting_properties_forks_them_in_a_combiner() {
    let schema = r#"{"oneOf":[{"type":"object","properties":{"a":{"type":"string"},"c":{"type":"integer"}},"required":["a","c"]},{"type":"object","properties":{"a":{"type":"number"}},"required":["a"]}]}"#;
    let body = body_with(
        &schema_request(schema),
        &plan(None, None),
        Dialect::of("opencode", "", "glm-5.3"),
    );
    // `a` conflicts, so the merge forks it; `c` stays; the required names are
    // the branch-common ones.
    let parsed: sonic_rs::Value = sonic_rs::from_str(&body).unwrap();
    let params = parsed
        .get("tools")
        .and_then(|tools| tools.as_array())
        .unwrap()[0]
        .get("function")
        .and_then(|function| function.get("parameters"))
        .unwrap()
        .clone();
    assert_eq!(
        params.get("type").and_then(|value| value.as_str()),
        Some("object")
    );
    let branches = params
        .get("properties")
        .and_then(|properties| properties.get("a"))
        .and_then(|field| field.get("anyOf"))
        .and_then(|members| members.as_array())
        .unwrap();
    assert_eq!(branches.len(), 2);
    assert!(
        params
            .get("properties")
            .and_then(|properties| properties.get("c"))
            .is_some()
    );
    let required = params
        .get("required")
        .and_then(|value| value.as_array())
        .unwrap();
    assert_eq!(required.len(), 1);
    assert_eq!(required[0].as_str(), Some("a"));
}

#[test]
fn an_openai_compat_schema_nested_past_the_limit_fails_naming_the_tool() {
    let deep = nested_any_of(140);
    let error = request_body(
        &schema_request(&deep),
        &plan(None, None),
        Dialect::of("bai", "", "glm-5.3"),
    )
    .unwrap_err();
    assert!(
        matches!(
            &error,
            ChatBodyError::Encode { message }
                if message.contains("tool `read`") && message.contains("128")
        ),
        "{error:?}"
    );
}

/// The `reasoning_content` member of each assistant message when the
/// conversation was written by `model` itself.
fn replayed_by(provider: &str, model: &str) -> Vec<Option<String>> {
    let own = ReplaySource {
        family: Family::Chat,
        model: model.into(),
    };
    let other = ReplaySource {
        family: Family::Chat,
        model: "another-model".into(),
    };
    let history = vec![
        ContextItem::User {
            parts: vec![text("go")],
        },
        assistant_of(own.clone(), vec![thought("Weigh it."), called("c1")]),
        result_of("c1"),
        assistant_of(own.clone(), vec![called("c2")]),
        result_of("c2"),
        assistant_of(other, vec![thought("Foreign."), called("c3")]),
        result_of("c3"),
        assistant_of(own.clone(), vec![said("Plain.")]),
        assistant_of(own, vec![thought("Then this."), said("Last.")]),
    ];
    let mut request = request(Vec::new(), history);
    request.model = ModelRoute::Api {
        family: Family::Chat,
        model: model.into(),
        provider: None,
    };
    let body = body_with(
        &request,
        &plan(None, None),
        Dialect::of(provider, "", model),
    );
    let body: sonic_rs::Value = sonic_rs::from_str(&body).unwrap();
    body.get("messages")
        .and_then(|messages| messages.as_array())
        .unwrap()
        .iter()
        .filter(|message| message.get("role").and_then(|role| role.as_str()) == Some("assistant"))
        .map(|message| {
            message
                .get("reasoning_content")
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        })
        .collect()
}

#[test]
fn deepseek_format_rows_carry_reasoning_content_on_every_assistant_message() {
    let filled = [
        Some("Weigh it.".to_owned()),
        Some(String::new()),
        Some(String::new()),
        Some(String::new()),
        Some("Then this.".to_owned()),
    ];
    assert_eq!(replayed_by("alibaba-token-plan", "deepseek-v4-pro"), filled);
    assert_eq!(replayed_by("opencode-go", "deepseek-v4.1-flash"), filled);
    assert_eq!(replayed_by("opencode", "deepseek-v4-pro"), filled);

    // The kimi toggle row and the no-switch rows replay nothing.
    let dropped = [None, None, None, None, None];
    assert_eq!(replayed_by("opencode", "kimi-k2.6"), dropped);
    assert_eq!(replayed_by("zai", "glm-5.3-flash"), dropped);
}

/// The `(accepted, none_supported)` ladder a row publishes.
fn ladder_of(provider: &str, model: &str) -> (Vec<ThinkingLevel>, bool) {
    match Dialect::thinking_support(provider, model) {
        Some(crate::thinking::ThinkingSupport::OpenAi {
            accepted,
            none_supported,
        }) => (accepted, none_supported),
        other => panic!("unexpected ladder for {provider}/{model}: {other:?}"),
    }
}

#[test]
fn the_s8_ladders_come_from_the_same_table_as_the_request_members() {
    use ThinkingLevel::{High, Low, Max, Medium, Minimal, Xhigh};
    let every_on_level = vec![Minimal, Low, Medium, High, Xhigh, Max];

    // The Z.AI always-on rows cannot be switched off.
    assert_eq!(
        ladder_of("zai", "glm-5.3-flash"),
        (every_on_level.clone(), false)
    );
    assert_eq!(ladder_of("zai", "glm-5.2"), (every_on_level.clone(), true));
    assert_eq!(
        ladder_of("zai-coding-cn", "glm-4.6v"),
        (every_on_level.clone(), true)
    );
    assert_eq!(ladder_of("zai", "glm-9"), (every_on_level.clone(), true));

    // The toggle rows of the token plan and the Go lane.
    assert_eq!(
        ladder_of("alibaba-token-plan", "qwen3.8-max"),
        (every_on_level.clone(), true)
    );
    assert_eq!(
        ladder_of("alibaba-token-plan", "deepseek-v3.2"),
        (every_on_level.clone(), true)
    );
    assert_eq!(
        ladder_of("opencode-go", "deepseek-v4-pro"),
        (every_on_level.clone(), true)
    );
    assert_eq!(
        ladder_of(
            "cloudflare-workers-ai",
            "@cf/deepseek-ai/deepseek-v4-pro-0813"
        ),
        (every_on_level.clone(), true)
    );

    // The rows without a switch publish no on level at all.
    assert_eq!(ladder_of("bai", "qwen3.8-max"), (Vec::new(), false));
    assert_eq!(
        ladder_of("alibaba-token-plan", "glm-5"),
        (Vec::new(), false)
    );
    assert_eq!(
        ladder_of("opencode", "deepseek-v4-pro"),
        (Vec::new(), false)
    );
    assert_eq!(
        ladder_of("cloudflare-ai-gateway", "workers-ai/@cf/zai-org/glm-5.3"),
        (Vec::new(), false)
    );
}

#[test]
fn bai_lowering_keeps_literal_zero() {
    let dialect = Dialect::of("bai", "", "qwen3.8-max");
    let params_of = |schema: &str| {
        let body = body_with(&schema_request(schema), &plan(None, None), dialect);
        let parsed: sonic_rs::Value = sonic_rs::from_str(&body).unwrap();
        parsed
            .get("tools")
            .and_then(|tools| tools.as_array())
            .unwrap()[0]
            .get("function")
            .and_then(|function| function.get("parameters"))
            .unwrap()
            .clone()
    };
    // `default: 0` stays, with `optional` forcing the rewrite path.
    // `false` and `""` cover the other falsy defaults a truthy check drops.
    let params = params_of(
        r#"{"optional":true,"type":"object","properties":{"count":{"type":"integer","default":0},"flag":{"type":"boolean","default":false},"name":{"type":"string","default":""}}}"#,
    );
    assert_eq!(
        params
            .get("properties")
            .and_then(|properties| properties.get("count"))
            .and_then(|count| count.get("default"))
            .and_then(sonic_rs::JsonValueTrait::as_u64),
        Some(0)
    );
    assert_eq!(
        params
            .get("properties")
            .and_then(|properties| properties.get("flag"))
            .and_then(|flag| flag.get("default"))
            .and_then(sonic_rs::JsonValueTrait::as_bool),
        Some(false)
    );
    assert_eq!(
        params
            .get("properties")
            .and_then(|properties| properties.get("name"))
            .and_then(|field| field.get("default"))
            .and_then(|value| value.as_str()),
        Some("")
    );
    // `minimum: 0` stays, with `optional` forcing the rewrite path.
    let params = params_of(
        r#"{"optional":true,"type":"object","properties":{"n":{"type":"integer","minimum":0}}}"#,
    );
    assert_eq!(
        params
            .get("properties")
            .and_then(|properties| properties.get("n"))
            .and_then(|field| field.get("minimum"))
            .and_then(sonic_rs::JsonValueTrait::as_u64),
        Some(0)
    );
    // An all-`const` union with 0 collapses to a typed enum keeping both values.
    let params = params_of(
        r#"{"type":"object","properties":{"v":{"anyOf":[{"type":"integer","const":0},{"type":"integer","const":1}]}}}"#,
    );
    let values = params
        .get("properties")
        .and_then(|properties| properties.get("v"))
        .and_then(|field| field.get("enum"))
        .and_then(|members| members.as_array())
        .unwrap()
        .iter()
        .map(|value| value.as_u64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(values, vec![0, 1]);
}
