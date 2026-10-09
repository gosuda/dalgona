// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! Batteries-included composition for the dalgona product.

mod battery_names;
mod docs;
mod plugins;
mod sections;

use dal_agent::ext::Extension;
use dal_core::{Claimant, Name, Origin, RegistrationError};
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
    battery_names::validate(cx.config, BATTERY_NAMES)?;
    let sections = Sections::decode(cx.config)?;
    let mut parts = dalgon::parts(cx)?;
    plugins::require_listed(cx, &parts.bundled)?;
    let batteries = compose(cx, sections, &mut parts)?;
    parts.batteries = batteries;
    let mut product = dalgon::assemble(cx, parts)?;
    // The bundled `skills` battery owns that name; dal's Builtin placeholder
    // carries an empty registry and would shadow it.
    product.extensions.retain(|extension| {
        !(extension.name() == "skills" && extension.origin() == Origin::Builtin)
    });
    reject_shadowed_names(&product.extensions)?;
    product.name = NAME;
    product.defaults = DEFAULTS;
    Ok(product)
}

/// A user plugin never replaces a bundled battery or built-in extension of the
/// same name; the operator disables the battery first.
fn reject_shadowed_names(extensions: &[Extension]) -> Result<(), dalgon::BuildError> {
    for (index, user) in extensions
        .iter()
        .enumerate()
        .filter(|(_, extension)| extension.origin() == Origin::User)
    {
        let Some(held) = extensions
            .iter()
            .take(index)
            .find(|earlier| earlier.name() == user.name() && earlier.origin() != Origin::User)
        else {
            continue;
        };
        let name = Name::parse(user.name())?;
        let claimant = if held.origin() == Origin::Builtin {
            Claimant::Builtin(name.clone())
        } else {
            Claimant::Battery(name.clone())
        };
        return Err(RegistrationError::Conflict {
            kind: "extension",
            name,
            claimant,
        }
        .into());
    }
    Ok(())
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
        orchestration_config.data_root = Some(cx.data_root.to_path_buf());
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
