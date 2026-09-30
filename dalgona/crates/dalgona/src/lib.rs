// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! Batteries-included composition for the dalgona product.

mod docs;
mod sections;

use dal_agent::ext::Extension;
use dalgona_batteries::{
    ask, history, judged, mcp, orchestration, quality, review, skills, ttsr_rules, web, work,
};
use sections::Sections;

const BATTERY_NAMES: &[&str] = &[
    "ask",
    "history",
    "judged",
    "mcp",
    "orchestration",
    "quality",
    "review",
    "skills",
    "ttsr-rules",
    "web",
    "work",
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
    cx.config.validate_battery_names(BATTERY_NAMES)?;
    let sections = Sections::decode(cx.config)?;
    let mut parts = dalgon::parts(cx)?;
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
        orchestration: orchestration_config,
        quality: quality_config,
        review: review_config,
        plan: plan_config,
        web: web_config,
        rule_sets,
    } = sections;
    let mut batteries = vec![docs::extension()?];
    if selected(cx, "ask") {
        batteries.push(ask::ask()?);
    }
    if selected(cx, "history") {
        batteries.push(history::history(history_config)?);
    }
    if selected(cx, "judged") {
        let built = judged::judged(judged_config)?;
        parts.tools.rerank = Some(built.rerank);
        batteries.push(built.extension);
    }
    if selected(cx, "mcp") && mcp_settings.enabled {
        let client = mcp::McpConfig {
            tokens_path: cx.data_root.join("mcp").join("tokens.json"),
            client_version: env!("CARGO_PKG_VERSION").to_owned(),
        };
        batteries.push(mcp::mcp(&client)?);
    }
    if selected(cx, "orchestration") {
        batteries.push(orchestration::orchestration(orchestration_config)?);
    }
    if selected(cx, "quality") {
        let guard = sections::guard(cx.config)?;
        parts.guard = guard.extension;
        parts.tools.observer = Some(std::sync::Arc::clone(&guard.observer));
        batteries.push(quality::quality(
            quality_config,
            guard.findings,
            guard.observer,
        )?);
    }
    if selected(cx, "review") && review_config.enabled {
        batteries.push(review::review(review_config)?);
    }
    if selected(cx, "skills") {
        batteries.push(skills::skills()?);
    }
    if selected(cx, "ttsr-rules") {
        batteries.push(ttsr_rules::ttsr_rules(&rule_sets)?);
    }
    if selected(cx, "web") && web_config.enabled {
        batteries.push(web::web(web_config)?);
    }
    if selected(cx, "work") && plan_config.enabled {
        batteries.push(work::work(plan_config)?);
    }
    batteries.sort_by(|left, right| left.name().cmp(right.name()));
    Ok(batteries)
}
