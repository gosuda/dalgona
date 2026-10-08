use std::{error::Error, sync::Arc};

use dal_core::{ModelToolSpec, Purpose, ReplaySource, RequestParams, ThinkingLevel};
use futures::executor::block_on;

use super::*;
use crate::sse::{decode_stream, encode};

type TestResult = Result<(), Box<dyn Error>>;

/// Frames `data` as SSE, splits the bytes into 7-byte chunks, and decodes.
fn run(data: &[&str]) -> Vec<Result<StreamEvent, ProviderError>> {
    let events: Vec<SseEvent> = data
        .iter()
        .map(|data| SseEvent {
            name: None,
            data: (*data).into(),
        })
        .collect();
    let wire = encode(&events);
    let chunks: Vec<Vec<u8>> = wire.chunks(7).map(<[u8]>::to_vec).collect();
    block_on(
        decode(
            decode_stream(stream::iter(chunks)),
            Family::Responses,
            "gpt-5",
        )
        .collect(),
    )
}

fn ok(results: Vec<Result<StreamEvent, ProviderError>>) -> Result<Vec<StreamEvent>, ProviderError> {
    results.into_iter().collect()
}

fn calls_of(events: &[StreamEvent]) -> Vec<ToolCall> {
    events
        .iter()
        .find_map(|event| match event {
            StreamEvent::ToolCallsDone { calls } => Some(calls.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

const REASONING_DONE: &str = r#"{"id":"rs_1", "type":"reasoning","summary":[{"type":"summary_text","text":"Check the file."}],"encrypted_content":"gAAAAB","n":1e+02}"#;

fn turn() -> Vec<String> {
    vec![
            r#"{"type":"response.created","sequence_number":0,"response":{"id":"resp_1","object":"response","status":"in_progress","output":[],"usage":null}}"#.into(),
            r#"{"type":"response.in_progress","sequence_number":1,"response":{"id":"resp_1","status":"in_progress","output":[],"usage":null}}"#.into(),
            r#"{"type":"response.output_item.added","sequence_number":2,"output_index":0,"item":{"id":"rs_1","type":"reasoning","summary":[]}}"#.into(),
            r#"{"type":"response.reasoning_summary_part.added","sequence_number":3,"item_id":"rs_1","output_index":0,"summary_index":0,"part":{"type":"summary_text","text":""}}"#.into(),
            r#"{"type":"response.reasoning_summary_text.delta","sequence_number":4,"item_id":"rs_1","output_index":0,"summary_index":0,"delta":"Check the file."}"#.into(),
            r#"{"type":"response.reasoning_summary_text.done","sequence_number":5,"item_id":"rs_1","output_index":0,"summary_index":0,"text":"Check the file."}"#.into(),
            r#"{"type":"response.reasoning_summary_part.done","sequence_number":6,"item_id":"rs_1","output_index":0,"summary_index":0,"part":{"type":"summary_text","text":"Check the file."}}"#.into(),
            format!(r#"{{"type":"response.output_item.done","sequence_number":7,"output_index":0,"item":{REASONING_DONE}}}"#),
            r#"{"type":"response.output_item.added","sequence_number":8,"output_index":1,"item":{"id":"msg_1","type":"message","role":"assistant","status":"in_progress","content":[]}}"#.into(),
            r#"{"type":"response.content_part.added","sequence_number":9,"item_id":"msg_1","output_index":1,"content_index":0,"part":{"type":"output_text","text":"","annotations":[]}}"#.into(),
            r#"{"type":"response.output_text.delta","sequence_number":10,"item_id":"msg_1","output_index":1,"content_index":0,"delta":"Reading.","logprobs":[]}"#.into(),
            r#"{"type":"response.output_text.done","sequence_number":11,"item_id":"msg_1","output_index":1,"content_index":0,"text":"Reading.","logprobs":[]}"#.into(),
            r#"{"type":"response.content_part.done","sequence_number":12,"item_id":"msg_1","output_index":1,"content_index":0,"part":{"type":"output_text","text":"Reading.","annotations":[]}}"#.into(),
            r#"{"type":"response.output_item.done","sequence_number":13,"output_index":1,"item":{"id":"msg_1","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Reading.","annotations":[]}]}}"#.into(),
            r#"{"type":"response.output_item.added","sequence_number":14,"output_index":2,"item":{"id":"fc_1","type":"function_call","status":"in_progress","call_id":"call_1","name":"read","arguments":""}}"#.into(),
            r#"{"type":"response.function_call_arguments.delta","sequence_number":15,"item_id":"fc_1","output_index":2,"delta":"{\"path\":"}"#.into(),
            r#"{"type":"response.function_call_arguments.done","sequence_number":16,"item_id":"fc_1","output_index":2,"arguments":"{\"path\":\"a.rs\"}"}"#.into(),
            r#"{"type":"response.output_item.done","sequence_number":17,"output_index":2,"item":{"id":"fc_1","type":"function_call","status":"completed","call_id":"call_1","name":"read","arguments":"{\"path\":\"a.rs\"}"}}"#.into(),
            format!(
                r#"{{"type":"response.completed","sequence_number":18,"response":{{"id":"resp_1","status":"completed","output":[{REASONING_DONE},{{"id":"fc_1","type":"function_call","call_id":"call_1","name":"read","arguments":"{{\"path\":\"a.rs\"}}"}}],"usage":{{"input_tokens":120,"input_tokens_details":{{"cached_tokens":100}},"output_tokens":50,"output_tokens_details":{{"reasoning_tokens":20}},"total_tokens":170}}}}}}"#
            ),
        ]
}

#[test]
fn canonical_turn_decodes_in_causal_order_and_ignores_a_trailing_done() -> TestResult {
    let mut data = turn();
    data.push(DONE.into());
    data.push(
        r#"{"type":"response.output_text.delta","sequence_number":19,"delta":"late"}"#.into(),
    );
    let data: Vec<&str> = data.iter().map(String::as_str).collect();
    let events = ok(run(&data))?;
    let expected = vec![
        StreamEvent::ReasoningDelta {
            text: "Check the file.".into(),
        },
        StreamEvent::Replay {
            payload: ReplayPayload {
                family: Family::Responses,
                model: "gpt-5".into(),
                item: RawJson::parse(REASONING_DONE)?,
            },
        },
        StreamEvent::TextDelta {
            text: "Reading.".into(),
        },
        StreamEvent::ToolCallStarted {
            id: "call_1".into(),
            name: "read".into(),
        },
        StreamEvent::ToolArgsDelta {
            id: "call_1".into(),
            fragment: br#"{"path":"#.to_vec(),
        },
        StreamEvent::ToolCallsDone {
            calls: vec![ToolCall {
                id: "call_1".into(),
                name: "read".into(),
                args: ToolArgs::Parsed(RawJson::parse(r#"{"path":"a.rs"}"#)?),
            }],
        },
        StreamEvent::Usage {
            usage: Usage {
                input_tokens: 120,
                cached_input_tokens: 100,
                output_tokens: 30,
                reasoning_tokens: Some(20),
                cache_write_tokens: 0,
                cost_usd: None,
            },
        },
        StreamEvent::Stop {
            reason: StopReason::ToolUse,
        },
    ];
    assert_eq!(events, expected);
    let StreamEvent::Replay { payload } = &events[1] else {
        return Err("second event is not a replay".into());
    };
    assert_eq!(payload.item.as_str(), REASONING_DONE);
    Ok(())
}

#[test]
fn reasoning_without_encrypted_content_is_backfilled_from_the_completed_output() -> TestResult {
    let completed = r#"{"id":"rs_9","type":"reasoning","summary":[],"encrypted_content":"full"}"#;
    let terminal = format!(
        r#"{{"type":"response.completed","response":{{"output":[{completed}],"usage":null}}}}"#
    );
    let events = ok(run(&[
        r#"{"type":"response.output_item.done","item":{"id":"rs_9","type":"reasoning","summary":[]}}"#,
        r#"{"type":"response.output_text.delta","delta":"hi"}"#,
        terminal.as_str(),
    ]))?;
    assert!(matches!(&events[0], StreamEvent::TextDelta { text } if text == "hi"));
    assert!(matches!(
        &events[1],
        StreamEvent::Replay { payload } if payload.item.as_str() == completed
    ));
    assert_eq!(events.len(), 5);
    assert_eq!(
        events[4],
        StreamEvent::Stop {
            reason: StopReason::EndTurn
        }
    );
    let unrecoverable = ok(run(&[
        r#"{"type":"response.output_item.done","item":{"id":"rs_9","type":"reasoning","summary":[]}}"#,
        r#"{"type":"response.completed","response":{"output":[{"id":"rs_9","type":"reasoning","summary":[]}]}}"#,
    ]))?;
    assert!(
        !unrecoverable
            .iter()
            .any(|event| matches!(event, StreamEvent::Replay { .. })),
        "replayed a reasoning item without encrypted_content"
    );
    Ok(())
}

#[test]
fn final_arguments_prefer_item_done_then_arguments_done_then_deltas() -> TestResult {
    let added = r#"{"type":"response.output_item.added","item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"f","arguments":""}}"#;
    let deltas = [
        r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"{\"a\":"}"#,
        r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"1}"}"#,
    ];
    let args_done = r#"{"type":"response.function_call_arguments.done","item_id":"fc_1","arguments":"{\"a\":13}"}"#;
    let item_done = r#"{"type":"response.output_item.done","item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"f","arguments":"{\"a\":23}"}}"#;
    let completed = r#"{"type":"response.completed","response":{"output":[{"type":"function_call","call_id":"call_1","name":"f","arguments":""}]}}"#;
    for (script, expected) in [
        (
            vec![added, deltas[0], deltas[1], args_done, item_done, completed],
            r#"{"a":23}"#,
        ),
        (
            vec![added, deltas[0], deltas[1], args_done, completed],
            r#"{"a":13}"#,
        ),
        (vec![added, deltas[0], deltas[1], completed], r#"{"a":1}"#),
    ] {
        let events = ok(run(&script))?;
        let calls = calls_of(&events);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].args, ToolArgs::Parsed(RawJson::parse(expected)?));
        assert_eq!(
            events.last(),
            Some(&StreamEvent::Stop {
                reason: StopReason::ToolUse
            })
        );
    }
    Ok(())
}

#[test]
fn empty_final_arguments_parse_as_an_empty_object() -> TestResult {
    let events = ok(run(&[
        r#"{"type":"response.output_item.done","item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"now","arguments":""}}"#,
        r#"{"type":"response.completed","response":{"output":[{"type":"function_call","call_id":"call_1","name":"now","arguments":""}]}}"#,
    ]))?;
    assert_eq!(
        events[0],
        StreamEvent::ToolCallStarted {
            id: "call_1".into(),
            name: "now".into()
        }
    );
    assert_eq!(
        calls_of(&events)[0].args,
        ToolArgs::Parsed(RawJson::parse("{}")?)
    );
    Ok(())
}

#[test]
fn incomplete_maps_the_reason_and_truncates_every_call_under_max_tokens() -> TestResult {
    let closed = r#"{"type":"response.output_item.done","item":{"id":"fc_0","type":"function_call","call_id":"call_0","name":"f","arguments":"{\"b\":2}"}}"#;
    let added = r#"{"type":"response.output_item.added","item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"f","arguments":""}}"#;
    let delta =
        r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"{\"a\""}"#;
    for (reason, stop) in [
        (r#""max_output_tokens""#, StopReason::MaxTokens),
        (r#""content_filter""#, StopReason::Refusal),
        (
            r#""max_messages""#,
            StopReason::Other("max_messages".into()),
        ),
    ] {
        let terminal = format!(
            r#"{{"type":"response.incomplete","response":{{"status":"incomplete","incomplete_details":{{"reason":{reason}}},"output":[]}}}}"#
        );
        let events = ok(run(&[closed, added, delta, terminal.as_str()]))?;
        let calls = calls_of(&events);
        if stop == StopReason::MaxTokens {
            // D-13: arguments from a length-cut response are never run,
            // even for a call the server already closed.
            assert_eq!(calls[0].args, ToolArgs::Truncated);
            assert_eq!(calls[1].args, ToolArgs::Truncated);
        } else {
            assert_eq!(
                calls[0].args,
                ToolArgs::Parsed(RawJson::parse(r#"{"b":2}"#)?)
            );
            assert!(matches!(calls[1].args, ToolArgs::Invalid { .. }));
        }
        assert_eq!(events.last(), Some(&StreamEvent::Stop { reason: stop }));
    }
    Ok(())
}

#[test]
fn failures_map_by_code_and_end_the_stream_after_delivery() {
    let delta = r#"{"type":"response.output_text.delta","delta":"par"}"#;
    let failed = |code: &str| {
        format!(
            r#"{{"type":"response.failed","response":{{"status":"failed","error":{{"code":"{code}","message":"boom"}}}}}}"#
        )
    };
    let server_error = failed("server_error");
    let server = run(&[delta, server_error.as_str(), delta]);
    assert_eq!(server.len(), 2);
    assert!(matches!(&server[0], Ok(StreamEvent::TextDelta { text }) if text == "par"));
    assert!(matches!(
        &server[1],
        Err(ProviderError::Status { family: Family::Responses, status: 200, message }) if message == "boom"
    ));
    assert!(matches!(
        run(&[failed("usage_not_included").as_str()]).as_slice(),
        [Err(ProviderError::UsageNotIncluded { message })] if message == "boom"
    ));
    assert!(matches!(
        run(&[failed("rate_limit_exceeded").as_str()]).as_slice(),
        [Err(ProviderError::RateLimited { message, retry_after: None })] if message == "boom"
    ));
    assert!(matches!(
        run(&[r#"{"type":"error","code":"rate_limit_exceeded","message":"slow"}"#]).as_slice(),
        [Err(ProviderError::RateLimited { message, retry_after: None })] if message == "slow"
    ));
    assert!(matches!(
        run(&[r#"{"type":"error","code":"usage_not_included","message":"no"}"#]).as_slice(),
        [Err(ProviderError::Status { status: 200, message, .. })] if message == "no"
    ));
    for (event, code) in [
        (
            r#"{"type":"response.failed","response":{"error":{"code":"context_length_exceeded","message":"too long"}}}"#,
            "context_length_exceeded",
        ),
        (
            r#"{"type":"error","code":"context_window_exceeded","message":"too long"}"#,
            "context_window_exceeded",
        ),
    ] {
        assert!(
            matches!(
                run(&[event]).as_slice(),
                [Err(ProviderError::ContextOverflow { family: Family::Responses, code: c, message })]
                    if c == code && message == "too long"
            ),
            "{event} did not map to ContextOverflow"
        );
    }
    assert!(matches!(
            run(&[r#"{"type":"response.failed","response":{"error":{"code":"server_error","message":"context length exceeded"}}}"#]).as_slice(),
            [Err(ProviderError::Status { status: 200, .. })]
        ));
    assert!(matches!(
        run(&[r#"{"type":"response.failed","response":{"error":null}}"#]).as_slice(),
        [Err(ProviderError::Status { status: 200, message, .. })] if message.is_empty()
    ));
}

#[test]
fn decreasing_sequence_number_is_a_protocol_error() -> TestResult {
    let results = run(&[
        r#"{"type":"response.in_progress","sequence_number":5}"#,
        r#"{"type":"response.in_progress","sequence_number":4}"#,
    ]);
    let [Err(error)] = results.as_slice() else {
        return Err(format!("expected one error, got {results:?}").into());
    };
    assert_eq!(
        error.to_string(),
        "openai sent an invalid stream: sequence_number went backwards"
    );
    Ok(())
}

#[test]
fn malformed_or_missing_terminal_is_an_error() {
    assert!(matches!(
        run(&[r#"{"type":"response.output_text.delta","delta":"a"}"#]).as_slice(),
        [
            Ok(StreamEvent::TextDelta { .. }),
            Err(ProviderError::StreamCut)
        ]
    ));
    assert!(matches!(
        run(&[DONE]).as_slice(),
        [Err(ProviderError::StreamCut)]
    ));
    for malformed in [
        r#"{"type":"response.completed","response":{"output":[{"id":"x"}]}}"#,
        r#"{"type":"response.completed","response":"done"}"#,
        r#"{"type":"response.completed""#,
        r#"{"sequence_number":1}"#,
    ] {
        assert!(
            matches!(
                run(&[malformed]).as_slice(),
                [Err(ProviderError::Protocol {
                    family: Family::Responses,
                    ..
                })]
            ),
            "accepted {malformed}"
        );
    }
}

fn request() -> Result<ModelRequest, Box<dyn Error>> {
    Ok(ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::Responses,
            model: "gpt-5".into(),
        },
        system: Arc::from("Be brief."),
        tools: Arc::from([ModelToolSpec {
            name: "read".into(),
            description: "Read a file.".into(),
            parameters: RawJson::parse(r#"{"type":"object", "properties":{}}"#)?,
            grammar: None,
        }]),
        context: Arc::from([
            ContextItem::User {
                parts: vec![
                    Part::Text {
                        text: "Look".into(),
                    },
                    Part::Image {
                        mime: "image/png".into(),
                        bytes: Box::from(*b"\x89PNG"),
                    },
                ],
            },
            ContextItem::Assistant {
                source: ReplaySource {
                    family: Family::Responses,
                    model: "gpt-5".into(),
                },
                parts: vec![
                    AssistantPart::Thinking {
                        text: "t".into(),
                        replay: Some(RawJson::parse(
                            r#"{"type":"reasoning","id":"rs_1","encrypted_content":"e", "n":1e+02}"#,
                        )?),
                    },
                    AssistantPart::Text {
                        text: "Reading.".into(),
                    },
                    AssistantPart::ToolCall {
                        call: CallId::new("call_1"),
                        name: "read".into(),
                        args: RawJson::parse(r#"{"path": "a.rs"}"#)?,
                    },
                ],
            },
            ContextItem::ToolResult {
                call: CallId::new("call_1"),
                name: "read".into(),
                is_error: false,
                parts: vec![
                    Part::Text {
                        text: "fn main".into(),
                    },
                    Part::Image {
                        mime: "image/jpeg".into(),
                        bytes: Box::from(*b"img"),
                    },
                ],
            },
            ContextItem::User {
                parts: vec![Part::Text {
                    text: "Next".into(),
                }],
            },
        ]),
        params: RequestParams {
            thinking: ThinkingLevel::High,
            effort: None,
            temperature: None,
            max_output_tokens: None,
        },
        cache_key: Some("sess-1".into()),
    })
}

const EXPECTED_BODY: &str = concat!(
    r#"{"model":"gpt-5","instructions":"Be brief.","input":["#,
    r#"{"type":"message","role":"user","content":[{"type":"input_text","text":"Look"},{"type":"input_image","detail":"auto","image_url":"data:image/png;base64,iVBORw=="}]},"#,
    r#"{"type":"reasoning","id":"rs_1","encrypted_content":"e", "n":1e+02},"#,
    r#"{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Reading."}]},"#,
    r#"{"type":"function_call","call_id":"call_1","name":"read","arguments":"{\"path\": \"a.rs\"}"},"#,
    r#"{"type":"function_call_output","call_id":"call_1","output":"fn main"},"#,
    r#"{"type":"message","role":"user","content":[{"type":"input_text","text":"Images from tool call call_1."},{"type":"input_image","detail":"auto","image_url":"data:image/jpeg;base64,aW1n"}]},"#,
    r#"{"type":"message","role":"user","content":[{"type":"input_text","text":"Next"}]}],"#,
    r#""tools":[{"type":"function","name":"read","description":"Read a file.","parameters":{"type":"object", "properties":{}}}],"#,
    r#""tool_choice":"auto","parallel_tool_calls":true,"reasoning":{"effort":"high","summary":"auto"},"#,
    r#""store":false,"stream":true,"include":["reasoning.encrypted_content"],"prompt_cache_key":"sess-1"}"#,
);

#[test]
fn responses_and_codex_bodies_map_mapped_tool_names_for_advertisements_and_history() -> TestResult {
    let mut request = request()?;
    request.tools = Arc::from([ModelToolSpec {
        name: "deploy.web-x.list".into(),
        description: "Mapped tool.".into(),
        parameters: RawJson::parse(r#"{"type":"object","properties":{}}"#)?,
        grammar: None,
    }]);
    request.context = Arc::from([ContextItem::Assistant {
        source: ReplaySource {
            family: Family::Responses,
            model: "gpt-5".into(),
        },
        parts: vec![AssistantPart::ToolCall {
            call: CallId::new("call-1"),
            name: "deploy.web-x.list".into(),
            args: RawJson::parse("{}")?,
        }],
    }]);
    for family in [Family::Responses, Family::Codex] {
        request.model = ModelRoute::Api {
            family,
            model: "gpt-5".into(),
        };
        let body = String::from_utf8(request_body(
            &request,
            WireThinking::OpenAi { effort: None },
            false,
        )?)?;
        assert_eq!(
            body.matches(r#""name":"deploy_web-x_list_41588e56""#)
                .count(),
            2
        );
        assert!(!body.contains("deploy.web-x.list"));
    }
    Ok(())
}

#[test]
fn body_is_byte_exact_and_replays_only_to_the_bound_route() -> TestResult {
    let request = request()?;
    let high = WireThinking::OpenAi {
        effort: Some("high"),
    };
    let body = request_body(&request, high, true)?;
    assert_eq!(String::from_utf8(body)?, EXPECTED_BODY);
    assert_eq!(
        request_body(&request, high, true)?,
        EXPECTED_BODY.as_bytes()
    );

    for (family, model) in [(Family::Responses, "gpt-4.1"), (Family::Codex, "gpt-5")] {
        let mut foreign = request.clone();
        let mut context = foreign.context.to_vec();
        let ContextItem::Assistant { source, .. } = &mut context[1] else {
            return Err("fixture assistant item is missing".into());
        };
        source.family = family;
        source.model = model.into();
        foreign.context = Arc::from(context);
        let body = String::from_utf8(request_body(&foreign, high, true)?)?;
        assert!(
            !body.contains("rs_1"),
            "replayed to a foreign route: {body}"
        );
        assert!(body.contains(r#"},{"type":"message","role":"assistant""#));
    }
    Ok(())
}

#[test]
fn off_sends_effort_none_only_when_supported() -> TestResult {
    let request = request()?;
    let none = String::from_utf8(request_body(
        &request,
        WireThinking::OpenAi {
            effort: Some("none"),
        },
        true,
    )?)?;
    assert!(
        none.contains(r#""parallel_tool_calls":true,"reasoning":{"effort":"none"},"store":false"#)
    );
    let omitted = String::from_utf8(request_body(
        &request,
        WireThinking::OpenAi { effort: None },
        true,
    )?)?;
    assert!(omitted.contains(r#""parallel_tool_calls":true,"store":false"#));
    assert!(!omitted.contains("reasoning\":"));
    assert!(!omitted.contains("previous_response_id"));
    Ok(())
}

#[test]
fn body_without_tools_keeps_empty_tool_members() -> TestResult {
    let mut request = request()?;
    request.tools = Arc::from([]);
    request.cache_key = None;
    let body = String::from_utf8(request_body(
        &request,
        WireThinking::OpenAi { effort: None },
        false,
    )?)?;
    assert!(body.contains(r#""tools":[],"tool_choice":"auto","parallel_tool_calls":true"#));
    assert!(!body.contains("prompt_cache_key"));
    assert!(!body.contains("previous_response_id"));
    Ok(())
}

#[test]
fn a_repeated_terminal_event_yields_one_stop_and_nothing_after() {
    let mut events = turn();
    let completed = events.last().unwrap().clone();
    events.push(completed);
    let refs: Vec<&str> = events.iter().map(String::as_str).collect();
    let results = run(&refs);
    let stops = results
        .iter()
        .filter(|result| matches!(result, Ok(StreamEvent::Stop { .. })))
        .count();
    assert_eq!(stops, 1);
    assert!(matches!(results.last(), Some(Ok(StreamEvent::Stop { .. }))));
}

#[test]
fn invalid_utf8_in_a_delta_is_replaced_not_fatal() {
    let mut wire = b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"a".to_vec();
    wire.extend_from_slice(&[0xFF, 0xFE]);
    wire.extend_from_slice(b"b\"}\n\n");
    let results: Vec<_> = block_on(
        decode(
            decode_stream(stream::iter(vec![wire])),
            Family::Responses,
            "gpt-5",
        )
        .collect(),
    );
    let Some(Ok(StreamEvent::TextDelta { text })) = results.first() else {
        panic!("expected a text delta first, got {results:?}");
    };
    assert_eq!(text, "a\u{FFFD}\u{FFFD}b");
    assert!(matches!(
        results.last(),
        Some(Err(ProviderError::StreamCut))
    ));
}
