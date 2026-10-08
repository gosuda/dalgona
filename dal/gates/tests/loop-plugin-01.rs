#![expect(
    clippy::disallowed_methods,
    reason = "SC test invokes the real startup path"
)]
#![expect(
    dead_code,
    reason = "gate support exposes helpers shared across independent targets"
)]

//! The v1 focus fixture loads, and invalid tool keys name their source file.

mod support;

use std::{error::Error, fs, path::PathBuf, process::Command};

use support::{TestDir, dalgon_binary};

#[test]
fn focus_plugin_loads_and_negative_tool_name_is_rejected()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let dir = TestDir::new()?;
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    let config_dir = home.join(".config/dal");
    let data_root = home.join(".local/share/dal");
    let plugin_dir = data_root.join("plugins/focus");
    fs::create_dir_all(&config_dir)?;
    fs::create_dir_all(&plugin_dir)?;
    fs::create_dir_all(&workspace)?;
    fs::write(config_dir.join("dal.toml"), "plugins = [\"focus\"]\n")?;
    let fixtures =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates/dalgon/tests/fixtures/plugins");
    fs::copy(fixtures.join("focus.star"), plugin_dir.join("plugin.star"))?;
    let valid = Command::new(dalgon_binary("dalgon")?)
        .current_dir(&workspace)
        .env_clear()
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .args(["plugin", "list"])
        .output()?;
    assert!(
        valid.status.success(),
        "{}",
        String::from_utf8_lossy(&valid.stderr)
    );
    assert!(String::from_utf8(valid.stdout)?.contains("focus"));

    fs::copy(
        fixtures.join("focus-negative.star"),
        plugin_dir.join("plugin.star"),
    )?;
    let invalid = Command::new(dalgon_binary("dalgon")?)
        .current_dir(&workspace)
        .env_clear()
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .args(["plugin", "list"])
        .output()?;
    let error = String::from_utf8(invalid.stderr)?;
    assert!(!invalid.status.success());
    assert!(error.contains("plugin.star:"), "{error}");
    assert!(
        error.contains("tool name \"BadName\" is invalid; names must match [a-z][a-z0-9_-]{0,63}"),
        "{error}"
    );
    Ok(())
}
