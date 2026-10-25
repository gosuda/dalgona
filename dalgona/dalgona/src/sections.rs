// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! Strict decoding of the product's battery sections.

use std::error::Error;
use std::io::Write;

use dal_tools::{Calibration, GuardConfig, GuardParts};
use dalgona_batteries::history::{self, HistoryConfig};
use dalgona_batteries::judged::JudgedConfig;
use dalgona_batteries::mcp::McpSettings;
use dalgona_batteries::orchestration::{self, OrchestrationConfig};
use dalgona_batteries::procs::ProcsConfig;
use dalgona_batteries::quality::QualityConfig;
use dalgona_batteries::review::ReviewConfig;
use dalgona_batteries::ttsr_rules;
use dalgona_batteries::web::{self, WebConfig};
use dalgona_batteries::work::PlanConfig;

/// A `[rule_sets]` value that is not a list of set names.
#[derive(Debug, thiserror::Error)]
#[error("dalgona: dal.toml: rule_sets.enabled {value} is invalid\n{hint}")]
struct InvalidRuleSetList {
    value: String,
    hint: String,
}

/// A `[rule_sets]` key that the product does not read.
#[derive(Debug, thiserror::Error)]
#[error("dalgona: dal.toml: rule_sets has an unknown key \"{key}\"\nUse only `enabled`.")]
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
    pub(crate) procs: ProcsConfig,
    pub(crate) quality: QualityConfig,
    pub(crate) review: ReviewConfig,
    pub(crate) plan: PlanConfig,
    pub(crate) web: WebConfig,
    pub(crate) rule_sets: Vec<String>,
}

pub(crate) fn section_error(
    section: &str,
    source: impl Error + Send + Sync + 'static,
) -> dalgon::BuildError {
    dalgon::BuildError::Section {
        section: section.into(),
        source: Box::new(source),
    }
}

/// Decodes `[plugin.history]`, the one battery section that does not abort startup.
///
/// The history battery documents that an invalid section prints one startup
/// warning and then refuses every compaction with that same text. The typed
/// `HistoryConfigError` text becomes `HistoryConfig::Invalid`, so the battery
/// still registers its failing compactor and no consolidation task runs.
fn history_section(section: Option<&toml::Value>) -> HistoryConfig {
    match history::parse_config(section) {
        Ok(decoded) => decoded,
        Err(error) => HistoryConfig::Invalid {
            message: error.to_string().into_boxed_str(),
        },
    }
}

impl Sections {
    /// Decodes every section, rejecting unknown keys and invalid values.
    ///
    /// An invalid `[plugin.history]` section is the authored exception: it
    /// decodes to `HistoryConfig::Invalid` and prints one startup warning on
    /// stderr, but only after every other section has decoded, so a build that
    /// goes on to fail prints no warning.
    pub(crate) fn decode(config: &dalgon::Config) -> Result<Self, dalgon::BuildError> {
        Self::decode_with(config, &mut std::io::stderr().lock())
    }

    pub(crate) fn decode_with(
        config: &dalgon::Config,
        warnings: &mut dyn Write,
    ) -> Result<Self, dalgon::BuildError> {
        validate_plugin_sections(config)?;
        let sections = Self {
            history: history_section(config.section("plugin.history")),
            judged: JudgedConfig::from_config(config.section("plugin.judged"))
                .map_err(|source| section_error("plugin.judged", source))?,
            mcp: McpSettings::parse(config.section("plugin.mcp"))
                .map_err(|source| section_error("plugin.mcp", source))?,
            orchestration: orchestration::parse_config(config.section("plugin.orchestration"))
                .map_err(|source| section_error("plugin.orchestration", source))?,
            procs: ProcsConfig::parse_config(config.section("plugin.procs"))
                .map_err(|source| section_error("plugin.procs", source))?,
            quality: QualityConfig::parse_config(config.section("plugin.quality"))
                .map_err(|source| section_error("plugin.quality", source))?,
            review: ReviewConfig::parse_config(config.section("plugin.review"))
                .map_err(|source| section_error("plugin.review", source))?,
            plan: PlanConfig::parse_config(config.section("plugin.plan"))
                .map_err(|source| section_error("plugin.plan", source))?,
            web: web::parse_config(config.section("plugin.web"))
                .map_err(|source| section_error("plugin.web", source))?,
            rule_sets: rule_sets(config.section("rule_sets"))?,
        };
        if let HistoryConfig::Invalid { message } = &sections.history {
            // A closed stderr must not abort startup; the same text still
            // reaches every compaction refusal.
            let _ = writeln!(warnings, "{message}");
        }
        Ok(sections)
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

#[cfg(test)]
mod tests {
    use dal_core::{Config, ConfigProduct};
    use dalgona_batteries::history::{HistoryConfig, HistoryConfigError};

    use super::Sections;

    fn config(user_toml: &str) -> Config {
        Config::load(
            ConfigProduct::Dalgona,
            &std::env::temp_dir(),
            crate::DEFAULTS,
            Some(user_toml),
        )
        .expect("fixture configuration loads")
    }

    /// Decodes `user_toml` and returns the sections with every warning line written.
    fn decode(user_toml: &str) -> (Sections, String) {
        let mut warnings = Vec::new();
        let sections =
            Sections::decode_with(&config(user_toml), &mut warnings).expect("startup continues");
        (
            sections,
            String::from_utf8(warnings).expect("warnings are text"),
        )
    }

    #[test]
    fn invalid_history_section_warns_once_and_keeps_startup_going() {
        for (body, error) in [
            ("enabled = \"yes\"", HistoryConfigError::Enabled),
            ("share = 0.9", HistoryConfigError::Share),
            ("share = \"lots\"", HistoryConfigError::Share),
            (
                "unknown = true",
                HistoryConfigError::UnknownKey("unknown".to_owned()),
            ),
        ] {
            let (sections, warnings) = decode(&format!("[plugin.history]\n{body}\n"));
            let text = error.to_string();
            assert_eq!(
                sections.history,
                HistoryConfig::Invalid {
                    message: text.as_str().into(),
                },
                "{body}"
            );
            assert_eq!(warnings, format!("{text}\n"), "{body}");
        }
    }

    #[test]
    fn valid_history_sections_decode_without_a_warning() {
        let (defaulted, warnings) = decode("");
        assert!(matches!(defaulted.history, HistoryConfig::Enabled { .. }));
        assert_eq!(warnings, "");
        let (sized, warnings) = decode("[plugin.history]\nshare = 0.5\n");
        assert_eq!(sized.history, HistoryConfig::Enabled { share: 0.5 });
        assert_eq!(warnings, "");
        let (off, warnings) = decode("[plugin.history]\nenabled = false\n");
        assert_eq!(off.history, HistoryConfig::Disabled);
        assert_eq!(warnings, "");
    }

    /// The refusal text for `[plugin.web]` overrides, or `None` when they decode.
    fn web_refusal(body: &str) -> Option<String> {
        let mut warnings = Vec::new();
        Sections::decode_with(&config(&format!("[plugin.web]\n{body}\n")), &mut warnings)
            .err()
            .map(|error| error.to_string())
    }

    #[test]
    fn the_embedded_defaults_decode_to_the_documented_web_limits() {
        let (sections, warnings) = decode("");
        let web = sections.web;
        assert!(web.enabled);
        assert_eq!(web.provider, "brave");
        assert_eq!(web.api_key_env, "BRAVE_API_KEY");
        assert_eq!(web.search_depth, None);
        assert_eq!(web.timeout_secs, 30);
        assert_eq!(web.max_redirects, 10);
        assert_eq!(web.max_body_bytes, 2_097_152);
        assert_eq!(web.max_markdown_bytes, 131_072);
        assert_eq!(warnings, "");
    }

    /// Attacks one limit from both sides: every `accepted` value decodes, every
    /// `refused` value fails with exactly `message`.
    fn assert_web_edge(key: &str, accepted: [i64; 3], refused: [i64; 2], message: &str) {
        for value in accepted {
            assert_eq!(
                web_refusal(&format!("{key} = {value}")),
                None,
                "{key} = {value}"
            );
        }
        for value in refused {
            assert_eq!(
                web_refusal(&format!("{key} = {value}")).as_deref(),
                Some(format!("invalid [plugin.web] config: {message}").as_str()),
                "{key} = {value}"
            );
        }
    }

    #[test]
    fn web_timeout_accepts_one_and_three_hundred_and_refuses_zero_and_one_past() {
        assert_web_edge(
            "timeout_secs",
            [1, 2, 300],
            [0, 301],
            "timeout_secs must be 1 to 300",
        );
    }

    #[test]
    fn web_redirects_accept_zero_and_twenty_and_refuse_one_past() {
        assert_web_edge(
            "max_redirects",
            [0, 1, 20],
            [21, 22],
            "max_redirects must be 0 to 20",
        );
    }

    #[test]
    fn web_body_ceiling_accepts_1024_and_refuses_1023() {
        assert_web_edge(
            "max_body_bytes",
            [1024, 1025, 2_097_152],
            [1023, 0],
            "max_body_bytes must be at least 1024",
        );
    }

    #[test]
    fn web_markdown_ceiling_accepts_one_and_refuses_zero() {
        assert_web_edge(
            "max_markdown_bytes",
            [1, 2, 131_072],
            [0, 0],
            "max_markdown_bytes must be at least 1",
        );
    }

    #[test]
    fn a_web_limit_that_is_negative_wrapped_or_mistyped_is_refused_not_coerced() {
        for body in [
            "timeout_secs = -1",
            "max_redirects = -1",
            "max_body_bytes = -1",
            "max_markdown_bytes = -1",
            "timeout_secs = 4294967297",
            "max_redirects = 4294967296",
            "timeout_secs = \"30\"",
            "timeout_secs = 30.5",
            "max_body_bytes = true",
        ] {
            let message = web_refusal(body).unwrap_or_default();
            assert!(
                message.starts_with("invalid [plugin.web] config: "),
                "{body}: {message:?}"
            );
            assert!(
                !message.contains("must be"),
                "{body} must fail in the decoder, not slip into a range check: {message}"
            );
        }
    }

    #[test]
    fn a_web_provider_and_depth_outside_the_closed_sets_are_refused() {
        for (body, message) in [
            (
                "provider = \"Brave\"",
                "unknown provider \"Brave\"; the providers are \"brave\" and \"tavily\"",
            ),
            (
                "provider = \"\"",
                "unknown provider \"\"; the providers are \"brave\" and \"tavily\"",
            ),
            (
                "search_depth = \"Advanced\"",
                "search_depth must be \"basic\" or \"advanced\"",
            ),
            ("api_key_env = \"\"", "api_key_env must not be empty"),
        ] {
            assert_eq!(
                web_refusal(body).as_deref(),
                Some(format!("invalid [plugin.web] config: {message}").as_str()),
                "{body}"
            );
        }
        assert_eq!(web_refusal("search_depth = \"advanced\""), None);
        assert_eq!(web_refusal("provider = \"tavily\""), None);
    }

    #[test]
    fn a_failing_neighbour_section_still_aborts_startup_without_a_history_warning() {
        let mut warnings = Vec::new();
        let user_toml = "[plugin.history]\nshare = 0.9\n[plugin.web]\nbogus = 1\n";
        let Err(error) = Sections::decode_with(&config(user_toml), &mut warnings) else {
            panic!("an unknown web key must abort startup");
        };
        assert!(error.to_string().contains("bogus"), "{error}");
        assert!(warnings.is_empty(), "no warning precedes a failed build");
    }
}
