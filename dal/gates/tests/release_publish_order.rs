#[path = "release_support/mod.rs"]
mod support;

use std::{collections::HashMap, error::Error, fs, path::PathBuf};

use proptest::prelude::*;

fn dal_root() -> PathBuf {
    support::repo_root().join("dal")
}

#[test]
fn release_publish_order_diamond() -> Result<(), Box<dyn Error>> {
    let (code, stdout, stderr) = support::run_publish_script(
        &dal_root(),
        &["--dry-run", "crates/dalgon/tests/fixtures/publish-order"],
    )?;
    assert_eq!(code, 0);
    assert_eq!(
        stdout,
        "cargo publish -p base\ncargo publish -p left\ncargo publish -p right\ncargo publish -p top\n"
    );
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    Ok(())
}

#[test]
fn release_publish_order_real_workspace() -> Result<(), Box<dyn Error>> {
    let (code, stdout, stderr) = support::run_publish_script(&dal_root(), &["--dry-run", "."])?;
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 10);
    assert_eq!(lines.last(), Some(&"cargo publish -p dalgon"));
    assert!(
        lines[..9]
            .iter()
            .all(|line| line.starts_with("cargo publish -p dal-")),
        "unexpected publish order: {lines:?}"
    );
    Ok(())
}

fn materialize_workspace(
    vertex_count: usize,
    edges: &[(usize, usize)],
) -> Result<tempfile::TempDir, Box<dyn Error>> {
    let workspace = tempfile::tempdir()?;
    let names: Vec<String> = (0..vertex_count)
        .map(|index| format!("crate{index}"))
        .collect();
    let mut root_manifest = String::from("[workspace]\nresolver = \"3\"\nmembers = [\n");
    for name in &names {
        root_manifest.push_str(&format!("    \"{name}\",\n"));
    }
    root_manifest.push_str("]\n\n[workspace.package]\nversion = \"0.1.0\"\nedition = \"2024\"\n");
    fs::write(workspace.path().join("Cargo.toml"), root_manifest)?;
    for (index, name) in names.iter().enumerate() {
        let crate_root = workspace.path().join(name);
        fs::create_dir_all(crate_root.join("src"))?;
        let mut manifest = format!(
            "[package]\nname = \"{name}\"\nversion.workspace = true\nedition.workspace = true\n"
        );
        let dependencies: Vec<usize> = edges
            .iter()
            .filter_map(|(dependent, dependency)| (*dependent == index).then_some(*dependency))
            .collect();
        if !dependencies.is_empty() {
            manifest.push_str("\n[dependencies]\n");
            for dependency in dependencies {
                manifest.push_str(&format!(
                    "crate{dependency} = {{ path = \"../crate{dependency}\" }}\n"
                ));
            }
        }
        fs::write(crate_root.join("Cargo.toml"), manifest)?;
        fs::write(crate_root.join("src/lib.rs"), "")?;
    }
    Ok(workspace)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn release_publish_order_property(vertex_count in 2usize..=8, seed in any::<u64>()) {
        let mut state = seed;
        let mut edges = Vec::new();
        for from in 0..vertex_count {
            for to in (from + 1)..vertex_count {
                if to == from + 1 {
                    edges.push((to, from));
                } else {
                    state = state
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1);
                    if state >> 63 != 0 {
                        edges.push((to, from));
                    }
                }
            }
        }
        let workspace = match materialize_workspace(vertex_count, &edges) {
            Ok(workspace) => workspace,
            Err(error) => return Err(TestCaseError::fail(format!("could not create property workspace: {error}"))),
        };
        let (code, stdout, stderr) = match support::run_publish_script(workspace.path(), &["--dry-run", "."]) {
            Ok(output) => output,
            Err(error) => return Err(TestCaseError::fail(format!("could not run release script: {error}"))),
        };
        prop_assert!(code == 0, "stderr: {stderr}");
        prop_assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
        let mut positions = HashMap::new();
        for (position, line) in stdout.lines().enumerate() {
            let Some(name) = line.strip_prefix("cargo publish -p ") else {
                return Err(TestCaseError::fail(format!("unexpected output line: {line}")));
            };
            positions.insert(name.to_owned(), position);
        }
        prop_assert_eq!(positions.len(), vertex_count);
        for (dependent, dependency) in &edges {
            let dependent_name = format!("crate{dependent}");
            let dependency_name = format!("crate{dependency}");
            let Some(dependent_position) = positions.get(&dependent_name) else {
                return Err(TestCaseError::fail(format!("missing {dependent_name} from order")));
            };
            let Some(dependency_position) = positions.get(&dependency_name) else {
                return Err(TestCaseError::fail(format!("missing {dependency_name} from order")));
            };
            prop_assert!(
                dependency_position < dependent_position,
                "dependency {dependency_name} followed dependent {dependent_name} in {stdout:?}"
            );
        }
    }
}
