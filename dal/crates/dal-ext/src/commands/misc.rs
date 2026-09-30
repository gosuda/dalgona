//! Tracked jobs and always-refused commands.
//!
//! `/compact` starts its job through the host table; `/reload` runs the
//! `PluginReload` seam (load plus host publication) and prints the published
//! counts. `/share`, `/bug`, and `/trust` refuse without error.

use dal_agent::ext::command::{BuildError, CommandCx, ReloadSummary};
use dal_core::command::{Command, ErrorTriple, Output, Reply};

use super::PluginReload;

/// Runs `/compact`: starts one compaction job over the raw instructions.
///
/// # Errors
///
/// Returns the nothing-to-compact pair when no turn has finished yet.
pub(super) fn compact(cx: &CommandCx<'_>, instructions: &str) -> Result<Reply, ErrorTriple> {
    if cx.leaf_entries().is_empty() {
        return Err(nothing_to_compact());
    }
    let focus = if instructions.is_empty() {
        None
    } else {
        Some(instructions.into())
    };
    Ok(Reply::Started(cx.start_job(Command::Compact { focus })))
}

/// The nothing-to-compact pair for `/compact` with no finished turn yet.
pub(super) fn nothing_to_compact() -> ErrorTriple {
    super::error_triple(
        "There is nothing to compact yet",
        "the session has no finished turn",
        "Send a message first.",
    )
}

/// Runs `/reload`: reloads user plugins and publishes one generation.
///
/// The seam owns load and host publication; this renders the published
/// summary. Counts stay unit-testable through `reload_text`.
///
/// # Errors
///
/// Returns the seam's triple when load or publication fails.
pub(super) async fn reload(
    cx: &CommandCx<'_>,
    loader: &dyn PluginReload,
) -> Result<Reply, ErrorTriple> {
    let summary = loader.reload(cx).await?;
    Ok(Reply::Done(Output::Text(reload_text(&summary).into())))
}

/// Renders the `/reload` reply for one published summary.
pub(super) fn reload_text(summary: &ReloadSummary) -> String {
    if summary.plugins == 0 {
        return "No plugins to reload: the plugins key in dal.toml is empty.".to_owned();
    }
    format!(
        "Reloaded {p} plugin{ps}: {t} tool{ts}. New turns use them. A running turn keeps the plugins it started with.",
        p = summary.plugins,
        ps = super::plural(summary.plugins),
        t = summary.tools,
        ts = super::plural(summary.tools),
    )
}

/// Maps a host publication failure to the plan pair.
///
/// Splits plan 4887's `reload failed: <rendered error>; the previous plugin
/// set stays live` across the triple the terminal renders as `{what}: {why}`
/// then `{fix}`. Exported for the product-local adapter, which performs
/// publication inside the seam.
#[must_use]
pub fn publish_failure(error: BuildError) -> ErrorTriple {
    super::error_triple(
        "reload failed",
        error.to_string(),
        "the previous plugin set stays live",
    )
}

/// Refuses `/trust`: dal loads plugins only from its data directory.
pub(super) fn trust(cx: &CommandCx<'_>) -> Result<Reply, ErrorTriple> {
    Ok(Reply::Done(Output::Text(trust_text(cx.data_root()).into())))
}

/// Renders the `/trust` refusal for one data directory.
pub(super) fn trust_text(data: &std::path::Path) -> String {
    format!(
        "/trust is not in dalgon: dalgon loads plugins only from its data directory and reads AGENTS.md in every workspace, so there is no project trust to save\nPut plugins under {}/plugins and name them in the plugins key of dal.toml.",
        data.display()
    )
}

/// Refuses `/share`: dal uploads no session data to any service.
pub(super) fn share() -> Result<Reply, ErrorTriple> {
    Ok(Reply::Done(Output::Text(
        "/share is not in dalgon: dalgon uploads no session data to any service\nType /export to write the session to a file, then share the file yourself."
            .into(),
    )))
}

/// Refuses `/bug`: dal sends no reports or session data to its developers.
pub(super) fn bug() -> Result<Reply, ErrorTriple> {
    Ok(Reply::Done(Output::Text(
        "/bug is not in dalgon: dalgon sends no reports or session data to its developers\nType /export to write the session to a file, and attach it with the log path from /session to your report."
            .into(),
    )))
}
