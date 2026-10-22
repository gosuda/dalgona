//! A turn opening and the turn's first request share one resolved model.
//!
//! The input hooks of an opening see the model the request will run on.
//! The driver reuses that resolution for the first request, and for a
//! compaction that runs before it, instead of fetching the catalog again,
//! for as long as the requested route is unchanged. An explicit model
//! selection that folds first keeps its precedence: it drops the carried
//! identity and the request resolves the new route as it always did.
//!
//! The live tests drive a real host against a cleartext HTTP server on
//! `127.0.0.1` that counts `GET /v1/models`. The compaction tests drive real
//! registered models and a real compactor.
//!
//! One catalog lookup per prompt stays outside this contract: the transport
//! resolves the route once more when it opens the provider stream
//! (`TRANSPORT_LOOKUPS`). These tests pin the lookups the driver's request
//! preparation used to add, not the transport's.

#![expect(clippy::expect_used, reason = "test assertions abort on failure")]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use dal_agent::ext::{
    BoxFuture, CompactError, CompactInput, Compaction, Compactor, Extension, ExtensionBuilder,
    Hook, HookCx, HookError, ModelCx, ModelError, ModelHandler, ModelRecord, Services,
};
use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef, Subscription};
use dal_core::ext::{BeforeRequest, InputEvent, InputVerdict};
use dal_core::{
    Caps, ClientId, Command, Config, ConfigProduct, Expect, ModelId, ModelInfo, ModelRequest,
    ModelRoute, Origin, Part, Reply, RequestParams, Save, ServiceSet, Stop, ThinkingLevel, TurnId,
    UpdateKind, Usage, Workspace,
};
use dal_provider::{EventStream, ProviderError, StopReason, StreamEvent};
use futures::stream;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio::task::JoinSet;

/// Bounds each real-clock wait.
const WAIT: Duration = Duration::from_secs(60);

/// Catalog lookups the opening makes for its input hooks.
const OPENING_LOOKUPS: usize = 1;

/// Catalog lookups the transport makes when it opens the provider stream.
/// `Driver::resolve_request` used to add a third one per prompt.
const TRANSPORT_LOOKUPS: usize = 1;

/// A prompt long enough to cross the compaction threshold and to keep the
/// compaction cut inside the latest user entry.
const BIG: usize = 200_000;

const MODELS: &str = r#"{"data":[{"id":"gpt-4o"},{"id":"gpt-4o-mini"}]}"#;

const CHAT_REPLY: &str = concat!(
    r#"data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"gpt-4o","choices":[{"index":0,"delta":{"role":"assistant","content":"ok"},"logprobs":null,"finish_reason":null}],"usage":null}"#,
    "\n\n",
    r#"data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"gpt-4o","choices":[{"index":0,"delta":{},"logprobs":null,"finish_reason":"stop"}],"usage":null}"#,
    "\n\n",
    r#"data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"gpt-4o","choices":[],"usage":{"prompt_tokens":19,"completion_tokens":10,"total_tokens":29,"prompt_tokens_details":{"cached_tokens":4,"audio_tokens":0},"completion_tokens_details":{"reasoning_tokens":3,"audio_tokens":0}}}"#,
    "\n\n",
    "data: [DONE]\n\n",
);

/// One request the loopback server answered.
#[derive(Clone, Debug)]
struct Hit {
    method: String,
    path: String,
    body: String,
}

/// A cleartext provider on `127.0.0.1`: a model list and a chat stream.
struct Loopback {
    address: SocketAddr,
    hits: Arc<Mutex<Vec<Hit>>>,
    _server: JoinSet<()>,
}

impl Loopback {
    async fn start() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("loopback listener");
        let address = listener.local_addr().expect("loopback address");
        let hits = Arc::new(Mutex::new(Vec::new()));
        let mut server = JoinSet::new();
        server.spawn(accept(listener, Arc::clone(&hits)));
        Self {
            address,
            hits,
            _server: server,
        }
    }

    /// The provider block that points the `openai` preset at this server.
    fn config(&self) -> String {
        format!(
            "model = \"openai-chat/gpt-4o\"\n\n[providers.openai]\napi = \"openai_chat\"\nbase_url = \"http://{}/v1\"\n",
            self.address
        )
    }

    fn hits(&self) -> Vec<Hit> {
        self.hits
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Catalog fetches served so far.
    fn gets(&self) -> usize {
        self.hits()
            .iter()
            .filter(|hit| hit.method == "GET" && hit.path == "/v1/models")
            .count()
    }

    /// The bodies of the chat requests served so far, in order.
    fn chats(&self) -> Vec<String> {
        self.hits()
            .into_iter()
            .filter(|hit| hit.method == "POST" && hit.path == "/v1/chat/completions")
            .map(|hit| hit.body)
            .collect()
    }
}

async fn accept(listener: TcpListener, hits: Arc<Mutex<Vec<Hit>>>) {
    let mut connections = JoinSet::new();
    while let Ok((socket, _)) = listener.accept().await {
        connections.spawn(serve(socket, Arc::clone(&hits)));
    }
}

async fn serve(socket: TcpStream, hits: Arc<Mutex<Vec<Hit>>>) {
    let mut reader = BufReader::new(socket);
    let mut line = String::new();
    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
        return;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let target = parts.next().unwrap_or_default();
    let path = target.split('?').next().unwrap_or_default().to_owned();
    let mut length = 0_usize;
    loop {
        line.clear();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0_u8; length];
    if reader.read_exact(&mut body).await.is_err() {
        return;
    }
    let (status, kind, reply) = match (method.as_str(), path.as_str()) {
        ("GET", "/v1/models") => ("200 OK", "application/json", MODELS),
        ("POST", "/v1/chat/completions") => ("200 OK", "text/event-stream", CHAT_REPLY),
        _ => ("404 Not Found", "text/plain", ""),
    };
    hits.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(Hit {
            method,
            path,
            body: String::from_utf8_lossy(&body).into_owned(),
        });
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
        reply.len()
    );
    let _ = reader.get_mut().write_all(response.as_bytes()).await;
    let _ = reader.get_mut().shutdown().await;
}

/// A call the test holds until it releases it.
struct Hold {
    armed: AtomicBool,
    entered: Notify,
    gate: Notify,
}

impl Hold {
    fn armed() -> Arc<Self> {
        Arc::new(Self {
            armed: AtomicBool::new(true),
            entered: Notify::new(),
            gate: Notify::new(),
        })
    }

    /// Parks the first caller until `release`.
    async fn park(&self) {
        if self.armed.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.gate.notified().await;
        }
    }

    async fn entered(&self) {
        tokio::time::timeout(WAIT, self.entered.notified())
            .await
            .expect("the held call is entered");
    }

    fn release(&self) {
        self.gate.notify_one();
    }
}

/// Records the model every input hook and every request hook saw.
#[derive(Clone)]
struct Probe {
    inputs: Arc<Mutex<Vec<Option<ModelInfo>>>>,
    requests: Arc<Mutex<Vec<ModelInfo>>>,
    hold: Option<Arc<Hold>>,
}

impl Probe {
    fn new(hold: Option<Arc<Hold>>) -> Self {
        Self {
            inputs: Arc::default(),
            requests: Arc::default(),
            hold,
        }
    }

    fn extension(&self) -> Extension {
        ExtensionBuilder::new("probe", "0.1.0", ServiceSet::EMPTY)
            .expect("builder")
            .with_origin(Origin::Builtin, None)
            .on_input(self.clone())
            .on_before_request(self.clone())
            .build()
            .expect("probe extension")
    }

    fn inputs(&self) -> Vec<Option<ModelInfo>> {
        self.inputs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn requests(&self) -> Vec<ModelInfo> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Hook<InputEvent, InputVerdict> for Probe {
    fn call(
        &self,
        input: InputEvent,
        _cx: HookCx,
    ) -> BoxFuture<'static, Result<InputVerdict, HookError>> {
        self.inputs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(input.model);
        let hold = self.hold.clone();
        Box::pin(async move {
            if let Some(hold) = hold {
                hold.park().await;
            }
            Ok(InputVerdict::Continue)
        })
    }
}

impl Hook<BeforeRequest, Option<RequestParams>> for Probe {
    fn call(
        &self,
        input: BeforeRequest,
        _cx: HookCx,
    ) -> BoxFuture<'static, Result<Option<RequestParams>, HookError>> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(input.model);
        Box::pin(async { Ok(None) })
    }
}

/// A started host with one open session and one subscription.
struct Live {
    host: Host,
    agent: Agent,
    subscription: Subscription,
    _tmp: tempfile::TempDir,
}

impl Live {
    async fn start(config: &str, extensions: Vec<Extension>, vars: &[(&str, &str)]) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let data = tmp.path().join("data");
        let work = tmp.path().join("w");
        std::fs::create_dir_all(&data).expect("data dir");
        std::fs::create_dir_all(&work).expect("workspace dir");
        let config = Config::load(ConfigProduct::Dalgon, &data, "", Some(config)).expect("config");
        let product = Product {
            name: "dal",
            data_root: data,
            defaults: "",
            extensions,
            bundled: Vec::new(),
        };
        let env = Env {
            vars: vars
                .iter()
                .map(|(key, value)| (OsString::from(key), OsString::from(value)))
                .collect::<BTreeMap<_, _>>(),
            cwd: work.clone(),
            sandbox_helper: None,
        };
        let host = Host::start(product, config, env).await.expect("host");
        let agent = host
            .open(
                SessionRef::New {
                    workspace: Workspace::new(work).expect("workspace"),
                    name: None,
                },
                ClientId::new("identity"),
            )
            .await
            .expect("open");
        let subscription = agent.subscribe(None).expect("subscribe");
        Self {
            host,
            agent,
            subscription,
            _tmp: tmp,
        }
    }

    async fn submit_prompt(&self, text: &str) -> TurnId {
        let reply = self
            .agent
            .submit(Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text { text: text.into() }],
            })
            .await
            .expect("submit prompt");
        let turn = if let Reply::Accepted { turn, .. } = reply {
            Some(turn)
        } else {
            None
        };
        turn.expect("the prompt was accepted")
    }

    /// Selects `route` and returns once the actor folded the command.
    async fn select(&self, route: ModelRoute) {
        tokio::time::timeout(
            WAIT,
            self.agent.submit(Command::SetModel {
                model: route,
                save: Save::SessionOnly,
            }),
        )
        .await
        .expect("model selection settles")
        .expect("model selection");
    }

    /// Waits for the end of `turn` and returns its stop.
    async fn ended(&mut self, turn: TurnId) -> Stop {
        let wait = async {
            loop {
                let delivery = self.subscription.next().await.expect("stream open");
                let Delivery::Update(update) = delivery else {
                    continue;
                };
                if let UpdateKind::TurnEnded { turn: seen, stop } = &update.kind
                    && *seen == turn
                {
                    return *stop;
                }
            }
        };
        tokio::time::timeout(WAIT, wait)
            .await
            .expect("the turn ends in time")
    }

    async fn finish(self) {
        let session = self.agent.session();
        self.host.close(session).await.expect("close session");
        self.host.shutdown(Duration::from_secs(1)).await;
    }
}

fn live_vars() -> [(&'static str, &'static str); 1] {
    [("OPENAI_API_KEY", "sk-test")]
}

fn names(body: &str, model: &str) -> bool {
    body.contains(&format!("\"model\":\"{model}\""))
}

/// One hooked prompt against a live provider. The hook sees one resolved
/// model; the first request carries that same identity; the request
/// preparation adds no catalog lookup of its own. The preimage fetched the
/// catalog a third time in `Driver::resolve_request`.
#[tokio::test]
async fn a_hooked_prompt_shares_one_catalog_resolution_between_hook_and_request() {
    let server = Loopback::start().await;
    let probe = Probe::new(None);
    let mut live = Live::start(&server.config(), vec![probe.extension()], &live_vars()).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let before = server.gets();
    let turn = live.submit_prompt("hello").await;
    assert_eq!(live.ended(turn).await, Stop::EndTurn);
    assert_eq!(
        server.gets() - before,
        OPENING_LOOKUPS + TRANSPORT_LOOKUPS,
        "catalog fetches for one hooked prompt"
    );
    let inputs = probe.inputs();
    let requests = probe.requests();
    assert_eq!((inputs.len(), requests.len()), (1, 1));
    let admitted = inputs[0].clone().expect("the opening resolved a model");
    assert_eq!(
        admitted, requests[0],
        "the hook and the request saw one identity"
    );
    let chats = server.chats();
    assert_eq!(chats.len(), 1);
    assert!(names(&chats[0], "gpt-4o"), "the request body: {}", chats[0]);
    live.finish().await;
}

/// A model selection submitted while the input hook is held cannot fold
/// before the opening's request batch: the first request keeps the model the
/// hook saw, and the selection applies to the next prompt, which again
/// resolves once for its hook and its request.
#[tokio::test]
async fn a_model_selection_during_a_held_input_hook_applies_from_the_next_prompt() {
    let server = Loopback::start().await;
    let hold = Hold::armed();
    let probe = Probe::new(Some(Arc::clone(&hold)));
    let mut live = Live::start(&server.config(), vec![probe.extension()], &live_vars()).await;
    let first = live.submit_prompt("hello").await;
    hold.entered().await;
    let mut selecting = Box::pin(live.agent.submit(Command::SetModel {
        model: ModelRoute::from_id("openai-chat/gpt-4o-mini"),
        save: Save::SessionOnly,
    }));
    // The hook stays held while the selection waits in the command lane.
    let _ = tokio::time::timeout(Duration::from_millis(300), &mut selecting).await;
    hold.release();
    tokio::time::timeout(WAIT, selecting)
        .await
        .expect("model selection settles")
        .expect("model selection");
    assert_eq!(live.ended(first).await, Stop::EndTurn);
    let before = server.gets();
    let second = live.submit_prompt("again").await;
    assert_eq!(live.ended(second).await, Stop::EndTurn);
    assert_eq!(
        server.gets() - before,
        OPENING_LOOKUPS + TRANSPORT_LOOKUPS,
        "catalog fetches for the prompt after the selection"
    );
    let inputs = probe.inputs();
    let requests = probe.requests();
    assert_eq!((inputs.len(), requests.len()), (2, 2));
    let chats = server.chats();
    assert_eq!(chats.len(), 2);
    assert!(
        names(&chats[0], "gpt-4o"),
        "the held opening's request: {}",
        chats[0]
    );
    assert!(
        names(&chats[1], "gpt-4o-mini"),
        "the selected model's request: {}",
        chats[1]
    );
    for (hooked, requested) in inputs.iter().zip(&requests) {
        let hooked = hooked.clone().expect("the opening resolved a model");
        assert_eq!(&hooked, requested, "hook and request identity");
    }
    assert_ne!(requests[0], requests[1]);
    live.finish().await;
}

/// A registered model answering with fixed usage, recording each request.
struct Answer {
    seen: Arc<Mutex<Vec<ModelRoute>>>,
    tokens: u64,
}

impl ModelHandler for Answer {
    fn run<'a>(
        &'a self,
        request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request.model);
        let events = vec![
            StreamEvent::TextDelta { text: "ok".into() },
            StreamEvent::ToolCallsDone { calls: Vec::new() },
            StreamEvent::Usage {
                usage: Usage {
                    input_tokens: self.tokens,
                    cached_input_tokens: 0,
                    output_tokens: 5,
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
        Box::pin(async move {
            Ok(EventStream::new(
                stream::iter(events.into_iter().map(Ok::<_, ProviderError>)),
                || {},
            ))
        })
    }
}

fn model_extension(
    name: &str,
    id: &str,
    window: u32,
    seen: &Arc<Mutex<Vec<ModelRoute>>>,
) -> Extension {
    ExtensionBuilder::new(name, "0.1.0", ServiceSet::EMPTY)
        .expect("builder")
        .with_origin(Origin::Builtin, None)
        .model(ModelRecord {
            id: ModelId::parse(id).expect("model id"),
            caps: Caps {
                context_window: Some(window),
                thinking: Box::new([ThinkingLevel::Off]),
                tool_use: false,
                image_input: false,
                custom_grammar: false,
            },
            handler: Arc::new(Answer {
                seen: Arc::clone(seen),
                tokens: 90_000,
            }),
            export: None,
        })
        .build()
        .expect("model extension")
}

/// What the compactor was handed.
#[derive(Clone, Debug, PartialEq)]
struct Squeezed {
    model: ModelRoute,
    window: Option<u64>,
}

/// A compactor that records its input, optionally holds, and declines.
struct Squeeze {
    seen: Arc<Mutex<Vec<Squeezed>>>,
    hold: Option<Arc<Hold>>,
}

impl Compactor for Squeeze {
    fn compact<'a>(
        &'a self,
        input: CompactInput<'a>,
        _services: Arc<dyn Services>,
    ) -> BoxFuture<'a, Result<Option<Compaction>, CompactError>> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Squeezed {
                model: input.model.clone(),
                window: input.context_window,
            });
        let hold = self.hold.clone();
        Box::pin(async move {
            if let Some(hold) = hold {
                hold.park().await;
            }
            Ok(None)
        })
    }
}

const A_WINDOW: u32 = 100_000;
const B_WINDOW: u32 = 64_000;

/// Two registered models with different windows, a probe, and a compactor.
struct Windowed {
    live: Live,
    probe: Probe,
    a: Arc<Mutex<Vec<ModelRoute>>>,
    b: Arc<Mutex<Vec<ModelRoute>>>,
    squeezed: Arc<Mutex<Vec<Squeezed>>>,
}

impl Windowed {
    async fn start(hold: Option<Arc<Hold>>) -> Self {
        let a = Arc::new(Mutex::new(Vec::new()));
        let b = Arc::new(Mutex::new(Vec::new()));
        let squeezed = Arc::new(Mutex::new(Vec::new()));
        let probe = Probe::new(None);
        let squeeze = ExtensionBuilder::new("squeeze", "0.1.0", ServiceSet::EMPTY)
            .expect("builder")
            .with_origin(Origin::Builtin, None)
            .compactor(
                "squeeze",
                Arc::new(Squeeze {
                    seen: Arc::clone(&squeezed),
                    hold,
                }),
            )
            .build()
            .expect("compactor extension");
        let extensions = vec![
            model_extension("model-a", "fixture/a", A_WINDOW, &a),
            model_extension("model-b", "fixture/b", B_WINDOW, &b),
            probe.extension(),
            squeeze,
        ];
        let live = Live::start("model = \"fixture/a\"\n", extensions, &[]).await;
        Self {
            live,
            probe,
            a,
            b,
            squeezed,
        }
    }

    /// One prompt, long enough that the next opening compacts first.
    async fn long_prompt(&self) -> TurnId {
        self.live.submit_prompt(&"x".repeat(BIG)).await
    }

    fn requests(routes: &Arc<Mutex<Vec<ModelRoute>>>) -> usize {
        routes.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    fn squeezed(&self) -> Vec<Squeezed> {
        self.squeezed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

fn route(id: &str) -> ModelRoute {
    ModelRoute::synthetic(id).expect("synthetic route")
}

/// An explicit model selection that folds while the opening's compaction
/// runs still takes the turn's first request, exactly as before the carry
/// existed: the carried identity is dropped, not reused, and no hook runs
/// again. The compaction itself ran on the identity the opening resolved.
///
/// Known and unchanged: the input hook admitted its content against the
/// first model, so a selection folding inside this window sends admitted
/// content to the second model.
#[tokio::test]
async fn a_model_selection_during_the_pre_request_compaction_still_takes_the_first_request() {
    let hold = Hold::armed();
    let mut fixture = Windowed::start(Some(Arc::clone(&hold))).await;
    let warm = fixture.long_prompt().await;
    assert_eq!(fixture.live.ended(warm).await, Stop::EndTurn);
    assert_eq!(
        fixture.squeezed(),
        Vec::new(),
        "the first turn compacts nothing"
    );
    let turn = fixture.long_prompt().await;
    hold.entered().await;
    fixture.live.select(route("fixture/b")).await;
    hold.release();
    assert_eq!(fixture.live.ended(turn).await, Stop::EndTurn);
    assert_eq!(
        fixture.squeezed(),
        vec![Squeezed {
            model: route("fixture/a"),
            window: Some(u64::from(A_WINDOW)),
        }],
        "the compaction ran on the opening's model"
    );
    assert_eq!(
        Windowed::requests(&fixture.a),
        1,
        "only the warm turn used a"
    );
    assert_eq!(
        Windowed::requests(&fixture.b),
        1,
        "the selection took the first request"
    );
    let inputs = fixture.probe.inputs();
    assert_eq!(inputs.len(), 2, "no hook ran twice");
    let admitted = inputs[1].clone().expect("the opening resolved a model");
    assert_eq!(admitted.route, route("fixture/a"));
    assert_eq!(fixture.probe.requests()[1].route, route("fixture/b"));
    fixture.live.finish().await;
}

/// With the route unchanged since the opening, a compaction before the
/// first request is sized from the model the opening resolved, not from the
/// previous turn's catalog row, and the request that follows keeps that
/// identity. The preimage sized this compaction from the previous turn's
/// row, so the window below came out as the first model's.
#[tokio::test]
async fn a_pre_request_compaction_is_sized_from_the_opening_model() {
    let mut fixture = Windowed::start(None).await;
    let warm = fixture.long_prompt().await;
    assert_eq!(fixture.live.ended(warm).await, Stop::EndTurn);
    fixture.live.select(route("fixture/b")).await;
    let turn = fixture.long_prompt().await;
    assert_eq!(fixture.live.ended(turn).await, Stop::EndTurn);
    assert_eq!(
        fixture.squeezed(),
        vec![Squeezed {
            model: route("fixture/b"),
            window: Some(u64::from(B_WINDOW)),
        }],
        "the compaction used the opening's catalog row"
    );
    assert_eq!(Windowed::requests(&fixture.b), 1);
    let inputs = fixture.probe.inputs();
    let requests = fixture.probe.requests();
    assert_eq!(
        inputs[1].clone().expect("the opening resolved a model"),
        requests[1],
        "the hook and the first request saw one identity"
    );
    fixture.live.finish().await;
}
