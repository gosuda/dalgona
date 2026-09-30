// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
#[path = "support/mod.rs"]
mod support;

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
    assert_eq!(
        String::from_utf8(output.stderr)?,
        "workspace member extra depends on fake-dal by path; dalgona builds only against published dalgon crates\n"
    );
    Ok(())
}
