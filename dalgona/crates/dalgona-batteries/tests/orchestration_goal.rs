// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Real-session tests for the orchestration goal sidecar.

use dal_agent::ext::grants::{GrantKey, GrantStore};
use dal_agent::{Delivery, Env, Host, Product, SessionRef};
use dal_core::{
    ClientId, Command, Config, ConfigProduct, Expect, Name, Origin, Output, Part, Reply, SessionId,
    Timestamp, UpdateKind, Workspace,
};
use dalgona_batteries::orchestration::{
    BatteryConfig, OrchestrationAgentsConfig, OrchestrationConfig, OrchestrationMonitorConfig,
    orchestration,
};
use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn goal_config() -> OrchestrationConfig {
    OrchestrationConfig {
        loop_guard: BatteryConfig { enabled: false },
        sleep: BatteryConfig { enabled: false },
        monitor: OrchestrationMonitorConfig::default(),
        inflight: BatteryConfig { enabled: false },
        goal: BatteryConfig { enabled: true },
        arbiter: BatteryConfig { enabled: false },
        agents: OrchestrationAgentsConfig::default(),
        isolation: BatteryConfig { enabled: false },
        workflows: None,
        data_root: None,
    }
}

struct TestRoot(PathBuf);

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

async fn fixture() -> Result<(TestRoot, Host, PathBuf, dal_agent::Agent), Box<dyn Error>> {
    let root = TestRoot(std::env::temp_dir().join(format!("dalgona-goal-{}", SessionId::new_v7())));
    let data = root.0.join("data");
    let workspace = root.0.join("workspace");
    fs::create_dir_all(&data)?;
    let fixture = data.join("script.jsonl");
    fs::write(
        &fixture,
        r#"{"kind":"events","events":[{"type":"text_delta","text":"ready"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}"#,
    )?;
    let user = format!(
        "model = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.display()
    );
    let config = Config::load(ConfigProduct::Dalgona, &data, "", Some(&user))?;
    let mut orchestration_config = goal_config();
    orchestration_config.data_root = Some(data.clone());
    let extension = orchestration(orchestration_config)?;
    GrantStore::new(data.clone())
        .grant(
            GrantKey {
                extension: Name::parse("orchestration")?,
                origin: Origin::Bundled,
                services: extension.inject().capabilities(),
            },
            ClientId::new("goal-test"),
            Timestamp::now(),
        )
        .await?;
    let product = Product {
        name: "dalgona",
        data_root: data.clone(),
        defaults: "",
        extensions: vec![extension],
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
            SessionRef::New {
                workspace: Workspace::new(workspace.clone())?,
                name: None,
            },
            ClientId::new("goal-test"),
        )
        .await?;
    let mut subscription = agent.subscribe(None)?;
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "initialize the session".into(),
            }],
        })
        .await?;
    assert!(matches!(reply, Reply::Accepted { .. }), "{reply:?}");
    let mut ended = false;
    for _ in 0..20 {
        let delivery = tokio::time::timeout(Duration::from_secs(30), subscription.next()).await?;
        let Some(delivery) = delivery else {
            break;
        };
        if let Delivery::Update(update) = delivery
            && matches!(update.kind, UpdateKind::TurnEnded { .. })
        {
            ended = true;
            break;
        }
    }
    assert!(ended, "the scripted prompt did not finish the turn");
    drop(subscription);
    Ok((root, host, workspace, agent))
}

async fn run_goal(agent: &dal_agent::Agent, args: &str) -> Result<Reply, Box<dyn Error>> {
    Ok(agent
        .submit(Command::Run {
            name: "goal".into(),
            args: args.into(),
            expected: None,
        })
        .await?)
}

fn output_text(reply: Reply) -> Result<String, Box<dyn Error>> {
    let Reply::Done(Output::Text(text)) = reply else {
        return Err(format!("unexpected goal reply: {reply:?}").into());
    };
    Ok(text.to_string())
}

fn find_session_dir(data: &Path, session: &dal_core::SessionId) -> Result<PathBuf, Box<dyn Error>> {
    let sessions = data.join("sessions");
    for workspace in fs::read_dir(sessions)? {
        let workspace = workspace?.path();
        let candidate = workspace.join(session.to_string());
        if candidate.is_dir() {
            return Ok(candidate);
        }
    }
    Err("session directory was not created".into())
}

#[tokio::test]
async fn goal_sidecar_survives_session_reopen() -> Result<(), Box<dyn Error>> {
    let (_root, host, workspace, agent) = fixture().await?;
    let session = agent.view(dal_core::PageReq::default())?.session.id;
    let created = output_text(run_goal(&agent, "write the parser").await?)?;
    assert!(created.contains("goal g1: active"), "{created}");
    host.close(session).await?;
    let reopened = host
        .open(
            SessionRef::Resume {
                key: session.to_string().into(),
                workspace: Workspace::new(workspace)?,
            },
            ClientId::new("goal-reopen"),
        )
        .await?;
    let shown = output_text(run_goal(&reopened, "").await?)?;
    assert!(shown.contains("write the parser"), "{shown}");
    host.shutdown(Duration::from_secs(5)).await;
    Ok(())
}

#[tokio::test]
async fn failed_goal_sidecar_write_is_reported() -> Result<(), Box<dyn Error>> {
    let (root, host, workspace, agent) = fixture().await?;
    let session = agent.view(dal_core::PageReq::default())?.session.id;
    output_text(run_goal(&agent, "write the parser").await?)?;
    let directory = find_session_dir(&root.0.join("data"), &session)?;
    let sidecar_dir = directory.join("sidecar").join("orchestration");
    fs::remove_dir_all(&sidecar_dir)?;
    fs::write(&sidecar_dir, b"blocked")?;
    let error = run_goal(&agent, "pause")
        .await
        .expect_err("write must fail");
    assert!(
        error.to_string().contains("goal: saving the goal failed:"),
        "{error}"
    );
    host.close(session).await?;
    host.shutdown(Duration::from_secs(5)).await;
    let _ = workspace;
    Ok(())
}
