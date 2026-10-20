//! Plugin limits tests.

use super::super::{Config, ConfigError, ConfigProduct};
use super::{DATA_ROOT, load};
use crate::config::PluginLimits;
use std::path::Path;

#[test]
fn plugin_limits_default_to_engine_constants() {
    let config = load(ConfigProduct::Dalgon, "").unwrap();
    assert_eq!(
        config.plugin_limits(),
        &crate::config::PluginLimits::default()
    );
    assert_eq!(config.plugin_limits().load_ticks, 1_000_000);
    assert_eq!(config.plugin_limits().stack_depth, 100);
    let custom = load(
        ConfigProduct::Dalgon,
        "[limits.plugins]\nload_ticks = 5\nstack_depth = 50\n",
    )
    .unwrap();
    assert_eq!(custom.plugin_limits().load_ticks, 5);
    assert_eq!(custom.plugin_limits().stack_depth, 50);
    assert_eq!(custom.plugin_limits().handler_ticks, 200_000,);
}

#[test]
fn every_unset_limit_keeps_its_literal_engine_default() {
    let literal = PluginLimits {
        load_ticks: 1_000_000,
        load_heap_bytes: 16_777_216,
        handler_ticks: 200_000,
        handler_heap_bytes: 8_388_608,
        cell_ticks: 2_000_000,
        cell_heap_bytes: 33_554_432,
        stack_depth: 100,
    };
    assert_eq!(PluginLimits::default(), literal);
    // A table that names one key must leave the other six at their defaults:
    // the parser repeats each default, so each one is checked on its own.
    for (key, expected) in [
        (
            "load_ticks",
            PluginLimits {
                load_ticks: 5,
                ..literal
            },
        ),
        (
            "load_heap_bytes",
            PluginLimits {
                load_heap_bytes: 5,
                ..literal
            },
        ),
        (
            "handler_ticks",
            PluginLimits {
                handler_ticks: 5,
                ..literal
            },
        ),
        (
            "handler_heap_bytes",
            PluginLimits {
                handler_heap_bytes: 5,
                ..literal
            },
        ),
        (
            "cell_ticks",
            PluginLimits {
                cell_ticks: 5,
                ..literal
            },
        ),
        (
            "cell_heap_bytes",
            PluginLimits {
                cell_heap_bytes: 5,
                ..literal
            },
        ),
        (
            "stack_depth",
            PluginLimits {
                stack_depth: 5,
                ..literal
            },
        ),
    ] {
        let config = load(
            ConfigProduct::Dalgon,
            &format!("[limits.plugins]\n{key} = 5\n"),
        )
        .expect("a one-key limits table loads");
        assert_eq!(config.plugin_limits(), &expected, "{key}");
    }
}

#[test]
fn limits_are_integers_that_fit_their_field_width() {
    // `stack_depth` is a u32 and the budgets are u64; TOML integers are i64.
    for (document, key, value) in [
        ("stack_depth = -1", "limits.plugins.stack_depth", "-1"),
        (
            "stack_depth = 4294967296",
            "limits.plugins.stack_depth",
            "4294967296",
        ),
        ("load_ticks = -1", "limits.plugins.load_ticks", "-1"),
        (
            "cell_heap_bytes = -9223372036854775808",
            "limits.plugins.cell_heap_bytes",
            "-9223372036854775808",
        ),
        ("handler_ticks = 5.0", "limits.plugins.handler_ticks", "5.0"),
        (
            "load_heap_bytes = \"5\"",
            "limits.plugins.load_heap_bytes",
            "5",
        ),
        ("cell_ticks = true", "limits.plugins.cell_ticks", "true"),
        ("stack_depth = [1]", "limits.plugins.stack_depth", "[1]"),
    ] {
        let error = load(
            ConfigProduct::Dalgon,
            &format!("[limits.plugins]\n{document}\n"),
        )
        .expect_err(document);
        assert!(
            matches!(
                &error,
                ConfigError::InvalidValue { key: found, value: shown, expected }
                    if found.as_ref() == key
                        && shown.as_ref() == value
                        && expected.as_ref() == "non-negative integer"
            ),
            "{document}: {error:?}"
        );
    }
    let widest = load(
        ConfigProduct::Dalgon,
        "[limits.plugins]\nstack_depth = 4294967295\nload_ticks = 9223372036854775807\nhandler_ticks = 0\n",
    )
    .expect("the widest representable limits load");
    let limits = widest.plugin_limits();
    assert_eq!(limits.stack_depth, u32::MAX);
    assert_eq!(limits.load_ticks, i64::MAX.unsigned_abs());
    assert_eq!(
        limits.handler_ticks, 0,
        "an explicit zero is kept, not defaulted"
    );
}

#[test]
fn limits_tables_reject_unknown_keys_and_wrong_container_types() {
    let error = load(
        ConfigProduct::Dalgon,
        "[limits.plugins]\nload_ticks = 5\nload_tick = 6\n",
    )
    .expect_err("a misspelled budget key is refused");
    assert!(
        matches!(
            &error,
            ConfigError::UnknownKey { key, known_keys, .. }
                if key.as_ref() == "limits.plugins.load_tick"
                    && known_keys.len() == 7
                    && known_keys.iter().all(|known| known.starts_with("limits.plugins."))
        ),
        "{error:?}"
    );
    for (document, key, value) in [
        ("limits = 3", "limits", "3"),
        ("limits = [1]", "limits", "[1]"),
        ("[limits]\nplugins = 3", "limits.plugins", "3"),
        ("[limits]\nplugins = \"x\"", "limits.plugins", "x"),
    ] {
        let error = load(ConfigProduct::Dalgon, document).expect_err(document);
        assert!(
            matches!(
                &error,
                ConfigError::InvalidValue { key: found, value: shown, expected }
                    if found.as_ref() == key
                        && shown.as_ref() == value
                        && expected.as_ref() == "table"
            ),
            "{document}: {error:?}"
        );
    }
    // Sibling `[limits]` keys other consumers read are not this table's.
    let sibling = load(
        ConfigProduct::Dalgon,
        "[limits]\nagents = 64\n[limits.plugins]\nstack_depth = 7\n",
    )
    .expect("sibling limit keys stay legal");
    assert_eq!(sibling.plugin_limits().stack_depth, 7);
}

#[test]
fn limits_merge_per_key_across_the_product_and_user_layers() {
    let defaults = "[limits]\nagents = 64\n[limits.plugins]\nload_ticks = 7\nstack_depth = 9\n";
    let user = "[limits.plugins]\nstack_depth = 11\ncell_ticks = 13\n";
    let config = Config::load(
        ConfigProduct::Dalgona,
        Path::new(DATA_ROOT),
        defaults,
        Some(user),
    )
    .expect("both layers load");
    assert_eq!(
        config.plugin_limits(),
        &PluginLimits {
            load_ticks: 7,
            stack_depth: 11,
            cell_ticks: 13,
            ..PluginLimits::default()
        },
        "the user layer replaces only the keys it names"
    );
    let untouched = Config::load(
        ConfigProduct::Dalgona,
        Path::new(DATA_ROOT),
        defaults,
        Some("mode = \"normal\"\n"),
    )
    .expect("a user layer without limits keeps the product limits");
    assert_eq!(untouched.plugin_limits().load_ticks, 7);
    assert_eq!(untouched.plugin_limits().stack_depth, 9);
}
