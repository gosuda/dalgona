//! Wire decoders turning provider model lists into catalog rows.
//!
//! Decoders tolerate unknown reply fields (external producers evolve) but
//! never invent limits: unlisted ids fall back to family defaults.

use dal_core::{Family, ThinkingLevel};
use serde::Deserialize;

use super::{
    CatalogEntry, ImageProfile, Listing, ToolSupport,
    builtin::built_in_entries,
    prices::compiled_temperature,
    resolve::{bind_family_capabilities, capability_row, default_entry},
};
use crate::{
    error::ProviderError,
    provider::ProviderEntry,
    thinking::{Effort, ThinkingSupport},
};

/// Replaces terminal control and format characters in a provider-supplied
/// catalog identifier with the replacement character, so a configured or
/// compromised provider cannot smuggle newlines, bidi reordering, or escape
/// sequences through model ids or display names into CLI output or the models
/// cache.
pub(crate) fn sanitize_identifier(value: &str) -> Box<str> {
    if value
        .chars()
        .all(|character| !is_control_or_format(character))
    {
        return value.into();
    }
    value
        .chars()
        .map(|character| {
            if is_control_or_format(character) {
                char::REPLACEMENT_CHARACTER
            } else {
                character
            }
        })
        .collect::<String>()
        .into_boxed_str()
}

fn is_control_or_format(character: char) -> bool {
    character.is_control()
        || matches!(
            u32::from(character),
            0x00AD
                | 0x0600..=0x0605
                | 0x061C
                | 0x06DD
                | 0x070F
                | 0x0890..=0x0891
                | 0x08E2
                | 0x180E
                | 0x200B..=0x200F
                | 0x202A..=0x202E
                | 0x2060..=0x2064
                | 0x2066..=0x206F
                | 0xFEFF
                | 0xFFF9..=0xFFFB
                | 0x110BD
                | 0x110CD
                | 0x13430..=0x1343F
                | 0x1BCA0..=0x1BCA3
                | 0x1D173..=0x1D17A
                | 0xE0001
                | 0xE0020..=0xE007F
        )
}

pub(crate) fn decode_openai_models(
    provider: &ProviderEntry,
    bytes: &[u8],
) -> Result<Vec<CatalogEntry>, ProviderError> {
    let response: OpenAiModels = decode_json(provider.family, bytes, "OpenAI model list")?;
    let builtins = built_in_entries();
    Ok(response
        .data
        .into_iter()
        .map(|model| {
            let id = sanitize_identifier(&model.id);
            let base = capability_row(&builtins, provider, &id)
                .cloned()
                .unwrap_or_else(|| default_entry(provider, &id));
            let mut entry = CatalogEntry {
                provider: provider.id.clone(),
                id,
                display: base.display,
                listing: Listing::Listed,
                context_window: base.context_window,
                max_output: base.max_output,
                thinking: base.thinking,
                image_input: base.image_input,
                image_profile: base.image_profile,
                remote_compact: base.remote_compact,
                supports_reasoning_summaries: base.supports_reasoning_summaries,
                tool_support: base.tool_support,
                custom_grammar: base.custom_grammar,
                temperature_allowed: base.temperature_allowed,
                display_supported: base.display_supported,
            };
            bind_family_capabilities(&mut entry, provider.family);
            entry
        })
        .collect())
}

pub(crate) fn decode_codex_models(
    provider: &ProviderEntry,
    bytes: &[u8],
) -> Result<Vec<CatalogEntry>, ProviderError> {
    let response: CodexModels = decode_json(Family::Codex, bytes, "Codex model list")?;
    response
        .models
        .into_iter()
        .map(|model| {
            let levels = model
                .supported_reasoning_levels
                .iter()
                .filter_map(CodexReasoningLevel::effort)
                .filter_map(parse_effort)
                .collect::<Vec<_>>();
            let none_supported = model
                .supported_reasoning_levels
                .iter()
                .any(|level| level.effort().is_some_and(|effort| effort == "none"));
            let effective_context = model
                .context_window
                .map(|context| {
                    let percent = model.effective_context_window_percent.unwrap_or(95);
                    let usable = u64::from(context) * u64::from(percent) / 100;
                    u32::try_from(usable).map_err(|_| {
                        protocol(
                            Family::Codex,
                            "effective context window is outside the supported range",
                        )
                    })
                })
                .transpose()?;
            Ok(CatalogEntry {
                provider: provider.id.clone(),
                id: sanitize_identifier(&model.slug),
                display: model
                    .display_name
                    .as_deref()
                    .map_or_else(|| sanitize_identifier(&model.slug), sanitize_identifier),
                listing: if model.visibility == "list" {
                    Listing::Listed
                } else {
                    Listing::Hidden
                },
                context_window: effective_context,
                max_output: None,
                thinking: ThinkingSupport::OpenAi {
                    accepted: levels,
                    none_supported,
                },
                image_input: model
                    .input_modalities
                    .iter()
                    .any(|modality| modality == "image"),
                image_profile: None,
                remote_compact: true,
                supports_reasoning_summaries: model.supports_reasoning_summaries,
                tool_support: ToolSupport::Any,
                custom_grammar: false,
                temperature_allowed: compiled_temperature(
                    provider.id.as_ref(),
                    &sanitize_identifier(&model.slug),
                ),
                display_supported: false,
            })
        })
        .collect()
}
pub(crate) fn decode_anthropic_page(
    provider: &ProviderEntry,
    bytes: &[u8],
) -> Result<AnthropicPage, ProviderError> {
    let response: AnthropicModels = decode_json(Family::Anthropic, bytes, "Anthropic model list")?;
    let rows = response
        .data
        .into_iter()
        .map(|model| {
            let capabilities = model.capabilities.unwrap_or_default();
            let thinking = anthropic_thinking(&capabilities);
            let image_input = capabilities
                .image_input
                .is_some_and(|support| support.supported);
            let image_profile = model.max_input_tokens.and_then(|window| {
                capabilities
                    .image_input
                    .as_ref()
                    .filter(|support| support.supported)
                    .map(|_| ImageProfile::anthropic_standard(window))
            });
            CatalogEntry {
                provider: provider.id.clone(),
                id: sanitize_identifier(&model.id),
                display: model
                    .display_name
                    .as_deref()
                    .map_or_else(|| sanitize_identifier(&model.id), sanitize_identifier),
                listing: Listing::Listed,
                context_window: model.max_input_tokens,
                max_output: model.max_tokens,
                thinking,
                image_input,
                image_profile,
                remote_compact: capabilities
                    .context_management
                    .and_then(|context| context.compact_20260112)
                    .is_some_and(|support| support.supported),
                supports_reasoning_summaries: false,
                tool_support: ToolSupport::Any,
                custom_grammar: false,
                temperature_allowed: compiled_temperature(
                    provider.id.as_ref(),
                    &sanitize_identifier(&model.id),
                ),
                display_supported: false,
            }
        })
        .collect();
    Ok(AnthropicPage {
        rows,
        has_more: response.has_more,
        last_id: response.last_id,
    })
}

fn anthropic_thinking(capabilities: &AnthropicCapabilities) -> ThinkingSupport {
    let types = capabilities
        .thinking
        .as_ref()
        .and_then(|thinking| thinking.types.as_ref());
    let can_disable = types
        .and_then(|types| types.disabled)
        .is_some_and(|support| support.supported);
    if types
        .and_then(|types| types.adaptive)
        .is_some_and(|support| support.supported)
    {
        ThinkingSupport::Adaptive {
            can_disable,
            accepted: accepted_anthropic_efforts(capabilities.effort.as_ref()),
        }
    } else if types
        .and_then(|types| types.enabled)
        .is_some_and(|support| support.supported)
    {
        ThinkingSupport::Budget { can_disable }
    } else {
        ThinkingSupport::UnknownAdaptive
    }
}

fn accepted_anthropic_efforts(effort: Option<&AnthropicEffortCapabilities>) -> Vec<Effort> {
    let Some(effort) = effort.filter(|effort| effort.supported) else {
        return Vec::new();
    };
    [
        (effort.low, Effort::Low),
        (effort.medium, Effort::Medium),
        (effort.high, Effort::High),
        (effort.xhigh, Effort::Xhigh),
        (effort.max, Effort::Max),
    ]
    .into_iter()
    .filter_map(|(support, level)| support.filter(|support| support.supported).map(|_| level))
    .collect()
}

fn parse_effort(effort: &str) -> Option<ThinkingLevel> {
    match effort {
        "minimal" => Some(ThinkingLevel::Minimal),
        "low" => Some(ThinkingLevel::Low),
        "medium" => Some(ThinkingLevel::Medium),
        "high" => Some(ThinkingLevel::High),
        "xhigh" => Some(ThinkingLevel::Xhigh),
        "max" => Some(ThinkingLevel::Max),
        _ => None,
    }
}

pub(crate) fn parse_anthropic_effort(effort: &str) -> Option<Effort> {
    match effort {
        "low" => Some(Effort::Low),
        "medium" => Some(Effort::Medium),
        "high" => Some(Effort::High),
        "xhigh" => Some(Effort::Xhigh),
        "max" => Some(Effort::Max),
        _ => None,
    }
}

fn decode_json<T: for<'de> Deserialize<'de>>(
    family: Family,
    bytes: &[u8],
    name: &str,
) -> Result<T, ProviderError> {
    sonic_rs::from_slice(bytes)
        .map_err(|error| protocol(family, format!("{name} is invalid JSON: {error}")))
}

pub(crate) fn protocol(family: Family, detail: impl Into<String>) -> ProviderError {
    ProviderError::Protocol {
        family,
        detail: detail.into(),
    }
}
#[derive(Deserialize)]
struct OpenAiModels {
    data: Vec<OpenAiModel>,
}

#[derive(Deserialize)]
struct OpenAiModel {
    id: String,
}

#[derive(Deserialize)]
struct AnthropicModels {
    data: Vec<AnthropicModel>,
    #[serde(default)]
    has_more: bool,
    #[serde(default)]
    last_id: Option<String>,
}

pub(crate) struct AnthropicPage {
    pub(crate) rows: Vec<CatalogEntry>,
    pub(crate) has_more: bool,
    pub(crate) last_id: Option<String>,
}

#[derive(Deserialize)]
struct AnthropicModel {
    id: String,
    display_name: Option<String>,
    max_input_tokens: Option<u32>,
    max_tokens: Option<u32>,
    capabilities: Option<AnthropicCapabilities>,
}

#[derive(Default, Deserialize)]
struct AnthropicCapabilities {
    image_input: Option<CapabilitySupport>,
    thinking: Option<AnthropicThinkingCapabilities>,
    effort: Option<AnthropicEffortCapabilities>,
    context_management: Option<ContextManagementCapabilities>,
}

#[derive(Default, Deserialize)]
struct AnthropicEffortCapabilities {
    #[serde(default)]
    supported: bool,
    low: Option<CapabilitySupport>,
    medium: Option<CapabilitySupport>,
    high: Option<CapabilitySupport>,
    xhigh: Option<CapabilitySupport>,
    max: Option<CapabilitySupport>,
}

#[derive(Deserialize)]
struct AnthropicThinkingCapabilities {
    types: Option<ThinkingTypes>,
}

#[derive(Deserialize)]
struct ThinkingTypes {
    adaptive: Option<CapabilitySupport>,
    enabled: Option<CapabilitySupport>,
    disabled: Option<CapabilitySupport>,
}

#[derive(Deserialize)]
struct ContextManagementCapabilities {
    compact_20260112: Option<CapabilitySupport>,
}

#[derive(Clone, Copy, Deserialize)]
struct CapabilitySupport {
    #[serde(default)]
    supported: bool,
}

#[derive(Deserialize)]
struct CodexModels {
    models: Vec<CodexModel>,
}

#[derive(Deserialize)]
struct CodexModel {
    slug: String,
    display_name: Option<String>,
    #[serde(default)]
    visibility: String,
    #[serde(default)]
    supports_reasoning_summaries: bool,
    context_window: Option<u32>,
    effective_context_window_percent: Option<u32>,
    #[serde(default)]
    supported_reasoning_levels: Vec<CodexReasoningLevel>,
    #[serde(default)]
    input_modalities: Vec<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum CodexReasoningLevel {
    String(String),
    Object { effort: Option<String> },
}

impl CodexReasoningLevel {
    fn effort(&self) -> Option<&str> {
        match self {
            Self::String(effort) => Some(effort),
            Self::Object { effort } => effort.as_deref(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::sanitize_identifier;

    #[test]
    fn sanitize_identifier_replaces_format_controls_without_losing_unicode_text() {
        let sanitized = sanitize_identifier("café\u{202E}模型\u{2069}\u{FEFF}");
        assert_eq!(sanitized.as_ref(), "café�模型��");
    }
}
