//! Patch-tool workspace containment: symlink escapes, dot-dot walks, and
//! rename destinations.
//!
//! Containment decisions live in `patch::resolve`; every probe here denies a
//! path that canonicalizes outside the workspace and pins the one allowed
//! in-workspace symlink shape.
//!
//! Fixture rules: an "outside" directory is always a sibling tempdir of the
//! workspace (never a child), and every fixture fails loud at its setup
//! boundary.
#![expect(clippy::expect_used, reason = "integration tests fail loudly")]

use std::path::Path;

use dal_tools::patch::resolve::resolve_path;

/// Creates a directory strictly outside `workspace`: a sibling under the same
/// temp parent. A link from the workspace into it truly escapes.
fn outside_dir(workspace: &tempfile::TempDir) -> tempfile::TempDir {
    let parent = workspace
        .path()
        .parent()
        .expect("tempdir has a parent")
        .to_path_buf();
    tempfile::tempdir_in(parent).expect("outside sibling root")
}

#[test]
fn symlinked_source_path_resolving_outside_the_workspace_is_denied() {
    let workspace = tempfile::tempdir().expect("workspace root");
    let outside = outside_dir(&workspace);
    std::fs::write(outside.path().join("secret.txt"), b"outside\n").expect("outside fixture");
    std::fs::create_dir_all(workspace.path().join("src")).expect("src dir");
    std::os::unix::fs::symlink(outside.path(), workspace.path().join("src").join("escape"))
        .expect("escape symlink");
    let error = resolve_path(workspace.path(), Path::new("src/escape/secret.txt"))
        .expect_err("a path through a symlinked directory to outside must be denied");
    assert!(
        error.message.contains("outside the workspace"),
        "containment wording, got: {}",
        error.message
    );
    assert_eq!(
        std::fs::read(outside.path().join("secret.txt")).expect("outside bytes"),
        b"outside\n",
        "the probe only resolves; the outside file must be untouched"
    );
}

#[test]
fn workspace_root_symlink_alias_for_the_target_is_denied() {
    let workspace = tempfile::tempdir().expect("workspace root");
    let outside = outside_dir(&workspace);
    std::fs::write(outside.path().join("target.txt"), b"old\n").expect("outside fixture");
    std::os::unix::fs::symlink(outside.path(), workspace.path().join("alias"))
        .expect("alias symlink");
    let error = resolve_path(workspace.path(), Path::new("alias/target.txt"))
        .expect_err("an alias resolving to outside the workspace must be denied");
    assert!(
        error.message.contains("outside the workspace"),
        "got: {}",
        error.message
    );
    assert_eq!(
        std::fs::read(outside.path().join("target.txt")).expect("outside bytes"),
        b"old\n"
    );
}

#[test]
fn parent_link_escape_after_canonicalization_is_denied() {
    let workspace = tempfile::tempdir().expect("workspace root");
    let outside = outside_dir(&workspace);
    std::fs::write(outside.path().join("secret.txt"), b"top\n").expect("sibling fixture");
    std::fs::create_dir_all(workspace.path().join("a/b")).expect("nested dirs");
    std::os::unix::fs::symlink(outside.path(), workspace.path().join("a").join("up"))
        .expect("sibling link");
    let error = resolve_path(workspace.path(), Path::new("a/up/secret.txt"))
        .expect_err("a path through a parent-link to outside must be denied");
    assert!(
        error.message.contains("outside the workspace"),
        "got: {}",
        error.message
    );
    assert_eq!(
        std::fs::read(outside.path().join("secret.txt")).expect("outside bytes"),
        b"top\n"
    );
}

#[test]
fn bare_dot_dot_walk_past_the_root_is_denied() {
    let workspace = tempfile::tempdir().expect("workspace root");
    let (root, depth) = {
        let mut path = workspace.path();
        let mut depth = 0_usize;
        loop {
            let Some(parent) = path.parent() else {
                break (path.to_path_buf(), depth);
            };
            depth += 1;
            path = parent;
            if path.file_name().is_none() {
                break (path.to_path_buf(), depth);
            }
        }
    };
    let escape = "../".repeat(depth + 1);
    let error = resolve_path(workspace.path(), Path::new(&escape))
        .expect_err("a dot-dot walk past the filesystem root must be denied");
    assert!(
        error.message.contains("outside the workspace"),
        "got: {}",
        error.message
    );
    assert!(root.exists());
}

#[test]
fn in_workspace_symlink_resolves_to_its_target() {
    let workspace = tempfile::tempdir().expect("workspace root");
    std::fs::write(workspace.path().join("real.txt"), b"alpha\n").expect("target fixture");
    std::os::unix::fs::symlink(
        workspace.path().join("real.txt"),
        workspace.path().join("link.txt"),
    )
    .expect("in-workspace link");
    let (display, canonical) =
        resolve_path(workspace.path(), Path::new("link.txt")).expect("inside stays inside");
    assert_eq!(display, std::path::PathBuf::from("link.txt"));
    assert_eq!(
        canonical,
        std::fs::canonicalize(workspace.path().join("real.txt")).expect("canonical target"),
        "the link resolves to its target, not the link path"
    );
}

#[test]
fn missing_target_under_an_inside_parent_is_accepted_for_creation() {
    let workspace = tempfile::tempdir().expect("workspace root");
    std::fs::create_dir_all(workspace.path().join("new")).expect("inside parent");
    let (display, canonical) = resolve_path(workspace.path(), Path::new("new/made.txt"))
        .expect("a not-yet-existing file under an inside parent plans");
    assert_eq!(display, std::path::PathBuf::from("new/made.txt"));
    assert_eq!(canonical, workspace.path().join("new").join("made.txt"));
}

#[test]
fn absolute_path_is_denied() {
    let workspace = tempfile::tempdir().expect("workspace root");
    let error = resolve_path(workspace.path(), Path::new("/etc/passwd"))
        .expect_err("an absolute patch path must be denied");
    assert!(
        error.message.contains("outside the workspace"),
        "got: {}",
        error.message
    );
}

#[test]
fn empty_path_is_denied() {
    let workspace = tempfile::tempdir().expect("workspace root");
    let error = resolve_path(workspace.path(), Path::new(""))
        .expect_err("an empty patch path must be denied");
    assert!(error.message.contains("must not be empty"));
}

#[test]
fn resolve_path_rejects_an_outside_alias_through_a_created_link() {
    let workspace = tempfile::tempdir().expect("workspace root");
    let outside = outside_dir(&workspace);
    std::fs::create_dir_all(workspace.path().join("deep/inner")).expect("deep dirs");
    std::os::unix::fs::symlink(
        workspace.path().join("deep/inner"),
        workspace.path().join("loop"),
    )
    .expect("inside alias");
    let inside =
        resolve_path(workspace.path(), Path::new("loop")).expect("the alias itself is inside");
    assert_eq!(
        inside.1,
        std::fs::canonicalize(workspace.path().join("deep/inner")).expect("inner canonical")
    );
    std::os::unix::fs::symlink(outside.path(), workspace.path().join("deep/inner/out"))
        .expect("escape link under the alias target");
    let error = resolve_path(workspace.path(), Path::new("loop/out/file"))
        .expect_err("the alias chain must not hide the escape");
    assert!(error.message.contains("outside the workspace"));
}
