// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! The served batteries page names the live extension registry.
use dalgona_gates::support;

use std::time::Duration;

#[test]
fn batteries_page_matches_registry_and_config() -> support::TestResult<()> {
    let scratch = support::Scratch::new("batteries-page")?;
    let product = support::build_product(scratch.path().to_path_buf(), None)?;
    let registry: Vec<String> = support::battery_names(&product)
        .into_iter()
        .map(str::to_owned)
        .collect();
    // The live registry is exactly the bundled battery set: dropping one
    // from the product fails here even when the markdown still names it.
    assert_eq!(registry.len(), support::BATTERIES.len());
    for name in support::BATTERIES {
        assert!(
            registry.iter().any(|registered| registered == name),
            "live registry omits {name}"
        );
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let host = support::start_dalgona(scratch.path().to_path_buf()).await?;
        let served = host.doc("dalgona://batteries")?;
        let page = format!("{served:?}");
        // Every registered battery is documented on the served page, and the
        // page belongs to the dalgona product itself.
        for name in &registry {
            assert!(page.contains(name), "served page omits live battery {name}");
        }
        assert!(page.contains("dalgona"), "served page names no product");
        let report = host.shutdown(Duration::from_secs(2)).await;
        assert_eq!(report.sessions_closed, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
