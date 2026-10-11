//! Guard table types for `[guard]` configuration.

/// The `[guard]` table as plain data, decoded strictly.
///
/// Unknown keys at any depth fail config load with
/// `unknown guard policy key <key>`.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields, default)]
pub struct GuardSection {
    /// Whether the guard extension is enabled.
    pub enabled: bool,
    /// Policy thresholds and modes.
    pub policies: GuardPolicies,
}

/// Policy thresholds for the guard extension.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields, default)]
pub struct GuardPolicies {
    /// Metric bands for complexity checks.
    pub bands: GuardBands,
    /// How G2 findings are reported.
    pub g2_mode: GuardCheckMode,
    /// How G3 findings are reported.
    pub g3_mode: GuardCheckMode,
    /// Whether the tiny-helper hint is enabled.
    pub g4_enabled: bool,
    /// Calibrated G8 rules that may block.
    pub g8_calibrated_rules: Vec<String>,
    /// Erosion share that triggers a report.
    pub erosion_report_threshold: f64,
    /// Turn count that triggers churn reporting.
    pub churn_turn_threshold: u32,
}

impl Default for GuardPolicies {
    fn default() -> Self {
        Self {
            bands: GuardBands::default(),
            g2_mode: GuardCheckMode::Report,
            g3_mode: GuardCheckMode::Report,
            g4_enabled: false,
            g8_calibrated_rules: Vec::new(),
            erosion_report_threshold: 0.05,
            churn_turn_threshold: 3,
        }
    }
}

/// Metric bands for the guard extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields, default)]
pub struct GuardBands {
    /// Cognitive complexity band.
    pub cognitive: u32,
    /// Cyclomatic complexity band.
    pub cyclomatic: u32,
    /// Function physical-lines band.
    pub function_ploc: u32,
    /// Nesting depth band.
    pub nesting: u32,
    /// File physical-lines band.
    pub file_ploc: u32,
}

impl Default for GuardBands {
    fn default() -> Self {
        Self {
            cognitive: 15,
            cyclomatic: 15,
            function_ploc: 50,
            nesting: 4,
            file_ploc: 500,
        }
    }
}

/// How a guard check reports when it fires.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum GuardCheckMode {
    /// Report the finding without blocking.
    #[default]
    Report,
    /// Block once the rule is calibrated.
    BlockAfterCalibration,
}
