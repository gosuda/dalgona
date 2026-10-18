use dal_core::{
    CallId, ContextItem, Family, ModelRequest, ModelRoute, ModelToolSpec, Part, Purpose, RawJson,
    RequestParams, ThinkingLevel, Usage,
};
use futures::{executor::block_on, stream};

use super::*;
use crate::{
    sse::SseEvent,
    stream::{StopReason, StreamEvent, ToolArgs, ToolCall},
};

fn raw(text: &str) -> RawJson {
    RawJson::parse(text).expect("valid JSON")
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
            family: Family::PiMessages,
            model: "balanced".into(),
        },
        system: "You are a helpful assistant.".into(),
        tools: tools.into(),
        context: context.into(),
        params: RequestParams {
            thinking: ThinkingLevel::High,
            effort: None,
            temperature: Some(0.7),
            max_output_tokens: Some(4096),
        },
        cache_key: Some("test-session".into()),
    }
}

fn event(data: &str) -> SseEvent {
    SseEvent {
        name: None,
        data: data.to_owned(),
    }
}

fn run(datas: &[&str]) -> (Vec<StreamEvent>, Option<ProviderError>) {
    let events: Vec<Result<SseEvent, ProviderError>> =
        datas.iter().map(|data| Ok(event(data))).collect();
    let decoded = decode_events(
        stream::iter(events),
        PiMessagesDecoder::new("radius", "balanced"),
    );
    let items: Vec<_> = block_on(decoded.collect());
    let mut out = Vec::new();
    let mut error = None;
    for item in items {
        assert!(error.is_none(), "no item should follow the terminal error");
        match item {
            Ok(result) => out.push(result),
            Err(failure) => error = Some(failure),
        }
    }
    (out, error)
}

#[test]
fn request_body_serializes_model_context_and_options() {
    let tool_spec = ModelToolSpec {
        name: "calculate".into(),
        description: "Perform calculation".into(),
        parameters: raw(r#"{"type":"object","properties":{"expr":{"type":"string"}}}"#),
        grammar: None,
        defer_loading: false,
    };
    let context = vec![
        ContextItem::User {
            parts: vec![
                Part::Text {
                    text: "What is 2+2?".into(),
                },
                Part::Image {
                    mime: "image/png".into(),
                    bytes: b"\x89PNG\r\n\x1a\n".as_slice().into(),
                },
            ],
        },
        ContextItem::Assistant {
            source: dal_core::ReplaySource {
                family: Family::PiMessages,
                model: "balanced".into(),
            },
            parts: vec![
                AssistantPart::Text {
                    text: "Let me compute that.".into(),
                    replay: None,
                },
                AssistantPart::Thinking {
                    text: "2+2 equals 4".into(),
                    replay: Some(raw(r#"{"thinkingSignature":"sig-123","redacted":false}"#)),
                },
                AssistantPart::ToolCall {
                    call: CallId::new("call-1"),
                    name: "calculate".into(),
                    args: raw(r#"{"expr":"2+2"}"#),
                    replay: None,
                },
            ],
            replay: None,
        },
        ContextItem::ToolResult {
            call: CallId::new("call-1"),
            name: "calculate".into(),
            is_error: false,
            parts: vec![Part::Text { text: "4".into() }],
        },
    ];
    let req = request(vec![tool_spec], context);
    let p = plan(Some("high"), Some(0.7));
    let bytes = request_body(&req, "radius", &p).expect("request body lowers");
    let text = String::from_utf8(bytes).expect("valid UTF-8");

    assert!(text.contains(r#""model":"balanced""#));
    assert!(text.contains(r#""temperature":0.7"#));
    assert!(text.contains(r#""maxTokens":4096"#));
    assert!(text.contains(r#""reasoning":"high""#));
    assert!(text.contains(r#""sessionId":"test-session""#));
    // dal has no toolChoice channel; options must not send toolChoice:"auto"
    assert!(!text.contains(r#""toolChoice""#));
    assert!(text.contains(r#""role":"system""#));
    assert!(text.contains("You are a helpful assistant."));
    assert!(text.contains(r#""toolsAdded":[{"#));
    assert!(text.contains(r#""name":"calculate""#));
    assert!(text.contains(r#""role":"user""#));
    assert!(text.contains("What is 2+2?"));
    assert!(text.contains(r#""mimeType":"image/png""#));
    assert!(text.contains(r#""role":"assistant""#));
    assert!(text.contains(r#""api":"pi-messages""#));
    assert!(text.contains(r#""provider":"radius""#));
    assert!(text.contains(r#""stopReason":"toolUse""#));
    assert!(text.contains(r#""thinkingSignature":"sig-123""#));
    assert!(text.contains(r#""type":"text","text":"Let me compute that.""#));
    assert!(text.contains(r#""type":"thinking","thinking":"2+2 equals 4""#));
    assert!(text.contains(r#""type":"toolCall""#));
    assert!(text.contains(r#""role":"toolResult""#));
    assert!(text.contains(r#""toolCallId":"call-1""#));
}

#[test]
fn fragmented_frames_reassemble_and_decode() {
    let sse_source = concat!(
        "data: {\"type\":\"start\"}\n\n",
        "data: {\"type\":\"text_delta\",\"contentIndex\":0,\"delta\":\"Frag\"}\n\n",
        "data: {\"type\":\"text_delta\",\"contentIndex\":0,\"delta\":\"mented \"}\n\n",
        "data: {\"type\":\"text_delta\",\"contentIndex\":0,\"delta\":\"stream\"}\n\n",
        "data: {\"type\":\"done\",\"reason\":\"stop\",\"usage\":{\"input\":10,\"output\":3}}\n\n"
    );
    let bytes = sse_source.as_bytes();
    let chunks: Vec<Vec<u8>> = bytes.chunks(7).map(<[u8]>::to_vec).collect();
    let sse_stream = crate::sse::decode_stream(stream::iter(chunks));
    let decoded = decode_events(sse_stream, PiMessagesDecoder::new("radius", "balanced"));
    let items: Vec<Result<StreamEvent, ProviderError>> = block_on(decoded.collect());

    let mut texts = Vec::new();
    let mut saw_stop = false;
    for item in items {
        match item.expect("clean decode") {
            StreamEvent::TextDelta { text } => texts.push(text),
            StreamEvent::ToolCallsDone { calls } => {
                assert_eq!(calls, [] as [crate::stream::ToolCall; 0]);
            }
            StreamEvent::Usage { usage } => {
                assert_eq!(usage.input_tokens, 10);
                assert_eq!(usage.output_tokens, 3);
            }
            StreamEvent::Stop { reason } => {
                assert_eq!(reason, StopReason::EndTurn);
                saw_stop = true;
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
    assert_eq!(texts.concat(), "Fragmented stream");
    assert!(saw_stop);
}

#[test]
fn tool_call_sequence_decodes_start_deltas_end() {
    let (events, error) = run(&[
        r#"{"type":"start"}"#,
        r#"{"type":"toolcall_start","contentIndex":0,"id":"call-99","toolName":"fetch"}"#,
        r#"{"type":"toolcall_delta","contentIndex":0,"delta":"{\"url\":"}"#,
        r#"{"type":"toolcall_delta","contentIndex":0,"delta":"\"https://example.com\"}"}"#,
        r#"{"type":"toolcall_end","contentIndex":0,"toolCall":{"type":"toolCall","id":"call-99","name":"fetch","arguments":{"url":"https://example.com"}}}"#,
        r#"{"type":"done","reason":"toolUse","usage":{"input":50,"output":25}}"#,
    ]);
    assert!(error.is_none());

    assert_eq!(
        events,
        vec![
            StreamEvent::ToolCallStarted {
                id: "call-99".into(),
                name: "fetch".into(),
            },
            StreamEvent::ToolArgsDelta {
                id: "call-99".into(),
                fragment: b"{\"url\":".to_vec(),
            },
            StreamEvent::ToolArgsDelta {
                id: "call-99".into(),
                fragment: b"\"https://example.com\"}".to_vec(),
            },
            StreamEvent::ToolCallsDone {
                calls: vec![ToolCall {
                    id: "call-99".into(),
                    name: "fetch".into(),
                    args: ToolArgs::from_bytes(b"{\"url\":\"https://example.com\"}"),
                }],
            },
            StreamEvent::Usage {
                usage: Usage {
                    input_tokens: 50,
                    cached_input_tokens: 0,
                    output_tokens: 25,
                    reasoning_tokens: None,
                    cache_write_tokens: 0,
                    premium_requests: None,
                    cost_usd: None,
                },
            },
            StreamEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ]
    );
}

#[test]
fn tool_call_incomplete_flag_marks_truncated() {
    let (events, error) = run(&[
        r#"{"type":"toolcall_start","contentIndex":0,"id":"call-100","toolName":"bash"}"#,
        r#"{"type":"toolcall_delta","contentIndex":0,"delta":"{\"cmd\":\"cargo t"}"#,
        r#"{"type":"toolcall_end","contentIndex":0,"toolCall":{"type":"toolCall","id":"call-100","name":"bash","arguments":{},"incomplete":true}}"#,
        r#"{"type":"done","reason":"stop","usage":{"input":10,"output":10}}"#,
    ]);
    assert!(error.is_none());

    let calls_done = events
        .iter()
        .find_map(|e| match e {
            StreamEvent::ToolCallsDone { calls } => Some(calls),
            _ => None,
        })
        .expect("ToolCallsDone emitted");

    assert_eq!(calls_done.len(), 1);
    assert_eq!(calls_done[0].id, "call-100");
    assert_eq!(calls_done[0].args, ToolArgs::Truncated);
}

#[test]
fn tool_call_non_object_arguments_yields_invalid() {
    let (events, error) = run(&[
        r#"{"type":"toolcall_start","contentIndex":0,"id":"call-101","toolName":"read"}"#,
        r#"{"type":"toolcall_delta","contentIndex":0,"delta":"[1, 2, 3]"}"#,
        r#"{"type":"done","reason":"stop","usage":{"input":10,"output":10}}"#,
    ]);
    assert!(error.is_none());

    let calls_done = events
        .iter()
        .find_map(|e| match e {
            StreamEvent::ToolCallsDone { calls } => Some(calls),
            _ => None,
        })
        .expect("ToolCallsDone emitted");

    assert_eq!(calls_done.len(), 1);
    assert!(matches!(&calls_done[0].args, ToolArgs::Invalid { .. }));
}

#[test]
fn usage_on_done_maps_all_counters_with_cache_inclusion() {
    // Usage contract: input_tokens = input + cacheRead + cacheWrite;
    // output_tokens = output - reasoning
    let (events, error) = run(&[
        r#"{"type":"text_delta","contentIndex":0,"delta":"Hi"}"#,
        r#"{"type":"done","reason":"stop","usage":{"input":120,"output":45,"cacheRead":30,"cacheWrite":15,"reasoning":10,"cost":{"total":0.0025}}}"#,
    ]);
    assert!(error.is_none());

    let usage = events
        .iter()
        .find_map(|e| match e {
            StreamEvent::Usage { usage } => Some(usage),
            _ => None,
        })
        .expect("usage emitted");

    assert_eq!(usage.input_tokens, 120 + 30 + 15);
    assert_eq!(usage.cached_input_tokens, 30);
    assert_eq!(usage.cache_write_tokens, 15);
    assert_eq!(usage.output_tokens, 45 - 10);
    assert_eq!(usage.reasoning_tokens, Some(10));
    assert_eq!(usage.cost_usd, Some(0.0025));
}

#[test]
fn thinking_end_with_signature_emits_replay() {
    let (events, error) = run(&[
        r#"{"type":"thinking_start","contentIndex":0}"#,
        r#"{"type":"thinking_delta","contentIndex":0,"delta":"pondering"}"#,
        r#"{"type":"thinking_end","contentIndex":0,"content":"pondering","contentSignature":"sig-xyz","redacted":false}"#,
        r#"{"type":"done","reason":"stop","usage":{"input":5,"output":5}}"#,
    ]);
    assert!(error.is_none());

    let replay = events
        .iter()
        .find_map(|e| match e {
            StreamEvent::Replay { payload } => Some(payload),
            _ => None,
        })
        .expect("Replay emitted");

    assert_eq!(replay.family, Family::PiMessages);
    assert_eq!(replay.model.as_ref(), "balanced");
    let json_text = replay.item.as_str();
    assert!(json_text.contains(r#""thinkingSignature":"sig-xyz""#));
    assert!(json_text.contains(r#""redacted":false"#));
}

#[test]
fn error_event_with_error_message_yields_typed_status_error() {
    let (events, error) = run(&[
        r#"{"type":"text_delta","contentIndex":0,"delta":"partial"}"#,
        r#"{"type":"error","reason":"error","errorMessage":"upstream gateway timeout"}"#,
    ]);

    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0], StreamEvent::TextDelta { text } if text == "partial"));

    let err = error.expect("error returned");
    assert!(
        matches!(
            &err,
            ProviderError::Status {
                family: Family::PiMessages,
                status: 200,
                message,
            } if message == "upstream gateway timeout"
        ),
        "actual: {err:?}"
    );
}

#[test]
fn error_event_aborted_yields_typed_status_error() {
    let (events, error) = run(&[
        r#"{"type":"text_delta","contentIndex":0,"delta":"start"}"#,
        r#"{"type":"error","reason":"aborted"}"#,
    ]);

    assert_eq!(events.len(), 1);
    let err = error.expect("error returned");
    assert!(
        matches!(
            &err,
            ProviderError::Status {
                family: Family::PiMessages,
                status: 200,
                message,
            } if message.contains("aborted")
        ),
        "actual: {err:?}"
    );
}

#[test]
fn cancellation_via_stream_cut() {
    let (events, error) = run(&[r#"{"type":"text_delta","contentIndex":0,"delta":"incomplete"}"#]);

    assert_eq!(events.len(), 1);
    assert!(matches!(error, Some(ProviderError::StreamCut)));
}

#[test]
fn malformed_event_data_yields_protocol_error() {
    let (events, error) = run(&[
        r#"{"type":"text_delta","contentIndex":0,"delta":"before"}"#,
        "not valid JSON at all",
    ]);

    assert_eq!(events.len(), 1);
    assert!(
        matches!(
            &error,
            Some(ProviderError::Protocol {
                family: Family::PiMessages,
                ..
            })
        ),
        "actual: {error:?}"
    );
}

#[test]
fn text_and_thinking_stream_deltas() {
    let (events, error) = run(&[
        r#"{"type":"start"}"#,
        r#"{"type":"thinking_start","contentIndex":0}"#,
        r#"{"type":"thinking_delta","contentIndex":0,"delta":"Reflecting on math."}"#,
        r#"{"type":"thinking_end","contentIndex":0,"content":"Reflecting on math."}"#,
        r#"{"type":"text_start","contentIndex":1}"#,
        r#"{"type":"text_delta","contentIndex":1,"delta":"Hello! "}"#,
        r#"{"type":"text_delta","contentIndex":1,"delta":"How can I help?"}"#,
        r#"{"type":"text_end","contentIndex":1,"content":"Hello! How can I help?"}"#,
        r#"{"type":"done","reason":"stop","usage":{"input":5,"output":12}}"#,
    ]);
    assert!(error.is_none());

    assert_eq!(
        events,
        vec![
            StreamEvent::ReasoningDelta {
                text: "Reflecting on math.".into(),
            },
            StreamEvent::TextDelta {
                text: "Hello! ".into(),
            },
            StreamEvent::TextDelta {
                text: "How can I help?".into(),
            },
            StreamEvent::ToolCallsDone { calls: Vec::new() },
            StreamEvent::Usage {
                usage: Usage {
                    input_tokens: 5,
                    cached_input_tokens: 0,
                    output_tokens: 12,
                    reasoning_tokens: None,
                    cache_write_tokens: 0,
                    premium_requests: None,
                    cost_usd: None,
                },
            },
            StreamEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ]
    );
}

#[test]
fn tool_call_truncation_on_length_stop() {
    let (events, error) = run(&[
        r#"{"type":"toolcall_start","contentIndex":0,"id":"call-10","toolName":"read"}"#,
        r#"{"type":"toolcall_delta","contentIndex":0,"delta":"{\"path\":\"/etc/p"}"#,
        r#"{"type":"done","reason":"length","usage":{"input":100,"output":100}}"#,
    ]);
    assert!(error.is_none());

    assert_eq!(
        events,
        vec![
            StreamEvent::ToolCallStarted {
                id: "call-10".into(),
                name: "read".into(),
            },
            StreamEvent::ToolArgsDelta {
                id: "call-10".into(),
                fragment: b"{\"path\":\"/etc/p".to_vec(),
            },
            StreamEvent::ToolCallsDone {
                calls: vec![ToolCall {
                    id: "call-10".into(),
                    name: "read".into(),
                    args: ToolArgs::Truncated,
                }],
            },
            StreamEvent::Usage {
                usage: Usage {
                    input_tokens: 100,
                    cached_input_tokens: 0,
                    output_tokens: 100,
                    reasoning_tokens: None,
                    cache_write_tokens: 0,
                    premium_requests: None,
                    cost_usd: None,
                },
            },
            StreamEvent::Stop {
                reason: StopReason::MaxTokens,
            },
        ]
    );
}

#[test]
fn foreign_family_replay_is_omitted_while_content_survives() {
    let context = vec![ContextItem::Assistant {
        source: dal_core::ReplaySource {
            family: Family::Chat,
            model: "balanced".into(),
        },
        parts: vec![AssistantPart::Thinking {
            text: "deep thought".into(),
            replay: Some(raw(
                r#"{"thinkingSignature":"foreign-sig","redacted":false}"#,
            )),
        }],
        replay: None,
    }];
    let req = request(Vec::new(), context);
    let p = plan(None, None);
    let bytes = request_body(&req, "radius", &p).expect("request body lowers");
    let text = String::from_utf8(bytes).expect("valid UTF-8");

    assert!(text.contains(r#""type":"thinking","thinking":"deep thought""#));
    assert!(!text.contains("foreign-sig"));
    assert!(!text.contains("thinkingSignature"));
    assert!(text.contains(r#""api":"openai""#));
}

#[test]
fn different_model_replay_is_omitted_while_content_survives() {
    let context = vec![ContextItem::Assistant {
        source: dal_core::ReplaySource {
            family: Family::PiMessages,
            model: "cheap".into(),
        },
        parts: vec![AssistantPart::Thinking {
            text: "cheap thought".into(),
            replay: Some(raw(r#"{"thinkingSignature":"other-sig","redacted":false}"#)),
        }],
        replay: None,
    }];
    let req = request(Vec::new(), context);
    let p = plan(None, None);
    let bytes = request_body(&req, "radius", &p).expect("request body lowers");
    let text = String::from_utf8(bytes).expect("valid UTF-8");

    assert!(text.contains(r#""type":"thinking","thinking":"cheap thought""#));
    assert!(!text.contains("other-sig"));
    assert!(!text.contains("thinkingSignature"));
    assert!(text.contains(r#""model":"cheap""#));
    assert!(text.contains(r#""api":"pi-messages""#));
}
