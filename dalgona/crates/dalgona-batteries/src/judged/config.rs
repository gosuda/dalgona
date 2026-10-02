use serde::Deserialize;

/// Feature switches for the judged battery. Every feature defaults to enabled.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool is one independently keyed `[plugin.judged]` feature switch"
)]
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct JudgedConfig {
    /// Enables auto-thinking classification.
    pub thinking: bool,
    /// Enables judged search-result ranking.
    pub ranking: bool,
    /// Enables anchored-ask classification.
    pub ask_anchor: bool,
    /// Enables settled-reply claim verification.
    pub claim_check: bool,
    /// Enables judge-based reminder deduplication.
    pub dedup: bool,
}

impl Default for JudgedConfig {
    fn default() -> Self {
        Self {
            thinking: true,
            ranking: true,
            ask_anchor: true,
            claim_check: true,
            dedup: true,
        }
    }
}

impl JudgedConfig {
    /// The accepted feature keys in deterministic validation order.
    pub const KEYS: [&str; 5] = ["thinking", "ranking", "ask_anchor", "claim_check", "dedup"];

    /// The canonical all-enabled TOML table for documentation and fixtures.
    pub const DEFAULTS_TOML: &str = "\
[plugin.judged]
thinking = true
ranking = true
ask_anchor = true
claim_check = true
dedup = true
";

    /// Decodes the optional `[plugin.judged]` section.
    ///
    /// Unknown keys are checked before values. Values are then checked in
    /// [`Self::KEYS`] order, so diagnostics are deterministic.
    ///
    /// # Errors
    /// Returns [`JudgedConfigError::UnknownKey`] for an unrecognized feature
    /// key, [`JudgedConfigError::WrongType`] for a non-boolean feature value,
    /// or [`JudgedConfigError::WrongSectionType`] when the section is not a table.
    pub fn from_config(section: Option<&toml::Value>) -> Result<Self, JudgedConfigError> {
        let Some(section) = section else {
            return Ok(Self::default());
        };
        let Some(table) = section.as_table() else {
            return Err(JudgedConfigError::WrongSectionType);
        };

        if let Some(key) = table.keys().find(|key| !Self::KEYS.contains(&key.as_str())) {
            let suggestion = Self::KEYS
                .iter()
                .find(|candidate| edit_distance_at_most_2(key, candidate).is_some())
                .map_or_else(String::new, |candidate| {
                    format!(" Did you mean \"{candidate}\"?")
                });
            return Err(JudgedConfigError::UnknownKey {
                key: key.clone().into_boxed_str(),
                suggestion: suggestion.into_boxed_str(),
            });
        }

        for key in Self::KEYS {
            if table
                .get(key)
                .is_some_and(|value| value.as_bool().is_none())
            {
                return Err(JudgedConfigError::WrongType {
                    key: Box::from(key),
                });
            }
        }

        section
            .clone()
            .try_into()
            .map_err(|_| JudgedConfigError::WrongSectionType)
    }
}

/// An invalid `[plugin.judged]` configuration entry.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum JudgedConfigError {
    /// The table contains a key that does not name one of the five features.
    #[error("judged: [plugin.judged] has an unknown key \"{key}\".{suggestion}")]
    UnknownKey {
        /// The unrecognized key.
        key: Box<str>,
        /// A complete optional ` Did you mean "<key>"?` sentence.
        suggestion: Box<str>,
    },
    /// A recognized feature key does not have a boolean value.
    #[error("judged: [plugin.judged].{key} must be true or false.")]
    WrongType {
        /// The feature key whose value is invalid.
        key: Box<str>,
    },
    /// The `[plugin.judged]` value is not a table.
    #[error("judged: [plugin.judged] must be a table.")]
    WrongSectionType,
}

fn edit_distance_at_most_2(left: &str, right: &str) -> Option<u32> {
    const MAX_KEY_BYTES: usize = 11;
    const ROW_LENGTH: usize = MAX_KEY_BYTES + 3;

    if left.len().abs_diff(right.len()) > 2 || left.len() > MAX_KEY_BYTES + 2 {
        return None;
    }

    let left = left.as_bytes();
    let right = right.as_bytes();
    let mut previous = [0_usize; ROW_LENGTH];
    let mut current = [0_usize; ROW_LENGTH];
    for (column, value) in previous.iter_mut().take(right.len() + 1).enumerate() {
        *value = column;
    }

    for (row, left_byte) in left.iter().enumerate() {
        current[0] = row + 1;
        let mut row_min = current[0];
        for (column, right_byte) in right.iter().enumerate() {
            let substitution =
                previous[column] + usize::from(!left_byte.eq_ignore_ascii_case(right_byte));
            let deletion = previous[column + 1] + 1;
            let insertion = current[column] + 1;
            let distance = substitution.min(deletion).min(insertion);
            current[column + 1] = distance;
            row_min = row_min.min(distance);
        }
        if row_min > 2 {
            return None;
        }
        std::mem::swap(&mut previous, &mut current);
    }

    let distance = previous[right.len()];
    if distance > 2 {
        return None;
    }
    u32::try_from(distance).ok()
}
