// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate: the goal and continuation commands
//! answer through the real host with their exact refusal texts, so a
//! wording change fails here instead of drifting on users.
use dalgona_gates::support;

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

struct TestRoot(PathBuf);

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn goal_commands_answer_with_exact_texts() -> support::TestResult<()> {
    let root =
        TestRoot(std::env::temp_dir().join(format!("dalgona-goal-cmd-{}", uuid::Uuid::now_v7())));
    let data = root.0.join("data");
    let workspace = root.0.join("workspace");
    fs::create_dir_all(&data)?;
    let fixture = data.join("script.jsonl");
    fs::write(&fixture, support::text_step("ready"))?;
    let user = format!(
        "model = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.display()
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let product = support::build_product(data.clone(), Some(&user))?;
        for extension in &product.extensions {
            let services = extension.inject().capabilities();
            if services.is_empty() || extension.origin() == dal_core::Origin::Builtin {
                continue;
            }
            dal_agent::ext::grants::GrantStore::new(data.clone())
                .grant(
                    dal_agent::ext::grants::GrantKey::for_extension(extension, &workspace)
                        .expect("registered extension has a code identity"),
                    dal_core::ClientId::new("goal-cmd-gate"),
                    dal_core::Timestamp::now(),
                )
                .await?;
        }
        drop(product);
        let host = support::start_dalgona_with_config(data.clone(), Some(&user)).await?;
        let agent = host
            .open(
                dal_agent::SessionRef::New {
                    workspace: dal_core::Workspace::new(workspace)?,
                    name: None,
                },
                dal_core::ClientId::new("goal-cmd-gate"),
            )
            .await?;

        // Empty show on a fresh session.
        assert_eq!(run_command(&agent, "goal", "").await?, "No goal.");
        // Clearing an absent goal keeps the same answer.
        assert_eq!(run_command(&agent, "goal", "clear").await?, "No goal.");

        // Creation, then a second create replaces the goal: the command
        // door creates or replaces, while only the model tool refuses.
        assert!(
            run_command(&agent, "goal", "fix the flaky test")
                .await?
                .contains("goal g1: active")
        );
        let replaced = run_command(&agent, "goal", "another objective").await?;
        assert!(
            replaced.contains("goal g2: active"),
            "second create did not replace the goal: {replaced}"
        );

        // Pause and resume through both doors, exact texts.
        assert_eq!(
            run_command(&agent, "goal", "pause").await?,
            "goal g2 paused."
        );
        assert!(
            run_command(&agent, "goal", "resume")
                .await?
                .starts_with("goal g2 is active."),
            "resume reply drifted"
        );
        assert_eq!(
            run_command(&agent, "continuation", "pause").await?,
            "automatic turns: paused (paused by the user)"
        );
        assert_eq!(
            run_command(&agent, "continuation", "run").await?,
            "automatic turns: run"
        );

        // Unknown command text fails with the typed error, not a panic.
        let unknown = agent
            .submit(dal_core::Command::Run {
                name: "no_such_command".into(),
                args: String::new().into(),
                expected: None,
            })
            .await;
        assert!(unknown.is_err(), "unknown commands must fail");

        let report = host.shutdown(Duration::from_secs(5)).await;
        assert_eq!(report.tasks_remaining, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}

async fn run_command(
    agent: &dal_agent::Agent,
    name: &str,
    args: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let reply = agent
        .submit(dal_core::Command::Run {
            name: name.into(),
            args: args.into(),
            expected: None,
        })
        .await?;
    let dal_core::Reply::Done(output) = reply else {
        return Err(format!("unexpected command reply: {reply:?}").into());
    };
    let dal_core::Output::Text(text) = output else {
        return Err(format!("unexpected command output: {output:?}").into());
    };
    Ok(text.to_string())
}
