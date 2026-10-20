use dal_core::{CallId, ModelToolSpec, Purpose, ReplaySource, RequestParams, ThinkingLevel};
use futures::executor::block_on;

use super::*;
use crate::sse::SseEvent;
use crate::stream::ReplayPayload;

fn raw(text: &str) -> RawJson {
    RawJson::parse(text).unwrap()
}

fn replay_source() -> ReplaySource {
    ReplaySource {
        family: Family::Mistral,
        model: "mistral-medium-latest".into(),
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
            family: Family::Mistral,
            model: "mistral-medium-latest".into(),
            provider: None,
        },
        system: "Be concise.".into(),
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

fn tool(name: &str, description: &str, parameters: &str) -> ModelToolSpec {
    ModelToolSpec {
        name: name.into(),
        description: description.into(),
        parameters: raw(parameters),
        grammar: None,
        defer_loading: false,
    }
}

fn body_text(request: &ModelRequest, thinking: &ThinkingPlan) -> String {
    String::from_utf8(request_body(request, thinking).unwrap()).unwrap()
}

#[test]
fn request_body_matches_the_member_order_byte_for_byte() {
    let mut request = request(
        vec![tool(
            "search",
            "Search the web.",
            r#"{"type":"object","properties":{"q":{"type":"string"}}}"#,
        )],
        vec![
            ContextItem::User {
                parts: vec![text("Find cats"), png()],
            },
            ContextItem::Assistant {
                source: replay_source(),
                replay: None,
                parts: vec![
                    AssistantPart::Thinking {
                        text: "search first".into(),
                        replay: None,
                    },
                    AssistantPart::Text {
                        text: "On it.".into(),
                        replay: None,
                    },
                    AssistantPart::ToolCall {
                        // Nine alphanumerics already: the wire id is the call id.
                        call: CallId::new("a1b2c3d4e"),
                        name: "search".into(),
                        args: raw(r#"{"q":  "cats"}"#),
                        replay: None,
                    },
                ],
            },
            ContextItem::ToolResult {
                call: CallId::new("a1b2c3d4e"),
                name: "search".into(),
                is_error: false,
                parts: vec![text("two cats"), png()],
            },
        ],
    );
    request.params.max_output_tokens = Some(512);
    let expected = concat!(
        r#"{"model":"mistral-medium-latest","inputs":["#,
        r#"{"type":"message.input","role":"user","content":["#,
        r#"{"type":"text","text":"Find cats"},"#,
        r#"{"type":"image_url","image_url":"data:image/png;base64,iVBORw=="}]},"#,
        r#"{"type":"message.output","role":"assistant","content":["#,
        r#"{"type":"thinking","thinking":[{"type":"text","text":"search first"}]},"#,
        r#"{"type":"text","text":"On it."}]},"#,
        r#"{"type":"function.call","tool_call_id":"a1b2c3d4e","name":"search","#,
        r#""arguments":{"q":  "cats"}},"#,
        r#"{"type":"function.result","tool_call_id":"a1b2c3d4e","result":"two cats"},"#,
        r#"{"type":"message.input","role":"user","content":["#,
        r#"{"type":"text","text":"Images from tool call a1b2c3d4e."},"#,
        r#"{"type":"image_url","image_url":"data:image/png;base64,iVBORw=="}]}],"#,
        r#""instructions":"Be concise.","#,
        r#""tools":[{"type":"function","function":{"name":"search","description":"Search the web.","#,
        r#""parameters":{"type":"object","properties":{"q":{"type":"string"}}}}}],"#,
        r#""stream":true,"store":false,"#,
        r#""completion_args":{"temperature":0.7,"max_tokens":512,"tool_choice":"auto","#,
        r#""reasoning_effort":"high"}}"#,
    );
    assert_eq!(
        body_text(&request, &plan(Some("high"), Some(0.7))),
        expected
    );
    // Two builds of the same request are equal byte for byte.
    assert_eq!(
        request_body(&request, &plan(Some("high"), Some(0.7))),
        request_body(&request, &plan(Some("high"), Some(0.7)))
    );
}

#[test]
fn request_body_without_tools_system_or_counts_sends_only_inputs() {
    let mut request = request(Vec::new(), Vec::new());
    request.system = "".into();
    assert_eq!(
        body_text(&request, &plan(None, None)),
        r#"{"model":"mistral-medium-latest","inputs":[],"stream":true,"store":false}"#
    );
}

/// Parses a request body and returns the `completion_args` member spellings
/// `(reasoning_effort, tool_choice)`; `None` for a member the body omits.
fn completion_members(body: &str) -> (Option<String>, Option<String>) {
    let parsed: sonic_rs::Value = sonic_rs::from_str(body).expect("the body is valid JSON");
    let args = parsed
        .get("completion_args")
        .expect("completion_args is present");
    let member = |name: &str| {
        args.get(name)
            .map(|value| value.as_str().expect("a string member").to_owned())
    };
    (member("reasoning_effort"), member("tool_choice"))
}

#[test]
fn reasoning_effort_spells_the_conversations_ladder() {
    let mut request = request(
        vec![tool("search", "Search.", r#"{"type":"object"}"#)],
        Vec::new(),
    );
    request.system = "".into();
    for (effort, spelled) in [
        ("none", "none"),
        ("minimal", "low"),
        ("low", "low"),
        ("medium", "medium"),
        ("high", "high"),
        ("xhigh", "high"),
        ("max", "max"),
    ] {
        let body = body_text(&request, &plan(Some(effort), None));
        assert_eq!(
            completion_members(&body),
            (Some(spelled.to_owned()), Some("auto".to_owned())),
            "effort {effort}: {body}"
        );
    }
    // A spelling the ladder does not know, or no effort at all, omits the
    // member while keeping the rest of `completion_args`.
    for effort in [Some("ludicrous"), None] {
        let body = body_text(&request, &plan(effort, None));
        assert_eq!(
            completion_members(&body),
            (None, Some("auto".to_owned())),
            "effort {effort:?}: {body}"
        );
    }
}

#[test]
fn an_unknown_effort_without_other_arguments_omits_completion_args() {
    let mut request = request(Vec::new(), Vec::new());
    request.system = "".into();
    let body = body_text(&request, &plan(Some("ludicrous"), None));
    let parsed: sonic_rs::Value = sonic_rs::from_str(&body).expect("the body is valid JSON");
    assert!(parsed.get("completion_args").is_none(), "{body}");
    assert!(!body.contains("reasoning_effort"), "{body}");
}

#[test]
fn consecutive_user_entries_coalesce_into_one_input() {
    let request = request(
        Vec::new(),
        vec![
            ContextItem::User {
                parts: vec![text("first")],
            },
            ContextItem::User {
                parts: vec![text("second")],
            },
        ],
    );
    let body = body_text(&request, &plan(None, None));
    assert_eq!(
        body.matches(r#""type":"message.input""#).count(),
        1,
        "{body}"
    );
    assert!(
        body.contains(r#""text":"first"},{"type":"text","text":"second""#),
        "{body}"
    );
}

#[test]
fn request_body_rejects_what_mistral_cannot_carry() {
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
        request_body(&blob, &plan(None, None)),
        Err(MistralBodyError::UnresolvedBlob { blob_id })
    );

    let mut other = request(Vec::new(), Vec::new());
    other.model = ModelRoute::Api {
        family: Family::Chat,
        model: "mistral-medium-latest".into(),
        provider: None,
    };
    assert_eq!(
        request_body(&other, &plan(None, None)),
        Err(MistralBodyError::NotMistral {
            route: "mistral-medium-latest".into()
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
        request_body(&request(Vec::new(), Vec::new()), &anthropic),
        Err(MistralBodyError::ForeignThinking)
    );
}

#[test]
fn an_assistant_message_with_nothing_to_send_is_omitted() {
    let request = request(
        Vec::new(),
        vec![ContextItem::Assistant {
            source: replay_source(),
            replay: None,
            parts: Vec::new(),
        }],
    );
    let body = body_text(&request, &plan(None, None));
    assert!(!body.contains("message.output"), "{body}");
}

#[test]
fn prior_reasoning_replays_only_while_reasoning_is_on() {
    let history = || {
        request(
            Vec::new(),
            vec![ContextItem::Assistant {
                source: replay_source(),
                replay: None,
                parts: vec![
                    AssistantPart::Thinking {
                        text: "prior reasoning".into(),
                        replay: None,
                    },
                    AssistantPart::Text {
                        text: "previous answer".into(),
                        replay: None,
                    },
                ],
            }],
        )
    };
    // `none` is the thinking-off wire form and no effort means no reasoning
    // control: neither takes a `thinking` chunk back, the answer still goes.
    for effort in [Some("none"), None] {
        let body = body_text(&history(), &plan(effort, None));
        assert!(!body.contains(r#""type":"thinking""#), "{effort:?}: {body}");
        assert!(body.contains("previous answer"), "{effort:?}: {body}");
    }
    for effort in ["low", "medium", "high", "max"] {
        let body = body_text(&history(), &plan(Some(effort), None));
        assert!(
            body.contains(
                r#"{"type":"thinking","thinking":[{"type":"text","text":"prior reasoning"}]}"#
            ),
            "{effort}: {body}"
        );
    }
    // A reasoning-only reply has nothing to send once reasoning is off.
    let only_thoughts = request(
        Vec::new(),
        vec![ContextItem::Assistant {
            source: replay_source(),
            replay: None,
            parts: vec![AssistantPart::Thinking {
                text: "just thoughts".into(),
                replay: None,
            }],
        }],
    );
    let body = body_text(&only_thoughts, &plan(Some("none"), None));
    assert!(!body.contains("message.output"), "{body}");
}

#[test]
fn wire_ids_are_nine_alphanumeric_characters_and_stable_per_build() {
    let mut normalizer = CallIdNormalizer::default();
    let first = normalizer.normalize("session:tool/3");
    let second = normalizer.normalize("session:tool/3");
    assert_eq!(first, second);
    assert_eq!(first.len(), TOOL_CALL_ID_LENGTH);
    assert!(
        first
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
    );

    // Nine alphanumerics already pass through untouched.
    assert_eq!(normalizer.normalize("a1b2c3d4e"), "a1b2c3d4e");

    // Two different calls whose ids project alike collide, so the normalizer
    // disambiguates with a counter and still returns distinct ids.
    let mut other = CallIdNormalizer::default();
    let left = other.normalize("call-1");
    let right = other.normalize("call_1");
    assert_ne!(left, right);
    assert_eq!(left.len(), TOOL_CALL_ID_LENGTH);
    assert_eq!(right.len(), TOOL_CALL_ID_LENGTH);
    assert_eq!(other.normalize("call-1"), left);
    assert_eq!(other.normalize("call_1"), right);
}

fn event(name: Option<&str>, data: &str) -> SseEvent {
    SseEvent {
        name: name.map(str::to_owned),
        data: data.to_owned(),
    }
}

/// Runs SSE datas through the Mistral decoder. `named` wraps every event in
/// the matching SSE event name, proving dispatch reads the JSON `type`
/// member and never the event name.
fn run(datas: &[&str], named: bool) -> (Vec<StreamEvent>, Option<ProviderError>) {
    let events: Vec<Result<SseEvent, ProviderError>> = datas
        .iter()
        .map(|data| {
            Ok(event(
                named.then_some(match data_event_type(data) {
                    "conversation.response.started" => "conversation.response.started",
                    "conversation.response.done" => "conversation.response.done",
                    "conversation.response.error" => "conversation.response.error",
                    "function.call.delta" => "function.call.delta",
                    _ => "message.output.delta",
                }),
                data,
            ))
        })
        .collect();
    let decoded = decode_events(
        stream::iter(events),
        MistralDecoder::new("mistral", "mistral-medium-latest"),
    );
    let items: Vec<_> = block_on(decoded.collect());
    let mut out = Vec::new();
    let mut error = None;
    for item in items {
        assert!(error.is_none(), "an item followed the terminal error");
        match item {
            Ok(result) => out.push(result),
            Err(failure) => error = Some(failure),
        }
    }
    (out, error)
}

/// Reads the JSON `type` member of a test event without the decoder.
fn data_event_type(data: &str) -> &str {
    data.split("\"type\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap_or("")
}

fn done(calls: Vec<ToolCall>, usage: Usage, reason: StopReason) -> Vec<StreamEvent> {
    vec![
        StreamEvent::ToolCallsDone { calls },
        StreamEvent::Usage { usage },
        StreamEvent::Stop { reason },
    ]
}

const TURN: [&str; 6] = [
    r#"{"type":"conversation.response.started","conversation_id":"conv-1"}"#,
    r#"{"type":"message.output.delta","output_index":0,"id":"e1","content_index":0,"role":"assistant","content":"Hé"}"#,
    r#"{"type":"message.output.delta","output_index":0,"id":"e2","content_index":0,"content":[{"type":"thinking","thinking":[{"type":"text","text":"why"}]},{"type":"text","text":"llo"}]}"#,
    r#"{"type":"function.call.delta","output_index":1,"id":"e3","name":"search","tool_call_id":"a1b2c3d4e","arguments":"{\"q"}"#,
    r#"{"type":"function.call.delta","output_index":1,"id":"e4","name":"search","tool_call_id":"a1b2c3d4e","arguments":"\":\"cats\"}"}"#,
    r#"{"type":"conversation.response.done","usage":{"prompt_tokens":41,"completion_tokens":12,"total_tokens":53}}"#,
];

#[test]
fn turn_decodes_text_reasoning_and_fragmented_arguments() {
    let args = r#"{"q":"cats"}"#;
    let mut expected = vec![
        StreamEvent::TextDelta { text: "Hé".into() },
        StreamEvent::ReasoningDelta { text: "why".into() },
        StreamEvent::TextDelta { text: "llo".into() },
        StreamEvent::ToolCallStarted {
            id: "a1b2c3d4e".into(),
            name: "search".into(),
        },
        StreamEvent::ToolArgsDelta {
            id: "a1b2c3d4e".into(),
            fragment: r#"{"q"#.as_bytes().to_vec(),
        },
        StreamEvent::ToolArgsDelta {
            id: "a1b2c3d4e".into(),
            fragment: r#"":"cats"}"#.as_bytes().to_vec(),
        },
    ];
    expected.extend(done(
        vec![ToolCall {
            id: "a1b2c3d4e".into(),
            name: "search".into(),
            args: ToolArgs::Parsed(raw(args)),
        }],
        Usage {
            input_tokens: 41,
            output_tokens: 12,
            ..NO_USAGE
        },
        StopReason::ToolUse,
    ));
    for named in [false, true] {
        let (events, error) = run(&TURN, named);
        assert!(error.is_none(), "{error:?}");
        assert_eq!(events, expected, "named {named}");
    }
}

#[test]
fn two_unindexed_calls_keep_distinct_ids_and_fragmented_arguments() {
    // Two `function.call.delta` events without an `output_index` name two
    // distinct calls: the second opens the next slot instead of merging
    // into slot zero. A keyless fragment continues the call that last
    // spoke, and a fragment naming the first id resumes it in place.
    let datas = [
        r#"{"type":"function.call.delta","id":"u1","name":"search","tool_call_id":"a1b2c3d4e","arguments":"{\"q"}"#,
        r#"{"type":"function.call.delta","id":"u2","name":"lookup","tool_call_id":"f5e6d7c8b","arguments":"{\"k"}"#,
        r#"{"type":"function.call.delta","id":"u3","arguments":"\":\"v\"}"}"#,
        r#"{"type":"function.call.delta","id":"u4","tool_call_id":"a1b2c3d4e","arguments":"\":\"cats\"}"}"#,
        r#"{"type":"conversation.response.done","usage":{"prompt_tokens":9,"completion_tokens":4,"total_tokens":13}}"#,
    ];
    let mut expected = vec![
        StreamEvent::ToolCallStarted {
            id: "a1b2c3d4e".into(),
            name: "search".into(),
        },
        StreamEvent::ToolArgsDelta {
            id: "a1b2c3d4e".into(),
            fragment: r#"{"q"#.as_bytes().to_vec(),
        },
        StreamEvent::ToolCallStarted {
            id: "f5e6d7c8b".into(),
            name: "lookup".into(),
        },
        StreamEvent::ToolArgsDelta {
            id: "f5e6d7c8b".into(),
            fragment: r#"{"k"#.as_bytes().to_vec(),
        },
        StreamEvent::ToolArgsDelta {
            id: "f5e6d7c8b".into(),
            fragment: r#"":"v"}"#.as_bytes().to_vec(),
        },
        StreamEvent::ToolArgsDelta {
            id: "a1b2c3d4e".into(),
            fragment: r#"":"cats"}"#.as_bytes().to_vec(),
        },
    ];
    expected.extend(done(
        vec![
            ToolCall {
                id: "a1b2c3d4e".into(),
                name: "search".into(),
                args: ToolArgs::Parsed(raw(r#"{"q":"cats"}"#)),
            },
            ToolCall {
                id: "f5e6d7c8b".into(),
                name: "lookup".into(),
                args: ToolArgs::Parsed(raw(r#"{"k":"v"}"#)),
            },
        ],
        Usage {
            input_tokens: 9,
            output_tokens: 4,
            ..NO_USAGE
        },
        StopReason::ToolUse,
    ));
    for named in [false, true] {
        let (events, error) = run(&datas, named);
        assert!(error.is_none(), "{error:?}");
        assert_eq!(events, expected, "named {named}");
    }
}

#[test]
fn the_started_event_records_the_conversation_id() {
    let mut decoder = MistralDecoder::new("mistral", "mistral-medium-latest");
    let mut out = Vec::new();
    assert_eq!(decoder.conversation_id.as_deref(), None);
    assert_eq!(decoder.feed(TURN[0], &mut out).unwrap(), Progress::Open);
    assert_eq!(out, []);
    assert_eq!(decoder.conversation_id.as_deref(), Some("conv-1"));
    // A second started event never overwrites the first.
    assert_eq!(
        decoder
            .feed(
                r#"{"type":"conversation.response.started","conversation_id":"conv-2"}"#,
                &mut out,
            )
            .unwrap(),
        Progress::Open
    );
    assert_eq!(decoder.conversation_id.as_deref(), Some("conv-1"));
}

#[test]
fn a_plain_text_turn_stops_with_zero_usage() {
    let (events, error) = run(
        &[
            r#"{"type":"conversation.response.started","conversation_id":"conv-1"}"#,
            r#"{"type":"message.output.delta","content":"hi"}"#,
            r#"{"type":"conversation.response.done","usage":{}}"#,
        ],
        false,
    );
    assert!(error.is_none(), "{error:?}");
    assert_eq!(
        events,
        [
            vec![StreamEvent::TextDelta { text: "hi".into() }],
            done(Vec::new(), NO_USAGE, StopReason::EndTurn),
        ]
        .concat()
    );
}

#[test]
fn connector_handoff_and_unknown_events_are_ignored() {
    let (events, error) = run(
        &[
            r#"{"type":"agent.handoff.started","from":"a","to":"b"}"#,
            r#"{"type":"tool.execution.delta","output_index":2,"id":"t1","name":"web_search","arguments":"x"}"#,
            r#"{"type":"some.future.event","payload":[1,2]}"#,
            r#"{"type":"message.output.delta","content":"here"}"#,
            r#"{"type":"agent.handoff.done"}"#,
            r#"{"type":"conversation.response.done","usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#,
        ],
        false,
    );
    assert!(error.is_none(), "{error:?}");
    assert_eq!(
        events,
        [
            vec![StreamEvent::TextDelta {
                text: "here".into()
            }],
            done(
                Vec::new(),
                Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    ..NO_USAGE
                },
                StopReason::EndTurn,
            ),
        ]
        .concat()
    );
}

#[test]
fn an_error_event_maps_its_numeric_code_to_the_http_twin() {
    let (events, error) = run(
        &[
            r#"{"type":"message.output.delta","content":"par"}"#,
            r#"{"type":"conversation.response.error","message":"slow down","code":429}"#,
            r#"{"type":"message.output.delta","content":"never"}"#,
        ],
        false,
    );
    assert_eq!(events, [StreamEvent::TextDelta { text: "par".into() }]);
    assert!(matches!(error, Some(ProviderError::RateLimited { .. })));

    let (events, error) = run(
        &[r#"{"type":"conversation.response.error","message":"bad key","code":401}"#],
        false,
    );
    assert_eq!(events, []);
    assert!(matches!(error, Some(ProviderError::AuthRejected { .. })));
}

#[test]
fn an_error_event_without_a_numeric_code_is_a_status_not_a_transport_failure() {
    for data in [
        r#"{"type":"conversation.response.error","message":"boom"}"#,
        r#"{"type":"conversation.response.error","message":"boom","code":null}"#,
        r#"{"type":"conversation.response.error","message":"boom","code":"backend exploded"}"#,
        r#"{"type":"conversation.response.error","message":"boom","code":70000}"#,
    ] {
        let (events, error) = run(&[data], false);
        assert_eq!(events, [], "{data}");
        assert!(
            matches!(error, Some(ProviderError::Status { status: 200, .. })),
            "{data}: {error:?}"
        );
    }
    // A string spelling the retry table knows keeps its meaning.
    let (events, error) = run(
        &[r#"{"type":"conversation.response.error","message":"slow","code":"rate_limit_error"}"#],
        false,
    );
    assert_eq!(events, []);
    assert!(
        matches!(error, Some(ProviderError::RateLimited { .. })),
        "{error:?}"
    );
}

#[test]
fn malformed_events_and_end_without_done_fail() {
    let (events, error) = run(&["this is not json"], false);
    assert_eq!(events, []);
    assert!(matches!(
        error,
        Some(ProviderError::Protocol {
            family: Family::Mistral,
            ..
        })
    ));

    let (events, error) = run(
        &[r#"{"type":"function.call.delta","output_index":0,"name":"","tool_call_id":""}"#],
        false,
    );
    assert_eq!(events, []);
    assert!(matches!(
        error,
        Some(ProviderError::Protocol {
            family: Family::Mistral,
            ..
        })
    ));

    let (events, error) = run(&[TURN[1]], false);
    assert_eq!(events.len(), 1);
    assert!(matches!(error, Some(ProviderError::StreamCut)), "{error:?}");
}

#[test]
fn done_closes_the_stream_and_later_input_is_ignored() {
    let mut decoder = MistralDecoder::new("mistral", "mistral-medium-latest");
    let mut out = Vec::new();
    for data in &TURN {
        decoder.feed(data, &mut out).unwrap();
    }
    let settled = out.len();
    assert_eq!(settled, 9);
    assert_eq!(decoder.feed(TURN[1], &mut out).unwrap(), Progress::Ended);
    assert!(decoder.end(&mut out).is_ok());
    assert_eq!(
        out.len(),
        settled,
        "input after the terminal produced events"
    );
}

#[test]
fn a_sent_done_marker_finishes_without_usage() {
    let (events, error) = run(
        &[
            r#"{"type":"message.output.delta","content":"hi"}"#,
            "[DONE]",
        ],
        false,
    );
    assert!(error.is_none(), "{error:?}");
    assert_eq!(
        events,
        [
            vec![StreamEvent::TextDelta { text: "hi".into() }],
            done(Vec::new(), NO_USAGE, StopReason::EndTurn),
        ]
        .concat()
    );
}

#[test]
fn a_sent_done_marker_without_a_finish_reason_keeps_open_calls_as_tool_use() {
    let (events, error) = run(
        &[
            r#"{"type":"function.call.delta","output_index":0,"name":"search","tool_call_id":"a1b2c3d4e","arguments":"{}"}"#,
            "[DONE]",
        ],
        false,
    );
    assert!(error.is_none(), "{error:?}");
    assert_eq!(
        events[events.len() - 3..],
        done(
            vec![ToolCall {
                id: "a1b2c3d4e".into(),
                name: "search".into(),
                args: ToolArgs::Parsed(raw("{}")),
            }],
            NO_USAGE,
            StopReason::ToolUse,
        )
    );
}

#[test]
fn cumulative_deltas_stop_at_the_budgets() {
    let mut decoder = MistralDecoder::new("mistral", "mistral-medium-latest");
    let data = r#"{"type":"message.output.delta","content":"x"}"#;
    let mut out = Vec::new();
    for _ in 0..crate::stream_budget::STREAM_EVENTS_LIMIT {
        assert!(decoder.feed(data, &mut out).is_ok());
        out.clear();
    }
    assert!(matches!(
        decoder.feed(data, &mut out),
        Err(ProviderError::Limit(crate::error::LimitError::StreamEvents))
    ));

    let mut decoder = MistralDecoder::new("mistral", "mistral-medium-latest");
    let call = r#"{"type":"function.call.delta","output_index":0,"name":"n","tool_call_id":"id"}"#;
    let mut out = Vec::new();
    decoder.feed(call, &mut out).unwrap();
    out.clear();
    for index in 1..crate::stream_budget::STREAM_ITEMS_LIMIT {
        let delta = format!(
            r#"{{"type":"function.call.delta","output_index":{index},"name":"n","tool_call_id":"id{index}"}}"#
        );
        assert!(decoder.feed(&delta, &mut out).is_ok(), "index {index}");
        out.clear();
    }
    assert!(matches!(
        decoder.feed(
            r#"{"type":"function.call.delta","output_index":65536,"name":"n","tool_call_id":"idx"}"#,
            &mut out
        ),
        Err(ProviderError::Limit(crate::error::LimitError::StreamItems))
    ));
}

#[test]
fn dropping_the_stream_before_its_terminal_drops_the_event_source() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    struct Flag(Arc<AtomicBool>);
    impl Drop for Flag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let dropped = Arc::new(AtomicBool::new(false));
    let flag = Flag(Arc::clone(&dropped));
    let first = Ok(event(None, TURN[1]));
    let source = stream::unfold((flag, Some(first)), |(flag, item)| async move {
        let item = item?;
        Some((item, (flag, None)))
    });
    let mut decoded = Box::pin(decode_events(
        source,
        MistralDecoder::new("mistral", "mistral-medium-latest"),
    ));
    let first = block_on(decoded.next());
    assert!(matches!(first, Some(Ok(StreamEvent::TextDelta { .. }))));
    assert!(!dropped.load(Ordering::SeqCst));
    drop(decoded);
    assert!(
        dropped.load(Ordering::SeqCst),
        "the event source outlived the stream"
    );
}

#[test]
fn the_full_path_runs_through_the_event_stream_guard() {
    let chunks: Vec<Vec<u8>> = TURN
        .iter()
        .map(|data| format!("data: {data}\n\n").into_bytes())
        .collect();
    let mut events = crate::stream::EventStream::new(
        decode_events(
            crate::sse::decode_stream(futures::stream::iter(chunks)),
            MistralDecoder::new("mistral", "mistral-medium-latest"),
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
    assert_eq!(seen.len(), 9);
    assert!(matches!(seen.last(), Some(StreamEvent::Stop { .. })));
}

#[test]
fn request_body_ignores_text_tool_and_message_replay() {
    let assistant = |replay: fn() -> Option<RawJson>| ContextItem::Assistant {
        source: replay_source(),
        replay: replay(),
        parts: vec![
            AssistantPart::Text {
                text: "Hi.".into(),
                replay: replay(),
            },
            AssistantPart::ToolCall {
                call: CallId::new("call-9"),
                name: "read".into(),
                args: raw("{}"),
                replay: replay(),
            },
        ],
    };
    let payload = || Some(raw(r#"{"foreign":"REPLAY-MARKER"}"#));
    let plain = body_text(
        &request(Vec::new(), vec![assistant(|| None)]),
        &plan(None, None),
    );
    let laden = body_text(
        &request(Vec::new(), vec![assistant(payload)]),
        &plan(None, None),
    );
    assert_eq!(laden, plain);
    assert!(!laden.contains("REPLAY-MARKER"), "{laden}");
}

// Signed reasoning: the `signature` of a `thinking` chunk, captured by the
// decoder and sent back by the encoder.

const STARTED: &str = r#"{"type":"conversation.response.started","conversation_id":"conv-1"}"#;
const DONE: &str =
    r#"{"type":"conversation.response.done","usage":{"prompt_tokens":5,"completion_tokens":2}}"#;
/// Three distinct adjacent chunks of one run of reasoning: signed; unsigned
/// with an explicit `null` and odd spacing; signed with escapes and `closed`.
const SIGNED_A: &str =
    r#"{"type":"thinking","thinking":[{"type":"text","text":"alpha "}],"signature":"SIG-A"}"#;
const UNSIGNED_B: &str =
    r#"{"type":"thinking", "thinking":[{"type":"text","text":"beta "}],  "signature":null}"#;
const SIGNED_C: &str = r#"{"type":"thinking","thinking":[{"type":"text","text":"gamma"}],"closed":false,"signature":"SIG-C\/\u00e9\"q"}"#;
/// The text-only chunk the three chunks above replay as when they may not
/// go back signed.
const TEXT_ONLY: &str =
    r#"{"type":"thinking","thinking":[{"type":"text","text":"alpha beta gamma"}]}"#;
/// A signed chunk of one word, and a second one for a second run.
const SIGNED_ONE: &str =
    r#"{"type":"thinking","thinking":[{"type":"text","text":"one"}],"signature":"SIG-1"}"#;
const SIGNED_TWO: &str =
    r#"{"type":"thinking","thinking":[{"type":"text","text":"two"}],"signature":"SIG-2"}"#;
const CALL_DELTA: &str = r#"{"type":"function.call.delta","output_index":1,"id":"e3","name":"search","tool_call_id":"a1b2c3d4e","arguments":"{}"}"#;

fn delta(content: &str) -> String {
    format!(
        r#"{{"type":"message.output.delta","output_index":0,"id":"e1","content_index":0,"role":"assistant","content":{content}}}"#
    )
}

fn run_all(datas: &[String]) -> (Vec<StreamEvent>, Option<ProviderError>) {
    let datas: Vec<&str> = datas.iter().map(String::as_str).collect();
    run(&datas, false)
}

/// The stream of one turn: the three chunks above, each in its own event,
/// then the visible answer.
fn signed_turn() -> Vec<String> {
    vec![
        STARTED.to_owned(),
        delta(&format!("[{SIGNED_A}]")),
        delta(&format!("[{UNSIGNED_B}]")),
        delta(&format!("[{SIGNED_C}]")),
        delta(r#""Answer.""#),
        DONE.to_owned(),
    ]
}

fn signed_run() -> RawJson {
    raw(&format!("[{SIGNED_A},{UNSIGNED_B},{SIGNED_C}]"))
}

fn replay_event(chunks: &str) -> StreamEvent {
    StreamEvent::Replay {
        payload: ReplayPayload {
            family: Family::Mistral,
            model: "mistral-medium-latest".into(),
            item: raw(&format!("[{chunks}]")),
        },
    }
}

fn five_two_done(calls: Vec<ToolCall>, reason: StopReason) -> Vec<StreamEvent> {
    let usage = Usage {
        input_tokens: 5,
        output_tokens: 2,
        ..NO_USAGE
    };
    done(calls, usage, reason)
}

fn reasoning(text: &str) -> StreamEvent {
    StreamEvent::ReasoningDelta { text: text.into() }
}

fn visible(text: &str) -> StreamEvent {
    StreamEvent::TextDelta { text: text.into() }
}

fn thinking_part(text: &str, replay: Option<RawJson>) -> AssistantPart {
    AssistantPart::Thinking {
        text: text.into(),
        replay,
    }
}

fn answer_part(text: &str) -> AssistantPart {
    AssistantPart::Text {
        text: text.into(),
        replay: None,
    }
}

/// Folds decoded events into the assistant parts a session persists and
/// projects back into the next request: the reasoning deltas of one run join
/// into one part, the run's replay item rides on it (`null` when the run has
/// none, the way the journal stores an absent one), visible text closes the
/// run, and the tool calls follow. This restates the session fold of
/// `dal_core` and the context projection of `dal_agent`, which this crate
/// does not depend on; the decoder and the encoder around it are the real ones.
fn persisted_parts(events: &[StreamEvent]) -> Vec<AssistantPart> {
    fn flush_text(parts: &mut Vec<AssistantPart>, pending: &mut String) {
        if !pending.is_empty() {
            parts.push(answer_part(&mem::take(pending)));
        }
    }
    let mut parts = Vec::new();
    let mut open: Option<usize> = None;
    let mut pending = String::new();
    for event in events {
        match event {
            StreamEvent::TextDelta { text } => {
                open = None;
                pending.push_str(text);
            }
            StreamEvent::ReasoningDelta { text } => {
                flush_text(&mut parts, &mut pending);
                if let Some(AssistantPart::Thinking { text: so_far, .. }) =
                    open.and_then(|index| parts.get_mut(index))
                {
                    *so_far = format!("{so_far}{text}").into();
                } else {
                    parts.push(thinking_part(text, Some(RawJson::null())));
                    open = Some(parts.len() - 1);
                }
            }
            StreamEvent::Replay { payload } => {
                flush_text(&mut parts, &mut pending);
                if let Some(AssistantPart::Thinking { replay, .. }) =
                    open.and_then(|index| parts.get_mut(index))
                {
                    *replay = Some(payload.item.clone());
                } else {
                    parts.push(thinking_part("", Some(payload.item.clone())));
                    open = Some(parts.len() - 1);
                }
            }
            StreamEvent::ToolCallsDone { calls } => {
                flush_text(&mut parts, &mut pending);
                open = None;
                for call in calls {
                    parts.push(AssistantPart::ToolCall {
                        call: CallId::new(call.id.as_str()),
                        name: call.name.as_str().into(),
                        args: match &call.args {
                            ToolArgs::Parsed(args) => args.clone(),
                            _ => RawJson::null(),
                        },
                        replay: None,
                    });
                }
            }
            _ => {}
        }
    }
    flush_text(&mut parts, &mut pending);
    parts
}

/// The next request of a conversation whose last answer is `parts`, recorded
/// from `source`.
fn next_turn(source: ReplaySource, parts: Vec<AssistantPart>) -> ModelRequest {
    request(
        Vec::new(),
        vec![
            ContextItem::User {
                parts: vec![text("First")],
            },
            ContextItem::Assistant {
                source,
                replay: None,
                parts,
            },
            ContextItem::User {
                parts: vec![text("Next")],
            },
        ],
    )
}

fn replayed_body(request: &ModelRequest) -> String {
    body_text(request, &plan(Some("high"), None))
}

#[test]
fn signed_reasoning_chunks_are_captured_verbatim_and_in_arrival_order() {
    let (events, error) = run_all(&signed_turn());
    assert!(error.is_none(), "{error:?}");
    let expected = [
        vec![
            reasoning("alpha "),
            reasoning("beta "),
            reasoning("gamma"),
            StreamEvent::Replay {
                payload: ReplayPayload {
                    family: Family::Mistral,
                    model: "mistral-medium-latest".into(),
                    item: signed_run(),
                },
            },
            visible("Answer."),
        ],
        five_two_done(Vec::new(), StopReason::EndTurn),
    ]
    .concat();
    assert_eq!(events, expected);
}

#[test]
fn signed_reasoning_survives_persistence_into_the_next_request_byte_for_byte() {
    let (events, error) = run_all(&signed_turn());
    assert!(error.is_none(), "{error:?}");
    let parts = persisted_parts(&events);
    assert_eq!(
        parts,
        vec![
            thinking_part("alpha beta gamma", Some(signed_run())),
            answer_part("Answer."),
        ]
    );
    // The parts also survive their own JSON form, the way a journal keeps them.
    let stored = sonic_rs::to_string(&parts).unwrap();
    let restored: Vec<AssistantPart> = sonic_rs::from_str(&stored).unwrap();
    assert_eq!(restored, parts);
    let next = next_turn(replay_source(), restored);
    let body = replayed_body(&next);
    // Three chunks, the unsigned one still unsigned, none merged, none
    // re-encoded, the text chunk after them.
    let expected = format!(
        r#"{{"type":"message.output","role":"assistant","content":[{SIGNED_A},{UNSIGNED_B},{SIGNED_C},{{"type":"text","text":"Answer."}}]}}"#
    );
    assert!(body.contains(&expected), "{body}");
    assert_eq!(body.matches("SIG-").count(), 2, "{body}");
    assert_eq!(replayed_body(&next), body);
}

#[test]
fn signed_reasoning_is_replayed_only_to_the_family_and_model_that_produced_it() {
    let (events, _) = run_all(&signed_turn());
    let parts = persisted_parts(&events);
    for (family, model) in [
        (Family::Chat, "mistral-medium-latest"),
        (Family::Anthropic, "mistral-medium-latest"),
        (Family::Mistral, "mistral-large-latest"),
    ] {
        let source = ReplaySource {
            family,
            model: model.into(),
        };
        let body = replayed_body(&next_turn(source, parts.clone()));
        assert!(body.contains(TEXT_ONLY), "{family:?} {model}: {body}");
        assert!(!body.contains("SIG-"), "{family:?} {model}: {body}");
        assert!(!body.contains("signature"), "{family:?} {model}: {body}");
    }
    // The producing source sent to another model of the family is foreign too.
    let mut other = next_turn(replay_source(), parts);
    other.model = ModelRoute::Api {
        family: Family::Mistral,
        model: "mistral-large-latest".into(),
        provider: None,
    };
    let body = replayed_body(&other);
    assert!(body.contains(TEXT_ONLY), "{body}");
    assert!(!body.contains("signature"), "{body}");
}

#[test]
fn signed_reasoning_is_dropped_while_reasoning_is_off() {
    let (events, _) = run_all(&signed_turn());
    let parts = persisted_parts(&events);
    for effort in [Some("none"), None] {
        let next = next_turn(replay_source(), parts.clone());
        let body = body_text(&next, &plan(effort, None));
        assert!(!body.contains(r#""type":"thinking""#), "{effort:?}: {body}");
        assert!(!body.contains("signature"), "{effort:?}: {body}");
        assert!(
            body.contains(r#"{"type":"text","text":"Answer."}"#),
            "{effort:?}: {body}"
        );
    }
}

#[test]
fn history_recorded_before_signatures_replays_as_text_only() {
    for replay in [None, Some(RawJson::null())] {
        let parts = vec![thinking_part("old reasoning", replay), answer_part("Ok.")];
        let body = replayed_body(&next_turn(replay_source(), parts));
        let chunk = r#"{"type":"thinking","thinking":[{"type":"text","text":"old reasoning"}]}"#;
        assert!(body.contains(chunk), "{body}");
        assert!(!body.contains("signature"), "{body}");
    }
}

#[test]
fn a_payload_that_does_not_match_its_part_is_never_sent() {
    const TEXT_X: &str = r#"{"type":"thinking","thinking":[{"type":"text","text":"x"}]}"#;
    let refused = [
        // Not an array.
        r#"{"type":"thinking","thinking":[{"type":"text","text":"x"}],"signature":"SIG-X"}"#,
        "[]",
        "[1]",
        // A chunk that is not a `thinking` chunk.
        r#"[{"type":"text","thinking":[{"type":"text","text":"x"}],"signature":"SIG-X"}]"#,
        // A signature the schema does not allow, and none or an empty one.
        r#"[{"type":"thinking","thinking":[{"type":"text","text":"x"}],"signature":7}]"#,
        r#"[{"type":"thinking","thinking":[{"type":"text","text":"x"}]}]"#,
        r#"[{"type":"thinking","thinking":[{"type":"text","text":"x"}],"signature":""}]"#,
        // Text that is no longer the text of the part, and parts that are no array.
        r#"[{"type":"thinking","thinking":[{"type":"text","text":"y"}],"signature":"SIG-X"}]"#,
        r#"[{"type":"thinking","thinking":"x","signature":"SIG-X"}]"#,
    ];
    for payload in refused {
        let parts = vec![thinking_part("x", Some(raw(payload)))];
        let body = replayed_body(&next_turn(replay_source(), parts));
        assert!(body.contains(TEXT_X), "{payload}: {body}");
        assert!(!body.contains("SIG-X"), "{payload}: {body}");
    }
    // The control: the same text with a well-formed signed payload goes back.
    let chunk =
        r#"{"type":"thinking","thinking":[{"type":"text","text":"x"}],"signature":"SIG-X"}"#;
    let parts = vec![thinking_part("x", Some(raw(&format!("[{chunk}]"))))];
    let body = replayed_body(&next_turn(replay_source(), parts));
    assert!(body.contains(&format!(r#""content":[{chunk}]"#)), "{body}");
}

#[test]
fn reasoning_runs_split_by_visible_text_keep_separate_signed_items() {
    let (first, second) = (SIGNED_ONE, SIGNED_TWO);
    let datas = [
        STARTED.to_owned(),
        delta(&format!("[{first}]")),
        delta(r#"[{"type":"text","text":"t1 "}]"#),
        delta(&format!("[{second}]")),
        delta(r#""t2""#),
        DONE.to_owned(),
    ];
    let (events, error) = run_all(&datas);
    assert!(error.is_none(), "{error:?}");
    let expected = [
        vec![
            reasoning("one"),
            replay_event(first),
            visible("t1 "),
            reasoning("two"),
            replay_event(second),
            visible("t2"),
        ],
        five_two_done(Vec::new(), StopReason::EndTurn),
    ]
    .concat();
    assert_eq!(events, expected);
    let parts = persisted_parts(&events);
    assert_eq!(
        parts,
        vec![
            thinking_part("one", Some(raw(&format!("[{first}]")))),
            answer_part("t1 "),
            thinking_part("two", Some(raw(&format!("[{second}]")))),
            answer_part("t2"),
        ]
    );
    let body = replayed_body(&next_turn(replay_source(), parts));
    let expected = format!(
        r#"{{"type":"message.output","role":"assistant","content":[{first},{second},{{"type":"text","text":"t1 \nt2"}}]}}"#
    );
    assert!(body.contains(&expected), "{body}");
}

#[test]
fn unsigned_reasoning_emits_no_replay_item_and_replays_as_text() {
    for signature in [
        None,
        Some("null"),
        Some(r#""""#),
        Some("7"),
        Some(r#"{"a":1}"#),
    ] {
        let member = signature
            .map(|value| format!(r#","signature":{value}"#))
            .unwrap_or_default();
        let chunk = format!(
            r#"{{"type":"thinking","thinking":[{{"type":"text","text":"plain"}}]{member}}}"#
        );
        let datas = [
            delta(&format!("[{chunk}]")),
            delta(r#""Answer.""#),
            DONE.to_owned(),
        ];
        let (events, error) = run_all(&datas);
        assert!(error.is_none(), "{signature:?}: {error:?}");
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, StreamEvent::Replay { .. })),
            "{signature:?}: {events:?}"
        );
        let parts = persisted_parts(&events);
        assert_eq!(
            parts[0],
            thinking_part("plain", Some(RawJson::null())),
            "{signature:?}"
        );
        let body = replayed_body(&next_turn(replay_source(), parts));
        assert!(
            body.contains(r#"{"type":"thinking","thinking":[{"type":"text","text":"plain"}]}"#),
            "{signature:?}: {body}"
        );
        assert!(!body.contains("signature"), "{signature:?}: {body}");
    }
}

#[test]
fn a_signature_only_chunk_before_a_tool_call_is_replayed_ahead_of_the_call() {
    let chunk = r#"{"type":"thinking","thinking":[],"signature":"SIG-ONLY"}"#;
    let datas = [
        delta(&format!("[{chunk}]")),
        CALL_DELTA.to_owned(),
        DONE.to_owned(),
    ];
    let (events, error) = run_all(&datas);
    assert!(error.is_none(), "{error:?}");
    let call = ToolCall {
        id: "a1b2c3d4e".into(),
        name: "search".into(),
        args: ToolArgs::Parsed(raw("{}")),
    };
    let expected = [
        vec![
            StreamEvent::ToolCallStarted {
                id: "a1b2c3d4e".into(),
                name: "search".into(),
            },
            StreamEvent::ToolArgsDelta {
                id: "a1b2c3d4e".into(),
                fragment: b"{}".to_vec(),
            },
            replay_event(chunk),
        ],
        five_two_done(vec![call], StopReason::ToolUse),
    ]
    .concat();
    assert_eq!(events, expected);
    let parts = persisted_parts(&events);
    assert_eq!(parts.len(), 2, "{parts:?}");
    assert_eq!(
        parts[0],
        thinking_part("", Some(raw(&format!("[{chunk}]"))))
    );
    let body = replayed_body(&next_turn(replay_source(), parts));
    let expected = format!(
        r#"{{"type":"message.output","role":"assistant","content":[{chunk}]}},{{"type":"function.call","tool_call_id":"a1b2c3d4e","name":"search","arguments":{{}}}}"#
    );
    assert!(body.contains(&expected), "{body}");
}

#[test]
fn a_run_with_a_signature_the_schema_forbids_goes_back_as_text_only() {
    let signed =
        r#"{"type":"thinking","thinking":[{"type":"text","text":"ok "}],"signature":"SIG-OK"}"#;
    let odd = r#"{"type":"thinking","thinking":[{"type":"text","text":"odd"}],"signature":7}"#;
    let datas = [
        delta(&format!("[{signed},{odd}]")),
        delta(r#""Answer.""#),
        DONE.to_owned(),
    ];
    let (events, error) = run_all(&datas);
    assert!(error.is_none(), "{error:?}");
    // Capture is verbatim and judges nothing; the encoder refuses the run.
    assert_eq!(
        events[..4],
        [
            reasoning("ok "),
            reasoning("odd"),
            replay_event(&format!("{signed},{odd}")),
            visible("Answer."),
        ]
    );
    let body = replayed_body(&next_turn(replay_source(), persisted_parts(&events)));
    assert!(
        body.contains(r#"{"type":"thinking","thinking":[{"type":"text","text":"ok odd"}]}"#),
        "{body}"
    );
    assert!(!body.contains("SIG-OK"), "{body}");
}

#[test]
fn visible_text_closes_the_run_in_the_event_it_arrives_in() {
    let (first, second) = (SIGNED_ONE, SIGNED_TWO);
    let datas = [
        delta(&format!(r#"[{first},{{"type":"text","text":"before"}}]"#)),
        delta(&format!(r#"[{{"type":"text","text":" mid"}},{second}]"#)),
        DONE.to_owned(),
    ];
    let (events, error) = run_all(&datas);
    assert!(error.is_none(), "{error:?}");
    let expected = [
        vec![
            reasoning("one"),
            replay_event(first),
            visible("before"),
            visible(" mid"),
            reasoning("two"),
            replay_event(second),
        ],
        five_two_done(Vec::new(), StopReason::EndTurn),
    ]
    .concat();
    assert_eq!(events, expected);
}

#[test]
fn the_done_marker_flushes_an_open_signed_run_before_the_terminal() {
    let datas = [delta(&format!("[{SIGNED_A}]")), "[DONE]".to_owned()];
    let (events, error) = run_all(&datas);
    assert!(error.is_none(), "{error:?}");
    let expected = [
        vec![reasoning("alpha "), replay_event(SIGNED_A)],
        done(Vec::new(), NO_USAGE, StopReason::EndTurn),
    ]
    .concat();
    assert_eq!(events, expected);
}

#[test]
fn the_signed_stream_runs_through_sse_framing_and_the_event_stream_guard() {
    let chunks: Vec<Vec<u8>> = signed_turn()
        .iter()
        .map(|data| format!("data: {data}\n\n").into_bytes())
        .collect();
    let mut events = crate::stream::EventStream::new(
        decode_events(
            crate::sse::decode_stream(futures::stream::iter(chunks)),
            MistralDecoder::new("mistral", "mistral-medium-latest"),
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
    let expected = [
        vec![
            reasoning("alpha "),
            reasoning("beta "),
            reasoning("gamma"),
            StreamEvent::Replay {
                payload: ReplayPayload {
                    family: Family::Mistral,
                    model: "mistral-medium-latest".into(),
                    item: signed_run(),
                },
            },
            visible("Answer."),
        ],
        five_two_done(Vec::new(), StopReason::EndTurn),
    ]
    .concat();
    assert_eq!(seen, expected);
}

#[test]
fn wire_ids_never_leave_the_ascii_alphanumeric_alphabet() {
    let mut normalizer = CallIdNormalizer::default();
    // Nine characters that are not nine ASCII alphanumerics, ids of any other
    // length, and ids with nothing usable in them.
    for id in [
        "é1b2c3d4e",
        "a1b2c3d4é",
        "１２３４５６７８９",
        "日本語日本語日本語",
        "a1b2c3d4",
        "a1b2c3d4ef",
        "",
        "-",
        "..........",
    ] {
        let wire = normalizer.normalize(id);
        assert_eq!(wire.len(), TOOL_CALL_ID_LENGTH, "{id:?}: {wire}");
        assert!(
            wire.chars()
                .all(|character| character.is_ascii_alphanumeric()),
            "{id:?}: {wire}"
        );
    }
    // Eight and ten alphanumerics are not nine, so they never pass through.
    assert_ne!(normalizer.normalize("a1b2c3d4"), "a1b2c3d4");
    assert_ne!(normalizer.normalize("a1b2c3d4ef"), "a1b2c3d4ef");
}

#[test]
fn colliding_call_id_projections_get_distinct_wire_ids_and_each_call_pairs_with_its_result() {
    use sonic_rs::JsonContainerTrait as _;
    // "a1b2-c3d4e" projects onto the nine alphanumerics of the other id, so
    // whichever comes first takes the plain spelling and the second must not.
    for order in [["a1b2c3d4e", "a1b2-c3d4e"], ["a1b2-c3d4e", "a1b2c3d4e"]] {
        let mut context = vec![ContextItem::Assistant {
            source: replay_source(),
            replay: None,
            parts: order
                .iter()
                .map(|id| AssistantPart::ToolCall {
                    call: CallId::new(*id),
                    name: "search".into(),
                    args: raw("{}"),
                    replay: None,
                })
                .collect(),
        }];
        for id in order {
            context.push(ContextItem::ToolResult {
                call: CallId::new(id),
                name: "search".into(),
                is_error: false,
                parts: vec![text("found")],
            });
        }
        let body = body_text(&request(Vec::new(), context), &plan(None, None));
        let parsed: sonic_rs::Value = sonic_rs::from_str(&body).expect("the body is valid JSON");
        let ids_of = |kind: &str| -> Vec<String> {
            parsed
                .get("inputs")
                .and_then(|inputs| inputs.as_array())
                .expect("inputs is an array")
                .iter()
                .filter(|input| input.get("type").and_then(|value| value.as_str()) == Some(kind))
                .map(|input| {
                    input
                        .get("tool_call_id")
                        .and_then(|value| value.as_str())
                        .expect("a string id")
                        .to_owned()
                })
                .collect()
        };
        let calls = ids_of("function.call");
        let results = ids_of("function.result");
        assert_eq!(calls.len(), 2, "{order:?}: {body}");
        assert_eq!(calls, results, "{order:?}: each call pairs with its result");
        assert_ne!(calls[0], calls[1], "{order:?}: {body}");
        for wire in &calls {
            assert_eq!(wire.len(), TOOL_CALL_ID_LENGTH, "{order:?}: {wire}");
            assert!(
                wire.chars()
                    .all(|character| character.is_ascii_alphanumeric()),
                "{order:?}: {wire}"
            );
        }
    }
}
