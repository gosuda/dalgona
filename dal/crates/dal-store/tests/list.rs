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
        "the CLI-owned caller text is unchanged"
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
