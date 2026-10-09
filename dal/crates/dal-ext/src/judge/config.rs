use dal_core::{Config, ConfigError};

const KEYS: [&str; 5] = [
    "gate",
    "model",
    "timeout_ms",
    "max_concurrent",
    "max_per_turn",
];
const TABLE_EXPECTED: &str =
    "Use a [judge] table with gate, model, timeout_ms, max_concurrent, max_per_turn.";
const GATE_EXPECTED: &str = "Use one of auto, on, off.";
const MODEL_EXPECTED: &str = "Use a string. The empty string means the session's model.";
const TIMEOUT_EXPECTED: &str = "Use a whole number from 1000 to 300000.";
const CONCURRENT_EXPECTED: &str = "Use a whole number from 1 to 32.";
const PER_TURN_EXPECTED: &str = "Use a whole number from 1 to 256.";

/// The configured startup policy for the judge role.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GateSetting {
    /// Enable the role when a usable model and credentials resolve.
    Auto,
    /// Require a usable model and credentials at session open.
    On,
    /// Disable judge calls for the session.
    Off,
}

/// Configuration for the session's single judge model role.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JudgeConfig {
    /// Startup policy, defaulting to automatic resolution.
    pub gate: GateSetting,
    /// Optional model id override; empty selects the session model.
    pub model: Box<str>,
    /// Maximum duration of one judge call in milliseconds.
    pub timeout_ms: u64,
    /// Maximum simultaneous judge requests in the session.
    pub max_concurrent: u32,
    /// Maximum admitted judge calls in one turn window.
    pub max_per_turn: u32,
}

impl Default for JudgeConfig {
    fn default() -> Self {
        Self {
            gate: GateSetting::Auto,
            model: Box::from(""),
            timeout_ms: 30_000,
            max_concurrent: 4,
            max_per_turn: 16,
        }
    }
}

impl JudgeConfig {
    /// Decodes the merged `[judge]` configuration section.
    ///
    /// # Errors
    /// Returns [`ConfigError`] for a non-table section, an unknown key, or a
    /// value outside the judge configuration contract.
    pub fn from_config(config: &Config) -> Result<Self, ConfigError> {
        let Some(section) = config.section("judge") else {
            return Ok(Self::default());
        };
        let Some(table) = section.as_table() else {
            return Err(invalid_value("judge", section, TABLE_EXPECTED));
        };

        for key in table.keys() {
            if !KEYS.contains(&key.as_str()) {
                return Err(ConfigError::UnknownKey {
                    key: Box::from(format!("judge.{key}")),
                    suggestion: suggest(key),
                    known_keys: KEYS
                        .iter()
                        .map(|known| Box::from(format!("judge.{known}")))
                        .collect(),
                });
            }
        }

        let gate = match table.get("gate") {
            None => GateSetting::Auto,
            Some(value) => match value.as_str() {
                Some("auto") => GateSetting::Auto,
                Some("on") => GateSetting::On,
                Some("off") => GateSetting::Off,
                _ => return Err(invalid_value("judge.gate", value, GATE_EXPECTED)),
            },
        };
        let model = match table.get("model") {
            None => Box::from(""),
            Some(value) => match value.as_str() {
                Some(model) => Box::from(model),
                None => return Err(invalid_value("judge.model", value, MODEL_EXPECTED)),
            },
        };
        let timeout_ms: u64 =
            bounded(table, "timeout_ms", 1000, 300_000, TIMEOUT_EXPECTED)?.unwrap_or(30_000);
        let max_concurrent: u32 =
            bounded(table, "max_concurrent", 1, 32, CONCURRENT_EXPECTED)?.unwrap_or(4);
        let max_per_turn: u32 =
            bounded(table, "max_per_turn", 1, 256, PER_TURN_EXPECTED)?.unwrap_or(16);

        Ok(Self {
            gate,
            model,
            timeout_ms,
            max_concurrent,
            max_per_turn,
        })
    }
}

fn invalid_value(key: &'static str, value: &toml::Value, expected: &'static str) -> ConfigError {
    ConfigError::InvalidValue {
        key: Box::from(key),
        value: Box::from(value.to_string()),
        expected: Box::from(expected),
    }
}

fn bounded<T: TryFrom<i64>>(
    table: &toml::Table,
    key: &str,
    min: i64,
    max: i64,
    expected: &'static str,
) -> Result<Option<T>, ConfigError> {
    let Some(value) = table.get(key) else {
        return Ok(None);
    };
    let failure = || invalid_value_for(table, key, expected);
    let number = value
        .as_integer()
        .filter(|number| (min..=max).contains(number))
        .ok_or_else(failure)?;
    T::try_from(number).map_err(|_| failure()).map(Some)
}

fn invalid_value_for(table: &toml::Table, key: &str, expected: &'static str) -> ConfigError {
    let value = table.get(key).map(ToString::to_string).unwrap_or_default();
    ConfigError::InvalidValue {
        key: Box::from(format!("judge.{key}")),
        value: Box::from(value),
        expected: Box::from(expected),
    }
}

/// Nearest judge key within edit distance 2, mirroring the core's
/// did-you-mean rule scoped to this section's five keys.
fn suggest(key: &str) -> Option<Box<str>> {
    let mut best: Option<(&str, usize)> = None;
    for candidate in KEYS {
        let distance = edit_distance(key.as_bytes(), candidate.as_bytes());
        if distance > 2 {
            continue;
        }
        match best {
            Some((_, best_distance)) if distance >= best_distance => {}
            _ => best = Some((candidate, distance)),
        }
    }
    best.map(|(candidate, _)| Box::from(candidate))
}

fn edit_distance(left: &[u8], right: &[u8]) -> usize {
    const MAX_DISTANCE: usize = 2;
    if left.len().abs_diff(right.len()) > MAX_DISTANCE {
        return MAX_DISTANCE + 1;
    }
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    let mut current = vec![0; right.len() + 1];
    for (left_index, left_byte) in left.iter().enumerate() {
        current[0] = left_index + 1;
        let mut row_min = current[0];
        for (right_index, right_byte) in right.iter().enumerate() {
            let substitution = previous[right_index] + usize::from(left_byte != right_byte);
            current[right_index + 1] = (previous[right_index + 1] + 1)
                .min(current[right_index] + 1)
                .min(substitution);
            row_min = row_min.min(current[right_index + 1]);
        }
        if row_min > MAX_DISTANCE {
            return MAX_DISTANCE + 1;
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

#[cfg(test)]
mod tests {
    use super::{GateSetting, JudgeConfig};
    use dal_core::{Config, ConfigError, ConfigProduct};
    use std::path::Path;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn load(user_toml: Option<&str>) -> Result<Config, ConfigError> {
        Config::load(ConfigProduct::Dalgon, Path::new("."), "", user_toml)
    }

    fn invalid(config: &Config) -> ConfigError {
        JudgeConfig::from_config(config).expect_err("expected invalid judge config")
    }

    #[test]
    fn omitted_judge_table_uses_exact_defaults() -> TestResult {
        let config = load(None)?;
        assert_eq!(JudgeConfig::from_config(&config)?, JudgeConfig::default());
        Ok(())
    }

    #[test]
    fn judge_table_decodes_every_supported_field() -> TestResult {
        let config = load(Some(
            "[judge]\ngate = \"on\"\nmodel = \"gpt-5.6-luna\"\ntimeout_ms = 1000\nmax_concurrent = 1\nmax_per_turn = 256\n",
        ))?;
        let parsed = JudgeConfig::from_config(&config)?;
        assert_eq!(parsed.gate, GateSetting::On);
        assert_eq!(parsed.model.as_ref(), "gpt-5.6-luna");
        assert_eq!(parsed.timeout_ms, 1000);
        assert_eq!(parsed.max_concurrent, 1);
        assert_eq!(parsed.max_per_turn, 256);
        Ok(())
    }

    #[test]
    fn all_gate_literals_decode_to_distinct_settings() -> TestResult {
        for (value, expected) in [
            ("auto", GateSetting::Auto),
            ("on", GateSetting::On),
            ("off", GateSetting::Off),
        ] {
            let user_toml = format!("[judge]\ngate = \"{value}\"\n");
            let config = load(Some(&user_toml))?;
            assert_eq!(JudgeConfig::from_config(&config)?.gate, expected);
        }
        Ok(())
    }

    #[test]
    fn gate_error_preserves_toml_value_and_remedy() -> TestResult {
        let config = load(Some("[judge]\ngate = \"maybe\"\n"))?;
        let error = invalid(&config);
        assert!(matches!(
            error,
            ConfigError::InvalidValue { key, value, expected }
                if key.as_ref() == "judge.gate"
                    && value.as_ref() == "\"maybe\""
                    && expected.as_ref() == "Use one of auto, on, off."
        ));
        Ok(())
    }

    #[test]
    fn timeout_error_preserves_boundary_and_remedy() -> TestResult {
        let config = load(Some("[judge]\ntimeout_ms = 100\n"))?;
        let error = invalid(&config);
        assert!(matches!(
            error,
            ConfigError::InvalidValue { key, value, expected }
                if key.as_ref() == "judge.timeout_ms"
                    && value.as_ref() == "100"
                    && expected.as_ref() == "Use a whole number from 1000 to 300000."
        ));
        Ok(())
    }

    #[test]
    fn concurrent_limit_rejects_zero() -> TestResult {
        let config = load(Some("[judge]\nmax_concurrent = 0\n"))?;
        let error = invalid(&config);
        assert!(matches!(
            error,
            ConfigError::InvalidValue { key, value, expected }
                if key.as_ref() == "judge.max_concurrent"
                    && value.as_ref() == "0"
                    && expected.as_ref() == "Use a whole number from 1 to 32."
        ));
        Ok(())
    }

    #[test]
    fn per_turn_limit_rejects_values_above_range() -> TestResult {
        let config = load(Some("[judge]\nmax_per_turn = 257\n"))?;
        let error = invalid(&config);
        assert!(matches!(
            error,
            ConfigError::InvalidValue { key, value, expected }
                if key.as_ref() == "judge.max_per_turn"
                    && value.as_ref() == "257"
                    && expected.as_ref() == "Use a whole number from 1 to 256."
        ));
        Ok(())
    }

    #[test]
    fn model_requires_string_value() -> TestResult {
        let config = load(Some("[judge]\nmodel = 5\n"))?;
        let error = invalid(&config);
        assert!(matches!(
            error,
            ConfigError::InvalidValue { key, value, expected }
                if key.as_ref() == "judge.model"
                    && value.as_ref() == "5"
                    && expected.as_ref()
                        == "Use a string. The empty string means the session's model."
        ));
        Ok(())
    }

    #[test]
    fn unknown_judge_key_gets_the_core_suggestion() -> TestResult {
        let config = load(Some("[judge]\ntimeot_ms = 1000\n"))?;
        let error = invalid(&config);
        assert!(matches!(
            error,
            ConfigError::UnknownKey { key, suggestion, .. }
                if key.as_ref() == "judge.timeot_ms"
                    && suggestion.as_deref() == Some("timeout_ms")
        ));
        Ok(())
    }

    #[test]
    fn non_table_judge_section_uses_table_remedy() {
        // Core rejects a non-table `judge` while loading, before decode
        // runs: the plan remedy text must survive that load boundary.
        let error = load(Some("judge = 3\n")).expect_err("non-table judge fails");
        assert!(
            matches!(
                error,
                ConfigError::InvalidValue { ref key, ref value, ref expected }
                if key.as_ref() == "judge"
                    && value.as_ref() == "3"
                    && expected.as_ref() == super::TABLE_EXPECTED
            ),
            "unexpected non-table judge error: {error:?}"
        );
    }
}
