//! Closed configuration policy values shared by dal's command and runtime layers.

use std::path::PathBuf;

/// The product execution mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// Run a normal interaction.
    Normal,
    /// Run the evaluation pass before the interaction.
    EvalFirst,
    /// Run only the evaluation pass.
    EvalOnly,
}

impl Mode {
    /// Projects this mode to the two mutually exclusive runtime switches.
    #[must_use]
    pub fn projection(self) -> ModeProjection {
        match self {
            Self::Normal => ModeProjection {
                eval_first: false,
                non_eval_only: false,
            },
            Self::EvalFirst => ModeProjection {
                eval_first: true,
                non_eval_only: false,
            },
            Self::EvalOnly => ModeProjection {
                eval_first: false,
                non_eval_only: true,
            },
        }
    }

    /// Returns this mode's canonical configuration spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::EvalFirst => "eval-first",
            Self::EvalOnly => "eval-only",
        }
    }
}

/// Policy for requesting approval before a tool action.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    /// Ask before every action requiring approval.
    Ask,
    /// Automatically approve edits, but ask for other actions.
    Edits,
    /// Approve all actions.
    All,
}

/// Presentation mode for the terminal interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Screen {
    /// Render inline in the existing terminal buffer.
    Inline,
    /// Use the terminal's fullscreen screen.
    Fullscreen,
}

/// Policy for whether and when judge evaluation is enabled.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum JudgeMode {
    /// Select judge behavior automatically.
    Auto,
    /// Enable judge evaluation.
    On,
    /// Disable judge evaluation.
    Off,
}

/// The product whose defaults are being loaded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ConfigProduct {
    /// The dalgon command-line product.
    Dalgon,
    /// The dalgona command-line product.
    Dalgona,
}

/// The runtime flags corresponding to a [`Mode`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ModeProjection {
    /// Whether to run the evaluation pass before the interaction.
    pub eval_first: bool,
    /// Whether execution is restricted to evaluation.
    pub non_eval_only: bool,
}

/// Configuration for serving the local control endpoint.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ServeConfig {
    /// Address on which the endpoint binds.
    pub bind: Box<str>,
    /// TCP port for the endpoint.
    pub port: u16,
    /// Path from which the endpoint's authentication token is read.
    pub token_file: PathBuf,
    /// Approval policy applied to endpoint actions.
    pub approval: ApprovalMode,
    /// Allowed browser origins.
    pub origins: Vec<Box<str>>,
}

#[cfg(test)]
mod tests {
    use super::{ApprovalMode, JudgeMode, Mode, ModeProjection, Screen};

    fn parse_enum<T: serde::de::DeserializeOwned>(spelling: &str) -> Result<T, toml::de::Error> {
        let document = format!("value = {spelling:?}");
        let mut table: toml::Table = toml::from_str(&document)?;
        table
            .remove("value")
            .expect("the test document contains value")
            .try_into()
    }

    #[test]
    fn mode_spellings_are_closed_and_canonical() {
        for (spelling, mode) in [
            ("normal", Mode::Normal),
            ("eval-first", Mode::EvalFirst),
            ("eval-only", Mode::EvalOnly),
        ] {
            assert_eq!(parse_enum::<Mode>(spelling).unwrap(), mode);
            assert_eq!(mode.as_str(), spelling);
        }
        for spelling in ["NORMAL", "eval_first", "evalfirst", "other"] {
            assert!(parse_enum::<Mode>(spelling).is_err(), "accepted {spelling}");
        }
    }

    #[test]
    fn other_policy_spellings_are_closed_snake_case() {
        for spelling in ["ask", "edits", "all"] {
            assert!(parse_enum::<ApprovalMode>(spelling).is_ok());
        }
        for spelling in ["inline", "fullscreen"] {
            assert!(parse_enum::<Screen>(spelling).is_ok());
        }
        for spelling in ["auto", "on", "off"] {
            assert!(parse_enum::<JudgeMode>(spelling).is_ok());
        }
        for spelling in ["ASK", "eval-first", "full_screen", "enabled"] {
            assert!(parse_enum::<ApprovalMode>(spelling).is_err());
            assert!(parse_enum::<Screen>(spelling).is_err());
            assert!(parse_enum::<JudgeMode>(spelling).is_err());
        }
    }

    #[test]
    fn mode_projection_matches_all_modes() {
        assert_eq!(
            Mode::Normal.projection(),
            ModeProjection {
                eval_first: false,
                non_eval_only: false,
            }
        );
        assert_eq!(
            Mode::EvalFirst.projection(),
            ModeProjection {
                eval_first: true,
                non_eval_only: false,
            }
        );
        assert_eq!(
            Mode::EvalOnly.projection(),
            ModeProjection {
                eval_first: false,
                non_eval_only: true,
            }
        );
    }
}
