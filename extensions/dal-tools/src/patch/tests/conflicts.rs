//! Edits that cannot combine fail while planning, instead of one of them being
//! dropped or overwritten, and a link never stands in for a missing file.

use std::path::Path;

use super::super::{
    ir::{DialectId, EngineError, ErrorClass},
    write::plan,
};
use super::test_session;

async fn refused(dir: &Path, input: &str) -> EngineError {
    let session = test_session(dir, false);
    plan(&session, DialectId::Replace, input)
        .await
        .expect_err("these edits cannot combine")
}

#[tokio::test]
async fn delete_and_create_of_one_path_do_not_combine() {
    let dir = tempfile::tempdir().expect("temp workspace");
    std::fs::write(dir.path().join("a.txt"), b"keep\n").expect("seed");

    let error = refused(
        dir.path(),
        r#"{"changes":[{"path":"a.txt","delete":true},{"path":"a.txt","create":"new\n"}]}"#,
    )
    .await;

    assert_eq!(error.class, ErrorClass::Resolve);
    assert_eq!(
        error.message,
        "patch: changes[0] and changes[1] both change a.txt in ways that cannot combine."
    );
    assert_eq!(
        std::fs::read(dir.path().join("a.txt")).expect("read"),
        b"keep\n"
    );
}

#[tokio::test]
async fn an_edit_beside_a_create_is_refused_not_dropped() {
    let dir = tempfile::tempdir().expect("temp workspace");

    let error = refused(
        dir.path(),
        r#"{"changes":[{"path":"n.txt","create":"x\n"},{"path":"n.txt","old":"x","new":"y"}]}"#,
    )
    .await;

    assert_eq!(error.class, ErrorClass::Resolve);
    assert_eq!(
        error.message,
        "patch: changes[0] and changes[1] both change n.txt in ways that cannot combine."
    );
    assert!(!dir.path().join("n.txt").exists());
}

#[tokio::test]
async fn a_whole_file_replacement_beside_another_change_does_not_combine() {
    let dir = tempfile::tempdir().expect("temp workspace");
    std::fs::write(dir.path().join("a.txt"), b"alpha\nbeta\n").expect("seed");

    let error = refused(
        dir.path(),
        r#"{"changes":[{"path":"a.txt","tag":"DEADBEEF","new":"x\n"},{"path":"a.txt","old":"alpha","new":"A"}]}"#,
    )
    .await;

    assert_eq!(error.class, ErrorClass::Resolve);
    assert_eq!(
        error.message,
        "patch: changes[0] and changes[1] both change a.txt in ways that cannot combine."
    );
    assert_eq!(
        std::fs::read(dir.path().join("a.txt")).expect("read"),
        b"alpha\nbeta\n"
    );
}

#[tokio::test]
async fn two_renames_of_one_file_do_not_combine() {
    let dir = tempfile::tempdir().expect("temp workspace");
    std::fs::write(dir.path().join("a.txt"), b"alpha\n").expect("seed");

    let error = refused(
        dir.path(),
        r#"{"changes":[{"path":"a.txt","rename":"b.txt"},{"path":"a.txt","rename":"c.txt"}]}"#,
    )
    .await;

    assert_eq!(error.class, ErrorClass::Resolve);
    assert_eq!(
        error.message,
        "patch: changes[0] and changes[1] both change a.txt in ways that cannot combine."
    );
    assert!(dir.path().join("a.txt").exists());
}

#[tokio::test]
async fn a_rename_destination_another_change_creates_is_refused() {
    let dir = tempfile::tempdir().expect("temp workspace");
    std::fs::write(dir.path().join("a.txt"), b"alpha\n").expect("seed");

    let error = refused(
        dir.path(),
        r#"{"changes":[{"path":"a.txt","rename":"b.txt"},{"path":"b.txt","create":"fresh\n"}]}"#,
    )
    .await;

    assert_eq!(error.class, ErrorClass::File);
    assert_eq!(
        error.message,
        "patch: cannot rename a.txt to b.txt: another change in this call also writes b.txt."
    );
    assert_eq!(
        std::fs::read(dir.path().join("a.txt")).expect("read"),
        b"alpha\n"
    );
    assert!(!dir.path().join("b.txt").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_and_its_target_in_one_call_do_not_combine() {
    let dir = tempfile::tempdir().expect("temp workspace");
    std::fs::write(dir.path().join("real.txt"), b"alpha\nbeta\n").expect("seed");
    std::os::unix::fs::symlink(dir.path().join("real.txt"), dir.path().join("link.txt"))
        .expect("in-workspace link");

    let error = refused(
        dir.path(),
        r#"{"changes":[{"path":"real.txt","old":"alpha","new":"A"},{"path":"link.txt","old":"beta","new":"B"}]}"#,
    )
    .await;

    assert_eq!(error.class, ErrorClass::Resolve);
    assert_eq!(
        error.message,
        "patch: changes[0] and changes[1] both change link.txt in ways that cannot combine."
    );
    assert_eq!(
        std::fs::read(dir.path().join("real.txt")).expect("read"),
        b"alpha\nbeta\n"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_named_alone_still_writes_through_to_its_target() {
    use super::super::write::commit;

    let dir = tempfile::tempdir().expect("temp workspace");
    std::fs::write(dir.path().join("real.txt"), b"alpha\n").expect("seed");
    std::os::unix::fs::symlink(dir.path().join("real.txt"), dir.path().join("link.txt"))
        .expect("in-workspace link");
    let session = test_session(dir.path(), false);
    let staged = plan(
        &session,
        DialectId::Replace,
        r#"{"changes":[{"path":"link.txt","old":"alpha","new":"ALPHA"}]}"#,
    )
    .await
    .expect("a lone link plans");

    let output = commit(&session, staged, &[]).await;

    assert!(output.error_class.is_none(), "{}", output.text);
    assert_eq!(
        std::fs::read(dir.path().join("real.txt")).expect("read"),
        b"ALPHA\n"
    );
    let metadata = std::fs::symlink_metadata(dir.path().join("link.txt")).expect("link");
    assert!(metadata.file_type().is_symlink(), "the link itself is kept");
}

#[cfg(unix)]
#[tokio::test]
async fn creating_over_a_dangling_symlink_is_refused_and_keeps_the_link() {
    let dir = tempfile::tempdir().expect("temp workspace");
    std::os::unix::fs::symlink(dir.path().join("nowhere.txt"), dir.path().join("made.txt"))
        .expect("dangling link");

    let error = refused(
        dir.path(),
        r#"{"changes":[{"path":"made.txt","create":"x\n"}]}"#,
    )
    .await;

    assert_eq!(error.class, ErrorClass::File);
    assert!(
        error.message.contains("not a regular file"),
        "{}",
        error.message
    );
    let metadata = std::fs::symlink_metadata(dir.path().join("made.txt")).expect("link");
    assert!(metadata.file_type().is_symlink(), "the link is untouched");
    assert!(!dir.path().join("nowhere.txt").exists());
}
