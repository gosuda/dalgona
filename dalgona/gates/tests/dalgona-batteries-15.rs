// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate tests.
use gates::support;

use std::{fs, time::Duration};

#[test]
fn config_page_matches_every_product_key() -> support::TestResult<()> {
    let root = support::repo_root();
    let page = fs::read_to_string(root.join("dalgona/crates/dalgona/src/docs/config.md"))?;
    for key in [
        "guard",
        "search_symbols",
        "edit_style",
        "disabled_batteries",
        "experimental_batteries",
        "rule_sets",
    ] {
        assert!(page.contains(key), "config page omits {key}");
    }
    let scratch = support::Scratch::new("config-page")?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let host = support::start_dalgona(scratch.path().to_path_buf()).await?;
        let served = host.doc("dalgona://config")?;
        assert!(format!("{served:?}").contains(&format!("{page:?}")));
        let report = host.shutdown(Duration::from_secs(2)).await;
        assert_eq!(report.sessions_closed, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
