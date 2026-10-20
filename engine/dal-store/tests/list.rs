#![expect(clippy::expect_used, reason = "integration tests fail loudly")]
#![expect(clippy::panic, reason = "integration tests fail loudly")]
#![expect(clippy::too_many_lines, reason = "integration tests fail loudly")]
//! Session listing, resume resolution, renaming, and workspace keys.

mod support;

use std::{fs, num::NonZeroU64, time::SystemTime};

use dal_core::{
    Entry, EntryId, EntryKind, JournalPart, ListQuery, Product, Record, SessionId, Workspace,
    encode,
};
use dal_store::{Store, StoreError};
use support::temp_dir::TempDir;

fn entry_id(value: u64) -> EntryId {
    EntryId::new(NonZeroU64::new(value).expect("nonzero test entry id"))
}

fn timestamp() -> jiff::Timestamp {
    jiff::Timestamp::now()
}

fn user(id: u64, text: &str) -> Record {
    Record::User(Entry {
        id: entry_id(id),
        parent: None,
        at: timestamp(),
        kind: EntryKind::User {
            parts: vec![JournalPart::Text { text: text.into() }],
        },
    })
}

fn fixed_id(n: u64) -> SessionId {
    SessionId::parse(&format!("0192aa00-0000-7000-8000-{n:012x}")).expect("fixed id parses")
}

fn setup(tag: &str) -> (TempDir, Store, Workspace) {
    let temp = TempDir::new(tag);
    let workspace =
        Workspace::new(temp.path().join("workspace")).expect("temporary workspace is absolute");
    let store = Store::new(
        temp.path().join("data"),
        workspace.clone(),
        Product::Dalgona,
    );
    (temp, store, workspace)
}

fn journal_path(data_root: &std::path::Path, id: SessionId) -> std::path::PathBuf {
    let sessions = data_root.join("sessions");
    let name = id.to_string();
    let mut stack = vec![sessions];
    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir).expect("list data-root subtree");
        for entry in entries {
            let path = entry.expect("read data-root entry").path();
            if path.is_dir() {
                if path.file_name().is_some_and(|base| base == name.as_str()) {
                    return path.join("journal.jsonl");
                }
                stack.push(path);
            }
        }
    }
    panic!("journal file exists");
}

fn name_of(store: &Store, id: SessionId) -> Option<String> {
    let page = store
        .list(ListQuery {
            limit: Some(500),
            cursor: None,
            search: None,
        })
        .expect("listing succeeds");
    page.items
        .iter()
        .find(|info| info.id == id)
        .and_then(|info| info.name.as_deref().map(str::to_owned))
}

#[tokio::test]
async fn resume_resolution_matrix() {
    let (temp, store, workspace) = setup("list-resolve");
    let data_root = temp.path().join("data");
    let id_a = SessionId::parse("0192aa00-0000-7000-8000-000000000001").expect("id a parses");
    let id_b = SessionId::parse("0192ab00-0000-7000-8000-000000000002").expect("id b parses");
    let mut session_a = store.create_session(id_a);
    session_a
        .append(vec![user(1, "alpha")])
        .await
        .expect("session a appends");
    session_a
        .set_name(Some("parser fix"))
        .await
        .expect("session a is named");
    session_a
        .set_archived(true)
        .await
        .expect("session a is archived");
    session_a.close().await.expect("session a closes");
    let mut session_b = store.create_session(id_b);
    session_b
        .append(vec![user(1, "beta")])
        .await
        .expect("session b appends");
    session_b.close().await.expect("session b closes");
    let path_b = journal_path(&data_root, id_b);
    let mut staged = fs::read(&path_b).expect("read session b journal");
    staged.extend_from_slice(
        &encode(&Record::Name {
            at: timestamp(),
            name: Some("parser fix".into()),
        })
        .expect("name encodes"),
    );
    fs::write(&path_b, &staged).expect("stage a duplicate name");
    let mut session_c = store.create_session(SessionId::new_v7());
    session_c
        .append(vec![user(1, "gamma")])
        .await
        .expect("session c appends");
    session_c
        .set_name(Some("sleepy"))
        .await
        .expect("session c is named");
    session_c
        .set_archived(true)
        .await
        .expect("session c is archived");
    let id_c = session_c.id();
    session_c.close().await.expect("session c closes");

    match store.resolve(&workspace, "parser fix") {
        Err(StoreError::Ambiguous { .. }) => {}
        other => panic!("exact duplicate name is ambiguous, got {other:?}"),
    }
    match store.resolve(&workspace, "0192a") {
        Err(StoreError::Ambiguous { .. }) => {}
        other => panic!("shared id prefix is ambiguous, got {other:?}"),
    }
    assert_eq!(
        store
            .resolve(&workspace, "0192aa")
            .expect("prefix resolves"),
        id_a,
        "0192aa resolves"
    );
    assert!(
        matches!(store.resolve(&workspace, ""), Err(StoreError::EmptyRef)),
        "empty gives EmptyRef"
    );
    let missing = store
        .resolve(&workspace, "zzz")
        .expect_err("zzz matches nothing");
    assert_eq!(
        missing.to_string(),
        format!(
            "no session in workspace {} matches \"zzz\"",
            workspace.as_path().display()
        ),
        "exact NoMatch text"
    );
    assert!(
        matches!(
            store.resolve(&workspace, "0192AA"),
            Err(StoreError::NoMatch { .. })
        ),
        "uppercase prefix gives NoMatch"
    );
    assert_eq!(
        store
            .resolve(&workspace, "sleepy")
            .expect("archived name matches"),
        id_c,
        "archived names still match"
    );
    let other_workspace =
        Workspace::new(temp.path().join("elsewhere")).expect("second workspace is absolute");
    let other_store = Store::new(data_root.clone(), other_workspace.clone(), Product::Dalgona);
    let mut foreign = other_store.create_session(SessionId::new_v7());
    foreign
        .append(vec![user(1, "foreign")])
        .await
        .expect("foreign session appends");
    let foreign_id = foreign.id();
    foreign.close().await.expect("foreign session closes");
    assert!(
        matches!(
            store.resolve(&workspace, &foreign_id.to_string()),
            Err(StoreError::NoMatch { .. })
        ),
        "another workspace's full id does not match"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn symlinked_and_canonical_workspace_spellings_share_sessions() {
    let temp = TempDir::new("list-symlink");
    let real = temp.path().join("real");
    fs::create_dir_all(&real).expect("real workspace exists");
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&real, &link).expect("workspace symlink");

    // A session opened under the symlinked spelling must resolve and list
    // identically from the canonical spelling: both name the same directory.
    let store = Store::new(
        temp.path().join("data"),
        Workspace::new(link.clone()).expect("linked workspace is absolute"),
        Product::Dalgona,
    );
    let id = SessionId::new_v7();
    let mut session = store.create_session(id);
    session
        .append(vec![user(1, "linked")])
        .await
        .expect("session appends");
    session.close().await.expect("session closes");

    let spelled = Workspace::new(real.clone()).expect("real workspace is absolute");
    assert_eq!(
        store
            .resolve(&spelled, &id.to_string())
            .expect("resolve from the canonical spelling"),
        id,
        "symlinked and canonical spellings reach the same session"
    );
    let page = store
        .list(ListQuery {
            limit: Some(500),
            cursor: None,
            search: None,
        })
        .expect("listing succeeds");
    let canonical = std::fs::canonicalize(&real).expect("canonical workspace");
    assert!(
        page.items
            .iter()
            .any(|info| info.id == id && info.workspace.as_path() == canonical.as_path()),
        "sessions record the canonical workspace spelling"
    );
}

#[tokio::test]
async fn continue_skips_archived_newest() {
    let (temp, store, _) = setup("list-newest");
    let data_root = temp.path().join("data");
    let base = SystemTime::now();
    let mut ids = Vec::new();
    for (index, stamp) in [0, 60, 120].into_iter().enumerate() {
        let id = SessionId::new_v7();
        let mut session = store.create_session(id);
        session
            .append(vec![user(1, &format!("session {index}"))])
            .await
            .expect("session appends");
        if index == 2 {
            session
                .set_archived(true)
                .await
                .expect("newest is archived");
        }
        session.close().await.expect("session closes");
        let path = journal_path(&data_root, id);
        let file = fs::File::options()
            .write(true)
            .open(&path)
            .expect("open journal");
        file.set_modified(base + std::time::Duration::from_secs(stamp))
            .expect("stamp the journal");
        ids.push(id);
    }
    assert_eq!(
        store.newest().expect("newest resolves"),
        Some(ids[1]),
        "newest returns the second newest when the newest is archived"
    );
    let empty_workspace =
        Workspace::new(temp.path().join("empty")).expect("empty workspace is absolute");
    let empty_store = Store::new(data_root.clone(), empty_workspace, Product::Dalgona);
    assert_eq!(
        empty_store.newest().expect("empty newest resolves"),
        None,
        "an empty workspace returns None"
    );
    assert_eq!(
        dal_store::NO_EARLIER_SESSION,
        "No earlier session in this workspace. dalgon started a new session.",
        "the exported notice constant keeps its wording (no caller reads it yet)"
    );
    let tie_workspace = Workspace::new(temp.path().join("tie")).expect("tie workspace is absolute");
    let tie_store = Store::new(data_root, tie_workspace, Product::Dalgona);
    let low = fixed_id(1);
    let high = fixed_id(2);
    for id in [low, high] {
        let mut session = tie_store.create_session(id);
        session
            .append(vec![user(1, "tie")])
            .await
            .expect("tie session appends");
        session.close().await.expect("tie session closes");
    }
    let stamp = base + std::time::Duration::from_secs(1000);
    for id in [low, high] {
        let sessions = temp.path().join("data").join("sessions");
        let mut stack = vec![sessions];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).expect("list sessions") {
                let path = entry.expect("read entry").path();
                if path.is_dir() {
                    if path
                        .file_name()
                        .is_some_and(|base| base == id.to_string().as_str())
                    {
                        let file = fs::File::options()
                            .write(true)
                            .open(path.join("journal.jsonl"))
                            .expect("open tie journal");
                        file.set_modified(stamp).expect("stamp the tie journal");
                    } else {
                        stack.push(path);
                    }
                }
            }
        }
    }
    assert_eq!(
        tie_store.newest().expect("tie newest resolves"),
        Some(high),
        "equal mtimes choose the higher id"
    );
}

#[tokio::test]
async fn rename_validation_cases() {
    let (_temp, store, _) = setup("list-rename");
    let mut first = store.create_session(SessionId::new_v7());
    first
        .append(vec![user(1, "first")])
        .await
        .expect("first session appends");
    let error = first
        .set_name(Some("cafe"))
        .await
        .expect_err("id-like name is invalid");
    assert!(
        matches!(error, StoreError::InvalidName),
        "cafe is invalid because it is id-like"
    );
    first
        .set_name(Some("  a\nb  "))
        .await
        .expect("whitespace name normalizes");
    assert_eq!(
        name_of(&store, first.id()).as_deref(),
        Some("a b"),
        "trimming yields a b"
    );
    let mut second = store.create_session(SessionId::new_v7());
    second
        .append(vec![user(1, "second")])
        .await
        .expect("second session appends");
    second
        .set_name(Some("taken"))
        .await
        .expect("second session takes the name");
    let taken = second.id();
    let duplicate = first
        .set_name(Some("taken"))
        .await
        .expect_err("duplicate fails");
    assert_eq!(
        duplicate.to_string(),
        format!("the name \"taken\" is already used by session {taken} in this workspace"),
        "exact NameTaken text"
    );
    first.set_name(None).await.expect("name removes");
    assert_eq!(name_of(&store, first.id()), None, "None removes the name");
    let mut ephemeral = store.ephemeral_session(SessionId::new_v7());
    ephemeral
        .set_name(Some("taken"))
        .await
        .expect("an ephemeral duplicate succeeds");
}

#[tokio::test]
async fn name_cleared_before_first_flush_claims_nothing() {
    let (_temp, store, _) = setup("list-clear-lazy");
    let mut first = store.create_session(SessionId::new_v7());
    first
        .set_name(Some("taken"))
        .await
        .expect("name buffers while lazy");
    first
        .set_name(None)
        .await
        .expect("clear buffers while lazy");
    let mut second = store.create_session(SessionId::new_v7());
    second
        .append(vec![user(1, "second")])
        .await
        .expect("second session appends");
    second
        .set_name(Some("taken"))
        .await
        .expect("second session takes the name");
    first
        .append(vec![user(1, "first")])
        .await
        .expect("a name cleared before the first flush claims nothing");
    assert_eq!(
        name_of(&store, first.id()),
        None,
        "the cleared name stays cleared after the first flush"
    );
}

#[tokio::test]
async fn paged_listing_cursor_and_search() {
    let (temp, store, workspace) = setup("list-paged");
    let data_root = temp.path().join("data");
    let mut created = Vec::new();
    for index in 1u64..=1000 {
        let id = fixed_id(index);
        let mut session = store.create_session(id);
        let text = if index == 777 {
            "fix the parser"
        } else {
            "routine note"
        };
        session
            .append(vec![user(1, text)])
            .await
            .expect("session appends");
        session.close().await.expect("session closes");
        created.push(id);
    }
    let mut seen = Vec::new();
    let mut cursor: Option<std::boxed::Box<str>> = None;
    loop {
        let page = store
            .list(ListQuery {
                limit: Some(2),
                cursor: cursor.clone(),
                search: None,
            })
            .expect("page lists");
        seen.extend(page.items.iter().map(|info| info.id));
        match page.next_before {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(seen.len(), 1000, "every session appears once");
    let mut ordered: Vec<String> = seen.iter().map(ToString::to_string).collect();
    ordered.sort();
    ordered.dedup();
    assert_eq!(ordered.len(), 1000, "no session repeats");
    let mut expected = created.clone();
    expected.reverse();
    assert_eq!(seen, expected, "pages run in descending order");
    assert!(
        matches!(
            store.list(ListQuery {
                limit: Some(0),
                cursor: None,
                search: None
            }),
            Err(StoreError::ListLimit)
        ),
        "limit 0 returns ListLimit"
    );
    assert!(
        matches!(
            store.list(ListQuery {
                limit: Some(501),
                cursor: None,
                search: None
            }),
            Err(StoreError::ListLimit)
        ),
        "limit 501 returns ListLimit"
    );
    assert!(
        matches!(
            store.list(ListQuery {
                limit: Some(2),
                cursor: Some("bogus".into()),
                search: None,
            }),
            Err(StoreError::MalformedCursor)
        ),
        "malformed cursor returns MalformedCursor"
    );
    let found = store
        .list(ListQuery {
            limit: Some(500),
            cursor: None,
            search: Some("FIX".into()),
        })
        .expect("search lists");
    assert!(
        found
            .items
            .iter()
            .any(|info| info.preview.as_ref() == "fix the parser"),
        "search FIX matches preview fix the parser"
    );
    let mut damaged = store.create_session(SessionId::new_v7());
    damaged
        .append(vec![user(1, "will break")])
        .await
        .expect("damaged session appends");
    let damaged_id = damaged.id();
    damaged.close().await.expect("damaged session closes");
    fs::write(journal_path(&data_root, damaged_id), b"not json at all\n")
        .expect("break the header");
    let relisted = store
        .list(ListQuery {
            limit: Some(500),
            cursor: None,
            search: None,
        })
        .expect("listing survives damage");
    let placeholder = relisted
        .items
        .iter()
        .find(|info| info.id == damaged_id)
        .expect("damaged session is listed");
    assert_eq!(
        placeholder.preview.as_ref(),
        "(damaged session file)",
        "unreadable header uses the damaged placeholder"
    );
    let ghost = fixed_id(999_999);
    let sessions = data_root.join("sessions");
    let mut workspace_dir = None;
    let mut stack = vec![sessions.clone()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("list sessions") {
            let path = entry.expect("read entry").path();
            if path.is_dir() {
                if path.join("journal.jsonl").is_file() {
                    workspace_dir = Some(path.parent().expect("workspace dir").to_path_buf());
                } else {
                    stack.push(path);
                }
            }
        }
    }
    let workspace_dir = workspace_dir.expect("workspace dir exists");
    fs::create_dir_all(workspace_dir.join(ghost.to_string())).expect("ghost dir");
    let skipped = store
        .list(ListQuery {
            limit: Some(500),
            cursor: None,
            search: None,
        })
        .expect("listing skips ghosts");
    assert!(
        skipped.items.iter().all(|info| info.id != ghost),
        "a directory without a journal is skipped"
    );
    let _ = workspace;
}

#[tokio::test]
async fn stale_cache_triggers_rescan() {
    let (_temp, store, _) = setup("list-stale");
    let mut session = store.create_session(SessionId::new_v7());
    session
        .append(vec![user(1, "first")])
        .await
        .expect("session appends");
    let id = session.id();
    session.close().await.expect("session closes");
    let first = store
        .list(ListQuery {
            limit: Some(500),
            cursor: None,
            search: None,
        })
        .expect("first listing primes the cache");
    let before = first
        .items
        .iter()
        .find(|info| info.id == id)
        .expect("session is listed")
        .updated_at;
    let (mut reopened, _) = store.open_session(id).await.expect("session reopens");
    reopened
        .append(vec![user(2, "second")])
        .await
        .expect("session grows");
    reopened.close().await.expect("session closes");
    let second = store
        .list(ListQuery {
            limit: Some(500),
            cursor: None,
            search: None,
        })
        .expect("second listing rescans");
    let after = second
        .items
        .iter()
        .find(|info| info.id == id)
        .expect("session is listed again")
        .updated_at;
    assert!(after > before, "list returns rescanned journal facts");
    let third = store
        .list(ListQuery {
            limit: Some(500),
            cursor: None,
            search: None,
        })
        .expect("third listing succeeds");
    let cached = third
        .items
        .iter()
        .find(|info| info.id == id)
        .expect("session is listed a third time")
        .updated_at;
    assert_eq!(cached, after, "the next call reuses the in-memory cache");
}

#[tokio::test]
async fn workspace_key_uses_full_canonical_path() {
    let temp = TempDir::new("list-wskey");
    let data_root = temp.path().join("data");
    let workspace_a =
        Workspace::new(temp.path().join("ws-a").join("same")).expect("workspace a is absolute");
    let workspace_b =
        Workspace::new(temp.path().join("ws-b").join("same")).expect("workspace b is absolute");
    let store_a = Store::new(data_root.clone(), workspace_a, Product::Dalgona);
    let store_b = Store::new(data_root.clone(), workspace_b, Product::Dalgona);
    for store in [&store_a, &store_b] {
        let mut session = store.create_session(SessionId::new_v7());
        session
            .append(vec![user(1, "hello")])
            .await
            .expect("session appends");
        session.close().await.expect("session closes");
    }
    let mut dirs = Vec::new();
    let sessions = data_root.join("sessions");
    for entry in fs::read_dir(&sessions).expect("list workspace keys") {
        let path = entry.expect("read entry").path();
        if path.is_dir() {
            dirs.push(path);
        }
    }
    assert_eq!(dirs.len(), 2, "same basenames get distinct workspace dirs");
    let names: Vec<String> = dirs
        .iter()
        .map(|dir| {
            dir.file_name()
                .expect("dir name")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_ne!(names[0], names[1], "suffixes derive from the full path");
    for name in &names {
        assert!(name.starts_with("same-"), "sanitized basename is kept");
        let suffix = name.rsplit('-').next().expect("suffix exists");
        assert_eq!(suffix.len(), 12, "twelve hex digits of digest");
        assert!(
            suffix.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "suffix is hex"
        );
    }
    let odd_workspace =
        Workspace::new(temp.path().join("we ird!x")).expect("odd workspace is absolute");
    let odd_store = Store::new(data_root.clone(), odd_workspace, Product::Dalgona);
    let mut odd = odd_store.create_session(SessionId::new_v7());
    odd.append(vec![user(1, "hello")])
        .await
        .expect("odd session appends");
    odd.close().await.expect("odd session closes");
    let mut odd_dir = None;
    for entry in fs::read_dir(&sessions).expect("relist workspace keys") {
        let path = entry.expect("read entry").path();
        let name = path
            .file_name()
            .expect("dir name")
            .to_string_lossy()
            .into_owned();
        if name.starts_with("we_ird_x-") {
            odd_dir = Some(name);
        }
    }
    let odd_name = odd_dir.expect("invalid basename bytes become _");
    let visible = odd_name.rsplit_once('-').expect("suffix split").0;
    assert!(
        visible.len() <= 32,
        "the visible prefix is at most 32 bytes"
    );
    let long_workspace =
        Workspace::new(temp.path().join("a".repeat(40))).expect("long workspace is absolute");
    let long_store = Store::new(data_root, long_workspace, Product::Dalgona);
    let mut long = long_store.create_session(SessionId::new_v7());
    long.append(vec![user(1, "hello")])
        .await
        .expect("long session appends");
    long.close().await.expect("long session closes");
    let mut long_name = None;
    for entry in fs::read_dir(&sessions).expect("relist again") {
        let path = entry.expect("read entry").path();
        let name = path
            .file_name()
            .expect("dir name")
            .to_string_lossy()
            .into_owned();
        if name.starts_with(&"a".repeat(32)) && name.len() == 32 + 1 + 12 {
            long_name = Some(name);
        }
    }
    let long_name = long_name.expect("long basename is cut to 32 bytes");
    assert_eq!(
        long_name.len(),
        32 + 1 + 12,
        "prefix plus separator plus digest"
    );
}

#[tokio::test]
async fn unusable_info_cache_falls_back_to_the_journal() {
    let (_temp, store, _) = setup("list-cache-loss");
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "first")])
        .await
        .expect("first append");
    journal.set_name(Some("kept")).await.expect("name appends");
    journal.close().await.expect("session closes");
    let info = store.session_file(id).with_file_name("info.json");

    fs::write(&info, b"\xff{not json").expect("corrupt the cache");
    assert_eq!(
        name_of(&store, id).as_deref(),
        Some("kept"),
        "garbage cache"
    );
    fs::remove_file(&info).expect("remove the cache");
    assert_eq!(name_of(&store, id).as_deref(), Some("kept"), "absent cache");
}

#[tokio::test]
async fn list_limit_of_one_returns_one_of_two_sessions() {
    let (_temp, store, _) = setup("list-limit-one");
    for n in 1..=2 {
        let mut journal = store.create_session(fixed_id(n));
        journal
            .append(vec![user(1, "first")])
            .await
            .expect("append");
        journal.close().await.expect("session closes");
    }
    let page = store
        .list(ListQuery {
            limit: Some(1),
            cursor: None,
            search: None,
        })
        .expect("limit 1 is the lower bound");
    assert_eq!(page.items.len(), 1, "exactly the limit comes back");
    assert!(
        page.next_before.is_some(),
        "the second session is reachable by cursor"
    );
}

fn leaf(to: u64) -> Record {
    Record::Leaf {
        at: timestamp(),
        to: Some(entry_id(to)),
    }
}

fn message_count_of(store: &Store, id: SessionId) -> Option<u64> {
    let page = store
        .list(ListQuery {
            limit: Some(500),
            cursor: None,
            search: None,
        })
        .expect("listing succeeds");
    page.items
        .iter()
        .find(|info| info.id == id)
        .expect("the session is listed")
        .message_count
}

/// The listed count is the length of the visible leaf path: the store parents
/// each appended entry on the current leaf, a leaf record moves it, and
/// entries on other branches do not count. A durable session with no entry (a journal of
/// header and boot only) lists zero, not unknown.
#[tokio::test]
async fn listing_counts_the_entries_on_the_visible_leaf_path() {
    let (_temp, store, _) = setup("list-leaf-count");
    let empty = fixed_id(1);
    let mut journal = store.create_session(empty);
    journal
        .materialize()
        .await
        .expect("empty session materializes");
    journal.close().await.expect("empty session closes");
    assert_eq!(message_count_of(&store, empty), Some(0), "no entry is zero");

    // 1 -> 2 -> 3, then the leaf moves back to 1 and 4 starts a sibling branch.
    let branched = fixed_id(2);
    let mut journal = store.create_session(branched);
    journal
        .append(vec![
            user(1, "a"),
            user(2, "b"),
            user(3, "c"),
            leaf(1),
            user(4, "d"),
        ])
        .await
        .expect("append");
    journal.close().await.expect("session closes");
    assert_eq!(
        message_count_of(&store, branched),
        Some(2),
        "the last appended entry is the leaf: path 1 -> 4"
    );

    let moved = fixed_id(3);
    let mut journal = store.create_session(moved);
    journal
        .append(vec![
            user(1, "a"),
            user(2, "b"),
            user(3, "c"),
            leaf(1),
            user(4, "d"),
            leaf(3),
        ])
        .await
        .expect("append");
    journal.close().await.expect("session closes");
    assert_eq!(
        message_count_of(&store, moved),
        Some(3),
        "a leaf record moves the path to 1 -> 2 -> 3"
    );
}

/// A cache written before the count existed lacks the member. It reads as a
/// miss and the journal is rescanned: the count is still exact, never zero.
#[tokio::test]
async fn an_info_cache_without_the_count_rescans_the_journal() {
    let (temp, store, _) = setup("list-count-old-cache");
    let id = fixed_id(1);
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "a"), user(2, "b"), user(3, "c")])
        .await
        .expect("append");
    journal.close().await.expect("session closes");
    let info = store.session_file(id).with_file_name("info.json");
    let text = fs::read_to_string(&info).expect("the open wrote its cache");
    let cut = text
        .find(",\"message_count\"")
        .expect("the cache carries the count");
    fs::write(&info, format!("{}}}", &text[..cut])).expect("rewrite the cache as an old writer");

    let fresh = Store::new(
        temp.path().join("data"),
        Workspace::new(temp.path().join("workspace")).expect("absolute workspace"),
        Product::Dalgona,
    );
    assert_eq!(message_count_of(&fresh, id), Some(3));
}

/// A leaf that names an entry the journal never appended is corruption at
/// rest, not an empty branch: the rewritten tail invalidates the cache, the
/// listing rescans the journal, validation refuses the dangling reference,
/// and the count lists as unknown (`None`) — never `Some(0)`, never the
/// pre-damage count.
#[tokio::test]
async fn listing_reports_unknown_count_for_a_leaf_without_its_entry() {
    let (temp, store, workspace) = setup("list-leaf-unknown");
    let id = fixed_id(1);
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "a"), user(2, "b"), leaf(2)])
        .await
        .expect("append");
    journal.close().await.expect("session closes");
    assert_eq!(
        message_count_of(&store, id),
        Some(2),
        "two entries list two before the damage"
    );

    // Read the real journal bytes and flip only the final Leaf record's
    // target id: the last line is the exact on-disk shape of the leaf move,
    // so replacing just its `"to":` number keeps every other byte intact.
    // The rewritten journal invalidates the cache; removing it forces the
    // rescan anyway.
    let journal_path = store.session_file(id);
    let text = fs::read_to_string(&journal_path).expect("read the real journal");
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let last = lines.pop().expect("journal ends in a real leaf line");
    assert!(
        last.contains("\"leaf\"") && last.contains("\"to\":2}"),
        "the final line is the real leaf move to entry 2, got: {last}"
    );
    lines.push(last.replacen("\"to\":2}", "\"to\":777777}", 1));
    let mut staged = lines.join("\n");
    staged.push('\n');
    fs::write(&journal_path, staged).expect("stage the dangling leaf");
    fs::remove_file(journal_path.with_file_name("info.json")).expect("drop the cache");

    let fresh = Store::new(temp.path().join("data"), workspace, Product::Dalgona);
    assert_eq!(
        message_count_of(&fresh, id),
        None,
        "a leaf without its entry lists unknown, not zero and not the old count"
    );
}

/// Lookups and uniqueness checks read identity only. A cache that predates
/// the message count still answers them, so `--resume NAME` never rescans
/// every journal in the workspace just because the count is absent; only a
/// listing, which shows the count, falls back to the journal.
#[tokio::test]
async fn name_resolution_trusts_an_info_cache_that_predates_the_count() {
    let (temp, store, workspace) = setup("resolve-old-cache");
    let id = fixed_id(1);
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "first")])
        .await
        .expect("append");
    journal.set_name(Some("kept")).await.expect("name appends");
    journal.close().await.expect("session closes");

    let info = store.session_file(id).with_file_name("info.json");
    let text = fs::read_to_string(&info).expect("the close wrote its cache");
    let cut = text
        .find(",\"message_count\"")
        .expect("the cache carries the count");
    fs::write(&info, format!("{}}}", &text[..cut])).expect("rewrite the cache as an old writer");
    // Same-length garbage over the journal: a rescan could not recover the
    // name, so only the cache can answer for it.
    let journal_path = store.session_file(id);
    let len = usize::try_from(fs::metadata(&journal_path).expect("journal stats").len())
        .expect("journal length fits");
    fs::write(&journal_path, vec![b'x'; len]).expect("damage the journal in place");

    let fresh = Store::new(
        temp.path().join("data"),
        workspace.clone(),
        Product::Dalgona,
    );
    assert_eq!(
        fresh.resolve(&workspace, "kept").expect("identity read"),
        id,
        "the identity read trusts the cache"
    );
    assert_eq!(
        message_count_of(&fresh, id),
        None,
        "a listing needs the count, rescans, and finds the damage"
    );
}

/// `Store::info` reads one session's listing row by id, with no page and no
/// cap, and reports a missing session as not found.
#[tokio::test]
async fn info_reads_one_session_by_id() {
    let (_temp, store, _) = setup("info-by-id");
    let id = fixed_id(7);
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "a"), user(2, "b"), user(3, "c")])
        .await
        .expect("append");
    journal.set_name(Some("solo")).await.expect("name appends");
    journal.close().await.expect("session closes");

    let info = store.info(id).expect("a stored session reads");
    assert_eq!(info.id, id);
    assert_eq!(info.name.as_deref(), Some("solo"));
    assert_eq!(info.message_count, Some(3));
    assert!(matches!(
        store.info(fixed_id(8)),
        Err(StoreError::NotFound { .. })
    ));
}

fn page(
    store: &Store,
    limit: u32,
    cursor: Option<&str>,
    search: Option<&str>,
) -> dal_core::Page<dal_core::SessionInfo, Box<str>> {
    store
        .list(ListQuery {
            limit: Some(limit),
            cursor: cursor.map(Into::into),
            search: search.map(Into::into),
        })
        .expect("listing succeeds")
}

async fn store_with_sessions(tag: &str, count: u64) -> (TempDir, Store) {
    let (temp, store, _) = setup(tag);
    for n in 1..=count {
        let mut journal = store.create_session(fixed_id(n));
        journal
            .append(vec![user(1, "needle in a haystack")])
            .await
            .expect("session appends");
        journal.close().await.expect("session closes");
    }
    (temp, store)
}

/// A page that reaches the last session must say so: a cursor on the final
/// full page would send every client back for one more empty request.
#[tokio::test]
async fn paging_reports_no_cursor_once_the_limit_reaches_the_last_session() {
    let (_temp, store) = store_with_sessions("list-edge-pages", 3).await;
    for search in [None, Some("needle")] {
        let all = page(&store, 3, None, search);
        assert_eq!(all.items.len(), 3, "limit == count returns every session");
        assert_eq!(
            all.next_before, None,
            "limit == count: nothing remains ({search:?})"
        );
        let roomy = page(&store, 4, None, search);
        assert_eq!(
            roomy.items.len(),
            3,
            "limit == count + 1 returns every session"
        );
        assert_eq!(
            roomy.next_before, None,
            "limit == count + 1: nothing remains ({search:?})"
        );

        let first = page(&store, 2, None, search);
        assert_eq!(first.items.len(), 2, "limit == count - 1 fills the page");
        let cursor = first
            .next_before
            .as_deref()
            .expect("one session remains, so a cursor is issued");
        let rest = page(&store, 2, Some(cursor), search);
        assert_eq!(
            rest.items.len(),
            1,
            "the cursor reaches exactly the remainder"
        );
        assert_eq!(
            rest.next_before, None,
            "the remainder ends the listing ({search:?})"
        );
        let ids: Vec<_> = first
            .items
            .iter()
            .chain(&rest.items)
            .map(|info| info.id)
            .collect();
        assert_eq!(
            ids,
            vec![fixed_id(3), fixed_id(2), fixed_id(1)],
            "newest first, with no session repeated or skipped"
        );
    }
}

/// Id prefixes shorter than four characters are too ambiguous to resolve; the
/// boundary is exact on both sides.
#[tokio::test]
async fn resolve_prefix_length_boundaries_hold_on_both_sides() {
    let (_temp, store, workspace) = setup("list-prefix-bounds");
    let id = fixed_id(1);
    let mut journal = store.create_session(id);
    journal
        .append(vec![user(1, "only session")])
        .await
        .expect("session appends");
    journal.close().await.expect("session closes");
    let full = id.to_string();
    assert!(
        matches!(
            store.resolve(&workspace, &full[..3]),
            Err(StoreError::NoMatch { .. })
        ),
        "a 3-character prefix never resolves"
    );
    assert_eq!(
        store
            .resolve(&workspace, &full[..4])
            .expect("4-character prefix resolves"),
        id,
        "a 4-character prefix is the shortest that resolves"
    );
    assert_eq!(
        store
            .resolve(&workspace, &full[..35])
            .expect("35-character prefix resolves"),
        id,
        "a prefix one character short of the full id resolves"
    );
    assert_eq!(
        store.resolve(&workspace, &full).expect("full id resolves"),
        id
    );
    assert!(
        matches!(
            store.resolve(&workspace, &format!("{full}1")),
            Err(StoreError::NoMatch { .. })
        ),
        "an id plus one extra character matches nothing"
    );
}
