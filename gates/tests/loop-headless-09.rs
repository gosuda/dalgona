//! Headless shutdown waits for registered extension status to go quiet, and
//! gives up at the grace bound when it never does.
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{
    error::Error,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use dal_agent::ext::{ExtensionBuilder, StatusCx, StatusPoll, StatusSnapshot};
use dal_agent::{Env, SessionRef, ShutdownReport};
use dal_core::{Command, Config, ConfigProduct, Expect, Part, Reply, Workspace};
use support::{GateHarness, TestDir, scripted_session};

struct GateStatus {
    quiet: AtomicBool,
}

impl StatusPoll for GateStatus {
    fn snapshot(&self, _cx: &StatusCx) -> StatusSnapshot {
        StatusSnapshot {
            quiet: self.quiet.load(Ordering::SeqCst),
            text: None,
        }
    }
}

/// A session with one registered status kind that starts busy, after one
/// accepted prompt.
async fn busy_status_session()
-> Result<(GateHarness, Arc<GateStatus>, TestDir, TestDir), Box<dyn Error + Send + Sync>> {
    let poll = Arc::new(GateStatus {
        quiet: AtomicBool::new(false),
    });
    let extension = ExtensionBuilder::new("gate-status", "0.1.0", dal_core::ServiceSet::default())?
        .status_kind("gate-status", poll.clone())
        .build()?;
    let (kind, _) = extension.status().ok_or("status kind must be registered")?;
    assert_eq!(kind, "gate-status");

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
    product.extensions.push(extension);
    let env = Env {
        vars: support::captured_shell_vars(),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    let session = SessionRef::Ephemeral {
        workspace: Workspace::new(workspace.path().to_path_buf())?,
    };
    let harness = scripted_session(product, config, env, session).await?;
    let reply = harness
        .agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "hold status until quiet".into(),
            }],
        })
        .await?;
    assert!(matches!(reply, Reply::Accepted { .. }));
    Ok((harness, poll, data, workspace))
}

/// While the status is busy the shutdown stays pending across several host
/// quiet-poll sweeps and the session actor stays open; the moment the status
/// goes quiet shutdown completes and reports the quiet outcome.
#[tokio::test]
async fn headless_shutdown_waits_for_registered_status_quiet()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let (harness, poll, _data, _workspace) = busy_status_session().await?;
    let mut shutdown = Box::pin(harness.host.clone().shutdown(Duration::from_secs(30)));

    let early = tokio::time::timeout(Duration::from_millis(400), shutdown.as_mut()).await;
    assert!(
        early.is_err(),
        "shutdown returned while the status kind was busy: {early:?}"
    );
    assert!(
        !harness.agent.is_quiet().await?,
        "the session must stay open and report busy while shutdown waits"
    );

    poll.quiet.store(true, Ordering::SeqCst);
    let report = tokio::time::timeout(Duration::from_secs(10), shutdown).await?;
    assert_eq!(
        report,
        ShutdownReport {
            sessions_closed: 1,
            status_quiet: true,
            tasks_remaining: 0,
        }
    );
    Ok(())
}

/// A status kind that never goes quiet costs exactly the grace: shutdown
/// waits at least the grace, reports `status_quiet: false`, still closes the
/// session, and does not wait unbounded.
#[tokio::test]
async fn headless_shutdown_gives_up_on_a_never_quiet_status_at_the_grace_bound()
-> Result<(), Box<dyn Error + Send + Sync>> {
    const GRACE: Duration = Duration::from_millis(300);
    let (harness, _poll, _data, _workspace) = busy_status_session().await?;
    let started = Instant::now();
    let report =
        tokio::time::timeout(Duration::from_secs(10), harness.host.shutdown(GRACE)).await?;
    let waited = started.elapsed();
    assert!(
        !report.status_quiet,
        "a never-quiet status must not report quiet: {report:?}"
    );
    assert_eq!(report.sessions_closed, 1, "the session still closes");
    assert!(
        waited >= GRACE,
        "shutdown gave up after {waited:?}, before the {GRACE:?} grace"
    );
    assert!(
        waited < Duration::from_secs(5),
        "shutdown waited {waited:?} for a {GRACE:?} grace"
    );
    Ok(())
}

/// A status that is already quiet costs none of the grace: shutdown reports
/// the quiet outcome without sleeping out the bound.
#[tokio::test]
async fn headless_shutdown_of_an_already_quiet_status_does_not_spend_the_grace()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let (harness, poll, _data, _workspace) = busy_status_session().await?;
    poll.quiet.store(true, Ordering::SeqCst);
    assert!(harness.agent.is_quiet().await?);
    let started = Instant::now();
    let report = tokio::time::timeout(
        Duration::from_secs(10),
        harness.host.shutdown(Duration::from_secs(30)),
    )
    .await?;
    assert!(report.status_quiet);
    assert_eq!(report.sessions_closed, 1);
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "an already-quiet host spent {:?} of a 30s grace",
        started.elapsed()
    );
    Ok(())
}
