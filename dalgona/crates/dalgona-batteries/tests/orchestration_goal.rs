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

use sonic_rs::{JsonContainerTrait, JsonValueTrait};

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

const READY_STEP: &str = r#"{"kind":"events","events":[{"type":"text_delta","text":"ready"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}"#;

const TOO_LARGE_STEP: &str =
    r#"{"kind":"fail","message":"Request Entity Too Large","status":413,"family":"openai_chat"}"#;

async fn fixture() -> Result<(TestRoot, Host, PathBuf, dal_agent::Agent), Box<dyn Error>> {
    fixture_with(&[READY_STEP]).await
}

/// Opens a goal session whose scripted provider answers one step per
/// request, then runs the first step through an initial prompt.
async fn fixture_with(
    steps: &[&str],
) -> Result<(TestRoot, Host, PathBuf, dal_agent::Agent), Box<dyn Error>> {
    let root = TestRoot(std::env::temp_dir().join(format!("dalgona-goal-{}", SessionId::new_v7())));
    let data = root.0.join("data");
    let workspace = root.0.join("workspace");
    fs::create_dir_all(&data)?;
    let fixture = data.join("script.jsonl");
    fs::write(&fixture, steps.join("\n"))?;
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

#[tokio::test]
async fn clear_replaces_a_damaged_goal_file_with_the_recovery_document()
-> Result<(), Box<dyn Error>> {
    let (root, host, workspace, agent) = fixture().await?;
    let session = agent.view(dal_core::PageReq::default())?.session.id;
    output_text(run_goal(&agent, "write the parser").await?)?;
    let goal_file = find_session_dir(&root.0.join("data"), &session)?
        .join("sidecar")
        .join("orchestration")
        .join("goal.json");
    host.close(session).await?;
    // Damage only the goal status; `next_goal` stays readable for salvage.
    let bytes = fs::read_to_string(&goal_file)?;
    let damaged = bytes.replace("\"status\":\"active\"", "\"status\":\"running\"");
    assert_ne!(bytes, damaged, "the damage did not change the document");
    fs::write(&goal_file, damaged)?;
    let reopened = host
        .open(
            SessionRef::Resume {
                key: session.to_string().into(),
                workspace: Workspace::new(workspace.clone())?,
            },
            ClientId::new("goal-recovery"),
        )
        .await?;
    // The damaged document arms nothing and refuses every goal operation.
    let error = run_goal(&reopened, "").await.expect_err("show must fail");
    assert!(
        error
            .to_string()
            .contains("goal: the goal file is damaged:"),
        "{error}"
    );
    // Only the person's `/goal clear` replaces the document.
    let cleared = output_text(run_goal(&reopened, "clear").await?)?;
    assert_eq!(cleared, "No goal.");
    host.close(session).await?;
    let repaired = host
        .open(
            SessionRef::Resume {
                key: session.to_string().into(),
                workspace: Workspace::new(workspace.clone())?,
            },
            ClientId::new("goal-repaired"),
        )
        .await?;
    let shown = output_text(run_goal(&repaired, "").await?)?;
    assert_eq!(shown, "No goal.");
    let recovered = fs::read_to_string(&goal_file)?;
    let doc: sonic_rs::Value = sonic_rs::from_str(recovered.trim_end())?;
    let object = doc
        .as_object()
        .ok_or_else(|| format!("recovery document is not an object: {recovered}"))?;
    assert_eq!(
        object.get(&"next_goal").and_then(sonic_rs::Value::as_u64),
        Some(2),
        "{recovered}"
    );
    assert!(object.get(&"goal").is_some_and(sonic_rs::Value::is_null));
    // The salvaged counter keeps session-local ids stable across the repair.
    let created = output_text(run_goal(&repaired, "parse faster").await?)?;
    assert!(created.contains("goal g2: active"), "{created}");
    host.close(session).await?;
    host.shutdown(Duration::from_secs(5)).await;
    Ok(())
}

#[tokio::test]
async fn an_overflowed_turn_blocks_the_goal_through_the_real_host() -> Result<(), Box<dyn Error>> {
    let (_root, host, _workspace, agent) = fixture_with(&[READY_STEP, TOO_LARGE_STEP]).await?;
    let created = output_text(run_goal(&agent, "write the parser").await?)?;
    assert!(created.contains("goal g1: active"), "{created}");
    let mut subscription = agent.subscribe(None)?;
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "keep going".into(),
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
    assert!(ended, "the rejected request did not end the turn");
    drop(subscription);
    // The turn-end hook may run just after the turn-ended update.
    let mut shown = String::new();
    for _ in 0..50 {
        shown = output_text(run_goal(&agent, "").await?)?;
        if shown.contains("blocked:") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        shown.contains("blocked: context overflow ended the turn (compaction did not recover)"),
        "{shown}"
    );
    host.shutdown(Duration::from_secs(5)).await;
    Ok(())
}
