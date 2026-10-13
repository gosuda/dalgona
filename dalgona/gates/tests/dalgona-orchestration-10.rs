// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion scale gate: background exec jobs
//! with monitors and rewritten sleeps hold their process contract. Set
//! `DALGONA_ORCH_STRESS=1` to run the fan-out.
//!
//! With `approval = "all"` the session starts 60 background `sleep 30`
//! exec jobs (each rewritten to a 5 s foreground window by the sleep
//! classifier and detached), watches 20 of them with `monitor`, cancels
//! the watches, and closes. Every monitor ends with its job, the sleep
//! rewrites detach instead of parking the turn, and the process keeps its
//! fd and RSS ceilings.
#![cfg(unix)]
#![expect(
    clippy::disallowed_methods,
    reason = "gate reads its opt-in flag and scratch dirs from the environment"
)]

use dalgona_gates::support;

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const JOBS: usize = 60;
const MONITORS: usize = 20;
const RSS_LIMIT_MIB: u64 = 256;
const FD_LIMIT: usize = 2_048;

struct TestRoot(PathBuf);

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn exec_jobs_and_monitors_end_clean_within_the_process_contract() -> support::TestResult<()> {
    if std::env::var_os("DALGONA_ORCH_STRESS").is_none() {
        return Ok(());
    }
    let root = TestRoot(
        std::env::temp_dir().join(format!("dalgona-exec-stress-{}", uuid::Uuid::now_v7())),
    );
    let data = root.0.join("data");
    let workspace = root.0.join("workspace");
    fs::create_dir_all(&data)?;
    let mut script = String::new();
    for index in 0..JOBS {
        script.push_str(&support::tool_step(
            &format!("e{index}"),
            "exec",
            r#"{"command":"sleep 30","run_in_background":true}"#,
        ));
        script.push('\n');
    }
    for index in 0..MONITORS {
        script.push_str(&support::tool_step(
            &format!("m{index}"),
            "monitor",
            r#"{"action":"watch","job":"jPLACEHOLDER","filter":"tick","description":"watch PLACEHOLDER"}"#
                .replace("PLACEHOLDER", &index.to_string())
                .as_str(),
        ));
        script.push('\n');
    }
    script.push_str(&support::text_step("done"));
    fs::write(data.join("script.jsonl"), script)?;
    let user = format!(
        "model = \"openai/gpt-6-luna\"\napproval = \"all\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        data.join("script.jsonl").display()
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let host = support::start_dalgona_with_config(data.clone(), Some(&user)).await?;
        let agent = host
            .open(
                dal_agent::SessionRef::New {
                    workspace: dal_core::Workspace::new(workspace)?,
                    name: None,
                },
                dal_core::ClientId::new("exec-stress-gate"),
            )
            .await?;
        let started = Instant::now();
        agent
            .submit(dal_core::Command::Prompt {
                expect: dal_core::Expect::Idle,
                content: vec![dal_core::Part::Text {
                    text: "start the jobs".into(),
                }],
            })
            .await?;
        // The rewritten sleeps detach within 5 s: the whole turn must come
        // back idle well inside that bound, or a sleep parked the owner.
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(5),
            "the exec turn took {elapsed:?}; a sleep did not detach in time"
        );

        // Every monitor must end with its job: close the session while the
        // sleeps still run, then shutdown must leave no tasks behind.
        let session = agent.view(dal_core::PageReq::default())?.session.id;
        host.close(session).await?;
        let report = host.shutdown(Duration::from_secs(10)).await;
        assert_eq!(report.tasks_remaining, 0, "monitors kept tasks alive");
        assert_eq!(report.sessions_closed, 0);

        // The process contract still holds after the fan-out and close.
        let status = std::fs::read_to_string("/proc/self/status")?;
        let rss_kib: u64 = status
            .lines()
            .find_map(|line| line.strip_prefix("VmRSS:"))
            .and_then(|rest| rest.split_whitespace().next()?.parse().ok())
            .ok_or("no VmRSS line")?;
        assert!(
            rss_kib / 1_024 < RSS_LIMIT_MIB,
            "RSS {} MiB exceeds the cap",
            rss_kib / 1_024
        );
        let fds = std::fs::read_dir("/proc/self/fd")?.count();
        assert!(fds <= FD_LIMIT, "open fds {fds} exceed the cap");
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
