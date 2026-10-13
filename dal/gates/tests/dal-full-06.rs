#![expect(clippy::expect_used, reason = "SC test")]
#![expect(missing_docs, reason = "SC test")]

#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    sync::{Arc, Mutex},
    time::Duration,
};

use dal_agent::{
    Delivery, Env, SessionRef,
    ext::{
        BoxFuture, EventStream, ExtensionBuilder, ModelCx, ModelError, ModelHandler, ModelRecord,
        ScopeError, ScopeStatus, ScopeValue,
    },
};
use dal_core::{
    Budget, Caps, Command, Config, ConfigProduct, ContextItem, Expect, ModelId, ModelRequest,
    ModelRoute, OnError, Part, Purpose, Reply, RequestParams, ScopeSpec, ServiceSet, Stop,
    UpdateKind, Usage, Workspace,
};
use dal_provider::{StopReason, StreamEvent as ProviderEvent};
use futures::stream;
use support::{TestDir, scripted_session};
use tokio::sync::watch;

const MEMBER_COUNT: usize = 500;
const ADMITTED: usize = 64;
const API_ROUTE: &str = "openai-responses/gpt-6";
const MEMBER_ROUTE: &str = "gate/scope-member";
const OWNER_ROUTE: &str = "gate/scope-owner";
const REPLAY_STEP: &str = r#"{"kind":"events","events":[{"type":"text_delta","text":"member"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":0.001}},{"type":"stop","reason":"end_turn"}]}"#;

struct ScopeControl {
    releases: Vec<watch::Sender<bool>>,
    started: Mutex<Vec<usize>>,
    started_count: watch::Sender<usize>,
    observations: Mutex<Option<ScopeObservations>>,
}

#[derive(Default)]
struct ScopeObservations {
    admitted: usize,
    waiting: usize,
    fifo: bool,
    completed: usize,
    readable: usize,
    requests: u64,
    input_tokens: u64,
    output_tokens: u64,
    cost_usd: Option<f64>,
    budget_exceeded: bool,
    unpriced_model: Option<String>,
}

impl ScopeControl {
    fn new() -> Self {
        let releases = (0..MEMBER_COUNT).map(|_| watch::channel(false).0).collect();
        let (started_count, _) = watch::channel(0);
        Self {
            releases,
            started: Mutex::new(Vec::with_capacity(MEMBER_COUNT)),
            started_count,
            observations: Mutex::new(None),
        }
    }
}

struct MemberModel {
    control: Arc<ScopeControl>,
}

impl ModelHandler for MemberModel {
    fn run<'a>(
        &'a self,
        mut request: ModelRequest,
        cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        let control = Arc::clone(&self.control);
        Box::pin(async move {
            let index = request
                .system
                .parse::<usize>()
                .map_err(|_| ModelError::PrivateRounds)?;
            let mut release = control.releases[index].subscribe();
            {
                let mut started = control.started.lock().expect("started mutex");
                started.push(index);
                control.started_count.send_replace(started.len());
            }
            while !*release.borrow_and_update() {
                release
                    .changed()
                    .await
                    .map_err(|_| ModelError::PrivateRounds)?;
            }
            request.model = ModelRoute::from_id(API_ROUTE);
            request.system = Arc::from("");
            cx.forward(request, &[]).await
        })
    }
}

struct OwnerModel {
    control: Arc<ScopeControl>,
}

impl ModelHandler for OwnerModel {
    #[expect(
        clippy::too_many_lines,
        reason = "SC model stages the whole scope scenario in one run"
    )]
    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        let control = Arc::clone(&self.control);
        Box::pin(async move {
            let spec = ScopeSpec {
                limit: 64,
                on_error: OnError::Settle,
                budget: Budget {
                    requests: Some(500),
                    input_tokens: Some(500),
                    output_tokens: Some(500),
                    wall: Some(Duration::from_secs(300)),
                    usd: Some(1.00),
                },
            };
            let scope = cx.scope(spec).expect("bounded scope opens");
            let mut handles = Vec::with_capacity(MEMBER_COUNT);
            for index in 0..MEMBER_COUNT {
                handles.push(
                    scope
                        .infer(model_request(MEMBER_ROUTE, &index.to_string()))
                        .expect("priced member inference is admitted"),
                );
            }

            let mut started = control.started_count.subscribe();
            wait_for_started(&mut started, ADMITTED).await;
            let admitted = handles[..ADMITTED]
                .iter()
                .filter(|handle| handle.status() == ScopeStatus::Running)
                .count();
            let waiting = handles[ADMITTED..]
                .iter()
                .filter(|handle| handle.status() == ScopeStatus::Pending)
                .count();
            let initially_started = control
                .started
                .lock()
                .expect("started mutex")
                .iter()
                .copied()
                .collect::<BTreeSet<_>>();
            let mut fifo = initially_started == (0..ADMITTED).collect::<BTreeSet<_>>();

            control.releases[0].send_replace(true);
            let first = tokio::time::timeout(Duration::from_secs(30), handles[0].result())
                .await
                .expect("first admitted inference finishes")
                .expect("first admitted inference succeeds");
            assert!(matches!(first, ScopeValue::Inference(_)));

            for index in ADMITTED..MEMBER_COUNT {
                wait_for_started(&mut started, index + 1).await;
                let newly_started = {
                    let started_indices = control.started.lock().expect("started mutex");
                    started_indices.contains(&index)
                };
                fifo &= newly_started;
                fifo &= handles[index].status() == ScopeStatus::Running;
                fifo &= handles[index + 1..]
                    .iter()
                    .all(|handle| handle.status() == ScopeStatus::Pending);
                control.releases[index].send_replace(true);
                let result = tokio::time::timeout(Duration::from_secs(30), handles[index].result())
                    .await
                    .expect("promoted inference finishes")
                    .expect("promoted inference succeeds");
                assert!(matches!(result, ScopeValue::Inference(_)));
            }

            for (release, handle) in control.releases.iter().zip(handles.iter()).skip(1) {
                release.send_replace(true);
                let result = tokio::time::timeout(Duration::from_secs(30), handle.result())
                    .await
                    .expect("remaining admitted inference finishes")
                    .expect("remaining admitted inference succeeds");
                assert!(matches!(result, ScopeValue::Inference(_)));
            }
            let all = tokio::time::timeout(Duration::from_secs(60), scope.all())
                .await
                .expect("all scope members finish");
            let mut observations = ScopeObservations {
                admitted,
                waiting,
                fifo,
                completed: 0,
                readable: 0,
                requests: 0,
                input_tokens: 0,
                output_tokens: 0,
                cost_usd: Some(0.0),
                budget_exceeded: false,
                unpriced_model: None,
            };
            for handle in &all {
                if handle.status() == ScopeStatus::Done {
                    observations.completed += 1;
                }
                let usage = handle.usage();
                observations.requests += 1;
                observations.input_tokens += usage.input_tokens;
                observations.output_tokens += usage.output_tokens;
                observations.cost_usd = match (observations.cost_usd, usage.cost_usd) {
                    (Some(total), Some(cost)) => Some(total + cost),
                    _ => None,
                };
                observations.budget_exceeded |=
                    matches!(handle.error(), Some(ScopeError::Exhausted));
                if matches!(
                    tokio::time::timeout(Duration::from_secs(10), handle.result())
                        .await
                        .expect("settled result stays readable"),
                    Ok(ScopeValue::Inference(_))
                ) {
                    observations.readable += 1;
                }
            }
            let unpriced = cx
                .scope(ScopeSpec {
                    limit: 1,
                    on_error: OnError::Settle,
                    budget: Budget {
                        usd: Some(1.0),
                        ..Budget::default()
                    },
                })
                .expect("USD scope opens");
            observations.unpriced_model =
                match unpriced.infer(model_request("gate/unpriced", "boundary")) {
                    Err(ScopeError::UnpricedModel { model }) => Some(model.into()),
                    _ => None,
                };
            *control.observations.lock().expect("observations mutex") = Some(observations);
            Ok(done_stream())
        })
    }
}

async fn wait_for_started(started: &mut watch::Receiver<usize>, target: usize) {
    while *started.borrow_and_update() < target {
        tokio::time::timeout(Duration::from_secs(30), started.changed())
            .await
            .expect("scope members start in time")
            .expect("scope member tracker stays connected");
    }
}

fn model_request(route: &str, system: &str) -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::from_id(route),
        system: Arc::from(system),
        tools: Vec::new().into(),
        context: Vec::<ContextItem>::new().into(),
        params: RequestParams::default(),
        cache_key: None,
    }
}

fn done_stream() -> EventStream {
    EventStream::new(
        stream::iter([
            Ok(ProviderEvent::TextDelta {
                text: "scope complete".into(),
            }),
            Ok(ProviderEvent::ToolCallsDone { calls: Vec::new() }),
            Ok(ProviderEvent::Usage {
                usage: Usage {
                    input_tokens: 0,
                    cached_input_tokens: 0,
                    output_tokens: 0,
                    reasoning_tokens: None,
                    cache_write_tokens: 0,
                    cost_usd: None,
                },
            }),
            Ok(ProviderEvent::Stop {
                reason: StopReason::EndTurn,
            }),
        ]),
        || {},
    )
}

fn caps() -> Caps {
    Caps {
        context_window: Some(8192),
        thinking: Box::default(),
        tool_use: false,
        image_input: false,
        custom_grammar: false,
    }
}

fn scripted_fixture() -> String {
    let mut fixture = String::with_capacity(REPLAY_STEP.len() * MEMBER_COUNT);
    for _ in 0..MEMBER_COUNT {
        fixture.push_str(REPLAY_STEP);
        fixture.push('\n');
    }
    fixture
}

#[expect(clippy::too_many_lines, reason = "SC gate is one long scope scenario")]
#[tokio::test]
async fn scope_500_members_admits_fifo_and_stays_within_budget()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    let fixture = data.path().join("scope-usage.jsonl");
    std::fs::write(&fixture, scripted_fixture())?;
    let factory = dalgon::product();
    let user = format!(
        "model = \"{OWNER_ROUTE}\"\n[providers.scripted]\nfixture = {:?}\n[prices.\"{MEMBER_ROUTE}\"]\ninput = 1.0\ncached_input = 0.0\noutput = 1.0\nreasoning = 0.0\n",
        fixture.to_string_lossy()
    );
    let config = Config::load(
        ConfigProduct::Dalgon,
        data.path(),
        factory.defaults,
        Some(&user),
    )?;
    let control = Arc::new(ScopeControl::new());
    let extension = ExtensionBuilder::new("scope-gate", "0.1.0", ServiceSet::EMPTY)?
        .model(ModelRecord {
            id: ModelId::parse(MEMBER_ROUTE)?,
            caps: caps(),
            handler: Arc::new(MemberModel {
                control: Arc::clone(&control),
            }),
            export: None,
        })
        .model(ModelRecord {
            id: ModelId::parse(OWNER_ROUTE)?,
            caps: caps(),
            handler: Arc::new(OwnerModel {
                control: Arc::clone(&control),
            }),
            export: None,
        })
        .build()?;
    let mut product = (factory.build)(&dalgon::BuildCx {
        data_root: data.path().to_path_buf(),
        config: &config,
    })?;
    product.extensions.push(extension);
    let env = Env {
        vars: BTreeMap::default(),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    let session = SessionRef::Ephemeral {
        workspace: Workspace::new(workspace.path().to_path_buf())?,
    };
    let harness = scripted_session(product, config, env, session).await?;
    let mut updates = harness.agent.subscribe(None)?;
    let reply = harness
        .agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "run the scoped fan-out".into(),
            }],
        })
        .await?;
    assert!(matches!(reply, Reply::Accepted { .. }));
    let ended = tokio::time::timeout(Duration::from_secs(120), async {
        while let Some(delivery) = updates.next().await {
            if let Delivery::Update(update) = delivery
                && matches!(
                    &update.kind,
                    UpdateKind::TurnEnded {
                        stop: Stop::EndTurn,
                        ..
                    }
                )
            {
                return true;
            }
        }
        false
    })
    .await
    .expect("scope turn completes");
    assert!(ended, "the owner turn has a terminal update");
    let observations = control
        .observations
        .lock()
        .expect("observations mutex")
        .take()
        .expect("scope handler records its completed results");
    assert_eq!(observations.admitted, ADMITTED);
    assert_eq!(observations.waiting, MEMBER_COUNT - ADMITTED);
    assert!(
        observations.fifo,
        "later admissions follow submission order"
    );
    assert_eq!(observations.completed, MEMBER_COUNT);
    assert_eq!(observations.readable, MEMBER_COUNT);
    assert_eq!(observations.requests, 500);
    assert_eq!(observations.input_tokens, 500);
    assert_eq!(observations.output_tokens, 500);
    assert!((observations.cost_usd.unwrap() - 0.50).abs() < 1e-9);
    assert!(!observations.budget_exceeded);
    assert_eq!(
        observations.unpriced_model.as_deref(),
        Some("gate/unpriced")
    );
    let _ = harness.host.shutdown(Duration::from_secs(1)).await;
    Ok(())
}
