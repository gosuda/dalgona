#![expect(
    clippy::disallowed_methods,
    reason = "SC test invokes the real startup path"
)]

//! Real-process startup rejects absent extension libraries.

#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{error::Error, fs, path::PathBuf, process::Command};

use support::{TestDir, dalgon_binary};

#[test]
fn cold_plugin_error_reports_path_line_col() -> Result<(), Box<dyn Error + Send + Sync>> {
    let dir = TestDir::new()?;
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    let config_dir = home.join(".config/dal");
    let plugin_dir = home.join(".local/share/dal/plugins/broken");
    fs::create_dir_all(&config_dir)?;
    fs::create_dir_all(&plugin_dir)?;
    fs::create_dir_all(&workspace)?;
    fs::write(config_dir.join("dal.toml"), "plugins = [\"broken\"]\n")?;
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../crates/dalgon/tests/fixtures/plugins/broken-reload.star");
    fs::copy(fixture, plugin_dir.join("plugin.star"))?;
    let output = Command::new(dalgon_binary("dalgon")?)
        .current_dir(&workspace)
        .env_clear()
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .args(["plugin", "list"])
        .output()?;
    let error = String::from_utf8(output.stderr)?;
    assert!(!output.status.success());
    assert!(error.contains("plugin.star:1:"));
    assert!(error.contains("plugin load failed:"));
    Ok(())
}
