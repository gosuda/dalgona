//! Starlark evaluator budgets under `[limits.plugins]`.

use super::parse::closest_key;
use super::{ConfigError, invalid_value, value_text};

/// The `[limits.plugins]` table: Starlark evaluator budgets.
///
/// Seven keys; landed as `[limits.plugins]` so the gate-pinned root
/// `plugins` array and limits coexist in one document. Defaults match
/// the `dal-star` engine constants; the single `stack_depth` fans out to
/// all three contexts in the adapter conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields, default)]
pub struct PluginLimits {
    /// Load-time tick budget.
    pub load_ticks: u64,
    /// Load-time heap budget in bytes.
    pub load_heap_bytes: u64,
    /// Handler tick budget.
    pub handler_ticks: u64,
    /// Handler heap budget in bytes.
    pub handler_heap_bytes: u64,
    /// Cell tick budget.
    pub cell_ticks: u64,
    /// Cell heap budget in bytes.
    pub cell_heap_bytes: u64,
    /// Shared call-stack depth for all three contexts.
    pub stack_depth: u32,
}

impl Default for PluginLimits {
    fn default() -> Self {
        Self {
            load_ticks: 1_000_000,
            load_heap_bytes: 16_777_216,
            handler_ticks: 200_000,
            handler_heap_bytes: 8_388_608,
            cell_ticks: 2_000_000,
            cell_heap_bytes: 33_554_432,
            stack_depth: 100,
        }
    }
}

/// The seven keys of `[limits.plugins]`, in schema order.
const KEYS: [&str; 7] = [
    "load_ticks",
    "load_heap_bytes",
    "handler_ticks",
    "handler_heap_bytes",
    "cell_ticks",
    "cell_heap_bytes",
    "stack_depth",
];

/// The largest budget a TOML integer can name. TOML integers are signed
/// 64-bit, so no 64-bit budget can exceed this.
const MAX_BUDGET: u64 = i64::MAX.unsigned_abs();

/// Parses `[limits.plugins]` from the merged sections.
///
/// Each key the table names replaces its default; a missing key keeps the
/// default. Unknown keys and wrong types are refused, and so is every value
/// outside the range its field accepts. The seven budget names directly under
/// `[limits]` and the singular `[limits.plugin]` table are refused as
/// misplacements; any other `[limits]` key belongs to another consumer.
pub(super) fn parse_plugin_limits(
    sections: &std::collections::BTreeMap<Box<str>, toml::Value>,
) -> Result<PluginLimits, ConfigError> {
    let defaults = PluginLimits::default();
    let Some(limits) = sections.get("limits") else {
        return Ok(defaults);
    };
    let toml::Value::Table(limits) = limits else {
        return Err(invalid_value("limits", value_text(limits), "table"));
    };
    let known_keys = || -> Vec<Box<str>> {
        KEYS.iter()
            .map(|known| Box::<str>::from(format!("limits.plugins.{known}")))
            .collect()
    };
    for key in KEYS {
        if limits.contains_key(key) {
            return Err(ConfigError::UnknownKey {
                key: Box::<str>::from(format!("limits.{key}")),
                suggestion: Some(Box::<str>::from(format!("limits.plugins.{key}"))),
                known_keys: known_keys(),
            });
        }
    }
    if limits.contains_key("plugin") {
        return Err(ConfigError::UnknownKey {
            key: Box::<str>::from("limits.plugin"),
            suggestion: Some(Box::<str>::from("limits.plugins")),
            known_keys: known_keys(),
        });
    }
    let Some(plugins) = limits.get("plugins") else {
        return Ok(defaults);
    };
    let toml::Value::Table(table) = plugins else {
        return Err(invalid_value(
            "limits.plugins",
            value_text(plugins),
            "table",
        ));
    };
    for key in table.keys() {
        if !KEYS.contains(&key.as_str()) {
            let unknown = format!("limits.plugins.{key}");
            let known_keys = known_keys();
            return Err(ConfigError::UnknownKey {
                suggestion: closest_key(&unknown, &known_keys),
                key: Box::<str>::from(unknown),
                known_keys,
            });
        }
    }
    let parsed = PluginLimits {
        load_ticks: budget(table, "load_ticks", defaults.load_ticks, MAX_BUDGET)?,
        load_heap_bytes: budget(
            table,
            "load_heap_bytes",
            defaults.load_heap_bytes,
            MAX_BUDGET,
        )?,
        handler_ticks: budget(table, "handler_ticks", defaults.handler_ticks, MAX_BUDGET)?,
        handler_heap_bytes: budget(
            table,
            "handler_heap_bytes",
            defaults.handler_heap_bytes,
            MAX_BUDGET,
        )?,
        cell_ticks: budget(table, "cell_ticks", defaults.cell_ticks, MAX_BUDGET)?,
        cell_heap_bytes: budget(
            table,
            "cell_heap_bytes",
            defaults.cell_heap_bytes,
            MAX_BUDGET,
        )?,
        stack_depth: budget(
            table,
            "stack_depth",
            defaults.stack_depth,
            u64::from(u32::MAX),
        )?,
    };
    for (key, value) in [
        ("load_heap_bytes", parsed.load_heap_bytes),
        ("handler_heap_bytes", parsed.handler_heap_bytes),
        ("cell_heap_bytes", parsed.cell_heap_bytes),
    ] {
        if usize::try_from(value).is_err() {
            return Err(invalid_value(
                &format!("limits.plugins.{key}"),
                value.to_string(),
                &format!("fits host usize (max {})", usize::MAX),
            ));
        }
    }
    Ok(parsed)
}

/// Reads one budget key: a TOML integer from 1 through `max` that fits `T`.
///
/// A missing key returns `fallback`. Zero is refused because the evaluator
/// rejects a zero budget on every run, so a zero here could only fail later.
///
/// # Errors
///
/// Returns an invalid-value error that names the key and the accepted range
/// when the value is not an integer, is outside the range, or does not fit `T`.
fn budget<T: TryFrom<u64>>(
    table: &toml::Table,
    key: &str,
    fallback: T,
    max: u64,
) -> Result<T, ConfigError> {
    let shown = match table.get(key) {
        None => return Ok(fallback),
        Some(toml::Value::Integer(value)) => {
            if let Some(accepted) = u64::try_from(*value)
                .ok()
                .filter(|n| (1..=max).contains(n))
                .and_then(|n| T::try_from(n).ok())
            {
                return Ok(accepted);
            }
            value.to_string().into_boxed_str()
        }
        Some(other) => value_text(other),
    };
    Err(invalid_value(
        &format!("limits.plugins.{key}"),
        shown,
        &format!("integer from 1 to {max}"),
    ))
}
