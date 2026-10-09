// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! This gate verifies diamond publish order is stable and topological.
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
fn publish_order_diamond_is_topological_and_stable() -> support::TestResult<()> {
    let root = support::repo_root();
    let fixture = root.join("dalgona/gates/tests/fixtures/release/publish-order");
    let output = support::run_command(
        Command::new("bash")
            .arg(root.join("scripts/publish-crates.sh"))
            .args(["--dry-run"])
            .arg(fixture)
            .current_dir(&root),
    )?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout)?,
        "cargo publish -p base\ncargo publish -p left\ncargo publish -p right\ncargo publish -p top\n"
    );
    Ok(())
}
