// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate tests.
#![expect(
    clippy::disallowed_methods,
    reason = "gate runs the real product binaries"
)]
use dalgona_gates::support;
use sonic_rs::JsonValueTrait;

use std::{collections::BTreeSet, fs, process::Command};

/// The native release lanes. Every workflow runner builds for its own host,
/// so the native archive assertion admits exactly these six triples and
/// fails any other runner instead of checking a foreign archive.
const NATIVE_TARGETS: [&str; 6] = [
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
];

/// The dalgona product version read from its own manifest, so the release
/// tag tracks independent dalgona bumps instead of this gates crate's
/// version. Mirrors the oracle in dalgona-alias-01.rs.
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

/// The runner's own host triple, read from `cargo -vV` the same way
/// cargo-dist derives its native build target, so the gate always selects
/// and checks the current native lane instead of a hard-coded row.
fn cargo_host_target() -> support::TestResult<String> {
    let output = support::run_command(Command::new("cargo").arg("-vV"))?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout)?;
    for line in text.lines() {
        if let Some(host) = line.strip_prefix("host: ") {
            return Ok(host.trim_end().to_owned());
        }
    }
    Err("cargo -vV reported no host target triple".into())
}

#[test]
fn dalgona_native_archive_has_only_binaries_metadata_and_man_pages() -> support::TestResult<()> {
    let workspace = support::repo_root();
    let target = cargo_host_target()?;
    assert!(
        NATIVE_TARGETS.contains(&target.as_str()),
        "the runner triple {target} is not one of the six native release lanes"
    );
    let tag = format!("dalgona-v{}", dalgona_product_version()?);
    let build = support::run_command(
        Command::new("dist")
            .args([
                "build",
                "--tag",
                &tag,
                &format!("--target={target}"),
                "--output-format=json",
            ])
            .current_dir(&workspace),
    )?;
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let manifest: sonic_rs::Value = sonic_rs::from_slice(&build.stdout)?;
    // cargo-dist zips Windows archives flat and tars the other platforms
    // under a target-named root directory, the same shapes release-06
    // pins from the product binstall metadata.
    let windows = target.contains("windows");
    let archive_name = format!(
        "dalgona-{target}.{}",
        if windows { "zip" } else { "tar.xz" }
    );
    let archive = manifest
        .get("artifacts")
        .and_then(|artifacts| artifacts.get(&archive_name))
        .and_then(|artifact| artifact.get("path"))
        .and_then(sonic_rs::Value::as_str)
        .map(std::path::Path::new)
        .ok_or(format!(
            "cargo-dist omitted the Dalgona {target} archive path"
        ))?;
    if !archive.is_file() {
        return Err(format!("cargo-dist produced no {archive_name} archive").into());
    }
    let zip = archive
        .extension()
        .is_some_and(|extension| extension == "zip");
    let tool = if zip { "unzip" } else { "tar" };
    let flag = if zip { "-l" } else { "-tf" };
    let listing = support::run_command(Command::new(tool).arg(flag).arg(archive))
        .map_err(|error| format!("cannot list {} with `{tool}`: {error}", archive.display()))?;
    assert!(
        listing.status.success(),
        "{}",
        String::from_utf8_lossy(&listing.stderr)
    );
    let listed = String::from_utf8(listing.stdout)?;
    let members = if zip {
        zip_members(&listed)
    } else {
        tar_members(&listed, &format!("dalgona-{target}/"))
    };
    let exe = if windows { ".exe" } else { "" };
    let mut expected = BTreeSet::from([
        format!("dalgona{exe}"),
        format!("dg{exe}"),
        "README.md".to_owned(),
        "LICENSE.md".to_owned(),
        "CHANGELOG.md".to_owned(),
        "man".to_owned(),
    ]);
    for entry in fs::read_dir(workspace.join("dalgona/dalgona/man"))? {
        let path = entry?.path();
        if path.extension().is_some_and(|extension| extension == "1") {
            expected.insert(format!(
                "man/{}",
                path.file_name()
                    .ok_or("man page has no filename")?
                    .to_string_lossy()
            ));
        }
    }
    assert_eq!(members, expected);
    Ok(())
}

/// Collects the member names of a tar archive under `root`. Tar members hang
/// under `dalgona-{target}/` and directory headers carry a trailing slash on
/// every flavor, so normalization happens before comparing.
fn tar_members(listed: &str, root: &str) -> BTreeSet<String> {
    listed
        .lines()
        .map(|line| line.trim_end_matches('/'))
        .filter_map(|line| {
            line.strip_prefix(root)
                .filter(|member| !member.is_empty())
                .map(ToOwned::to_owned)
        })
        .collect()
}

/// Collects the member names of a flat zip from `unzip -l`, which frames each
/// entry in `Length Date Time Name` columns inside a header and a summary
/// footer.
fn zip_members(listed: &str) -> BTreeSet<String> {
    listed
        .lines()
        .map(|line| line.trim_end_matches('/'))
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty()
                || trimmed.starts_with("Archive:")
                || trimmed.starts_with("Length")
                || trimmed.chars().all(|cell| cell == '-' || cell == ' ')
                || trimmed.ends_with(" file")
                || trimmed.ends_with(" files")
            {
                return None;
            }
            let name = trimmed
                .split_whitespace()
                .skip(3)
                .collect::<Vec<_>>()
                .join(" ");
            (!name.is_empty()).then_some(name)
        })
        .collect()
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

    #[test]
    fn tar_members_strip_the_target_root_and_drop_directories() {
        let listed = "dalgona-aarch64-apple-darwin/\n\
                      dalgona-aarch64-apple-darwin/dalgona\n\
                      dalgona-aarch64-apple-darwin/dg\n\
                      dalgona-aarch64-apple-darwin/README.md\n\
                      dalgona-aarch64-apple-darwin/man/\n\
                      dalgona-aarch64-apple-darwin/man/dalgona.1\n\
                      stray-other-root/dalgona\n";
        let members = tar_members(listed, "dalgona-aarch64-apple-darwin/");
        assert_eq!(
            members,
            BTreeSet::from([
                "dalgona".to_owned(),
                "dg".to_owned(),
                "README.md".to_owned(),
                "man".to_owned(),
                "man/dalgona.1".to_owned(),
            ])
        );
    }

    #[test]
    fn zip_members_skip_the_unzip_framing_and_take_the_name_column() {
        let listed = "Archive:  target/distrib/dalgona-aarch64-pc-windows-msvc.zip\n\
                      \x20 Length      Date    Time    Name\n\
                      ---------  ---------- -----   ----\n\
                      \x20   123456  2026-10-21 10:00   dalgona.exe\n\
                      \x20     2048  2026-10-21 10:00   dg.exe\n\
                      \x20     4096  2026-10-21 10:00   README.md\n\
                      \x20     1024  2026-10-21 10:00   man/dalgona.1\n\
                      ---------                     -------\n\
                      \x20   130624                     4 files\n";
        let members = zip_members(listed);
        assert_eq!(
            members,
            BTreeSet::from([
                "dalgona.exe".to_owned(),
                "dg.exe".to_owned(),
                "README.md".to_owned(),
                "man/dalgona.1".to_owned(),
            ])
        );
    }
}
