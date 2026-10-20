// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dg == dalgona alias parity plus dalgona version independence.
#![expect(
    clippy::disallowed_methods,
    reason = "gate runs the real product binaries"
)]
use dalgona_gates::support;

use std::path::PathBuf;
use std::process::Command;

/// The compiled `dg` binary beside the resolved `dalgona` binary.
fn dg_binary() -> support::TestResult<PathBuf> {
    let dalgona = support::dalgona_binary()?;
    let dg = dalgona.with_file_name(format!("dg{}", std::env::consts::EXE_SUFFIX));
    if dg.is_file() {
        return Ok(dg);
    }
    Err(format!(
        "the dg binary is missing at {}; run `cargo build -p dalgona --bins` before this gate",
        dg.display()
    )
    .into())
}

/// The dalgona product version read from its own manifest, so the oracle
/// tracks independent dalgona bumps instead of this gates crate's version.
fn dalgona_product_version() -> support::TestResult<String> {
    let manifest =
        std::fs::read_to_string(support::repo_root().join("dalgona/dalgona/Cargo.toml"))?;
    for line in manifest.lines() {
        let line = line.trim();
        if let Some(version) = line.strip_prefix("version = ") {
            let version = version.trim().trim_matches('"');
            if !version.is_empty() {
                return Ok(version.to_owned());
            }
        }
    }
    Err("dalgona/dalgona/Cargo.toml carries no version line".into())
}

#[test]
fn dg_reports_the_dalgona_version_line() -> support::TestResult<()> {
    let dalgona = support::dalgona_binary()?;
    let dg = dg_binary()?;
    let expected = format!("dalgona {}\n", dalgona_product_version()?);
    for (name, bin) in [("dg", &dg), ("dalgona", &dalgona)] {
        let output = support::run_command(Command::new(bin).arg("--version"))?;
        assert_eq!(output.status.code(), Some(0), "{name} --version failed");
        assert!(
            output.stderr.is_empty(),
            "{name} --version leaked to stderr"
        );
        assert_eq!(
            String::from_utf8(output.stdout)?,
            expected,
            "{name} version line"
        );
    }
    Ok(())
}

#[test]
fn dg_shares_the_help_tree_and_eval_entry() -> support::TestResult<()> {
    let dalgona = support::dalgona_binary()?;
    let dg = dg_binary()?;
    for (name, bin) in [("dg", &dg), ("dalgona", &dalgona)] {
        let help = support::run_command(Command::new(bin).arg("--help"))?;
        assert_eq!(help.status.code(), Some(0), "{name} --help failed");
        let text = String::from_utf8(help.stdout)?;
        for sub in ["plugin", "rules", "serve", "docs", "completion", "login"] {
            assert!(text.contains(sub), "{name} help omits {sub}");
        }
        let eval = support::run_command(Command::new(bin).args(["eval", "--help"]))?;
        assert_eq!(eval.status.code(), Some(0), "{name} eval --help failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_oracle_reads_the_dalgona_manifest() -> support::TestResult<()> {
        let version = dalgona_product_version()?;
        assert!(
            version.split('.').count() >= 2,
            "oracle version is not dotted: {version}"
        );
        assert_eq!(
            version,
            support::dalgona_version()?,
            "the manifest scrape must agree with cargo metadata"
        );
        Ok(())
    }
}
