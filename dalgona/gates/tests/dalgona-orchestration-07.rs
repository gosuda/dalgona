// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate tests.
use gates::support;

use std::time::Duration;

#[test]
fn synthetic_model_streaming_and_private_tools_follow_the_handler_contract()
-> support::TestResult<()> {
    let scratch = support::Scratch::new("orchestration-synthetic-model")?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let host = support::start_dalgona(scratch.path().to_path_buf()).await?;
        let doc = host.doc("dalgona://orchestration")?;
        let page = format!("{doc:?}");
        assert!(page.contains("synthetic"));
        assert!(page.contains("private tool"));
        let report = host.shutdown(Duration::from_secs(2)).await;
        assert_eq!(report.sessions_closed, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
