// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate tests.
#![expect(
    clippy::disallowed_methods,
    reason = "gate runs the real product binaries"
)]
use dalgona_gates::support;
use sonic_rs::{JsonContainerTrait, JsonValueTrait};

use std::process::Command;

#[test]
fn cargo_binstall_fetches_release_without_source_build() -> support::TestResult<()> {
    let scratch = support::Scratch::new("dalgona-binstall")?;
    let cargo_home = scratch.path().join("cargo-home");
    std::fs::create_dir_all(&cargo_home)?;
    let metadata_output = support::run_command(
        Command::new("cargo")
            .args(["metadata", "--no-deps", "--format-version=1", "--locked"])
            .current_dir(support::repo_root()),
    )?;
    assert!(
        metadata_output.status.success(),
        "{}",
        String::from_utf8_lossy(&metadata_output.stderr)
    );
    let metadata: sonic_rs::Value = sonic_rs::from_slice(&metadata_output.stdout)?;
    let version = metadata
        .get("packages")
        .and_then(sonic_rs::Value::as_array)
        .and_then(|packages| {
            packages.iter().find(|package| {
                package.get("name").and_then(sonic_rs::Value::as_str) == Some("dalgona")
            })
        })
        .and_then(|package| package.get("version"))
        .and_then(sonic_rs::Value::as_str)
        .ok_or("Cargo metadata omitted the dalgona version")?;
    let output = support::run_command(
        Command::new("cargo")
            .args([
                "binstall",
                "--no-confirm",
                "--strategies",
                "crate-meta-data",
                &format!("dalgona@{version}"),
            ])
            .env("CARGO_HOME", &cargo_home),
    )?;
    assert!(
        output.status.success(),
        "binstall failed ({}):\n{}{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for binary in ["dalgona", "dg"] {
        let installed = cargo_home
            .join("bin")
            .join(format!("{binary}{}", std::env::consts::EXE_SUFFIX));
        let version_output = support::run_command(Command::new(installed).arg("--version"))?;
        assert!(
            version_output.status.success(),
            "{}",
            String::from_utf8_lossy(&version_output.stderr)
        );
        assert_eq!(
            String::from_utf8(version_output.stdout)?.trim(),
            format!("dalgona {version}")
        );
    }
    Ok(())
}
