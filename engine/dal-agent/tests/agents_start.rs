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
use dal_agent::{Delivery, Env, Host, Product, ServiceError, SessionRef, Subscription};
use dal_core::{
    AgentRefusal, AgentStart, AgentsOp, AgentsReply, CallId, ClientId, Command, Config,
    ConfigProduct, Expect, ModelInfo, Name, Part, RawJson, Service, ServiceSet, SessionId, Stop,
    ToolClass, ToolSpec, UpdateKind, Workspace,
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
        name: Some(name.into()),
        prompt: "say done".into(),
        model: None,
        role: None,
        system: None,
        tools: None,
        workspace: workspace.cloned(),
    }
}

type StartReply = Result<AgentsReply, ServiceError>;

#[derive(Debug)]
struct Outcome {
    escaped: Option<StartReply>,
    id_named: Option<StartReply>,
    started: Option<StartReply>,
    awaited: Option<(SessionId, StartReply)>,
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
                awaited: None,
            };
            // A workspace outside the session root must be denied, not
            // reported as a cancelled start.
            let escaped = services
                .agents(
                    caller,
                    AgentsOp::Start(start_args("escape", Some(&self.escape_root))),
                )
                .await;
            seen.escaped = Some(escaped);
            // A name made only of session-id characters is refused by the
            // store; the refusal must name the rule, not collapse into a
            // cancellation.
            let id_named = services
                .agents(caller, AgentsOp::Start(start_args("a", None)))
                .await;
            seen.id_named = Some(id_named);
            // A valid start still reports Started, and the child finishes
            // the scripted turn.
            let started = services
                .agents(caller, AgentsOp::Start(start_args("worker", None)))
                .await;
            if let Ok(AgentsReply::Started { id }) = &started {
                let awaited = services
                    .agents(
                        caller,
                        AgentsOp::Await {
                            id: *id,
                            timeout: Some(Duration::from_secs(20)),
                        },
                    )
                    .await;
                seen.awaited = Some((*id, awaited));
            }
            seen.started = Some(started);
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

#[expect(
    clippy::too_many_lines,
    reason = "one scripted turn drives every start case and each outcome is checked in place"
)]
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
        awaited: None,
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
    let escaped = outcome.escaped.as_ref().expect("the escape case ran");
    assert!(
        matches!(
            escaped,
            Ok(AgentsReply::Refused {
                reason: AgentRefusal::WorkspaceOutsideRoot
            })
        ),
        "an out-of-scope workspace is refused with its reason: {escaped:?}"
    );
    let id_named = outcome.id_named.as_ref().expect("the id-name case ran");
    let Err(ServiceError::Failed { service, message }) = id_named else {
        panic!("an id-shaped name fails with the store rule: {id_named:?}");
    };
    assert_eq!(
        *service,
        Some(Service::Agents),
        "the failure is attributed to the agents service"
    );
    assert!(
        message.starts_with("could not start child session: ")
            && message.contains("1 to 64 characters"),
        "the refusal keeps the store's reason: {message}"
    );
    let started = outcome.started.as_ref().expect("the start case ran");
    let Ok(AgentsReply::Started { id: started_id }) = started else {
        panic!("a valid child reports Started: {started:?}");
    };
    let (awaited_id, awaited) = outcome.awaited.as_ref().expect("the await case ran");
    assert_eq!(
        awaited_id, started_id,
        "the await targeted the started child"
    );
    let Ok(AgentsReply::Await { report }) = awaited else {
        panic!("the await resolves to a completed report, not Pending or Cancelled: {awaited:?}");
    };
    assert_eq!(report.session, *started_id, "the report names the child");
    assert_eq!(
        report.stop,
        Stop::EndTurn,
        "the report carries its stop kind"
    );
    assert_eq!(
        report.text.as_ref(),
        "done",
        "the report carries the child's final text"
    );
}
