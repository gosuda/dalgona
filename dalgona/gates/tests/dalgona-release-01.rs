// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate tests.
#![expect(
    clippy::disallowed_methods,
    reason = "gate runs the real product binaries"
)]

use dalgona_gates::support;
use sonic_rs::{JsonContainerTrait, JsonValueTrait};

use std::{collections::BTreeSet, process::Command};

const TARGETS: [&str; 6] = [
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
];

/// The archive `dist` plans for one Dalgona target: a zip on Windows, a
/// tar.xz elsewhere.
fn archive_name(target: &str) -> String {
    let extension = if target.ends_with("windows-msvc") {
        "zip"
    } else {
        "tar.xz"
    };
    format!("dalgona-{target}.{extension}")
}

/// Every artifact a Dalgona tag plans, and no other: the six target archives
/// with their checksums, both installers, the unified checksum, and the
/// source archive with its checksum.
fn expected_artifacts() -> BTreeSet<String> {
    let mut names = BTreeSet::from([
        "dalgona-installer.sh".to_owned(),
        "dalgona-installer.ps1".to_owned(),
        "sha256.sum".to_owned(),
        "source.tar.gz".to_owned(),
        "source.tar.gz.sha256".to_owned(),
    ]);
    for target in TARGETS {
        let archive = archive_name(target);
        names.insert(format!("{archive}.sha256"));
        names.insert(archive);
    }
    names
}

/// The `dist plan` JSON for `tag`. The planner reads the workspace only: it
/// runs without `GH_TOKEN` or `GITHUB_TOKEN`, so it never authenticates
/// against a host.
fn plan_for_tag(tag: &str) -> support::TestResult<sonic_rs::Value> {
    let output = support::run_command(
        Command::new("dist")
            .args(["plan", "--output-format=json"])
            .arg(format!("--tag={tag}"))
            .env_remove("GH_TOKEN")
            .env_remove("GITHUB_TOKEN")
            .current_dir(support::repo_root()),
    )?;
    assert!(
        output.status.success(),
        "dist plan --tag={tag} failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(sonic_rs::from_slice(&output.stdout)?)
}

/// One planned artifact by name.
fn artifact<'plan>(
    artifacts: &'plan sonic_rs::Value,
    name: &str,
) -> support::TestResult<&'plan sonic_rs::Value> {
    artifacts
        .get(name)
        .ok_or_else(|| format!("dist plan omitted {name}").into())
}

fn kind_of(artifact: &sonic_rs::Value) -> Option<&str> {
    artifact.get("kind").and_then(sonic_rs::Value::as_str)
}

fn triples_of(artifact: &sonic_rs::Value) -> Vec<&str> {
    artifact
        .get("target_triples")
        .and_then(sonic_rs::Value::as_array)
        .map(|triples| triples.iter().filter_map(sonic_rs::Value::as_str).collect())
        .unwrap_or_default()
}

/// One target archive: an executable archive for exactly that target, whose
/// checksum artifact is the target's checksum, holding both Dalgona binaries
/// and the product's documents.
fn assert_archive(artifacts: &sonic_rs::Value, target: &str) -> support::TestResult<()> {
    let name = archive_name(target);
    let archive = artifact(artifacts, &name)?;
    assert_eq!(
        kind_of(archive),
        Some("executable-zip"),
        "{name} is not an executable archive"
    );
    assert_eq!(
        triples_of(archive),
        [target],
        "{name} does not target {target}"
    );
    let checksum_name = format!("{name}.sha256");
    assert_eq!(
        archive.get("checksum").and_then(sonic_rs::Value::as_str),
        Some(checksum_name.as_str()),
        "{name} does not name its checksum"
    );
    let checksum = artifact(artifacts, &checksum_name)?;
    assert_eq!(
        kind_of(checksum),
        Some("checksum"),
        "{checksum_name} is not a checksum"
    );
    assert_eq!(
        triples_of(checksum),
        [target],
        "{checksum_name} is not the {target} checksum"
    );
    let assets = archive
        .get("assets")
        .and_then(sonic_rs::Value::as_array)
        .ok_or("dist plan omitted the archive assets")?;
    let paths: BTreeSet<&str> = assets
        .iter()
        .filter_map(|asset| asset.get("path").and_then(sonic_rs::Value::as_str))
        .collect();
    let suffix = if target.ends_with("windows-msvc") {
        ".exe"
    } else {
        ""
    };
    for binary in ["dalgona", "dg"] {
        let path = format!("{binary}{suffix}");
        assert!(paths.contains(path.as_str()), "{name} omits {path}");
    }
    for document in ["CHANGELOG.md", "LICENSE.md", "README.md", "man"] {
        assert!(paths.contains(document), "{name} omits {document}");
    }
    Ok(())
}

/// The installers, unified checksum, and source archive: each has its kind,
/// each installer installs from the tag's release, and the source archive
/// names its checksum artifact.
fn assert_support_artifacts(artifacts: &sonic_rs::Value, tag: &str) -> support::TestResult<()> {
    for installer in ["dalgona-installer.sh", "dalgona-installer.ps1"] {
        let planned = artifact(artifacts, installer)?;
        assert_eq!(
            kind_of(planned),
            Some("installer"),
            "{installer} is not an installer"
        );
        let hint = planned
            .get("install_hint")
            .and_then(sonic_rs::Value::as_str)
            .ok_or_else(|| format!("{installer} has no install hint"))?;
        assert!(
            hint.contains(&format!("/releases/download/{tag}/{installer}")),
            "{installer} does not install from the {tag} release: {hint}"
        );
    }
    assert_eq!(
        kind_of(artifact(artifacts, "sha256.sum")?),
        Some("unified-checksum"),
        "sha256.sum is not the unified checksum"
    );
    let source = artifact(artifacts, "source.tar.gz")?;
    assert_eq!(
        kind_of(source),
        Some("source-tarball"),
        "source.tar.gz is not a source archive"
    );
    assert_eq!(
        source.get("checksum").and_then(sonic_rs::Value::as_str),
        Some("source.tar.gz.sha256"),
        "the source archive does not name its checksum"
    );
    assert_eq!(
        kind_of(artifact(artifacts, "source.tar.gz.sha256")?),
        Some("checksum"),
        "source.tar.gz.sha256 is not a checksum"
    );
    Ok(())
}

#[test]
fn dalgona_dist_plan_lists_the_six_target_archives() -> support::TestResult<()> {
    let version = support::dalgona_version()?;
    let tag = format!("dalgona-v{version}");
    let plan = plan_for_tag(&tag)?;
    assert_eq!(
        plan.get("announcement_tag")
            .and_then(sonic_rs::Value::as_str),
        Some(tag.as_str())
    );
    assert_eq!(
        plan.get("announcement_tag_is_implicit")
            .and_then(sonic_rs::Value::as_bool),
        Some(false),
        "the singular product tag selects Dalgona, not the workspace"
    );

    let releases = plan
        .get("releases")
        .and_then(sonic_rs::Value::as_array)
        .ok_or("dist plan omitted releases")?;
    assert_eq!(releases.len(), 1, "the tag must select only dalgona");
    let release = releases
        .iter()
        .next()
        .ok_or("dist plan omitted the dalgona release")?;
    assert_eq!(
        release.get("app_name").and_then(sonic_rs::Value::as_str),
        Some("dalgona")
    );
    assert_eq!(
        release.get("app_version").and_then(sonic_rs::Value::as_str),
        Some(version.as_str())
    );
    let download_path = release
        .get("hosting")
        .and_then(|hosting| hosting.get("github"))
        .and_then(|github| github.get("artifact_download_path"))
        .and_then(sonic_rs::Value::as_str)
        .ok_or("dist plan omitted the product release download path")?;
    assert!(
        download_path.ends_with(&format!("/releases/download/{tag}")),
        "{tag} downloads from {download_path}"
    );

    let artifacts = plan.get("artifacts").ok_or("dist plan omitted artifacts")?;
    let planned: BTreeSet<&str> = artifacts
        .as_object()
        .ok_or("dist plan artifacts is not an object")?
        .iter()
        .map(|(name, _)| name)
        .collect();
    let expected = expected_artifacts();
    let wanted: BTreeSet<&str> = expected.iter().map(String::as_str).collect();
    assert_eq!(
        planned, wanted,
        "{tag} must plan exactly the six archives, their checksums, both installers, \
         the unified checksum, and the source archive"
    );
    for target in TARGETS {
        assert_archive(artifacts, target)?;
    }
    assert_support_artifacts(artifacts, &tag)
}
