// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! This gate verifies Dalgona man pages match the CLI tree.
#![expect(
    clippy::disallowed_methods,
    reason = "release gate drives real release commands"
)]
#[path = "support/mod.rs"]
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{collections::BTreeMap, fs, process::Command};

#[test]
fn dalgona_man_pages_match_the_cli_tree() -> support::TestResult<()> {
    let root = support::repo_root();
    let workspace = root.join("dalgona");
    let scratch = support::Scratch::new("dalgona-man-pages")?;
    let output_dir = scratch.path().join("rendered");
    let render = support::run_command(
        Command::new("cargo")
            .args([
                "run",
                "--locked",
                "-p",
                "dalgona",
                "--release",
                "--example",
                "render-man",
                "--",
            ])
            .arg(&output_dir)
            .current_dir(&workspace),
    )?;
    assert!(
        render.status.success(),
        "{}",
        String::from_utf8_lossy(&render.stderr)
    );
    let expected_dir = workspace.join("crates/dalgona/man");
    let mut expected = BTreeMap::new();
    for entry in fs::read_dir(&expected_dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|extension| extension == "1") {
            expected.insert(
                path.file_name()
                    .ok_or("man page has no filename")?
                    .to_owned(),
                fs::read(path)?,
            );
        }
    }
    let mut rendered = BTreeMap::new();
    for entry in fs::read_dir(&output_dir)? {
        let path = entry?.path();
        rendered.insert(
            path.file_name()
                .ok_or("rendered man page has no filename")?
                .to_owned(),
            fs::read(path)?,
        );
    }
    assert_eq!(rendered, expected);
    Ok(())
}
