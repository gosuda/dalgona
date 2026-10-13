// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Cargo metadata gate for the one-root workspace and product boundary.
#![expect(
    clippy::disallowed_methods,
    reason = "the gate drives Cargo metadata to inspect the resolved graph"
)]

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;

use dalgona_gates::support;
use sonic_rs::{JsonContainerTrait, JsonValueTrait};

const DAL_MEMBERS: [(&str, &str); 11] = [
    ("dal-core", "core/dal-core"),
    ("dal-agent", "engine/dal-agent"),
    ("dal-provider", "engine/dal-provider"),
    ("dal-store", "engine/dal-store"),
    ("dal-ext", "extensions/dal-ext"),
    ("dal-star", "extensions/dal-star"),
    ("dal-tools", "extensions/dal-tools"),
    ("dal-tui", "clients/dal-tui"),
    ("dal-wire", "clients/dal-wire"),
    ("dalgon", "cli/dalgon"),
    ("dal-gates", "gates"),
];
const DALGONA_MEMBERS: [(&str, &str); 3] = [
    ("dalgona", "dalgona/dalgona"),
    ("dalgona-batteries", "dalgona/dalgona-batteries"),
    ("dalgona-gates", "dalgona/gates"),
];

#[derive(Debug)]
struct Package {
    name: String,
    manifest_path: String,
    source_is_null: bool,
}

#[derive(Debug)]
struct DepKind {
    kind: String,
    target: Option<String>,
}

#[derive(Debug)]
struct Edge {
    target: String,
    kinds: Vec<DepKind>,
}

#[derive(Debug)]
struct Graph {
    root: PathBuf,
    packages: BTreeMap<String, Package>,
    members: BTreeSet<String>,
    edges: BTreeMap<String, Vec<Edge>>,
}

fn string_field(value: &sonic_rs::Value, field: &str) -> support::TestResult<String> {
    value
        .get(field)
        .and_then(sonic_rs::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("Cargo metadata omitted string field {field}").into())
}
fn array_field<'a>(
    value: &'a sonic_rs::Value,
    field: &str,
) -> support::TestResult<&'a sonic_rs::Array> {
    value
        .get(field)
        .and_then(sonic_rs::Value::as_array)
        .ok_or_else(|| format!("Cargo metadata omitted array field {field}").into())
}

fn parse_graph(bytes: &[u8]) -> support::TestResult<Graph> {
    let document: sonic_rs::Value = sonic_rs::from_slice(bytes)?;
    let mut packages = BTreeMap::new();
    for package in array_field(&document, "packages")? {
        let id = string_field(package, "id")?;
        let source = package
            .get("source")
            .ok_or("Cargo metadata omitted package source")?;
        packages.insert(
            id,
            Package {
                name: string_field(package, "name")?,
                manifest_path: string_field(package, "manifest_path")?,
                source_is_null: source.is_null(),
            },
        );
    }

    let members = array_field(&document, "workspace_members")?
        .iter()
        .map(|member| {
            member
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "Cargo metadata contained a non-string workspace member".into())
        })
        .collect::<support::TestResult<BTreeSet<_>>>()?;
    let resolve = document
        .get("resolve")
        .ok_or("Cargo metadata omitted resolve")?;
    let mut edges = BTreeMap::new();
    for node in array_field(resolve, "nodes")? {
        let id = string_field(node, "id")?;
        let mut node_edges = Vec::new();
        for dependency in array_field(node, "deps")? {
            let mut kinds = Vec::new();
            for dep_kind in array_field(dependency, "dep_kinds")? {
                let kind = dep_kind
                    .get("kind")
                    .and_then(sonic_rs::Value::as_str)
                    .unwrap_or("normal")
                    .to_owned();
                let target = dep_kind
                    .get("target")
                    .and_then(sonic_rs::Value::as_str)
                    .map(str::to_owned);
                kinds.push(DepKind { kind, target });
            }
            node_edges.push(Edge {
                target: string_field(dependency, "pkg")?,
                kinds,
            });
        }
        edges.insert(id, node_edges);
    }
    Ok(Graph {
        root: PathBuf::from(string_field(&document, "workspace_root")?),
        packages,
        members,
        edges,
    })
}

fn expected_members() -> BTreeMap<&'static str, &'static str> {
    DAL_MEMBERS.into_iter().chain(DALGONA_MEMBERS).collect()
}

fn package_name<'a>(graph: &'a Graph, id: &str) -> support::TestResult<&'a str> {
    graph
        .packages
        .get(id)
        .map(|package| package.name.as_str())
        .ok_or_else(|| format!("Cargo resolve referenced unknown package {id}").into())
}

fn check_inventory(graph: &Graph, root: &Path) -> support::TestResult<()> {
    let expected = expected_members();
    assert_eq!(
        graph.members.len(),
        expected.len(),
        "workspace member count changed"
    );
    let actual = graph
        .members
        .iter()
        .map(|id| package_name(graph, id))
        .collect::<support::TestResult<BTreeSet<_>>>()?;
    let expected_names = expected.keys().copied().collect::<BTreeSet<_>>();
    assert_eq!(actual, expected_names, "workspace member inventory changed");

    for id in &graph.members {
        let package = graph
            .packages
            .get(id)
            .ok_or_else(|| format!("workspace member {id} is absent from packages"))?;
        let Some(relative) = expected.get(package.name.as_str()) else {
            return Err(format!("unexpected workspace member {}", package.name).into());
        };
        assert!(
            package.source_is_null,
            "workspace member {} must have null metadata source",
            package.name
        );
        assert_eq!(
            Path::new(&package.manifest_path),
            root.join(relative).join("Cargo.toml"),
            "{} resolved from an unexpected manifest",
            package.name
        );
    }
    Ok(())
}

fn check_edges(
    graph: &Graph,
    dal_names: &BTreeSet<&str>,
    dalgona_names: &BTreeSet<&str>,
    root: &Path,
) -> support::TestResult<()> {
    let expected = expected_members();
    for package in graph.packages.values() {
        let Some(relative) = expected.get(package.name.as_str()) else {
            continue;
        };
        if !package.source_is_null
            || Path::new(&package.manifest_path) != root.join(relative).join("Cargo.toml")
        {
            return Err(
                format!("internal package {} is not the local member", package.name).into(),
            );
        }
    }
    for edge in graph.edges.values().flatten() {
        let _ = package_name(graph, &edge.target)?;
        if edge.kinds.is_empty()
            || edge.kinds.iter().any(|kind| {
                kind.kind.is_empty() || kind.target.as_deref().is_some_and(str::is_empty)
            })
        {
            return Err(format!(
                "dependency {} has invalid kind or target metadata",
                edge.target
            )
            .into());
        }
    }

    let mut queue: VecDeque<_> = graph
        .members
        .iter()
        .filter_map(|id| {
            let package = graph.packages.get(id)?;
            dal_names
                .contains(package.name.as_str())
                .then_some((id.as_str(), package.name.as_str()))
        })
        .collect();
    let mut visited: BTreeSet<_> = queue.iter().map(|(id, _)| *id).collect();
    while let Some((id, root_name)) = queue.pop_front() {
        let current_name = package_name(graph, id)?;
        if dalgona_names.contains(current_name) {
            return Err(
                format!("dal package {root_name} reaches Dalgona package {current_name}").into(),
            );
        }
        let edges = graph
            .edges
            .get(id)
            .ok_or_else(|| format!("resolve omitted node for package {current_name}"))?;
        for edge in edges {
            if visited.insert(edge.target.as_str()) {
                queue.push_back((edge.target.as_str(), root_name));
            }
        }
    }
    Ok(())
}

#[test]
fn workspace_metadata_preserves_products_and_dependency_direction() -> support::TestResult<()> {
    let root = support::repo_root();
    let output = support::run_command(
        Command::new("cargo")
            .args([
                "metadata",
                "--locked",
                "--all-features",
                "--format-version",
                "1",
            ])
            .current_dir(&root),
    )?;
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let graph = parse_graph(&output.stdout)?;
    assert_eq!(graph.root.canonicalize()?, root.canonicalize()?);
    check_inventory(&graph, &graph.root)?;
    let dal_names = DAL_MEMBERS.iter().map(|(name, _)| *name).collect();
    let dalgona_names = DALGONA_MEMBERS.iter().map(|(name, _)| *name).collect();
    check_edges(&graph, &dal_names, &dalgona_names, &graph.root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_graph(edges: &[(&str, &str, &str, Option<&str>)]) -> Graph {
        let mut packages = BTreeMap::new();
        for (name, directory) in [
            ("dal-core", "core/dal-core"),
            ("bridge", "external/bridge"),
            ("dalgona", "dalgona/dalgona"),
        ] {
            packages.insert(
                name.to_owned(),
                Package {
                    name: name.to_owned(),
                    manifest_path: format!("/workspace/{directory}/Cargo.toml"),
                    source_is_null: true,
                },
            );
        }
        let members = BTreeSet::from(["dal-core".to_owned(), "dalgona".to_owned()]);
        let mut graph_edges: BTreeMap<String, Vec<Edge>> =
            packages.keys().map(|id| (id.clone(), Vec::new())).collect();
        for (source, target, kind, target_cfg) in edges {
            graph_edges
                .entry((*source).to_owned())
                .or_default()
                .push(Edge {
                    target: (*target).to_owned(),
                    kinds: vec![DepKind {
                        kind: (*kind).to_owned(),
                        target: target_cfg.map(str::to_owned),
                    }],
                });
        }
        Graph {
            root: PathBuf::from("/workspace"),
            packages,
            members,
            edges: graph_edges,
        }
    }

    #[test]
    fn direction_check_rejects_transitive_build_cfg_edge() {
        let graph = synthetic_graph(&[
            ("dal-core", "bridge", "build", Some("cfg(windows)")),
            ("bridge", "dalgona", "dev", None),
        ]);
        let dal = BTreeSet::from(["dal-core"]);
        let dalgona = BTreeSet::from(["dalgona"]);
        let error = check_edges(&graph, &dal, &dalgona, &graph.root).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("dal package dal-core reaches Dalgona package dalgona")
        );
    }

    #[test]
    fn direction_check_rejects_direct_dev_edge() {
        let graph = synthetic_graph(&[("dal-core", "dalgona", "dev", None)]);
        let dal = BTreeSet::from(["dal-core"]);
        let dalgona = BTreeSet::from(["dalgona"]);
        let error = check_edges(&graph, &dal, &dalgona, &graph.root).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("dal package dal-core reaches Dalgona package dalgona")
        );
    }

    #[test]
    fn direction_check_accepts_product_depending_on_dal() {
        let graph = synthetic_graph(&[("dalgona", "dal-core", "normal", None)]);
        let dal = BTreeSet::from(["dal-core"]);
        let dalgona = BTreeSet::from(["dalgona"]);
        check_edges(&graph, &dal, &dalgona, &graph.root).unwrap();
    }

    #[test]
    fn identity_check_rejects_transitive_registry_copy() {
        let mut graph = synthetic_graph(&[
            ("dal-core", "bridge", "normal", None),
            ("bridge", "registry-dal-core", "normal", None),
        ]);
        graph.packages.insert(
            "registry-dal-core".to_owned(),
            Package {
                name: "dal-core".to_owned(),
                manifest_path: "/registry/dal-core/Cargo.toml".to_owned(),
                source_is_null: false,
            },
        );
        graph
            .edges
            .insert("registry-dal-core".to_owned(), Vec::new());
        let dal = BTreeSet::from(["dal-core"]);
        let dalgona = BTreeSet::from(["dalgona"]);
        let error = check_edges(&graph, &dal, &dalgona, &graph.root).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("internal package dal-core is not the local member")
        );
    }
}
