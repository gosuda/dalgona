// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate tests.
#![expect(
    clippy::disallowed_methods,
    reason = "gate runs the real product binaries"
)]
use gates::support;

use std::process::Command;

#[test]
fn cargo_binstall_fetches_release_without_source_build() -> support::TestResult<()> {
    let scratch = support::Scratch::new("dalgona-binstall")?;
    let cargo_home = scratch.path().join("cargo-home");
    std::fs::create_dir_all(&cargo_home)?;
    let output = support::run_command(
        Command::new("cargo")
            .args([
                "binstall",
                "--no-confirm",
                "--strategies",
                "crate-meta-data",
                "dalgona@0.1.0",
            ])
            .env("CARGO_HOME", &cargo_home),
    )?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        log.contains("releases/download/dalgona-v0.1.0/dalgona-"),
        "{log}"
    );
    assert!(
        !log.contains("Compiling dalgona v0.1.0"),
        "binstall source-built dalgona: {log}"
    );
    for binary in ["dalgona", "dg"] {
        assert!(
            cargo_home.join("bin").join(binary).is_file(),
            "missing installed {binary}"
        );
    }
    Ok(())
}
