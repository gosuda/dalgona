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
use dal_agent::{Agent, Delivery, Env, Host, Product, ServiceError, SessionRef};
use dal_core::ext::BeforeTurn;
use dal_core::{
    AgentStart, AgentsOp, AgentsReply, ApprovalMode, CallId, Caps, ClientId, Command, CommandName,
    CommandSpec, Config, ConfigProduct, Expect, ModelId, ModelInfo, ModelRequest, Name, Origin,
    Output, Part, RawJson, Reply, Save, ServiceSet, SessionId, ThinkingLevel, ToolClass, ToolSpec,
    TurnState, UpdateKind, Visibility, Workspace,
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
    stream(vec![
        StreamEvent::ToolCallStarted {
            id: "c1".into(),
            name: name.into(),
        },
        StreamEvent::ToolCallsDone {
            calls: vec![dal_provider::ToolCall {
                id: "c1".into(),
                name: name.into(),
                args: ToolArgs::Parsed(RawJson::parse("{}").expect("args json")),
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
            ToolOutcome::Ok(ToolOutput::from_text("ran"))
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
    probe: Arc<Probe>,
    children: Arc<Mutex<Vec<SessionId>>>,
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
    let alpha_runs = Arc::new(AtomicUsize::new(0));
    let beta_runs = Arc::new(AtomicUsize::new(0));
    let gamma_runs = Arc::new(AtomicUsize::new(0));
    let inject = ServiceSet::from_names(["agents"]).expect("inject set");
    let extension = ExtensionBuilder::new("kit", "0.1.0", inject)
        .expect("builder")
        .tool(Counter::new("alpha", alpha_runs), Visibility::Model)
        .tool(
            Counter::new("beta", Arc::clone(&beta_runs)),
            Visibility::Model,
        )
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
                children: Arc::clone(&children),
                next_name: Arc::clone(&next_name),
                fixed_name: Some("member".into()),
            }),
        )
        .command(
            command("recompose"),
            Arc::new(Recompose {
                gamma_runs: Arc::clone(&gamma_runs),
            }),
        )
        .on_before_turn(NestedCaller)
        .model(ModelRecord {
            id: ModelId::parse(PROBE_MODEL).expect("model id"),
            caps: caps(),
            handler: Arc::clone(&probe) as Arc<dyn ModelHandler>,
            export: None,
        })
        .build()
        .expect("extension");
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: workspace.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await.expect("host");
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
        probe,
        children,
        beta_runs,
        gamma_runs,
    }
}

impl Rig {
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
    assert_eq!(rig.probe.requests()[0], ["alpha", "beta"]);

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
        ["alpha", "beta", "gamma"],
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
