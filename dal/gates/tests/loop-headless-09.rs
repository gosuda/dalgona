//! Headless shutdown waits for registered extension status to go quiet.
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{
    collections::BTreeMap,
    error::Error,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use dal_agent::ext::{ExtensionBuilder, StatusCx, StatusPoll, StatusSnapshot};
use dal_agent::{Env, SessionRef};
use dal_core::{Command, Config, ConfigProduct, Expect, Part, Reply, Workspace};
use support::{TestDir, scripted_session};

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

#[expect(
    clippy::disallowed_methods,
    reason = "SC test races the shutdown call outside the actor's task set"
)]
#[tokio::test]
async fn headless_shutdown_waits_for_registered_status_quiet()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let poll = Arc::new(GateStatus {
        quiet: AtomicBool::new(false),
    });
    let extension = ExtensionBuilder::new("gate-status", "0.1.0", dal_core::ServiceSet::default())?
        .status_kind("gate-status", poll.clone())
        .build()?;
    let (kind, _) = extension.status().expect("status kind must be registered");
    assert_eq!(kind, "gate-status");

    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../crates/dalgon/tests/fixtures/replay/stress-scripted.jsonl");
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
        vars: BTreeMap::new(),
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

    let started = Instant::now();
    let shutdown = tokio::spawn(harness.host.shutdown(Duration::from_secs(2)));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !shutdown.is_finished(),
        "shutdown must wait while status is busy"
    );
    poll.quiet.store(true, Ordering::SeqCst);
    let report = shutdown.await?;
    assert_eq!(report.sessions_closed, 1);
    assert!(started.elapsed() >= Duration::from_millis(200));
    Ok(())
}
