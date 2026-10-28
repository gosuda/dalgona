//! Factory Droid proxy replay.
//!
//! Full-stack proof that the Droid proxy multiplexer reuses the shared Chat
//! wire: a `factory-droid` entry with a `WorkOS` OAuth credential routes
//! through the provider stack (proxy path, bearer authorization beside the
//! Droid identity headers, loopback base kept) and decodes through the Chat
//! grammar. The fixture asserts method, path, and the bearer + default
//! upstream headers; the event assertions prove the shared decoder reads the
//! proxy framing. Route lowerings, bodies, and decoder edges already live in
//! `src/family/droid/tests.rs`; nothing here repeats them.
//!
//! This is a standalone target on purpose: the shared `tests/replay.rs`
//! suite is owned by concurrent lanes, so this proof wires its own fixture
//! case instead of registering there.

#![expect(clippy::expect_used, reason = "integration tests fail loudly")]

#[expect(
    dead_code,
    reason = "droid shares the replay server but never reserves: the reservation readers stay live for the shared replay suite"
)]
#[path = "support/replay_server.rs"]
mod replay_server;

use std::sync::Arc;

use dal_core::{
    Config, ConfigProduct, ContextItem, Family, ModelRequest, ModelRoute, Part, Purpose,
    RequestParams, ThinkingLevel,
};
use dal_provider::{
    AuthStore, CatalogEntry, Credential, EnvSnapshot, Listing, NoticeSink, OAuthCredential,
    ProviderConfig, ProviderIdentity, ProviderSet, ResolvedModel, SecretString, StopReason,
    StreamEvent, ThinkingSupport, ToolSupport,
};
use tokio_util::sync::CancellationToken;

/// Synthetic `WorkOS` token; never a live secret. Fixtures only assert its presence.
const ACCESS_TOKEN: &str = "workos-test-token";

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
        provider: "factory-droid".into(),
        id: "kimi-k3".into(),
        display: "kimi-k3".into(),
        listing: Listing::Listed,
        context_window: Some(32_000),
        max_output: Some(4_096),
        thinking: ThinkingSupport::OpenAi {
            accepted: vec![ThinkingLevel::Medium],
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

fn seed_auth(dir: &std::path::Path) {
    let mut store = AuthStore::empty(dir.join("auth.json"));
    store
        .set(
            "factory-droid",
            Credential::OAuth(OAuthCredential {
                access_token: SecretString::from(ACCESS_TOKEN),
                refresh_token: SecretString::from("workos-test-refresh"),
                expires_at: None,
                id_token: None,
                account_id: None,
                project_id: None,
            }),
        )
        .expect("fake droid credential stores");
    store.store().expect("auth.json writes");
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "dalgona-replay-droid-{tag}-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    seed_auth(&dir);
    dir
}

fn config(base: &str) -> ProviderConfig {
    let table = format!("[providers.factory-droid]\nbase_url = \"{base}\"\n");
    let core = Config::load(
        ConfigProduct::Dalgon,
        std::path::Path::new("/tmp/dal-provider-droid-replay"),
        "",
        Some(&table),
    )
    .expect("provider table parses");
    let mut decoded = ProviderConfig::from_config(&core).expect("providers decode");
    decoded.request_max_retries = 0;
    decoded.stream_max_retries = 0;
    decoded
        .providers
        .retain(|provider| &*provider.id == "factory-droid");
    decoded
}

fn request() -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::Droid,
            model: "kimi-k3".into(),
            provider: None,
        },
        system: Arc::from("You are terse."),
        tools: Arc::from([]),
        context: Arc::from([ContextItem::User {
            parts: vec![Part::Text { text: "Hi".into() }],
        }]),
        params: RequestParams {
            thinking: ThinkingLevel::Medium,
            effort: None,
            temperature: None,
            max_output_tokens: None,
        },
        cache_key: None,
    }
}

/// Behavior `droid-completions-text`: the Droid entry posts to the proxy
/// completions path with the `WorkOS` bearer and the registry-default upstream
/// (matched by the fixture), and the shared Chat grammar decodes the turn.
#[tokio::test]
async fn droid_completions_text_turn() {
    let server = replay_server::ReplayServer::start(&replay_server::fixture_dir(
        "droid",
        "completions_text",
    ))
    .await
    .expect("replay server starts");
    let dir = temp_dir("completions");
    let set = ProviderSet::new(
        &config(&server.base_url()),
        identity(),
        EnvSnapshot::test(&[]),
        &dir,
        &dir,
    )
    .expect("provider set builds");
    let resolved = ResolvedModel {
        provider: "factory-droid".into(),
        route: ModelRoute::Api {
            family: Family::Droid,
            model: "kimi-k3".into(),
            provider: None,
        },
        entry: entry(),
    };
    let provider = set.provider(resolved).expect("provider binds");
    let notices: NoticeSink = Arc::new(|_| {});
    let cancel = CancellationToken::new();
    let mut stream = provider
        .open(
            dal_core::SessionId::new_v7(),
            &request(),
            &[],
            notices,
            &cancel,
        )
        .await
        .expect("droid turn opens");
    let mut texts = String::new();
    let mut saw_done = false;
    let mut saw_usage = false;
    let mut saw_stop = false;
    let mut error = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(StreamEvent::TextDelta { text }) => texts.push_str(&text),
            Ok(StreamEvent::ToolCallsDone { calls }) => {
                assert!(calls.is_empty(), "no tool calls: {calls:?}");
                saw_done = true;
            }
            Ok(StreamEvent::Usage { usage }) => {
                assert_eq!(usage.input_tokens, 19);
                assert_eq!(usage.output_tokens, 10);
                saw_usage = true;
            }
            Ok(StreamEvent::Stop { reason }) => {
                saw_stop = reason == StopReason::EndTurn;
            }
            Ok(_) => {}
            Err(failure) => {
                error = Some(failure);
                break;
            }
        }
    }
    assert!(error.is_none(), "unexpected stream error: {error:?}");
    assert_eq!(texts, "Hello");
    assert!(saw_done && saw_usage && saw_stop);
    let mismatches = server.mismatches();
    assert!(mismatches.is_empty(), "replay mismatches: {mismatches:?}");
    assert_eq!(server.consumed(), 1);
    assert_eq!(server.open_requests(), 0);
    assert!(server.max_open_requests() >= 1);
    server.finish().await.expect("replay consumed");
    std::fs::remove_dir_all(&dir).ok();
}
