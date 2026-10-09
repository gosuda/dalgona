// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! This gate verifies Dalgona and dg report identical versions.
#![expect(
    clippy::disallowed_methods,
    reason = "release gate drives real release commands"
)]
#[path = "support/mod.rs"]
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::process::Command;

fn version(binary: &str) -> support::TestResult<String> {
    let workspace = support::repo_root().join("dalgona");
    let output = support::run_command(
        Command::new("cargo")
            .args([
                "run",
                "--locked",
                "-p",
                "dalgona",
                "--bin",
                binary,
                "--",
                "--version",
            ])
            .current_dir(&workspace),
    )?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}

#[test]
fn dalgona_and_dg_versions_are_identical() -> support::TestResult<()> {
    assert_eq!(version("dalgona")?, "dalgona 0.1.0\n");
    assert_eq!(version("dg")?, "dalgona 0.1.0\n");
    Ok(())
}
