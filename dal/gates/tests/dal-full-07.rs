//! Gate-full scenario 7: hook deadlines end scopes and cancel handles.
#![expect(clippy::expect_used, reason = "SC test")]

#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{
    error::Error,
    future::pending,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use dal_agent::{
    Delivery, Env, SessionRef,
    ext::{
        BoxFuture, EventStream, ExtensionBuilder, Hook, HookCx, HookError, ModelCx, ModelError,
        ModelHandler, ModelRecord, ScopeError, ScopeHandle, ScopeStatus,
    },
};
use dal_core::{
    Budget, CancelScope, Caps, Command, Config, ConfigProduct, ContextItem, Expect, ModelId,
    ModelRequest, ModelRoute, OnError, Part, Purpose, Reply, RequestParams, ScopeSpec, ServiceSet,
    Stop, UpdateKind, Workspace,
};
use support::{TestDir, scripted_session};
use tokio::{sync::watch, time::Instant};

const MEMBER_COUNT: usize = 3;
const OWNER_ROUTE: &str = "gate/hook-owner";
const MEMBER_ROUTE: &str = "gate/hook-member";
const SCRIPTED_RESPONSE: &str = r#"{"kind":"events","events":[{"type":"text_delta","text":"turn complete"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}"#;

struct HookState {
    scope_count: AtomicUsize,
    member_started: watch::Sender<usize>,
    started_total: AtomicUsize,
    active: AtomicUsize,
    dropped: AtomicUsize,
    groups: Mutex<Vec<Vec<ScopeHandle>>>,
    hook_started: Mutex<Vec<Instant>>,
    hook_finished: Mutex<Vec<Instant>>,
}

impl HookState {
    fn new() -> Self {
        let (member_started, _) = watch::channel(0);
        Self {
            scope_count: AtomicUsize::new(0),
            member_started,
            started_total: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
            groups: Mutex::new(Vec::new()),
            hook_started: Mutex::new(Vec::new()),
            hook_finished: Mutex::new(Vec::new()),
        }
    }
}

struct HookFutureGuard {
    state: Arc<HookState>,
}

impl Drop for HookFutureGuard {
    fn drop(&mut self) {
        self.state
            .hook_finished
            .lock()
            .expect("hook finish mutex")
            .push(Instant::now());
    }
}

struct MemberGuard {
    state: Arc<HookState>,
}

impl MemberGuard {
    fn start(state: Arc<HookState>) -> Self {
        state.active.fetch_add(1, Ordering::SeqCst);
        let started = state.started_total.fetch_add(1, Ordering::SeqCst) + 1;
        state.member_started.send_replace(started);
        Self { state }
    }
}

impl Drop for MemberGuard {
    fn drop(&mut self) {
        self.state.active.fetch_sub(1, Ordering::SeqCst);
        self.state.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

struct HangingMember {
    state: Arc<HookState>,
}

impl ModelHandler for HangingMember {
    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            let _guard = MemberGuard::start(state);
            pending::<Result<EventStream, ModelError>>().await
        })
    }
}

struct HookOwner {
    state: Arc<HookState>,
}

impl ModelHandler for HookOwner {
    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            let scope = cx
                .scope(ScopeSpec {
                    limit: 3,
                    on_error: OnError::Settle,
                    budget: Budget::default(),
                })
                .expect("hook scope opens");
            let handles = (0..MEMBER_COUNT)
                .map(|index| {
                    scope
                        .infer(model_request(MEMBER_ROUTE, &index.to_string()))
                        .expect("hook member inference starts")
                })
                .collect::<Vec<_>>();
            let group = state.scope_count.fetch_add(1, Ordering::SeqCst);
            state
                .groups
                .lock()
                .expect("scope group mutex")
                .push(handles);
            let target = (group + 1) * MEMBER_COUNT;
            let mut started = state.member_started.subscribe();
            while *started.borrow_and_update() < target {
                tokio::time::timeout(Duration::from_secs(10), started.changed())
                    .await
                    .expect("nested scope members start")
                    .expect("member-start tracker stays open");
            }
            pending::<Result<EventStream, ModelError>>().await
        })
    }
}

struct DeadlineHook {
    state: Arc<HookState>,
}

impl Hook<dal_core::ext::BeforeTurn, Option<String>> for DeadlineHook {
    fn call(
        &self,
        _input: dal_core::ext::BeforeTurn,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<Option<String>, HookError>> {
        let state = Arc::clone(&self.state);
        let caller = cx.caller.clone();
        let services = Arc::clone(&cx.services);
        Box::pin(async move {
            state
                .hook_started
                .lock()
                .expect("hook start mutex")
                .push(Instant::now());
            let _guard = HookFutureGuard { state };
            services
                .infer(&caller, model_request(OWNER_ROUTE, "hook scope"))
                .await
                .map_err(|_| HookError::Cancelled)?;
            Ok(None)
        })
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

fn scripted_fixture() -> String {
    format!("{SCRIPTED_RESPONSE}\n{SCRIPTED_RESPONSE}\n")
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

async fn wait_for_members(state: &HookState, target: usize) {
    let mut started = state.member_started.subscribe();
    while *started.borrow_and_update() < target {
        tokio::time::timeout(Duration::from_secs(10), started.changed())
            .await
            .expect("nested scope reaches the hook")
            .expect("member-start tracker stays open");
    }
}

async fn wait_for_turn_end(
    updates: &mut dal_agent::Subscription,
    expected: Stop,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let ended = tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(delivery) = updates.next().await {
            if let Delivery::Update(update) = delivery
                && let UpdateKind::TurnEnded { stop, .. } = &update.kind
            {
                assert_eq!(*stop, expected);
                return true;
            }
        }
        false
    })
    .await?;
    assert!(ended, "turn ends with its terminal update");
    Ok(())
}

fn assert_cancelled(handles: &[ScopeHandle]) {
    assert_eq!(handles.len(), MEMBER_COUNT);
    assert!(
        handles
            .iter()
            .all(|handle| handle.status() == ScopeStatus::Cancelled)
    );
    assert!(
        handles
            .iter()
            .all(|handle| handle.error() == Some(ScopeError::Cancelled))
    );
}

#[tokio::test(start_paused = true)]
#[expect(
    clippy::too_many_lines,
    reason = "the hook-deadline scenario drives setup, cancellation, and shutdown in one walkthrough"
)]
async fn hook_deadline_ends_scope_and_cancels_handles() -> Result<(), Box<dyn Error + Send + Sync>>
{
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    let fixture = data.path().join("hook-scripted.jsonl");
    std::fs::write(&fixture, scripted_fixture())?;
    let factory = dalgon::product();
    let user = format!(
        "model = \"openai-responses/gpt-6\"\n[providers.scripted]\nfixture = {:?}\n",
        fixture.to_string_lossy()
    );
    let config = Config::load(
        ConfigProduct::Dalgon,
        data.path(),
        factory.defaults,
        Some(&user),
    )?;
    let state = Arc::new(HookState::new());
    let extension = ExtensionBuilder::new("hook-deadline-gate", "0.1.0", ServiceSet::EMPTY)?
        .model(ModelRecord {
            id: ModelId::parse(OWNER_ROUTE)?,
            caps: caps(),
            handler: Arc::new(HookOwner {
                state: Arc::clone(&state),
            }),
            export: None,
        })
        .model(ModelRecord {
            id: ModelId::parse(MEMBER_ROUTE)?,
            caps: caps(),
            handler: Arc::new(HangingMember {
                state: Arc::clone(&state),
            }),
            export: None,
        })
        .on_before_turn(DeadlineHook {
            state: Arc::clone(&state),
        })
        .build()?;
    let mut product = (factory.build)(&dalgon::BuildCx {
        data_root: data.path().to_path_buf(),
        config: &config,
    })?;
    product.extensions.push(extension);
    let env = Env {
        vars: support::captured_shell_vars(),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    let session = SessionRef::Ephemeral {
        workspace: Workspace::new(workspace.path().to_path_buf())?,
    };
    let harness = scripted_session(product, config, env, session).await?;
    let mut updates = harness.agent.subscribe(None)?;
    let first = harness
        .agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "trigger the deadline hook".into(),
            }],
        })
        .await
        .map_err(|error| format!("deadline prompt submit: {error}"))?;
    assert!(matches!(first, Reply::Accepted { .. }));
    wait_for_members(&state, MEMBER_COUNT).await;
    tokio::time::advance(Duration::from_secs(5)).await;
    wait_for_turn_end(&mut updates, Stop::EndTurn)
        .await
        .map_err(|error| format!("deadline turn end: {error}"))?;
    let groups = state.groups.lock().expect("scope group mutex").clone();
    assert_eq!(groups.len(), 1);
    assert_cancelled(groups.first().unwrap());
    assert_eq!(state.active.load(Ordering::SeqCst), 0);
    assert_eq!(state.dropped.load(Ordering::SeqCst), MEMBER_COUNT);
    let first_started = state.hook_started.lock().expect("hook start mutex")[0];
    let first_finished = state.hook_finished.lock().expect("hook finish mutex")[0];
    assert_eq!(
        first_finished.duration_since(first_started),
        Duration::from_secs(5)
    );

    let Reply::Accepted { turn, .. } = harness
        .agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "cancel the owning hook turn".into(),
            }],
        })
        .await
        .map_err(|error| format!("cancel-turn prompt submit: {error}"))?
    else {
        return Err("the cancellation prompt was not accepted".into());
    };
    wait_for_members(&state, 2 * MEMBER_COUNT).await;
    let cancelled = harness
        .agent
        .submit(Command::Cancel {
            scope: CancelScope::Turn(turn),
        })
        .await
        .map_err(|error| format!("cancel command submit: {error}"))?;
    assert!(matches!(cancelled, Reply::Done(_)));
    wait_for_turn_end(&mut updates, Stop::Cancelled)
        .await
        .map_err(|error| format!("cancelled turn end: {error}"))?;

    let groups = state.groups.lock().expect("scope group mutex").clone();
    assert_eq!(groups.len(), 2);
    assert_cancelled(groups.get(1).unwrap());
    assert_eq!(state.active.load(Ordering::SeqCst), 0);
    assert_eq!(state.dropped.load(Ordering::SeqCst), 2 * MEMBER_COUNT);
    let starts_len = state.hook_started.lock().expect("hook start mutex").len();
    let ends_len = state.hook_finished.lock().expect("hook finish mutex").len();
    let end_after_start = {
        let starts = state.hook_started.lock().expect("hook start mutex");
        let ends = state.hook_finished.lock().expect("hook finish mutex");
        ends[1].duration_since(starts[1]) < Duration::from_secs(5)
    };
    assert_eq!(starts_len, 2);
    assert_eq!(ends_len, 2);
    assert!(end_after_start);
    let _ = harness.host.shutdown(Duration::from_secs(1)).await;
    Ok(())
}
