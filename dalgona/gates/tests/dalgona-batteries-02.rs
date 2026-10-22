// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! The guard reports its four measurements; off means silent.
use dalgona_gates::support;

use std::{path::Path, time::Duration};

use dal_agent::{Host, SessionRef};
use dal_core::{ClientId, Command, Expect, Part, Workspace};

const WAIT: Duration = Duration::from_secs(30);
/// The product wires the guard through the quality battery, so the guard
/// halves keep it while every other battery stays off.
const WITH_QUALITY: &str = "disabled_batteries = [\"ask\", \"history\", \"judged\", \"mcp\", \"orchestration\", \"review\", \"skills\", \"ttsr-rules\", \"web\", \"work\"]";

/// A new file whose shape crosses guard bands by construction: nesting depth
/// six, more than fifty physical lines, and fifteen branch points.
const PROBE: &str = r"pub fn probe_alpha(flag: u64, values: &[u64]) -> u64 {
    let mut total = 0u64;
    if flag > 0 {
        for value in values {
            if *value > 200 {
                for inner in 0..*value {
                    if inner % 2 == 0 {
                        if inner % 3 == 0 {
                            if inner % 5 == 0 {
                                if inner % 7 == 0 {
                                    total += inner * 8;
                                } else {
                                    total += inner * 4;
                                }
                            } else if inner % 11 == 0 {
                                total += inner + 11;
                            } else {
                                total += inner * 2;
                            }
                        } else if inner % 5 == 0 {
                            total += 5;
                        } else if inner % 7 == 0 {
                            total += 7;
                        } else {
                            total += inner;
                        }
                    } else if inner % 3 == 0 {
                        total += inner + 3;
                    } else if inner % 5 == 0 {
                        total += inner + 5;
                    } else if inner % 7 == 0 {
                        total += inner + 7;
                    } else {
                        total += 1;
                    }
                }
            } else if *value > 100 {
                total += value * 3;
            } else if *value > 50 {
                total += value * 2;
            } else if *value > 10 {
                total += value;
            } else {
                total += 1;
            }
        }
    } else if flag == 0 {
        total += 1000;
    } else {
        total += 2000;
    }
    total
}
";

fn escape_json(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// A live host over the real product with a scripted provider.
async fn scripted_host(
    root: &Path,
    isolation: &str,
    user_extra: &str,
    steps: &[String],
) -> support::TestResult<Host> {
    let script = root.join("script.jsonl");
    std::fs::write(&script, steps.join("\n"))?;
    let user = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n{isolation}\n{user_extra}\n[providers.scripted]\nfixture = {:?}\n",
        script.display().to_string()
    );
    let factory = dalgona::product();
    let config = dal_core::Config::load(
        dal_core::ConfigProduct::Dalgona,
        root,
        factory.defaults,
        Some(&user),
    )?;
    let product = (factory.build)(&dalgon::BuildCx {
        data_root: root.into(),
        config: &config,
    })?;
    let env = dal_agent::Env {
        vars: std::collections::BTreeMap::from([("OPENAI_API_KEY".into(), "sk-test".into())]),
        cwd: root.into(),
        sandbox_helper: None,
    };
    Ok(Host::start(product, config, env).await?)
}

async fn prompt(
    agent: &dal_agent::Agent,
    subscription: &mut dal_agent::Subscription,
    text: &str,
) -> support::TestResult<()> {
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: text.into() }],
        })
        .await?;
    while let Some(delivery) = tokio::time::timeout(WAIT, subscription.next()).await? {
        if let dal_agent::Delivery::Update(update) = delivery
            && let dal_core::UpdateKind::TurnEnded { .. } = &update.kind
        {
            return Ok(());
        }
    }
    Err("subscription closed before the turn ended".into())
}

/// The session view after a measuring turn followed by a second turn, which
/// carries the guard's pending report.
async fn measured_view(
    root: &Path,
    isolation: &str,
    guard_toml: &str,
) -> support::TestResult<String> {
    let body = PROBE
        .lines()
        .map(|line| format!("+{line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let create = format!("[metrics_probe.rs#NEW]\nPUT >$:\n{body}");
    let host = scripted_host(
        root,
        isolation,
        guard_toml,
        &[
            support::tool_step(
                "patch-1",
                "patch",
                &format!("{{\"input\":\"{}\"}}", escape_json(&create)),
            ),
            support::text_step("done"),
            support::text_step("again"),
        ],
    )
    .await?;
    let workspace = Workspace::new(root.join("workspace"))?;
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("guard-measurements-gate"),
        )
        .await?;
    let mut subscription = agent.subscribe(None)?;
    prompt(&agent, &mut subscription, "add the probe file").await?;
    prompt(&agent, &mut subscription, "second turn").await?;
    let view = format!("{:?}", agent.view(dal_core::PageReq::default())?);
    let key = agent.view(dal_core::PageReq::default())?.session.id;
    host.close(key).await?;
    drop(subscription);
    drop(agent);
    host.shutdown(Duration::from_secs(2)).await;
    Ok(view)
}

#[test]
fn guard_is_on_by_default_and_reports_the_four_measurements() -> support::TestResult<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let scratch = support::Scratch::new("guard-measurements")?;
        let root = scratch.path().to_path_buf();
        std::fs::create_dir_all(root.join("workspace"))?;
        let view = measured_view(&root, WITH_QUALITY, "").await?;
        // Turn growth: added, deleted, and net lines with the files touched.
        assert!(
            view.contains("TURN CHANGE SUMMARY"),
            "guard reports turn growth: {view}"
        );
        for marker in ["Added", "deleted", "net"] {
            assert!(view.contains(marker), "turn growth omits {marker}: {view}");
        }
        // Per-file absolute metrics on the touched file.
        assert!(
            view.contains("metrics_probe.rs") && view.contains("ploc"),
            "guard reports per-file metrics: {view}"
        );
        // Per-function band crossings name the function and the crossing.
        assert!(
            view.contains("probe_alpha") && view.contains("→"),
            "guard reports per-function crossings: {view}"
        );
        // Best-current ranks the erosion the turn introduced.
        assert!(
            view.contains("METRICS") && view.contains("erosion"),
            "guard reports best-current: {view}"
        );
        let written = std::fs::read_to_string(root.join("workspace/metrics_probe.rs"))?;
        assert!(
            written.contains("probe_alpha"),
            "the measuring turn writes the probe file"
        );
        // A disabled guard emits none of the four on its own measuring turn:
        // a fresh workspace keeps the create from failing over the first
        // run's file, which would leave the guard with nothing to stay
        // silent about. The crossing marker is the guard's "→" rather than
        // the function name, which the tool-call input echo carries even
        // when the guard reports nothing.
        let quiet = support::Scratch::new("guard-measurements-off")?;
        let quiet_root = quiet.path().to_path_buf();
        std::fs::create_dir_all(quiet_root.join("workspace"))?;
        let silent = measured_view(&quiet_root, WITH_QUALITY, "[guard]\nenabled = false").await?;
        let quiet_written = std::fs::read_to_string(quiet_root.join("workspace/metrics_probe.rs"))?;
        assert!(
            quiet_written.contains("probe_alpha"),
            "the silent turn still writes the probe file"
        );
        for marker in ["TURN CHANGE SUMMARY", "METRICS", "→"] {
            assert!(
                !silent.contains(marker),
                "a disabled guard stays silent, found {marker}"
            );
        }
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
