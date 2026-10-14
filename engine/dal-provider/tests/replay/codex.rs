//! Codex HTTPS replay integration suite.
//!
//! The public provider drives an HTTP replay server on loopback. The server
//! checks the Codex route, headers, and request body. The tests exercise
//! streamed decoding, cancellation, and remote compaction without live
//! credentials or a TLS endpoint.

use super::replay_server;

use std::sync::{Arc, Mutex};

use dal_core::{
    ContextItem, Family, ModelRequest, ModelRoute, Part, Purpose, RequestParams, SessionId,
    ThinkingLevel,
};
use dal_provider::{
    AuthStore, AuthStyle, BeforeRequest, BeforeRequestInput, BeforeRequestPatch, CatalogEntry,
    CompactOutcome, Credential, EnvSnapshot, Listing, NoticeSink, OAuthCredential, ProviderConfig,
    ProviderEntry, ProviderError, ProviderIdentity, ProviderSet, ResolvedModel, SecretString,
    StopReason, StreamEvent, ThinkingSupport, ToolSupport, Transport,
};
use futures::future::BoxFuture;
use tokio_util::sync::CancellationToken;

/// Pinned session id, so wire bytes are byte-equal with the fixtures.
const SESSION: &str = "0193f5a1-4d2c-7a1b-9c3d-5e6f7a8b9c0d";
const CACHE_KEY: &str = "0193f5a1-4d2c-7a1b-9c3d-5e6f7a8b9c0d:1";
const MODEL: &str = "gpt-test";
/// Fake bearer token; never a live secret. Fixtures only assert its presence.
const ACCESS_TOKEN: &str = "test-access-token-9d2e";
const REFRESH_TOKEN: &str = "test-refresh-token-3f1a";
/// Fake account id; never a live secret. Fixtures only assert its presence.
const ACCOUNT_ID: &str = "test-account-7f3a9c";
/// Fake three-part ID token whose claims agree with `ACCOUNT_ID`. The wire
/// uses the stored account id, so this token is never parsed on this path.
const ID_TOKEN: &str = "test-header.eyJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9hY2NvdW50X2lkIjoidGVzdC1hY2NvdW50LTdmM2E5YyJ9fQ.test-signature";

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
        provider: "openai-codex".into(),
        id: MODEL.into(),
        display: MODEL.into(),
        listing: Listing::Listed,
        context_window: Some(32_000),
        max_output: Some(4_096),
        thinking: ThinkingSupport::OpenAi {
            accepted: vec![ThinkingLevel::High],
            none_supported: true,
        },
        image_input: false,
        image_profile: None,
        remote_compact: true,
        supports_reasoning_summaries: true,
        tool_support: ToolSupport::Any,
        temperature_allowed: false,
        display_supported: false,
        custom_grammar: false,
    }
}

fn entry_without_summaries() -> CatalogEntry {
    CatalogEntry {
        supports_reasoning_summaries: false,
        ..entry()
    }
}

/// A hook that sets a sampling temperature the Codex catalog row forbids.
struct TemperatureHook;

impl BeforeRequest for TemperatureHook {
    fn before_request<'a>(
        &'a self,
        _input: BeforeRequestInput<'a>,
    ) -> BoxFuture<'a, Result<BeforeRequestPatch, ProviderError>> {
        Box::pin(async move {
            Ok(BeforeRequestPatch {
                effort: None,
                temperature: Some(0.9),
            })
        })
    }
}

fn session() -> SessionId {
    SessionId::parse(SESSION).expect("pinned session id parses")
}

fn request(purpose: Purpose) -> ModelRequest {
    ModelRequest {
        purpose,
        model: ModelRoute::Api {
            family: Family::Codex,
            model: MODEL.into(),
        },
        system: Arc::from("You are terse."),
        tools: Arc::from([]),
        context: Arc::from([ContextItem::User {
            parts: vec![Part::Text { text: "Hi".into() }],
        }]),
        params: RequestParams {
            thinking: ThinkingLevel::High,
            effort: None,
            temperature: None,
            max_output_tokens: None,
        },
        cache_key: Some(CACHE_KEY.into()),
    }
}

fn seed_auth(dir: &std::path::Path) {
    let mut store = AuthStore::empty(dir.join("auth.json"));
    store
        .set(
            "openai-codex",
            Credential::OAuth(OAuthCredential {
                access_token: SecretString::from(ACCESS_TOKEN),
                refresh_token: SecretString::from(REFRESH_TOKEN),
                expires_at: None,
                id_token: Some(String::from(ID_TOKEN)),
                account_id: Some(String::from(ACCOUNT_ID)),
            }),
        )
        .expect("fake codex credential stores");
    store.store().expect("auth.json writes");
}

fn temp_dir(case: &str) -> std::path::PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "dalgona-replay-codex-{}-{stamp}-{case}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    seed_auth(&dir);
    dir
}

fn config(base: &str) -> ProviderConfig {
    ProviderConfig {
        default_model: None,
        thinking: ThinkingLevel::Medium,
        aliases: Vec::new(),
        request_max_retries: 2,
        stream_max_retries: 2,
        providers: vec![ProviderEntry {
            id: "openai-codex".into(),
            def: None,
            family: Family::Codex,
            base_url: base.into(),
            transport: Transport::Https,
            key_env: None,
            auth: AuthStyle::Bearer,
            max_concurrent_requests: 4,
        }],
        scripted: None,
    }
}

/// Binds one Codex provider against the replay server. The set is returned
/// alongside the provider because the provider only holds it weakly.
fn setup(
    base: &str,
    dir: &std::path::Path,
    entry: CatalogEntry,
) -> (ProviderSet, dal_provider::Provider) {
    let set = ProviderSet::new(&config(base), identity(), EnvSnapshot::test(&[]), dir, dir)
        .expect("provider set builds");
    let resolved = ResolvedModel {
        provider: "openai-codex".into(),
        route: ModelRoute::Api {
            family: Family::Codex,
            model: MODEL.into(),
        },
        entry,
    };
    let provider = set.provider(resolved).expect("provider binds");
    (set, provider)
}

#[tokio::test]
async fn codex_https_text_turn() {
    let case = replay_server::fixture_dir("codex", "https_text_turn");
    let server = replay_server::ReplayServer::start(&case)
        .await
        .expect("replay server starts");
    let dir = temp_dir("text-turn");
    let (_set, provider) = setup(&server.base_url(), &dir, entry());
    let notices: NoticeSink = Arc::new(|_| {});
    let cancel = CancellationToken::new();
    let hooks: Vec<Arc<dyn BeforeRequest>> = vec![Arc::new(TemperatureHook)];
    let stream = provider
        .open(session(), &request(Purpose::Turn), &hooks, notices, &cancel)
        .await
        .expect("codex turn opens");
    let mut order = Vec::new();
    let mut reasoning = String::new();
    let mut texts = String::new();
    let mut usage = None;
    let mut stop = None;
    let mut events = std::pin::pin!(stream);
    while let Some(event) = events.next().await {
        match event.expect("no stream error") {
            StreamEvent::ReasoningDelta { text } => {
                order.push("reasoning");
                reasoning.push_str(&text);
            }
            StreamEvent::TextDelta { text } => {
                order.push("text");
                texts.push_str(&text);
            }
            StreamEvent::ToolCallsDone { calls } => {
                order.push("done");
                assert!(calls.is_empty(), "expected no calls, got {calls:?}");
            }
            StreamEvent::Usage { usage: got } => {
                order.push("usage");
                usage = Some(got);
            }
            StreamEvent::Stop { reason } => {
                order.push("stop");
                stop = Some(reason);
            }
            _ => {}
        }
    }
    assert_eq!(order, ["reasoning", "text", "done", "usage", "stop"]);
    assert_eq!(reasoning, "Checking the plan.");
    assert_eq!(texts, "Hello");
    let usage = usage.expect("usage reported");
    assert_eq!(usage.input_tokens, 19);
    assert_eq!(usage.cached_input_tokens, 4);
    assert_eq!(usage.output_tokens, 10);
    assert_eq!(usage.reasoning_tokens, Some(3));
    assert_eq!(usage.cache_write_tokens, 0);
    assert!(usage.cost_usd.is_none());
    assert!(matches!(stop, Some(StopReason::EndTurn)));
    assert_eq!(server.addr().ip().to_string(), "127.0.0.1");
    assert_eq!(server.mismatches(), [] as [String; 0]);
    assert_eq!(server.consumed(), 1);
    server.finish().await.expect("replay consumed");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn codex_https_summary_omitted_without_capability() {
    let case = replay_server::fixture_dir("codex", "https_summary_off");
    let server = replay_server::ReplayServer::start(&case)
        .await
        .expect("replay server starts");
    let dir = temp_dir("summary-off");
    let (_set, provider) = setup(&server.base_url(), &dir, entry_without_summaries());
    let notices: NoticeSink = Arc::new(|_| {});
    let cancel = CancellationToken::new();
    let hooks: Vec<Arc<dyn BeforeRequest>> = vec![Arc::new(TemperatureHook)];
    let stream = provider
        .open(session(), &request(Purpose::Turn), &hooks, notices, &cancel)
        .await
        .expect("codex turn opens");
    let mut order = Vec::new();
    let mut texts = String::new();
    let mut stop = None;
    let mut events = std::pin::pin!(stream);
    while let Some(event) = events.next().await {
        match event.expect("no stream error") {
            StreamEvent::TextDelta { text } => {
                order.push("text");
                texts.push_str(&text);
            }
            StreamEvent::ToolCallsDone { calls } => {
                order.push("done");
                assert!(calls.is_empty(), "expected no calls, got {calls:?}");
            }
            StreamEvent::Usage { .. } => order.push("usage"),
            StreamEvent::Stop { reason } => {
                order.push("stop");
                stop = Some(reason);
            }
            _ => {}
        }
    }
    assert_eq!(order, ["text", "done", "usage", "stop"]);
    assert_eq!(texts, "Hi back.");
    assert!(matches!(stop, Some(StopReason::EndTurn)));
    assert_eq!(server.addr().ip().to_string(), "127.0.0.1");
    assert_eq!(server.mismatches(), [] as [String; 0]);
    assert_eq!(server.consumed(), 1);
    server.finish().await.expect("replay consumed");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn codex_https_failed_event_is_status_without_retry() {
    let case = replay_server::fixture_dir("codex", "https_failed_after_delivery");
    let server = replay_server::ReplayServer::start(&case)
        .await
        .expect("replay server starts");
    let dir = temp_dir("failed");
    let (_set, provider) = setup(&server.base_url(), &dir, entry());
    let notices: NoticeSink = Arc::new(|_| {});
    let cancel = CancellationToken::new();
    let hooks: Vec<Arc<dyn BeforeRequest>> = vec![Arc::new(TemperatureHook)];
    let stream = provider
        .open(session(), &request(Purpose::Turn), &hooks, notices, &cancel)
        .await
        .expect("codex turn opens");
    let mut events = std::pin::pin!(stream);
    let first = events.next().await.expect("first event arrives");
    assert!(matches!(
        first,
        Ok(StreamEvent::TextDelta { ref text }) if text == "Partial"
    ));
    let failure = events
        .next()
        .await
        .expect("failure arrives")
        .expect_err("in-stream failure is an error");
    assert_eq!(
        failure.to_string(),
        "codex error 200: codex backend exploded"
    );
    assert_eq!(server.addr().ip().to_string(), "127.0.0.1");
    assert_eq!(server.mismatches(), [] as [String; 0]);
    assert_eq!(server.consumed(), 1);
    server.finish().await.expect("replay consumed");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn codex_https_transport_failed_after_delivery_does_not_retry() {
    let case = replay_server::fixture_dir("codex", "https_transport_failed_after_delivery");
    let server = replay_server::ReplayServer::start(&case)
        .await
        .expect("replay server starts");
    let dir = temp_dir("transport-failed");
    let (_set, provider) = setup(&server.base_url(), &dir, entry());
    let notices: NoticeSink = Arc::new(|_| {});
    let cancel = CancellationToken::new();
    let hooks: Vec<Arc<dyn BeforeRequest>> = vec![Arc::new(TemperatureHook)];
    let stream = provider
        .open(session(), &request(Purpose::Turn), &hooks, notices, &cancel)
        .await
        .expect("codex turn opens");
    let mut events = std::pin::pin!(stream);
    let first = events.next().await.expect("first event arrives");
    assert!(matches!(
        first,
        Ok(StreamEvent::TextDelta { ref text }) if text == "Partial"
    ));
    let failure = events
        .next()
        .await
        .expect("failure arrives")
        .expect_err("transport failure is an error");
    assert!(matches!(
        failure,
        ProviderError::Transport {
            family: Family::Codex,
            ..
        }
    ));
    assert_eq!(server.addr().ip().to_string(), "127.0.0.1");
    assert_eq!(server.mismatches(), [] as [String; 0]);
    assert_eq!(server.consumed(), 1);
    server.finish().await.expect("replay consumed");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn codex_https_transport_failed_before_delivery_retries_once() {
    let case =
        replay_server::fixture_dir("codex", "https_transport_failed_before_delivery_retries");
    let server = replay_server::ReplayServer::start(&case)
        .await
        .expect("replay server starts");
    let dir = temp_dir("transport-retry");
    let (_set, provider) = setup(&server.base_url(), &dir, entry());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&seen);
    let notices: NoticeSink = Arc::new(move |notice| {
        captured.lock().expect("notices lock").push(notice);
    });
    let cancel = CancellationToken::new();
    let hooks: Vec<Arc<dyn BeforeRequest>> = vec![Arc::new(TemperatureHook)];
    let stream = provider
        .open(session(), &request(Purpose::Turn), &hooks, notices, &cancel)
        .await
        .expect("codex turn opens");
    let mut order = Vec::new();
    let mut texts = String::new();
    let mut usage = None;
    let mut stop = None;
    let mut events = std::pin::pin!(stream);
    while let Some(event) = events.next().await {
        match event.expect("retry recovers the pre-delivery transport cut") {
            StreamEvent::TextDelta { text } => {
                order.push("text");
                texts.push_str(&text);
            }
            StreamEvent::ToolCallsDone { calls } => {
                order.push("done");
                assert!(calls.is_empty(), "expected no calls, got {calls:?}");
            }
            StreamEvent::Usage { usage: got } => {
                order.push("usage");
                usage = Some(got);
            }
            StreamEvent::Stop { reason } => {
                order.push("stop");
                stop = Some(reason);
            }
            _ => {}
        }
    }
    assert_eq!(order, ["text", "done", "usage", "stop"]);
    assert_eq!(texts, "Recovered");
    let usage = usage.expect("usage reported");
    assert_eq!(usage.input_tokens, 19);
    assert_eq!(usage.cached_input_tokens, 4);
    assert_eq!(usage.output_tokens, 10);
    assert_eq!(usage.reasoning_tokens, Some(0));
    assert_eq!(usage.cache_write_tokens, 0);
    assert!(usage.cost_usd.is_none());
    assert!(matches!(stop, Some(StopReason::EndTurn)));
    let retries: Vec<String> = seen
        .lock()
        .expect("notices lock")
        .iter()
        .filter(|notice| notice.starts_with("Retrying in "))
        .cloned()
        .collect();
    assert_eq!(retries.len(), 1, "exactly one retry notice");
    assert_eq!(server.addr().ip().to_string(), "127.0.0.1");
    assert_eq!(server.mismatches(), [] as [String; 0]);
    assert_eq!(server.consumed(), 2);
    server.finish().await.expect("replay consumed");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn codex_https_consumer_cancel_closes_transfer() {
    let case = replay_server::fixture_dir("codex", "https_consumer_cancel");
    let server = replay_server::ReplayServer::start(&case)
        .await
        .expect("replay server starts");
    let dir = temp_dir("cancel");
    let (_set, provider) = setup(&server.base_url(), &dir, entry());
    let notices: NoticeSink = Arc::new(|_| {});
    let cancel = CancellationToken::new();
    let hooks: Vec<Arc<dyn BeforeRequest>> = vec![Arc::new(TemperatureHook)];
    let mut stream = provider
        .open(session(), &request(Purpose::Turn), &hooks, notices, &cancel)
        .await
        .expect("codex turn opens");
    let first = stream.next().await.expect("first event arrives");
    assert!(matches!(
        first,
        Ok(StreamEvent::TextDelta { ref text }) if text == "Partial"
    ));
    assert_eq!(
        server.open_requests(),
        1,
        "the fixture holds the response open"
    );
    drop(stream);
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while server.open_requests() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("dropping the stream closes the active transfer");
    assert_eq!(server.addr().ip().to_string(), "127.0.0.1");
    assert_eq!(server.mismatches(), [] as [String; 0]);
    assert_eq!(server.consumed(), 1);
    server.finish().await.expect("replay consumed");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn codex_https_secret_values_stay_out_of_verdict() {
    let case = replay_server::fixture_dir("codex", "https_secret_redaction");
    let server = replay_server::ReplayServer::start(&case)
        .await
        .expect("replay server starts");
    let dir = temp_dir("secrets");
    let (_set, provider) = setup(&server.base_url(), &dir, entry());
    let notices: NoticeSink = Arc::new(|_| {});
    let cancel = CancellationToken::new();
    let hooks: Vec<Arc<dyn BeforeRequest>> = vec![];
    let mut opened = provider
        .open(session(), &request(Purpose::Turn), &hooks, notices, &cancel)
        .await
        .expect("valid Codex request creates a lazy stream");
    let failure = opened
        .next()
        .await
        .expect("the refused exchange yields a result")
        .expect_err("refused exchange fails the turn");
    assert!(matches!(failure, ProviderError::Status { .. }));
    let mismatches = server.mismatches();
    assert_eq!(mismatches.len(), 1);
    assert!(
        mismatches[0].contains("header authorization"),
        "unexpected verdict: {}",
        mismatches[0]
    );
    assert!(
        mismatches[0].contains("<other value>"),
        "unexpected verdict: {}",
        mismatches[0]
    );
    assert!(!mismatches[0].contains(ACCESS_TOKEN));
    assert!(!mismatches[0].contains(ACCOUNT_ID));
    assert_eq!(server.consumed(), 1);
    let verdict = server.finish().await.expect_err("mismatch fails finish");
    assert!(
        verdict.to_string().contains("replay mismatch"),
        "unexpected verdict: {verdict}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn codex_remote_compaction_returns_triggered_history() {
    let case = replay_server::fixture_dir("codex", "https_compaction");
    let server = replay_server::ReplayServer::start(&case)
        .await
        .expect("replay server starts");
    let dir = temp_dir("compaction");
    let (_set, provider) = setup(&server.base_url(), &dir, entry());
    let notices: NoticeSink = Arc::new(|_| {});
    let cancel = CancellationToken::new();
    let outcome = provider
        .compact(session(), &request(Purpose::Compact), notices, &cancel)
        .await
        .expect("codex compaction runs");
    let Some(CompactOutcome::Compacted(history)) = outcome else {
        panic!("expected a compacted history, got {outcome:?}");
    };
    assert_eq!(history.family, Family::Codex);
    assert_eq!(history.model.as_ref(), MODEL);
    assert_eq!(history.items.len(), 2);
    assert_eq!(
        history.items[0].as_str(),
        r#"{"type":"message","role":"user","content":[{"type":"input_text","text":"Hi"}]}"#
    );
    assert_eq!(
        history.items[1].as_str(),
        r#"{"type":"compaction","id":"comp-1","summary":"Prior work recap."}"#
    );
    assert_eq!(server.addr().ip().to_string(), "127.0.0.1");
    assert_eq!(server.mismatches(), [] as [String; 0]);
    assert_eq!(server.consumed(), 1);
    server.finish().await.expect("replay consumed");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn codex_remote_compaction_missing_item_errors() {
    let case = replay_server::fixture_dir("codex", "https_compaction_missing");
    let server = replay_server::ReplayServer::start(&case)
        .await
        .expect("replay server starts");
    let dir = temp_dir("compaction-missing");
    let (_set, provider) = setup(&server.base_url(), &dir, entry());
    let notices: NoticeSink = Arc::new(|_| {});
    let cancel = CancellationToken::new();
    let error = provider
        .compact(session(), &request(Purpose::Compact), notices, &cancel)
        .await
        .expect_err("stream without a compaction item fails");
    assert!(matches!(
        error,
        ProviderError::CompactionMissing {
            family: Family::Codex,
            noun: "item"
        }
    ));
    assert_eq!(
        error.to_string(),
        "codex compaction returned no compaction item."
    );
    assert_eq!(server.addr().ip().to_string(), "127.0.0.1");
    assert_eq!(server.mismatches(), [] as [String; 0]);
    assert_eq!(server.consumed(), 1);
    server.finish().await.expect("replay consumed");
    std::fs::remove_dir_all(&dir).ok();
}
