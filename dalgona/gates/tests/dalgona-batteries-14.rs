// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! This gate verifies the batteries page matches the registry and configuration.
#[path = "support/mod.rs"]
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{fs, time::Duration};

#[test]
fn batteries_page_matches_registry_and_config() -> support::TestResult<()> {
    let root = support::repo_root();
    let page = fs::read_to_string(root.join("dalgona/crates/dalgona/src/docs/batteries.md"))?;
    let scratch = support::Scratch::new("batteries-page")?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let host = support::start_dalgona(scratch.path().to_path_buf()).await?;
        let served = host.doc("dalgona://batteries")?;
        assert!(format!("{served:?}").contains(&format!("{page:?}")));
        for name in [
            "ask",
            "history",
            "judged",
            "mcp",
            "orchestration",
            "quality",
            "review",
            "skills",
            "ttsr-rules",
            "web",
            "work",
            "dalgona",
        ] {
            assert!(page.contains(name), "batteries page omits {name}");
        }
        let report = host.shutdown(Duration::from_secs(2)).await;
        assert_eq!(report.sessions_closed, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
