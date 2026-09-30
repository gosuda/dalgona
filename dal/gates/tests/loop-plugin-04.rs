#![expect(
    clippy::disallowed_methods,
    reason = "SC test runs print mode through the CLI"
)]

//! Undeclared operations and headless process approvals fail closed.

mod support;

use std::{collections::BTreeMap, error::Error, fs, process::Command};

use dal_agent::{Delivery, Env, SessionRef};
use dal_core::{
    Command as AgentCommand, Config, ConfigProduct, Expect, Part, Question, Reply, UpdateKind,
    Workspace,
};
use support::{TestDir, dalgon_binary, scripted_session};

const NET_PLUGIN: &str = r#"load("@dal/v1", "dal")

def call_net(ctx, args):
    return ctx.net.fetch(url = args.url, method = "GET")

net_check = dal.tool(
    description = "Check the net service gate.",
    input = dal.schema(url = dal.optional(dal.string())),
    uses = [],
    run = call_net,
)

plugin = dal.plugin(
    name = "net-gate",
    version = "0.1.0",
    tools = {"net-check": net_check},
)
"#;
const RUN_PLUGIN: &str = r#"load("@dal/v1", "dal")

def call_run(ctx, args):
    return ctx.tools.exec(args.command)

run_check = dal.tool(
    description = "Check the run service gate.",
    input = dal.schema(command = dal.string()),
    uses = ["tools.exec"],
    run = call_run,
)

plugin = dal.plugin(
    name = "run-gate",
    version = "0.1.0",
    tools = {"run-check": run_check},
)
"#;

fn tool_script(name: &str, args: &str) -> String {
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"tool_call_started\",\"id\":\"grant-call\",\"name\":\"{name}\"}},{{\"type\":\"tool_calls_done\",\"calls\":[{{\"id\":\"grant-call\",\"name\":\"{name}\",\"args\":{{\"kind\":\"parsed\",\"value\":{args}}}}}]}},{{\"type\":\"usage\",\"usage\":{{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}}},{{\"type\":\"stop\",\"reason\":\"tool_use\"}}]}}\n{{\"kind\":\"events\",\"events\":[{{\"type\":\"text_delta\",\"text\":\"done\"}},{{\"type\":\"tool_calls_done\",\"calls\":[]}},{{\"type\":\"usage\",\"usage\":{{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}}},{{\"type\":\"stop\",\"reason\":\"end_turn\"}}]}}\n"
    )
}

#[tokio::test]
async fn undeclared_net_operation_is_denied_without_grant_request()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    let plugin_dir = data.path().join("plugins/net-gate");
    fs::create_dir_all(&plugin_dir)?;
    fs::write(plugin_dir.join("plugin.star"), NET_PLUGIN)?;
    let replay = data.path().join("net-scripted.jsonl");
    fs::write(
        &replay,
        tool_script("net-gate__net-check", "{\"url\":\"http://127.0.0.1:1\"}"),
    )?;
    let factory = dalgon::product();
    let user = format!(
        "model = \"openai/gpt-6\"\nplugins = [\"net-gate\"]\n[providers.scripted]\nfixture = {:?}\n",
        replay.to_string_lossy()
    );
    let config = Config::load(
        ConfigProduct::Dalgon,
        data.path(),
        factory.defaults,
        Some(&user),
    )?;
    let product = (factory.build)(&dalgon::BuildCx {
        data_root: data.path().to_path_buf(),
        config: &config,
    })?;
    let env = Env {
        vars: BTreeMap::new(),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    let session = SessionRef::Ephemeral {
        workspace: Workspace::new(workspace.path().to_path_buf())?,
    };
    let harness = scripted_session(product, config, env, session).await?;
    let mut subscription = harness.agent.subscribe(None)?;
    let prompt_reply = harness
        .agent
        .submit(AgentCommand::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Call net-check.".into(),
            }],
        })
        .await?;
    assert!(matches!(prompt_reply, Reply::Accepted { .. }));
    let mut denied_without_use = false;
    let mut settled_outcome = None;
    let mut grant_requests = 0;
    while let Some(delivery) = subscription.next().await {
        let Delivery::Update(update) = delivery else {
            continue;
        };
        match &update.kind {
            UpdateKind::ToolSettled { outcome, .. } => {
                let text = sonic_rs::to_string(outcome)?;
                denied_without_use |=
                    text.contains("denied: net.fetch is outside the approved scope");
                settled_outcome = Some(text);
            }
            UpdateKind::RequestOpened(request)
                if matches!(request.question, Question::Grant { .. }) =>
            {
                grant_requests += 1;
            }
            UpdateKind::TurnEnded { .. } => break,
            _ => {}
        }
    }
    assert!(denied_without_use, "settled outcome: {settled_outcome:?}");
    assert_eq!(grant_requests, 0);
    let _ = harness
        .host
        .shutdown(std::time::Duration::from_secs(1))
        .await;

    Ok(())
}

#[test]
fn headless_print_denies_exec_without_prompt() -> Result<(), Box<dyn Error + Send + Sync>> {
    let print = TestDir::new()?;
    let home = print.path().join("home");
    let config_dir = home.join(".config/dal");
    let data_home = home.join(".local/share");
    let plugin_dir = data_home.join("dal/plugins/run-gate");
    fs::create_dir_all(&config_dir)?;
    fs::create_dir_all(&plugin_dir)?;
    fs::write(plugin_dir.join("plugin.star"), RUN_PLUGIN)?;
    let replay = print.path().join("run-scripted.jsonl");
    let probe = print.path().join("exec-ran");
    let command = format!("touch {}", probe.display());
    let args = sonic_rs::to_string(&BTreeMap::from([("command", command)]))?;
    fs::write(&replay, tool_script("run-gate__run-check", &args))?;
    fs::write(
        config_dir.join("dal.toml"),
        format!(
            "model = \"openai/gpt-6\"\nplugins = [\"run-gate\"]\n[providers.scripted]\nfixture = {:?}\n",
            replay.to_string_lossy()
        ),
    )?;
    let output = Command::new(dalgon_binary("dalgon")?)
        .current_dir(print.path())
        .env_clear()
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", &data_home)
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .args(["-p", "--approval", "ask", "Call run-check."])
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(output.stderr)?;
    let rendered = format!("{stdout}\n{stderr}");
    assert!(
        !probe.exists(),
        "the denied exec command created its probe file"
    );
    assert!(
        !rendered.contains("Allow "),
        "headless print surfaced an approval prompt: {rendered}"
    );
    Ok(())
}

#[tokio::test]
async fn no_controller_denies_plugin_exec_without_opening_request()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    let plugin_dir = data.path().join("plugins/run-gate");
    fs::create_dir_all(&plugin_dir)?;
    fs::write(plugin_dir.join("plugin.star"), RUN_PLUGIN)?;
    let replay = data.path().join("run-scripted.jsonl");
    let probe = data.path().join("exec-ran");
    let command = format!("touch {}", probe.display());
    let args = sonic_rs::to_string(&BTreeMap::from([("command", command)]))?;
    fs::write(&replay, tool_script("run-gate__run-check", &args))?;
    let factory = dalgon::product();
    let user = format!(
        "approval = \"ask\"\nmodel = \"openai/gpt-6\"\nplugins = [\"run-gate\"]\n[providers.scripted]\nfixture = {:?}\n",
        replay.to_string_lossy()
    );
    let config = Config::load(
        ConfigProduct::Dalgon,
        data.path(),
        factory.defaults,
        Some(&user),
    )?;
    let product = (factory.build)(&dalgon::BuildCx {
        data_root: data.path().to_path_buf(),
        config: &config,
    })?;
    let env = Env {
        vars: BTreeMap::new(),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    let session = SessionRef::Ephemeral {
        workspace: Workspace::new(workspace.path().to_path_buf())?,
    };
    let harness = scripted_session(product, config, env, session).await?;
    let mut subscription = harness.agent.subscribe(None)?;
    let prompt_reply = harness
        .agent
        .submit(AgentCommand::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Call run-check.".into(),
            }],
        })
        .await?;
    assert!(matches!(prompt_reply, Reply::Accepted { .. }));
    let mut exec_outcome = None;
    let mut opened_requests = 0;
    while let Some(delivery) = subscription.next().await {
        let Delivery::Update(update) = delivery else {
            continue;
        };
        match &update.kind {
            UpdateKind::ToolSettled { outcome, .. } => {
                exec_outcome = Some(sonic_rs::to_string(outcome)?);
            }
            UpdateKind::RequestOpened(_) => opened_requests += 1,
            UpdateKind::TurnEnded { .. } => break,
            _ => {}
        }
    }
    let exec_outcome = exec_outcome.ok_or_else(|| std::io::Error::other("missing exec outcome"))?;
    assert!(
        exec_outcome.contains("\"text\":\"denied: no front end can answer\""),
        "{exec_outcome}"
    );
    assert_eq!(opened_requests, 0);
    assert!(
        !probe.exists(),
        "the denied exec command created its probe file"
    );
    let _ = harness
        .host
        .shutdown(std::time::Duration::from_secs(1))
        .await;
    Ok(())
}
