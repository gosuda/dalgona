#![expect(
    clippy::expect_used,
    reason = "integration fixture failures must fail at their specific setup boundary"
)]
//! `agents.start` refusals reach the caller as typed service errors, and a
//! start that the host accepts reports `Started` with the child's id.
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::ext::{
    BoxFuture, Extension, ExtensionBuilder, RawValue, Tool, ToolCall, ToolCx, ToolOutcome,
    ToolOutput,
};
use dal_agent::{Delivery, Env, Host, Product, SessionRef, Subscription};
use dal_core::{
    AgentStart, AgentsOp, AgentsReply, CallId, ClientId, Command, Config, ConfigProduct, Expect,
    ModelInfo, Name, Part, RawJson, ServiceSet, ToolClass, ToolSpec, UpdateKind, Workspace,
};

const USAGE: &str = "{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}";

fn turn_script(tool: &str) -> String {
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"tool_call_started\",\"id\":\"c-{tool}\",\"name\":\"{tool}\"}},{{\"type\":\"tool_calls_done\",\"calls\":[{{\"id\":\"c-{tool}\",\"name\":\"{tool}\",\"args\":{{\"kind\":\"parsed\",\"value\":{{}}}}}}]}},{USAGE},{{\"type\":\"stop\",\"reason\":\"tool_use\"}}]}}\n{{\"kind\":\"events\",\"events\":[{{\"type\":\"text_delta\",\"text\":\"done\"}},{{\"type\":\"tool_calls_done\",\"calls\":[]}},{USAGE},{{\"type\":\"stop\",\"reason\":\"end_turn\"}}]}}\n"
    )
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

fn start_args(name: &str, workspace: Option<&Workspace>) -> AgentStart {
    AgentStart {
        call: CallId::new("c-spawn"),
        name: name.into(),
        prompt: "say done".into(),
        model: None,
        role: None,
        system: None,
        tools: None,
        workspace: workspace.cloned(),
    }
}

#[derive(Debug)]
struct Outcome {
    escaped: Option<String>,
    id_named: Option<String>,
    started: Option<String>,
}

struct Spawner {
    name: Name,
    spec: Arc<ToolSpec>,
    escape_root: Workspace,
    seen: Arc<Mutex<Outcome>>,
}

impl Tool for Spawner {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(
        &self,
        _args: &RawValue,
        _workspace: &Workspace,
    ) -> Result<ToolClass, dal_agent::ext::ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, _call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let services = cx.services();
            let caller = cx.caller();
            let mut seen = Outcome {
                escaped: None,
                id_named: None,
                started: None,
            };
            // A workspace outside the session root must be denied, not
            // reported as a cancelled start.
            let escaped = services
                .agents(
                    caller,
                    AgentsOp::Start(start_args("escape", Some(&self.escape_root))),
                )
                .await;
            seen.escaped = Some(format!("{escaped:?}"));
            // A name made only of session-id characters is refused by the
            // store; the refusal must name the rule, not collapse into a
            // cancellation.
            let id_named = services
                .agents(caller, AgentsOp::Start(start_args("a", None)))
                .await;
            seen.id_named = Some(format!("{id_named:?}"));
            // A valid start still reports Started, and the child finishes
            // the scripted turn.
            let started = services
                .agents(caller, AgentsOp::Start(start_args("worker", None)))
                .await;
            let record = match started {
                Ok(AgentsReply::Started { id }) => {
                    let awaited = services
                        .agents(
                            caller,
                            AgentsOp::Await {
                                id,
                                timeout: Some(Duration::from_secs(20)),
                            },
                        )
                        .await;
                    format!("started {id:?} await {awaited:?}")
                }
                other => format!("{other:?}"),
            };
            seen.started = Some(record);
            *self
                .seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = seen;
            ToolOutcome::Ok(Box::new(ToolOutput::from_text("done")))
        })
    }
}

fn spawner_extension(escape_root: Workspace, seen: Arc<Mutex<Outcome>>) -> Extension {
    let name = Name::parse("spawn").expect("tool name");
    let inject = ServiceSet::from_names(["agents"]).expect("agents service");
    ExtensionBuilder::new("spawner", "0.1.0", inject)
        .expect("builder")
        .tool(
            Arc::new(Spawner {
                name: name.clone(),
                spec: Arc::new(ToolSpec {
                    name,
                    description: "fixture tool".into(),
                    parameters: RawJson::parse(r#"{"type":"object"}"#).expect("schema"),
                    grammar: None,
                }),
                escape_root,
                seen,
            }),
            dal_core::Visibility::Model,
        )
        .build()
        .expect("spawner extension")
}

const WAIT: Duration = Duration::from_secs(30);

async fn turn_ended(subscription: &mut Subscription) {
    // A timed-out drain leaves the assertions to fail on the recorded
    // outcome.
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

#[tokio::test]
async fn a_refused_child_start_is_a_typed_error_not_a_cancellation() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("workspace");
    let escape_dir = tmp.path().join("escape");
    std::fs::create_dir_all(&data).expect("data directory");
    std::fs::create_dir_all(&workspace_dir).expect("workspace directory");
    std::fs::create_dir_all(&escape_dir).expect("escape directory");
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, turn_script("spawn")).expect("script fixture");
    let config_text = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(config_text.as_str())).expect("config");
    let seen = Arc::new(Mutex::new(Outcome {
        escaped: None,
        id_named: None,
        started: None,
    }));
    let escape_root = Workspace::new(escape_dir).expect("escape workspace");
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions: vec![spawner_extension(escape_root, Arc::clone(&seen))],
        bundled: Vec::new(),
    };
    let env = fixture_env(tmp.path(), workspace_dir.clone());
    let host = Host::start(product, config, env).await.expect("host");
    let workspace = Workspace::new(workspace_dir).expect("workspace");
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("agents-test"),
        )
        .await
        .expect("session");
    let mut subscription = agent.subscribe(None).expect("subscription");
    tokio::time::timeout(WAIT, async {
        loop {
            match agent
                .submit(Command::Prompt {
                    expect: Expect::Idle,
                    content: vec![Part::Text {
                        text: "spawn children".into(),
                    }],
                })
                .await
            {
                Ok(_) => break,
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    })
    .await
    .expect("the session accepted the prompt");
    turn_ended(&mut subscription).await;
    let outcome = seen
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let escaped = outcome.escaped.as_deref().expect("the escape case ran");
    assert!(
        escaped.starts_with("Ok(Refused") && escaped.contains("WorkspaceOutsideRoot"),
        "an out-of-scope workspace is refused with its reason: {escaped}"
    );
    let id_named = outcome.id_named.as_deref().expect("the id-name case ran");
    assert!(
        id_named.starts_with("Err(Failed"),
        "an id-shaped name fails with the store rule: {id_named}"
    );
    assert!(
        id_named.contains("1 to 64 characters"),
        "the refusal keeps the store's reason: {id_named}"
    );
    let started = outcome.started.as_deref().expect("the start case ran");
    assert!(
        started.starts_with("started "),
        "a valid child reports Started: {started}"
    );
    assert!(
        started.contains("Await {"),
        "the await resolves to a completed report, not Pending or Cancelled: {started}"
    );
    assert!(
        started.contains("stop: EndTurn"),
        "the report carries a terminal stop kind: {started}"
    );
}
