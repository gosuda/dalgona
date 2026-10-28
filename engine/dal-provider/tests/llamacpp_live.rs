#![expect(
    clippy::expect_used,
    reason = "live tests fail loudly on missing prerequisites"
)]
#![expect(
    clippy::disallowed_methods,
    reason = "live tests read the process edge"
)]
//! Live llama.cpp suite: a real `llama-server` on the wire, no credential.
//!
//! Every case is `#[ignore]`d and runs only against a server the operator
//! names at the test-process edge:
//!
//! - `DAL_LIVE_LLAMACPP_URL`: the server's `OpenAI` base URL, ending in `/v1`,
//!   for example `http://127.0.0.1:8080/v1`.
//! - `DAL_LIVE_LLAMACPP_MODEL`: the id the server lists, set with `--alias`
//!   (`qwen3-0.6b` below). The model must be a reasoning model whose chat
//!   template supports tools (`--jinja`) and whose reasoning surfaces as
//!   `reasoning_content` (`--reasoning-format deepseek`).
//!
//! Reproduction on a machine with no GPU (CPU inference of the 0.6B model is
//! enough; the suite was recorded against llama.cpp build b11717):
//!
//! ```text
//! tar -xzf llama-b11717-bin-ubuntu-x64.tar.gz -C "$DIR"   # official release
//! LD_LIBRARY_PATH="$DIR/llama-b11717" "$DIR/llama-b11717/llama-server" \
//!   -m Qwen3-0.6B-Q8_0.gguf --alias qwen3-0.6b --host 127.0.0.1 --port 8089 \
//!   --jinja -c 4096 --reasoning-format deepseek &
//! DAL_LIVE_LLAMACPP_URL=http://127.0.0.1:8089/v1 DAL_LIVE_LLAMACPP_MODEL=qwen3-0.6b \
//!   cargo test --locked -p dal-provider --test llamacpp_live -- --ignored
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
    EnvSnapshot, NoticeSink, Provider, ProviderConfig, ProviderEntry, ProviderIdentity,
    ProviderSet, ResolvedModel, StopReason, StreamEvent, ToolArgs, ToolCall,
};
use tokio_util::sync::CancellationToken;

fn required(name: &str, how: &str) -> String {
    let message = format!("live suite: missing prerequisite {name}: {how}");
    let value = std::env::var(name).expect(message.as_str());
    assert!(!value.is_empty(), "{message}");
    value
}

fn server_url() -> String {
    required(
        "DAL_LIVE_LLAMACPP_URL",
        "export the /v1 base URL of a running llama-server",
    )
}

fn model_id() -> String {
    required(
        "DAL_LIVE_LLAMACPP_MODEL",
        "export the model id the server lists (llama-server --alias)",
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

/// The built-in `llamacpp` row pointed at the live server, credential-less:
/// the row keeps its bearer default, and a keyless local server takes the
/// documented `auth = "none"` override.
fn llamacpp_entry(url: &str) -> ProviderEntry {
    let table = format!(
        "[providers.llamacpp]\nbase_url = \"{url}\"\nauth = \"none\"\nmax_concurrent_requests = 1\n"
    );
    let core = Config::load(
        ConfigProduct::Dalgon,
        std::path::Path::new("/tmp/dal-provider-llamacpp-live"),
        "",
        Some(&table),
    )
    .expect("provider table parses");
    ProviderConfig::from_config(&core)
        .expect("providers decode")
        .providers
        .into_iter()
        .find(|provider| &*provider.id == "llamacpp")
        .expect("llamacpp decoded")
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
        providers: vec![llamacpp_entry(&server_url())],
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

async fn bind(set: &ProviderSet) -> (Provider, ModelRoute) {
    let catalog = set.catalog().await.expect("live catalog loads");
    let resolved: ResolvedModel = set
        .resolve(&catalog, &format!("llamacpp/{}", model_id()))
        .expect("live model resolves");
    let route = resolved.route.clone();
    let provider = set.provider(resolved).expect("live provider binds");
    (provider, route)
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
        .expect("live llama.cpp turn opens");
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

/// Behavior `live-llamacpp-discovery`: `GET /v1/models` on a real
/// `llama-server` lists the served model under the row's provider id.
#[tokio::test]
#[ignore = "live: needs a running llama-server named by DAL_LIVE_LLAMACPP_URL"]
async fn live_llamacpp_discovery_lists_the_served_model() {
    let (set, cache_dir) = live_set("llamacpp-discovery");
    let catalog = set.catalog().await.expect("live catalog loads");
    let id = model_id();
    assert!(
        catalog
            .entries()
            .iter()
            .any(|entry| entry.provider.as_ref() == "llamacpp" && entry.id.as_ref() == id),
        "discovery listed no llamacpp row for {id}"
    );
    set.shutdown().await;
    std::fs::remove_dir_all(&cache_dir).ok();
}

/// Behavior `live-llamacpp-reasoning-stream`: llama.cpp's `reasoning_content`
/// deltas stream as reasoning, then the answer streams as text.
#[tokio::test]
#[ignore = "live: needs a running llama-server named by DAL_LIVE_LLAMACPP_URL"]
async fn live_llamacpp_reasoning_content_streams_as_reasoning() {
    let (set, cache_dir) = live_set("llamacpp-reasoning");
    let (provider, route) = bind(&set).await;
    let request = turn_request(
        route,
        ThinkingLevel::High,
        vec![],
        "What is 17 plus 25? Answer with the number only.",
    );
    let drained = run(&provider, &request).await;
    assert!(
        !drained.reasoning.is_empty(),
        "reasoning_content streamed no reasoning delta"
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

/// Behavior `live-llamacpp-tool-turn`: a tool turn completes at least one call
/// whose arguments assemble to one valid JSON value.
#[tokio::test]
#[ignore = "live: needs a running llama-server named by DAL_LIVE_LLAMACPP_URL"]
async fn live_llamacpp_tool_turn_completes_a_call() {
    let (set, cache_dir) = live_set("llamacpp-tool");
    let (provider, route) = bind(&set).await;
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
    for call in &drained.calls {
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
