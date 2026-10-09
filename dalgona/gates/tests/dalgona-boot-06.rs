// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! This gate verifies that a user plugin cannot reuse a bundled battery name.
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

use std::{fs, process::Command};

#[test]
fn user_plugin_named_like_a_battery_is_rejected_by_name() -> support::TestResult<()> {
    let root = support::repo_root();
    let scratch = support::Scratch::new("user-plugin-ask")?;
    let data_home = scratch.path().join("xdg-data");
    let config_home = scratch.path().join("xdg-config");
    let data_root = data_home.join("dalgona");
    fs::create_dir_all(data_root.join("plugins/ask"))?;
    fs::copy(
        root.join("dalgona/gates/tests/fixtures/plugins/ask/plugin.star"),
        data_root.join("plugins/ask/plugin.star"),
    )?;
    let config_dir = config_home.join("dalgona");
    fs::create_dir_all(&config_dir)?;
    fs::write(config_dir.join("dal.toml"), "plugins = [\"ask\"]\n")?;
    let output = support::run_command(
        Command::new(support::dalgona_binary()?)
            .args(["plugin", "list"])
            .env("XDG_DATA_HOME", &data_home)
            .env("XDG_CONFIG_HOME", &config_home),
    )?;
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("\"ask\""), "{stderr}");
    assert!(stderr.contains("already registered"), "{stderr}");
    Ok(())
}
