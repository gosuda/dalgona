//! GitHub Copilot replay integration suite (ENG-02).
//!
//! Real-transport proof for the Copilot route lowering over the Chat and
//! Anthropic families: an entry with a Copilot id routes through
//! `family::copilot` (surface path, bearer authorization beside the
//! announced `copilot-integration-id` identity, loopback base kept) and
//! decodes through the surface family's grammar. The fixtures assert
//! method, path, and the identity headers; the event assertions prove the
//! family decoders read the Copilot framing. Decoder-level value coverage
//! already lives in `src/family/copilot.rs`; nothing here repeats it.

use super::replay_server;

use std::path::PathBuf;
use std::sync::Arc;

use dal_core::{
    ContextItem, Family, ModelRequest, ModelRoute, Part, Purpose, RequestParams, ThinkingLevel,
    Usage,
};
use dal_provider::{
    AuthStyle, CatalogEntry, Effort, EnvSnapshot, Listing, NoticeSink, ProviderConfig,
    ProviderEntry, ProviderError, ProviderIdentity, ProviderSet, ProviderSettings, ResolvedModel,
    StopReason, StreamEvent, ThinkingSupport, ToolSupport, Transport,
};
use tokio_util::sync::CancellationToken;

/// Synthetic Copilot credential: isolated to this suite, never a real secret.
const KEY_ENV: &str = "DAL_REPLAY_COPILOT_KEY";
const KEY_VALUE: &str = "copilot-secret";

fn identity() -> ProviderIdentity {
    ProviderIdentity {
        version: "test".into(),
        os: "linux".into(),
        os_version: "test".into(),
        arch: "x86_64".into(),
    }
}

fn chat_entry() -> CatalogEntry {
    CatalogEntry {
        provider: "github-copilot".into(),
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

fn anthropic_entry() -> CatalogEntry {
    CatalogEntry {
        provider: "github-copilot-anthropic".into(),
        id: "claude-test-1".into(),
        display: "claude-test-1".into(),
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

fn config(id: &str, base: &str, family: Family) -> ProviderConfig {
    ProviderConfig {
        default_model: None,
        thinking: ThinkingLevel::Medium,
        aliases: Vec::new(),
        request_max_retries: 0,
        stream_max_retries: 0,
        providers: vec![ProviderEntry {
            id: id.into(),
            def: None,
            family,
            base_url: base.into(),
            transport: Transport::Https,
            key_env: Some(KEY_ENV.into()),
            auth: AuthStyle::Bearer,
            max_concurrent_requests: 4,
            settings: ProviderSettings::default(),
        }],
        scripted: None,
    }
}

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

fn anthropic_request() -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::Anthropic,
            model: "claude-test-1".into(),
        },
        system: Arc::from("Be brief."),
        tools: Arc::from([]),
        context: Arc::from([ContextItem::User {
            parts: vec![Part::Text {
                text: "Look".into(),
            }],
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
        "dalgona-replay-copilot-{tag}-{}-{stamp}",
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

async fn drive_chat(case: &str, tag: &str) -> Driven {
    drive(
        case,
        tag,
        "github-copilot",
        Family::Chat,
        "gpt-6-astra",
        chat_entry(),
        chat_request(),
    )
    .await
}

async fn drive_anthropic(case: &str, tag: &str) -> Driven {
    drive(
        case,
        tag,
        "github-copilot-anthropic",
        Family::Anthropic,
        "claude-test-1",
        anthropic_entry(),
        anthropic_request(),
    )
    .await
}

async fn drive(
    case: &str,
    tag: &str,
    id: &str,
    family: Family,
    model: &str,
    entry: CatalogEntry,
    request: ModelRequest,
) -> Driven {
    let server = replay_server::ReplayServer::start(&replay_server::fixture_dir("copilot", case))
        .await
        .expect("replay server starts");
    let dir = temp_dir(tag);
    let set = ProviderSet::new(
        &config(id, &server.base_url(), family),
        identity(),
        EnvSnapshot::test(&[(KEY_ENV, KEY_VALUE)]),
        &dir,
        &dir,
    )
    .expect("provider set builds");
    let resolved = ResolvedModel {
        provider: id.into(),
        route: ModelRoute::Api {
            family,
            model: model.into(),
        },
        entry,
    };
    let provider = set.provider(resolved).expect("provider binds");
    let notices: NoticeSink = Arc::new(|_| {});
    let cancel = CancellationToken::new();
    let mut stream = provider
        .open(
            dal_core::SessionId::new_v7(),
            &request,
            &[],
            notices,
            &cancel,
        )
        .await
        .expect("copilot turn opens");
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

/// Behavior `copilot-chat-text`: the Copilot Chat entry posts to the Chat
/// path with the bearer authorization and the announced `copilot-chat`
/// identity (matched by the fixture), and the Chat grammar decodes the turn.
#[tokio::test]
async fn copilot_chat_text_turn() {
    let driven = drive_chat("chat_text", "chat").await;
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

/// Behavior `copilot-anthropic-text`: the Copilot Anthropic entry posts to
/// the Messages path with the bearer authorization and the announced
/// `copilot-chat` identity (matched by the fixture); signed thinking
/// replays bound to the catalog model, then text, usage, and stop arrive.
#[tokio::test]
async fn copilot_anthropic_text_turn() {
    let driven = drive_anthropic("anthropic_text", "anthropic").await;
    assert!(
        driven.error.is_none(),
        "unexpected stream error: {:?}",
        driven.error
    );
    assert_eq!(driven.events.len(), 7);
    assert!(
        matches!(&driven.events[0], StreamEvent::ReasoningDelta { text } if text == "deep "),
        "first reasoning delta arrives"
    );
    assert!(
        matches!(&driven.events[1], StreamEvent::ReasoningDelta { text } if text == "thought"),
        "second reasoning delta arrives"
    );
    assert!(
        matches!(&driven.events[2], StreamEvent::Replay { payload }
            if payload.family == Family::Anthropic
                && payload.model.as_ref() == "claude-test-1"
                && payload.item.as_str().contains(r#""type":"thinking""#)
                && payload.item.as_str().contains(r#""thinking":"deep thought""#)
                && payload.item.as_str().contains(r#""signature":"EqQBCgIYAhIM""#)),
        "signed thinking replays bound to the catalog model"
    );
    assert_eq!(
        driven.events[3],
        StreamEvent::TextDelta {
            text: "The answer is 21.".into()
        }
    );
    assert_eq!(
        driven.events[4],
        StreamEvent::ToolCallsDone { calls: Vec::new() }
    );
    assert_eq!(
        driven.events[5],
        StreamEvent::Usage {
            usage: Usage {
                input_tokens: 25,
                cached_input_tokens: 0,
                output_tokens: 12,
                reasoning_tokens: None,
                cache_write_tokens: 0,
                cost_usd: None,
                // A user-initiated Copilot turn bills one premium request.
                premium_requests: Some(1.0),
            },
        }
    );
    assert_eq!(
        driven.events[6],
        StreamEvent::Stop {
            reason: StopReason::EndTurn
        }
    );
    expect_fully_consumed(driven, 1).await;
}
