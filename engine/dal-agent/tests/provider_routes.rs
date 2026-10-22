//! Provider-aware model routes through a real host: a real session, real
//! journal and config files in a temp directory, and loopback provider
//! servers that record every request they receive.
//!
//! The proofs: same-family providers listing one native id, a synthetic id
//! spelled like a provider's native id, a saved provider default across a
//! restart and a journal reopen, typed failures for an unavailable provider,
//! and the legacy provider-less records and files.
#![expect(clippy::expect_used, reason = "test assertions abort on failure")]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::ext::{
    BoxFuture, Caller, Extension, ExtensionBuilder, HookCx, HookError, ModelCx, ModelError,
    ModelHandler, ModelRecord, ObserveHook, Services, SessionCx, SessionInstalls,
};
use dal_agent::{Agent, Delivery, Env, Host, HostError, Product, SessionRef, Subscription};
use dal_core::{
    ApprovalMode, Caps, ClientId, Command, Config, ConfigProduct, ContextItem, Entry, EntryId,
    EntryKind, Expect, Family, JournalPart, ModelId, ModelRequest, ModelRoute, PageReq, Part,
    Purpose, Record, Reply, RequestParams, Save, ServiceSet, SessionId, SessionStart, Stop,
    ThinkingLevel, UpdateKind, Usage, Workspace,
};
use dal_provider::{EventStream, ProviderError, StopReason, StreamEvent};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;

const CLOSE: Duration = Duration::from_secs(5);
const TURN: Duration = Duration::from_secs(30);
const KEY_ALPHA: &str = "key-alpha";
const KEY_BETA: &str = "key-beta";
const KEY_OPENROUTER: &str = "key-openrouter";
/// The provider-native id both same-family providers list.
const SHARED_ID: &str = "gpt-4o";
/// A native id of the `openrouter` provider that is also a synthetic id.
const SYNTHETIC_ID: &str = "openrouter/free";
/// A native id only `alpha` lists, so a provider-less route finds one owner.
const LEGACY_ID: &str = "legacy-gpt";
const SYNTHETIC_REPLY: &str = "served-by-the-synthetic-handler";

/// The text the upstream named `label` answers every chat request with.
fn reply_of(label: &str) -> String {
    format!("served-by-{label}")
}

/// One text-only chat-completions SSE response.
fn sse_text(text: &str) -> String {
    let usage = r#"{"prompt_tokens":1,"completion_tokens":1}"#;
    format!(
        "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"{text}\"}}}}]}}\n\ndata: {{\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}],\"usage\":{usage}}}\n\ndata: [DONE]\n\n"
    )
}

/// One request a loopback upstream received.
#[derive(Clone, Debug)]
struct Seen {
    method: String,
    path: String,
    host: String,
    authorization: String,
    body: Vec<u8>,
}

impl Seen {
    /// The `model` member of a JSON request body.
    fn model(&self) -> String {
        use sonic_rs::JsonValueTrait as _;
        let body: sonic_rs::Value = sonic_rs::from_slice(&self.body).expect("chat body is JSON");
        body.get("model")
            .and_then(|model| model.as_str())
            .expect("chat body names a model")
            .to_owned()
    }
}

/// A loopback provider: lists `models` on `GET /v1/models`, answers every
/// `POST /v1/chat/completions` with its own label, and records each request.
struct Upstream {
    address: SocketAddr,
    seen: Arc<Mutex<Vec<Seen>>>,
    /// Owns the accept loop; dropping the upstream drops the set, which
    /// aborts the loop and, through the loop's own set, every open connection.
    _server: tokio::task::JoinSet<()>,
}

impl Upstream {
    async fn start(label: &'static str, models: &[&str]) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("upstream listener");
        let address = listener.local_addr().expect("upstream address");
        let rows: Vec<String> = models
            .iter()
            .map(|id| format!("{{\"id\":\"{id}\"}}"))
            .collect();
        let listing = format!("{{\"data\":[{}]}}", rows.join(","));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut server = tokio::task::JoinSet::new();
        server.spawn(serve(listener, label, listing, Arc::clone(&seen)));
        Self {
            address,
            seen,
            _server: server,
        }
    }

    fn base_url(&self) -> String {
        format!("http://{}/v1", self.address)
    }

    fn chats(&self) -> Vec<Seen> {
        self.seen
            .lock()
            .expect("seen lock")
            .iter()
            .filter(|seen| seen.method == "POST" && seen.path.ends_with("/chat/completions"))
            .cloned()
            .collect()
    }
}

async fn serve(
    listener: TcpListener,
    label: &'static str,
    listing: String,
    seen: Arc<Mutex<Vec<Seen>>>,
) {
    // The set owns every connection task: aborting this accept loop drops it
    // and aborts whatever is still open.
    let mut connections = tokio::task::JoinSet::new();
    loop {
        while connections.try_join_next().is_some() {}
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let listing = listing.clone();
        let seen = Arc::clone(&seen);
        connections.spawn(async move {
            let _ = answer(stream, label, &listing, &seen).await;
        });
    }
}

/// Reads one HTTP request, records it, and answers it.
async fn answer(
    stream: TcpStream,
    label: &str,
    listing: &str,
    seen: &Mutex<Vec<Seen>>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    let mut length = 0_usize;
    let mut host = String::new();
    let mut authorization = String::new();
    loop {
        line.clear();
        reader.read_line(&mut line).await?;
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            let value = value.trim();
            match name.trim().to_ascii_lowercase().as_str() {
                "content-length" => length = value.parse().unwrap_or(0),
                "host" => value.clone_into(&mut host),
                "authorization" => value.clone_into(&mut authorization),
                _ => {}
            }
        }
    }
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body).await?;
    let route = path.split('?').next().unwrap_or_default();
    let (status, kind, payload) = if method == "GET" && route.ends_with("/models") {
        ("200 OK", "application/json", listing.to_owned())
    } else if method == "POST" && route.ends_with("/chat/completions") {
        ("200 OK", "text/event-stream", sse_text(&reply_of(label)))
    } else {
        ("404 Not Found", "text/plain", String::new())
    };
    seen.lock().expect("seen lock").push(Seen {
        method,
        path,
        host,
        authorization,
        body,
    });
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
        payload.len()
    );
    let mut stream = reader.into_inner();
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

/// Asserts `upstream` answered exactly `count` chat requests and that the
/// newest one was built for this provider: its own host, its own key, and
/// the provider-native `model`.
fn assert_chats(upstream: &Upstream, key: &str, model: &str, count: usize) {
    let chats = upstream.chats();
    assert_eq!(chats.len(), count, "chat requests at {}", upstream.address);
    if let Some(last) = chats.last() {
        assert_eq!(last.path, "/v1/chat/completions");
        assert_eq!(last.host, upstream.address.to_string());
        assert_eq!(last.authorization, format!("Bearer {key}"));
        assert_eq!(last.model(), model);
    }
}

/// A synthetic model whose handler counts its runs and answers with
/// [`SYNTHETIC_REPLY`].
struct Synthetic {
    calls: Arc<AtomicUsize>,
}

impl ModelHandler for Synthetic {
    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let events = vec![
            StreamEvent::TextDelta {
                text: SYNTHETIC_REPLY.into(),
            },
            StreamEvent::ToolCallsDone { calls: Vec::new() },
            StreamEvent::Usage {
                usage: Usage {
                    input_tokens: 1,
                    cached_input_tokens: 0,
                    output_tokens: 1,
                    reasoning_tokens: None,
                    cache_write_tokens: 0,
                    cost_usd: None,
                    premium_requests: None,
                },
            },
            StreamEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ];
        let stream = EventStream::new(
            futures::stream::iter(events.into_iter().map(Ok::<_, ProviderError>)),
            || {},
        );
        Box::pin(std::future::ready(Ok(stream)))
    }
}

fn synthetic_extension(id: &str, calls: Arc<AtomicUsize>) -> Extension {
    ExtensionBuilder::new("synth", "0.1.0", ServiceSet::default())
        .expect("extension builder")
        .model(ModelRecord {
            id: ModelId::parse(id).expect("model id"),
            caps: Caps {
                context_window: Some(100_000),
                thinking: Box::new([ThinkingLevel::Off]),
                tool_use: false,
                image_input: false,
                custom_grammar: false,
            },
            handler: Arc::new(Synthetic { calls }),
            export: None,
        })
        .build()
        .expect("extension")
}

/// The services and caller a session-start hook captured.
type Captured = Arc<Mutex<Option<(Arc<dyn Services>, Caller)>>>;

/// Captures the session-scoped services and caller at session start, so a
/// test can ask `model_ready` the way an extension does.
#[derive(Clone, Default)]
struct Capture {
    slot: Captured,
    ready: Arc<Notify>,
}

impl ObserveHook<SessionStart> for Capture {
    fn call(&self, _input: SessionStart, cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        let slot = Arc::clone(&self.slot);
        let ready = Arc::clone(&self.ready);
        let services = Arc::clone(&cx.services);
        let caller = cx.caller.clone();
        Box::pin(async move {
            *slot.lock().expect("capture lock") = Some((services, caller));
            ready.notify_one();
            Ok(())
        })
    }
}

impl Capture {
    fn extension(&self) -> Extension {
        ExtensionBuilder::new("capture", "0.1.0", ServiceSet::default())
            .expect("extension builder")
            .on_session_start_lossless(self.clone())
            .build()
            .expect("extension")
    }

    async fn taken(&self) -> (Arc<dyn Services>, Caller) {
        tokio::time::timeout(CLOSE, self.ready.notified())
            .await
            .expect("session start reached the hook");
        self.slot
            .lock()
            .expect("capture lock")
            .take()
            .expect("captured services")
    }
}

/// The temp-dir roots of one scenario: data, workspace, and the user
/// `dal.toml` a host with local default authority rewrites.
struct Rig {
    tmp: tempfile::TempDir,
}

impl Rig {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        for name in ["data", "workspace", "config"] {
            std::fs::create_dir_all(tmp.path().join(name)).expect("fixture directory");
        }
        Self { tmp }
    }

    fn data(&self) -> PathBuf {
        self.tmp.path().join("data")
    }

    fn user_path(&self) -> PathBuf {
        self.tmp.path().join("config").join("dal.toml")
    }

    fn workspace(&self) -> Workspace {
        Workspace::new(self.tmp.path().join("workspace")).expect("workspace")
    }

    fn write_user(&self, text: &str) {
        std::fs::write(self.user_path(), text).expect("user config");
    }

    fn user_text(&self) -> String {
        std::fs::read_to_string(self.user_path()).expect("user config reads")
    }

    fn journal_text(&self, session: SessionId) -> String {
        let store = dal_store::Store::new(self.data(), self.workspace(), dal_core::Product::Dal);
        std::fs::read_to_string(store.session_file(session)).expect("journal reads")
    }

    /// Starts two loopback providers listing the given native ids and writes
    /// a user configuration with `top` (top-level keys) followed by the two
    /// providers `alpha` and `beta`, each with its own key variable.
    async fn pair(&self, top: &str, alpha: &[&str], beta: &[&str]) -> (Upstream, Upstream) {
        let first = Upstream::start("alpha", alpha).await;
        let second = Upstream::start("beta", beta).await;
        self.write_user(&format!(
            "{top}[providers.alpha]\napi = \"openai_chat\"\nbase_url = \"{}\"\nkey_env = \"ALPHA_KEY\"\n\n[providers.beta]\napi = \"openai_chat\"\nbase_url = \"{}\"\nkey_env = \"BETA_KEY\"\n",
            first.base_url(),
            second.base_url(),
        ));
        (first, second)
    }

    /// Starts a host on the roots, loading the configuration from the user
    /// file as a fresh process would.
    async fn start(&self, extensions: Vec<Extension>) -> Host {
        let text = self.user_text();
        let config = Config::load(ConfigProduct::Dalgon, &self.data(), "", Some(text.as_str()))
            .expect("config loads");
        let product = Product {
            name: "dal",
            data_root: self.data(),
            defaults: "",
            extensions,
            bundled: Vec::new(),
        };
        let env = Env {
            vars: BTreeMap::from([
                (OsString::from("ALPHA_KEY"), OsString::from(KEY_ALPHA)),
                (OsString::from("BETA_KEY"), OsString::from(KEY_BETA)),
                (
                    OsString::from("OPENROUTER_API_KEY"),
                    OsString::from(KEY_OPENROUTER),
                ),
                (OsString::from("HOME"), self.tmp.path().join("home").into()),
                (
                    OsString::from("XDG_CACHE_HOME"),
                    self.tmp.path().join("cache").into(),
                ),
            ]),
            cwd: self.tmp.path().join("workspace"),
            sandbox_helper: None,
        };
        Host::start(product, config, env).await.expect("host")
    }

    async fn open_new(&self, host: &Host) -> Agent {
        host.open(
            SessionRef::New {
                workspace: self.workspace(),
                name: None,
            },
            ClientId::new("route-test"),
        )
        .await
        .expect("open new session")
    }

    async fn resume(&self, host: &Host, session: SessionId) -> Agent {
        host.open(
            SessionRef::Resume {
                key: session.to_string().into(),
                workspace: self.workspace(),
            },
            ClientId::new("route-test"),
        )
        .await
        .expect("resume session")
    }
}

fn chat_route(provider: Option<&str>, model: &str) -> ModelRoute {
    ModelRoute::Api {
        family: Family::Chat,
        model: model.into(),
        provider: provider.map(Into::into),
    }
}

fn settings_model(agent: &Agent) -> Option<ModelRoute> {
    let view = agent.view(PageReq::default()).expect("view");
    view.settings.model
}

fn transcript(agent: &Agent) -> String {
    format!("{:?}", agent.view(PageReq::default()).expect("view"))
}

/// Selects `route` for the session only.
async fn select(agent: &Agent, route: &ModelRoute) {
    agent
        .submit(Command::SetModel {
            model: route.clone(),
            save: Save::SessionOnly,
        })
        .await
        .expect("the model selection is accepted");
}

/// How one prompted turn ended.
struct Turn {
    stop: Stop,
    /// The `turn.failed` notice text, when the turn failed.
    failure: Option<String>,
}

/// Submits one prompt and waits for its own turn to end.
async fn prompt(agent: &Agent, events: &mut Subscription, text: &str) -> Turn {
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: text.into() }],
        })
        .await
        .expect("submit prompt");
    let Reply::Accepted { turn, .. } = reply else {
        unreachable!("prompt not accepted: {reply:?}");
    };
    let mut failure = None;
    loop {
        let delivery = tokio::time::timeout(TURN, events.next())
            .await
            .expect("the turn reports in time")
            .expect("the update stream stays open");
        let Delivery::Update(update) = delivery else {
            continue;
        };
        match &update.kind {
            UpdateKind::Notice(notice) if &*notice.kind == "turn.failed" => {
                failure = Some(notice.text.to_string());
            }
            UpdateKind::TurnEnded {
                turn: ended, stop, ..
            } if *ended == turn => {
                return Turn {
                    stop: *stop,
                    failure,
                };
            }
            _ => {}
        }
    }
}

/// Prompts and asserts the turn ended normally.
async fn answered(agent: &Agent, events: &mut Subscription, text: &str) {
    let turn = prompt(agent, events, text).await;
    assert_eq!(turn.stop, Stop::EndTurn, "{:?}", turn.failure);
}

#[tokio::test]
async fn each_provider_route_reaches_its_own_server_when_two_providers_list_one_native_id() {
    let rig = Rig::new();
    let (alpha, beta) = rig.pair("", &[SHARED_ID], &[SHARED_ID]).await;
    let host = rig.start(Vec::new()).await;
    let agent = rig.open_new(&host).await;
    let mut events = agent.subscribe(None).expect("subscribe");

    let to_beta = chat_route(Some("beta"), SHARED_ID);
    select(&agent, &to_beta).await;
    assert_eq!(settings_model(&agent), Some(to_beta.clone()));
    answered(&agent, &mut events, "to beta").await;
    assert_chats(&alpha, KEY_ALPHA, SHARED_ID, 0);
    assert_chats(&beta, KEY_BETA, SHARED_ID, 1);
    assert!(transcript(&agent).contains(&reply_of("beta")));

    let to_alpha = chat_route(Some("alpha"), SHARED_ID);
    select(&agent, &to_alpha).await;
    assert_eq!(settings_model(&agent), Some(to_alpha));
    answered(&agent, &mut events, "to alpha").await;
    assert_chats(&alpha, KEY_ALPHA, SHARED_ID, 1);
    assert_chats(&beta, KEY_BETA, SHARED_ID, 1);
    assert!(transcript(&agent).contains(&reply_of("alpha")));

    select(&agent, &to_beta).await;
    answered(&agent, &mut events, "to beta again").await;
    assert_chats(&alpha, KEY_ALPHA, SHARED_ID, 1);
    assert_chats(&beta, KEY_BETA, SHARED_ID, 2);
    let _ = host.shutdown(CLOSE).await;
}

#[tokio::test]
async fn listed_models_name_their_provider_so_duplicate_native_ids_stay_distinct() {
    let rig = Rig::new();
    let (_alpha, _beta) = rig.pair("", &[SHARED_ID], &[SHARED_ID]).await;
    let host = rig.start(Vec::new()).await;
    let rows = host.models(Some(Family::Chat)).await.expect("models list");
    let routes: Vec<ModelRoute> = rows.into_iter().map(|row| row.route).collect();
    for provider in ["alpha", "beta"] {
        assert!(
            routes.contains(&chat_route(Some(provider), SHARED_ID)),
            "{provider} lists {SHARED_ID} under its own provider: {routes:?}"
        );
    }
    assert!(
        routes.iter().all(|route| route.provider().is_some()),
        "every listed API row names the provider that produced it: {routes:?}"
    );
    let _ = host.shutdown(CLOSE).await;
}

fn relay_request(route: &ModelRoute) -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: route.clone(),
        system: "".into(),
        tools: Arc::from(Vec::new()),
        context: Arc::from(vec![ContextItem::User {
            parts: vec![Part::Text { text: "hi".into() }],
        }]),
        params: RequestParams::default(),
        cache_key: None,
    }
}

#[tokio::test]
async fn relay_opens_the_provider_a_route_names_when_two_providers_list_one_native_id() {
    let rig = Rig::new();
    let (alpha, beta) = rig.pair("", &[SHARED_ID], &[SHARED_ID]).await;
    let host = rig.start(Vec::new()).await;
    let route = chat_route(Some("beta"), SHARED_ID);
    let mut stream = host
        .relay(
            ClientId::new("router"),
            route.clone(),
            relay_request(&route),
        )
        .await
        .expect("relay opens the named provider");
    let mut text = String::new();
    while let Some(item) = tokio::time::timeout(TURN, stream.next())
        .await
        .expect("stream item in time")
    {
        if let StreamEvent::TextDelta { text: delta } = item.expect("stream event") {
            text.push_str(&delta);
        }
    }
    assert_eq!(text, reply_of("beta"));
    assert_chats(&alpha, KEY_ALPHA, SHARED_ID, 0);
    assert_chats(&beta, KEY_BETA, SHARED_ID, 1);
    let _ = host.shutdown(CLOSE).await;
}

#[tokio::test]
async fn relay_refuses_a_request_that_names_another_provider_than_its_route() {
    let rig = Rig::new();
    let (alpha, beta) = rig.pair("", &[SHARED_ID], &[SHARED_ID]).await;
    let host = rig.start(Vec::new()).await;
    let named = chat_route(Some("alpha"), SHARED_ID);
    let requested = chat_route(Some("beta"), SHARED_ID);
    let result = host
        .relay(ClientId::new("router"), named, relay_request(&requested))
        .await;
    let Err(HostError::Config { message }) = result else {
        unreachable!("relay accepted a request for another provider than its route");
    };
    assert!(message.contains("does not match"), "{message}");
    assert_chats(&alpha, KEY_ALPHA, SHARED_ID, 0);
    assert_chats(&beta, KEY_BETA, SHARED_ID, 0);
    let _ = host.shutdown(CLOSE).await;
}

/// The saved default a fixture configuration carries, written as the merged
/// `model` string the branch persists (no `[model_route]` table remains).
#[derive(Clone, Copy)]
enum Saved {
    /// No default at all.
    Nothing,
    /// A default API route for `SYNTHETIC_ID` that names no provider.
    Providerless,
    /// A default API route for `SYNTHETIC_ID` that names this provider.
    Provider(&'static str),
}

/// The `openrouter` provider (Chat wire, loopback) listing `SYNTHETIC_ID`,
/// with the saved default `saved`: a provider-qualified reference keeps its
/// provider, while the bare id resolves the synthetic first.
fn openrouter_config(upstream: &Upstream, saved: Saved) -> String {
    let mut text = String::new();
    match saved {
        Saved::Nothing => {}
        Saved::Provider(provider) => {
            let _ = writeln!(text, "model = \"{provider}/{SYNTHETIC_ID}\"\n");
        }
        Saved::Providerless => {
            let _ = writeln!(text, "model = \"{SYNTHETIC_ID}\"\n");
        }
    }
    let _ = writeln!(
        text,
        "[providers.openrouter]\napi = \"openai_chat\"\nbase_url = \"{}\"",
        upstream.base_url()
    );
    text
}

/// Starts a host with the synthetic `openrouter/free` registered and the
/// provider `openrouter` listing the same id.
async fn collision(saved: Saved) -> (Rig, Upstream, Arc<AtomicUsize>, Host) {
    let rig = Rig::new();
    let openrouter = Upstream::start("openrouter", &[SYNTHETIC_ID]).await;
    rig.write_user(&openrouter_config(&openrouter, saved));
    let calls = Arc::new(AtomicUsize::new(0));
    let host = rig
        .start(vec![synthetic_extension(SYNTHETIC_ID, Arc::clone(&calls))])
        .await;
    (rig, openrouter, calls, host)
}

#[tokio::test]
async fn a_provider_route_runs_the_provider_not_a_synthetic_spelling_its_native_id() {
    let (rig, openrouter, calls, host) = collision(Saved::Nothing).await;
    let agent = rig.open_new(&host).await;
    let mut events = agent.subscribe(None).expect("subscribe");
    let route = chat_route(Some("openrouter"), SYNTHETIC_ID);
    select(&agent, &route).await;
    assert_eq!(settings_model(&agent), Some(route));
    let turn = prompt(&agent, &mut events, "provider").await;
    assert_eq!(turn.stop, Stop::EndTurn, "{:?}", turn.failure);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "the synthetic handler never ran for the provider's native id"
    );
    assert_chats(&openrouter, KEY_OPENROUTER, SYNTHETIC_ID, 1);
    let text = transcript(&agent);
    assert!(text.contains(&reply_of("openrouter")), "{text}");
    assert!(!text.contains(SYNTHETIC_REPLY), "{text}");
    let _ = host.shutdown(CLOSE).await;
}

#[tokio::test]
async fn a_providerless_route_naming_a_synthetic_id_still_runs_the_synthetic() {
    let (rig, openrouter, calls, host) = collision(Saved::Nothing).await;
    let agent = rig.open_new(&host).await;
    let mut events = agent.subscribe(None).expect("subscribe");
    let route = chat_route(None, SYNTHETIC_ID);
    select(&agent, &route).await;
    assert_eq!(settings_model(&agent), Some(route));
    let turn = prompt(&agent, &mut events, "legacy").await;
    assert_eq!(turn.stop, Stop::EndTurn, "{:?}", turn.failure);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the legacy provider-less route keeps resolving the synthetic first"
    );
    assert_chats(&openrouter, KEY_OPENROUTER, SYNTHETIC_ID, 0);
    assert!(transcript(&agent).contains(SYNTHETIC_REPLY));
    let _ = host.shutdown(CLOSE).await;
}

#[tokio::test]
async fn a_synthetic_route_runs_the_synthetic_not_the_provider_listing_its_id() {
    let (rig, openrouter, calls, host) = collision(Saved::Nothing).await;
    let agent = rig.open_new(&host).await;
    let mut events = agent.subscribe(None).expect("subscribe");
    let route = ModelRoute::synthetic(SYNTHETIC_ID).expect("synthetic route");
    select(&agent, &route).await;
    assert_eq!(settings_model(&agent), Some(route));
    let turn = prompt(&agent, &mut events, "synthetic").await;
    assert_eq!(turn.stop, Stop::EndTurn, "{:?}", turn.failure);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_chats(&openrouter, KEY_OPENROUTER, SYNTHETIC_ID, 0);
    assert!(transcript(&agent).contains(SYNTHETIC_REPLY));
    let _ = host.shutdown(CLOSE).await;
}

#[tokio::test]
async fn a_saved_provider_default_wins_over_a_synthetic_spelling_its_native_id() {
    let (rig, openrouter, calls, host) = collision(Saved::Provider("openrouter")).await;
    let saved = chat_route(Some("openrouter"), SYNTHETIC_ID);
    assert_eq!(host.default_route().await, Some(saved));
    let agent = rig.open_new(&host).await;
    // `SettingsView.model` is the route the session itself selected. This
    // session inherits the saved default, which the request below proves.
    assert_eq!(settings_model(&agent), None);
    let mut events = agent.subscribe(None).expect("subscribe");
    let turn = prompt(&agent, &mut events, "default").await;
    assert_eq!(turn.stop, Stop::EndTurn, "{:?}", turn.failure);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_chats(&openrouter, KEY_OPENROUTER, SYNTHETIC_ID, 1);
    let _ = host.shutdown(CLOSE).await;
}

#[tokio::test]
async fn a_providerless_saved_default_still_resolves_the_synthetic_first() {
    let (rig, openrouter, calls, host) = collision(Saved::Providerless).await;
    assert_eq!(
        host.default_route().await,
        Some(ModelRoute::synthetic(SYNTHETIC_ID).expect("synthetic route"))
    );
    let agent = rig.open_new(&host).await;
    // The session inherits the default and selected nothing, so its view
    // names no model; the synthetic handler answering below proves which
    // route the default resolves to.
    assert_eq!(settings_model(&agent), None);
    let mut events = agent.subscribe(None).expect("subscribe");
    let turn = prompt(&agent, &mut events, "default").await;
    assert_eq!(turn.stop, Stop::EndTurn, "{:?}", turn.failure);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_chats(&openrouter, KEY_OPENROUTER, SYNTHETIC_ID, 0);
    let _ = host.shutdown(CLOSE).await;
}

/// Host one: the product default is seeded as the provider-qualified `model`
/// string the merged config surface persists, then a session selects `beta`'s
/// route for itself and answers one prompt through it; the host then stops.
/// Returns that session.
async fn save_beta_default(rig: &Rig, beta: &Upstream) -> SessionId {
    rig.write_user(&format!(
        "model = \"beta/{SHARED_ID}\"\n{}",
        rig.user_text()
    ));
    let host = rig.start(Vec::new()).await;
    let agent = rig.open_new(&host).await;
    let mut events = agent.subscribe(None).expect("subscribe");
    agent
        .submit(Command::SetModel {
            model: chat_route(Some("beta"), SHARED_ID),
            save: Save::SessionOnly,
        })
        .await
        .expect("the session selects the provider route");
    let turn = prompt(&agent, &mut events, "saving").await;
    assert_eq!(turn.stop, Stop::EndTurn, "{:?}", turn.failure);
    assert_chats(beta, KEY_BETA, SHARED_ID, 1);
    let session = agent.session();
    let _ = host.shutdown(CLOSE).await;
    session
}

#[tokio::test]
async fn saving_a_provider_route_as_default_writes_the_provider_qualified_model_string() {
    let rig = Rig::new();
    let (alpha, beta) = rig
        .pair("# kept user comment\n", &[SHARED_ID], &[SHARED_ID])
        .await;
    let session = save_beta_default(&rig, &beta).await;
    let saved = rig.user_text();
    assert!(saved.contains("# kept user comment"), "{saved}");
    assert!(
        saved.contains("model = \"beta/gpt-4o\""),
        "the merged default names the provider-qualified reference: {saved}"
    );
    assert!(
        !saved.contains("model_route"),
        "the merged default is a model string, not a table: {saved}"
    );
    assert!(
        !saved.contains("openai-chat/gpt-4o"),
        "the provider-qualified default never falls back to a family spelling: {saved}"
    );
    let reloaded = Config::load(ConfigProduct::Dalgon, &rig.data(), "", Some(saved.as_str()))
        .expect("the saved file loads");
    assert_eq!(reloaded.model(), Some("beta/gpt-4o"));
    let journal = rig.journal_text(session);
    let record = journal
        .lines()
        .find(|line| line.contains("\"type\":\"model\""))
        .expect("the journal holds the model record");
    assert!(record.contains("\"provider\":\"beta\""), "{record}");
    assert_chats(&alpha, KEY_ALPHA, SHARED_ID, 0);
}

#[tokio::test]
async fn a_saved_provider_default_reaches_its_provider_after_a_restart_and_a_reopened_session() {
    let rig = Rig::new();
    let (alpha, beta) = rig.pair("", &[SHARED_ID], &[SHARED_ID]).await;
    let saved_session = save_beta_default(&rig, &beta).await;

    let host = rig.start(Vec::new()).await;
    let saved = chat_route(Some("beta"), SHARED_ID);
    assert_eq!(host.default_route().await, Some(saved.clone()));
    let fresh = rig.open_new(&host).await;
    assert_eq!(
        settings_model(&fresh),
        None,
        "a new session inherits the saved default and selects no model"
    );
    let mut events = fresh.subscribe(None).expect("subscribe");
    let turn = prompt(&fresh, &mut events, "no selection").await;
    assert_eq!(turn.stop, Stop::EndTurn, "{:?}", turn.failure);
    assert_chats(&alpha, KEY_ALPHA, SHARED_ID, 0);
    assert_chats(&beta, KEY_BETA, SHARED_ID, 2);

    let reopened = rig.resume(&host, saved_session).await;
    assert_eq!(
        settings_model(&reopened),
        Some(saved),
        "the reopened session keeps the full route including its provider"
    );
    let mut events = reopened.subscribe(None).expect("subscribe");
    let turn = prompt(&reopened, &mut events, "after reopen").await;
    assert_eq!(turn.stop, Stop::EndTurn, "{:?}", turn.failure);
    assert_chats(&alpha, KEY_ALPHA, SHARED_ID, 0);
    assert_chats(&beta, KEY_BETA, SHARED_ID, 3);
    let _ = host.shutdown(CLOSE).await;
}

#[tokio::test]
async fn a_session_journal_replays_the_selected_provider_route_after_a_restart() {
    let rig = Rig::new();
    let (alpha, beta) = rig.pair("", &[SHARED_ID], &[SHARED_ID]).await;
    let host = rig.start(Vec::new()).await;
    let agent = rig.open_new(&host).await;
    let mut events = agent.subscribe(None).expect("subscribe");
    let route = chat_route(Some("alpha"), SHARED_ID);
    select(&agent, &route).await;
    let turn = prompt(&agent, &mut events, "before").await;
    assert_eq!(turn.stop, Stop::EndTurn, "{:?}", turn.failure);
    let session = agent.session();
    let _ = host.shutdown(CLOSE).await;
    assert_chats(&alpha, KEY_ALPHA, SHARED_ID, 1);

    // No default names a provider: only the journal can supply the route.
    let restarted = rig.start(Vec::new()).await;
    assert_eq!(restarted.default_route().await, None);
    let reopened = rig.resume(&restarted, session).await;
    assert_eq!(settings_model(&reopened), Some(route));
    let mut events = reopened.subscribe(None).expect("subscribe");
    let turn = prompt(&reopened, &mut events, "after").await;
    assert_eq!(turn.stop, Stop::EndTurn, "{:?}", turn.failure);
    assert_chats(&alpha, KEY_ALPHA, SHARED_ID, 2);
    assert_chats(&beta, KEY_BETA, SHARED_ID, 0);
    let _ = restarted.shutdown(CLOSE).await;
}

/// What a session-setup hook captured: the route the host handed setup, and
/// the services and caller an extension would send a request with.
type SetupCaptured = Arc<Mutex<Option<(Option<ModelRoute>, Arc<dyn Services>, Caller)>>>;

fn setup_capture_extension(slot: SetupCaptured) -> Extension {
    ExtensionBuilder::new("setup-capture", "0.1.0", ServiceSet::default())
        .expect("extension builder")
        .session_setup(
            move |cx: SessionCx| -> BoxFuture<'static, Result<SessionInstalls, HookError>> {
                let slot = Arc::clone(&slot);
                Box::pin(async move {
                    *slot.lock().expect("setup capture lock") =
                        Some((cx.model, cx.services, cx.caller));
                    Ok(SessionInstalls::default())
                })
            },
        )
        .build()
        .expect("extension")
}

#[tokio::test]
async fn a_reopened_session_without_a_model_record_sets_extensions_up_with_the_provider_route() {
    let rig = Rig::new();
    let top = format!("model = \"beta/{SHARED_ID}\"\n\n");
    let (alpha, beta) = rig.pair(&top, &[SHARED_ID], &[SHARED_ID]).await;
    let saved = chat_route(Some("beta"), SHARED_ID);

    // Host one: the configured default serves a turn. Nothing selects a
    // model, so the journal holds an assistant record and no model record.
    // `SettingsView.model` is the route a client explicitly selected, so a
    // session that inherits the configured default shows none. The first
    // request reaching `beta`, asserted below, proves the default applies.
    let host = rig.start(Vec::new()).await;
    let agent = rig.open_new(&host).await;
    let mut events = agent.subscribe(None).expect("subscribe");
    answered(&agent, &mut events, "first").await;
    let session = agent.session();
    let _ = host.shutdown(CLOSE).await;
    assert!(
        !rig.journal_text(session).contains("\"type\":\"model\""),
        "no model record was journaled"
    );
    assert_chats(&alpha, KEY_ALPHA, SHARED_ID, 0);
    assert_chats(&beta, KEY_BETA, SHARED_ID, 1);

    // Host two reopens it. Setup consumers (reviewer, judge, history) send
    // requests with the route setup hands them.
    let captured: SetupCaptured = Arc::default();
    let restarted = rig
        .start(vec![setup_capture_extension(Arc::clone(&captured))])
        .await;
    let _reopened = rig.resume(&restarted, session).await;
    let (setup_route, services, caller) = captured
        .lock()
        .expect("setup capture lock")
        .take()
        .expect("setup ran on reopen");
    assert_eq!(
        setup_route,
        Some(saved.clone()),
        "setup keeps the provider the response did not record"
    );
    // Both providers list the model: a provider-less route is ambiguous, so
    // this request reaches `beta` only if setup handed over the full route.
    let route = setup_route.expect("a setup route");
    let inference = services
        .infer(&caller, relay_request(&route))
        .await
        .expect("the setup route reaches its provider");
    assert!(
        format!("{inference:?}").contains(&reply_of("beta")),
        "{inference:?}"
    );
    assert_chats(&alpha, KEY_ALPHA, SHARED_ID, 0);
    assert_chats(&beta, KEY_BETA, SHARED_ID, 2);
    let _ = restarted.shutdown(CLOSE).await;
}

#[tokio::test]
async fn a_legacy_only_config_still_starts_and_resolves_by_its_reference() {
    let rig = Rig::new();
    let (alpha, beta) = rig
        .pair("model = \"alpha/gpt-4o\"\n\n", &[SHARED_ID], &[SHARED_ID])
        .await;
    let captured: SetupCaptured = Arc::default();
    let host = rig
        .start(vec![setup_capture_extension(Arc::clone(&captured))])
        .await;
    assert!(
        !rig.user_text().contains("model_route"),
        "the fixture is a legacy-only file"
    );
    assert_eq!(
        host.default_route().await,
        Some(chat_route(Some("alpha"), SHARED_ID)),
        "the legacy reference resolves through the catalog to its provider"
    );
    let agent = rig.open_new(&host).await;
    let (seeded, ..) = captured
        .lock()
        .expect("setup capture lock")
        .take()
        .expect("setup ran at open");
    assert_eq!(
        seeded,
        Some(ModelRoute::Synthetic {
            id: "alpha/gpt-4o".into()
        }),
        "the legacy string seeds the session as a namespaced id, never with a provider"
    );
    assert_eq!(
        settings_model(&agent),
        None,
        "the session inherits that seed and selects no model"
    );
    let mut events = agent.subscribe(None).expect("subscribe");
    let turn = prompt(&agent, &mut events, "legacy").await;
    assert_eq!(turn.stop, Stop::EndTurn, "{:?}", turn.failure);
    assert_chats(&alpha, KEY_ALPHA, SHARED_ID, 1);
    assert_chats(&beta, KEY_BETA, SHARED_ID, 0);
    let _ = host.shutdown(CLOSE).await;
}

#[tokio::test]
async fn an_unconfigured_provider_route_fails_typed_and_reaches_no_server() {
    let rig = Rig::new();
    let (alpha, beta) = rig.pair("", &[SHARED_ID], &[SHARED_ID]).await;
    let capture = Capture::default();
    let host = rig.start(vec![capture.extension()]).await;
    let agent = rig.open_new(&host).await;
    let (services, caller) = capture.taken().await;

    let missing = chat_route(Some("gamma"), SHARED_ID);
    assert_eq!(
        services.model_ready(&caller, missing.clone()).await,
        Ok(None),
        "a route naming an unconfigured provider is not ready"
    );
    let known = chat_route(Some("beta"), SHARED_ID);
    assert_eq!(
        services.model_ready(&caller, known.clone()).await,
        Ok(Some(known)),
        "the same native id under a configured provider is ready"
    );

    let mut events = agent.subscribe(None).expect("subscribe");
    select(&agent, &missing).await;
    assert_eq!(settings_model(&agent), Some(missing));
    let turn = prompt(&agent, &mut events, "nowhere").await;
    assert_eq!(turn.stop, Stop::Failed);
    let message = turn.failure.expect("the failed turn names its cause");
    assert!(
        message.contains("provider gamma is not configured for model gpt-4o"),
        "{message}"
    );
    assert_chats(&alpha, KEY_ALPHA, SHARED_ID, 0);
    assert_chats(&beta, KEY_BETA, SHARED_ID, 0);
    let _ = host.shutdown(CLOSE).await;
}

#[tokio::test]
async fn a_provider_serving_another_family_fails_typed_and_reaches_no_server() {
    let rig = Rig::new();
    let (alpha, beta) = rig.pair("", &[SHARED_ID], &[SHARED_ID]).await;
    let host = rig.start(Vec::new()).await;
    let agent = rig.open_new(&host).await;
    let mut events = agent.subscribe(None).expect("subscribe");
    let wrong_family = ModelRoute::Api {
        family: Family::Responses,
        model: SHARED_ID.into(),
        provider: Some("alpha".into()),
    };
    select(&agent, &wrong_family).await;
    let turn = prompt(&agent, &mut events, "wrong wire").await;
    assert_eq!(turn.stop, Stop::Failed);
    let message = turn.failure.expect("the failed turn names its cause");
    assert!(
        message.contains("provider alpha does not serve the API family of model gpt-4o"),
        "{message}"
    );
    assert_chats(&alpha, KEY_ALPHA, SHARED_ID, 0);
    assert_chats(&beta, KEY_BETA, SHARED_ID, 0);
    let _ = host.shutdown(CLOSE).await;
}

fn entry_id(id: u64) -> EntryId {
    EntryId::new(std::num::NonZeroU64::new(id).expect("nonzero entry id"))
}

/// Writes a journal exactly as an older binary did: a user entry and a model
/// record whose route names no provider.
async fn seed_legacy_journal(rig: &Rig, session: SessionId, route: &ModelRoute) {
    let store = dal_store::Store::new(rig.data(), rig.workspace(), dal_core::Product::Dal);
    let mut journal = store.create_session(session);
    journal
        .append(vec![
            Record::User(Entry {
                id: entry_id(1),
                parent: None,
                at: jiff::Timestamp::UNIX_EPOCH,
                kind: EntryKind::User {
                    parts: vec![JournalPart::Text {
                        text: "earlier question".into(),
                    }],
                },
            }),
            Record::Model(Entry {
                id: entry_id(2),
                parent: Some(entry_id(1)),
                at: jiff::Timestamp::UNIX_EPOCH,
                kind: EntryKind::Model {
                    route: route.clone(),
                },
            }),
        ])
        .await
        .expect("seed legacy history");
    journal.close().await.expect("close seeded journal");
}

fn model_record(journal: &str) -> String {
    journal
        .lines()
        .find(|line| line.contains("\"type\":\"model\""))
        .expect("the journal holds a model record")
        .to_owned()
}

#[tokio::test]
async fn a_legacy_journal_model_record_replays_without_a_provider_and_runs_by_the_legacy_rules() {
    let rig = Rig::new();
    let (alpha, beta) = rig.pair("", &[LEGACY_ID], &["beta-only"]).await;
    let legacy = chat_route(None, LEGACY_ID);
    let session = SessionId::new_v7();
    seed_legacy_journal(&rig, session, &legacy).await;
    let before = model_record(&rig.journal_text(session));
    assert!(
        before.contains("\"model\":\"legacy-gpt\"") && !before.contains("provider"),
        "the seeded record has the provider-less legacy shape: {before}"
    );

    let host = rig.start(Vec::new()).await;
    let agent = rig.resume(&host, session).await;
    assert_eq!(
        settings_model(&agent),
        Some(legacy),
        "the legacy record replays as a route without a provider"
    );
    let mut events = agent.subscribe(None).expect("subscribe");
    let turn = prompt(&agent, &mut events, "continue").await;
    assert_eq!(turn.stop, Stop::EndTurn, "{:?}", turn.failure);
    assert_chats(&alpha, KEY_ALPHA, LEGACY_ID, 1);
    assert_chats(&beta, KEY_BETA, LEGACY_ID, 0);
    let _ = host.shutdown(CLOSE).await;
    assert_eq!(
        model_record(&rig.journal_text(session)),
        before,
        "replaying and continuing never rewrites the legacy record"
    );
}

/// A configuration whose default is `beta`'s route for `SHARED_ID`, with
/// both providers listing that id.
async fn beta_default(rig: &Rig) -> (Upstream, Upstream) {
    let top = format!("model = \"beta/{SHARED_ID}\"\n\n");
    rig.pair(&top, &[SHARED_ID], &[SHARED_ID]).await
}

#[tokio::test]
async fn a_reopened_session_restores_its_approval_and_leaves_an_inherited_default_unselected() {
    let rig = Rig::new();
    let (alpha, beta) = beta_default(&rig).await;

    // Host one: the session inherits the configured default, changes only its
    // approval mode, and answers one prompt. The journal then holds an
    // approval record and no model record.
    let host = rig.start(Vec::new()).await;
    let agent = rig.open_new(&host).await;
    let mode = ApprovalMode::All;
    let opened = agent
        .view(PageReq::default())
        .expect("view")
        .settings
        .approval;
    assert_ne!(opened, mode, "the fixture changes the approval mode");
    agent
        .submit(Command::SetApproval {
            mode,
            save: Save::SessionOnly,
        })
        .await
        .expect("the approval mode is accepted");
    let mut events = agent.subscribe(None).expect("subscribe");
    answered(&agent, &mut events, "first").await;
    let session = agent.session();
    let _ = host.shutdown(CLOSE).await;
    let journal = rig.journal_text(session);
    assert!(journal.contains("\"type\":\"approval\""), "{journal}");
    assert!(!journal.contains("\"type\":\"model\""), "{journal}");

    // Host two reopens it: the journaled mode shows again, the default stays
    // an inheritance rather than a selection, and the next request still
    // reaches the default's provider.
    let restarted = rig.start(Vec::new()).await;
    let reopened = rig.resume(&restarted, session).await;
    let view = reopened.view(PageReq::default()).expect("view");
    assert_eq!(view.settings.approval, mode, "the journaled mode returns");
    assert_eq!(
        view.settings.model, None,
        "a default the session inherited is not a selection"
    );
    let mut events = reopened.subscribe(None).expect("subscribe");
    answered(&reopened, &mut events, "second").await;
    assert_chats(&alpha, KEY_ALPHA, SHARED_ID, 0);
    assert_chats(&beta, KEY_BETA, SHARED_ID, 2);
    let _ = restarted.shutdown(CLOSE).await;
}

#[tokio::test]
async fn a_live_settings_change_neither_selects_the_default_nor_drops_a_selection() {
    let rig = Rig::new();
    let _upstreams = beta_default(&rig).await;
    let host = rig.start(Vec::new()).await;
    let agent = rig.open_new(&host).await;
    let selected = chat_route(Some("beta"), SHARED_ID);
    agent
        .submit(Command::SetApproval {
            mode: ApprovalMode::All,
            save: Save::SessionOnly,
        })
        .await
        .expect("the approval mode is accepted");
    let view = agent.view(PageReq::default()).expect("view");
    assert_eq!(view.settings.approval, ApprovalMode::All);
    assert_eq!(
        view.settings.model, None,
        "changing another setting selects no model"
    );

    select(&agent, &selected).await;
    agent
        .submit(Command::SetApproval {
            mode: ApprovalMode::Edits,
            save: Save::SessionOnly,
        })
        .await
        .expect("the approval mode is accepted");
    let view = agent.view(PageReq::default()).expect("view");
    assert_eq!(view.settings.approval, ApprovalMode::Edits);
    assert_eq!(
        view.settings.model,
        Some(selected),
        "a selection equal to the default survives a later change"
    );
    let _ = host.shutdown(CLOSE).await;
}

#[tokio::test]
async fn a_selection_equal_to_the_default_still_shows_after_a_reopen() {
    let rig = Rig::new();
    let (alpha, beta) = beta_default(&rig).await;
    let selected = chat_route(Some("beta"), SHARED_ID);

    // Host one: the session selects the very route its default names, so only
    // the model record tells the selection from the inheritance.
    let host = rig.start(Vec::new()).await;
    let agent = rig.open_new(&host).await;
    assert_eq!(settings_model(&agent), None, "the default is inherited");
    select(&agent, &selected).await;
    let mut events = agent.subscribe(None).expect("subscribe");
    answered(&agent, &mut events, "first").await;
    let session = agent.session();
    let _ = host.shutdown(CLOSE).await;
    assert!(
        rig.journal_text(session).contains("\"type\":\"model\""),
        "the selection was journaled"
    );

    let restarted = rig.start(Vec::new()).await;
    let reopened = rig.resume(&restarted, session).await;
    assert_eq!(
        settings_model(&reopened),
        Some(selected),
        "a journaled selection shows even when it equals the default"
    );
    assert_chats(&alpha, KEY_ALPHA, SHARED_ID, 0);
    assert_chats(&beta, KEY_BETA, SHARED_ID, 1);
    let _ = restarted.shutdown(CLOSE).await;
}

#[tokio::test]
async fn a_reopened_route_naming_an_unconfigured_provider_stays_visible_and_fails_typed() {
    let rig = Rig::new();
    let (alpha, beta) = rig.pair("", &[SHARED_ID], &[SHARED_ID]).await;
    let missing = chat_route(Some("gamma"), SHARED_ID);
    let session = SessionId::new_v7();
    {
        let store = dal_store::Store::new(rig.data(), rig.workspace(), dal_core::Product::Dal);
        let mut journal = store.create_session(session);
        journal
            .append(vec![
                Record::User(Entry {
                    id: entry_id(1),
                    parent: None,
                    at: jiff::Timestamp::UNIX_EPOCH,
                    kind: EntryKind::User {
                        parts: vec![JournalPart::Text {
                            text: "earlier question".into(),
                        }],
                    },
                }),
                Record::Model(Entry {
                    id: entry_id(2),
                    parent: Some(entry_id(1)),
                    at: jiff::Timestamp::UNIX_EPOCH,
                    kind: EntryKind::Model {
                        route: missing.clone(),
                    },
                }),
            ])
            .await
            .expect("seed the unconfigured-provider journal");
        journal.close().await.expect("close seeded journal");
    }
    let host = rig.start(Vec::new()).await;
    let reopened = rig.resume(&host, session).await;
    assert_eq!(
        settings_model(&reopened),
        Some(missing.clone()),
        "the journaled provider stays visible even though nothing configures it"
    );
    let mut events = reopened.subscribe(None).expect("subscribe");
    let turn = prompt(&reopened, &mut events, "continue").await;
    assert_eq!(turn.stop, Stop::Failed, "{:?}", turn.failure);
    let message = turn.failure.expect("the failed turn names its cause");
    assert!(
        message.contains("provider gamma is not configured for model gpt-4o"),
        "{message}"
    );
    assert_chats(&alpha, KEY_ALPHA, SHARED_ID, 0);
    assert_chats(&beta, KEY_BETA, SHARED_ID, 0);
    assert_eq!(
        settings_model(&reopened),
        Some(missing),
        "the failed turn does not clear the restored selection"
    );
    let _ = host.shutdown(CLOSE).await;
}
