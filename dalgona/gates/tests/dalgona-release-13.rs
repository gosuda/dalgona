// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! This gate verifies generated publish order preserves every dependency edge.
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

use proptest::{
    prelude::any,
    test_runner::{Config, TestCaseError, TestRunner},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs, io,
    process::Command,
};

fn check_generated_graph(seed: u64) -> support::TestResult<()> {
    let root = support::repo_root();
    let scratch = support::Scratch::new("publish-order-property")?;
    let workspace = scratch.path();
    let count = 2 + usize::try_from(seed % 7)
        .map_err(|_| io::Error::other("seed remainder is too large"))?;
    let mut edges = BTreeSet::new();
    for index in 0..count - 1 {
        edges.insert((index, index + 1));
    }
    let mut bits = seed.rotate_left(13);
    for from in 0..count {
        for to in from + 2..count {
            bits ^= bits << 13;
            bits ^= bits >> 7;
            bits ^= bits << 17;
            if bits & 1 == 1 {
                edges.insert((from, to));
            }
        }
    }
    let members = (0..count)
        .map(|index| format!("crates/crate{index}"))
        .collect::<Vec<_>>();
    fs::write(
        workspace.join("Cargo.toml"),
        format!(
            "[workspace]\nresolver = \"3\"\nmembers = [{}]\n\n[workspace.package]\nversion = \"0.1.0\"\nedition = \"2024\"\n",
            members
                .iter()
                .map(|member| format!("\"{member}\""))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )?;
    for index in 0..count {
        let directory = workspace.join(format!("crates/crate{index}"));
        fs::create_dir_all(directory.join("src"))?;
        let mut manifest = format!(
            "[package]\nname = \"crate{index}\"\nversion.workspace = true\nedition.workspace = true\n"
        );
        let dependencies = edges
            .iter()
            .filter(|(_, to)| *to == index)
            .collect::<Vec<_>>();
        if !dependencies.is_empty() {
            manifest.push_str("\n[dependencies]\n");
            for (from, _) in dependencies {
                let _ = writeln!(
                    &mut manifest,
                    "crate{from} = {{ path = \"../crate{from}\", version = \"=0.1.0\" }}"
                );
            }
        }
        fs::write(directory.join("Cargo.toml"), manifest)?;
        fs::write(directory.join("src/lib.rs"), "")?;
    }
    let output = support::run_command(
        Command::new("bash")
            .arg(root.join("scripts/publish-crates.sh"))
            .args(["--dry-run"])
            .arg(workspace)
            .current_dir(&root),
    )?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let positions = String::from_utf8(output.stdout)?
        .lines()
        .enumerate()
        .map(|(position, line)| {
            (
                line.strip_prefix("cargo publish -p ")
                    .unwrap_or(line)
                    .to_owned(),
                position,
            )
        })
        .collect::<BTreeMap<_, _>>();
    for (dependency, dependent) in edges {
        assert!(positions[&format!("crate{dependency}")] < positions[&format!("crate{dependent}")]);
    }
    Ok(())
}

#[test]
fn publish_order_property_checks_every_generated_edge() -> support::TestResult<()> {
    let mut runner = TestRunner::new(Config {
        cases: 64,
        ..Config::default()
    });
    runner.run(&any::<u64>(), |seed| {
        check_generated_graph(seed).map_err(|error| TestCaseError::fail(error.to_string()))
    })?;
    Ok(())
}
