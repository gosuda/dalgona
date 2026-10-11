// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate tests.
use gates::support;

use std::collections::BTreeSet;

use dal_core::{Config, ConfigProduct};

#[test]
fn defaults_reach_patch_search_and_guard() -> support::TestResult<()> {
    let scratch = support::Scratch::new("defaults-reach-tools")?;
    let root = scratch.path().to_path_buf();
    let default_product = support::build_product(root.clone(), None)?;
    let overridden = support::build_product(
        root.clone(),
        Some("search_symbols = false\nedit_style = \"anchor\"\n"),
    )?;
    assert_eq!(default_product.name, "dalgona");
    assert_eq!(overridden.name, "dalgona");
    let extension_names = |product: &dal_agent::Product| -> BTreeSet<String> {
        product
            .extensions
            .iter()
            .map(|extension| extension.name().to_owned())
            .collect()
    };
    assert_eq!(
        extension_names(&default_product),
        extension_names(&overridden)
    );
    let factory = dalgona::product();
    assert!(factory.defaults.contains("edit_style = \"hashline\""));
    assert!(
        Config::load(
            ConfigProduct::Dalgona,
            &root,
            factory.defaults,
            Some("search_symbols = false\nedit_style = \"anchor\"\n")
        )
        .is_ok()
    );
    Ok(())
}
