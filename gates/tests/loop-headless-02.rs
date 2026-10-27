//! Cancelling mid-exec kills the process tree and ends exactly once.
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{error::Error, fs, path::PathBuf, time::Duration};

use dal_agent::{AgentError, Delivery, Env, SessionRef};
use dal_core::{
    CancelScope, Command, Config, ConfigProduct, Expect, PageReq, Part, Reply, Stop, UpdateKind,
    Workspace,
};
use support::{TestDir, scripted_session};

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "the kill scenario drives a full nested process tree in one walk"
)]
async fn cancel_mid_exec_kills_grandchild_and_ends_once() -> Result<(), Box<dyn Error + Send + Sync>>
{
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../cli/dalgon/tests/fixtures");
    fs::copy(
        fixtures.join("process/grandchild.sh"),
        workspace.path().join("grandchild.sh"),
    )?;
    let replay = fixtures.join("replay/loop-headless.jsonl");
    let factory = dalgon::product();
    let user = format!(
        "model = \"openai-responses/gpt-6\"\napproval = \"all\"\n[providers.scripted]\nfixture = {:?}\n",
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
        vars: support::captured_shell_vars(),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    let session = SessionRef::Ephemeral {
        workspace: Workspace::new(workspace.path().to_path_buf())?,
    };
    let harness = scripted_session(product, config, env, session).await?;
    let mut subscription = harness.agent.subscribe(None)?;
    let Reply::Accepted { turn, .. } = harness
        .agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Run the grandchild fixture and wait.".into(),
            }],
        })
        .await?
    else {
        return Err("the scripted prompt was not accepted".into());
    };

    let pid_file = workspace.path().join("grandchild.pid");
    let mut cancelled = false;
    let mut turn_ends = 0;
    // cancel_kills_process_group: every running process settles exactly once.
    // The fixture runs read/patch/read before the cancelled exec, so the
    // cancelled exec is the one running process; each started call must settle
    // at most once and each started job must settle exactly once.
    let mut started_calls: Vec<String> = Vec::new();
    let mut settled_calls: Vec<String> = Vec::new();
    let mut started_jobs: Vec<String> = Vec::new();
    let mut settled_jobs: Vec<String> = Vec::new();
    while let Some(delivery) = subscription.next().await {
        let Delivery::Update(update) = delivery else {
            continue;
        };
        match &update.kind {
            UpdateKind::ToolStarted { call, tool, .. } => {
                started_calls.push(call.as_str().to_owned());
                if tool.as_ref() != "exec" {
                    continue;
                }
                tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        if fs::read_to_string(&pid_file).is_ok_and(|text| !text.trim().is_empty()) {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await?;
                harness
                    .agent
                    .submit(Command::Cancel {
                        scope: CancelScope::Turn(turn),
                    })
                    .await?;
                cancelled = true;
            }
            UpdateKind::ToolSettled { call, .. } => {
                settled_calls.push(call.as_str().to_owned());
            }
            UpdateKind::JobStarted { job } => {
                started_jobs.push(format!("{job:?}"));
            }
            UpdateKind::JobSettled { job } => {
                settled_jobs.push(format!("{job:?}"));
            }
            UpdateKind::TurnEnded { stop, .. } => {
                turn_ends += 1;
                assert_eq!(*stop, Stop::Cancelled);
                break;
            }
            _ => {}
        }
    }
    // The reaper kills the process tree asynchronously, so settlement records
    // (especially JobSettled) can arrive after TurnEnded: breaking above
    // truncates the stream and the per-process checks below would pass
    // vacuously on an empty tail. Drain trailing updates with a bounded
    // quiet-period cap before asserting.
    for _ in 0..10 {
        match tokio::time::timeout(Duration::from_secs(1), subscription.next()).await {
            Ok(Some(Delivery::Update(update))) => match &update.kind {
                UpdateKind::ToolStarted { call, .. } => {
                    started_calls.push(call.as_str().to_owned());
                }
                UpdateKind::ToolSettled { call, .. } => {
                    settled_calls.push(call.as_str().to_owned());
                }
                UpdateKind::JobStarted { job } => {
                    started_jobs.push(format!("{job:?}"));
                }
                UpdateKind::JobSettled { job } => {
                    settled_jobs.push(format!("{job:?}"));
                }
                _ => {}
            },
            _ => break,
        }
    }
    assert!(cancelled, "exec must have started before cancellation");
    assert_eq!(turn_ends, 1);
    assert!(
        started_calls
            .iter()
            .any(|call| settled_calls.iter().any(|settled| settled == call)),
        "the cancelled exec must settle at least once: started={started_calls:?} settled={settled_calls:?}"
    );
    for call in &started_calls {
        let count = settled_calls
            .iter()
            .filter(|settled| *settled == call)
            .count();
        assert!(
            count <= 1,
            "call {call} settled {count} times, expected at most once: {settled_calls:?}"
        );
    }
    for settled in &settled_calls {
        assert!(
            started_calls.contains(settled),
            "settled call {settled} was never started: {started_calls:?}"
        );
    }
    for job in &started_jobs {
        let count = settled_jobs
            .iter()
            .filter(|settled| *settled == job)
            .count();
        assert_eq!(
            count, 1,
            "job {job} settled {count} times, expected exactly once: {settled_jobs:?}"
        );
    }
    for settled in &settled_jobs {
        assert!(
            started_jobs.contains(settled),
            "settled job {settled} was never started: {started_jobs:?}"
        );
    }
    // cancel_mid_stream_latency (non-timing half): a second cancel on the
    // already-cancelled turn reports WrongTurn and journals nothing new. The
    // 250 ms wall bound is env-sensitive and intentionally not asserted here.
    let before = harness.agent.view(PageReq::default())?;
    let second = harness
        .agent
        .submit(Command::Cancel {
            scope: CancelScope::Turn(turn),
        })
        .await;
    assert!(
        matches!(second, Err(AgentError::WrongTurn { .. })),
        "second cancel on ended turn {turn} must be WrongTurn, got {second:?}"
    );
    let after = harness.agent.view(PageReq::default())?;
    assert_eq!(
        before.seq, after.seq,
        "second cancel must not journal new records"
    );
    assert_eq!(
        before.entries.items.len(),
        after.entries.items.len(),
        "second cancel must not change journaled entries"
    );
    let pid = fs::read_to_string(pid_file)?.trim().parse::<u32>()?;
    // The kill propagates asynchronously: the turn ends before the reaper
    // finishes the tree, so the process check polls until the grandchild is
    // really gone inside a bound.
    tokio::time::timeout(Duration::from_secs(10), async {
        while support::process_named_alive(pid, "sleep") {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .map_err(|_| format!("grandchild {pid} still alive 10 s after cancellation"))?;
    let _ = harness.host.shutdown(Duration::from_secs(1)).await;
    Ok(())
}
