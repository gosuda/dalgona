//! Extension status kinds publish `ext_status` updates on change only and
//! feed the public quiet predicate.
#![expect(clippy::expect_used, reason = "test assertions abort on failure")]
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::ext::{ExtensionBuilder, StatusCx, StatusPoll, StatusSnapshot};
use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef, Subscription};
use dal_core::{
    ClientId, Config, ConfigProduct, ExtState, ExtStatus, ServiceSet, UpdateKind, Workspace,
};

struct Controlled {
    state: Mutex<(bool, Option<Box<str>>)>,
}

impl Controlled {
    fn set(&self, quiet: bool, text: Option<&str>) {
        *self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = (quiet, text.map(Into::into));
    }
}

impl StatusPoll for Controlled {
    fn snapshot(&self, _cx: &StatusCx) -> StatusSnapshot {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        StatusSnapshot {
            quiet: state.0,
            text: state.1.clone(),
        }
    }
}

async fn open(poll: &Arc<Controlled>) -> (tempfile::TempDir, Host, Agent) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("w");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
    let user = "model = \"openai/gpt-6-luna\"\n";
    let config = Config::load(ConfigProduct::Dalgon, &data, "", Some(user)).expect("config");
    let extension = ExtensionBuilder::new("focus", "0.1.0", ServiceSet::default())
        .expect("builder")
        .status_kind("focus", Arc::clone(poll) as Arc<dyn StatusPoll>)
        .build()
        .expect("extension");
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: workspace_dir.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await.expect("host");
    let workspace = Workspace::new(workspace_dir).expect("workspace");
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("probe"),
        )
        .await
        .expect("open");
    (tmp, host, agent)
}

async fn drain(subscription: &mut Subscription) -> Vec<ExtStatus> {
    let mut seen = Vec::new();
    while let Ok(Some(delivery)) =
        tokio::time::timeout(Duration::from_millis(250), subscription.next()).await
    {
        if let Delivery::Update(update) = delivery
            && let UpdateKind::ExtStatus(status) = &update.kind
        {
            seen.push(status.clone());
        }
    }
    seen
}

fn status(state: ExtState, text: &str) -> ExtStatus {
    ExtStatus {
        ext: "focus".into(),
        state,
        text: Some(text.into()),
    }
}

#[tokio::test]
async fn ext_status_publishes_changes_once_and_tracks_quiet() {
    let poll = Arc::new(Controlled {
        state: Mutex::new((true, None)),
    });
    let (_tmp, host, agent) = open(&poll).await;
    let mut subscription = agent.subscribe(None).expect("subscribe");
    assert!(drain(&mut subscription).await.is_empty());
    assert!(agent.is_quiet().await.expect("quiet predicate"));
    assert!(agent.ext_status().is_empty());

    poll.set(false, Some("indexing"));
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!agent.is_quiet().await.expect("quiet predicate"));
    assert_eq!(agent.ext_status(), vec![status(ExtState::Busy, "indexing")]);

    poll.set(false, Some("linking"));
    tokio::time::sleep(Duration::from_millis(400)).await;
    poll.set(true, None);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(agent.is_quiet().await.expect("quiet predicate"));
    assert!(agent.ext_status().is_empty());

    let seen = drain(&mut subscription).await;
    assert_eq!(
        seen,
        vec![
            status(ExtState::Busy, "indexing"),
            status(ExtState::Busy, "linking"),
            ExtStatus {
                ext: "focus".into(),
                state: ExtState::Quiet,
                text: None,
            },
        ]
    );
    host.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn poll_status_reads_a_fresh_state_without_waiting_for_the_tick() {
    let poll = Arc::new(Controlled {
        state: Mutex::new((true, None)),
    });
    let (_tmp, host, agent) = open(&poll).await;
    assert!(agent.poll_status().await.expect("poll").is_empty());
    poll.set(false, None);
    let busy = agent.poll_status().await.expect("poll");
    assert_eq!(
        busy,
        vec![ExtStatus {
            ext: "focus".into(),
            state: ExtState::Busy,
            text: None,
        }]
    );
    assert!(!agent.is_quiet().await.expect("quiet predicate"));
    poll.set(true, None);
    assert!(agent.poll_status().await.expect("poll").is_empty());
    assert!(agent.is_quiet().await.expect("quiet predicate"));
    host.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn host_quiet_predicate_follows_open_sessions() {
    let poll = Arc::new(Controlled {
        state: Mutex::new((false, Some("busy".into()))),
    });
    let (_tmp, host, agent) = open(&poll).await;
    agent.poll_status().await.expect("poll");
    assert!(!host.is_quiet().await);
    poll.set(true, None);
    agent.poll_status().await.expect("poll");
    assert!(host.is_quiet().await);
    host.shutdown(Duration::from_secs(1)).await;
}
