// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
#[path = "support/mod.rs"]
mod support;

use std::{fs, process::Command};

#[test]
fn disabling_ask_lets_a_user_plugin_take_the_name() -> support::TestResult<()> {
    let root = support::repo_root();
    let scratch = support::Scratch::new("disabled-shadow-ask")?;
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
    fs::write(
        config_dir.join("dal.toml"),
        "disabled_batteries = [\"ask\"]\nplugins = [\"ask\"]\n",
    )?;
    let output = support::run_command(
        Command::new(support::dalgona_binary()?)
            .args(["plugin", "list"])
            .env("XDG_DATA_HOME", &data_home)
            .env("XDG_CONFIG_HOME", &config_home),
    )?;
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let listing = String::from_utf8(output.stdout)?;
    assert!(listing.lines().any(|line| line.starts_with("ask (user):")), "{listing}");
    assert!(!listing.lines().any(|line| line.starts_with("ask (bundled):")), "{listing}");
    Ok(())
}
