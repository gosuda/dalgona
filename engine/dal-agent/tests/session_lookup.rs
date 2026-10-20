//! Direct session lookup and deletion by the host: `find_session` resolves a
//! reference without the listing cap and names the workspace the session
//! lives in, and `delete_session` refuses a live session while removing a
//! stored one. The listing the resume picker reads is capped at 200 rows per
//! workspace, so a session far down it is reachable only through the lookup.
#![expect(
    clippy::expect_used,
    reason = "test fixtures abort at the failed boundary"
)]

use std::collections::BTreeMap;
use std::num::NonZeroU64;

use dal_agent::{Agent, Env, Host, HostError, Product, SessionRef};
use dal_core::{
    ClientId, Config, ConfigProduct, Entry, EntryId, EntryKind, JournalPart, ListQuery,
    Product as JournalProduct, Record, SessionId, Workspace,
};
use dal_store::{Store, StoreError};

/// The newest-first page size the store applies when a listing names no limit.
const LISTING_CAP: usize = 200;

struct Rig {
    _temp: tempfile::TempDir,
    host: Host,
    store: Store,
    workspace: Workspace,
    /// The live session that makes the workspace one the host knows.
    live: Agent,
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
async fn seed(store: &Store, id: SessionId, name: Option<&str>, entries: u64) {
    let mut journal = store.create_session(id);
    let records: Vec<Record> = (1..=entries)
        .map(|n| user(n, (n > 1).then(|| n - 1), "seed"))
        .collect();
    journal.append(records).await.expect("seed entries");
    if let Some(name) = name {
        journal.set_name(Some(name)).await.expect("seed name");
    }
    journal.close().await.expect("close seeded journal");
}

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
    let config = Config::load(ConfigProduct::Dalgon, &data, "", Some("")).expect("configuration");
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
    let live = host
        .open(
            SessionRef::Ephemeral {
                workspace: workspace.clone(),
            },
            ClientId::new("lookup-test"),
        )
        .await
        .expect("live session");
    Rig {
        _temp: temp,
        host,
        store,
        workspace,
        live,
    }
}

fn same_directory(left: &Workspace, right: &Workspace) -> bool {
    std::fs::canonicalize(left.as_path()).ok() == std::fs::canonicalize(right.as_path()).ok()
}

fn listing() -> ListQuery {
    ListQuery {
        limit: None,
        cursor: None,
        search: None,
    }
}

/// A session beyond the 200-row listing page is invisible to the listing but
/// found by an exact name or its full id, with its workspace and its measured
/// message count.
#[tokio::test]
async fn find_session_reaches_a_session_beyond_the_listing_cap() {
    let rig = rig().await;
    let needle = SessionId::new_v7();
    seed(&rig.store, needle, Some("needle"), 2).await;
    for _ in 0..LISTING_CAP {
        seed(&rig.store, SessionId::new_v7(), None, 1).await;
    }

    let page = rig.host.sessions(listing()).expect("listing");
    assert!(
        page.items.iter().all(|info| info.id != needle),
        "the capped listing never reaches the oldest session"
    );

    let by_name = rig.host.find_session("needle").expect("name lookup");
    assert_eq!(by_name.id, needle);
    assert_eq!(by_name.message_count, Some(2));
    assert!(same_directory(&by_name.workspace, &rig.workspace));

    let by_id = rig
        .host
        .find_session(&needle.to_string())
        .expect("full id lookup");
    assert_eq!(by_id.id, needle);
    assert!(same_directory(&by_id.workspace, &rig.workspace));
}

/// A reference nothing holds is a typed not-found, and an empty one is
/// refused: neither guesses a session or a workspace.
#[tokio::test]
async fn find_session_refuses_what_it_cannot_resolve() {
    let rig = rig().await;
    seed(&rig.store, SessionId::new_v7(), Some("present"), 1).await;
    assert!(matches!(
        rig.host.find_session("absent"),
        Err(HostError::NotFound { .. })
    ));
    assert!(matches!(
        rig.host.find_session(&SessionId::new_v7().to_string()),
        Err(HostError::NotFound { .. })
    ));
    assert!(
        matches!(
            rig.host.find_session(""),
            Err(HostError::Store(StoreError::EmptyRef))
        ),
        "an empty reference is the typed empty-ref refusal, not a not-found"
    );
}

fn fixed(id: &str) -> SessionId {
    SessionId::parse(id).expect("a lowercase hyphenated v7 id")
}

/// The reference rules at their edges, through the host seam: an id prefix
/// needs 4 characters (3 never resolves, even when only one session starts
/// with them) and may stop one character short of the full id; a prefix two
/// sessions share is a typed ambiguity, not the first match; a reference is
/// trimmed before it is looked up and before it is named in the error; a
/// blank one is the empty-reference refusal.
#[tokio::test]
async fn find_session_applies_the_reference_rules_at_their_boundaries() {
    let rig = rig().await;
    let alpha = fixed("0190aaaa-0000-7000-8000-000000000001");
    let twin = fixed("0190aaaa-1111-7000-8000-000000000002");
    let cafe = fixed("cafe0000-0000-7000-8000-000000000003");
    seed(&rig.store, alpha, Some("alpha"), 1).await;
    seed(&rig.store, twin, None, 1).await;
    seed(&rig.store, cafe, None, 1).await;

    assert_eq!(
        rig.host.find_session("cafe").expect("4 characters").id,
        cafe
    );
    assert!(
        matches!(
            rig.host.find_session("caf"),
            Err(HostError::NotFound { .. })
        ),
        "3 characters is below the prefix minimum even when only one session matches"
    );

    let almost_full = &alpha.to_string()[..35];
    assert_eq!(
        rig.host
            .find_session(almost_full)
            .expect("one character short of the full id")
            .id,
        alpha
    );

    let Err(HostError::Store(StoreError::Ambiguous { count, .. })) =
        rig.host.find_session("0190aaaa")
    else {
        panic!("a prefix two sessions share must be an ambiguity");
    };
    assert_eq!(count, 2, "both sessions are counted, neither is picked");

    assert_eq!(
        rig.host
            .find_session(" \talpha\n")
            .expect("a padded name resolves")
            .id,
        alpha
    );
    for blank in ["", "    ", "\t\n"] {
        assert!(
            matches!(
                rig.host.find_session(blank),
                Err(HostError::Store(StoreError::EmptyRef))
            ),
            "{blank:?} is the empty-reference refusal"
        );
    }
    for long in [format!("{alpha}0"), "a".repeat(10_000)] {
        assert!(
            matches!(
                rig.host.find_session(&long),
                Err(HostError::NotFound { .. })
            ),
            "a reference longer than any id never resolves: {} bytes",
            long.len()
        );
    }
    let Err(HostError::NotFound { message }) = rig.host.find_session("  absent ") else {
        panic!("an unknown padded name is a not-found");
    };
    assert_eq!(
        message.as_ref(),
        "no session matches \"absent\".",
        "the error names the trimmed reference"
    );
}

/// A session that is live in the host is found by its full id even though it
/// has no journal on disk (an ephemeral one).
#[tokio::test]
async fn find_session_finds_a_live_session_by_id() {
    let rig = rig().await;
    let live = rig.live.session();
    let found = rig
        .host
        .find_session(&live.to_string())
        .expect("a live session resolves");
    assert_eq!(found.id, live);
    assert_eq!(found.message_count, Some(0));
    let padded = rig
        .host
        .find_session(&format!(" {live}\n"))
        .expect("a padded live id resolves: it has no journal for the store to trim and find");
    assert_eq!(padded.id, live);
}

/// `delete_session` removes a stored session nobody holds, leaves a session
/// that is live in this host untouched, and reports an unknown id as missing.
#[tokio::test]
async fn delete_session_removes_a_stored_session_and_refuses_a_live_one() {
    let rig = rig().await;
    let doomed = SessionId::new_v7();
    let held = SessionId::new_v7();
    seed(&rig.store, doomed, Some("doomed"), 1).await;
    seed(&rig.store, held, Some("held"), 1).await;

    let open = rig
        .host
        .open(
            SessionRef::Resume {
                key: held.to_string().into(),
                workspace: rig.workspace.clone(),
            },
            ClientId::new("lookup-test"),
        )
        .await
        .expect("the stored session opens");
    assert!(
        matches!(rig.host.delete_session(held), Err(HostError::Config { .. })),
        "a live session is refused"
    );
    assert!(
        rig.store.session_file(held).is_file(),
        "the refused session's journal is untouched"
    );
    drop(open);

    rig.host
        .delete_session(doomed)
        .expect("a stored session nobody holds deletes");
    assert!(!rig.store.session_file(doomed).exists());
    assert!(matches!(
        rig.host.find_session(&doomed.to_string()),
        Err(HostError::NotFound { .. })
    ));
    assert!(matches!(
        rig.host.delete_session(SessionId::new_v7()),
        Err(HostError::NotFound { .. })
    ));
}
