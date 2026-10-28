#![expect(
    clippy::expect_used,
    reason = "live tests fail loudly on missing prerequisites"
)]
#![expect(
    clippy::disallowed_methods,
    reason = "live tests read the process edge"
)]
//! Live native-Ollama suite: a real Ollama server on the wire, no credential.
//!
//! Every case is `#[ignore]`d and runs only against a server the operator
//! names at the test-process edge:
//!
//! - `DAL_LIVE_OLLAMA_URL`: the server's base URL, for example
//!   `http://127.0.0.1:11434`.
//! - `DAL_LIVE_OLLAMA_THINKER`: the id of a model that reports both the
//!   `thinking` and `tools` capabilities (`qwen3:0.6b` is the smallest
//!   public one).
//! - `DAL_LIVE_OLLAMA_PLAIN`: the id of a model that reports neither
//!   (`gemma3:270m` is the smallest public one).
//!
//! Reproduction on a machine with no GPU (CPU inference of the two models
//! above is enough; the suite was recorded against Ollama v0.40.6):
//!
//! ```text
//! tar --zstd -xf ollama-linux-amd64.tar.zst -C "$DIR"   # official release
//! OLLAMA_HOST=127.0.0.1:11435 OLLAMA_MODELS="$DIR/models" "$DIR/bin/ollama" serve &
//! OLLAMA_HOST=127.0.0.1:11435 "$DIR/bin/ollama" pull qwen3:0.6b
//! OLLAMA_HOST=127.0.0.1:11435 "$DIR/bin/ollama" pull gemma3:270m
//! DAL_LIVE_OLLAMA_URL=http://127.0.0.1:11435 \
//! DAL_LIVE_OLLAMA_THINKER=qwen3:0.6b DAL_LIVE_OLLAMA_PLAIN=gemma3:270m \
//!   cargo test --locked -p dal-provider --test ollama_live -- --ignored
//! ```
//!
//! A missing variable fails the case with
//! `live suite: missing prerequisite <name>: <how>`; no case skips or passes
//! without its server, and none asserts on model text. The tool case needs a
//! model that follows a plain instruction to call a tool: it fails, rather
//! than passing vacuously, when the turn ends without a completed call.

use std::sync::Arc;

use dal_core::{
    Config, ConfigProduct, ContextItem, ModelRequest, ModelRoute, ModelToolSpec, Part, Purpose,
    RawJson, RequestParams, SessionId, ThinkingLevel,
};
use dal_provider::{
    CatalogEntry, EnvSnapshot, NoticeSink, Provider, ProviderConfig, ProviderEntry,
    ProviderIdentity, ProviderSet, ResolvedModel, StopReason, StreamEvent, ThinkingSupport,
    ToolArgs, ToolCall, ToolSupport,
};
use tokio_util::sync::CancellationToken;

/// A prerequisite from the process edge, or the case's failure.
fn required(name: &str, how: &str) -> String {
    let message = format!("live suite: missing prerequisite {name}: {how}");
    let value = std::env::var(name).expect(message.as_str());
    assert!(!value.is_empty(), "{message}");
    value
}

fn server_url() -> String {
    required(
        "DAL_LIVE_OLLAMA_URL",
        "export the base URL of a running Ollama server",
    )
}

fn thinker() -> String {
    required(
        "DAL_LIVE_OLLAMA_THINKER",
        "export a model id reporting `thinking` and `tools`, e.g. qwen3:0.6b",
    )
}

fn plain() -> String {
    required(
        "DAL_LIVE_OLLAMA_PLAIN",
        "export a model id reporting neither, e.g. gemma3:270m",
    )
}

fn identity() -> ProviderIdentity {
    ProviderIdentity {
        version: env!("CARGO_PKG_VERSION").into(),
        os: std::env::consts::OS.into(),
        os_version: "live".into(),
        arch: std::env::consts::ARCH.into(),
    }
}

/// The built-in `ollama` row pointed at the live server, credential-less.
fn ollama_entry(url: &str) -> ProviderEntry {
    let table = format!(
        "[providers.ollama]\nbase_url = \"{url}\"\nauth = \"none\"\nmax_concurrent_requests = 1\n"
    );
    let core = Config::load(
        ConfigProduct::Dalgon,
        std::path::Path::new("/tmp/dal-provider-ollama-live"),
        "",
        Some(&table),
    )
    .expect("provider table parses");
    ProviderConfig::from_config(&core)
        .expect("providers decode")
        .providers
        .into_iter()
        .find(|provider| &*provider.id == "ollama")
        .expect("ollama decoded")
}

fn live_set(case: &str) -> (ProviderSet, std::path::PathBuf) {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let cache_dir = std::env::temp_dir().join(format!(
        "dalgona-live-{case}-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&cache_dir).expect("live cache dir");
    let config = ProviderConfig {
        default_model: None,
        thinking: ThinkingLevel::Medium,
        aliases: Vec::new(),
        request_max_retries: 0,
        stream_max_retries: 0,
        providers: vec![ollama_entry(&server_url())],
        scripted: None,
    };
    let set = ProviderSet::new(
        &config,
        identity(),
        EnvSnapshot::test(&[]),
        &cache_dir,
        &cache_dir,
    )
    .expect("live provider set builds");
    (set, cache_dir)
}

/// Resolve through the live catalog, then bind: the production path.
async fn bind(set: &ProviderSet, model: &str) -> (Provider, ModelRoute, CatalogEntry) {
    let catalog = set.catalog().await.expect("live catalog loads");
    let resolved: ResolvedModel = set
        .resolve(&catalog, &format!("ollama/{model}"))
        .expect("live model resolves");
    let route = resolved.route.clone();
    let entry = resolved.entry.clone();
    let provider = set.provider(resolved).expect("live provider binds");
    (provider, route, entry)
}

#[derive(Default)]
struct Drained {
    text: String,
    reasoning: String,
    calls: Vec<ToolCall>,
    stop: Option<StopReason>,
}

async fn drain(stream: dal_provider::EventStream) -> Drained {
    let mut drained = Drained::default();
    let mut stops = 0_u32;
    let mut events = std::pin::pin!(stream);
    while let Some(event) = events.next().await {
        match event.expect("live stream decodes") {
            StreamEvent::TextDelta { text } => drained.text.push_str(&text),
            StreamEvent::ReasoningDelta { text } => drained.reasoning.push_str(&text),
            StreamEvent::ToolCallsDone { calls } => drained.calls.extend(calls),
            StreamEvent::Stop { reason } => {
                stops += 1;
                drained.stop = Some(reason);
            }
            _ => {}
        }
    }
    assert_eq!(stops, 1, "live turn ends with exactly one terminal");
    drained
}

fn turn_request(
    route: ModelRoute,
    thinking: ThinkingLevel,
    tools: Vec<ModelToolSpec>,
    prompt: &str,
) -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: route,
        system: Arc::from("You are terse."),
        tools: Arc::from(tools),
        context: Arc::from([ContextItem::User {
            parts: vec![Part::Text {
                text: prompt.into(),
            }],
        }]),
        params: RequestParams {
            thinking,
            effort: None,
            temperature: None,
            // Thinking models spend tokens on reasoning first: bound the turn
            // without starving the answer.
            max_output_tokens: Some(1024),
        },
        cache_key: None,
    }
}

fn notices() -> NoticeSink {
    Arc::new(|_| {})
}

async fn run(provider: &Provider, request: &ModelRequest) -> Drained {
    let stream = provider
        .open(
            SessionId::new_v7(),
            request,
            &[],
            notices(),
            &CancellationToken::new(),
        )
        .await
        .expect("live ollama turn opens");
    drain(stream).await
}

fn time_tool() -> ModelToolSpec {
    ModelToolSpec {
        name: "get_time".into(),
        description: "Report the current local time for a city.".into(),
        parameters: RawJson::parse(
            r#"{"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}"#,
        )
        .expect("tool schema parses"),
        grammar: None,
        defer_loading: false,
    }
}

/// Behavior `live-ollama-discovery`: `GET api/tags` joined to `POST api/show`
/// on a real server yields each model's capabilities, not family defaults.
#[tokio::test]
#[ignore = "live: needs a running Ollama server named by DAL_LIVE_OLLAMA_URL"]
async fn live_ollama_discovery_joins_tags_and_show() {
    let (thinker_id, plain_id) = (thinker(), plain());
    let (set, cache_dir) = live_set("ollama-discovery");
    let catalog = set.catalog().await.expect("live catalog loads");
    let row = |id: &str| -> CatalogEntry {
        catalog
            .entries()
            .iter()
            .find(|entry| entry.provider.as_ref() == "ollama" && entry.id.as_ref() == id)
            .unwrap_or_else(|| panic!("discovery listed no row for {id}"))
            .clone()
    };

    let thinking = row(&thinker_id);
    assert_eq!(thinking.tool_support, ToolSupport::Any);
    assert!(
        thinking.context_window.is_some_and(|window| window > 0),
        "{thinker_id} reports a context window"
    );
    match &thinking.thinking {
        ThinkingSupport::OpenAi { accepted, .. } => {
            assert!(
                !accepted.is_empty(),
                "{thinker_id} reports reasoning levels"
            );
        }
        other => panic!("{thinker_id} carries {other:?}"),
    }

    let plain_row = row(&plain_id);
    assert_eq!(plain_row.tool_support, ToolSupport::None);
    assert!(
        plain_row.context_window.is_some_and(|window| window > 0),
        "{plain_id} reports a context window"
    );
    match &plain_row.thinking {
        ThinkingSupport::OpenAi {
            accepted,
            none_supported,
        } => assert!(
            accepted.is_empty() && !none_supported,
            "{plain_id} sends no reasoning control: {accepted:?} {none_supported}"
        ),
        other => panic!("{plain_id} carries {other:?}"),
    }
    set.shutdown().await;
    std::fs::remove_dir_all(&cache_dir).ok();
}

/// Behavior `live-ollama-thinking-stream`: a reasoning turn streams native
/// `message.thinking` as reasoning deltas, then ends with one terminal.
#[tokio::test]
#[ignore = "live: needs a running Ollama server named by DAL_LIVE_OLLAMA_URL"]
async fn live_ollama_thinking_turn_streams_reasoning() {
    let (set, cache_dir) = live_set("ollama-thinking");
    let (provider, route, _) = bind(&set, &thinker()).await;
    let request = turn_request(
        route,
        ThinkingLevel::High,
        vec![],
        "What is 17 plus 25? Answer with the number only.",
    );
    let drained = run(&provider, &request).await;
    assert!(
        !drained.reasoning.is_empty(),
        "a thinking-level turn streamed no reasoning"
    );
    assert!(
        matches!(
            drained.stop,
            Some(StopReason::EndTurn | StopReason::MaxTokens)
        ),
        "turn ended with {:?}",
        drained.stop
    );
    set.shutdown().await;
    std::fs::remove_dir_all(&cache_dir).ok();
}

/// Behavior `live-ollama-thinking-off`: level `off` sends `think: false` and
/// the server streams an answer with no reasoning.
#[tokio::test]
#[ignore = "live: needs a running Ollama server named by DAL_LIVE_OLLAMA_URL"]
async fn live_ollama_thinking_off_streams_no_reasoning() {
    let (set, cache_dir) = live_set("ollama-thinking-off");
    let (provider, route, _) = bind(&set, &thinker()).await;
    let request = turn_request(route, ThinkingLevel::Off, vec![], "Say hello in one word.");
    let drained = run(&provider, &request).await;
    assert_eq!(drained.reasoning, "", "think:false still reasoned");
    assert!(!drained.text.is_empty(), "the turn produced no answer");
    assert!(matches!(drained.stop, Some(StopReason::EndTurn)));
    set.shutdown().await;
    std::fs::remove_dir_all(&cache_dir).ok();
}

/// Behavior `live-ollama-plain-model-thinking-level`: a model without the
/// `thinking` capability takes a thinking level without a `think` member:
/// the server answers `400 does not support thinking` to `think: true`, so a
/// completed turn proves the member was omitted.
#[tokio::test]
#[ignore = "live: needs a running Ollama server named by DAL_LIVE_OLLAMA_URL"]
async fn live_ollama_plain_model_takes_a_thinking_level_without_think() {
    let (set, cache_dir) = live_set("ollama-plain");
    let (provider, route, _) = bind(&set, &plain()).await;
    let request = turn_request(
        route,
        ThinkingLevel::High,
        vec![],
        "Say hello in one short sentence.",
    );
    let drained = run(&provider, &request).await;
    assert_eq!(drained.reasoning, "");
    assert!(!drained.text.is_empty(), "the turn produced no answer");
    assert!(matches!(drained.stop, Some(StopReason::EndTurn)));
    set.shutdown().await;
    std::fs::remove_dir_all(&cache_dir).ok();
}

/// Behavior `live-ollama-tool-turn`: a tool turn completes at least one call
/// whose arguments keep their raw JSON bytes, under the synthesized
/// `ollama:<ordinal>:<name>` id.
#[tokio::test]
#[ignore = "live: needs a running Ollama server named by DAL_LIVE_OLLAMA_URL"]
async fn live_ollama_tool_turn_completes_a_call() {
    let (set, cache_dir) = live_set("ollama-tool");
    let (provider, route, _) = bind(&set, &thinker()).await;
    let request = turn_request(
        route,
        ThinkingLevel::Off,
        vec![time_tool()],
        "Call the get_time tool with city Seoul. Do not answer in text.",
    );
    let drained = run(&provider, &request).await;
    assert!(!drained.calls.is_empty(), "the turn completed no tool call");
    assert!(
        matches!(drained.stop, Some(StopReason::ToolUse)),
        "calls completed without a ToolUse stop: {:?}",
        drained.stop
    );
    for (index, call) in drained.calls.iter().enumerate() {
        assert_eq!(call.id, format!("ollama:{index}:{}", call.name));
        match &call.args {
            ToolArgs::Parsed(raw) => {
                RawJson::parse(raw.as_str()).expect("assembled tool args are one JSON value");
            }
            other => panic!("tool args did not assemble: {other:?}"),
        }
    }
    set.shutdown().await;
    std::fs::remove_dir_all(&cache_dir).ok();
}
