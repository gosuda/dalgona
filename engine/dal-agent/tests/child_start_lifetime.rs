#![expect(
    clippy::expect_used,
    reason = "lifetime fixtures fail loudly at their setup boundary"
)]
//! Cancellation while `agents.start` is still opening its child.
//!
//! The real `SessionStart` observer is held after the child session has been
//! wired, but before host activation inserts it in the registry. The owning
//! tool selects the parent turn cancellation against the pending `Start`
//! future, releases the observer, and then uses the public parent list and
//! session-end hook to prove that the opening child was terminated rather
//! than detached. No `Await` is issued, so `OutstandingAwait` is not involved.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dal_agent::ext::tool::{ArgError, RawValue, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput};
use dal_agent::ext::{BoxFuture, ExtensionBuilder, HookCx, HookError, ObserveHook};
use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef, Subscription};
use dal_core::{
    AgentInfo, AgentStart, AgentsOp, AgentsReply, CallId, CancelScope, ClientId, Command, Config,
    ConfigProduct, Expect, Name, Part, ServiceSet, SessionEnd, SessionId, Stop, ToolClass,
    ToolSpec, TurnId, UpdateKind, Visibility, Workspace,
};

const WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(10);
const USAGE: &str = "{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}";

fn tool_block(tool: &str) -> String {
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"tool_call_started\",\"id\":\"c-{tool}\",\"name\":\"{tool}\"}},{{\"type\":\"tool_calls_done\",\"calls\":[{{\"id\":\"c-{tool}\",\"name\":\"{tool}\",\"args\":{{\"kind\":\"parsed\",\"value\":{{}}}}}}]}},{USAGE},{{\"type\":\"stop\",\"reason\":\"tool_use\"}}]}}\n"
    )
}

fn end_block() -> String {
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"text_delta\",\"text\":\"done\"}},{{\"type\":\"tool_calls_done\",\"calls\":[]}},{USAGE},{{\"type\":\"stop\",\"reason\":\"end_turn\"}}]}}\n"
    )
}

fn start_args() -> AgentStart {
    AgentStart {
        max_steps: None,
        call: CallId::new("c-start-lifetime"),
        name: "starting_child".into(),
        prompt: "work".into(),
        model: None,
        role: None,
        system: None,
        tools: None,
        workspace: None,
    }
}

fn tool_spec(name: &Name) -> Arc<ToolSpec> {
    Arc::new(ToolSpec {
        name: name.clone(),
        description: "child-start lifetime fixture".into(),
        parameters: dal_core::RawJson::parse(r#"{"type":"object"}"#).expect("schema"),
        grammar: None,
    })
}

#[derive(Default)]
struct Records {
    child: Option<SessionId>,
    listed: Option<Vec<AgentInfo>>,
    failure: Option<String>,
}

struct DropMark(Arc<AtomicBool>);

impl Drop for DropMark {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Holds exactly the child `SessionStart` dispatched by the real Host while
/// `agents.start` remains pending. The parent setup ran before `armed` was
/// set, so only the child opening enters the gate.
struct HeldStart {
    armed: Arc<AtomicBool>,
    entered: Arc<AtomicBool>,
    dropped: Arc<AtomicBool>,
    completed: Arc<AtomicBool>,
    gate: Arc<Mutex<Option<tokio::sync::oneshot::Receiver<()>>>>,
    child: Arc<Mutex<Option<SessionId>>>,
}

impl ObserveHook<dal_core::SessionStart> for HeldStart {
    fn call(
        &self,
        input: dal_core::SessionStart,
        _cx: HookCx,
    ) -> BoxFuture<'static, Result<(), HookError>> {
        if !self.armed.load(Ordering::SeqCst) {
            return Box::pin(std::future::ready(Ok(())));
        }
        *self
            .child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(input.session);
        self.entered.store(true, Ordering::SeqCst);
        let gate = Arc::clone(&self.gate);
        let dropped = Arc::clone(&self.dropped);
        let completed = Arc::clone(&self.completed);
        Box::pin(async move {
            let _drop = DropMark(dropped);
            let receiver = gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
                .expect("the child start observer is held exactly once");
            let _ = receiver.await;
            completed.store(true, Ordering::SeqCst);
            Ok(())
        })
    }
}

type SessionEndLog = Arc<Mutex<Vec<(SessionId, Box<str>)>>>;

struct EndRecorder {
    ends: SessionEndLog,
}

/// Records the first terminal session end and holds its dispatch while the
/// gate holds, so shutdown must drain the host-owned cleanup tail that
/// awaits this observer before the count reports.
struct HeldEnd {
    ends: SessionEndLog,
    entered: Arc<AtomicBool>,
    gate: Arc<Mutex<Option<tokio::sync::oneshot::Receiver<()>>>>,
}

impl ObserveHook<SessionEnd> for HeldEnd {
    fn call(&self, input: SessionEnd, _cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        let ends = Arc::clone(&self.ends);
        let entered = Arc::clone(&self.entered);
        let gate = Arc::clone(&self.gate);
        Box::pin(async move {
            ends.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((input.session, input.reason));
            entered.store(true, Ordering::SeqCst);
            let receiver = gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(receiver) = receiver {
                let _ = receiver.await;
            }
            Ok(())
        })
    }
}

impl ObserveHook<SessionEnd> for EndRecorder {
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

/// Starts one child and explicitly selects cancellation against the pending
/// real `agents.start` service future. This is the causal discriminator: no
/// child id can be recorded by this tool before the cancellation wins.
struct StartAndCancel {
    name: Name,
    spec: Arc<ToolSpec>,
    records: Arc<Mutex<Records>>,
}

impl Tool for StartAndCancel {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &dal_core::ModelInfo) -> Arc<ToolSpec> {
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
            let started = tokio::select! {
                biased;
                () = cancel.cancelled() => return ToolOutcome::Interrupted,
                started = services.agents(caller, AgentsOp::Start(start_args())) => started,
            };
            match started {
                Ok(AgentsReply::Started { id }) => {
                    self.records
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .child = Some(id);
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

struct ListChildren {
    name: Name,
    spec: Arc<ToolSpec>,
    records: Arc<Mutex<Records>>,
}

impl Tool for ListChildren {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &dal_core::ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _workspace: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, _call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let listed = cx.services().agents(cx.caller(), AgentsOp::List).await;
            match listed {
                Ok(AgentsReply::Listed(members)) => {
                    self.records
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .listed = Some(members);
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

struct Harness {
    _tmp: tempfile::TempDir,
    host: Host,
    agent: Agent,
    parent: SessionId,
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

async fn harness(fixture: &str, extension: dal_agent::ext::Extension) -> Harness {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data directory");
    std::fs::create_dir_all(&workspace_dir).expect("workspace directory");
    let fixture_path = data.join("script.jsonl");
    std::fs::write(&fixture_path, fixture).expect("script fixture");
    let config_text = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n\n[agents]\n",
        fixture_path.to_string_lossy().replace('\\', "\\\\")
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
    let workspace = dal_core::Workspace::new(workspace_dir).expect("workspace");
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("child-start-test"),
        )
        .await
        .expect("parent session");
    let parent = agent.session();
    let subscription = agent.subscribe(None).expect("subscription");
    Harness {
        _tmp: tmp,
        host,
        agent,
        parent,
        subscription,
    }
}

/// A started Host plus workspace with no session opened yet, for root-open
/// cancellation regressions.
struct BareHost {
    _tmp: tempfile::TempDir,
    host: Host,
    workspace: Workspace,
}

async fn bare_host(
    fixture: &str,
    extension: dal_agent::ext::Extension,
    armed: &Arc<AtomicBool>,
) -> BareHost {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data directory");
    std::fs::create_dir_all(&workspace_dir).expect("workspace directory");
    let fixture_path = data.join("script.jsonl");
    std::fs::write(&fixture_path, fixture).expect("script fixture");
    let config_text = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n\n[agents]\n",
        fixture_path.to_string_lossy().replace('\\', "\\\\")
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
    let workspace = dal_core::Workspace::new(workspace_dir).expect("workspace");
    armed.store(true, Ordering::SeqCst);
    BareHost {
        _tmp: tmp,
        host,
        workspace,
    }
}

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

#[expect(
    clippy::too_many_lines,
    reason = "root-open fixture walks hold, abort, and drain in place"
)]
#[tokio::test]
async fn cancelled_root_open_holds_its_end_until_shutdown_drains_it() {
    let armed = Arc::new(AtomicBool::new(false));
    let start_entered = Arc::new(AtomicBool::new(false));
    let end_entered = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicBool::new(false));
    let completed = Arc::new(AtomicBool::new(false));
    let child = Arc::new(Mutex::new(None));
    let ends = Arc::new(Mutex::new(Vec::<(SessionId, Box<str>)>::new()));
    // The start-gate sender is kept alive but never fired: dropping it
    // would resolve the held observer early and race the opener past its
    // held dispatch.
    let (_release, hold) = tokio::sync::oneshot::channel::<()>();
    let gate = Arc::new(Mutex::new(Some(hold)));
    let (end_release, end_hold) = tokio::sync::oneshot::channel::<()>();
    let end_gate = Arc::new(Mutex::new(Some(end_hold)));
    let extension = ExtensionBuilder::new(
        "child_start_lifetime",
        "0.1.0",
        ServiceSet::from_names(["agents"]).expect("agents service"),
    )
    .expect("builder")
    .on_session_start(HeldStart {
        armed: Arc::clone(&armed),
        entered: Arc::clone(&start_entered),
        dropped: Arc::clone(&dropped),
        completed: Arc::clone(&completed),
        gate: Arc::clone(&gate),
        child: Arc::clone(&child),
    })
    .on_session_end(HeldEnd {
        ends: Arc::clone(&ends),
        entered: Arc::clone(&end_entered),
        gate: Arc::clone(&end_gate),
    })
    .build()
    .expect("root-start lifetime extension");
    let fixture = end_block();
    let rig = bare_host(&fixture, extension, &armed).await;

    #[expect(
        clippy::disallowed_methods,
        reason = "test concurrency: the opener task is aborted on the test path"
    )]
    let opener = {
        let host = rig.host.clone();
        let workspace = rig.workspace.clone();
        tokio::spawn(async move {
            host.open(
                SessionRef::New {
                    workspace,
                    name: None,
                },
                ClientId::new("root-start-test"),
            )
            .await
        })
    };
    wait_for("the root start observer to be held", || {
        start_entered.load(Ordering::SeqCst)
    })
    .await;
    let root = child
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .expect("the held root opening captured its session id");
    opener.abort();
    let joined = opener.await;
    assert!(
        matches!(&joined, Err(error) if error.is_cancelled()),
        "the opener must end cancelled, never committed or failed"
    );
    wait_for("the abandoned root opening to drop", || {
        dropped.load(Ordering::SeqCst)
    })
    .await;
    assert!(
        !completed.load(Ordering::SeqCst),
        "the dropped opener must not complete the held start observer"
    );
    wait_for("the cleanup tail to reach its held end observer", || {
        end_entered.load(Ordering::SeqCst)
    })
    .await;

    let shutdown = rig
        .host
        .clone()
        .shutdown_after_quiet_wait(Duration::from_secs(10));
    tokio::pin!(shutdown);
    std::future::poll_fn(|cx| {
        assert!(
            shutdown.as_mut().poll(cx).is_pending(),
            "shutdown must remain pending while the held end observer blocks"
        );
        std::task::Poll::Ready(())
    })
    .await;
    let _ = end_release.send(());
    let shutdown = tokio::time::timeout(WAIT, shutdown)
        .await
        .expect("shutdown finishes after the held end dispatch resolves");
    assert_eq!(
        shutdown.tasks_remaining, 0,
        "the cancelled root open leaves no host-owned work: {shutdown:?}"
    );
    let recorded = ends
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert_eq!(
        recorded,
        vec![(root, "close".into())],
        "the cancelled root opening dispatches exactly one terminal end: {recorded:?}"
    );
}

#[expect(
    clippy::too_many_lines,
    reason = "held-start fixture walks cancel, list, and close in place"
)]
#[tokio::test]
async fn cancelling_start_terminates_the_held_child_before_await_ownership() {
    let records = Arc::new(Mutex::new(Records::default()));
    let armed = Arc::new(AtomicBool::new(false));
    let entered = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicBool::new(false));
    let completed = Arc::new(AtomicBool::new(false));
    let child = Arc::new(Mutex::new(None));
    let ends = Arc::new(Mutex::new(Vec::<(SessionId, Box<str>)>::new()));
    let (release, hold) = tokio::sync::oneshot::channel::<()>();
    let gate = Arc::new(Mutex::new(Some(hold)));
    let start_name = Name::parse("start_during_child_start").expect("tool name");
    let list_name = Name::parse("list_children").expect("tool name");
    let extension = ExtensionBuilder::new(
        "child_start_lifetime",
        "0.1.0",
        ServiceSet::from_names(["agents"]).expect("agents service"),
    )
    .expect("builder")
    .on_session_start(HeldStart {
        armed: Arc::clone(&armed),
        entered: Arc::clone(&entered),
        dropped: Arc::clone(&dropped),
        completed: Arc::clone(&completed),
        gate: Arc::clone(&gate),
        child: Arc::clone(&child),
    })
    .on_session_end(EndRecorder {
        ends: Arc::clone(&ends),
    })
    .tool(
        Arc::new(StartAndCancel {
            name: start_name.clone(),
            spec: tool_spec(&start_name),
            records: Arc::clone(&records),
        }),
        Visibility::Model,
    )
    .tool(
        Arc::new(ListChildren {
            name: list_name.clone(),
            spec: tool_spec(&list_name),
            records: Arc::clone(&records),
        }),
        Visibility::Model,
    )
    .build()
    .expect("child-start lifetime extension");
    let fixture = format!(
        "{}{}{}",
        tool_block("start_during_child_start"),
        tool_block("list_children"),
        end_block()
    );
    let mut rig = harness(&fixture, extension).await;
    armed.store(true, Ordering::SeqCst);

    prompt(&rig.agent, "start a child").await;
    let turn = next_turn_started(&mut rig.subscription)
        .await
        .expect("the parent turn started");
    wait_for("the child start observer to be held", || {
        entered.load(Ordering::SeqCst)
    })
    .await;
    let child = child
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .expect("the held opening has a child session id");

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
    let _ = release.send(());
    wait_for("the abandoned start observer to drop", || {
        dropped.load(Ordering::SeqCst)
    })
    .await;
    assert!(
        !completed.load(Ordering::SeqCst),
        "the cancelled Start must not continue child setup after release"
    );

    prompt(&rig.agent, "list children").await;
    turn_ended(&mut rig.subscription).await;
    let (listed, failure) = {
        let snapshot = records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (
            snapshot.listed.clone().expect("the list turn settled"),
            snapshot.failure.clone(),
        )
    };
    assert!(
        !listed.iter().any(|info| info.id == child),
        "the cancelled start must remove the child from the parent list: {listed:?}"
    );
    assert!(
        failure.is_none(),
        "the start and list calls settled at their intended boundaries: {failure:?}"
    );

    let _ = rig.host.close(rig.parent).await;
    let shutdown = rig.host.shutdown(Duration::from_secs(10)).await;
    assert_eq!(
        shutdown.tasks_remaining, 0,
        "the cancelled start leaves no host-owned work: {shutdown:?}"
    );
    let ended = ends
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .any(|(id, _reason)| *id == child);
    assert!(
        ended,
        "the cancelled start must publish child termination/removal for {child}; observed ends: {:?}",
        ends.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    );
}
