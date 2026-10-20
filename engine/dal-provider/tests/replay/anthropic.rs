//! Anthropic-family replay integration suite.
//!
//! Real-transport proof for the retained Anthropic obligations of the provider
//! layer part: the signed thinking replay, tool-input chunk fragmentation,
//! input/output usage accumulation, the pause/refusal/other stop mapping, the
//! overload retry boundary before versus after first delivery, the
//! empty-signature exclusion, the byte-equal API-key request snapshot, the
//! OAuth wire framing, remote compaction request/result handling, mid-stream
//! cancellation, and the API-key/bearer/OAuth request header distinctions.
//!
//! Every replay case serves an authored synthetic fixture through the shared
//! [`ReplayServer`] and drives the actual `ProviderSet`/`open`/`compact` paths.
//! The server compares the request method, path, and required headers of each
//! exchange, and the full body bytes where the fixture names them: a
//! difference answers 418 and fails `finish`. The decoder-level value
//! coverage already lives in `src/family/anthropic/tests.rs`; these tests
//! prove the same grammar over real HTTP framing, plus the retry,
//! cancellation, and credential-framing behavior that only exists above the
//! decoder.
//!
//! The shared server checks only the headers a fixture lists, so it cannot
//! prove a header is absent. The three `*_header_distinction` tests therefore
//! serve the same requests from a tiny loopback capture service in this file
//! and assert the full security-relevant header set, present and absent, with
//! the exact synthetic credential values. Failure messages never print those
//! values.

use super::replay_server;

use std::{
    convert::Infallible,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use dal_core::{
    AssistantPart, CallId, ContextItem, Family, ModelRequest, ModelRoute, ModelToolSpec, Part,
    Purpose, RawJson, ReplaySource, RequestParams, SessionId, ThinkingLevel, Usage,
};
use dal_provider::{
    AuthStyle, CatalogEntry, CompactOutcome, Effort, EnvSnapshot, EventStream, Listing, NoticeSink,
    Provider, ProviderConfig, ProviderEntry, ProviderError, ProviderIdentity, ProviderSet,
    ProviderSettings, ResolvedModel, StopReason, StreamEvent, ThinkingSupport, ToolArgs, ToolCall,
    ToolSupport, Transport,
};
use hyper::{
    Request, Response,
    body::{Body, Bytes, Frame, Incoming, SizeHint},
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use tokio_util::sync::CancellationToken;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;

/// The wire model id every case in this suite sends.
const MODEL: &str = "claude-test-1";

/// Synthetic API-key credential: isolated to this suite, never a real secret.
const API_KEY: &str = "synthetic-anthropic-key-0001";
/// Synthetic bearer credential: isolated to this suite, never a real secret.
const BEARER_KEY: &str = "synthetic-anthropic-bearer-0001";
/// Environment name for the synthetic API-key credential.
const KEY_ENV: &str = "DAL_REPLAY_ANTHROPIC_KEY";
/// Environment name for the synthetic bearer credential.
const BEARER_ENV: &str = "DAL_REPLAY_ANTHROPIC_BEARER";
/// The exact `authorization` header value the bearer case sends.
const BEARER_HEADER: &str = "Bearer synthetic-anthropic-bearer-0001";
/// The exact `authorization` header value the OAuth cases send.
const OAUTH_HEADER: &str = "Bearer synthetic-claude-access-0001";

/// The synthetic `auth.json` store for the OAuth cases: one `oauth` member
/// for the built-in `anthropic` provider, with no expiry so no refresh runs.
const OAUTH_STORE: &str = r#"{"anthropic":{"kind":"oauth","access_token":"synthetic-claude-access-0001","refresh_token":"synthetic-claude-refresh-0001"}}"#;

fn identity() -> ProviderIdentity {
    ProviderIdentity {
        version: "test".into(),
        os: "linux".into(),
        os_version: "test".into(),
        arch: "x86_64".into(),
    }
}

fn entry(provider: &str, model: &str) -> CatalogEntry {
    CatalogEntry {
        provider: provider.into(),
        id: model.into(),
        display: model.into(),
        listing: Listing::Listed,
        context_window: Some(32_000),
        max_output: Some(4_096),
        thinking: ThinkingSupport::Adaptive {
            can_disable: true,
            accepted: vec![Effort::Low, Effort::Medium, Effort::High],
        },
        image_input: false,
        image_profile: None,
        remote_compact: false,
        supports_reasoning_summaries: false,
        tool_support: ToolSupport::Any,
        temperature_allowed: false,
        display_supported: true,
        custom_grammar: false,
        premium_multiplier: None,
        price: None,
    }
}

fn compact_entry(provider: &str, model: &str) -> CatalogEntry {
    CatalogEntry {
        remote_compact: true,
        ..entry(provider, model)
    }
}

fn params(thinking: ThinkingLevel) -> RequestParams {
    RequestParams {
        thinking,
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

fn web_search_tool() -> ModelToolSpec {
    ModelToolSpec {
        name: "web_search".into(),
        description: "Search the web.".into(),
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

/// The minimal turn every behavior case sends: one user message, no tools,
/// thinking off, no cache key.
fn minimal_request() -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::Anthropic,
            model: MODEL.into(),
            provider: None,
        },
        system: Arc::from("Be brief."),
        tools: Arc::from([]),
        context: Arc::from([user_text("Look")]),
        params: params(ThinkingLevel::Off),
        cache_key: None,
    }
}

/// The snapshot turn: adaptive thinking, one advertised tool, and the exact
/// body `request.json` of the `request_snapshot` fixture pins byte for byte.
fn snapshot_request() -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::Anthropic,
            model: MODEL.into(),
            provider: None,
        },
        system: Arc::from("Be brief."),
        tools: Arc::from([read_tool()]),
        context: Arc::from([user_text("Look")]),
        params: params(ThinkingLevel::Medium),
        cache_key: None,
    }
}

fn compact_request() -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Compact,
        model: ModelRoute::Api {
            family: Family::Anthropic,
            model: MODEL.into(),
            provider: None,
        },
        system: Arc::from(""),
        tools: Arc::from([]),
        context: Arc::from([user_text("Summarize this.")]),
        params: params(ThinkingLevel::Off),
        cache_key: None,
    }
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "dalgona-replay-anthropic-{tag}-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn build_set(
    base: String,
    dir: &std::path::Path,
    provider_id: &str,
    key_env: Option<&str>,
    auth: AuthStyle,
    env: &[(&str, &str)],
    auth_json: Option<&str>,
    request_max_retries: u32,
    stream_max_retries: u32,
) -> ProviderSet {
    if let Some(store) = auth_json {
        std::fs::write(dir.join("auth.json"), store).expect("auth store writes");
        #[cfg(unix)]
        std::fs::set_permissions(
            dir.join("auth.json"),
            std::fs::Permissions::from_mode(0o600),
        )
        .expect("auth store perms");
    }
    let config = ProviderConfig {
        default_model: None,
        thinking: ThinkingLevel::Off,
        aliases: Vec::new(),
        request_max_retries,
        stream_max_retries,
        providers: vec![ProviderEntry {
            id: provider_id.into(),
            def: None,
            family: Family::Anthropic,
            base_url: base.into_boxed_str(),
            transport: Transport::Https,
            key_env: key_env.map(Into::into),
            auth,
            max_concurrent_requests: 4,
            settings: ProviderSettings::default(),
        }],
        scripted: None,
    };
    ProviderSet::new(&config, identity(), EnvSnapshot::test(env), dir, dir)
        .expect("provider set builds")
}

struct Harness {
    server: replay_server::ReplayServer,
    dir: std::path::PathBuf,
    set: ProviderSet,
}

impl Harness {
    async fn start_api(case: &str, stream_max_retries: u32) -> Self {
        let case = replay_server::fixture_dir("anthropic", case);
        let server = replay_server::ReplayServer::start(&case)
            .await
            .expect("replay server starts");
        let base = server.base_url();
        let dir = temp_dir(case_name(&case));
        let set = build_set(
            base,
            &dir,
            "anthropic-test",
            Some(KEY_ENV),
            AuthStyle::XApiKey,
            &[(KEY_ENV, API_KEY)],
            None,
            0,
            stream_max_retries,
        );
        Self { server, dir, set }
    }

    async fn start_oauth(case: &str) -> Self {
        let case = replay_server::fixture_dir("anthropic", case);
        let server = replay_server::ReplayServer::start(&case)
            .await
            .expect("replay server starts");
        let base = server.base_url();
        let dir = temp_dir(case_name(&case));
        let set = build_set(
            base,
            &dir,
            "anthropic",
            None,
            AuthStyle::XApiKey,
            &[],
            Some(OAUTH_STORE),
            0,
            0,
        );
        Self { server, dir, set }
    }

    fn provider(&self, provider_id: &str) -> Provider {
        self.set
            .provider(ResolvedModel {
                provider: provider_id.into(),
                route: ModelRoute::Api {
                    family: Family::Anthropic,
                    model: MODEL.into(),
                    provider: None,
                },
                entry: entry(provider_id, MODEL),
            })
            .expect("provider binds")
    }

    fn compact_provider(&self, provider_id: &str) -> Provider {
        self.set
            .provider(ResolvedModel {
                provider: provider_id.into(),
                route: ModelRoute::Api {
                    family: Family::Anthropic,
                    model: MODEL.into(),
                    provider: None,
                },
                entry: compact_entry(provider_id, MODEL),
            })
            .expect("compact provider binds")
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

fn case_name(case: &std::path::Path) -> &str {
    case.file_name()
        .and_then(|name| name.to_str())
        .expect("fixture case name")
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

fn usage(input: u64, output: u64) -> StreamEvent {
    StreamEvent::Usage {
        usage: Usage {
            input_tokens: input,
            cached_input_tokens: 0,
            output_tokens: output,
            reasoning_tokens: None,
            cache_write_tokens: 0,
            cost_usd: None,
            premium_requests: None,
        },
    }
}

fn done() -> StreamEvent {
    StreamEvent::ToolCallsDone { calls: Vec::new() }
}

fn stop(reason: StopReason) -> StreamEvent {
    StreamEvent::Stop { reason }
}

#[tokio::test]
async fn anthropic_thinking_stream_signed_replay() {
    let harness = Harness::start_api("thinking_stream", 0).await;
    let (notices, _) = Harness::notices();
    let cancel = CancellationToken::new();
    let stream = harness
        .provider("anthropic-test")
        .open(
            SessionId::new_v7(),
            &minimal_request(),
            &[],
            notices,
            &cancel,
        )
        .await
        .expect("anthropic turn opens");
    let events = ok_events(drain(stream).await);
    assert_eq!(events.len(), 7);
    assert!(
        matches!(&events[0], StreamEvent::ReasoningDelta { text } if text == "deep "),
        "first reasoning delta arrives"
    );
    assert!(
        matches!(&events[1], StreamEvent::ReasoningDelta { text } if text == "thought"),
        "second reasoning delta arrives"
    );
    assert!(
        matches!(&events[2], StreamEvent::Replay { payload }
            if payload.family == Family::Anthropic
                && payload.model.as_ref() == MODEL
                && payload.item.as_str().contains(r#""type":"thinking""#)
                && payload.item.as_str().contains(r#""thinking":"deep thought""#)
                && payload.item.as_str().contains(r#""signature":"EqQBCgIYAhIM""#)
                && payload.item.as_str().contains("\"dal_prefix\"")),
        "signed thinking replays with its signature and producing prefix"
    );
    assert_eq!(
        events[3],
        StreamEvent::TextDelta {
            text: "The answer is 21.".into()
        }
    );
    assert_eq!(events[4], done());
    assert_eq!(events[5], usage(25, 12));
    assert_eq!(events[6], stop(StopReason::EndTurn));
    harness.finish(1).await;
}

#[tokio::test]
async fn anthropic_tool_fragments_assemble_mid_key() {
    let harness = Harness::start_api("tool_fragments", 0).await;
    let (notices, _) = Harness::notices();
    let cancel = CancellationToken::new();
    let stream = harness
        .provider("anthropic-test")
        .open(
            SessionId::new_v7(),
            &minimal_request(),
            &[],
            notices,
            &cancel,
        )
        .await
        .expect("anthropic turn opens");
    let events = ok_events(drain(stream).await);
    assert_eq!(events.len(), 7);
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
    assert!(
        matches!(&events[4], StreamEvent::ToolCallsDone { calls } if calls
        == &[ToolCall {
            id: "toolu_01".into(),
            name: "get_weather".into(),
            args: ToolArgs::Parsed(
                RawJson::parse(r#"{"location":"San Francisco, CA"}"#).expect("call args")
            ),
        }])
    );
    assert_eq!(events[5], usage(25, 891));
    assert_eq!(events[6], stop(StopReason::ToolUse));
    harness.finish(1).await;
}

#[tokio::test]
async fn anthropic_usage_accumulates_start_and_deltas() {
    let harness = Harness::start_api("usage_merge", 0).await;
    let (notices, _) = Harness::notices();
    let cancel = CancellationToken::new();
    let stream = harness
        .provider("anthropic-test")
        .open(
            SessionId::new_v7(),
            &minimal_request(),
            &[],
            notices,
            &cancel,
        )
        .await
        .expect("anthropic turn opens");
    let events = ok_events(drain(stream).await);
    assert_eq!(
        events,
        vec![done(), usage(25, 891), stop(StopReason::EndTurn)]
    );
    harness.finish(1).await;
}

#[tokio::test]
async fn anthropic_stop_pause() {
    let harness = Harness::start_api("stop_pause", 0).await;
    let (notices, _) = Harness::notices();
    let cancel = CancellationToken::new();
    let stream = harness
        .provider("anthropic-test")
        .open(
            SessionId::new_v7(),
            &minimal_request(),
            &[],
            notices,
            &cancel,
        )
        .await
        .expect("anthropic turn opens");
    let events = ok_events(drain(stream).await);
    assert_eq!(
        events,
        vec![
            StreamEvent::TextDelta { text: "v".into() },
            done(),
            usage(5, 0),
            stop(StopReason::Paused),
        ]
    );
    harness.finish(1).await;
}

#[tokio::test]
async fn anthropic_stop_refusal() {
    let harness = Harness::start_api("stop_refusal", 0).await;
    let (notices, _) = Harness::notices();
    let cancel = CancellationToken::new();
    let stream = harness
        .provider("anthropic-test")
        .open(
            SessionId::new_v7(),
            &minimal_request(),
            &[],
            notices,
            &cancel,
        )
        .await
        .expect("anthropic turn opens");
    let events = ok_events(drain(stream).await);
    assert_eq!(
        events,
        vec![
            StreamEvent::TextDelta { text: "v".into() },
            done(),
            usage(5, 0),
            stop(StopReason::Refusal),
        ]
    );
    harness.finish(1).await;
}

#[tokio::test]
async fn anthropic_stop_other() {
    let harness = Harness::start_api("stop_other", 0).await;
    let (notices, _) = Harness::notices();
    let cancel = CancellationToken::new();
    let stream = harness
        .provider("anthropic-test")
        .open(
            SessionId::new_v7(),
            &minimal_request(),
            &[],
            notices,
            &cancel,
        )
        .await
        .expect("anthropic turn opens");
    let events = ok_events(drain(stream).await);
    assert_eq!(
        events,
        vec![
            StreamEvent::TextDelta { text: "v".into() },
            done(),
            usage(5, 0),
            stop(StopReason::Other("model_context_window_exceeded".into())),
        ]
    );
    harness.finish(1).await;
}

#[tokio::test]
async fn anthropic_overload_before_delivery_retries_once() {
    let harness = Harness::start_api("overload_retry", 1).await;
    let (notices, captured) = Harness::notices();
    let cancel = CancellationToken::new();
    let stream = harness
        .provider("anthropic-test")
        .open(
            SessionId::new_v7(),
            &minimal_request(),
            &[],
            notices,
            &cancel,
        )
        .await
        .expect("anthropic turn opens");
    let events = ok_events(drain(stream).await);
    assert_eq!(
        events,
        vec![
            StreamEvent::TextDelta {
                text: "Recovered.".into()
            },
            done(),
            usage(8, 2),
            stop(StopReason::EndTurn),
        ]
    );
    {
        let seen = captured.lock().expect("notices lock");
        assert_eq!(seen.len(), 1, "exactly one retry notice");
    }
    harness.finish(2).await;
}

#[tokio::test]
async fn anthropic_overload_after_delivery_is_terminal() {
    let harness = Harness::start_api("overload_late", 1).await;
    let (notices, captured) = Harness::notices();
    let cancel = CancellationToken::new();
    let stream = harness
        .provider("anthropic-test")
        .open(
            SessionId::new_v7(),
            &minimal_request(),
            &[],
            notices,
            &cancel,
        )
        .await
        .expect("anthropic turn opens");
    let events = drain(stream).await;
    assert_eq!(events.len(), 2);
    assert!(
        matches!(&events[0], Ok(StreamEvent::TextDelta { text }) if text == "Hi"),
        "the delivered delta stays"
    );
    assert!(
        matches!(events[1], Err(ProviderError::Overloaded)),
        "post-delivery overload ends the stream"
    );
    assert!(
        captured.lock().expect("notices lock").is_empty(),
        "no retry after delivery"
    );
    harness.finish(1).await;
}

#[tokio::test]
async fn anthropic_empty_signature_never_replayed() {
    let harness = Harness::start_api("empty_signature", 0).await;
    let (notices, _) = Harness::notices();
    let cancel = CancellationToken::new();
    let stream = harness
        .provider("anthropic-test")
        .open(
            SessionId::new_v7(),
            &minimal_request(),
            &[],
            notices,
            &cancel,
        )
        .await
        .expect("anthropic turn opens");
    let events = ok_events(drain(stream).await);
    assert_eq!(
        events,
        vec![
            StreamEvent::ReasoningDelta { text: "hm".into() },
            StreamEvent::TextDelta {
                text: "done".into()
            },
            done(),
            usage(25, 7),
            stop(StopReason::EndTurn),
        ]
    );
    harness.finish(1).await;
}

#[tokio::test]
async fn anthropic_request_snapshot_exact() {
    let harness = Harness::start_api("request_snapshot", 0).await;
    let (notices, _) = Harness::notices();
    let cancel = CancellationToken::new();
    let stream = harness
        .provider("anthropic-test")
        .open(
            SessionId::new_v7(),
            &snapshot_request(),
            &[],
            notices,
            &cancel,
        )
        .await
        .expect("anthropic turn opens");
    let events = ok_events(drain(stream).await);
    assert_eq!(
        events,
        vec![
            StreamEvent::TextDelta {
                text: "Done.".into()
            },
            done(),
            usage(10, 4),
            stop(StopReason::EndTurn),
        ]
    );
    harness.finish(1).await;
}

/// The native tool-search session: the first turn defers two tools, the model
/// searches and calls the discovered one, and the next turn (that tool now
/// loading up front, one still deferred) sends the stored search pair back
/// byte for byte. The server compares both request bodies exactly.
#[tokio::test]
async fn anthropic_native_tool_search_session_replays_the_search_pair() {
    let harness = Harness::start_api("tool_search", 0).await;
    let provider = harness.provider("anthropic-test");
    let deferred = |name: &str, description: &str| ModelToolSpec {
        name: name.into(),
        description: description.into(),
        parameters: RawJson::parse(r#"{"type":"object","properties":{}}"#).expect("tool schema"),
        grammar: None,
        defer_loading: true,
    };
    let grep = deferred("grep", "Search files.");

    let mut first = snapshot_request();
    first.tools = Arc::from([
        read_tool(),
        grep.clone(),
        deferred("web_search", "Search the web."),
    ]);
    let (notices, _) = Harness::notices();
    let cancel = CancellationToken::new();
    let stream = provider
        .open(SessionId::new_v7(), &first, &[], notices, &cancel)
        .await
        .expect("first turn opens");
    let events = ok_events(drain(stream).await);
    let stored = events
        .iter()
        .find_map(|event| match event {
            StreamEvent::Replay { payload } => Some(payload.item.clone()),
            _ => None,
        })
        .expect("the search pair is stored for replay");
    let [call] = events
        .iter()
        .find_map(|event| match event {
            StreamEvent::ToolCallsDone { calls } => Some(calls.as_slice()),
            _ => None,
        })
        .expect("the response ends its calls")
    else {
        panic!("one discovered tool is called");
    };
    assert_eq!(call.name, "web_search");
    let ToolArgs::Parsed(args) = call.args.clone() else {
        panic!("the call arguments parse");
    };

    let mut second = snapshot_request();
    second.tools = Arc::from([read_tool(), web_search_tool(), grep]);
    second.context = Arc::from([
        user_text("Look"),
        ContextItem::Assistant {
            source: ReplaySource {
                family: Family::Anthropic,
                model: MODEL.into(),
            },
            replay: None,
            parts: vec![
                AssistantPart::Thinking {
                    text: String::new().into(),
                    replay: Some(stored),
                },
                AssistantPart::ToolCall {
                    call: CallId::new(call.id.clone()),
                    name: call.name.clone().into(),
                    args,
                    replay: None,
                },
            ],
        },
        ContextItem::ToolResult {
            call: CallId::new(call.id.clone()),
            name: call.name.clone().into(),
            is_error: false,
            parts: vec![Part::Text {
                text: "found".into(),
            }],
        },
    ]);
    let (notices, _) = Harness::notices();
    let stream = provider
        .open(SessionId::new_v7(), &second, &[], notices, &cancel)
        .await
        .expect("second turn opens");
    assert_eq!(
        ok_events(drain(stream).await),
        vec![
            StreamEvent::TextDelta {
                text: "Done.".into()
            },
            done(),
            usage(50, 4),
            stop(StopReason::EndTurn),
        ]
    );
    harness.finish(2).await;
}

#[tokio::test]
async fn anthropic_remote_compact_returns_block() {
    let harness = Harness::start_api("compact_summarize", 0).await;
    let provider = harness.compact_provider("anthropic-test");
    let (notices, _) = Harness::notices();
    let cancel = CancellationToken::new();
    let outcome = provider
        .compact(SessionId::new_v7(), &compact_request(), notices, &cancel)
        .await
        .expect("anthropic compaction runs");
    let Some(CompactOutcome::Compacted(history)) = outcome else {
        panic!("expected an Anthropic compaction block, got {outcome:?}");
    };
    assert_eq!(history.family, Family::Anthropic);
    assert_eq!(history.model.as_ref(), MODEL);
    assert_eq!(history.items.len(), 1);
    assert_eq!(
        history.items[0].as_str(),
        r#"{"type":"compaction","summary":"Prior work recap."}"#
    );
    harness.finish(1).await;
}

#[tokio::test]
async fn anthropic_remote_compact_without_block_errors() {
    let harness = Harness::start_api("compact_missing", 0).await;
    let provider = harness.compact_provider("anthropic-test");
    let (notices, _) = Harness::notices();
    let cancel = CancellationToken::new();
    let error = provider
        .compact(SessionId::new_v7(), &compact_request(), notices, &cancel)
        .await
        .expect_err("a response without a compaction block fails");
    assert!(matches!(
        error,
        ProviderError::CompactionMissing {
            family: Family::Anthropic,
            noun: "block",
        }
    ));
    assert_eq!(
        error.to_string(),
        "anthropic compaction returned no compaction block."
    );
    harness.finish(1).await;
}

#[tokio::test]
async fn anthropic_oauth_wire_framing() {
    let harness = Harness::start_oauth("oauth_headers").await;
    let (notices, _) = Harness::notices();
    let cancel = CancellationToken::new();
    let stream = harness
        .provider("anthropic")
        .open(
            SessionId::new_v7(),
            &minimal_request(),
            &[],
            notices,
            &cancel,
        )
        .await
        .expect("anthropic turn opens");
    let events = ok_events(drain(stream).await);
    assert_eq!(
        events,
        vec![
            StreamEvent::TextDelta {
                text: "Hi via OAuth.".into()
            },
            done(),
            usage(6, 3),
            stop(StopReason::EndTurn),
        ]
    );
    harness.finish(1).await;
}

#[tokio::test]
async fn anthropic_cancel_mid_stream_stays_pending() {
    let harness = Harness::start_api("cancel_stall", 0).await;
    let (notices, _) = Harness::notices();
    let cancel = CancellationToken::new();
    let mut stream = harness
        .provider("anthropic-test")
        .open(
            SessionId::new_v7(),
            &minimal_request(),
            &[],
            notices,
            &cancel,
        )
        .await
        .expect("anthropic turn opens");
    let first = stream
        .next()
        .await
        .expect("stream yields")
        .expect("no stream error");
    assert!(
        matches!(first, StreamEvent::TextDelta { text } if text == "half"),
        "the pre-cancel delta arrives"
    );
    cancel.cancel();
    let pending = tokio::time::timeout(std::time::Duration::from_millis(200), stream.next()).await;
    assert!(pending.is_err(), "a cancelled turn emits no terminal event");
    let closed = tokio::time::timeout(std::time::Duration::from_millis(200), async {
        while harness.server.open_requests() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        closed.is_ok(),
        "cancellation closes the stalled transport before stream drop"
    );
    drop(stream);
    let Harness { server, dir, .. } = harness;
    assert!(
        server.mismatches().is_empty(),
        "replay mismatches: {:?}",
        server.mismatches()
    );
    assert_eq!(server.consumed(), 1);
    server.finish().await.expect("replay consumed");
    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// Header-distinction capture service.
//
// The shared replay server only checks listed headers, so header absence is
// unobservable there. These cases serve one canned turn from a loopback
// capture service instead and assert the full security-relevant header set:
// presence, exact values, and absence. Assertion messages never carry
// credential values.

/// One request the capture service recorded.
#[derive(Clone, Debug)]
struct Recorded {
    method: String,
    path: String,
    values: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Recorded {
    fn value(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn has(&self, name: &str) -> bool {
        self.values.iter().any(|(key, _)| key == name)
    }

    fn body_text(&self) -> &str {
        std::str::from_utf8(&self.body).expect("request body is UTF-8")
    }
}

/// A one-shot response body: the canned turn bytes, then end of stream.
struct OnceBody {
    bytes: &'static [u8],
    sent: bool,
}

impl Body for OnceBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        if this.sent {
            return Poll::Ready(None);
        }
        this.sent = true;
        Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(this.bytes)))))
    }

    fn is_end_stream(&self) -> bool {
        self.sent
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(u64::try_from(self.bytes.len()).unwrap_or(u64::MAX))
    }
}

/// The canned turn every capture case serves.
const CAPTURE_SSE: &[u8] = br#"event: message_start
data: {"type":"message_start","message":{"id":"msg_cap","usage":{"input_tokens":3}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}

event: message_stop
data: {"type":"message_stop"}
"#;

async fn read_body(mut body: Incoming) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(frame) = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await {
        let frame = frame.expect("request body reads");
        if let Ok(data) = frame.into_data() {
            out.extend_from_slice(&data);
        }
    }
    out
}

async fn handle_capture(
    request: Request<Incoming>,
    recorded: Arc<Mutex<Vec<Recorded>>>,
) -> Result<Response<OnceBody>, Infallible> {
    let (parts, body) = request.into_parts();
    let bytes = read_body(body).await;
    recorded.lock().expect("capture lock").push(Recorded {
        method: parts.method.to_string(),
        path: parts.uri.path().to_owned(),
        values: parts
            .headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    value.to_str().unwrap_or("").to_owned(),
                )
            })
            .collect(),
        body: bytes,
    });
    Ok(Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .body(OnceBody {
            bytes: CAPTURE_SSE,
            sent: false,
        })
        .expect("capture response builds"))
}

/// Serves one canned turn per connection on a fresh loopback port and records
/// every request. Returns the base URL and the recording.
async fn serve_capture() -> (String, Arc<Mutex<Vec<Recorded>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("capture binds");
    let base = format!("http://{}", listener.local_addr().expect("capture address"));
    let recorded: Arc<Mutex<Vec<Recorded>>> = Arc::default();
    tokio::spawn({
        let recorded = Arc::clone(&recorded);
        async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let recorded = Arc::clone(&recorded);
                tokio::spawn(async move {
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(
                            TokioIo::new(stream),
                            service_fn(move |request| {
                                let recorded = Arc::clone(&recorded);
                                async move { handle_capture(request, recorded).await }
                            }),
                        )
                        .await;
                });
            }
        }
    });
    (base, recorded)
}

/// Opens one turn against the capture service and returns its events plus
/// the recorded request.
async fn capture_turn(
    provider_id: &str,
    key_env: Option<&str>,
    env: &[(&str, &str)],
    auth: AuthStyle,
    auth_json: Option<&str>,
    request: &ModelRequest,
) -> (Vec<StreamEvent>, Recorded) {
    let (base, recorded) = serve_capture().await;
    let dir = temp_dir(provider_id);
    let set = build_set(base, &dir, provider_id, key_env, auth, env, auth_json, 0, 0);
    let provider = set
        .provider(ResolvedModel {
            provider: provider_id.into(),
            route: ModelRoute::Api {
                family: Family::Anthropic,
                model: MODEL.into(),
                provider: None,
            },
            entry: entry(provider_id, MODEL),
        })
        .expect("provider binds");
    let (notices, _) = Harness::notices();
    let cancel = CancellationToken::new();
    let stream = provider
        .open(SessionId::new_v7(), request, &[], notices, &cancel)
        .await
        .expect("anthropic turn opens");
    let events = ok_events(drain(stream).await);
    let seen = recorded.lock().expect("capture lock");
    assert_eq!(seen.len(), 1, "exactly one request reaches the server");
    let first = seen[0].clone();
    drop(seen);
    drop(set);
    std::fs::remove_dir_all(&dir).ok();
    (events, first)
}

fn captured_turn() -> Vec<StreamEvent> {
    vec![
        StreamEvent::TextDelta { text: "ok".into() },
        done(),
        usage(3, 1),
        stop(StopReason::EndTurn),
    ]
}

#[tokio::test]
async fn anthropic_api_key_header_distinction() {
    let request = ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::Anthropic,
            model: MODEL.into(),
            provider: None,
        },
        system: Arc::from("Be brief."),
        tools: Arc::from([read_tool()]),
        context: Arc::from([user_text("Look")]),
        params: params(ThinkingLevel::Medium),
        cache_key: None,
    };
    let (events, recorded) = capture_turn(
        "anthropic-key",
        Some(KEY_ENV),
        &[(KEY_ENV, API_KEY)],
        AuthStyle::XApiKey,
        None,
        &request,
    )
    .await;
    assert_eq!(events, captured_turn());
    assert_eq!(recorded.method, "POST");
    assert_eq!(recorded.path, "/v1/messages");
    assert_eq!(recorded.value("anthropic-version"), Some("2023-06-01"));
    assert!(
        recorded.value("x-api-key") == Some(API_KEY),
        "api-key request carries its key header"
    );
    assert!(
        !recorded.has("authorization"),
        "api-key request sends no bearer header"
    );
    assert!(
        !recorded.has("anthropic-beta"),
        "api-key adaptive request sends no beta header"
    );
    assert!(
        !recorded.has("x-app"),
        "api-key request sends no app marker"
    );
    assert!(
        recorded
            .value("user-agent")
            .is_some_and(|agent| !agent.contains("claude-cli")),
        "api-key request keeps the library user-agent"
    );
    let body = recorded.body_text();
    assert!(
        body.contains(r#""text":"Be brief.""#),
        "api-key request keeps the caller system block"
    );
    assert!(
        !body.contains("Claude Code"),
        "api-key request adds no OAuth identity block"
    );
    assert!(
        body.contains(r#""name":"read""#),
        "api-key request keeps plain tool names"
    );
    assert!(
        !body.contains(r#""name":"_read""#),
        "api-key request adds no OAuth tool prefix"
    );
}

#[tokio::test]
async fn anthropic_bearer_header_distinction() {
    let (events, recorded) = capture_turn(
        "anthropic-bearer",
        Some(BEARER_ENV),
        &[(BEARER_ENV, BEARER_KEY)],
        AuthStyle::Bearer,
        None,
        &minimal_request(),
    )
    .await;
    assert_eq!(events, captured_turn());
    assert_eq!(recorded.method, "POST");
    assert_eq!(recorded.path, "/v1/messages");
    assert!(
        recorded.value("authorization") == Some(BEARER_HEADER),
        "bearer request carries its bearer header"
    );
    assert!(
        !recorded.has("x-api-key"),
        "bearer request sends no api-key header"
    );
    assert!(
        !recorded.has("anthropic-beta"),
        "bearer request sends no beta header"
    );
    assert!(!recorded.has("x-app"), "bearer request sends no app marker");
    assert!(
        recorded
            .value("user-agent")
            .is_some_and(|agent| !agent.contains("claude-cli")),
        "bearer request keeps the library user-agent"
    );
    assert!(
        !recorded.body_text().contains("Claude Code"),
        "bearer request adds no OAuth identity block"
    );
}

#[tokio::test]
async fn anthropic_oauth_header_distinction() {
    let request = ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::Anthropic,
            model: MODEL.into(),
            provider: None,
        },
        system: Arc::from("Be brief."),
        tools: Arc::from([read_tool(), web_search_tool()]),
        context: Arc::from([user_text("Look")]),
        params: params(ThinkingLevel::Off),
        cache_key: None,
    };
    let (events, recorded) = capture_turn(
        "anthropic",
        None,
        &[],
        AuthStyle::XApiKey,
        Some(OAUTH_STORE),
        &request,
    )
    .await;
    assert_eq!(events, captured_turn());
    assert_eq!(recorded.method, "POST");
    assert_eq!(recorded.path, "/v1/messages");
    assert!(
        recorded.value("authorization") == Some(OAUTH_HEADER),
        "oauth request carries its bearer token"
    );
    assert!(
        recorded.value("anthropic-beta") == Some("claude-code-20250219,oauth-2025-04-20"),
        "oauth request carries the fingerprint betas first"
    );
    assert_eq!(recorded.value("x-app"), Some("cli"));
    assert_eq!(
        recorded.value("user-agent"),
        Some("claude-cli/2.1.280 (external, cli)")
    );
    assert!(
        !recorded.has("x-api-key"),
        "oauth request sends no api-key header"
    );
    let body = recorded.body_text();
    assert!(
        body.contains(r#""text":"You are Claude Code, Anthropic's official CLI for Claude.""#),
        "oauth request leads with the identity block"
    );
    assert!(
        body.contains(r#""name":"_read""#),
        "oauth request prefixes the custom tool name"
    );
    assert!(
        body.contains(r#""name":"web_search""#),
        "oauth request keeps the built-in tool name"
    );
    assert!(
        !body.contains(r#""name":"_web_search""#),
        "oauth request never prefixes the built-in tool name"
    );
}
