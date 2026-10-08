//! Checks the semver gate accepts minor bumps and rejects patch-only breaks.
#![expect(
    dead_code,
    reason = "release support exposes gate helpers shared across release targets"
)]

#[path = "release_support/mod.rs"]
mod support;

use std::{error::Error, path::PathBuf};

fn fixture(name: &str) -> PathBuf {
    support::repo_root()
        .join("dal/crates/dalgon/tests/fixtures")
        .join(name)
}

#[test]
fn release_semver_patch_bump_violates_and_minor_bump_passes() -> Result<(), Box<dyn Error>> {
    let baseline = fixture("semver-fixture-baseline");
    let (patch_code, _, patch_stderr) =
        support::run_semver_gate(&fixture("semver-fixture-patch"), &baseline)?;
    assert_eq!(patch_code, 1);
    assert!(
        patch_stderr
            .ends_with("semver violation in semver-fixture; fix the change or bump the minor\n")
    );

    let (minor_code, _, minor_stderr) =
        support::run_semver_gate(&fixture("semver-fixture-minor"), &baseline)?;
    assert_eq!(minor_code, 0, "{minor_stderr}");
    assert!(
        !minor_stderr.contains("semver violation in semver-fixture"),
        "unexpected violation on minor bump: {minor_stderr}"
    );
    Ok(())
}
