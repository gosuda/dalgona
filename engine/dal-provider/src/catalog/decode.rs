//! Wire decoders turning provider model lists into catalog rows.
//!
//! Decoders tolerate unknown reply fields (external producers evolve) but
//! never invent limits: unlisted ids fall back to family defaults.

use dal_core::{Family, ModelPrice, RawJson, ThinkingLevel};
use serde::Deserialize;
use sonic_rs::{JsonValueTrait, Value};

use super::{
    CatalogEntry, ImageProfile, Listing, ToolSupport,
    builtin::built_in_entries,
    prices::compiled_temperature,
    resolve::{bind_family_capabilities, capability_row, default_entry},
};
use crate::{
    error::ProviderError,
    family::{
        copilot::{CopilotModel, Price, Surface, decode_models},
        devin::decode::{DiscoveredModel, decode_discovery},
        ollama::{
            CLOUD_NUM_PREDICT_CAP, Deployment, OllamaShow, cloud_output_capped, decode_tags,
            thinking_ladder,
        },
    },
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
                premium_multiplier: base.premium_multiplier,
                price: base.price,
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
                premium_multiplier: None,
                price: None,
            })
        })
        .collect()
}

pub(crate) fn decode_devin_models(
    provider: &ProviderEntry,
    bytes: &[u8],
) -> Result<Vec<CatalogEntry>, ProviderError> {
    let models = decode_discovery(bytes)?;
    models
        .iter()
        .map(|model| devin_entry(provider, model))
        .collect()
}

fn devin_entry(
    provider: &ProviderEntry,
    model: &DiscoveredModel,
) -> Result<CatalogEntry, ProviderError> {
    let mut entry = default_entry(provider, &sanitize_identifier(&model.uid));
    entry.display = sanitize_identifier(&model.label);
    entry.context_window = positive_tokens(provider.family, model.max_tokens)?;
    entry.max_output = positive_tokens(provider.family, model.max_output_tokens)?;
    entry.image_input = model.supports_images;
    if !model.supports_tool_calls {
        entry.tool_support = ToolSupport::None;
    }
    bind_family_capabilities(&mut entry, provider.family);
    Ok(entry)
}

fn positive_tokens(family: Family, value: i32) -> Result<Option<u32>, ProviderError> {
    if value <= 0 {
        return Ok(None);
    }
    u32::try_from(value).map(Some).map_err(|_| {
        protocol(
            family,
            "devin discovery token count is outside the supported range",
        )
    })
}

pub(crate) fn decode_ollama_models(
    provider: &ProviderEntry,
    bytes: &[u8],
) -> Result<Vec<CatalogEntry>, ProviderError> {
    let models = decode_tags(bytes)?;
    let mut seen = std::collections::HashSet::new();
    let mut rows = Vec::new();
    for model in &models {
        let Some(id) = model.wire_id().map(|id| sanitize_identifier(id.trim())) else {
            continue;
        };
        if id.is_empty() || !seen.insert(id.to_string()) {
            continue;
        }
        let mut entry = default_entry(provider, &id);
        // `api/tags` spells the same id twice; a display name only exists
        // when `name` says something the id does not.
        if let Some(name) = model
            .name
            .as_deref()
            .map(|name| sanitize_identifier(name.trim()))
            .filter(|name| !name.is_empty() && **name != *id)
        {
            entry.display = name;
        }
        bind_family_capabilities(&mut entry, provider.family);
        rows.push(entry);
    }
    Ok(rows)
}

/// Applies one `api/show` probe to a row decoded from `api/tags`.
///
/// The trained context window sizes the row when the probe names one. A probe
/// that lists capabilities decides the rest: `thinking` opens the model's
/// ladder while a model without it accepts no reasoning level and gets no
/// `think` member at all; `vision` opens image input; a model without `tools`
/// takes no tool advertisements. The ladder is what the server reports in
/// `thinking.values` (named levels, and whether `false` turns reasoning off)
/// and only when a server reports none does it come from the model id. A probe
/// that lists no capabilities leaves the row as `api/tags` made it. On the
/// cloud, the `DeepSeek` V4 deployments that cap output below their window carry
/// that cap as their output limit, which `api/show` never reports.
pub(crate) fn apply_ollama_show(
    entry: &mut CatalogEntry,
    show: &OllamaShow,
    deployment: Deployment,
) {
    if let Some(window) = show.context_window {
        entry.context_window = Some(window);
    }
    let thinks = show
        .reports("thinking")
        .or_else(|| show.thinking.as_ref().map(|_| true));
    if let Some(thinks) = thinks {
        let (accepted, none_supported) = match &show.thinking {
            _ if !thinks => (Vec::new(), false),
            Some(values) => values.ladder(),
            None => (thinking_ladder(deployment, &entry.id).to_vec(), true),
        };
        entry.thinking = ThinkingSupport::OpenAi {
            accepted,
            none_supported,
        };
    }
    if let Some(sees) = show.reports("vision") {
        entry.image_input = sees;
    }
    if show.reports("tools") == Some(false) {
        entry.tool_support = ToolSupport::None;
    }
    if deployment == Deployment::Cloud && cloud_output_capped(&entry.id) {
        entry.max_output = Some(
            entry
                .context_window
                .map_or(CLOUD_NUM_PREDICT_CAP, |window| {
                    window.min(CLOUD_NUM_PREDICT_CAP)
                }),
        );
    }
}

pub(crate) fn decode_mistral_models(
    provider: &ProviderEntry,
    bytes: &[u8],
) -> Result<Vec<CatalogEntry>, ProviderError> {
    let list: MistralList = decode_json(Family::Mistral, bytes, "Mistral model list")?;
    let cards = match list {
        MistralList::Array(cards) => cards,
        MistralList::Object { data } => data,
    };
    let mut seen = std::collections::HashSet::new();
    let mut rows = Vec::new();
    for card in &cards {
        let Some(id) = card.id.as_deref().map(|id| sanitize_identifier(id.trim())) else {
            continue;
        };
        if id.is_empty() || card.archived || !seen.insert(id.to_string()) {
            continue;
        }
        let mut entry = default_entry(provider, &id);
        entry.context_window = mistral_window(card.max_context_length)?;
        let capabilities = card.capabilities.as_ref();
        entry.image_input = capabilities.and_then(|flags| flags.vision).unwrap_or(false);
        if capabilities.and_then(|flags| flags.function_calling) == Some(false) {
            entry.tool_support = ToolSupport::None;
        }
        if let Some(reasoning) = capabilities.and_then(|flags| flags.reasoning) {
            entry.thinking = mistral_thinking(&id, reasoning);
        }
        bind_family_capabilities(&mut entry, provider.family);
        rows.push(entry);
    }
    Ok(rows)
}

/// The `reasoning_effort` values one Mistral card accepts, from its
/// `capabilities.reasoning` flag.
///
/// The card says only whether a model reasons. The documented adjustable
/// models take `none` and `high`; the hosted GLM models name their own
/// ladders (`zai-glm-5-2`: `none`, `high`, `max`; `zai-glm-5-3`: `low`, `high`,
/// `max`, with no `none`). A model that does not reason accepts no level and
/// no `reasoning_effort` member at all.
fn mistral_thinking(id: &str, reasoning: bool) -> ThinkingSupport {
    use ThinkingLevel::{High, Low, Max};
    let (accepted, none_supported) = match (reasoning, id) {
        (false, _) => (Vec::new(), false),
        (true, id) if id.starts_with("zai-glm-5-3") => (vec![Low, High, Max], false),
        (true, id) if id.starts_with("zai-glm-5-2") => (vec![High, Max], true),
        (true, _) => (vec![High], true),
    };
    ThinkingSupport::OpenAi {
        accepted,
        none_supported,
    }
}

fn mistral_window(value: Option<u64>) -> Result<Option<u32>, ProviderError> {
    let Some(value) = value.filter(|value| *value > 0) else {
        return Ok(None);
    };
    u32::try_from(value).map(Some).map_err(|_| {
        protocol(
            Family::Mistral,
            "mistral max context length is outside the supported range",
        )
    })
}

#[derive(Deserialize)]
#[serde(untagged)]
enum MistralList {
    Array(Vec<MistralCard>),
    Object {
        #[serde(default)]
        data: Vec<MistralCard>,
    },
}

#[derive(Deserialize)]
struct MistralCard {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    capabilities: Option<MistralCapabilities>,
    #[serde(default)]
    max_context_length: Option<u64>,
    #[serde(default)]
    archived: bool,
}

#[derive(Default, Deserialize)]
struct MistralCapabilities {
    function_calling: Option<bool>,
    vision: Option<bool>,
    reasoning: Option<bool>,
}

/// The prefix the Gemini API puts on every model resource name.
const GEMINI_MODEL_PREFIX: &str = "models/";

/// The one generation method that marks a row as a chat model.
const GEMINI_GENERATE_CONTENT: &str = "generateContent";

/// Id fragments that mark a Gemini row as accepting images. The list reports
/// no modality, so the id is the only signal, as in the reference.
const GEMINI_IMAGE_HINTS: [&str; 3] = ["vision", "image", "gemini"];

/// One page of the direct Gemini model list.
pub(crate) struct GeminiPage {
    /// The page's text-generation rows in wire order, not yet merged or sorted.
    pub(crate) rows: Vec<CatalogEntry>,
    /// The trimmed, non-blank `nextPageToken`, when the page names one.
    pub(crate) next_page_token: Option<String>,
}

/// Decodes one page of the direct Gemini API's `GET models`.
///
/// Follows the reference: only a row that lists `generateContent` among its
/// `supportedGenerationMethods` is a chat model; its id is the resource `name`
/// without the `models/` prefix, `displayName` names it, and `inputTokenLimit`
/// and `outputTokenLimit` become the context window and output limit when
/// positive. A row that is not an object, has no string `name`, or carries a
/// method list that is not an array of strings is dropped, not an error; a
/// display name or limit of the wrong type is unknown. The reply must be an
/// object whose `models`, when present, is an array.
///
/// The list reports no thinking support and no price, so a row keeps the
/// family defaults and is the same row a typed id of the family resolves to; a
/// price comes from the compiled snapshot at cost time. Images are the one
/// exception: the list names no modality, so an id that mentions `gemini`,
/// `vision`, or `image` accepts them, as in the reference.
pub(crate) fn decode_gemini_page(
    provider: &ProviderEntry,
    bytes: &[u8],
) -> Result<GeminiPage, ProviderError> {
    // A JSON array would fill the all-default struct below and read as an
    // empty list, so only an object is a model list.
    if !bytes.trim_ascii_start().starts_with(b"{") {
        return Err(protocol(
            Family::GoogleGenerativeAi,
            "Gemini model list is not a JSON object",
        ));
    }
    let list: GeminiList = decode_json(Family::GoogleGenerativeAi, bytes, "Gemini model list")?;
    let next_page_token = list
        .next_page_token
        .as_ref()
        .and_then(JsonValueTrait::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_owned);
    Ok(GeminiPage {
        rows: list
            .models
            .iter()
            .filter_map(|raw| gemini_entry(provider, raw))
            .collect(),
        next_page_token,
    })
}

fn gemini_entry(provider: &ProviderEntry, raw: &RawJson) -> Option<CatalogEntry> {
    // `RawJson` holds trimmed text, so a leading `{` means an object: serde
    // would fill a struct's defaults from a JSON array.
    if !raw.as_str().starts_with('{') {
        return None;
    }
    let card: GeminiCard = raw.decode_as().ok()?;
    if !card.methods.as_deref().is_some_and(|methods| {
        methods
            .iter()
            .any(|method| method == GEMINI_GENERATE_CONTENT)
    }) {
        return None;
    }
    let name = card.name.as_ref().and_then(JsonValueTrait::as_str)?.trim();
    let id = sanitize_identifier(name.strip_prefix(GEMINI_MODEL_PREFIX).unwrap_or(name));
    if id.is_empty() {
        return None;
    }
    let mut entry = default_entry(provider, &id);
    if let Some(display) = card
        .display_name
        .as_ref()
        .and_then(JsonValueTrait::as_str)
        .map(str::trim)
        .filter(|display| !display.is_empty())
    {
        entry.display = sanitize_identifier(display);
    }
    entry.context_window = gemini_limit(card.input_token_limit.as_ref());
    entry.max_output = gemini_limit(card.output_token_limit.as_ref());
    entry.image_input = GEMINI_IMAGE_HINTS
        .iter()
        .any(|hint| contains_ignore_ascii_case(&id, hint));
    bind_family_capabilities(&mut entry, provider.family);
    Some(entry)
}

/// A token limit when it is a positive integer that fits `u32`; anything else
/// is unknown, never zero.
fn gemini_limit(value: Option<&Value>) -> Option<u32> {
    value
        .and_then(JsonValueTrait::as_u64)
        .and_then(|limit| u32::try_from(limit).ok())
        .filter(|limit| *limit > 0)
}

/// Whether `haystack` holds the non-empty `needle`, ignoring ASCII case,
/// without lowercasing a copy.
fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    haystack
        .as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

#[derive(Deserialize)]
struct GeminiList {
    #[serde(default)]
    models: Vec<RawJson>,
    /// Any JSON value: a token of the wrong type ends pagination, not the refresh.
    #[serde(default, rename = "nextPageToken")]
    next_page_token: Option<Value>,
}

/// One model resource. Members the decoder reads as loosely typed values stay
/// [`Value`]s, so a wrong type drops that member, not the row.
#[derive(Deserialize)]
struct GeminiCard {
    #[serde(default)]
    name: Option<Value>,
    #[serde(default, rename = "displayName")]
    display_name: Option<Value>,
    #[serde(default, rename = "supportedGenerationMethods")]
    methods: Option<Vec<String>>,
    #[serde(default, rename = "inputTokenLimit")]
    input_token_limit: Option<Value>,
    #[serde(default, rename = "outputTokenLimit")]
    output_token_limit: Option<Value>,
}

/// Decodes the pi-messages gateway config (`GET <gateway>/v1/config`) into
/// catalog rows.
///
/// Follows the reference sanitizer: the document needs a string `baseUrl` and
/// an array `models`, and a model that lacks a string `id` or `name`, a
/// boolean `reasoning`, an `input` array, an object `cost`, or a numeric
/// `contextWindow` or `maxTokens` is dropped, not an error. The gateway's
/// `baseUrl` is not kept: the provider entry already names the endpoint. The
/// flat `cost` rates (USD per million tokens) become the row price; reasoning
/// tokens are billed at the output rate, as the gateway counts them inside
/// `output`. Request-size price tiers are not mapped.
pub(crate) fn decode_radius_models(
    provider: &ProviderEntry,
    bytes: &[u8],
) -> Result<Vec<CatalogEntry>, ProviderError> {
    let config: GatewayConfig =
        decode_json(Family::PiMessages, bytes, "pi-messages gateway config")?;
    let mut seen = std::collections::HashSet::new();
    let mut rows = Vec::new();
    for raw in &config.models {
        let Ok(model) = raw.decode_as::<GatewayModel>() else {
            continue;
        };
        let id = sanitize_identifier(model.id.trim());
        if id.is_empty() || seen.contains(id.as_ref()) {
            continue;
        }
        // `RawJson::parse` trims the text, so a leading `{` means an object:
        // serde would happily fill a struct's defaults from a JSON array.
        // The cost gate runs before `seen.insert`: a malformed first row must
        // not suppress a later valid row that reuses the same id.
        let Some(cost) = model
            .cost
            .as_str()
            .starts_with('{')
            .then(|| model.cost.decode_as::<GatewayCost>().ok())
            .flatten()
        else {
            continue;
        };
        seen.insert(id.to_string());
        let mut entry = default_entry(provider, &id);
        entry.display = sanitize_identifier(&model.name);
        entry.context_window = u32::try_from(model.context_window).ok().filter(|n| *n > 0);
        entry.max_output = u32::try_from(model.max_tokens).ok().filter(|n| *n > 0);
        entry.image_input = model.input.iter().any(|kind| kind == "image");
        entry.thinking = gateway_thinking(&model);
        entry.price = gateway_price(&cost);
        bind_family_capabilities(&mut entry, provider.family);
        rows.push(entry);
    }
    Ok(rows)
}

/// The thinking levels a gateway model accepts.
///
/// A model without `reasoning` accepts none. For a reasoning model a level
/// the `thinkingLevelMap` maps to `null` is unsupported and the others up to
/// `high` are supported; `xhigh` and `max` are supported only when the map
/// names them, since the reference decides those from the model family.
fn gateway_thinking(model: &GatewayModel) -> ThinkingSupport {
    if !model.reasoning {
        return ThinkingSupport::OpenAi {
            accepted: Vec::new(),
            none_supported: true,
        };
    }
    let map = model.thinking_level_map.as_ref();
    let mapped = |name: &str| map.and_then(|levels| levels.get(name));
    let default_on = |name: &str| !matches!(mapped(name), Some(None));
    let named = |name: &str| matches!(mapped(name), Some(Some(_)));
    let accepted = [
        (ThinkingLevel::Minimal, default_on("minimal")),
        (ThinkingLevel::Low, default_on("low")),
        (ThinkingLevel::Medium, default_on("medium")),
        (ThinkingLevel::High, default_on("high")),
        (ThinkingLevel::Xhigh, named("xhigh")),
        (ThinkingLevel::Max, named("max")),
    ]
    .into_iter()
    .filter_map(|(level, supported)| supported.then_some(level))
    .collect();
    ThinkingSupport::OpenAi {
        accepted,
        none_supported: default_on("off"),
    }
}

fn gateway_price(cost: &GatewayCost) -> Option<ModelPrice> {
    let rates = [cost.input, cost.output, cost.cache_read, cost.cache_write];
    if !rates.iter().all(|rate| rate.is_finite() && *rate >= 0.0) {
        return None;
    }
    Some(ModelPrice {
        input: cost.input,
        cached_input: cost.cache_read,
        output: cost.output,
        reasoning: cost.output,
        cache_write: Some(cost.cache_write),
        tiers: Box::default(),
    })
}

#[derive(Deserialize)]
struct GatewayConfig {
    /// Required by the reference sanitizer; the provider entry owns the endpoint.
    #[serde(rename = "baseUrl")]
    _base_url: String,
    models: Vec<dal_core::RawJson>,
}

#[derive(Deserialize)]
struct GatewayModel {
    id: String,
    name: String,
    reasoning: bool,
    #[serde(default, rename = "thinkingLevelMap")]
    thinking_level_map: Option<std::collections::HashMap<String, Option<String>>>,
    input: Vec<String>,
    cost: RawJson,
    #[serde(rename = "contextWindow")]
    context_window: u64,
    #[serde(rename = "maxTokens")]
    max_tokens: u64,
}

#[derive(Deserialize)]
struct GatewayCost {
    #[serde(default)]
    input: f64,
    #[serde(default)]
    output: f64,
    #[serde(default, rename = "cacheRead")]
    cache_read: f64,
    #[serde(default, rename = "cacheWrite")]
    cache_write: f64,
}

pub(crate) fn decode_copilot_models(
    provider: &ProviderEntry,
    bytes: &[u8],
) -> Result<Vec<CatalogEntry>, ProviderError> {
    let models = decode_models(bytes)?;
    Ok(copilot_rows(provider, &models))
}

fn copilot_rows(provider: &ProviderEntry, models: &[CopilotModel]) -> Vec<CatalogEntry> {
    let surface = Surface::of_family(provider.family);
    models
        .iter()
        .filter(|model| Some(model.surface) == surface)
        .map(|model| copilot_entry(provider, model))
        .collect()
}

fn copilot_entry(provider: &ProviderEntry, model: &CopilotModel) -> CatalogEntry {
    let mut entry = default_entry(provider, &sanitize_identifier(&model.id));
    entry.display = sanitize_identifier(&model.name);
    entry.context_window = model.context_window;
    entry.max_output = model.max_output;
    entry.image_input = model.image_input.unwrap_or(false);
    if model.tool_calls == Some(false) {
        entry.tool_support = ToolSupport::None;
    }
    entry.premium_multiplier = model
        .premium_multiplier
        .filter(|multiplier| multiplier.is_finite() && *multiplier >= 0.0);
    entry.price = model.price.and_then(copilot_price);
    bind_family_capabilities(&mut entry, provider.family);
    entry
}

fn copilot_price(price: Price) -> Option<ModelPrice> {
    if ![price.input, price.output, price.cache_read]
        .iter()
        .all(|rate| rate.is_finite() && *rate >= 0.0)
    {
        return None;
    }
    Some(ModelPrice {
        input: price.input,
        cached_input: price.cache_read,
        output: price.output,
        reasoning: 0.0,
        cache_write: price
            .cache_write
            .filter(|rate| rate.is_finite() && *rate >= 0.0),
        tiers: Box::default(),
    })
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
                premium_multiplier: None,
                price: None,
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
