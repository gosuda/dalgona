// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate tests.
#![expect(
    clippy::disallowed_methods,
    reason = "gate runs the real product binaries"
)]
use dalgona_gates::support;
use sonic_rs::{JsonContainerTrait, JsonValueTrait};

use std::process::Command;

#[test]
fn binstall_templates_expand_to_dist_artifacts() -> support::TestResult<()> {
    let workspace = support::repo_root();
    let metadata_output = support::run_command(
        Command::new("cargo")
            .args(["metadata", "--no-deps", "--format-version=1", "--locked"])
            .current_dir(&workspace),
    )?;
    assert!(
        metadata_output.status.success(),
        "{}",
        String::from_utf8_lossy(&metadata_output.stderr)
    );
    let metadata: sonic_rs::Value = sonic_rs::from_slice(&metadata_output.stdout)?;
    let package = metadata
        .get("packages")
        .and_then(sonic_rs::Value::as_array)
        .and_then(|packages| {
            packages.iter().find(|package| {
                package.get("name").and_then(sonic_rs::Value::as_str) == Some("dalgona")
            })
        })
        .ok_or("Cargo metadata omitted dalgona")?;
    let version = package
        .get("version")
        .and_then(sonic_rs::Value::as_str)
        .ok_or("dalgona has no version")?;
    let tag = format!("dalgona-v{version}");
    let output = support::run_command(
        Command::new("dist")
            .args(["plan", "--tag", &tag, "--output-format=json"])
            .current_dir(&workspace),
    )?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let plan: sonic_rs::Value = sonic_rs::from_slice(&output.stdout)?;
    assert_eq!(
        plan.get("announcement_tag")
            .and_then(sonic_rs::Value::as_str),
        Some(tag.as_str())
    );
    let releases = plan
        .get("releases")
        .and_then(sonic_rs::Value::as_array)
        .ok_or("dist plan omitted releases")?;
    assert_eq!(releases.len(), 1, "the tag must select only dalgona");
    assert_eq!(
        releases[0]
            .get("app_name")
            .and_then(sonic_rs::Value::as_str),
        Some("dalgona")
    );
    let download_path = releases[0]
        .get("hosting")
        .and_then(|hosting| hosting.get("github"))
        .and_then(|github| github.get("artifact_download_path"))
        .and_then(sonic_rs::Value::as_str)
        .ok_or("dist plan omitted the product release download path")?;
    assert!(download_path.ends_with(&format!("/releases/download/{tag}")));
    assert_binstall_templates(&plan, package)?;
    Ok(())
}

fn assert_binstall_templates(
    plan: &sonic_rs::Value,
    package: &sonic_rs::Value,
) -> support::TestResult<()> {
    let version = package
        .get("version")
        .and_then(sonic_rs::Value::as_str)
        .ok_or("dalgona has no version")?;
    let repository = package
        .get("repository")
        .and_then(sonic_rs::Value::as_str)
        .ok_or("dalgona has no repository")?;
    let binstall = package
        .get("metadata")
        .and_then(|metadata| metadata.get("binstall"))
        .ok_or("dalgona has no binstall metadata")?;
    let artifacts = plan.get("artifacts").ok_or("dist plan omitted artifacts")?;
    for target in [
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "x86_64-apple-darwin",
        "aarch64-apple-darwin",
        "x86_64-pc-windows-msvc",
        "aarch64-pc-windows-msvc",
    ] {
        let windows = target.ends_with("windows-msvc");
        let config = if windows {
            binstall
                .get("overrides")
                .and_then(|overrides| overrides.get("cfg(windows)"))
                .ok_or("dalgona has no Windows binstall override")?
        } else {
            binstall
        };
        let extension = if windows { "zip" } else { "tar.xz" };
        let archive = format!("dalgona-{target}.{extension}");
        let artifact = artifacts.get(&archive).ok_or("dist plan omitted archive")?;
        let url = config
            .get("pkg-url")
            .and_then(sonic_rs::Value::as_str)
            .ok_or("binstall has no package URL")?
            .replace("{ repo }", repository)
            .replace("{ version }", version)
            .replace("{ name }", "dalgona")
            .replace("{ target }", target);
        assert_eq!(
            url,
            format!("{repository}/releases/download/dalgona-v{version}/{archive}")
        );
        assert_eq!(
            config.get("pkg-fmt").and_then(sonic_rs::Value::as_str),
            Some(if windows { "zip" } else { "txz" })
        );
        let bin_template = config
            .get("bin-dir")
            .and_then(sonic_rs::Value::as_str)
            .ok_or("binstall has no binary path")?;
        let assets = artifact
            .get("assets")
            .and_then(sonic_rs::Value::as_array)
            .ok_or("dist archive omitted assets")?;
        for binary in ["dalgona", "dg"] {
            let filename = format!("{binary}{}", if windows { ".exe" } else { "" });
            assert!(assets.iter().any(|asset| {
                asset.get("path").and_then(sonic_rs::Value::as_str) == Some(filename.as_str())
            }));
            let binary_path = bin_template
                .replace("{ name }", "dalgona")
                .replace("{ target }", target)
                .replace("{ bin }", binary)
                .replace("{ binary-ext }", if windows { ".exe" } else { "" });
            assert_eq!(
                binary_path,
                if windows {
                    filename
                } else {
                    format!("dalgona-{target}/{filename}")
                }
            );
        }
    }
    Ok(())
}
