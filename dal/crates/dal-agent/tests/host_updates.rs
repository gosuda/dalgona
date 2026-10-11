//! Repeated `SessionChanged` publishes for one session coalesce: a rename
//! burst cannot flood a subscriber's channel, and the one delivered
//! update still says "re-read this session". Reverting the per-session
//! dedupe delivers one update per rename.
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::time::Duration;

use dal_agent::{Env, Host, HostUpdate, Product, SessionRef};
use dal_core::{ClientId, Command, Config, ConfigProduct, Workspace};

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
