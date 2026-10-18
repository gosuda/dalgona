//! Behavioral witnesses for the Cursor codec: real frames through the real
//! decoders, hand-built wire bytes pinning the protobuf tags, and full
//! request builds. Every test drives production code; no mock mirrors it.

use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
};

use bytes::Bytes;
use dal_core::{
    AssistantPart, BlobId, CallId, ContextItem, Family, ModelRequest, ModelRoute, ModelToolSpec,
    Part, Purpose, RawJson, ReplayCapture, ReplayRefusal, ReplaySource, RequestParams,
};
use futures::{SinkExt as _, channel::mpsc};
use prost::Message as _;

use super::{
    decode::{CursorTransport, RunStream, Session, SessionConfig},
    frame::{
        COMPRESSED_FLAG, END_STREAM_FLAG, FrameDecoder, FrameError, MAX_FRAME_BYTES, encode_frame,
        map_connect_error, map_status, parse_trailer,
    },
    lower::{
        BlobStore, CursorBodyError, CursorHistory, CursorRouteParts, SegmentReplay, TurnDigest,
        WireIds, WireModel, build_run, lower_root_entries, match_replays, normalize_call_id,
    },
    proto::*,
};
use crate::{
    error::ProviderError,
    run::{HostToolResult, RunLostReason, RunTransport},
    stream::{StopReason, StreamEvent, ToolArgs},
};

fn frame_of(payload: &[u8], flags: u8) -> Vec<u8> {
    encode_frame(payload, flags).unwrap()
}

fn proto_frame(message: &impl prost::Message) -> Vec<u8> {
    frame_of(&message.encode_to_vec(), 0)
}

fn text_message(text: &str) -> AgentServerMessage {
    AgentServerMessage {
        message: Some(server_message::Body::InteractionUpdate(InteractionUpdate {
            update: Some(interaction_update::Update::TextDelta(TextDeltaUpdate {
                text: text.to_owned(),
            })),
        })),
    }
}

fn turn_ended_message(input: i64, output: i64) -> AgentServerMessage {
    AgentServerMessage {
        message: Some(server_message::Body::InteractionUpdate(InteractionUpdate {
            update: Some(interaction_update::Update::TurnEnded(TurnEndedUpdate {
                input_tokens: Some(input),
                output_tokens: Some(output),
                cache_read_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
            })),
        })),
    }
}

fn step_completed_message(step_id: u64) -> AgentServerMessage {
    AgentServerMessage {
        message: Some(server_message::Body::InteractionUpdate(InteractionUpdate {
            update: Some(interaction_update::Update::StepCompleted(
                StepCompletedUpdate { step_id },
            )),
        })),
    }
}

fn exec_mcp_message(id: u32, tool_call_id: &str, tool: &str) -> AgentServerMessage {
    AgentServerMessage {
        message: Some(server_message::Body::ExecServerMessage(ExecServerMessage {
            id,
            exec_id: String::new(),
            body: Some(exec_server_message::Body::McpArgs(McpArgs {
                name: tool.to_owned(),
                args: BTreeMap::new(),
                tool_call_id: tool_call_id.to_owned(),
                tool_name: tool.to_owned(),
                provider_identifier: String::new(),
                smart_mode_approval_only: false,
                skip_approval: false,
                server_identifier: String::new(),
            })),
        })),
    }
}

fn test_route() -> CursorRouteParts {
    CursorRouteParts {
        model: "cursor-test-model".into(),
        details: "cursor-test-model".into(),
        max_mode: false,
        parameters: Vec::new(),
    }
}

fn test_session() -> Session {
    Session::new(
        SessionConfig {
            provider: "cursor-test".into(),
            restore: HashMap::new(),
            wire_names: vec![String::from("fetch_data")],
            tools: Vec::new(),
            rules: Vec::new(),
            blobs: BTreeMap::new(),
            digest: TurnDigest::new(),
            has_active_user: true,
        },
        test_route(),
        String::from("req-1"),
    )
}

fn feed(
    session: &mut Session,
    chunks: &[Vec<u8>],
) -> (Vec<StreamEvent>, Vec<Vec<u8>>, Option<ProviderError>) {
    let mut events = Vec::new();
    let mut writes = Vec::new();
    let mut failure = None;
    for chunk in chunks {
        let (mut out, error) = session.on_bytes(chunk);
        events.append(&mut out.events);
        writes.append(&mut out.writes);
        if error.is_some() {
            failure = error;
            break;
        }
    }
    (events, writes, failure)
}

#[test]
fn frame_round_trip_over_any_split() {
    let payload = b"hello cursor".to_vec();
    let wire = frame_of(&payload, 0);
    for split in 1..wire.len() {
        let mut decoder = FrameDecoder::new();
        let mut frames = Vec::new();
        decoder.push(&wire[..split], &mut frames).unwrap();
        assert!(frames.is_empty(), "no frame before the split at {split}");
        decoder.push(&wire[split..], &mut frames).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].flags, 0);
        assert_eq!(frames[0].payload, payload);
    }
}

#[test]
fn oversize_prefix_fails_closed() {
    let mut header = vec![0_u8];
    header.extend_from_slice(&u32::try_from(MAX_FRAME_BYTES + 1).unwrap().to_be_bytes());
    let mut decoder = FrameDecoder::new();
    let mut frames = Vec::new();
    assert!(matches!(
        decoder.push(&header, &mut frames),
        Err(FrameError::TooLarge { .. })
    ));
    assert_eq!(frames, []);
    assert!(decoder.push(b"more", &mut frames).is_err());
    assert_eq!(decoder.pending(), 0);
}

#[test]
fn gzip_flag_round_trips() {
    use std::io::Write as _;

    let payload = b"compressed-payload".repeat(100);
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&payload).unwrap();
    let compressed = encoder.finish().unwrap();
    let wire = frame_of(&compressed, COMPRESSED_FLAG);
    let mut decoder = FrameDecoder::new();
    let mut frames = Vec::new();
    decoder.push(&wire, &mut frames).unwrap();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].payload, payload);

    let wire = frame_of(b"not gzip at all", COMPRESSED_FLAG);
    let mut decoder = FrameDecoder::new();
    let mut frames = Vec::new();
    assert!(matches!(
        decoder.push(&wire, &mut frames),
        Err(FrameError::Gzip { .. })
    ));
}

#[test]
fn trailer_shapes() {
    assert_eq!(parse_trailer(b"{}").unwrap(), None);
    let error = parse_trailer(br#"{"error":{"code":"unauthenticated","message":"nope"}}"#)
        .unwrap()
        .unwrap();
    assert_eq!(error.code, "unauthenticated");
    assert_eq!(error.message, "nope");
    assert!(parse_trailer(b"[1,2]").is_err());
}

#[test]
fn connect_errors_map() {
    let provider = "cursor-test";
    let structured = |code: i32, retryable: Option<bool>| {
        map_connect_error(
            provider,
            &super::frame::ConnectError {
                code: String::from("error"),
                message: String::from("m"),
                details: vec![structured_detail(code, retryable)],
            },
        )
    };
    assert!(matches!(
        structured(8, None),
        ProviderError::RateLimited { .. }
    ));
    assert!(matches!(structured(10, None), ProviderError::Quota { .. }));
    assert!(matches!(
        structured(43, None),
        ProviderError::ContextOverflow { .. }
    ));
    assert!(matches!(
        structured(21, None),
        ProviderError::Transport { .. }
    ));
    assert!(matches!(structured(13, None), ProviderError::Overloaded));
    assert!(matches!(
        structured(1, None),
        ProviderError::AuthRejected { .. }
    ));
    assert!(matches!(
        structured(36, None),
        ProviderError::InvalidRequest { .. }
    ));
    let plain = |code: &str| {
        map_connect_error(
            provider,
            &super::frame::ConnectError {
                code: String::from(code),
                message: String::from("m"),
                details: Vec::new(),
            },
        )
    };
    assert!(matches!(
        plain("unauthenticated"),
        ProviderError::AuthRejected { .. }
    ));
    assert!(matches!(
        plain("resource_exhausted"),
        ProviderError::RateLimited { .. }
    ));
    assert!(matches!(plain("unavailable"), ProviderError::Overloaded));
    assert!(matches!(
        plain("bogus-code"),
        ProviderError::Status { status: 500, .. }
    ));
    let status = map_status(provider, 401, b"proxy refused");
    assert!(matches!(status, ProviderError::Status { status: 401, .. }));
    let status = map_status(
        provider,
        400,
        br#"{"code":"invalid_argument","message":"bad"}"#,
    );
    assert!(matches!(status, ProviderError::InvalidRequest { .. }));
}

fn structured_detail(code: i32, retryable: Option<bool>) -> super::frame::ConnectDetail {
    let detail = ErrorDetails {
        error: code,
        details: Some(CustomErrorDetails {
            title: String::from("t"),
            detail: String::from("d"),
            is_retryable: retryable,
        }),
    };
    let raw = detail.encode_to_vec();
    super::frame::ConnectDetail {
        detail_type: Some(String::from("aiserver.v1.ErrorDetails")),
        value: Some(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            raw,
        )),
        debug: None,
    }
}

#[test]
fn protobuf_tags_match_the_pin() {
    let text = TextDeltaUpdate {
        text: String::from("hi"),
    };
    assert_eq!(text.encode_to_vec(), vec![0x0A, 0x02, b'h', b'i']);
    let update = InteractionUpdate {
        update: Some(interaction_update::Update::TextDelta(text)),
    };
    assert_eq!(
        update.encode_to_vec(),
        vec![0x0A, 0x04, 0x0A, 0x02, b'h', b'i']
    );
    let hand_rolled = vec![0x0A, 0x06, 0x0A, 0x04, 0x0A, 0x02, b'h', b'i'];
    let decoded = AgentServerMessage::decode(hand_rolled.as_slice()).unwrap();
    match decoded.message {
        Some(server_message::Body::InteractionUpdate(update)) => match update.update {
            Some(interaction_update::Update::TextDelta(delta)) => assert_eq!(delta.text, "hi"),
            other => panic!("wrong update: {other:?}"),
        },
        other => panic!("wrong body: {other:?}"),
    }
    let hand_ended = vec![0x0A, 0x06, 0x72, 0x04, 0x08, 0x64, 0x10, 0x14];
    let decoded = AgentServerMessage::decode(hand_ended.as_slice()).unwrap();
    match decoded.message {
        Some(server_message::Body::InteractionUpdate(update)) => match update.update {
            Some(interaction_update::Update::TurnEnded(ended)) => {
                assert_eq!(ended.input_tokens, Some(100));
                assert_eq!(ended.output_tokens, Some(20));
            }
            other => panic!("wrong update: {other:?}"),
        },
        other => panic!("wrong body: {other:?}"),
    }
}

#[test]
fn text_turn_emits_one_terminal() {
    let mut session = test_session();
    let (events, writes, failure) = feed(
        &mut session,
        &[
            proto_frame(&text_message("hello ")),
            proto_frame(&text_message("world")),
            proto_frame(&turn_ended_message(100, 20)),
        ],
    );
    assert!(failure.is_none());
    assert_eq!(writes, [] as [Vec<u8>; 0]);
    assert!(matches!(
        events.as_slice(),
        [
            StreamEvent::TextDelta { .. },
            StreamEvent::TextDelta { .. },
            StreamEvent::ToolCallsDone { .. },
            StreamEvent::Usage { .. },
            StreamEvent::Stop { .. },
        ]
    ));
    match &events[3] {
        StreamEvent::Usage { usage } => {
            assert_eq!(usage.input_tokens, 100);
            assert_eq!(usage.output_tokens, 20);
            assert_eq!(usage.cached_input_tokens, 0);
            assert_eq!(usage.reasoning_tokens, None);
        }
        other => panic!("wrong event: {other:?}"),
    }
    assert!(matches!(
        events[4],
        StreamEvent::Stop {
            reason: StopReason::EndTurn,
        }
    ));
    let (events, _, failure) = feed(&mut session, &[proto_frame(&text_message("late"))]);
    assert_eq!(events, []);
    assert!(failure.is_none());
}

#[test]
fn cut_before_terminal_is_a_cut() {
    let mut session = test_session();
    let end_stream = frame_of(
        br#"{"error":{"code":"unavailable","message":"gone"}}"#,
        END_STREAM_FLAG,
    );
    let (events, _, failure) = feed(
        &mut session,
        &[proto_frame(&text_message("hi")), end_stream],
    );
    assert!(matches!(events.as_slice(), [StreamEvent::TextDelta { .. }]));
    assert!(matches!(failure, Some(ProviderError::Overloaded)));
}

#[test]
fn clean_end_after_the_final_step_is_lenient() {
    let mut session = test_session();
    let end_stream = frame_of(b"{}", END_STREAM_FLAG);
    let (events, _, failure) = feed(
        &mut session,
        &[
            proto_frame(&text_message("done")),
            proto_frame(&step_completed_message(7)),
            end_stream,
        ],
    );
    assert!(failure.is_none());
    assert!(matches!(
        events.as_slice(),
        [
            StreamEvent::TextDelta { .. },
            StreamEvent::ToolCallsDone { .. },
            StreamEvent::Usage { .. },
            StreamEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ]
    ));
}

#[test]
fn turn_ended_with_unanswered_calls_is_a_protocol_error() {
    let mut session = test_session();
    let (events, _, failure) = feed(
        &mut session,
        &[
            proto_frame(&exec_mcp_message(3, "call-9", "fetch_data")),
            proto_frame(&turn_ended_message(10, 5)),
        ],
    );
    assert!(matches!(
        events.as_slice(),
        [
            StreamEvent::ToolCallStarted { .. },
            StreamEvent::ToolArgsDelta { .. },
        ]
    ));
    assert!(matches!(failure, Some(ProviderError::Protocol { .. })));
}

#[test]
fn connect_scm_announcement_loses_the_run() {
    let mut session = test_session();
    let started = ToolCallStartedUpdate {
        call_id: String::from("env-1"),
        tool_call: Some(ToolCall {
            tool: Some(tool_call::Tool::ConnectScm(OpaqueToolCall {})),
            tool_call_id: None,
        }),
    };
    let message = AgentServerMessage {
        message: Some(server_message::Body::InteractionUpdate(InteractionUpdate {
            update: Some(interaction_update::Update::ToolCallStarted(started)),
        })),
    };
    let (events, _, failure) = feed(&mut session, &[proto_frame(&message)]);
    assert_eq!(events, []);
    match failure {
        Some(ProviderError::RunLost { reason, .. }) => {
            assert!(matches!(reason, RunLostReason::Authority(_)));
        }
        other => panic!("wrong failure: {other:?}"),
    }
}

#[test]
fn interaction_queries_are_refused_in_band() {
    let mut session = test_session();
    let query = AgentServerMessage {
        message: Some(server_message::Body::InteractionQuery(InteractionQuery {
            id: 11,
            query: Some(interaction_query::Query::WebSearch(EmptyQuery {})),
        })),
    };
    let (events, writes, failure) = feed(&mut session, &[proto_frame(&query)]);
    assert_eq!(events, []);
    assert!(failure.is_none());
    assert_eq!(writes.len(), 1);
    let frame = &writes[0][5..];
    let client = AgentClientMessage::decode(frame).unwrap();
    match client.message {
        Some(client_message::Body::InteractionResponse(response)) => {
            assert_eq!(response.id, 11);
            assert!(matches!(
                response.result,
                Some(interaction_response::Result::WebSearch(_))
            ));
        }
        other => panic!("wrong reply: {other:?}"),
    }
}

#[test]
fn approval_probe_and_unknown_tool_answer_at_once() {
    let mut session = test_session();
    let probe = AgentServerMessage {
        message: Some(server_message::Body::ExecServerMessage(ExecServerMessage {
            id: 5,
            exec_id: String::new(),
            body: Some(exec_server_message::Body::McpArgs(McpArgs {
                name: String::from("fetch_data"),
                args: BTreeMap::new(),
                tool_call_id: String::from("call-1"),
                tool_name: String::from("fetch_data"),
                provider_identifier: String::new(),
                smart_mode_approval_only: true,
                skip_approval: false,
                server_identifier: String::new(),
            })),
        })),
    };
    let (events, writes, failure) = feed(&mut session, &[proto_frame(&probe)]);
    assert_eq!(events, []);
    assert!(failure.is_none());
    assert_eq!(writes.len(), 1);

    let unknown = AgentServerMessage {
        message: Some(server_message::Body::ExecServerMessage(ExecServerMessage {
            id: 6,
            exec_id: String::new(),
            body: Some(exec_server_message::Body::McpArgs(McpArgs {
                name: String::from("nope"),
                args: BTreeMap::new(),
                tool_call_id: String::from("call-2"),
                tool_name: String::from("nope"),
                provider_identifier: String::new(),
                smart_mode_approval_only: false,
                skip_approval: false,
                server_identifier: String::new(),
            })),
        })),
    };
    let (events, writes, failure) = feed(&mut session, &[proto_frame(&unknown)]);
    assert_eq!(events, []);
    assert!(failure.is_none());
    assert_eq!(writes.len(), 1);
    let frame = &writes[0][5..];
    let client = AgentClientMessage::decode(frame).unwrap();
    match client.message {
        Some(client_message::Body::ExecClientMessage(exec)) => match exec.message {
            Some(exec_client_message::Body::McpResult(result)) => match result.result {
                Some(mcp_result::Result::ToolNotFound(not_found)) => {
                    assert_eq!(not_found.name, "nope");
                    assert_eq!(not_found.available_tools, vec![String::from("fetch_data")]);
                }
                other => panic!("wrong result: {other:?}"),
            },
            other => panic!("wrong body: {other:?}"),
        },
        other => panic!("wrong reply: {other:?}"),
    }
}

#[test]
fn native_shell_is_refused_never_run() {
    let mut session = test_session();
    let shell = AgentServerMessage {
        message: Some(server_message::Body::ExecServerMessage(ExecServerMessage {
            id: 7,
            exec_id: String::new(),
            body: Some(exec_server_message::Body::ShellArgs(ShellArgs {
                command: String::from("rm -rf /"),
                working_directory: String::from("/tmp"),
            })),
        })),
    };
    let (events, writes, failure) = feed(&mut session, &[proto_frame(&shell)]);
    assert_eq!(events, []);
    assert!(failure.is_none());
    assert_eq!(writes.len(), 1);
    let frame = &writes[0][5..];
    let client = AgentClientMessage::decode(frame).unwrap();
    assert!(matches!(
        client.message,
        Some(client_message::Body::ExecClientMessage(_))
    ));
}

#[test]
fn kv_store_round_trips() {
    let mut session = test_session();
    let set = AgentServerMessage {
        message: Some(server_message::Body::KvServerMessage(KvServerMessage {
            id: 9,
            message: Some(kv_server_message::Body::SetBlob(SetBlobArgs {
                blob_id: vec![1, 2, 3],
                blob_data: b"blob-bytes".to_vec(),
            })),
        })),
    };
    let get = AgentServerMessage {
        message: Some(server_message::Body::KvServerMessage(KvServerMessage {
            id: 10,
            message: Some(kv_server_message::Body::GetBlob(GetBlobArgs {
                blob_id: vec![1, 2, 3],
            })),
        })),
    };
    let missing = AgentServerMessage {
        message: Some(server_message::Body::KvServerMessage(KvServerMessage {
            id: 11,
            message: Some(kv_server_message::Body::GetBlob(GetBlobArgs {
                blob_id: vec![9, 9, 9],
            })),
        })),
    };
    let (events, writes, failure) = feed(&mut session, &[proto_frame(&set), proto_frame(&get)]);
    assert_eq!(events, []);
    assert!(failure.is_none());
    assert_eq!(writes.len(), 2);
    let get_reply = AgentClientMessage::decode(&writes[1][5..]).unwrap();
    match get_reply.message {
        Some(client_message::Body::KvClientMessage(kv)) => match kv.message {
            Some(kv_client_message::Body::GetBlobResult(result)) => {
                assert_eq!(result.blob_data, Some(b"blob-bytes".to_vec()));
            }
            other => panic!("wrong kv reply: {other:?}"),
        },
        other => panic!("wrong reply: {other:?}"),
    }
    let (_, writes, failure) = feed(&mut session, &[proto_frame(&missing)]);
    assert!(failure.is_none());
    let missing_reply = AgentClientMessage::decode(&writes[0][5..]).unwrap();
    match missing_reply.message {
        Some(client_message::Body::KvClientMessage(kv)) => match kv.message {
            Some(kv_client_message::Body::GetBlobResult(result)) => {
                assert_eq!(result.blob_data, None);
            }
            other => panic!("wrong kv reply: {other:?}"),
        },
        other => panic!("wrong reply: {other:?}"),
    }
}

#[test]
fn cancel_frame_carries_the_cancel_action() {
    let frame = Session::cancel_frame();
    let client = AgentClientMessage::decode(&frame[5..]).unwrap();
    assert!(matches!(
        client.message,
        Some(client_message::Body::ConversationAction(_))
    ));
}

fn model_request(context: Vec<ContextItem>) -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::Cursor,
            model: "cursor-test-model".into(),
        },
        system: "You are a test assistant.".into(),
        tools: vec![ModelToolSpec {
            name: "fetch_data".into(),
            description: "Fetches data.".into(),
            parameters: RawJson::parse(r#"{"type":"object"}"#).unwrap(),
            grammar: None,
            defer_loading: false,
        }]
        .into(),
        context: context.into(),
        params: RequestParams::default(),
        cache_key: None,
    }
}

fn history_context() -> Vec<ContextItem> {
    vec![
        ContextItem::User {
            parts: vec![Part::Text {
                text: "hello".into(),
            }],
        },
        ContextItem::Assistant {
            source: ReplaySource {
                family: Family::Cursor,
                model: "cursor-test-model".into(),
            },
            parts: vec![
                AssistantPart::Text {
                    text: "hi".into(),
                    replay: None,
                },
                AssistantPart::ToolCall {
                    call: CallId::new("c.1"),
                    name: "fetch_data".into(),
                    args: RawJson::parse(r#"{"q":1}"#).unwrap(),
                    replay: None,
                },
            ],
            replay: None,
        },
        ContextItem::ToolResult {
            call: CallId::new("c.1"),
            name: "fetch_data".into(),
            is_error: false,
            parts: vec![Part::Text {
                text: "done".into(),
            }],
        },
        ContextItem::User {
            parts: vec![Part::Text {
                text: "second question".into(),
            }],
        },
    ]
}

fn run_request_of(frame: &[u8]) -> AgentRunRequest {
    let client = AgentClientMessage::decode(&frame[5..]).unwrap();
    match client.message {
        Some(client_message::Body::RunRequest(request)) => request,
        other => panic!("wrong first frame: {other:?}"),
    }
}

#[test]
fn run_request_lowers_history_and_tools() {
    let request = model_request(history_context());
    let lowered = build_run(
        &request,
        &WireModel::plain("cursor-test-model"),
        "conv-1",
        "msg-1",
    )
    .unwrap();
    let run = run_request_of(&lowered.first_frame);
    assert_eq!(run.conversation_id.as_deref(), Some("conv-1"));
    assert_eq!(
        run.model_details.as_ref().unwrap().model_id,
        "cursor-test-model"
    );
    assert_eq!(
        run.requested_model.as_ref().unwrap().model_id,
        "cursor-test-model"
    );
    match run.action.as_ref().unwrap().action.as_ref().unwrap() {
        conversation_action::Action::UserMessageAction(action) => {
            assert_eq!(
                action.user_message.as_ref().unwrap().text,
                "second question"
            );
            assert_eq!(action.request_context.as_ref().unwrap().tools.len(), 1);
        }
        other => panic!("wrong action: {other:?}"),
    }
    let state = run.conversation_state.as_ref().unwrap();
    assert_ne!(state.root_prompt_messages_json, [] as [Vec<u8>; 0]);
    let mut saw_user = false;
    let mut saw_call = false;
    for id in &state.root_prompt_messages_json {
        use sonic_rs::JsonValueTrait as _;
        let bytes = lowered.blobs.get(id).unwrap();
        let text = std::str::from_utf8(bytes).unwrap();
        let value: sonic_rs::Value = sonic_rs::from_str(text).unwrap();
        if value.get("role").and_then(sonic_rs::JsonValueTrait::as_str) == Some("user") {
            saw_user = true;
        }
        if text.contains("c_1") {
            saw_call = true;
        }
    }
    assert!(saw_user);
    assert!(saw_call);
}

#[test]
fn run_request_builds_byte_identical() {
    let first = build_run(
        &model_request(history_context()),
        &WireModel::plain("cursor-test-model"),
        "conv-1",
        "msg-1",
    )
    .unwrap();
    let second = build_run(
        &model_request(history_context()),
        &WireModel::plain("cursor-test-model"),
        "conv-1",
        "msg-1",
    )
    .unwrap();
    assert_eq!(first.first_frame, second.first_frame);
}

#[test]
fn run_request_rejects_blob_parts() {
    let mut context = history_context();
    context.push(ContextItem::User {
        parts: vec![Part::Blob {
            blob_id: BlobId::from_bytes(b"x"),
            mime: "text/plain".into(),
            bytes: 1,
        }],
    });
    let error = build_run(
        &model_request(context),
        &WireModel::plain("cursor-test-model"),
        "conv-1",
        "msg-1",
    )
    .unwrap_err();
    assert!(matches!(error, CursorBodyError::UnresolvedBlob { .. }));
}

#[test]
fn call_ids_normalize() {
    assert_eq!(normalize_call_id("a.b|c"), "a_b_c");
    assert_eq!(normalize_call_id(""), "call");
    assert_eq!(normalize_call_id(&"x".repeat(100)).len(), 64);
}

proptest::proptest! {
    #[test]
    fn blob_tool_results_keep_textual_digest_identity(bytes: Vec<u8>) {
        let blob_id = BlobId::from_bytes(&bytes);
        let mut blob_digest = TurnDigest::new();
        blob_digest.tool_result("call", true, &[Part::Blob {
            blob_id,
            mime: "application/octet-stream".into(),
            bytes: u64::try_from(bytes.len()).unwrap(),
        }]);
        let mut textual_digest = TurnDigest::new();
        textual_digest.tool_result("call", true, &[Part::Text {
            text: blob_id.to_string().into(),
        }]);
        proptest::prop_assert_eq!(blob_digest.hex(), textual_digest.hex());
    }
}

#[test]
fn replays_bind_route_and_digest() {
    let route = test_route();
    let history = history_context();
    let user_parts = match &history[0] {
        ContextItem::User { parts } => parts.clone(),
        _ => unreachable!(),
    };
    let opening_parts = match &history[1] {
        ContextItem::Assistant { parts, .. } => parts.clone(),
        _ => unreachable!(),
    };
    // A captured turn rides its final step: the opening step issued the
    // call, the host answered it, and the closing step carries no calls.
    let closing_parts = vec![AssistantPart::Text {
        text: "all done".into(),
        replay: None,
    }];
    let mut digest = TurnDigest::new();
    digest.route(&route);
    digest.user_parts(&user_parts).unwrap();
    digest.assistant_parts(&opening_parts);
    if let ContextItem::ToolResult {
        call,
        is_error,
        parts,
        ..
    } = &history[2]
    {
        digest.tool_result(call.as_str(), *is_error, parts);
    }
    digest.assistant_parts(&closing_parts);
    let segment = |payload: RawJson| {
        vec![
            history[0].clone(),
            history[1].clone(),
            history[2].clone(),
            ContextItem::Assistant {
                source: ReplaySource {
                    family: Family::Cursor,
                    model: "cursor-test-model".into(),
                },
                parts: closing_parts.clone(),
                replay: Some(payload),
            },
        ]
    };
    let stored = CursorHistory::new(&route, digest.hex(), vec![String::from("c.1")], vec![]);
    let with_replay = segment(stored.to_payload().unwrap());
    let found = match_replays(&with_replay, "cursor-test-model", &route).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].user, 0);
    assert_eq!(found[0].assistant, 3);

    let tampered = CursorHistory::new(&route, String::from("nope"), vec![], vec![]);
    let with_tampered = segment(tampered.to_payload().unwrap());
    assert!(
        match_replays(&with_tampered, "cursor-test-model", &route)
            .unwrap()
            .is_empty()
    );

    let mut without_result = with_replay.clone();
    without_result.remove(2);
    assert!(
        match_replays(&without_result, "cursor-test-model", &route)
            .unwrap()
            .is_empty()
    );

    assert!(
        match_replays(&with_replay, "other-model", &route)
            .unwrap()
            .is_empty()
    );
    let mut other_route = route.clone();
    other_route.max_mode = true;
    assert!(
        match_replays(&with_replay, "cursor-test-model", &other_route)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn run_stream_closes_a_tool_step_on_quiescence() {
    use futures::StreamExt as _;

    let session = Arc::new(Mutex::new(test_session()));
    let (mut body_tx, body_rx) = mpsc::channel::<Result<Bytes, ProviderError>>(8);
    let (out_tx, mut out_rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    let mut stream = RunStream::new(session.clone(), Box::pin(body_rx), out_tx.clone());
    body_tx
        .send(Ok(Bytes::from(proto_frame(&exec_mcp_message(
            3,
            "call-9",
            "fetch_data",
        )))))
        .await
        .unwrap();
    let mut events = Vec::new();
    for _ in 0..8 {
        match futures::poll!(stream.next()) {
            std::task::Poll::Ready(Some(event)) => events.push(event),
            _ => break,
        }
    }
    assert!(matches!(
        events.as_slice(),
        [
            Ok(StreamEvent::ToolCallStarted { .. }),
            Ok(StreamEvent::ToolArgsDelta { .. }),
            Ok(StreamEvent::ToolCallsDone { .. }),
            Ok(StreamEvent::Usage { .. }),
            Ok(StreamEvent::Stop {
                reason: StopReason::ToolUse,
            }),
        ]
    ));
    match &events[2] {
        Ok(StreamEvent::ToolCallsDone { calls }) => {
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].id, "call-9");
            assert_eq!(calls[0].name, "fetch_data");
            assert!(matches!(calls[0].args, ToolArgs::Parsed(_)));
        }
        other => panic!("wrong event: {other:?}"),
    }

    // The transport shares the stream's outbox, as `open_run` wires them:
    // host results travel on the same request body the heartbeats use.
    let mut transport = CursorTransport::new(session.clone(), out_tx);
    transport
        .send(vec![HostToolResult {
            call: CallId::new("call-9"),
            name: "fetch_data".into(),
            is_error: false,
            parts: vec![Part::Text {
                text: "result-text".into(),
            }],
        }])
        .unwrap();
    let frame = out_rx.next().await.unwrap().unwrap();
    let client = AgentClientMessage::decode(&frame[5..]).unwrap();
    match client.message {
        Some(client_message::Body::ExecClientMessage(exec)) => match exec.message {
            Some(exec_client_message::Body::McpResult(result)) => match result.result {
                Some(mcp_result::Result::Success(success)) => {
                    assert!(!success.is_error);
                    assert_eq!(success.content.len(), 1);
                }
                other => panic!("wrong result: {other:?}"),
            },
            other => panic!("wrong body: {other:?}"),
        },
        other => panic!("wrong reply: {other:?}"),
    }
    drop(transport);
    drop(body_tx);
    // The answered call never reached a terminal: a body that ends here is
    // a cut, reported once, and the closed step wrote nothing further.
    assert!(matches!(
        stream.next().await,
        Some(Err(ProviderError::StreamCut))
    ));
    assert!(stream.next().await.is_none());
    assert!(matches!(out_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));

    let mut transport = CursorTransport::new(session.clone(), {
        let (tx, _) = mpsc::channel(8);
        tx
    });
    transport.abort();
    assert!(session.lock().unwrap().is_released());
    assert!(matches!(
        transport.send(Vec::new()),
        Err(RunLostReason::Transport(_))
    ));
}
fn decoded_root_texts(frame: &[u8], blobs: &BTreeMap<Vec<u8>, Vec<u8>>) -> Vec<String> {
    let run = run_request_of(frame);
    let state = run.conversation_state.as_ref().unwrap();
    state
        .root_prompt_messages_json
        .iter()
        .map(|id| {
            std::str::from_utf8(blobs.get(id).unwrap())
                .unwrap()
                .to_owned()
        })
        .collect()
}

fn count_containing(haystacks: &[String], needle: &str) -> usize {
    haystacks
        .iter()
        .filter(|text| text.contains(needle))
        .count()
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "replay fixture tests covered span replacement in place"
)]
fn replay_replaces_covered_span_once() {
    let route = test_route();
    let user_parts = vec![Part::Text {
        text: "hello-replay-7f3a".into(),
    }];
    let opening_parts = vec![
        AssistantPart::Text {
            text: "hi-segment-inner-7f3a".into(),
            replay: None,
        },
        AssistantPart::ToolCall {
            call: CallId::new("c.seg1"),
            name: "fetch_data".into(),
            args: RawJson::parse(r#"{"q":1}"#).unwrap(),
            replay: None,
        },
    ];
    let result_parts = vec![Part::Text {
        text: "done-segment-inner-7f3a".into(),
    }];
    let closing_parts = vec![AssistantPart::Text {
        text: "close-segment-7f3a".into(),
        replay: None,
    }];
    let mut digest = TurnDigest::new();
    digest.route(&route);
    digest.user_parts(&user_parts).unwrap();
    digest.assistant_parts(&opening_parts);
    digest.tool_result("c.seg1", false, &result_parts);
    digest.assistant_parts(&closing_parts);
    let stored = CursorHistory::new(
        &route,
        digest.hex(),
        vec![String::from("c.seg1")],
        vec![String::from(
            r#"{"role":"assistant","content":[{"type":"text","text":"STORED-SEGMENT-MARKER-7f3a"}]}"#,
        )],
    );
    let cursor_source = || ReplaySource {
        family: Family::Cursor,
        model: "cursor-test-model".into(),
    };
    let context = vec![
        ContextItem::User {
            parts: vec![Part::Text {
                text: "earlier-question-7f3a".into(),
            }],
        },
        ContextItem::Assistant {
            source: cursor_source(),
            parts: vec![AssistantPart::Text {
                text: "earlier-answer-7f3a".into(),
                replay: None,
            }],
            replay: None,
        },
        ContextItem::User {
            parts: user_parts.clone(),
        },
        ContextItem::Assistant {
            source: cursor_source(),
            parts: opening_parts.clone(),
            replay: None,
        },
        ContextItem::ToolResult {
            call: CallId::new("c.seg1"),
            name: "fetch_data".into(),
            is_error: false,
            parts: result_parts.clone(),
        },
        ContextItem::Assistant {
            source: cursor_source(),
            parts: closing_parts.clone(),
            replay: Some(stored.to_payload().unwrap()),
        },
        ContextItem::User {
            parts: vec![Part::Text {
                text: "later-question-7f3a".into(),
            }],
        },
        ContextItem::Assistant {
            source: cursor_source(),
            parts: vec![AssistantPart::Text {
                text: "later-answer-7f3a".into(),
                replay: None,
            }],
            replay: None,
        },
        ContextItem::User {
            parts: vec![Part::Text {
                text: "live-trailing-7f3a".into(),
            }],
        },
    ];
    let lowered = build_run(
        &model_request(context),
        &WireModel::plain("cursor-test-model"),
        "conv-replay-seg-1",
        "msg-replay-seg-1",
    )
    .unwrap();
    let texts = decoded_root_texts(&lowered.first_frame, &lowered.blobs);
    assert_eq!(count_containing(&texts, "STORED-SEGMENT-MARKER-7f3a"), 1);
    assert_eq!(count_containing(&texts, "hi-segment-inner-7f3a"), 0);
    assert_eq!(count_containing(&texts, "done-segment-inner-7f3a"), 0);
    assert_eq!(count_containing(&texts, "close-segment-7f3a"), 0);
    assert_eq!(count_containing(&texts, "hello-replay-7f3a"), 1);
    assert_eq!(count_containing(&texts, "earlier-answer-7f3a"), 1);
    assert_eq!(count_containing(&texts, "later-answer-7f3a"), 1);
    assert_eq!(count_containing(&texts, "later-question-7f3a"), 1);
    let pos = |needle: &str| texts.iter().position(|text| text.contains(needle)).unwrap();
    assert!(pos("earlier-answer-7f3a") < pos("hello-replay-7f3a"));
    assert!(pos("hello-replay-7f3a") < pos("STORED-SEGMENT-MARKER-7f3a"));
    assert!(pos("STORED-SEGMENT-MARKER-7f3a") < pos("later-question-7f3a"));
}

#[test]
fn replay_invalid_digest_falls_back_completely() {
    let route = test_route();
    let user_parts = vec![Part::Text {
        text: "hello-replay-7f3a".into(),
    }];
    let opening_parts = vec![
        AssistantPart::Text {
            text: "hi-segment-inner-7f3a".into(),
            replay: None,
        },
        AssistantPart::ToolCall {
            call: CallId::new("c.seg1"),
            name: "fetch_data".into(),
            args: RawJson::parse(r#"{"q":1}"#).unwrap(),
            replay: None,
        },
    ];
    let result_parts = vec![Part::Text {
        text: "done-segment-inner-7f3a".into(),
    }];
    let closing_parts = vec![AssistantPart::Text {
        text: "close-segment-7f3a".into(),
        replay: None,
    }];
    let tampered = CursorHistory::new(
        &route,
        String::from("nope"),
        Vec::new(),
        vec![String::from(
            r#"{"role":"assistant","content":[{"type":"text","text":"STORED-SEGMENT-MARKER-7f3a"}]}"#,
        )],
    );
    let cursor_source = || ReplaySource {
        family: Family::Cursor,
        model: "cursor-test-model".into(),
    };
    let context = vec![
        ContextItem::User {
            parts: user_parts.clone(),
        },
        ContextItem::Assistant {
            source: cursor_source(),
            parts: opening_parts.clone(),
            replay: None,
        },
        ContextItem::ToolResult {
            call: CallId::new("c.seg1"),
            name: "fetch_data".into(),
            is_error: false,
            parts: result_parts.clone(),
        },
        ContextItem::Assistant {
            source: cursor_source(),
            parts: closing_parts.clone(),
            replay: Some(tampered.to_payload().unwrap()),
        },
        ContextItem::User {
            parts: vec![Part::Text {
                text: "live-trailing-7f3a".into(),
            }],
        },
    ];
    let lowered = build_run(
        &model_request(context),
        &WireModel::plain("cursor-test-model"),
        "conv-replay-seg-2",
        "msg-replay-seg-2",
    )
    .unwrap();
    let texts = decoded_root_texts(&lowered.first_frame, &lowered.blobs);
    assert_eq!(count_containing(&texts, "STORED-SEGMENT-MARKER-7f3a"), 0);
    assert_eq!(count_containing(&texts, "hi-segment-inner-7f3a"), 1);
    assert_eq!(count_containing(&texts, "done-segment-inner-7f3a"), 1);
    assert_eq!(count_containing(&texts, "close-segment-7f3a"), 1);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "adjacent replay fixture tests tool boundary in place"
)]
fn adjacent_replays_keep_tool_boundary() {
    let route = test_route();
    let cursor_source = || ReplaySource {
        family: Family::Cursor,
        model: "cursor-test-model".into(),
    };
    let user_a = vec![Part::Text {
        text: "first-question-a1b2".into(),
    }];
    let opening_a = vec![
        AssistantPart::Text {
            text: "first-hi-a1b2".into(),
            replay: None,
        },
        AssistantPart::ToolCall {
            call: CallId::new("c.a1"),
            name: "fetch_data".into(),
            args: RawJson::parse(r#"{"q":1}"#).unwrap(),
            replay: None,
        },
    ];
    let result_a = vec![Part::Text {
        text: "first-done-a1b2".into(),
    }];
    let closing_a = vec![AssistantPart::Text {
        text: "first-close-a1b2".into(),
        replay: None,
    }];
    let mut digest_a = TurnDigest::new();
    digest_a.route(&route);
    digest_a.user_parts(&user_a).unwrap();
    digest_a.assistant_parts(&opening_a);
    digest_a.tool_result("c.a1", false, &result_a);
    digest_a.assistant_parts(&closing_a);
    let stored_a = CursorHistory::new(
        &route,
        digest_a.hex(),
        vec![String::from("c.a1")],
        vec![String::from(
            r#"{"role":"assistant","content":[{"type":"text","text":"STORED-A-MARKER-a1b2"}]}"#,
        )],
    );
    let user_b = vec![Part::Text {
        text: "second-question-a1b2".into(),
    }];
    let opening_b = vec![
        AssistantPart::Text {
            text: "second-hi-a1b2".into(),
            replay: None,
        },
        AssistantPart::ToolCall {
            call: CallId::new("c.b1"),
            name: "fetch_data".into(),
            args: RawJson::parse(r#"{"q":2}"#).unwrap(),
            replay: None,
        },
    ];
    let result_b = vec![Part::Text {
        text: "second-done-a1b2".into(),
    }];
    let closing_b = vec![AssistantPart::Text {
        text: "second-close-a1b2".into(),
        replay: None,
    }];
    let mut digest_b = TurnDigest::new();
    digest_b.route(&route);
    digest_b.user_parts(&user_b).unwrap();
    digest_b.assistant_parts(&opening_b);
    digest_b.tool_result("c.b1", false, &result_b);
    digest_b.assistant_parts(&closing_b);
    let stored_b = CursorHistory::new(
        &route,
        digest_b.hex(),
        vec![String::from("c.b1")],
        vec![String::from(
            r#"{"role":"assistant","content":[{"type":"text","text":"STORED-B-MARKER-a1b2"}]}"#,
        )],
    );
    let context = vec![
        ContextItem::User {
            parts: user_a.clone(),
        },
        ContextItem::Assistant {
            source: cursor_source(),
            parts: opening_a.clone(),
            replay: None,
        },
        ContextItem::ToolResult {
            call: CallId::new("c.a1"),
            name: "fetch_data".into(),
            is_error: false,
            parts: result_a.clone(),
        },
        ContextItem::Assistant {
            source: cursor_source(),
            parts: closing_a.clone(),
            replay: Some(stored_a.to_payload().unwrap()),
        },
        ContextItem::User {
            parts: user_b.clone(),
        },
        ContextItem::Assistant {
            source: cursor_source(),
            parts: opening_b.clone(),
            replay: None,
        },
        ContextItem::ToolResult {
            call: CallId::new("c.b1"),
            name: "fetch_data".into(),
            is_error: false,
            parts: result_b.clone(),
        },
        ContextItem::Assistant {
            source: cursor_source(),
            parts: closing_b.clone(),
            replay: Some(stored_b.to_payload().unwrap()),
        },
        ContextItem::User {
            parts: vec![Part::Text {
                text: "after-question-a1b2".into(),
            }],
        },
        ContextItem::Assistant {
            source: cursor_source(),
            parts: vec![AssistantPart::ToolCall {
                call: CallId::new("c.outside"),
                name: "fetch_data".into(),
                args: RawJson::parse(r#"{"q":3}"#).unwrap(),
                replay: None,
            }],
            replay: None,
        },
        ContextItem::ToolResult {
            call: CallId::new("c.outside"),
            name: "fetch_data".into(),
            is_error: false,
            parts: vec![Part::Text {
                text: "outside-result-a1b2".into(),
            }],
        },
        ContextItem::User {
            parts: vec![Part::Text {
                text: "live-trailing-a1b2".into(),
            }],
        },
    ];
    let lowered = build_run(
        &model_request(context),
        &WireModel::plain("cursor-test-model"),
        "conv-replay-seg-3",
        "msg-replay-seg-3",
    )
    .unwrap();
    let texts = decoded_root_texts(&lowered.first_frame, &lowered.blobs);
    assert_eq!(count_containing(&texts, "STORED-A-MARKER-a1b2"), 1);
    assert_eq!(count_containing(&texts, "STORED-B-MARKER-a1b2"), 1);
    assert_eq!(count_containing(&texts, "first-hi-a1b2"), 0);
    assert_eq!(count_containing(&texts, "first-done-a1b2"), 0);
    assert_eq!(count_containing(&texts, "second-hi-a1b2"), 0);
    assert_eq!(count_containing(&texts, "second-done-a1b2"), 0);
    assert_eq!(count_containing(&texts, "after-question-a1b2"), 1);
    assert_eq!(count_containing(&texts, "outside-result-a1b2"), 1);
    assert_eq!(count_containing(&texts, "c_outside"), 2);
    assert_eq!(count_containing(&texts, "c_a1"), 0);
    assert_eq!(count_containing(&texts, "c_b1"), 0);
    let pos = |needle: &str| texts.iter().position(|text| text.contains(needle)).unwrap();
    assert!(pos("STORED-A-MARKER-a1b2") < pos("second-question-a1b2"));
    assert!(pos("second-question-a1b2") < pos("STORED-B-MARKER-a1b2"));
    assert!(pos("STORED-B-MARKER-a1b2") < pos("after-question-a1b2"));
}

#[test]
fn earlier_round_result_inside_replay_is_suppressed() {
    let route = test_route();
    let cursor_source = || ReplaySource {
        family: Family::Cursor,
        model: "cursor-test-model".into(),
    };
    let earlier_parts = vec![
        AssistantPart::Text {
            text: "pre-call-r4".into(),
            replay: None,
        },
        AssistantPart::ToolCall {
            call: CallId::new("c.mid"),
            name: "fetch_data".into(),
            args: RawJson::parse(r#"{"q":9}"#).unwrap(),
            replay: None,
        },
        AssistantPart::Text {
            text: "post-call-r4".into(),
            replay: None,
        },
    ];
    let carrier_parts = vec![AssistantPart::Text {
        text: "carrier-close-r4".into(),
        replay: None,
    }];
    let result_parts = vec![Part::Text {
        text: "inside-span-result-r4".into(),
    }];
    let opener_parts = vec![Part::Text {
        text: "replay-opener-r4".into(),
    }];
    let mut digest = TurnDigest::new();
    digest.route(&route);
    digest.user_parts(&opener_parts).unwrap();
    digest.tool_result("c.mid", false, &result_parts);
    digest.assistant_parts(&carrier_parts);
    let stored = CursorHistory::new(
        &route,
        digest.hex(),
        vec![String::from("c.mid")],
        vec![String::from(
            r#"{"role":"assistant","content":[{"type":"text","text":"STORED-CARRIER-r4"}]}"#,
        )],
    );
    let context = vec![
        ContextItem::User {
            parts: vec![Part::Text {
                text: "earlier-question-r4".into(),
            }],
        },
        ContextItem::Assistant {
            source: cursor_source(),
            parts: earlier_parts,
            replay: None,
        },
        ContextItem::User {
            parts: opener_parts,
        },
        ContextItem::ToolResult {
            call: CallId::new("c.mid"),
            name: "fetch_data".into(),
            is_error: false,
            parts: result_parts,
        },
        ContextItem::Assistant {
            source: cursor_source(),
            parts: carrier_parts,
            replay: Some(stored.to_payload().unwrap()),
        },
        ContextItem::User {
            parts: vec![Part::Text {
                text: "live-trailing-r4".into(),
            }],
        },
    ];
    let lowered = build_run(
        &model_request(context),
        &WireModel::plain("cursor-test-model"),
        "conv-replay-seg-4",
        "msg-replay-seg-4",
    )
    .unwrap();
    let texts = decoded_root_texts(&lowered.first_frame, &lowered.blobs);
    assert_eq!(count_containing(&texts, "inside-span-result-r4"), 0);
    assert_eq!(count_containing(&texts, "pre-call-r4"), 1);
    assert_eq!(count_containing(&texts, "post-call-r4"), 1);
    assert_eq!(count_containing(&texts, "earlier-question-r4"), 1);
    assert_eq!(count_containing(&texts, "replay-opener-r4"), 1);
    assert_eq!(count_containing(&texts, "STORED-CARRIER-r4"), 1);
    let pos = |needle: &str| texts.iter().position(|text| text.contains(needle)).unwrap();
    assert!(pos("pre-call-r4") < pos("replay-opener-r4"));
    assert!(pos("replay-opener-r4") < pos("STORED-CARRIER-r4"));
}

const MAPPED_WIRE: &str = "fetch_data";
const MAPPED_INTERNAL: &str = "acme.server.fetch_data";

fn mapped_session() -> Session {
    let mut restore = HashMap::new();
    restore.insert(MAPPED_WIRE.into(), MAPPED_INTERNAL.into());
    Session::new(
        SessionConfig {
            provider: "cursor-test".into(),
            restore,
            wire_names: vec![String::from(MAPPED_WIRE)],
            tools: Vec::new(),
            rules: Vec::new(),
            blobs: BTreeMap::new(),
            digest: TurnDigest::new(),
            has_active_user: true,
        },
        test_route(),
        String::from("req-1"),
    )
}

async fn pump_frames(
    session: Arc<Mutex<Session>>,
    frames: Vec<Vec<u8>>,
) -> Vec<Result<StreamEvent, ProviderError>> {
    use futures::StreamExt as _;

    let (mut body_tx, body_rx) = mpsc::channel::<Result<Bytes, ProviderError>>(8);
    let (out_tx, _out_rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    let mut stream = RunStream::new(session, Box::pin(body_rx), out_tx);
    for frame in frames {
        body_tx.send(Ok(Bytes::from(frame))).await.unwrap();
    }
    let mut events = Vec::new();
    for _ in 0..16 {
        match futures::poll!(stream.next()) {
            std::task::Poll::Ready(Some(event)) => events.push(event),
            _ => break,
        }
    }
    events
}

#[tokio::test]
async fn tool_events_carry_internal_names() {
    let session = Arc::new(Mutex::new(mapped_session()));
    let events = pump_frames(
        session,
        vec![proto_frame(&exec_mcp_message(3, "call-9", MAPPED_WIRE))],
    )
    .await;
    assert!(matches!(
        events.as_slice(),
        [
            Ok(StreamEvent::ToolCallStarted { .. }),
            Ok(StreamEvent::ToolArgsDelta { .. }),
            Ok(StreamEvent::ToolCallsDone { .. }),
            Ok(StreamEvent::Usage { .. }),
            Ok(StreamEvent::Stop {
                reason: StopReason::ToolUse,
            }),
        ]
    ));
    match &events[0] {
        Ok(StreamEvent::ToolCallStarted { id, name }) => {
            assert_eq!(id, "call-9");
            assert_eq!(name, MAPPED_INTERNAL);
        }
        other => panic!("wrong event: {other:?}"),
    }
    match &events[2] {
        Ok(StreamEvent::ToolCallsDone { calls }) => {
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].id, "call-9");
            assert_eq!(calls[0].name, MAPPED_INTERNAL);
            assert!(matches!(calls[0].args, ToolArgs::Parsed(_)));
        }
        other => panic!("wrong event: {other:?}"),
    }
}

#[tokio::test]
async fn redispatch_reuses_the_announced_call() {
    use futures::StreamExt as _;

    let session = Arc::new(Mutex::new(mapped_session()));
    let (mut body_tx, body_rx) = mpsc::channel::<Result<Bytes, ProviderError>>(8);
    let (out_tx, mut out_rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    let mut stream = RunStream::new(session.clone(), Box::pin(body_rx), out_tx.clone());
    // A streamed announcement arrives first, then the exec frame, then a
    // redispatch of the same wire call under a second numeric id.
    let announced = AgentServerMessage {
        message: Some(server_message::Body::InteractionUpdate(InteractionUpdate {
            update: Some(interaction_update::Update::ToolCallStarted(
                ToolCallStartedUpdate {
                    call_id: String::from("env-1"),
                    tool_call: Some(ToolCall {
                        tool: Some(tool_call::Tool::McpToolCall(McpToolCall {
                            args: Some(McpArgs {
                                name: String::from(MAPPED_WIRE),
                                args: BTreeMap::new(),
                                tool_call_id: String::from("call-9"),
                                tool_name: String::from(MAPPED_WIRE),
                                provider_identifier: String::new(),
                                smart_mode_approval_only: false,
                                skip_approval: false,
                                server_identifier: String::new(),
                            }),
                        })),
                        tool_call_id: None,
                    }),
                },
            )),
        })),
    };
    for frame in [
        proto_frame(&announced),
        proto_frame(&exec_mcp_message(3, "call-9", MAPPED_WIRE)),
        proto_frame(&exec_mcp_message(4, "call-9", MAPPED_WIRE)),
    ] {
        body_tx.send(Ok(Bytes::from(frame))).await.unwrap();
    }
    let mut events = Vec::new();
    for _ in 0..16 {
        match futures::poll!(stream.next()) {
            std::task::Poll::Ready(Some(event)) => events.push(event),
            _ => break,
        }
    }
    // One announcement, one pending call: the redispatch only gained a route.
    let started = events
        .iter()
        .filter(|event| matches!(event, Ok(StreamEvent::ToolCallStarted { .. })))
        .count();
    assert_eq!(started, 1);
    match events.iter().find_map(|event| match event {
        Ok(StreamEvent::ToolCallsDone { calls }) => Some(calls),
        _ => None,
    }) {
        Some(calls) => {
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].id, "call-9");
            assert_eq!(calls[0].name, MAPPED_INTERNAL);
        }
        None => panic!("no ToolCallsDone in {events:?}"),
    }

    // The host answers with the internal name; one result frame travels per
    // live route, so both numeric ids are bound to the one wire call.
    let mut transport = CursorTransport::new(session.clone(), out_tx);
    transport
        .send(vec![HostToolResult {
            call: CallId::new("call-9"),
            name: MAPPED_INTERNAL.into(),
            is_error: false,
            parts: vec![Part::Text {
                text: "result-text".into(),
            }],
        }])
        .unwrap();
    let mut ids = Vec::new();
    for _ in 0..2 {
        let frame = out_rx.next().await.unwrap().unwrap();
        let client = AgentClientMessage::decode(&frame[5..]).unwrap();
        match client.message {
            Some(client_message::Body::ExecClientMessage(exec)) => {
                ids.push(exec.id);
                assert!(matches!(
                    exec.message,
                    Some(exec_client_message::Body::McpResult(_))
                ));
            }
            other => panic!("wrong reply: {other:?}"),
        }
    }
    ids.sort_unstable();
    assert_eq!(ids, vec![3, 4]);
    drop(stream);
    drop(transport);
}

#[tokio::test]
async fn digest_folds_internal_names() {
    let session = Arc::new(Mutex::new(mapped_session()));
    let events = pump_frames(
        session.clone(),
        vec![proto_frame(&exec_mcp_message(3, "call-9", MAPPED_WIRE))],
    )
    .await;
    let args_text = match events.iter().find_map(|event| match event {
        Ok(StreamEvent::ToolCallsDone { calls }) => Some(calls),
        _ => None,
    }) {
        Some(calls) => match &calls[0].args {
            ToolArgs::Parsed(raw) => raw.as_str().to_owned(),
            ToolArgs::Invalid { message } => message.clone(),
            ToolArgs::Truncated => String::from("truncated"),
        },
        None => panic!("no ToolCallsDone in {events:?}"),
    };
    let hex = session.lock().unwrap().digest_hex();
    let mut internal_digest = TurnDigest::new();
    internal_digest.assistant_tool_call("call-9", MAPPED_INTERNAL, &args_text);
    assert_eq!(hex, internal_digest.hex());
    let mut wire_digest = TurnDigest::new();
    wire_digest.assistant_tool_call("call-9", MAPPED_WIRE, &args_text);
    assert_ne!(hex, wire_digest.hex());
}

#[test]
fn a_repeated_call_id_in_one_entry_is_issued_once() {
    let source = || ReplaySource {
        family: Family::Cursor,
        model: "cursor-test-model".into(),
    };
    let dup = || AssistantPart::ToolCall {
        call: CallId::new("c.dup"),
        name: "fetch_data".into(),
        args: RawJson::parse(r#"{"q":1}"#).unwrap(),
        replay: None,
    };
    let say = |text: &str| AssistantPart::Text {
        text: text.into(),
        replay: None,
    };
    let result = |text: &str| ContextItem::ToolResult {
        call: CallId::new("c.dup"),
        name: "fetch_data".into(),
        is_error: false,
        parts: vec![Part::Text { text: text.into() }],
    };
    let context = vec![
        ContextItem::User {
            parts: vec![Part::Text {
                text: "q-5b2c".into(),
            }],
        },
        ContextItem::Assistant {
            source: source(),
            parts: vec![dup(), say("mid-5b2c"), dup(), say("end-5b2c")],
            replay: None,
        },
        result("stale-5b2c"),
        result("fresh-5b2c"),
        ContextItem::User {
            parts: vec![Part::Text {
                text: "active-5b2c".into(),
            }],
        },
    ];
    let lowered = build_run(
        &model_request(context),
        &WireModel::plain("cursor-test-model"),
        "conv-dup-1",
        "msg-dup-1",
    )
    .unwrap();
    let texts = decoded_root_texts(&lowered.first_frame, &lowered.blobs);
    let tool_calls: usize = texts
        .iter()
        .map(|record| record.matches("\"type\":\"tool-call\"").count())
        .sum();
    assert_eq!(tool_calls, 1, "the repeat is dropped: {texts:?}");
    assert_eq!(
        count_containing(&texts, "\"role\":\"assistant\""),
        2,
        "the repeat does not open a round: {texts:?}"
    );
    assert_eq!(count_containing(&texts, "fresh-5b2c"), 1, "{texts:?}");
    assert_eq!(count_containing(&texts, "stale-5b2c"), 0, "{texts:?}");
}

#[test]
fn history_calls_carry_the_advertised_provider_identifier() {
    let lowered = build_run(
        &model_request(history_context()),
        &WireModel::plain("cursor-test-model"),
        "conv-ident-1",
        "msg-ident-1",
    )
    .unwrap();
    assert_eq!(lowered.tools.len(), 1);
    let advertised = lowered.tools[0].provider_identifier.as_str();
    assert_ne!(advertised, "");
    let run = run_request_of(&lowered.first_frame);
    let state = run.conversation_state.as_ref().unwrap();
    let mut identifiers = Vec::new();
    for turn_id in &state.turns {
        let turn =
            ConversationTurnStructure::decode(lowered.blobs.get(turn_id).unwrap().as_slice())
                .unwrap();
        let Some(conversation_turn::Turn::AgentConversationTurn(agent)) = turn.turn else {
            continue;
        };
        for step_id in &agent.steps {
            let step = ConversationStep::decode(lowered.blobs[step_id].as_slice()).unwrap();
            if let Some(conversation_step::Step::ToolCall(HistoryToolCall {
                tool: Some(history_tool_call::Tool::McpToolCall(call)),
                ..
            })) = step.message
            {
                identifiers.push(call.args.unwrap().provider_identifier);
            }
        }
    }
    assert_eq!(identifiers, vec![advertised.to_owned()]);
}

#[test]
fn paired_results_keep_their_text_and_only_orphans_name_themselves() {
    let call = |id: &str| AssistantPart::ToolCall {
        call: CallId::new(id),
        name: "fetch_data".into(),
        args: RawJson::parse("{}").unwrap(),
        replay: None,
    };
    let result = |id: &str, is_error: bool, parts: Vec<Part>| ContextItem::ToolResult {
        call: CallId::new(id),
        name: "fetch_data".into(),
        is_error,
        parts,
    };
    let context = vec![
        ContextItem::User {
            parts: vec![Part::Text {
                text: "q-8c41".into(),
            }],
        },
        ContextItem::Assistant {
            source: ReplaySource {
                family: Family::Cursor,
                model: "cursor-test-model".into(),
            },
            parts: vec![call("c.a"), call("c.b"), call("c.i")],
            replay: None,
        },
        result("c.a", false, vec![Part::Text { text: "".into() }]),
        result(
            "c.b",
            true,
            vec![Part::Text {
                text: "boom-8c41".into(),
            }],
        ),
        result(
            "c.i",
            false,
            vec![Part::Image {
                mime: "image/png".into(),
                bytes: b"\x89PNG".as_slice().into(),
            }],
        ),
        result("c.z", false, vec![Part::Text { text: "".into() }]),
        ContextItem::User {
            parts: vec![Part::Text {
                text: "active-8c41".into(),
            }],
        },
    ];
    let lowered = build_run(
        &model_request(context),
        &WireModel::plain("cursor-test-model"),
        "conv-result-1",
        "msg-result-1",
    )
    .unwrap();
    let texts = decoded_root_texts(&lowered.first_frame, &lowered.blobs);
    let tool_record = |id: &str| {
        let needle = format!("\"id\":\"{id}\"");
        let found: Vec<&String> = texts
            .iter()
            .filter(|text| text.contains("\"role\":\"tool\"") && text.contains(&needle))
            .collect();
        assert_eq!(found.len(), 1, "{id}: {texts:?}");
        found[0].clone()
    };
    let empty = tool_record("c_a");
    assert!(empty.contains("\"result\":\"\""), "{empty}");
    assert!(!empty.contains("isError"), "{empty}");
    let failed = tool_record("c_b");
    assert!(failed.contains("\"isError\":true"), "{failed}");
    let image = tool_record("c_i");
    assert!(image.contains("\"result\":\"[image/png image]\""), "{image}");
    // Only the orphan result (no call issued it) names itself.
    assert_eq!(count_containing(&texts, "(empty result)"), 1, "{texts:?}");
}

const USER_RECORD: &str =
    r#"{"role":"user","content":[{"type":"text","text":"hello"}],"requestId":"req-1"}"#;
const ANSWER_RECORD: &str = r#"{"role":"assistant","content":[{"type":"text","text":"answer"}]}"#;

fn kv_set_message(id: u32, blob_id: &[u8], data: &str) -> AgentServerMessage {
    AgentServerMessage {
        message: Some(server_message::Body::KvServerMessage(KvServerMessage {
            id,
            message: Some(kv_server_message::Body::SetBlob(SetBlobArgs {
                blob_id: blob_id.to_vec(),
                blob_data: data.as_bytes().to_vec(),
            })),
        })),
    }
}

fn checkpoint_message(roots: &[&[u8]]) -> AgentServerMessage {
    AgentServerMessage {
        message: Some(server_message::Body::Checkpoint(ConversationStateStructure {
            root_prompt_messages_json: roots.iter().map(|root| root.to_vec()).collect(),
            turns: Vec::new(),
        })),
    }
}

fn update_message(update: interaction_update::Update) -> AgentServerMessage {
    AgentServerMessage {
        message: Some(server_message::Body::InteractionUpdate(InteractionUpdate {
            update: Some(update),
        })),
    }
}

/// The two server records of the turn, stored ahead of any checkpoint.
fn record_frames() -> Vec<Vec<u8>> {
    vec![
        proto_frame(&kv_set_message(1, b"u", USER_RECORD)),
        proto_frame(&kv_set_message(2, b"a", ANSWER_RECORD)),
    ]
}

fn covering_checkpoint_frame() -> Vec<u8> {
    proto_frame(&checkpoint_message(&[b"u", b"a"]))
}

fn captured_payload(events: &[StreamEvent]) -> Option<&RawJson> {
    events.iter().find_map(|event| match event {
        StreamEvent::MessageReplay {
            capture: ReplayCapture::Captured { payload },
        } => Some(payload),
        _ => None,
    })
}

fn refused_reason(events: &[StreamEvent]) -> Option<ReplayRefusal> {
    events.iter().find_map(|event| match event {
        StreamEvent::MessageReplay {
            capture: ReplayCapture::Refused { reason },
        } => Some(*reason),
        _ => None,
    })
}

fn feed_shared(
    session: &Arc<Mutex<Session>>,
    chunks: &[Vec<u8>],
) -> (Vec<StreamEvent>, Vec<Vec<u8>>, Option<ProviderError>) {
    let mut guard = session.lock().unwrap();
    feed(&mut guard, chunks)
}

#[test]
fn a_covering_final_checkpoint_captures_the_turns_records() {
    let mut session = test_session();
    let mut frames = record_frames();
    frames.push(proto_frame(&text_message("answer")));
    frames.push(covering_checkpoint_frame());
    frames.push(proto_frame(&turn_ended_message(10, 5)));
    let (events, _, failure) = feed(&mut session, &frames);
    assert!(failure.is_none());
    assert!(matches!(
        events.as_slice(),
        [
            StreamEvent::TextDelta { .. },
            StreamEvent::ToolCallsDone { .. },
            StreamEvent::MessageReplay { .. },
            StreamEvent::Usage { .. },
            StreamEvent::Stop { .. },
        ]
    ));
    let payload = captured_payload(&events).expect("the covered turn is captured");
    let history: CursorHistory = sonic_rs::from_str(payload.as_str()).unwrap();
    assert_eq!(history.records, vec![ANSWER_RECORD.to_owned()]);
    assert_eq!(history.route.model, "cursor-test-model");
    let mut digest = TurnDigest::new();
    digest.assistant_text("answer");
    assert_eq!(history.digest, digest.hex());
}

#[test]
fn content_after_the_checkpoint_voids_the_capture() {
    let mut session = test_session();
    let mut frames = record_frames();
    frames.push(covering_checkpoint_frame());
    frames.push(proto_frame(&text_message("late")));
    frames.push(proto_frame(&turn_ended_message(10, 5)));
    let (events, _, failure) = feed(&mut session, &frames);
    assert!(failure.is_none());
    assert!(
        captured_payload(&events).is_none(),
        "records taken before the closing text cannot stand for the turn: {events:?}"
    );
}

#[test]
fn keepalives_and_token_estimates_keep_the_checkpoint_covering() {
    let mut session = test_session();
    let mut frames = record_frames();
    frames.push(proto_frame(&text_message("answer")));
    frames.push(covering_checkpoint_frame());
    frames.push(proto_frame(&update_message(
        interaction_update::Update::Heartbeat(HeartbeatUpdate {}),
    )));
    frames.push(proto_frame(&update_message(
        interaction_update::Update::TokenDelta(TokenDeltaUpdate { tokens: 3 }),
    )));
    frames.push(proto_frame(&turn_ended_message(10, 5)));
    let (events, _, failure) = feed(&mut session, &frames);
    assert!(failure.is_none());
    assert!(captured_payload(&events).is_some(), "{events:?}");
}

#[test]
fn an_unmodeled_update_after_the_checkpoint_voids_the_capture() {
    // An update with field 9 (a summary) decodes into nothing: it is progress
    // the records may not hold, not a keepalive.
    let summary = frame_of(&[0x0A, 0x02, 0x4A, 0x00], 0);
    let mut session = test_session();
    let mut frames = record_frames();
    frames.push(proto_frame(&text_message("answer")));
    frames.push(covering_checkpoint_frame());
    frames.push(summary);
    frames.push(proto_frame(&turn_ended_message(10, 5)));
    let (events, _, failure) = feed(&mut session, &frames);
    assert!(failure.is_none());
    assert!(captured_payload(&events).is_none(), "{events:?}");
}

/// A call answered after a covering checkpoint, then the turn end, with or
/// without a later checkpoint.
fn run_with_a_result_delivered_after_the_checkpoint(final_checkpoint: bool) -> Vec<StreamEvent> {
    let session = Arc::new(Mutex::new(test_session()));
    let (out_tx, _out_rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    let mut transport = CursorTransport::new(session.clone(), out_tx);
    let mut frames = record_frames();
    frames.push(proto_frame(&exec_mcp_message(3, "call-9", "fetch_data")));
    frames.push(covering_checkpoint_frame());
    let (_, _, failure) = feed_shared(&session, &frames);
    assert!(failure.is_none());
    transport
        .send(vec![HostToolResult {
            call: CallId::new("call-9"),
            name: "fetch_data".into(),
            is_error: false,
            parts: vec![Part::Text {
                text: "result".into(),
            }],
        }])
        .unwrap();
    let mut tail = Vec::new();
    if final_checkpoint {
        tail.push(covering_checkpoint_frame());
    }
    tail.push(proto_frame(&turn_ended_message(10, 5)));
    let (events, _, failure) = feed_shared(&session, &tail);
    assert!(failure.is_none());
    events
}

#[test]
fn a_delivered_result_voids_the_checkpoint_taken_before_it() {
    let stale = run_with_a_result_delivered_after_the_checkpoint(false);
    assert!(
        captured_payload(&stale).is_none(),
        "a checkpoint that predates the result cannot stand for the turn: {stale:?}"
    );
    let covered = run_with_a_result_delivered_after_the_checkpoint(true);
    assert!(captured_payload(&covered).is_some(), "{covered:?}");
}

#[test]
fn a_new_step_after_the_final_step_makes_a_cut_stream_a_cut() {
    let end_stream = || frame_of(b"{}", END_STREAM_FLAG);
    let mut session = test_session();
    let (_, _, failure) = feed(
        &mut session,
        &[
            proto_frame(&text_message("done")),
            proto_frame(&step_completed_message(7)),
            proto_frame(&update_message(interaction_update::Update::StepStarted(
                StepStartedUpdate { step_id: 8 },
            ))),
            end_stream(),
        ],
    );
    assert!(matches!(failure, Some(ProviderError::StreamCut)));

    // A keepalive between the step end and the clean end changes nothing.
    let mut session = test_session();
    let (events, _, failure) = feed(
        &mut session,
        &[
            proto_frame(&text_message("done")),
            proto_frame(&step_completed_message(7)),
            proto_frame(&update_message(interaction_update::Update::Heartbeat(
                HeartbeatUpdate {},
            ))),
            end_stream(),
        ],
    );
    assert!(failure.is_none());
    assert!(matches!(
        events.last(),
        Some(StreamEvent::Stop {
            reason: StopReason::EndTurn,
        })
    ));
}

#[test]
fn frames_ahead_of_a_framing_failure_are_still_handled() {
    let mut chunk = proto_frame(&text_message("kept"));
    chunk.push(0);
    chunk.extend_from_slice(&u32::try_from(MAX_FRAME_BYTES + 1).unwrap().to_be_bytes());
    let mut session = test_session();
    let (events, _, failure) = feed(&mut session, &[chunk]);
    assert!(matches!(
        events.as_slice(),
        [StreamEvent::TextDelta { text }] if text == "kept"
    ));
    assert!(matches!(failure, Some(ProviderError::Limit(_))));
}

#[test]
fn an_undecodable_cursor_detail_does_not_hide_a_later_one() {
    let broken = |value: Option<&str>| super::frame::ConnectDetail {
        detail_type: Some(String::from("aiserver.v1.ErrorDetails")),
        value: value.map(str::to_owned),
        debug: None,
    };
    for first in [
        broken(None),
        broken(Some("")),
        broken(Some("!!!not-base64!!!")),
        broken(Some("AAAA")),
    ] {
        let error = map_connect_error(
            "cursor-test",
            &super::frame::ConnectError {
                code: String::from("error"),
                message: String::from("m"),
                details: vec![first, structured_detail(8, None)],
            },
        );
        assert!(
            matches!(error, ProviderError::RateLimited { .. }),
            "{error:?}"
        );
    }
}

#[test]
fn structured_codes_follow_the_reference_status_table() {
    let map = |code: i32| {
        map_connect_error(
            "cursor-test",
            &super::frame::ConnectError {
                code: String::from("error"),
                message: String::from("m"),
                details: vec![structured_detail(code, None)],
            },
        )
    };
    for code in [28, 35] {
        let error = map(code);
        assert!(
            matches!(error, ProviderError::RateLimited { .. }),
            "code {code}: {error:?}"
        );
    }
    for code in [4, 23, 46, 47, 48, 49, 54, 58, 64] {
        let error = map(code);
        assert!(
            matches!(error, ProviderError::Status { status: 403, .. }),
            "code {code}: {error:?}"
        );
    }
}

#[test]
fn the_context_handshake_declares_hosted_web_search_off() {
    let mut session = test_session();
    let request = AgentServerMessage {
        message: Some(server_message::Body::ExecServerMessage(ExecServerMessage {
            id: 12,
            exec_id: String::new(),
            body: Some(exec_server_message::Body::RequestContextArgs(
                RequestContextArgs {},
            )),
        })),
    };
    let (events, writes, failure) = feed(&mut session, &[proto_frame(&request)]);
    assert_eq!(events, []);
    assert!(failure.is_none());
    assert_eq!(writes.len(), 1);
    let client = AgentClientMessage::decode(&writes[0][5..]).unwrap();
    let exec = match client.message {
        Some(client_message::Body::ExecClientMessage(exec)) => exec,
        other => panic!("wrong reply: {other:?}"),
    };
    assert_eq!(exec.id, 12);
    let result = match exec.message {
        Some(exec_client_message::Body::RequestContextResult(result)) => result,
        other => panic!("wrong body: {other:?}"),
    };
    let Some(request_context_result::Result::Success(success)) = result.result else {
        panic!("the handshake carries no result");
    };
    assert_eq!(
        success.request_context.unwrap().web_search_enabled,
        Some(false)
    );
}

#[test]
fn normalized_collision_keeps_both_calls_and_results() {
    let call = |id: &str| AssistantPart::ToolCall {
        call: CallId::new(id),
        name: "fetch_data".into(),
        args: RawJson::parse("{}").unwrap(),
        replay: None,
    };
    let result = |id: &str, text: &str| ContextItem::ToolResult {
        call: CallId::new(id),
        name: "fetch_data".into(),
        is_error: false,
        parts: vec![Part::Text {
            text: text.into(),
        }],
    };
    let context = vec![
        ContextItem::User {
            parts: vec![Part::Text {
                text: "q-collide".into(),
            }],
        },
        ContextItem::Assistant {
            source: ReplaySource {
                family: Family::Cursor,
                model: "cursor-test-model".into(),
            },
            parts: vec![call("a.b"), call("a_b")],
            replay: None,
        },
        result("a.b", "first-collide"),
        result("a_b", "second-collide"),
        ContextItem::User {
            parts: vec![Part::Text {
                text: "active-collide".into(),
            }],
        },
    ];
    let lowered = build_run(
        &model_request(context),
        &WireModel::plain("cursor-test-model"),
        "conv-collide-1",
        "msg-collide-1",
    )
    .unwrap();
    let texts = decoded_root_texts(&lowered.first_frame, &lowered.blobs);
    assert_eq!(
        texts
            .iter()
            .map(|record| record.matches("\"type\":\"tool-call\"").count())
            .sum::<usize>(),
        2,
        "both colliding calls are kept: {texts:?}"
    );
    assert!(
        texts.iter().any(|text| text.contains("first-collide")),
        "{texts:?}"
    );
    assert!(
        texts.iter().any(|text| text.contains("second-collide")),
        "{texts:?}"
    );
    assert!(
        texts.iter().any(|text| text.contains("\"toolCallId\":\"a_b\"")),
        "{texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|text| text.contains("\"toolCallId\":\"a_b_2\"")),
        "the collision is suffixed, not dropped: {texts:?}"
    );
}

#[test]
fn initial_request_declares_hosted_web_search_off() {
    let lowered = build_run(
        &model_request(history_context()),
        &WireModel::plain("cursor-test-model"),
        "conv-search-1",
        "msg-search-1",
    )
    .unwrap();
    let run = run_request_of(&lowered.first_frame);
    let action = run.action.as_ref().unwrap().action.as_ref().unwrap();
    match action {
        conversation_action::Action::UserMessageAction(action) => {
            assert_eq!(
                action
                    .request_context
                    .as_ref()
                    .unwrap()
                    .web_search_enabled,
                Some(false),
                "the opener opts out exactly like the handshake"
            );
        }
        other => panic!("wrong action: {other:?}"),
    }
}

#[test]
fn foreign_provider_identifier_is_not_dispatched() {
    fn mcp_frame(id: u32, provider: &str, server: &str) -> AgentServerMessage {
        AgentServerMessage {
            message: Some(server_message::Body::ExecServerMessage(ExecServerMessage {
                id,
                exec_id: String::new(),
                body: Some(exec_server_message::Body::McpArgs(McpArgs {
                    name: String::from("fetch_data"),
                    args: BTreeMap::new(),
                    tool_call_id: String::from("call-f4"),
                    tool_name: String::from("fetch_data"),
                    provider_identifier: provider.to_owned(),
                    smart_mode_approval_only: false,
                    skip_approval: false,
                    server_identifier: server.to_owned(),
                })),
            })),
        }
    }
    // Empty (legacy omission) and the advertised identifier dispatch.
    for provider in ["", "dal-agent"] {
        let mut session = test_session();
        let (events, writes, failure) = feed(&mut session, &[proto_frame(&mcp_frame(21, provider, ""))]);
        assert!(failure.is_none(), "{provider}: {failure:?}");
        assert_eq!(events.len(), 2, "{provider}: {events:?}");
        assert!(writes.is_empty(), "{provider}: {writes:?}");
    }
    // A non-empty foreign provider is not ours: `ToolNotFound`, no events.
    let mut session = test_session();
    let (events, writes, failure) =
        feed(&mut session, &[proto_frame(&mcp_frame(22, "foreign-provider", ""))]);
    assert!(failure.is_none(), "{failure:?}");
    assert_eq!(events, []);
    assert_eq!(writes.len(), 1);
    let client = AgentClientMessage::decode(&writes[0][5..]).unwrap();
    match client.message {
        Some(client_message::Body::ExecClientMessage(exec)) => match exec.message {
            Some(exec_client_message::Body::McpResult(result)) => {
                assert!(
                    matches!(result.result, Some(mcp_result::Result::ToolNotFound(_))),
                    "{result:?}"
                );
            }
            other => panic!("wrong body: {other:?}"),
        },
        other => panic!("wrong reply: {other:?}"),
    }
    // `server_identifier` carries no declaration and is ignored.
    let mut session = test_session();
    let (events, _, failure) =
        feed(&mut session, &[proto_frame(&mcp_frame(23, "", "any-server"))]);
    assert!(failure.is_none(), "{failure:?}");
    assert_eq!(events.len(), 2, "{events:?}");
}

#[test]
fn urlsafe_detail_maps_structured() {
    let detail = ErrorDetails {
        error: 8,
        details: Some(CustomErrorDetails {
            title: String::from("t"),
            detail: String::from("d"),
            is_retryable: None,
        }),
    };
    let raw = detail.encode_to_vec();
    let urlsafe = base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE, &raw);
    let error = map_connect_error(
        "cursor-test",
        &super::frame::ConnectError {
            code: String::from("error"),
            message: String::from("m"),
            details: vec![super::frame::ConnectDetail {
                detail_type: Some(String::from("aiserver.v1.ErrorDetails")),
                value: Some(urlsafe),
                debug: None,
            }],
        },
    );
    assert!(
        matches!(error, ProviderError::RateLimited { .. }),
        "URL-safe alphabet decodes like the reference Buffer: {error:?}"
    );
}

#[test]
fn unmapped_codes_follow_reference_defaults() {
    let map = |code: i32| {
        map_connect_error(
            "cursor-test",
            &super::frame::ConnectError {
                code: String::from("error"),
                message: String::from("m"),
                details: vec![structured_detail(code, None)],
            },
        )
    };
    // Reference `CURSOR_ERROR_STATUS` has no row for these; the reference
    // falls through to 400, which this client reports as `InvalidRequest`.
    for code in [31, 37, 52, 53, 62] {
        assert!(
            matches!(map(code), ProviderError::InvalidRequest { .. }),
            "code {code}: {:?}",
            map(code)
        );
    }
    // Reference maps 65 to 401; this client reports `AuthRejected`.
    assert!(
        matches!(map(65), ProviderError::AuthRejected { .. }),
        "{:?}",
        map(65)
    );
}

#[test]
fn token_estimate_after_final_step_cuts() {
    let end_stream = || frame_of(b"{}", END_STREAM_FLAG);
    let mut session = test_session();
    let (_, _, failure) = feed(
        &mut session,
        &[
            proto_frame(&text_message("done")),
            proto_frame(&step_completed_message(7)),
            proto_frame(&update_message(interaction_update::Update::TokenDelta(
                TokenDeltaUpdate { tokens: 3 },
            ))),
            end_stream(),
        ],
    );
    assert!(
        matches!(failure, Some(ProviderError::StreamCut)),
        "a running estimate ends the closed-step marker, as in the reference: {failure:?}"
    );
}

#[test]
fn malformed_present_replay_is_refused_not_rebuilt_silently() {
    // Truly absent (stale checkpoint) yields no MessageReplay at all; the
    // turn rebuilds. Each malformed present case below yields a refused
    // capture so the fold fails instead of rebuilding.
    let stale = run_with_a_result_delivered_after_the_checkpoint(false);
    assert!(captured_payload(&stale).is_none());
    assert!(refused_reason(&stale).is_none());

    // Non-UTF8 bytes under a covering checkpoint.
    let bad_msg = AgentServerMessage {
        message: Some(server_message::Body::KvServerMessage(KvServerMessage {
            id: 2,
            message: Some(kv_server_message::Body::SetBlob(SetBlobArgs {
                blob_id: b"bad".to_vec(),
                blob_data: vec![0xff, 0xfe],
            })),
        })),
    };
    let mut session = test_session();
    let (events, _, failure) = feed(
        &mut session,
        &[
            proto_frame(&kv_set_message(1, b"u", USER_RECORD)),
            proto_frame(&bad_msg),
            proto_frame(&text_message("answer")),
            proto_frame(&checkpoint_message(&[b"u", b"bad"])),
            proto_frame(&turn_ended_message(10, 5)),
        ],
    );
    assert!(failure.is_none(), "{failure:?}");
    assert_eq!(
        refused_reason(&events),
        Some(ReplayRefusal::RecordNotUtf8),
        "{events:?}"
    );

    // Non-JSON record.
    let mut session = test_session();
    let (events, _, failure) = feed(
        &mut session,
        &[
            proto_frame(&kv_set_message(1, b"u", USER_RECORD)),
            proto_frame(&kv_set_message(2, b"j", "not json")),
            proto_frame(&text_message("answer")),
            proto_frame(&checkpoint_message(&[b"u", b"j"])),
            proto_frame(&turn_ended_message(10, 5)),
        ],
    );
    assert!(failure.is_none(), "{failure:?}");
    assert_eq!(
        refused_reason(&events),
        Some(ReplayRefusal::RecordNotJson),
        "{events:?}"
    );

    // No user record for this request.
    let mut session = test_session();
    let (events, _, failure) = feed(
        &mut session,
        &[
            proto_frame(&kv_set_message(2, b"a", ANSWER_RECORD)),
            proto_frame(&text_message("answer")),
            proto_frame(&checkpoint_message(&[b"a"])),
            proto_frame(&turn_ended_message(10, 5)),
        ],
    );
    assert!(failure.is_none(), "{failure:?}");
    assert_eq!(
        refused_reason(&events),
        Some(ReplayRefusal::UserRecordNotFound),
        "{events:?}"
    );

    // User record with no answer records after it.
    let mut session = test_session();
    let (events, _, failure) = feed(
        &mut session,
        &[
            proto_frame(&kv_set_message(1, b"u", USER_RECORD)),
            proto_frame(&text_message("answer")),
            proto_frame(&checkpoint_message(&[b"u"])),
            proto_frame(&turn_ended_message(10, 5)),
        ],
    );
    assert!(failure.is_none(), "{failure:?}");
    assert_eq!(
        refused_reason(&events),
        Some(ReplayRefusal::NoRecords),
        "{events:?}"
    );
}

#[test]
fn replay_cover_uses_stored_wire_ids_verbatim() {
    // Stored `calls` are wire spellings: the capture normalizes before it
    // stores (`finish_step_calls`), so the cover must not route them through
    // the internal-to-wire table again. With colliding internals (`a.b`
    // takes `a_b` first, `a_b` becomes `a_b_2`), a table lookup of the
    // stored `a_b` returns `a_b_2`, leaving the covered `a.b` result to be
    // re-emitted as a duplicate.
    let call = |id: &str| AssistantPart::ToolCall {
        call: CallId::new(id),
        name: "fetch_data".into(),
        args: RawJson::parse("{}").unwrap(),
        replay: None,
    };
    let result = |id: &str, text: &str| ContextItem::ToolResult {
        call: CallId::new(id),
        name: "fetch_data".into(),
        is_error: false,
        parts: vec![Part::Text {
            text: text.into(),
        }],
    };
    let history = vec![
        ContextItem::User {
            parts: vec![Part::Text {
                text: "q-cover".into(),
            }],
        },
        ContextItem::Assistant {
            source: ReplaySource {
                family: Family::Cursor,
                model: "cursor-test-model".into(),
            },
            parts: vec![call("a.b"), call("a_b")],
            replay: None,
        },
        result("a.b", "first-cover"),
        result("a_b", "second-cover"),
    ];
    let mut wire_ids = WireIds::new();
    for item in &history {
        if let ContextItem::Assistant { parts, .. } = item {
            for part in parts {
                if let AssistantPart::ToolCall { call, .. } = part {
                    wire_ids.wire(call.as_str());
                }
            }
        }
    }
    let stored = CursorHistory::new(
        &test_route(),
        String::from("cover-digest"),
        vec![String::from("a_b"), String::from("a_b_2")],
        vec![String::from("{\"stored\":\"cover-marker\"}")],
    );
    let replays = vec![SegmentReplay {
        user: 0,
        assistant: 1,
        history: stored,
    }];
    let mut store = BlobStore::new();
    let mut root: Vec<Vec<u8>> = Vec::new();
    lower_root_entries(
        &history,
        "cursor-test-model",
        &replays,
        &wire_ids,
        &mut store,
        &mut root,
    )
    .unwrap();
    let texts: Vec<String> = store
        .into_map()
        .into_values()
        .map(|bytes| String::from_utf8(bytes).unwrap())
        .collect();
    assert!(
        texts.iter().any(|text| text.contains("cover-marker")),
        "{texts:?}"
    );
    assert!(
        !texts.iter().any(|text| text.contains("first-cover")),
        "covered colliding results are not re-emitted: {texts:?}"
    );
    assert!(
        !texts.iter().any(|text| text.contains("second-cover")),
        "covered colliding results are not re-emitted: {texts:?}"
    );
}
