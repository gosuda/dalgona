// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
#[path = "support/mod.rs"]
mod support;

use std::process::Command;

#[test]
fn installer_reinstall_is_idempotent() -> support::TestResult<()> {
    let scratch = support::Scratch::new("dalgona-installer")?;
    let home = scratch.path().join("home");
    let cargo_home = home.join(".cargo");
    std::fs::create_dir_all(&cargo_home)?;
    let installer = scratch.path().join("dalgona-installer.sh");
    let url = "https://github.com/gosuda/dalgona/releases/download/dalgona-v0.1.0/dalgona-installer.sh";
    let download = support::run_command(
        Command::new("curl").args(["--fail", "--silent", "--show-error", "--location", "--output"]).arg(&installer).arg(url),
    )?;
    assert!(download.status.success(), "{}", String::from_utf8_lossy(&download.stderr));
    let first = support::run_command(
        Command::new("bash").arg(&installer).env("HOME", &home).env("CARGO_HOME", &cargo_home),
    )?;
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    let before = support::run_command(Command::new(cargo_home.join("bin/dalgona")).arg("--version"))?;
    assert_eq!(String::from_utf8(before.stdout)?, "dalgona 0.1.0\n");
    let second = support::run_command(
        Command::new("bash").arg(&installer).env("HOME", &home).env("CARGO_HOME", &cargo_home),
    )?;
    assert!(second.status.success(), "{}", String::from_utf8_lossy(&second.stderr));
    let after = support::run_command(Command::new(cargo_home.join("bin/dalgona")).arg("--version"))?;
    assert_eq!(String::from_utf8(after.stdout)?, "dalgona 0.1.0\n");
    Ok(())
}
