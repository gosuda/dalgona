mod support;

use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::OsString;
use std::time::Duration;

use dal_agent::{Delivery, Env, Host, Product, SessionRef};
use dal_core::{
    ClientId, Command, Config, ConfigProduct, Expect, PageReq, Part, Reply, ToolOutcomeView,
    UpdateKind, Workspace,
};
use support::system_with_plugin;

const REPLAY: &str = r#"{"kind":"events","events":[{"type":"tool_call_started","id":"hook-call","name":"HOOK_TOOL"},{"type":"tool_calls_done","calls":[{"id":"hook-call","name":"HOOK_TOOL","args":{"kind":"parsed","value":{}}}]},{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"tool_use"}]}
{"kind":"events","events":[{"type":"text_delta","text":"finished"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}"#;

fn plugin_source(hook_return: &str) -> String {
    let mut source = String::from(
        r#"load("@dal/v1", "dal")
def hit(ctx, args):
    return "tool body ran"

hit_tool = dal.tool(description = "Return a marker.", input = dal.schema(), run = hit)
def guard(ctx, event):
    return "#,
    );
    source.push_str(hook_return);
    source.push_str(
        r#"

plugin = dal.plugin(
    name = "hookcase",
    version = "0.1.0",
    tools = {"hit": hit_tool},
    hooks = [dal.on("tool_call", guard)],
)
"#,
    );
    source
}

async fn settled_tool_call(
    hook_return: &str,
) -> Result<ToolOutcomeView, Box<dyn Error + Send + Sync>> {
    let (data, system) = system_with_plugin("hookcase", &plugin_source(hook_return));
    let extensions = system.extensions()?;
    let data_root = data.path().to_path_buf();
    let workspace = tempfile::tempdir()?;
    let fixture = data_root.join("hook-scripted.jsonl");
    std::fs::write(&fixture, REPLAY.replace("HOOK_TOOL", "hookcase__hit"))?;
    let user = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6\"\n[providers.scripted]\nfixture = {:?}\n",
        fixture.to_string_lossy()
    );
    let config = Config::load(ConfigProduct::Dalgon, &data_root, "", Some(&user))?;
    let product = Product {
        name: "dal",
        data_root: data_root.clone(),
        defaults: "",
        extensions,
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await?;
    let agent = host
        .open(
            SessionRef::Ephemeral {
                workspace: Workspace::new(workspace.path().to_path_buf())?,
            },
            ClientId::new("hook-test"),
        )
        .await?;
    let result = async {
        let mut subscription = agent.subscribe(None)?;
        let reply = agent
            .submit(Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text {
                    text: "Call hookcase__hit once.".into(),
                }],
            })
            .await?;
        if !matches!(reply, Reply::Accepted { .. }) {
            return Err(std::io::Error::other("the scripted prompt was not accepted").into());
        }
        let mut settled = None;
        let mut seen = Vec::new();
        loop {
            let delivery = tokio::time::timeout(Duration::from_secs(5), subscription.next()).await;
            let Ok(delivery) = delivery else {
                let view = agent.view(PageReq::default())?;
                return Err(std::io::Error::other(format!(
                    "no update from the scripted turn; seen={seen:?}; view={view:?}"
                ))
                .into());
            };
            let Some(Delivery::Update(update)) = delivery else {
                break;
            };
            seen.push(format!("{:?}", update.kind));
            match &update.kind {
                UpdateKind::ToolSettled { outcome, .. } => settled = Some(outcome.clone()),
                UpdateKind::TurnEnded { .. } if settled.is_some() => break,
                _ => {}
            }
        }
        settled.ok_or_else(|| {
            std::io::Error::other("the host did not settle the scripted tool call").into()
        })
    }
    .await;
    let _ = host.shutdown(Duration::from_secs(1)).await;
    result
}

#[tokio::test(flavor = "multi_thread")]
async fn dictionary_tool_call_hook_result_fails_closed() {
    let outcome = settled_tool_call("{\"ok\": True}")
        .await
        .expect("hook call settles");

    assert!(outcome.is_error, "{outcome:?}");
    assert!(
        outcome
            .text
            .contains("tool_call hooks must return an event verdict"),
        "{outcome:?}"
    );
}

const LIFECYCLE_PLUGIN: &str = r#"load("@dal/v1", "dal")

def on_start(ctx, event):
    return None

def on_end(ctx, event):
    return None

plugin = dal.plugin(
    name = "lifecycle",
    version = "0.1.0",
    hooks = [dal.on("session_start", on_start), dal.on("session_end", on_end)],
)
"#;

/// Opens a session over the lifecycle plugin and returns every notice the
/// session published up to `window`.
async fn lifecycle_notices(
    window: std::time::Duration,
) -> Result<Vec<String>, Box<dyn Error + Send + Sync>> {
    let (data, system) = system_with_plugin("lifecycle", LIFECYCLE_PLUGIN);
    let extensions = system.extensions()?;
    let data_root = data.path().to_path_buf();
    let workspace = tempfile::tempdir()?;
    let fixture = data_root.join("lifecycle-scripted.jsonl");
    std::fs::write(&fixture, "")?;
    let user = format!(
        "model = \"openai/gpt-6\"\n[providers.scripted]\nfixture = {:?}\n",
        fixture.to_string_lossy()
    );
    let config = Config::load(ConfigProduct::Dalgon, &data_root, "", Some(&user))?;
    let product = Product {
        name: "dal",
        data_root: data_root.clone(),
        defaults: "",
        extensions,
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::new(),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await?;
    let agent = host
        .open(
            SessionRef::Ephemeral {
                workspace: Workspace::new(workspace.path().to_path_buf())?,
            },
            ClientId::new("lifecycle-test"),
        )
        .await?;
    let mut subscription = agent.subscribe(None)?;
    let mut notices = Vec::new();
    let deadline = tokio::time::Instant::now() + window;
    loop {
        match tokio::time::timeout_at(deadline, subscription.next()).await {
            Ok(Some(Delivery::Update(update))) => match &update.kind {
                UpdateKind::Notice(notice) => notices.push(notice.text.to_string()),
                _ => {}
            },
            _ => break,
        }
    }
    let _ = host.shutdown(Duration::from_secs(2)).await;
    Ok(notices)
}

#[tokio::test(flavor = "multi_thread")]
async fn session_lifecycle_hooks_receive_a_script_context() {
    let notices = lifecycle_notices(Duration::from_secs(2))
        .await
        .expect("lifecycle session runs");

    let failed = notices
        .iter()
        .filter(|text| text.contains("hook \"lifecycle\" failed"))
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        failed.is_empty(),
        "session lifecycle hooks must run; got: {failed:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_tool_call_hook_verdict_fails_closed() {
    let outcome = settled_tool_call("None").await.expect("hook call settles");

    assert!(outcome.is_error, "{outcome:?}");
    assert!(
        outcome
            .text
            .contains("tool_call hooks must return an event verdict"),
        "{outcome:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_call_event_rejects_a_verdict_method_from_another_event() {
    let outcome = settled_tool_call("event.continue_()")
        .await
        .expect("hook call settles");

    assert!(outcome.is_error, "{outcome:?}");
    assert!(
        outcome.text.contains("hook \"hookcase\" failed"),
        "{outcome:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_call_block_verdict_preserves_its_reason() {
    let outcome = settled_tool_call("event.block(\"x\")")
        .await
        .expect("hook call settles");

    assert!(outcome.is_error, "{outcome:?}");
    assert_eq!(outcome.text.as_ref(), "x");
}
