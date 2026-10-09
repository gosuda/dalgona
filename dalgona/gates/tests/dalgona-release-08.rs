// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! This gate verifies a tag version mismatch fails the distribution plan.
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
fn tag_version_mismatch_fails_the_dist_plan() -> support::TestResult<()> {
    let workspace = support::repo_root().join("dalgona");
    let output = support::run_command(
        Command::new("dist")
            .args(["plan", "--tag=dalgona-v9.9.9"])
            .current_dir(&workspace),
    )?;
    assert!(!output.status.success());
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(
        diagnostic.contains("version"),
        "unexpected dist diagnostic: {diagnostic}"
    );
    Ok(())
}
