#![expect(clippy::unwrap_used, reason = "SC test")]
#![expect(clippy::expect_used, reason = "SC test")]
#![cfg_attr(
    unix,
    expect(
        clippy::disallowed_methods,
        reason = "SC test runs the real dalgon binary"
    )
)]
//! Forces a lagging subscriber to resync and verifies TUI recovery.

#[cfg(unix)]
#[expect(
    dead_code,
    reason = "PTY support includes helpers used by other gate targets"
)]
#[path = "support/pty.rs"]
mod pty;
mod support;

use std::{collections::BTreeMap, error::Error, time::Duration};

use dal_agent::{Delivery, Env, SessionRef};
use dal_core::{
    Command, Config, ConfigProduct, Expect, PageReq, Part, Reply, TurnState, Workspace,
};
use support::{TestDir, scripted_session};

#[tokio::test]
async fn tui_resync_recovers_lagging_subscriber() -> Result<(), Box<dyn Error + Send + Sync>> {
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    let fixture_path = data.path().join("resync.jsonl");
    let fixture = replay_fixture();
    std::fs::write(&fixture_path, &fixture)?;
    let factory = dalgon::product();
    let user = format!(
        "model = \"openai-responses/gpt-6\"\n[providers.scripted]\nfixture = {:?}\n",
        fixture_path.to_string_lossy()
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
    let baseline = harness.agent.view(PageReq::default())?;
    let mut lagging = harness
        .agent
        .subscribe(Some((baseline.r#gen, baseline.seq)))?;
    let first = harness
        .agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "overflow the subscriber queue".into(),
            }],
        })
        .await?;
    assert!(matches!(first, Reply::Accepted { .. }));
    wait_for_idle(&harness.agent).await?;
    let final_view = harness.agent.view(PageReq::default())?;
    let final_entry = final_view.entries.items.last().unwrap();
    assert!(matches!(
        &final_entry.kind,
        dal_core::EntryKind::Assistant { .. }
    ));

    let (generation, sequence) = loop {
        let delivery = tokio::time::timeout(Duration::from_secs(20), lagging.next())
            .await?
            .expect("the lagging subscriber produces its terminal Resync delivery");
        if let Delivery::Resync { generation, seq } = delivery {
            break (generation, seq);
        }
    };
    assert_eq!(generation, final_view.r#gen);
    assert!(sequence <= final_view.seq);

    let rebuilt = harness.agent.view(PageReq::default())?;
    let mut recovered = harness
        .agent
        .subscribe(Some((rebuilt.r#gen, rebuilt.seq)))?;
    let second = harness
        .agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "continue after resync".into(),
            }],
        })
        .await?;
    assert!(matches!(second, Reply::Accepted { .. }));
    let delivery = tokio::time::timeout(Duration::from_secs(10), recovered.next())
        .await?
        .expect("the reattached subscriber receives the first post-view update");
    let Delivery::Update(update) = delivery else {
        return Err("fresh subscription unexpectedly required another resync".into());
    };
    assert_eq!(update.seq.get(), rebuilt.seq.get() + 1);
    wait_for_idle(&harness.agent).await?;

    #[cfg(unix)]
    tokio::task::spawn_blocking(move || verify_real_tui_rebuild(&fixture)).await??;
    let _ = harness.host.shutdown(Duration::from_secs(1)).await;
    Ok(())
}

async fn wait_for_idle(agent: &dal_agent::Agent) -> Result<(), Box<dyn Error + Send + Sync>> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if matches!(
                agent.view(PageReq::default()).map(|view| view.turn)?,
                TurnState::Idle
            ) {
                return Ok::<(), dal_agent::AgentError>(());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await??;
    Ok(())
}

fn replay_fixture() -> String {
    let mut fixture = String::from("{\"kind\":\"events\",\"events\":[");
    for index in 0..1_200 {
        if index != 0 {
            fixture.push(',');
        }
        fixture.push_str("{\"type\":\"text_delta\",\"text\":\"x\"}");
    }
    fixture.push_str(
        ",{\"type\":\"text_delta\",\"text\":\"RESYNC-RECOVERED\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n",
    );
    fixture.push_str(
        "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"after resync\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n",
    );
    fixture
}

#[cfg(unix)]
fn verify_real_tui_rebuild(fixture: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
    use std::time::Duration;

    use pty::{PtyProcess, dalgon_command_with_fixture};

    let dir = TestDir::new()?;
    let mut command = dalgon_command_with_fixture(dir.path(), fixture)?;
    command.args(["--screen", "inline"]);
    let mut terminal = PtyProcess::spawn(&mut command, 80, 24)?;
    terminal.wait_for(
        dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(),
        Duration::from_secs(10),
    )?;
    terminal.write(b"render through the real TUI\r")?;
    terminal.wait_for(b"RESYNC-RECOVERED", Duration::from_secs(20))?;
    terminal.wait_for_count(b"enter send", 2, Duration::from_secs(20))?;
    terminal.write(b"\x04")?;
    let status = terminal.wait_for_exit(Duration::from_secs(5))?;
    assert!(status.success(), "dalgon exited with {status}");
    Ok(())
}
