//! A failed plugin reload leaves the validated generation callable.

mod support;

use std::{collections::BTreeMap, error::Error, fs, path::PathBuf};

use dal_agent::{Delivery, Env, SessionRef};
use dal_core::{Command, Config, ConfigProduct, Expect, Part, Reply, UpdateKind, Workspace};
use support::{TestDir, scripted_session};

const FOCUS_RESPONSE: &str = r#"{"kind":"events","events":[{"type":"tool_call_started","id":"focus-call","name":"focus__focus"},{"type":"tool_calls_done","calls":[{"id":"focus-call","name":"focus__focus","args":{"kind":"parsed","value":{"value":"still-live"}}}]},{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"tool_use"}]}
{"kind":"events","events":[{"type":"text_delta","text":"focus is still callable"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}"#;

#[tokio::test]
async fn failed_reload_preserves_live_generation() -> Result<(), Box<dyn Error + Send + Sync>> {
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    let plugin_dir = data.path().join("plugins/focus");
    fs::create_dir_all(&plugin_dir)?;
    let fixture_root =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates/dalgon/tests/fixtures");
    fs::copy(
        fixture_root.join("plugins/focus.star"),
        plugin_dir.join("plugin.star"),
    )?;
    let replay = data.path().join("focus-scripted.jsonl");
    fs::write(&replay, FOCUS_RESPONSE)?;
    let factory = dalgon::product();
    let user = format!(
        "model = \"openai/gpt-6\"\nplugins = [\"focus\"]\n[providers.scripted]\nfixture = {:?}\n",
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
        vars: BTreeMap::new(),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    let session = SessionRef::Ephemeral {
        workspace: Workspace::new(workspace.path().to_path_buf())?,
    };
    let harness = scripted_session(product, config, env, session).await?;
    fs::copy(
        fixture_root.join("plugins/broken-reload.star"),
        plugin_dir.join("plugin.star"),
    )?;

    let reload = harness
        .agent
        .submit(Command::Run {
            name: "reload".into(),
            args: String::new().into(),
            expected: None,
        })
        .await;
    let Err(error) = reload else {
        panic!("reload with malformed plugin source unexpectedly succeeded");
    };
    assert!(
        error
            .to_string()
            .contains("Parse error: unexpected symbol ':'"),
        "{error}"
    );
    let mut subscription = harness.agent.subscribe(None)?;
    let prompt = harness
        .agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Call focus with value still-live.".into(),
            }],
        })
        .await?;
    assert!(matches!(prompt, Reply::Accepted { .. }));
    let mut focus_calls = 0;
    while let Some(delivery) = subscription.next().await {
        let Delivery::Update(update) = delivery else {
            continue;
        };
        if matches!(&update.kind, UpdateKind::ToolStarted { tool, .. } if tool.as_ref() == "focus__focus")
        {
            focus_calls += 1;
        }
        if matches!(update.kind, UpdateKind::TurnEnded { .. }) {
            break;
        }
    }
    assert_eq!(focus_calls, 1);
    let _ = harness
        .host
        .shutdown(std::time::Duration::from_secs(1))
        .await;
    Ok(())
}
