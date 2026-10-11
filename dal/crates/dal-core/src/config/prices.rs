use super::parse::{PRICE_KEYS, unknown_key};
use super::partial::{PartialPrice, PriceText};
use super::values::parse_number;
use super::{ConfigError, ConfigProduct, invalid_value, value_text};

pub(super) fn rules_invalid(key: &str, value: &toml::Value, hint: &str) -> ConfigError {
    invalid_value(key, value.to_string(), hint)
}

pub(super) fn parse_rules_choice<T: Copy>(
    key: &str,
    value: &toml::Value,
    choices: &[(&str, T)],
    hint: &str,
) -> Result<T, ConfigError> {
    if let toml::Value::String(text) = value
        && let Some((_, choice)) = choices
            .iter()
            .find(|(spelling, _)| *spelling == text.as_str())
    {
        return Ok(*choice);
    }
    Err(rules_invalid(key, value, hint))
}

pub(super) fn parse_rules_integer<T: TryFrom<i64>>(
    key: &str,
    value: &toml::Value,
    range: std::ops::RangeInclusive<i64>,
    hint: &str,
) -> Result<T, ConfigError> {
    if let toml::Value::Integer(number) = *value
        && range.contains(&number)
        && let Ok(number) = T::try_from(number)
    {
        return Ok(number);
    }
    Err(rules_invalid(key, value, hint))
}

pub(super) fn parse_rules_disabled(value: &toml::Value) -> Result<Vec<Box<str>>, ConfigError> {
    if let toml::Value::Array(items) = value
        && let Some(names) = items
            .iter()
            .map(|item| match item {
                toml::Value::String(name) if is_rule_name(name) => {
                    Some(Box::<str>::from(name.as_str()))
                }
                _ => None,
            })
            .collect::<Option<Vec<Box<str>>>>()
    {
        return Ok(names);
    }
    Err(rules_invalid(
        "rules.disabled",
        value,
        "Use a list of rule names, such as [\"no-sleep\"].",
    ))
}

/// Checks the rule name grammar `[A-Za-z0-9][A-Za-z0-9._-]{0,63}`.
pub(super) fn is_rule_name(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.len() <= 64
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'.' | b'_' | b'-'))
}

fn parse_tiers(
    key: &str,
    value: toml::Value,
) -> Result<Box<[crate::model::PriceTier]>, ConfigError> {
    let items = match value {
        toml::Value::Array(items) if !items.is_empty() => items,
        other => {
            return Err(invalid_value(
                key,
                value_text(&other),
                "a non-empty array of context price tiers",
            ));
        }
    };
    let mut tiers = Vec::with_capacity(items.len());
    let mut previous_size = None;
    for item in items {
        let table = match item {
            toml::Value::Table(table) => table,
            other => {
                return Err(invalid_value(
                    key,
                    value_text(&other),
                    "each tier must be a table",
                ));
            }
        };
        for field in table.keys() {
            if !matches!(
                field.as_str(),
                "size" | "input" | "cached_input" | "output" | "reasoning"
            ) {
                return Err(unknown_key(
                    &format!("{key}.{field}"),
                    ConfigProduct::Dalgon,
                    None,
                ));
            }
        }
        let size = match table.get("size") {
            Some(toml::Value::Integer(size)) if *size >= 0 => {
                u64::try_from(*size).map_err(|_| {
                    invalid_value(key, size.to_string(), "a non-negative context token count")
                })?
            }
            Some(other) => {
                return Err(invalid_value(
                    key,
                    value_text(other),
                    "a non-negative context token count",
                ));
            }
            None => {
                return Err(invalid_value(
                    key,
                    value_text(&toml::Value::Table(table.clone())),
                    "every tier needs a size",
                ));
            }
        };
        if previous_size.is_some_and(|previous| size <= previous) {
            return Err(invalid_value(
                key,
                size.to_string(),
                "strictly increasing context tier sizes",
            ));
        }
        let parse_optional = |field: &str| {
            table
                .get(field)
                .cloned()
                .map(|value| parse_rate(&format!("{key}.{field}"), value))
                .transpose()
        };
        let input = parse_optional("input")?;
        let cached_input = parse_optional("cached_input")?;
        let output = parse_optional("output")?;
        let reasoning = parse_optional("reasoning")?;
        if input.is_none() && cached_input.is_none() && output.is_none() && reasoning.is_none() {
            return Err(invalid_value(
                key,
                size.to_string(),
                "each tier needs at least one rate",
            ));
        }
        tiers.push(crate::model::PriceTier {
            size,
            input,
            cached_input,
            output,
            reasoning,
        });
        previous_size = Some(size);
    }
    Ok(tiers.into_boxed_slice())
}

pub(super) fn parse_prices(
    product: ConfigProduct,
    value: toml::Value,
) -> Result<std::collections::BTreeMap<Box<str>, PartialPrice>, ConfigError> {
    let prices = match value {
        toml::Value::Table(prices) => prices,
        other => {
            return Err(invalid_value(
                "prices",
                value_text(&other),
                "table of model price tables",
            ));
        }
    };
    let mut parsed = std::collections::BTreeMap::new();
    for (model_id, value) in prices {
        if model_id.is_empty() {
            return Err(invalid_value(
                "prices",
                Box::<str>::from(""),
                "non-empty model id",
            ));
        }
        let rates = match value {
            toml::Value::Table(rates) => rates,
            other => {
                return Err(invalid_value(
                    &format!("prices.{model_id}"),
                    value_text(&other),
                    "table of price rates",
                ));
            }
        };
        for key in rates.keys() {
            if !PRICE_KEYS.contains(&key.as_str()) {
                return Err(unknown_key(
                    &format!("prices.{model_id}.{key}"),
                    product,
                    Some(&model_id),
                ));
            }
        }
        let text: PriceText =
            toml::Value::Table(rates)
                .try_into()
                .map_err(|error| ConfigError::Syntax {
                    line: None,
                    message: Box::<str>::from(error.message()),
                })?;
        let mut partial = PartialPrice::default();
        if let Some(value) = text.input {
            partial.input = Some(parse_rate(&format!("prices.{model_id}.input"), value)?);
        }
        if let Some(value) = text.cached_input {
            partial.cached_input = Some(parse_rate(
                &format!("prices.{model_id}.cached_input"),
                value,
            )?);
        }
        if let Some(value) = text.output {
            partial.output = Some(parse_rate(&format!("prices.{model_id}.output"), value)?);
        }
        if let Some(value) = text.reasoning {
            partial.reasoning = Some(parse_rate(&format!("prices.{model_id}.reasoning"), value)?);
        }
        if let Some(value) = text.tiers {
            partial.tiers = Some(parse_tiers(&format!("prices.{model_id}.tiers"), value)?);
        }
        parsed.insert(model_id.into_boxed_str(), partial);
    }
    Ok(parsed)
}

pub(super) fn parse_rate(key: &str, value: toml::Value) -> Result<f64, ConfigError> {
    let rate = parse_number(key, value, "finite non-negative USD rate")?;
    if !rate.is_finite() || rate < 0.0 {
        return Err(invalid_value(
            key,
            format!("{rate}"),
            "finite non-negative USD rate",
        ));
    }
    Ok(rate)
}

pub(super) fn finish_prices(
    price_layers: std::collections::BTreeMap<Box<str>, PartialPrice>,
) -> Result<std::collections::BTreeMap<Box<str>, crate::model::ModelPrice>, ConfigError> {
    let mut prices = std::collections::BTreeMap::new();
    for (model_id, price) in price_layers {
        let price = price.finish(&model_id)?;
        prices.insert(model_id, price);
    }
    Ok(prices)
}
