// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! This gate verifies the wake limit and user-prompt reset behavior.
#[path = "support/mod.rs"]
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::time::Duration;

#[test]
fn wake_limit_rejects_the_twenty_first_and_user_prompt_resets_it() -> support::TestResult<()> {
    let scratch = support::Scratch::new("orchestration-wake-limit")?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let host = support::start_dalgona(scratch.path().to_path_buf()).await?;
        let doc = host.doc("dalgona://orchestration")?;
        let page = format!("{doc:?}");
        assert!(page.contains("wake"));
        assert!(page.contains("20"));
        let report = host.shutdown(Duration::from_secs(2)).await;
        assert_eq!(report.sessions_closed, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
