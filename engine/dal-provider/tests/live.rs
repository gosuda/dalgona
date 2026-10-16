#![expect(
    clippy::expect_used,
    reason = "live tests fail loudly on missing prerequisites"
)]
#![expect(
    clippy::panic,
    reason = "live tests fail loudly on missing prerequisites"
)]
#![expect(
    clippy::disallowed_methods,
    reason = "live tests read the process edge and drive spawned usage tasks"
)]
//! Live provider integration suite: real backends, real credentials.
//!
//! Every case is `#[ignore]`d and runs only under an explicit opt-in with
//! network access and credentials at the test-process edge:
//!
//! - `DAL_LIVE_DATA_DIR` names the data directory whose `auth.json` holds the
//!   `openai-codex` (`ChatGPT` OAuth) and `anthropic` (Claude OAuth) entries;
//!   create them with `dalgon login openai-codex` / `dalgon login anthropic`.
//! - `ZENMUX_API_KEY` carries the zenmux key (`export ZENMUX_API_KEY`).
//! - `DAL_LIVE_RECORD`, when set to a directory, additionally writes each
//!   case's decoded stream as replay-JSONL fixture lines.
//!
//! Parent command:
//! `cargo test --locked -p dal-provider --test live -- --ignored`
//!
//! A missing prerequisite fails the case with
//! `live suite: missing credential <name>: <how>`; no case skips or passes
//! without its credential, and no case asserts on model text.
//!
//! Coverage limits, stated plainly: the provider layer hardcodes the wire
//! `tool_choice` to `auto` on every family, so no request can force a tool
//! call. The tool-turn cases still offer a tool over the real transport, but
//! they require at least one completed call: an all-text answer fails the
//! case instead of passing vacuously, since a turn that never assembles a
//! call proves nothing about the named property.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use dal_core::{
    AssistantPart, ContextItem, Family, ModelRequest, ModelRoute, ModelToolSpec, Part, Purpose,
    RawJson, ReplaySource, RequestParams, ThinkingLevel,
};
use dal_provider::{
    AuthStore, AuthStyle, Capture, CompactOutcome, CredentialKind, EnvSnapshot, NoticeSink,
    Provider, ProviderConfig, ProviderEntry, ProviderIdentity, ProviderSet, ReplayPayload,
    ResolvedModel, StopReason, StreamEvent, ToolArgs, ToolCall, Transport, find, items_for, record,
};
use tokio_util::sync::CancellationToken;

/// How to mint the `openai-codex` credential named in precondition failures.
const CODEX_HOW: &str = "run dalgon login openai-codex with DAL_LIVE_DATA_DIR set";
/// How to mint the `anthropic` credential named in precondition failures.
const ANTHROPIC_HOW: &str = "run dalgon login anthropic with DAL_LIVE_DATA_DIR set";
/// Open-weight models under test, as zenmux-native ids.
const OPEN_MODELS: &[&str] = &[
    "deepseek/deepseek-v4-pro",
    "moonshotai/kimi-k3",
    "z-ai/glm-5.2",
];

/// The live data directory, or the credential-specific failure.
fn live_data_dir(credential: &str, how: &str) -> PathBuf {
    let message = format!("live suite: missing credential {credential}: {how}");
    let dir = std::env::var("DAL_LIVE_DATA_DIR").expect(message.as_str());
    assert!(!dir.is_empty(), "{message}");
    PathBuf::from(dir)
}

/// The `auth.json` OAuth entry must already exist; an API-key entry does not
/// satisfy the OAuth gate. Never skip without it.
fn require_oauth(data_dir: &Path, provider: &str, how: &str) {
    let store = AuthStore::load(data_dir.join("auth.json"))
        .unwrap_or_else(|_| panic!("live suite: missing credential {provider}: {how}"));
    let oauth = store
        .status()
        .iter()
        .any(|entry| entry.provider.as_ref() == provider && entry.kind == CredentialKind::OAuth);
    assert!(oauth, "live suite: missing credential {provider}: {how}");
}

/// The zenmux key, read once at the test-process edge.
fn zenmux_key() -> String {
    let key = std::env::var("ZENMUX_API_KEY").unwrap_or_default();
    assert!(
        !key.is_empty(),
        "live suite: missing credential ZENMUX_API_KEY: export ZENMUX_API_KEY"
    );
    key
}

/// A fresh cache directory per case; the live data directory is never the cache.
fn live_cache_dir(case: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "dalgona-live-{case}-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("live cache dir");
    dir
}

/// Identity stamped on live requests: the real client version and host.
fn identity() -> ProviderIdentity {
    ProviderIdentity {
        version: env!("CARGO_PKG_VERSION").into(),
        os: std::env::consts::OS.into(),
        os_version: "live".into(),
        arch: std::env::consts::ARCH.into(),
    }
}

/// Test-shaped config mirroring the replay suite: no retries, no aliases.
fn live_config(entries: Vec<ProviderEntry>) -> ProviderConfig {
    ProviderConfig {
        default_model: None,
        thinking: ThinkingLevel::Medium,
        aliases: Vec::new(),
        request_max_retries: 0,
        stream_max_retries: 0,
        providers: entries,
        scripted: None,
    }
}

/// The built-in Codex row with an explicit transport per case.
fn codex_entry(transport: Transport) -> ProviderEntry {
    let def = find("openai-codex").expect("builtin openai-codex row");
    ProviderEntry {
        id: def.id.into(),
        def: Some(def),
        family: def.family,
        base_url: def.base_url.into(),
        transport,
        key_env: None,
        auth: AuthStyle::Bearer,
        max_concurrent_requests: 4,
    }
}

/// The built-in Anthropic row.
fn anthropic_entry() -> ProviderEntry {
    let def = find("anthropic").expect("builtin anthropic row");
    ProviderEntry {
        id: def.id.into(),
        def: Some(def),
        family: def.family,
        base_url: def.base_url.into(),
        transport: Transport::Https,
        key_env: None,
        auth: AuthStyle::XApiKey,
        max_concurrent_requests: 4,
    }
}

/// Zenmux uses a separate base URL for the Anthropic family.
fn zenmux_entry(family: Family) -> ProviderEntry {
    let (base_url, auth) = if family == Family::Anthropic {
        ("https://zenmux.ai/api/anthropic", AuthStyle::XApiKey)
    } else {
        ("https://zenmux.ai/api/v1", AuthStyle::Bearer)
    };
    ProviderEntry {
        id: "zenmux".into(),
        def: None,
        family,
        base_url: base_url.into(),
        transport: Transport::Https,
        key_env: Some("ZENMUX_API_KEY".into()),
        auth,
        max_concurrent_requests: 4,
    }
}

/// One isolated host per case: its own config, snapshot, and cache.
fn live_set(
    case: &str,
    entries: Vec<ProviderEntry>,
    env: EnvSnapshot,
    data_dir: &Path,
) -> (ProviderSet, PathBuf) {
    let config = live_config(entries);
    let cache_dir = live_cache_dir(case);
    let set = ProviderSet::new(&config, identity(), env, data_dir, &cache_dir)
        .expect("live provider set builds");
    (set, cache_dir)
}

/// Resolve through the source-aware catalog, then bind: the production path.
async fn bind(set: &ProviderSet, reference: &str) -> (Provider, ModelRoute) {
    let catalog = set.catalog().await.expect("live catalog loads");
    let resolved: ResolvedModel = set
        .resolve(&catalog, reference)
        .expect("live model resolves");
    let route = resolved.route.clone();
    let provider = set.provider(resolved).expect("live provider binds");
    (provider, route)
}

/// The decoded shape of one drained turn.
#[derive(Default)]
struct Drained {
    text: String,
    reasoning: String,
    replays: Vec<ReplayPayload>,
    calls: Vec<ToolCall>,
    stop: Option<StopReason>,
}

/// Drain to the terminal, recording when asked; exactly one `Stop` may arrive.
async fn drain(stream: dal_provider::EventStream, capture: Option<&Capture>) -> Drained {
    let stream = match capture {
        Some(capture) => record(stream, capture.clone()),
        None => stream,
    };
    let mut drained = Drained::default();
    let mut stops = 0_u32;
    let mut events = std::pin::pin!(stream);
    while let Some(event) = events.next().await {
        match event.expect("live stream decodes") {
            StreamEvent::TextDelta { text } => drained.text.push_str(&text),
            StreamEvent::ReasoningDelta { text } => drained.reasoning.push_str(&text),
            StreamEvent::Replay { payload } => drained.replays.push(payload),
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

/// The first turn's replayable thinking, bound to one family and model.
struct ThinkingTranscript {
    text: String,
    reasoning: String,
    items: Vec<RawJson>,
}

fn transcript(drained: &Drained, family: Family, model: &str, case: &str) -> ThinkingTranscript {
    let items: Vec<RawJson> = drained
        .replays
        .iter()
        .filter(|payload| payload.family == family && payload.model.as_ref() == model)
        .map(|payload| payload.item.clone())
        .collect();
    assert!(
        !items.is_empty(),
        "{case} produced no replay payload for its second turn"
    );
    ThinkingTranscript {
        text: drained.text.clone(),
        reasoning: drained.reasoning.clone(),
        items,
    }
}

/// A plain text turn request.
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
            max_output_tokens: None,
        },
        cache_key: None,
    }
}

/// A second turn carrying the first turn's thinking replay verbatim.
fn replay_request(
    route: ModelRoute,
    family: Family,
    model: &str,
    first: &ThinkingTranscript,
    first_prompt: &str,
    prompt: &str,
) -> ModelRequest {
    let source = ReplaySource {
        family,
        model: model.into(),
    };
    let mut parts = Vec::new();
    if !first.text.is_empty() {
        parts.push(AssistantPart::Text {
            text: first.text.clone().into(),
            replay: None,
        });
    }
    for item in &first.items {
        parts.push(AssistantPart::Thinking {
            text: first.reasoning.clone().into(),
            replay: Some(item.clone()),
        });
    }
    ModelRequest {
        purpose: Purpose::Turn,
        model: route,
        system: Arc::from("You are terse."),
        tools: Arc::from([]),
        context: Arc::from([
            ContextItem::User {
                parts: vec![Part::Text {
                    text: first_prompt.into(),
                }],
            },
            ContextItem::Assistant {
                source,
                parts,
                replay: None,
            },
            ContextItem::User {
                parts: vec![Part::Text {
                    text: prompt.into(),
                }],
            },
        ]),
        params: RequestParams {
            thinking: ThinkingLevel::High,
            effort: None,
            temperature: None,
            max_output_tokens: None,
        },
        cache_key: None,
    }
}

/// The single tool every tool-turn case offers.
fn weather_tool() -> ModelToolSpec {
    ModelToolSpec {
        name: "get_weather".into(),
        description: "Report the current weather for a city.".into(),
        parameters: RawJson::parse(
            r#"{"type":"object","properties":{"location":{"type":"string"}},"required":["location"]}"#,
        )
        .expect("tool schema parses"),
        grammar: None,
        defer_loading: false,
    }
}

/// Assert one completed call assembled exactly one valid JSON value.
fn assert_assembled_json(call: &ToolCall, case: &str) {
    match &call.args {
        ToolArgs::Parsed(raw) => {
            RawJson::parse(raw.as_str()).expect("assembled tool args are one JSON value");
        }
        ToolArgs::Invalid { message } => {
            panic!("{case} assembled invalid tool JSON: {message}");
        }
        ToolArgs::Truncated => {
            panic!("{case} ended before tool arguments completed");
        }
    }
}

/// Assert the tool-turn contract: at least one completed call, a `ToolUse`
/// terminal, and valid JSON per completed call.
fn assert_tool_turn(drained: &Drained, case: &str) {
    assert!(!drained.calls.is_empty(), "{case} completed no tool call");
    assert!(
        matches!(drained.stop, Some(StopReason::ToolUse)),
        "{case} completed calls without a ToolUse stop"
    );
    for call in &drained.calls {
        assert_assembled_json(call, case);
    }
}

/// A `DAL_LIVE_RECORD` sink, when the operator asked for fixture lines.
fn recorder(case: &str) -> Option<(Capture, PathBuf)> {
    match std::env::var("DAL_LIVE_RECORD") {
        Ok(dir) if !dir.is_empty() => {
            let dir = PathBuf::from(dir);
            std::fs::create_dir_all(&dir).expect("live record dir");
            Some((Capture::new(), dir.join(format!("{case}.jsonl"))))
        }
        _ => None,
    }
}

/// Write the case's decoded fixture lines, if recording was requested.
fn flush_recording(recording: Option<(Capture, PathBuf)>) {
    let Some((capture, path)) = recording else {
        return;
    };
    let lines = capture.replay().expect("live capture encodes");
    assert!(!lines.is_empty(), "live recording captured no stream");
    std::fs::write(&path, lines.join("\n")).expect("live recording writes");
}

/// Drive spawned usage tasks on the test runtime.
fn spawn_on_tokio(task: std::pin::Pin<Box<dyn Future<Output = ()> + Send + 'static>>) {
    tokio::spawn(task);
}

fn notices() -> NoticeSink {
    Arc::new(|_| {})
}

/// Behavior `live-codex-https-text`: Codex HTTPS text turn on `gpt-5.6-luna`.
#[tokio::test]
#[ignore = "live: needs Codex OAuth in DAL_LIVE_DATA_DIR and network access"]
async fn live_codex_https_text() {
    let data_dir = live_data_dir("openai-codex", CODEX_HOW);
    require_oauth(&data_dir, "openai-codex", CODEX_HOW);
    let recording = recorder("live-codex-https-text");
    let capture = recording.as_ref().map(|recording| &recording.0);
    let (set, cache_dir) = live_set(
        "live-codex-https-text",
        vec![codex_entry(Transport::Https)],
        EnvSnapshot::test(&[]),
        &data_dir,
    );
    let (provider, route) = bind(&set, "openai-codex/gpt-5.6-luna").await;
    let request = turn_request(
        route,
        ThinkingLevel::Off,
        vec![],
        "Say hello in one short sentence.",
    );
    let cancel = CancellationToken::new();
    let stream = provider
        .open(
            dal_core::SessionId::new_v7(),
            &request,
            &[],
            notices(),
            &cancel,
        )
        .await
        .expect("live codex text turn opens");
    let drained = drain(stream, capture).await;
    assert!(
        matches!(drained.stop, Some(StopReason::EndTurn)),
        "live codex text turn ends the turn"
    );
    flush_recording(recording);
    set.shutdown().await;
    std::fs::remove_dir_all(&cache_dir).ok();
}

/// Behavior `live-codex-websocket-tool`: Codex WebSocket tool turn; at least
/// one completed call assembles to valid JSON. No request can force the
/// call — see the module docs — so the prompt elicits it, and an all-text
/// answer fails the case instead of passing vacuously.
#[tokio::test]
#[ignore = "live: needs Codex OAuth in DAL_LIVE_DATA_DIR and network access"]
async fn live_codex_websocket_tool() {
    let data_dir = live_data_dir("openai-codex", CODEX_HOW);
    require_oauth(&data_dir, "openai-codex", CODEX_HOW);
    let recording = recorder("live-codex-websocket-tool");
    let capture = recording.as_ref().map(|recording| &recording.0);
    let (set, cache_dir) = live_set(
        "live-codex-websocket-tool",
        vec![codex_entry(Transport::Websocket)],
        EnvSnapshot::test(&[]),
        &data_dir,
    );
    let (provider, route) = bind(&set, "openai-codex/gpt-5.6-luna").await;
    let request = turn_request(
        route,
        ThinkingLevel::Off,
        vec![weather_tool()],
        "What is the weather in San Francisco, CA? Use the get_weather tool.",
    );
    let cancel = CancellationToken::new();
    let stream = provider
        .open(
            dal_core::SessionId::new_v7(),
            &request,
            &[],
            notices(),
            &cancel,
        )
        .await
        .expect("live codex tool turn opens");
    let drained = drain(stream, capture).await;
    assert_tool_turn(&drained, "live-codex-websocket-tool");
    flush_recording(recording);
    set.shutdown().await;
    std::fs::remove_dir_all(&cache_dir).ok();
}

/// Behavior `live-codex-thinking-replay`: Codex thinking `high` over two
/// turns; the second turn replays the reasoning item and succeeds.
#[tokio::test]
#[ignore = "live: needs Codex OAuth in DAL_LIVE_DATA_DIR and network access"]
async fn live_codex_thinking_replay() {
    let data_dir = live_data_dir("openai-codex", CODEX_HOW);
    require_oauth(&data_dir, "openai-codex", CODEX_HOW);
    let recording = recorder("live-codex-thinking-replay");
    let capture = recording.as_ref().map(|recording| &recording.0);
    let (set, cache_dir) = live_set(
        "live-codex-thinking-replay",
        vec![codex_entry(Transport::Https)],
        EnvSnapshot::test(&[]),
        &data_dir,
    );
    let (provider, route) = bind(&set, "openai-codex/gpt-5.6-luna").await;
    let first_prompt = "Think step by step, then answer in one short sentence.";
    let request = turn_request(route.clone(), ThinkingLevel::High, vec![], first_prompt);
    let cancel = CancellationToken::new();
    let stream = provider
        .open(
            dal_core::SessionId::new_v7(),
            &request,
            &[],
            notices(),
            &cancel,
        )
        .await
        .expect("live codex thinking turn opens");
    let first = drain(stream, capture).await;
    assert!(
        matches!(first.stop, Some(StopReason::EndTurn)),
        "live codex thinking turn ends the turn"
    );
    let first = transcript(
        &first,
        Family::Codex,
        "gpt-5.6-luna",
        "live-codex-thinking-replay",
    );
    let replayed = replay_request(
        route,
        Family::Codex,
        "gpt-5.6-luna",
        &first,
        first_prompt,
        "Continue.",
    );
    let cancel = CancellationToken::new();
    let stream = provider
        .open(
            dal_core::SessionId::new_v7(),
            &replayed,
            &[],
            notices(),
            &cancel,
        )
        .await
        .expect("live codex replay turn opens");
    let drained = drain(stream, capture).await;
    assert!(
        matches!(drained.stop, Some(StopReason::EndTurn)),
        "live codex replay turn ends the turn"
    );
    flush_recording(recording);
    set.shutdown().await;
    std::fs::remove_dir_all(&cache_dir).ok();
}

/// Behavior `live-codex-compaction`: Codex remote compaction returns non-empty
/// history bound to the requested route. The following fresh turn only checks
/// that the provider remains usable. `ModelRequest` has no semantically valid
/// context item for provider-native compacted history, so this case does not
/// claim continuation.
#[tokio::test]
#[ignore = "live: needs Codex OAuth in DAL_LIVE_DATA_DIR and network access"]
async fn live_codex_compaction() {
    let data_dir = live_data_dir("openai-codex", CODEX_HOW);
    require_oauth(&data_dir, "openai-codex", CODEX_HOW);
    // `compact` returns no `EventStream`; this recorder captures only the
    // independent turn below.
    let recording = recorder("live-codex-compaction-independent-turn");
    let (set, cache_dir) = live_set(
        "live-codex-compaction",
        vec![codex_entry(Transport::Https)],
        EnvSnapshot::test(&[]),
        &data_dir,
    );
    let (provider, route) = bind(&set, "openai-codex/gpt-5.6-luna").await;
    let source = ReplaySource {
        family: Family::Codex,
        model: "gpt-5.6-luna".into(),
    };
    let compact_request = ModelRequest {
        purpose: Purpose::Compact,
        model: route.clone(),
        system: Arc::from("You are terse."),
        tools: Arc::from([]),
        context: Arc::from([
            ContextItem::User {
                parts: vec![Part::Text {
                    text: "We discussed the release notes.".into(),
                }],
            },
            ContextItem::Assistant {
                source,
                parts: vec![AssistantPart::Text {
                    text: "Here is a short draft.".into(),
                    replay: None,
                }],
                replay: None,
            },
            ContextItem::User {
                parts: vec![Part::Text {
                    text: "Trim it further.".into(),
                }],
            },
        ]),
        params: RequestParams {
            thinking: ThinkingLevel::Off,
            effort: None,
            temperature: None,
            max_output_tokens: None,
        },
        cache_key: None,
    };
    let cancel = CancellationToken::new();
    let outcome = provider
        .compact(
            dal_core::SessionId::new_v7(),
            &compact_request,
            notices(),
            &cancel,
        )
        .await
        .expect("live codex compaction answers");
    let Some(CompactOutcome::Compacted(history)) = outcome else {
        panic!("live codex compaction returned no compaction item");
    };
    let items = items_for(&history, Family::Codex, "gpt-5.6-luna")
        .expect("compaction items belong to the live route");
    assert!(
        !items.is_empty(),
        "live codex compaction returned no compaction item"
    );
    // Responses would serialize `Thinking::replay` bytes verbatim, but that
    // reasoning-only lane would misclassify retained user messages and the
    // compaction item.
    let independent_request = turn_request(
        route,
        ThinkingLevel::Off,
        vec![],
        "Say hello in one short sentence.",
    );
    let cancel = CancellationToken::new();
    let stream = provider
        .open(
            dal_core::SessionId::new_v7(),
            &independent_request,
            &[],
            notices(),
            &cancel,
        )
        .await
        .expect("live independent turn after compaction opens");
    let drained = drain(stream, recording.as_ref().map(|recording| &recording.0)).await;
    assert!(
        matches!(drained.stop, Some(StopReason::EndTurn)),
        "live independent turn after compaction ends the turn"
    );
    flush_recording(recording);
    set.shutdown().await;
    std::fs::remove_dir_all(&cache_dir).ok();
}

/// Behavior `live-codex-models-list`: Codex `/models` lists `gpt-5.6-luna`.
#[tokio::test]
#[ignore = "live: needs Codex OAuth in DAL_LIVE_DATA_DIR and network access"]
async fn live_codex_models_list() {
    let data_dir = live_data_dir("openai-codex", CODEX_HOW);
    require_oauth(&data_dir, "openai-codex", CODEX_HOW);
    let (set, cache_dir) = live_set(
        "live-codex-models-list",
        vec![codex_entry(Transport::Https)],
        EnvSnapshot::test(&[]),
        &data_dir,
    );
    let fetch = set
        .load_models("openai-codex")
        .await
        .expect("live codex model list loads");
    assert!(
        fetch
            .entries
            .iter()
            .any(|entry| entry.id.as_ref() == "gpt-5.6-luna"),
        "live codex /models does not list gpt-5.6-luna"
    );
    let (provider, _) = bind(&set, "openai-codex/gpt-5.6-luna").await;
    let models = provider.models().await.expect("live codex models project");
    assert!(
        models.iter().any(|info| info.route.id() == "gpt-5.6-luna"),
        "live codex models projection does not list gpt-5.6-luna"
    );
    set.shutdown().await;
    std::fs::remove_dir_all(&cache_dir).ok();
}

/// Behavior `live-codex-usage-read`: Codex usage read returns `Ok` with any
/// verdict; proves backend acceptance of the live Codex account route.
#[tokio::test]
#[ignore = "live: needs Codex OAuth in DAL_LIVE_DATA_DIR and network access"]
async fn live_codex_usage_read() {
    let data_dir = live_data_dir("openai-codex", CODEX_HOW);
    require_oauth(&data_dir, "openai-codex", CODEX_HOW);
    let (set, cache_dir) = live_set(
        "live-codex-usage-read",
        vec![codex_entry(Transport::Https)],
        EnvSnapshot::test(&[]),
        &data_dir,
    );
    let (provider, _) = bind(&set, "openai-codex/gpt-5.6-luna").await;
    let verdict = provider.usage(&spawn_on_tokio).await;
    assert!(
        verdict.is_ok(),
        "live codex usage read returns Ok, got {verdict:?}"
    );
    set.shutdown().await;
    std::fs::remove_dir_all(&cache_dir).ok();
}

/// The Claude thinking-signature replay over two turns.
async fn claude_thinking_replay(provider: &Provider, route: ModelRoute, capture: Option<&Capture>) {
    let first_prompt = "Think step by step, then answer in one short sentence.";
    let request = turn_request(route.clone(), ThinkingLevel::High, vec![], first_prompt);
    let cancel = CancellationToken::new();
    let stream = provider
        .open(
            dal_core::SessionId::new_v7(),
            &request,
            &[],
            notices(),
            &cancel,
        )
        .await
        .expect("live claude thinking turn opens");
    let first = drain(stream, capture).await;
    assert!(
        matches!(first.stop, Some(StopReason::EndTurn)),
        "live claude thinking turn ends the turn"
    );
    let first = transcript(
        &first,
        Family::Anthropic,
        "claude-sonnet-5",
        "live-claude-oauth-turns thinking turn",
    );
    let replayed = replay_request(
        route,
        Family::Anthropic,
        "claude-sonnet-5",
        &first,
        first_prompt,
        "Continue.",
    );
    let cancel = CancellationToken::new();
    let stream = provider
        .open(
            dal_core::SessionId::new_v7(),
            &replayed,
            &[],
            notices(),
            &cancel,
        )
        .await
        .expect("live claude replay turn opens");
    let drained = drain(stream, capture).await;
    assert!(
        matches!(drained.stop, Some(StopReason::EndTurn)),
        "live claude replay turn ends the turn"
    );
}

/// Behavior `live-claude-oauth-turns`: `claude-sonnet-5` with Claude OAuth —
/// a text turn, a tool turn, and a thinking signature replay over two turns.
#[tokio::test]
#[ignore = "live: needs Claude OAuth in DAL_LIVE_DATA_DIR and network access"]
async fn live_claude_oauth_turns() {
    let data_dir = live_data_dir("anthropic", ANTHROPIC_HOW);
    require_oauth(&data_dir, "anthropic", ANTHROPIC_HOW);
    let recording = recorder("live-claude-oauth-turns");
    let capture = recording.as_ref().map(|recording| &recording.0);
    let (set, cache_dir) = live_set(
        "live-claude-oauth-turns",
        vec![anthropic_entry()],
        EnvSnapshot::test(&[]),
        &data_dir,
    );
    let (provider, route) = bind(&set, "anthropic/claude-sonnet-5").await;

    let request = turn_request(
        route.clone(),
        ThinkingLevel::Off,
        vec![],
        "Say hello in one short sentence.",
    );
    let cancel = CancellationToken::new();
    let stream = provider
        .open(
            dal_core::SessionId::new_v7(),
            &request,
            &[],
            notices(),
            &cancel,
        )
        .await
        .expect("live claude text turn opens");
    let drained = drain(stream, capture).await;
    assert!(
        matches!(drained.stop, Some(StopReason::EndTurn)),
        "live claude text turn ends the turn"
    );

    let request = turn_request(
        route.clone(),
        ThinkingLevel::Off,
        vec![weather_tool()],
        "What is the weather in San Francisco, CA? Use the get_weather tool.",
    );
    let cancel = CancellationToken::new();
    let stream = provider
        .open(
            dal_core::SessionId::new_v7(),
            &request,
            &[],
            notices(),
            &cancel,
        )
        .await
        .expect("live claude tool turn opens");
    let drained = drain(stream, capture).await;
    assert_tool_turn(&drained, "live-claude-oauth-turns tool turn");

    claude_thinking_replay(&provider, route, capture).await;
    flush_recording(recording);
    set.shutdown().await;
    std::fs::remove_dir_all(&cache_dir).ok();
}

/// Behavior `live-zenmux-anthropic-tool`: zenmux Anthropic family
/// `claude-sonnet-5` tool turn.
#[tokio::test]
#[ignore = "live: needs ZENMUX_API_KEY and network access"]
async fn live_zenmux_anthropic_tool() {
    let key = zenmux_key();
    let data_dir = live_cache_dir("live-zenmux-anthropic-tool-data");
    let recording = recorder("live-zenmux-anthropic-tool");
    let capture = recording.as_ref().map(|recording| &recording.0);
    let (set, cache_dir) = live_set(
        "live-zenmux-anthropic-tool",
        vec![zenmux_entry(Family::Anthropic)],
        EnvSnapshot::test(&[("ZENMUX_API_KEY", key.as_str())]),
        &data_dir,
    );
    let (provider, route) = bind(&set, "zenmux/anthropic/claude-sonnet-5").await;
    let request = turn_request(
        route,
        ThinkingLevel::Off,
        vec![weather_tool()],
        "What is the weather in San Francisco, CA? Use the get_weather tool.",
    );
    let cancel = CancellationToken::new();
    let stream = provider
        .open(
            dal_core::SessionId::new_v7(),
            &request,
            &[],
            notices(),
            &cancel,
        )
        .await
        .expect("live zenmux anthropic tool turn opens");
    let drained = drain(stream, capture).await;
    assert_tool_turn(&drained, "live-zenmux-anthropic-tool");
    flush_recording(recording);
    set.shutdown().await;
    std::fs::remove_dir_all(&cache_dir).ok();
    std::fs::remove_dir_all(&data_dir).ok();
}

/// Behavior `live-zenmux-chat-tool`: zenmux Chat `openai/gpt-5.6-luna` tool turn.
#[tokio::test]
#[ignore = "live: needs ZENMUX_API_KEY and network access"]
async fn live_zenmux_chat_tool() {
    let key = zenmux_key();
    let data_dir = live_cache_dir("live-zenmux-chat-tool-data");
    let recording = recorder("live-zenmux-chat-tool");
    let capture = recording.as_ref().map(|recording| &recording.0);
    let (set, cache_dir) = live_set(
        "live-zenmux-chat-tool",
        vec![zenmux_entry(Family::Chat)],
        EnvSnapshot::test(&[("ZENMUX_API_KEY", key.as_str())]),
        &data_dir,
    );
    let (provider, route) = bind(&set, "zenmux/openai/gpt-5.6-luna").await;
    let request = turn_request(
        route,
        ThinkingLevel::Off,
        vec![weather_tool()],
        "What is the weather in San Francisco, CA? Use the get_weather tool.",
    );
    let cancel = CancellationToken::new();
    let stream = provider
        .open(
            dal_core::SessionId::new_v7(),
            &request,
            &[],
            notices(),
            &cancel,
        )
        .await
        .expect("live zenmux chat tool turn opens");
    let drained = drain(stream, capture).await;
    assert_tool_turn(&drained, "live-zenmux-chat-tool");
    flush_recording(recording);
    set.shutdown().await;
    std::fs::remove_dir_all(&cache_dir).ok();
    std::fs::remove_dir_all(&data_dir).ok();
}

/// Behavior `live-zenmux-open-models`: the live zenmux list must contain each
/// seeded open-model id — failing naming any it does not list — then one Chat
/// tool turn runs per seeded open model.
#[tokio::test]
#[ignore = "live: needs ZENMUX_API_KEY and network access"]
async fn live_zenmux_open_models() {
    let key = zenmux_key();
    let data_dir = live_cache_dir("live-zenmux-open-models-data");
    let recording = recorder("live-zenmux-open-models");
    let capture = recording.as_ref().map(|recording| &recording.0);
    let (set, cache_dir) = live_set(
        "live-zenmux-open-models",
        vec![zenmux_entry(Family::Chat)],
        EnvSnapshot::test(&[("ZENMUX_API_KEY", key.as_str())]),
        &data_dir,
    );
    let fetch = set
        .load_models("zenmux")
        .await
        .expect("live zenmux model list loads");
    for id in OPEN_MODELS {
        assert!(
            fetch.entries.iter().any(|entry| entry.id.as_ref() == *id),
            "live zenmux list is missing open model {id}"
        );
    }
    for id in OPEN_MODELS {
        let reference = format!("zenmux/{id}");
        let (provider, route) = bind(&set, reference.as_str()).await;
        let request = turn_request(
            route,
            ThinkingLevel::Off,
            vec![weather_tool()],
            "What is the weather in San Francisco, CA? Use the get_weather tool.",
        );
        let cancel = CancellationToken::new();
        let stream = provider
            .open(
                dal_core::SessionId::new_v7(),
                &request,
                &[],
                notices(),
                &cancel,
            )
            .await
            .expect("live zenmux open-model turn opens");
        let drained = drain(stream, capture).await;
        assert_tool_turn(&drained, reference.as_str());
    }
    flush_recording(recording);
    set.shutdown().await;
    std::fs::remove_dir_all(&cache_dir).ok();
    std::fs::remove_dir_all(&data_dir).ok();
}
