// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate tests.
#![expect(
    clippy::disallowed_methods,
    reason = "gate runs the real product binaries"
)]
use gates::support;

use std::{collections::BTreeSet, fs, process::Command};

#[test]
fn dalgona_linux_archive_has_only_binaries_metadata_and_man_pages() -> support::TestResult<()> {
    let root = support::repo_root();
    let workspace = root.join("dalgona");
    let build = support::run_command(
        Command::new("dist")
            .args(["build", "--target=x86_64-unknown-linux-gnu"])
            .current_dir(&workspace),
    )?;
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map_or_else(|| workspace.join("target"), std::path::PathBuf::from);
    let distrib = target.join("distrib");
    let archive = fs::read_dir(&distrib)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|extension| extension == "xz"))
        .ok_or("cargo-dist produced no Linux tar.xz archive")?;
    let listing = support::run_command(Command::new("tar").arg("-tf").arg(&archive))?;
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
    ]);
    for entry in fs::read_dir(workspace.join("crates/dalgona/man"))? {
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
