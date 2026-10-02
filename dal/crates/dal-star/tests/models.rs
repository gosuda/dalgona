//! Scripted model registrations through the host model context.

#![expect(clippy::expect_used, reason = "SC test")]
#![expect(clippy::panic, reason = "SC test")]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::time::Duration;

use dal_agent::{Delivery, Env, Host, Product, SessionRef};
use dal_core::{
    Command, Config, ConfigProduct, Expect, PageReq, Part, Stop, UpdateKind, Workspace,
};

mod support;

use support::system_with_plugin;

fn model_source(run: &str) -> String {
    format!(
        r#"load("@dal/v1", "dal")
def run(ctx, request):
{run}

model = dal.model(
    id = "dalgona/fusion",
    caps = {{
        "context_window": 100000,
        "thinking": ["off"],
        "tool_use": True,
        "image_input": False,
    }},
    run = run,
    uses = ["models.forward", "models.infer"],
)
plugin = dal.plugin(name = "fusion", version = "0.1.0", inject = ["infer"], models = {{"fusion": model}})
"#,
        run = run
            .lines()
            .map(|line| format!("    {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

async fn host_for(run: &str) -> (tempfile::TempDir, Host) {
    let (data, system) = system_with_plugin("fusion", &model_source(run));
    let extensions = system.extensions().expect("model extension conversion");
    let fixture = data.path().join("script.jsonl");
    std::fs::write(
        &fixture,
        r#"{"kind":"events","events":[{"type":"text_delta","text":"forwarded"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}"#,
    )
    .expect("provider replay fixture");
    let user = format!(
        "model = \"dalgona/fusion\"\n\n[providers.scripted]\nfixture = {:?}\n",
        fixture.display().to_string()
    );
    let config =
        Config::load(ConfigProduct::Dalgon, data.path(), "", Some(&user)).expect("host config");
    let workspace = data.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let product = Product {
        name: "dal",
        data_root: data.path().to_path_buf(),
        defaults: "",
        extensions,
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: workspace,
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env)
        .await
        .expect("host starts");
    (data, host)
}

async fn prompt(host: &Host, data: &tempfile::TempDir) -> (Stop, String, Vec<String>) {
    let workspace = Workspace::new(data.path().join("workspace")).expect("workspace");
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            dal_core::ClientId::new("model-test"),
        )
        .await
        .expect("session opens");
    let mut subscription = agent.subscribe(None).expect("subscription opens");
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "input".into(),
            }],
        })
        .await
        .expect("prompt submits");
    let mut seen = Vec::new();
    let stop = loop {
        let delivery = tokio::time::timeout(Duration::from_secs(10), subscription.next())
            .await
            .expect("turn update arrives");
        let Some(delivery) = delivery else {
            panic!("subscription closed before the turn ended");
        };
        let Delivery::Update(update) = delivery else {
            continue;
        };
        seen.push(format!("{:?}", update.kind));
        if let UpdateKind::TurnEnded { stop, .. } = update.kind {
            break stop;
        }
    };
    let view = format!(
        "{:?}",
        agent.view(PageReq::default()).expect("session view")
    );
    (stop, view, seen)
}

#[tokio::test]
async fn scripted_model_forwards_through_its_invocation_context() {
    let run = r#"return ctx.models.forward(
    purpose = request.purpose,
    model = {"kind": "api", "family": "openai_chat", "model": "gpt-6-luna"},
    system = request.system,
    tools = request.tools,
    context = request.context,
    params = request.params,
    cache_key = request.cache_key,
)"#;
    let (data, host) = host_for(run).await;
    let (stop, view, seen) = prompt(&host, &data).await;
    assert_eq!(stop, Stop::EndTurn, "{seen:?} {view}");
    assert!(view.contains("forwarded"), "{view}");
}

#[tokio::test]
async fn scripted_model_scope_passes_policy_and_usd_budget_to_scoped_infer() {
    let run = r#"scope = ctx.scope(limit = 8, on_error = "settle", usd = 0.40)
request = {
    "purpose": request.purpose,
    "model": {"kind": "api", "family": "openai_chat", "model": "gpt-6-luna"},
    "system": request.system,
    "tools": request.tools,
    "context": request.context,
    "params": request.params,
    "cache_key": request.cache_key,
}
task = scope.infer(request)
results = scope.settle()
return results[0].value"#;
    let (data, host) = host_for(run).await;
    let (stop, view, seen) = prompt(&host, &data).await;
    assert_eq!(stop, Stop::EndTurn, "{seen:?} {view}");
    assert!(view.contains("forwarded"), "{seen:?} {view}");
}

#[tokio::test]
async fn scripted_model_second_forward_call_returns_a_named_failure() {
    let run = r#"first = ctx.models.forward(
    purpose = request.purpose,
    model = {"kind": "api", "family": "openai_chat", "model": "gpt-6-luna"},
    system = request.system,
    tools = request.tools,
    context = request.context,
    params = request.params,
    cache_key = request.cache_key,
)
second = ctx.try_call(
    ctx.models.forward,
    purpose = request.purpose,
    model = {"kind": "api", "family": "openai_chat", "model": "gpt-6-luna"},
    system = request.system,
    tools = request.tools,
    context = request.context,
    params = request.params,
    cache_key = request.cache_key,
)
if second.error.message == "forward may be called at most once":
    return first
return None"#;
    let (data, host) = host_for(run).await;
    let (stop, view, seen) = prompt(&host, &data).await;
    assert_eq!(stop, Stop::EndTurn, "{seen:?} {view}");
    assert!(view.contains("forwarded"), "{seen:?} {view}");
}
