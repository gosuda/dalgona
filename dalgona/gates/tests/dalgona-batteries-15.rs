// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
// Anchor: Tony Hoare, Richard Feynman
//! The served config page documents every product-level key the live product
//! reads; battery tables live on their battery pages.
use dalgona_gates::support;

use std::{collections::BTreeSet, time::Duration};

use dal_core::{Config, ConfigProduct};

/// Product-level schema with no defaults entry: the builder still reads it.
const SCHEMA_KEYS: [&str; 2] = ["guard.enabled", "rule_sets.enabled"];

/// Parses a Markdown table row into trimmed cell slices when enclosed in pipes.
fn parse_table_row_cells(line: &str) -> Option<Vec<&str>> {
    let trimmed = line.trim();
    if !trimmed.starts_with('|') || !trimmed.ends_with('|') {
        return None;
    }
    let inner = &trimmed[1..trimmed.len() - 1];
    let cells: Vec<&str> = inner.split('|').map(str::trim).collect();
    if cells.iter().any(|cell| cell.starts_with("---")) {
        return None;
    }
    Some(cells)
}

/// Cleans a standalone backticked cell, stripping surrounding backticks and
/// normalizing away brackets (`[tui].diagrams` -> `tui.diagrams`).
fn clean_field_key(cell: &str) -> Option<String> {
    let trimmed = cell.trim();
    if !trimmed.starts_with('`') || !trimmed.ends_with('`') || trimmed.len() < 2 {
        return None;
    }
    let raw = &trimmed[1..trimmed.len() - 1];
    let cleaned = raw.replace(['[', ']'], "").trim().to_string();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// Extracts declared field keys from Markdown table rows only:
/// - 3-cell rows: default comparison table (key in column 0)
/// - 4-cell rows: picker reference table (key in column 1)
fn extract_table_keys(page: &str) -> (Vec<String>, Vec<String>) {
    let mut defaults_keys = Vec::new();
    let mut picker_keys = Vec::new();
    for line in page.lines() {
        let Some(cells) = parse_table_row_cells(line) else {
            continue;
        };
        let key_cell = match cells.len() {
            3 => cells.first(),
            4 => cells.get(1),
            _ => None,
        };
        let Some(raw_cell) = key_cell else {
            continue;
        };
        let Some(key) = clean_field_key(raw_cell) else {
            continue;
        };
        if cells.len() == 3 {
            defaults_keys.push(key);
        } else {
            picker_keys.push(key);
        }
    }
    (defaults_keys, picker_keys)
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
            node.and_then(toml::Value::as_table)
                .and_then(|nested| nested.get(segment))
        };
    }
    node.is_some()
}

#[test]
fn pure_boundary_probe_rejects_prose_and_detects_stale_table_keys()
-> Result<(), Box<dyn std::error::Error>> {
    let mock_doc = "\
# Settings

Prose mentioning `plugin.orchestration.workflows` or `search_symbols` outside tables.

| key | dal default | dalgona default |
|---|---|---|
| `search_symbols` | `false` | `true` |
| `[guard].enabled` | `false` | `true` |
| `stale.defaults.key` | none | `x` |

### Allowlisted settings rows

Prose with `plugin.web.api_key_env` excluded from picker.

| Group | Key | Control | Description |
|---|---|---|---|
| `search` | `search_symbols` | toggle | Symbol search |
| `mcp` | `unknown.misspelled.key` | toggle | Misspelled |
";
    let (def_keys, pick_keys) = extract_table_keys(mock_doc);

    // Prose backticks must never become declared keys.
    assert!(!def_keys.contains(&"plugin.orchestration.workflows".to_string()));
    assert!(!pick_keys.contains(&"plugin.orchestration.workflows".to_string()));
    assert!(!pick_keys.contains(&"plugin.web.api_key_env".to_string()));

    // Standalone table cells are extracted correctly.
    assert_eq!(
        def_keys,
        vec!["search_symbols", "guard.enabled", "stale.defaults.key"]
    );
    assert_eq!(pick_keys, vec!["search_symbols", "unknown.misspelled.key"]);

    // Stale or misspelled keys fail resolution.
    let factory = dalgona::product();
    let table: toml::Table = factory.defaults.parse()?;
    assert!(!resolves(&table, "stale.defaults.key"));
    Ok(())
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

    // The required keys load through the real builder and populate the catalog.
    let config = Config::load(ConfigProduct::Dalgona, &root, factory.defaults, None)?;
    let catalog = (factory.settings)(&config)?;
    let catalog_keys: BTreeSet<String> = catalog
        .into_iter()
        .map(|item| item.key.to_string())
        .collect();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let host = support::start_dalgona(scratch.path().to_path_buf()).await?;
        let served = host.doc("dalgona://config")?;
        let page = &served.text;

        let (defaults_keys, picker_keys) = extract_table_keys(page);

        // Every product-level required key appears in the defaults comparison table.
        for key in &required {
            assert!(
                defaults_keys.contains(key),
                "served config page defaults table omits product key {key}"
            );
        }

        // Defaults table keys must resolve against defaults or schema.
        for key in &defaults_keys {
            assert!(
                resolves(&table, key),
                "served config page defaults table documents stale key {key}"
            );
        }

        // Declared picker keys match the live catalog exactly.
        let declared_picker_set: BTreeSet<String> = picker_keys.into_iter().collect();
        assert_eq!(
            declared_picker_set, catalog_keys,
            "served picker table declared keys differ from live product settings catalog"
        );

        let report = host.shutdown(Duration::from_secs(2)).await;
        assert_eq!(report.sessions_closed, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
