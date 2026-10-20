//! Devin codec tests: real bytes through the real decoders.
//!
//! Every fixture is built by encoding genuine protobuf messages, framing
//! them as Connect frames, and delivering the bytes in adversarial chunk
//! splits. Nothing here mocks the decoder's own output: assertions read the
//! neutral events a production stream would carry.

use dal_core::{
    AssistantPart, CallId, ContextItem, Family, ModelRequest, ModelRoute, Part, Purpose, RawJson,
    ReplaySource, RequestParams, ThinkingLevel, Usage,
};
use futures::{StreamExt, executor::block_on, stream};
use prost::Message;

use super::connect::{
    COMPRESSED_FLAG, ConnectError, END_STREAM_FLAG, FrameDecoder, MAX_FRAME_PAYLOAD, decode_unary,
    frame_message, gunzip_capped, gzip, parse_trailer,
};
use super::decode::{ChatDecoder, decode_chunks, decode_discovery};
use super::history::{chat_prompts, deterministic_id, router_prompt};
use super::proto::{
    AssignModelRequest, ChatToolCall, ClientModelConfig, GetChatMessageRequest,
    GetChatMessageResponse, GetCliModelConfigsResponse, GetUserJwtRequest, GetUserJwtResponse,
    ModelDimension, ModelFamilyMetadata, ModelFamilyMetadataEntry, ModelFamilyMetadataValue,
    ModelFeatures, ModelInfo, ModelUsageStats, cache_control_type, chat_message_request_type,
    chat_message_source, conversational_planner_mode, display_option, model_dimension_kind,
    stop_reason,
};
use super::request::{
    DevinChatInput, assign_body, chat_frame, chat_message, discovery_body, error_detail,
    normalize_session_token, user_jwt_body, user_jwt_fallback_body,
};
use super::terminal::Terminal;
use crate::error::ProviderError;
use crate::stream::{StopReason, StreamEvent, ToolArgs};

fn raw(text: &str) -> RawJson {
    RawJson::parse(text).unwrap()
}

fn replay_source() -> ReplaySource {
    ReplaySource {
        family: Family::Devin,
        model: "devin-test".into(),
    }
}

fn devin_request(context: Vec<ContextItem>) -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::Devin,
            model: "devin-test".into(),
            provider: None,
        },
        system: "You are terse.".into(),
        tools: Vec::new().into(),
        context: context.into(),
        params: RequestParams {
            thinking: ThinkingLevel::High,
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

fn chat_input(request: &ModelRequest) -> DevinChatInput<'_> {
    DevinChatInput {
        request,
        api_key: "devin-session-token$token",
        user_jwt: "jwt",
        cascade_id: "cascade-1",
        execution_id: "exec-1",
        chat_model_uid: "devin-test",
        assignment_jwt: None,
        max_tokens: 1000,
        parallel_tool_calls: false,
        temperature: None,
        top_p: None,
        stop_sequences: &[],
    }
}

/// Frames one response message, plain or gzipped.
fn frame_response(message: &GetChatMessageResponse, flags: u8, compressed: bool) -> Vec<u8> {
    let body = message.encode_to_vec();
    let payload = if compressed {
        gzip(&body).unwrap()
    } else {
        body
    };
    let mut flags = flags;
    if compressed {
        flags |= COMPRESSED_FLAG;
    }
    frame_message(&payload, flags)
}

/// A clean end-of-stream frame.
fn end_stream_frame() -> Vec<u8> {
    frame_message(b"{}", END_STREAM_FLAG)
}

/// Drives the chunk decoder over whole frames delivered byte by byte.
#[expect(
    clippy::needless_pass_by_value,
    reason = "convenience test helper taking owned frame batch"
)]
fn run_stream(frames: Vec<Vec<u8>>) -> Vec<Result<StreamEvent, ProviderError>> {
    let bytes: Vec<u8> = frames.concat();
    let chunks: Vec<Vec<u8>> = bytes.into_iter().map(|byte| vec![byte]).collect();
    block_on(decode_chunks(stream::iter(chunks), "devin", "devin-test").collect())
}

fn ok_events(results: Vec<Result<StreamEvent, ProviderError>>) -> Vec<StreamEvent> {
    results.into_iter().map(|result| result.unwrap()).collect()
}

fn text_response(text: &str, stop: i32) -> GetChatMessageResponse {
    GetChatMessageResponse {
        message_id: String::from("msg-1"),
        delta_text: String::from(text),
        stop_reason: stop,
        usage: Some(ModelUsageStats {
            input_tokens: 100,
            output_tokens: 30,
            cache_write_tokens: 5,
            cache_read_tokens: 10,
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[test]
fn golden_delta_text_bytes_decode() {
    // field 3 (delta_text), wire type 2: tag 0x1A, length 2, "hi".
    // No stop reason and no end-of-stream frame: end of input is a cut.
    let frame = frame_message(&[0x1A, 0x02, b'h', b'i'], 0);
    let events = run_stream(vec![frame]);
    assert!(matches!(
        events.as_slice(),
        [
            Ok(StreamEvent::TextDelta { text }),
            Err(ProviderError::StreamCut),
        ] if text == "hi"
    ));
}

#[test]
fn text_turn_decodes_with_clean_end_stream() {
    let events = ok_events(run_stream(vec![
        frame_response(&text_response("Hello", stop_reason::STOP_PATTERN), 0, false),
        end_stream_frame(),
    ]));
    assert!(matches!(
        events.as_slice(),
        [
            StreamEvent::TextDelta { .. },
            StreamEvent::ToolCallsDone { calls },
            StreamEvent::MessageReplay { .. },
            StreamEvent::Usage { .. },
            StreamEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ] if calls.is_empty()
    ));
    let [_, _, _, StreamEvent::Usage { usage }, _] = events.as_slice() else {
        panic!("usage shape changed");
    };
    assert_eq!(
        *usage,
        Usage {
            input_tokens: 115,
            cached_input_tokens: 10,
            output_tokens: 30,
            reasoning_tokens: None,
            cache_write_tokens: 5,
            cost_usd: None,
            premium_requests: None,
        }
    );
}

#[test]
fn sparse_usage_snapshot_never_regresses_earlier_totals() {
    // Usage messages are cumulative snapshots: the second message grows the
    // input total while omitting every other counter (proto3 reads the absent
    // fields as zero). The fold keeps the per-counter maximum — latest total
    // without double-counting — instead of overwriting the earlier totals
    // with the sparse zeros.
    let usage =
        |input: u64, output: u64, cache_write: u64, cache_read: u64| GetChatMessageResponse {
            usage: Some(ModelUsageStats {
                input_tokens: input,
                output_tokens: output,
                cache_write_tokens: cache_write,
                cache_read_tokens: cache_read,
                ..Default::default()
            }),
            ..Default::default()
        };
    // Input and cache totals as in `text_response` with output 10, then a
    // cumulative input update that omits every other counter.
    let mut growing = usage(150, 0, 0, 0);
    growing.stop_reason = stop_reason::STOP_PATTERN;
    let events = ok_events(run_stream(vec![
        frame_response(&usage(100, 10, 5, 10), 0, false),
        frame_response(&growing, 0, false),
        end_stream_frame(),
    ]));
    let usage = events.iter().find_map(|event| match event {
        StreamEvent::Usage { usage } => Some(usage),
        _ => None,
    });
    let usage = usage.expect("terminal usage");
    assert_eq!(usage.input_tokens, 165);
    assert_eq!(usage.cached_input_tokens, 10);
    assert_eq!(usage.output_tokens, 10);
    assert_eq!(usage.cache_write_tokens, 5);
}

#[test]
fn eof_with_stop_reason_completes_without_end_stream() {
    // The reference tolerates a missing end-of-stream frame; dal completes
    // the turn because a non-zero stop reason arrived.
    let events = ok_events(run_stream(vec![frame_response(
        &text_response("done", stop_reason::STOP_PATTERN),
        0,
        false,
    )]));
    assert!(matches!(
        events.last(),
        Some(StreamEvent::Stop {
            reason: StopReason::EndTurn,
        })
    ));
}

#[test]
fn thinking_signature_replays_bound_to_devin() {
    let message = GetChatMessageResponse {
        message_id: String::from("msg-2"),
        delta_thinking: String::from("let me think"),
        delta_signature: String::from("sig-abc"),
        stop_reason: stop_reason::STOP_PATTERN,
        ..Default::default()
    };
    let events = ok_events(run_stream(vec![
        frame_response(&message, 0, true),
        end_stream_frame(),
    ]));
    let replay = events.iter().find_map(|event| match event {
        StreamEvent::Replay { payload } => Some(payload),
        _ => None,
    });
    let payload = replay.expect("thinking signature replays");
    assert_eq!(payload.family, Family::Devin);
    assert_eq!(payload.model.as_ref(), "devin-test");
    assert!(payload.item.as_str().contains("sig-abc"));
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::ReasoningDelta { text } if text == "let me think"
    )));
}

#[test]
fn tool_assembly_supports_cumulative_and_incremental_args() {
    let first = GetChatMessageResponse {
        delta_tool_calls: vec![ChatToolCall {
            id: String::from("call-1"),
            name: String::from("grep"),
            arguments_json: String::from(r#"{"pattern":"a"#),
        }],
        ..Default::default()
    };
    // Same call continues without an id and resends the cumulative prefix.
    let second = GetChatMessageResponse {
        delta_tool_calls: vec![ChatToolCall {
            id: String::new(),
            name: String::new(),
            arguments_json: String::from(r#"{"pattern":"ab"}"#),
        }],
        stop_reason: stop_reason::FUNCTION_CALL,
        ..Default::default()
    };
    let events = ok_events(run_stream(vec![
        frame_response(&first, 0, false),
        frame_response(&second, 0, false),
        end_stream_frame(),
    ]));
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::ToolCallStarted { id, name } if id == "call-1" && name == "grep"
    )));
    let done = events.iter().find_map(|event| match event {
        StreamEvent::ToolCallsDone { calls } => Some(calls),
        _ => None,
    });
    let calls = done.expect("terminal call group");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "call-1");
    assert!(
        matches!(&calls[0].args, ToolArgs::Parsed(args) if args.as_str() == r#"{"pattern":"ab"}"#)
    );
    assert!(matches!(
        events.last(),
        Some(StreamEvent::Stop {
            reason: StopReason::ToolUse,
        })
    ));
    // Tool-call turns carry no message replay: there is no closing entry.
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, StreamEvent::MessageReplay { .. }))
    );
}

#[test]
fn max_tokens_truncates_calls_and_wins() {
    let message = GetChatMessageResponse {
        delta_tool_calls: vec![ChatToolCall {
            id: String::from("call-9"),
            name: String::from("read"),
            arguments_json: String::from(r#"{"file":"a"#),
        }],
        stop_reason: stop_reason::MAX_TOKENS,
        ..Default::default()
    };
    let events = ok_events(run_stream(vec![
        frame_response(&message, 0, false),
        end_stream_frame(),
    ]));
    let done = events.iter().find_map(|event| match event {
        StreamEvent::ToolCallsDone { calls } => Some(calls),
        _ => None,
    });
    let calls = done.expect("terminal call group");
    assert!(matches!(calls[0].args, ToolArgs::Truncated));
    assert!(matches!(
        events.last(),
        Some(StreamEvent::Stop {
            reason: StopReason::MaxTokens,
        })
    ));
}

#[test]
fn stop_map_covers_filter_and_unknown() {
    for (wire, expected) in [
        (stop_reason::CONTENT_FILTER, StopReason::Refusal),
        (
            stop_reason::ERROR,
            StopReason::Other(String::from("STOP_REASON_ERROR")),
        ),
        (
            stop_reason::PARTIAL,
            StopReason::Other(String::from("STOP_REASON_PARTIAL")),
        ),
        (99, StopReason::Other(String::from("STOP_REASON_UNKNOWN"))),
    ] {
        let events = ok_events(run_stream(vec![
            frame_response(&text_response("x", wire), 0, false),
            end_stream_frame(),
        ]));
        assert!(
            matches!(events.last(), Some(StreamEvent::Stop { reason }) if *reason == expected),
            "wire stop {wire}"
        );
    }
}

#[test]
fn invalid_argument_trailer_is_status_not_overflow() {
    let trailer = br#"{"error":{"code":"invalid_argument","message":"internal error"}}"#;
    let events = run_stream(vec![frame_message(trailer, END_STREAM_FLAG)]);
    assert!(matches!(
        events.as_slice(),
        [Err(ProviderError::Status { status: 200, .. })]
    ));
}

#[test]
fn message_only_trailer_is_status_not_transport() {
    let trailer = br#"{"error":{"message":"boom"}}"#;
    let events = run_stream(vec![frame_message(trailer, END_STREAM_FLAG)]);
    assert!(matches!(
        events.as_slice(),
        [Err(ProviderError::Status { status: 200, .. })]
    ));
}

#[test]
fn trailer_details_format_names_code_and_evidence() {
    let trailer = br#"{"error":{"code":"unavailable","message":"Error","details":[{"type":"t","value":"v"}]}}"#;
    let events = run_stream(vec![frame_message(trailer, END_STREAM_FLAG)]);
    let [Err(ProviderError::Status { message, .. })] = events.as_slice() else {
        panic!("trailer shape changed");
    };
    assert!(message.contains("unavailable"), "{message}");
    assert!(message.contains("t: v"), "{message}");
}

#[test]
fn torn_frame_at_eof_is_a_cut() {
    let frame = frame_response(&text_response("part", stop_reason::STOP_PATTERN), 0, false);
    let torn = frame[..frame.len().saturating_sub(3)].to_vec();
    let events = run_stream(vec![torn]);
    assert!(matches!(events.last(), Some(Err(ProviderError::StreamCut))));
}

#[test]
fn undecodable_message_is_a_protocol_error() {
    // Tag 5, wire type 2 (length-delimited) on a varint-only field: invalid.
    let frame = frame_message(&[0x2A, 0x01, 0xFF], 0);
    let events = run_stream(vec![frame]);
    assert!(matches!(
        events.as_slice(),
        [Err(ProviderError::Protocol { .. })]
    ));
}

#[test]
fn oversize_frame_is_a_protocol_error() {
    let mut frame = vec![0x00, 0xFF, 0xFF, 0xFF, 0xFF];
    frame.extend_from_slice(b"junk");
    let events = run_stream(vec![frame]);
    assert!(matches!(
        events.as_slice(),
        [Err(ProviderError::Protocol { .. })]
    ));
}

#[test]
fn post_terminal_frames_are_ignored() {
    let mut decoder = ChatDecoder::new("devin", "devin-test");
    let mut out = Vec::new();
    let done = decoder
        .feed_frame(END_STREAM_FLAG, b"{}", &mut out)
        .unwrap();
    assert!(done);
    let terminal_count = out
        .iter()
        .filter(|event| matches!(event, StreamEvent::Stop { .. }))
        .count();
    assert_eq!(terminal_count, 1);
    let mut late = Vec::new();
    let done = decoder
        .feed_frame(
            0,
            &text_response("late", stop_reason::STOP_PATTERN).encode_to_vec(),
            &mut late,
        )
        .unwrap();
    assert!(done);
    assert_eq!(late, []);
    let mut end = Vec::new();
    decoder.finish_eof(&mut end).unwrap();
    assert_eq!(end, []);
}

#[test]
fn terminal_latch_tracks_delivery() {
    let mut terminal = Terminal::new();
    assert!(!terminal.is_finished());
    assert!(!terminal.delivered());
    // An empty batch delivers nothing, a real batch does, and a later empty
    // batch must not erase the delivery the retry table keys on.
    terminal.note_events(0);
    assert!(!terminal.delivered());
    terminal.note_events(2);
    assert!(terminal.delivered());
    terminal.note_events(0);
    assert!(
        terminal.delivered(),
        "a later empty batch erased an earlier delivery"
    );
    // Delivery does not close the latch, and closing it keeps the delivery.
    assert!(!terminal.is_finished());
    terminal.finish_ok();
    assert!(terminal.is_finished());
    assert!(terminal.delivered());
    // The failure path closes the latch as well, with nothing delivered.
    let mut failed = Terminal::new();
    failed.finish_err();
    assert!(failed.is_finished());
    assert!(!failed.delivered());
}

#[test]
fn frame_decoder_reassembles_split_chunks() {
    let message = text_response("split", stop_reason::STOP_PATTERN);
    let frame = frame_response(&message, 0, false);
    let mut decoder = FrameDecoder::new();
    for byte in &frame {
        decoder.push(std::slice::from_ref(byte)).unwrap();
    }
    assert!(!decoder.has_partial());
    let frames = decoder.drain();
    assert_eq!(frames.len(), 1);
    let response = GetChatMessageResponse::decode(frames[0].payload.as_slice()).unwrap();
    assert_eq!(response.delta_text, "split");
}

#[test]
fn frame_decoder_yields_preceding_frames_before_oversize() {
    let good = frame_response(&text_response("ok", 0), 0, false);
    let mut wire = good.clone();
    wire.extend_from_slice(&[0x00, 0x02, 0x00, 0x00, 0x00]);
    let mut decoder = FrameDecoder::new();
    let result = decoder.push(&wire);
    assert!(result.is_err());
    assert_eq!(decoder.drain().len(), 1);
}

#[test]
fn gunzip_roundtrip_and_cap() {
    let gz = gzip(b"hello devin").unwrap();
    assert_eq!(gunzip_capped(&gz).unwrap(), b"hello devin");
    let trailer = parse_trailer(b"").or_else(|| parse_trailer(b"{}"));
    assert!(trailer.is_none());
    let error = parse_trailer(br#"{"error":{"code":"c","message":"m"}}"#).expect("error trailer");
    assert_eq!(error.code, "c");
    assert!(error.formatted.contains("Devin stream error c: m"));
    assert!(parse_trailer(b"not json").is_none());
    assert!(parse_trailer(br#"{"error":{"code":"","message":""}}"#).is_none());
    assert!(parse_trailer(br#"{"error":null}"#).is_none());
}

#[test]
fn frame_cap_boundary_rejects_max_plus_one_and_holds_max() {
    // A length prefix one past the 16 MiB cap fails before a payload byte.
    let mut over = vec![0x00];
    let over_len = u32::try_from(MAX_FRAME_PAYLOAD).expect("16 MiB cap fits u32") + 1;
    over.extend_from_slice(&over_len.to_be_bytes());
    let mut decoder = FrameDecoder::new();
    assert!(matches!(
        decoder.push(&over),
        Err(ConnectError::FrameTooLarge { .. })
    ));
    // Exactly at the cap the header is accepted and the payload stays pending.
    let mut exact = vec![0x00];
    let exact_len = u32::try_from(MAX_FRAME_PAYLOAD).expect("16 MiB cap fits u32");
    exact.extend_from_slice(&exact_len.to_be_bytes());
    let mut decoder = FrameDecoder::new();
    decoder.push(&exact).unwrap();
    assert!(decoder.has_partial());
    assert_eq!(decoder.drain().len(), 0);
}

#[test]
fn gunzip_cap_rejects_decompression_bomb() {
    // 16 MiB + 1 of zeros compresses tiny but must fail the capped inflate.
    let big = vec![0u8; MAX_FRAME_PAYLOAD + 1];
    let gz = gzip(&big).unwrap();
    assert!(gz.len() < MAX_FRAME_PAYLOAD);
    assert!(matches!(
        gunzip_capped(&gz),
        Err(ConnectError::InflateTooLarge)
    ));
}

#[test]
fn gunzip_cap_accepts_exact_max_and_rejects_tampered_checksum() {
    // Exactly 16 MiB inflates clean; one flipped body byte breaks the stream.
    let exact = vec![7u8; MAX_FRAME_PAYLOAD];
    let gz = gzip(&exact).unwrap();
    let out = gunzip_capped(&gz).unwrap();
    assert_eq!(out, exact);
    let mut tampered = gz.clone();
    let mid = tampered.len() / 2;
    tampered[mid] ^= 0xFF;
    assert!(gunzip_capped(&tampered).is_err());
}

#[test]
fn unary_decode_accepts_raw_and_gzipped() {
    let response = GetUserJwtResponse {
        user_jwt: String::from("jwt"),
        custom_api_server_url: String::from("https://edge.example.com/"),
    };
    let raw = response.encode_to_vec();
    let decoded: GetUserJwtResponse = decode_unary(&raw, "GetUserJwtResponse").unwrap();
    assert_eq!(decoded.user_jwt, "jwt");
    let gzipped = gzip(&raw).unwrap();
    let decoded: GetUserJwtResponse = decode_unary(&gzipped, "GetUserJwtResponse").unwrap();
    assert_eq!(decoded.custom_api_server_url, "https://edge.example.com/");
    let Err(error) = decode_unary::<GetUserJwtResponse>(b"\xFF\xFF", "GetUserJwtResponse") else {
        panic!("garbage must not decode");
    };
    assert!(error.to_string().contains("GetUserJwtResponse"));
}

#[test]
fn history_maps_native_and_foreign_turns() {
    let request = devin_request(vec![
        user("start"),
        ContextItem::Assistant {
            source: ReplaySource {
                family: Family::Chat,
                model: "other".into(),
            },
            parts: vec![
                AssistantPart::Thinking {
                    text: "foreign reasoning".into(),
                    replay: None,
                },
                AssistantPart::Text {
                    text: "foreign answer".into(),
                    replay: None,
                },
            ],
            replay: None,
        },
        user("go"),
        ContextItem::Assistant {
            source: replay_source(),
            parts: vec![AssistantPart::Thinking {
                text: "native reasoning".into(),
                replay: Some(raw(r#"{"signature":"sig-1"}"#)),
            }],
            replay: Some(raw(r#"{"message_id":"resp-native"}"#)),
        },
        user("next"),
    ]);
    let prompts = chat_prompts(&request, "cascade-9").unwrap();
    assert_eq!(prompts.len(), 5);
    let foreign = &prompts[1];
    assert_eq!(foreign.source, chat_message_source::SYSTEM);
    assert!(foreign.prompt.contains("foreign reasoning"));
    assert!(foreign.prompt.contains("foreign answer"));
    assert_eq!(foreign.thinking, "");
    assert_eq!(foreign.signature, "");
    let native = &prompts[3];
    assert_eq!(native.message_id, "resp-native");
    assert_eq!(native.thinking, "native reasoning");
    assert_eq!(native.signature, "sig-1");
    // UUID-shaped message ids.
    assert_eq!(prompts[0].message_id.len(), 36);
    // Deterministic ids: a second build is byte-identical.
    let again = chat_prompts(&request, "cascade-9").unwrap();
    assert_eq!(
        GetChatMessageRequest {
            chat_message_prompts: prompts,
            ..Default::default()
        }
        .encode_to_vec(),
        GetChatMessageRequest {
            chat_message_prompts: again,
            ..Default::default()
        }
        .encode_to_vec()
    );
}

#[test]
fn history_sanitizes_call_ids_and_pairs_results() {
    let long = format!("call_{}", "a".repeat(60));
    let request = devin_request(vec![
        ContextItem::Assistant {
            source: ReplaySource {
                family: Family::Responses,
                model: "other".into(),
            },
            parts: vec![
                AssistantPart::ToolCall {
                    call: CallId::new(format!("{long}|item-1")),
                    name: "grep".into(),
                    args: raw(r#"{"q":"a"}"#),
                    replay: None,
                },
                AssistantPart::ToolCall {
                    call: CallId::new(format!("{long}|item-2")),
                    name: "grep".into(),
                    args: raw(r#"{"q":"b"}"#),
                    replay: None,
                },
            ],
            replay: None,
        },
        ContextItem::ToolResult {
            call: CallId::new(format!("{long}|item-1")),
            name: "grep".into(),
            is_error: false,
            parts: vec![Part::Text { text: "hit".into() }],
        },
        ContextItem::ToolResult {
            call: CallId::new(format!("{long}|item-2")),
            name: "grep".into(),
            is_error: true,
            parts: vec![Part::Text {
                text: "miss".into(),
            }],
        },
    ]);
    let prompts = chat_prompts(&request, "cascade-7").unwrap();
    assert_eq!(prompts.len(), 3);
    let calls = &prompts[0].tool_calls;
    assert_eq!(calls.len(), 2);
    // Pipe halves shed, capped at 40 chars, deduplicated.
    assert!(calls[0].id.len() <= 40);
    assert!(calls[1].id.len() <= 40);
    assert_ne!(calls[0].id, calls[1].id);
    assert!(!calls[0].id.contains('|'));
    // Results resolve through the same table, in order.
    assert_eq!(prompts[1].tool_call_id, calls[0].id);
    assert_eq!(prompts[2].tool_call_id, calls[1].id);
    assert!(prompts[2].tool_result_is_error);
}

#[test]
fn history_skips_empty_turns_and_bad_calls() {
    let request = devin_request(vec![
        user("hi"),
        ContextItem::Assistant {
            source: replay_source(),
            parts: vec![],
            replay: None,
        },
        ContextItem::Assistant {
            source: replay_source(),
            parts: vec![AssistantPart::ToolCall {
                call: CallId::new(""),
                name: "".into(),
                args: raw("{}"),
                replay: None,
            }],
            replay: None,
        },
    ]);
    let prompts = chat_prompts(&request, "cascade-3").unwrap();
    assert_eq!(prompts.len(), 1);
}

#[test]
fn history_aborts_on_empty_tool_result_id() {
    // The matching call is skipped, but its result must not vanish with it:
    // an empty id is corrupt input the server could not route, so the whole
    // lowering fails loudly instead of dropping tool output.
    let request = devin_request(vec![
        user("hi"),
        ContextItem::Assistant {
            source: replay_source(),
            parts: vec![AssistantPart::ToolCall {
                call: CallId::new(""),
                name: String::new().into(),
                args: raw("{}"),
                replay: None,
            }],
            replay: None,
        },
        ContextItem::ToolResult {
            call: CallId::new(""),
            name: String::new().into(),
            is_error: false,
            parts: vec![Part::Text { text: "hit".into() }],
        },
    ]);
    assert!(matches!(
        chat_prompts(&request, "cascade-3"),
        Err(ProviderError::InvalidRequest { .. })
    ));
}

#[test]
fn history_rejects_unresolved_blobs() {
    let request = devin_request(vec![ContextItem::User {
        parts: vec![Part::Blob {
            blob_id: dal_core::BlobId::from_bytes(b"blob"),
            mime: "image/png".into(),
            bytes: 4,
        }],
    }]);
    assert!(matches!(
        chat_prompts(&request, "cascade-1"),
        Err(ProviderError::UnresolvedBlob { .. })
    ));
}

#[test]
fn history_carries_images_as_base64() {
    let request = devin_request(vec![ContextItem::User {
        parts: vec![Part::Image {
            mime: "image/png".into(),
            bytes: b"\x89PNG".as_slice().into(),
        }],
    }]);
    let prompts = chat_prompts(&request, "cascade-1").unwrap();
    assert_eq!(prompts[0].images.len(), 1);
    assert_eq!(prompts[0].images[0].mime_type, "image/png");
    assert_ne!(prompts[0].images[0].base64_data, "");
}

#[test]
fn router_prompt_scores_the_latest_user_turn() {
    let request = devin_request(vec![user("older"), user("route me")]);
    let prompt = router_prompt(&request)
        .expect("router prompt builds")
        .expect("router prompt");
    assert_eq!(prompt.source, chat_message_source::USER);
    assert_eq!(prompt.prompt, "route me");
    assert_eq!(prompt.message_id, "");
}

#[test]
fn router_prompt_propagates_blob_instead_of_scoring_stale_turn() {
    // The latest user turn carries an unresolved blob while an older turn
    // renders fine: falling back to the older turn would bill an assignment
    // against history the chat request itself rejects.
    let request = devin_request(vec![
        user("older"),
        ContextItem::User {
            parts: vec![Part::Blob {
                blob_id: dal_core::BlobId::from_bytes(b"blob"),
                mime: "image/png".into(),
                bytes: 4,
            }],
        },
    ]);
    assert!(matches!(
        router_prompt(&request),
        Err(ProviderError::UnresolvedBlob { .. })
    ));
    assert!(
        router_prompt(&devin_request(vec![]))
            .expect("no user turn")
            .is_none()
    );
}

#[test]
fn deterministic_ids_are_stable_and_shaped() {
    let first = deterministic_id("cascade-1\0user");
    assert_eq!(first, deterministic_id("cascade-1\0user"));
    assert_ne!(first, deterministic_id("cascade-2\0user"));
    assert_eq!(first.len(), 36);
    assert_eq!(&first[8..9], "-");
}

#[test]
fn chat_body_carries_the_pinned_shape() {
    let request = devin_request(vec![user("hello")]);
    let input = DevinChatInput {
        assignment_jwt: Some("assign-jwt"),
        chat_model_uid: "claude-sonnet-4-5",
        ..chat_input(&request)
    };
    let message = chat_message(&input).unwrap();
    assert_eq!(message.chat_model_uid, "claude-sonnet-4-5");
    assert_eq!(message.model_assignment_jwt.as_deref(), Some("assign-jwt"));
    assert_eq!(message.request_type, chat_message_request_type::CASCADE);
    assert_eq!(message.planner_mode, conversational_planner_mode::DEFAULT);
    assert_eq!(message.cascade_id, "cascade-1");
    assert_eq!(message.execution_id, "exec-1");
    assert_eq!(
        message
            .tool_choice
            .as_ref()
            .and_then(|choice| choice.option_name.as_deref()),
        Some("auto")
    );
    assert_eq!(
        message
            .system_prompt_cache_options
            .as_ref()
            .map(|options| options.r#type),
        Some(cache_control_type::EPHEMERAL)
    );
    assert!(message.disable_parallel_tool_calls);
    let configuration = message.configuration.as_ref().expect("configuration");
    assert_eq!(configuration.num_completions, 1);
    assert_eq!(configuration.max_tokens, 1000);
    assert_eq!(configuration.max_newlines, 400);
    assert_eq!(configuration.temperature, 1.0);
    assert_eq!(configuration.top_k, 40);
    assert!((configuration.top_p - f64::from(0.95_f32)).abs() < f64::EPSILON);
    assert_eq!(configuration.stop_patterns, [] as [String; 0]);
    assert_eq!(configuration.fim_eot_prob_threshold, 1.0);
    assert_eq!(
        message.metadata.as_ref().expect("metadata").ide_type,
        "chisel"
    );
    assert_eq!(message.metadata.as_ref().expect("metadata").user_jwt, "jwt");
    assert_eq!(message.chat_message_prompts.len(), 1);
    // The frame round-trips through gzip and the Connect envelope.
    let frame = chat_frame(&input).unwrap();
    assert_eq!(frame[0], COMPRESSED_FLAG);
    let length = u32::from_be_bytes([frame[1], frame[2], frame[3], frame[4]]) as usize;
    assert_eq!(frame.len(), 5 + length);
}

#[test]
fn chat_body_rejects_foreign_routes() {
    let mut request = devin_request(vec![user("hi")]);
    request.model = ModelRoute::Api {
        family: Family::Chat,
        model: "other".into(),
        provider: None,
    };
    let input = chat_input(&request);
    assert!(matches!(
        chat_message(&input),
        Err(ProviderError::InvalidRequest { .. })
    ));
}

#[test]
fn gemini_routes_scalarize_type_arrays() {
    let params =
        raw(r#"{"type":"object","properties":{"skip":{"type":["number","null"]}},"required":[]}"#);
    let mut request = devin_request(vec![user("hi")]);
    request.tools = vec![dal_core::ModelToolSpec {
        name: "grep".into(),
        description: "Search files".into(),
        parameters: params,
        grammar: None,
        defer_loading: false,
    }]
    .into();
    let input = DevinChatInput {
        chat_model_uid: "MODEL_GOOGLE_GEMINI_3_FLASH",
        ..chat_input(&request)
    };
    let message = chat_message(&input).unwrap();
    let schema = message.tools[0].json_schema_string.clone();
    assert!(schema.contains(r#""type":"number""#), "{schema}");
    assert!(schema.contains(r#""nullable":true"#), "{schema}");
    // Non-Gemini routes keep the schema text byte-exact.
    let input = chat_input(&request);
    let message = chat_message(&input).unwrap();
    assert!(
        message.tools[0]
            .json_schema_string
            .contains(r#""type":["number","null"]"#)
    );
}

#[test]
fn bare_gemini_mention_is_not_a_gemini_route() {
    // Only the server's enum namespace prefix selects the Gemini schema
    // rewrite; a uid that merely mentions the word rides the byte-exact
    // path like every other non-Gemini route.
    let params =
        raw(r#"{"type":"object","properties":{"skip":{"type":["number","null"]}},"required":[]}"#);
    let mut request = devin_request(vec![user("hi")]);
    request.tools = vec![dal_core::ModelToolSpec {
        name: "grep".into(),
        description: "Search files".into(),
        parameters: params,
        grammar: None,
        defer_loading: false,
    }]
    .into();
    let input = DevinChatInput {
        chat_model_uid: "custom-gemini-flash",
        ..chat_input(&request)
    };
    let message = chat_message(&input).unwrap();
    let schema = message.tools[0].json_schema_string.as_str();
    assert!(schema.contains(r#""type":["number","null"]"#), "{schema}");
}

#[test]
fn auth_and_discovery_bodies_carry_identity() {
    assert_eq!(
        normalize_session_token("token"),
        "devin-session-token$token"
    );
    assert_eq!(
        normalize_session_token("devin-session-token$token"),
        "devin-session-token$token"
    );
    let auth = GetUserJwtRequest::decode(user_jwt_body("token").as_slice()).unwrap();
    let metadata = auth.metadata.expect("metadata");
    assert_eq!(metadata.api_key, "devin-session-token$token");
    assert_eq!(metadata.ide_name, "devin-cli");
    assert_eq!(metadata.ide_type, "chisel");
    assert_eq!(metadata.ide_version, "3000.11.3");
    assert_eq!(metadata.user_jwt, "");
    let fallback = GetUserJwtRequest::decode(user_jwt_fallback_body("token").as_slice()).unwrap();
    assert_eq!(fallback.metadata.expect("metadata").api_key, "token");
    let assign = AssignModelRequest::decode(
        assign_body("wire-key", "adaptive", "cascade-1", None).as_slice(),
    )
    .unwrap();
    assert_eq!(assign.model_router_uid, "adaptive");
    assert_eq!(assign.cascade_id, "cascade-1");
    assert!(assign.chat_message_prompt.is_none());
    let discovery =
        super::proto::GetCliModelConfigsRequest::decode(discovery_body("token").as_slice())
            .unwrap();
    assert_eq!(
        discovery
            .metadata
            .expect("metadata")
            .supported_model_displays,
        vec![3, 4, 6, 7, 8]
    );
}

#[test]
fn error_detail_extracts_envelopes_and_rejects_html() {
    let body = br#"{"error":{"message":"boom"}}"#;
    assert_eq!(
        error_detail(body, Some("application/json")).as_deref(),
        Some("boom")
    );
    assert_eq!(error_detail(b"<html>proxy</html>", None), None);
    assert_eq!(error_detail(b"<html>proxy</html>", Some("text/html")), None);
    assert_eq!(error_detail(b"\xFF\xFE binary", None), None);
    assert_eq!(error_detail(b"   ", None), None);
}

#[test]
fn error_detail_cap_is_the_pinned_4096_units_without_a_marker() {
    // At the cap the text survives whole; past it the cut adds nothing.
    let at_cap = "x".repeat(4096);
    assert_eq!(
        error_detail(at_cap.as_bytes(), None).as_deref(),
        Some(at_cap.as_str())
    );
    let over = "x".repeat(5000);
    assert_eq!(
        error_detail(over.as_bytes(), None).as_deref(),
        Some(at_cap.as_str())
    );
    // The cap counts UTF-16 units like the pinned `String.length`: an astral
    // scalar costs two, and a cut never splits it.
    let astral = format!("{}😀😀", "x".repeat(4095));
    assert_eq!(
        error_detail(astral.as_bytes(), None).as_deref(),
        Some("x".repeat(4095).as_str())
    );
    let fits = format!("{}😀", "x".repeat(4094));
    assert_eq!(
        error_detail(fits.as_bytes(), None).as_deref(),
        Some(fits.as_str())
    );
    // An unwrapped envelope message takes the same cap.
    let envelope = format!(r#"{{"message":"{}"}}"#, "y".repeat(5000));
    assert_eq!(
        error_detail(envelope.as_bytes(), Some("application/json")).as_deref(),
        Some("y".repeat(4096).as_str())
    );
}

#[test]
fn discovery_cost_normalizes_each_denominator_to_a_million_tokens() {
    let dimension = |label: &str, value: f32, denominator: &str, kind: i32| ModelDimension {
        label: String::from(label),
        value,
        denominator: String::from(denominator),
        kind,
    };
    let response = GetCliModelConfigsResponse {
        client_model_configs: vec![
            ClientModelConfig {
                model_uid: String::from("per-million"),
                model_dimensions: vec![
                    dimension("Input", 3.0, "1M tokens", model_dimension_kind::COST),
                    dimension("Cached input", 0.3, "1M tokens", model_dimension_kind::COST),
                    dimension(
                        "Output",
                        15.0,
                        "1M tokens",
                        model_dimension_kind::COST_FUZZY,
                    ),
                    dimension("Sidekick", 0.0, "1M tokens", model_dimension_kind::COST),
                    dimension("Input", 99.0, "1M tokens", model_dimension_kind::COST),
                ],
                ..Default::default()
            },
            ClientModelConfig {
                model_uid: String::from("per-thousand"),
                model_dimensions: vec![
                    dimension("Input", 0.003, "1K tokens", model_dimension_kind::COST),
                    dimension("Output", 7.5, "500K tokens", model_dimension_kind::COST),
                    dimension(
                        "Cached input",
                        1.0,
                        "1 M tokens",
                        model_dimension_kind::COST,
                    ),
                ],
                ..Default::default()
            },
            ClientModelConfig {
                model_uid: String::from("unparseable"),
                model_dimensions: vec![
                    dimension("Input", 2.0, "tokens", model_dimension_kind::COST),
                    dimension("Output", 8.0, "0 tokens", model_dimension_kind::COST),
                ],
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let models = decode_discovery(&response.encode_to_vec()).unwrap();
    let rates = |uid: &str| {
        let model = models.iter().find(|model| model.uid == uid).expect(uid);
        (model.cost_input, model.cost_output, model.cost_cache_read)
    };
    // The marker dimension ends the headline card.
    assert_eq!(rates("per-million"), (3.0, 15.0, 0.3));
    // `1K` and `500K` denominators scale to per-million; `1 M` reads too.
    assert_eq!(rates("per-thousand"), (3.0, 15.0, 1.0));
    // No usable denominator count falls back to a million tokens.
    assert_eq!(rates("unparseable"), (2.0, 8.0, 0.0));
}

#[test]
fn discovery_decode_normalizes_rows() {
    let config = |uid: &str, label: &str| ClientModelConfig {
        label: String::from(label),
        model_uid: String::from(uid),
        supports_images: true,
        max_tokens: 200_000,
        model_info: Some(ModelInfo {
            max_output_tokens: 64_000,
            display_option: display_option::NORMAL,
            model_features: Some(ModelFeatures {
                supports_images: true,
                supports_tool_calls: true,
                supports_parallel_tool_calls: true,
                supports_thinking: true,
            }),
            ..Default::default()
        }),
        model_dimensions: vec![ModelDimension {
            label: String::from("input"),
            value: 3.0,
            denominator: String::from("1M tokens"),
            kind: model_dimension_kind::COST,
        }],
        model_family_metadata: Some(ModelFamilyMetadata {
            model_family_label: String::from("Sol"),
            entries: vec![ModelFamilyMetadataEntry {
                key: String::from("effort"),
                value: Some(ModelFamilyMetadataValue {
                    order: 3,
                    name: String::from("High"),
                }),
            }],
            is_default_model_in_family: true,
        }),
        ..Default::default()
    };
    let mut response = GetCliModelConfigsResponse {
        client_model_configs: vec![
            config("claude-x", "Claude X"),
            config("swe-1-6", "SWE 1.6"),
            ClientModelConfig {
                disabled: true,
                ..config("dead", "Dead")
            },
            config("claude-x", "Claude X dup"),
        ],
        ..Default::default()
    };
    response.client_model_configs[1]
        .model_family_metadata
        .take();
    let body = response.encode_to_vec();
    let models = decode_discovery(&body).unwrap();
    assert_eq!(models.len(), 2);
    let first = &models[0];
    assert_eq!(first.uid, "claude-x");
    assert_eq!(first.max_tokens, 200_000);
    assert_eq!(first.max_output_tokens, 64_000);
    assert!(first.supports_images);
    assert!(first.supports_thinking);
    assert!(first.supports_parallel_tool_calls);
    assert_eq!(first.cost_input, 3.0);
    assert_eq!(first.family_label.as_deref(), Some("Sol"));
    assert_eq!(
        first.family_entries,
        vec![(String::from("effort"), 3, String::from("High"))]
    );
    assert!(first.is_family_default);
    // The blind uid loses images despite advertising them.
    assert!(!models[1].supports_images);
}

#[test]
fn thinking_labels_match_whole_words_not_substrings() {
    // Featureless rows fall back to the label wording: ladder names need a
    // whole word (`Maximum`, `allow`, `flowing`, and `highlight` claim no
    // reasoning), while the `think` stem still catches `Thinking`.
    let thinking = |uid: &str, label: &str| {
        let response = GetCliModelConfigsResponse {
            client_model_configs: vec![ClientModelConfig {
                label: String::from(label),
                model_uid: String::from(uid),
                ..Default::default()
            }],
            ..Default::default()
        };
        decode_discovery(&response.encode_to_vec()).expect("row decodes")
    };
    let flag = |uid: &str, label: &str| {
        thinking(uid, label)
            .first()
            .expect("one row")
            .supports_thinking
    };
    assert!(flag("t1", "Cascade Thinking"));
    assert!(flag("t2", "Claude Max"));
    assert!(flag("t3", "High effort lane"));
    assert!(!flag("f1", "Maximum output"));
    assert!(!flag("f2", "allow-list model"));
    assert!(!flag("f3", "flowing highlight"));
    assert!(!flag("f4", "Standard model"));
}

#[test]
fn stream_drop_after_first_event_is_clean() {
    let frames = [
        frame_response(&text_response("one", 0), 0, false),
        frame_response(&text_response("two", stop_reason::STOP_PATTERN), 0, false),
        end_stream_frame(),
    ];
    let bytes: Vec<u8> = frames.concat();
    let chunks: Vec<Vec<u8>> = bytes.into_iter().map(|byte| vec![byte]).collect();
    let mut stream = Box::pin(decode_chunks(stream::iter(chunks), "devin", "devin-test"));
    let first = block_on(stream.next()).expect("first event");
    assert!(matches!(first, Ok(StreamEvent::TextDelta { .. })));
    drop(stream);
}

#[test]
fn assign_body_carries_the_router_turn() {
    let request = devin_request(vec![user("route me")]);
    let prompt = router_prompt(&request)
        .expect("router prompt builds")
        .expect("router prompt");
    assert_eq!(prompt.message_id, "");
    let assign = AssignModelRequest::decode(
        assign_body(
            "devin-session-token$wire-key",
            "adaptive",
            "cascade-1",
            Some(prompt),
        )
        .as_slice(),
    )
    .unwrap();
    assert_eq!(assign.model_router_uid, "adaptive");
    assert_eq!(assign.cascade_id, "cascade-1");
    assert_eq!(
        assign.metadata.expect("metadata").api_key,
        "devin-session-token$wire-key"
    );
    let carried = assign.chat_message_prompt.expect("router turn");
    assert_eq!(carried.source, chat_message_source::USER);
    assert_eq!(carried.prompt, "route me");
    assert_eq!(carried.message_id, "");
}
