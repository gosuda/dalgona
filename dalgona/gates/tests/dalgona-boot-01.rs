// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
#[path = "support/mod.rs"]
mod support;

use dal_core::{Config, ConfigProduct, Origin};
use std::{collections::BTreeSet, time::Duration};

#[test]
fn product_registers_all_eleven_batteries() -> support::TestResult<()> {
    let scratch = support::Scratch::new("dalgona-battery-set")?;
    let data_root = scratch.path().to_path_buf();
    let factory = dalgona::product();
    let config = Config::load(ConfigProduct::Dalgona, &data_root, factory.defaults, None)?;
    let cx = dalgon::BuildCx {
        data_root: data_root.clone(),
        config: &config,
    };
    let product = dalgona::build(&cx)?;
    const BATTERIES: [&str; 11] = [
        "ask",
        "history",
        "judged",
        "mcp",
        "orchestration",
        "quality",
        "review",
        "skills",
        "ttsr-rules",
        "web",
        "work",
    ];
    let battery_names: BTreeSet<_> = product
        .extensions
        .iter()
        .map(|extension| extension.name())
        .filter(|name| BATTERIES.contains(name))
        .collect();
    assert_eq!(battery_names, BTreeSet::from(BATTERIES));
    assert!(
        product.bundled.is_empty(),
        "no Starlark source ships in the product"
    );
    for extension in &product.extensions {
        if BATTERIES.contains(&extension.name()) {
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
        for name in BATTERIES {
            assert!(view.contains(name), "battery inventory omits {name}");
        }
        let report = host.shutdown(Duration::from_secs(2)).await;
        assert_eq!(report.sessions_closed, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
