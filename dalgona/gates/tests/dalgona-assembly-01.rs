// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Static workspace-unity gate: one lockfile, 14 members, inherited
//! metadata, alias binaries that delegate.
//!
//! The metadata-graph gate owns transitive dependency direction; this file
//! owns the static facts that need no `cargo metadata` run.
use dalgona_gates::support;

use std::path::Path;

const MEMBERS: [&str; 14] = [
    "core/dal-core",
    "engine/dal-agent",
    "engine/dal-provider",
    "engine/dal-store",
    "extensions/dal-ext",
    "extensions/dal-star",
    "extensions/dal-tools",
    "clients/dal-tui",
    "clients/dal-wire",
    "cli/dalgon",
    "gates",
    "dalgona/dalgona",
    "dalgona/dalgona-batteries",
    "dalgona/gates",
];

const DAL_SIDE: [&str; 11] = [
    "core/dal-core",
    "engine/dal-agent",
    "engine/dal-provider",
    "engine/dal-store",
    "extensions/dal-ext",
    "extensions/dal-star",
    "extensions/dal-tools",
    "clients/dal-tui",
    "clients/dal-wire",
    "cli/dalgon",
    "gates",
];

const DAL_BINS: [(&str, &str); 3] = [
    (
        "cli/dalgon/src/bin/dalgon.rs",
        "dalgon::run(dalgon::product())",
    ),
    (
        "cli/dalgon/src/bin/dal.rs",
        "dalgon::run(dalgon::product())",
    ),
    ("cli/dalgon/src/bin/dl.rs", "dalgon::run(dalgon::product())"),
];

const DALGONA_BINS: [(&str, &str); 2] = [
    ("dalgona/dalgona/src/bin/dalgona.rs", "dalgona::run()"),
    ("dalgona/dalgona/src/bin/dg.rs", "dalgona::run()"),
];

fn manifest(root: &Path, member: &str) -> support::TestResult<toml::Value> {
    let text = std::fs::read_to_string(root.join(member).join("Cargo.toml"))?;
    Ok(toml::from_str(&text)?)
}

fn workspace_table(root: &Path) -> support::TestResult<toml::Value> {
    let text = std::fs::read_to_string(root.join("Cargo.toml"))?;
    let value: toml::Value = toml::from_str(&text)?;
    value
        .get("workspace")
        .cloned()
        .ok_or_else(|| "root Cargo.toml has no [workspace] table".into())
}

fn manifest_references_dalgona(text: &str) -> bool {
    text.contains("dalgona")
}

#[test]
fn workspace_keeps_one_root_lockfile() -> support::TestResult<()> {
    let root = support::repo_root();
    assert!(
        root.join("Cargo.lock").is_file(),
        "the workspace root must carry Cargo.lock"
    );
    // The tracked set is authoritative: scratch outputs (.build-target,
    // scratch/, local/) are git-ignored, while semver fixtures ship
    // deliberately under tests/fixtures/.
    let stdout = support::git_output(&root, &["ls-files", "-z"])?;
    let mut locks = stdout
        .split('\0')
        .filter(|path| path.ends_with("Cargo.lock"))
        .collect::<Vec<_>>();
    locks.sort_unstable();
    let mut unexpected = Vec::new();
    let mut root_locks = 0;
    for lock in &locks {
        if *lock == "Cargo.lock" {
            root_locks += 1;
        } else if !(lock.starts_with("cli/dalgon/tests/fixtures/")
            || lock.starts_with("dalgona/gates/tests/fixtures/"))
        {
            unexpected.push(*lock);
        }
    }
    assert!(
        unexpected.is_empty(),
        "unexpected tracked lockfiles: {unexpected:?}"
    );
    assert_eq!(
        root_locks, 1,
        "the workspace root must track one Cargo.lock"
    );
    Ok(())
}

#[test]
fn workspace_lists_fourteen_members_under_resolver_three() -> support::TestResult<()> {
    let root = support::repo_root();
    let workspace = workspace_table(&root)?;
    let resolver = workspace
        .get("resolver")
        .and_then(toml::Value::as_str)
        .ok_or("workspace resolver is missing")?;
    assert_eq!(resolver, "3", "workspace resolver drifted");
    let members = workspace
        .get("members")
        .and_then(toml::Value::as_array)
        .ok_or("workspace members list is missing")?;
    let actual = members
        .iter()
        .filter_map(toml::Value::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    let expected = MEMBERS
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(actual, expected, "workspace member inventory drifted");
    for member in MEMBERS {
        assert!(
            root.join(member).join("Cargo.toml").is_file(),
            "member {member} has no manifest"
        );
    }
    Ok(())
}

#[test]
fn members_inherit_edition_and_license() -> support::TestResult<()> {
    let root = support::repo_root();
    for member in MEMBERS {
        let package = manifest(&root, member)?;
        let package = package
            .get("package")
            .ok_or_else(|| format!("{member} has no [package] table"))?;
        for key in ["edition", "license-file"] {
            let inherited = package
                .get(key)
                .and_then(|value| value.get("workspace"))
                .and_then(toml::Value::as_bool)
                .unwrap_or(false);
            assert!(inherited, "{member} must set {key}.workspace = true");
        }
    }
    Ok(())
}

#[test]
fn dal_side_manifests_reference_no_dalgona_crate() -> support::TestResult<()> {
    let root = support::repo_root();
    for member in DAL_SIDE {
        let text = std::fs::read_to_string(root.join(member).join("Cargo.toml"))?;
        assert!(
            !manifest_references_dalgona(&text),
            "{member} must not reference a dalgona crate"
        );
    }
    Ok(())
}

#[test]
fn alias_binaries_delegate_to_their_product_entry() -> support::TestResult<()> {
    let root = support::repo_root();
    for (bin, delegate) in DAL_BINS.into_iter().chain(DALGONA_BINS) {
        let text = std::fs::read_to_string(root.join(bin))?;
        assert!(
            text.contains(delegate),
            "{bin} must delegate through `{delegate}`"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scanner_rejects_a_dalgona_reference_in_any_table() {
        for snippet in [
            "[dependencies]\ndalgona-batteries = { path = \"../dalgona-batteries\" }\n",
            "[dev-dependencies]\ndalgona = { path = \"../dalgona\" }\n",
            "[target.'cfg(unix)'.dependencies]\ndalgona-gates = { path = \"x\" }\n",
        ] {
            assert!(
                manifest_references_dalgona(snippet),
                "scanner missed a dalgona reference"
            );
        }
        assert!(!manifest_references_dalgona(
            "[dependencies]\ndal-core = { workspace = true }\n"
        ));
    }
}
