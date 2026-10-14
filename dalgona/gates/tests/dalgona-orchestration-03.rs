// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate: every checked orchestration constant the
//! code enforces appears verbatim in the live `dalgona://orchestration`
//! page. The rows are read from the battery's code, not from the page.
use dalgona_gates::support;

use std::time::Duration;

#[test]
fn constants_page_states_the_enforced_values() -> support::TestResult<()> {
    let scratch = support::Scratch::new("orchestration-constants")?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let host = support::start_dalgona(scratch.path().to_path_buf()).await?;
        let doc = host.doc("dalgona://orchestration")?;
        let page = &*doc.text;
        for (name, value) in dalgona_batteries::orchestration::constant_pairs() {
            let row = format!("| {name} | {value} |");
            assert!(
                page.contains(&row),
                "orchestration page omits or contradicts constant {name:?} = {value:?}"
            );
        }
        let report = host.shutdown(Duration::from_secs(2)).await;
        assert_eq!(report.tasks_remaining, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
