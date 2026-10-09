// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
#[path = "support/mod.rs"]
use gates::support::*;

use std::process::Command;

#[test]
fn binstall_templates_expand_to_dist_artifacts() -> support::TestResult<()> {
    let workspace = support::repo_root().join("dalgona");
    let output = support::run_command(Command::new("dist").arg("plan").current_dir(&workspace))?;
    assert!(
        output.status.success(),
        "{}",
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
        let archive = format!("dalgona-{target}");
        assert!(plan.contains(&archive), "dist plan omits {archive}");
    }
    assert!(plan.contains("dalgona-v0.1.0"));
    assert!(!plan.contains("releases/latest/download"));
    Ok(())
}
