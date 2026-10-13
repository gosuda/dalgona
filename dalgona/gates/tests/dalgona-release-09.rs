// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate tests.
#![expect(
    clippy::disallowed_methods,
    reason = "gate runs the real product binaries"
)]
use dalgona_gates::support;
use sonic_rs::JsonValueTrait;

use std::{collections::BTreeSet, fs, process::Command};

#[test]
fn dalgona_linux_archive_has_only_binaries_metadata_and_man_pages() -> support::TestResult<()> {
    let workspace = support::repo_root();
    let tag = format!("dalgona-v{}", env!("CARGO_PKG_VERSION"));
    let build = support::run_command(
        Command::new("dist")
            .args([
                "build",
                "--tag",
                &tag,
                "--target=x86_64-unknown-linux-gnu",
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
    let archive = manifest
        .get("artifacts")
        .and_then(|artifacts| artifacts.get("dalgona-x86_64-unknown-linux-gnu.tar.xz"))
        .and_then(|artifact| artifact.get("path"))
        .and_then(sonic_rs::Value::as_str)
        .map(std::path::Path::new)
        .ok_or("cargo-dist omitted the Dalgona Linux archive path")?;
    if !archive.is_file() {
        return Err("cargo-dist produced no Dalgona Linux tar.xz archive".into());
    }
    let listing = support::run_command(Command::new("tar").arg("-tf").arg(archive))?;
    assert!(
        listing.status.success(),
        "{}",
        String::from_utf8_lossy(&listing.stderr)
    );
    let listed = String::from_utf8(listing.stdout)?;
    let members: BTreeSet<_> = listed
        .lines()
        .filter_map(|line| line.split_once('/').map(|(_, member)| member.to_owned()))
        .filter(|member| !member.is_empty())
        .collect();
    let mut expected = BTreeSet::from([
        "dalgona".to_owned(),
        "dg".to_owned(),
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
