// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate tests.
#![expect(
    clippy::disallowed_methods,
    reason = "gate runs the real product binaries"
)]

use dalgona_gates::support;
use std::process::Command;

#[test]
fn path_dependency_to_unpublished_dal_is_rejected() -> support::TestResult<()> {
    let root = support::repo_root();
    let fixture = root.join("dalgona/gates/tests/fixtures/release/publish-order-path-dep");
    let output = support::run_command(
        Command::new("bash")
            .arg(root.join("scripts/publish-crates.sh"))
            .args(["--check-registry-deps", "--dry-run"])
            .arg(fixture)
            .current_dir(&root),
    )?;
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(String::from_utf8(output.stdout)?, "");
    let diagnostic = String::from_utf8(output.stderr)?;
    assert!(diagnostic.contains("extra") && diagnostic.contains("fake-dal"));
    assert!(diagnostic.contains("not a workspace member"));
    Ok(())
}
