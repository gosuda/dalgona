#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "fixtures fail loudly at their setup boundary"
)]
//! A child start carries its explicit system prompt or role preset through
//! the real `ModelRequest.system` channel: the product base is replaced or
//! extended, the user prompt bytes are unchanged, extension prompt sections
//! keep rendering, and the policy survives later turns and a reopen. Real
//! host admission, journal persistence, and replay run unmodified; the
//! probe inspects the exact bytes the provider boundary receives.
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::ext::command::{CommandCx, CommandHandler};
use dal_agent::ext::{
    BoxFuture, EventStream, ExtensionBuilder, ModelCx, ModelError, ModelHandler, ModelRecord,
    PromptOrder, PromptSection,
};
use dal_agent::{Env, Host, Product, ServiceError, SessionRef, Subscription};
use dal_core::{
    AgentStart, AgentsOp, AgentsReply, CallId, ClientId, Command, CommandName, CommandSpec, Config,
    ConfigProduct, ContextItem, Expect, ModelId, Output, Part, Reply, ServiceSet, SessionId,
    ThinkingLevel, TurnState, UpdateKind, Workspace,
};
use dal_provider::{ProviderError, StreamEvent};

use dal_agent::Delivery;

const PROBE_MODEL: &str = "kit/probe";
const EXTENSION_SECTION: &str = "PROBE-EXT-SECTION-SENTINEL";
const SYSTEM_TEXT: &str = "You are an expert auditor.";
const ROLE_TEXT: &str = "Role: Reviewer.";
const WAIT: Duration = Duration::from_secs(10);

/// One provider request reduced to the bytes this suite asserts on.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Captured {
    system: String,
    user: Option<String>,
}

/// The synthetic model every child uses: it records the system prompt and
/// the first user text of each request, then replies with plain text.
struct Probe {
    requests: Mutex<Vec<Captured>>,
}

impl Probe {
    fn requests(&self) -> Vec<Captured> {
        self.requests.lock().expect("probe lock").clone()
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
        premium_requests: None,
    }
}

fn text_stream() -> EventStream {
    let items: Vec<Result<StreamEvent, ProviderError>> = vec![
        Ok(StreamEvent::TextDelta {
            text: "done".into(),
        }),
        Ok(StreamEvent::ToolCallsDone { calls: Vec::new() }),
        Ok(StreamEvent::Usage { usage: usage() }),
        Ok(StreamEvent::Stop {
            reason: dal_provider::StopReason::EndTurn,
        }),
    ];
    EventStream::new(futures::stream::iter(items), || {})
}

impl ModelHandler for Probe {
    fn run<'a>(
        &'a self,
        request: dal_core::ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            let user = request.context.iter().find_map(|item| match item {
                ContextItem::User { parts } => parts.iter().find_map(|part| match part {
                    Part::Text { text } => Some(text.to_string()),
                    _ => None,
                }),
                _ => None,
            });
            self.requests.lock().expect("probe lock").push(Captured {
                system: request.system.to_string(),
                user,
            });
            Ok(text_stream())
        })
    }
}

fn caps() -> dal_core::Caps {
    dal_core::Caps {
        context_window: Some(100_000),
        thinking: Box::new([ThinkingLevel::Off]),
        tool_use: true,
        image_input: false,
        custom_grammar: false,
    }
}

/// Which start policy one spawn command issues.
#[derive(Clone, Copy)]
enum SpawnKind {
    /// No prompt customization: the control.
    Plain,
    /// An explicit system prompt.
    System,
    /// A role preset.
    Role,
    /// Both fields set: the start must refuse.
    Both,
}

/// `/spawn-*`: starts one probe-model child with the fixed policy and
/// waits for its terminal report.
struct Spawn {
    kind: SpawnKind,
    name: &'static str,
    children: Arc<Mutex<Vec<SessionId>>>,
    errors: Arc<Mutex<Vec<String>>>,
}

impl CommandHandler for Spawn {
    fn run<'a>(
        &'a self,
        _args: &'a str,
        cx: CommandCx<'a>,
    ) -> BoxFuture<'a, Result<Reply, ServiceError>> {
        Box::pin(async move {
            let (role, system, prompt) = match self.kind {
                SpawnKind::Plain => (None, None, "work"),
                SpawnKind::System => (None, Some(SYSTEM_TEXT), "audit this"),
                SpawnKind::Role => (Some(ROLE_TEXT), None, "review this"),
                SpawnKind::Both => (Some("r"), Some("s"), "work"),
            };
            let start = AgentStart {
                call: CallId::new("spawn-call"),
                name: Some(self.name.into()),
                prompt: prompt.into(),
                model: Some(PROBE_MODEL.into()),
                role: role.map(Into::into),
                system: system.map(Into::into),
                tools: None,
                workspace: None,
            };
            let services = cx.services();
            match services.agents(cx.caller(), AgentsOp::Start(start)).await {
                Ok(AgentsReply::Started { id }) => {
                    self.children.lock().expect("children lock").push(id);
                    services
                        .agents(cx.caller(), AgentsOp::Await { id, timeout: None })
                        .await?;
                    Ok(Reply::Done(Output::Nothing))
                }
                Ok(other) => Err(ServiceError::failed(
                    None,
                    format!("the child did not start: {other:?}"),
                )),
                Err(error) => {
                    self.errors
                        .lock()
                        .expect("errors lock")
                        .push(error.to_string());
                    Err(error)
                }
            }
        })
    }
}

/// An extension whose static model-visible prompt section must reach a
/// child under every prompt policy.
fn sentinel_extension() -> dal_agent::ext::Extension {
    ExtensionBuilder::new("probeext", "0.1.0", ServiceSet::EMPTY)
        .expect("builder")
        .prompt_section(PromptSection::static_text(
            PromptOrder::Plugins,
            EXTENSION_SECTION.into(),
        ))
        .build()
        .expect("sentinel extension")
}

fn command(name: &str) -> CommandSpec {
    CommandSpec {
        name: CommandName::parse(name).expect("command name"),
        summary: "Test command.".into(),
        args_hint: None,
    }
}

struct Rig {
    _tmp: tempfile::TempDir,
    host: Host,
    root: dal_agent::Agent,
    workspace: PathBuf,
    data_root: PathBuf,
    config: Config,
    env: Env,
    extensions: Vec<dal_agent::ext::Extension>,
    probe: Arc<Probe>,
    children: Arc<Mutex<Vec<SessionId>>>,
    errors: Arc<Mutex<Vec<String>>>,
}

/// A rig without the sentinel prompt section.
async fn rig() -> Rig {
    rig_with_fixture_section(false).await
}

/// A rig whose second extension contributes a static model-visible prompt
/// section at the plugins order.
async fn rig_with_extension_section() -> Rig {
    rig_with_fixture_section(true).await
}

async fn rig_with_fixture_section(with_section: bool) -> Rig {
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
    });
    let children = Arc::new(Mutex::new(Vec::new()));
    let errors = Arc::new(Mutex::new(Vec::new()));
    let inject = ServiceSet::from_names(["agents"]).expect("inject set");
    let mut builder = ExtensionBuilder::new("kit", "0.1.0", inject).expect("builder");
    for (name, kind) in [
        ("spawn_plain", SpawnKind::Plain),
        ("spawn_system", SpawnKind::System),
        ("spawn_role", SpawnKind::Role),
        ("spawn_both", SpawnKind::Both),
    ] {
        builder = builder.command(
            command(name),
            Arc::new(Spawn {
                kind,
                name,
                children: Arc::clone(&children),
                errors: Arc::clone(&errors),
            }),
        );
    }
    let kit = builder
        .model(ModelRecord {
            id: ModelId::parse(PROBE_MODEL).expect("model id"),
            caps: caps(),
            handler: Arc::clone(&probe) as Arc<dyn ModelHandler>,
            export: None,
        })
        .build()
        .expect("kit extension");
    let mut extensions = vec![kit];
    if with_section {
        extensions.push(sentinel_extension());
    }
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
            ClientId::new("child-system"),
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
        errors,
    }
}

impl Rig {
    async fn run(&self, name: &str) {
        tokio::time::timeout(
            WAIT,
            self.root.submit(Command::Run {
                name: name.into(),
                args: "".into(),
                expected: None,
            }),
        )
        .await
        .unwrap_or_else(|_| panic!("/{name} did not answer"))
        .unwrap_or_else(|error| panic!("/{name} failed: {error}"));
    }

    fn child(&self) -> SessionId {
        self.children.lock().expect("children lock")[0]
    }

    /// Waits until the probe has served `requests` requests and the child
    /// sits idle again.
    async fn settled(&self, id: SessionId, requests: usize) {
        tokio::time::timeout(WAIT, async {
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
                ClientId::new("child-system"),
            )
            .await
            .expect("resume child");
        let mut subscription: Subscription = agent.subscribe(None).expect("subscribe");
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
            let delivery = tokio::time::timeout(WAIT, subscription.next())
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
                ClientId::new("child-system"),
            )
            .await
            .expect("resume child");
        agent.view(dal_core::PageReq::default()).expect("view")
    }

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
}

#[tokio::test]
async fn an_explicit_system_prompt_replaces_the_base_and_keeps_extension_sections() {
    let rig = rig_with_extension_section().await;
    rig.run("spawn_system").await;
    let child = rig.child();
    rig.settled(child, 1).await;
    let captured = &rig.probe.requests()[0];
    assert_eq!(
        captured.system,
        format!("{SYSTEM_TEXT}\n\n{EXTENSION_SECTION}"),
        "the explicit system takes the product base's place; the extension section still renders"
    );
    assert_eq!(
        captured.user.as_deref(),
        Some("audit this"),
        "the user prompt carries its own bytes, with no System prefix"
    );
}

#[tokio::test]
async fn a_role_appends_after_the_base_so_the_prefix_stays_shared() {
    let rig = rig_with_extension_section().await;
    rig.run("spawn_role").await;
    let child = rig.child();
    rig.settled(child, 1).await;
    let captured = &rig.probe.requests()[0];
    assert_eq!(
        captured.system,
        format!("{EXTENSION_SECTION}\n\n{ROLE_TEXT}"),
        "the role lands after the assembled base, at the end of the system prompt"
    );
    assert_eq!(captured.user.as_deref(), Some("review this"));
}

#[tokio::test]
async fn a_plain_child_keeps_the_default_system_and_its_own_prompt() {
    let rig = rig().await;
    rig.run("spawn_plain").await;
    let child = rig.child();
    rig.settled(child, 1).await;
    let captured = &rig.probe.requests()[0];
    assert!(
        captured.system.is_empty(),
        "no policy means the default assembly, which is empty here: {:?}",
        captured.system
    );
    assert_eq!(
        captured.user.as_deref(),
        Some("work"),
        "the task text must reach the model byte for byte"
    );
}

#[tokio::test]
async fn the_system_policy_holds_on_a_later_turn() {
    let rig = rig().await;
    rig.run("spawn_system").await;
    let child = rig.child();
    rig.settled(child, 1).await;
    rig.prompt_child(child).await;
    rig.settled(child, 2).await;
    assert_eq!(
        rig.probe.requests()[1].system,
        SYSTEM_TEXT,
        "the second turn runs under the same explicit system prompt"
    );
}

#[tokio::test]
async fn the_role_policy_holds_on_a_later_turn() {
    let rig = rig().await;
    rig.run("spawn_role").await;
    let child = rig.child();
    rig.settled(child, 1).await;
    rig.prompt_child(child).await;
    rig.settled(child, 2).await;
    assert_eq!(
        rig.probe.requests()[1].system,
        ROLE_TEXT,
        "the second turn runs under the same role preset"
    );
}

#[tokio::test]
async fn the_prompt_policy_survives_a_reopen() {
    let mut rig = rig_with_extension_section().await;
    rig.run("spawn_system").await;
    let child = rig.child();
    rig.settled(child, 1).await;
    rig.restart(child).await;
    rig.prompt_child(child).await;
    rig.settled(child, 2).await;
    assert_eq!(
        rig.probe.requests()[1].system,
        format!("{SYSTEM_TEXT}\n\n{EXTENSION_SECTION}"),
        "the replayed child policy restores the system prompt after a reopen"
    );
}

#[tokio::test]
async fn the_role_policy_survives_a_reopen() {
    let mut rig = rig_with_extension_section().await;
    rig.run("spawn_role").await;
    let child = rig.child();
    rig.settled(child, 1).await;
    rig.restart(child).await;
    rig.prompt_child(child).await;
    rig.settled(child, 2).await;
    assert_eq!(
        rig.probe.requests()[1].system,
        format!("{EXTENSION_SECTION}\n\n{ROLE_TEXT}"),
        "the replayed child policy restores the role preset after a reopen"
    );
}

#[tokio::test]
async fn a_start_cannot_combine_role_and_system() {
    let rig = rig().await;
    let error = tokio::time::timeout(
        WAIT,
        rig.root.submit(Command::Run {
            name: "spawn_both".into(),
            args: "".into(),
            expected: None,
        }),
    )
    .await
    .expect("the combined start answered")
    .expect_err("a combined role and system start must fail");
    let text = error.to_string();
    assert!(
        text.contains("role and system cannot both be set"),
        "the refusal names the exclusivity rule: {text}"
    );
    assert!(
        rig.errors
            .lock()
            .expect("errors lock")
            .iter()
            .any(|error| error.contains("role and system cannot both be set")),
        "the typed error reaches the calling tool: {:?}",
        rig.errors
    );
    assert!(
        rig.children.lock().expect("children lock").is_empty(),
        "a refused start leaves no child session behind"
    );
}
