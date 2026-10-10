#![expect(clippy::unwrap_used, reason = "SC test")]
#![expect(
    clippy::disallowed_methods,
    reason = "SC test inspects tracked public content"
)]

//! Gate ledgers: manifest test names match public targets and pending state.
use std::{
    error::Error,
    path::{Path, PathBuf},
    process::Command,
};

const INVENTORIES: [(&str, &[&str]); 6] = [
    (
        "loop-headless",
        &[
            "loop-headless-01",
            "loop-headless-02",
            "loop-headless-03",
            "loop-headless-04",
            "loop-headless-05",
            "loop-headless-06",
            "loop-headless-07",
            "loop-headless-08",
            "loop-headless-09",
        ],
    ),
    (
        "loop-tui",
        &[
            "loop-tui-01",
            "loop-tui-02",
            "loop-tui-03",
            "loop-tui-04",
            "loop-tui-05",
            "loop-tui-06",
            "loop-tui-07",
            "loop-tui-08",
            "loop-tui-09",
            "loop-tui-10",
            "loop-tui-11",
            "loop-tui-12",
        ],
    ),
    (
        "loop-plugin",
        &[
            "loop-plugin-01",
            "loop-plugin-02",
            "loop-plugin-03",
            "loop-plugin-04",
            "loop-plugin-05",
            "loop-plugin-06",
            "loop-plugin-07",
            "loop-plugin-08",
            "loop-plugin-09",
            "loop-plugin-10",
            "loop-plugin-11",
            "loop-plugin-12",
            "loop-plugin-13",
        ],
    ),
    ("soak", &["soak-01"]),
    (
        "wires",
        &[
            "wires-01", "wires-02", "wires-03", "wires-04", "wires-05", "wires-06", "wires-07",
            "wires-08",
        ],
    ),
    (
        "dal-full",
        &[
            "dal-full-01",
            "dal-full-02",
            "dal-full-03",
            "dal-full-04",
            "dal-full-05",
            "dal-full-06",
            "dal-full-07",
            "dal-full-08",
            "dal-full-09",
        ],
    ),
];

const RELEASE_TARGETS: [&str; 4] = [
    "release_publish_order",
    "release_guards",
    "release_semver",
    "release_dist",
];

#[test]
fn all_gate_ledgers_match_public_targets_and_pending_state()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest = std::fs::read_to_string(root.join("dal/gates/Cargo.toml"))?;
    let expected = INVENTORIES
        .iter()
        .flat_map(|(_, targets)| targets.iter().copied())
        .chain(RELEASE_TARGETS)
        .collect::<std::collections::BTreeSet<_>>();
    let actual = manifest_test_names(&manifest);
    assert_eq!(
        actual, expected,
        "every explicit target must have one source"
    );

    let private_path = ["dalgona-", "private"].concat();
    assert!(
        !manifest.contains(&private_path),
        "the public gate manifest must not depend on a private path"
    );

    let output = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(&root)
        .output()?;
    assert!(output.status.success(), "git ls-files failed");
    for relative in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let relative = std::str::from_utf8(relative)?;
        let path = root.join(relative);
        let bytes = std::fs::read(&path)?;
        assert_public_content_is_private_free(relative.as_bytes(), &path, &bytes);
    }
    Ok(())
}

fn manifest_test_names(manifest: &str) -> std::collections::BTreeSet<&str> {
    let mut names = std::collections::BTreeSet::new();
    let mut in_test = false;
    for line in manifest.lines() {
        if line.trim() == "[[test]]" {
            in_test = true;
        } else if in_test && line.trim_start().starts_with("name = ") {
            let value = line.trim().strip_prefix("name = ").unwrap();
            names.insert(value.trim_matches('"'));
            in_test = false;
        }
    }
    names
}

fn assert_public_content_is_private_free(path: &[u8], display_path: &Path, content: &[u8]) {
    let display_path = display_path.display();
    let local_prefix = ["local:", "/"].concat();
    let planning_files = [
        ["dalgon-v0-", "plan.md"].concat(),
        ["dalgon-v0-", "decisions.md"].concat(),
        ["dalgon-v0-", "skeleton.md"].concat(),
        ["spec-", "dalgon-v0.md"].concat(),
        ["plan-parts/", "steps/"].concat(),
        ["diamond-", "whole-plan/"].concat(),
    ];

    assert!(
        !contains(path, local_prefix.as_bytes()),
        "private path in {display_path}"
    );
    assert!(
        !contains(content, local_prefix.as_bytes()),
        "private path in {display_path}"
    );
    for filename in planning_files {
        assert!(
            !contains(path, filename.as_bytes()),
            "planning filename in {display_path}"
        );
        assert!(
            !contains(content, filename.as_bytes()),
            "planning filename in {display_path}"
        );
    }
    assert!(!has_row_number(path), "planning citation in {display_path}");
    assert!(
        !has_row_number(content),
        "planning citation in {display_path}"
    );
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn has_row_number(bytes: &[u8]) -> bool {
    let prefix = *b"row ";
    for (index, window) in bytes.windows(prefix.len()).enumerate() {
        if window != prefix {
            continue;
        }
        // A citation token cannot start inside a longer word: model names
        // such as "Arrow X.Y" carry the prefix plus a digit inside them.
        if index > 0 && bytes[index - 1].is_ascii_alphanumeric() {
            continue;
        }
        let start = index + prefix.len();
        let mut end = start;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        if end > start && !matches!(bytes.get(end), Some(b'x' | b'X')) {
            return true;
        }
    }
    false
}
