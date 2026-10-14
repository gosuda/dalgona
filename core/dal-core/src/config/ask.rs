// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! Typed `[ask]` table: the one timeout of the shared ask service.

use std::time::Duration;

use super::error::{ConfigError, invalid_value, value_text};

/// How long one raised question stays open by default: 30 minutes.
const DEFAULT_TIMEOUT: Duration = Duration::from_mins(30);

/// The inclusive clamp bounds: one minute to 120 minutes.
const MIN_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_TIMEOUT: Duration = Duration::from_hours(2);

/// The expected wording of an invalid `ask.timeout` value.
const TIMEOUT_EXPECTED: &str = "a duration such as \"30m\", \"90s\", or \"2h\"";

/// Strict typed `[ask]` table owned by core vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields, default)]
pub struct AskConfig {
    /// How long one raised question stays open before its fail-closed
    /// default applies. Clamped at load into 1 m to 120 m.
    pub timeout_secs: u64,
}

impl Default for AskConfig {
    fn default() -> Self {
        Self {
            timeout_secs: DEFAULT_TIMEOUT.as_secs(),
        }
    }
}

impl AskConfig {
    /// The clamped ask timeout as a duration.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs)
    }
}

/// One clamped `[ask] timeout`: the pieces the startup notice names.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct AskTimeoutClamp {
    /// The configured value before clamping, in its shortest exact unit.
    pub value: Box<str>,
    /// The direction the clamp moved.
    pub side: ClampSide,
}

/// Which bound a clamped `[ask] timeout` hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum ClampSide {
    /// The configured value was under one minute.
    Below,
    /// The configured value was over 120 minutes.
    Above,
}

impl AskTimeoutClamp {
    /// Renders the one startup notice naming the key, the value, and the clamp.
    #[must_use]
    pub fn notice(&self) -> String {
        let clamp = match self.side {
            ClampSide::Below => "under the 1m minimum, clamped up to 1m",
            ClampSide::Above => "over the 120m maximum, clamped down to 120m",
        };
        format!("ask: [ask] timeout {} is {clamp}.", self.value)
    }
}

/// Decodes the merged `[ask]` table with strict keys, then clamps the
/// timeout into its inclusive range.
pub(super) fn parse_ask(
    sections: &std::collections::BTreeMap<Box<str>, toml::Value>,
) -> Result<(AskConfig, Option<AskTimeoutClamp>), ConfigError> {
    let Some(ask) = sections.get("ask") else {
        return Ok((AskConfig::default(), None));
    };
    let toml::Value::Table(table) = ask else {
        return Err(invalid_value("ask", value_text(ask), "table"));
    };
    for key in table.keys() {
        if key.as_str() != "timeout" {
            return Err(ConfigError::UnknownKey {
                key: Box::<str>::from(format!("ask.{key}")),
                suggestion: None,
                known_keys: vec![Box::<str>::from("ask.timeout")],
            });
        }
    }
    let configured = match table.get("timeout") {
        None => DEFAULT_TIMEOUT,
        Some(value) => crate::config::duration::parse(value)
            .map_err(|()| invalid_value("ask.timeout", value_text(value), TIMEOUT_EXPECTED))?,
    };
    Ok(clamp(configured))
}

fn clamp(configured: Duration) -> (AskConfig, Option<AskTimeoutClamp>) {
    if configured < MIN_TIMEOUT {
        return (
            AskConfig {
                timeout_secs: MIN_TIMEOUT.as_secs(),
            },
            Some(AskTimeoutClamp {
                value: render(configured),
                side: ClampSide::Below,
            }),
        );
    }
    if configured > MAX_TIMEOUT {
        return (
            AskConfig {
                timeout_secs: MAX_TIMEOUT.as_secs(),
            },
            Some(AskTimeoutClamp {
                value: render(configured),
                side: ClampSide::Above,
            }),
        );
    }
    (
        AskConfig {
            timeout_secs: configured.as_secs(),
        },
        None,
    )
}

/// Renders a duration in its shortest exact unit: hours, minutes, or seconds.
fn render(timeout: Duration) -> Box<str> {
    let secs = timeout.as_secs();
    if secs.is_multiple_of(3600) {
        format!("{}h", secs / 3600)
    } else if secs.is_multiple_of(60) {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
    .into_boxed_str()
}
