// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! Binary-boundary proof that `dalgona dev run` drives the dalgona product:
//! its config defaults and its batteries, not the bare dal composition.

use std::{
    error::Error,
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Output},
};

struct Root(PathBuf);

impl Root {
    fn create() -> io::Result<Self> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "dalgona-dev-run-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos()),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Runs `dalgona dev run --consent` on `lines` with hermetic roots: no user
/// config, no credentials, the run root under `root`.
#[expect(
    clippy::disallowed_methods,
    reason = "binary-boundary test launches dalgona with isolated roots"
)]
fn dev_run(root: &Path, lines: &[&str]) -> io::Result<Output> {
    let scenario = root.join("scenario.jsonl");
    fs::write(&scenario, lines.join("\n"))?;
    let run_root = root.join("run");
    Command::new(env!("CARGO_BIN_EXE_dalgona"))
        .args(["dev", "run", "--consent", "--root"])
        .arg(&run_root)
        .arg(&scenario)
        .current_dir(root)
        .env_clear()
        .env("HOME", root)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("TMPDIR", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .output()
}

/// The documented `work` battery (`/todos`) is a dalgona battery the dal
/// product lacks. A dev run that builds the dal product instead of the
/// invoking factory rejects the command with `no command registered`.
#[test]
fn dev_run_drives_the_dalgona_batteries() -> Result<(), Box<dyn Error>> {
    let root = Root::create()?;
    let output = dev_run(
        &root.0,
        &[
            r#"{"config":"model = \"openai-responses/gpt-6\""}"#,
            r#"{"run":"todos"}"#,
        ],
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}{stderr}");
    assert!(stdout.contains("run todos -> done"), "{stdout}");
    assert!(stdout.contains("scenario passed (2 steps)"), "{stdout}");
    Ok(())
}

/// The config loads as the dalgona kind and reaches the dalgona build:
/// `disabled_batteries` is a dalgona-only key, and naming `work` there
/// removes `/todos`. Loading as the dal kind rejects the key at session
/// start (`config rejected`); building the dal product never registered
/// `/todos`. Only the command's own rejection after a dalgona load passes;
/// the first test pins `/todos` present, so together they catch either half.
#[test]
fn dev_run_loads_config_as_the_dalgona_kind() -> Result<(), Box<dyn Error>> {
    let root = Root::create()?;
    let output = dev_run(
        &root.0,
        &[
            r#"{"config":"model = \"openai-responses/gpt-6\"\ndisabled_batteries = [\"work\"]"}"#,
            r#"{"run":"todos"}"#,
        ],
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stdout}{stderr}");
    assert!(stdout.contains("   1  config"), "{stdout}{stderr}");
    assert!(stderr.contains("step 2:"), "{stdout}{stderr}");
    assert!(
        stderr.contains("no command registered for todos"),
        "{stdout}{stderr}"
    );
    Ok(())
}
