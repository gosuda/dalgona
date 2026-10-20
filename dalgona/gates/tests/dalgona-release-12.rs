// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate tests.
#![expect(
    clippy::disallowed_methods,
    reason = "gate runs the real product binaries"
)]
use dalgona_gates::support;

use std::process::Command;

#[test]
fn installer_reinstall_is_idempotent() -> support::TestResult<()> {
    if !support::released_artifacts_requested() {
        return Ok(());
    }
    let version = support::dalgona_version()?;
    support::assert_release_tag_matches_version(&version)?;
    let scratch = support::Scratch::new("dalgona-installer")?;
    let home = scratch.path().join("home");
    let cargo_home = home.join(".cargo");
    std::fs::create_dir_all(&cargo_home)?;
    let installer = scratch.path().join("dalgona-installer.sh");
    let url = format!(
        "https://github.com/gosuda/dalgona/releases/download/dalgona-v{version}/dalgona-installer.sh"
    );
    let download = support::run_command(
        Command::new("curl")
            .args([
                "--fail",
                "--silent",
                "--show-error",
                "--location",
                "--output",
            ])
            .arg(&installer)
            .arg(url),
    )?;
    assert!(
        download.status.success(),
        "{}",
        String::from_utf8_lossy(&download.stderr)
    );
    let first = support::run_command(
        Command::new("bash")
            .arg(&installer)
            .env("HOME", &home)
            .env("CARGO_HOME", &cargo_home),
    )?;
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let before =
        support::run_command(Command::new(cargo_home.join("bin/dalgona")).arg("--version"))?;
    assert_eq!(
        String::from_utf8(before.stdout)?,
        format!("dalgona {version}\n")
    );
    let second = support::run_command(
        Command::new("bash")
            .arg(&installer)
            .env("HOME", &home)
            .env("CARGO_HOME", &cargo_home),
    )?;
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let after =
        support::run_command(Command::new(cargo_home.join("bin/dalgona")).arg("--version"))?;
    assert_eq!(
        String::from_utf8(after.stdout)?,
        format!("dalgona {version}\n")
    );
    Ok(())
}
