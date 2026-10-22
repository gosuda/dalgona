#![expect(
    clippy::expect_used,
    reason = "integration fixture failures must fail at their specific setup boundary"
)]
//! The prompt-cache key and the tool list of a session's turn requests: the
//! key stays constant while the list does, and a deferred tool's promotion
//! changes the `<visibility>` component once and the tool list with it. A
//! synthetic model records each request exactly as the driver built it.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::ext::{
    ArgError, BoxFuture, EventStream, ExtensionBuilder, ModelCx, ModelError, ModelHandler,
    ModelRecord, RawValue, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput,
};
use dal_agent::{Delivery, Env, Host, Product, SessionRef};
use dal_core::{
    Caps, ClientId, Command, Config, ConfigProduct, Expect, Family, ModelId, ModelInfo,
    ModelRequest, ModelRoute, Name, Part, RawJson, ServiceSet, Stop, ThinkingLevel, ToolClass,
    ToolSpec, UpdateKind, Visibility, Workspace,
};

const END: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"done\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";
const CALL: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"tool_call_started\",\"id\":\"c1\",\"name\":\"mcp_echo\"},{\"type\":\"tool_calls_done\",\"calls\":[{\"id\":\"c1\",\"name\":\"mcp_echo\",\"args\":{\"kind\":\"parsed\",\"value\":{}}}]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"tool_use\"}]}\n";

struct Echo {
    name: Name,
    spec: Arc<ToolSpec>,
}

impl Tool for Echo {
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
        Box::pin(async { ToolOutcome::Ok(Box::new(ToolOutput::from_text("echoed"))) })
    }
}

fn echo() -> Arc<dyn Tool> {
    let name = Name::parse("mcp_echo").expect("tool name");
    Arc::new(Echo {
        spec: Arc::new(ToolSpec {
            name: name.clone(),
            description: "deferred echo".into(),
            parameters: RawJson::parse(r#"{"type":"object"}"#).expect("schema"),
            grammar: None,
        }),
        name,
    })
}

/// Records each request the driver built, then forwards it to the scripted
/// provider.
struct Probe {
    seen: Arc<Mutex<Vec<ModelRequest>>>,
}

impl ModelHandler for Probe {
    fn run<'a>(
        &'a self,
        request: ModelRequest,
        cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            self.seen.lock().expect("seen lock").push(request.clone());
            let mut forwarded = request;
            forwarded.model = ModelRoute::Api {
                family: Family::Chat,
                model: "gpt-6-luna".into(),
                provider: None,
            };
            cx.forward(forwarded, &[]).await
        })
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "cache-key fixture walks twelve turns and key assertions in place"
)]
#[tokio::test]
async fn the_cache_key_changes_once_when_a_deferred_tool_is_promoted_and_never_otherwise() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data directory");
    std::fs::create_dir_all(&workspace_dir).expect("workspace directory");
    let fixture = data.join("script.jsonl");
    // Ten plain turns, one turn that calls the deferred tool by name, one more.
    let script = format!("{}{CALL}{END}{END}", END.repeat(10));
    std::fs::write(&fixture, script).expect("script fixture");
    let config_text = format!(
        "approval = \"all\"\nmodel = \"probe/pool\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(config_text.as_str())).expect("config");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let extension = ExtensionBuilder::new("probe", "0.1.0", ServiceSet::default())
        .expect("extension builder")
        .model(ModelRecord {
            id: ModelId::parse("probe/pool").expect("model id"),
            caps: Caps {
                context_window: Some(100_000),
                thinking: Box::new([ThinkingLevel::Off]),
                tool_use: true,
                image_input: false,
                custom_grammar: false,
            },
            handler: Arc::new(Probe {
                seen: Arc::clone(&seen),
            }),
            export: None,
        })
        .tool(echo(), Visibility::Deferred)
        .build()
        .expect("extension");
    let product = Product {
        name: "dal",
        data_root: data.clone(),
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([
            (OsString::from("OPENAI_API_KEY"), OsString::from("sk-test")),
            (
                OsString::from("HOME"),
                OsString::from(tmp.path().join("home")),
            ),
        ]),
        cwd: workspace_dir.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await.expect("host");
    let workspace = Workspace::new(workspace_dir).expect("workspace");
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("cache-key-test"),
        )
        .await
        .expect("session");
    let session = agent
        .view(dal_core::PageReq::default())
        .expect("view")
        .session
        .id;
    let mut subscription = agent.subscribe(None).expect("subscribe");
    for round in 0..12 {
        agent
            .submit(Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text {
                    text: format!("turn {round}").into(),
                }],
            })
            .await
            .expect("prompt accepted");
        let ended = loop {
            let delivery = tokio::time::timeout(Duration::from_secs(10), subscription.next())
                .await
                .expect("a delivery in time")
                .expect("the subscription stays open");
            if let Delivery::Update(update) = delivery
                && let UpdateKind::TurnEnded { stop, .. } = update.kind
            {
                break stop;
            }
        };
        assert_eq!(ended, Stop::EndTurn, "turn {round}");
    }
    host.close(session).await.expect("close");

    let seen = std::mem::take(&mut *seen.lock().expect("seen lock"));
    // Ten plain turns, the promoting turn's two provider calls, then one more
    // turn: a turn's tool list is frozen when it starts, so the promotion the
    // call journals first reaches the request of the turn after it.
    assert_eq!(seen.len(), 13, "one request per provider call");
    let keys: Vec<&str> = seen
        .iter()
        .map(|request| {
            request
                .cache_key
                .as_deref()
                .expect("a turn request carries a cache key")
        })
        .collect();
    let at = |visibility: u32| format!("{session}:0:{visibility}");
    for (index, key) in keys.iter().take(12).enumerate() {
        assert_eq!(
            *key,
            at(0),
            "request {index} changed nothing in the tool list"
        );
    }
    assert_eq!(
        keys[12],
        at(1),
        "the promotion is one boundary: one component changes"
    );

    let names = |request: &ModelRequest| -> Vec<String> {
        request
            .tools
            .iter()
            .map(|tool| tool.name.to_string())
            .collect()
    };
    for request in &seen[..12] {
        assert_eq!(
            request.tools, seen[0].tools,
            "the tool list is byte-identical inside and across turns until a promotion"
        );
    }
    assert_eq!(names(&seen[0]), ["tool_search"]);
    assert_eq!(
        names(&seen[12]),
        ["mcp_echo"],
        "after promotion the tool lists in place of the search tool"
    );
    assert!(
        seen.iter()
            .all(|request| request.tools.iter().all(|tool| !tool.defer_loading)),
        "a synthetic route takes the client surface: nothing is deferred on the wire"
    );
}
