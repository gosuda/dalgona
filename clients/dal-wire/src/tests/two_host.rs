// Copyright (c) Cognition Inc. and other dal contributors.
// SPDX-License-Identifier: MIT

// A pair of `RemoteHost` clients against one real `Host` served over the real
// local transport: session dedupe, listing parity, and cross-client host
// subscriptions.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use dal_agent::{Env, Host, Product, SessionRef};
use dal_core::{Command, Config, ConfigProduct, ListQuery, Workspace};
use tempfile::TempDir;

use crate::remote::{RemoteEndpoint, RemoteHost, RemoteHostUpdate};
use crate::rpc::serve_rpc;
use crate::transport::serve_local;

const TIMEOUT: Duration = Duration::from_secs(10);

/// One real `Host` served on a local socket plus the data root it writes into.
struct WireFixture {
    _dir: TempDir,
    workspace: PathBuf,
    endpoint: RemoteEndpoint,
    server: tokio::task::JoinSet<()>,
}

impl WireFixture {
    async fn start() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let product = Product {
            name: "dal",
            data_root: dir.path().join("data"),
            defaults: "",
            extensions: Vec::new(),
            bundled: Vec::new(),
        };
        let workspace = dir.path().join("ws");
        let env = Env {
            vars: BTreeMap::new(),
            cwd: dir.path().to_path_buf(),
            sandbox_helper: None,
        };
        let config = Config::load(ConfigProduct::Dalgon, &dir.path().join("data"), "", None)
            .expect("default config");
        let host = Host::start(product, config, env)
            .await
            .expect("host starts");

        // The listener refuses sockets whose parent directory is not private.
        let socket_dir = dir.path().join("rpc");
        std::fs::create_dir(&socket_dir).expect("socket dir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&socket_dir, std::fs::Permissions::from_mode(0o700))
                .expect("private socket dir");
        }
        let socket = socket_dir.join("dal.sock");
        // `tokio::spawn` is workspace-banned; JoinSet is the tracked spawn.
        let mut server = tokio::task::JoinSet::new();
        {
            let socket = socket.clone();
            server.spawn(async move {
                serve_local(&socket, None, None, move |transport| {
                    let host = host.clone();
                    Box::pin(async move { serve_rpc(host, transport).await })
                })
                .await
                .expect("local server runs");
            });
        }

        // Wait until the listener materializes before clients dial.
        let endpoint = RemoteEndpoint::LocalSocket(socket);
        for _ in 0..50 {
            if RemoteHost::connect(endpoint.clone()).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        Self {
            _dir: dir,
            workspace,
            endpoint,
            server,
        }
    }

    async fn client(&self) -> RemoteHost {
        RemoteHost::connect(self.endpoint.clone())
            .await
            .expect("client connects")
    }

    fn workspace(&self) -> Workspace {
        Workspace::new(self.workspace.clone()).expect("absolute workspace")
    }
}

impl Drop for WireFixture {
    fn drop(&mut self) {
        self.server.abort_all();
    }
}

/// A `New` open from one client and a `Resume` from another must resolve to the
/// same live session, and both clients must see it in `session/list`.
#[tokio::test]
async fn a_second_client_resumes_the_session_the_first_opened() {
    let fixture = WireFixture::start().await;
    let first = fixture.client().await;
    let second = fixture.client().await;

    let workspace = fixture.workspace();
    let opened = first
        .open(SessionRef::New {
            workspace: workspace.clone(),
            name: Some("pair".into()),
        })
        .await
        .expect("first client opens a session");
    let resumed = second
        .open(SessionRef::Resume {
            key: opened.session().to_string().into_boxed_str(),
            workspace,
        })
        .await
        .expect("second client resumes by id");

    assert_eq!(
        opened.session(),
        resumed.session(),
        "both clients resolved the same live session"
    );

    let query = ListQuery {
        limit: None,
        cursor: None,
        search: None,
    };
    let (listed_a, listed_b) = tokio::join!(first.sessions(&query), second.sessions(&query),);
    let listed_a = listed_a.expect("first listing");
    let listed_b = listed_b.expect("second listing");
    assert_eq!(
        listed_a.items.len(),
        listed_b.items.len(),
        "both clients see the same session count"
    );
    // The at-open name is buffered in the lazy journal and only materializes in
    // `session/list` after the first durable record — the listing parity claim
    // here is session identity, which the rename test covers for names.
    assert!(
        listed_b
            .items
            .iter()
            .any(|item| item.id == opened.session())
    );
}

/// A rename submitted through one client must appear in the other's
/// `session/list` and on the other's host subscription: cross-client
/// visibility is the transport contract, not a local shortcut.
#[tokio::test]
async fn a_rename_by_one_client_reaches_the_other() {
    let fixture = WireFixture::start().await;
    let first = fixture.client().await;
    let second = fixture.client().await;

    let agent = first
        .open(SessionRef::New {
            workspace: fixture.workspace(),
            name: None,
        })
        .await
        .expect("open session");
    let mut host_updates = second.subscribe().await.expect("host subscription");

    agent
        .submit(Command::Rename("renamed".into()))
        .await
        .expect("rename accepted");

    // The second client's session listing must already reflect the rename.
    let listed = second
        .sessions(&ListQuery {
            limit: None,
            cursor: None,
            search: None,
        })
        .await
        .expect("listing");
    let entry = listed
        .items
        .iter()
        .find(|item| item.id == agent.session())
        .expect("session is listed");
    assert_eq!(entry.name.as_deref(), Some("renamed"));

    // And its host subscription delivers the matching SessionChanged.
    let mut saw = false;
    for _ in 0..8 {
        let update = tokio::time::timeout(TIMEOUT, host_updates.next())
            .await
            .expect("host update arrives")
            .expect("update decodes");
        if let RemoteHostUpdate::SessionChanged(info) = update
            && info.id == agent.session()
            && info.name.as_deref() == Some("renamed")
        {
            saw = true;
            break;
        }
    }
    assert!(saw, "rename reached the other client's host subscription");
}

/// Closing a session through one client removes it from the other client's
/// listing and posts `SessionRemoved` on its host subscription.
#[tokio::test]
async fn a_close_by_one_client_removes_the_session_for_the_other() {
    let fixture = WireFixture::start().await;
    let first = fixture.client().await;
    let second = fixture.client().await;

    let agent = first
        .open(SessionRef::New {
            workspace: fixture.workspace(),
            name: Some("pair".into()),
        })
        .await
        .expect("open session");
    let mut host_updates = second.subscribe().await.expect("host subscription");

    first.close(agent.session()).await.expect("close session");

    let listed = second
        .sessions(&ListQuery {
            limit: None,
            cursor: None,
            search: None,
        })
        .await
        .expect("listing");
    assert!(listed.items.iter().all(|item| item.id != agent.session()));

    let mut saw = false;
    for _ in 0..8 {
        let update = tokio::time::timeout(TIMEOUT, host_updates.next())
            .await
            .expect("host update arrives")
            .expect("update decodes");
        if let RemoteHostUpdate::SessionRemoved(id) = update
            && id == agent.session()
        {
            saw = true;
            break;
        }
    }
    assert!(saw, "close reached the other client's host subscription");
}
