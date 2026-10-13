// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! The product defaults carry the literal key partition the schema reads.
use dalgona_gates::support;

use dal_core::{Config, ConfigProduct};

#[test]
fn product_defaults_match_the_literal_key_partition() -> support::TestResult<()> {
    let scratch = support::Scratch::new("product-default-layer")?;
    let root = scratch.path().to_path_buf();
    let factory = dalgona::product();
    let table: toml::Table = factory.defaults.parse()?;
    // The partition the builder reads: search, style, guard, battery layers.
    assert_eq!(
        table.get("search_symbols").and_then(toml::Value::as_bool),
        Some(true),
        "defaults must enable symbol search"
    );
    assert_eq!(
        table.get("edit_style").and_then(|value| value.as_str()),
        Some("hashline"),
        "defaults must select the hashline dialect"
    );
    let guard = table
        .get("guard")
        .and_then(|value| value.as_table())
        .ok_or("defaults must carry [guard]")?;
    assert_eq!(
        guard.get("enabled").and_then(toml::Value::as_bool),
        Some(true),
        "defaults must enable the guard"
    );
    assert_eq!(
        table.get("disabled_batteries"),
        Some(&toml::Value::Array(Vec::new())),
        "defaults must start with no disabled battery"
    );
    // The defaults load cleanly; an unknown key is rejected, so stray
    // substrings can never pass as the partition.
    Config::load(ConfigProduct::Dalgona, &root, factory.defaults, None)?;
    let unknown = Config::load(
        ConfigProduct::Dalgona,
        &root,
        factory.defaults,
        Some("no_such_partition_key = true\n"),
    );
    assert!(
        unknown.is_err(),
        "an unknown key must not load silently beside the partition"
    );
    Ok(())
}
