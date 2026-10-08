//! Print mode refuses `ask` approval and names the flag.
#![expect(
    clippy::disallowed_methods,
    reason = "SC test invokes the real CLI boundary"
)]
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{error::Error, fs, path::PathBuf, process::Command};

use support::{TestDir, dalgon_binary};

#[test]
fn print_mode_denies_ask_and_names_approval_flag() -> Result<(), Box<dyn Error + Send + Sync>> {
    let dir = TestDir::new()?;
    let home = dir.path().join("home");
    let config_dir = home.join(".config/dal");
    let data_dir = home.join(".local/share/dal");
    fs::create_dir_all(&config_dir)?;
    fs::create_dir_all(&data_dir)?;
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../crates/dalgon/tests/fixtures/replay/loop-headless.jsonl");
    fs::write(
        config_dir.join("dal.toml"),
        format!(
            "model = \"openai-responses/gpt-6\"\napproval = \"ask\"\n[providers.scripted]\nfixture = {:?}\n",
            fixture.to_string_lossy()
        ),
    )?;
    let output = Command::new(dalgon_binary("dalgon")?)
        .current_dir(&home)
        .env_clear()
        .envs(support::captured_shell_vars())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .args(["-p", "--approval", "ask", "Read test.txt and patch it."])
        .output()?;
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)?.contains("--approval"));
    Ok(())
}
