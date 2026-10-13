//! Configuration error values.

/// Errors produced while parsing or validating product configuration.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ConfigError {
    /// The TOML document could not be parsed.
    #[error("dal.toml syntax error")]
    Syntax {
        /// One-based source line when the TOML parser provides a span.
        line: Option<usize>,
        /// Parser diagnostic without its source-location prefix.
        message: Box<str>,
    },
    /// A key is not supported by the selected product or table.
    #[error("unknown config key: {key}")]
    UnknownKey {
        /// Dotted path of the unknown key.
        key: Box<str>,
        /// Closest supported key when it is within the suggestion distance.
        suggestion: Option<Box<str>>,
        /// Supported keys in their schema order.
        known_keys: Vec<Box<str>>,
    },
    /// A supported key has an invalid value.
    #[error("invalid config value {value} for {key}: {expected}")]
    InvalidValue {
        /// Dotted path of the invalid value.
        key: Box<str>,
        /// Text representation of the invalid value.
        value: Box<str>,
        /// Accepted value or constraint.
        expected: Box<str>,
    },
    /// An unknown key inside `[guard]` or its subtables.
    #[error("unknown guard policy key {key}")]
    UnknownGuardKey {
        /// Bare key name without its dotted prefix.
        key: Box<str>,
    },
    /// Compiled product defaults are invalid.
    #[error("invalid product defaults")]
    DefaultsInvalid {
        /// Internal detail describing the invalid compiled defaults.
        message: Box<str>,
    },
}

impl ConfigError {
    /// Returns the two exact diagnostic lines of an invalid `[rules]` value, or
    /// `None` for every other error.
    ///
    /// `program` is the binary name, `dalgon` or `dalgona`. The first line names
    /// the key and the value in TOML form; the second line says what to write.
    #[must_use]
    pub fn rules_lines(&self, program: &str) -> Option<[String; 2]> {
        match self {
            Self::InvalidValue {
                key,
                value,
                expected,
            } if key.starts_with("rules.") => Some([
                format!("{program}: dal.toml: {key} {value} is invalid"),
                expected.to_string(),
            ]),
            _ => None,
        }
    }

    /// Returns the two exact diagnostic lines of a non-table `[judge]` value,
    /// or `None` for every other error.
    ///
    /// The first line names the offending key in the provider-table wording;
    /// the second line is the `expected` text verbatim.
    #[must_use]
    pub fn judge_lines(&self) -> Option<[String; 2]> {
        match self {
            Self::InvalidValue { key, expected, .. } if key.as_ref() == "judge" => {
                Some(["judge must be a table".to_string(), expected.to_string()])
            }
            _ => None,
        }
    }
}

pub(super) fn invalid_value(key: &str, value: impl Into<Box<str>>, expected: &str) -> ConfigError {
    ConfigError::InvalidValue {
        key: Box::<str>::from(key),
        value: value.into(),
        expected: Box::<str>::from(expected),
    }
}

pub(super) fn value_text(value: &toml::Value) -> Box<str> {
    match value {
        toml::Value::String(value) => Box::<str>::from(value.as_str()),
        _ => format!("{value}").into_boxed_str(),
    }
}
