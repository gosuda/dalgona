//! Real-host boundary tests for the permission-gate example plugin.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use dal_agent::ext::{
    BoxFuture, ExtensionBuilder, RawValue, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput,
};
use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef};
use dal_core::{
    ClientId, Command, Config, ConfigProduct, Expect, Output, Part, Reply, RunRequest, ToolClass,
    ToolOutcomeView, UpdateKind, Workspace,
};
use dal_star::{LoadRoots, PluginSystem, PluginsConfig, load};

const PLUGIN_SOURCE: &str = include_str!("../../../examples/plugins/permission-gate/plugin.star");

struct FixtureExec {
    name: dal_core::Name,
    spec: Arc<dal_core::ToolSpec>,
    command: String,
}

impl Tool for FixtureExec {
    fn name(&self) -> &dal_core::Name {
        &self.name
    }

    fn spec(&self, _model: &dal_core::ModelInfo) -> Arc<dal_core::ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(
        &self,
        _args: &RawValue,
        _workspace: &Workspace,
    ) -> Result<ToolClass, dal_agent::ext::ArgError> {
        Ok(ToolClass::Other)
    }

    fn run<'a>(&'a self, _call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let request = RunRequest {
                argv: vec![
                    OsString::from("sh"),
                    OsString::from("-c"),
                    OsString::from(self.command.as_str()),
                ],
                cwd: None,
                stdin: None,
                timeout: Some(Duration::from_secs(5)),
                env: Vec::new(),
                stdout_prefix_limit: 4096,
            };
            match cx.services().run(cx.caller(), request).await {
                Ok(output) => ToolOutcome::Ok(Box::new(ToolOutput::from_text(
                    String::from_utf8_lossy(&output.stdout_tail)
                        .into_owned()
                        .into_boxed_str(),
                ))),
                Err(error) => ToolOutcome::Err(dal_agent::ToolError::message(error.to_string())),
            }
        })
    }
}

fn fixture_exec(command: &str) -> Result<dal_agent::ext::Extension, Box<dyn Error + Send + Sync>> {
    let name = dal_core::Name::parse("exec")?;
    let spec = Arc::new(dal_core::ToolSpec {
        name: name.clone(),
        description: "Run a fixture command.".into(),
        parameters: dal_core::RawJson::parse(
            r#"{"type":"object","properties":{"command":{"type":"string"},"timeout_seconds":{"type":"integer"}},"required":["command"]}"#,
        )?,
        grammar: None,
    });
    let services = dal_core::ServiceSet::from_names(["run"])?;
    Ok(ExtensionBuilder::new("fixture-exec", "0.1.0", services)?
        .tool(
            Arc::new(FixtureExec {
                name,
                spec,
                command: command.to_owned(),
            }),
            dal_core::Visibility::Model,
        )
        .build()?)
}

fn exec_replay(command: &str) -> Result<String, sonic_rs::Error> {
    let command = sonic_rs::to_string(&command)?;
    Ok(format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"tool_call_started\",\"id\":\"exec-call\",\"name\":\"exec\"}},{{\"type\":\"tool_calls_done\",\"calls\":[{{\"id\":\"exec-call\",\"name\":\"exec\",\"args\":{{\"kind\":\"parsed\",\"value\":{{\"command\":{command},\"timeout_seconds\":5}}}}}}]}},{{\"type\":\"usage\",\"usage\":{{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}}},{{\"type\":\"stop\",\"reason\":\"tool_use\"}}]}}\n{{\"kind\":\"events\",\"events\":[{{\"type\":\"text_delta\",\"text\":\"finished\"}},{{\"type\":\"tool_calls_done\",\"calls\":[]}},{{\"type\":\"usage\",\"usage\":{{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}}},{{\"type\":\"stop\",\"reason\":\"end_turn\"}}]}}\n"
    ))
}

async fn start_host(
    config_json: Option<&str>,
    command: &str,
) -> Result<(tempfile::TempDir, Host, Agent, PathBuf), Box<dyn Error + Send + Sync>> {
    let root = tempfile::tempdir()?;
    let data = root.path().join("data");
    let plugin_dir = data.join("plugins/permission-gate");
    std::fs::create_dir_all(&plugin_dir)?;
    std::fs::write(plugin_dir.join("plugin.star"), PLUGIN_SOURCE)?;

    let mut plugin_config = PluginsConfig::default();
    if let Some(config_json) = config_json {
        plugin_config
            .configs
            .insert("permission-gate".to_owned(), config_json.to_owned());
    }
    let roots = LoadRoots {
        data_root: data.clone(),
        bundled: Vec::new(),
    };
    let generation = load(&roots, &plugin_config)?;
    let mut extensions = vec![fixture_exec(command)?];
    extensions.extend(PluginSystem::new(generation, roots, plugin_config).extensions()?);

    let fixture = data.join("permission-gate-scripted.jsonl");
    std::fs::write(&fixture, exec_replay(command)?)?;
    let user = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6\"\n[providers.scripted]\nfixture = {:?}\n",
        fixture.to_string_lossy()
    );
    let config = Config::load(ConfigProduct::Dalgon, &data, "", Some(&user))?;

    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions,
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: workspace.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await?;
    let agent = host
        .open(
            SessionRef::Ephemeral {
                workspace: Workspace::new(workspace.clone())?,
            },
            ClientId::new("permission-gate-test"),
        )
        .await?;
    Ok((root, host, agent, workspace))
}

async fn run_exec(agent: &Agent) -> Result<ToolOutcomeView, Box<dyn Error + Send + Sync>> {
    let mut subscription = agent.subscribe(None)?;
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Run the requested command.".into(),
            }],
        })
        .await?;
    if !matches!(reply, Reply::Accepted { .. }) {
        return Err(std::io::Error::other(format!("prompt was not accepted: {reply:?}")).into());
    }

    let mut settled = None;
    loop {
        let delivery = tokio::time::timeout(Duration::from_secs(5), subscription.next()).await?;
        let Some(Delivery::Update(update)) = delivery else {
            break;
        };
        match &update.kind {
            UpdateKind::ToolSettled { outcome, .. } => settled = Some(outcome.clone()),
            UpdateKind::TurnEnded { .. } if settled.is_some() => break,
            _ => {}
        }
    }
    settled.ok_or_else(|| std::io::Error::other("the real host did not settle exec").into())
}

async fn gate_count(agent: &Agent) -> Result<usize, Box<dyn Error + Send + Sync>> {
    let reply = agent
        .submit(Command::Run {
            name: "permission-gate:gate".into(),
            args: String::new().into(),
            expected: None,
        })
        .await?;
    let Reply::Done(Output::Text(text)) = reply else {
        return Err(std::io::Error::other("unexpected /permission-gate:gate reply").into());
    };
    text.split_whitespace()
        .find_map(|word| word.parse::<usize>().ok())
        .ok_or_else(|| std::io::Error::other("gate output has no active-pattern count").into())
}

#[tokio::test(flavor = "multi_thread")]
async fn permission_gate_blocks_default_dangerous_exec_without_side_effect() {
    let (_root, host, agent, workspace) = start_host(None, "rm -rf blocked.txt")
        .await
        .expect("permission-gate host starts");
    let marker = workspace.join("blocked.txt");
    std::fs::write(&marker, b"keep").expect("blocked marker");

    let outcome = run_exec(&agent).await.expect("exec settles");
    assert!(
        outcome.is_error,
        "dangerous exec must be refused: {outcome:?}"
    );
    assert!(
        outcome.text.contains("permission-gate: blocked exec"),
        "block reason names the real tool: {outcome:?}"
    );
    assert!(
        outcome.text.contains("matches \"rm -rf\""),
        "block reason names the matching default pattern: {outcome:?}"
    );
    assert!(marker.exists(), "blocked exec must not remove the marker");

    host.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn permission_gate_blocks_configured_patterns_without_side_effect() {
    let (_root, host, agent, workspace) = start_host(
        Some(r#"{"patterns":["drop table"]}"#),
        "printf 'drop table' > configured.txt",
    )
    .await
    .expect("permission-gate host starts");
    let marker = workspace.join("configured.txt");
    std::fs::write(&marker, b"keep").expect("configured marker");

    let outcome = run_exec(&agent).await.expect("exec settles");
    assert!(
        outcome.is_error,
        "configured dangerous exec must be refused"
    );
    assert!(
        outcome.text.contains("matches \"drop table\""),
        "configured pattern appears in the block reason: {outcome:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&marker).expect("configured marker remains"),
        "keep"
    );
    assert_eq!(
        gate_count(&agent)
            .await
            .expect("/permission-gate:gate reports active patterns"),
        3 + 1
    );

    host.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn permission_gate_keeps_its_default_patterns_when_extra_patterns_are_configured() {
    // Configuring `drop table` extends the defaults; it must not replace them.
    let (_root, host, agent, workspace) =
        start_host(Some(r#"{"patterns":["drop table"]}"#), "rm -rf blocked.txt")
            .await
            .expect("permission-gate host starts");
    let marker = workspace.join("blocked.txt");
    std::fs::write(&marker, b"keep").expect("blocked marker");

    let outcome = run_exec(&agent).await.expect("exec settles");
    assert!(
        outcome.is_error,
        "a default pattern must still refuse: {outcome:?}"
    );
    assert!(
        outcome.text.contains("matches \"rm -rf\""),
        "the default pattern is the one named: {outcome:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&marker).expect("blocked marker remains"),
        "keep"
    );

    host.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn permission_gate_allows_safe_exec_to_reach_the_real_tool() {
    let (_root, host, agent, workspace) = start_host(None, "printf allowed > allowed.txt")
        .await
        .expect("permission-gate host starts");

    let outcome = run_exec(&agent).await.expect("exec settles");
    assert!(!outcome.is_error, "safe exec must be allowed: {outcome:?}");
    assert_eq!(
        std::fs::read_to_string(workspace.join("allowed.txt")).expect("allowed output"),
        "allowed"
    );

    host.shutdown(Duration::from_secs(1)).await;
}
