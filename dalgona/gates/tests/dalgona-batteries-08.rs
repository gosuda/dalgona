// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! This gate verifies MCP stdio tools are listed as deferred.
#[path = "support/mod.rs"]
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::time::Duration;

#[test]
fn mcp_stdio_tools_are_listed_as_deferred() -> support::TestResult<()> {
    let scratch = support::Scratch::new("mcp-deferred")?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let host = support::start_dalgona(scratch.path().to_path_buf()).await?;
        let doc = host.doc("dalgona://mcp")?;
        let page = format!("{doc:?}");
        assert!(page.contains("deferred"));
        let report = host.shutdown(Duration::from_secs(2)).await;
        assert_eq!(report.sessions_closed, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
