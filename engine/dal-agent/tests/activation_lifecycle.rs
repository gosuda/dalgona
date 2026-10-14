//! The session-start observer's terminal contract during fallible activation.
//!
//! One contract row, through the real Host seam with a real parent close:
//! the host dispatches `session_start` for a spawning child before the
//! activation commits, and a parent close landing while that start
//! observer is still held must still end the observers' lifecycle with
//! exactly one terminal `session_end` for the refused child. A pre-check
//! before the start observer cannot close this race: the close can land
//! inside the observer dispatch itself, so the correction has to run on
//! the refusal path. The public refusal error and every existing reason
//! stay untouched; no new event type exists.
//!
//! * Closing the parent while a child's `session_start` observer is held
//!   refuses the child's open with the unchanged `NotFound` message and
//!   dispatches the child's terminal `session_end` exactly once, after
//!   the parent's own close end. The refused child never joins the
//!   table and writes no journal records, so it cannot be resumed.
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dal_agent::ext::{BoxFuture, ExtensionBuilder, HookCx, HookError, ObserveHook};
use dal_agent::{Env, Host, HostError, Product, SessionRef};
use dal_core::{
    CallId, ClientId, Config, ConfigProduct, SessionEnd, SessionId, SessionStart, Workspace,
};

/// Bound for every settle wait; failures assert on the recorded outcome.
const WAIT: Duration = Duration::from_secs(30);
/// Poll step for held-gate flags; state flags, never sleeps, order.
const POLL: Duration = std::time::Duration::from_millis(10);

/// Records every dispatched session start and holds the one that arrives
/// while armed: the child's start observer stays inside its dispatch
/// while the parent closes. The gate holds exactly once; later sessions
/// (the reopen probe) pass through, and an already-released hold resolves
/// at once, so the fixture can never hang the test runner.
struct HeldStart {
    starts: Arc<Mutex<Vec<SessionId>>>,
    armed: Arc<AtomicBool>,
    entered: Arc<AtomicBool>,
    gate: Arc<Mutex<Option<tokio::sync::oneshot::Receiver<()>>>>,
}

impl ObserveHook<SessionStart> for HeldStart {
    fn call(&self, input: SessionStart, _cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        self.starts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(input.session);
        if !self.armed.load(Ordering::SeqCst) {
            return Box::pin(std::future::ready(Ok(())));
        }
        self.entered.store(true, Ordering::SeqCst);
        let gate = Arc::clone(&self.gate);
        Box::pin(async move {
            let Some(receiver) = gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            else {
                return Ok(());
            };
            // A dropped sender also releases the hold: the fixture must
            // never hang the test runner, even on an early abort.
            let _ = receiver.await;
            Ok(())
        })
    }
}

type SessionEndLog = Arc<Mutex<Vec<(SessionId, Box<str>)>>>;

/// Records every session end the host dispatches.
struct EndRecorder {
    ends: SessionEndLog,
}

impl ObserveHook<SessionEnd> for EndRecorder {
    fn call(&self, input: SessionEnd, _cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        let ends = Arc::clone(&self.ends);
        Box::pin(async move {
            ends.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((input.session, input.reason));
            Ok(())
        })
    }
}

fn fixture_env(tmp: &std::path::Path, cwd: std::path::PathBuf) -> Env {
    Env {
        vars: BTreeMap::from([
            (OsString::from("OPENAI_API_KEY"), OsString::from("sk-test")),
            (OsString::from("HOME"), OsString::from(tmp.join("home"))),
            (
                OsString::from("XDG_CACHE_HOME"),
                OsString::from(tmp.join("cache")),
            ),
        ]),
        cwd,
        sandbox_helper: None,
    }
}

/// Spins on a flag with a bound; the assertion after names the recorded
/// state when the flag never settles.
async fn wait_for(description: &str, settled: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !settled() {
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(POLL).await;
    }
    assert!(settled(), "{description} settled before the bound");
}

#[expect(
    clippy::too_many_lines,
    reason = "refusal fixture walks open, close, and resume in place"
)]
#[tokio::test]
async fn closing_the_parent_during_a_held_start_observer_still_ends_the_opening() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data directory");
    std::fs::create_dir_all(&workspace_dir).expect("workspace directory");
    let fixture_path = data.join("script.jsonl");
    // No turn runs in this fixture; one valid end-turn line keeps the
    // scripted provider's fixture shape intact.
    std::fs::write(
        &fixture_path,
        "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"done\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n",
    )
    .expect("script fixture");
    let config_text = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture_path.to_string_lossy().replace('\\', "\\\\")
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(config_text.as_str())).expect("config");
    let starts = Arc::new(Mutex::new(Vec::new()));
    let ends = Arc::new(Mutex::new(Vec::new()));
    let armed = Arc::new(AtomicBool::new(false));
    let entered = Arc::new(AtomicBool::new(false));
    let (release, hold) = tokio::sync::oneshot::channel::<()>();
    let gate = Arc::new(Mutex::new(Some(hold)));
    let extension = ExtensionBuilder::new("activation", "0.1.0", dal_core::ServiceSet::EMPTY)
        .expect("builder")
        .on_session_start(HeldStart {
            starts: Arc::clone(&starts),
            armed: Arc::clone(&armed),
            entered: Arc::clone(&entered),
            gate: Arc::clone(&gate),
        })
        .on_session_end(EndRecorder {
            ends: Arc::clone(&ends),
        })
        .build()
        .expect("activation extension");
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = fixture_env(tmp.path(), workspace_dir.clone());
    let host = Host::start(product, config, env).await.expect("host");
    let workspace = Workspace::new(workspace_dir).expect("workspace");
    let parent = host
        .open(
            SessionRef::New {
                workspace: workspace.clone(),
                name: None,
            },
            ClientId::new("activation-test"),
        )
        .await
        .expect("parent session")
        .session();

    // Arm the hold, then spawn the child from outside any session turn:
    // its start observer is dispatched and held while the close lands.
    armed.store(true, Ordering::SeqCst);
    #[expect(
        clippy::disallowed_methods,
        reason = "test concurrency: the opener task is joined on the test path"
    )]
    let opener = {
        let host = host.clone();
        let workspace = workspace.clone();
        tokio::spawn(async move {
            host.open(
                SessionRef::Child {
                    parent,
                    call: CallId::new("activation-child"),
                    workspace,
                    name: None,
                },
                ClientId::new("activation-test"),
            )
            .await
        })
    };
    wait_for("the child start observer to be held", || {
        entered.load(Ordering::SeqCst)
    })
    .await;

    // The parent close lands inside the held start dispatch, so no
    // pre-check before the observer can ever refuse this open.
    host.close(parent)
        .await
        .expect("the parent closes while the child start observer is held");
    let _ = release.send(());
    // JoinHandle::expect would need the joined Result to be Debug, and
    // Agent is Clone-only, so the join is matched instead.
    let joined = match opener.await {
        Ok(joined) => joined,
        Err(join_error) => panic!("the opener task joins: {join_error:?}"),
    };
    let message = match joined {
        Ok(agent) => panic!("the refused open returns no agent: {:?}", agent.session()),
        Err(HostError::NotFound { message }) => message,
        Err(other) => panic!("the refused open reports NotFound: {other:?}"),
    };

    // The terminal obligation first: the refused child's end closes the
    // observers' lifecycle exactly once, after the parent's own close
    // end. A pre-fix tree dispatches no such end, so this row fails red.
    let ends = ends
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert_eq!(
        ends.len(),
        2,
        "a dispatched session_start is never left without its terminal end: {ends:?}"
    );
    assert_eq!(
        ends[0],
        (parent, "close".into()),
        "the parent close dispatches its own end first: {ends:?}"
    );
    let (child, reason) = ends[1].clone();
    assert_eq!(
        reason.to_string(),
        "parent closed while opening",
        "the terminal end names the refused open: {reason}"
    );
    let starts = starts
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert_eq!(
        starts,
        vec![parent, child],
        "only the parent and the refused child dispatched session_start"
    );
    assert_eq!(
        message.to_string(),
        format!("parent session {parent} closed while opening {child}."),
        "the public refusal message is unchanged"
    );

    let resumed = host
        .open(
            SessionRef::Resume {
                key: child.to_string().into(),
                workspace: workspace.clone(),
            },
            ClientId::new("activation-probe"),
        )
        .await;
    assert!(
        matches!(
            resumed,
            Err(HostError::Store(dal_store::StoreError::NoMatch { .. }))
        ),
        "the refused, unwritten child is neither live nor resumable"
    );
    let _ = host.shutdown(Duration::from_secs(10)).await;
}
