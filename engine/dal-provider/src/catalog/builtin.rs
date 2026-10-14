//! Built-in model rows used by the provider catalog.
//!
//! Keep each row's source notes aligned with its values when updating the catalog.

use dal_core::ThinkingLevel;

use super::{CatalogEntry, ImageProfile, Listing, ToolSupport};
use crate::{
    provider::{OPENAI, OPENAI_CODEX},
    thinking::ThinkingSupport,
    usage::{LUNA_RESERVE_DISPLAY, LUNA_RESERVE_MODEL},
};

/// Returns the built-in model rows.
#[must_use]
pub fn built_in_entries() -> Vec<CatalogEntry> {
    use ThinkingLevel::{High, Low, Max, Medium, Xhigh};

    // `prices/models.dev.json` marks each built-in OpenAI row below as not
    // supporting temperature.
    let levels = vec![Low, Medium, High, Xhigh, Max];
    // Verified 2026-09-28: https://developers.openai.com/api/docs/models/gpt-6-astra
    // states 922,000 maximum input and 128,000 maximum output tokens.
    // The model page lists Responses tools.
    // This row uses the Responses API.
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

/// Builds the hidden Codex reserve row.
///
/// The reserve input window is 95% of 272,000 tokens (258,400), and no output
/// cap is set. Remote compaction and reasoning summaries remain enabled.
fn codex_reserve_entry(accepted: Vec<ThinkingLevel>) -> CatalogEntry {
    CatalogEntry {
        provider: OPENAI_CODEX.id.into(),
        id: LUNA_RESERVE_MODEL.into(),
        display: LUNA_RESERVE_DISPLAY.into(),
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

/// Builds a listed `OpenAI` catalog row with remote compaction enabled.
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
        provider: OPENAI.id.into(),
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
