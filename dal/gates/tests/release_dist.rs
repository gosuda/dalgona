//! Distribution layout gate: packaged binaries and manifest artifacts.
use std::{
    collections::{BTreeMap, HashSet},
    error::Error,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

use serde::Deserialize;

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
    metadata: Option<PackageMetadata>,
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

const MAN_PAGES: [&str; 19] = [
    "man/dalgon.1",
    "man/dalgon-login.1",
    "man/dalgon-login-status.1",
    "man/dalgon-logout.1",
    "man/dalgon-models.1",
    "man/dalgon-docs.1",
    "man/dalgon-serve.1",
    "man/dalgon-serve-token.1",
    "man/dalgon-rpc.1",
    "man/dalgon-acp.1",
    "man/dalgon-plugin.1",
    "man/dalgon-plugin-grant.1",
    "man/dalgon-plugin-revoke.1",
    "man/dalgon-plugin-list.1",
    "man/dalgon-rules.1",
    "man/dalgon-rules-test.1",
    "man/dalgon-completion.1",
    "man/dalgon-app-server.1",
    "man/dalgon-__sandbox.1",
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn dal_root() -> PathBuf {
    repo_root().join("dal")
}

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
    if !output.status.success() || !version.contains("0.32.0") {
        return Err(
            io::Error::other(format!("expected cargo-dist 0.32.0, received {version:?}")).into(),
        );
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
fn load_plan(workspace: &Path, what: &str) -> Result<Plan, Box<dyn Error>> {
    let output = Command::new("dist")
        .args(["plan", "--output-format=json"])
        .current_dir(workspace)
        .output()?;
    if !output.status.success() {
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        if text.contains("no packages in your workspace with distable binaries") {
            eprintln!("skipped {what}: no distable binaries yet");
            return Ok(Plan::Skip);
        }
        if text.contains("no matching package named") {
            eprintln!("blocked {what}: dal crates are unpublished on crates.io");
            return Ok(Plan::Skip);
        }
        return Err(io::Error::other(text).into());
    }
    Ok(Plan::Ready(sonic_rs::from_slice(&output.stdout)?))
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

fn expand_template(template: &str, version: &str, product: &str, target: &str) -> String {
    template
        .replace("{ repo }", "https://github.com/gosuda/dalgona")
        .replace("{ version }", version)
        .replace("{ name }", product)
        .replace("{ target }", target)
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
        assert!(artifacts.artifacts.contains_key(&archive));
        assert_eq!(format, if is_windows { "zip" } else { "txz" });
        let url = expand_template(url_template, &package.version, product, target);
        assert!(
            url.ends_with(&format!(
                "/releases/download/{tag_prefix}v{}/{archive}",
                package.version
            )),
            "binstall URL did not select its release archive: {url}"
        );
        let bin_dir = bin_template
            .replace("{ name }", product)
            .replace("{ target }", target);
        assert_eq!(
            bin_dir,
            if is_windows {
                "{ bin }".to_owned()
            } else {
                format!("{product}-{target}/{{ bin }}")
            }
        );
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
    if !dist_ready()? {
        eprintln!("skipped release_dist_plan_validity: cargo-dist 0.32.0 is not installed");
        return Ok(());
    }
    let repo = repo_root();
    let dal = match load_plan(&dal_root(), "release_dist_plan_validity dalgon")? {
        Plan::Ready(manifest) => manifest,
        Plan::Skip => return Ok(()),
    };
    assert_manifest_artifacts("dalgon", &dal);
    match load_plan(&repo.join("dalgona"), "release_dist_plan_validity dalgona")? {
        Plan::Ready(dalgona) => assert_manifest_artifacts("dalgona", &dalgona),
        Plan::Skip => {}
    }
    Ok(())
}

#[test]
fn release_binstall_url_expansion() -> Result<(), Box<dyn Error>> {
    if !dist_ready()? {
        eprintln!("skipped release_binstall_url_expansion: cargo-dist 0.32.0 is not installed");
        return Ok(());
    }
    let repo = repo_root();
    let dal = match load_plan(&dal_root(), "release_binstall_url_expansion dalgon")? {
        Plan::Ready(manifest) => manifest,
        Plan::Skip => return Ok(()),
    };
    assert_binstall_urls(&dal_root(), "dalgon", "dalgon-", &dal)?;
    match cargo_package(&repo.join("dalgona"), "dalgona") {
        Ok(package)
            if package
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.binstall.as_ref())
                .is_some() =>
        {
            let dalgona_manifest = match load_plan(
                &repo.join("dalgona"),
                "release_binstall_url_expansion dalgona",
            )? {
                Plan::Ready(manifest) => manifest,
                Plan::Skip => return Ok(()),
            };
            assert_binstall_urls(
                &repo.join("dalgona"),
                "dalgona",
                "dalgona-",
                &dalgona_manifest,
            )?;
        }
        Ok(_) => eprintln!(
            "skipped dalgona binstall expansion: dalgona binstall metadata is not in v0 scope"
        ),
        Err(error) if error.to_string().contains("no matching package named") => {
            eprintln!(
                "blocked release_binstall_url_expansion dalgona: dal crates are unpublished on crates.io"
            );
        }
        Err(error) => return Err(error),
    }
    Ok(())
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "the dist gate shells out to the real cargo-dist binary"
)]
fn release_tag_version_mismatch() -> Result<(), Box<dyn Error>> {
    if !dist_ready()? {
        eprintln!("skipped release_tag_version_mismatch: cargo-dist 0.32.0 is not installed");
        return Ok(());
    }
    let output = Command::new("dist")
        .arg("plan")
        .arg("--tag=dalgon-v9.9.9")
        .current_dir(dal_root())
        .output()?;
    let stderr = String::from_utf8(output.stderr)?;
    assert!(!output.status.success());
    assert!(
        stderr.contains("claims we're releasing dalgon 9.9.9"),
        "unexpected tag diagnostics: {stderr}"
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
        eprintln!("skipped release_archive_members_exact: cargo-dist 0.32.0 is not installed");
        return Ok(());
    }
    let workspace = dal_root();
    let manifest = match load_plan(&workspace, "release_archive_members_exact dalgon")? {
        Plan::Ready(manifest) => manifest,
        Plan::Skip => return Ok(()),
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
