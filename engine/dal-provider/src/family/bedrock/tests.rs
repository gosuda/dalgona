use std::sync::Arc;

use dal_core::{
    CallId, ContextItem, Family, ModelRequest, ModelRoute, ModelToolSpec, Part, Purpose, RawJson,
    ReplaySource, RequestParams, ThinkingLevel,
};
use futures::{StreamExt, executor::block_on, stream};
use sha2::{Digest, Sha256};
use sonic_rs::{JsonContainerTrait, JsonValueTrait};

use super::*;
use crate::{
    error::ProviderError,
    stream::{StopReason, StreamEvent, ToolArgs},
    thinking::{AnthropicThinking, ThinkingPlan, WireThinking},
};

/// CRC-32 (IEEE) over `bytes`, the encoder side of the decoder's check.
fn crc(bytes: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(bytes);
    hasher.finalize()
}

/// One wire frame with string headers, built independently of the decoder.
fn frame(headers: &[(&str, &str)], payload: &[u8]) -> Vec<u8> {
    let mut block = Vec::new();
    for (name, value) in headers {
        block.push(u8::try_from(name.len()).expect("short header name"));
        block.extend_from_slice(name.as_bytes());
        block.push(7);
        block.extend_from_slice(
            &u16::try_from(value.len())
                .expect("short header value")
                .to_be_bytes(),
        );
        block.extend_from_slice(value.as_bytes());
    }
    let total = u32::try_from(12 + block.len() + payload.len() + 4).expect("small frame");
    let headers_len = u32::try_from(block.len()).expect("small header block");
    let mut wire = Vec::new();
    wire.extend_from_slice(&total.to_be_bytes());
    wire.extend_from_slice(&headers_len.to_be_bytes());
    wire.extend_from_slice(&crc(&wire).to_be_bytes());
    wire.extend_from_slice(&block);
    wire.extend_from_slice(payload);
    let message_crc = crc(&wire);
    wire.extend_from_slice(&message_crc.to_be_bytes());
    wire
}

fn event(kind: &str, payload: &str) -> Vec<u8> {
    frame(
        &[
            (":message-type", "event"),
            (":event-type", kind),
            (":content-type", "application/json"),
        ],
        payload.as_bytes(),
    )
}

fn exception(shape: &str, message: &str) -> Vec<u8> {
    frame(
        &[
            (":message-type", "exception"),
            (":exception-type", shape),
            (":content-type", "application/json"),
        ],
        format!(r#"{{"message":"{message}"}}"#).as_bytes(),
    )
}

/// Splits `wire` into chunks of `size` bytes, cutting through frames.
fn chunked(wire: &[u8], size: usize) -> Vec<Vec<u8>> {
    wire.chunks(size).map(<[u8]>::to_vec).collect()
}

fn run(chunks: Vec<Vec<u8>>) -> Vec<Result<StreamEvent, ProviderError>> {
    block_on(
        decode_stream(stream::iter(chunks), "bedrock", "anthropic.claude-4").collect::<Vec<_>>(),
    )
}

fn ok_events(results: Vec<Result<StreamEvent, ProviderError>>) -> Vec<StreamEvent> {
    results
        .into_iter()
        .map(|item| item.unwrap_or_else(|error| panic!("expected an event, got {error:?}")))
        .collect()
}

fn conversation() -> Vec<u8> {
    [
        event("messageStart", r#"{"role":"assistant"}"#),
        event("contentBlockDelta", r#"{"contentBlockIndex":0,"delta":{"text":"Hel"}}"#),
        event("contentBlockDelta", r#"{"contentBlockIndex":0,"delta":{"text":"lo"}}"#),
        event("contentBlockStop", r#"{"contentBlockIndex":0}"#),
        event(
            "contentBlockStart",
            r#"{"contentBlockIndex":1,"start":{"toolUse":{"toolUseId":"tooluse_1","name":"read"}}}"#,
        ),
        event(
            "contentBlockDelta",
            r#"{"contentBlockIndex":1,"delta":{"toolUse":{"input":"{\"path\":"}}}"#,
        ),
        event(
            "contentBlockDelta",
            r#"{"contentBlockIndex":1,"delta":{"toolUse":{"input":"\"a.txt\"}"}}}"#,
        ),
        event("contentBlockStop", r#"{"contentBlockIndex":1}"#),
        event("messageStop", r#"{"stopReason":"tool_use"}"#),
        event(
            "metadata",
            r#"{"usage":{"inputTokens":120,"outputTokens":30,"totalTokens":150,"cacheReadInputTokens":100,"cacheWriteInputTokens":7},"metrics":{"latencyMs":1}}"#,
        ),
    ]
    .concat()
}

#[test]
fn a_tool_use_turn_decodes_into_the_neutral_grammar_at_any_chunk_size() {
    let wire = conversation();
    for size in [1, 7, 64, wire.len()] {
        let events = ok_events(run(chunked(&wire, size)));
        let texts: String = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::TextDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, "Hello", "chunk size {size}");
        assert!(matches!(
            events.iter().find(|event| matches!(event, StreamEvent::ToolCallStarted { .. })),
            Some(StreamEvent::ToolCallStarted { id, name }) if id == "tooluse_1" && name == "read"
        ));
        let Some(StreamEvent::ToolCallsDone { calls }) = events
            .iter()
            .find(|event| matches!(event, StreamEvent::ToolCallsDone { .. }))
        else {
            panic!("a tool-use turn ends its calls");
        };
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "tooluse_1");
        assert!(
            matches!(&calls[0].args, ToolArgs::Parsed(raw) if raw.as_str() == r#"{"path":"a.txt"}"#)
        );
        assert_eq!(
            events.last(),
            Some(&StreamEvent::Stop {
                reason: StopReason::ToolUse
            })
        );
    }
}

#[test]
fn the_terminal_group_is_exactly_one_calls_usage_stop_in_order() {
    let events = ok_events(run(chunked(&conversation(), 5)));
    let tail: Vec<&str> = events
        .iter()
        .rev()
        .take(3)
        .map(|event| match event {
            StreamEvent::ToolCallsDone { .. } => "calls",
            StreamEvent::Usage { .. } => "usage",
            StreamEvent::Stop { .. } => "stop",
            _ => "other",
        })
        .collect();
    assert_eq!(tail, ["stop", "usage", "calls"]);
    for kind in ["calls", "usage", "stop"] {
        let count = events
            .iter()
            .filter(|event| {
                matches!(
                    (kind, event),
                    ("calls", StreamEvent::ToolCallsDone { .. })
                        | ("usage", StreamEvent::Usage { .. })
                        | ("stop", StreamEvent::Stop { .. })
                )
            })
            .count();
        assert_eq!(count, 1, "{kind} appears exactly once");
    }
}

#[test]
fn metadata_usage_maps_cache_reads_without_double_counting() {
    let events = ok_events(run(chunked(&conversation(), 11)));
    let usage = events
        .iter()
        .find_map(|event| match event {
            StreamEvent::Usage { usage } => Some(usage),
            _ => None,
        })
        .expect("a usage event");
    assert_eq!(usage.input_tokens, 120);
    assert_eq!(usage.cached_input_tokens, 100);
    assert_eq!(usage.output_tokens, 30);
    assert_eq!(usage.cache_write_tokens, 7);
    assert_eq!(usage.reasoning_tokens, None);
}

#[test]
fn bytes_after_the_terminal_are_never_decoded() {
    // messageStop ends the response; a metadata frame after it still
    // contributes usage (it precedes input end), but no event follows Stop.
    let events = ok_events(run(chunked(&conversation(), 13)));
    assert!(matches!(events.last(), Some(StreamEvent::Stop { .. })));
}

#[test]
fn max_tokens_truncates_every_open_call() {
    let wire = [
        event("messageStart", r#"{"role":"assistant"}"#),
        event(
            "contentBlockStart",
            r#"{"contentBlockIndex":0,"start":{"toolUse":{"toolUseId":"t","name":"edit"}}}"#,
        ),
        event(
            "contentBlockDelta",
            r#"{"contentBlockIndex":0,"delta":{"toolUse":{"input":"{\"a\":"}}}"#,
        ),
        event("messageStop", r#"{"stopReason":"max_tokens"}"#),
    ]
    .concat();
    let events = ok_events(run(vec![wire]));
    let Some(StreamEvent::ToolCallsDone { calls }) = events
        .iter()
        .find(|event| matches!(event, StreamEvent::ToolCallsDone { .. }))
    else {
        panic!("calls close at messageStop");
    };
    assert_eq!(calls[0].args, ToolArgs::Truncated);
    assert_eq!(
        events.last(),
        Some(&StreamEvent::Stop {
            reason: StopReason::MaxTokens
        })
    );
}

#[test]
fn guardrail_and_unknown_stop_reasons_map_without_losing_the_wire_word() {
    for (word, expected) in [
        ("guardrail_intervened", StopReason::Refusal),
        ("content_filtered", StopReason::Refusal),
        ("end_turn", StopReason::EndTurn),
        ("stop_sequence", StopReason::EndTurn),
        ("model_context_window_exceeded", StopReason::MaxTokens),
        (
            "brand_new_reason",
            StopReason::Other(String::from("brand_new_reason")),
        ),
    ] {
        let wire = [
            event("messageStart", r#"{"role":"assistant"}"#),
            event("messageStop", &format!(r#"{{"stopReason":"{word}"}}"#)),
        ]
        .concat();
        let events = ok_events(run(vec![wire]));
        assert_eq!(
            events.last(),
            Some(&StreamEvent::Stop { reason: expected }),
            "{word}"
        );
    }
}

#[test]
fn exception_frames_map_through_the_retry_table_by_shape() {
    type ErrorPredicate = fn(&ProviderError) -> bool;
    let cases: [(&str, ErrorPredicate); 6] = [
        ("throttlingException", |error| {
            matches!(error, ProviderError::RateLimited { .. })
        }),
        ("validationException", |error| {
            matches!(error, ProviderError::InvalidRequest { .. })
        }),
        ("internalServerException", |error| {
            matches!(
                error,
                ProviderError::Status {
                    family: Family::Bedrock,
                    status: 500,
                    ..
                }
            )
        }),
        ("serviceUnavailableException", |error| {
            matches!(
                error,
                ProviderError::Status {
                    family: Family::Bedrock,
                    status: 503,
                    ..
                }
            )
        }),
        ("modelStreamErrorException", |error| {
            matches!(
                error,
                ProviderError::Status {
                    family: Family::Bedrock,
                    status: 424,
                    ..
                }
            )
        }),
        ("accessDeniedException", |error| {
            matches!(
                error,
                ProviderError::Status {
                    family: Family::Bedrock,
                    status: 403,
                    ..
                }
            )
        }),
    ];
    for (shape, check) in cases {
        let wire = [
            event("messageStart", r#"{"role":"assistant"}"#),
            exception(shape, "boom"),
        ]
        .concat();
        let results = run(vec![wire]);
        let error = results
            .into_iter()
            .find_map(Result::err)
            .unwrap_or_else(|| panic!("{shape} ends the stream with an error"));
        assert!(check(&error), "{shape} mapped to {error:?}");
    }
}

#[test]
fn an_error_frame_carries_its_code_and_message_in_headers() {
    let wire = frame(
        &[
            (":message-type", "error"),
            (":error-code", "ThrottlingException"),
            (":error-message", "slow down"),
        ],
        b"",
    );
    let error = run(vec![wire])
        .into_iter()
        .find_map(Result::err)
        .expect("an error frame ends the stream");
    assert!(matches!(&error, ProviderError::RateLimited { message, .. } if message == "slow down"));
}

#[test]
fn an_exception_after_delivery_is_final_and_ends_the_stream_once() {
    let wire = [
        event("messageStart", r#"{"role":"assistant"}"#),
        event(
            "contentBlockDelta",
            r#"{"contentBlockIndex":0,"delta":{"text":"partial"}}"#,
        ),
        exception("throttlingException", "late"),
        event("messageStop", r#"{"stopReason":"end_turn"}"#),
    ]
    .concat();
    let results = run(chunked(&wire, 9));
    let errors = results.iter().filter(|item| item.is_err()).count();
    assert_eq!(errors, 1, "exactly one error terminal");
    assert!(
        results.last().is_some_and(Result::is_err),
        "the error is the last item; nothing is decoded after it"
    );
    assert!(
        !results
            .iter()
            .any(|item| matches!(item, Ok(StreamEvent::Stop { .. }))),
        "no Stop after an error"
    );
}

#[test]
fn input_that_ends_before_message_stop_is_a_cut() {
    let wire = [
        event("messageStart", r#"{"role":"assistant"}"#),
        event(
            "contentBlockDelta",
            r#"{"contentBlockIndex":0,"delta":{"text":"x"}}"#,
        ),
    ]
    .concat();
    let results = run(chunked(&wire, 6));
    assert!(matches!(
        results.last(),
        Some(Err(ProviderError::StreamCut))
    ));
}

#[test]
fn a_frame_cut_mid_body_is_a_bedrock_protocol_error_not_a_cut() {
    let wire = event("messageStart", r#"{"role":"assistant"}"#);
    let results = run(vec![wire[..wire.len() - 2].to_vec()]);
    assert!(matches!(
        results.last(),
        Some(Err(ProviderError::Protocol {
            family: Family::Bedrock,
            ..
        }))
    ));
}

#[test]
fn a_flipped_crc_ends_the_stream_with_a_bedrock_protocol_error() {
    let mut wire = conversation();
    wire[20] ^= 0x01;
    let results = run(chunked(&wire, 17));
    let last = results.last().expect("the stream reports its failure");
    assert!(matches!(
        last,
        Err(ProviderError::Protocol {
            family: Family::Bedrock,
            ..
        })
    ));
    assert_eq!(results.iter().filter(|item| item.is_err()).count(), 1);
}

#[test]
fn grammar_violations_are_bedrock_protocol_errors() {
    let bad_role = [event("messageStart", r#"{"role":"user"}"#)].concat();
    let bad_json = [event("contentBlockDelta", "{not json")].concat();
    let twice = [
        event(
            "contentBlockStart",
            r#"{"contentBlockIndex":0,"start":{"toolUse":{"toolUseId":"a","name":"n"}}}"#,
        ),
        event(
            "contentBlockStart",
            r#"{"contentBlockIndex":0,"start":{"toolUse":{"toolUseId":"b","name":"n"}}}"#,
        ),
    ]
    .concat();
    let nameless = [event(
        "contentBlockStart",
        r#"{"contentBlockIndex":0,"start":{"toolUse":{"toolUseId":"a"}}}"#,
    )]
    .concat();
    let no_type = frame(&[(":content-type", "application/json")], b"{}");
    let bad_type = frame(&[(":message-type", "weird")], b"{}");
    for (label, wire) in [
        ("role", bad_role),
        ("json", bad_json),
        ("double start", twice),
        ("nameless call", nameless),
        ("missing message type", no_type),
        ("unknown message type", bad_type),
    ] {
        let error = run(vec![wire])
            .into_iter()
            .find_map(Result::err)
            .unwrap_or_else(|| panic!("{label} must fail"));
        assert!(
            matches!(
                error,
                ProviderError::Protocol {
                    family: Family::Bedrock,
                    ..
                }
            ),
            "{label}: {error:?}"
        );
    }
}

#[test]
fn a_stop_without_a_start_closes_nothing() {
    // An empty text block sends `contentBlockStop` without ever sending a
    // delta; the stream continues to its terminal.
    let wire = [
        event("messageStart", r#"{"role":"assistant"}"#),
        event("contentBlockStop", r#"{"contentBlockIndex":3}"#),
        event(
            "contentBlockDelta",
            r#"{"contentBlockIndex":0,"delta":{"text":"x"}}"#,
        ),
        event("contentBlockStop", r#"{"contentBlockIndex":0}"#),
        event("messageStop", r#"{"stopReason":"end_turn"}"#),
    ]
    .concat();
    let events = ok_events(run(vec![wire]));
    assert_eq!(
        events.last(),
        Some(&StreamEvent::Stop {
            reason: StopReason::EndTurn
        })
    );
}

#[test]
fn closed_tool_calls_reach_the_terminal_in_index_order() {
    // A `contentBlockStop` closes a call's arguments; the call itself stays
    // for the terminal group, as the reference Bedrock clients do.
    let wire = [
        event("messageStart", r#"{"role":"assistant"}"#),
        event(
            "contentBlockStart",
            r#"{"contentBlockIndex":2,"start":{"toolUse":{"toolUseId":"b","name":"write"}}}"#,
        ),
        event(
            "contentBlockStart",
            r#"{"contentBlockIndex":0,"start":{"toolUse":{"toolUseId":"a","name":"read"}}}"#,
        ),
        event(
            "contentBlockDelta",
            r#"{"contentBlockIndex":0,"delta":{"toolUse":{"input":"{\"p\":1}"}}}"#,
        ),
        event("contentBlockStop", r#"{"contentBlockIndex":0}"#),
        event(
            "contentBlockDelta",
            r#"{"contentBlockIndex":2,"delta":{"toolUse":{"input":"{}"}}}"#,
        ),
        event("contentBlockStop", r#"{"contentBlockIndex":2}"#),
        event("messageStop", r#"{"stopReason":"tool_use"}"#),
    ]
    .concat();
    let events = ok_events(run(chunked(&wire, 3)));
    let Some(StreamEvent::ToolCallsDone { calls }) = events
        .iter()
        .find(|event| matches!(event, StreamEvent::ToolCallsDone { .. }))
    else {
        panic!("closed calls still reach the terminal");
    };
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].id, "a");
    assert_eq!(calls[1].id, "b");
    assert!(matches!(&calls[0].args, ToolArgs::Parsed(raw) if raw.as_str() == r#"{"p":1}"#));
}

#[test]
fn a_restart_after_stop_fails_and_late_fragments_are_ignored() {
    // A new start on an index whose call already closed is a double start.
    let restart = [
        event(
            "contentBlockStart",
            r#"{"contentBlockIndex":0,"start":{"toolUse":{"toolUseId":"a","name":"n"}}}"#,
        ),
        event("contentBlockStop", r#"{"contentBlockIndex":0}"#),
        event(
            "contentBlockStart",
            r#"{"contentBlockIndex":0,"start":{"toolUse":{"toolUseId":"b","name":"n"}}}"#,
        ),
    ]
    .concat();
    let error = run(vec![restart])
        .into_iter()
        .find_map(Result::err)
        .unwrap_or_else(|| panic!("a restart after stop must fail"));
    assert!(matches!(
        error,
        ProviderError::Protocol {
            family: Family::Bedrock,
            ..
        }
    ));

    // An argument fragment for an already-closed call is ignored, not
    // appended: only the bytes before the stop reach the terminal.
    let late = [
        event("messageStart", r#"{"role":"assistant"}"#),
        event(
            "contentBlockStart",
            r#"{"contentBlockIndex":0,"start":{"toolUse":{"toolUseId":"a","name":"n"}}}"#,
        ),
        event(
            "contentBlockDelta",
            r#"{"contentBlockIndex":0,"delta":{"toolUse":{"input":"{\"p\":1}"}}}"#,
        ),
        event("contentBlockStop", r#"{"contentBlockIndex":0}"#),
        event(
            "contentBlockDelta",
            r#"{"contentBlockIndex":0,"delta":{"toolUse":{"input":",\"q\":2}"}}}"#,
        ),
        event("messageStop", r#"{"stopReason":"tool_use"}"#),
    ]
    .concat();
    let events = ok_events(run(vec![late]));
    let Some(StreamEvent::ToolCallsDone { calls }) = events
        .iter()
        .find(|event| matches!(event, StreamEvent::ToolCallsDone { .. }))
    else {
        panic!("the call still reaches the terminal");
    };
    assert_eq!(calls.len(), 1);
    assert!(matches!(&calls[0].args, ToolArgs::Parsed(raw) if raw.as_str() == r#"{"p":1}"#));
}

#[test]
fn reasoning_without_a_start_still_reaches_stop() {
    // Signature-only and redacted reasoning blocks send deltas with no
    // contentBlockStart; the stream must not die on their stops.
    let wire = [
        event("messageStart", r#"{"role":"assistant"}"#),
        event(
            "contentBlockDelta",
            r#"{"contentBlockIndex":2,"delta":{"reasoningContent":{"text":"hmm"}}}"#,
        ),
        event("contentBlockStop", r#"{"contentBlockIndex":2}"#),
        event("messageStop", r#"{"stopReason":"end_turn"}"#),
    ]
    .concat();
    let events = ok_events(run(vec![wire]));
    assert!(events.contains(&StreamEvent::ReasoningDelta {
        text: String::from("hmm")
    }));
    assert_eq!(
        events.last(),
        Some(&StreamEvent::Stop {
            reason: StopReason::EndTurn
        })
    );
}

#[test]
fn unknown_events_and_blocks_are_ignored() {
    let wire = [
        event("messageStart", r#"{"role":"assistant"}"#),
        event("somethingNew", r#"{"x":1}"#),
        event(
            "contentBlockStart",
            r#"{"contentBlockIndex":0,"start":{"reasoningContent":{}}}"#,
        ),
        event(
            "contentBlockDelta",
            r#"{"contentBlockIndex":0,"delta":{"reasoningContent":{"text":"thinking"}}}"#,
        ),
        event("contentBlockStop", r#"{"contentBlockIndex":0}"#),
        event("messageStop", r#"{"stopReason":"end_turn"}"#),
    ]
    .concat();
    let events = ok_events(run(vec![wire]));
    assert!(events.contains(&StreamEvent::ReasoningDelta {
        text: String::from("thinking")
    }));
    assert_eq!(
        events.last(),
        Some(&StreamEvent::Stop {
            reason: StopReason::EndTurn
        })
    );
}

#[test]
fn the_decoded_stream_drops_the_byte_source_when_dropped_early() {
    struct Witness {
        chunks: std::vec::IntoIter<Vec<u8>>,
        dropped: Arc<std::sync::atomic::AtomicBool>,
    }
    impl Drop for Witness {
        fn drop(&mut self) {
            self.dropped
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    impl futures::Stream for Witness {
        type Item = Vec<u8>;
        fn poll_next(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            std::task::Poll::Ready(self.chunks.next())
        }
    }
    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let source = Witness {
        chunks: vec![conversation()].into_iter(),
        dropped: Arc::clone(&dropped),
    };
    let mut events = Box::pin(decode_stream(source, "bedrock", "m"));
    let first = block_on(events.next())
        .expect("an event")
        .expect("a TextDelta");
    assert!(matches!(first, StreamEvent::TextDelta { .. }));
    drop(events);
    assert!(
        dropped.load(std::sync::atomic::Ordering::SeqCst),
        "consumer drop cancels the transfer"
    );
}

fn raw(text: &str) -> RawJson {
    RawJson::parse(text).unwrap()
}

fn route(family: Family) -> ModelRoute {
    ModelRoute::Api {
        family,
        model: "anthropic.claude-sonnet-4:0".into(),
    }
}

fn plan(wire: WireThinking, temperature: Option<f32>) -> ThinkingPlan {
    ThinkingPlan {
        level: ThinkingLevel::High,
        wire,
        temperature,
        notices: Vec::new(),
    }
}

fn request(context: Vec<ContextItem>, tools: Vec<ModelToolSpec>) -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: route(Family::Bedrock),
        system: "You are terse.".into(),
        tools: tools.into(),
        context: context.into(),
        params: RequestParams {
            thinking: ThinkingLevel::High,
            effort: None,
            temperature: None,
            max_output_tokens: Some(2048),
        },
        cache_key: None,
    }
}

fn tool(name: &str) -> ModelToolSpec {
    ModelToolSpec {
        name: name.into(),
        description: "Read a file.".into(),
        parameters: raw(r#"{"type": "object","properties":{"path":{"type":"string"}}}"#),
        grammar: None,
        defer_loading: false,
    }
}

fn text(value: &str) -> Part {
    Part::Text { text: value.into() }
}

fn assistant_source() -> ReplaySource {
    ReplaySource {
        family: Family::Bedrock,
        model: "anthropic.claude-sonnet-4:0".into(),
    }
}

fn body_value(request: &ModelRequest, plan: &ThinkingPlan) -> sonic_rs::Value {
    let bytes = request_body(request, plan).unwrap();
    sonic_rs::from_slice(&bytes).unwrap()
}

/// The `index`-th element of the array member `key` of `value`.
fn nth<'a>(value: &'a sonic_rs::Value, key: &str, index: usize) -> Option<&'a sonic_rs::Value> {
    at(value.get(key)?, index)
}

/// The `index`-th element of `value`, which is already an array.
fn at(value: &sonic_rs::Value, index: usize) -> Option<&sonic_rs::Value> {
    value
        .as_array()
        .and_then(|array| array.as_slice().get(index))
}

/// The elements of the array member `key` of `value`, in order.
fn items<'a>(value: &'a sonic_rs::Value, key: &str) -> Vec<&'a sonic_rs::Value> {
    value.get(key).map(elems).unwrap_or_default()
}

/// The elements of `value`, which is already an array, in order.
fn elems(value: &sonic_rs::Value) -> Vec<&sonic_rs::Value> {
    value
        .as_array()
        .map(|array| array.as_slice().iter().collect())
        .unwrap_or_default()
}

fn anthropic_plan() -> ThinkingPlan {
    plan(
        WireThinking::Anthropic {
            thinking: AnthropicThinking::Enabled {
                budget_tokens: 4096,
                max_tokens: 8192,
            },
            effort: None,
        },
        Some(0.5),
    )
}

#[test]
fn the_body_carries_converse_members_in_camel_case() {
    let value = body_value(
        &request(
            vec![ContextItem::User {
                parts: vec![text("hi")],
            }],
            vec![tool("read")],
        ),
        &anthropic_plan(),
    );
    assert_eq!(
        nth(&value, "system", 0)
            .and_then(|s| s.get("text"))
            .and_then(|t| t.as_str()),
        Some("You are terse.")
    );
    assert_eq!(
        value
            .get("inferenceConfig")
            .and_then(|c| c.get("maxTokens"))
            .and_then(sonic_rs::JsonValueTrait::as_u64),
        Some(2048)
    );
    assert_eq!(
        value
            .get("additionalModelRequestFields")
            .and_then(|f| f.get("thinking"))
            .and_then(|t| t.get("budget_tokens"))
            .and_then(sonic_rs::JsonValueTrait::as_u64),
        Some(4096)
    );
    let tools = value
        .get("toolConfig")
        .and_then(|c| nth(c, "tools", 0))
        .expect("a tool");
    let spec = tools.get("toolSpec").expect("a tool spec");
    assert_eq!(spec.get("name").and_then(|n| n.as_str()), Some("read"));
    assert!(
        spec.get("inputSchema")
            .and_then(|s| s.get("json"))
            .is_some()
    );
    assert!(
        value
            .get("toolConfig")
            .and_then(|c| c.get("toolChoice"))
            .and_then(|t| t.get("auto"))
            .is_some()
    );
}

#[test]
fn tool_schema_and_arguments_travel_verbatim() {
    let bytes = request_body(
        &request(
            vec![ContextItem::Assistant {
                source: assistant_source(),
                replay: None,
                parts: vec![AssistantPart::ToolCall {
                    call: CallId::new("tooluse_1"),
                    name: "read".into(),
                    args: raw(r#"{"path": "a.txt" ,"n":1.50}"#),
                    replay: None,
                }],
            }],
            vec![tool("read")],
        ),
        &anthropic_plan(),
    )
    .unwrap();
    let body = String::from_utf8(bytes).unwrap();
    assert!(
        body.contains(r#"{"path": "a.txt" ,"n":1.50}"#),
        "arguments keep their spelling"
    );
    assert!(
        body.contains(r#"{"type": "object","properties":{"path":{"type":"string"}}}"#),
        "schema keeps its spelling"
    );
}

#[test]
fn consecutive_tool_results_fold_into_one_user_message_with_role_alternation() {
    let value = body_value(
        &request(
            vec![
                ContextItem::User {
                    parts: vec![text("go")],
                },
                ContextItem::Assistant {
                    source: assistant_source(),
                    replay: None,
                    parts: vec![
                        AssistantPart::ToolCall {
                            call: CallId::new("a"),
                            name: "read".into(),
                            args: raw("{}"),
                            replay: None,
                        },
                        AssistantPart::ToolCall {
                            call: CallId::new("b"),
                            name: "read".into(),
                            args: raw("{}"),
                            replay: None,
                        },
                    ],
                },
                ContextItem::ToolResult {
                    call: CallId::new("a"),
                    name: "read".into(),
                    is_error: false,
                    parts: vec![text("one")],
                },
                ContextItem::ToolResult {
                    call: CallId::new("b"),
                    name: "read".into(),
                    is_error: true,
                    parts: vec![],
                },
                ContextItem::User {
                    parts: vec![text("then?")],
                },
            ],
            vec![tool("read")],
        ),
        &anthropic_plan(),
    );
    let messages = items(&value, "messages");
    let roles: Vec<&str> = messages
        .iter()
        .filter_map(|message| message.get("role").and_then(|r| r.as_str()))
        .collect();
    assert_eq!(
        roles,
        ["user", "assistant", "user"],
        "roles alternate; results and the next user merge"
    );
    let content = messages
        .get(2)
        .and_then(|m| m.get("content"))
        .expect("the folded user message");
    fn status(block: &sonic_rs::Value) -> Option<&str> {
        block
            .get("toolResult")
            .and_then(|r| r.get("status"))
            .and_then(|s| s.as_str())
    }
    let blocks = elems(content);
    assert_eq!(
        blocks.len(),
        3,
        "two tool results plus the merged user text"
    );
    assert_eq!(blocks.first().copied().and_then(status), Some("success"));
    assert_eq!(blocks.get(1).copied().and_then(status), Some("error"));
    assert_eq!(
        blocks
            .get(1)
            .and_then(|b| b.get("toolResult"))
            .and_then(|r| r.get("content"))
            .and_then(|c| at(c, 0))
            .and_then(|c| c.get("text"))
            .and_then(|t| t.as_str()),
        Some("<empty>"),
        "an empty tool result sends the placeholder, not an empty block list"
    );
    assert_eq!(
        blocks
            .last()
            .and_then(|b| b.get("text"))
            .and_then(|t| t.as_str()),
        Some("then?")
    );
}

#[test]
fn blank_user_text_becomes_the_placeholder_and_empty_assistant_turns_are_dropped() {
    let value = body_value(
        &request(
            vec![
                ContextItem::User {
                    parts: vec![text("")],
                },
                ContextItem::Assistant {
                    source: assistant_source(),
                    replay: None,
                    parts: vec![AssistantPart::Thinking {
                        text: "hidden".into(),
                        replay: None,
                    }],
                },
            ],
            vec![],
        ),
        &anthropic_plan(),
    );
    assert_eq!(
        nth(&value, "messages", 0)
            .and_then(|m| m.get("content"))
            .and_then(|c| at(c, 0))
            .and_then(|b| b.get("text"))
            .and_then(|t| t.as_str()),
        Some("<empty>")
    );
    assert!(
        nth(&value, "messages", 1).is_none(),
        "a thinking-only assistant turn is omitted"
    );
    assert!(value.get("toolConfig").is_none(), "no tools, no toolConfig");
}

#[test]
fn request_errors_are_typed() {
    let ok = request(
        vec![ContextItem::User {
            parts: vec![text("hi")],
        }],
        vec![],
    );
    let mut wrong = ok.clone();
    wrong.model = route(Family::Chat);
    assert!(matches!(
        request_body(&wrong, &anthropic_plan()),
        Err(BedrockBodyError::NotBedrock { .. })
    ));
    assert!(matches!(
        request_body(&ok, &plan(WireThinking::OpenAi { effort: None }, None)),
        Err(BedrockBodyError::ForeignThinking)
    ));
    let blob = request(
        vec![ContextItem::User {
            parts: vec![Part::Blob {
                blob_id: dal_core::BlobId::from_bytes(b"blob"),
                mime: "image/png".into(),
                bytes: 4,
            }],
        }],
        vec![],
    );
    assert!(matches!(
        request_body(&blob, &anthropic_plan()),
        Err(BedrockBodyError::UnresolvedBlob { .. })
    ));
    let bmp = request(
        vec![ContextItem::User {
            parts: vec![Part::Image {
                mime: "image/bmp".into(),
                bytes: b"BM".as_slice().into(),
            }],
        }],
        vec![],
    );
    assert!(matches!(
        request_body(&bmp, &anthropic_plan()),
        Err(BedrockBodyError::ImageMime { .. })
    ));
}

#[test]
fn model_ids_encode_once_into_one_path_segment() {
    assert_eq!(
        model_id_segment("anthropic.claude-sonnet-4:0"),
        "anthropic.claude-sonnet-4%3A0"
    );
    assert_eq!(
        model_id_segment("arn:aws:bedrock:us-east-2:123:application-inference-profile/x"),
        "arn%3Aaws%3Abedrock%3Aus-east-2%3A123%3Aapplication-inference-profile%2Fx"
    );
    assert_eq!(request_path("m:0"), "model/m%3A0/converse-stream");
}

#[test]
fn an_aws_regional_host_names_its_own_region_and_other_hosts_use_the_configured_one() {
    assert_eq!(
        region_for("bedrock-runtime.eu-west-1.amazonaws.com", None),
        "eu-west-1"
    );
    assert_eq!(
        region_for("Bedrock-Runtime.EU-West-1.AmazonAWS.com", None),
        "eu-west-1"
    );
    assert_eq!(
        region_for("bedrock-runtime-fips.us-gov-west-1.amazonaws.com", None),
        "us-gov-west-1"
    );
    assert_eq!(
        region_for("bedrock-runtime.eu-central-1.api.aws", None),
        "eu-central-1"
    );
    assert_eq!(
        region_for("bedrock-runtime.cn-north-1.amazonaws.com.cn", None),
        "cn-north-1"
    );
    // AWS refuses a credential scope for another region on its own
    // endpoint, so the ambient region the caller resolved never overrides
    // the region the host names.
    assert_eq!(
        region_for(
            "bedrock-runtime.eu-west-1.amazonaws.com",
            Some("us-east-1")
        ),
        "eu-west-1"
    );
    // Hosts that are not exactly an AWS regional runtime host name no
    // region: a gateway, a VPC endpoint, an extra label, a suffix attack.
    for host in [
        "vpce.example.internal",
        "bedrock-runtime.eu-west-1.vpce.amazonaws.com",
        "bedrock-runtime.eu-west-1.amazonaws.com.evil.example",
        "bedrock-runtime..amazonaws.com",
        "bedrock-runtime.a.b.amazonaws.com",
        "bedrock-runtime.eu_west.amazonaws.com",
        "gateway.example",
    ] {
        assert_eq!(
            region_for(host, Some("ap-south-1")),
            "ap-south-1",
            "{host}"
        );
        assert_eq!(region_for(host, Some("  ap-south-1 ")), "ap-south-1", "{host}");
        assert_eq!(region_for(host, None), "us-east-1", "{host}");
        assert_eq!(region_for(host, Some("  ")), "us-east-1", "{host}");
    }
}

fn credential() -> AwsCredential {
    AwsCredential {
        access_key_id: "AKIDEXAMPLE".into(),
        secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
        session_token: Some("token-1".into()),
        expires_at: Some(1_768_478_400 + 3600),
    }
}

fn header<'a>(wire: &'a BedrockWire, name: &str) -> Option<&'a str> {
    wire.headers
        .iter()
        .find(|(header, _)| header == name)
        .map(|(_, value)| value.as_str())
}

#[test]
fn a_signed_wire_addresses_the_encoded_model_and_covers_the_body() {
    let request = request(
        vec![ContextItem::User {
            parts: vec![text("hi")],
        }],
        vec![],
    );
    let wire = build(
        &request,
        &anthropic_plan(),
        "https://bedrock-runtime.eu-west-1.amazonaws.com",
        None,
        &credential(),
        1_768_478_400,
    )
    .unwrap();
    assert_eq!(
        wire.url.as_str(),
        "https://bedrock-runtime.eu-west-1.amazonaws.com/model/anthropic.claude-sonnet-4%3A0/converse-stream"
    );
    assert_eq!(
        header(&wire, "accept"),
        Some("application/vnd.amazon.eventstream")
    );
    assert_eq!(header(&wire, "content-type"), Some("application/json"));
    assert_eq!(header(&wire, "x-amz-date"), Some("20260115T120000Z"));
    assert_eq!(header(&wire, "x-amz-security-token"), Some("token-1"));
    let mut hasher = Sha256::new();
    hasher.update(&wire.body);
    let mut digest = String::with_capacity(64);
    for byte in &hasher.finalize()[..] {
        use std::fmt::Write as _;
        let _ = write!(digest, "{byte:02x}");
    }
    assert_eq!(header(&wire, "x-amz-content-sha256"), Some(digest.as_str()));
    let authorization = header(&wire, "authorization").expect("authorization");
    assert!(authorization.starts_with(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260115/eu-west-1/bedrock/aws4_request, SignedHeaders="
    ));
    assert!(authorization.contains("x-amz-security-token"));
    assert!(
        !format!("{wire:?}").contains("wJalrXUtnFEMI"),
        "Debug never prints secrets"
    );
    assert!(!format!("{wire:?}").contains("token-1"));
}

#[test]
fn a_regional_host_signs_its_own_region_whatever_the_caller_resolved() {
    let request = request(
        vec![ContextItem::User {
            parts: vec![text("hi")],
        }],
        vec![],
    );
    let wire = build(
        &request,
        &anthropic_plan(),
        "https://bedrock-runtime.eu-west-1.amazonaws.com",
        Some("us-east-1"),
        &credential(),
        1_768_478_400,
    )
    .unwrap();
    assert!(
        header(&wire, "authorization")
            .unwrap()
            .contains("/20260115/eu-west-1/bedrock/aws4_request"),
        "{wire:?}"
    );
}

/// The authorization of the same request signed directly against `host`,
/// the value the signature must cover to match the `host` header the HTTP
/// client sends for the URL.
fn authorization_for_host(wire: &BedrockWire, host: &str, region: &str) -> String {
    sigv4::SignedRequest {
        method: "POST",
        host,
        path: wire.url.path(),
        query: wire.url.query(),
        headers: &[
            ("content-type", String::from("application/json")),
            ("accept", String::from("application/vnd.amazon.eventstream")),
        ],
        service: sigv4::SERVICE,
        region,
        credential: &credential(),
        date: AmzDate::from_unix(1_768_478_400),
    }
    .sign(&wire.body)
    .authorization
}

#[test]
fn the_signed_host_carries_the_port_the_client_sends() {
    let request = request(
        vec![ContextItem::User {
            parts: vec![text("hi")],
        }],
        vec![],
    );
    let sign = |base: &str| {
        build(
            &request,
            &anthropic_plan(),
            base,
            Some("us-west-2"),
            &credential(),
            1_768_478_400,
        )
        .unwrap()
    };
    // A non-default port is part of the `Host` header, so it is signed.
    let wire = sign("http://127.0.0.1:8123");
    let authorization = header(&wire, "authorization").unwrap();
    assert_eq!(
        authorization,
        authorization_for_host(&wire, "127.0.0.1:8123", "us-west-2")
    );
    assert_ne!(
        authorization,
        authorization_for_host(&wire, "127.0.0.1", "us-west-2")
    );
    // The scheme's default port never appears in the `Host` header.
    let wire = sign("https://gateway.example:443/runtime");
    assert_eq!(
        header(&wire, "authorization").unwrap(),
        authorization_for_host(&wire, "gateway.example", "us-west-2")
    );
    // A non-default port on the secure scheme, and an IPv6 literal host.
    let wire = sign("https://gateway.example:8443");
    assert_eq!(
        header(&wire, "authorization").unwrap(),
        authorization_for_host(&wire, "gateway.example:8443", "us-west-2")
    );
    let wire = sign("http://[::1]:9000");
    assert_eq!(
        header(&wire, "authorization").unwrap(),
        authorization_for_host(&wire, "[::1]:9000", "us-west-2")
    );
    // A fully qualified host keeps its root label: that is the `Host`
    // header the client sends, and the region still reads past it.
    let wire = sign("https://bedrock-runtime.eu-west-1.amazonaws.com.");
    let authorization = header(&wire, "authorization").unwrap();
    assert_eq!(
        authorization,
        authorization_for_host(
            &wire,
            "bedrock-runtime.eu-west-1.amazonaws.com.",
            "eu-west-1"
        )
    );
}

#[test]
fn a_credential_that_cannot_travel_as_a_header_refuses_before_signing() {
    let request = request(
        vec![ContextItem::User {
            parts: vec![text("hi")],
        }],
        vec![],
    );
    let mut hostile = credential();
    hostile.session_token = Some("token with spaces".into());
    let error = build(
        &request,
        &anthropic_plan(),
        "https://bedrock-runtime.us-east-1.amazonaws.com",
        None,
        &hostile,
        1_768_478_400,
    )
    .unwrap_err();
    let ProviderError::Status {
        family: Family::Bedrock,
        status: 401,
        message,
    } = &error
    else {
        panic!("a typed 401 refusal, got {error:?}");
    };
    assert!(message.contains("session token"), "{message}");
    assert!(!message.contains("token with spaces"), "{message}");
}

#[test]
fn a_path_that_leaves_the_model_route_is_refused_like_the_bearer_route() {
    // One route builder serves both auth modes, so a model id that would
    // normalize away its own segment never signs a different path.
    for model in ["..", ".", ""] {
        let error = build_signed(
            &request_path(model),
            b"{}".to_vec(),
            "https://bedrock-runtime.us-east-1.amazonaws.com",
            None,
            &credential(),
            1_768_478_400,
        )
        .unwrap_err();
        assert!(
            matches!(
                error,
                ProviderError::Transport {
                    family: Family::Bedrock,
                    ..
                }
            ),
            "{model:?}: {error:?}"
        );
    }
}

#[test]
fn a_gateway_prefix_survives_and_region_overrides_the_host() {
    let request = request(
        vec![ContextItem::User {
            parts: vec![text("hi")],
        }],
        vec![],
    );
    let wire = build(
        &request,
        &anthropic_plan(),
        "https://gateway.example/runtime/",
        Some("us-west-2"),
        &credential(),
        1_768_478_400,
    )
    .unwrap();
    assert_eq!(
        wire.url.as_str(),
        "https://gateway.example/runtime/model/anthropic.claude-sonnet-4%3A0/converse-stream"
    );
    assert!(
        header(&wire, "authorization")
            .unwrap()
            .contains("/us-west-2/bedrock/aws4_request")
    );
}

#[test]
fn an_expired_credential_refuses_before_any_signing() {
    let request = request(
        vec![ContextItem::User {
            parts: vec![text("hi")],
        }],
        vec![],
    );
    let error = build(
        &request,
        &anthropic_plan(),
        "https://bedrock-runtime.us-east-1.amazonaws.com",
        None,
        &credential(),
        1_768_478_400 + 3600,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ProviderError::Status {
            family: Family::Bedrock,
            status: 401,
            ..
        }
    ));
}

#[test]
fn plain_http_to_a_non_loopback_host_is_refused() {
    let request = request(
        vec![ContextItem::User {
            parts: vec![text("hi")],
        }],
        vec![],
    );
    let error = build(
        &request,
        &anthropic_plan(),
        "http://bedrock.example.com",
        None,
        &credential(),
        1_768_478_400,
    )
    .unwrap_err();
    assert!(matches!(error, ProviderError::PlainHttp { .. }));
}

#[test]
fn request_body_ignores_text_tool_and_message_replay() {
    let assistant = |replay: fn() -> Option<RawJson>| ContextItem::Assistant {
        source: assistant_source(),
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
    let body = |replay: fn() -> Option<RawJson>| {
        let built = request(vec![assistant(replay)], Vec::new());
        String::from_utf8(request_body(&built, &anthropic_plan()).unwrap()).unwrap()
    };
    let laden = body(payload);
    assert_eq!(laden, body(|| None));
    assert!(!laden.contains("REPLAY-MARKER"), "{laden}");
}
