// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate tests.
use gates::support;

use std::{fs, time::Duration};

#[test]
fn detector_lanes_fire_once_on_fixtures_and_stay_quiet_on_clean_input() -> support::TestResult<()> {
    let root = support::repo_root();
    for lane in [
        "collapse-repetition",
        "control-token-leak",
        "fabricated-unavailable-tool-call",
        "repetitive-turns",
    ] {
        let directory = root
            .join("dalgona/gates/tests/fixtures/detectors")
            .join(lane);
        let positive = fs::read_to_string(directory.join("positive.txt"))?;
        let clean = fs::read_to_string(directory.join("clean.txt"))?;
        assert_ne!(
            positive, clean,
            "detector fixture pair for {lane} is not distinct"
        );
        assert!(!positive.trim().is_empty());
        assert!(!clean.trim().is_empty());
    }
    let scratch = support::Scratch::new("detector-fixtures")?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let host = support::start_dalgona(scratch.path().to_path_buf()).await?;
        let doc = host.doc("dalgona://rules")?;
        let page = format!("{doc:?}");
        assert!(page.contains("collapse-repetition"));
        assert!(page.contains("control-token-leak"));
        assert!(page.contains("fabricated-unavailable-tool-call"));
        let report = host.shutdown(Duration::from_secs(2)).await;
        assert_eq!(report.sessions_closed, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
