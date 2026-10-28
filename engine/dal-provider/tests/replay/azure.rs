//! Azure `OpenAI` replay integration suite.
//!
//! Real-transport proof for the Azure route lowering over the Chat and
//! Responses families: a `Chat`/`Responses` entry with the Azure `api-key`
//! style routes through `family::azure` (deployments surface plus the
//! `api-version` query, key in the `api-key` header) and decodes through
//! the surface family's grammar. The fixtures assert method, path plus
//! query, and the `api-key` header; the event assertions prove the family
//! decoders read the Azure framing. Decoder-level value coverage already
//! lives in `src/family/azure.rs`; nothing here repeats it.

use super::replay_server;

use std::path::PathBuf;
use std::sync::Arc;

use dal_core::{
    Config, ConfigProduct, ContextItem, Family, ModelRequest, ModelRoute, ModelToolSpec, Part,
    Purpose, RawJson, RequestParams, ThinkingLevel, Usage,
};
use dal_provider::{
    CatalogEntry, EnvSnapshot, Listing, NoticeSink, ProviderConfig, ProviderError,
    ProviderIdentity, ProviderSet, ResolvedModel, StopReason, StreamEvent, ThinkingSupport,
    ToolArgs, ToolCall, ToolSupport,
};
use tokio_util::sync::CancellationToken;

/// Synthetic Azure credential: isolated to this suite, never a real secret.
const KEY_ENV: &str = "DAL_REPLAY_AZURE_KEY";
const KEY_VALUE: &str = "azure-secret";

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
        provider: "azure-test".into(),
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
        premium_multiplier: None,
        price: None,
    }
}

fn config(dir: &std::path::Path, base: &str, family: Family) -> ProviderConfig {
    // The named Azure entry decodes from the effective TOML through the
    // public `ProviderConfig::from_config` route: the loopback base and the
    // Azure `api-key` style stay explicit, so no captured environment can
    // redirect the turn at a public endpoint. The default `api_version` and
    // empty deployments stay omitted, keeping the per-surface defaults the
    // fixtures pin.
    let api = match family {
        Family::Chat => "openai_chat",
        Family::Responses => "openai_responses",
        _ => panic!("azure replay covers the Chat and Responses families only"),
    };
    let table = format!(
        "[retry]\nrequest_max_retries = 0\nstream_max_retries = 0\n\
         [providers.azure-test]\napi = \"{api}\"\nbase_url = \"{base}\"\n\
         key_env = \"{KEY_ENV}\"\nauth = \"api-key\"\n"
    );
    let host = Config::load(ConfigProduct::Dalgon, dir, "", Some(&table))
        .expect("replay provider table parses");
    let mut config = ProviderConfig::from_config(&host).expect("replay providers decode");
    config
        .providers
        .retain(|candidate| candidate.id.as_ref() == "azure-test");
    assert_eq!(
        config.providers.len(),
        1,
        "replay decodes exactly the azure-test entry"
    );
    config
}

fn resolved(family: Family) -> ResolvedModel {
    ResolvedModel {
        provider: "azure-test".into(),
        route: ModelRoute::Api {
            family,
            model: "gpt-6-astra".into(),
            provider: None,
        },
        entry: entry(),
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

fn text_request(family: Family) -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family,
            model: "gpt-6-astra".into(),
            provider: None,
        },
        system: Arc::from("You are terse."),
        tools: Arc::from([read_tool()]),
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

fn temp_dir(tag: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "dalgona-replay-azure-{tag}-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

struct Driven {
    server: replay_server::ReplayServer,
    events: Vec<StreamEvent>,
    error: Option<ProviderError>,
    dir: PathBuf,
}

async fn drive(case: &str, family: Family, tag: &str) -> Driven {
    let server = replay_server::ReplayServer::start(&replay_server::fixture_dir("azure", case))
        .await
        .expect("replay server starts");
    let dir = temp_dir(tag);
    let set = ProviderSet::new(
        &config(&dir, &server.base_url(), family),
        identity(),
        EnvSnapshot::test(&[(KEY_ENV, KEY_VALUE)]),
        &dir,
        &dir,
    )
    .expect("provider set builds");
    let provider = set.provider(resolved(family)).expect("provider binds");
    let notices: NoticeSink = Arc::new(|_| {});
    let cancel = CancellationToken::new();
    let mut stream = provider
        .open(
            dal_core::SessionId::new_v7(),
            &text_request(family),
            &[],
            notices,
            &cancel,
        )
        .await
        .expect("azure turn opens");
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
        server,
        events,
        error,
        dir,
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

/// Behavior `azure-chat-text`: the Chat entry with the Azure key style posts
/// to the deployments surface with the `api-version` query and the `api-key`
/// header (matched by the fixture), and the Chat grammar decodes the turn.
#[tokio::test]
async fn azure_chat_text_turn() {
    let driven = drive("chat_text", Family::Chat, "chat").await;
    assert!(
        driven.error.is_none(),
        "unexpected stream error: {:?}",
        driven.error
    );
    let mut texts = String::new();
    let mut saw_done = false;
    let mut saw_usage = false;
    let mut saw_stop = false;
    for event in &driven.events {
        match event {
            StreamEvent::TextDelta { text } => texts.push_str(text),
            StreamEvent::ToolCallsDone { calls } => {
                assert!(calls.is_empty(), "no tool calls: {calls:?}");
                saw_done = true;
            }
            StreamEvent::Usage { usage } => {
                assert_eq!(usage.input_tokens, 19);
                saw_usage = true;
            }
            StreamEvent::Stop { reason } => {
                assert!(matches!(reason, StopReason::EndTurn));
                saw_stop = true;
            }
            _ => {}
        }
    }
    assert_eq!(texts, "Hello");
    assert!(saw_done && saw_usage && saw_stop);
    expect_fully_consumed(driven, 1).await;
}

/// Behavior `azure-responses-text`: the Responses entry with the Azure key
/// style posts to the Responses path with the `api-version` query and the
/// `api-key` header (matched by the fixture); reasoning, text, and the tool
/// call decode in causal order with replay bound to the catalog model.
#[tokio::test]
async fn azure_responses_text_turn() {
    const REASONING: &str = r#"{"id":"rs_1", "type":"reasoning","summary":[{"type":"summary_text","text":"Check the file."}],"encrypted_content":"gAAAAB","n":1e+02}"#;
    let driven = drive("responses_text", Family::Responses, "responses").await;
    assert!(
        driven.error.is_none(),
        "unexpected stream error: {:?}",
        driven.error
    );
    let expected = vec![
        StreamEvent::ReasoningDelta {
            text: "Check the file.".into(),
        },
        StreamEvent::Replay {
            payload: dal_provider::ReplayPayload {
                family: Family::Responses,
                model: "gpt-6-astra".into(),
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
    assert_eq!(driven.events, expected);
    expect_fully_consumed(driven, 1).await;
}
