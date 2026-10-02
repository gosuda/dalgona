//! Unknown update variants map to fallbacks while the turn continues.
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{collections::BTreeMap, error::Error, path::PathBuf};

use dal_agent::{Delivery, Env, SessionRef};
use dal_core::{
    Command, Config, ConfigProduct, Expect, Part, Reply, Stop, Update, UpdateKind, Workspace,
};
use support::{TestDir, scripted_session};

#[tokio::test]
async fn unknown_update_variant_maps_to_fallback_and_turn_continues()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let unknown: Update = sonic_rs::from_str(
        r#"{"gen":1,"seq":4,"kind":{"type":"future_update","payload":{"value":7}}}"#,
    )?;
    assert!(matches!(unknown.kind, UpdateKind::Unknown));

    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../crates/dalgon/tests/fixtures/replay/stress-scripted.jsonl");
    let factory = dalgon::product();
    let user = format!(
        "model = \"openai/gpt-6\"\n[providers.scripted]\nfixture = {:?}\n",
        fixture.to_string_lossy()
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
                text: "continue after an unknown update".into(),
            }],
        })
        .await?;
    assert!(matches!(reply, Reply::Accepted { .. }));
    let mut ended = false;
    while let Some(delivery) = subscription.next().await {
        if let Delivery::Update(update) = delivery
            && matches!(
                update.kind,
                UpdateKind::TurnEnded {
                    stop: Stop::EndTurn,
                    ..
                }
            )
        {
            ended = true;
            break;
        }
    }
    assert!(ended);
    let _ = harness
        .host
        .shutdown(std::time::Duration::from_secs(1))
        .await;
    Ok(())
}
