//! Synthetic models through the real host: routing before API families,
//! the id stack, scopes, budgets, and the journal-free relay.
#![expect(clippy::expect_used, reason = "test assertions abort on failure")]
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::ext::{
    BoxFuture, EventStream, ExtensionBuilder, ModelCx, ModelError, ModelHandler, ModelRecord,
    ScopeError, ScopeStatus,
};
use dal_agent::{Env, Host, Product, SessionRef};
use dal_core::{
    AgentStart, Budget, CallId, Caps, ClientId, Command, Config, ConfigProduct, ContextItem,
    Expect, Family, ModelId, ModelRequest, ModelRoute, OnError, Part, Purpose, RequestParams,
    ScopeSpec, ServiceSet, ThinkingLevel, Workspace,
};
use dal_provider::{ProviderError, StreamEvent as ProviderEvent};

#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<String>>>);

impl Log {
    fn push(&self, line: impl Into<String>) {
        self.0.lock().expect("log lock").push(line.into());
    }

    fn lines(&self) -> Vec<String> {
        self.0.lock().expect("log lock").clone()
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Forward(&'static str),
    DoubleForward,
    Hang,
    Budget,
    Policy(OnError, &'static str),
    Fifo,
    Usd,
    Agent,
}

struct Handler {
    id: &'static str,
    log: Log,
    mode: Mode,
}

fn api_route() -> ModelRoute {
    ModelRoute::Api {
        family: Family::Chat,
        model: "gpt-6-luna".into(),
    }
}

fn route_of(id: &str) -> ModelRoute {
    if id == "api" {
        return api_route();
    }
    ModelRoute::synthetic(id).expect("valid synthetic id")
}

fn request(model: ModelRoute) -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model,
        system: "".into(),
        tools: Arc::from(Vec::new()),
        context: Arc::from(vec![ContextItem::User {
            parts: vec![Part::Text { text: "hi".into() }],
        }]),
        params: RequestParams::default(),
        cache_key: None,
    }
}

fn done_stream() -> EventStream {
    let usage = dal_core::Usage {
        input_tokens: 1,
        cached_input_tokens: 0,
        output_tokens: 1,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        cost_usd: None,
    };
    let items: Vec<Result<ProviderEvent, ProviderError>> = vec![
        Ok(ProviderEvent::TextDelta { text: "ok".into() }),
        Ok(ProviderEvent::ToolCallsDone { calls: Vec::new() }),
        Ok(ProviderEvent::Usage { usage }),
        Ok(ProviderEvent::Stop {
            reason: dal_provider::StopReason::EndTurn,
        }),
    ];
    EventStream::new(futures::stream::iter(items), || {})
}

async fn until(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !ready() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("condition reached");
}

impl Handler {
    async fn budget(&self, cx: &ModelCx<'_>) {
        let spec = ScopeSpec {
            limit: 2,
            on_error: OnError::Settle,
            budget: Budget {
                input_tokens: Some(10),
                ..Budget::default()
            },
        };
        let scope = cx.scope(spec).expect("scope opens");
        let first = scope.infer(request(api_route())).expect("first admitted");
        let done = scope.next().await.expect("first finished");
        self.log.push(format!(
            "first {:?} usage {}",
            done.status(),
            first.usage().input_tokens
        ));
        let second = scope.infer(request(api_route()));
        self.log.push(format!("second {:?}", second.err()));
    }

    async fn policy(&self, cx: &ModelCx<'_>, on_error: OnError, other: &str) {
        let spec = ScopeSpec {
            limit: 2,
            on_error,
            budget: Budget::default(),
        };
        let scope = cx.scope(spec).expect("scope opens");
        let bad = scope
            .infer(request(route_of("acme/loop")))
            .expect("bad admitted");
        let good = scope
            .infer(request(route_of(other)))
            .expect("good admitted");
        let handles = scope.all().await;
        self.log.push(format!(
            "bad {:?} good {:?} count {} error {:?}",
            bad.status(),
            good.status(),
            handles.len(),
            bad.error()
        ));
    }

    async fn fifo(&self, cx: &ModelCx<'_>) {
        let spec = ScopeSpec {
            limit: 1,
            on_error: OnError::Settle,
            budget: Budget::default(),
        };
        let scope = cx.scope(spec).expect("scope opens");
        let first = scope.infer(request(route_of("acme/hang"))).expect("first");
        let second = scope.infer(request(route_of("acme/hang"))).expect("second");
        until(|| first.status() == ScopeStatus::Running).await;
        self.log
            .push(format!("before {:?} {:?}", first.status(), second.status()));
        first.cancel();
        let _ = first.result().await;
        until(|| second.status() == ScopeStatus::Running).await;
        self.log
            .push(format!("after {:?} {:?}", first.status(), second.status()));
    }

    #[expect(
        clippy::unused_async,
        reason = "handler methods share an async interface; this one never awaits"
    )]
    async fn usd(&self, cx: &ModelCx<'_>) {
        let spec = ScopeSpec {
            limit: 1,
            on_error: OnError::Settle,
            budget: Budget {
                usd: Some(1.0),
                ..Budget::default()
            },
        };
        let scope = cx.scope(spec).expect("scope opens");
        let refused = scope.infer(request(route_of("acme/hang")));
        self.log.push(format!("usd {:?}", refused.err()));
    }

    async fn agent(&self, cx: &ModelCx<'_>) {
        let spec = ScopeSpec {
            limit: 1,
            on_error: OnError::Settle,
            budget: Budget::default(),
        };
        let scope = cx.scope(spec).expect("scope opens");
        let start = AgentStart {
            call: CallId::new("call"),
            name: "member".into(),
            prompt: "work".into(),
            model: None,
            role: None,
            system: None,
            tools: None,
            workspace: None,
        };
        let handle = scope.agent(start).expect("agent handle admitted");
        let result = handle.result().await;
        self.log.push(format!("agent {:?}", result.err()));
    }
}

impl ModelHandler for Handler {
    fn run<'a>(
        &'a self,
        request: ModelRequest,
        cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            self.log.push(format!("ran {}", self.id));
            match self.mode {
                Mode::Forward(target) => {
                    let mut request = request;
                    request.model = route_of(target);
                    let forwarded = cx.forward(request, &[]).await;
                    if let Err(error) = &forwarded {
                        self.log.push(format!("forward {}: {error:?}", self.id));
                    }
                    forwarded
                }
                Mode::DoubleForward => {
                    let mut request = request;
                    request.model = api_route();
                    let first = cx.forward(request.clone(), &[]).await?;
                    let second = cx.forward(request, &[]).await;
                    self.log.push(format!("second forward: {:?}", second.err()));
                    Ok(first)
                }
                Mode::Hang => std::future::pending().await,
                Mode::Budget => {
                    self.budget(&cx).await;
                    Ok(done_stream())
                }
                Mode::Policy(on_error, other) => {
                    self.policy(&cx, on_error, other).await;
                    Ok(done_stream())
                }
                Mode::Fifo => {
                    self.fifo(&cx).await;
                    Ok(done_stream())
                }
                Mode::Usd => {
                    self.usd(&cx).await;
                    Ok(done_stream())
                }
                Mode::Agent => {
                    self.agent(&cx).await;
                    Ok(done_stream())
                }
            }
        })
    }
}

fn caps() -> Caps {
    Caps {
        context_window: Some(100_000),
        thinking: Box::new([ThinkingLevel::Off]),
        tool_use: true,
        image_input: false,
        custom_grammar: false,
    }
}

fn text_step(text: &str, input: u64, output: u64) -> String {
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"text_delta\",\"text\":\"{text}\"}},{{\"type\":\"tool_calls_done\",\"calls\":[]}},{{\"type\":\"usage\",\"usage\":{{\"input_tokens\":{input},\"cached_input_tokens\":0,\"output_tokens\":{output},\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}}},{{\"type\":\"stop\",\"reason\":\"end_turn\"}}]}}"
    )
}

struct Rig {
    host: Host,
    log: Log,
    data: PathBuf,
    workspace: PathBuf,
    _tmp: tempfile::TempDir,
}

const MODELS: [(&str, Mode); 15] = [
    ("acme/echo", Mode::Forward("api")),
    ("acme/double", Mode::DoubleForward),
    ("acme/loop", Mode::Forward("acme/loop")),
    ("acme/hang", Mode::Hang),
    ("acme/budget", Mode::Budget),
    ("acme/cancel", Mode::Policy(OnError::Cancel, "acme/hang")),
    ("acme/settle", Mode::Policy(OnError::Settle, "api")),
    ("acme/fifo", Mode::Fifo),
    ("acme/usd", Mode::Usd),
    ("acme/agent", Mode::Agent),
    ("acme/d1", Mode::Forward("acme/d2")),
    ("acme/d2", Mode::Forward("acme/d3")),
    ("acme/d3", Mode::Forward("acme/d4")),
    ("acme/d4", Mode::Forward("acme/d5")),
    ("acme/d5", Mode::Forward("api")),
];

async fn rig(steps: &[String], model: &str) -> Rig {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace = tmp.path().join("w");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&workspace).expect("workspace dir");
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, steps.join("\n")).expect("fixture");
    let user = format!(
        "model = \"{model}\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.display()
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(user.as_str())).expect("config");
    let log = Log::default();
    let mut builder =
        ExtensionBuilder::new("synth", "0.1.0", ServiceSet::default()).expect("builder");
    for (id, mode) in MODELS {
        builder = builder.model(ModelRecord {
            id: ModelId::parse(id).expect("model id"),
            caps: caps(),
            handler: Arc::new(Handler {
                id,
                log: log.clone(),
                mode,
            }),
            export: None,
        });
    }
    let product = Product {
        name: "dal",
        data_root: data.clone(),
        defaults: "",
        extensions: vec![builder.build().expect("extension")],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: workspace.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await.expect("host");
    Rig {
        host,
        log,
        data,
        workspace,
        _tmp: tmp,
    }
}

async fn relay(rig: &Rig, id: &str) -> Vec<Result<ProviderEvent, ProviderError>> {
    let mut stream = rig
        .host
        .relay(ClientId::new("router"), route_of(id), request(route_of(id)))
        .await
        .expect("relay opens");
    let mut items = Vec::new();
    while let Some(item) = tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .expect("stream item in time")
    {
        items.push(item);
    }
    items
}

fn failure(items: &[Result<ProviderEvent, ProviderError>]) -> Option<dal_core::InferFailure> {
    match items.last() {
        Some(Err(ProviderError::Synthetic(failure))) => Some(failure.clone()),
        _ => None,
    }
}

fn used_input(items: &[Result<ProviderEvent, ProviderError>]) -> Option<u64> {
    items.iter().find_map(|item| match item {
        Ok(ProviderEvent::Usage { usage }) => Some(usage.input_tokens),
        _ => None,
    })
}

#[tokio::test]
async fn a_model_runtime_rejects_a_second_forward() {
    let rig = rig(&[text_step("once", 1, 1)], "acme/double").await;
    let items = relay(&rig, "acme/double").await;
    assert!(matches!(items.last(), Some(Ok(ProviderEvent::Stop { .. }))));
    assert!(
        rig.log
            .lines()
            .contains(&"second forward: Some(SecondForward)".to_owned()),
        "{:?}",
        rig.log.lines()
    );
}

#[tokio::test]
async fn session_turn_runs_the_registered_handler_before_api_routing() {
    let rig = rig(&[text_step("Hello", 10, 5)], "acme/echo").await;
    let workspace = Workspace::new(rig.workspace.clone()).expect("workspace");
    let agent = rig
        .host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("probe"),
        )
        .await
        .expect("open");
    let mut subscription = agent.subscribe(None).expect("subscribe");
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: "hi".into() }],
        })
        .await
        .expect("submit");
    assert!(matches!(reply, dal_core::Reply::Accepted { .. }));
    let mut ended = None;
    while let Ok(Some(delivery)) =
        tokio::time::timeout(Duration::from_secs(10), subscription.next()).await
    {
        if let dal_agent::Delivery::Update(update) = delivery
            && let dal_core::UpdateKind::TurnEnded { stop, .. } = update.kind
        {
            ended = Some(stop);
            break;
        }
    }
    assert_eq!(
        ended,
        Some(dal_core::Stop::EndTurn),
        "log {:?}",
        rig.log.lines()
    );
    assert_eq!(rig.log.lines(), vec!["ran acme/echo".to_owned()]);
    let dump = format!(
        "{:?}",
        agent.view(dal_core::PageReq::default()).expect("view")
    );
    assert!(
        dump.contains("Hello"),
        "the forwarded text reaches the journaled entry: {dump}"
    );
}

#[tokio::test]
async fn relay_runs_the_handler_and_returns_usage_without_a_session() {
    let rig = rig(&[text_step("Hi", 10, 5)], "acme/echo").await;
    let items = relay(&rig, "acme/echo").await;
    assert_eq!(used_input(&items), Some(10), "{items:?}");
    assert!(matches!(items.last(), Some(Ok(ProviderEvent::Stop { .. }))));
    assert_eq!(rig.log.lines(), vec!["ran acme/echo".to_owned()]);
    let store = dal_store::Store::new(
        rig.data.clone(),
        Workspace::new(rig.workspace.clone()).expect("workspace"),
        dal_core::Product::Dal,
    );
    let listed = store
        .list(dal_core::ListQuery {
            limit: None,
            cursor: None,
            search: None,
        })
        .expect("list");
    assert!(
        listed.items.is_empty(),
        "the relay creates no session journal"
    );
}

#[tokio::test]
async fn forward_to_the_same_synthetic_id_is_a_typed_cycle() {
    let rig = rig(&[], "acme/echo").await;
    let items = relay(&rig, "acme/loop").await;
    let lines = rig.log.lines();
    assert!(
        lines.iter().any(|line| line.contains("SyntheticCycle")),
        "{lines:?}"
    );
    let expected = vec![route_of("acme/loop"), route_of("acme/loop")];
    assert_eq!(
        failure(&items),
        Some(dal_core::InferFailure::SyntheticCycle { chain: expected })
    );
}

#[tokio::test]
async fn nesting_four_synthetic_models_runs_and_five_is_typed_depth() {
    let deep_rig = rig(&[text_step("deep", 3, 1)], "acme/echo").await;
    let items = relay(&deep_rig, "acme/d2").await;
    assert_eq!(
        used_input(&items),
        Some(3),
        "four nested ids run: {items:?}"
    );
    let shallow_rig = rig(&[], "acme/echo").await;
    let items = relay(&shallow_rig, "acme/d1").await;
    let chain: Vec<ModelRoute> = ["d1", "d2", "d3", "d4", "d5"]
        .iter()
        .map(|name| route_of(&format!("acme/{name}")))
        .collect();
    assert_eq!(
        failure(&items),
        Some(dal_core::InferFailure::SyntheticDepth { chain })
    );
    assert!(
        shallow_rig
            .log
            .lines()
            .iter()
            .any(|line| line.contains("SyntheticDepth"))
    );
}

#[tokio::test]
async fn budget_limit_refuses_the_next_handle_once_reached() {
    let rig = rig(&[text_step("a", 10, 5)], "acme/echo").await;
    let items = relay(&rig, "acme/budget").await;
    assert!(matches!(items.last(), Some(Ok(ProviderEvent::Stop { .. }))));
    assert_eq!(
        rig.log.lines(),
        vec![
            "ran acme/budget".to_owned(),
            "first Done usage 10".to_owned(),
            "second Some(Exhausted)".to_owned(),
        ]
    );
}

#[tokio::test]
async fn cancel_policy_cancels_siblings_of_the_first_failure() {
    let rig = rig(&[], "acme/echo").await;
    let _ = relay(&rig, "acme/cancel").await;
    let lines = rig.log.lines();
    let outcome = lines.last().expect("policy line");
    assert!(
        outcome.starts_with("bad Failed good Cancelled count 2"),
        "{outcome}"
    );
}

#[tokio::test]
async fn settle_policy_keeps_every_result_after_a_failure() {
    let rig = rig(&[text_step("s", 2, 1)], "acme/echo").await;
    let _ = relay(&rig, "acme/settle").await;
    let lines = rig.log.lines();
    let outcome = lines.last().expect("policy line");
    assert!(
        outcome.starts_with("bad Failed good Done count 2"),
        "{outcome}"
    );
}

#[tokio::test]
async fn handles_past_the_limit_wait_in_fifo_order() {
    let rig = rig(&[], "acme/echo").await;
    let _ = relay(&rig, "acme/fifo").await;
    let lines = rig.log.lines();
    assert!(
        lines.contains(&"before Running Pending".to_owned()),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"after Cancelled Running".to_owned()),
        "{lines:?}"
    );
}

#[tokio::test]
async fn usd_budget_refuses_an_unpriced_route() {
    let rig = rig(&[], "acme/echo").await;
    let _ = relay(&rig, "acme/usd").await;
    let lines = rig.log.lines();
    assert!(
        lines.contains(&"usd Some(UnpricedModel { model: \"acme/hang\" })".to_owned()),
        "{lines:?}"
    );
}

#[tokio::test]
async fn member_handles_outside_a_session_are_denied_unavailable() {
    let rig = rig(&[], "acme/echo").await;
    let _ = relay(&rig, "acme/agent").await;
    let lines = rig.log.lines();
    let expected = ScopeError::Denied(dal_core::DenyReason::Unavailable {
        what: "member sessions outside a session".into(),
    });
    assert!(
        lines.contains(&format!("agent {:?}", Some(expected))),
        "{lines:?}"
    );
}

#[tokio::test]
async fn unregistered_synthetic_ids_never_reach_a_handler() {
    let rig = rig(&[], "acme/echo").await;
    let result = rig
        .host
        .relay(
            ClientId::new("router"),
            route_of("acme/unregistered"),
            request(route_of("acme/unregistered")),
        )
        .await;
    assert!(matches!(result, Err(dal_agent::HostError::Config { .. })));
    assert!(
        rig.log.lines().is_empty(),
        "no handler ran: {:?}",
        rig.log.lines()
    );
}
