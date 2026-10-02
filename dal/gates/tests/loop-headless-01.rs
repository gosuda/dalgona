//! Headless tool calls preserve order and see the applied patch.
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{collections::BTreeMap, error::Error, fs, path::PathBuf};

use dal_agent::{Delivery, Env, SessionRef};
use dal_core::{Command, Config, ConfigProduct, Expect, Part, Reply, Stop, UpdateKind, Workspace};
use support::{TestDir, scripted_session};

#[expect(
    clippy::too_many_lines,
    reason = "SC headless scenario is one long script"
)]
#[tokio::test]
async fn headless_tools_preserve_call_order_and_see_patch()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    fs::write(workspace.path().join("test.txt"), "before\n")?;
    let fixtures =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates/dalgon/tests/fixtures");
    fs::copy(
        fixtures.join("process/grandchild.sh"),
        workspace.path().join("grandchild.sh"),
    )?;
    let replay_fixture = fixtures.join("replay/loop-headless.jsonl");
    let factory = dalgon::product();
    let user = format!(
        "model = \"openai-responses/gpt-6\"\nedit_style = \"replace\"\n[providers.scripted]\nfixture = {:?}\n",
        replay_fixture.to_string_lossy()
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
    let reply = harness
        .agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Read test.txt, replace before with after, read it again, then run the grandchild fixture.".into(),
            }],
        })
        .await?;
    assert!(matches!(reply, Reply::Accepted { .. }));

    let mut started = Vec::<(String, String)>::new();
    let mut settled = Vec::<String>::new();
    let mut read_results = Vec::new();
    let mut ended = false;
    while let Some(delivery) = subscription.next().await {
        let Delivery::Update(update) = delivery else {
            continue;
        };
        match &update.kind {
            UpdateKind::ToolStarted { call, tool, .. } => {
                started.push((call.as_str().to_owned(), tool.to_string()));
            }
            UpdateKind::ToolSettled { call, outcome } => {
                let name = started
                    .iter()
                    .find(|(started_call, _)| started_call == &call.as_str().to_owned())
                    .map_or("missing-start", |(_, name)| name.as_str());
                settled.push(name.to_owned());
                if name == "read" {
                    read_results.push(sonic_rs::to_string(outcome)?);
                }
            }
            UpdateKind::TurnEnded {
                stop: Stop::EndTurn,
                ..
            } => {
                ended = true;
                break;
            }
            _ => {}
        }
    }

    assert_eq!(
        started
            .iter()
            .map(|(_, name)| name.as_str())
            .collect::<Vec<_>>(),
        ["read", "patch", "read", "exec"]
    );
    assert_eq!(settled, ["read", "patch", "read", "exec"]);
    assert!(
        read_results
            .get(1)
            .is_some_and(|result| result.contains("after"))
    );
    assert_eq!(
        fs::read_to_string(workspace.path().join("test.txt"))?,
        "after\n"
    );
    assert!(ended, "the turn must reach one terminal update");
    let report = harness
        .host
        .shutdown(std::time::Duration::from_secs(1))
        .await;
    assert_eq!(report.sessions_closed, 1);
    Ok(())
}
