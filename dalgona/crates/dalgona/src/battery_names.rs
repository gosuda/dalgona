// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! The unknown-battery-name diagnostic with the known names in the fix line.

use dal_core::ConfigError;

/// A `disabled_batteries` or `experimental_batteries` entry no battery registered.
#[derive(Debug, thiserror::Error)]
#[error("config: {key} names unknown battery \"{value}\".{suggestion}\nKnown batteries: {known}.")]
struct UnknownBattery {
    key: Box<str>,
    value: Box<str>,
    suggestion: String,
    known: String,
}

/// Validates both battery-name arrays against `registered`.
///
/// # Errors
/// An unknown name becomes a section error whose text names the entry, a
/// did-you-mean at edit distance at most 2, and every known name; any other
/// config error passes through unchanged.
pub(crate) fn validate(
    config: &dal_core::Config,
    registered: &[&str],
) -> Result<(), dalgon::BuildError> {
    match config.validate_battery_names(registered) {
        Ok(()) => Ok(()),
        Err(ConfigError::InvalidValue { key, value, .. }) => {
            let suggestion = registered
                .iter()
                .map(|name| (edit_distance(name, &value), name))
                .filter(|(distance, _)| *distance <= 2)
                .min_by_key(|(distance, _)| *distance)
                .map_or_else(String::new, |(_, name)| {
                    format!(" Did you mean \"{name}\"?")
                });
            Err(dalgon::BuildError::Section {
                section: key.clone(),
                source: Box::new(UnknownBattery {
                    key,
                    value,
                    suggestion,
                    known: registered.join(", "),
                }),
            })
        }
        Err(other) => Err(other.into()),
    }
}

fn edit_distance(left: &str, right: &str) -> usize {
    let right: Vec<char> = right.chars().collect();
    let mut row: Vec<usize> = (0..=right.len()).collect();
    for (i, l) in left.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, r) in right.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = if l == *r {
                diagonal
            } else {
                1 + diagonal.min(above).min(row[j])
            };
            diagonal = above;
        }
    }
    row[right.len()]
}
