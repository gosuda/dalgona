//! Cold plugin errors report the Starlark location; real startup rejects absent extension libraries.
#![expect(
    clippy::disallowed_methods,
    reason = "SC test invokes the real startup path"
)]

#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{error::Error, fs, path::PathBuf, process::Command};

use support::{TestDir, dalgon_binary};

/// Runs `dalgon plugin list` against one enabled plugin whose `plugin.star`
/// holds `source`, returning the failure text.
fn plugin_list_failure(source: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
    let dir = TestDir::new()?;
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    let config_dir = home.join(".config/dal");
    let plugin_dir = home.join(".local/share/dal/plugins/broken");
    fs::create_dir_all(&config_dir)?;
    fs::create_dir_all(&plugin_dir)?;
    fs::create_dir_all(&workspace)?;
    fs::write(config_dir.join("dal.toml"), "plugins = [\"broken\"]\n")?;
    fs::write(plugin_dir.join("plugin.star"), source)?;
    let output = Command::new(dalgon_binary()?)
        .current_dir(&workspace)
        .env_clear()
        .envs(support::captured_shell_vars())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .args(["plugin", "list"])
        .output()?;
    assert!(!output.status.success());
    Ok(String::from_utf8(output.stderr)?)
}

/// The fixture's syntax error sits on line 1, where the loader's `1:1`
/// prefix (parse failures are reported at the entry head, see
/// `dal-star/tests/load_errors.rs`) is also the true position. Put the error
/// on other lines and columns: the vendor text the loader forwards must still
/// carry the true location and the offending source line.
#[test]
fn cold_plugin_parse_error_text_carries_the_real_line_and_column()
-> Result<(), Box<dyn Error + Send + Sync>> {
    for (source, line, column, offending) in [
        (
            "load(\"@dal/v1\", \"dal\")\n\n# the error is on line 4\ndef broken(:\n",
            4,
            12,
            "def broken(:",
        ),
        (
            "load(\"@dal/v1\", \"dal\")\nvalue = 1 +* 2\n",
            2,
            12,
            "value = 1 +* 2",
        ),
    ] {
        let error = plugin_list_failure(source)?;
        assert!(
            error.contains(&format!("plugin.star:{line}:{column}\n")),
            "expected the parse error located at line {line}, column {column}, got: {error}"
        );
        assert!(
            error.contains(&format!("{line} | {offending}\n")),
            "expected the offending source line {line} quoted, got: {error}"
        );
        assert!(error.contains("plugin load failed:"), "{error}");
    }
    Ok(())
}

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
        .join("../cli/dalgon/tests/fixtures/plugins/broken-reload.star");
    fs::copy(fixture, plugin_dir.join("plugin.star"))?;
    let output = Command::new(dalgon_binary()?)
        .current_dir(&workspace)
        .env_clear()
        .envs(support::captured_shell_vars())
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
