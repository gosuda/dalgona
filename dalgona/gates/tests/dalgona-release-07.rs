// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! This gate verifies semver rejects patch changes and accepts minor changes.
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

fn run_semver(
    current: &std::path::Path,
    baseline: &std::path::Path,
) -> support::TestResult<std::process::Output> {
    let root = support::repo_root();
    Ok(support::run_command(
        Command::new("bash")
            .arg(root.join("scripts/semver-gate.sh"))
            .arg("--baseline-root")
            .arg(baseline)
            .arg(current)
            .current_dir(&root),
    )?)
}

#[test]
fn semver_gate_rejects_patch_and_accepts_minor() -> support::TestResult<()> {
    let root = support::repo_root().join("dalgona/gates/tests/fixtures/release");
    let baseline = root.join("semver-baseline");
    let patch = run_semver(&root.join("semver-patch"), &baseline)?;
    assert_eq!(patch.status.code(), Some(1));
    let stderr = String::from_utf8(patch.stderr)?;
    assert!(
        stderr.ends_with("semver violation in semver-fixture; fix the change or bump the minor\n"),
        "{stderr}"
    );
    let minor = run_semver(&root.join("semver-minor"), &baseline)?;
    assert!(
        minor.status.success(),
        "{}",
        String::from_utf8_lossy(&minor.stderr)
    );
    Ok(())
}
