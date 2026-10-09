use dal_core::{CallId, ModelToolSpec, Purpose, ReplaySource, RequestParams, ThinkingLevel};
use futures::executor::block_on;

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

fn body_text(request: &ModelRequest, thinking: &ThinkingPlan) -> String {
    String::from_utf8(request_body(request, thinking).unwrap()).unwrap()
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
            },
            ModelToolSpec {
                name: "list".into(),
                description: "List files.".into(),
                parameters: raw(r#"{"type": "object"}"#),
                grammar: None,
            },
        ],
        vec![
            ContextItem::User {
                parts: vec![text("Describe"), png()],
            },
            ContextItem::Assistant {
                source: replay_source(),
                parts: vec![
                    AssistantPart::Thinking {
                        text: "hidden".into(),
                        replay: None,
                    },
                    AssistantPart::Text {
                        text: "Checking.".into(),
                    },
                    AssistantPart::ToolCall {
                        call: CallId::new("call_1"),
                        name: "read".into(),
                        args: raw(r#"{"path": "a.txt" ,"n":1.50}"#),
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
        request_body(&blob, &plan(None, None)),
        Err(ChatBodyError::UnresolvedBlob { blob_id })
    );

    let mut other = request(Vec::new(), Vec::new());
    other.model = ModelRoute::Api {
        family: Family::Responses,
        model: "gpt-6-astra".into(),
    };
    assert_eq!(
        request_body(&other, &plan(None, None)),
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
        request_body(&request(Vec::new(), Vec::new()), &anthropic),
        Err(ChatBodyError::ForeignThinking)
    );
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
        }],
        vec![ContextItem::Assistant {
            source: replay_source(),
            parts: vec![AssistantPart::ToolCall {
                call: CallId::new("call-1"),
                name: "deploy.web-x.list".into(),
                args: raw("{}"),
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
