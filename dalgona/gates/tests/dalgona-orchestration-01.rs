// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate: the orchestration battery task ends
//! within 5 s of a close, no timer fires after it, and the goal sidecar
//! holds the last controller mode.
use dalgona_gates::support;

use std::time::{Duration, Instant};

use sonic_rs::JsonContainerTrait;

#[test]
fn close_ends_the_battery_within_5s_and_holds_the_last_mode() -> support::TestResult<()> {
    let scratch = support::Scratch::new("orchestration-shutdown")?;
    let fixture = scratch.path().join("empty-script.jsonl");
    std::fs::write(&fixture, "")?;
    let user = format!(
        "model = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = {:?}\n",
        fixture.to_string_lossy()
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let product = support::build_product(scratch.path().to_path_buf(), Some(&user))?;
        for extension in &product.extensions {
            let services = extension.inject().capabilities();
            if services.is_empty() || extension.origin() == dal_core::Origin::Builtin {
                continue;
            }
            dal_agent::ext::grants::GrantStore::new(scratch.path().to_path_buf())
                .grant(
                    dal_agent::ext::grants::GrantKey::for_extension(
                        extension,
                        &scratch.path().join("ws"),
                    )
                    .expect("registered extension has a code identity"),
                    dal_core::ClientId::new("shutdown-gate"),
                    dal_core::Timestamp::now(),
                )
                .await?;
        }
        drop(product);
        let host =
            support::start_dalgona_with_config(scratch.path().to_path_buf(), Some(&user)).await?;
        let agent = host
            .open(
                dal_agent::SessionRef::New {
                    workspace: dal_core::Workspace::new(scratch.path().join("ws"))?,
                    name: None,
                },
                dal_core::ClientId::new("shutdown-gate"),
            )
            .await?;
        let session = agent.view(dal_core::PageReq::default())?.session.id;
        let mut subscription = agent.subscribe(None)?;

        // Pause automatic turns through the real command door; the goal
        // sidecar persists the mode when the session is saved.
        let paused = agent
            .submit(dal_core::Command::Run {
                name: "continuation".into(),
                args: "pause".into(),
                expected: None,
            })
            .await?;
        assert!(
            matches!(paused, dal_core::Reply::Done(_)),
            "continuation pause failed: {paused:?}"
        );

        // Closing the session must end the battery task within 5 s.
        let started = Instant::now();
        host.close(session).await?;
        let elapsed = started.elapsed();
        assert!(
            elapsed <= Duration::from_secs(5),
            "close took {elapsed:?}; the contract bounds it at 5 s"
        );

        // No timer fires after the close: a late wake would open a turn,
        // so no TurnStarted may arrive after the close.
        let closed = tokio::time::timeout(Duration::from_millis(1_500), subscription.next()).await;
        match closed {
            Err(_) | Ok(None) => {}
            Ok(Some(delivery)) => {
                let started = matches!(
                    &delivery,
                    dal_agent::Delivery::Update(update)
                        if matches!(update.kind, dal_core::UpdateKind::TurnStarted { .. })
                );
                assert!(
                    !started,
                    "a turn started after the close: a timer fired: {delivery:?}"
                );
            }
        }

        let controller = paused_controller(&scratch.path().join("sessions"))?;
        assert_eq!(controller, "paused", "the sidecar lost the last mode");

        // Full shutdown leaves no tasks behind.
        let report = host.shutdown(Duration::from_secs(5)).await;
        assert_eq!(report.tasks_remaining, 0, "tasks survived shutdown");
        assert_eq!(report.sessions_closed, 0, "sessions outlived the close");
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
fn stress_runtime() -> support::TestResult<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?)
}

#[test]
fn stress_500_subagents_and_200_jobs_hold_the_process_contract() -> support::TestResult<()> {
    if !support::stress_enabled() {
        return Ok(());
    }
    stress_runtime()?.block_on(dalgona_gates::stress::run_composite())?;
    eprintln!("{}", support::scenario_marker("composite500x200ws8"));
    Ok(())
}

#[test]
fn stress_cancel_p99_under_250ms_idle_and_under_full_load() -> support::TestResult<()> {
    if !support::stress_enabled() {
        return Ok(());
    }
    stress_runtime()?.block_on(dalgona_gates::stress::run_cancel_p99())?;
    eprintln!("{}", support::scenario_marker("cancel-p99-idle100"));
    eprintln!("{}", support::scenario_marker("cancel-p99-loaded100"));
    Ok(())
}

#[test]
fn stress_rss_stays_below_256_mib() -> support::TestResult<()> {
    if !support::stress_enabled() {
        return Ok(());
    }
    stress_runtime()?.block_on(dalgona_gates::stress::run_rss())?;
    eprintln!("{}", support::scenario_marker("rss-peak-256mib"));
    Ok(())
}

#[test]
fn stress_open_fds_stay_below_2048() -> support::TestResult<()> {
    if !support::stress_enabled() {
        return Ok(());
    }
    stress_runtime()?.block_on(dalgona_gates::stress::run_fds())?;
    eprintln!("{}", support::scenario_marker("fds-peak-2048"));
    Ok(())
}

#[test]
fn stress_tui_keypress_echo_p99_stays_below_24ms() -> support::TestResult<()> {
    if !support::stress_enabled() {
        return Ok(());
    }
    stress_runtime()?.block_on(dalgona_gates::stress::run_keypress())?;
    eprintln!("{}", support::scenario_marker("keypress-p99-24ms"));
    Ok(())
}

fn paused_controller(sessions: &std::path::Path) -> support::TestResult<String> {
    let mut goal_file = None;
    let mut stack = vec![sessions.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.file_name().is_some_and(|name| name == "goal.json") {
                goal_file = Some(path);
            }
        }
    }
    let goal_file = goal_file.ok_or("no goal sidecar was written")?;
    let document = std::fs::read_to_string(&goal_file)?;
    let value: sonic_rs::Value = sonic_rs::from_str(document.trim_end())?;
    let controller = value
        .as_object()
        .and_then(|object| object.get(&"controller"))
        .and_then(sonic_rs::JsonValueTrait::as_str)
        .ok_or("the sidecar has no controller member")?;
    Ok(controller.to_owned())
}
