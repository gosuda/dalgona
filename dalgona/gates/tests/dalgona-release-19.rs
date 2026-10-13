// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate tests.
#![expect(
    clippy::disallowed_methods,
    reason = "gate runs the real product binaries"
)]
use dalgona_gates::support;

use sonic_rs::{JsonContainerTrait, JsonValueTrait};
use std::{path::Path, process::Command};
#[test]
fn dal_dependencies_are_exact_and_local() -> support::TestResult<()> {
    let root = support::repo_root();
    let metadata = support::run_command(
        Command::new("cargo")
            .args(["metadata", "--locked", "--format-version", "1"])
            .current_dir(&root),
    )?;
    assert!(
        metadata.status.success(),
        "{}",
        String::from_utf8_lossy(&metadata.stderr)
    );
    let metadata: sonic_rs::Value = sonic_rs::from_slice(&metadata.stdout)?;
    let packages = metadata
        .get("packages")
        .and_then(sonic_rs::Value::as_array)
        .ok_or("cargo metadata omitted packages")?;
    for (name, path) in [
        ("dal-agent", "engine/dal-agent/Cargo.toml"),
        ("dal-core", "core/dal-core/Cargo.toml"),
        ("dal-provider", "engine/dal-provider/Cargo.toml"),
        ("dal-store", "engine/dal-store/Cargo.toml"),
        ("dal-ext", "extensions/dal-ext/Cargo.toml"),
        ("dal-star", "extensions/dal-star/Cargo.toml"),
        ("dal-tools", "extensions/dal-tools/Cargo.toml"),
        ("dal-tui", "clients/dal-tui/Cargo.toml"),
        ("dal-wire", "clients/dal-wire/Cargo.toml"),
        ("dalgon", "cli/dalgon/Cargo.toml"),
        ("dal-gates", "gates/Cargo.toml"),
    ] {
        let package = packages
            .iter()
            .find(|package| package.get("name").and_then(sonic_rs::Value::as_str) == Some(name))
            .ok_or_else(|| format!("metadata omitted local package {name}"))?;
        assert_eq!(
            package.get("version").and_then(sonic_rs::Value::as_str),
            Some("0.1.0"),
            "{name} version changed"
        );
        assert!(
            package.get("source").is_some_and(sonic_rs::Value::is_null),
            "{name} must resolve from the workspace, not crates.io"
        );
        let manifest = package
            .get("manifest_path")
            .and_then(sonic_rs::Value::as_str)
            .ok_or_else(|| format!("{name} omitted manifest_path"))?;
        assert_eq!(
            Path::new(manifest).canonicalize()?,
            root.join(path).canonicalize()?,
            "{name} resolved from an unexpected manifest"
        );
    }
    assert!(!root.join(".cargo/config.toml").exists());
    Ok(())
}
