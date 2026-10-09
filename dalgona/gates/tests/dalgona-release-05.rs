// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! This gate verifies lockstep errors name non-inheriting members.
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
fn lockstep_guard_names_noninheriting_member() -> support::TestResult<()> {
    let root = support::repo_root();
    let fixture = root.join("dalgona/gates/tests/fixtures/release/publish-order-lockstep");
    let output = support::run_command(
        Command::new("bash")
            .arg(root.join("scripts/publish-crates.sh"))
            .args(["--dry-run"])
            .arg(fixture)
            .current_dir(&root),
    )?;
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(String::from_utf8(output.stdout)?, "");
    assert_eq!(
        String::from_utf8(output.stderr)?,
        "workspace member top does not inherit the workspace version; lockstep is broken\n"
    );
    Ok(())
}

#[test]
fn lockstep_guard_ignores_assignments_inside_multiline_strings() -> support::TestResult<()> {
    let root = support::repo_root();
    let fixture =
        root.join("dalgona/gates/tests/fixtures/release/publish-order-lockstep-multiline");
    let output = support::run_command(
        Command::new("bash")
            .arg(root.join("scripts/publish-crates.sh"))
            .args(["--dry-run"])
            .arg(fixture)
            .current_dir(&root),
    )?;
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(String::from_utf8(output.stdout)?, "");
    assert_eq!(
        String::from_utf8(output.stderr)?,
        "workspace member only does not inherit the workspace version; lockstep is broken\n"
    );
    Ok(())
}
