//! Publish-order gate: crates publish in dependency order.
#[path = "release_support/mod.rs"]
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt::Write as _,
    fs,
    process::Command,
};

use proptest::prelude::*;
use serde::Deserialize;

#[derive(Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
}

#[derive(Deserialize)]
struct CargoPackage {
    name: String,
    dependencies: Vec<CargoDependency>,
}

#[derive(Deserialize)]
struct CargoDependency {
    name: String,
    kind: Option<String>,
}

/// Parses `cargo publish -p <name>` dry-run lines into name -> position,
/// rejecting any other line and any crate published twice.
fn publish_positions(stdout: &str) -> Result<HashMap<String, usize>, Box<dyn Error>> {
    let mut positions = HashMap::new();
    for (position, line) in stdout.lines().enumerate() {
        let name = line
            .strip_prefix("cargo publish -p ")
            .ok_or_else(|| format!("unexpected output line: {line}"))?;
        if positions.insert(name.to_owned(), position).is_some() {
            return Err(format!("{name} is published twice in {stdout:?}").into());
        }
    }
    Ok(positions)
}

#[test]
fn release_publish_order_diamond() -> Result<(), Box<dyn Error>> {
    let (code, stdout, stderr) = support::run_publish_script(
        &support::repo_root(),
        &[
            "--product",
            "dal",
            "--dry-run",
            "cli/dalgon/tests/fixtures/publish-order",
        ],
    )?;
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stdout,
        "cargo publish -p base\ncargo publish -p left\ncargo publish -p right\ncargo publish -p top\n"
    );
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    Ok(())
}

#[expect(
    clippy::disallowed_methods,
    reason = "the publish-order gate reads the real cargo metadata"
)]
fn workspace_packages() -> Result<HashMap<String, CargoPackage>, Box<dyn Error>> {
    let output = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .arg("--manifest-path")
        .arg(support::repo_root().join("Cargo.toml"))
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8(output.stderr)?.into());
    }
    let metadata: CargoMetadata = sonic_rs::from_slice(&output.stdout)?;
    Ok(metadata
        .packages
        .into_iter()
        .map(|package| (package.name.clone(), package))
        .collect())
}

/// The real workspace's dry-run order must be a valid topological order of
/// the manifests' own dependency edges, read from `cargo metadata` rather
/// than from the publish script, and it must carry everything `dalgon`
/// needs: a lexicographic sort would also start every name with `dal-` and
/// end with `dalgon`, so name shape and count prove nothing.
#[test]
fn release_publish_order_real_workspace() -> Result<(), Box<dyn Error>> {
    let (code, stdout, stderr) = support::run_publish_script(
        &support::repo_root(),
        &["--product", "dal", "--dry-run", "."],
    )?;
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    let positions = publish_positions(&stdout)?;
    let packages = workspace_packages()?;

    for name in positions.keys() {
        assert!(
            packages.contains_key(name),
            "{name} is published but is not a workspace member"
        );
    }
    for (name, position) in &positions {
        for dependency in &packages[name].dependencies {
            if dependency.kind.as_deref() == Some("dev") {
                continue;
            }
            if let Some(dependency_position) = positions.get(&dependency.name) {
                assert!(
                    dependency_position < position,
                    "{name} (position {position}) publishes before its dependency {} (position {dependency_position})",
                    dependency.name
                );
            }
        }
    }

    // Everything dalgon builds against inside the workspace must publish.
    let mut pending = vec!["dalgon"];
    let mut seen = HashSet::new();
    while let Some(name) = pending.pop() {
        if !seen.insert(name) {
            continue;
        }
        assert!(
            positions.contains_key(name),
            "{name} is needed by dalgon but absent from the publish order {stdout:?}"
        );
        for dependency in &packages[name].dependencies {
            if dependency.kind.as_deref() != Some("dev") && packages.contains_key(&dependency.name)
            {
                pending.push(dependency.name.as_str());
            }
        }
    }
    Ok(())
}

fn materialize_workspace(
    names: &[String],
    edges: &[(usize, usize)],
) -> Result<tempfile::TempDir, Box<dyn Error>> {
    let workspace = tempfile::tempdir()?;
    let mut root_manifest = String::from("[workspace]\nresolver = \"3\"\nmembers = [\n");
    for name in names {
        let _ = writeln!(root_manifest, "    \"{name}\",");
    }
    root_manifest += "]\n\n[workspace.package]\nversion = \"0.1.0\"\nedition = \"2024\"\n";
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
            manifest += "\n[dependencies]\n";
            for dependency in dependencies {
                let dependency_name = &names[dependency];
                let _ = writeln!(
                    manifest,
                    "{dependency_name} = {{ path = \"../{dependency_name}\" }}"
                );
            }
        }
        fs::write(crate_root.join("Cargo.toml"), manifest)?;
        fs::write(crate_root.join("src/lib.rs"), "")?;
    }
    Ok(workspace)
}

/// A chain whose names sort opposite to its dependencies: the only valid
/// order is the dependency order, and the alphabetical order is its reverse.
#[test]
fn release_publish_order_follows_dependencies_not_names() -> Result<(), Box<dyn Error>> {
    let names = ["zeta-base", "mid", "app-top"].map(str::to_owned);
    let workspace = materialize_workspace(&names, &[(1, 0), (2, 1)])?;
    let (code, stdout, stderr) =
        support::run_publish_script(workspace.path(), &["--dry-run", "."])?;
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    assert_eq!(
        stdout,
        "cargo publish -p zeta-base\ncargo publish -p mid\ncargo publish -p app-top\n"
    );
    Ok(())
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
        // Edges always run from a higher vertex index to a lower one, so
        // naming vertices by index would make the alphabetical order a valid
        // publish order. Shuffle the labels and force a non-identity
        // permutation so the dependency order can never coincide with it.
        let mut labels: Vec<usize> = (0..vertex_count).collect();
        let mut shuffle = seed ^ 0x9e37_79b9_7f4a_7c15;
        for slot in (1..vertex_count).rev() {
            shuffle = shuffle
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let pick = usize::try_from(shuffle >> 33).unwrap_or(0) % (slot + 1);
            labels.swap(slot, pick);
        }
        if labels.iter().copied().eq(0..vertex_count) {
            labels.rotate_left(1);
        }
        let names: Vec<String> = labels.iter().map(|label| format!("crate{label}")).collect();
        let workspace = match materialize_workspace(&names, &edges) {
            Ok(workspace) => workspace,
            Err(error) => return Err(TestCaseError::fail(format!("could not create property workspace: {error}"))),
        };
        let (code, stdout, stderr) = match support::run_publish_script(workspace.path(), &["--dry-run", "."]) {
            Ok(output) => output,
            Err(error) => return Err(TestCaseError::fail(format!("could not run release script: {error}"))),
        };
        prop_assert!(code == 0, "stderr: {stderr}");
        prop_assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
        let positions = match publish_positions(&stdout) {
            Ok(positions) => positions,
            Err(error) => return Err(TestCaseError::fail(error.to_string())),
        };
        prop_assert_eq!(positions.len(), vertex_count);
        for (dependent, dependency) in &edges {
            let dependent_name = &names[*dependent];
            let dependency_name = &names[*dependency];
            let Some(dependent_position) = positions.get(dependent_name) else {
                return Err(TestCaseError::fail(format!("missing {dependent_name} from order")));
            };
            let Some(dependency_position) = positions.get(dependency_name) else {
                return Err(TestCaseError::fail(format!("missing {dependency_name} from order")));
            };
            prop_assert!(
                dependency_position < dependent_position,
                "dependency {dependency_name} followed dependent {dependent_name} in {stdout:?}"
            );
        }
    }
}
