//! Provenance-backed built-in model rows.
//!
//! Each row carries the source that vouches for its numbers; the release
//! process updates rows and notes together, never numbers alone.

use dal_core::ThinkingLevel;

use super::{CatalogEntry, ImageProfile, Listing, ToolSupport};
use crate::thinking::ThinkingSupport;

/// Returns built-in model rows whose capabilities have a source-backed note.
#[must_use]
pub fn built_in_entries() -> Vec<CatalogEntry> {
    use ThinkingLevel::{High, Low, Max, Medium, Xhigh};

    // `prices/models.dev.json` marks each built-in OpenAI row below as not
    // supporting temperature.
    let levels = vec![Low, Medium, High, Xhigh, Max];
    // Verified 2026-09-28: https://developers.openai.com/api/docs/models/gpt-6-astra
    // states 922,000 maximum input and 128,000 maximum output tokens. The model
    // page lists Responses tools; provider-layer change 16 limits it to Responses.
    let astra = openai_entry(
        "gpt-6-astra",
        "GPT-6 Astra",
        922_000,
        128_000,
        levels.clone(),
        false,
        ToolSupport::ResponsesOnly,
    );
    // Verified 2026-09-28: https://developers.openai.com/api/docs/models/gpt-6-sol
    // states 922,000 maximum input and 128,000 maximum output tokens. Chat
    // function calling requires reasoning_effort `none` on this model.
    let sol = openai_entry(
        "gpt-6-sol",
        "GPT-6 Sol",
        922_000,
        128_000,
        levels.clone(),
        true,
        ToolSupport::ChatWhenNoReasoning,
    );
    // Verified 2026-09-28: https://developers.openai.com/api/docs/models/gpt-6-luna
    // states 922,000 maximum input and 128,000 maximum output tokens. Chat
    // function calling requires reasoning_effort `none` on this model.
    let luna = openai_entry(
        "gpt-6-luna",
        "GPT-6 Luna",
        922_000,
        128_000,
        levels.clone(),
        true,
        ToolSupport::ChatWhenNoReasoning,
    );
    // Verified 2026-09-28: https://developers.openai.com/api/docs/models/gpt-5.6-sol
    // documents `gpt-5.6` as its alias, with 922,000 input and 128,000 output.
    let gpt_56 = openai_entry(
        "gpt-5.6",
        "GPT-5.6",
        922_000,
        128_000,
        levels.clone(),
        true,
        ToolSupport::Any,
    );
    // Chat Completions, Responses, and function calling are supported.
    // Verified 2026-09-28: https://developers.openai.com/api/docs/models/gpt-5.6-sol
    let gpt_56_sol = openai_entry(
        "gpt-5.6-sol",
        "GPT-5.6 Sol",
        922_000,
        128_000,
        levels.clone(),
        true,
        ToolSupport::Any,
    );
    // Chat Completions, Responses, and function calling are supported.
    // Verified 2026-09-28: https://developers.openai.com/api/docs/models/gpt-5.6-luna
    let gpt_56_luna = openai_entry(
        "gpt-5.6-luna",
        "GPT-5.6 Luna",
        922_000,
        128_000,
        levels.clone(),
        true,
        ToolSupport::Any,
    );
    // Chat Completions, Responses, and function calling are supported.
    // Verified 2026-09-28: https://developers.openai.com/api/docs/models/gpt-5.6-terra
    let gpt_56_terra = openai_entry(
        "gpt-5.6-terra",
        "GPT-5.6 Terra",
        922_000,
        128_000,
        levels.clone(),
        true,
        ToolSupport::Any,
    );
    vec![
        astra,
        sol,
        luna,
        gpt_56,
        gpt_56_sol,
        gpt_56_luna,
        gpt_56_terra,
        codex_reserve_entry(levels),
    ]
}

// Provenance: `.references/codex/codex-rs/models-manager/models.json`, the
// `gpt-6-luna` row (`context_window` 272,000, `tool_mode` code-only,
// `supports_reasoning_summaries` true); D-51 assigns its limits to Reserve.
// D-45's default 95% yields the 258,400 input window; no output cap is given.
fn codex_reserve_entry(accepted: Vec<ThinkingLevel>) -> CatalogEntry {
    CatalogEntry {
        provider: "openai-codex".into(),
        id: "gpt-reserve".into(),
        display: "Luna Reserve".into(),
        listing: Listing::Hidden,
        context_window: Some(258_400),
        max_output: None,
        thinking: ThinkingSupport::OpenAi {
            accepted,
            none_supported: false,
        },
        image_input: true,
        image_profile: None,
        remote_compact: true,
        supports_reasoning_summaries: true,
        tool_support: ToolSupport::Any,
        custom_grammar: false,
        temperature_allowed: false,
        display_supported: false,
    }
}

/// `OpenAI` Responses rows use the remote compaction endpoint described by D-47.
fn openai_entry(
    id: &str,
    display: &str,
    context_window: u32,
    max_output: u32,
    accepted: Vec<ThinkingLevel>,
    none_supported: bool,
    tool_support: ToolSupport,
) -> CatalogEntry {
    CatalogEntry {
        provider: "openai".into(),
        id: id.into(),
        display: display.into(),
        listing: Listing::Listed,
        context_window: Some(context_window),
        max_output: Some(max_output),
        thinking: ThinkingSupport::OpenAi {
            accepted,
            none_supported,
        },
        image_input: true,
        image_profile: Some(ImageProfile::openai()),
        remote_compact: true,
        supports_reasoning_summaries: false,
        tool_support,
        custom_grammar: false,
        temperature_allowed: false,
        display_supported: false,
    }
}
