//! Chat-family replay integration suite: the retained transport proofs for the
//! `OpenAI` Chat body lowering and stream decoding (private provider-layer
//! clauses 315-319, 489-492, 506, 507, 512).
//!
//! Every case drives a real turn through [`ProviderSet::open`] against the
//! shared replay server and asserts the neutral [`StreamEvent`] values and
//! order, the terminal error where one is specified, and that every exchange
//! was consumed with no mismatch. The decoder-level rows in
//! `src/family/chat/tests.rs` stay the unit proofs; nothing here repeats them.
//! The only pre-existing transport proof, `chat_text_turn` in `tests/replay.rs`,
//! is unchanged and not duplicated.

use super::replay_server;

use std::path::PathBuf;
use std::sync::Arc;

use dal_core::{
    AssistantPart, CallId, ContextItem, Family, ModelRequest, ModelRoute, ModelToolSpec, Part,
    Purpose, RawJson, ReplaySource, RequestParams, ThinkingLevel, Usage,
};
use dal_provider::{
    AuthStyle, CatalogEntry, EnvSnapshot, Listing, NoticeSink, ProviderConfig, ProviderEntry,
    ProviderError, ProviderIdentity, ProviderSet, ResolvedModel, StopReason, StreamEvent,
    ThinkingSupport, ToolArgs, ToolCall, ToolSupport, Transport,
};

use tokio_util::sync::CancellationToken;

fn identity() -> ProviderIdentity {
    ProviderIdentity {
        version: "test".into(),
        os: "linux".into(),
        os_version: "test".into(),
        arch: "x86_64".into(),
    }
}

fn entry() -> CatalogEntry {
    CatalogEntry {
        provider: "chat-test".into(),
        id: "gpt-6-astra".into(),
        display: "gpt-6-astra".into(),
        listing: Listing::Listed,
        context_window: Some(32_000),
        max_output: Some(4_096),
        thinking: ThinkingSupport::OpenAi {
            accepted: Vec::new(),
            none_supported: true,
        },
        image_input: false,
        image_profile: None,
        remote_compact: false,
        supports_reasoning_summaries: false,
        tool_support: ToolSupport::Any,
        temperature_allowed: false,
        display_supported: false,
        custom_grammar: false,
    }
}

fn config(base: &str) -> ProviderConfig {
    ProviderConfig {
        default_model: None,
        thinking: ThinkingLevel::Medium,
        aliases: Vec::new(),
        request_max_retries: 0,
        stream_max_retries: 0,
        providers: vec![ProviderEntry {
            id: "chat-test".into(),
            def: None,
            family: Family::Chat,
            base_url: base.into(),
            transport: Transport::Https,
            key_env: Some("CHAT_TEST_KEY".into()),
            auth: AuthStyle::Bearer,
            max_concurrent_requests: 4,
        }],
        scripted: None,
    }
}

fn resolved() -> ResolvedModel {
    ResolvedModel {
        provider: "chat-test".into(),
        route: ModelRoute::Api {
            family: Family::Chat,
            model: "gpt-6-astra".into(),
        },
        entry: entry(),
    }
}

/// A tool-free turn request: terse system text and one user text part.
fn chat_request() -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::Chat,
            model: "gpt-6-astra".into(),
        },
        system: Arc::from("You are terse."),
        tools: Arc::from([]),
        context: Arc::from([ContextItem::User {
            parts: vec![Part::Text { text: "Hi".into() }],
        }]),
        params: RequestParams {
            thinking: ThinkingLevel::Off,
            effort: None,
            temperature: None,
            max_output_tokens: None,
        },
        cache_key: None,
    }
}

/// The fixed request of the body snapshot: two tools in registration order,
/// a user message, an assistant message with text and one call, and the
/// matching tool result.
fn snapshot_request() -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::Chat,
            model: "gpt-6-astra".into(),
        },
        system: Arc::from("You are terse."),
        tools: Arc::from(vec![
            ModelToolSpec {
                name: "read".into(),
                description: "Read a file.".into(),
                parameters: RawJson::parse(
                    r#"{"type":"object","properties":{"path":{"type":"string"}}}"#,
                )
                .expect("tool schema parses"),
                grammar: None,
                defer_loading: false,
            },
            ModelToolSpec {
                name: "list".into(),
                description: "List files.".into(),
                parameters: RawJson::parse(r#"{"type": "object"}"#).expect("tool schema parses"),
                grammar: None,
                defer_loading: false,
            },
        ]),
        context: Arc::from([
            ContextItem::User {
                parts: vec![Part::Text {
                    text: "Describe the repo.".into(),
                }],
            },
            ContextItem::Assistant {
                source: ReplaySource {
                    family: Family::Chat,
                    model: "gpt-6-astra".into(),
                },
                parts: vec![
                    AssistantPart::Text {
                        text: "Checking.".into(),
                        replay: None,
                    },
                    AssistantPart::ToolCall {
                        call: CallId::new("call_1"),
                        name: "read".into(),
                        args: RawJson::parse(r#"{"path":"a.txt"}"#).expect("call args parse"),
                        replay: None,
                    },
                ],
                replay: None,
            },
            ContextItem::ToolResult {
                call: CallId::new("call_1"),
                name: "read".into(),
                is_error: false,
                parts: vec![Part::Text {
                    text: "line1".into(),
                }],
            },
        ]),
        params: RequestParams {
            thinking: ThinkingLevel::Off,
            effort: None,
            temperature: None,
            max_output_tokens: None,
        },
        cache_key: None,
    }
}

fn temp_dir(tag: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "dalgona-replay-chat-{tag}-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn zero_usage() -> Usage {
    Usage {
        input_tokens: 0,
        cached_input_tokens: 0,
        output_tokens: 0,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        cost_usd: None,
    }
}

struct Harness {
    server: replay_server::ReplayServer,
    set: ProviderSet,
    dir: PathBuf,
}

async fn harness(case: &str, tag: &str) -> Harness {
    let server = replay_server::ReplayServer::start(&replay_server::fixture_dir("chat", case))
        .await
        .expect("replay server starts");
    let dir = temp_dir(tag);
    let set = ProviderSet::new(
        &config(&server.base_url()),
        identity(),
        EnvSnapshot::test(&[("CHAT_TEST_KEY", "chat-secret")]),
        &dir,
        &dir,
    )
    .expect("provider set builds");
    Harness { server, set, dir }
}

struct Driven {
    server: replay_server::ReplayServer,
    events: Vec<StreamEvent>,
    error: Option<ProviderError>,
    dir: PathBuf,
}

/// Opens one turn against the case fixture and collects the whole stream:
/// every event in order, then at most one terminal error.
async fn drive(case: &str, request: &ModelRequest, tag: &str) -> Driven {
    let harness = harness(case, tag).await;
    let provider = harness.set.provider(resolved()).expect("provider binds");
    let notices: NoticeSink = Arc::new(|_| {});
    let cancel = CancellationToken::new();
    let mut stream = provider
        .open(
            dal_core::SessionId::new_v7(),
            request,
            &[],
            notices,
            &cancel,
        )
        .await
        .expect("chat turn opens");
    let mut events = Vec::new();
    let mut error = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(event) => events.push(event),
            Err(failure) => {
                error = Some(failure);
                break;
            }
        }
    }
    Driven {
        server: harness.server,
        events,
        error,
        dir: harness.dir,
    }
}

async fn expect_fully_consumed(driven: Driven, exchanges: usize) {
    let mismatches = driven.server.mismatches();
    assert!(mismatches.is_empty(), "replay mismatches: {mismatches:?}");
    assert_eq!(driven.server.consumed(), exchanges);
    assert_eq!(driven.server.open_requests(), 0);
    assert!(driven.server.max_open_requests() >= 1);
    driven.server.finish().await.expect("replay consumed");
    std::fs::remove_dir_all(&driven.dir).ok();
}

/// Behavior `chat-tool-turn`: one real streamed tool call with its argument
/// split across five deltas. The first id and name must survive a later
/// conflicting id/name, the raw fragments must assemble to the parsed JSON
/// object, and the terminal group must contain exactly one usage and stop.
#[tokio::test]
async fn chat_tool_turn() {
    let driven = drive("tool_turn", &chat_request(), "tool").await;
    assert!(
        driven.error.is_none(),
        "unexpected stream error: {:?}",
        driven.error
    );
    let call_id = String::from("call_abc123");
    let mut expected = vec![StreamEvent::ToolCallStarted {
        id: call_id.clone(),
        name: "get_current_weather".into(),
    }];
    for fragment in ["{\n\"", "location", "\": \"Bos", "ton, MA\"", "\n}"] {
        expected.push(StreamEvent::ToolArgsDelta {
            id: call_id.clone(),
            fragment: fragment.as_bytes().to_vec(),
        });
    }
    expected.extend([
        StreamEvent::ToolCallsDone {
            calls: vec![ToolCall {
                id: call_id,
                name: "get_current_weather".into(),
                args: ToolArgs::Parsed(
                    RawJson::parse("{\n\"location\": \"Boston, MA\"\n}").expect("args parse"),
                ),
            }],
        },
        StreamEvent::Usage {
            usage: Usage {
                input_tokens: 82,
                cached_input_tokens: 0,
                output_tokens: 17,
                reasoning_tokens: None,
                cache_write_tokens: 0,
                cost_usd: None,
            },
        },
        StreamEvent::Stop {
            reason: StopReason::ToolUse,
        },
    ]);
    assert_eq!(driven.events, expected);
    expect_fully_consumed(driven, 1).await;
}

/// Consumer cancellation after the first delivered delta must close the
/// active HTTP transfer. The stalled fixture proves that a pending read is
/// not mistaken for a terminal event; cancellation closes the transfer while
/// the outer stream remains pending.
#[tokio::test]
async fn chat_consumer_cancel_closes_transfer() {
    let harness = harness("cancel_stall", "cancel").await;
    let provider = harness.set.provider(resolved()).expect("provider binds");
    let notices: NoticeSink = Arc::new(|_| {});
    let cancel = CancellationToken::new();
    let mut stream = provider
        .open(
            dal_core::SessionId::new_v7(),
            &chat_request(),
            &[],
            notices,
            &cancel,
        )
        .await
        .expect("chat turn opens");
    let first = stream
        .next()
        .await
        .expect("first event arrives")
        .expect("no stream error");
    assert_eq!(
        first,
        StreamEvent::TextDelta {
            text: "Partial".into(),
        }
    );
    assert_eq!(harness.server.consumed(), 1);
    assert_eq!(
        harness.server.open_requests(),
        1,
        "response must remain open after the first delta"
    );
    cancel.cancel();

    let pending = tokio::time::timeout(std::time::Duration::from_millis(200), stream.next()).await;
    assert!(
        pending.is_err(),
        "a stalled transfer must not synthesize a terminal event"
    );
    let closed = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while harness.server.open_requests() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(closed.is_ok(), "cancellation must close the replay request");
    assert!(
        harness.server.mismatches().is_empty(),
        "replay mismatches: {:?}",
        harness.server.mismatches()
    );
    assert_eq!(harness.server.consumed(), 1);
    assert_eq!(harness.server.open_requests(), 0);
    assert!(harness.server.max_open_requests() >= 1);
    drop(stream);
    harness.server.finish().await.expect("replay consumed");
    std::fs::remove_dir_all(&harness.dir).ok();
}

/// Behavior `chat-parallel-calls`: two calls at indexes 0 and 1 with
/// interleaved deltas. Index 1 opens first, a later chunk for the open index
/// carries a foreign id and name that must be ignored, and the terminal lists
/// both calls in index order with their assembled arguments. The chunks also
/// carry unknown external fields (`service_tier`, `prompt_filter_results`,
/// `logprobs`, `system_fingerprint`, `obfuscation`) that must not disturb the
/// decode.
#[tokio::test]
async fn chat_parallel_calls() {
    let driven = drive("parallel_calls", &chat_request(), "parallel").await;
    assert!(
        driven.error.is_none(),
        "unexpected stream error: {:?}",
        driven.error
    );
    let args_a = ToolArgs::Parsed(RawJson::parse(r#"{"x":1}"#).expect("args parse"));
    let args_b = ToolArgs::Parsed(RawJson::parse(r#"{"y":2}"#).expect("args parse"));
    assert_eq!(
        driven.events,
        vec![
            StreamEvent::ToolCallStarted {
                id: "call_b".into(),
                name: "b".into(),
            },
            StreamEvent::ToolCallStarted {
                id: "call_a".into(),
                name: "a".into(),
            },
            StreamEvent::ToolArgsDelta {
                id: "call_a".into(),
                fragment: b"{\"x\":".to_vec(),
            },
            StreamEvent::ToolArgsDelta {
                id: "call_b".into(),
                fragment: b"{\"y\":2}".to_vec(),
            },
            StreamEvent::ToolArgsDelta {
                id: "call_a".into(),
                fragment: b"1}".to_vec(),
            },
            StreamEvent::ToolCallsDone {
                calls: vec![
                    ToolCall {
                        id: "call_a".into(),
                        name: "a".into(),
                        args: args_a,
                    },
                    ToolCall {
                        id: "call_b".into(),
                        name: "b".into(),
                        args: args_b,
                    },
                ],
            },
            StreamEvent::Usage {
                usage: zero_usage(),
            },
            StreamEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ]
    );
    expect_fully_consumed(driven, 1).await;
}

/// Behavior `chat-eof-without-done`: `finish_reason:"stop"` then transport
/// EOF with no `[DONE]` and no usage chunk. The turn still stops with
/// `EndTurn` and zero usage.
#[tokio::test]
async fn chat_eof_without_done() {
    let driven = drive("eof_without_done", &chat_request(), "eof-done").await;
    assert!(
        driven.error.is_none(),
        "unexpected stream error: {:?}",
        driven.error
    );
    assert_eq!(
        driven.events,
        vec![
            StreamEvent::TextDelta {
                text: "Hello".into(),
            },
            StreamEvent::ToolCallsDone { calls: Vec::new() },
            StreamEvent::Usage {
                usage: zero_usage(),
            },
            StreamEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ]
    );
    expect_fully_consumed(driven, 1).await;
}

/// Behavior `chat-eof-before-reason`: transport EOF before any
/// `finish_reason`. The text delta arrives, then the stream ends with the
/// exact cut error after exactly one request.
#[tokio::test]
async fn chat_eof_before_reason() {
    let driven = drive("eof_before_reason", &chat_request(), "eof-cut").await;
    assert_eq!(
        driven.events,
        vec![StreamEvent::TextDelta {
            text: "Hello".into(),
        }]
    );
    let error = driven.error.as_ref().expect("the cut must surface");
    assert!(matches!(error, ProviderError::StreamCut));
    assert_eq!(error.to_string(), "stream cut off before completion.");
    expect_fully_consumed(driven, 1).await;
}

/// Behavior `invalid-tool-arguments`: one call whose assembled arguments are
/// `{"a":`, then `finish_reason:"tool_calls"` and `[DONE]`. The call closes
/// as `Invalid` with a non-empty parser message, the terminal group still
/// arrives with `Stop(ToolUse)`, no stream error is raised, and the invalid
/// call triggers no follow-up request.
#[tokio::test]
async fn invalid_tool_arguments() {
    let driven = drive("invalid_tool_arguments", &chat_request(), "badargs").await;
    assert!(
        driven.error.is_none(),
        "unexpected stream error: {:?}",
        driven.error
    );
    assert_eq!(
        driven.events.len(),
        5,
        "unexpected events: {:?}",
        driven.events
    );
    assert_eq!(
        driven.events[0],
        StreamEvent::ToolCallStarted {
            id: "call_1".into(),
            name: "f".into(),
        }
    );
    assert_eq!(
        driven.events[1],
        StreamEvent::ToolArgsDelta {
            id: "call_1".into(),
            fragment: b"{\"a\":".to_vec(),
        }
    );
    let StreamEvent::ToolCallsDone { calls } = &driven.events[2] else {
        panic!("expected ToolCallsDone, got {:?}", driven.events[2]);
    };
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "call_1");
    assert_eq!(calls[0].name, "f");
    let ToolArgs::Invalid { message } = &calls[0].args else {
        panic!(
            "unparsable arguments must stay Invalid, got {:?}",
            calls[0].args
        );
    };
    assert!(!message.is_empty(), "parser message must be non-empty");
    assert_eq!(
        driven.events[3],
        StreamEvent::Usage {
            usage: zero_usage(),
        }
    );
    assert_eq!(
        driven.events[4],
        StreamEvent::Stop {
            reason: StopReason::ToolUse,
        }
    );
    expect_fully_consumed(driven, 1).await;
}

/// Behavior `chat-request-snapshot`: the fixed request must reach the server
/// byte for byte as `request-body.json` (member order, tool order, raw
/// spellings, `reasoning_effort:"none"` for the off level, no temperature,
/// no cache key, no `max_completion_tokens`). Any drift answers 418 and fails
/// the open. The canned stop stream then completes the turn.
#[tokio::test]
async fn chat_request_snapshot() {
    let driven = drive("request_snapshot", &snapshot_request(), "snapshot").await;
    assert!(
        driven.error.is_none(),
        "unexpected stream error: {:?}",
        driven.error
    );
    assert_eq!(
        driven.events,
        vec![
            StreamEvent::ToolCallsDone { calls: Vec::new() },
            StreamEvent::Usage {
                usage: zero_usage(),
            },
            StreamEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ]
    );
    expect_fully_consumed(driven, 1).await;
}

/// Behavior `chat-no-tools`: the tool-free request body carries no `tools`,
/// `tool_choice`, or `parallel_tool_calls` member, proved by the byte-exact
/// `request-body.json` match.
#[tokio::test]
async fn chat_no_tools() {
    let driven = drive("no_tools", &chat_request(), "notools").await;
    assert!(
        driven.error.is_none(),
        "unexpected stream error: {:?}",
        driven.error
    );
    assert_eq!(
        driven.events,
        vec![
            StreamEvent::ToolCallsDone { calls: Vec::new() },
            StreamEvent::Usage {
                usage: zero_usage(),
            },
            StreamEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ]
    );
    expect_fully_consumed(driven, 1).await;
}

/// Cancellation: an open against an already-cancelled token performs no HTTP
/// at all. The exact transport error surfaces, the server sees zero requests,
/// and the untouched exchange stays unconsumed.
#[tokio::test]
async fn chat_open_cancelled_before_start() {
    let harness = harness("no_tools", "cancelled").await;
    let provider = harness.set.provider(resolved()).expect("provider binds");
    let notices: NoticeSink = Arc::new(|_| {});
    let cancel = CancellationToken::new();
    cancel.cancel();
    let error = provider
        .open(
            dal_core::SessionId::new_v7(),
            &chat_request(),
            &[],
            notices,
            &cancel,
        )
        .await
        .expect_err("a cancelled open performs no request");
    assert!(matches!(error, ProviderError::Transport { .. }));
    assert_eq!(
        error.to_string(),
        "openai request failed: provider request cancelled before it started"
    );
    assert_eq!(harness.server.consumed(), 0);
    assert!(
        harness.server.mismatches().is_empty(),
        "replay mismatches: {:?}",
        harness.server.mismatches()
    );
    assert_eq!(harness.server.open_requests(), 0);
    let verdict = harness
        .server
        .finish()
        .await
        .expect_err("no exchange was claimed");
    assert!(
        verdict
            .to_string()
            .contains("replay incomplete: 0 of 1 exchanges consumed"),
        "unexpected verdict: {verdict}"
    );
    std::fs::remove_dir_all(&harness.dir).ok();
}
