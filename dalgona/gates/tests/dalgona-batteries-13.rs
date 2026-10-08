// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
#[path = "support/mod.rs"]
mod support;

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
