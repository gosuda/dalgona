// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! This gate verifies the Sustainable Use license is the only product license.
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

use std::{fs, path::Path};

fn rust_sources(path: &Path, output: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(path)? {
        let path = entry?.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            rust_sources(&path, output)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            output.push(path);
        }
    }
    Ok(())
}

#[test]
fn sul_license_is_the_only_product_license() -> support::TestResult<()> {
    let root = support::repo_root();
    let mut sources = Vec::new();
    rust_sources(&root.join("dalgona"), &mut sources)?;
    assert!(!sources.is_empty());
    for source in sources {
        let text = fs::read_to_string(source)?;
        assert!(text.starts_with("// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0\n"));
    }
    let license = fs::read_to_string(root.join("LICENSE.md"))?;
    assert!(license.contains("Sustainable Use License"));
    assert!(license.contains("metaphorics"));
    assert!(!root.join("dalgona/LICENSE").exists());
    assert!(!root.join("dalgona/NOTICE").exists());
    let workspace = fs::read_to_string(root.join("dalgona/Cargo.toml"))?;
    assert!(workspace.contains("license-file = \"../LICENSE.md\""));
    assert!(!workspace.contains("license = \"Apache"));
    for member in [
        root.join("dalgona/crates/dalgona/Cargo.toml"),
        root.join("dalgona/crates/dalgona-batteries/Cargo.toml"),
    ] {
        let contents = fs::read_to_string(member)?;
        assert!(contents.contains("license-file.workspace = true"));
        assert!(!contents.contains("license = \"Apache"));
    }
    let deny = support::run_command(
        std::process::Command::new("cargo")
            .args(["deny", "--locked", "check"])
            .current_dir(root.join("dalgona")),
    )?;
    assert!(
        deny.status.success(),
        "{}",
        String::from_utf8_lossy(&deny.stderr)
    );
    Ok(())
}
