// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate tests.
use dalgona_gates::support;

use std::collections::BTreeMap;
use std::path::Path;
use std::{fs, io, time::Duration};

fn body_after_heading<'a>(
    document: &'a str,
    expected_heading: &str,
) -> support::TestResult<&'a str> {
    let (heading, body) = document
        .split_once('\n')
        .ok_or_else(|| io::Error::other("document has no body"))?;
    if heading != expected_heading {
        return Err(io::Error::other(format!(
            "expected heading {expected_heading:?}, got {heading:?}"
        ))
        .into());
    }
    Ok(body)
}

fn collect_markdown(
    root: &Path,
    current: &Path,
    pages: &mut BTreeMap<String, String>,
) -> io::Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(io::Error::other(format!(
                "symbolic link in Dalgona manual mirror: {}",
                path.display()
            )));
        }
        if file_type.is_dir() {
            collect_markdown(root, &path, pages)?;
            continue;
        }
        if !file_type.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .map_err(io::Error::other)?
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        pages.insert(relative, fs::read_to_string(path)?);
    }
    Ok(())
}

#[test]
fn committed_mirror_matches_product_callback() -> support::TestResult<()> {
    let manual = (dalgona::product().docs)()
        .into_iter()
        .find(|manual| manual.scheme == "dalgona")
        .ok_or_else(|| io::Error::other("dalgona product did not publish its manual"))?;
    let expected: BTreeMap<_, _> = manual
        .pages
        .into_iter()
        .map(|(page, text)| (format!("{page}.md"), text))
        .collect();
    let root = support::repo_root().join("dalgona/docs");
    let mut actual = BTreeMap::new();
    collect_markdown(&root, &root, &mut actual)?;
    assert_eq!(actual, expected);
    Ok(())
}

#[test]
fn readme_philosophy_and_served_page_are_byte_equal() -> support::TestResult<()> {
    let root = support::repo_root();
    let readme = fs::read_to_string(root.join("dalgona/README.md"))?;
    let page = fs::read_to_string(root.join("dalgona/docs/philosophy.md"))?;
    let readme_body = body_after_heading(&readme, "# dalgona")?;
    let page_body = body_after_heading(&page, "# dalgona: dalgon with batteries")?;
    assert_eq!(readme_body.as_bytes(), page_body.as_bytes());

    let scratch = support::Scratch::new("philosophy-page")?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let host = support::start_dalgona(scratch.path().to_path_buf()).await?;
        let served = host.doc("dalgona://philosophy")?;
        assert!(format!("{served:?}").contains(&format!("{page:?}")));
        let report = host.shutdown(Duration::from_secs(2)).await;
        assert_eq!(report.sessions_closed, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
