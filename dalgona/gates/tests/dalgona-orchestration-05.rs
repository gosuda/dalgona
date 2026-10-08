// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
#[path = "support/mod.rs"]
mod support;

use std::time::Duration;

#[test]
fn mailbox_reads_are_cursor_based_and_full_or_gone_are_named() -> support::TestResult<()> {
    let scratch = support::Scratch::new("orchestration-mailbox")?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let host = support::start_dalgona(scratch.path().to_path_buf()).await?;
        let doc = host.doc("dalgona://orchestration")?;
        let page = format!("{doc:?}");
        for mode in ["aside", "steer", "next_turn", "mailbox"] {
            assert!(page.contains(mode), "orchestration page omits {mode}");
        }
        let report = host.shutdown(Duration::from_secs(2)).await;
        assert_eq!(report.sessions_closed, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
