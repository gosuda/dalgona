// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! This gate verifies that the plugin list shows all eleven bundled batteries.
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
fn plugin_list_shows_the_eleven_bundled_batteries() -> support::TestResult<()> {
    let scratch = support::Scratch::new("plugin-list-batteries")?;
    let output = support::run_command(
        Command::new(support::dalgona_binary()?)
            .args(["plugin", "list"])
            .env("XDG_DATA_HOME", scratch.path().join("data"))
            .env("XDG_CONFIG_HOME", scratch.path().join("config")),
    )?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let listing = String::from_utf8(output.stdout)?;
    for name in support::BATTERIES {
        let row = format!("{name} (bundled): services:");
        assert!(
            listing.lines().any(|line| line.starts_with(&row)),
            "plugin list has no bundled row for {name}:\n{listing}"
        );
    }
    assert!(!listing.contains("(user)"), "{listing}");
    Ok(())
}
