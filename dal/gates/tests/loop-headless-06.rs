//! An empty extension list still produces a text-only turn.
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{collections::BTreeMap, error::Error, path::PathBuf, time::Duration};

use dal_agent::{Delivery, Env, SessionRef};
use dal_core::{Command, Config, ConfigProduct, Expect, Part, Reply, Stop, UpdateKind, Workspace};
use support::{TestDir, scripted_session};

#[tokio::test]
async fn empty_extension_list_produces_text_only_turn() -> Result<(), Box<dyn Error + Send + Sync>>
{
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../crates/dalgon/tests/fixtures/replay/stress-scripted.jsonl");
    let factory = dalgon::product();
    let user = format!(
        "model = \"openai-responses/gpt-6\"\n[providers.scripted]\nfixture = {:?}\n",
        fixture.to_string_lossy()
    );
    let config = Config::load(
        ConfigProduct::Dalgon,
        data.path(),
        factory.defaults,
        Some(&user),
    )?;
    let mut product = (factory.build)(&dalgon::BuildCx {
        data_root: data.path().to_path_buf(),
        config: &config,
    })?;
    product.extensions.clear();
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
                text: "Say the scripted word.".into(),
            }],
        })
        .await?;
    assert!(matches!(reply, Reply::Accepted { .. }));
    let mut assistant_text = String::new();
    let mut model_tools = Vec::new();
    while let Some(delivery) = subscription.next().await {
        let Delivery::Update(update) = delivery else {
            continue;
        };
        match &update.kind {
            UpdateKind::Delta {
                channel: dal_core::StreamChannel::Text,
                text,
                ..
            } => {
                assistant_text.push_str(text);
            }
            UpdateKind::ToolStarted { tool, .. } => model_tools.push(tool.to_string()),
            UpdateKind::TurnEnded {
                stop: Stop::EndTurn,
                ..
            } => break,
            _ => {}
        }
    }
    assert_eq!(assistant_text, "scripted assistant response");
    assert!(model_tools.is_empty());
    let _ = harness.host.shutdown(Duration::from_secs(1)).await;
    Ok(())
}
