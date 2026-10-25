// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! The editable, documented, non-secret dalgona settings.
//!
//! Every row is read from the strictly decoded battery sections, so the
//! catalog doubles as the product validation of a candidate `dal.toml`: a
//! configuration this function refuses is one `dalgona` refuses to start
//! with. Credential pointers (`[plugin.web] api_key_env`), saved workflows,
//! `experimental_batteries`, and plugin tables dalgona does not own are
//! never listed.

use dal_core::{ProductSetting, SettingControl, SettingValue};
use dalgona_batteries::history::{self, HistoryConfig};
use dalgona_batteries::judged::JudgedConfig;
use dalgona_batteries::mcp::McpSettings;
use dalgona_batteries::orchestration::{OrchestrationConfig, OrchestrationMonitorConfig};
use dalgona_batteries::procs::ProcsConfig;
use dalgona_batteries::review::ReviewConfig;
use dalgona_batteries::ttsr_rules;
use dalgona_batteries::web::WebConfig;
use dalgona_batteries::work::PlanConfig;

use crate::sections::{Sections, section_error};

fn row(
    key: &str,
    group: &str,
    label: &str,
    value: SettingValue,
    control: SettingControl,
) -> ProductSetting {
    ProductSetting {
        key: key.into(),
        group: group.into(),
        label: label.into(),
        value,
        running: None,
        control,
    }
}

fn toggle(key: &str, group: &str, label: &str, value: bool) -> ProductSetting {
    row(
        key,
        group,
        label,
        SettingValue::Bool(value),
        SettingControl::Toggle,
    )
}

fn integer(key: &str, group: &str, label: &str, value: impl Into<u64>) -> ProductSetting {
    let value = i64::try_from(value.into()).unwrap_or(i64::MAX);
    row(
        key,
        group,
        label,
        SettingValue::Integer(value),
        SettingControl::Integer,
    )
}

fn names(values: &[&str]) -> Vec<Box<str>> {
    values.iter().map(|name| Box::<str>::from(*name)).collect()
}

/// Decodes `config` exactly as the product build does and returns its rows.
///
/// # Errors
/// Returns the build error naming the first battery name, section, or key
/// the product refuses, including an invalid `[plugin.history]` section,
/// which startup only warns about but an edit must not write.
pub(crate) fn catalog(config: &dalgon::Config) -> Result<Vec<ProductSetting>, dalgon::BuildError> {
    crate::battery_names::validate(config, crate::BATTERY_NAMES)?;
    let sections = Sections::decode_with(config, &mut std::io::sink())?;
    let history = history::parse_config(config.section("plugin.history"))
        .map_err(|source| section_error("plugin.history", source))?;
    // Every start decodes `[guard]` in the shared dal parts; refuse here what
    // that start would refuse.
    dal_tools::GuardConfig::from_section(config.guard(), &dal_tools::Calibration::none())
        .map_err(|source| section_error("guard", source))?;
    let Sections {
        judged,
        mcp,
        orchestration,
        procs,
        review,
        plan,
        web,
        rule_sets,
        ..
    } = sections;
    let mut rows = Vec::new();
    basic_rows(&mut rows, config, rule_sets, &history, judged, &mcp);
    orchestration_rows(&mut rows, &orchestration);
    review_and_web_rows(&mut rows, &plan, &procs, review, web);
    Ok(rows)
}

fn basic_rows(
    rows: &mut Vec<ProductSetting>,
    config: &dalgon::Config,
    rule_sets: Vec<String>,
    history: &HistoryConfig,
    judged: JudgedConfig,
    mcp: &McpSettings,
) {
    rows.extend([
        row(
            "disabled_batteries",
            "batteries",
            "disabled batteries",
            SettingValue::List(config.disabled_batteries().to_vec()),
            SettingControl::Subset(names(crate::BATTERY_NAMES)),
        ),
        row(
            "rule_sets.enabled",
            "ttsr-rules",
            "enabled rule sets",
            SettingValue::List(rule_sets.into_iter().map(Into::into).collect()),
            SettingControl::Subset(names(ttsr_rules::SETS)),
        ),
        toggle(
            "search_symbols",
            "search",
            "symbol search",
            config.search_symbols(),
        ),
        toggle(
            "guard.enabled",
            "guard",
            "guard enabled",
            config.guard().enabled,
        ),
        toggle(
            "plugin.history.enabled",
            "history",
            "section enabled",
            matches!(history, HistoryConfig::Enabled { .. }),
        ),
    ]);
    if let HistoryConfig::Enabled { share } = history {
        rows.push(row(
            "plugin.history.share",
            "history",
            "image share",
            SettingValue::Number(*share),
            SettingControl::Number,
        ));
    }
    rows.extend([
        toggle(
            "plugin.judged.thinking",
            "judged",
            "auto thinking",
            judged.thinking,
        ),
        toggle(
            "plugin.judged.ranking",
            "judged",
            "search ranking",
            judged.ranking,
        ),
        toggle(
            "plugin.judged.ask_anchor",
            "judged",
            "anchored ask",
            judged.ask_anchor,
        ),
        toggle(
            "plugin.judged.claim_check",
            "judged",
            "claim check",
            judged.claim_check,
        ),
        toggle(
            "plugin.judged.dedup",
            "judged",
            "reminder dedup",
            judged.dedup,
        ),
        toggle("plugin.mcp.enabled", "mcp", "section enabled", mcp.enabled),
    ]);
}

fn orchestration_monitor_rows(
    rows: &mut Vec<ProductSetting>,
    monitor: &OrchestrationMonitorConfig,
) {
    rows.extend([
        toggle(
            "plugin.orchestration.monitor.enabled",
            "orchestration",
            "job monitor",
            monitor.enabled,
        ),
        integer(
            "plugin.orchestration.monitor.coalesce_ms",
            "orchestration",
            "monitor coalesce ms",
            monitor.coalesce_ms,
        ),
        integer(
            "plugin.orchestration.monitor.rate_limit_ms",
            "orchestration",
            "monitor rate limit ms",
            monitor.rate_limit_ms,
        ),
        integer(
            "plugin.orchestration.monitor.max_lines",
            "orchestration",
            "monitor max lines",
            u64::try_from(monitor.max_lines).unwrap_or(u64::MAX),
        ),
        integer(
            "plugin.orchestration.monitor.max_chars",
            "orchestration",
            "monitor max chars",
            u64::try_from(monitor.max_chars).unwrap_or(u64::MAX),
        ),
        integer(
            "plugin.orchestration.monitor.wake_budget",
            "orchestration",
            "monitor wake budget",
            monitor.wake_budget,
        ),
    ]);
}

fn orchestration_rows(rows: &mut Vec<ProductSetting>, orchestration: &OrchestrationConfig) {
    rows.extend([
        toggle(
            "plugin.orchestration.loop_guard.enabled",
            "orchestration",
            "loop guard",
            orchestration.loop_guard.enabled,
        ),
        toggle(
            "plugin.orchestration.sleep.enabled",
            "orchestration",
            "sleep rewrite",
            orchestration.sleep.enabled,
        ),
    ]);
    orchestration_monitor_rows(rows, &orchestration.monitor);
    rows.extend([
        toggle(
            "plugin.orchestration.inflight.enabled",
            "orchestration",
            "inflight status",
            orchestration.inflight.enabled,
        ),
        toggle(
            "plugin.orchestration.goal.enabled",
            "orchestration",
            "goals",
            orchestration.goal.enabled,
        ),
        toggle(
            "plugin.orchestration.arbiter.enabled",
            "orchestration",
            "turn arbiter",
            orchestration.arbiter.enabled,
        ),
        toggle(
            "plugin.orchestration.agents.enabled",
            "orchestration",
            "child agents",
            orchestration.agents.enabled,
        ),
        integer(
            "plugin.orchestration.agents.child_max_steps",
            "orchestration",
            "child max steps",
            orchestration.agents.child_max_steps,
        ),
        integer(
            "plugin.orchestration.agents.child_max_minutes",
            "orchestration",
            "child max minutes",
            orchestration.agents.child_max_minutes,
        ),
        integer(
            "plugin.orchestration.agents.max_runs",
            "orchestration",
            "max runs",
            orchestration.agents.max_runs,
        ),
        toggle(
            "plugin.orchestration.isolation.enabled",
            "orchestration",
            "worktree isolation",
            orchestration.isolation.enabled,
        ),
    ]);
}

fn review_and_web_rows(
    rows: &mut Vec<ProductSetting>,
    plan: &PlanConfig,
    procs: &ProcsConfig,
    review: ReviewConfig,
    web: WebConfig,
) {
    rows.extend([
        toggle(
            "plugin.plan.enabled",
            "work",
            "section enabled",
            plan.enabled,
        ),
        toggle(
            "plugin.procs.enabled",
            "procs",
            "section enabled",
            procs.enabled,
        ),
        toggle(
            "plugin.review.enabled",
            "review",
            "section enabled",
            review.enabled,
        ),
        integer(
            "plugin.review.max_rounds",
            "review",
            "max rounds",
            review.max_rounds,
        ),
        row(
            "plugin.review.reviewer_model",
            "review",
            "reviewer model",
            SettingValue::Text(review.reviewer_model.into()),
            SettingControl::Text,
        ),
        row(
            "plugin.review.diff_base",
            "review",
            "diff base",
            SettingValue::Text(review.diff_base.into()),
            SettingControl::Text,
        ),
        toggle("plugin.web.enabled", "web", "section enabled", web.enabled),
        row(
            "plugin.web.provider",
            "web",
            "search provider",
            SettingValue::Text(web.provider.into()),
            // The providers `WebConfig::provider` documents; the web decoder
            // still refuses any other spelling before a save.
            SettingControl::Choice(names(&["brave", "tavily"])),
        ),
        row(
            "plugin.web.search_depth",
            "web",
            "tavily search depth",
            // An unset depth is the documented `basic`.
            SettingValue::Text(web.search_depth.as_deref().unwrap_or("basic").into()),
            SettingControl::Choice(names(&["basic", "advanced"])),
        ),
        integer(
            "plugin.web.timeout_secs",
            "web",
            "timeout secs",
            web.timeout_secs,
        ),
        integer(
            "plugin.web.max_redirects",
            "web",
            "max redirects",
            web.max_redirects,
        ),
        integer(
            "plugin.web.max_body_bytes",
            "web",
            "max body bytes",
            u64::try_from(web.max_body_bytes).unwrap_or(u64::MAX),
        ),
        integer(
            "plugin.web.max_markdown_bytes",
            "web",
            "max markdown bytes",
            u64::try_from(web.max_markdown_bytes).unwrap_or(u64::MAX),
        ),
    ]);
}

#[cfg(test)]
mod tests {
    use dal_core::{ConfigProduct, ProductSetting, SettingControl, SettingValue};

    fn load(user_toml: &str) -> dalgon::Config {
        dalgon::Config::load(
            ConfigProduct::Dalgona,
            &std::env::temp_dir(),
            crate::DEFAULTS,
            Some(user_toml),
        )
        .expect("configuration loads")
    }

    fn find<'a>(rows: &'a [ProductSetting], key: &str) -> &'a ProductSetting {
        rows.iter()
            .find(|row| row.key.as_ref() == key)
            .unwrap_or_else(|| panic!("{key} is listed"))
    }

    #[test]
    fn rows_carry_the_effective_values_of_every_layer() {
        let rows = super::catalog(&load(
            "disabled_batteries = [\"web\"]\n[plugin.web]\ntimeout_secs = 45\n[plugin.procs]\nenabled = false\n",
        ))
        .expect("catalog decodes");
        assert_eq!(
            find(&rows, "disabled_batteries").value,
            SettingValue::List(vec!["web".into()])
        );
        assert_eq!(
            find(&rows, "disabled_batteries").control,
            SettingControl::Subset(super::names(crate::BATTERY_NAMES))
        );
        assert_eq!(
            find(&rows, "plugin.web.timeout_secs").value,
            SettingValue::Integer(45)
        );
        assert_eq!(
            find(&rows, "plugin.web.max_redirects").value,
            SettingValue::Integer(10)
        );
        assert_eq!(
            find(&rows, "plugin.procs.enabled").value,
            SettingValue::Bool(false)
        );
        assert_eq!(
            find(&rows, "plugin.history.share").value,
            SettingValue::Number(0.4)
        );
        assert_eq!(
            find(&rows, "search_symbols").value,
            SettingValue::Bool(true)
        );
        assert_eq!(
            find(&rows, "plugin.web.search_depth").value,
            SettingValue::Text("basic".into())
        );
        assert!(rows.iter().all(|row| row.running.is_none()));
    }

    #[test]
    fn credentials_workflows_and_experimental_lists_are_never_listed() {
        let rows = super::catalog(&load("")).expect("catalog decodes");
        for key in [
            "plugin.web.api_key_env",
            "experimental_batteries",
            "plugin.orchestration.workflows",
        ] {
            assert!(
                rows.iter().all(|row| row.key.as_ref() != key),
                "{key} is hidden"
            );
        }
    }

    #[test]
    fn the_catalog_refuses_what_the_product_refuses() {
        for (user_toml, needle) in [
            ("[plugin.web]\ntimeout_secs = 301\n", "timeout_secs"),
            ("disabled_batteries = [\"webb\"]\n", "webb"),
            ("[plugin.history]\nshare = 0.9\n", "plugin.history"),
        ] {
            let error = super::catalog(&load(user_toml))
                .err()
                .unwrap_or_else(|| panic!("{user_toml} must be refused"));
            assert!(
                error.to_string().contains(needle) || format!("{error:?}").contains(needle),
                "{error}"
            );
        }
    }
}
