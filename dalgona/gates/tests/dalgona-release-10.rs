// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
#[path = "support/mod.rs"]
mod support;

use std::process::Command;

#[test]
fn cargo_install_delivers_both_dalgona_binaries() -> support::TestResult<()> {
    let scratch = support::Scratch::new("dalgona-cargo-install")?;
    let cargo_home = scratch.path().join("cargo-home");
    std::fs::create_dir_all(&cargo_home)?;
    let install = support::run_command(
        Command::new("cargo")
            .args(["install", "dalgona", "--version", "0.1.0", "--locked"])
            .env("CARGO_HOME", &cargo_home),
    )?;
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    for binary in ["dalgona", "dg"] {
        let output = support::run_command(
            Command::new(cargo_home.join("bin").join(binary)).arg("--version"),
        )?;
        assert!(
            output.status.success(),
            "{binary}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8(output.stdout)?, "dalgona 0.1.0\n");
    }
    Ok(())
}
