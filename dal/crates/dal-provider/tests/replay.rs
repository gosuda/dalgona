//! Provider replay integration suite (initial case; full plan list follows).

#[path = "support/replay_server.rs"]
mod replay_server;

use std::sync::Arc;

use dal_core::{
    ContextItem, Family, ModelRequest, ModelRoute, Part, Purpose, RequestParams, ThinkingLevel,
};
use dal_provider::{
    AuthStyle, CatalogEntry, EnvSnapshot, Listing, NoticeSink, ProviderConfig, ProviderEntry,
    ProviderIdentity, ProviderSet, ResolvedModel, StopReason, StreamEvent, ThinkingSupport,
    ToolSupport, Transport,
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

#[tokio::test]
async fn chat_text_turn() {
    let case = replay_server::fixture_dir("chat", "text_turn");
    let server = replay_server::ReplayServer::start(&case)
        .await
        .expect("replay server starts");
    let base = server.base_url();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("dalgona-replay-{}-{stamp}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let config = ProviderConfig {
        default_model: None,
        thinking: ThinkingLevel::Medium,
        aliases: Vec::new(),
        request_max_retries: 0,
        stream_max_retries: 0,
        providers: vec![ProviderEntry {
            id: "chat-test".into(),
            def: None,
            family: Family::Chat,
            base_url: base.into_boxed_str(),
            transport: Transport::Https,
            key_env: Some("CHAT_TEST_KEY".into()),
            auth: AuthStyle::Bearer,
            max_concurrent_requests: 4,
        }],
        scripted: None,
    };
    let set = ProviderSet::new(
        &config,
        identity(),
        EnvSnapshot::test(&[("CHAT_TEST_KEY", "chat-secret")]),
        &dir,
        &dir,
    )
    .expect("provider set builds");
    let resolved = ResolvedModel {
        provider: "chat-test".into(),
        route: ModelRoute::Api {
            family: Family::Chat,
            model: "gpt-6-astra".into(),
        },
        entry: entry(),
    };
    let provider = set.provider(resolved).expect("provider binds");
    let notices: NoticeSink = Arc::new(|_| {});
    let cancel = CancellationToken::new();
    let stream = provider
        .open(
            dal_core::SessionId::new_v7(),
            &chat_request(),
            &[],
            notices,
            &cancel,
        )
        .await
        .expect("chat turn opens");
    let mut texts = String::new();
    let mut saw_done = false;
    let mut saw_usage = false;
    let mut stop = None;
    let mut events = std::pin::pin!(stream);
    while let Some(event) = events.next().await {
        match event.expect("no stream error") {
            StreamEvent::TextDelta { text } => texts.push_str(&text),
            StreamEvent::ToolCallsDone { calls } => {
                assert_eq!(calls, [] as [dal_provider::ToolCall; 0]);
                saw_done = true;
            }
            StreamEvent::Usage { usage } => {
                assert_eq!(usage.input_tokens, 19);
                saw_usage = true;
            }
            StreamEvent::Stop { reason } => stop = Some(reason),
            _ => {}
        }
    }
    assert_eq!(texts, "Hello");
    assert!(saw_done && saw_usage);
    assert!(matches!(stop, Some(StopReason::EndTurn)));
    assert_eq!(server.addr().ip().to_string(), "127.0.0.1");
    assert_eq!(server.mismatches(), [] as [std::string::String; 0]);
    assert_eq!(server.consumed(), 1);
    assert_eq!(server.open_requests(), 0);
    assert!(server.max_open_requests() >= 1);
    server.finish().await.expect("replay consumed");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn reserved_port_refuses_until_listen() {
    let case = replay_server::fixture_dir("chat", "text_turn");
    let reserved = replay_server::ReplayServer::reserve(&case).expect("reserve binds");
    assert_eq!(reserved.base_url(), format!("http://{}", reserved.addr()));
    assert_eq!(reserved.addr().ip().to_string(), "127.0.0.1");
    let refused = tokio::net::TcpStream::connect(reserved.addr()).await;
    assert!(refused.is_err(), "bound reservation refuses connects");
    let server = reserved.listen().expect("reservation listens");
    let error = server.finish().await.expect_err("untouched fixture");
    assert!(
        error
            .to_string()
            .contains("replay incomplete: 0 of 1 exchanges consumed"),
        "unexpected verdict: {error}"
    );
}
