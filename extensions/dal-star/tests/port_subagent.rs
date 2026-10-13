//! The subagent example runs real child sessions through the agents service.

#![expect(
    clippy::expect_used,
    reason = "integration tests use unwrap/expect freely per repo test convention"
)]

use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use dal_agent::ext::{
    BoxFuture, ExtensionBuilder, RawValue, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput,
};
use dal_agent::{Agent, Delivery, Env, Host, HostSubscription, HostUpdate, Product, SessionRef};
use dal_core::{
    Answer, CallId, ClientId, Command, Config, ConfigProduct, ExitStatusKind, Expect, PageReq,
    Part, RunRequest, ToolClass, ToolOutcomeView, UpdateKind, Workspace,
};
use dal_star::{LoadRoots, PluginSystem, PluginsConfig, load};

const USAGE: &str = r#"{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}"#;
const PLUGIN_TOOL: &str = "subagent__subagent";
#[derive(serde::Deserialize)]
struct FixtureExecArgs {
    command: String,
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

struct FixtureExec {
    name: dal_core::Name,
    spec: Arc<dal_core::ToolSpec>,
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
        Ok(ToolClass::Exec {
            read_only: false,
            grant: None,
        })
    }

    fn run<'a>(&'a self, call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let args: FixtureExecArgs = match sonic_rs::from_str(call.args.as_str()) {
                Ok(args) => args,
                Err(error) => {
                    return ToolOutcome::Err(dal_agent::ToolError::message(error.to_string()));
                }
            };
            let timeout = args.timeout_seconds.unwrap_or(10);
            let command = args.command;
            let command_display = format!("{command:?}");
            let request = RunRequest {
                argv: vec![
                    OsString::from("sh"),
                    OsString::from("-c"),
                    OsString::from(command),
                ],
                cwd: None,
                stdin: None,
                timeout: Some(Duration::from_secs(timeout)),
                env: Vec::new(),
                stdout_prefix_limit: 4096,
            };
            match cx.services().run(cx.caller(), request).await {
                Ok(output) if matches!(output.status, ExitStatusKind::Exited(0)) => {
                    ToolOutcome::Ok(Box::new(ToolOutput::from_text(
                        String::from_utf8_lossy(&output.stdout_tail)
                            .into_owned()
                            .into_boxed_str(),
                    )))
                }
                Ok(output) => ToolOutcome::Err(dal_agent::ToolError::message(format!(
                    "fixture command failed: cwd={}, command={}, \
                     status={:?}, stdout={:?}, stderr={:?}, log={:?}",
                    cx.workspace().as_path().display(),
                    command_display,
                    output.status,
                    String::from_utf8_lossy(&output.stdout_tail),
                    String::from_utf8_lossy(&output.stderr_tail),
                    output.log,
                ))),
                Err(error) => ToolOutcome::Err(dal_agent::ToolError::message(format!(
                    "fixture command unavailable: cwd={}, command={}: {error}",
                    cx.workspace().as_path().display(),
                    command_display,
                ))),
            }
        })
    }
}

fn fixture_exec() -> Result<dal_agent::ext::Extension, Box<dyn Error + Send + Sync>> {
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
        .with_origin(dal_core::Origin::Bundled, None)
        .tool(
            Arc::new(FixtureExec { name, spec }),
            dal_core::Visibility::Model,
        )
        .build()?)
}

fn json_string(value: &str) -> String {
    sonic_rs::to_string(value).expect("fixture string is serializable")
}

fn tool_step(id: &str, name: &str, args: &str) -> String {
    let id = json_string(id);
    let name = json_string(name);
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"tool_call_started\",\"id\":{id},\"name\":{name}}},{{\"type\":\"tool_calls_done\",\"calls\":[{{\"id\":{id},\"name\":{name},\"args\":{{\"kind\":\"parsed\",\"value\":{args}}}}}]}},{{\"type\":\"usage\",\"usage\":{USAGE}}},{{\"type\":\"stop\",\"reason\":\"tool_use\"}}]}}"
    )
}

fn text_step(text: &str) -> String {
    let text = json_string(text);
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"text_delta\",\"text\":{text}}},{{\"type\":\"tool_calls_done\",\"calls\":[]}},{{\"type\":\"usage\",\"usage\":{USAGE}}},{{\"type\":\"stop\",\"reason\":\"end_turn\"}}]}}"
    )
}

fn write_plugin(data: &Path) -> Result<(), Box<dyn Error + Send + Sync>> {
    let directory = data.join("plugins").join("subagent");
    std::fs::create_dir_all(&directory)?;
    std::fs::write(
        directory.join("plugin.star"),
        include_str!("../../../examples/plugins/subagent/plugin.star"),
    )?;
    Ok(())
}

async fn host_with_fixture(
    fixture: &str,
    max_depth: u32,
) -> Result<(tempfile::TempDir, tempfile::TempDir, Host), Box<dyn Error + Send + Sync>> {
    let data = tempfile::tempdir()?;
    write_plugin(data.path())?;
    let roots = LoadRoots {
        data_root: data.path().to_path_buf(),
        bundled: Vec::new(),
    };
    let generation = load(&roots, &PluginsConfig::default())?;
    let mut extensions = vec![fixture_exec()?];
    extensions.extend(PluginSystem::new(generation, roots, PluginsConfig::default()).extensions()?);
    let fixture_path = data.path().join("subagent-scripted.jsonl");
    std::fs::write(&fixture_path, fixture)?;
    let user = format!(
        "approval = \"ask\"\nmodel = \"openai/gpt-6\"\n[agents]\nmax_depth = {max_depth}\n[providers.scripted]\nfixture = {:?}\n",
        fixture_path.to_string_lossy()
    );
    let config = Config::load(ConfigProduct::Dalgon, data.path(), "", Some(&user))?;
    let workspace = tempfile::tempdir()?;
    let product = Product {
        name: "dal",
        data_root: data.path().to_path_buf(),
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
    Ok((data, workspace, host))
}

async fn run_agent_turn(
    agent: &Agent,
    prompt: &str,
) -> Result<ToolOutcomeView, Box<dyn Error + Send + Sync>> {
    let mut updates = agent.subscribe(None)?;
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: prompt.into(),
            }],
        })
        .await?;
    let mut settled = None;
    loop {
        let delivery = tokio::time::timeout(Duration::from_secs(15), updates.next())
            .await
            .map_err(|_| std::io::Error::other("scripted subagent turn timed out"))?
            .ok_or_else(|| std::io::Error::other("subagent session closed before turn end"))?;
        let Delivery::Update(update) = delivery else {
            continue;
        };
        match &update.kind {
            UpdateKind::RequestOpened(request) => {
                if agent
                    .view(PageReq::default())?
                    .open
                    .iter()
                    .any(|open| open.id == request.id)
                {
                    agent.answer(request.id, Answer::ApproveForSession).await?;
                }
            }
            UpdateKind::ToolSettled { outcome, .. } => settled = Some(outcome.clone()),
            UpdateKind::TurnEnded { .. } => break,
            _ => {}
        }
    }
    settled.ok_or_else(|| std::io::Error::other("subagent tool did not settle").into())
}

async fn run_turn(
    host: &Host,
    workspace: &Path,
    prompt: &str,
) -> Result<ToolOutcomeView, Box<dyn Error + Send + Sync>> {
    let agent = host
        .open(
            SessionRef::New {
                workspace: Workspace::new(workspace.to_path_buf())?,
                name: None,
            },
            ClientId::new("subagent-port-test"),
        )
        .await?;
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    #[expect(
        clippy::disallowed_methods,
        reason = "the child approval router must run while the parent turn is driven"
    )]
    let controller = tokio::spawn(route_child_approvals(
        host.clone(),
        workspace.to_path_buf(),
        host.subscribe(),
        stop_rx,
    ));
    let outcome = run_agent_turn(&agent, prompt).await;
    let _ = stop_tx.send(());
    let controller_result = controller.await.map_err(|error| {
        std::io::Error::other(format!("child approval controller failed: {error}"))
    })?;
    match (outcome, controller_result) {
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        (Ok(outcome), Ok(())) => Ok(outcome),
    }
}

async fn drain_host_updates(subscription: &mut HostSubscription) -> Vec<HostUpdate> {
    let mut updates = Vec::new();
    while let Ok(Some(update)) =
        tokio::time::timeout(Duration::from_millis(100), subscription.next()).await
    {
        updates.push(update);
    }
    updates
}
async fn approve_child(
    host: Host,
    workspace: std::path::PathBuf,
    child_id: dal_core::SessionId,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let workspace = Workspace::new(workspace)?;
    let child = host
        .open(
            SessionRef::Resume {
                key: child_id.to_string().into(),
                workspace,
            },
            ClientId::new("subagent-child-controller"),
        )
        .await?;
    let mut updates = child.subscribe(None)?;
    for request in child.view(PageReq::default())?.open {
        child.answer(request.id, Answer::ApproveForSession).await?;
    }
    while let Some(delivery) = updates.next().await {
        let Delivery::Update(update) = delivery else {
            continue;
        };
        match &update.kind {
            UpdateKind::RequestOpened(request) => {
                if child
                    .view(PageReq::default())?
                    .open
                    .iter()
                    .any(|open| open.id == request.id)
                {
                    child.answer(request.id, Answer::ApproveForSession).await?;
                }
            }
            UpdateKind::TurnEnded { .. } => break,
            _ => {}
        }
    }
    Ok(())
}

async fn route_child_approvals(
    host: Host,
    workspace: std::path::PathBuf,
    mut host_updates: HostSubscription,
    mut stop: tokio::sync::oneshot::Receiver<()>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut children = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
                _ = &mut stop => {
                    children.abort_all();
                    while children.join_next().await.is_some() {}
                    return Ok(());
                }
                update = host_updates.next() => {
                    let Some(update) = update else {
                        return Ok(());
                    };
                    if let HostUpdate::ChildStarted { child, .. } = update {
                        children.spawn(approve_child(host.clone(), workspace.clone(), child));
                    }
                }
                Some(result) = children.join_next(), if !children.is_empty() => {
                    let child_result = result
                        .map_err(Box::<dyn Error + Send + Sync>::from)
                        .flatten();
                    if let Err(error) = child_result {
                        children.abort_all();
                        while children.join_next().await.is_some() {}
                        return Err(error);
                    }
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn subagent_runs_a_real_child_in_the_parent_workspace()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let args = r#"{"task":"write the shared marker"}"#;
    let command_args =
        r#"{"command":"test -f shared-marker && : > child-saw-marker","timeout_seconds":10}"#;
    let fixture = [
        tool_step("parent-subagent", PLUGIN_TOOL, args),
        tool_step("child-exec", "exec", command_args),
        text_step("child answer from scripted provider"),
        text_step("parent answer"),
    ]
    .join("\n");
    let (_data, workspace, host) = host_with_fixture(&fixture, 1).await?;
    std::fs::write(workspace.path().join("shared-marker"), "marker")?;
    let outcome = run_turn(&host, workspace.path(), "delegate one task").await?;
    assert!(!outcome.is_error, "tool failed: {outcome:?}");
    assert!(
        outcome.text.contains("child answer from scripted provider"),
        "the parent returned the real child report: {outcome:?}"
    );
    assert!(
        workspace.path().join("child-saw-marker").is_file(),
        "the child process used the parent's workspace"
    );
    let _ = host.shutdown(Duration::from_secs(1)).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn subagent_batches_eight_real_children_and_aggregates_every_report()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let tasks = (0..8)
        .map(|index| format!("task-{index}"))
        .collect::<Vec<_>>();
    let args = format!("{{\"tasks\":{}}}", sonic_rs::to_string(&tasks)?);
    let mut steps = vec![tool_step("parent-subagent", PLUGIN_TOOL, &args)];
    for index in 0..8 {
        steps.push(text_step(&format!("child report {index}")));
    }
    steps.push(text_step("parent answer"));
    let fixture = steps.join("\n");
    let (_data, workspace, host) = host_with_fixture(&fixture, 1).await?;
    let mut host_updates = host.subscribe();
    let outcome = run_turn(&host, workspace.path(), "delegate eight tasks").await?;
    let lifecycle = drain_host_updates(&mut host_updates).await;
    assert!(!outcome.is_error, "tool failed: {outcome:?}");
    for index in 0..8 {
        assert!(
            outcome.text.contains(&format!("child report {index}")),
            "missing child report {index}: {outcome:?}"
        );
    }
    let mut started = 0;
    let mut running = 0;
    let mut max_running = 0;
    for update in lifecycle {
        match update {
            HostUpdate::ChildStarted { .. } => {
                started += 1;
                running += 1;
                max_running = max_running.max(running);
            }
            HostUpdate::SessionRemoved { .. } if running > 0 => running -= 1,
            _ => {}
        }
    }
    assert_eq!(started, 8, "every requested child started");
    assert!(
        max_running <= 4,
        "the plugin never has more than four running children: max={max_running}"
    );
    assert_eq!(running, 0, "every completed child is closed");
    let _ = host.shutdown(Duration::from_secs(1)).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn subagent_reports_a_clear_nested_depth_refusal() -> Result<(), Box<dyn Error + Send + Sync>>
{
    let nested_args = r#"{"task":"start a grandchild"}"#;
    let fixture = [
        tool_step("child-subagent", PLUGIN_TOOL, nested_args),
        text_step("child completed after refusal"),
    ]
    .join("\n");
    let (_data, workspace, host) = host_with_fixture(&fixture, 1).await?;
    let root = host
        .open(
            SessionRef::New {
                workspace: Workspace::new(workspace.path().to_path_buf())?,
                name: None,
            },
            ClientId::new("refusal-root"),
        )
        .await?;
    let child = host
        .open(
            SessionRef::Child {
                parent: root.session(),
                call: CallId::new("refusal-call"),
                workspace: Workspace::new(workspace.path().to_path_buf())?,
                name: Some("refusal-child".into()),
            },
            ClientId::new("refusal-child"),
        )
        .await?;
    let outcome = run_agent_turn(&child, "delegate one task").await?;
    assert!(!outcome.is_error, "tool failed: {outcome:?}");
    assert!(
        outcome.text.contains("subagent: child refused: max_depth"),
        "nested start refusal reaches the caller clearly: {outcome:?}"
    );
    let _ = host.shutdown(Duration::from_secs(1)).await;
    Ok(())
}
