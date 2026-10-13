//! Release presence, lockstep, and path-dependency guards.
#[path = "release_support/mod.rs"]
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{error::Error, path::PathBuf};

#[test]
fn release_presence_gate_accepts_matching_version() -> Result<(), Box<dyn Error>> {
    let (code, stdout, stderr) =
        support::run_gate_dep("dal-journal", "0.1", "{\"vers\":\"0.1.0\"}\n")?;
    assert_eq!(code, 0);
    assert_eq!(stdout, "");
    assert_eq!(stderr, "");
    Ok(())
}

#[test]
fn release_presence_gate_rejects_missing_version() -> Result<(), Box<dyn Error>> {
    let (code, stdout, stderr) =
        support::run_gate_dep("dal-journal", "0.1", "{\"vers\":\"0.2.0\"}\n")?;
    assert_eq!(code, 3);
    assert_eq!(stdout, "");
    assert_eq!(
        stderr,
        "dalgon dependency dal-journal \"0.1\" not on crates.io; release dal first\n"
    );
    Ok(())
}

#[test]
fn release_presence_gate_matches_complete_exact_versions() -> Result<(), Box<dyn Error>> {
    for (requirement, version, expected) in [
        ("=1.2.3", "1.2.3", 0),
        ("=0.1.0-rc.1", "0.1.0-rc.1", 0),
        ("=1.2.3-rc.1+local", "1.2.3-rc.1+registry.01", 0),
        ("=1.2.3+local", "1.2.3", 0),
        ("=1.2.3", "1.2.3+registry", 0),
        ("=1.2.3-0.01a.-", "1.2.3-0.01a.-", 0),
        ("=1.2.3", "1.2.4", 3),
        ("=1.2.3", "1.3.3", 3),
        ("=1.2.3", "2.2.3", 3),
        ("=1.2.3-rc.1", "1.2.3-rc.2", 3),
        ("=1.2.3-rc.1", "1.2.3-RC.1", 3),
        ("=1.2.3-rc.1", "1.2.3", 3),
        ("=1.2.3", "1.2.3-rc.1", 3),
        ("=1.2.3-01", "1.2.3-01", 3),
        ("=1.2.3-rc..1", "1.2.3-rc..1", 3),
        ("=1.2.3", "1.2.3+bad..build", 3),
        ("=1.2.3+bad..build", "1.2.3", 3),
        ("=0.1", "0.1.0", 0),
        ("=0.1", "0.1.1", 3),
        ("0.1.2", "0.1.3", 0),
        ("0.1.2", "0.1.1", 3),
    ] {
        let index = format!("{{\"vers\":\"{version}\"}}\n");
        let (code, stdout, stderr) = support::run_gate_dep("dal-journal", requirement, &index)?;
        assert_eq!(code, expected, "{requirement} against {version}: {stderr}");
        assert!(stdout.is_empty(), "{stdout}");
        if expected == 0 {
            assert!(stderr.is_empty(), "{stderr}");
        } else {
            assert!(
                stderr.contains(requirement) && stderr.contains("not on crates.io"),
                "{stderr}"
            );
        }
    }
    Ok(())
}

#[test]
fn release_rejects_mixed_product_versions() -> Result<(), Box<dyn Error>> {
    let (code, stdout, stderr) = support::run_publish_script(
        &support::repo_root(),
        &[
            "--product",
            "dal",
            "--dry-run",
            "cli/dalgon/tests/fixtures/publish-order-lockstep",
        ],
    )?;
    assert_eq!(code, 2, "{stderr}");
    assert_eq!(stdout, "");
    Ok(())
}

#[test]
fn release_rejects_unowned_path_dependency() -> Result<(), Box<dyn Error>> {
    let (code, stdout, stderr) = support::run_publish_script(
        &support::repo_root(),
        &[
            "--product",
            "dal",
            "--check-registry-deps",
            "--dry-run",
            "cli/dalgon/tests/fixtures/publish-order-path-dep",
        ],
    )?;
    assert_eq!(code, 2, "{stderr}");
    assert_eq!(stdout, "");
    Ok(())
}

#[test]
fn release_registry_check_requires_complete_exact_cross_product_versions()
-> Result<(), Box<dyn Error>> {
    let workspace = tempfile::tempdir()?;
    std::fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nresolver = \"3\"\nmembers = [\"core/base\", \"dalgona/product\"]\n",
    )?;
    for (path, name) in [("core/base", "base"), ("dalgona/product", "product")] {
        let directory = workspace.path().join(path);
        std::fs::create_dir_all(directory.join("src"))?;
        std::fs::write(directory.join("src/lib.rs"), "")?;
        std::fs::write(
            directory.join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"),
        )?;
    }
    for requirement in [None, Some("0.1.0"), Some("=0.1"), Some("=0.1.0")] {
        let version =
            requirement.map_or_else(String::new, |value| format!(", version = \"{value}\""));
        std::fs::write(
            workspace.path().join("dalgona/product/Cargo.toml"),
            format!(
                "[package]\nname = \"product\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nbase = {{ path = \"../../core/base\"{version} }}\n"
            ),
        )?;
        let (code, stdout, stderr) = support::run_publish_script(
            workspace.path(),
            &[
                "--product",
                "dalgona",
                "--check-registry-deps",
                "--dry-run",
                ".",
            ],
        )?;
        if requirement == Some("=0.1.0") {
            assert_eq!(code, 0, "{stderr}");
            assert_eq!(stdout, "cargo publish -p product\n");
            assert!(stderr.is_empty(), "{stderr}");
        } else {
            assert_eq!(code, 2, "{requirement:?}: {stdout}{stderr}");
            assert!(stdout.is_empty(), "{stdout}");
            assert!(
                stderr.contains("product") && stderr.contains("base") && stderr.contains("exact"),
                "{stderr}"
            );
        }
    }
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
    use std::{fs, io};
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
    let output = support::shell()
        .arg(script)
        .arg(".")
        .current_dir(workspace.path())
        .env("PATH", path)
        .env("RELEASE_TEST_REAL_CARGO", real_cargo)
        .output()?;
    let stderr = String::from_utf8(output.stderr)?;
    assert_eq!(output.status.code(), Some(5));
    assert_eq!(String::from_utf8(output.stdout)?, "");
    assert!(stderr.contains("publish failed from fixture\n"));
    assert!(stderr.ends_with("cargo publish failed for tiny; see the output above\n"));
    Ok(())
}
