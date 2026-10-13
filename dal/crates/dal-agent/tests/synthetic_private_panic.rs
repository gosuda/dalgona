#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "fixtures fail loudly at their setup boundary and the timeout asserts the hang"
)]
//! A panicking synthetic private tool ends its own round, not the session.
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use dal_agent::ext::tool::{ArgError, RawValue, Tool, ToolCall, ToolCx, ToolOutcome};
use dal_agent::ext::{
    BoxFuture, EventStream, ExtensionBuilder, ModelCx, ModelError, ModelRecord, PrivateTool,
};
use dal_agent::{Delivery, Env, Host, Product, SessionRef};
use dal_core::{
    Caps, ClientId, Command, Config, ConfigProduct, Expect, Family, ModelId, ModelInfo,
    ModelRequest, ModelRoute, Name, Part, Reply, ServiceSet, ThinkingLevel, ToolClass, ToolSpec,
    UpdateKind, Workspace,
};

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
            parameters: dal_core::RawJson::parse(r#"{"type":"object","properties":{}}"#)
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
        panic!("private tool state was corrupt")
    }
}

const USAGE: &str = r#"{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}}"#;

fn tool_step() -> String {
    format!(
        r#"{{"kind":"events","events":[{{"type":"tool_call_started","id":"p1","name":"explode"}},{{"type":"tool_calls_done","calls":[{{"id":"p1","name":"explode","args":{{"kind":"parsed","value":{{}}}}}}]}},{USAGE},{{"type":"stop","reason":"tool_use"}}]}}"#
    )
}

fn text_step(text: &str) -> String {
    format!(
        r#"{{"kind":"events","events":[{{"type":"text_delta","text":"{text}"}},{{"type":"tool_calls_done","calls":[]}},{USAGE},{{"type":"stop","reason":"end_turn"}}]}}"#
    )
}

struct ExplodeModel;

impl dal_agent::ext::ModelHandler for ExplodeModel {
    fn run<'a>(
        &'a self,
        mut request: ModelRequest,
        cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            request.model = ModelRoute::Api {
                family: Family::Chat,
                model: "gpt-6-luna".into(),
            };
            let private = [PrivateTool(Arc::new(Explode::new()))];
            cx.forward(request, &private).await
        })
    }
}

async fn rig(steps: &[String]) -> (tempfile::TempDir, Host) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("w");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, steps.join("\n")).expect("fixture");
    let user = format!(
        "model = \"acme/exploder\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(user.as_str())).expect("config");
    let extension = ExtensionBuilder::new("synth-panic", "0.1.0", ServiceSet::default())
        .expect("builder")
        .model(ModelRecord {
            id: ModelId::parse("acme/exploder").expect("model id"),
            caps: Caps {
                context_window: Some(100_000),
                thinking: Box::new([ThinkingLevel::Off]),
                tool_use: true,
                image_input: false,
                custom_grammar: false,
            },
            handler: Arc::new(ExplodeModel),
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
        cwd: workspace_dir.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await.expect("host");
    (tmp, host)
}

async fn prompt_to_end(
    agent: &dal_agent::Agent,
    subscription: &mut dal_agent::Subscription,
    text: &str,
) -> String {
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
        let delivery = tokio::time::timeout(Duration::from_secs(15), subscription.next())
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
async fn a_panicking_private_tool_ends_its_turn_and_the_session_stays_usable() {
    let (tmp, host) = rig(&[tool_step(), text_step("recovered"), text_step("second")]).await;
    let agent = host
        .open(
            SessionRef::New {
                workspace: Workspace::new(tmp.path().join("w")).expect("workspace"),
                name: None,
            },
            ClientId::new("probe"),
        )
        .await
        .expect("open");
    let mut subscription = agent.subscribe(None).expect("subscribe");
    let first = prompt_to_end(&agent, &mut subscription, "go").await;
    assert!(
        first.contains("TurnEnded"),
        "the synthetic turn ended\n{first}"
    );
    let view = agent.view(dal_core::PageReq::default()).expect("view");
    let dump = format!("{view:?}");
    assert!(
        dump.contains("recovered"),
        "the model saw the error and answered\n{dump}"
    );
    let second = prompt_to_end(&agent, &mut subscription, "again").await;
    assert!(
        second.contains("TurnEnded"),
        "a second prompt ended\n{second}"
    );
    host.shutdown(Duration::from_secs(1)).await;
}
