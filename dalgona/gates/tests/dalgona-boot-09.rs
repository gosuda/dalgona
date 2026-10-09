// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! This gate verifies bundled Rust batteries pass the public builder.
#[path = "support/mod.rs"]
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::time::Duration;

#[test]
fn bundled_rust_batteries_pass_the_public_builder() -> support::TestResult<()> {
    let scratch = support::Scratch::new("bundled-plugin-builder")?;
    let root = scratch.path().to_path_buf();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let host = support::start_dalgona(root).await?;
        for uri in [
            "dalgona://batteries",
            "dalgona://config",
            "dalgona://rules",
            "skill://find-anything",
            "skill://delegate-with-contracts",
            "skill://initializer-and-sprints",
        ] {
            let doc = host.doc(uri)?;
            assert!(
                !format!("{doc:?}").is_empty(),
                "the public builder left {uri} unresolved"
            );
        }
        let report = host.shutdown(Duration::from_secs(2)).await;
        assert_eq!(report.sessions_closed, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
