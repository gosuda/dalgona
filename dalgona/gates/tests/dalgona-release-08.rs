// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate tests.
#![expect(
    clippy::disallowed_methods,
    reason = "gate runs the real product binaries"
)]
use gates::support;

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
