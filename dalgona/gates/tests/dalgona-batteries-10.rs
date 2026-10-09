// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate tests.
use gates::support;

use dal_core::{Config, ConfigProduct};

#[test]
fn product_defaults_match_the_literal_key_partition() -> support::TestResult<()> {
    let scratch = support::Scratch::new("product-default-layer")?;
    let root = scratch.path().to_path_buf();
    let factory = dalgona::product();
    assert!(factory.defaults.contains("search_symbols = true"));
    assert!(factory.defaults.contains("edit_style = \"hashline\""));
    Config::load(ConfigProduct::Dalgona, &root, factory.defaults, None)?;
    Config::load(
        ConfigProduct::Dalgona,
        &root,
        factory.defaults,
        Some("search_symbols = true\nedit_style = \"hashline\"\n[guard]\nenabled = true\n"),
    )?;
    Ok(())
}
