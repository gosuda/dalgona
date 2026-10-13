// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Strict `[plugin.review]` settings decoded before battery construction.

use serde::{Deserialize, Deserializer, de};

/// Settings for the review battery.
///
/// The product factory strictly decodes this value before constructing the
/// battery. A missing field uses its code default.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewConfig {
    /// Whether the Dalgona product should register this battery.
    pub enabled: bool,
    /// Maximum non-converged review rounds, from 1 through 10.
    pub max_rounds: u8,
    /// Model id or alias; the empty string selects the session model.
    pub reviewer_model: String,
    /// Git revision to diff against; the empty string selects `HEAD`.
    pub diff_base: String,
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_rounds: 3,
            reviewer_model: String::new(),
            diff_base: String::new(),
        }
    }
}

/// A review configuration is not a table, has an unknown key, or contains an invalid value.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReviewConfigError {
    /// The configuration value is not a TOML table.
    #[error("plugin.review must be a table. Use a table with the supported review settings.")]
    InvalidSection,
    /// The configuration table contains an unsupported key.
    #[error(
        "[plugin.review] has no key \"{key}\". Use one of enabled, max_rounds, reviewer_model, diff_base."
    )]
    UnknownKey {
        /// The unsupported key inside `[plugin.review]`.
        key: Box<str>,
    },
    /// A setting has the wrong type or is outside its supported range.
    #[error("{key} {value} is invalid. Use {expected}.")]
    InvalidValue {
        /// The full setting path.
        key: Box<str>,
        /// The supplied TOML value.
        value: Box<str>,
        /// The required value shape or range.
        expected: Box<str>,
    },
    /// The strictly checked settings could not be decoded into their Rust type.
    #[error("plugin.review could not be decoded: {0}")]
    Decode(#[from] toml::de::Error),
}

impl ReviewConfig {
    /// Decodes one optional `[plugin.review]` table.
    ///
    /// # Errors
    /// Returns `ReviewConfigError` for an unknown key, wrong type, or invalid range.
    pub fn parse_config(section: Option<&toml::Value>) -> Result<Self, ReviewConfigError> {
        let Some(section) = section else {
            return Ok(Self::default());
        };
        let toml::Value::Table(table) = section else {
            return Err(ReviewConfigError::InvalidSection);
        };

        for key in table.keys() {
            if !REVIEW_CONFIG_KEYS.contains(&key.as_str()) {
                return Err(review_unknown_key(key));
            }
        }
        for (key, value) in table {
            let full_key = format!("plugin.review.{key}");
            match (key.as_str(), value) {
                ("enabled", toml::Value::Boolean(_))
                | ("reviewer_model", toml::Value::String(_)) => {}
                ("diff_base", toml::Value::String(base)) if !base.starts_with('-') => {}
                ("max_rounds", toml::Value::Integer(rounds)) if (1..=10).contains(rounds) => {}
                ("enabled", value) => {
                    return Err(invalid_review_value(&full_key, value, "boolean"));
                }
                ("max_rounds", value) => {
                    return Err(invalid_review_value(
                        &full_key,
                        value,
                        "whole number from 1 to 10",
                    ));
                }
                ("diff_base", value) => {
                    return Err(invalid_review_value(
                        &full_key,
                        value,
                        "a Git revision that does not start with '-'",
                    ));
                }
                ("reviewer_model", value) => {
                    return Err(invalid_review_value(&full_key, value, "string"));
                }
                _ => return Err(review_unknown_key(key)),
            }
        }
        section
            .clone()
            .try_into()
            .map_err(ReviewConfigError::Decode)
    }
}

const REVIEW_CONFIG_KEYS: [&str; 4] = ["enabled", "max_rounds", "reviewer_model", "diff_base"];

fn invalid_review_value(key: &str, value: &toml::Value, expected: &str) -> ReviewConfigError {
    ReviewConfigError::InvalidValue {
        key: key.into(),
        value: value.to_string().into_boxed_str(),
        expected: expected.into(),
    }
}

fn review_unknown_key(key: &str) -> ReviewConfigError {
    ReviewConfigError::UnknownKey { key: key.into() }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewConfigInput {
    #[serde(default = "default_enabled")]
    enabled: bool,
    #[serde(default = "default_max_rounds")]
    max_rounds: u8,
    #[serde(default)]
    reviewer_model: String,
    #[serde(default)]
    diff_base: String,
}

const fn default_enabled() -> bool {
    true
}

const fn default_max_rounds() -> u8 {
    3
}

impl<'de> Deserialize<'de> for ReviewConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let input = ReviewConfigInput::deserialize(deserializer)?;
        if !(1..=10).contains(&input.max_rounds) {
            return Err(de::Error::custom("max_rounds must be between 1 and 10"));
        }
        Ok(Self {
            enabled: input.enabled,
            max_rounds: input.max_rounds,
            reviewer_model: input.reviewer_model,
            diff_base: input.diff_base,
        })
    }
}
