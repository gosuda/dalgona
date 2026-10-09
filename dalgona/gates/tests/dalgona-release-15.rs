// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! This gate verifies an empty publishable workspace returns exit four.
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
fn empty_publishable_workspace_returns_exit_four() -> support::TestResult<()> {
    let root = support::repo_root();
    let fixture = root.join("dalgona/gates/tests/fixtures/release/publish-order-empty");
    let output = support::run_command(
        Command::new("bash")
            .arg(root.join("scripts/publish-crates.sh"))
            .args(["--dry-run"])
            .arg(&fixture)
            .current_dir(&root),
    )?;
    assert_eq!(output.status.code(), Some(4));
    assert_eq!(String::from_utf8(output.stdout)?, "");
    assert_eq!(
        String::from_utf8(output.stderr)?,
        format!("no publishable member found in {}\n", fixture.display())
    );
    Ok(())
}
