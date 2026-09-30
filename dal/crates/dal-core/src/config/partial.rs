use super::layer::replace_if_present;
use super::{
    ApprovalMode, ConfigError, EditStyleInput, GuardCheckMode, JudgeMode, Mode, PathBuf,
    RulesConfig, Screen, TuiConfig, eval::EvalConfig, invalid_value,
};

pub(super) struct PartialGuard {
    pub(super) enabled: Option<bool>,
    pub(super) policies: Option<PartialGuardPolicies>,
}

/// Partial `[guard.policies]` layer.
#[derive(Default)]
pub(super) struct PartialGuardPolicies {
    pub(super) bands: Option<PartialGuardBands>,
    pub(super) g2_mode: Option<GuardCheckMode>,
    pub(super) g3_mode: Option<GuardCheckMode>,
    pub(super) g4_enabled: Option<bool>,
    pub(super) g8_calibrated_rules: Option<Vec<String>>,
    pub(super) erosion_report_threshold: Option<f64>,
    pub(super) churn_turn_threshold: Option<u32>,
}

/// Partial `[guard.policies.bands]` layer.
#[derive(Default)]
pub(super) struct PartialGuardBands {
    pub(super) cognitive: Option<u32>,
    pub(super) cyclomatic: Option<u32>,
    pub(super) function_ploc: Option<u32>,
    pub(super) nesting: Option<u32>,
    pub(super) file_ploc: Option<u32>,
}

#[derive(Default)]
pub(super) struct ParsedLayer {
    pub(super) mode: Option<Mode>,
    pub(super) model: Option<Box<str>>,
    pub(super) thinking: Option<crate::model::ThinkingLevel>,
    pub(super) approval: Option<ApprovalMode>,
    pub(super) screen: Option<Screen>,
    pub(super) tui: Option<TuiConfig>,
    pub(super) theme: Option<Box<str>>,
    pub(super) sandbox: Option<bool>,
    pub(super) images: Option<bool>,
    pub(super) motion: Option<bool>,
    pub(super) compact_ratio: Option<f64>,
    pub(super) edit_style: Option<EditStyleInput>,
    pub(super) guard: Option<PartialGuard>,
    pub(super) search_symbols: Option<bool>,
    pub(super) plugins: Option<Vec<Box<str>>>,
    pub(super) aliases: Option<std::collections::BTreeMap<Box<str>, Box<str>>>,
    pub(super) serve: Option<PartialServe>,
    pub(super) rules: Option<PartialRules>,
    pub(super) prices: std::collections::BTreeMap<Box<str>, PartialPrice>,
    pub(super) disabled_batteries: Option<Vec<Box<str>>>,
    pub(super) experimental_batteries: Option<Vec<Box<str>>>,
    pub(super) eval: Option<EvalConfig>,
    pub(super) sections: std::collections::BTreeMap<Box<str>, toml::Value>,
}

#[derive(Default)]
pub(super) struct PartialServe {
    pub(super) bind: Option<Box<str>>,
    pub(super) port: Option<u16>,
    pub(super) token_file: Option<PathBuf>,
    pub(super) approval: Option<ApprovalMode>,
    pub(super) origins: Option<Vec<Box<str>>>,
}

#[derive(Default)]
pub(super) struct PartialRules {
    pub(super) watch: Option<bool>,
    pub(super) interrupt: Option<crate::ext::InterruptMode>,
    pub(super) repeat: Option<crate::ext::RepeatMode>,
    pub(super) repeat_gap: Option<u16>,
    pub(super) max_retries: Option<u32>,
    pub(super) disabled: Option<Vec<Box<str>>>,
    pub(super) judge: Option<JudgeMode>,
}

impl PartialRules {
    /// Replaces each leaf present in this layer; `disabled` replaces as a whole.
    pub(super) fn apply_to(self, target: &mut RulesConfig) {
        replace_if_present(&mut target.watch, self.watch);
        replace_if_present(&mut target.interrupt, self.interrupt);
        replace_if_present(&mut target.repeat, self.repeat);
        replace_if_present(&mut target.repeat_gap, self.repeat_gap);
        replace_if_present(&mut target.max_retries, self.max_retries);
        replace_if_present(&mut target.disabled, self.disabled);
        replace_if_present(&mut target.judge, self.judge);
    }
}

#[derive(Default)]
pub(super) struct PartialPrice {
    pub(super) input: Option<f64>,
    pub(super) cached_input: Option<f64>,
    pub(super) output: Option<f64>,
    pub(super) reasoning: Option<f64>,
}

impl PartialPrice {
    /// Folds one config layer into another: a rate present in `later` wins,
    /// including an explicit `0.0`; an absent rate keeps the earlier value.
    pub(super) fn merge(&mut self, later: &Self) {
        self.input = later.input.or(self.input);
        self.cached_input = later.cached_input.or(self.cached_input);
        self.output = later.output.or(self.output);
        self.reasoning = later.reasoning.or(self.reasoning);
    }

    pub(super) fn finish(self, model_id: &str) -> Result<crate::model::ModelPrice, ConfigError> {
        match (self.input, self.cached_input, self.output, self.reasoning) {
            (Some(input), Some(cached_input), Some(output), Some(reasoning)) => {
                Ok(crate::model::ModelPrice {
                    input,
                    cached_input,
                    output,
                    reasoning,
                })
            }
            (input, cached_input, output, reasoning) => {
                let missing = [
                    input.is_none().then_some("input"),
                    cached_input.is_none().then_some("cached_input"),
                    output.is_none().then_some("output"),
                    reasoning.is_none().then_some("reasoning"),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
                Err(invalid_value(
                    &format!("prices.{model_id}"),
                    format!("missing {}", missing.join(", ")),
                    "all four rates: input, cached_input, output, reasoning",
                ))
            }
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FileText {
    pub(super) mode: Option<toml::Value>,
    pub(super) model: Option<toml::Value>,
    pub(super) thinking: Option<toml::Value>,
    pub(super) approval: Option<toml::Value>,
    pub(super) screen: Option<toml::Value>,
    pub(super) tui: Option<toml::Value>,
    pub(super) theme: Option<toml::Value>,
    pub(super) sandbox: Option<toml::Value>,
    pub(super) images: Option<toml::Value>,
    pub(super) motion: Option<toml::Value>,
    pub(super) compact_ratio: Option<toml::Value>,
    pub(super) edit_style: Option<toml::Value>,
    pub(super) guard: Option<toml::Value>,
    pub(super) search_symbols: Option<toml::Value>,
    pub(super) judge: Option<toml::Value>,
    pub(super) plugins: Option<toml::Value>,
    pub(super) aliases: Option<toml::Value>,
    pub(super) serve: Option<toml::Value>,
    pub(super) prices: Option<toml::Value>,
    pub(super) rules: Option<toml::Value>,
    pub(super) disabled_batteries: Option<toml::Value>,
    pub(super) experimental_batteries: Option<toml::Value>,
    pub(super) sandbox_writable: Option<toml::Value>,
    pub(super) agents: Option<toml::Value>,
    pub(super) limits: Option<toml::Value>,
    pub(super) models: Option<toml::Value>,
    pub(super) retry: Option<toml::Value>,
    pub(super) providers: Option<toml::Value>,
    pub(super) ask: Option<toml::Value>,
    pub(super) compact: Option<toml::Value>,
    pub(super) rule_sets: Option<toml::Value>,
    pub(super) plugin: Option<toml::Value>,
    pub(super) eval: Option<toml::Value>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ServeText {
    pub(super) bind: Option<toml::Value>,
    pub(super) port: Option<toml::Value>,
    pub(super) token_file: Option<toml::Value>,
    pub(super) approval: Option<toml::Value>,
    pub(super) origins: Option<toml::Value>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RulesText {
    pub(super) watch: Option<toml::Value>,
    pub(super) interrupt: Option<toml::Value>,
    pub(super) repeat: Option<toml::Value>,
    pub(super) repeat_gap: Option<toml::Value>,
    pub(super) max_retries: Option<toml::Value>,
    pub(super) disabled: Option<toml::Value>,
    pub(super) judge: Option<toml::Value>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PriceText {
    pub(super) input: Option<toml::Value>,
    pub(super) cached_input: Option<toml::Value>,
    pub(super) output: Option<toml::Value>,
    pub(super) reasoning: Option<toml::Value>,
}
