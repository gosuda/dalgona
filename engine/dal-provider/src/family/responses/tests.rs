use std::{error::Error, sync::Arc};

use dal_core::{ModelToolSpec, Purpose, ReplaySource, RequestParams, ThinkingLevel};
use futures::executor::block_on;
use sonic_rs::{JsonContainerTrait, JsonValueTrait};

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
                premium_requests: None,
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
            // Arguments from a response cut short by the token limit are never
            // run, even for a call the server already closed.
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
            provider: None,
        },
        system: Arc::from("Be brief."),
        tools: Arc::from([ModelToolSpec {
            name: "read".into(),
            description: "Read a file.".into(),
            parameters: RawJson::parse(r#"{"type":"object", "properties":{}}"#)?,
            grammar: None,
            defer_loading: false,
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
                replay: None,
                parts: vec![
                    AssistantPart::Thinking {
                        text: "t".into(),
                        replay: Some(RawJson::parse(
                            r#"{"type":"reasoning","id":"rs_1","encrypted_content":"e", "n":1e+02}"#,
                        )?),
                    },
                    AssistantPart::Text {
                        text: "Reading.".into(),
                        replay: None,
                    },
                    AssistantPart::ToolCall {
                        call: CallId::new("call_1"),
                        name: "read".into(),
                        args: RawJson::parse(r#"{"path": "a.rs"}"#)?,
                        replay: None,
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
        defer_loading: false,
    }]);
    request.context = Arc::from([ContextItem::Assistant {
        source: ReplaySource {
            family: Family::Responses,
            model: "gpt-5".into(),
        },
        replay: None,
        parts: vec![AssistantPart::ToolCall {
            call: CallId::new("call-1"),
            name: "deploy.web-x.list".into(),
            args: RawJson::parse("{}")?,
            replay: None,
        }],
    }]);
    for family in [Family::Responses, Family::Codex] {
        request.model = ModelRoute::Api {
            family,
            model: "gpt-5".into(),
            provider: None,
        };
        let body = String::from_utf8(request_body(
            &request,
            WireThinking::OpenAi { effort: None },
            false,
            BodyPolicy {
                output_cap: OutputCap::Honor { ceiling: None },
                tool_schemas: ToolSchemas::Verbatim,
            },
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
    let body = request_body(
        &request,
        high,
        true,
        BodyPolicy {
            output_cap: OutputCap::Honor { ceiling: None },
            tool_schemas: ToolSchemas::Verbatim,
        },
    )?;
    assert_eq!(String::from_utf8(body)?, EXPECTED_BODY);
    assert_eq!(
        request_body(
            &request,
            high,
            true,
            BodyPolicy {
                output_cap: OutputCap::Honor { ceiling: None },
                tool_schemas: ToolSchemas::Verbatim,
            },
        )?,
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
        let body = String::from_utf8(request_body(
            &foreign,
            high,
            true,
            BodyPolicy {
                output_cap: OutputCap::Honor { ceiling: None },
                tool_schemas: ToolSchemas::Verbatim,
            },
        )?)?;
        assert!(
            !body.contains("rs_1"),
            "replayed to a foreign route: {body}"
        );
        assert!(body.contains(r#"},{"type":"message","role":"assistant""#));
    }
    Ok(())
}

#[test]
fn body_ignores_text_tool_and_message_replay() -> TestResult {
    let high = WireThinking::OpenAi {
        effort: Some("high"),
    };
    let mut laden = request()?;
    let plain = request_body(
        &laden,
        high,
        true,
        BodyPolicy {
            output_cap: OutputCap::Honor { ceiling: None },
            tool_schemas: ToolSchemas::Verbatim,
        },
    )?;
    let mut context = laden.context.to_vec();
    let ContextItem::Assistant { parts, replay, .. } = &mut context[1] else {
        return Err("fixture assistant item is missing".into());
    };
    *replay = Some(RawJson::parse(r#"{"foreign":"payload"}"#)?);
    let AssistantPart::Text { replay, .. } = &mut parts[1] else {
        return Err("fixture text part is missing".into());
    };
    *replay = Some(RawJson::parse(r#"{"foreign":"payload"}"#)?);
    let AssistantPart::ToolCall { replay, .. } = &mut parts[2] else {
        return Err("fixture tool call is missing".into());
    };
    *replay = Some(RawJson::parse(r#"{"foreign":"payload"}"#)?);
    laden.context = Arc::from(context);
    assert_eq!(
        request_body(
            &laden,
            high,
            true,
            BodyPolicy {
                output_cap: OutputCap::Honor { ceiling: None },
                tool_schemas: ToolSchemas::Verbatim,
            },
        )?,
        plain
    );
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
        BodyPolicy {
            output_cap: OutputCap::Honor { ceiling: None },
            tool_schemas: ToolSchemas::Verbatim,
        },
    )?)?;
    assert!(
        none.contains(r#""parallel_tool_calls":true,"reasoning":{"effort":"none"},"store":false"#)
    );
    let omitted = String::from_utf8(request_body(
        &request,
        WireThinking::OpenAi { effort: None },
        true,
        BodyPolicy {
            output_cap: OutputCap::Honor { ceiling: None },
            tool_schemas: ToolSchemas::Verbatim,
        },
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
        BodyPolicy {
            output_cap: OutputCap::Honor { ceiling: None },
            tool_schemas: ToolSchemas::Verbatim,
        },
    )?)?;
    assert!(body.contains(r#""tools":[],"tool_choice":"auto","parallel_tool_calls":true"#));
    assert!(!body.contains("prompt_cache_key"));
    assert!(!body.contains("previous_response_id"));
    Ok(())
}

/// The request of [`request`] routed through `family`, asking for `cap` as
/// its output ceiling.
fn capped_request(family: Family, cap: Option<u32>) -> Result<ModelRequest, Box<dyn Error>> {
    let mut request = request()?;
    request.model = ModelRoute::Api {
        family,
        model: "gpt-5".into(),
        provider: None,
    };
    request.params.max_output_tokens = cap;
    Ok(request)
}

/// The `max_output_tokens` member of a serialized body, read back as a number.
fn output_cap(body: &[u8]) -> Result<Option<u64>, Box<dyn Error>> {
    let body: sonic_rs::Value = sonic_rs::from_slice(body)?;
    Ok(body
        .get("max_output_tokens")
        .and_then(sonic_rs::JsonValueTrait::as_u64))
}

/// The body a plain `openai_responses` host receives for `request`.
fn plain_host_body(request: &ModelRequest) -> Result<Vec<u8>, Box<dyn Error>> {
    let credential = Credential::None;
    let built = wire(&ResponsesRequest {
        request,
        thinking: WireThinking::OpenAi {
            effort: Some("high"),
        },
        reasoning_summary: false,
        max_output: None,
        auth: AuthStyle::None,
        credential: &credential,
        session_id: SessionId::new_v7(),
        user_agent: "dalgon/test",
        tool_schemas: ToolSchemas::Verbatim,
    })?;
    Ok(built.body)
}

#[test]
fn a_requested_output_cap_is_the_max_output_tokens_of_a_plain_responses_host() -> TestResult {
    for cap in [1_024_u32, 4_096, 128_000] {
        let request = capped_request(Family::Responses, Some(cap))?;
        let body = plain_host_body(&request)?;
        assert_eq!(output_cap(&body)?, Some(u64::from(cap)), "HTTPS body");
        // The WebSocket `response.create` frame is cut from the same body,
        // so the cap must survive that cut too.
        let frame = crate::family::codex::websocket_frame(&body)?;
        assert_eq!(output_cap(&frame)?, Some(u64::from(cap)), "WebSocket frame");
    }
    // Without a requested cap the member stays out: the host applies its own
    // default rather than a catalog value the caller never asked for.
    let uncapped = capped_request(Family::Responses, None)?;
    assert_eq!(output_cap(&plain_host_body(&uncapped)?)?, None);
    Ok(())
}

#[test]
fn the_codex_backend_never_receives_a_requested_output_cap() -> TestResult {
    use crate::{
        auth::credential::{OAuthCredential, SecretString},
        family::codex::{self, CodexRequest},
    };

    // The Codex backend rejects a caller-supplied output cap (omp pin
    // `openai-codex-responses.ts:1589-1596`), so a requested cap must not
    // change a byte of what Codex receives, over HTTPS or WebSocket.
    let credential = OAuthCredential {
        access_token: SecretString::from("test-access"),
        refresh_token: SecretString::from("test-refresh"),
        expires_at: None,
        id_token: None,
        account_id: Some(String::from("acct-7f3a9c")),
        project_id: None,
    };
    let high = WireThinking::OpenAi {
        effort: Some("high"),
    };
    let codex_body = |cap: Option<u32>| -> Result<Vec<u8>, Box<dyn Error>> {
        let request = capped_request(Family::Codex, cap)?;
        Ok(codex::build(&CodexRequest {
            request: &request,
            thinking: high,
            reasoning_summaries: false,
            credential: &credential,
            session_id: SessionId::new_v7(),
            user_agent: "dalgon/test",
        })?
        .body)
    };
    let uncapped = codex_body(None)?;
    for cap in [1_024_u32, 4_096, 128_000] {
        let capped = codex_body(Some(cap))?;
        assert_eq!(output_cap(&capped)?, None, "Codex body for {cap}");
        assert_eq!(capped, uncapped, "Codex body for {cap}");
        let frame = codex::websocket_frame(&capped)?;
        assert_eq!(output_cap(&frame)?, None, "Codex frame for {cap}");
    }
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

#[test]
fn before_turn_text_follows_its_user_message_in_the_body() -> TestResult {
    let mut request = request()?;
    request.context = Arc::from([
        ContextItem::User {
            parts: vec![Part::Text {
                text: "question".into(),
            }],
        },
        ContextItem::User {
            parts: vec![Part::Text {
                text: "first\nsecond".into(),
            }],
        },
    ]);
    let high = WireThinking::OpenAi {
        effort: Some("high"),
    };
    let body = String::from_utf8(request_body(
        &request,
        high,
        true,
        BodyPolicy {
            output_cap: OutputCap::Honor { ceiling: None },
            tool_schemas: ToolSchemas::Verbatim,
        },
    )?)?;
    assert!(
        body.contains(concat!(
            r#"{"type":"message","role":"user","content":[{"type":"input_text","text":"question"}]},"#,
            r#"{"type":"message","role":"user","content":[{"type":"input_text","text":"first\nsecond"}]}"#,
        )),
        "{body}"
    );
    Ok(())
}

#[test]
fn done_markers_stay_ignored_but_still_charge_the_event_budget() {
    let mut decoder = Decoder::new(Family::Responses, "model");
    let mut out = Vec::new();
    for _ in 0..crate::stream_budget::STREAM_EVENTS_LIMIT {
        assert!(matches!(decoder.feed("[DONE]", &mut out), Ok(false)));
    }
    assert!(matches!(
        decoder.feed("[DONE]", &mut out),
        Err(ProviderError::Limit(crate::error::LimitError::StreamEvents))
    ));
    assert_eq!(out, [] as [StreamEvent; 0]);
}

#[test]
fn cumulative_websocket_and_sse_arguments_share_the_event_budget() {
    let mut decoder = Decoder::new(Family::Responses, "model");
    let start = r#"{"type":"response.output_item.added","item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"run"}}"#;
    let delta = r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"x"}"#;
    let mut out = Vec::new();
    assert!(decoder.feed(start, &mut out).is_ok());
    out.clear();
    for _ in 1..crate::stream_budget::STREAM_EVENTS_LIMIT {
        assert!(decoder.feed(delta, &mut out).is_ok());
        out.clear();
    }
    assert!(matches!(
        decoder.feed(delta, &mut out),
        Err(ProviderError::Limit(crate::error::LimitError::StreamEvents))
    ));
    assert_eq!(
        decoder.calls[0].deltas.len(),
        crate::stream_budget::STREAM_EVENTS_LIMIT - 1
    );
}

#[test]
fn reasoning_beyond_the_output_count_leaves_zero_output_and_cache_writes_survive() -> TestResult {
    // (output, reasoning, expected output): below, at, and above the
    // boundary where reasoning would consume the whole output count.
    for (output, reasoning, expected) in [(3_u64, 9_u64, 0_u64), (9, 9, 0), (10, 9, 1)] {
        let terminal = format!(
            r#"{{"type":"response.completed","response":{{"output":[],"usage":{{"input_tokens":10,"input_tokens_details":{{"cached_tokens":4,"cache_write_tokens":2}},"output_tokens":{output},"output_tokens_details":{{"reasoning_tokens":{reasoning}}}}}}}}}"#
        );
        let events = ok(run(&[terminal.as_str()]))?;
        let usage = events
            .iter()
            .find_map(|event| match event {
                StreamEvent::Usage { usage } => Some(*usage),
                _ => None,
            })
            .ok_or("a completed response carries one usage")?;
        assert_eq!(
            usage,
            Usage {
                input_tokens: 10,
                cached_input_tokens: 4,
                output_tokens: expected,
                reasoning_tokens: Some(reasoning),
                cache_write_tokens: 2,
                premium_requests: None,
                cost_usd: None,
            },
            "output {output}, reasoning {reasoning}"
        );
    }
    Ok(())
}

#[test]
fn repeated_or_skipping_sequence_numbers_are_accepted() {
    // Only a decrease is an error: the same number again, the next one, and
    // a jump are all fine, up to the largest representable value.
    let numbered = |numbers: &[&str]| -> Vec<String> {
        numbers
            .iter()
            .map(|number| {
                format!(r#"{{"type":"response.in_progress","sequence_number":{number}}}"#)
            })
            .collect()
    };
    for numbers in [
        &["5", "5", "6", "100"][..],
        &["0", "0"][..],
        &["18446744073709551615", "18446744073709551615"][..],
    ] {
        let events = numbered(numbers);
        let refs: Vec<&str> = events.iter().map(String::as_str).collect();
        // No terminal arrives, so the only error is the cut itself.
        assert!(
            matches!(run(&refs).as_slice(), [Err(ProviderError::StreamCut)]),
            "{numbers:?}"
        );
    }
}

#[test]
fn an_unnumbered_event_does_not_forgive_a_later_decrease() {
    let results = run(&[
        r#"{"type":"response.in_progress","sequence_number":5}"#,
        r#"{"type":"response.in_progress"}"#,
        r#"{"type":"response.in_progress","sequence_number":4}"#,
    ]);
    assert!(
        matches!(
            results.as_slice(),
            [Err(ProviderError::Protocol { detail, .. })] if detail == "sequence_number went backwards"
        ),
        "{results:?}"
    );
}

/// A schema nested `levels` deep in `anyOf`: the depth-limit refusal probe.
fn nested_any_of(levels: usize) -> String {
    let mut schema = String::from(r#"{"type":"string"}"#);
    for _ in 0..levels {
        schema = format!(r#"{{"anyOf":[{schema}]}}"#);
    }
    schema
}

/// The union-root schema the B.AI Responses policy probes repair: one branch
/// carries `properties`, `choice`, and an out-of-`u64` `minimum`; the other
/// carries a nested `branch` union.
fn bai_union_schema() -> &'static str {
    r#"{"optional":true,"anyOf":[{"type":"object","properties":{"properties":{"type":"string","default":{"properties":"data"}},"choice":{"anyOf":[{"type":"string","const":"a"},{"type":"string","const":"b"}]},"big":{"type":"integer","minimum":18446744073709551617}},"required":["properties"]},{"type":"object","properties":{"branch":{"type":"object","anyOf":[{"properties":{"x":{"type":"string"}}},{"properties":{"y":{"type":"integer"}}}]}}}]}"#
}

/// A request carrying the union-root schema as its single tool.
fn bai_union_request() -> Result<ModelRequest, Box<dyn Error>> {
    let mut request = request()?;
    request.tools = Arc::from([ModelToolSpec {
        name: "bai_tool".into(),
        description: "B.AI tool.".into(),
        parameters: RawJson::parse(bai_union_schema())?,
        grammar: None,
        defer_loading: false,
    }]);
    Ok(request)
}

/// The lowered parameters of the union-root request with the raw body text
/// the out-of-`u64` `minimum` assertion needs.
fn repaired_union_parameters(
    request: &ModelRequest,
) -> Result<(String, sonic_rs::Value), Box<dyn Error>> {
    let text = String::from_utf8(request_body(
        request,
        WireThinking::OpenAi { effort: None },
        false,
        BodyPolicy {
            output_cap: OutputCap::Honor { ceiling: None },
            tool_schemas: ToolSchemas::OpenAiCompat,
        },
    )?)?;
    // The past-`u64` minimum cannot survive a parsed round trip, so its exact
    // digits stay a text assertion; everything else is JSON semantics.
    assert!(text.contains(r#""minimum":18446744073709551617"#), "{text}");
    let body: sonic_rs::Value = sonic_rs::from_slice(text.as_bytes())?;
    let parameters = body
        .get("tools")
        .and_then(sonic_rs::Value::as_array)
        .and_then(|tools| tools.first())
        .and_then(|tool| tool.get("parameters"));
    let Some(parameters) = parameters.cloned() else {
        return Err("lowered body has no tools[0].parameters".into());
    };
    Ok((text, parameters))
}

/// The member of the lowered union-root parameters stored under `key`.
fn lowered_member<'a>(parameters: &'a sonic_rs::Value, key: &str) -> Option<&'a sonic_rs::Value> {
    parameters
        .get("properties")
        .and_then(|members| members.get(key))
}

#[test]
fn bai_responses_schema_policy_merges_the_union_root() -> TestResult {
    let request = bai_union_request()?;
    let (text, parameters) = repaired_union_parameters(&request)?;
    assert_eq!(
        parameters
            .get("type")
            .and_then(sonic_rs::JsonValueTrait::as_str),
        Some("object"),
        "{text}"
    );
    assert!(parameters.get("optional").is_none(), "{text}");
    let Some(properties) = parameters
        .get("properties")
        .and_then(sonic_rs::Value::as_object)
    else {
        return Err("lowered parameters have no properties object".into());
    };
    let mut names: Vec<&str> = properties.iter().map(|(name, _)| name).collect();
    names.sort_unstable();
    assert_eq!(names, ["big", "branch", "choice", "properties"], "{text}");
    // Only the first union branch required `properties`, so the merged root
    // requires nothing: the merge keeps branch-common names only.
    assert!(parameters.get("required").is_none(), "{text}");
    Ok(())
}

#[test]
fn bai_responses_schema_policy_lowers_scalar_members() -> TestResult {
    let request = bai_union_request()?;
    let (text, parameters) = repaired_union_parameters(&request)?;
    let Some(choice) = lowered_member(&parameters, "choice") else {
        return Err("lowered parameters lost the choice member".into());
    };
    assert_eq!(
        choice
            .get("type")
            .and_then(sonic_rs::JsonValueTrait::as_str),
        Some("string"),
        "{text}"
    );
    let union: Vec<&str> = choice
        .get("enum")
        .and_then(sonic_rs::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(sonic_rs::JsonValueTrait::as_str)
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(union, ["a", "b"], "{text}");
    let Some(big) = lowered_member(&parameters, "big") else {
        return Err("lowered parameters lost the big member".into());
    };
    assert_eq!(
        big.get("type").and_then(sonic_rs::JsonValueTrait::as_str),
        Some("integer"),
        "{text}"
    );
    Ok(())
}

#[test]
fn bai_responses_schema_policy_keeps_branch_variants_and_data_default() -> TestResult {
    let request = bai_union_request()?;
    let (text, parameters) = repaired_union_parameters(&request)?;
    let branches: Vec<&sonic_rs::Value> = lowered_member(&parameters, "branch")
        .and_then(|branch| branch.get("anyOf"))
        .and_then(sonic_rs::Value::as_array)
        .map(|branches| branches.iter().collect())
        .unwrap_or_default();
    assert_eq!(branches.len(), 2, "{text}");
    assert_eq!(
        branches[0]
            .get("type")
            .and_then(sonic_rs::JsonValueTrait::as_str),
        Some("object"),
        "{text}"
    );
    assert_eq!(
        branches[1]
            .get("type")
            .and_then(sonic_rs::JsonValueTrait::as_str),
        Some("object"),
        "{text}"
    );
    assert!(
        branches[0]
            .get("properties")
            .and_then(|members| members.get("x"))
            .is_some(),
        "{text}"
    );
    assert!(
        branches[1]
            .get("properties")
            .and_then(|members| members.get("y"))
            .is_some(),
        "{text}"
    );
    assert_eq!(
        lowered_member(&parameters, "properties")
            .and_then(|member| member.get("default"))
            .and_then(|default| default.get("properties"))
            .and_then(sonic_rs::JsonValueTrait::as_str),
        Some("data"),
        "{text}"
    );
    Ok(())
}

#[test]
fn bai_responses_schema_policy_verbatim_keeps_schema_bytes() -> TestResult {
    let request = bai_union_request()?;
    let untouched = String::from_utf8(request_body(
        &request,
        WireThinking::OpenAi { effort: None },
        false,
        BodyPolicy {
            output_cap: OutputCap::Honor { ceiling: None },
            tool_schemas: ToolSchemas::Verbatim,
        },
    )?)?;
    assert!(untouched.contains(bai_union_schema()), "{untouched}");
    Ok(())
}

#[test]
fn bai_responses_schema_policy_refuses_nonobject_and_too_deep_roots() -> TestResult {
    let mut request = request()?;
    request.tools = Arc::from([ModelToolSpec {
        name: "invalid_root".into(),
        description: "Invalid.".into(),
        parameters: RawJson::parse(r#"{"type":"string"}"#)?,
        grammar: None,
        defer_loading: false,
    }]);
    let error = request_body(
        &request,
        WireThinking::OpenAi { effort: None },
        false,
        BodyPolicy {
            output_cap: OutputCap::Honor { ceiling: None },
            tool_schemas: ToolSchemas::OpenAiCompat,
        },
    )
    .expect_err("a non-object Responses tool schema is invalid");
    assert!(matches!(
        error,
        ProviderError::InvalidRequest { message }
            if message.contains("tool `invalid_root`")
                && message.contains("root schema must have type")
    ));

    let deep = nested_any_of(140);
    request.tools = Arc::from([ModelToolSpec {
        name: "deep_root".into(),
        description: "Deep.".into(),
        parameters: RawJson::parse(&deep)?,
        grammar: None,
        defer_loading: false,
    }]);
    let error = request_body(
        &request,
        WireThinking::OpenAi { effort: None },
        false,
        BodyPolicy {
            output_cap: OutputCap::Honor { ceiling: None },
            tool_schemas: ToolSchemas::OpenAiCompat,
        },
    )
    .expect_err("a schema past the shared depth limit is invalid");
    assert!(matches!(
        error,
        ProviderError::InvalidRequest { message }
            if message.contains("tool `deep_root`")
                && message.contains("128")
    ));
    Ok(())
}

#[test]
fn cf_aig_gateway_key_rides_its_own_header() -> TestResult {
    use crate::auth::credential::{OAuthCredential, SecretString};

    let request = request()?;
    let credential = Credential::ApiKey {
        key: SecretString::from("gateway-key"),
    };
    let built = wire(&ResponsesRequest {
        request: &request,
        thinking: WireThinking::OpenAi { effort: None },
        reasoning_summary: false,
        max_output: None,
        auth: AuthStyle::CfAigAuthorization,
        credential: &credential,
        session_id: SessionId::new_v7(),
        user_agent: "dalgon/test",
        tool_schemas: ToolSchemas::Verbatim,
    })?;
    assert_eq!(
        built.auth_header,
        Some(("cf-aig-authorization", String::from("Bearer gateway-key")))
    );

    let oauth = Credential::OAuth(OAuthCredential {
        access_token: SecretString::from("test-access"),
        refresh_token: SecretString::from("test-refresh"),
        expires_at: None,
        id_token: None,
        account_id: None,
        project_id: None,
    });
    let denied = wire(&ResponsesRequest {
        request: &request,
        thinking: WireThinking::OpenAi { effort: None },
        reasoning_summary: false,
        max_output: None,
        auth: AuthStyle::CfAigAuthorization,
        credential: &oauth,
        session_id: SessionId::new_v7(),
        user_agent: "dalgon/test",
        tool_schemas: ToolSchemas::Verbatim,
    });
    assert!(
        matches!(denied, Err(ProviderError::InvalidRequest { .. })),
        "an OAuth credential must not ride the gateway header"
    );
    Ok(())
}
