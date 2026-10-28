//! Distribution layout gate: packaged binaries and manifest artifacts.
#[path = "release_support/mod.rs"]
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    error::Error,
    fmt, fs, io,
    path::Path,
    process::Command,
};

use serde::Deserialize;
use sonic_rs::{JsonContainerTrait, JsonValueTrait};

#[derive(Deserialize)]
struct DistManifest {
    artifacts: BTreeMap<String, sonic_rs::Value>,
}

/// The `dist plan --output-format=json` fields the layout gate reads. A plan
/// for one announcement tag lists the one release that tag selects.
#[derive(Deserialize)]
struct TaggedPlan {
    announcement_tag: String,
    announcement_tag_is_implicit: bool,
    releases: Vec<PlanRelease>,
    artifacts: BTreeMap<String, PlanArtifact>,
}

#[derive(Deserialize)]
struct PlanRelease {
    app_name: String,
    app_version: String,
    artifacts: Vec<String>,
    hosting: PlanHosting,
}

#[derive(Deserialize)]
struct PlanHosting {
    github: PlanGithub,
}

#[derive(Deserialize)]
struct PlanGithub {
    artifact_download_path: String,
}

#[derive(Deserialize)]
struct PlanArtifact {
    kind: String,
    #[serde(default)]
    target_triples: Vec<String>,
    checksum: Option<String>,
    #[serde(default)]
    assets: Vec<PlanAsset>,
    install_hint: Option<String>,
}

#[derive(Deserialize)]
struct PlanAsset {
    path: String,
    kind: String,
}

/// What one product's tagged plan must say: the tag naming the Cargo
/// version, and the binaries Cargo builds for the product.
struct ExpectedRelease {
    product: &'static str,
    version: String,
    tag: String,
    binaries: Vec<String>,
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
/// The binstall URL gate expands the product manifests against this fixed
/// input. The plan-layout gate does not read it: `release_dist_plan_validity`
/// runs the real planner for each product's tag. A missing file is an error,
/// never a skip.
fn fixture_plan() -> Result<DistManifest, Box<dyn Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dist-plan.json");
    Ok(sonic_rs::from_str(&fs::read_to_string(path)?)?)
}

/// The archive `dist` plans for one product and target: a zip on Windows,
/// a tar.xz elsewhere.
fn archive_name(product: &str, target: &str) -> String {
    let extension = if target.ends_with("windows-msvc") {
        "zip"
    } else {
        "tar.xz"
    };
    format!("{product}-{target}.{extension}")
}

/// Every artifact name a plan for `product`'s tag lists, and no other: the
/// six target archives with their checksums, both installers, the unified
/// checksum, and the source archive with its checksum.
fn expected_plan_artifacts(product: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::from([
        "sha256.sum".to_owned(),
        "source.tar.gz".to_owned(),
        "source.tar.gz.sha256".to_owned(),
        format!("{product}-installer.sh"),
        format!("{product}-installer.ps1"),
    ]);
    for target in TARGETS {
        let archive = archive_name(product, target);
        names.insert(format!("{archive}.sha256"));
        names.insert(archive);
    }
    names
}

/// The executable suffix `dist` gives binaries shipped for one target.
fn executable_suffix(target: &str) -> &'static str {
    if target.ends_with("windows-msvc") {
        ".exe"
    } else {
        ""
    }
}

/// A tagged plan that lacks the topology its release requires.
#[derive(Debug)]
struct PlanMismatch(String);

impl fmt::Display for PlanMismatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for PlanMismatch {}

fn ensure(condition: bool, message: impl FnOnce() -> String) -> Result<(), PlanMismatch> {
    if condition {
        Ok(())
    } else {
        Err(PlanMismatch(message()))
    }
}

fn planned_artifact<'plan>(
    plan: &'plan TaggedPlan,
    name: &str,
) -> Result<&'plan PlanArtifact, PlanMismatch> {
    plan.artifacts
        .get(name)
        .ok_or_else(|| PlanMismatch(format!("the plan lists no artifact {name}")))
}

/// Reads the expectation for `product` from Cargo: the tag that names its
/// version and the binaries it builds.
fn expected_release(
    workspace: &Path,
    product: &'static str,
) -> Result<ExpectedRelease, Box<dyn Error>> {
    let package = cargo_package(workspace, product)?;
    let binaries = package
        .targets
        .iter()
        .filter(|target| target.kind.iter().any(|kind| kind == "bin"))
        .map(|target| target.name.clone())
        .collect();
    Ok(ExpectedRelease {
        product,
        tag: format!("{product}-v{}", package.version),
        version: package.version,
        binaries,
    })
}

/// Checks the release a tagged plan selects: the explicit announcement tag,
/// exactly one release for the product at its Cargo version, the download
/// path under that tag, and a release artifact list that is the plan's whole
/// artifact table.
fn check_plan_release(expected: &ExpectedRelease, plan: &TaggedPlan) -> Result<(), PlanMismatch> {
    let tag = expected.tag.as_str();
    ensure(plan.announcement_tag == tag, || {
        format!("the plan announces {}, not {tag}", plan.announcement_tag)
    })?;
    ensure(!plan.announcement_tag_is_implicit, || {
        format!("{tag} was inferred, not selected by the explicit tag")
    })?;
    let [release] = plan.releases.as_slice() else {
        return Err(PlanMismatch(format!(
            "{tag} must select exactly one release, not {}",
            plan.releases.len()
        )));
    };
    ensure(
        release.app_name == expected.product && release.app_version == expected.version,
        || {
            format!(
                "{tag} selects {} {}, not {} {}",
                release.app_name, release.app_version, expected.product, expected.version
            )
        },
    )?;
    let download = format!("/releases/download/{tag}");
    ensure(
        release
            .hosting
            .github
            .artifact_download_path
            .ends_with(&download),
        || {
            format!(
                "{tag} downloads from {}, not under {download}",
                release.hosting.github.artifact_download_path
            )
        },
    )?;
    let listed: BTreeSet<&str> = release.artifacts.iter().map(String::as_str).collect();
    let planned: BTreeSet<&str> = plan.artifacts.keys().map(String::as_str).collect();
    ensure(
        listed == planned && release.artifacts.len() == planned.len(),
        || format!("the release of {tag} lists artifacts other than the plan's table"),
    )
}

/// Checks that the plan's artifact table is exactly the product's set, so no
/// artifact of the other product, and no stray file, rides along.
fn check_plan_artifact_set(
    expected: &ExpectedRelease,
    plan: &TaggedPlan,
) -> Result<(), PlanMismatch> {
    let wanted = expected_plan_artifacts(expected.product);
    let actual: BTreeSet<String> = plan.artifacts.keys().cloned().collect();
    ensure(actual == wanted, || {
        format!(
            "{} plans the wrong artifacts: missing {:?}, unexpected {:?}",
            expected.tag,
            wanted.difference(&actual).collect::<Vec<_>>(),
            actual.difference(&wanted).collect::<Vec<_>>()
        )
    })
}

/// Checks one target archive's contents: exactly the product's executables,
/// plus the README, license, changelog, and manual directory.
fn check_archive_assets(
    expected: &ExpectedRelease,
    archive: &str,
    target: &str,
    artifact: &PlanArtifact,
) -> Result<(), PlanMismatch> {
    let suffix = executable_suffix(target);
    let wanted: BTreeSet<String> = expected
        .binaries
        .iter()
        .map(|binary| format!("{binary}{suffix}"))
        .collect();
    let shipped: BTreeSet<String> = artifact
        .assets
        .iter()
        .filter(|asset| asset.kind == "executable")
        .map(|asset| asset.path.clone())
        .collect();
    ensure(shipped == wanted, || {
        format!("{archive} ships executables {shipped:?}, not {wanted:?}")
    })?;
    for document in ["README.md", "LICENSE.md", "CHANGELOG.md", "man"] {
        ensure(
            artifact
                .assets
                .iter()
                .any(|asset| asset.path == document && asset.kind != "executable"),
            || format!("{archive} omits {document}"),
        )?;
    }
    Ok(())
}

/// Checks the six target archives: each is one target's executable archive
/// with a checksum artifact that names it, and holds the product's files.
fn check_plan_archives(expected: &ExpectedRelease, plan: &TaggedPlan) -> Result<(), PlanMismatch> {
    for target in TARGETS {
        let name = archive_name(expected.product, target);
        let checksum_name = format!("{name}.sha256");
        let archive = planned_artifact(plan, &name)?;
        ensure(archive.kind == "executable-zip", || {
            format!("{name} has kind {}, not executable-zip", archive.kind)
        })?;
        ensure(archive.target_triples == [target], || {
            format!("{name} targets {:?}, not {target}", archive.target_triples)
        })?;
        ensure(
            archive.checksum.as_deref() == Some(checksum_name.as_str()),
            || format!("{name} names checksum {:?}", archive.checksum),
        )?;
        let checksum = planned_artifact(plan, &checksum_name)?;
        ensure(
            checksum.kind == "checksum" && checksum.target_triples == [target],
            || format!("{checksum_name} is not the {target} checksum"),
        )?;
        check_archive_assets(expected, &name, target, archive)?;
    }
    Ok(())
}

/// Checks the installers, unified checksum, and source archive: each has its
/// kind, and each installer's hint downloads from the tag's release.
fn check_plan_support_artifacts(
    expected: &ExpectedRelease,
    plan: &TaggedPlan,
) -> Result<(), PlanMismatch> {
    let tag = expected.tag.as_str();
    for installer in [
        format!("{}-installer.sh", expected.product),
        format!("{}-installer.ps1", expected.product),
    ] {
        let artifact = planned_artifact(plan, &installer)?;
        ensure(artifact.kind == "installer", || {
            format!("{installer} has kind {}, not installer", artifact.kind)
        })?;
        let hint = artifact.install_hint.as_deref().unwrap_or_default();
        ensure(
            hint.contains(&format!("/releases/download/{tag}/{installer}")),
            || format!("{installer} does not install from the {tag} release: {hint}"),
        )?;
    }
    ensure(
        planned_artifact(plan, "sha256.sum")?.kind == "unified-checksum",
        || "sha256.sum is not the unified checksum".to_owned(),
    )?;
    let source = planned_artifact(plan, "source.tar.gz")?;
    ensure(
        source.kind == "source-tarball"
            && source.checksum.as_deref() == Some("source.tar.gz.sha256"),
        || {
            format!(
                "source.tar.gz is not a source archive with its checksum: {}",
                source.kind
            )
        },
    )?;
    ensure(
        planned_artifact(plan, "source.tar.gz.sha256")?.kind == "checksum",
        || "source.tar.gz.sha256 is not a checksum".to_owned(),
    )
}

/// Checks that `plan` is the whole release plan of the tag `expected` names:
/// the release it selects, its exact artifact set, the six target archives,
/// and the installers and source archive.
fn check_plan_topology(expected: &ExpectedRelease, plan: &TaggedPlan) -> Result<(), PlanMismatch> {
    check_plan_release(expected, plan)?;
    check_plan_artifact_set(expected, plan)?;
    check_plan_archives(expected, plan)?;
    check_plan_support_artifacts(expected, plan)
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

/// Runs the real `dist plan` for the tag naming `product`'s Cargo version and
/// parses the plan it prints, with the expectation that tag implies.
fn tagged_plan(
    workspace: &Path,
    product: &'static str,
) -> Result<(ExpectedRelease, TaggedPlan), Box<dyn Error>> {
    let expected = expected_release(workspace, product)?;
    let (planned, plan) = plan_for_tag(workspace, &expected.tag)?;
    if !planned {
        return Err(io::Error::other(format!(
            "{} names the Cargo version and must plan: {plan:?}",
            expected.tag
        ))
        .into());
    }
    // Decode through JSON text, as the wire client does: `sonic_rs::from_value`
    // rejects some serde shapes, and the plan is already parsed once.
    let text = sonic_rs::to_string(&plan)?;
    Ok((expected, sonic_rs::from_str(&text)?))
}

/// The release plan of each product, from the real `dist plan` for the tag
/// naming that product's Cargo version: it selects exactly that product's
/// release, with the six target archives and checksums, both installers, the
/// unified checksum, and the source archive. The same attack from the other
/// side: each plan fails the other product's expectation, and a live plan
/// missing an archive, or the source archive, fails the layout gate.
#[test]
fn release_dist_plan_validity() -> Result<(), Box<dyn Error>> {
    if !dist_ready()? {
        return Err(
            io::Error::other("cargo-dist is required for release_dist_plan_validity").into(),
        );
    }
    let workspace = support::repo_root();
    let (dal_expected, dal_plan) = tagged_plan(&workspace, "dalgon")?;
    let (dalgona_expected, dalgona_plan) = tagged_plan(&workspace, "dalgona")?;
    check_plan_topology(&dal_expected, &dal_plan)?;
    check_plan_topology(&dalgona_expected, &dalgona_plan)?;

    assert!(
        check_plan_topology(&dalgona_expected, &dal_plan).is_err(),
        "the dalgon tag's plan must not satisfy the dalgona release"
    );
    assert!(
        check_plan_topology(&dal_expected, &dalgona_plan).is_err(),
        "the dalgona tag's plan must not satisfy the dalgon release"
    );

    let mut missing_archive = dal_plan;
    missing_archive
        .artifacts
        .remove(&archive_name("dalgon", "x86_64-unknown-linux-gnu"));
    assert!(
        check_plan_topology(&dal_expected, &missing_archive).is_err(),
        "a plan missing an archive must fail the layout gate"
    );
    let mut missing_source = dalgona_plan;
    missing_source.artifacts.remove("source.tar.gz");
    assert!(
        check_plan_topology(&dalgona_expected, &missing_source).is_err(),
        "a plan without the source archive must fail the layout gate"
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

/// Runs the real `dist plan` for one announcement tag and returns its exit
/// success with the parsed JSON stdout: the plan on success, the diagnostic
/// on failure. The planner reads the workspace only: it runs without
/// `GH_TOKEN` or `GITHUB_TOKEN`, so it never authenticates against a host.
#[expect(
    clippy::disallowed_methods,
    reason = "the dist gate runs the real cargo-dist plan against announcement tags"
)]
fn plan_for_tag(workspace: &Path, tag: &str) -> Result<(bool, sonic_rs::Value), Box<dyn Error>> {
    let output = Command::new("dist")
        .args(["plan", "--output-format=json"])
        .arg(format!("--tag={tag}"))
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .current_dir(workspace)
        .output()?;
    Ok((
        output.status.success(),
        sonic_rs::from_slice(&output.stdout)?,
    ))
}

/// The version one patch release past `version`; the gate fixes its
/// mismatched tag one step from the workspace version, not at a far-off value.
fn next_patch(version: &str) -> Result<String, Box<dyn Error>> {
    let mut parts = version.split('.');
    let (Some(major), Some(minor), Some(patch), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(io::Error::other(format!("{version} is not a plain major.minor.patch")).into());
    };
    Ok(format!("{major}.{minor}.{}", patch.parse::<u64>()? + 1))
}

/// The announcement-tag boundary, attacked from both sides for each product:
/// the tag naming the workspace version plans, the tag one patch past it is
/// refused with a diagnostic naming the product, the claimed version, and the
/// real version. Runs the real `dist plan` rather than reading a recorded
/// diagnostic, so the gate sees `dist-workspace.toml` and the manifests.
#[test]
fn release_tag_version_mismatch() -> Result<(), Box<dyn Error>> {
    if !dist_ready()? {
        return Err(
            io::Error::other("cargo-dist is required for release_tag_version_mismatch").into(),
        );
    }
    let workspace = support::repo_root();
    for product in ["dalgon", "dalgona"] {
        let package = cargo_package(&workspace, product)?;
        let matching = format!("{product}-v{}", package.version);
        let (planned, plan) = plan_for_tag(&workspace, &matching)?;
        assert!(
            planned,
            "{matching} names the workspace version and must plan: {plan:?}"
        );
        assert_eq!(
            plan.get("announcement_tag")
                .and_then(sonic_rs::Value::as_str),
            Some(matching.as_str())
        );

        let bumped = next_patch(&package.version)?;
        let mismatched = format!("{product}-v{bumped}");
        let (refused, diagnostic) = plan_for_tag(&workspace, &mismatched)?;
        assert!(
            !refused,
            "{mismatched} is past the workspace version and must be refused"
        );
        let message = diagnostic
            .get("diagnostic")
            .and_then(|diagnostic| diagnostic.get("message"))
            .and_then(sonic_rs::Value::as_str)
            .ok_or_else(|| format!("no diagnostic message for {mismatched}: {diagnostic:?}"))?;
        assert!(
            message.contains(&format!("claims we're releasing {product} {bumped}")),
            "the diagnostic must name the product and claimed version: {message}"
        );
        assert!(
            message.contains(&format!("that package is version {}", package.version)),
            "the diagnostic must name the workspace version {}: {message}",
            package.version
        );
    }
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
