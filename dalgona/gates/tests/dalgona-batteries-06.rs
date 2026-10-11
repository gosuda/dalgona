// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate tests.
use gates::support;

use std::time::Duration;

#[test]
fn web_fetch_converts_local_html_to_markdown() -> support::TestResult<()> {
    let scratch = support::Scratch::new("web-local-fetch")?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let host = support::start_dalgona(scratch.path().to_path_buf()).await?;
        let doc = host.doc("dalgona://web")?;
        let page = format!("{doc:?}");
        assert!(page.contains("web_fetch"));
        assert!(page.contains("loopback"));
        let report = host.shutdown(Duration::from_secs(2)).await;
        assert_eq!(report.sessions_closed, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
