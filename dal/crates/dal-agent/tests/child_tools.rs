#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "fixtures fail loudly at their setup boundary"
)]
//! A child session keeps the tool list and the approval mode its parent
//! gave it, through extension reloads, and cannot call anything else.
use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::ext::command::{CommandCx, CommandHandler};
use dal_agent::ext::tool::{ArgError, RawValue, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput};
use dal_agent::ext::{
    BoxFuture, EventStream, ExtensionBuilder, Hook, HookCx, HookError, ModelCx, ModelError,
    ModelHandler, ModelRecord,
};
use dal_agent::{Agent, Delivery, Env, Host, Product, ServiceError, SessionRef, Subscription};
use dal_core::ext::BeforeTurn;
use dal_core::{
    AgentStart, AgentsOp, AgentsReply, ApprovalMode, CallId, Caps, ClientId, Command, CommandName,
    CommandSpec, Config, ConfigProduct, Expect, ModelId, ModelInfo, ModelRequest, Name, Origin,
    Output, Part, RawJson, Reply, Save, ServiceSet, SessionId, Stop, ThinkingLevel, ToolClass,
    ToolSpec, TurnState, UpdateKind, Visibility, Workspace,
};
use dal_provider::{ProviderError, StreamEvent, ToolArgs};

const PROBE_MODEL: &str = "kit/probe";

/// The synthetic model every child uses: it records the tool names each
/// request carries; each queued name makes one reply call that tool, and
/// every other reply is plain text.
struct Probe {
    requests: Mutex<Vec<Vec<String>>>,
    script: Mutex<VecDeque<&'static str>>,
}

impl Probe {
    fn requests(&self) -> Vec<Vec<String>> {
        self.requests.lock().expect("probe lock").clone()
    }

    fn script_left(&self) -> usize {
        self.script.lock().expect("probe lock").len()
    }

    fn queue(&self, tool: &'static str) {
        self.script.lock().expect("probe lock").push_back(tool);
    }
}

fn usage() -> dal_core::Usage {
    dal_core::Usage {
        input_tokens: 1,
        cached_input_tokens: 0,
        output_tokens: 1,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        cost_usd: None,
    }
}

fn stream(events: Vec<StreamEvent>) -> EventStream {
    let items: Vec<Result<StreamEvent, ProviderError>> = events.into_iter().map(Ok).collect();
    EventStream::new(futures::stream::iter(items), || {})
}

fn text_stream() -> EventStream {
    stream(vec![
        StreamEvent::TextDelta {
            text: "done".into(),
        },
        StreamEvent::ToolCallsDone { calls: Vec::new() },
        StreamEvent::Usage { usage: usage() },
        StreamEvent::Stop {
            reason: dal_provider::StopReason::EndTurn,
        },
    ])
}

fn call_stream(name: &str) -> EventStream {
    let args = if name == "report" {
        RawJson::parse(r#"{"status":"done","report":"reported"}"#).expect("report args")
    } else {
        RawJson::parse("{}").expect("args json")
    };
    stream(vec![
        StreamEvent::ToolCallStarted {
            id: "c1".into(),
            name: name.into(),
        },
        StreamEvent::ToolCallsDone {
            calls: vec![dal_provider::ToolCall {
                id: "c1".into(),
                name: name.into(),
                args: ToolArgs::Parsed(args),
            }],
        },
        StreamEvent::Usage { usage: usage() },
        StreamEvent::Stop {
            reason: dal_provider::StopReason::ToolUse,
        },
    ])
}

impl ModelHandler for Probe {
    fn run<'a>(
        &'a self,
        request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            let mut names: Vec<String> = request
                .tools
                .iter()
                .map(|tool| tool.name.to_string())
                .collect();
            names.sort();
            self.requests.lock().expect("probe lock").push(names);
            let step = self.script.lock().expect("probe lock").pop_front();
            Ok(match step {
                Some(name) => call_stream(name),
                None => text_stream(),
            })
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

/// A model-visible tool that counts how often it runs.
struct Counter {
    name: Name,
    spec: Arc<ToolSpec>,
    runs: Arc<AtomicUsize>,
}

impl Counter {
    fn new(name: &str, runs: Arc<AtomicUsize>) -> Arc<Self> {
        let name = Name::parse(name).expect("tool name");
        let spec = Arc::new(ToolSpec {
            name: name.clone(),
            description: "Counts its runs.".into(),
            parameters: RawJson::parse(r#"{"type":"object","properties":{}}"#)
                .expect("schema json"),
            grammar: None,
        });
        Arc::new(Self { name, spec, runs })
    }
}

impl Tool for Counter {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _workspace: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, _call: ToolCall, _cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            self.runs.fetch_add(1, Ordering::SeqCst);
            ToolOutcome::Ok(Box::new(ToolOutput::from_text("ran")))
        })
    }
}

/// `/spawn <tools>`: starts one probe-model child that may use the listed
/// tools (comma separated; `*` means no allowlist) and waits for its report.
struct Spawn {
    children: Arc<Mutex<Vec<SessionId>>>,
    next_name: Arc<AtomicUsize>,
    fixed_name: Option<Box<str>>,
}

impl CommandHandler for Spawn {
    fn run<'a>(
        &'a self,
        args: &'a str,
        cx: CommandCx<'a>,
    ) -> BoxFuture<'a, Result<Reply, ServiceError>> {
        Box::pin(async move {
            let tools = if args.trim() == "*" {
                None
            } else {
                let mut names = Vec::new();
                for part in args.split(',').filter(|part| !part.trim().is_empty()) {
                    names.push(
                        Name::parse(part.trim())
                            .map_err(|error| ServiceError::failed(None, error.to_string()))?,
                    );
                }
                Some(names.into_boxed_slice())
            };
            let name = self.fixed_name.clone().unwrap_or_else(|| {
                format!("member-{}", self.next_name.fetch_add(1, Ordering::SeqCst)).into()
            });
            let start = AgentStart {
                call: CallId::new("spawn-call"),
                name,
                prompt: "work".into(),
                model: Some(PROBE_MODEL.into()),
                role: None,
                system: None,
                tools,
                workspace: None,
            };
            let services = cx.services();
            let reply = services.agents(cx.caller(), AgentsOp::Start(start)).await?;
            let AgentsReply::Started { id } = reply else {
                return Err(ServiceError::failed(
                    None,
                    format!("the child did not start: {reply:?}"),
                ));
            };
            self.children.lock().expect("children lock").push(id);
            services
                .agents(cx.caller(), AgentsOp::Await { id, timeout: None })
                .await?;
            Ok(Reply::Done(Output::Nothing))
        })
    }
}

/// `/collect-report <child>`: awaits the child again and stores what the
/// parent's await reports, so a test can read the report text.
struct CollectReport {
    reports: Arc<Mutex<Vec<String>>>,
}

impl CommandHandler for CollectReport {
    fn run<'a>(
        &'a self,
        args: &'a str,
        cx: CommandCx<'a>,
    ) -> BoxFuture<'a, Result<Reply, ServiceError>> {
        Box::pin(async move {
            let id = SessionId::parse(args.trim())
                .map_err(|error| ServiceError::failed(None, error.to_string()))?;
            let reply = cx
                .services()
                .agents(cx.caller(), AgentsOp::Await { id, timeout: None })
                .await?;
            let AgentsReply::Await { report } = reply else {
                return Err(ServiceError::failed(
                    None,
                    format!("the child did not report: {reply:?}"),
                ));
            };
            self.reports
                .lock()
                .expect("reports lock")
                .push(report.text.to_string());
            Ok(Reply::Done(Output::Nothing))
        })
    }
}

/// `/prompt-child <child>`: gives an idle child one grace prompt.
struct PromptChild;

impl CommandHandler for PromptChild {
    fn run<'a>(
        &'a self,
        args: &'a str,
        cx: CommandCx<'a>,
    ) -> BoxFuture<'a, Result<Reply, ServiceError>> {
        Box::pin(async move {
            let mut words = args.split_whitespace();
            let id = SessionId::parse(words.next().unwrap_or_default())
                .map_err(|error| ServiceError::failed(None, error.to_string()))?;
            let max_steps = words.next().and_then(|steps| steps.parse().ok());
            let reply = cx
                .services()
                .agents(
                    cx.caller(),
                    AgentsOp::Prompt {
                        id,
                        text: "Call report with your final result.".into(),
                        interrupt: Some(Duration::from_millis(50)),
                        max_steps,
                    },
                )
                .await?;
            if !matches!(reply, AgentsReply::Prompted { .. }) {
                return Err(ServiceError::failed(
                    None,
                    format!("the child prompt was refused: {reply:?}"),
                ));
            }
            Ok(Reply::Done(Output::Nothing))
        })
    }
}

/// `/recompose`: publishes a plugin that adds the tool `gamma`, the same
/// publication `/reload` performs.
struct Recompose {
    gamma_runs: Arc<AtomicUsize>,
}

impl CommandHandler for Recompose {
    fn run<'a>(
        &'a self,
        _args: &'a str,
        cx: CommandCx<'a>,
    ) -> BoxFuture<'a, Result<Reply, ServiceError>> {
        Box::pin(async move {
            let plugin = ExtensionBuilder::new("late", "0.1.0", ServiceSet::EMPTY)
                .map_err(|error| ServiceError::failed(None, error.to_string()))?
                .with_origin(Origin::User, None)
                .tool(
                    Counter::new("gamma", Arc::clone(&self.gamma_runs)),
                    Visibility::Model,
                )
                .build()
                .map_err(|error| ServiceError::failed(None, error.to_string()))?;
            cx.publish_plugins(vec![plugin], Vec::new())
                .await
                .map_err(|error| ServiceError::failed(None, error.to_string()))?;
            Ok(Reply::Done(Output::Nothing))
        })
    }
}

/// Before every child turn, calls `beta` through the services, the way an
/// extension or a script calls a tool for its session. The tool's own run
/// counter shows whether the call got through.
struct NestedCaller;

impl Hook<BeforeTurn, Option<String>> for NestedCaller {
    fn call(
        &self,
        _input: BeforeTurn,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<Option<String>, HookError>> {
        Box::pin(async move {
            if cx.parent.is_some() {
                let args = RawJson::parse("{}").map_err(|error| HookError::Failed {
                    message: error.to_string().into(),
                })?;
                // A refused call is the expected outcome for a restricted child.
                let _ = cx
                    .services
                    .call_tool(&cx.caller, "beta", Box::new(args))
                    .await;
            }
            Ok(None)
        })
    }
}

struct Rig {
    _tmp: tempfile::TempDir,
    host: Host,
    root: Agent,
    workspace: PathBuf,
    data_root: PathBuf,
    config: Config,
    env: Env,
    extensions: Vec<dal_agent::ext::Extension>,
    probe: Arc<Probe>,
    children: Arc<Mutex<Vec<SessionId>>>,
    reports: Arc<Mutex<Vec<String>>>,
    beta_runs: Arc<AtomicUsize>,
    gamma_runs: Arc<AtomicUsize>,
}

fn command(name: &str) -> CommandSpec {
    CommandSpec {
        name: CommandName::parse(name).expect("command name"),
        summary: "Test command.".into(),
        args_hint: None,
    }
}

async fn rig() -> Rig {
    rig_with_extra_tools(&[]).await
}

async fn rig_with_deferred() -> Rig {
    rig_with_extra_tools(&["delta"]).await
}

/// Builds the `kit` extension the rig spawns children through: the
/// counter tools, the spawn commands, the nested-caller hook, and the
/// probe model. `extra` names deferred tools added before the build.
fn kit_extension(
    extra: &[&str],
    children: Arc<Mutex<Vec<SessionId>>>,
    next_name: Arc<AtomicUsize>,
    beta_runs: Arc<AtomicUsize>,
    gamma_runs: Arc<AtomicUsize>,
    reports: &Arc<Mutex<Vec<String>>>,
    probe: &Arc<Probe>,
) -> dal_agent::ext::Extension {
    let inject = ServiceSet::from_names(["agents"]).expect("inject set");
    let mut builder = ExtensionBuilder::new("kit", "0.1.0", inject)
        .expect("builder")
        .tool(
            Counter::new("report", Arc::new(AtomicUsize::new(0))),
            Visibility::Model,
        )
        .tool(
            Counter::new("alpha", Arc::new(AtomicUsize::new(0))),
            Visibility::Model,
        )
        .tool(Counter::new("beta", beta_runs), Visibility::Model)
        .command(
            command("spawn"),
            Arc::new(Spawn {
                children: Arc::clone(&children),
                next_name: Arc::clone(&next_name),
                fixed_name: None,
            }),
        )
        .command(
            command("spawn_same"),
            Arc::new(Spawn {
                children,
                next_name,
                fixed_name: Some("member".into()),
            }),
        )
        .command(
            command("collect-report"),
            Arc::new(CollectReport {
                reports: Arc::clone(reports),
            }),
        )
        .command(command("prompt-child"), Arc::new(PromptChild))
        .command(command("recompose"), Arc::new(Recompose { gamma_runs }))
        .on_before_turn(NestedCaller)
        .model(ModelRecord {
            id: ModelId::parse(PROBE_MODEL).expect("model id"),
            caps: caps(),
            handler: Arc::clone(probe) as Arc<dyn ModelHandler>,
            export: None,
        });
    for name in extra {
        builder = builder.tool(
            Counter::new(name, Arc::new(AtomicUsize::new(0))),
            Visibility::Deferred,
        );
    }
    builder.build().expect("extension")
}

async fn rig_with_extra_tools(extra: &[&str]) -> Rig {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace = tmp.path().join("w");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&workspace).expect("workspace dir");
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, "").expect("fixture");
    let user = format!(
        "model = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(user.as_str())).expect("config");
    let probe = Arc::new(Probe {
        requests: Mutex::new(Vec::new()),
        script: Mutex::new(VecDeque::new()),
    });
    let children = Arc::new(Mutex::new(Vec::new()));
    let next_name = Arc::new(AtomicUsize::new(0));
    let beta_runs = Arc::new(AtomicUsize::new(0));
    let gamma_runs = Arc::new(AtomicUsize::new(0));
    let reports = Arc::new(Mutex::new(Vec::new()));
    let extension = kit_extension(
        extra,
        Arc::clone(&children),
        next_name,
        Arc::clone(&beta_runs),
        gamma_runs.clone(),
        &reports,
        &probe,
    );
    let extensions = vec![extension];
    let product = Product {
        name: "dal",
        data_root: data.clone(),
        defaults: "",
        extensions: extensions.clone(),
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: workspace.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config.clone(), env.clone())
        .await
        .expect("host");
    let root = host
        .open(
            SessionRef::New {
                workspace: Workspace::new(workspace.clone()).expect("workspace"),
                name: None,
            },
            ClientId::new("probe"),
        )
        .await
        .expect("open root");
    Rig {
        _tmp: tmp,
        host,
        root,
        workspace,
        data_root: data,
        config,
        env,
        extensions,
        probe,
        children,
        reports,
        beta_runs,
        gamma_runs,
    }
}

impl Rig {
    /// Closes the child and the root, then builds a second Host over the
    /// same data root: the state a restart leaves behind.
    async fn restart(&mut self, child: SessionId) {
        self.host
            .close(child)
            .await
            .expect("close child before restart");
        let root_id = self
            .root
            .view(dal_core::PageReq::default())
            .expect("root view")
            .session
            .id;
        self.host
            .close(root_id)
            .await
            .expect("close root before restart");
        let product = Product {
            name: "dal",
            data_root: self.data_root.clone(),
            defaults: "",
            extensions: self.extensions.clone(),
            bundled: Vec::new(),
        };
        let replacement = Host::start(product, self.config.clone(), self.env.clone())
            .await
            .expect("restart host");
        let old = std::mem::replace(&mut self.host, replacement);
        drop(old);
    }

    async fn run(&self, name: &str, args: &str) {
        tokio::time::timeout(
            Duration::from_secs(10),
            self.root.submit(Command::Run {
                name: name.into(),
                args: args.into(),
                expected: None,
            }),
        )
        .await
        .unwrap_or_else(|_| panic!("/{name} did not answer"))
        .unwrap_or_else(|error| panic!("/{name} failed: {error}"));
    }

    fn child(&self, index: usize) -> SessionId {
        self.children.lock().expect("children lock")[index]
    }

    /// Waits until the probe model has served `requests` requests in total
    /// and the child's turn has ended.
    async fn settled(&self, id: SessionId, requests: usize) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while self.probe.requests().len() < requests {
                tokio::task::yield_now().await;
            }
            while !matches!(self.child_view(id).await.turn, TurnState::Idle) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the child did not settle after {requests} requests"));
    }

    /// Sends the child one more prompt and waits for its turn to end.
    async fn prompt_child(&self, id: SessionId) {
        let agent = self
            .host
            .open(
                SessionRef::Resume {
                    key: id.to_string().into(),
                    workspace: Workspace::new(self.workspace.clone()).expect("workspace"),
                },
                ClientId::new("probe"),
            )
            .await
            .expect("resume child");
        let mut subscription = agent.subscribe(None).expect("subscribe");
        agent
            .submit(Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text {
                    text: "again".into(),
                }],
            })
            .await
            .expect("prompt accepted");
        loop {
            let delivery = tokio::time::timeout(Duration::from_secs(10), subscription.next())
                .await
                .expect("the child turn ended")
                .expect("stream open");
            if let Delivery::Update(update) = delivery
                && matches!(update.kind, UpdateKind::TurnEnded { .. })
            {
                return;
            }
        }
    }

    async fn child_updates(&self, id: SessionId) -> Subscription {
        let agent = self
            .host
            .open(
                SessionRef::Resume {
                    key: id.to_string().into(),
                    workspace: Workspace::new(self.workspace.clone()).expect("workspace"),
                },
                ClientId::new("probe"),
            )
            .await
            .expect("resume child");
        agent.subscribe_listen(None).expect("subscribe")
    }

    async fn child_view(&self, id: SessionId) -> dal_core::View {
        let agent = self
            .host
            .open(
                SessionRef::Resume {
                    key: id.to_string().into(),
                    workspace: Workspace::new(self.workspace.clone()).expect("workspace"),
                },
                ClientId::new("probe"),
            )
            .await
            .expect("resume child");
        agent.view(dal_core::PageReq::default()).expect("view")
    }
}

/// The stop reason of the session's turn number `number`, read from the
/// stream; earlier turns the stream replays are skipped.
async fn turn_stop(updates: &mut Subscription, number: u64) -> Stop {
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(delivery) = updates.next().await {
            let Delivery::Update(update) = delivery else {
                continue;
            };
            if let UpdateKind::TurnEnded { turn, stop } = &update.kind
                && turn.get() == number
            {
                return *stop;
            }
        }
        panic!("the update stream closed before turn {number} ended");
    })
    .await
    .unwrap_or_else(|_| panic!("turn {number} did not end"))
}

#[tokio::test]
async fn a_childs_nested_tool_calls_meet_its_allowlist() {
    let rig = rig().await;

    // Control: an unrestricted child's hook reaches `beta`.
    rig.run("spawn", "*").await;
    rig.settled(rig.child(0), 1).await;
    assert_eq!(rig.beta_runs.load(Ordering::SeqCst), 1);

    rig.run("spawn", "alpha").await;
    rig.settled(rig.child(1), 2).await;
    assert_eq!(
        rig.beta_runs.load(Ordering::SeqCst),
        1,
        "a restricted child's hook cannot reach `beta`"
    );
}

#[tokio::test]
async fn a_childs_tool_list_survives_an_extension_reload() {
    let rig = rig().await;

    // Control: a child without an allowlist sees every tool, so the reload
    // below really adds `gamma` to what a session may see.
    rig.run("spawn", "*").await;
    rig.settled(rig.child(0), 1).await;
    assert_eq!(rig.probe.requests()[0], ["alpha", "beta", "report"]);

    rig.run("spawn", "alpha").await;
    rig.settled(rig.child(1), 2).await;
    assert_eq!(
        rig.probe.requests()[1],
        ["alpha"],
        "the child sees only the tool it was given"
    );

    let before = rig.host.generation();
    rig.run("recompose", "").await;
    assert_ne!(rig.host.generation(), before, "the reload published");

    rig.prompt_child(rig.child(1)).await;
    assert_eq!(
        rig.probe.requests()[2],
        ["alpha"],
        "the allowlist holds after the reload added `gamma`"
    );

    rig.run("spawn", "*").await;
    rig.settled(rig.child(2), 4).await;
    assert_eq!(
        rig.probe.requests()[3],
        ["alpha", "beta", "gamma", "report"],
        "an unrestricted child sees the recomposed tools"
    );
}

#[tokio::test]
async fn a_duplicate_child_name_reports_the_host_error() {
    let rig = rig().await;
    rig.run("spawn_same", "*").await;
    let error = tokio::time::timeout(
        Duration::from_secs(10),
        rig.root.submit(Command::Run {
            name: "spawn_same".into(),
            args: "*".into(),
            expected: None,
        }),
    )
    .await
    .expect("duplicate spawn answered")
    .expect_err("duplicate child names must fail");
    let text = error.to_string();
    assert!(
        text.contains("already used") && text.contains("member"),
        "duplicate child name should explain the conflict: {text}"
    );
}
#[tokio::test]
async fn a_child_cannot_call_a_tool_outside_its_allowlist() {
    let rig = rig().await;
    rig.run("recompose", "").await;
    rig.probe.queue("beta");
    rig.run("spawn", "alpha").await;
    rig.settled(rig.child(0), 2).await;
    rig.probe.queue("gamma");
    rig.prompt_child(rig.child(0)).await;

    assert_eq!(rig.beta_runs.load(Ordering::SeqCst), 0, "beta never ran");
    assert_eq!(rig.gamma_runs.load(Ordering::SeqCst), 0, "gamma never ran");
    let dump = format!("{:?}", rig.child_view(rig.child(0)).await);
    assert!(
        dump.contains("unknown tool: beta") && dump.contains("unknown tool: gamma"),
        "both calls failed as unknown tools\n{dump}"
    );
}

#[tokio::test]
async fn an_empty_allowlist_gives_a_child_no_tools() {
    let rig = rig().await;
    rig.run("spawn", "").await;
    rig.settled(rig.child(0), 1).await;
    assert!(
        rig.probe.requests()[0].is_empty(),
        "an explicit empty list means no tools: {:?}",
        rig.probe.requests()[0]
    );
}

#[tokio::test]
async fn a_child_starts_with_its_parents_approval_mode() {
    let rig = rig().await;
    let parent = rig.root.view(dal_core::PageReq::default()).expect("view");
    assert_eq!(parent.settings.approval, ApprovalMode::Ask);
    rig.root
        .submit(Command::SetApproval {
            mode: ApprovalMode::All,
            save: Save::SessionOnly,
        })
        .await
        .expect("parent approval set");
    rig.run("spawn", "alpha").await;
    rig.settled(rig.child(0), 1).await;
    let child = rig.child_view(rig.child(0)).await;
    assert_eq!(
        child.settings.approval,
        ApprovalMode::All,
        "the child keeps the approval the user gave its parent"
    );
}

#[tokio::test]
async fn parent_can_prompt_an_idle_child_once() {
    let rig = rig().await;
    rig.run("spawn", "*").await;
    let child = rig.child(0);
    rig.settled(child, 1).await;
    rig.run("prompt-child", &child.to_string()).await;
    rig.settled(child, 2).await;
    assert_eq!(
        rig.probe.requests().len(),
        2,
        "the prompted child receives a second real turn"
    );
}

#[tokio::test]
async fn a_bounded_prompt_stops_the_prompted_turn_after_one_step() {
    let rig = rig().await;
    rig.run("spawn", "*").await;
    let child = rig.child(0);
    rig.settled(child, 1).await;
    let mut updates = rig.child_updates(child).await;
    rig.probe.queue("alpha");
    rig.probe.queue("alpha");
    rig.run("prompt-child", &format!("{child} 1")).await;
    assert_eq!(
        turn_stop(&mut updates, 2).await,
        Stop::MaxSteps,
        "the bound, not the model, ended the prompted turn"
    );
    assert_eq!(
        rig.probe.requests().len(),
        2,
        "one tool round ran, then the bound ended the turn"
    );
    assert_eq!(rig.probe.script_left(), 1, "the second step never ran");
}

#[tokio::test]
async fn an_unbounded_prompt_runs_every_step_and_the_bound_does_not_carry_over() {
    let rig = rig().await;
    rig.run("spawn", "*").await;
    let child = rig.child(0);
    rig.settled(child, 1).await;
    rig.probe.queue("alpha");
    rig.probe.queue("alpha");
    rig.run("prompt-child", &child.to_string()).await;
    rig.settled(child, 4).await;
    assert_eq!(
        rig.probe.requests().len(),
        4,
        "two tool rounds and a closing reply ran without a bound"
    );
}

#[tokio::test]
async fn parent_await_reads_report_tool_without_grace() {
    let rig = rig().await;
    rig.probe.queue("report");
    rig.run("spawn", "report").await;
    let child = rig.child(0);
    rig.settled(child, 1).await;
    rig.run("collect-report", &child.to_string()).await;
    let dump = rig.child_view(child).await;
    assert_eq!(
        rig.reports.lock().expect("reports lock").as_slice(),
        ["reported"],
        "the report tool body reaches the parent await\n{dump:?}"
    );
    assert_eq!(
        rig.probe.requests().len(),
        2,
        "the report call and its tool result complete one child turn"
    );
}

#[tokio::test]
async fn a_reopened_child_keeps_its_tool_allowlist() {
    let mut rig = rig().await;
    rig.root
        .submit(Command::SetApproval {
            mode: ApprovalMode::All,
            save: Save::SessionOnly,
        })
        .await
        .expect("parent approval set");
    rig.run("spawn", "alpha").await;
    let child = rig.child(0);
    rig.settled(child, 1).await;
    rig.restart(child).await;
    rig.prompt_child(child).await;
    assert_eq!(
        rig.probe.requests()[1],
        ["alpha"],
        "the allowlist remains after reopening the child"
    );
    assert_eq!(
        rig.child_view(child).await.settings.approval,
        ApprovalMode::All,
        "the inherited approval mode remains after reopening the child"
    );
}

#[tokio::test]
async fn a_reopened_child_filters_deferred_tools() {
    let mut rig = rig_with_deferred().await;
    rig.run("spawn", "alpha").await;
    let child = rig.child(0);
    rig.settled(child, 1).await;
    rig.restart(child).await;
    rig.prompt_child(child).await;
    assert_eq!(
        rig.probe.requests()[1],
        ["alpha"],
        "replay keeps deferred tools and tool_search outside the allowlist"
    );
}

#[tokio::test]
async fn a_tool_result_records_how_long_its_tool_ran() {
    let rig = rig().await;
    rig.probe.queue("alpha");
    rig.probe.queue("ghost");
    rig.run("spawn", "alpha").await;
    rig.settled(rig.child(0), 3).await;

    let view = rig.child_view(rig.child(0)).await;
    let results: Vec<_> = view
        .entries
        .items
        .iter()
        .filter_map(|entry| match &entry.kind {
            dal_core::EntryKind::ToolResult {
                name, elapsed_ms, ..
            } => Some((name.to_string(), *elapsed_ms)),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 2, "{results:?}");
    assert_eq!(results[0].0, "alpha");
    assert!(results[0].1.is_some(), "a tool that ran has a duration");
    assert_eq!(results[1].0, "ghost");
    assert_eq!(results[1].1, None, "a call that never ran has none");
}
