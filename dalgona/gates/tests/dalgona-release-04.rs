// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate tests.
#![expect(
    clippy::disallowed_methods,
    reason = "gate runs the real product binaries"
)]
use gates::support;

use std::{
    io::Write,
    process::{Command, Stdio},
};

fn check_presence(response: &str) -> support::TestResult<std::process::Output> {
    let root = support::repo_root();
    let mut child = Command::new("bash")
        .arg(root.join("scripts/publish-crates.sh"))
        .args(["--check-dep-only", "dal-journal", "0.1"])
        .arg(root.join("dalgona"))
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or("publisher stdin was not piped")?;
    stdin.write_all(response.as_bytes())?;
    drop(stdin);
    Ok(child.wait_with_output()?)
}

#[test]
fn dalgona_presence_gate_accepts_and_rejects_dal_versions() -> support::TestResult<()> {
    let matching = check_presence("{\"vers\":\"0.1.0\"}\n")?;
    assert!(
        matching.status.success(),
        "{}",
        String::from_utf8_lossy(&matching.stderr)
    );

    let mismatching = check_presence("{\"vers\":\"0.2.0\"}\n")?;
    assert_eq!(mismatching.status.code(), Some(3));
    assert_eq!(String::from_utf8(mismatching.stdout)?, "");
    assert_eq!(
        String::from_utf8(mismatching.stderr)?,
        "dalgon dependency dal-journal \"0.1\" not on crates.io; release dal first\n"
    );
    Ok(())
}
