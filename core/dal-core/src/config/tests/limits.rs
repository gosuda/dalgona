//! Plugin limits tests.

use super::super::{Config, ConfigError, ConfigProduct};
use super::{DATA_ROOT, load};
use crate::config::PluginLimits;
use std::path::Path;

/// The largest value a TOML integer holds, so the largest budget any key accepts.
const I64_MAX: &str = "9223372036854775807";
/// The largest value of the `u32` `stack_depth` field.
const U32_MAX: &str = "4294967295";
/// Every plugin budget key with the largest integer it accepts, in schema order.
const BUDGETS: [(&str, &str); 7] = [
    ("load_ticks", I64_MAX),
    ("load_heap_bytes", I64_MAX),
    ("handler_ticks", I64_MAX),
    ("handler_heap_bytes", I64_MAX),
    ("cell_ticks", I64_MAX),
    ("cell_heap_bytes", I64_MAX),
    ("stack_depth", U32_MAX),
];
/// The products that share the one `Config::load` parser.
const PRODUCTS: [ConfigProduct; 2] = [ConfigProduct::Dalgon, ConfigProduct::Dalgona];

/// Whether `expected` states both ends of the accepted range, `1` and `max`, as decimal numbers.
fn states_range(expected: &str, max: &str) -> bool {
    let numbers: Vec<&str> = expected
        .split(|c: char| !c.is_ascii_digit())
        .filter(|number| !number.is_empty())
        .collect();
    numbers.contains(&"1") && numbers.contains(&max)
}

/// Asserts that a `[limits.plugins]` table holding `entry` is refused as an invalid value that
/// names the budget key and states the accepted range.
fn assert_range_refused(product: ConfigProduct, entry: &str, budget: &str, max: &str) {
    let document = format!("[limits.plugins]\n{entry}\n");
    let error = load(product, &document).expect_err(&document);
    let key = format!("limits.plugins.{budget}");
    assert!(
        matches!(
            &error,
            ConfigError::InvalidValue { key: found, expected, .. }
                if found.as_ref() == key.as_str() && states_range(expected, max)
        ),
        "{document}: {error:?}"
    );
}

/// Whether `error` names `key` and points the user at `target`, whichever typed variant
/// carries the advice.
fn names_and_points_at(error: &ConfigError, key: &str, target: &str) -> bool {
    match error {
        ConfigError::UnknownKey {
            key: found,
            suggestion,
            ..
        } => found.as_ref() == key && suggestion.as_deref() == Some(target),
        ConfigError::InvalidValue {
            key: found,
            expected,
            ..
        } => found.as_ref() == key && expected.contains(target),
        _ => false,
    }
}

/// Reads one budget by its key name.
fn budget_value(limits: &PluginLimits, budget: &str) -> u64 {
    match budget {
        "load_ticks" => limits.load_ticks,
        "load_heap_bytes" => limits.load_heap_bytes,
        "handler_ticks" => limits.handler_ticks,
        "handler_heap_bytes" => limits.handler_heap_bytes,
        "cell_ticks" => limits.cell_ticks,
        "cell_heap_bytes" => limits.cell_heap_bytes,
        "stack_depth" => u64::from(limits.stack_depth),
        other => unreachable!("{other} is not a plugin budget"),
    }
}

/// Loads a table naming only `budget = value` and checks that exactly that budget left its
/// default and kept the written value.
fn assert_only_budget_set(budget: &str, value: &str) {
    let document = format!("[limits.plugins]\n{budget} = {value}\n");
    let config = load(ConfigProduct::Dalgon, &document).expect(&document);
    let defaults = PluginLimits::default();
    for (other, _) in BUDGETS {
        let expected = if other == budget {
            value.parse::<u64>().expect("a decimal budget")
        } else {
            budget_value(&defaults, other)
        };
        assert_eq!(
            budget_value(config.plugin_limits(), other),
            expected,
            "{document}: {other}"
        );
    }
}

#[test]
fn limits_are_positive_integers_that_fit_their_field_width() {
    // `stack_depth` is a u32 and the budgets are u64; TOML integers are i64.
    // Zero is refused too: the evaluator rejects a zero budget on every run.
    for product in PRODUCTS {
        for (budget, max) in BUDGETS {
            for literal in [
                "0",
                "-1",
                "-9223372036854775808",
                "5.0",
                "\"5\"",
                "true",
                "[1]",
            ] {
                let entry = format!("{budget} = {literal}");
                assert_range_refused(product, &entry, budget, max);
            }
            // One past the widest TOML integer is not an integer the document can hold.
            let document = format!("[limits.plugins]\n{budget} = 9223372036854775808\n");
            assert!(load(product, &document).is_err(), "{document}");
        }
        for literal in ["4294967296", I64_MAX] {
            assert_range_refused(
                product,
                &format!("stack_depth = {literal}"),
                "stack_depth",
                U32_MAX,
            );
        }
    }
    let widest = load(
        ConfigProduct::Dalgon,
        "[limits.plugins]\nstack_depth = 4294967295\nload_ticks = 9223372036854775807\nhandler_ticks = 1\n",
    )
    .expect("the widest representable limits load");
    let limits = widest.plugin_limits();
    assert_eq!(limits.stack_depth, u32::MAX);
    assert_eq!(limits.load_ticks, i64::MAX.unsigned_abs());
    assert_eq!(
        limits.handler_ticks, 1,
        "the smallest accepted budget is kept, not defaulted"
    );
}

#[test]
fn each_budget_keeps_its_boundary_values_and_leaves_the_rest_default() {
    for (budget, max) in BUDGETS {
        assert_only_budget_set(budget, "1");
        // A heap budget must also fit the host `usize`, so only `u32::MAX` is reachable on
        // every host; the 64-bit test below takes it further where that applies.
        let widest = if budget.ends_with("_heap_bytes") {
            U32_MAX
        } else {
            max
        };
        assert_only_budget_set(budget, widest);
    }
}

#[test]
fn misspelled_budget_keys_suggest_the_accepted_spelling() {
    for (budget, _) in BUDGETS {
        let misspelled = &budget[..budget.len() - 1];
        let document = format!("[limits.plugins]\n{misspelled} = 5\n");
        let error = load(ConfigProduct::Dalgon, &document).expect_err(&document);
        let key = format!("limits.plugins.{misspelled}");
        let target = format!("limits.plugins.{budget}");
        assert!(
            matches!(
                &error,
                ConfigError::UnknownKey { key: found, suggestion, .. }
                    if found.as_ref() == key.as_str()
                        && suggestion.as_deref() == Some(target.as_str())
            ),
            "{document}: {error:?}"
        );
    }
}

#[test]
fn budget_keys_directly_under_limits_are_refused_not_ignored() {
    for product in PRODUCTS {
        for (budget, _) in BUDGETS {
            let key = format!("limits.{budget}");
            let target = format!("limits.plugins.{budget}");
            for document in [
                format!("[limits]\n{budget} = 5\n"),
                format!("limits = {{ {budget} = 5 }}\n"),
                // A valid table beside the misplaced key must not mask it.
                format!("[limits]\n{budget} = 5\n[limits.plugins]\nstack_depth = 7\n"),
                // Neither may a valid sibling.
                format!("[limits]\nagents = 64\n{budget} = 5\n"),
                format!("[agents]\nenabled = true\n[limits]\n{budget} = 5\n"),
            ] {
                let error = load(product, &document).expect_err(&document);
                assert!(
                    names_and_points_at(&error, &key, &target),
                    "{document}: {error:?}"
                );
            }
        }
    }
}

#[test]
fn singular_limits_plugin_table_is_refused_not_ignored() {
    for product in PRODUCTS {
        for (budget, _) in BUDGETS {
            for document in [
                format!("[limits.plugin]\n{budget} = 5\n"),
                format!("[limits.plugin]\n{budget} = 5\n[limits.plugins]\nstack_depth = 7\n"),
                format!("[limits]\nagents = 64\n[limits.plugin]\n{budget} = 5\n"),
            ] {
                let error = load(product, &document).expect_err(&document);
                assert!(
                    names_and_points_at(&error, "limits.plugin", "limits.plugins"),
                    "{document}: {error:?}"
                );
            }
        }
    }
}

#[test]
fn sibling_agents_configuration_stays_legal_beside_the_budgets() {
    const PLUGINS: &str = "[limits.plugins]\nload_ticks = 11\nload_heap_bytes = 12\nhandler_ticks = 13\nhandler_heap_bytes = 14\ncell_ticks = 15\ncell_heap_bytes = 16\nstack_depth = 17\n";
    const AGENTS: &str = "[agents]\nenabled = true\nmax_concurrent = 4\nmax_depth = 2\n";
    let configured = PluginLimits {
        load_ticks: 11,
        load_heap_bytes: 12,
        handler_ticks: 13,
        handler_heap_bytes: 14,
        cell_ticks: 15,
        cell_heap_bytes: 16,
        stack_depth: 17,
    };
    for product in PRODUCTS {
        for document in [
            format!("[limits]\nagents = 64\n{PLUGINS}"),
            format!("{AGENTS}{PLUGINS}"),
            format!("{AGENTS}[limits]\nagents = 64\n{PLUGINS}"),
        ] {
            let config = load(product, &document).expect(&document);
            assert_eq!(config.plugin_limits(), &configured, "{document}");
        }
        let alone = load(product, "[limits]\nagents = 64\n").expect("a lone sibling loads");
        assert_eq!(alone.plugin_limits(), &PluginLimits::default());
        let agents = load(product, &format!("{AGENTS}{PLUGINS}")).expect("agents load");
        assert!(agents.agents().enabled);
        assert_eq!(agents.agents().max_concurrent.get(), 4);
        assert_eq!(agents.agents().max_depth.get(), 2);
    }
}

#[test]
fn user_layers_cannot_slip_a_bad_budget_past_valid_defaults() {
    let defaults = "[limits]\nagents = 64\n[limits.plugins]\nload_ticks = 7\n";
    let zero = Config::load(
        ConfigProduct::Dalgona,
        Path::new(DATA_ROOT),
        defaults,
        Some("[limits.plugins]\nload_ticks = 0\n"),
    )
    .expect_err("a zero budget from the user layer is refused");
    assert!(
        matches!(
            &zero,
            ConfigError::InvalidValue { key, expected, .. }
                if key.as_ref() == "limits.plugins.load_ticks" && states_range(expected, I64_MAX)
        ),
        "{zero:?}"
    );
    let misplaced = Config::load(
        ConfigProduct::Dalgona,
        Path::new(DATA_ROOT),
        defaults,
        Some("[limits]\nstack_depth = 5\n"),
    )
    .expect_err("a misplaced key from the user layer is refused");
    assert!(
        names_and_points_at(
            &misplaced,
            "limits.stack_depth",
            "limits.plugins.stack_depth"
        ),
        "{misplaced:?}"
    );
    let in_product_layer = Config::load(
        ConfigProduct::Dalgona,
        Path::new(DATA_ROOT),
        "[limits]\nload_ticks = 5\n",
        Some("[limits.plugins]\nstack_depth = 9\n"),
    );
    assert!(
        in_product_layer.is_err(),
        "a misplaced key in the product layer is refused beside a valid user table"
    );
}

// A TOML integer never exceeds `i64::MAX`, which every 64-bit `usize` holds, so the parse-time
// host-width check has nothing to refuse here and must neither clamp nor narrow the value.
// Every supported target is 64-bit, so the 32-bit refusal is unreachable and not simulated.
#[cfg(target_pointer_width = "64")]
#[test]
fn heap_budgets_keep_every_toml_integer_on_a_64_bit_host() {
    const HEAPS: [&str; 3] = ["load_heap_bytes", "handler_heap_bytes", "cell_heap_bytes"];
    for budget in HEAPS {
        for value in ["4294967296", I64_MAX] {
            assert_only_budget_set(budget, value);
        }
    }
    let all = load(
        ConfigProduct::Dalgon,
        "[limits.plugins]\nload_heap_bytes = 9223372036854775807\nhandler_heap_bytes = 9223372036854775807\ncell_heap_bytes = 9223372036854775807\n",
    )
    .expect("the widest heap budgets load together");
    for budget in HEAPS {
        assert_eq!(
            budget_value(all.plugin_limits(), budget),
            i64::MAX.unsigned_abs(),
            "{budget}"
        );
    }
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
