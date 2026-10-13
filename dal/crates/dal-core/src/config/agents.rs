//! Typed `[agents]` table: child-session admission.

use std::num::NonZeroU32;

use super::error::{ConfigError, invalid_value, value_text};

/// Strict typed `[agents]` table owned by core vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields, default)]
pub struct AgentsConfig {
    /// Whether the `agent` model tool is published.
    pub enabled: bool,
    /// FIFO admission cap for concurrent children.
    pub max_concurrent: NonZeroU32,
    /// Maximum nested child depth.
    pub max_depth: NonZeroU32,
}

impl Default for AgentsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_concurrent: NonZeroU32::new(32).unwrap_or(NonZeroU32::MIN),
            max_depth: NonZeroU32::MIN,
        }
    }
}

/// Decodes the merged `[agents]` table with strict keys and positive bounds.
pub(super) fn parse_agents(
    sections: &std::collections::BTreeMap<Box<str>, toml::Value>,
) -> Result<AgentsConfig, ConfigError> {
    let Some(agents) = sections.get("agents") else {
        return Ok(AgentsConfig::default());
    };
    let toml::Value::Table(table) = agents else {
        return Err(invalid_value("agents", value_text(agents), "table"));
    };
    for key in table.keys() {
        match key.as_str() {
            "enabled" | "max_concurrent" | "max_depth" => {}
            other => {
                return Err(ConfigError::UnknownKey {
                    key: Box::<str>::from(format!("agents.{other}")),
                    suggestion: None,
                    known_keys: vec![
                        Box::<str>::from("agents.enabled"),
                        Box::<str>::from("agents.max_concurrent"),
                        Box::<str>::from("agents.max_depth"),
                    ],
                });
            }
        }
    }
    let enabled = match table.get("enabled") {
        None => false,
        Some(toml::Value::Boolean(enabled)) => *enabled,
        Some(other) => {
            return Err(invalid_value(
                "agents.enabled",
                value_text(other),
                "boolean",
            ));
        }
    };
    let max_concurrent = match table.get("max_concurrent") {
        None => 32,
        Some(toml::Value::Integer(value)) => u32::try_from(*value).map_err(|_| {
            invalid_value(
                "agents.max_concurrent",
                format!("{value}"),
                "Use a positive integer.",
            )
        })?,
        Some(other) => {
            return Err(invalid_value(
                "agents.max_concurrent",
                value_text(other),
                "Use a positive integer.",
            ));
        }
    };
    let max_concurrent = NonZeroU32::new(max_concurrent)
        .ok_or_else(|| invalid_value("agents.max_concurrent", "0", "Use a positive integer."))?;
    let max_depth = match table.get("max_depth") {
        None => 1,
        Some(toml::Value::Integer(value)) => u32::try_from(*value).map_err(|_| {
            invalid_value(
                "agents.max_depth",
                format!("{value}"),
                "Use a positive integer.",
            )
        })?,
        Some(other) => {
            return Err(invalid_value(
                "agents.max_depth",
                value_text(other),
                "Use a positive integer.",
            ));
        }
    };
    let max_depth = NonZeroU32::new(max_depth)
        .ok_or_else(|| invalid_value("agents.max_depth", "0", "Use a positive integer."))?;
    Ok(AgentsConfig {
        enabled,
        max_concurrent,
        max_depth,
    })
}
