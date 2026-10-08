//! Release presence, lockstep, and path-dependency guards.
#[path = "release_support/mod.rs"]
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{error::Error, path::PathBuf};

fn dal_root() -> PathBuf {
    support::repo_root().join("dal")
}

#[test]
fn release_presence_gate_accepts_matching_version() -> Result<(), Box<dyn Error>> {
    let (code, stdout, stderr) =
        support::run_gate_dep("dal-journal", "0.1", "{\"vers\":\"0.1.0\"}\n")?;
    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    Ok(())
}

#[test]
fn release_presence_gate_rejects_missing_version() -> Result<(), Box<dyn Error>> {
    let (code, stdout, stderr) =
        support::run_gate_dep("dal-journal", "0.1", "{\"vers\":\"0.2.0\"}\n")?;
    assert_eq!(code, 3);
    assert!(stdout.is_empty());
    assert_eq!(
        stderr,
        "dalgon dependency dal-journal \"0.1\" not on crates.io; release dalgon first\n"
    );
    Ok(())
}

#[test]
fn release_lockstep_guard_names_the_member() -> Result<(), Box<dyn Error>> {
    let (code, stdout, stderr) = support::run_publish_script(
        &dal_root(),
        &[
            "--dry-run",
            "crates/dalgon/tests/fixtures/publish-order-lockstep",
        ],
    )?;
    assert_eq!(code, 2);
    assert!(stdout.is_empty());
    assert_eq!(
        stderr,
        "workspace member top does not inherit the workspace version; lockstep is broken\n"
    );
    Ok(())
}

#[test]
fn release_path_dependency_guard_names_the_edge() -> Result<(), Box<dyn Error>> {
    let (code, stdout, stderr) = support::run_publish_script(
        &dal_root(),
        &[
            "--check-registry-deps",
            "--dry-run",
            "crates/dalgon/tests/fixtures/publish-order-path-dep",
        ],
    )?;
    assert_eq!(code, 2);
    assert!(stdout.is_empty());
    assert_eq!(
        stderr,
        "workspace member extra depends on fake-dal by path; dalgona builds only against published dal-* crates\n"
    );
    Ok(())
}

#[cfg(unix)]
fn executable_in_path(path: &std::ffi::OsStr, name: &str) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

#[cfg(unix)]
fn write_executable(path: &std::path::Path, text: &str) -> Result<(), Box<dyn Error>> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, text)?;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn release_publish_failure_wraps_cargo_error() -> Result<(), Box<dyn Error>> {
    use std::{fs, io, process::Command};
    let workspace = tempfile::tempdir()?;
    fs::create_dir_all(workspace.path().join("tiny/src"))?;
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nresolver = \"3\"\nmembers = [\"tiny\"]\n\n[workspace.package]\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )?;
    fs::write(
        workspace.path().join("tiny/Cargo.toml"),
        "[package]\nname = \"tiny\"\nversion.workspace = true\nedition.workspace = true\n",
    )?;
    fs::write(workspace.path().join("tiny/src/lib.rs"), "")?;
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard test needs the ambient PATH to locate the real cargo"
    )]
    let real_path = std::env::var_os("PATH").ok_or_else(|| io::Error::other("PATH is not set"))?;
    let real_cargo = executable_in_path(&real_path, "cargo")
        .ok_or_else(|| io::Error::other("cargo is not on PATH"))?;
    let scratch = tempfile::tempdir()?;
    let fake_cargo = scratch.path().join("cargo");
    write_executable(
        &fake_cargo,
        "#!/bin/sh\nif [ \"$1\" = metadata ]; then exec \"$RELEASE_TEST_REAL_CARGO\" \"$@\"; fi\nprintf '%s\\n' 'publish failed from fixture' >&2\nexit 23\n",
    )?;
    let path = std::env::join_paths(
        std::iter::once(scratch.path().to_path_buf()).chain(std::env::split_paths(&real_path)),
    )?;
    let script = support::repo_root().join("scripts/publish-crates.sh");
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard test runs the repo's own publish script through bash"
    )]
    let output = Command::new("bash")
        .arg(script)
        .arg(".")
        .current_dir(workspace.path())
        .env("PATH", path)
        .env("RELEASE_TEST_REAL_CARGO", real_cargo)
        .output()?;
    let stderr = String::from_utf8(output.stderr)?;
    assert_eq!(output.status.code(), Some(5));
    assert!(String::from_utf8(output.stdout)?.is_empty());
    assert!(stderr.contains("publish failed from fixture\n"));
    assert!(stderr.ends_with("cargo publish failed for tiny; see the output above\n"));
    Ok(())
}
