//! Starlark evaluator budgets under `[limits.plugins]`.

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

pub(super) fn parse_plugin_limits(
    sections: &std::collections::BTreeMap<Box<str>, toml::Value>,
) -> Result<PluginLimits, ConfigError> {
    let Some(limits) = sections.get("limits") else {
        return Ok(PluginLimits::default());
    };
    let toml::Value::Table(limits) = limits else {
        return Err(invalid_value("limits", value_text(limits), "table"));
    };
    let Some(plugins) = limits.get("plugins") else {
        return Ok(PluginLimits::default());
    };
    let toml::Value::Table(table) = plugins else {
        return Err(invalid_value(
            "limits.plugins",
            value_text(plugins),
            "table",
        ));
    };
    for key in table.keys() {
        match key.as_str() {
            "load_ticks" | "load_heap_bytes" | "handler_ticks" | "handler_heap_bytes"
            | "cell_ticks" | "cell_heap_bytes" | "stack_depth" => {}
            other => {
                return Err(ConfigError::UnknownKey {
                    key: Box::<str>::from(format!("limits.plugins.{other}")),
                    suggestion: None,
                    known_keys: vec![
                        Box::<str>::from("limits.plugins.load_ticks"),
                        Box::<str>::from("limits.plugins.load_heap_bytes"),
                        Box::<str>::from("limits.plugins.handler_ticks"),
                        Box::<str>::from("limits.plugins.handler_heap_bytes"),
                        Box::<str>::from("limits.plugins.cell_ticks"),
                        Box::<str>::from("limits.plugins.cell_heap_bytes"),
                        Box::<str>::from("limits.plugins.stack_depth"),
                    ],
                });
            }
        }
    }
    let get_u64 = |key: &str, fallback: u64| -> Result<u64, ConfigError> {
        match table.get(key) {
            None => Ok(fallback),
            Some(toml::Value::Integer(value)) => u64::try_from(*value).map_err(|_| {
                invalid_value(
                    &format!("limits.plugins.{key}"),
                    format!("{value}"),
                    "non-negative integer",
                )
            }),
            Some(other) => Err(invalid_value(
                &format!("limits.plugins.{key}"),
                value_text(other),
                "non-negative integer",
            )),
        }
    };
    let get_u32 = |key: &str, fallback: u32| -> Result<u32, ConfigError> {
        match table.get(key) {
            None => Ok(fallback),
            Some(toml::Value::Integer(value)) => u32::try_from(*value).map_err(|_| {
                invalid_value(
                    &format!("limits.plugins.{key}"),
                    format!("{value}"),
                    "non-negative integer",
                )
            }),
            Some(other) => Err(invalid_value(
                &format!("limits.plugins.{key}"),
                value_text(other),
                "non-negative integer",
            )),
        }
    };
    Ok(PluginLimits {
        load_ticks: get_u64("load_ticks", 1_000_000)?,
        load_heap_bytes: get_u64("load_heap_bytes", 16_777_216)?,
        handler_ticks: get_u64("handler_ticks", 200_000)?,
        handler_heap_bytes: get_u64("handler_heap_bytes", 8_388_608)?,
        cell_ticks: get_u64("cell_ticks", 2_000_000)?,
        cell_heap_bytes: get_u64("cell_heap_bytes", 33_554_432)?,
        stack_depth: get_u32("stack_depth", 100)?,
    })
}
