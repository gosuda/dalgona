//! The Starlark Fusion model runs its panels and preserves session calls.

#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{error::Error, fs, path::PathBuf, time::Duration};

use dal_agent::{Env, SessionRef};
use dal_core::{
    Command, Config, ConfigProduct, Expect, Part, Reply, StreamChannel, UpdateKind, Workspace,
};
use support::{TestDir, scripted_session};

#[expect(
    clippy::too_many_lines,
    reason = "SC fusion scenario is one long script"
)]
#[tokio::test]
async fn scripted_fusion_model_runs_panel_and_forwards_session_call()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    fs::write(workspace.path().join("fusion.txt"), "session file")?;
    let fixture_root =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates/dalgon/tests/fixtures");
    let plugin_dir = data.path().join("plugins/fusion");
    fs::create_dir_all(&plugin_dir)?;
    fs::copy(
        fixture_root.join("plugins/fusion.star"),
        plugin_dir.join("plugin.star"),
    )?;
    let replay = fixture_root.join("replay/fusion-scripted.jsonl");
    let factory = dalgon::product();
    let user = format!(
        "model = \"dalgona/fusion\"\nplugins = [\"fusion\"]\n[providers.scripted]\nfixture = {:?}\n[prices.\"claude-opus-5\"]\ninput = 1.0\ncached_input = 1.0\noutput = 1.0\nreasoning = 1.0\n[prices.\"gpt-6\"]\ninput = 1.0\ncached_input = 1.0\noutput = 1.0\nreasoning = 1.0\n[prices.\"gemini-3-pro\"]\ninput = 1.0\ncached_input = 1.0\noutput = 1.0\nreasoning = 1.0\n",
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
    let fusion = product
        .extensions
        .iter()
        .find(|extension| extension.name() == "fusion")
        .expect("fusion.star loaded");
    assert!(
        fusion
            .models()
            .iter()
            .any(|model| model.id.as_str() == "dalgona/fusion"),
        "fusion.star did not register the dalgona/fusion model"
    );
    assert!(
        fusion
            .tools()
            .iter()
            .any(|(tool, _)| tool.name().as_str() == "fusion__fusion"),
        "fusion.star did not register its private tool"
    );
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
    let prompt = harness
        .agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Analyze the file with three panels.".into(),
            }],
        })
        .await?;
    assert!(matches!(prompt, Reply::Accepted { .. }));
    let mut assistant_text = String::new();
    let mut session_calls = Vec::new();
    let mut grants = 0;
    loop {
        let delivery = tokio::time::timeout(Duration::from_secs(30), subscription.next())
            .await
            .expect("turn update arrives within 30s");
        let Some(delivery) = delivery else {
            panic!("subscription closed before the turn ended");
        };
        let dal_agent::Delivery::Update(update) = delivery else {
            continue;
        };
        match &update.kind {
            UpdateKind::Delta {
                channel: StreamChannel::Text,
                text,
                ..
            } => assistant_text.push_str(text),
            UpdateKind::ToolStarted { call, tool, args } => session_calls.push((
                call.as_str().to_owned(),
                tool.to_string(),
                args.as_str().to_owned(),
            )),
            UpdateKind::RequestOpened(request) => {
                assert!(
                    matches!(
                        &request.question,
                        dal_core::Question::Grant { ext, capabilities, .. }
                            if ext.as_ref() == "fusion"
                                && capabilities.iter().any(|c| c.as_ref() == "infer")
                    ),
                    "unexpected request: {:?}",
                    request.question
                );
                grants += 1;
                harness
                    .agent
                    .answer(request.id, dal_core::Answer::ApproveForSession)
                    .await?;
            }
            UpdateKind::TurnEnded { .. } => break,
            _ => {}
        }
    }
    assert_eq!(grants, 1, "the fusion infer grant must open exactly once");
    let view = harness
        .agent
        .view(dal_core::PageReq::default())
        .expect("session view");
    let view_text = format!("{view:?}");
    assert_eq!(assistant_text, "fusion complete", "{view_text}");
    assert_eq!(session_calls.len(), 1);
    let session_call = session_calls.pop().unwrap();
    assert_eq!(session_call.0, "fusion-session-call");
    assert_eq!(session_call.1, "read");
    assert_eq!(session_call.2, r#"{"path":"fusion.txt"}"#);
    let _ = harness.host.shutdown(Duration::from_secs(1)).await;
    Ok(())
}
