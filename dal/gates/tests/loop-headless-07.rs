#![expect(
    clippy::disallowed_methods,
    reason = "SC test invokes the real CLI boundary"
)]

//! JSON mode emits exactly one result or one error.
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{error::Error, fs, path::PathBuf, process::Command};

use sonic_rs::JsonValueTrait;
use support::{TestDir, dalgon_binary};

#[test]
fn json_mode_emits_one_result_or_one_error() -> Result<(), Box<dyn Error + Send + Sync>> {
    let dir = TestDir::new()?;
    let home = dir.path().join("home");
    let config_dir = home.join(".config/dal");
    let data_dir = home.join(".local/share/dal");
    fs::create_dir_all(&config_dir)?;
    fs::create_dir_all(&data_dir)?;
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../crates/dalgon/tests/fixtures/replay/stress-scripted.jsonl");
    fs::write(
        config_dir.join("dal.toml"),
        format!(
            "model = \"openai-responses/gpt-6\"\n[providers.scripted]\nfixture = {:?}\n",
            fixture.to_string_lossy()
        ),
    )?;
    let output = Command::new(dalgon_binary("dalgon")?)
        .current_dir(&home)
        .env_clear()
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .args(["--json", "Return the scripted response."])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout)?;
    let frames = stdout.lines().collect::<Vec<_>>();
    assert_eq!(frames.len(), 1);
    let response: sonic_rs::Value = sonic_rs::from_str(frames[0])?;
    assert!(response.get("result").is_some());
    Ok(())
}
