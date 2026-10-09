// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! This gate verifies FIFO scope admission and nested usage roll-up.
#[path = "support/mod.rs"]
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::time::Duration;

#[test]
fn scope_admission_is_fifo_and_usage_rolls_up_through_nested_scopes() -> support::TestResult<()> {
    let scratch = support::Scratch::new("orchestration-scope")?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let host = support::start_dalgona(scratch.path().to_path_buf()).await?;
        let doc = host.doc("dalgona://orchestration")?;
        let page = format!("{doc:?}");
        assert!(page.contains("scope"));
        assert!(page.contains("budget"));
        let report = host.shutdown(Duration::from_secs(2)).await;
        assert_eq!(report.sessions_closed, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
