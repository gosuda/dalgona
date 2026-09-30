// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! `[plugin.history]` configuration.

use super::{DEFAULT_SHARE, MAX_SHARE, MIN_SHARE};
use thiserror::Error;

/// History configuration decoded from `[plugin.history]`.
///
/// `parse_config` reports invalid sections as `Err`, never as `Invalid`;
/// the `Invalid` variant exists so a caller holding an exact warning text
/// can still install the failing compactor through [`super::wiring::history`].
#[derive(Clone, Debug, PartialEq)]
pub enum HistoryConfig {
    /// History images are enabled with an image-token share.
    Enabled {
        /// Fraction of the window billable to images, `0.1..=0.7`.
        share: f64,
    },
    /// History images are disabled; no compactor or hooks run.
    Disabled,
    /// The section was invalid; the failing compactor surfaces `message`.
    Invalid {
        /// The exact configuration warning text.
        message: Box<str>,
    },
}

/// A typed `[plugin.history]` decoding failure with the product's exact text.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum HistoryConfigError {
    /// `enabled` is present but not a boolean.
    #[error("history: [plugin.history].enabled must be true or false.")]
    Enabled,
    /// `share` is missing, non-numeric, non-finite, or outside `0.1..=0.7`.
    #[error("history: [plugin.history].share must be a number from 0.1 to 0.7.")]
    Share,
    /// The table carries an unrecognized key.
    #[error("history: [plugin.history] has an unknown key \"{0}\".")]
    UnknownKey(String),
}

/// Decodes the optional `[plugin.history]` section.
///
/// `None` and absent keys resolve to `enabled = true` and `share = 0.4`.
/// Out-of-range `share` is rejected, never clamped.
///
/// # Errors
/// Returns the typed `HistoryConfigError` for a non-table section, a
/// non-boolean `enabled`, a non-numeric or out-of-range `share`, or any
/// unknown key.
pub fn parse_config(section: Option<&toml::Value>) -> Result<HistoryConfig, HistoryConfigError> {
    let Some(section) = section else {
        return Ok(HistoryConfig::Enabled {
            share: DEFAULT_SHARE,
        });
    };
    let Some(table) = section.as_table() else {
        return Err(HistoryConfigError::Enabled);
    };
    let enabled = match table.get("enabled") {
        None => true,
        Some(value) => value.as_bool().ok_or(HistoryConfigError::Enabled)?,
    };
    let share = match table.get("share") {
        None => DEFAULT_SHARE,
        Some(value) => decode_share(value)?,
    };
    if let Some(key) = table
        .keys()
        .find(|key| key.as_str() != "enabled" && key.as_str() != "share")
    {
        return Err(HistoryConfigError::UnknownKey(key.clone()));
    }
    if !enabled {
        return Ok(HistoryConfig::Disabled);
    }
    Ok(HistoryConfig::Enabled { share })
}

fn decode_share(value: &toml::Value) -> Result<f64, HistoryConfigError> {
    let Some(number) = value.as_float() else {
        return Err(HistoryConfigError::Share);
    };
    if !number.is_finite() || number < MIN_SHARE || number > MAX_SHARE {
        return Err(HistoryConfigError::Share);
    }
    Ok(number)
}
