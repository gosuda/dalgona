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
    let tag = format!("dalgona-v{}", env!("CARGO_PKG_VERSION"));
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
    let listing = support::run_command(Command::new("tar").arg("-tf").arg(archive))?;
    assert!(
        listing.status.success(),
        "{}",
        String::from_utf8_lossy(&listing.stderr)
    );
    let listed = String::from_utf8(listing.stdout)?;
    // Tar members hang under dalgona-{target}/; the Windows zip is flat.
    // Directory headers carry a trailing slash on every tar flavor, so
    // normalize before comparing: the packaged shape is identical.
    let root = format!("dalgona-{target}/");
    let members: BTreeSet<String> = listed
        .lines()
        .map(|line| line.trim_end_matches('/'))
        .filter_map(|line| {
            if windows {
                (!line.is_empty()).then(|| line.to_owned())
            } else {
                line.strip_prefix(root.as_str())
                    .filter(|member| !member.is_empty())
                    .map(ToOwned::to_owned)
            }
        })
        .collect();
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
