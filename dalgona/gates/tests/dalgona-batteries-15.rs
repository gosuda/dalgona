// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! The served config page documents every product-level key the live product
//! reads; battery tables live on their battery pages.
use dalgona_gates::support;

use std::time::Duration;

use dal_core::{Config, ConfigProduct};

/// Product-level schema with no defaults entry: the builder still reads it.
const SCHEMA_KEYS: [&str; 2] = ["guard.enabled", "rule_sets.enabled"];

/// Backticked segments that read as dotted keys rather than values.
fn documented_keys(page: &str) -> Vec<String> {
    let mut keys = Vec::new();
    for (index, segment) in page.split('`').enumerate() {
        if index % 2 == 0 {
            continue;
        }
        let key = segment.trim();
        if !key.is_empty()
            && key.chars().all(|cell| {
                cell.is_ascii_lowercase() || matches!(cell, '_' | '-' | '.' | '[' | ']')
            })
            && key
                .chars()
                .any(|cell| matches!(cell, '.' | '_' | '-' | '[' | ']'))
        {
            keys.push(key.replace(['[', ']'], ""));
        }
    }
    keys
}

/// Whether a documented dotted key resolves against the live schema: a
/// defaults path, or the schema-only product switch.
fn resolves(table: &toml::Table, key: &str) -> bool {
    if SCHEMA_KEYS.contains(&key) {
        return true;
    }
    let mut node: Option<&toml::Value> = None;
    for (depth, segment) in key.split('.').enumerate() {
        node = if depth == 0 {
            table.get(segment)
        } else {
            node.and_then(|value| value.as_table())
                .and_then(|nested| nested.get(segment))
        };
    }
    node.is_some()
}

#[test]
fn config_page_matches_every_product_key() -> support::TestResult<()> {
    let scratch = support::Scratch::new("config-page")?;
    let root = scratch.path().to_path_buf();
    let factory = dalgona::product();
    let table: toml::Table = factory.defaults.parse()?;
    // Product-level surface: top-level scalars plus the guard switch.
    let mut required: Vec<String> = table
        .iter()
        .filter(|(_, value)| !value.is_table())
        .map(|(key, _)| key.clone())
        .collect();
    required.extend(SCHEMA_KEYS.iter().map(|key| (*key).to_owned()));
    assert!(!required.is_empty(), "the product defaults carry no keys");
    // The required keys load through the real builder.
    Config::load(ConfigProduct::Dalgona, &root, factory.defaults, None)?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let host = support::start_dalgona(scratch.path().to_path_buf()).await?;
        let served = host.doc("dalgona://config")?;
        let page = format!("{served:?}").replace(['[', ']'], "");
        // A product-level key added without documentation fails here by name.
        for key in &required {
            assert!(
                page.contains(key),
                "served config page omits product key {key}"
            );
        }
        // No documented key is stale: each resolves to the live schema.
        for key in documented_keys(&page) {
            assert!(
                resolves(&table, &key),
                "served config page documents stale key {key}"
            );
        }
        let report = host.shutdown(Duration::from_secs(2)).await;
        assert_eq!(report.sessions_closed, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
