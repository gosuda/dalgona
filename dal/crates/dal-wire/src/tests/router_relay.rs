use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use dal_core::{Family, RawJson, Usage};
use dal_provider::{
    EventStream, ProviderError, ReplayPayload, StopReason, StreamEvent, ToolArgs, ToolCall,
};
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::router::canonical_json;
use crate::router::pass::{Relay, RelayMapper};
use crate::router::sink::{SseEncoder, StreamSink};
use crate::router::stream::{ChatEncoder, MessagesEncoder, ResponsesEncoder, TurnIds};

fn ids() -> TurnIds {
    TurnIds {
        chat: "chatcmpl-dal-t".to_owned(),
        responses: "resp_t".to_owned(),
        messages: "msg_dal_t".to_owned(),
    }
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

fn args(id: &str, fragment: &[u8]) -> StreamEvent {
    StreamEvent::ToolArgsDelta {
        id: id.to_owned(),
        fragment: fragment.to_vec(),
    }
}

fn started(id: &str, name: &str) -> StreamEvent {
    StreamEvent::ToolCallStarted {
        id: id.to_owned(),
        name: name.to_owned(),
    }
}

fn done(id: &str, name: &str, json: &str) -> StreamEvent {
    StreamEvent::ToolCallsDone {
        calls: vec![ToolCall {
            id: id.to_owned(),
            name: name.to_owned(),
            args: ToolArgs::Parsed(RawJson::parse(json).expect("arguments are JSON")),
        }],
    }
}

fn text(delta: &str) -> StreamEvent {
    StreamEvent::TextDelta {
        text: delta.to_owned(),
    }
}

fn replay(family: Family, item: &str) -> StreamEvent {
    StreamEvent::Replay {
        payload: ReplayPayload {
            family,
            model: "m".into(),
            item: RawJson::parse(item).expect("replay item is JSON"),
        },
    }
}

fn scripted(events: Vec<Result<StreamEvent, ProviderError>>) -> EventStream {
    EventStream::new(futures::stream::iter(events), || {})
}

/// Pumps a scripted relay through one encoder and returns every SSE line.
async fn pump<E: SseEncoder + 'static>(
    events: Vec<Result<StreamEvent, ProviderError>>,
    route: Option<Family>,
    wire: Family,
    encoder: E,
) -> Vec<String> {
    let shutdown = CancellationToken::new();
    let relay = Relay::start(scripted(events), RelayMapper::new(route, wire), &shutdown)
        .await
        .expect("the relay starts");
    let (tx, mut rx) = mpsc::channel(256);
    let mut sink = StreamSink::new(encoder, tx);
    relay.pump(ids(), &shutdown, &mut sink).await;
    drop(sink);
    let mut lines = Vec::new();
    while let Some(Some(line)) = rx.recv().await {
        lines.push(line);
    }
    lines
}

/// One SSE data line carrying `json` with byte-sorted keys.
fn data(json: &str) -> String {
    let value: sonic_rs::Value = sonic_rs::from_str(json).expect("expected payload is JSON");
    format!("data: {}\n\n", canonical_json(&value))
}

fn chunk(delta: &str, reason: &str) -> String {
    data(&format!(
        r#"{{"id":"chatcmpl-dal-t","object":"chat.completion.chunk","created":0,"model":"openai-chat/gpt","choices":[{{"index":0,"delta":{delta},"finish_reason":{reason}}}]}}"#
    ))
}

#[tokio::test]
async fn chat_relay_streams_text_tool_chunks_finish_usage_and_done() {
    let events = vec![
        Ok(text("Hel")),
        Ok(text("lo")),
        Ok(started("call_1", "read")),
        Ok(args("call_1", br#"{"pa"#)),
        Ok(args("call_1", br#"th":"a"}"#)),
        Ok(done("call_1", "read", r#"{"path":"a"}"#)),
        Ok(usage(10, 5)),
        Ok(StreamEvent::Stop {
            reason: StopReason::ToolUse,
        }),
    ];
    let lines = pump(
        events,
        Some(Family::Chat),
        Family::Chat,
        ChatEncoder::new("openai-chat/gpt", true),
    )
    .await;
    let expected = vec![
        chunk(r#"{"role":"assistant"}"#, "null"),
        chunk(r#"{"content":"Hel"}"#, "null"),
        chunk(r#"{"content":"lo"}"#, "null"),
        chunk(
            r#"{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read","arguments":""}}]}"#,
            "null",
        ),
        chunk(
            r#"{"tool_calls":[{"index":0,"function":{"arguments":"{\"pa"}}]}"#,
            "null",
        ),
        chunk(
            r#"{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"a\"}"}}]}"#,
            "null",
        ),
        chunk("{}", r#""tool_calls""#),
        data(
            r#"{"id":"chatcmpl-dal-t","object":"chat.completion.chunk","created":0,"model":"openai-chat/gpt","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#,
        ),
        "data: [DONE]\n\n".to_owned(),
    ];
    assert_eq!(lines, expected);
}

#[tokio::test]
async fn chat_relay_failure_after_start_is_a_stream_error() {
    let events = vec![
        Ok(text("Hi")),
        Err(ProviderError::Status {
            family: Family::Chat,
            status: 500,
            message: "boom".to_owned(),
        }),
    ];
    let lines = pump(
        events,
        Some(Family::Chat),
        Family::Chat,
        ChatEncoder::new("openai-chat/gpt", false),
    )
    .await;
    let expected = vec![
        chunk(r#"{"role":"assistant"}"#, "null"),
        chunk(r#"{"content":"Hi"}"#, "null"),
        data(
            r#"{"error":{"message":"upstream provider failed: boom","type":"server_error","param":null,"code":null}}"#,
        ),
        "data: [DONE]\n\n".to_owned(),
    ];
    assert_eq!(lines, expected);
}

fn event(name: &str, json: &str) -> String {
    format!("event: {name}\n{}", data(json))
}

#[tokio::test]
async fn responses_relay_streams_items_in_sequence() {
    let events = vec![
        Ok(StreamEvent::ReasoningDelta {
            text: "think".to_owned(),
        }),
        Ok(replay(
            Family::Responses,
            r#"{"type":"reasoning","id":"rs_1","summary":[]}"#,
        )),
        Ok(text("Hi")),
        Ok(started("call_1", "read")),
        Ok(args("call_1", br#"{"p":1}"#)),
        Ok(done("call_1", "read", r#"{"p":1}"#)),
        Ok(usage(3, 4)),
        Ok(StreamEvent::Stop {
            reason: StopReason::ToolUse,
        }),
    ];
    let lines = pump(
        events,
        Some(Family::Responses),
        Family::Responses,
        ResponsesEncoder::new("openai-responses/o3"),
    )
    .await;
    let item = |status: &str, arguments: &str| {
        format!(
            r#"{{"type":"function_call","id":"fc_0","call_id":"call_1","name":"read","arguments":"{arguments}","status":"{status}"}}"#
        )
    };
    let expected = vec![
        event(
            "response.created",
            r#"{"type":"response.created","sequence_number":1,"response":{"id":"resp_t","object":"response","status":"in_progress"}}"#,
        ),
        event(
            "response.in_progress",
            r#"{"type":"response.in_progress","sequence_number":2,"response":{"id":"resp_t","object":"response","status":"in_progress"}}"#,
        ),
        event(
            "response.reasoning.delta",
            r#"{"type":"response.reasoning.delta","sequence_number":3,"item_id":"reasoning_0","output_index":0,"content_index":0,"delta":"think"}"#,
        ),
        event(
            "response.output_text.delta",
            r#"{"type":"response.output_text.delta","sequence_number":4,"item_id":"msg_0","output_index":1,"content_index":0,"delta":"Hi"}"#,
        ),
        event(
            "response.output_item.added",
            &format!(
                r#"{{"type":"response.output_item.added","sequence_number":5,"output_index":2,"item":{}}}"#,
                item("in_progress", "")
            ),
        ),
        event(
            "response.function_call_arguments.delta",
            r#"{"type":"response.function_call_arguments.delta","sequence_number":6,"item_id":"fc_0","output_index":2,"delta":"{\"p\":1}"}"#,
        ),
        event(
            "response.function_call_arguments.done",
            r#"{"type":"response.function_call_arguments.done","sequence_number":7,"item_id":"fc_0","output_index":2,"arguments":"{\"p\":1}"}"#,
        ),
        event(
            "response.output_item.done",
            &format!(
                r#"{{"type":"response.output_item.done","sequence_number":8,"output_index":2,"item":{}}}"#,
                item("completed", r#"{\"p\":1}"#)
            ),
        ),
        event(
            "response.completed",
            &format!(
                r#"{{"type":"response.completed","sequence_number":9,"response":{{"id":"resp_t","object":"response","status":"completed","model":"openai-responses/o3","output":[{{"type":"reasoning","id":"rs_1","summary":[]}},{{"type":"message","id":"msg_0","role":"assistant","content":[{{"type":"output_text","text":"Hi"}}]}},{}],"usage":{{"input_tokens":3,"output_tokens":4,"total_tokens":7}}}}}}"#,
                item("completed", r#"{\"p\":1}"#)
            ),
        ),
    ];
    assert_eq!(lines, expected);
}

#[tokio::test]
async fn messages_relay_streams_blocks_and_joins_split_utf8_arguments() {
    let events = vec![
        Ok(replay(
            Family::Anthropic,
            r#"{"type":"thinking","thinking":"hm","signature":"s"}"#,
        )),
        Ok(text("Hi")),
        Ok(started("toolu_1", "find")),
        Ok(args("toolu_1", b"{\"q\":\"\xc3")),
        Ok(args("toolu_1", b"\xa9\"}")),
        Ok(done("toolu_1", "find", "{\"q\":\"\u{e9}\"}")),
        Ok(usage(2, 6)),
        Ok(StreamEvent::Stop {
            reason: StopReason::ToolUse,
        }),
    ];
    let lines = pump(
        events,
        Some(Family::Anthropic),
        Family::Anthropic,
        MessagesEncoder::new("anthropic/claude"),
    )
    .await;
    let expected = vec![
        event(
            "message_start",
            r#"{"type":"message_start","message":{"id":"msg_dal_t","type":"message","role":"assistant","model":"anthropic/claude","content":[],"stop_reason":null,"usage":{"input_tokens":0,"output_tokens":0}}}"#,
        ),
        event(
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"hm","signature":"s"}}"#,
        ),
        event(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        ),
        event(
            "content_block_start",
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
        ),
        event(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hi"}}"#,
        ),
        event(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":1}"#,
        ),
        event(
            "content_block_start",
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_1","name":"find","input":{}}}"#,
        ),
        event(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"q\":\""}}"#,
        ),
        event(
            "content_block_delta",
            "{\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\u{e9}\\\"}\"}}",
        ),
        event(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":2}"#,
        ),
        event(
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":6}}"#,
        ),
        event("message_stop", r#"{"type":"message_stop"}"#),
    ];
    assert_eq!(lines, expected);
}

#[tokio::test]
async fn cross_family_replay_is_dropped() {
    let events = vec![
        Ok(replay(
            Family::Anthropic,
            r#"{"type":"thinking","thinking":"hm"}"#,
        )),
        Ok(StreamEvent::Stop {
            reason: StopReason::EndTurn,
        }),
    ];
    let lines = pump(
        events,
        Some(Family::Anthropic),
        Family::Chat,
        ChatEncoder::new("anthropic/claude", false),
    )
    .await;
    assert!(lines.iter().all(|line| !line.contains("thinking")));
    assert_eq!(lines.last().map(String::as_str), Some("data: [DONE]\n\n"));
}

#[tokio::test]
async fn early_provider_status_keeps_its_http_status() {
    let shutdown = CancellationToken::new();
    let stream = scripted(vec![Err(ProviderError::Status {
        family: Family::Chat,
        status: 429,
        message: "slow down".to_owned(),
    })]);
    let Err(fail) = Relay::start(
        stream,
        RelayMapper::new(Some(Family::Chat), Family::Chat),
        &shutdown,
    )
    .await
    else {
        panic!("a 429 before the first event fails the request");
    };
    assert_eq!(
        (fail.status, fail.code, fail.message.as_str()),
        (429, "rate_limit", "slow down")
    );
}

/// A provider that sends one text delta, then never ends; the flag records
/// transport cancellation.
fn endless(cancelled: &Arc<AtomicBool>) -> EventStream {
    let flag = Arc::clone(cancelled);
    EventStream::new(
        futures::stream::iter(vec![Ok(text("Hi"))]).chain(futures::stream::pending()),
        move || flag.store(true, Ordering::SeqCst),
    )
}

#[tokio::test]
async fn client_disconnect_cancels_the_provider_stream() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let shutdown = CancellationToken::new();
    let relay = Relay::start(
        endless(&cancelled),
        RelayMapper::new(Some(Family::Chat), Family::Chat),
        &shutdown,
    )
    .await
    .expect("the relay starts");
    let (tx, mut rx) = mpsc::channel(256);
    let mut sink = StreamSink::new(ChatEncoder::new("openai-chat/gpt", false), tx);
    let reader = async move {
        let role = rx.recv().await.flatten();
        let content = rx.recv().await.flatten();
        drop(rx);
        (role, content)
    };
    let pumped = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(relay.pump(ids(), &shutdown, &mut sink), reader)
    })
    .await
    .expect("the pump ends once the client is gone");
    assert_eq!(pumped.1.1, Some(chunk(r#"{"content":"Hi"}"#, "null")));
    assert!(cancelled.load(Ordering::SeqCst));
}

#[tokio::test]
async fn listener_shutdown_cancels_the_relay_with_a_failure_event() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let shutdown = CancellationToken::new();
    let relay = Relay::start(
        endless(&cancelled),
        RelayMapper::new(Some(Family::Chat), Family::Chat),
        &shutdown,
    )
    .await
    .expect("the relay starts");
    let (tx, mut rx) = mpsc::channel(256);
    let mut sink = StreamSink::new(ChatEncoder::new("openai-chat/gpt", false), tx);
    let stopper = shutdown.clone();
    let reader = async move {
        let mut lines = Vec::new();
        lines.push(rx.recv().await.flatten());
        lines.push(rx.recv().await.flatten());
        stopper.cancel();
        while let Some(Some(line)) = rx.recv().await {
            lines.push(Some(line));
        }
        lines
    };
    let pump = async {
        relay.pump(ids(), &shutdown, &mut sink).await;
        drop(sink);
    };
    let ((), lines) =
        tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(pump, reader) })
            .await
            .expect("shutdown ends the pump");
    assert_eq!(
        lines.last().cloned().flatten().as_deref(),
        Some("data: [DONE]\n\n")
    );
    assert_eq!(
        lines.get(2).cloned().flatten().as_deref(),
        Some(
            data(r#"{"error":{"message":"the turn was cancelled","type":"server_error","param":null,"code":null}}"#)
                .as_str()
        )
    );
    assert!(cancelled.load(Ordering::SeqCst));
}

#[tokio::test]
async fn messages_relay_keeps_one_block_open_for_interleaved_calls() {
    let events = vec![
        Ok(started("call_a", "read")),
        Ok(started("call_b", "list")),
        Ok(args("call_a", br#"{"a":1}"#)),
        Ok(args("call_b", br#"{"b":2}"#)),
        Ok(StreamEvent::ToolCallsDone {
            calls: vec![
                ToolCall {
                    id: "call_a".to_owned(),
                    name: "read".to_owned(),
                    args: ToolArgs::Parsed(RawJson::parse(r#"{"a":1}"#).expect("JSON")),
                },
                ToolCall {
                    id: "call_b".to_owned(),
                    name: "list".to_owned(),
                    args: ToolArgs::Parsed(RawJson::parse(r#"{"b":2}"#).expect("JSON")),
                },
            ],
        }),
        Ok(usage(1, 2)),
        Ok(StreamEvent::Stop {
            reason: StopReason::ToolUse,
        }),
    ];
    let lines = pump(
        events,
        Some(Family::Chat),
        Family::Anthropic,
        MessagesEncoder::new("openai-chat/gpt"),
    )
    .await;
    let expected = vec![
        event(
            "message_start",
            r#"{"type":"message_start","message":{"id":"msg_dal_t","type":"message","role":"assistant","model":"openai-chat/gpt","content":[],"stop_reason":null,"usage":{"input_tokens":0,"output_tokens":0}}}"#,
        ),
        event(
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call_a","name":"read","input":{}}}"#,
        ),
        event(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"a\":1}"}}"#,
        ),
        event(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        ),
        event(
            "content_block_start",
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"call_b","name":"list","input":{}}}"#,
        ),
        event(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"b\":2}"}}"#,
        ),
        event(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":1}"#,
        ),
        event(
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":2}}"#,
        ),
        event("message_stop", r#"{"type":"message_stop"}"#),
    ];
    assert_eq!(lines, expected);
}
