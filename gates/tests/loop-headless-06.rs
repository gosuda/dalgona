//! An empty extension list still produces a text-only turn.
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{error::Error, path::PathBuf, time::Duration};

use dal_agent::{Delivery, Env, SessionRef};
use dal_core::{
    Command, Config, ConfigProduct, EntryKind, Expect, Part, Reply, Stop, UpdateKind, Workspace,
};
use support::{TestDir, scripted_session};

#[tokio::test]
async fn empty_extension_list_produces_text_only_turn() -> Result<(), Box<dyn Error + Send + Sync>>
{
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../cli/dalgon/tests/fixtures/replay/stress-scripted.jsonl");
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
        vars: support::captured_shell_vars(),
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
    assert_eq!(model_tools, [] as [std::string::String; 0]);
    let _ = harness.host.shutdown(Duration::from_secs(1)).await;
    Ok(())
}

fn exec_then_text_script(marker: &std::path::Path) -> Result<String, Box<dyn Error + Send + Sync>> {
    let command = format!("touch '{}'", marker.display());
    let usage = sonic_rs::json!({
        "input_tokens": 1, "cached_input_tokens": 0, "output_tokens": 1,
        "reasoning_tokens": null, "cache_write_tokens": 0, "cost_usd": null
    });
    let call = sonic_rs::json!({
        "kind": "events",
        "events": [
            {"type": "tool_call_started", "id": "call-exec", "name": "exec"},
            {"type": "tool_calls_done", "calls": [{
                "id": "call-exec", "name": "exec",
                "args": {"kind": "parsed", "value": {"command": command, "timeout_seconds": 30}}
            }]},
            {"type": "usage", "usage": usage},
            {"type": "stop", "reason": "tool_use"}
        ]
    });
    let answer = sonic_rs::json!({
        "kind": "events",
        "events": [
            {"type": "text_delta", "text": "after the tool"},
            {"type": "tool_calls_done", "calls": []},
            {"type": "usage", "usage": usage},
            {"type": "stop", "reason": "end_turn"}
        ]
    });
    Ok(format!(
        "{}\n{}\n",
        sonic_rs::to_string(&call)?,
        sonic_rs::to_string(&answer)?
    ))
}

/// Runs the same exec-calling script once; `clear` empties the product's
/// extension list first. Returns whether the exec tool started, the `error`
/// flag of every tool-result entry the turn recorded, and whether the command
/// left its marker file.
async fn exec_turn(
    clear: bool,
) -> Result<(bool, Vec<bool>, bool, Vec<String>), Box<dyn Error + Send + Sync>> {
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    let marker = workspace.path().join("exec-ran");
    let script = data.path().join("exec-script.jsonl");
    std::fs::write(&script, exec_then_text_script(&marker)?)?;
    let factory = dalgon::product();
    let user = format!(
        "model = \"openai-responses/gpt-6\"\napproval = \"all\"\n[providers.scripted]\nfixture = {:?}\n",
        script.to_string_lossy()
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
    if clear {
        product.extensions.clear();
    }
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
    let reply = harness
        .agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Run the marker command.".into(),
            }],
        })
        .await?;
    assert!(matches!(reply, Reply::Accepted { .. }));
    let mut started = false;
    let mut tool_result_errors = Vec::new();
    let mut kinds = Vec::new();
    while let Some(delivery) =
        tokio::time::timeout(Duration::from_secs(30), subscription.next()).await?
    {
        let Delivery::Update(update) = delivery else {
            continue;
        };
        kinds.push(
            format!("{:?}", update.kind)
                .chars()
                .take(160)
                .collect::<String>(),
        );
        match &update.kind {
            UpdateKind::ToolStarted { tool, .. } if tool.as_ref() == "exec" => started = true,
            UpdateKind::Tree(delta) => {
                for entry in &delta.added {
                    if let EntryKind::ToolResult { error, .. } = &entry.kind {
                        tool_result_errors.push(*error);
                    }
                }
            }
            UpdateKind::TurnEnded { .. } => break,
            _ => {}
        }
    }
    let ran = marker.exists();
    let _ = harness.host.shutdown(Duration::from_secs(1)).await;
    Ok((started, tool_result_errors, ran, kinds))
}

/// The empty extension list is what removes the tools, not the fixture: the
/// identical script that runs `exec` with the product's extensions fails the
/// call and runs nothing once they are cleared.
#[tokio::test]
async fn clearing_the_extension_list_removes_the_exec_tool_the_script_calls()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let (started, tool_errors, ran, kinds) = exec_turn(false).await?;
    assert!(
        started,
        "control: the product's exec tool must start: {kinds:#?}"
    );
    assert_eq!(
        tool_errors,
        [false],
        "control: exec must record one clean result"
    );
    assert!(ran, "control: the command must run and leave its marker");

    let (started, tool_errors, ran, kinds) = exec_turn(true).await?;
    assert!(
        !ran,
        "with no extensions the exec command must never run (started: {started}): {kinds:#?}"
    );
    assert_eq!(
        tool_errors,
        [true],
        "with no extensions the exec call must record one error result: {kinds:#?}"
    );
    Ok(())
}
