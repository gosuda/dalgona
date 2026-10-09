// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! This gate verifies the Dalgona binary package publishes last.
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

#[test]
fn dalgona_binary_package_publishes_last() -> support::TestResult<()> {
    let root = support::repo_root();
    let output = support::run_command(
        Command::new("bash")
            .arg(root.join("scripts/publish-crates.sh"))
            .args(["--dry-run"])
            .arg(root.join("dalgona"))
            .current_dir(&root),
    )?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let packages = String::from_utf8(output.stdout)?;
    let lines: Vec<_> = packages.lines().collect();
    assert_eq!(lines.last(), Some(&"cargo publish -p dalgona"));
    assert!(lines.contains(&"cargo publish -p dalgona-batteries"));
    assert_eq!(lines.len(), 2);
    Ok(())
}
