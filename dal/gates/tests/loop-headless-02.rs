//! Cancelling mid-exec kills the process tree and ends exactly once.
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{error::Error, fs, path::PathBuf, time::Duration};

use dal_agent::{Delivery, Env, SessionRef};
use dal_core::{
    CancelScope, Command, Config, ConfigProduct, Expect, Part, Reply, Stop, UpdateKind, Workspace,
};
use support::{TestDir, scripted_session};

#[tokio::test]
async fn cancel_mid_exec_kills_grandchild_and_ends_once() -> Result<(), Box<dyn Error + Send + Sync>>
{
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    let fixtures =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates/dalgon/tests/fixtures");
    fs::copy(
        fixtures.join("process/grandchild.sh"),
        workspace.path().join("grandchild.sh"),
    )?;
    let replay = fixtures.join("replay/loop-headless.jsonl");
    let factory = dalgon::product();
    let user = format!(
        "model = \"openai-responses/gpt-6\"\napproval = \"all\"\n[providers.scripted]\nfixture = {:?}\n",
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
        vars: support::captured_shell_vars(),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    let session = SessionRef::Ephemeral {
        workspace: Workspace::new(workspace.path().to_path_buf())?,
    };
    let harness = scripted_session(product, config, env, session).await?;
    let mut subscription = harness.agent.subscribe(None)?;
    let Reply::Accepted { turn, .. } = harness
        .agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Run the grandchild fixture and wait.".into(),
            }],
        })
        .await?
    else {
        return Err("the scripted prompt was not accepted".into());
    };

    let pid_file = workspace.path().join("grandchild.pid");
    let mut cancelled = false;
    let mut turn_ends = 0;
    while let Some(delivery) = subscription.next().await {
        let Delivery::Update(update) = delivery else {
            continue;
        };
        match &update.kind {
            UpdateKind::ToolStarted { tool, .. } if tool.as_ref() == "exec" => {
                tokio::time::timeout(Duration::from_secs(2), async {
                    while !pid_file.exists() {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await?;
                harness
                    .agent
                    .submit(Command::Cancel {
                        scope: CancelScope::Turn(turn),
                    })
                    .await?;
                cancelled = true;
            }
            UpdateKind::TurnEnded { stop, .. } => {
                turn_ends += 1;
                assert_eq!(*stop, Stop::Cancelled);
                break;
            }
            _ => {}
        }
    }
    assert!(cancelled, "exec must have started before cancellation");
    assert_eq!(turn_ends, 1);
    let pid = fs::read_to_string(pid_file)?.trim().parse::<u32>()?;
    assert!(!support::process_alive(pid));
    let _ = harness.host.shutdown(Duration::from_secs(1)).await;
    Ok(())
}
