// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! Batteries-included composition for the dalgona product.

mod battery_names;
mod docs;
mod eval_cmd;
mod eval_host;
mod plugins;
mod sections;

use dal_agent::ext::Extension;
use dalgona_batteries::{
    ask, history, judged, mcp, orchestration, quality, review, skills, ttsr_rules, web, work,
};
use sections::Sections;

const BATTERY_ASK: &str = "ask";
const BATTERY_HISTORY: &str = "history";
const BATTERY_JUDGED: &str = "judged";
const BATTERY_MCP: &str = "mcp";
const BATTERY_ORCHESTRATION: &str = "orchestration";
const BATTERY_QUALITY: &str = "quality";
const BATTERY_REVIEW: &str = "review";
const BATTERY_SKILLS: &str = "skills";
const BATTERY_TTSR_RULES: &str = "ttsr-rules";
const BATTERY_WEB: &str = "web";
const BATTERY_WORK: &str = "work";

const BATTERY_NAMES: &[&str] = &[
    BATTERY_ASK,
    BATTERY_HISTORY,
    BATTERY_JUDGED,
    BATTERY_MCP,
    BATTERY_ORCHESTRATION,
    BATTERY_QUALITY,
    BATTERY_REVIEW,
    BATTERY_SKILLS,
    BATTERY_TTSR_RULES,
    BATTERY_WEB,
    BATTERY_WORK,
];

const NAME: &str = "dalgona";
const DEFAULTS: &str = include_str!("defaults.toml");

/// Returns the product factory consumed by the shared dalgon process entry.
#[must_use]
pub fn product() -> dalgon::ProductFactory {
    dalgon::ProductFactory {
        binary: NAME,
        defaults: DEFAULTS,
        build,
        docs: docs::manuals,
    }
}

/// Runs the `dalgona` process: `dalgona eval ...` here, every other command
/// in the shared dalgon entry.
#[must_use]
pub fn run() -> std::process::ExitCode {
    let argv: Vec<std::ffi::OsString> = std::env::args_os().collect();
    match argv.get(1) {
        Some(command) if command == "eval" => eval_cmd::run(&argv[2..]),
        _ => dalgon::run(product()),
    }
}

/// Builds the dalgona product from edge-resolved paths and layered config.
///
/// Every battery-owned `[plugin.*]` and `[rule_sets]` section is decoded
/// strictly before any battery is constructed. `disabled_batteries` omits the
/// named batteries. Most battery-specific `enabled` switches also omit their
/// extension; history remains registered without its compactor when disabled.
///
/// # Errors
/// Returns a config or registration error from the shared dalgon builder.
pub fn build(cx: &dalgon::BuildCx<'_>) -> Result<dal_agent::Product, dalgon::BuildError> {
    battery_names::validate(cx.config, BATTERY_NAMES)?;
    let sections = Sections::decode(cx.config)?;
    let mut parts = dalgon::parts(cx)?;
    plugins::require_listed(cx, &parts.bundled)?;
    let batteries = compose(cx, sections, &mut parts)?;
    parts.batteries = batteries;
    let mut product = dalgon::assemble(cx, parts)?;
    product.name = NAME;
    product.defaults = DEFAULTS;
    Ok(product)
}

fn selected(cx: &dalgon::BuildCx<'_>, battery: &str) -> bool {
    !cx.config
        .disabled_batteries()
        .iter()
        .any(|name| name.as_ref() == battery)
}

fn compose(
    cx: &dalgon::BuildCx<'_>,
    sections: Sections,
    parts: &mut dalgon::Parts,
) -> Result<Vec<Extension>, dalgon::BuildError> {
    let Sections {
        history: history_config,
        judged: judged_config,
        mcp: mcp_settings,
        orchestration: mut orchestration_config,
        quality: quality_config,
        review: review_config,
        plan: plan_config,
        web: web_config,
        rule_sets,
    } = sections;
    let mut batteries = vec![docs::extension()?];
    if selected(cx, BATTERY_ASK) {
        batteries.push(ask::ask()?);
    }
    if selected(cx, BATTERY_HISTORY) {
        batteries.push(history::history(history_config)?);
    }
    if selected(cx, BATTERY_JUDGED) {
        let built = judged::judged(judged_config)?;
        parts.tools.rerank = Some(built.rerank);
        batteries.push(built.extension);
    }
    if selected(cx, BATTERY_MCP) && mcp_settings.enabled {
        let client = mcp::McpConfig {
            tokens_path: cx.data_root.join("mcp").join("tokens.json"),
            client_version: env!("CARGO_PKG_VERSION").to_owned(),
        };
        batteries.push(mcp::mcp(&client)?);
    }
    let mut orchestration_view: Option<orchestration::View> = None;
    if selected(cx, BATTERY_ORCHESTRATION) {
        orchestration_config.data_root = Some(cx.data_root.clone());
        let built = orchestration::orchestration(orchestration_config)?;
        orchestration_view = Some(built.view);
        batteries.push(built.extension);
    }
    if selected(cx, BATTERY_QUALITY) {
        let guard = sections::guard(cx.config)?;
        parts.guard = guard.extension;
        parts.tools.observer = Some(std::sync::Arc::clone(&guard.observer));
        let quality_config = quality::QualityConfig {
            detectors_enabled: quality_config.detectors_enabled
                && rule_sets.iter().any(|name| name == "detectors"),
        };
        batteries.push(quality::quality(
            quality_config,
            guard.findings,
            guard.observer,
        )?);
    }
    if selected(cx, BATTERY_REVIEW) && review_config.enabled {
        batteries.push(review::review_with_view(review_config, orchestration_view)?);
    }
    if selected(cx, BATTERY_SKILLS) {
        batteries.push(skills::skills()?);
    }
    if selected(cx, BATTERY_TTSR_RULES) {
        batteries.push(ttsr_rules::ttsr_rules(&rule_sets)?);
    }
    if selected(cx, BATTERY_WEB) && web_config.enabled {
        batteries.push(web::web(web_config)?);
    }
    if selected(cx, BATTERY_WORK) && plan_config.enabled {
        batteries.push(work::work(plan_config)?);
    }
    batteries.sort_by(|left, right| left.name().cmp(right.name()));
    Ok(batteries)
}
