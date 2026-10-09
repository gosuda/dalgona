// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
#[path = "support/mod.rs"]
use gates::support::*;

use std::process::Command;

#[test]
fn dalgona_dist_plan_lists_the_six_target_archives() -> support::TestResult<()> {
    let root = support::repo_root().join("dalgona");
    let output = support::run_command(Command::new("dist").arg("plan").current_dir(&root))?;
    assert!(
        output.status.success(),
        "dist plan failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let plan = String::from_utf8(output.stdout)?;
    for target in [
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "x86_64-apple-darwin",
        "aarch64-apple-darwin",
        "x86_64-pc-windows-msvc",
        "aarch64-pc-windows-msvc",
    ] {
        assert!(plan.contains(target), "dist plan omits {target}");
    }
    for artifact in [
        "dalgona-installer.sh",
        "dalgona-installer.ps1",
        "sha256",
        "CHANGELOG.md",
    ] {
        assert!(plan.contains(artifact), "dist plan omits {artifact}");
    }
    Ok(())
}
