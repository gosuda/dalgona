// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! This gate verifies that version queries create no runtime resources.
#![expect(
    clippy::disallowed_methods,
    reason = "boot gate drives the real Dalgona binary boundary"
)]
#[path = "support/mod.rs"]
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::process::Command;

#[test]
fn version_query_does_not_create_runtime_resources() -> support::TestResult<()> {
    let scratch = support::Scratch::new("version-no-eager-work")?;
    let data_home = scratch.path().join("absent-xdg-home");
    let output = support::run_command(
        Command::new(support::dalgona_binary()?)
            .args(["--version"])
            .env("XDG_DATA_HOME", &data_home),
    )?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout)?, "dalgona 0.1.0\n");
    assert!(!data_home.exists());
    Ok(())
}
