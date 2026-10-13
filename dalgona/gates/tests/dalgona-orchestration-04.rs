// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate: the goal sidecar is durable and private.
//! A created goal is written atomically beside the session with mode 0600,
//! carries the owning session id, and survives a reopen with its counters.
use dalgona_gates::support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sonic_rs::JsonContainerTrait;

struct TestRoot(PathBuf);

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn goal_sidecar_is_durable_and_private_across_reopen() -> support::TestResult<()> {
    let root =
        TestRoot(std::env::temp_dir().join(format!("dalgona-goal-gate-{}", uuid::Uuid::now_v7())));
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
            if services.is_empty() {
                continue;
            }
            dal_agent::ext::grants::GrantStore::new(data.clone())
                .grant(
                    dal_agent::ext::grants::GrantKey {
                        extension: dal_core::Name::parse(extension.name())?,
                        origin: dal_core::Origin::Bundled,
                        services,
                    },
                    dal_core::ClientId::new("goal-gate"),
                    dal_core::Timestamp::now(),
                )
                .await?;
        }
        drop(product);
        let host = support::start_dalgona_with_config(data.clone(), Some(&user)).await?;
        let agent = host
            .open(
                dal_agent::SessionRef::New {
                    workspace: dal_core::Workspace::new(workspace.clone())?,
                    name: None,
                },
                dal_core::ClientId::new("goal-gate"),
            )
            .await?;
        let session = agent.view(dal_core::PageReq::default())?.session.id;
        let created = run_command(&agent, "goal", "write the parser").await?;
        assert!(
            created.contains("goal g1: active"),
            "unexpected create reply: {created}"
        );
        let goal_file = find_goal_file(&data, &session)?;
        let document = fs::read_to_string(&goal_file)?;
        let value: sonic_rs::Value = sonic_rs::from_str(document.trim_end())?;
        let object = value.as_object().ok_or("goal.json is not an object")?;
        assert_eq!(
            object
                .get(&"session")
                .and_then(sonic_rs::JsonValueTrait::as_str),
            Some(session.to_string().as_str()),
            "the sidecar must name its owning session"
        );
        assert_eq!(
            object.get(&"v").and_then(sonic_rs::JsonValueTrait::as_u64),
            Some(1),
            "the sidecar version must be 1"
        );
        let mode = fs::metadata(&goal_file)?.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the sidecar must be mode 0600, found {mode:o}");

        host.close(session).await?;
        let reopened = host
            .open(
                dal_agent::SessionRef::Resume {
                    key: session.to_string().into(),
                    workspace: dal_core::Workspace::new(workspace)?,
                },
                dal_core::ClientId::new("goal-gate-reopen"),
            )
            .await?;
        let shown = run_command(&reopened, "goal", "").await?;
        assert!(
            shown.contains("write the parser"),
            "the goal did not survive the reopen: {shown}"
        );
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

fn find_goal_file(
    data: &Path,
    session: &dal_core::SessionId,
) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    let mut stack = vec![data.join("sessions")];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|name| name == "goal.json") {
                return Ok(path);
            }
        }
    }
    Err(format!("no goal.json beside session {session}").into())
}
