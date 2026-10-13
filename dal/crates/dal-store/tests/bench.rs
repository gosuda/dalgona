#![expect(clippy::expect_used, reason = "integration tests fail loudly")]
#![expect(clippy::panic, reason = "integration tests fail loudly")]
//! Startup and listing benchmarks for the session store.
//!
//! Warms a large journal plus a session fleet, prints measured timings, and
//! fails when any measurement exceeds three times its budget.

use std::{fs, num::NonZeroU64, time::Instant};

use dal_core::{
    Entry, EntryId, EntryKind, JournalPart, ListQuery, Product, Record, SessionId, Workspace,
    encode,
};

fn entry_id(value: u64) -> EntryId {
    EntryId::new(NonZeroU64::new(value).expect("nonzero test entry id"))
}

fn find_journal(data_root: &std::path::Path, id: SessionId) -> std::path::PathBuf {
    let sessions = data_root.join("sessions");
    let name = id.to_string();
    let mut stack = vec![sessions];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("list data-root subtree") {
            let path = entry.expect("read entry").path();
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

fn user_record(id: u64, at: jiff::Timestamp, text: &str) -> Record {
    Record::User(Entry {
        id: entry_id(id),
        parent: None,
        at,
        kind: EntryKind::User {
            parts: vec![JournalPart::Text { text: text.into() }],
        },
    })
}

fn startup_and_listing_benchmarks() {
    let root = std::env::temp_dir().join(format!("dal-store-bench-{}", std::process::id()));
    fs::create_dir_all(&root).expect("bench root exists");
    let workspace =
        Workspace::new(root.join("workspace")).expect("temporary workspace is absolute");
    let data_root = root.join("data");
    let store = dal_store::Store::new(data_root.clone(), workspace.clone(), Product::Dalgona);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("bench runtime builds");

    let big_id = SessionId::new_v7();
    runtime.block_on(async {
        let mut journal = store.create_session(big_id);
        journal
            .append(vec![user_record(1, jiff::Timestamp::now(), "seed")])
            .await
            .expect("seed appends");
        journal.close().await.expect("seed closes");
    });
    let path = find_journal(&data_root, big_id);
    let mut staged = fs::read(&path).expect("read seed journal");
    let text = "b".repeat(500);
    for index in 2u64..=200_001 {
        let at = jiff::Timestamp::from_second(1_789_000_000).expect("bench timestamp");
        let line = encode(&user_record(index, at, &text)).expect("record encodes");
        staged.extend_from_slice(&line);
    }
    fs::write(&path, &staged).expect("stage a 100 MiB journal");
    assert!(
        fs::metadata(&path).expect("journal stat").len() >= 100 * 1024 * 1024,
        "a 100 MiB journal is warmed"
    );

    for index in 1u64..=1000 {
        let id = SessionId::parse(&format!("0192aa00-0000-7000-8000-{index:012x}"))
            .expect("fleet id parses");
        runtime.block_on(async {
            let mut journal = store.create_session(id);
            journal
                .append(vec![user_record(1, jiff::Timestamp::now(), "fleet")])
                .await
                .expect("fleet session appends");
            journal.close().await.expect("fleet session closes");
        });
    }

    let started = Instant::now();
    let (reopened, _) = runtime
        .block_on(store.open_session(big_id))
        .expect("big journal opens");
    let mut bytes = 0u64;
    let mut records = 0u32;
    for record in reopened.records() {
        let line = encode(record).expect("record encodes");
        bytes += u64::try_from(line.len()).expect("page fits memory");
        records += 1;
        if records >= 200 && bytes >= 1024 * 1024 {
            break;
        }
    }
    let open_page_ms = started.elapsed().as_millis();
    println!("open_plus_first_page_ms={open_page_ms} records={records} bytes={bytes}");

    let started = Instant::now();
    let page = store
        .list(ListQuery {
            limit: Some(50),
            cursor: None,
            search: None,
        })
        .expect("list succeeds");
    assert_eq!(page.items.len(), 50, "list returns 50 sessions");
    let list_ms = started.elapsed().as_millis();
    println!("list_50_ms={list_ms}");

    let started = Instant::now();
    let resolved = store
        .resolve(&workspace, &big_id.to_string())
        .expect("name resolves");
    assert_eq!(resolved, big_id, "full id resolves");
    let resolve_ms = started.elapsed().as_millis();
    println!("resolve_ms={resolve_ms}");

    assert!(
        open_page_ms <= 900,
        "open plus first page stays under 3x of 300 ms"
    );
    assert!(list_ms <= 450, "list 50 stays under 3x of 150 ms");
    assert!(resolve_ms <= 450, "name resolve stays under 3x of 150 ms");
    let _ = fs::remove_dir_all(&root);
}

fn main() {
    startup_and_listing_benchmarks();
}
