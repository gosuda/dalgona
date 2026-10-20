// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate tests.
#![expect(
    clippy::disallowed_methods,
    reason = "gate runs the real product binaries"
)]
use dalgona_gates::support;

use std::process::Command;

#[test]
fn cargo_binstall_fetches_release_without_source_build() -> support::TestResult<()> {
    if !support::released_artifacts_requested() {
        return Ok(());
    }
    let version = support::dalgona_version()?;
    support::assert_release_tag_matches_version(&version)?;
    let scratch = support::Scratch::new("dalgona-binstall")?;
    let cargo_home = scratch.path().join("cargo-home");
    std::fs::create_dir_all(&cargo_home)?;
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
