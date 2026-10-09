// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! Strict decoding of the product's battery sections.

use std::error::Error;

use dal_tools::{Calibration, GuardConfig, GuardParts};
use dalgona_batteries::history::{self, HistoryConfig};
use dalgona_batteries::judged::JudgedConfig;
use dalgona_batteries::mcp::McpSettings;
use dalgona_batteries::orchestration::{self, OrchestrationConfig};
use dalgona_batteries::quality::QualityConfig;
use dalgona_batteries::review::ReviewConfig;
use dalgona_batteries::ttsr_rules;
use dalgona_batteries::web::{self, WebConfig};
use dalgona_batteries::work::PlanConfig;

/// A `[rule_sets]` value that is not a list of set names.
#[derive(Debug, thiserror::Error)]
#[error("dalgona: config.toml: rule_sets.enabled {value} is invalid\n{hint}")]
struct InvalidRuleSetList {
    value: String,
    hint: String,
}

/// A `[rule_sets]` key that the product does not read.
#[derive(Debug, thiserror::Error)]
#[error("dalgona: config.toml: rule_sets has an unknown key \"{key}\"\nUse only `enabled`.")]
struct UnknownRuleSetKey {
    key: String,
}
/// An unsupported plugin section or key.
#[derive(Debug, thiserror::Error)]
enum PluginConfigError {
    #[error("[plugin] must be a table")]
    NotATable,
    #[error("[plugin.{plugin}] does not accept key \"{key}\"")]
    UnknownKey { plugin: String, key: String },
    #[error("configuration for battery \"{plugin}\" belongs under [plugin.plan]")]
    WrongSection { plugin: String },
}

/// Every decoded battery section.
pub(crate) struct Sections {
    pub(crate) history: HistoryConfig,
    pub(crate) judged: JudgedConfig,
    pub(crate) mcp: McpSettings,
    pub(crate) orchestration: OrchestrationConfig,
    pub(crate) quality: QualityConfig,
    pub(crate) review: ReviewConfig,
    pub(crate) plan: PlanConfig,
    pub(crate) web: WebConfig,
    pub(crate) rule_sets: Vec<String>,
}

fn section_error(section: &str, source: impl Error + Send + Sync + 'static) -> dalgon::BuildError {
    dalgon::BuildError::Section {
        section: section.into(),
        source: Box::new(source),
    }
}

impl Sections {
    /// Decodes every section, rejecting unknown keys and invalid values.
    pub(crate) fn decode(config: &dalgon::Config) -> Result<Self, dalgon::BuildError> {
        validate_plugin_sections(config)?;
        Ok(Self {
            history: history::parse_config(config.section("plugin.history"))
                .map_err(|source| section_error("plugin.history", source))?,
            judged: JudgedConfig::from_config(config.section("plugin.judged"))
                .map_err(|source| section_error("plugin.judged", source))?,
            mcp: McpSettings::parse(config.section("plugin.mcp"))
                .map_err(|source| section_error("plugin.mcp", source))?,
            orchestration: orchestration::parse_config(config.section("plugin.orchestration"))
                .map_err(|source| section_error("plugin.orchestration", source))?,
            quality: QualityConfig::parse_config(config.section("plugin.quality"))
                .map_err(|source| section_error("plugin.quality", source))?,
            review: ReviewConfig::parse_config(config.section("plugin.review"))
                .map_err(|source| section_error("plugin.review", source))?,
            plan: PlanConfig::parse_config(config.section("plugin.plan"))
                .map_err(|source| section_error("plugin.plan", source))?,
            web: web::parse_config(config.section("plugin.web"))
                .map_err(|source| section_error("plugin.web", source))?,
            rule_sets: rule_sets(config.section("rule_sets"))?,
        })
    }
}

/// Builds the guard extension, its patch observer, and the findings handle.
pub(crate) fn guard(config: &dalgon::Config) -> Result<GuardParts, dalgon::BuildError> {
    let settings = GuardConfig::from_section(config.guard(), &Calibration::none())
        .map_err(|source| section_error("guard", source))?;
    Ok(dal_tools::guard_extension(settings)?)
}

fn has_configured_plugin(config: &dalgon::Config, name: &str) -> bool {
    config
        .plugins()
        .iter()
        .any(|configured| configured.as_ref() == name)
}

fn empty_plugin_section(config: &dalgon::Config, name: &str) -> Result<(), dalgon::BuildError> {
    let section_name = format!("plugin.{name}");
    let Some(section) = config.section(&section_name) else {
        return Ok(());
    };
    let Some(table) = section.as_table() else {
        return Err(section_error(&section_name, PluginConfigError::NotATable));
    };
    if let Some(key) = table.keys().next() {
        return Err(section_error(
            &section_name,
            PluginConfigError::UnknownKey {
                plugin: name.to_owned(),
                key: key.clone(),
            },
        ));
    }
    Ok(())
}

/// Validates Dalgona-owned plugin sections and leaves user plugin tables to dal.
fn validate_plugin_sections(config: &dalgon::Config) -> Result<(), dalgon::BuildError> {
    if config
        .section("plugin")
        .is_some_and(|section| section.as_table().is_none())
    {
        return Err(section_error("plugin", PluginConfigError::NotATable));
    }
    for (name, _) in config.plugin_configs() {
        match name {
            "work" if !has_configured_plugin(config, name) => {
                return Err(section_error(
                    "plugin.work",
                    PluginConfigError::WrongSection {
                        plugin: name.to_owned(),
                    },
                ));
            }
            "ask" | "skills" | "ttsr-rules" if !has_configured_plugin(config, name) => {
                empty_plugin_section(config, name)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn invalid_list(value: &toml::Value) -> dalgon::BuildError {
    section_error(
        "rule_sets",
        InvalidRuleSetList {
            value: value.to_string(),
            hint: ttsr_rules::NOT_A_LIST_HINT.to_owned(),
        },
    )
}

fn default_rule_sets() -> Vec<String> {
    ttsr_rules::DEFAULT_ENABLED
        .iter()
        .map(|name| (*name).to_owned())
        .collect()
}

fn rule_sets(section: Option<&toml::Value>) -> Result<Vec<String>, dalgon::BuildError> {
    let Some(section) = section else {
        return Ok(default_rule_sets());
    };
    let Some(table) = section.as_table() else {
        return Err(invalid_list(section));
    };
    if let Some(key) = table.keys().find(|key| key.as_str() != "enabled") {
        return Err(section_error(
            "rule_sets",
            UnknownRuleSetKey { key: key.clone() },
        ));
    }
    let Some(enabled) = table.get("enabled") else {
        return Ok(default_rule_sets());
    };
    let Some(values) = enabled.as_array() else {
        return Err(invalid_list(enabled));
    };
    let names = values
        .iter()
        .map(|value| value.as_str().map(str::to_owned))
        .collect::<Option<Vec<String>>>()
        .ok_or_else(|| invalid_list(enabled))?;
    ttsr_rules::validate_enabled(&names).map_err(|source| section_error("rule_sets", source))?;
    Ok(names)
}
