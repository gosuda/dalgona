//! Tests for the workspace index.

use std::collections::BTreeSet;

use super::build::build_dir;
use super::store::{EXTRA, FILES, FOOTER_LEN, HEADER_LEN, META, POSTING_LEN, POSTINGS, STAMPS};
use super::*;

fn token(text: &str) -> Vec<Vec<Vec<u8>>> {
    vec![vec![text.as_bytes().to_vec()]]
}

async fn candidates(index: &Index, ws: &Path, text: &str) -> Vec<PathBuf> {
    index
        .search_candidates(ws, &token(text), false, None)
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn freshness_after_patch() {
    let ws = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    fs::write(ws.path().join("a.rs"), "fn main() {}\n").unwrap();
    let index = Index::new(Some(root.path().to_path_buf()));
    assert_eq!(
        candidates(&index, ws.path(), "fn main").await,
        [Path::new("a.rs")]
    );
    assert_eq!(index.ready(ws.path()), Some((1, BuildKind::Full)));

    index.dirty(ws.path(), &ws.path().join("a.rs"));
    assert_eq!(
        index.ready(ws.path()),
        None,
        "the bump lands before dirty returns"
    );
    fs::write(ws.path().join("a.rs"), "fn wombat_alpha() {}\n").unwrap();
    assert_eq!(
        candidates(&index, ws.path(), "wombat_alpha").await,
        [Path::new("a.rs")]
    );
    assert_eq!(index.ready(ws.path()), Some((2, BuildKind::Full)));
    assert_eq!(
        candidates(&index, ws.path(), "fn main").await,
        [] as [std::path::PathBuf; 0]
    );
}

#[tokio::test]
async fn freshness_after_exec() {
    let ws = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    fs::write(ws.path().join("old.txt"), "nothing here\n").unwrap();
    let index = Index::new(Some(root.path().to_path_buf()));
    assert_eq!(
        candidates(&index, ws.path(), "kestrel_unique_77").await,
        [] as [std::path::PathBuf; 0]
    );

    // An exec writes behind the index's back; without the lever the
    // snapshot stays current for its epoch.
    fs::write(ws.path().join("new.txt"), "a kestrel_unique_77 token\n").unwrap();
    assert_eq!(
        candidates(&index, ws.path(), "kestrel_unique_77").await,
        [] as [std::path::PathBuf; 0]
    );

    index.exec_ran();
    assert_eq!(
        candidates(&index, ws.path(), "kestrel_unique_77").await,
        [Path::new("new.txt")]
    );
    assert_eq!(index.ready(ws.path()), Some((1, BuildKind::Incremental)));
    assert_eq!(
        candidates(&index, ws.path(), "nothing here").await,
        [Path::new("old.txt")],
        "unchanged files keep their remapped postings"
    );

    index.exec_ran();
    candidates(&index, ws.path(), "kestrel").await;
    assert_eq!(index.ready(ws.path()), Some((1, BuildKind::Unchanged)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn index_publish_race() {
    let ws = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut truth = Vec::new();
    for n in 0..200 {
        let name = format!("f{n:03}.txt");
        let body = if n % 2 == 0 {
            format!("line {n} zebra_quux here\n")
        } else {
            format!("line {n} plain filler\n")
        };
        fs::write(ws.path().join(&name), body).unwrap();
        if n % 2 == 0 {
            truth.push(PathBuf::from(name));
        }
    }
    let clauses = token("zebra_quux");
    let writer = Index::new(Some(root.path().to_path_buf()));
    let first = writer
        .search_candidates(ws.path(), &clauses, false, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first, truth);

    let rebuild = async {
        for _ in 0..8 {
            writer.dirty(ws.path(), &ws.path().join("f000.txt"));
            if let Ok(found) = writer
                .search_candidates(ws.path(), &clauses, false, None)
                .await
            {
                assert_eq!(found.unwrap(), truth);
            }
        }
    };
    let read = async {
        let mut served = 0;
        for _ in 0..100 {
            // A fresh hub per query opens the published files like a new process.
            let reader = Index::new(Some(root.path().to_path_buf()));
            match reader
                .search_candidates(ws.path(), &clauses, false, None)
                .await
            {
                Ok(found) => {
                    assert_eq!(found.unwrap(), truth, "no partial snapshot is served");
                    served += 1;
                }
                // Only a contended builder lock is a tolerated fallback;
                // a build error would mean a torn or corrupt read.
                Err(IndexError::Busy) => {}
                Err(error) => panic!("unexpected index error: {error}"),
            }
        }
        served
    };
    let ((), served) = tokio::join!(rebuild, read);
    assert!(served > 0);
}

#[tokio::test]
async fn masks_prune_and_case_folding() {
    let ws = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    fs::write(ws.path().join("split.txt"), "abcX____bcd").unwrap();
    fs::write(ws.path().join("whole.txt"), "xxabcdxx").unwrap();
    fs::write(ws.path().join("upper.txt"), "Wombat").unwrap();
    fs::create_dir(ws.path().join("sub")).unwrap();
    fs::write(ws.path().join("sub/abcd.txt"), "abcd").unwrap();
    let index = Index::new(Some(root.path().to_path_buf()));
    assert_eq!(
        candidates(&index, ws.path(), "abcd").await,
        [Path::new("sub/abcd.txt"), Path::new("whole.txt")]
    );
    let folded = index
        .search_candidates(ws.path(), &token("wOMBAT"), true, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(folded, [Path::new("upper.txt")]);
    let scoped = index
        .search_candidates(ws.path(), &token("abcd"), false, Some(Path::new("sub")))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(scoped, [Path::new("sub/abcd.txt")]);
    let either = vec![vec![b"Wombat".to_vec(), b"xxabcd".to_vec()]];
    let found = index
        .search_candidates(ws.path(), &either, false, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found, [Path::new("upper.txt"), Path::new("whole.txt")]);
    let short = index
        .search_candidates(ws.path(), &token("ab"), false, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        short.len(),
        4,
        "a short literal leaves its clause unconstrained"
    );
}

#[tokio::test]
async fn indexed_find_equals_walk() {
    let ws = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    for (name, body) in [
        ("src/a.rs", &b"fn a() {}"[..]),
        ("src/deep/b.rs", b"fn b() {}"),
        ("src/blob.rs", b"\0\0"),
        ("ignored/c.rs", b"x"),
        ("dir.rs/x.txt", b"x"),
        (".gitignore", b"ignored/\n"),
    ] {
        let path = ws.path().join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
    fs::create_dir(ws.path().join("empty.rs")).unwrap();
    let index = Index::new(Some(root.path().to_path_buf()));
    for (scope, pattern) in [(None, "*.rs"), (Some("src"), "*.rs"), (None, "src/**")] {
        let glob = FindGlob::new(pattern).unwrap();
        let indexed = index
            .find_entries(ws.path(), scope.map(Path::new), &glob, 1000)
            .await
            .unwrap()
            .unwrap();
        let base = scope.map_or(ws.path().to_path_buf(), |s| ws.path().join(s));
        let walked = find::find_with(&base, &glob, 1000).unwrap();
        assert_eq!(indexed, walked, "{scope:?} {pattern}");
    }
    let glob = FindGlob::new("*").unwrap();
    let file_scope = index
        .find_entries(ws.path(), Some(Path::new("src/a.rs")), &glob, 10)
        .await
        .unwrap();
    assert_eq!(file_scope, None);
}

#[tokio::test]
async fn invalid_version_reads_as_absent() {
    let ws = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    fs::write(ws.path().join("a.txt"), "gamma_delta").unwrap();
    let first = Index::new(Some(root.path().to_path_buf()));
    candidates(&first, ws.path(), "gamma_delta").await;
    let dir = first.dir_of(ws.path());

    let reopened = Index::new(Some(root.path().to_path_buf()));
    candidates(&reopened, ws.path(), "gamma_delta").await;
    assert_eq!(reopened.ready(ws.path()), Some((1, BuildKind::Opened)));

    let mut meta = fs::read(dir.join(META)).unwrap();
    meta[..4].copy_from_slice(&2_u32.to_le_bytes());
    fs::write(dir.join(META), meta).unwrap();
    let rebuilt = Index::new(Some(root.path().to_path_buf()));
    assert_eq!(
        candidates(&rebuilt, ws.path(), "gamma_delta").await,
        [Path::new("a.txt")]
    );
    assert_eq!(rebuilt.ready(ws.path()), Some((1, BuildKind::Full)));

    let mut postings = fs::read(dir.join(POSTINGS)).unwrap();
    postings.truncate(postings.len() - 1);
    fs::write(dir.join(POSTINGS), postings).unwrap();
    let truncated = Index::new(Some(root.path().to_path_buf()));
    candidates(&truncated, ws.path(), "gamma_delta").await;
    assert_eq!(truncated.ready(ws.path()), Some((1, BuildKind::Full)));
}

#[tokio::test]
async fn corrupt_posting_ids_force_the_fallback() {
    let ws = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    fs::write(ws.path().join("a.txt"), "gamma_delta").unwrap();
    let first = Index::new(Some(root.path().to_path_buf()));
    candidates(&first, ws.path(), "gamma_delta").await;
    let dir = first.dir_of(ws.path());

    let mut postings = fs::read(dir.join(POSTINGS)).unwrap();
    let footer = postings.len() - FOOTER_LEN;
    let count = usize::try_from(u64::from_le_bytes(
        postings[footer..footer + 8].try_into().unwrap(),
    ))
    .unwrap();
    for at in 0..count {
        let base = HEADER_LEN + at * POSTING_LEN;
        postings[base..base + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    }
    fs::write(dir.join(POSTINGS), postings).unwrap();

    let reopened = Index::new(Some(root.path().to_path_buf()));
    let found = reopened
        .search_candidates(ws.path(), &token("gamma_delta"), false, None)
        .await;
    assert!(
        matches!(found, Err(IndexError::Build(_))),
        "a posting id outside the path table must fail validation, not prove absence: {found:?}"
    );
}

/// Re-encode a files-table section with its first stored path replaced by
/// `poison`, keeping the header, count, and other entries valid so that
/// only the path itself can fail validation.
fn poison_first_file_path(section: &[u8], poison: &[u8]) -> Vec<u8> {
    let mut out = section[..HEADER_LEN].to_vec();
    let mut rest = &section[HEADER_LEN..];
    let count = u64::from_le_bytes(rest[..8].try_into().unwrap());
    out.extend_from_slice(&count.to_le_bytes());
    rest = &rest[8..];
    for n in 0..count {
        let len = usize::try_from(u64::from_le_bytes(rest[..8].try_into().unwrap())).unwrap();
        rest = &rest[8..];
        let bytes = &rest[..len];
        rest = &rest[len..];
        let replacement = if n == 0 { poison } else { bytes };
        out.extend_from_slice(&(replacement.len() as u64).to_le_bytes());
        out.extend_from_slice(replacement);
    }
    assert!(
        rest.is_empty(),
        "the files-table format changed; update the poison helper"
    );
    out
}

/// Re-encode an extra-table section with its first stored path replaced by
/// `poison`, keeping the header, count, kinds, and other entries valid so
/// that only the path itself can fail validation.
fn poison_first_extra_path(section: &[u8], poison: &[u8]) -> Vec<u8> {
    let mut out = section[..HEADER_LEN].to_vec();
    let mut rest = &section[HEADER_LEN..];
    let count = u64::from_le_bytes(rest[..8].try_into().unwrap());
    out.extend_from_slice(&count.to_le_bytes());
    rest = &rest[8..];
    for n in 0..count {
        out.push(rest[0]);
        rest = &rest[1..];
        let len = usize::try_from(u64::from_le_bytes(rest[..8].try_into().unwrap())).unwrap();
        rest = &rest[8..];
        let bytes = &rest[..len];
        rest = &rest[len..];
        let replacement = if n == 0 { poison } else { bytes };
        out.extend_from_slice(&(replacement.len() as u64).to_le_bytes());
        out.extend_from_slice(replacement);
    }
    assert!(
        rest.is_empty(),
        "the extra-table format changed; update the poison helper"
    );
    out
}

#[test]
fn escaping_stored_paths_read_as_absent() {
    let ws = tempfile::tempdir().unwrap();
    fs::write(ws.path().join("a.txt"), "gamma_delta").unwrap();
    fs::create_dir(ws.path().join("sub")).unwrap();
    fs::write(ws.path().join("sub/b.txt"), "gamma_delta").unwrap();
    let canonical = fs::canonicalize(ws.path()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    build_dir(
        dir.path(),
        &canonical,
        None,
        find::walk_listing(&canonical).unwrap(),
        ARENA_BYTES,
    )
    .unwrap();
    assert!(
        super::store::open(dir.path(), &canonical).is_some(),
        "the honest index opens"
    );

    let files = fs::read(dir.path().join(FILES)).unwrap();
    let extra = fs::read(dir.path().join(EXTRA)).unwrap();
    for (name, poisoned) in [
        (FILES, poison_first_file_path(&files, b"../outside")),
        (
            FILES,
            poison_first_file_path(&files, b"/tmp/poisoned-absolute.txt"),
        ),
        (EXTRA, poison_first_extra_path(&extra, b"../outside-dir")),
    ] {
        fs::write(dir.path().join(name), &poisoned).unwrap();
        assert!(
            super::store::open(dir.path(), &canonical).is_none(),
            "a poisoned stored path in {name} is corruption"
        );
        let honest = if name == FILES { &files } else { &extra };
        fs::write(dir.path().join(name), honest).unwrap();
    }
}

#[tokio::test]
async fn poisoned_index_rebuilds_without_leaving_the_root() {
    let parent = tempfile::tempdir().unwrap();
    let ws = parent.path().join("ws");
    fs::create_dir(&ws).unwrap();
    fs::write(ws.join("a.txt"), "gamma_delta inside\n").unwrap();
    fs::write(parent.path().join("outside.txt"), "outside_secret_token\n").unwrap();
    let root = tempfile::tempdir().unwrap();
    let first = Index::new(Some(root.path().to_path_buf()));
    assert_eq!(
        candidates(&first, &ws, "gamma_delta").await,
        [Path::new("a.txt")]
    );
    let dir = first.dir_of(&ws);

    // A crafted index points at a file outside the root; every other byte
    // stays valid, so only path validation can reject it.
    let files = fs::read(dir.join(FILES)).unwrap();
    fs::write(
        dir.join(FILES),
        poison_first_file_path(&files, b"../outside.txt"),
    )
    .unwrap();

    let reopened = Index::new(Some(root.path().to_path_buf()));
    assert_eq!(
        candidates(&reopened, &ws, "gamma_delta").await,
        [Path::new("a.txt")]
    );
    assert_eq!(
        reopened.ready(&ws),
        Some((1, BuildKind::Full)),
        "a poisoned index is corrupt and rebuilds; it never opens"
    );
    assert!(
        candidates(&reopened, &ws, "outside_secret_token")
            .await
            .is_empty(),
        "the escape target outside the root stays unread"
    );
}

#[test]
fn spill_merge_matches_memory_and_cleans_up() {
    let ws = tempfile::tempdir().unwrap();
    for n in 0..40 {
        fs::write(
            ws.path().join(format!("f{n}.txt")),
            format!("Body {n} of the Spill test"),
        )
        .unwrap();
    }
    let canonical = fs::canonicalize(ws.path()).unwrap();
    let small = tempfile::tempdir().unwrap();
    let large = tempfile::tempdir().unwrap();
    let spilled = build_dir(
        small.path(),
        &canonical,
        None,
        find::walk_listing(&canonical).unwrap(),
        16,
    )
    .unwrap();
    let memory = build_dir(
        large.path(),
        &canonical,
        None,
        find::walk_listing(&canonical).unwrap(),
        ARENA_BYTES,
    )
    .unwrap();
    assert_eq!(
        spilled.postings.bytes[HEADER_LEN..],
        memory.postings.bytes[HEADER_LEN..]
    );
    let leftovers: BTreeSet<String> = fs::read_dir(small.path())
        .unwrap()
        .map(|item| item.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        leftovers,
        [EXTRA, FILES, META, POSTINGS, STAMPS]
            .map(str::to_owned)
            .into_iter()
            .collect()
    );

    fs::write(small.path().join("build-1-1.spill"), b"dead").unwrap();
    let held = lock_dir(small.path()).unwrap();
    assert_eq!(lock_dir(small.path()).unwrap_err(), IndexError::Busy);
    prune(small.path());
    drop(held);
    assert!(!small.path().join("build-1-1.spill").exists());
}

#[tokio::test]
async fn no_root_means_no_index() {
    let ws = tempfile::tempdir().unwrap();
    let index = Index::new(None);
    index.dirty(ws.path(), &ws.path().join("a"));
    index.exec_ran();
    let found = index
        .search_candidates(ws.path(), &token("abc"), false, None)
        .await;
    assert_eq!(found, Ok(None));

    // The freshness barrier still works with no index root.
    let captured = index.freshness(ws.path());
    assert!(index.is_current(ws.path(), &captured));
    index.dirty(ws.path(), &ws.path().join("a"));
    assert!(!index.is_current(ws.path(), &captured));
    let fresh = index.freshness(ws.path());
    index.exec_ran();
    assert!(!index.is_current(ws.path(), &fresh));
    assert!(index.is_current(ws.path(), &index.freshness(ws.path())));
}

fn peak_rss_kib() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmHWM:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "reference-runner timing; run on the nightly idle-machine lane"]
async fn build_budgets() {
    let ws = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    for n in 0..30_000 {
        let dir = ws.path().join(format!("d{:03}", n % 300));
        fs::create_dir_all(&dir).unwrap();
        let mut body = String::with_capacity(4096);
        while body.len() < 4000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let _ = std::fmt::Write::write_fmt(
                &mut body,
                format_args!(
                    "let ident_{:x} = call_{}();\n",
                    state & 0x00FF_FFFF,
                    state % 997
                ),
            );
        }
        fs::write(dir.join(format!("f{n}.rs")), body).unwrap();
    }
    let index = Index::new(Some(root.path().to_path_buf()));
    let started = std::time::Instant::now();
    candidates(&index, ws.path(), "call_1").await;
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_secs(4), "build took {elapsed:?}");
    if let Some(peak) = peak_rss_kib() {
        assert!(peak <= 256 * 1024, "peak build memory {peak} KiB");
    }
}
