#![expect(
    clippy::expect_used,
    reason = "lifetime fixtures fail loudly at their setup boundary"
)]
//! Who owns a child session after the parent is cancelled or closed.
//!
//! Four contract rows, all through the real Host/Agent seams with real
//! provider contracts (a scripted parent model plus `ModelHandler`
//! children). No mock services, no fabricated reports.
//!
//! * Parent-turn cancellation while a tool awaits a held child must not
//!   leave the child listed and running (adversarial regression; fails
//!   until the host/service owner adds cancellation-safe child
//!   ownership).
//! * Explicit `agents.cancel` closes the child: it leaves the parent's
//!   list and a later await reports `Cancelled`, never `Pending`.
//! * A completed await reports the child's real text and stop, and the
//!   child stays listed until someone explicitly cancels it.
//! * Closing the parent closes its parent-created child: the child must not
//!   remain in a live running state after the parent boundary, and the
//!   session-end hook observes the parent close.
//! * Cancelling the awaiting parent turn sweeps the whole owned subtree: a
//!   started-but-unawaited grandchild under `agents.max_depth = 2` is
//!   reaped before its owning child boundary, and neither reclaimed
//!   session is ever found running again.
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dal_agent::ext::tool::{ArgError, RawValue, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput};
use dal_agent::ext::{
    BoxFuture, EventStream, Extension, ExtensionBuilder, HookCx, HookError, ModelCx, ModelError,
    ModelHandler, ModelRecord, ObserveHook,
};
use dal_agent::{Agent, Delivery, Env, Host, HostUpdate, Product, SessionRef, Subscription};
use dal_core::{
    AgentInfo, AgentStart, AgentState, AgentsOp, AgentsReply, CallId, CancelScope, Caps, ClientId,
    Command, Config, ConfigProduct, Expect, ModelId, ModelInfo, ModelRequest, Name, Part, RawJson,
    ServiceSet, SessionEnd, SessionId, Stop, ThinkingLevel, ToolClass, ToolSpec, TurnId, TurnState,
    UpdateKind, Usage, Visibility, Workspace,
};
use dal_provider::{ProviderError, StreamEvent};

/// Bound for every settle wait; failures assert on the recorded outcome.
const WAIT: Duration = Duration::from_secs(30);
/// Poll step for held-gate and record flags; state flags, never sleeps, order.
const POLL: Duration = Duration::from_millis(10);

const USAGE: &str = "{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}";

/// One scripted parent-model block calling `tool`, ending in `tool_use`.
fn tool_block(tool: &str) -> String {
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"tool_call_started\",\"id\":\"c-{tool}\",\"name\":\"{tool}\"}},{{\"type\":\"tool_calls_done\",\"calls\":[{{\"id\":\"c-{tool}\",\"name\":\"{tool}\",\"args\":{{\"kind\":\"parsed\",\"value\":{{}}}}}}]}},{USAGE},{{\"type\":\"stop\",\"reason\":\"tool_use\"}}]}}\n"
    )
}

/// One scripted parent-model block ending the turn with plain text.
fn end_block() -> String {
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"text_delta\",\"text\":\"done\"}},{{\"type\":\"tool_calls_done\",\"calls\":[]}},{USAGE},{{\"type\":\"stop\",\"reason\":\"end_turn\"}}]}}\n"
    )
}

fn usage() -> Usage {
    Usage {
        input_tokens: 1,
        cached_input_tokens: 0,
        output_tokens: 1,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        cost_usd: None,
    }
}

/// A child-model reply carrying exactly `text`, then ending the turn.
fn text_stream(text: Box<str>) -> EventStream {
    let items: Vec<Result<StreamEvent, ProviderError>> = vec![
        Ok(StreamEvent::TextDelta { text: text.into() }),
        Ok(StreamEvent::ToolCallsDone { calls: Vec::new() }),
        Ok(StreamEvent::Usage { usage: usage() }),
        Ok(StreamEvent::Stop {
            reason: dal_provider::StopReason::EndTurn,
        }),
    ];
    EventStream::new(futures::stream::iter(items), || {})
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

fn child_model_record(id: &str, handler: Arc<dyn ModelHandler>) -> ModelRecord {
    ModelRecord {
        id: ModelId::parse(id).expect("model id"),
        caps: caps(),
        handler,
        export: None,
    }
}

/// A child model that finishes at once with fixed text.
struct DoneModel;

impl ModelHandler for DoneModel {
    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move { Ok(text_stream("child done".into())) })
    }
}

/// A child model held by the test: its single request blocks until the
/// test releases (or drops) the gate, then it marks `finished` and ends.
/// `entered` proves the child was mid-turn when the parent was cancelled.
struct HoldModel {
    gate: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    entered: AtomicBool,
    finished: AtomicBool,
}

impl ModelHandler for HoldModel {
    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            self.entered.store(true, Ordering::SeqCst);
            let receiver = self
                .gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
                .expect("the held child makes one model request");
            // A dropped sender also releases the hold: the fixture must
            // never hang the test runner, even on an early abort.
            let _ = receiver.await;
            self.finished.store(true, Ordering::SeqCst);
            Ok(text_stream("held done".into()))
        })
    }
}

/// A child-model response calling exactly `tool`, ending in `tool_use`.
fn child_tool_stream(tool: &str) -> EventStream {
    let items: Vec<Result<StreamEvent, ProviderError>> = vec![
        Ok(StreamEvent::ToolCallStarted {
            id: "c-orphan".into(),
            name: tool.into(),
        }),
        Ok(StreamEvent::ToolCallsDone {
            calls: vec![dal_provider::ToolCall {
                id: "c-orphan".into(),
                name: tool.into(),
                args: dal_provider::ToolArgs::Parsed(RawJson::parse("{}").expect("args json")),
            }],
        }),
        Ok(StreamEvent::Usage { usage: usage() }),
        Ok(StreamEvent::Stop {
            reason: dal_provider::StopReason::ToolUse,
        }),
    ];
    EventStream::new(futures::stream::iter(items), || {})
}

/// A child model that starts one grandchild through `tool`, then holds
/// its next request on the test's gate so the child stays mid-turn while
/// the parent's await is abandoned. `held` proves the child was mid-turn
/// and never reached any grandchild await: its second model request is
/// still open when the parent turn is cancelled.
struct SpawnThenHold {
    tool: &'static str,
    gate: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    held: AtomicBool,
}

impl ModelHandler for SpawnThenHold {
    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            if !self.held.swap(true, Ordering::SeqCst) {
                return Ok(child_tool_stream(self.tool));
            }
            let receiver = self
                .gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
                .expect("the spawner child makes two model requests");
            // A dropped sender also releases the hold: the fixture must
            // never hang the test runner, even on an early abort.
            let _ = receiver.await;
            Ok(text_stream("spawner done".into()))
        })
    }
}

/// Typed record of what the fixture tools observed through the real
/// agents service. Assertions read these fields plus the registry-path
/// `Listed` vectors, never bare reply strings alone.
#[derive(Clone, Debug, Default)]
struct ChildRecords {
    started: Option<SessionId>,
    awaiting: bool,
    settled: bool,
    await_reply: Option<AgentsReply>,
    cancel_reply: Option<AgentsReply>,
    listed_before: Option<Vec<AgentInfo>>,
    listed_after: Option<Vec<AgentInfo>>,
    failure: Option<String>,
}

fn tool_spec(name: &Name) -> Arc<ToolSpec> {
    Arc::new(ToolSpec {
        name: name.clone(),
        description: "lifetime fixture tool".into(),
        parameters: RawJson::parse(r#"{"type":"object"}"#).expect("schema"),
        grammar: None,
    })
}

fn start_args(child_name: &'static str, child_model: &'static str) -> AgentStart {
    AgentStart {
        call: CallId::new("c-lifetime-spawn"),
        name: Some(child_name.into()),
        prompt: "work".into(),
        model: Some(child_model.into()),
        role: None,
        system: None,
        tools: None,
        workspace: None,
    }
}

/// Starts one child, flags the outstanding await, and awaits it without a
/// timeout. The tool cancellation token is selected so a parent turn cancel
/// settles this fixture's invocation while leaving child ownership to the
/// service under test. With `cancel_after_await` it then lists, cancels, and
/// lists again, all inside the same invocation.
struct SpawnAwait {
    name: Name,
    spec: Arc<ToolSpec>,
    records: Arc<Mutex<ChildRecords>>,
    child_name: &'static str,
    child_model: &'static str,
    cancel_after_await: bool,
}

impl Tool for SpawnAwait {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _workspace: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, _call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let services = cx.services();
            let caller = cx.caller();
            let cancel = cx.cancel().clone();
            let started = services
                .agents(
                    caller,
                    AgentsOp::Start(start_args(self.child_name, self.child_model)),
                )
                .await;
            let id = match started {
                Ok(AgentsReply::Started { id }) => {
                    self.records
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .started = Some(id);
                    id
                }
                other => {
                    self.records
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .failure = Some(format!("start: {other:?}"));
                    return ToolOutcome::Ok(Box::new(ToolOutput::from_text("done")));
                }
            };
            self.records
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .awaiting = true;
            let awaited = tokio::select! {
                biased;
                () = cancel.cancelled() => return ToolOutcome::Interrupted,
                awaited = services
                    .agents(caller, AgentsOp::Await { id, timeout: None }) =>
                    awaited,
            };
            match awaited {
                Ok(reply) => {
                    self.records
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .await_reply = Some(reply);
                }
                Err(error) => {
                    self.records
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .failure = Some(format!("await: {error:?}"));
                }
            }
            if self.cancel_after_await {
                let listed = services.agents(caller, AgentsOp::List).await;
                match listed {
                    Ok(AgentsReply::Listed(members)) => {
                        self.records
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .listed_before = Some(members);
                    }
                    other => {
                        self.records
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .failure = Some(format!("list before cancel: {other:?}"));
                    }
                }
                let cancelled = services.agents(caller, AgentsOp::Cancel { id }).await;
                match cancelled {
                    Ok(reply) => {
                        self.records
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .cancel_reply = Some(reply);
                    }
                    Err(error) => {
                        self.records
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .failure = Some(format!("cancel: {error:?}"));
                    }
                }
                let listed = services.agents(caller, AgentsOp::List).await;
                match listed {
                    Ok(AgentsReply::Listed(members)) => {
                        self.records
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .listed_after = Some(members);
                    }
                    other => {
                        self.records
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .failure = Some(format!("list after cancel: {other:?}"));
                    }
                }
            }
            ToolOutcome::Ok(Box::new(ToolOutput::from_text("done")))
        })
    }
}

/// Starts one child and returns at once, leaving it running for a later
/// turn (or the host seam) to settle.
struct SpawnOnly {
    name: Name,
    spec: Arc<ToolSpec>,
    records: Arc<Mutex<ChildRecords>>,
    child_name: &'static str,
    child_model: &'static str,
}

impl Tool for SpawnOnly {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _workspace: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, _call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let services = cx.services();
            let caller = cx.caller();
            let started = services
                .agents(
                    caller,
                    AgentsOp::Start(start_args(self.child_name, self.child_model)),
                )
                .await;
            match started {
                Ok(AgentsReply::Started { id }) => {
                    self.records
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .started = Some(id);
                }
                other => {
                    self.records
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .failure = Some(format!("start: {other:?}"));
                }
            }
            ToolOutcome::Ok(Box::new(ToolOutput::from_text("done")))
        })
    }
}

/// Lists, cancels, lists again, then awaits the recorded child, all in one
/// turn, so the before/after registry states share one invocation.
struct SettleChild {
    name: Name,
    spec: Arc<ToolSpec>,
    records: Arc<Mutex<ChildRecords>>,
}

impl Tool for SettleChild {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _workspace: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, _call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let services = cx.services();
            let caller = cx.caller();
            let started = self
                .records
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .started;
            let Some(id) = started else {
                self.records
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .failure = Some("settle ran before any child started".into());
                return ToolOutcome::Ok(Box::new(ToolOutput::from_text("done")));
            };
            let listed = services.agents(caller, AgentsOp::List).await;
            match listed {
                Ok(AgentsReply::Listed(members)) => {
                    self.records
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .listed_before = Some(members);
                }
                other => {
                    self.records
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .failure = Some(format!("list before cancel: {other:?}"));
                }
            }
            let cancelled = services.agents(caller, AgentsOp::Cancel { id }).await;
            match cancelled {
                Ok(reply) => {
                    self.records
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .cancel_reply = Some(reply);
                }
                Err(error) => {
                    self.records
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .failure = Some(format!("cancel: {error:?}"));
                }
            }
            let listed = services.agents(caller, AgentsOp::List).await;
            match listed {
                Ok(AgentsReply::Listed(members)) => {
                    self.records
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .listed_after = Some(members);
                }
                other => {
                    self.records
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .failure = Some(format!("list after cancel: {other:?}"));
                }
            }
            let awaited = services
                .agents(caller, AgentsOp::Await { id, timeout: None })
                .await;
            match awaited {
                Ok(reply) => {
                    self.records
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .await_reply = Some(reply);
                }
                Err(error) => {
                    self.records
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .failure = Some(format!("await after cancel: {error:?}"));
                }
            }
            self.records
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .settled = true;
            ToolOutcome::Ok(Box::new(ToolOutput::from_text("done")))
        })
    }
}

/// Records one parent-scoped `agents.list` for a later turn to inspect.
struct ListOnly {
    name: Name,
    spec: Arc<ToolSpec>,
    records: Arc<Mutex<ChildRecords>>,
}

impl Tool for ListOnly {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _workspace: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, _call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let services = cx.services();
            let caller = cx.caller();
            let listed = services.agents(caller, AgentsOp::List).await;
            match listed {
                Ok(AgentsReply::Listed(members)) => {
                    let mut guard = self
                        .records
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    guard.listed_after = Some(members);
                    guard.settled = true;
                }
                other => {
                    self.records
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .failure = Some(format!("list: {other:?}"));
                }
            }
            ToolOutcome::Ok(Box::new(ToolOutput::from_text("done")))
        })
    }
}

type SessionEndLog = Arc<Mutex<Vec<(SessionId, Box<str>)>>>;

/// Records every session end the host dispatches while closing.
struct EndHook {
    ends: SessionEndLog,
}

impl ObserveHook<SessionEnd> for EndHook {
    fn call(&self, input: SessionEnd, _cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        let ends = Arc::clone(&self.ends);
        Box::pin(async move {
            ends.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((input.session, input.reason));
            Ok(())
        })
    }
}

struct Harness {
    _tmp: tempfile::TempDir,
    host: Host,
    agent: Agent,
    parent: SessionId,
    workspace: Workspace,
    subscription: Subscription,
}

fn fixture_env(tmp: &std::path::Path, cwd: std::path::PathBuf) -> Env {
    Env {
        vars: BTreeMap::from([
            (OsString::from("OPENAI_API_KEY"), OsString::from("sk-test")),
            (OsString::from("HOME"), OsString::from(tmp.join("home"))),
            (
                OsString::from("XDG_CACHE_HOME"),
                OsString::from(tmp.join("cache")),
            ),
        ]),
        cwd,
        sandbox_helper: None,
    }
}

/// A fresh Host over a fresh data root, one parent session, and the given
/// scripted parent-model fixture.
async fn harness(fixture: &str, extension: Extension, agents_table: &str) -> Harness {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data directory");
    std::fs::create_dir_all(&workspace_dir).expect("workspace directory");
    let fixture_path = data.join("script.jsonl");
    std::fs::write(&fixture_path, fixture).expect("script fixture");
    let config_text = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n\n[agents]\n{}",
        fixture_path.to_string_lossy().replace('\\', "\\\\"),
        agents_table
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(config_text.as_str())).expect("config");
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = fixture_env(tmp.path(), workspace_dir.clone());
    let host = Host::start(product, config, env).await.expect("host");
    let workspace = Workspace::new(workspace_dir).expect("workspace");
    let agent = host
        .open(
            SessionRef::New {
                workspace: workspace.clone(),
                name: None,
            },
            ClientId::new("lifetime-test"),
        )
        .await
        .expect("session");
    let parent = agent.session();
    let subscription = agent.subscribe(None).expect("subscription");
    Harness {
        _tmp: tmp,
        host,
        agent,
        parent,
        workspace,
        subscription,
    }
}

/// Prompts until the session accepts, then returns; the caller waits for
/// the matching turn updates.
async fn prompt(agent: &Agent, text: &str) {
    tokio::time::timeout(WAIT, async {
        loop {
            match agent
                .submit(Command::Prompt {
                    expect: Expect::Idle,
                    content: vec![Part::Text { text: text.into() }],
                })
                .await
            {
                Ok(_) => break,
                Err(_) => tokio::time::sleep(POLL).await,
            }
        }
    })
    .await
    .expect("the session accepted the prompt");
}

/// The next started turn, if one begins before the bound.
async fn next_turn_started(subscription: &mut Subscription) -> Option<TurnId> {
    tokio::time::timeout(WAIT, async {
        while let Some(delivery) = subscription.next().await {
            if let Delivery::Update(update) = delivery
                && let UpdateKind::TurnStarted { turn, .. } = update.kind
            {
                return Some(turn);
            }
        }
        None
    })
    .await
    .unwrap_or(None)
}

/// Drains until any turn ends; the matching assertions read the records.
async fn turn_ended(subscription: &mut Subscription) {
    let _ = tokio::time::timeout(WAIT, async {
        while let Some(delivery) = subscription.next().await {
            if let Delivery::Update(update) = delivery
                && matches!(update.kind, UpdateKind::TurnEnded { .. })
            {
                return;
            }
        }
    })
    .await;
}

/// Whether `turn` ended with the cancellation stop before the bound.
async fn turn_ended_cancelled(subscription: &mut Subscription, turn: TurnId) -> bool {
    tokio::time::timeout(WAIT, async {
        while let Some(delivery) = subscription.next().await {
            if let Delivery::Update(update) = delivery
                && let UpdateKind::TurnEnded { turn: ended, stop } = update.kind
                && ended == turn
            {
                return matches!(stop, Stop::Cancelled);
            }
        }
        false
    })
    .await
    .unwrap_or(false)
}

/// Spins on a record flag with a bound; the assertion after names the
/// recorded failure when the flag never settles.
async fn wait_for(description: &str, mut settled: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !settled() {
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(POLL).await;
    }
    assert!(settled(), "{description} settled before the bound");
}

fn snapshot(records: &Arc<Mutex<ChildRecords>>) -> ChildRecords {
    records
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

#[tokio::test]
async fn cancelled_parent_turn_leaves_no_listed_running_child() {
    let records = Arc::new(Mutex::new(ChildRecords::default()));
    let (release, gate_rx) = tokio::sync::oneshot::channel::<()>();
    let blocking_model = Arc::new(HoldModel {
        gate: Mutex::new(Some(gate_rx)),
        entered: AtomicBool::new(false),
        finished: AtomicBool::new(false),
    });
    let spawn_name = Name::parse("hold_spawn").expect("tool name");
    let inspect_name = Name::parse("inspect_children").expect("tool name");
    let inject = ServiceSet::from_names(["agents"]).expect("agents service");
    let extension = ExtensionBuilder::new("lifetime", "0.1.0", inject)
        .expect("builder")
        .tool(
            Arc::new(SpawnAwait {
                name: spawn_name.clone(),
                spec: tool_spec(&spawn_name),
                records: Arc::clone(&records),
                child_name: "orphan",
                child_model: "kit/hold",
                cancel_after_await: false,
            }),
            Visibility::Model,
        )
        .tool(
            Arc::new(ListOnly {
                name: inspect_name.clone(),
                spec: tool_spec(&inspect_name),
                records: Arc::clone(&records),
            }),
            Visibility::Model,
        )
        .model(child_model_record(
            "kit/hold",
            Arc::clone(&blocking_model) as Arc<dyn ModelHandler>,
        ))
        .build()
        .expect("lifetime extension");
    let fixture = format!(
        "{}{}{}",
        tool_block("hold_spawn"),
        tool_block("inspect_children"),
        end_block()
    );
    let mut rig = harness(&fixture, extension, "").await;

    prompt(&rig.agent, "start the held child").await;
    let turn = next_turn_started(&mut rig.subscription)
        .await
        .expect("the parent turn started");
    wait_for("the child to start", || {
        snapshot(&records).started.is_some()
    })
    .await;
    // The child reached its held model call: it is mid-turn, not queued.
    wait_for("the child turn to reach its model", || {
        blocking_model.entered.load(Ordering::SeqCst)
    })
    .await;
    // The parent tool reached the agents await with no timeout.
    wait_for("the parent await to be outstanding", || {
        snapshot(&records).awaiting
    })
    .await;
    let child = snapshot(&records)
        .started
        .expect("the child started before cancellation");
    rig.agent
        .submit(Command::Cancel {
            scope: CancelScope::Turn(turn),
        })
        .await
        .expect("the turn cancel submits");
    assert!(
        turn_ended_cancelled(&mut rig.subscription, turn).await,
        "the parent turn ends with the cancellation stop"
    );
    // The child is still held, so no await reply could have resolved: the
    // abandoned invocation delivered nothing back into the parent.
    let abandoned = snapshot(&records);
    assert!(
        abandoned.await_reply.is_none(),
        "no await reply lands after the parent turn is cancelled: {:?}",
        abandoned.await_reply
    );
    assert!(
        abandoned.failure.is_none(),
        "the spawn and await issued cleanly before cancellation: {:?}",
        abandoned.failure
    );
    // The parent session itself survives cancellation and can list.
    prompt(&rig.agent, "inspect children").await;
    wait_for("the inspect turn to list", || snapshot(&records).settled).await;
    turn_ended(&mut rig.subscription).await;
    let listed = snapshot(&records);

    // Release the gate and reclaim both sessions before asserting, so a
    // red run leaves no held child or open session behind.
    let _ = release.send(());
    let _ = rig.host.close(child).await;
    let _ = rig.host.close(rig.parent).await;
    let _ = rig.host.shutdown(Duration::from_secs(10)).await;

    let members = listed
        .listed_after
        .as_ref()
        .expect("the post-cancel inspect turn recorded the parent-scoped child list");
    assert!(
        !members.iter().any(|info| info.id == child),
        "a cancelled parent await must close its outstanding child: child {child} is still listed as {members:?}"
    );
}

#[tokio::test]
async fn explicit_agents_cancel_closes_the_child() {
    let records = Arc::new(Mutex::new(ChildRecords::default()));
    let spawn_name = Name::parse("spawn_once").expect("tool name");
    let settle_name = Name::parse("cancel_inspect").expect("tool name");
    let inject = ServiceSet::from_names(["agents"]).expect("agents service");
    let extension = ExtensionBuilder::new("lifetime", "0.1.0", inject)
        .expect("builder")
        .tool(
            Arc::new(SpawnOnly {
                name: spawn_name.clone(),
                spec: tool_spec(&spawn_name),
                records: Arc::clone(&records),
                child_name: "member",
                child_model: "kit/done",
            }),
            Visibility::Model,
        )
        .tool(
            Arc::new(SettleChild {
                name: settle_name.clone(),
                spec: tool_spec(&settle_name),
                records: Arc::clone(&records),
            }),
            Visibility::Model,
        )
        .model(child_model_record(
            "kit/done",
            Arc::new(DoneModel) as Arc<dyn ModelHandler>,
        ))
        .build()
        .expect("lifetime extension");
    let fixture = format!(
        "{}{}{}{}",
        tool_block("spawn_once"),
        end_block(),
        tool_block("cancel_inspect"),
        end_block()
    );
    let mut rig = harness(&fixture, extension, "").await;

    prompt(&rig.agent, "start a child").await;
    wait_for("the child to start", || {
        snapshot(&records).started.is_some()
    })
    .await;
    turn_ended(&mut rig.subscription).await;
    prompt(&rig.agent, "cancel and inspect the child").await;
    wait_for("the settle turn to finish", || snapshot(&records).settled).await;
    turn_ended(&mut rig.subscription).await;
    let snap = snapshot(&records);

    let _ = rig.host.close(rig.parent).await;
    let _ = rig.host.shutdown(Duration::from_secs(10)).await;

    let child = snap.started.expect("the child started");
    assert!(
        snap.failure.is_none(),
        "every agents call issued cleanly: {:?}",
        snap.failure
    );
    let before = snap
        .listed_before
        .as_ref()
        .expect("the settle turn listed before cancelling");
    assert!(
        before.iter().any(|info| info.id == child),
        "the started child is listed before the explicit cancel: {before:?}"
    );
    assert!(
        matches!(&snap.cancel_reply, Some(AgentsReply::Cancelled { id }) if *id == child),
        "explicit agents.cancel reports the closed child: {:?}",
        snap.cancel_reply
    );
    let after = snap
        .listed_after
        .as_ref()
        .expect("the settle turn listed after cancelling");
    assert!(
        !after.iter().any(|info| info.id == child),
        "explicit agents.cancel removes the child from the parent list: {after:?}"
    );
    assert!(
        matches!(&snap.await_reply, Some(AgentsReply::Cancelled { id }) if *id == child),
        "awaiting a cancelled child reports Cancelled, never Pending: {:?}",
        snap.await_reply
    );
}

#[tokio::test]
async fn completed_await_leaves_the_child_listed_until_cancel() {
    let records = Arc::new(Mutex::new(ChildRecords::default()));
    let spawn_name = Name::parse("spawn_await_close").expect("tool name");
    let inject = ServiceSet::from_names(["agents"]).expect("agents service");
    let extension = ExtensionBuilder::new("lifetime", "0.1.0", inject)
        .expect("builder")
        .tool(
            Arc::new(SpawnAwait {
                name: spawn_name.clone(),
                spec: tool_spec(&spawn_name),
                records: Arc::clone(&records),
                child_name: "member",
                child_model: "kit/done",
                cancel_after_await: true,
            }),
            Visibility::Model,
        )
        .model(child_model_record(
            "kit/done",
            Arc::new(DoneModel) as Arc<dyn ModelHandler>,
        ))
        .build()
        .expect("lifetime extension");
    let fixture = format!("{}{}", tool_block("spawn_await_close"), end_block());
    let mut rig = harness(&fixture, extension, "").await;

    prompt(&rig.agent, "await the child then close it").await;
    wait_for("the await to resolve", || {
        snapshot(&records).await_reply.is_some()
    })
    .await;
    turn_ended(&mut rig.subscription).await;
    let snap = snapshot(&records);

    let _ = rig.host.close(rig.parent).await;
    let _ = rig.host.shutdown(Duration::from_secs(10)).await;

    let child = snap.started.expect("the child started");
    assert!(
        snap.failure.is_none(),
        "every agents call issued cleanly: {:?}",
        snap.failure
    );
    assert!(
        matches!(&snap.await_reply, Some(AgentsReply::Await { report }) if report.session == child && report.text.to_string() == "child done" && matches!(report.stop, Stop::EndTurn)),
        "the await carries the child's real report text and terminal stop: {:?}",
        snap.await_reply
    );
    // Completion alone closes nothing: the child stays listed as done
    // until someone explicitly cancels it.
    let before = snap
        .listed_before
        .as_ref()
        .expect("the tool listed after the await resolved");
    assert!(
        before
            .iter()
            .any(|info| info.id == child && matches!(info.state, AgentState::Done(Stop::EndTurn))),
        "the completed child stays listed as done before the explicit cancel: {before:?}"
    );
    assert!(
        matches!(&snap.cancel_reply, Some(AgentsReply::Cancelled { id }) if *id == child),
        "the explicit cancel after completion reports the child: {:?}",
        snap.cancel_reply
    );
    let after = snap
        .listed_after
        .as_ref()
        .expect("the tool listed after cancelling");
    assert!(
        !after.iter().any(|info| info.id == child),
        "only the explicit cancel removes the completed child: {after:?}"
    );
}

#[expect(
    clippy::too_many_lines,
    reason = "parent-close fixture walks removal, reopen, and release in place"
)]
#[tokio::test]
async fn closing_the_parent_closes_the_child() {
    let records = Arc::new(Mutex::new(ChildRecords::default()));
    let (release, gate_rx) = tokio::sync::oneshot::channel::<()>();
    let blocking_model = Arc::new(HoldModel {
        gate: Mutex::new(Some(gate_rx)),
        entered: AtomicBool::new(false),
        finished: AtomicBool::new(false),
    });
    let ends = Arc::new(Mutex::new(Vec::new()));
    let spawn_name = Name::parse("spawn_once").expect("tool name");
    let inject = ServiceSet::from_names(["agents"]).expect("agents service");
    let extension = ExtensionBuilder::new("lifetime", "0.1.0", inject)
        .expect("builder")
        .tool(
            Arc::new(SpawnOnly {
                name: spawn_name.clone(),
                spec: tool_spec(&spawn_name),
                records: Arc::clone(&records),
                child_name: "member",
                child_model: "kit/hold",
            }),
            Visibility::Model,
        )
        .model(child_model_record(
            "kit/hold",
            Arc::clone(&blocking_model) as Arc<dyn ModelHandler>,
        ))
        .on_session_end(EndHook {
            ends: Arc::clone(&ends),
        })
        .build()
        .expect("lifetime extension");
    let fixture = format!("{}{}", tool_block("spawn_once"), end_block());
    let mut rig = harness(&fixture, extension, "").await;

    prompt(&rig.agent, "start a child").await;
    wait_for("the child to start", || {
        snapshot(&records).started.is_some()
    })
    .await;
    wait_for("the child turn to reach its model", || {
        blocking_model.entered.load(Ordering::SeqCst)
    })
    .await;
    turn_ended(&mut rig.subscription).await;
    let child = snapshot(&records).started.expect("the child started");

    // Parent-created children are session-owned members. Closing the
    // parent closes the whole owned subtree and publishes it: the child
    // removal reaches host subscribers before the parent boundary
    // removal, and each session-end hook observes its own boundary.
    let mut removals = rig.host.subscribe();
    rig.host.close(rig.parent).await.expect("the parent closes");
    let mut child_removed = false;
    loop {
        let update = tokio::time::timeout(WAIT, removals.next())
            .await
            .expect("the parent close publishes removals in time")
            .expect("the host subscription survives its own close");
        match update {
            HostUpdate::SessionRemoved { session } if session == child => {
                assert!(!child_removed, "the child removal publishes exactly once");
                child_removed = true;
            }
            HostUpdate::SessionRemoved { session } if session == rig.parent => break,
            _ => {}
        }
    }
    assert!(
        child_removed,
        "closing the parent publishes the child's SessionRemoved before its own boundary"
    );
    let ends = ends
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert!(
        ends.iter()
            .any(|(id, reason)| *id == rig.parent && reason.to_string() == "close"),
        "parent close dispatches the session-end hook: {ends:?}"
    );
    let child_agent = rig
        .host
        .open(
            SessionRef::Resume {
                key: child.to_string().into(),
                workspace: rig.workspace.clone(),
            },
            ClientId::new("lifetime-probe"),
        )
        .await
        .expect("the child journal can be reopened after parent close");
    let view = child_agent
        .view(dal_core::PageReq::default())
        .expect("the reopened child still serves views");
    assert_eq!(
        view.session.id, child,
        "the reopened handle resolves to the same child journal"
    );
    let child_was_running = matches!(view.turn, TurnState::Running { .. });
    if child_was_running {
        let _ = release.send(());
        wait_for("the released child to finish", || {
            blocking_model.finished.load(Ordering::SeqCst)
        })
        .await;
    }
    let _ = rig.host.close(child).await;
    let _ = rig.host.shutdown(Duration::from_secs(10)).await;
    assert!(
        !child_was_running,
        "closing the parent must close its child, not leave it running: {:?}",
        view.turn
    );
}

/// A started-but-never-awaited grandchild must not survive the abandoned
/// await of its owning child: cancelling the awaiting parent turn reclaims
/// the whole owned subtree, the held grandchild before its owning child.
#[expect(
    clippy::too_many_lines,
    reason = "sweep fixture walks spawn, cancel, and reclaim in place"
)]
#[tokio::test]
async fn cancelling_the_awaiting_parent_sweeps_its_unawaited_grandchildren() {
    let child_records = Arc::new(Mutex::new(ChildRecords::default()));
    let grandchild_records = Arc::new(Mutex::new(ChildRecords::default()));
    let ends = Arc::new(Mutex::new(Vec::new()));
    let (release_child, hold_child) = tokio::sync::oneshot::channel::<()>();
    let spawner = Arc::new(SpawnThenHold {
        tool: "orphan_grandchild",
        gate: Mutex::new(Some(hold_child)),
        held: AtomicBool::new(false),
    });
    let (release_grandchild, hold_grandchild) = tokio::sync::oneshot::channel::<()>();
    let held = Arc::new(HoldModel {
        gate: Mutex::new(Some(hold_grandchild)),
        entered: AtomicBool::new(false),
        finished: AtomicBool::new(false),
    });
    let spawn_name = Name::parse("spawn_await").expect("tool name");
    let orphan_name = Name::parse("orphan_grandchild").expect("tool name");
    let list_name = Name::parse("inspect_children").expect("tool name");
    let inject = ServiceSet::from_names(["agents"]).expect("agents service");
    let extension = ExtensionBuilder::new("lifetime", "0.1.0", inject)
        .expect("builder")
        .tool(
            Arc::new(SpawnAwait {
                name: spawn_name.clone(),
                spec: tool_spec(&spawn_name),
                records: Arc::clone(&child_records),
                child_name: "member",
                child_model: "kit/spawner",
                cancel_after_await: false,
            }),
            Visibility::Model,
        )
        .tool(
            Arc::new(SpawnOnly {
                name: orphan_name.clone(),
                spec: tool_spec(&orphan_name),
                records: Arc::clone(&grandchild_records),
                child_name: "hidden",
                child_model: "kit/held",
            }),
            Visibility::Model,
        )
        .tool(
            Arc::new(ListOnly {
                name: list_name.clone(),
                spec: tool_spec(&list_name),
                records: Arc::clone(&child_records),
            }),
            Visibility::Model,
        )
        .model(child_model_record(
            "kit/spawner",
            Arc::clone(&spawner) as Arc<dyn ModelHandler>,
        ))
        .model(child_model_record(
            "kit/held",
            Arc::clone(&held) as Arc<dyn ModelHandler>,
        ))
        .on_session_end(EndHook {
            ends: Arc::clone(&ends),
        })
        .build()
        .expect("lifetime extension");
    let fixture = format!(
        "{}{}{}",
        tool_block("spawn_await"),
        tool_block("inspect_children"),
        end_block()
    );
    // `max_depth = 2` makes the depth-2 grandchild a supported shape, so
    // the sweep is exercised on a reachable configuration.
    let mut rig = harness(&fixture, extension, "max_depth = 2\n").await;

    prompt(&rig.agent, "start the held child").await;
    let turn = next_turn_started(&mut rig.subscription)
        .await
        .expect("the parent turn started");
    // The direct child never awaits its own grandchild: its second model
    // request holds, so the only outstanding await in the tree is the
    // parent's, and the grandchild is established and held before the
    // awaiting ancestor is cancelled.
    wait_for(
        "the held grandchild and the holding child to be established",
        || {
            snapshot(&child_records).started.is_some()
                && snapshot(&grandchild_records).started.is_some()
                && spawner.held.load(Ordering::SeqCst)
                && held.entered.load(Ordering::SeqCst)
        },
    )
    .await;
    let child = snapshot(&child_records).started.expect("the child started");
    let grandchild = snapshot(&grandchild_records)
        .started
        .expect("the grandchild started");
    assert!(
        snapshot(&child_records).awaiting,
        "the parent's await is outstanding when the turn is cancelled"
    );

    // Subscribe before the cancel: the abandoned-await tail runs as soon
    // as it is scheduled, so a later subscription can miss the removals.
    let mut removals = rig.host.subscribe();

    // Cancel the awaiting ancestor: the tool invocation is abandoned and
    // the await's ownership guard fires. No reply could resolve; the
    // abandoned await delivered nothing back into the parent.
    rig.agent
        .submit(Command::Cancel {
            scope: CancelScope::Turn(turn),
        })
        .await
        .expect("the turn cancel submits");
    assert!(turn_ended_cancelled(&mut rig.subscription, turn).await);
    // The child is still held, so no await reply could have resolved: the
    // abandoned invocation delivered nothing back into the parent.
    let abandoned = snapshot(&child_records);
    assert!(
        abandoned.failure.is_none(),
        "every agents call issued cleanly: {:?}",
        abandoned.failure
    );
    assert!(
        abandoned.await_reply.is_none(),
        "an abandoned await delivers nothing back into the parent: {:?}",
        abandoned.await_reply
    );

    // The sweep reaps the started-but-unawaited grandchild before its
    // owning child boundary, and each swept session dispatches its own
    // session end before that boundary.
    let mut swept: Vec<SessionId> = Vec::new();
    loop {
        let update = tokio::time::timeout(WAIT, removals.next())
            .await
            .expect("the abandoned-await tail publishes removals in time")
            .expect("the host subscription survives the sweep");
        if let HostUpdate::SessionRemoved { session } = update {
            swept.push(session);
            if swept.len() == 2 {
                break;
            }
        }
    }
    assert_eq!(
        swept,
        vec![grandchild, child],
        "the held grandchild is reaped before its owning child boundary"
    );
    let ends = ends
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert_eq!(
        ends,
        vec![(grandchild, "close".into()), (child, "close".into())],
        "each swept session dispatches its own session end: {ends:?}"
    );

    // Neither reclaimed session can be found live again: both journals
    // reopen into the crash-repaired Idle state, never a Running one.
    for id in [child, grandchild] {
        let agent = rig
            .host
            .open(
                SessionRef::Resume {
                    key: id.to_string().into(),
                    workspace: rig.workspace.clone(),
                },
                ClientId::new("lifetime-probe"),
            )
            .await
            .expect("the reclaimed journal can be reopened after the sweep");
        let view = agent
            .view(dal_core::PageReq::default())
            .expect("the reopened session still serves views");
        assert_eq!(
            view.session.id, id,
            "the reopened handle resolves to the same journal"
        );
        assert!(
            !matches!(view.turn, TurnState::Running { .. }),
            "the sweep must not leave {id} running: {:?}",
            view.turn
        );
        let _ = rig.host.close(id).await;
    }

    // Release the holds; nothing resurrects either reclaimed session.
    let _ = release_grandchild.send(());
    let _ = release_child.send(());
    prompt(&rig.agent, "inspect the children").await;
    let () = turn_ended(&mut rig.subscription).await;
    wait_for("the inspect turn to list", || {
        snapshot(&child_records).settled
    })
    .await;
    let listed = snapshot(&child_records)
        .listed_after
        .expect("the inspect turn listed the parent-scoped children");
    assert!(
        !listed
            .iter()
            .any(|info| info.id == child || info.id == grandchild),
        "neither reclaimed session stays in the parent-scoped list: {listed:?}"
    );
    let _ = rig.host.shutdown(Duration::from_secs(10)).await;
}
