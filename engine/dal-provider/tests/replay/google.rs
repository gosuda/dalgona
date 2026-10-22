//! Google replay integration suite (ENG-03).
//!
//! Real-transport proof for the Google families that speak HTTP SSE through
//! the shared replay server: the direct Gemini API (`google-generative-ai`,
//! key in `x-goog-api-key`, `POST {base}/models/{id}:streamGenerateContent?alt=sse`)
//! and Antigravity (`google-antigravity`, the Cloud Code Assist stream, OAuth
//! `Authorization: Bearer` plus the Antigravity `User-Agent`). Every case
//! serves an authored synthetic fixture and drives the real
//! `ProviderSet`/`open` path, so the proofs cover method, path plus query,
//! credential headers, golden request bodies (direct cases), decoded events,
//! and the retry and cancellation behavior that lives above the decoder. The
//! decoder-level value coverage already lives in
//! `src/family/google/shared/tests.rs`; nothing here repeats it.
//!
//! Not replayed here, with the reason:
//!
//! - Vertex (`google-vertex`): `vertex::build` sets `base_override` to
//!   `https://{host}` for both the OAuth bearer and the Express key wire, and
//!   `google_shared::send` honours the override over the configured base, so a
//!   loopback base can never receive a Vertex request. No integration-level
//!   seam exists and production code gets none for tests. Vertex host, path,
//!   header, and region-fallback routing is proven in-crate by
//!   `family::google::vertex::tests` (`bearer_build_targets_the_project_path`,
//!   `express_build_targets_the_publishers_path_with_global_fallback`,
//!   `explicit_and_global_express_locations_do_not_fall_back`),
//!   `family::google::shared::tests`
//!   (`loopback_vertex_bearer_targets_the_project_path`,
//!   `loopback_vertex_express_falls_back_from_regional_to_global`), and
//!   `provider::tests::google_vertex_without_project_location_refuses_before_any_request`.
//! - Gemini CLI (`google-gemini-cli`): a credential-less local subprocess, not
//!   an HTTP client; proven in-crate by `family::google::gemini_cli::tests`.
//!
//! The Antigravity request body is not compared: the CCA envelope carries a
//! random agent id, trajectory id, and a wall-clock `requestId`, which no byte
//! golden can fix. The envelope members are witnessed in-crate by
//! `family::google::shared::tests::loopback_antigravity_posts_the_cca_envelope`
//! and `provider::tests::google_antigravity_posts_the_cca_envelope`.
//!
//! The Antigravity entry borrows the `factory-droid` table row as its OAuth
//! credential carrier: a stored sign-in resolves only for a provider id that
//! is a table row, and the table has no Antigravity row. The row supplies the
//! credential slot and nothing else; the entry overrides the family, the base
//! URL, and the `project` setting, which is decoded from a `[providers.*]`
//! table because `ProviderSettings` fields are crate-private. This is the
//! same seam the in-crate Antigravity tests use.
//!
//! Credentials are synthetic and isolated to this suite; assertion messages
//! never print them.

use super::replay_server;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_core::{
    AssistantPart, CallId, Config, ConfigProduct, ContextItem, Family, ModelRequest, ModelRoute,
    ModelToolSpec, Part, Purpose, RawJson, ReplaySource, RequestParams, SessionId, ThinkingLevel,
    Usage,
};
use dal_provider::{
    AuthStore, AuthStyle, CatalogEntry, Credential, EnvSnapshot, EventStream, Listing, NoticeSink,
    OAuthCredential, Provider, ProviderConfig, ProviderEntry, ProviderError, ProviderIdentity,
    ProviderSet, ProviderSettings, ReplayPayload, ResolvedModel, SecretString, StopReason,
    StreamEvent, ThinkingSupport, ToolArgs, ToolCall, ToolSupport, Transport,
};
use tokio_util::sync::CancellationToken;

/// Synthetic direct-API key: isolated to this suite, never a real secret.
const KEY_ENV: &str = "DAL_REPLAY_GOOGLE_KEY";
const KEY_VALUE: &str = "google-secret";
/// The direct entry id; not a table row, so its key resolves from `KEY_ENV`.
const DIRECT_PROVIDER: &str = "google-test";
const DIRECT_MODEL: &str = "gemini-3-pro";
/// The table row that carries the Antigravity OAuth credential.
const OAUTH_CARRIER: &str = "factory-droid";
/// Synthetic OAuth tokens: isolated to this suite, never real secrets.
const OAUTH_ACCESS: &str = "ya29-synthetic-access";
const OAUTH_REFRESH: &str = "ya29-synthetic-refresh";
const ANTIGRAVITY_MODEL: &str = "gemini-3-flash-agent";
/// The tool-call id and signature of the signed-call fixtures.
const CALL_ID: &str = "call_read_1";
const CALL_SIGNATURE: &str = "CiQBjz1rXq7N0b5Y3sT9vKdLmWcH4eUa2Fg8PxRZ6oJt+/yE1w==";
/// The thought-block signature of the signed-call fixtures: the visible
/// thought text `Plan: read the file.` (20 bytes) carries this signature in
/// `response-1.sse`, and the decoder binds it to the thought block.
const THOUGHT_SIGNATURE: &str = "dGhvdWdodC1zaWduYXR1cmU=";

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
        thinking: ThinkingSupport::OpenAi {
            accepted: vec![
                ThinkingLevel::Low,
                ThinkingLevel::Medium,
                ThinkingLevel::High,
            ],
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
        premium_multiplier: None,
        price: None,
    }
}

fn resolved(family: Family, provider: &str, model: &str) -> ResolvedModel {
    ResolvedModel {
        provider: provider.into(),
        route: ModelRoute::Api {
            family,
            model: model.into(),
            provider: None,
        },
        entry: entry(provider, model),
    }
}

fn read_tool() -> ModelToolSpec {
    ModelToolSpec {
        name: "read".into(),
        description: "Read a file.".into(),
        parameters: RawJson::parse(
            r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}"#,
        )
        .expect("tool schema"),
        grammar: None,
        defer_loading: false,
    }
}

fn user(text: &str) -> ContextItem {
    ContextItem::User {
        parts: vec![Part::Text { text: text.into() }],
    }
}

/// One turn request: terse system text, the `read` tool, thinking `high`.
fn request(family: Family, model: &str, context: Vec<ContextItem>) -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family,
            model: model.into(),
            provider: None,
        },
        system: Arc::from("You are terse."),
        tools: Arc::from([read_tool()]),
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

fn direct_request(context: Vec<ContextItem>) -> ModelRequest {
    request(Family::GoogleGenerativeAi, DIRECT_MODEL, context)
}

fn temp_dir(tag: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "dalgona-replay-google-{tag}-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// Stores the synthetic Antigravity sign-in in the carrier row's slot. The
/// credential has no expiry, so no refresh runs.
fn seed_oauth(dir: &Path) {
    let mut store = AuthStore::empty(dir.join("auth.json"));
    store
        .set(
            OAUTH_CARRIER,
            Credential::OAuth(OAuthCredential {
                access_token: SecretString::from(OAUTH_ACCESS),
                refresh_token: SecretString::from(OAUTH_REFRESH),
                expires_at: None,
                id_token: None,
                account_id: None,
            }),
        )
        .expect("synthetic oauth credential stores");
    store.store().expect("auth.json writes");
}

fn text(value: &str) -> StreamEvent {
    StreamEvent::TextDelta { text: value.into() }
}

fn done() -> StreamEvent {
    StreamEvent::ToolCallsDone { calls: Vec::new() }
}

fn usage(input: u64, cached: u64, output: u64, reasoning: u64) -> StreamEvent {
    StreamEvent::Usage {
        usage: Usage {
            input_tokens: input,
            cached_input_tokens: cached,
            output_tokens: output,
            reasoning_tokens: Some(reasoning),
            cache_write_tokens: 0,
            cost_usd: None,
            premium_requests: None,
        },
    }
}

fn stop(reason: StopReason) -> StreamEvent {
    StreamEvent::Stop { reason }
}

fn silent() -> NoticeSink {
    Arc::new(|_| {})
}

/// A notice sink that records every notice in arrival order.
fn recording() -> (NoticeSink, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&seen);
    let sink: NoticeSink = Arc::new(move |notice| {
        captured.lock().expect("notices lock").push(notice);
    });
    (sink, seen)
}

/// The events delivered before the stream ended, and the error that ended it.
struct Outcome {
    events: Vec<StreamEvent>,
    error: Option<ProviderError>,
}

impl Outcome {
    fn assert_clean(&self) {
        assert!(
            self.error.is_none(),
            "unexpected stream error: {:?}",
            self.error
        );
    }
}

async fn drain(mut stream: EventStream) -> Outcome {
    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(event) => events.push(event),
            Err(error) => {
                return Outcome {
                    events,
                    error: Some(error),
                };
            }
        }
    }
    Outcome {
        events,
        error: None,
    }
}

/// Opens one turn and drains it; a failure at open is the outcome's error.
async fn run_with(provider: &Provider, request: &ModelRequest, notices: NoticeSink) -> Outcome {
    let cancel = CancellationToken::new();
    match provider
        .open(SessionId::new_v7(), request, &[], notices, &cancel)
        .await
    {
        Ok(stream) => drain(stream).await,
        Err(error) => Outcome {
            events: Vec::new(),
            error: Some(error),
        },
    }
}

async fn run(provider: &Provider, request: &ModelRequest) -> Outcome {
    run_with(provider, request, silent()).await
}

struct Harness {
    server: replay_server::ReplayServer,
    /// The provider set; providers hold it weakly, so it lives here.
    set: ProviderSet,
    dir: PathBuf,
}

impl Harness {
    /// A direct Gemini API entry on the replay base, with the `/v1beta`
    /// prefix of the built-in row default, and `retries` for both budgets.
    async fn direct(case: &str, retries: u32) -> Self {
        let server =
            replay_server::ReplayServer::start(&replay_server::fixture_dir("google", case))
                .await
                .expect("replay server starts");
        let dir = temp_dir(case);
        let config = ProviderConfig {
            default_model: None,
            thinking: ThinkingLevel::Medium,
            aliases: Vec::new(),
            request_max_retries: retries,
            stream_max_retries: retries,
            providers: vec![ProviderEntry {
                id: DIRECT_PROVIDER.into(),
                def: None,
                family: Family::GoogleGenerativeAi,
                base_url: format!("{}/v1beta", server.base_url()).into(),
                transport: Transport::Https,
                key_env: Some(KEY_ENV.into()),
                auth: AuthStyle::XApiKey,
                max_concurrent_requests: 4,
                settings: ProviderSettings::default(),
            }],
            scripted: None,
        };
        let set = ProviderSet::new(
            &config,
            identity(),
            EnvSnapshot::test(&[(KEY_ENV, KEY_VALUE)]),
            &dir,
            &dir,
        )
        .expect("provider set builds");
        Self { server, set, dir }
    }

    /// An Antigravity entry on the replay base as its single configured
    /// endpoint, signed in with the synthetic OAuth credential, no retries.
    async fn antigravity(case: &str) -> Self {
        let server =
            replay_server::ReplayServer::start(&replay_server::fixture_dir("google", case))
                .await
                .expect("replay server starts");
        let dir = temp_dir(case);
        seed_oauth(&dir);
        let table = format!(
            "[providers.{OAUTH_CARRIER}]\nbase_url = \"{}\"\nproject = \"cca-proj\"\n",
            server.base_url()
        );
        let config = Config::load(ConfigProduct::Dalgon, &dir, "", Some(&table))
            .expect("provider table parses");
        let mut providers = ProviderConfig::from_config(&config).expect("providers decode");
        providers.request_max_retries = 0;
        providers.stream_max_retries = 0;
        providers
            .providers
            .retain(|provider| &*provider.id == OAUTH_CARRIER);
        let [carrier] = providers.providers.as_mut_slice() else {
            panic!("the table decodes exactly one {OAUTH_CARRIER} entry");
        };
        carrier.family = Family::GoogleAntigravity;
        let set = ProviderSet::new(&providers, identity(), EnvSnapshot::test(&[]), &dir, &dir)
            .expect("provider set builds");
        Self { server, set, dir }
    }

    fn provider(&self, family: Family, id: &str, model: &str) -> Provider {
        self.set
            .provider(resolved(family, id, model))
            .expect("provider binds")
    }

    fn direct_provider(&self) -> Provider {
        self.provider(Family::GoogleGenerativeAi, DIRECT_PROVIDER, DIRECT_MODEL)
    }

    fn antigravity_provider(&self) -> Provider {
        self.provider(Family::GoogleAntigravity, OAUTH_CARRIER, ANTIGRAVITY_MODEL)
    }

    /// The replay verdict: no mismatch, exactly `exchanges` consumed, and
    /// every request closed.
    async fn finish(self, exchanges: usize) {
        let mismatches = self.server.mismatches();
        assert!(mismatches.is_empty(), "replay mismatches: {mismatches:?}");
        assert_eq!(self.server.consumed(), exchanges);
        let closed = tokio::time::timeout(Duration::from_secs(1), async {
            while self.server.open_requests() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(closed.is_ok(), "every replay request must close");
        assert!(self.server.max_open_requests() >= 1);
        self.server.finish().await.expect("replay consumed");
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

/// Behavior `google-direct-text`: a direct Gemini turn posts to the model's
/// `streamGenerateContent?alt=sse` path under the base prefix with the key in
/// `x-goog-api-key`, and sends the flat body (contents, system instruction,
/// tool declarations under `parametersJsonSchema`, generation config with the
/// output cap and the Gemini 3 thinking level) byte for byte as the fixture
/// golden. Text deltas arrive in order, then the terminal group with the last
/// usage metadata (input includes the cached subset) and the end-of-turn stop.
#[tokio::test]
async fn google_direct_text_turn() {
    let harness = Harness::direct("direct_text_turn", 0).await;
    let provider = harness.direct_provider();
    let mut request = direct_request(vec![user("Hi")]);
    request.params.max_output_tokens = Some(256);
    let outcome = run(&provider, &request).await;
    outcome.assert_clean();
    assert_eq!(
        outcome.events,
        vec![
            text("Hel"),
            text("lo."),
            done(),
            usage(12, 4, 3, 0),
            stop(StopReason::EndTurn),
        ]
    );
    harness.finish(1).await;
}

/// Behavior `google-direct-thinking-tool`: a signed thought part, then a signed
/// `functionCall`, decode as reasoning, the call, one `Replay` carrier holding
/// the call signature under `calls` and the thought-block signature under
/// `parts` (thought index 0, 20 bytes), and a tool-use stop. The stored
/// assistant turn and a tool result then go back on a second request whose body
/// carries the signature on both the thought part and the `functionCall` part
/// and a `functionResponse` part, byte for byte as the fixture golden.
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one replay fixture proves both request bodies byte for byte in place"
)]
async fn google_direct_thinking_tool_turn_replays_the_signature() {
    let harness = Harness::direct("direct_thinking_tool_turn", 0).await;
    let provider = harness.direct_provider();

    let first = run(&provider, &direct_request(vec![user("Read a.txt")])).await;
    first.assert_clean();
    let args = RawJson::parse(r#"{"path":"a.txt"}"#).expect("call args");
    let carrier = RawJson::parse(&format!(
        r#"{{"calls":{{"{CALL_ID}":"{CALL_SIGNATURE}"}},"parts":[{{"kind":"thought","index":0,"bytes":20,"signature":"{THOUGHT_SIGNATURE}"}}]}}"#
    ))
    .expect("replay carrier");
    assert_eq!(
        first.events,
        vec![
            StreamEvent::ReasoningDelta {
                text: "Plan: read the file.".into(),
            },
            StreamEvent::ToolCallStarted {
                id: CALL_ID.into(),
                name: "read".into(),
            },
            StreamEvent::ToolArgsDelta {
                id: CALL_ID.into(),
                fragment: br#"{"path":"a.txt"}"#.to_vec(),
            },
            StreamEvent::Replay {
                payload: ReplayPayload {
                    family: Family::GoogleGenerativeAi,
                    model: DIRECT_MODEL.into(),
                    item: carrier,
                },
            },
            StreamEvent::ToolCallsDone {
                calls: vec![ToolCall {
                    id: CALL_ID.into(),
                    name: "read".into(),
                    args: ToolArgs::Parsed(args),
                }],
            },
            usage(40, 0, 7, 21),
            stop(StopReason::ToolUse),
        ]
    );

    // The next turn replays what the first turn produced, as a session does:
    // the reasoning text carries the stored carrier, the call keeps its id.
    let stored = first
        .events
        .iter()
        .find_map(|event| match event {
            StreamEvent::Replay { payload } => Some(payload.item.clone()),
            _ => None,
        })
        .expect("the signed call is stored for replay");
    let [call] = first
        .events
        .iter()
        .find_map(|event| match event {
            StreamEvent::ToolCallsDone { calls } => Some(calls.as_slice()),
            _ => None,
        })
        .expect("the response ends its calls")
    else {
        panic!("one call is made");
    };
    let ToolArgs::Parsed(call_args) = call.args.clone() else {
        panic!("the call arguments parse");
    };
    let second = direct_request(vec![
        user("Read a.txt"),
        ContextItem::Assistant {
            source: ReplaySource {
                family: Family::GoogleGenerativeAi,
                model: DIRECT_MODEL.into(),
            },
            replay: None,
            parts: vec![
                AssistantPart::Thinking {
                    text: "Plan: read the file.".into(),
                    replay: Some(stored),
                },
                AssistantPart::ToolCall {
                    call: CallId::new(call.id.clone()),
                    name: call.name.clone().into(),
                    args: call_args,
                    replay: None,
                },
            ],
        },
        ContextItem::ToolResult {
            call: CallId::new(call.id.clone()),
            name: call.name.clone().into(),
            is_error: false,
            parts: vec![Part::Text {
                text: "hello".into(),
            }],
        },
    ]);
    let second = run(&provider, &second).await;
    second.assert_clean();
    assert_eq!(
        second.events,
        vec![
            text("a.txt says hello."),
            done(),
            usage(71, 0, 5, 0),
            stop(StopReason::EndTurn),
        ]
    );
    harness.finish(2).await;
}

/// Behavior `google-direct-quota`: a 429 whose `ErrorInfo` reason is
/// `QUOTA_EXHAUSTED` is terminal quota. Retries are configured, so a retry
/// would need a second exchange the fixture does not have: the typed quota
/// error with the server message and exactly one consumed exchange prove
/// there was none.
#[tokio::test]
async fn google_direct_quota_429_is_terminal() {
    let harness = Harness::direct("direct_quota_429", 2).await;
    let provider = harness.direct_provider();
    let outcome = run(&provider, &direct_request(vec![user("Hi")])).await;
    assert!(outcome.events.is_empty(), "{:?}", outcome.events);
    match &outcome.error {
        Some(ProviderError::Quota { message }) => assert!(
            message.contains("You exceeded your current quota"),
            "quota message: {message}"
        ),
        other => panic!("expected terminal quota, got {other:?}"),
    }
    harness.finish(1).await;
}

/// Behavior `google-direct-rate-limit`: a 429 without an account-exhaustion
/// signal is the retryable rate-limit class, not quota. With no retries
/// configured it surfaces after one exchange, carrying the server message and
/// the `retry-after` wait of that response.
#[tokio::test]
async fn google_direct_bare_429_is_rate_limited() {
    let harness = Harness::direct("direct_rate_limited_429", 0).await;
    let provider = harness.direct_provider();
    let outcome = run(&provider, &direct_request(vec![user("Hi")])).await;
    assert!(outcome.events.is_empty(), "{:?}", outcome.events);
    match &outcome.error {
        Some(ProviderError::RateLimited {
            message,
            retry_after,
        }) => {
            assert!(
                message.contains("Resource has been exhausted"),
                "rate-limit message: {message}"
            );
            assert_eq!(*retry_after, Some(Duration::from_secs(7)));
        }
        other => panic!("expected a rate limit, got {other:?}"),
    }
    harness.finish(1).await;
}

/// Behavior `google-direct-cut-eof`: the body ends cleanly after delivered
/// text with no `finishReason`. The text arrives, then the typed stream cut;
/// stream retries are configured but the turn already delivered, so exactly
/// one exchange is consumed.
#[tokio::test]
async fn google_direct_clean_eof_after_text_is_a_cut_and_not_retried() {
    let harness = Harness::direct("direct_stream_cut_eof", 2).await;
    let provider = harness.direct_provider();
    let outcome = run(&provider, &direct_request(vec![user("Hi")])).await;
    assert_eq!(outcome.events, vec![text("Partial")]);
    assert!(
        matches!(outcome.error, Some(ProviderError::StreamCut)),
        "expected the stream cut, got {:?}",
        outcome.error
    );
    harness.finish(1).await;
}

/// Behavior `google-direct-cut-connection`: the server aborts the connection
/// mid-chunk after delivered text. The text arrives, then the transport
/// error; stream retries are configured but the turn already delivered, so
/// exactly one exchange is consumed.
#[tokio::test]
async fn google_direct_connection_cut_after_text_is_not_retried() {
    let harness = Harness::direct("direct_connection_cut", 2).await;
    let provider = harness.direct_provider();
    let outcome = run(&provider, &direct_request(vec![user("Hi")])).await;
    assert_eq!(outcome.events, vec![text("Partial")]);
    assert!(
        matches!(
            outcome.error,
            Some(ProviderError::Transport {
                family: Family::GoogleGenerativeAi,
                ..
            })
        ),
        "expected the transport error, got {:?}",
        outcome.error
    );
    harness.finish(1).await;
}

/// Behavior `google-direct-cut-before-delivery`: the same transport failure
/// before any event completes is retried once with a notice, and the second
/// exchange delivers the whole turn.
#[tokio::test]
async fn google_direct_connection_cut_before_first_event_retries_once() {
    let harness = Harness::direct("direct_cut_before_first_event", 1).await;
    let provider = harness.direct_provider();
    let (notices, seen) = recording();
    let outcome = run_with(&provider, &direct_request(vec![user("Hi")]), notices).await;
    outcome.assert_clean();
    assert_eq!(
        outcome.events,
        vec![
            text("Recovered."),
            done(),
            usage(9, 0, 2, 0),
            stop(StopReason::EndTurn),
        ]
    );
    {
        let seen = seen.lock().expect("notices lock");
        assert_eq!(seen.len(), 1, "exactly one retry notice: {seen:?}");
        assert!(seen[0].starts_with("Retrying in"), "{}", seen[0]);
    }
    harness.finish(2).await;
}

/// Behavior `google-direct-cancel`: cancelling after the first delivered
/// delta, with the server stalled mid-stream, leaves the consumer pending
/// with no synthesized terminal event and closes the stalled transport; the
/// replay then finishes with its one exchange consumed.
#[tokio::test]
async fn google_direct_cancel_mid_stream_closes_the_transfer() {
    let harness = Harness::direct("direct_cancel_stall", 0).await;
    let provider = harness.direct_provider();
    let cancel = CancellationToken::new();
    let mut stream = provider
        .open(
            SessionId::new_v7(),
            &direct_request(vec![user("Hi")]),
            &[],
            silent(),
            &cancel,
        )
        .await
        .expect("google turn opens");
    let first = stream
        .next()
        .await
        .expect("first event arrives")
        .expect("no stream error");
    assert_eq!(first, text("half"));
    assert_eq!(harness.server.consumed(), 1);
    assert_eq!(
        harness.server.open_requests(),
        1,
        "the response stays open after the first delta"
    );
    cancel.cancel();
    let pending = tokio::time::timeout(Duration::from_millis(200), stream.next()).await;
    assert!(pending.is_err(), "a cancelled turn emits no terminal event");
    let closed = tokio::time::timeout(Duration::from_secs(1), async {
        while harness.server.open_requests() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        closed.is_ok(),
        "cancellation must close the stalled request"
    );
    drop(stream);
    harness.finish(1).await;
}

/// Behavior `google-antigravity-cca`: an Antigravity turn posts to the CCA
/// stream path under the single configured base with the OAuth access token
/// as the bearer and the Antigravity user agent (both matched by the
/// fixture), and the Cloud Code Assist envelope (`{"response": {...}}` per
/// SSE event) decodes through the shared grammar.
#[tokio::test]
async fn google_antigravity_cca_turn() {
    let harness = Harness::antigravity("antigravity_cca_turn").await;
    let provider = harness.antigravity_provider();
    let request = request(
        Family::GoogleAntigravity,
        ANTIGRAVITY_MODEL,
        vec![user("Hi")],
    );
    let outcome = run(&provider, &request).await;
    outcome.assert_clean();
    assert_eq!(
        outcome.events,
        vec![
            text("Hi"),
            text(" there."),
            done(),
            usage(9, 0, 3, 0),
            stop(StopReason::EndTurn),
        ]
    );
    harness.finish(1).await;
}

/// Behavior `google-antigravity-validation`: a 403 whose `ErrorInfo` reason is
/// `VALIDATION_REQUIRED` with a valid `validation_url` renders the fixed
/// account-verification message carrying that URL, as a terminal status
/// failure after one exchange.
#[tokio::test]
async fn google_antigravity_validation_required_renders_the_verification_message() {
    let harness = Harness::antigravity("antigravity_validation_required").await;
    let provider = harness.antigravity_provider();
    let request = request(
        Family::GoogleAntigravity,
        ANTIGRAVITY_MODEL,
        vec![user("Hi")],
    );
    let outcome = run(&provider, &request).await;
    assert!(outcome.events.is_empty(), "{:?}", outcome.events);
    match &outcome.error {
        Some(ProviderError::Status {
            family: Family::GoogleAntigravity,
            status: 403,
            message,
        }) => assert_eq!(
            message,
            "Account verification required. Visit https://accounts.example.com/verify/abc123 to continue, then retry your request."
        ),
        other => panic!("expected the verification failure, got {other:?}"),
    }
    harness.finish(1).await;
}
