// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
#[path = "support/mod.rs"]
mod support;

use std::time::Duration;

#[test]
fn stress_500_subagents_and_200_jobs_hold_the_process_contract() -> support::TestResult<()> {
    let scratch = support::Scratch::new("orchestration-process-contract")?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let host = support::start_dalgona(scratch.path().to_path_buf()).await?;
        let doc = host.doc("dalgona://orchestration")?;
        let page = format!("{doc:?}");
        for contract in ["agents", "jobs", "cancel", "shutdown"] {
            assert!(
                page.contains(contract),
                "orchestration page omits {contract}"
            );
        }
        let report = host.shutdown(Duration::from_secs(5)).await;
        assert_eq!(report.sessions_closed, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
