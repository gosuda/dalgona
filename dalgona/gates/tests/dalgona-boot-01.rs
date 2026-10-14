// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate tests.

use dal_core::Origin;
use dalgona_gates::support;
use std::{collections::BTreeSet, time::Duration};

#[test]
fn product_registers_all_eleven_batteries() -> support::TestResult<()> {
    let scratch = support::Scratch::new("dalgona-battery-set")?;
    let data_root = scratch.path().to_path_buf();
    let product = support::build_product(data_root.clone(), None)?;
    let battery_names = support::battery_names(&product);
    assert_eq!(battery_names, BTreeSet::from(support::BATTERIES));
    assert!(
        product.bundled.is_empty(),
        "no Starlark source ships in the product"
    );
    for extension in &product.extensions {
        if support::BATTERIES.contains(&extension.name()) {
            assert_eq!(extension.origin(), Origin::Bundled, "{}", extension.name());
        }
        if extension.name() == "dalgona" {
            assert_eq!(extension.origin(), Origin::Builtin, "manual docs origin");
        }
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let host = support::start_dalgona(data_root).await?;
        let page = host.doc("dalgona://batteries")?;
        let view = format!("{page:?}");
        for name in support::BATTERIES {
            assert!(view.contains(name), "battery inventory omits {name}");
        }
        let report = host.shutdown(Duration::from_secs(2)).await;
        assert_eq!(report.sessions_closed, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
