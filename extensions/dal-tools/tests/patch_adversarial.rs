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
#![cfg_attr(
    unix,
    expect(clippy::expect_used, reason = "integration tests fail loudly")
)]

use std::path::Path;

use dal_tools::patch::resolve::resolve_path;

/// Creates a directory strictly outside `workspace`: a sibling under the same
/// temp parent. A link from the workspace into it truly escapes.
#[cfg(unix)]
fn outside_dir(workspace: &tempfile::TempDir) -> tempfile::TempDir {
    let parent = workspace
        .path()
        .parent()
        .expect("tempdir has a parent")
        .to_path_buf();
    tempfile::tempdir_in(parent).expect("outside sibling root")
}

#[cfg(unix)]
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

#[cfg(unix)]
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

#[cfg(unix)]
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
    // Steps from the workspace up to the filesystem root.
    let depth = workspace.path().ancestors().count() - 1;
    for steps in 1..=depth + 1 {
        for leaf in ["", "walked.txt"] {
            let escape = format!("{}{leaf}", "../".repeat(steps));
            let error = resolve_path(workspace.path(), Path::new(&escape)).expect_err(
                "a dot-dot walk out of the workspace must be denied, not clamped to its root",
            );
            assert!(
                error.message.contains("outside the workspace"),
                "{escape}: {}",
                error.message
            );
            assert!(
                error.message.contains(&escape),
                "{escape}: {}",
                error.message
            );
        }
    }
}

#[test]
fn dot_dot_staying_inside_the_workspace_is_normalized_and_one_step_further_is_denied() {
    let workspace = tempfile::tempdir().expect("workspace root");
    let root = std::fs::canonicalize(workspace.path()).expect("workspace canonicalizes");
    for raw in [
        "a/../inside.txt",
        "a/b/../../inside.txt",
        "./a/./../inside.txt",
        "a/../a/../inside.txt",
    ] {
        let (display, canonical) = resolve_path(workspace.path(), Path::new(raw))
            .expect("a walk that never leaves the workspace plans");
        assert_eq!(display, std::path::PathBuf::from("inside.txt"), "{raw}");
        assert_eq!(canonical, root.join("inside.txt"), "{raw}");
    }
    for raw in [
        "a/../../inside.txt",
        "a/b/../../../inside.txt",
        "../inside.txt",
        "a/..",
        ".",
    ] {
        let error = resolve_path(workspace.path(), Path::new(raw))
            .expect_err("a walk to or past the workspace root must be denied");
        assert!(
            error.message.contains("outside the workspace"),
            "{raw}: {}",
            error.message
        );
    }
}

#[cfg(unix)]
#[test]
fn dot_dot_into_a_sibling_directory_is_denied_not_remapped_inside() {
    let workspace = tempfile::tempdir().expect("workspace root");
    let outside = outside_dir(&workspace);
    std::fs::write(outside.path().join("secret.txt"), b"outside\n").expect("outside fixture");
    let sibling = outside
        .path()
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .expect("sibling name is UTF-8")
        .to_owned();
    let raw = format!("../{sibling}/secret.txt");
    let error = resolve_path(workspace.path(), Path::new(&raw))
        .expect_err("a sibling of the workspace must be denied");
    assert!(
        error.message.contains("outside the workspace"),
        "{raw}: {}",
        error.message
    );
}

#[cfg(unix)]
#[test]
fn dot_dot_after_a_symlinked_directory_is_resolved_lexically_inside() {
    let workspace = tempfile::tempdir().expect("workspace root");
    let outside = outside_dir(&workspace);
    std::os::unix::fs::symlink(outside.path(), workspace.path().join("hop"))
        .expect("escape symlink");
    let (display, canonical) = resolve_path(workspace.path(), Path::new("hop/../inside.txt"))
        .expect("the lexical walk returns to the workspace before any link is followed");
    assert_eq!(display, std::path::PathBuf::from("inside.txt"));
    assert_eq!(
        canonical,
        std::fs::canonicalize(workspace.path())
            .expect("workspace canonicalizes")
            .join("inside.txt"),
        "the walk must land in the workspace, not in the link target's parent"
    );
}

#[cfg(unix)]
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
    assert_eq!(
        canonical,
        std::fs::canonicalize(workspace.path())
            .expect("workspace canonicalizes")
            .join("new")
            .join("made.txt")
    );
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

#[cfg(unix)]
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

#[cfg(unix)]
#[test]
fn missing_directories_below_an_escaping_link_are_denied() {
    let workspace = tempfile::tempdir().expect("workspace root");
    let outside = outside_dir(&workspace);
    std::os::unix::fs::symlink(outside.path(), workspace.path().join("link")).expect("escape link");
    for raw in ["link/new.txt", "link/missing/new.txt", "link/a/b/c/new.txt"] {
        let error = resolve_path(workspace.path(), Path::new(raw))
            .expect_err("a link to an outside directory must not hide behind missing directories");
        assert!(error.message.contains("outside the workspace"), "{raw}");
    }
}
