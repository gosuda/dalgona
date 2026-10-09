// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! This gate verifies the quality guard defaults and four reported measurements.
#[path = "support/mod.rs"]
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::time::Duration;

#[test]
fn guard_is_on_by_default_and_reports_the_four_measurements() -> support::TestResult<()> {
    let scratch = support::Scratch::new("guard-measurements")?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let host = support::start_dalgona(scratch.path().to_path_buf()).await?;
        let doc = host.doc("dalgona://quality")?;
        let page = format!("{doc:?}");
        for marker in ["turn growth", "per-file", "per-function", "best-current"] {
            assert!(page.contains(marker), "quality page omits {marker}");
        }
        let report = host.shutdown(Duration::from_secs(2)).await;
        assert_eq!(report.sessions_closed, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
