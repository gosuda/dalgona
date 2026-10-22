//! `Host::open_replacing`: opening the successor of a session the caller is
//! about to close. Closing a session tears down every live session it owns,
//! so a successor that is the session itself or one of those descendants
//! would be gone before it could be attached. The host refuses such a target
//! while nothing has been closed; every other target opens as `Host::open`
//! opens it. Everything here runs on a real host over a temporary store.
#![expect(
    clippy::expect_used,
    reason = "test fixtures abort at the failed boundary"
)]

use std::collections::BTreeMap;
use std::num::NonZeroU64;

use dal_agent::{Agent, Env, Host, HostError, Product, SessionRef};
use dal_core::{
    CallId, ClientId, Config, ConfigProduct, Entry, EntryId, EntryKind, JournalPart, ListQuery,
    Product as JournalProduct, Record, SessionId, Workspace,
};
use dal_store::Store;

struct Rig {
    _temp: tempfile::TempDir,
    host: Host,
    store: Store,
    workspace: Workspace,
}

fn entry_id(value: u64) -> EntryId {
    EntryId::new(NonZeroU64::new(value).expect("nonzero entry id"))
}

fn user(id: u64, parent: Option<u64>, text: &str) -> Record {
    Record::User(Entry {
        id: entry_id(id),
        parent: parent.map(entry_id),
        at: jiff::Timestamp::UNIX_EPOCH,
        kind: EntryKind::User {
            parts: vec![JournalPart::Text { text: text.into() }],
        },
    })
}

/// Stores one session of `entries` chained user entries, then closes it.
async fn seed(store: &Store, id: SessionId, name: &str, entries: u64) {
    let mut journal = store.create_session(id);
    let records: Vec<Record> = (1..=entries)
        .map(|n| user(n, (n > 1).then(|| n - 1), "seed"))
        .collect();
    journal.append(records).await.expect("seed entries");
    journal.set_name(Some(name)).await.expect("seed name");
    journal.close().await.expect("close seeded journal");
}

/// A host whose depth limit admits a grandchild: the default admits one
/// level of children.
async fn rig() -> Rig {
    let temp = tokio::task::spawn_blocking(tempfile::tempdir)
        .await
        .expect("temporary store task")
        .expect("temporary store");
    let data = temp.path().join("data");
    let directory = temp.path().join("workspace");
    tokio::fs::create_dir_all(&data)
        .await
        .expect("data directory");
    tokio::fs::create_dir_all(&directory)
        .await
        .expect("workspace directory");
    let workspace = Workspace::new(directory.clone()).expect("workspace");
    let store = Store::new(data.clone(), workspace.clone(), JournalProduct::Dal);
    let config = Config::load(
        ConfigProduct::Dalgon,
        &data,
        "",
        Some("[agents]\nmax_depth = 2\n"),
    )
    .expect("configuration");
    let host = Host::start(
        Product {
            name: "dal",
            data_root: data,
            defaults: "",
            extensions: Vec::new(),
            bundled: Vec::new(),
        },
        config,
        Env {
            vars: BTreeMap::new(),
            cwd: directory,
            sandbox_helper: None,
        },
    )
    .await
    .expect("host");
    Rig {
        _temp: temp,
        host,
        store,
        workspace,
    }
}

fn client() -> ClientId {
    ClientId::new("resume-descendant")
}

fn listing() -> ListQuery {
    ListQuery {
        limit: None,
        cursor: None,
        search: None,
    }
}

async fn open_root(rig: &Rig) -> Agent {
    rig.host
        .open(
            SessionRef::New {
                workspace: rig.workspace.clone(),
                name: None,
            },
            client(),
        )
        .await
        .expect("root session")
}

async fn open_child(rig: &Rig, parent: SessionId, call: &str) -> Agent {
    rig.host
        .open(
            SessionRef::Child {
                parent,
                call: CallId::new(call),
                workspace: rig.workspace.clone(),
                name: None,
            },
            client(),
        )
        .await
        .expect("child session")
}

/// The reference the terminal opens a resume target by: the full id in the
/// workspace the listing named.
fn resume(rig: &Rig, id: SessionId) -> SessionRef {
    SessionRef::Resume {
        key: id.to_string().into(),
        workspace: rig.workspace.clone(),
    }
}

/// Whether the session's actor still answers.
async fn alive(agent: &Agent) -> bool {
    agent.poll_status().await.is_ok()
}

/// Whether `result` is the host's refusal and its text names every session
/// in `ids`.
fn refused_naming(result: &Result<Agent, HostError>, ids: &[SessionId]) -> bool {
    matches!(
        result,
        Err(HostError::Config { message })
            if ids.iter().all(|id| message.contains(&id.to_string()))
    )
}

/// A live child is refused, and so is a grandchild from the root and from its
/// own parent. Nothing moves: every session is still live, no session was
/// added, the refusal repeats, and an ordinary close still ends the tree.
#[tokio::test]
async fn a_live_descendant_is_refused_before_anything_is_closed() {
    let rig = rig().await;
    let root = open_root(&rig).await;
    let child = open_child(&rig, root.session(), "child").await;
    let grandchild = open_child(&rig, child.session(), "grandchild").await;
    let before = rig.host.sessions(listing()).expect("listing").items.len();

    for _ in 0..2 {
        for (leaving, target) in [
            (root.session(), child.session()),
            (root.session(), grandchild.session()),
            (child.session(), grandchild.session()),
        ] {
            let result = rig
                .host
                .open_replacing(leaving, resume(&rig, target), client())
                .await;
            assert!(
                refused_naming(&result, &[leaving, target]),
                "{target} under {leaving} is refused"
            );
        }
    }

    assert!(alive(&root).await, "the leaving session stays live");
    assert!(alive(&child).await, "the child stays live");
    assert!(alive(&grandchild).await, "the grandchild stays live");
    assert_eq!(
        rig.host.sessions(listing()).expect("listing").items.len(),
        before,
        "a refusal adds no session"
    );

    rig.host
        .close(root.session())
        .await
        .expect("an ordinary close still succeeds");
    assert!(!alive(&child).await, "the close still ends the subtree");
    assert!(
        !alive(&grandchild).await,
        "the close still ends the subtree"
    );
}

/// The session being left is not its own successor: the close that follows
/// would end it.
#[tokio::test]
async fn the_session_being_left_is_not_its_own_successor() {
    let rig = rig().await;
    let root = open_root(&rig).await;

    let result = rig
        .host
        .open_replacing(root.session(), resume(&rig, root.session()), client())
        .await;

    assert!(refused_naming(&result, &[root.session()]));
    assert!(alive(&root).await, "the refused session stays live");
}

/// A live session outside the leaving session's subtree is a valid
/// successor even while the leaving session owns children of its own, and
/// it outlives the close.
#[tokio::test]
async fn an_independent_live_session_still_takes_over() {
    let rig = rig().await;
    let leaving = open_root(&rig).await;
    let other = open_root(&rig).await;
    let child = open_child(&rig, leaving.session(), "child").await;

    let successor = rig
        .host
        .open_replacing(leaving.session(), resume(&rig, other.session()), client())
        .await
        .expect("an independent live session opens");
    assert_eq!(successor.session(), other.session());

    rig.host
        .close(leaving.session())
        .await
        .expect("the terminal closes the session it left");
    assert!(alive(&successor).await, "the successor outlives the close");
    assert!(!alive(&leaving).await, "the left session is closed");
    assert!(!alive(&child).await, "its own subtree ended as before");
}

/// A stored session that is not live opens as the successor, and survives
/// the close of the session it replaced.
#[tokio::test]
async fn a_stored_session_still_opens_as_the_successor() {
    let rig = rig().await;
    let stored = SessionId::new_v7();
    seed(&rig.store, stored, "stored", 2).await;
    let leaving = open_root(&rig).await;
    let child = open_child(&rig, leaving.session(), "child").await;

    let successor = rig
        .host
        .open_replacing(leaving.session(), resume(&rig, stored), client())
        .await
        .expect("a stored session opens");
    assert_eq!(successor.session(), stored);

    rig.host
        .close(leaving.session())
        .await
        .expect("the terminal closes the session it left");
    assert!(alive(&successor).await, "the successor outlives the close");
    assert!(!alive(&child).await, "the left session's subtree ended");
}

/// A live sibling and a live ancestor are valid successors: the close of the
/// session being left ends neither of them.
#[tokio::test]
async fn a_live_sibling_or_ancestor_still_takes_over() {
    let rig = rig().await;
    let root = open_root(&rig).await;
    let first = open_child(&rig, root.session(), "first").await;
    let second = open_child(&rig, root.session(), "second").await;

    let sibling = rig
        .host
        .open_replacing(first.session(), resume(&rig, second.session()), client())
        .await
        .expect("a sibling opens");
    assert_eq!(sibling.session(), second.session());
    rig.host
        .close(first.session())
        .await
        .expect("the sibling's close succeeds");
    assert!(alive(&sibling).await, "the sibling outlives its sibling");
    assert!(alive(&root).await, "the owner outlives its child");

    let ancestor = rig
        .host
        .open_replacing(second.session(), resume(&rig, root.session()), client())
        .await
        .expect("an ancestor opens");
    assert_eq!(ancestor.session(), root.session());
    rig.host
        .close(second.session())
        .await
        .expect("the child's close succeeds");
    assert!(alive(&ancestor).await, "the ancestor outlives its child");
}

/// A new child of the leaving session is refused as a successor: the close
/// that follows would end it with the rest of the subtree, and the refusal
/// leaves no new session behind. A child of a session outside that subtree
/// still spawns and outlives the close.
#[tokio::test]
async fn a_new_child_of_the_leaving_session_is_refused() {
    let rig = rig().await;
    let root = open_root(&rig).await;
    let other = open_root(&rig).await;
    let before = rig.host.sessions(listing()).expect("listing").items.len();

    let refused = rig
        .host
        .open_replacing(
            root.session(),
            SessionRef::Child {
                parent: root.session(),
                call: CallId::new("successor"),
                workspace: rig.workspace.clone(),
                name: None,
            },
            client(),
        )
        .await;
    assert!(
        refused_naming(&refused, &[root.session()]),
        "a new child of the leaving session is refused"
    );
    assert_eq!(
        rig.host.sessions(listing()).expect("listing").items.len(),
        before,
        "a refused spawn adds no session"
    );

    let nephew = rig
        .host
        .open_replacing(
            root.session(),
            SessionRef::Child {
                parent: other.session(),
                call: CallId::new("nephew"),
                workspace: rig.workspace.clone(),
                name: None,
            },
            client(),
        )
        .await
        .expect("a child outside the leaving subtree spawns");
    rig.host
        .close(root.session())
        .await
        .expect("the leaving session closes");
    assert!(alive(&nephew).await, "the outside child outlives the close");
    assert!(alive(&other).await, "its owner is untouched");
}
