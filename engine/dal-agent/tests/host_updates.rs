//! Repeated `SessionChanged` publishes for one session coalesce: a rename
//! burst cannot flood a subscriber's channel, and the one delivered
//! update still says "re-read this session". Reverting the per-session
//! dedupe delivers one update per rename.
#![expect(
    clippy::expect_used,
    reason = "integration fixture failures must fail at their specific setup boundary"
)]
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::time::Duration;

use dal_agent::{Agent, Env, Host, HostSubscription, HostUpdate, Product, SessionRef};
use dal_core::{ClientId, Command, Config, ConfigProduct, SessionId, Workspace};

const WAIT: Duration = Duration::from_secs(30);

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

async fn start_host(tmp: &tempfile::TempDir) -> (Host, Workspace) {
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data directory");
    std::fs::create_dir_all(&workspace_dir).expect("workspace directory");
    let config = Config::load(ConfigProduct::Dalgon, &data, "", Some("")).expect("config");
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions: Vec::new(),
        bundled: Vec::new(),
    };
    let env = fixture_env(tmp.path(), workspace_dir.clone());
    let host = Host::start(product, config, env).await.expect("host");
    let workspace = Workspace::new(workspace_dir).expect("workspace");
    (host, workspace)
}

async fn open_named(host: &Host, workspace: &Workspace, name: &str) -> Agent {
    host.open(
        SessionRef::New {
            workspace: workspace.clone(),
            name: Some(name.into()),
        },
        ClientId::new("updates-test"),
    )
    .await
    .expect("session")
}

async fn rename(agent: &Agent, name: &str) {
    agent
        .submit(Command::Rename(name.into()))
        .await
        .expect("rename");
}

/// Drains one subscription until it has been quiet for 500 ms and returns
/// the session of every `SessionChanged` it delivered, in delivery order.
async fn changed_sessions(updates: &mut HostSubscription) -> Vec<SessionId> {
    let mut changed = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_millis(500), updates.next()).await {
            Ok(Some(HostUpdate::SessionChanged { session })) => changed.push(session),
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return changed,
        }
    }
}

/// Once a coalesced `SessionChanged` has been read, the next rename of the
/// same session publishes a fresh one: reading re-arms the session.
#[tokio::test]
async fn a_delivered_session_changed_rearms_for_the_next_rename() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (host, workspace) = start_host(&tmp).await;
    let mut updates = host.subscribe();
    let agent = open_named(&host, &workspace, "rearm").await;
    for name in ["first-name", "second-name", "third-name"] {
        rename(&agent, name).await;
        assert_eq!(
            changed_sessions(&mut updates).await,
            vec![agent.session()],
            "the rename to {name} delivers one SessionChanged after the previous one was read"
        );
    }
    host.shutdown(WAIT).await;
}

/// Coalescing is keyed by session and held per subscriber: bursts on two
/// sessions deliver one update each to every subscriber, in publish order.
#[tokio::test]
async fn session_changed_coalescing_is_per_session_and_per_subscriber() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (host, workspace) = start_host(&tmp).await;
    let mut first = host.subscribe();
    let mut second = host.subscribe();
    let alpha = open_named(&host, &workspace, "alpha-session").await;
    let beta = open_named(&host, &workspace, "beta-session").await;
    rename(&alpha, "alpha-one").await;
    rename(&alpha, "alpha-two").await;
    rename(&beta, "beta-one").await;
    rename(&beta, "beta-two").await;
    let expected = vec![alpha.session(), beta.session()];
    assert_eq!(
        changed_sessions(&mut first).await,
        expected,
        "the first subscriber sees one update per session"
    );
    assert_eq!(
        changed_sessions(&mut second).await,
        expected,
        "the second subscriber's pending set is its own"
    );
    host.shutdown(WAIT).await;
}

#[tokio::test]
async fn repeated_renames_publish_one_session_changed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data directory");
    std::fs::create_dir_all(&workspace_dir).expect("workspace directory");
    let config = Config::load(ConfigProduct::Dalgon, &data, "", Some("")).expect("config");
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions: Vec::new(),
        bundled: Vec::new(),
    };
    let env = fixture_env(tmp.path(), workspace_dir.clone());
    let host = Host::start(product, config, env).await.expect("host");
    let mut updates = host.subscribe();
    let workspace = Workspace::new(workspace_dir).expect("workspace");
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: Some("session".into()),
            },
            ClientId::new("updates-test"),
        )
        .await
        .expect("session");
    // Two renames submitted back to back: the second publish finds the
    // first still queued for this subscriber and coalesces into it.
    agent
        .submit(Command::Rename("renamed-one".into()))
        .await
        .expect("first rename");
    agent
        .submit(Command::Rename("renamed-two".into()))
        .await
        .expect("second rename");
    // Drain until the channel goes quiet; each delivered SessionChanged
    // counts against the two renames.
    let mut changed = 0_u32;
    loop {
        match tokio::time::timeout(Duration::from_millis(500), updates.next()).await {
            Ok(Some(HostUpdate::SessionChanged { .. })) => changed += 1,
            // SessionOpened and other lifecycle updates are skipped; the
            // window closes on 500 ms quiet or the channel ending.
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    assert_eq!(
        changed, 1,
        "two renames produced {changed} SessionChanged updates"
    );
    host.shutdown(WAIT).await;
}
