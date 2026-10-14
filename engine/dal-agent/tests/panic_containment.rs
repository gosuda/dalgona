#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "fixtures fail loudly at their setup boundary and the tool and handler panic on purpose"
)]
//! A panicking tool or slash handler fails its own call, not the session.
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use dal_agent::ext::command::{CommandCx, CommandHandler};
use dal_agent::ext::tool::{ArgError, RawValue, Tool, ToolCall, ToolCx, ToolOutcome};
use dal_agent::ext::{BoxFuture, ExtensionBuilder};
use dal_agent::{
    Agent, AgentError, Delivery, Env, Host, Product, ServiceError, SessionRef, Subscription,
};
use dal_core::{
    ClientId, Command, CommandName, CommandSpec, Config, ConfigProduct, Expect, ModelInfo, Name,
    Part, RawJson, Reply, ServiceSet, ToolClass, ToolSpec, UpdateKind, Visibility, Workspace,
};

const USAGE: &str = r#"{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}}"#;

fn text_step(text: &str) -> String {
    format!(
        r#"{{"kind":"events","events":[{{"type":"text_delta","text":"{text}"}},{{"type":"tool_calls_done","calls":[]}},{USAGE},{{"type":"stop","reason":"end_turn"}}]}}"#
    )
}

fn explode_step() -> String {
    format!(
        r#"{{"kind":"events","events":[{{"type":"tool_call_started","id":"p1","name":"explode"}},{{"type":"tool_calls_done","calls":[{{"id":"p1","name":"explode","args":{{"kind":"parsed","value":{{}}}}}}]}},{USAGE},{{"type":"stop","reason":"tool_use"}}]}}"#
    )
}

struct Explode {
    name: Name,
    spec: Arc<ToolSpec>,
}

impl Explode {
    fn new() -> Self {
        let name = Name::parse("explode").expect("tool name");
        let spec = Arc::new(ToolSpec {
            name: name.clone(),
            description: "Panics when called.".into(),
            parameters: RawJson::parse(r#"{"type":"object","properties":{}}"#)
                .expect("schema json"),
            grammar: None,
        });
        Self { name, spec }
    }
}

impl Tool for Explode {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _ws: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, _call: ToolCall, _cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async { panic!("tool state was corrupt") })
    }
}

struct Boom;

impl CommandHandler for Boom {
    fn run<'a>(
        &'a self,
        _args: &'a str,
        _cx: CommandCx<'a>,
    ) -> BoxFuture<'a, Result<Reply, ServiceError>> {
        Box::pin(async { panic!("handler state was corrupt") })
    }
}

async fn open(steps: &[String]) -> (tempfile::TempDir, Host, Agent) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("w");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, steps.join("\n")).expect("fixture");
    let user = format!(
        "model = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(user.as_str())).expect("config");
    let extension = ExtensionBuilder::new("panics", "0.1.0", ServiceSet::EMPTY)
        .expect("builder")
        .tool(Arc::new(Explode::new()), Visibility::Model)
        .command(
            CommandSpec {
                name: CommandName::parse("boom").expect("command name"),
                summary: "Panics when run.".into(),
                args_hint: None,
            },
            Arc::new(Boom),
        )
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
        cwd: workspace_dir.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await.expect("host");
    let agent = host
        .open(
            SessionRef::New {
                workspace: Workspace::new(workspace_dir).expect("workspace"),
                name: None,
            },
            ClientId::new("probe"),
        )
        .await
        .expect("open");
    (tmp, host, agent)
}

/// Submits one prompt and returns the journal text seen up to its `TurnEnded`.
async fn prompt_to_end(agent: &Agent, subscription: &mut Subscription, text: &str) -> String {
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: text.into() }],
        })
        .await
        .expect("prompt is accepted");
    assert!(matches!(reply, Reply::Accepted { .. }), "{reply:?}");
    let mut seen = String::new();
    loop {
        let delivery = tokio::time::timeout(Duration::from_secs(10), subscription.next())
            .await
            .unwrap_or_else(|_| panic!("turn never ended; saw {seen}"))
            .expect("stream open");
        if let Delivery::Update(update) = &delivery {
            let _ = writeln!(seen, "{:?}", update.kind);
            if matches!(update.kind, UpdateKind::TurnEnded { .. }) {
                return seen;
            }
        }
    }
}

#[tokio::test]
async fn a_panicking_tool_fails_its_call_and_the_session_stays_usable() {
    let (_tmp, host, agent) =
        open(&[explode_step(), text_step("recovered"), text_step("second")]).await;
    let mut subscription = agent.subscribe(None).expect("subscribe");
    let first = prompt_to_end(&agent, &mut subscription, "go").await;
    let view = agent.view(dal_core::PageReq::default()).expect("view");
    let dump = format!("{view:?}");
    assert!(
        dump.contains("explode tool crashed") && dump.contains("tool state was corrupt"),
        "the tool result names the tool and carries the panic text\n{first}\n{dump}"
    );
    assert!(
        dump.contains("recovered"),
        "the model saw the error and answered\n{dump}"
    );
    let second = prompt_to_end(&agent, &mut subscription, "again").await;
    assert!(
        second.contains("second"),
        "the session ran a second turn\n{second}"
    );
    host.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn a_panicking_command_answers_an_error_and_the_session_stays_usable() {
    let (_tmp, host, agent) = open(&[text_step("after")]).await;
    let mut subscription = agent.subscribe(None).expect("subscribe");
    let outcome = tokio::time::timeout(
        Duration::from_secs(10),
        agent.submit(Command::Run {
            name: "boom".into(),
            args: String::new().into(),
            expected: None,
        }),
    )
    .await
    .expect("the command answered instead of hanging");
    let Err(AgentError::Invalid(error)) = outcome else {
        panic!("expected an invalid-command error, got {outcome:?}");
    };
    let text = error.to_string();
    assert!(
        text.contains("boom")
            && text.contains("crashed")
            && text.contains("handler state was corrupt"),
        "{text}"
    );
    let ended = prompt_to_end(&agent, &mut subscription, "hi").await;
    assert!(ended.contains("after"), "the session ran a turn\n{ended}");
    host.shutdown(Duration::from_secs(1)).await;
}
