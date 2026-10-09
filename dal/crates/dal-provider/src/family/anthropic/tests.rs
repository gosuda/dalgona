use std::sync::Arc;

use dal_core::{
    CallId, ModelRoute, ModelToolSpec, Purpose, ReplaySource, RequestParams, ThinkingLevel,
};
use futures::executor::block_on;

use super::*;
use crate::sse;

const BASIC_TEXT_STREAM: &str = concat!(
    "event: message_start\n",
    r#"data: {"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"claude-opus-5-5","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":25,"output_tokens":1}}}"#,
    "\n\nevent: content_block_start\n",
    r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
    "\n\nevent: ping\n",
    r#"data: {"type":"ping"}"#,
    "\n\nevent: content_block_delta\n",
    r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
    "\n\nevent: content_block_delta\n",
    r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"!"}}"#,
    "\n\nevent: content_block_stop\n",
    r#"data: {"type":"content_block_stop","index":0}"#,
    "\n\nevent: message_delta\n",
    r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":15}}"#,
    "\n\nevent: message_stop\n",
    r#"data: {"type":"message_stop"}"#,
    "\n\n",
);

/// Frames `data` payloads as `event: <type>` SSE events.
fn frames(datas: &[&str]) -> String {
    let mut wire = String::new();
    for data in datas {
        let kind = sonic_rs::get_from_str(data, ["type"])
            .ok()
            .and_then(|kind| kind.as_str().map(str::to_owned))
            .unwrap_or_default();
        let _ =
            std::fmt::Write::write_fmt(&mut wire, format_args!("event: {kind}\ndata: {data}\n\n"));
    }
    wire
}

/// Decodes wire bytes delivered in 7-byte chunks, as a network would.
fn decode(wire: &str, oauth: bool) -> Vec<Result<StreamEvent, ProviderError>> {
    decode_bound(wire, oauth, None)
}

/// Decodes like [`decode`], recording `prefix` as the producing request's
/// replay binding.
fn decode_bound(
    wire: &str,
    oauth: bool,
    prefix: Option<&str>,
) -> Vec<Result<StreamEvent, ProviderError>> {
    let chunks: Vec<Vec<u8>> = wire.as_bytes().chunks(7).map(<[u8]>::to_vec).collect();
    let events = sse::decode_stream(stream::iter(chunks));
    block_on(decode_stream(events, "claude-sonnet-5".into(), oauth, prefix).collect())
}

fn assert_protocol_failure(wire: &str) {
    assert!(decode(wire, false).iter().any(|result| matches!(
        result,
        Err(ProviderError::Protocol {
            family: Family::Anthropic,
            ..
        })
    )));
}

fn ok(results: Vec<Result<StreamEvent, ProviderError>>) -> Vec<StreamEvent> {
    results.into_iter().map(Result::unwrap).collect()
}

fn text(text: &str) -> StreamEvent {
    StreamEvent::TextDelta { text: text.into() }
}

fn usage(input: u64, output: u64) -> StreamEvent {
    StreamEvent::Usage {
        usage: Usage {
            input_tokens: input,
            cached_input_tokens: 0,
            output_tokens: output,
            reasoning_tokens: None,
            cache_write_tokens: 0,
            cost_usd: None,
        },
    }
}

fn stop(reason: StopReason) -> StreamEvent {
    StreamEvent::Stop { reason }
}

fn calls(results: &[StreamEvent]) -> Vec<ToolCall> {
    results
        .iter()
        .find_map(|event| match event {
            StreamEvent::ToolCallsDone { calls } => Some(calls.clone()),
            _ => None,
        })
        .expect("ToolCallsDone")
}

fn parsed(raw: &str) -> ToolArgs {
    ToolArgs::Parsed(RawJson::parse(raw).unwrap())
}

const START: &str =
    r#"{"type":"message_start","message":{"usage":{"input_tokens":25,"output_tokens":1}}}"#;
const STOP: &str = r#"{"type":"message_stop"}"#;

#[test]
fn basic_text_stream() {
    assert_eq!(
        ok(decode(BASIC_TEXT_STREAM, false)),
        vec![
            text("Hello"),
            text("!"),
            StreamEvent::ToolCallsDone { calls: vec![] },
            usage(25, 15),
            stop(StopReason::EndTurn),
        ]
    );
}

#[test]
fn thinking_stream_replays_signed_block() {
    let wire = frames(&[
        START,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"1071 = 2 × 462 + 147"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"\n462 = 3 × 147 + 21"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"EqQBCgIYAhIM"}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"GCD is 21."}}"#,
        r#"{"type":"content_block_stop","index":1}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null}}"#,
        STOP,
    ]);
    let replay = RawJson::parse(
            r#"{"type":"thinking","thinking":"1071 = 2 × 462 + 147\n462 = 3 × 147 + 21","signature":"EqQBCgIYAhIM"}"#,
        )
        .unwrap();
    assert_eq!(
        ok(decode(&wire, false)),
        vec![
            StreamEvent::ReasoningDelta {
                text: "1071 = 2 × 462 + 147".into()
            },
            StreamEvent::ReasoningDelta {
                text: "\n462 = 3 × 147 + 21".into()
            },
            StreamEvent::Replay {
                payload: ReplayPayload {
                    family: Family::Anthropic,
                    model: "claude-sonnet-5".into(),
                    item: replay,
                }
            },
            text("GCD is 21."),
            StreamEvent::ToolCallsDone { calls: vec![] },
            usage(25, 1),
            stop(StopReason::EndTurn),
        ]
    );
}

#[test]
fn empty_signature_is_never_replayed_and_redacted_is_verbatim() {
    let redacted = r#"{"type":"redacted_thinking","data":"EmwKAhgBEgy3va3pzix/LafPsn4a"}"#;
    let redacted_start =
        format!(r#"{{"type":"content_block_start","index":1,"content_block":{redacted}}}"#);
    let wire = frames(&[
        START,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hm"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":""}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        &redacted_start,
        r#"{"type":"content_block_stop","index":1}"#,
        STOP,
    ]);
    let events = ok(decode(&wire, false));
    let replays: Vec<&RawJson> = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Replay { payload } => Some(&payload.item),
            _ => None,
        })
        .collect();
    assert_eq!(replays, vec![&RawJson::parse(redacted).unwrap()]);
    assert_eq!(events.last(), Some(&stop(StopReason::Other("none".into()))));
}

#[test]
fn tool_arguments_split_mid_key_assemble_once() {
    let wire = frames(&[
        START,
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_01","name":"get_weather","input":{}}}"#,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":""}}"#,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"locat"}}"#,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"ion\":\"San Fra"}}"#,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"ncisco, CA\"}"}}"#,
        r#"{"type":"content_block_stop","index":1}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":891}}"#,
        STOP,
    ]);
    let events = ok(decode(&wire, false));
    assert_eq!(
        events[0],
        StreamEvent::ToolCallStarted {
            id: "toolu_01".into(),
            name: "get_weather".into()
        }
    );
    let fragments: Vec<u8> = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ToolArgsDelta { fragment, .. } => Some(fragment.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(fragments, br#"{"location":"San Francisco, CA"}"#);
    assert_eq!(
        calls(&events),
        vec![ToolCall {
            id: "toolu_01".into(),
            name: "get_weather".into(),
            args: parsed(r#"{"location":"San Francisco, CA"}"#),
        }]
    );
    assert_eq!(events.last(), Some(&stop(StopReason::ToolUse)));
}

#[test]
fn start_input_without_deltas_is_the_argument() {
    let wire = frames(&[
        START,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_2","name":"_read","input":{"path":"a.rs", "n":1.50}}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"}}"#,
        STOP,
    ]);
    let events = ok(decode(&wire, true));
    assert_eq!(
        calls(&events),
        vec![ToolCall {
            id: "toolu_2".into(),
            name: "read".into(),
            args: parsed(r#"{"path":"a.rs", "n":1.50}"#),
        }]
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, StreamEvent::ToolArgsDelta { .. }))
    );
}

#[test]
fn text_and_tool_calls_both_arrive() {
    let wire = frames(&[
        START,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Reading."}}"#,
        r#"{"type":"content_block_stop","index":1}"#,
        r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"t_a","name":"read","input":{}}}"#,
        r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"p\":1}"}}"#,
        r#"{"type":"content_block_stop","index":2}"#,
        r#"{"type":"content_block_start","index":3,"content_block":{"type":"tool_use","id":"t_b","name":"exec","input":{}}}"#,
        r#"{"type":"content_block_stop","index":3}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":40}}"#,
        STOP,
    ]);
    let events = ok(decode(&wire, false));
    assert_eq!(events[0], text("Reading."));
    assert_eq!(
        calls(&events),
        vec![
            ToolCall {
                id: "t_a".into(),
                name: "read".into(),
                args: parsed(r#"{"p":1}"#),
            },
            ToolCall {
                id: "t_b".into(),
                name: "exec".into(),
                args: parsed("{}"),
            },
        ]
    );
    assert_eq!(events.last(), Some(&stop(StopReason::ToolUse)));
}

#[test]
fn text_and_thinking_deltas_require_the_matching_open_block() {
    let text_on_thinking = frames(&[
        START,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"misattributed"}}"#,
    ]);
    assert_protocol_failure(&text_on_thinking);

    let thinking_on_text = frames(&[
        START,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"misattributed"}}"#,
    ]);
    assert_protocol_failure(&thinking_on_text);

    let unopened = frames(&[
        START,
        r#"{"type":"content_block_delta","index":4,"delta":{"type":"text_delta","text":"misattributed"}}"#,
    ]);
    assert_protocol_failure(&unopened);
}

#[test]
fn unknown_block_deltas_are_ignored() {
    let wire = frames(&[
        START,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"server","name":"web_search","input":{}}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ignored"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"ignored"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"ignored"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"ignored"}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        STOP,
    ]);
    let events = ok(decode(&wire, false));
    assert!(!events.iter().any(|event| matches!(
        event,
        StreamEvent::TextDelta { .. }
            | StreamEvent::ReasoningDelta { .. }
            | StreamEvent::ToolCallStarted { .. }
            | StreamEvent::ToolArgsDelta { .. }
    )));
}

#[test]
fn max_tokens_marks_every_tool_argument_truncated() {
    let wire = frames(&[
        START,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"start","name":"write","input":{"valid":true}}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"delta","name":"write","input":{}}}"#,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"valid\":true}"}}"#,
        r#"{"type":"content_block_stop","index":1}"#,
        r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"partial","name":"write","input":{}}}"#,
        r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"body\":\"ab"}}"#,
        r#"{"type":"content_block_stop","index":2}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"}}"#,
        STOP,
    ]);
    let events = ok(decode(&wire, false));
    let calls = calls(&events);
    assert_eq!(calls.len(), 3);
    assert!(calls.iter().all(|call| call.args == ToolArgs::Truncated));
    assert_eq!(events.last(), Some(&stop(StopReason::MaxTokens)));
}

#[test]
fn overloaded_error_ends_the_stream_before_or_after_delivery() {
    let overloaded =
        r#"{"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}"#;
    let first = decode(&frames(&[overloaded, STOP]), false);
    assert!(matches!(first.as_slice(), [Err(ProviderError::Overloaded)]));

    let wire = frames(&[
        START,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#,
        overloaded,
        STOP,
    ]);
    let late = decode(&wire, false);
    assert!(matches!(
        late.as_slice(),
        [
            Ok(StreamEvent::TextDelta { .. }),
            Err(ProviderError::Overloaded)
        ]
    ));

    let other = decode(
        &frames(&[r#"{"type":"error","error":{"type":"api_error","message":"boom"}}"#]),
        false,
    );
    assert!(matches!(
        other.as_slice(),
        [Err(ProviderError::Status { status: 500, message, .. })] if message == "boom"
    ));
}

#[test]
fn usage_is_start_overwritten_by_each_delta_member() {
    let wire = frames(&[
        r#"{"type":"message_start","message":{"usage":{"input_tokens":25,"cache_read_input_tokens":100,"cache_creation_input_tokens":50,"output_tokens":1}}}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":null},"usage":{"output_tokens":15}}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":891,"cache_read_input_tokens":null}}"#,
        STOP,
    ]);
    let events = ok(decode(&wire, false));
    assert_eq!(
        events[1],
        StreamEvent::Usage {
            usage: Usage {
                input_tokens: 175,
                cached_input_tokens: 100,
                output_tokens: 891,
                reasoning_tokens: None,
                cache_write_tokens: 50,
                cost_usd: None,
            }
        }
    );
    assert_eq!(events[2], stop(StopReason::EndTurn));
}

#[test]
fn stop_reason_map() {
    for (wire, reason) in [
        ("pause_turn", StopReason::Paused),
        ("refusal", StopReason::Refusal),
        ("stop_sequence", StopReason::EndTurn),
        (
            "model_context_window_exceeded",
            StopReason::Other("model_context_window_exceeded".into()),
        ),
    ] {
        let delta = format!(r#"{{"type":"message_delta","delta":{{"stop_reason":"{wire}"}}}}"#);
        let events = ok(decode(&frames(&[START, &delta, STOP]), false));
        assert_eq!(events.last(), Some(&stop(reason)));
    }
    let events = ok(decode(&frames(&[START, STOP]), false));
    assert_eq!(events[1], usage(25, 1));
    assert_eq!(events[2], stop(StopReason::Other("none".into())));
}

#[test]
fn end_of_input_before_message_stop_is_a_cut() {
    let cut = BASIC_TEXT_STREAM
        .rsplit_once("event: message_stop")
        .unwrap()
        .0;
    let results = decode(cut, false);
    assert!(matches!(
        results.last(),
        Some(Err(ProviderError::StreamCut))
    ));
    assert!(
        !results
            .iter()
            .any(|event| matches!(event, Ok(StreamEvent::Stop { .. })))
    );
}

fn request(context: Vec<ContextItem>, tools: Vec<ModelToolSpec>) -> ModelRequest {
    request_with_system("Be brief.", context, tools)
}

fn request_with_system(
    system: &str,
    context: Vec<ContextItem>,
    tools: Vec<ModelToolSpec>,
) -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::Anthropic,
            model: "claude-sonnet-5".into(),
        },
        system: Arc::from(system),
        tools: Arc::from(tools),
        context: Arc::from(context),
        params: RequestParams {
            thinking: ThinkingLevel::High,
            effort: None,
            temperature: None,
            max_output_tokens: None,
        },
        cache_key: None,
    }
}

fn tool(name: &str) -> ModelToolSpec {
    ModelToolSpec {
        name: name.into(),
        description: "d".into(),
        parameters: RawJson::parse(r#"{"type":"object","properties":{}}"#).unwrap(),
        grammar: None,
    }
}

fn replay_source(family: Family, model: &str) -> ReplaySource {
    ReplaySource {
        family,
        model: model.into(),
    }
}

fn history() -> Vec<ContextItem> {
    vec![
        ContextItem::User {
            parts: vec![Part::Text { text: "hi".into() }],
        },
        ContextItem::Assistant {
            source: replay_source(Family::Anthropic, "claude-sonnet-5"),
            parts: vec![
                AssistantPart::Thinking {
                    text: "t".into(),
                    replay: Some(
                        RawJson::parse(
                            r#"{ "signature" : "S", "type" : "thinking", "thinking" : "t" }"#,
                        )
                        .unwrap(),
                    ),
                },
                AssistantPart::Thinking {
                    text: "u".into(),
                    replay: Some(
                        RawJson::parse(r#"{"type":"thinking","thinking":"u","signature":""}"#)
                            .unwrap(),
                    ),
                },
                AssistantPart::Text { text: "ok".into() },
                AssistantPart::ToolCall {
                    call: CallId::new("toolu_1"),
                    name: "read".into(),
                    args: RawJson::parse(r#"{"path": "a", "n": 1.50}"#).unwrap(),
                },
                AssistantPart::ToolCall {
                    call: CallId::new("toolu_2"),
                    name: "web_search".into(),
                    args: RawJson::parse("[1]").unwrap(),
                },
            ],
        },
        ContextItem::ToolResult {
            call: CallId::new("toolu_1"),
            name: "read".into(),
            is_error: false,
            parts: vec![
                Part::Text { text: "x".into() },
                Part::Image {
                    mime: "image/png".into(),
                    bytes: Box::new([1, 2, 3]),
                },
            ],
        },
        ContextItem::ToolResult {
            call: CallId::new("toolu_2"),
            name: "web_search".into(),
            is_error: true,
            parts: vec![Part::Text { text: "no".into() }],
        },
    ]
}

fn header<'a>(wire: &'a AnthropicWire, name: &str) -> Option<&'a str> {
    wire.headers
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value.as_str())
}

fn anthropic_body(request: &ModelRequest) -> String {
    let input = AnthropicRequest {
        request,
        max_output: None,
        thinking: AnthropicThinking::Omit,
        effort: None,
        display_supported: false,
        temperature: None,
        compaction: None,
        summarize: false,
    };
    String::from_utf8(build(&input, AnthropicAuth::ApiKey("sk-ant")).unwrap().body).unwrap()
}

fn unbound_tool_context(source: ReplaySource) -> Vec<ContextItem> {
    vec![
        ContextItem::Assistant {
            source,
            parts: vec![
                AssistantPart::Thinking {
                    text: "private".into(),
                    replay: Some(
                        RawJson::parse(
                            r#"{ "type" : "thinking", "thinking" : "private", "signature" : "signed" }"#,
                        )
                        .unwrap(),
                    ),
                },
                AssistantPart::Text {
                    text: "visible".into(),
                },
                AssistantPart::ToolCall {
                    call: CallId::new("call_1"),
                    name: "read".into(),
                    args: RawJson::parse(r#"{"path":"notes.txt"}"#).unwrap(),
                },
            ],
        },
        ContextItem::ToolResult {
            call: CallId::new("call_1"),
            name: "read".into(),
            is_error: false,
            parts: vec![Part::Text { text: "done".into() }],
        },
    ]
}

fn replay_body(source: ReplaySource) -> String {
    let request = request(unbound_tool_context(source), vec![tool("read")]);
    anthropic_body(&request)
}

fn unbound_replay_body(system: &str) -> String {
    let request = request_with_system(
        system,
        vec![ContextItem::Assistant {
            source: replay_source(Family::Anthropic, "claude-sonnet-5"),
            parts: vec![
                AssistantPart::Thinking {
                    text: "private".into(),
                    replay: Some(
                        RawJson::parse(
                            r#"{ "type" : "thinking", "thinking" : "private", "signature" : "signed" }"#,
                        )
                        .unwrap(),
                    ),
                },
                AssistantPart::Text {
                    text: "visible".into(),
                },
            ],
        }],
        vec![tool("read")],
    );
    anthropic_body(&request)
}

fn settled_unbound_replay_body() -> String {
    let mut context = unbound_tool_context(replay_source(Family::Anthropic, "claude-sonnet-5"));
    context.push(ContextItem::User {
        parts: vec![Part::Text {
            text: "next prompt".into(),
        }],
    });
    let request = request_with_system("Current system.", context, vec![tool("read")]);
    anthropic_body(&request)
}

fn unbound_redacted_replay_body() -> String {
    let request = request_with_system(
        "Current system.",
        vec![ContextItem::Assistant {
            source: replay_source(Family::Anthropic, "claude-sonnet-5"),
            parts: vec![AssistantPart::Thinking {
                text: "".into(),
                replay: Some(
                    RawJson::parse(r#"{"type":"redacted_thinking","data":"opaque"}"#).unwrap(),
                ),
            }],
        }],
        vec![tool("read")],
    );
    anthropic_body(&request)
}

fn assert_replay_was_filtered(body: &str) {
    assert!(body.contains(concat!(
        r#""role":"assistant","content":[{"type":"text","text":"visible"},"#,
        r#"{"type":"tool_use","id":"call_1","name":"read","input":{"path":"notes.txt"}}]}"#
    )));
    assert!(!body.contains("private"));
    assert!(!body.contains("signature"));
    assert!(!body.contains(r#""type":"thinking""#));
}

#[test]
fn foreign_family_replay_is_omitted_without_losing_text_or_tool_calls() {
    let body = replay_body(replay_source(Family::Chat, "claude-sonnet-5"));
    assert_replay_was_filtered(&body);
}

#[test]
fn different_model_replay_is_omitted_without_losing_text_or_tool_calls() {
    let body = replay_body(replay_source(Family::Anthropic, "claude-opus-5"));
    assert_replay_was_filtered(&body);
}

#[test]
fn unbound_signed_replay_is_dropped_after_a_prefix_change() {
    let body = unbound_replay_body("Current system.");
    assert!(!body.contains("signature"));
    assert!(!body.contains(r#""type":"thinking""#));
    assert!(body.contains(r#""type":"text","text":"visible""#));
}

#[test]
fn unbound_signed_replay_stays_for_tool_use_continuation() {
    let body = replay_body(replay_source(Family::Anthropic, "claude-sonnet-5"));
    assert!(body.contains("private"));
    assert!(body.contains("signature"));
    assert!(body.contains(r#""type":"tool_use","id":"call_1""#));
    assert!(body.contains(r#""type":"tool_result","tool_use_id":"call_1""#));
}

#[test]
fn settled_unbound_signed_replay_is_dropped_after_tool_turn() {
    let body = settled_unbound_replay_body();
    assert!(!body.contains("signature"));
    assert!(!body.contains("private"));
    assert!(body.contains(r#""type":"tool_use","id":"call_1""#));
    assert!(body.contains(r#""type":"tool_result","tool_use_id":"call_1""#));
}

#[test]
fn unbound_redacted_replay_remains_verbatim() {
    let body = unbound_redacted_replay_body();
    assert!(body.contains(r#""type":"redacted_thinking","data":"opaque""#));
}

/// Builds a signed thinking replay bound to `prefix` in storage.
fn bound_replay(prefix: &str) -> RawJson {
    RawJson::parse(&format!(
        r#"{{"type":"thinking","thinking":"private","signature":"signed","dal_prefix":"{prefix}"}}"#
    ))
    .unwrap()
}

/// Builds a body whose history holds one prefix-bound signed thinking block.
fn bound_replay_body(system: &str, prefix: &str) -> String {
    bound_replay_body_with_tools(system, prefix, Vec::new())
}

fn bound_replay_body_with_tools(system: &str, prefix: &str, tools: Vec<ModelToolSpec>) -> String {
    let request = request_with_system(
        system,
        vec![ContextItem::Assistant {
            source: replay_source(Family::Anthropic, "claude-sonnet-5"),
            parts: vec![
                AssistantPart::Thinking {
                    text: "private".into(),
                    replay: Some(bound_replay(prefix)),
                },
                AssistantPart::Text {
                    text: "visible".into(),
                },
            ],
        }],
        tools,
    );
    let input = AnthropicRequest {
        request: &request,
        max_output: None,
        thinking: AnthropicThinking::Omit,
        effort: None,
        display_supported: false,
        temperature: None,
        compaction: None,
        summarize: false,
    };
    String::from_utf8(build(&input, AnthropicAuth::ApiKey("sk-ant")).unwrap().body).unwrap()
}

#[test]
fn changed_system_prompt_drops_the_stale_thinking_block() {
    // The block was produced under a prefix the current request no longer
    // sends: its stored binding names the earlier system prompt.
    let body = bound_replay_body("Current system.", "prefix-of-the-earlier-request");
    assert!(!body.contains("signature"));
    assert!(!body.contains(r#""type":"thinking""#));
    assert!(!body.contains("dal_prefix"));
    assert!(body.contains(r#""type":"text","text":"visible""#));
}

#[test]
fn unchanged_prefix_keeps_the_signed_block_without_the_binding_member() {
    let request = request_with_system(
        "Be brief.",
        vec![ContextItem::User {
            parts: vec![Part::Text { text: "hi".into() }],
        }],
        vec![],
    );
    let prefix = prefix_fingerprint(&request, false).to_string();
    let body = bound_replay_body("Be brief.", &prefix);
    assert!(body.contains(r#""type":"thinking","thinking":"private","signature":"signed""#));
    assert!(!body.contains("dal_prefix"));
    assert!(body.contains(r#""type":"text","text":"visible""#));
}

#[test]
fn a_changed_tool_list_drops_the_stale_thinking_block() {
    let request = request_with_system(
        "Be brief.",
        vec![],
        vec![ModelToolSpec {
            name: "read".into(),
            description: "d".into(),
            parameters: RawJson::parse(r#"{"type":"object","properties":{}}"#).unwrap(),
            grammar: None,
        }],
    );
    let prefix = prefix_fingerprint(&request, false).to_string();
    let tools = vec![tool("read"), tool("grep")];
    let body = bound_replay_body_with_tools("Be brief.", &prefix, tools);
    assert!(!body.contains("signature"));
    assert!(!body.contains(r#""type":"thinking""#));
}

#[test]
fn signed_replay_records_the_producing_prefix() {
    let wire = frames(&[
        START,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"why"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"EqQBCgIYAhIM"}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null}}"#,
        STOP,
    ]);
    let results = ok(decode_bound(&wire, false, Some("prefix-2026")));
    let Some(StreamEvent::Replay { payload }) = results
        .iter()
        .find(|event| matches!(event, StreamEvent::Replay { .. }))
    else {
        panic!("signed thinking block replays");
    };
    assert_eq!(payload.family, Family::Anthropic);
    assert_eq!(
        payload.item.as_str(),
        r#"{"type":"thinking","thinking":"why","signature":"EqQBCgIYAhIM","dal_prefix":"prefix-2026"}"#
    );
    // Without a producing prefix nothing is recorded, and the stored block
    // stays byte for byte what the API sent.
    let results = ok(decode(&wire, false));
    let Some(StreamEvent::Replay { payload }) = results
        .iter()
        .find(|event| matches!(event, StreamEvent::Replay { .. }))
    else {
        panic!("signed thinking block replays");
    };
    assert_eq!(
        payload.item.as_str(),
        r#"{"type":"thinking","thinking":"why","signature":"EqQBCgIYAhIM"}"#
    );
}

#[test]
fn adaptive_display_is_omitted_when_unsupported() {
    let request = request(vec![], vec![]);
    let input = AnthropicRequest {
        request: &request,
        max_output: None,
        thinking: AnthropicThinking::Adaptive,
        effort: None,
        display_supported: false,
        temperature: None,
        compaction: None,
        summarize: false,
    };
    let body =
        String::from_utf8(build(&input, AnthropicAuth::ApiKey("sk-ant")).unwrap().body).unwrap();
    assert!(body.contains(r#""thinking":{"type":"adaptive"}"#));
    assert!(!body.contains(r#""display""#));
}

#[test]
fn api_key_body_maps_mapped_tool_names_for_advertisements_and_history() {
    let request = request(
        vec![
            ContextItem::Assistant {
                source: replay_source(Family::Anthropic, "claude-sonnet-5"),
                parts: vec![AssistantPart::ToolCall {
                    call: CallId::new("call-1"),
                    name: "deploy.web-x.list".into(),
                    args: RawJson::parse("{}").unwrap(),
                }],
            },
            ContextItem::ToolResult {
                call: CallId::new("call-1"),
                name: "deploy.web-x.list".into(),
                is_error: false,
                parts: vec![Part::Text {
                    text: "done".into(),
                }],
            },
        ],
        vec![tool("deploy.web-x.list")],
    );
    let input = AnthropicRequest {
        request: &request,
        max_output: None,
        thinking: AnthropicThinking::Omit,
        effort: None,
        display_supported: false,
        temperature: None,
        compaction: None,
        summarize: false,
    };
    let body =
        String::from_utf8(build(&input, AnthropicAuth::ApiKey("sk-ant")).unwrap().body).unwrap();
    assert_eq!(
        body.matches(r#""name":"deploy_web-x_list_41588e56""#)
            .count(),
        2
    );
    assert!(!body.contains("deploy.web-x.list"));
}

#[test]
fn api_key_adaptive_body_is_exact() {
    let request = request(history(), vec![tool("read"), tool("web_search")]);
    let input = AnthropicRequest {
        request: &request,
        max_output: Some(64_000),
        thinking: AnthropicThinking::Adaptive,
        effort: Some(Effort::High),
        display_supported: true,
        temperature: None,
        compaction: None,
        summarize: false,
    };
    let wire = build(&input, AnthropicAuth::ApiKey("sk-ant")).unwrap();
    assert_eq!(
        wire.headers,
        vec![
            ("anthropic-version", "2023-06-01".to_owned()),
            ("x-api-key", "sk-ant".to_owned()),
        ]
    );
    assert_eq!(wire.user_agent, None);
    let body = String::from_utf8(wire.body).unwrap();
    let expected = concat!(
        r#"{"model":"claude-sonnet-5","max_tokens":32000,"stream":true,"#,
        r#""system":[{"type":"text","text":"Be brief.","cache_control":{"type":"ephemeral"}}],"#,
        r#""messages":[{"role":"user","content":[{"type":"text","text":"hi"}]},"#,
        r#"{"role":"assistant","content":[{ "signature" : "S", "type" : "thinking", "thinking" : "t" },"#,
        r#"{"type":"text","text":"ok"},"#,
        r#"{"type":"tool_use","id":"toolu_1","name":"read","input":{"path": "a", "n": 1.50}},"#,
        r#"{"type":"tool_use","id":"toolu_2","name":"web_search","input":{}}]},"#,
        r#"{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":[{"type":"text","text":"x"},"#,
        r#"{"type":"image","source":{"type":"base64","media_type":"image/png","data":"AQID"}}],"is_error":false},"#,
        r#"{"type":"tool_result","tool_use_id":"toolu_2","content":[{"type":"text","text":"no"}],"is_error":true,"cache_control":{"type":"ephemeral"}}]}],"#,
        r#""tools":[{"name":"read","description":"d","input_schema":{"type":"object","properties":{}}},"#,
        r#"{"name":"web_search","description":"d","input_schema":{"type":"object","properties":{}},"cache_control":{"type":"ephemeral"}}],"#,
        r#""tool_choice":{"type":"auto"},"thinking":{"type":"adaptive","display":"summarized"},"output_config":{"effort":"high"}}"#,
    );
    assert_eq!(body, expected);
    assert_eq!(body.matches("cache_control").count(), 3);
    let again = build(&input, AnthropicAuth::ApiKey("sk-ant")).unwrap();
    assert_eq!(again.body, expected.as_bytes());
}

#[test]
fn claude_oauth_mapped_name_respects_the_wire_length_limit() {
    let internal = format!("deploy.{}", "x".repeat(180));
    let request = request(vec![], vec![tool(&internal)]);
    let input = AnthropicRequest {
        request: &request,
        max_output: None,
        thinking: AnthropicThinking::Omit,
        effort: None,
        display_supported: false,
        temperature: None,
        compaction: None,
        summarize: false,
    };
    let wire = build(
        &input,
        AnthropicAuth::ClaudeOAuth {
            access_token: "sk-ant-oat",
            version: claude_fingerprint::CLAUDE_CODE_VERSION,
        },
    )
    .unwrap();
    let body = String::from_utf8(wire.body).unwrap();
    let Some(tool_name) = body
        .split(r#""tools":[{"name":""#)
        .nth(1)
        .and_then(|tail| tail.split_once('"').map(|(name, _)| name))
    else {
        panic!("the mapped tool must be present in the OAuth request");
    };
    assert_eq!(tool_name.len(), 64);
    assert!(tool_name.starts_with('_'));
    assert!(
        tool_name[1..]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    );
    assert!(!body.contains(&internal));
}

#[test]
fn claude_oauth_carries_the_reference_fingerprint() {
    let request = request(history(), vec![tool("read"), tool("web_search")]);
    let input = AnthropicRequest {
        request: &request,
        max_output: None,
        thinking: AnthropicThinking::Adaptive,
        effort: None,
        display_supported: true,
        temperature: None,
        compaction: None,
        summarize: false,
    };
    let auth = AnthropicAuth::ClaudeOAuth {
        access_token: "sk-ant-oat",
        version: claude_fingerprint::CLAUDE_CODE_VERSION,
    };
    let wire = build(&input, auth).unwrap();
    assert_eq!(
        wire.headers,
        vec![
            ("anthropic-version", "2023-06-01".to_owned()),
            ("authorization", "Bearer sk-ant-oat".to_owned()),
            (
                "anthropic-beta",
                "claude-code-20250219,oauth-2025-04-20".to_owned()
            ),
            ("x-app", "cli".to_owned()),
        ]
    );
    assert_eq!(
        wire.user_agent.as_deref(),
        Some("claude-cli/2.1.280 (external, cli)")
    );
    assert!(!format!("{wire:?}").contains("sk-ant-oat"));
    let body = String::from_utf8(wire.body).unwrap();
    assert!(body.contains(concat!(
            r#""system":[{"type":"text","text":"You are Claude Code, Anthropic's official CLI for Claude."},"#,
            r#"{"type":"text","text":"Be brief.","cache_control":{"type":"ephemeral"}}]"#
        )));
    assert!(body.contains(r#""name":"_read","input":{"path": "a", "n": 1.50}"#));
    assert!(body.contains(r#""tools":[{"name":"_read","#));
    assert!(body.contains(r#"{"name":"web_search","#));
    assert!(!body.contains("_web_search"));
}

#[test]
fn bearer_budget_compaction_omits_temperature_with_enabled_thinking() {
    let block = RawJson::parse(r#"{"type":"compaction","content":"summary"}"#).unwrap();
    let context = vec![
        ContextItem::Assistant {
            source: replay_source(Family::Anthropic, "claude-sonnet-5"),
            parts: vec![AssistantPart::Text { text: "a".into() }],
        },
        ContextItem::User {
            parts: vec![
                Part::Text { text: "q".into() },
                Part::Text {
                    text: String::new().into(),
                },
            ],
        },
    ];
    let request = request(context, vec![]);
    let input = AnthropicRequest {
        request: &request,
        max_output: Some(4096),
        thinking: AnthropicThinking::Enabled {
            budget_tokens: 3072,
            max_tokens: 4096,
        },
        effort: None,
        display_supported: false,
        temperature: None,
        compaction: Some(&block),
        summarize: false,
    };
    let wire = build(&input, AnthropicAuth::Bearer("zm-key")).unwrap();
    assert_eq!(header(&wire, "authorization"), Some("Bearer zm-key"));
    assert_eq!(header(&wire, "x-api-key"), None);
    assert_eq!(
        header(&wire, "anthropic-beta"),
        Some("interleaved-thinking-2025-05-14,compact-2026-09-04")
    );
    let body = String::from_utf8(wire.body).unwrap();
    assert_eq!(
        body,
        concat!(
            r#"{"model":"claude-sonnet-5","max_tokens":4096,"stream":true,"#,
            r#""system":[{"type":"text","text":"Be brief.","cache_control":{"type":"ephemeral"}}],"#,
            r#""messages":[{"role":"assistant","content":[{"type":"text","text":"a"}]},"#,
            r#"{"role":"user","content":[{"type":"compaction","content":"summary"},{"type":"text","text":"q","cache_control":{"type":"ephemeral"}}]}],"#,
            r#""thinking":{"type":"enabled","budget_tokens":3072,"display":"summarized"}}"#,
        )
    );
}

#[test]
fn unresolved_blob_is_refused() {
    let blob_id = dal_core::BlobId::from_bytes(&[7; 32]);
    let context = vec![ContextItem::User {
        parts: vec![Part::Blob {
            blob_id,
            mime: "image/png".into(),
            bytes: 32,
        }],
    }];
    let request = request(context, vec![]);
    let input = AnthropicRequest {
        request: &request,
        max_output: None,
        thinking: AnthropicThinking::Omit,
        effort: None,
        display_supported: false,
        temperature: None,
        compaction: None,
        summarize: false,
    };
    let Err(ProviderError::InvalidRequest { message }) = build(&input, AnthropicAuth::ApiKey("k"))
    else {
        panic!("an unresolved blob must be refused as an invalid request");
    };
    assert!(message.contains(&blob_id.to_string()));
}

fn call_item(ids: &[&str]) -> ContextItem {
    ContextItem::Assistant {
        source: replay_source(Family::Anthropic, "claude-sonnet-5"),
        parts: ids
            .iter()
            .map(|id| AssistantPart::ToolCall {
                call: CallId::new(*id),
                name: "read".into(),
                args: RawJson::parse("{}").unwrap(),
            })
            .collect(),
    }
}

fn result_item(id: &str) -> ContextItem {
    ContextItem::ToolResult {
        call: CallId::new(id),
        name: "read".into(),
        is_error: false,
        parts: vec![Part::Text {
            text: format!("out-{id}").into(),
        }],
    }
}

fn user_item(text: &str) -> ContextItem {
    ContextItem::User {
        parts: vec![Part::Text { text: text.into() }],
    }
}

fn build_context(context: Vec<ContextItem>) -> Result<AnthropicWire, ProviderError> {
    let request = request(context, vec![]);
    let input = AnthropicRequest {
        request: &request,
        max_output: None,
        thinking: AnthropicThinking::Omit,
        effort: None,
        display_supported: false,
        temperature: None,
        compaction: None,
        summarize: false,
    };
    build(&input, AnthropicAuth::ApiKey("k"))
}

fn refusal(context: Vec<ContextItem>) -> String {
    let Err(ProviderError::InvalidRequest { message }) = build_context(context) else {
        panic!("the history must be refused as an invalid request");
    };
    message
}

#[test]
fn two_results_lead_the_user_message_before_later_text() {
    let wire = build_context(vec![
        user_item("go"),
        call_item(&["toolu_a", "toolu_b"]),
        result_item("toolu_b"),
        result_item("toolu_a"),
        user_item("next"),
    ])
    .unwrap();
    let body = String::from_utf8(wire.body).unwrap();
    assert!(body.contains(concat!(
            r#"{"role":"user","content":["#,
            r#"{"type":"tool_result","tool_use_id":"toolu_b","content":[{"type":"text","text":"out-toolu_b"}],"is_error":false},"#,
            r#"{"type":"tool_result","tool_use_id":"toolu_a","content":[{"type":"text","text":"out-toolu_a"}],"is_error":false},"#,
            r#"{"type":"text","text":"next","cache_control":{"type":"ephemeral"}}]}"#,
        )));
}

#[test]
fn tool_result_without_preceding_call_is_refused() {
    let message = refusal(vec![result_item("toolu_orphan")]);
    assert!(message.contains("toolu_orphan"));
}

#[test]
fn duplicate_tool_result_is_refused() {
    let message = refusal(vec![
        call_item(&["toolu_a"]),
        result_item("toolu_a"),
        result_item("toolu_a"),
    ]);
    assert!(message.contains("toolu_a"));
    assert!(message.contains("already has a result"));
}

#[test]
fn user_content_before_pending_result_is_refused() {
    let message = refusal(vec![
        call_item(&["toolu_a"]),
        user_item("steer"),
        result_item("toolu_a"),
    ]);
    assert!(message.contains("tool call toolu_a has no result before user content"));
}

#[test]
fn adjacent_assistant_items_share_pending_calls() {
    let wire = build_context(vec![
        call_item(&["toolu_a"]),
        ContextItem::Assistant {
            source: replay_source(Family::Anthropic, "claude-sonnet-5"),
            parts: vec![AssistantPart::Text {
                text: "more".into(),
            }],
        },
        result_item("toolu_a"),
    ])
    .expect("an interleaved assistant item leaves the pending call resolvable");
    let body = String::from_utf8(wire.body).unwrap();
    assert!(
        body.contains(r#""tool_use_id":"toolu_a""#),
        "the result still resolves to toolu_a after the interleaved assistant item"
    );
}

#[test]
fn tool_call_without_result_at_end_is_refused() {
    let message = refusal(vec![
        user_item("go"),
        call_item(&["toolu_a", "toolu_b"]),
        result_item("toolu_a"),
    ]);
    assert!(message.contains("tool call toolu_b has no result before the end of the context"));
}

#[test]
fn a_repeated_message_stop_yields_one_stop_and_nothing_after() {
    let doubled =
        format!("{BASIC_TEXT_STREAM}event: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n");
    let results = decode(&doubled, false);
    let stops = results
        .iter()
        .filter(|result| matches!(result, Ok(StreamEvent::Stop { .. })))
        .count();
    assert_eq!(stops, 1);
    assert!(matches!(results.last(), Some(Ok(StreamEvent::Stop { .. }))));
}

#[test]
fn malformed_json_mid_stream_ends_in_one_protocol_error() {
    let (head, _) = BASIC_TEXT_STREAM
        .split_once("event: content_block_stop")
        .unwrap();
    let wire = format!(
        "{head}event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":\n\n{BASIC_TEXT_STREAM}"
    );
    let results = decode(&wire, false);
    let errors = results.iter().filter(|result| result.is_err()).count();
    assert_eq!(errors, 1);
    assert!(matches!(
        results.last(),
        Some(Err(ProviderError::Protocol {
            family: Family::Anthropic,
            ..
        }))
    ));
}

#[test]
fn before_turn_text_joins_its_user_message_in_the_body() {
    let user = |text: &str| ContextItem::User {
        parts: vec![Part::Text { text: text.into() }],
    };
    let body = anthropic_body(&request(
        vec![user("question"), user("first\nsecond")],
        Vec::new(),
    ));
    let question = body.find(r#""text":"question""#).expect("user text");
    let hook = body.find(r#""text":"first\nsecond""#).expect("hook text");
    assert!(question < hook, "{body}");
    assert_eq!(body.matches(r#""role":"user""#).count(), 1, "{body}");
}
