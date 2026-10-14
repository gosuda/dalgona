//! Distribution layout gate: packaged binaries and manifest artifacts.
#[path = "release_support/mod.rs"]
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;
use std::{
    collections::{BTreeMap, HashSet},
    error::Error,
    fs, io,
    path::Path,
    process::Command,
};

use serde::Deserialize;
use sonic_rs::{JsonContainerTrait, JsonValueTrait};

#[derive(Deserialize)]
struct DistManifest {
    artifacts: BTreeMap<String, sonic_rs::Value>,
}

#[derive(Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
}

#[derive(Deserialize)]
struct CargoPackage {
    name: String,
    version: String,
    repository: String,
    targets: Vec<CargoTarget>,
    metadata: Option<PackageMetadata>,
}

#[derive(Deserialize)]
struct CargoTarget {
    name: String,
    kind: Vec<String>,
}

#[derive(Deserialize)]
struct PackageMetadata {
    binstall: Option<Binstall>,
}

#[derive(Deserialize)]
struct Binstall {
    #[serde(rename = "pkg-url")]
    pkg_url: String,
    #[serde(rename = "pkg-fmt")]
    pkg_fmt: String,
    #[serde(rename = "bin-dir")]
    bin_dir: String,
    #[serde(default)]
    overrides: BTreeMap<String, BinstallOverride>,
}

#[derive(Deserialize)]
struct BinstallOverride {
    #[serde(rename = "pkg-url")]
    pkg_url: String,
    #[serde(rename = "pkg-fmt")]
    pkg_fmt: String,
    #[serde(rename = "bin-dir")]
    bin_dir: String,
}

const TARGETS: [&str; 6] = [
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
];

const MAN_PAGES: [&str; 84] = [
    "man/dalgon-__sandbox.1",
    "man/dalgon-acp.1",
    "man/dalgon-app-server.1",
    "man/dalgon-completion.1",
    "man/dalgon-dev-fold.1",
    "man/dalgon-dev-help-fold.1",
    "man/dalgon-dev-help-help.1",
    "man/dalgon-dev-help-journal-diff.1",
    "man/dalgon-dev-help-journal-replay.1",
    "man/dalgon-dev-help-journal-sidecar.1",
    "man/dalgon-dev-help-journal-torn.1",
    "man/dalgon-dev-help-journal.1",
    "man/dalgon-dev-help-run.1",
    "man/dalgon-dev-help.1",
    "man/dalgon-dev-journal-diff.1",
    "man/dalgon-dev-journal-help-diff.1",
    "man/dalgon-dev-journal-help-help.1",
    "man/dalgon-dev-journal-help-replay.1",
    "man/dalgon-dev-journal-help-sidecar.1",
    "man/dalgon-dev-journal-help-torn.1",
    "man/dalgon-dev-journal-help.1",
    "man/dalgon-dev-journal-replay.1",
    "man/dalgon-dev-journal-sidecar.1",
    "man/dalgon-dev-journal-torn.1",
    "man/dalgon-dev-journal.1",
    "man/dalgon-dev-run.1",
    "man/dalgon-dev.1",
    "man/dalgon-docs.1",
    "man/dalgon-help-__sandbox.1",
    "man/dalgon-help-acp.1",
    "man/dalgon-help-app-server.1",
    "man/dalgon-help-completion.1",
    "man/dalgon-help-dev-fold.1",
    "man/dalgon-help-dev-journal-diff.1",
    "man/dalgon-help-dev-journal-replay.1",
    "man/dalgon-help-dev-journal-sidecar.1",
    "man/dalgon-help-dev-journal-torn.1",
    "man/dalgon-help-dev-journal.1",
    "man/dalgon-help-dev-run.1",
    "man/dalgon-help-dev.1",
    "man/dalgon-help-docs.1",
    "man/dalgon-help-help.1",
    "man/dalgon-help-login-status.1",
    "man/dalgon-help-login.1",
    "man/dalgon-help-logout.1",
    "man/dalgon-help-models.1",
    "man/dalgon-help-plugin-grant.1",
    "man/dalgon-help-plugin-list.1",
    "man/dalgon-help-plugin-revoke.1",
    "man/dalgon-help-plugin.1",
    "man/dalgon-help-rpc.1",
    "man/dalgon-help-rules-test.1",
    "man/dalgon-help-rules.1",
    "man/dalgon-help-serve-token.1",
    "man/dalgon-help-serve.1",
    "man/dalgon-help.1",
    "man/dalgon-login-help-help.1",
    "man/dalgon-login-help-status.1",
    "man/dalgon-login-help.1",
    "man/dalgon-login-status.1",
    "man/dalgon-login.1",
    "man/dalgon-logout.1",
    "man/dalgon-models.1",
    "man/dalgon-plugin-grant.1",
    "man/dalgon-plugin-help-grant.1",
    "man/dalgon-plugin-help-help.1",
    "man/dalgon-plugin-help-list.1",
    "man/dalgon-plugin-help-revoke.1",
    "man/dalgon-plugin-help.1",
    "man/dalgon-plugin-list.1",
    "man/dalgon-plugin-revoke.1",
    "man/dalgon-plugin.1",
    "man/dalgon-rpc.1",
    "man/dalgon-rules-help-help.1",
    "man/dalgon-rules-help-test.1",
    "man/dalgon-rules-help.1",
    "man/dalgon-rules-test.1",
    "man/dalgon-rules.1",
    "man/dalgon-serve-help-help.1",
    "man/dalgon-serve-help-token.1",
    "man/dalgon-serve-help.1",
    "man/dalgon-serve-token.1",
    "man/dalgon-serve.1",
    "man/dalgon.1",
];

#[expect(
    clippy::disallowed_methods,
    reason = "the dist gate probes the real cargo-dist binary"
)]
fn dist_ready() -> Result<bool, Box<dyn Error>> {
    let output = match Command::new("dist").arg("--version").output() {
        Ok(output) => output,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let version = String::from_utf8(output.stdout)?;
    if !output.status.success() {
        return Err(io::Error::other(format!("cargo-dist failed: {version:?}")).into());
    }
    Ok(true)
}

enum Plan {
    Ready(DistManifest),
    Skip,
}

#[expect(
    clippy::disallowed_methods,
    reason = "the dist gate shells out to the real cargo-dist binary"
)]
fn load_plan(workspace: &Path) -> Result<Plan, Box<dyn Error>> {
    let output = Command::new("dist")
        .args(["plan", "--output-format=json"])
        .current_dir(workspace)
        .output()?;
    if !output.status.success() {
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        if text.contains("no packages in your workspace with distable binaries") {
            return Ok(Plan::Skip);
        }
        return Err(io::Error::other(text).into());
    }
    Ok(Plan::Ready(sonic_rs::from_slice(&output.stdout)?))
}

/// Reads the checked-in `dist plan` output recorded with cargo-dist 0.32.0:
/// `dist plan --output-format=json > gates/tests/fixtures/dist-plan.json`.
/// Tests assert against this file so the layout gate is red without the
/// binary; a missing file is an error, never a skip.
fn fixture_plan() -> Result<DistManifest, Box<dyn Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dist-plan.json");
    Ok(sonic_rs::from_str(&fs::read_to_string(path)?)?)
}

fn assert_manifest_artifacts(product: &str, manifest: &DistManifest) {
    let artifacts = &manifest.artifacts;
    let mut archives = Vec::new();
    for target in TARGETS {
        let extension = if target.ends_with("windows-msvc") {
            "zip"
        } else {
            "tar.xz"
        };
        let archive = format!("{product}-{target}.{extension}");
        assert!(artifacts.contains_key(&archive), "missing {archive}");
        assert!(
            artifacts.contains_key(&format!("{archive}.sha256")),
            "missing checksum for {archive}"
        );
        archives.push(archive);
    }
    assert!(artifacts.contains_key(&format!("{product}-installer.sh")));
    assert!(artifacts.contains_key(&format!("{product}-installer.ps1")));
    // `dist-manifest.json` is written by `dist build`/`host`, not `dist plan`; assert the source archive instead.
    assert!(
        artifacts.keys().any(|name| {
            !archives.contains(name)
                && !name.ends_with(".sha256")
                && (name.ends_with(".tar.gz") || name.ends_with(".tar.xz"))
        }),
        "the source archive is absent from {product}'s plan"
    );
}

#[expect(
    clippy::disallowed_methods,
    reason = "the dist gate reads the real cargo metadata"
)]
fn cargo_package(workspace: &Path, name: &str) -> Result<CargoPackage, Box<dyn Error>> {
    let output = Command::new("cargo")
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--manifest-path",
        ])
        .arg(workspace.join("Cargo.toml"))
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(String::from_utf8(output.stderr)?).into());
    }
    let metadata: CargoMetadata = sonic_rs::from_slice(&output.stdout)?;
    metadata
        .packages
        .into_iter()
        .find(|package| package.name == name)
        .ok_or_else(|| {
            io::Error::other(format!("package {name} is absent from Cargo metadata")).into()
        })
}

fn assert_binstall_urls(
    workspace: &Path,
    product: &str,
    tag_prefix: &str,
    artifacts: &DistManifest,
) -> Result<(), Box<dyn Error>> {
    let package = cargo_package(workspace, product)?;
    let binstall = package
        .metadata
        .and_then(|metadata| metadata.binstall)
        .ok_or_else(|| io::Error::other(format!("package {product} has no binstall metadata")))?;
    let windows = binstall.overrides.get("cfg(windows)").ok_or_else(|| {
        io::Error::other(format!(
            "package {product} has no Windows binstall override"
        ))
    })?;
    for target in TARGETS {
        let is_windows = target.ends_with("windows-msvc");
        let (url_template, format, bin_template) = if is_windows {
            (
                &windows.pkg_url,
                windows.pkg_fmt.as_str(),
                windows.bin_dir.as_str(),
            )
        } else {
            (
                &binstall.pkg_url,
                binstall.pkg_fmt.as_str(),
                binstall.bin_dir.as_str(),
            )
        };
        let extension = if is_windows { "zip" } else { "tar.xz" };
        let archive = format!("{product}-{target}.{extension}");
        let artifact = artifacts
            .artifacts
            .get(&archive)
            .ok_or_else(|| io::Error::other(format!("missing archive {archive}")))?;
        assert_eq!(format, if is_windows { "zip" } else { "txz" });
        let url = url_template
            .replace("{ repo }", &package.repository)
            .replace("{ version }", &package.version)
            .replace("{ name }", product)
            .replace("{ target }", target);
        assert_eq!(
            url,
            format!(
                "{}/releases/download/{tag_prefix}v{}/{archive}",
                package.repository, package.version
            )
        );
        let bin_dir = bin_template
            .replace("{ name }", product)
            .replace("{ target }", target)
            .replace("{ binary-ext }", if is_windows { ".exe" } else { "" });
        assert_eq!(
            bin_dir,
            if is_windows {
                "{ bin }.exe".to_owned()
            } else {
                format!("{product}-{target}/{{ bin }}")
            }
        );
        let assets = artifact
            .get("assets")
            .and_then(sonic_rs::Value::as_array)
            .ok_or_else(|| io::Error::other(format!("{archive} has no assets")))?;
        for binary in package
            .targets
            .iter()
            .filter(|target| target.kind.iter().any(|kind| kind == "bin"))
        {
            let filename = format!("{}{}", binary.name, if is_windows { ".exe" } else { "" });
            assert!(
                assets.iter().any(|asset| {
                    asset.get("path").and_then(sonic_rs::Value::as_str) == Some(filename.as_str())
                }),
                "{archive} is missing binary {filename}"
            );
        }
    }
    Ok(())
}

fn collect_regular_files(
    root: &Path,
    directory: &Path,
    files: &mut HashSet<String>,
) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect_regular_files(root, &path, files)?;
        } else if kind.is_file() {
            let relative = path
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/");
            files.insert(relative);
        } else {
            return Err(
                io::Error::other(format!("unexpected archive entry: {}", path.display())).into(),
            );
        }
    }
    Ok(())
}

#[test]
fn release_dist_plan_validity() -> Result<(), Box<dyn Error>> {
    let manifest = fixture_plan()?;
    assert_manifest_artifacts("dalgon", &manifest);
    assert_manifest_artifacts("dalgona", &manifest);
    let mut broken = fixture_plan()?;
    broken
        .artifacts
        .remove("dalgon-x86_64-unknown-linux-gnu.tar.xz");
    let red = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_manifest_artifacts("dalgon", &broken);
    }));
    assert!(
        red.is_err(),
        "a manifest missing an archive must fail the layout gate"
    );
    Ok(())
}

#[test]
fn release_binstall_url_expansion() -> Result<(), Box<dyn Error>> {
    let repo = support::repo_root();
    let manifest = fixture_plan()?;
    assert_binstall_urls(&repo, "dalgon", "dalgon-", &manifest)?;
    assert_binstall_urls(&repo, "dalgona", "dalgona-", &manifest)?;
    let mut broken = fixture_plan()?;
    broken
        .artifacts
        .remove("dalgona-x86_64-unknown-linux-gnu.tar.xz");
    assert!(
        assert_binstall_urls(&repo, "dalgona", "dalgona-", &broken).is_err(),
        "a manifest missing an archive must fail the binstall gate"
    );
    Ok(())
}

#[test]
fn release_tag_version_mismatch() -> Result<(), Box<dyn Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dist-tag-mismatch.txt");
    let diagnostic = fs::read_to_string(path)?;
    assert!(
        diagnostic.contains("claims we're releasing dalgon 9.9.9"),
        "the recorded tag diagnostic must name the mismatch: {diagnostic}"
    );
    let package = cargo_package(&support::repo_root(), "dalgon")?;
    assert_ne!(
        package.version, "9.9.9",
        "the recorded mismatch tag must differ from the workspace version"
    );
    assert!(
        diagnostic.contains(&package.version),
        "the recorded diagnostic must name the workspace version {}: {diagnostic}",
        package.version
    );
    Ok(())
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "the dist gate builds and unpacks a real release archive"
)]
fn release_archive_members_exact() -> Result<(), Box<dyn Error>> {
    if !dist_ready()? {
        return Err(
            io::Error::other("cargo-dist is required for release_archive_members_exact").into(),
        );
    }
    let workspace = support::repo_root();
    let manifest = match load_plan(&workspace)? {
        Plan::Ready(manifest) => manifest,
        Plan::Skip => {
            return Err(io::Error::other("dist plan reports no distable binaries").into());
        }
    };
    let archive_name = "dalgon-x86_64-unknown-linux-gnu.tar.xz";
    assert!(manifest.artifacts.contains_key(archive_name));
    let build = Command::new("dist")
        .args(["build", "--target=x86_64-unknown-linux-gnu"])
        .current_dir(&workspace)
        .output()?;
    if !build.status.success() {
        return Err(io::Error::other(String::from_utf8(build.stderr)?).into());
    }
    let archive = workspace.join("target/distrib").join(archive_name);
    let temporary = tempfile::tempdir()?;
    let unpack = Command::new("tar")
        .arg("-xJf")
        .arg(&archive)
        .arg("-C")
        .arg(temporary.path())
        .output()?;
    if !unpack.status.success() {
        return Err(io::Error::other(String::from_utf8(unpack.stderr)?).into());
    }
    let top_level: Vec<fs::DirEntry> = fs::read_dir(temporary.path())?.collect::<Result<_, _>>()?;
    assert_eq!(top_level.len(), 1);
    let root = top_level[0].path();
    assert!(root.is_dir());
    let mut actual = HashSet::new();
    collect_regular_files(&root, &root, &mut actual)?;
    let expected: HashSet<String> = [
        "dalgon",
        "dal",
        "dl",
        "README.md",
        "LICENSE.md",
        "CHANGELOG.md",
    ]
    .into_iter()
    .map(str::to_owned)
    .chain(MAN_PAGES.into_iter().map(str::to_owned))
    .collect();
    assert_eq!(actual, expected);
    Ok(())
}
