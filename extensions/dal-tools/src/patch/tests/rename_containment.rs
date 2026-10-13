//! Rename destinations obey the same workspace containment as every other
//! patch path, in every rename-capable dialect, at plan time and at commit
//! time.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::super::{
    ir::DialectId,
    write::{PatchSession, commit, plan},
};
use super::test_session;

const RENAME_DIALECTS: [DialectId; 6] = [
    DialectId::Replace,
    DialectId::ApplyPatch,
    DialectId::Hashline,
    DialectId::HashlineLight,
    DialectId::HashlineEnhanced,
    DialectId::Anchor,
];

struct Fixture {
    root: tempfile::TempDir,
    workspace: PathBuf,
    outside: PathBuf,
    session: PatchSession,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().expect("temp root");
    let workspace = root.path().join("ws");
    let outside = root.path().join("outside");
    std::fs::create_dir(&workspace).expect("workspace dir");
    std::fs::create_dir(&outside).expect("outside dir");
    std::fs::write(workspace.join("a.txt"), b"alpha\n").expect("seed source");
    std::fs::write(outside.join("keep.txt"), b"keep\n").expect("seed outside");
    let session = test_session(&workspace, false);
    Fixture {
        root,
        workspace,
        outside,
        session,
    }
}

fn outside_state(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        for entry in std::fs::read_dir(dir).expect("read dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            let relative = path.strip_prefix(root).expect("under root").to_path_buf();
            if relative == Path::new("ws") {
                continue;
            }
            let kind = entry.file_type().expect("file type");
            if kind.is_dir() {
                out.insert(relative, None);
                walk(&path, root, out);
            } else {
                let bytes = if kind.is_symlink() {
                    Vec::new()
                } else {
                    std::fs::read(&path).expect("read file")
                };
                out.insert(relative, Some(bytes));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn payload(fixture: &Fixture, dialect: DialectId, destination: &str) -> String {
    match dialect {
        DialectId::Replace => {
            let quoted = sonic_rs::to_string(destination).expect("quote destination");
            format!("{{\"changes\":[{{\"path\":\"a.txt\",\"rename\":{quoted}}}]}}")
        }
        DialectId::ApplyPatch => format!(
            "*** Begin Patch\n*** Update File: a.txt\n*** Move to: {destination}\n*** End Patch\n"
        ),
        DialectId::Hashline => format!("[a.txt#ABCD]\nMV {destination}\n"),
        DialectId::HashlineLight | DialectId::HashlineEnhanced => {
            let session = &fixture.session;
            let bytes = std::fs::read(fixture.workspace.join("a.txt")).expect("read source");
            let (reference, _) = session
                .snapshots
                .capture(
                    session.session,
                    session.generation,
                    session.consumer,
                    Path::new("a.txt"),
                    &bytes,
                )
                .expect("capture snapshot");
            format!("[a.txt@{}]\nMV {destination}\n", reference.display())
        }
        DialectId::Anchor => format!("*** Move: a.txt -> {destination}\n"),
    }
}

async fn assert_refused_everywhere(fixture: &Fixture, destination: &str) {
    let before = outside_state(fixture.root.path());
    for dialect in RENAME_DIALECTS {
        let input = payload(fixture, dialect, destination);
        let error = plan(&fixture.session, dialect, &input)
            .await
            .expect_err(&format!(
                "{dialect:?} must refuse destination {destination}"
            ));
        assert!(
            error.message.contains("outside the workspace"),
            "{dialect:?} {destination}: {}",
            error.message
        );
        assert_eq!(
            std::fs::read(fixture.workspace.join("a.txt")).expect("source intact"),
            b"alpha\n"
        );
        assert_eq!(
            outside_state(fixture.root.path()),
            before,
            "{dialect:?} {destination}: filesystem outside the workspace changed"
        );
    }
}

#[tokio::test]
async fn in_workspace_rename_plans_and_commits_in_every_dialect() {
    for dialect in RENAME_DIALECTS {
        let fixture = fixture();
        let input = payload(&fixture, dialect, "sub/b.txt");
        let staged = plan(&fixture.session, dialect, &input)
            .await
            .unwrap_or_else(|error| panic!("{dialect:?}: {}", error.message));
        let file = &staged.files[0];
        let dest = file.renamed_to.as_ref().expect("rename destination");
        assert_eq!(dest.path, Path::new("sub/b.txt"));
        assert_eq!(dest.absolute_path, fixture.workspace.join("sub/b.txt"));
        let output = commit(&fixture.session, staged, &[]).await;
        assert!(output.error_class.is_none(), "{dialect:?}: {}", output.text);
        assert_eq!(
            std::fs::read(fixture.workspace.join("sub/b.txt")).expect("destination"),
            b"alpha\n"
        );
        assert!(std::fs::metadata(fixture.workspace.join("a.txt")).is_err());
    }
}

#[tokio::test]
async fn absolute_rename_destination_is_refused() {
    let fixture = fixture();
    let target = fixture.outside.join("new.txt");
    assert_refused_everywhere(&fixture, &target.to_string_lossy()).await;
    let existing = fixture.outside.join("keep.txt");
    assert_refused_everywhere(&fixture, &existing.to_string_lossy()).await;
}

#[tokio::test]
async fn parent_walk_rename_destination_is_refused() {
    let fixture = fixture();
    assert_refused_everywhere(&fixture, "../outside/new.txt").await;
    assert_refused_everywhere(&fixture, "../escaped.txt").await;
    assert_refused_everywhere(&fixture, "sub/../../escaped.txt").await;
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_escape_rename_destination_is_refused() {
    let fixture = fixture();
    std::os::unix::fs::symlink(&fixture.outside, fixture.workspace.join("link"))
        .expect("directory link");
    std::os::unix::fs::symlink(
        fixture.outside.join("keep.txt"),
        fixture.workspace.join("file_link.txt"),
    )
    .expect("file link");
    assert_refused_everywhere(&fixture, "link/new.txt").await;
    assert_refused_everywhere(&fixture, "link/missing/deeper/new.txt").await;
    assert_refused_everywhere(&fixture, "file_link.txt").await;
}

#[cfg(unix)]
#[tokio::test]
async fn dangling_link_destination_never_writes_outside() {
    let fixture = fixture();
    std::os::unix::fs::symlink(
        fixture.outside.join("not_yet.txt"),
        fixture.workspace.join("dangling.txt"),
    )
    .expect("dangling link");
    let before = outside_state(fixture.root.path());
    let input = payload(&fixture, DialectId::Replace, "dangling.txt");
    if let Ok(staged) = plan(&fixture.session, DialectId::Replace, &input).await {
        let _ = commit(&fixture.session, staged, &[]).await;
    }
    assert_eq!(outside_state(fixture.root.path()), before);
}

#[cfg(unix)]
#[tokio::test]
async fn destination_directory_swapped_for_a_link_after_planning_is_refused_at_commit() {
    for existing_parent in [false, true] {
        let fixture = fixture();
        if existing_parent {
            std::fs::create_dir(fixture.workspace.join("sub")).expect("destination parent");
        }
        let input = payload(&fixture, DialectId::Replace, "sub/new.txt");
        let staged = plan(&fixture.session, DialectId::Replace, &input)
            .await
            .expect("destination is inside while planning");
        if existing_parent {
            std::fs::remove_dir(fixture.workspace.join("sub")).expect("remove parent");
        }
        std::os::unix::fs::symlink(&fixture.outside, fixture.workspace.join("sub"))
            .expect("swap in link");
        let before = outside_state(fixture.root.path());
        let output = commit(&fixture.session, staged, &[]).await;
        assert!(output.error_class.is_some(), "{}", output.text);
        assert!(
            output.text.contains("outside the workspace"),
            "{}",
            output.text
        );
        assert_eq!(outside_state(fixture.root.path()), before);
        assert_eq!(
            std::fs::read(fixture.workspace.join("a.txt")).expect("source intact"),
            b"alpha\n"
        );
    }
}
