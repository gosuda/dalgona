// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
#[path = "support/mod.rs"]
mod support;

use std::process::Command;

#[test]
fn dal_dependencies_are_exact_and_registry_sourced() -> support::TestResult<()> {
    let workspace = support::repo_root().join("dalgona");
    let metadata = support::run_command(
        Command::new("cargo")
            .args(["metadata", "--locked", "--format-version", "1"])
            .current_dir(&workspace),
    )?;
    assert!(
        metadata.status.success(),
        "{}",
        String::from_utf8_lossy(&metadata.stderr)
    );
    let metadata = String::from_utf8(metadata.stdout)?;
    for package in [
        "dal-agent",
        "dal-core",
        "dal-provider",
        "dal-star",
        "dal-tui",
        "dal-wire",
    ] {
        assert!(metadata.contains(&format!("\"name\":\"{package}\",\"version\":\"0.1.0\",\"source\":\"registry+https://github.com/rust-lang/crates.io-index\"")), "missing exact registry package {package}");
    }
    for override_marker in ["[patch.crates-io]", "path+file:"] {
        assert!(
            !metadata.contains(override_marker),
            "unexpected dependency override {override_marker}"
        );
    }
    assert!(!workspace.join(".cargo/config.toml").exists());
    Ok(())
}
