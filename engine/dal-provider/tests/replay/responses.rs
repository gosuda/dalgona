//! Responses-family replay integration suite.
//!
//! Real-transport proof for the retained Responses obligations of the
//! provider layer part: the canonical reasoning/tool/text stream order, the
//! completed-response replay backfill, final argument precedence over deltas,
//! the incomplete max-tokens mapping with an open call, the ignored trailing
//! `[DONE]` line, the in-stream failure without retry, the decreasing
//! sequence number error, the byte-equal request snapshot with a replayed
//! reasoning item, the family-and-model replay binding, and the Responses
//! remote-compaction round trip.
//!
//! Every case serves authored synthetic fixtures through the shared
//! [`ReplayServer`] and drives them with the actual `ProviderSet`/`open`
//! (or `compact`) path. The server compares the request method, path,
//! required headers, and full body bytes of each exchange: a byte difference
//! answers 418 and fails `finish`, so a green `finish` is the byte-equality
//! proof for the snapshot, cross-model, and compact bodies. The decoder-level
//! value coverage already lives in `src/family/responses/tests.rs`; these
//! tests prove the same grammar over real HTTP framing.

use super::replay_server;

use std::sync::{Arc, Mutex};

use dal_core::{
    AssistantPart, CallId, ContextItem, Family, ModelRequest, ModelRoute, ModelToolSpec, Part,
    Purpose, RawJson, ReplaySource, RequestParams, SessionId, ThinkingLevel, Usage,
};
use dal_provider::{
    AuthStyle, CatalogEntry, CompactOutcome, EnvSnapshot, EventStream, Listing, NoticeSink,
    ProviderConfig, ProviderEntry, ProviderError, ProviderIdentity, ProviderSet, ProviderSettings,
    ResolvedModel, StopReason, StreamEvent, ThinkingSupport, ToolArgs, ToolCall, ToolSupport,
    Transport,
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

fn base_entry(model: &str, remote_compact: bool) -> CatalogEntry {
    CatalogEntry {
        provider: "responses-test".into(),
        id: model.into(),
        display: model.into(),
        listing: Listing::Listed,
        context_window: Some(32_000),
        max_output: Some(4_096),
        thinking: ThinkingSupport::OpenAi {
            accepted: Vec::new(),
            none_supported: true,
        },
        image_input: false,
        image_profile: None,
        remote_compact,
        supports_reasoning_summaries: false,
        tool_support: ToolSupport::Any,
        temperature_allowed: false,
        display_supported: false,
        custom_grammar: false,
        premium_multiplier: None,
        price: None,
    }
}

fn entry(model: &str) -> CatalogEntry {
    base_entry(model, false)
}

fn compact_entry(model: &str) -> CatalogEntry {
    base_entry(model, true)
}

fn params() -> RequestParams {
    RequestParams {
        thinking: ThinkingLevel::Off,
        effort: None,
        temperature: None,
        max_output_tokens: None,
    }
}

fn read_tool() -> ModelToolSpec {
    ModelToolSpec {
        name: "read".into(),
        description: "Read a file.".into(),
        parameters: RawJson::parse(r#"{"type":"object","properties":{}}"#).expect("tool schema"),
        grammar: None,
        defer_loading: false,
    }
}

fn user_text(text: &str) -> ContextItem {
    ContextItem::User {
        parts: vec![Part::Text { text: text.into() }],
    }
}

/// The minimal turn every single-exchange case sends: one user message, no
/// tools, thinking off with `none` support, no cache key.
fn minimal_request() -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::Responses,
            model: "gpt-5".into(),
        },
        system: Arc::from("Be brief."),
        tools: Arc::from([]),
        context: Arc::from([user_text("Look")]),
        params: params(),
        cache_key: None,
    }
}

/// The canonical turn: one tool advertised and a prompt cache key, so the
/// served body covers the `tools` and `prompt_cache_key` members.
fn turn_request() -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::Responses,
            model: "gpt-5".into(),
        },
        system: Arc::from("Be brief."),
        tools: Arc::from([read_tool()]),
        context: Arc::from([user_text("Look")]),
        params: params(),
        cache_key: Some("sess-1".into()),
    }
}

/// The snapshot turn: user text, a replayed reasoning item bound to this
/// family and model, assistant text, one call, and its result.
fn snapshot_request() -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::Responses,
            model: "gpt-5".into(),
        },
        system: Arc::from("Be brief."),
        tools: Arc::from([read_tool()]),
        context: Arc::from([
            user_text("Look"),
            ContextItem::Assistant {
                source: ReplaySource {
                    family: Family::Responses,
                    model: "gpt-5".into(),
                },
                parts: vec![
                    AssistantPart::Thinking {
                        text: "t".into(),
                        replay: Some(
                            RawJson::parse(
                                r#"{"type":"reasoning","id":"rs_1","encrypted_content":"e"}"#,
                            )
                            .expect("reasoning item"),
                        ),
                    },
                    AssistantPart::Text {
                        text: "Reading.".into(),
                        replay: None,
                    },
                    AssistantPart::ToolCall {
                        call: CallId::new("call_1"),
                        name: "read".into(),
                        args: RawJson::parse(r#"{"path":"a.rs"}"#).expect("call args"),
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
                    text: "fn main".into(),
                }],
            },
        ]),
        params: params(),
        cache_key: Some("sess-1".into()),
    }
}

/// The cross-model turn: a reasoning item produced by `gpt-5-a`, sent once
/// to `gpt-5-b` (which must drop it) and once to `gpt-5-a` (which keeps it).
fn cross_model_request(model: &str) -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::Responses,
            model: model.into(),
        },
        system: Arc::from("Be brief."),
        tools: Arc::from([]),
        context: Arc::from([
            user_text("Next"),
            ContextItem::Assistant {
                source: ReplaySource {
                    family: Family::Responses,
                    model: "gpt-5-a".into(),
                },
                parts: vec![
                    AssistantPart::Thinking {
                        text: "t".into(),
                        replay: Some(
                            RawJson::parse(
                                r#"{"type":"reasoning","id":"rs_7","encrypted_content":"z"}"#,
                            )
                            .expect("reasoning item"),
                        ),
                    },
                    AssistantPart::Text {
                        text: "Done.".into(),
                        replay: None,
                    },
                ],
                replay: None,
            },
        ]),
        params: params(),
        cache_key: None,
    }
}

fn compact_request() -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Compact,
        model: ModelRoute::Api {
            family: Family::Responses,
            model: "gpt-5".into(),
        },
        system: Arc::from("Summarize."),
        tools: Arc::from([]),
        context: Arc::from([user_text("Hi")]),
        params: params(),
        cache_key: None,
    }
}

fn zero_usage() -> Usage {
    Usage {
        input_tokens: 0,
        cached_input_tokens: 0,
        output_tokens: 0,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        cost_usd: None,
        premium_requests: None,
    }
}

struct Harness {
    server: replay_server::ReplayServer,
    dir: std::path::PathBuf,
    set: ProviderSet,
}

impl Harness {
    async fn start(case: &str, request_max_retries: u32, stream_max_retries: u32) -> Self {
        let fixture = replay_server::fixture_dir("responses", case);
        let server = replay_server::ReplayServer::start(&fixture)
            .await
            .expect("replay server starts");
        let base = server.base_url();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "dalgona-replay-responses-{case}-{stamp}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let config = ProviderConfig {
            default_model: None,
            thinking: ThinkingLevel::Medium,
            aliases: Vec::new(),
            request_max_retries,
            stream_max_retries,
            providers: vec![ProviderEntry {
                id: "responses-test".into(),
                def: None,
                family: Family::Responses,
                base_url: base.into_boxed_str(),
                transport: Transport::Https,
                key_env: Some("RESP_TEST_KEY".into()),
                auth: AuthStyle::Bearer,
                max_concurrent_requests: 4,
                settings: ProviderSettings::default(),
            }],
            scripted: None,
        };
        let set = ProviderSet::new(
            &config,
            identity(),
            EnvSnapshot::test(&[("RESP_TEST_KEY", "resp-secret")]),
            &dir,
            &dir,
        )
        .expect("provider set builds");
        Self { server, dir, set }
    }

    fn notices() -> (NoticeSink, Arc<Mutex<Vec<String>>>) {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let sink: NoticeSink = Arc::new({
            let captured = Arc::clone(&captured);
            move |notice: String| {
                captured.lock().expect("notices lock").push(notice);
            }
        });
        (sink, captured)
    }

    /// The shared verdict: no mismatch recorded, every exchange consumed,
    /// nothing left open.
    async fn finish(self, consumed: usize) {
        assert!(
            self.server.mismatches().is_empty(),
            "replay mismatches: {:?}",
            self.server.mismatches()
        );
        assert_eq!(self.server.consumed(), consumed);
        assert_eq!(self.server.open_requests(), 0);
        assert!(self.server.max_open_requests() >= 1);
        self.server.finish().await.expect("replay consumed");
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

async fn drain(mut stream: EventStream) -> Vec<Result<StreamEvent, ProviderError>> {
    let mut out = Vec::new();
    while let Some(event) = stream.next().await {
        out.push(event);
    }
    out
}

fn ok_events(results: Vec<Result<StreamEvent, ProviderError>>) -> Vec<StreamEvent> {
    results
        .into_iter()
        .map(|result| result.expect("no stream error"))
        .collect()
}

#[tokio::test]
async fn responses_turn_canonical_order() {
    const REASONING: &str = r#"{"id":"rs_1", "type":"reasoning","summary":[{"type":"summary_text","text":"Check the file."}],"encrypted_content":"gAAAAB","n":1e+02}"#;
    let harness = Harness::start("turn", 0, 0).await;
    let (notices, _) = Harness::notices();
    let cancel = CancellationToken::new();
    let provider = harness
        .set
        .provider(ResolvedModel {
            provider: "responses-test".into(),
            route: ModelRoute::Api {
                family: Family::Responses,
                model: "gpt-5".into(),
            },
            entry: entry("gpt-5"),
        })
        .expect("provider binds");
    let stream = provider
        .open(SessionId::new_v7(), &turn_request(), &[], notices, &cancel)
        .await
        .expect("responses turn opens");
    let events = ok_events(drain(stream).await);
    let expected = vec![
        StreamEvent::ReasoningDelta {
            text: "Check the file.".into(),
        },
        StreamEvent::Replay {
            payload: dal_provider::ReplayPayload {
                family: Family::Responses,
                model: "gpt-5".into(),
                item: RawJson::parse(REASONING).expect("reasoning item"),
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
                args: ToolArgs::Parsed(RawJson::parse(r#"{"path":"a.rs"}"#).expect("call args")),
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
                premium_requests: None,
            },
        },
        StreamEvent::Stop {
            reason: StopReason::ToolUse,
        },
    ];
    assert_eq!(events, expected);
    harness.finish(1).await;
}

#[tokio::test]
async fn responses_replay_backfill_from_completed() {
    let harness = Harness::start("replay_backfill", 0, 0).await;
    let (notices, _) = Harness::notices();
    let cancel = CancellationToken::new();
    let provider = harness
        .set
        .provider(ResolvedModel {
            provider: "responses-test".into(),
            route: ModelRoute::Api {
                family: Family::Responses,
                model: "gpt-5".into(),
            },
            entry: entry("gpt-5"),
        })
        .expect("provider binds");
    let stream = provider
        .open(
            SessionId::new_v7(),
            &minimal_request(),
            &[],
            notices,
            &cancel,
        )
        .await
        .expect("responses turn opens");
    let events = ok_events(drain(stream).await);
    assert_eq!(events.len(), 5);
    assert!(matches!(&events[0], StreamEvent::TextDelta { text } if text == "hi"));
    assert!(matches!(&events[1], StreamEvent::Replay { .. }));
    if let StreamEvent::Replay { payload } = &events[1] {
        assert_eq!(payload.family, Family::Responses);
        assert_eq!(payload.model.as_ref(), "gpt-5");
        assert_eq!(
            payload.item.as_str(),
            r#"{"id":"rs_9","type":"reasoning","summary":[],"encrypted_content":"full"}"#
        );
    }
    assert!(matches!(&events[2], StreamEvent::ToolCallsDone { calls } if calls.is_empty()));
    assert!(matches!(&events[3], StreamEvent::Usage { usage } if *usage == zero_usage()));
    assert!(matches!(&events[4], StreamEvent::Stop { reason } if *reason == StopReason::EndTurn));
    harness.finish(1).await;
}

#[tokio::test]
async fn responses_argument_precedence_final_over_deltas() {
    let harness = Harness::start("argument_precedence", 0, 0).await;
    let (notices, _) = Harness::notices();
    let cancel = CancellationToken::new();
    let provider = harness
        .set
        .provider(ResolvedModel {
            provider: "responses-test".into(),
            route: ModelRoute::Api {
                family: Family::Responses,
                model: "gpt-5".into(),
            },
            entry: entry("gpt-5"),
        })
        .expect("provider binds");
    let stream = provider
        .open(
            SessionId::new_v7(),
            &minimal_request(),
            &[],
            notices,
            &cancel,
        )
        .await
        .expect("responses turn opens");
    let events = ok_events(drain(stream).await);
    assert_eq!(events.len(), 5);
    assert!(
        matches!(&events[0], StreamEvent::ToolCallStarted { id, name } if id == "call_7" && name == "calc")
    );
    assert!(
        matches!(&events[1], StreamEvent::ToolArgsDelta { id, fragment } if id == "call_7" && fragment.as_slice() == &br#"{"a":1"#[..])
    );
    assert!(matches!(&events[2], StreamEvent::ToolCallsDone { calls } if calls.len() == 1));
    if let StreamEvent::ToolCallsDone { calls } = &events[2] {
        assert_eq!(calls[0].id, "call_7");
        assert_eq!(calls[0].name, "calc");
        assert_eq!(
            calls[0].args,
            ToolArgs::Parsed(RawJson::parse(r#"{"a":2}"#).expect("final args"))
        );
    }
    assert!(matches!(&events[3], StreamEvent::Usage { usage } if *usage == zero_usage()));
    assert!(matches!(&events[4], StreamEvent::Stop { reason } if *reason == StopReason::ToolUse));
    harness.finish(1).await;
}

#[tokio::test]
async fn responses_incomplete_max_output_truncates_open_call() {
    let harness = Harness::start("incomplete_max_output", 0, 0).await;
    let (notices, _) = Harness::notices();
    let cancel = CancellationToken::new();
    let provider = harness
        .set
        .provider(ResolvedModel {
            provider: "responses-test".into(),
            route: ModelRoute::Api {
                family: Family::Responses,
                model: "gpt-5".into(),
            },
            entry: entry("gpt-5"),
        })
        .expect("provider binds");
    let stream = provider
        .open(
            SessionId::new_v7(),
            &minimal_request(),
            &[],
            notices,
            &cancel,
        )
        .await
        .expect("responses turn opens");
    let events = ok_events(drain(stream).await);
    assert_eq!(events.len(), 4);
    assert!(
        matches!(&events[0], StreamEvent::ToolCallStarted { id, name } if id == "call_9" && name == "write")
    );
    assert!(matches!(&events[1], StreamEvent::ToolCallsDone { calls } if calls.len() == 1));
    if let StreamEvent::ToolCallsDone { calls } = &events[1] {
        assert_eq!(calls[0].id, "call_9");
        assert_eq!(calls[0].args, ToolArgs::Truncated);
    }
    assert!(matches!(&events[2], StreamEvent::Usage { usage } if *usage == zero_usage()));
    assert!(matches!(&events[3], StreamEvent::Stop { reason } if *reason == StopReason::MaxTokens));
    harness.finish(1).await;
}

#[tokio::test]
async fn responses_done_line_before_completion_is_ignored() {
    let harness = Harness::start("ignored_done_line", 0, 0).await;
    let (notices, _) = Harness::notices();
    let cancel = CancellationToken::new();
    let provider = harness
        .set
        .provider(ResolvedModel {
            provider: "responses-test".into(),
            route: ModelRoute::Api {
                family: Family::Responses,
                model: "gpt-5".into(),
            },
            entry: entry("gpt-5"),
        })
        .expect("provider binds");
    let stream = provider
        .open(
            SessionId::new_v7(),
            &minimal_request(),
            &[],
            notices,
            &cancel,
        )
        .await
        .expect("responses turn opens");
    let events = ok_events(drain(stream).await);
    assert_eq!(events.len(), 4);
    assert!(matches!(&events[0], StreamEvent::TextDelta { text } if text == "hi"));
    assert!(matches!(&events[1], StreamEvent::ToolCallsDone { calls } if calls.is_empty()));
    assert!(matches!(&events[2], StreamEvent::Usage { usage } if *usage == zero_usage()));
    assert!(matches!(&events[3], StreamEvent::Stop { reason } if *reason == StopReason::EndTurn));
    harness.finish(1).await;
}

#[tokio::test]
async fn responses_failed_after_text_is_status_without_retry() {
    // Retries stay enabled so the single consumed exchange proves the
    // in-stream failure is not retried after delivery.
    let harness = Harness::start("failed_no_retry", 3, 3).await;
    let (notices, captured) = Harness::notices();
    let cancel = CancellationToken::new();
    let provider = harness
        .set
        .provider(ResolvedModel {
            provider: "responses-test".into(),
            route: ModelRoute::Api {
                family: Family::Responses,
                model: "gpt-5".into(),
            },
            entry: entry("gpt-5"),
        })
        .expect("provider binds");
    let stream = provider
        .open(
            SessionId::new_v7(),
            &minimal_request(),
            &[],
            notices,
            &cancel,
        )
        .await
        .expect("responses turn opens");
    let events = drain(stream).await;
    assert_eq!(events.len(), 2);
    assert!(matches!(&events[0], Ok(StreamEvent::TextDelta { text }) if text == "par"));
    assert!(
        matches!(&events[1], Err(ProviderError::Status { family: Family::Responses, status: 200, message }) if message == "boom")
    );
    if let Err(error) = &events[1] {
        assert_eq!(error.to_string(), "openai error 200: boom");
    }
    let seen = captured.lock().expect("notices lock").clone();
    assert!(
        !seen.iter().any(|notice| notice.contains("Retr")),
        "unexpected retry notices: {seen:?}"
    );
    harness.finish(1).await;
}

#[tokio::test]
async fn responses_sequence_number_backwards_is_protocol_error() {
    let harness = Harness::start("sequence_number_backwards", 0, 0).await;
    let (notices, _) = Harness::notices();
    let cancel = CancellationToken::new();
    let provider = harness
        .set
        .provider(ResolvedModel {
            provider: "responses-test".into(),
            route: ModelRoute::Api {
                family: Family::Responses,
                model: "gpt-5".into(),
            },
            entry: entry("gpt-5"),
        })
        .expect("provider binds");
    let stream = provider
        .open(
            SessionId::new_v7(),
            &minimal_request(),
            &[],
            notices,
            &cancel,
        )
        .await
        .expect("responses turn opens");
    let events = drain(stream).await;
    assert_eq!(events.len(), 1);
    assert!(
        matches!(&events[0], Err(ProviderError::Protocol { family: Family::Responses, detail }) if detail == "sequence_number went backwards")
    );
    if let Err(error) = &events[0] {
        assert_eq!(
            error.to_string(),
            "openai sent an invalid stream: sequence_number went backwards"
        );
    }
    harness.finish(1).await;
}

#[tokio::test]
async fn responses_request_snapshot_with_replay_item() {
    let harness = Harness::start("request_snapshot", 0, 0).await;
    let (notices, _) = Harness::notices();
    let cancel = CancellationToken::new();
    let provider = harness
        .set
        .provider(ResolvedModel {
            provider: "responses-test".into(),
            route: ModelRoute::Api {
                family: Family::Responses,
                model: "gpt-5".into(),
            },
            entry: entry("gpt-5"),
        })
        .expect("provider binds");
    let stream = provider
        .open(
            SessionId::new_v7(),
            &snapshot_request(),
            &[],
            notices,
            &cancel,
        )
        .await
        .expect("responses turn opens");
    let events = ok_events(drain(stream).await);
    assert_eq!(events.len(), 3);
    assert!(matches!(&events[0], StreamEvent::ToolCallsDone { calls } if calls.is_empty()));
    assert!(matches!(&events[1], StreamEvent::Usage { usage } if *usage == zero_usage()));
    assert!(matches!(&events[2], StreamEvent::Stop { reason } if *reason == StopReason::EndTurn));
    // The byte-equality proof is the server verdict below: the fixture
    // `request.json` carries the exact expected body, and any difference
    // answers 418 and fails `finish`.
    harness.finish(1).await;
}

#[tokio::test]
async fn responses_replay_bound_to_model_not_cross_model() {
    let harness = Harness::start("replay_not_cross_model", 0, 0).await;
    let (notices, _) = Harness::notices();
    let cancel = CancellationToken::new();
    for model in ["gpt-5-b", "gpt-5-a"] {
        let provider = harness
            .set
            .provider(ResolvedModel {
                provider: "responses-test".into(),
                route: ModelRoute::Api {
                    family: Family::Responses,
                    model: model.into(),
                },
                entry: entry(model),
            })
            .expect("provider binds");
        let stream = provider
            .open(
                SessionId::new_v7(),
                &cross_model_request(model),
                &[],
                Arc::clone(&notices),
                &cancel,
            )
            .await
            .expect("responses turn opens");
        let events = ok_events(drain(stream).await);
        assert_eq!(events.len(), 3, "model {model}");
        assert!(
            matches!(&events[2], StreamEvent::Stop { reason } if *reason == StopReason::EndTurn),
            "model {model}"
        );
    }
    // Exchange 0 proves the model-B body drops the foreign reasoning item;
    // exchange 1 proves the model-A body keeps it verbatim. Both bodies are
    // compared byte for byte by the server.
    harness.finish(2).await;
}

#[tokio::test]
async fn responses_remote_compact_round_trip() {
    let harness = Harness::start("compact", 0, 0).await;
    let (notices, _) = Harness::notices();
    let cancel = CancellationToken::new();
    let provider = harness
        .set
        .provider(ResolvedModel {
            provider: "responses-test".into(),
            route: ModelRoute::Api {
                family: Family::Responses,
                model: "gpt-5".into(),
            },
            entry: compact_entry("gpt-5"),
        })
        .expect("provider binds");
    let outcome = provider
        .compact(SessionId::new_v7(), &compact_request(), notices, &cancel)
        .await
        .expect("compact runs");
    assert!(matches!(outcome, Some(CompactOutcome::Compacted(_))));
    if let Some(CompactOutcome::Compacted(history)) = outcome {
        assert_eq!(history.family, Family::Responses);
        assert_eq!(history.model.as_ref(), "gpt-5");
        assert_eq!(history.items.len(), 2);
        assert_eq!(
            history.items[0].as_str(),
            r#"{"type":"reasoning","id":"rs_c","encrypted_content":"c"}"#
        );
        assert_eq!(
            history.items[1].as_str(),
            r#"{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Summary."}]}"#
        );
    }
    harness.finish(1).await;
}

#[tokio::test]
async fn responses_cancel_before_open_sends_nothing() {
    let harness = Harness::start("cancel_before_open", 0, 0).await;
    let (notices, _) = Harness::notices();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let provider = harness
        .set
        .provider(ResolvedModel {
            provider: "responses-test".into(),
            route: ModelRoute::Api {
                family: Family::Responses,
                model: "gpt-5".into(),
            },
            entry: entry("gpt-5"),
        })
        .expect("provider binds");
    let error = provider
        .open(
            SessionId::new_v7(),
            &minimal_request(),
            &[],
            notices,
            &cancel,
        )
        .await
        .expect_err("cancelled open fails");
    assert!(matches!(
        &error,
        ProviderError::Transport {
            family: Family::Responses,
            ..
        }
    ));
    if let ProviderError::Transport { family, reason } = error {
        assert_eq!(family, Family::Responses);
        assert_eq!(reason, "provider request cancelled before it started");
    }
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
        .expect_err("no exchange was consumed");
    assert!(
        verdict
            .to_string()
            .contains("replay incomplete: 0 of 1 exchanges consumed")
    );
    std::fs::remove_dir_all(&harness.dir).ok();
}
