// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! This gate verifies publish failures return the wrapper error literal.
#![expect(
    clippy::disallowed_methods,
    reason = "release gate drives real release commands"
)]
#[path = "support/mod.rs"]
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{env, fs, process::Command};

#[test]
fn publish_failure_returns_the_wrapper_literal() -> support::TestResult<()> {
    let root = support::repo_root();
    let scratch = support::Scratch::new("fake-cargo-publish")?;
    let fake_bin = scratch.path().join("bin");
    fs::create_dir_all(&fake_bin)?;
    let fake_cargo = fake_bin.join("cargo");
    let count_file = scratch.path().join("publish-count");
    let real_cargo = env::split_paths(&env::var_os("PATH").ok_or("PATH is unset")?)
        .map(|directory| directory.join(if cfg!(windows) { "cargo.exe" } else { "cargo" }))
        .find(|path| path.is_file())
        .ok_or("real cargo executable was not found in PATH")?;
    fs::write(
        &fake_cargo,
        "#!/bin/sh\nif [ \"$1\" = publish ]; then\n  count=$(cat \"$COUNT_FILE\" 2>/dev/null || printf 0)\n  count=$((count + 1))\n  printf '%s' \"$count\" > \"$COUNT_FILE\"\n  if [ \"$count\" -eq 2 ]; then printf 'fake cargo failure\\n' >&2; exit 1; fi\n  exit 0\nfi\nexec \"$REAL_CARGO\" \"$@\"\n",
    )?;
    let chmod = support::run_command(
        Command::new("bash")
            .args(["-c", "chmod +x \"$1\"", "bash"])
            .arg(&fake_cargo),
    )?;
    assert!(
        chmod.status.success(),
        "{}",
        String::from_utf8_lossy(&chmod.stderr)
    );
    let old_path = env::var_os("PATH").ok_or("PATH is unset")?;
    let mut paths = vec![fake_bin];
    paths.extend(env::split_paths(&old_path));
    let new_path = env::join_paths(paths)?;
    let fixture = root.join("dalgona/gates/tests/fixtures/release/publish-order");
    let output = support::run_command(
        Command::new("bash")
            .arg(root.join("scripts/publish-crates.sh"))
            .arg(fixture)
            .current_dir(&root)
            .env("PATH", new_path)
            .env("REAL_CARGO", real_cargo)
            .env("COUNT_FILE", count_file),
    )?;
    assert_eq!(output.status.code(), Some(5));
    let stderr = String::from_utf8(output.stderr)?;
    assert!(
        stderr.ends_with("cargo publish failed for left; see the output above\n"),
        "{stderr}"
    );
    Ok(())
}
