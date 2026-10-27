//! Joins Cursor account model availability and optional model metadata.
//!
//! A metadata failure preserves usable models. If neither list decodes, the
//! caller falls back to cached models and typed identifiers. The user's
//! `models.default` setting selects the default model.
//!
//! # Normalization contract
//!
//! Every id, display name, context limit, and image flag comes from the wire
//! or from the verified catalog source dal already trusts
//! ([`capability_row`], the checked-in rows; [`default_entry`], the family
//! defaults, and the compiled temperature snapshot behind them). There are
//! no guessed defaults: Cursor's 1M display-name heuristics and the
//! reference's 200k/64k fallbacks are deliberately not ported. A row the
//! wire gives no context number for keeps `context_window: None` — the same
//! never-invent-limits mechanism [`super::decode`] uses for Devin.
//!
//! Efforts are not ladders here: dal routes Cursor reasoning through the
//! model id (`gpt-5.5-high`), so a variant publishes its wire-named slug as
//! its own row and the per-row ladder stays the family default.

use std::{collections::HashSet, time::Duration};

use dal_core::Family;
use prost::Message;

use super::CatalogEntry;
use crate::{error::ProviderError, family::cursor::proto::ModelDetails, provider::ProviderEntry};

/// The usable-models RPC path, relative to the provider base URL.
pub(crate) const USABLE_PATH: &str = "/agent.v1.AgentService/GetUsableModels";

/// The rich metadata RPC path, relative to the provider base URL.
pub(crate) const AVAILABLE_PATH: &str = "/aiserver.v1.AiService/AvailableModels";

/// The single deadline the whole join runs under: the reference gives each
/// model-list RPC 5 s, and one deadline cancels every request together.
pub(crate) const JOIN_DEADLINE: Duration = Duration::from_secs(5);

/// The optional rich-metadata query's own budget, inside the join deadline.
pub(crate) const AVAILABLE_MODELS_BUDGET: Duration = Duration::from_secs(2);

/// A `GetUsableModelsRequest`: the custom ids to include; dal sends none.
///
/// Field numbers verbatim from the pinned `cursor-proto.ts`.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct GetUsableModelsRequest {
    /// Custom model ids to include in request context (field 1).
    #[prost(string, repeated, tag = "1")]
    pub(crate) custom_model_ids: Vec<String>,
}

/// A `GetUsableModelsResponse`: the account-usable models (field 1).
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct GetUsableModelsResponse {
    /// The exact legacy slugs the account can run (field 1).
    #[prost(message, repeated, tag = "1")]
    pub(crate) models: Vec<ModelDetails>,
}

/// The `aiserver.v1.AvailableModels` unary pair.
///
/// Only the fields the normalization reads are modeled; every other field
/// decodes into nothing (`prost` skips unknown tags), so a newer server
/// stays wire-compatible.
pub(crate) mod aiserver {
    /// An `AvailableModelsRequest` (fields 2, 5, 7, 12 of the pinned proto).
    #[derive(Clone, PartialEq, prost::Message)]
    pub(crate) struct AvailableModelsRequest {
        /// Include long-context models (field 2).
        #[prost(bool, tag = "2")]
        pub(crate) include_long_context_models: bool,
        /// Present parameter axes and variants (field 5).
        #[prost(bool, optional, tag = "5")]
        pub(crate) use_model_parameters: Option<bool>,
        /// Send plain text instead of markdown tooltips (field 7).
        #[prost(bool, optional, tag = "7")]
        pub(crate) do_not_use_markdown: Option<bool>,
        /// Present cloud-agent effort modes as variants (field 12).
        #[prost(bool, optional, tag = "12")]
        pub(crate) use_cloud_agent_effort_modes: Option<bool>,
    }

    /// An `AvailableModelsResponse`: the rich models (field 2).
    #[derive(Clone, PartialEq, prost::Message)]
    pub(crate) struct AvailableModelsResponse {
        /// The rich model list (field 2).
        #[prost(message, repeated, tag = "2")]
        pub(crate) models: Vec<AvailableModelsModelDetails>,
    }

    /// One `AvailableModelsResponse.ModelDetails`: the fields the
    /// normalization reads of the pinned proto's 47.
    #[derive(Clone, PartialEq, prost::Message)]
    pub(crate) struct AvailableModelsModelDetails {
        /// The base model id (field 1).
        #[prost(string, tag = "1")]
        pub(crate) name: String,
        /// Chat-only models never surface as agent rows (field 4).
        #[prost(bool, optional, tag = "4")]
        pub(crate) is_chat_only: Option<bool>,
        /// `false` removes the model from agent routes (field 5).
        #[prost(bool, optional, tag = "5")]
        pub(crate) supports_agent: Option<bool>,
        /// Whether the model accepts image input (field 10).
        #[prost(bool, optional, tag = "10")]
        pub(crate) supports_images: Option<bool>,
        /// The standard-mode context limit (field 15).
        #[prost(uint32, optional, tag = "15")]
        pub(crate) context_token_limit: Option<u32>,
        /// The max-mode context limit (field 16).
        #[prost(uint32, optional, tag = "16")]
        pub(crate) context_token_limit_for_max_mode: Option<u32>,
        /// The picker display name (field 17).
        #[prost(string, optional, tag = "17")]
        pub(crate) client_display_name: Option<String>,
        /// The server-side display name (field 18).
        #[prost(string, optional, tag = "18")]
        pub(crate) server_model_name: Option<String>,
        /// Parameter definitions backing the variants (field 29).
        #[prost(message, repeated, tag = "29")]
        pub(crate) parameter_definitions: Vec<ModelParameterDefinition>,
        /// The parameter configurations (field 30).
        #[prost(message, repeated, tag = "30")]
        pub(crate) variants: Vec<ModelVariantConfig>,
        /// The legacy slugs (field 36).
        #[prost(string, repeated, tag = "36")]
        pub(crate) legacy_slugs: Vec<String>,
        /// The id aliases (field 37).
        #[prost(string, repeated, tag = "37")]
        pub(crate) id_aliases: Vec<String>,
        /// Zero-data-retention-required models are never agent rows (field
        /// 46).
        #[prost(bool, optional, tag = "46")]
        pub(crate) requires_data_retention: Option<bool>,
    }

    /// One `ModelVariantConfig`: a routable parameter configuration.
    #[derive(Clone, PartialEq, prost::Message)]
    pub(crate) struct ModelVariantConfig {
        /// The parameter values (field 1).
        #[prost(message, repeated, tag = "1")]
        pub(crate) parameter_values: Vec<ModelParameterValue>,
        /// The variant display name (field 2).
        #[prost(string, optional, tag = "2")]
        pub(crate) display_name: Option<String>,
        /// Whether the route needs max mode (field 3).
        #[prost(bool, optional, tag = "3")]
        pub(crate) is_max_mode: Option<bool>,
        /// The variant's wire spelling (field 9).
        #[prost(string, optional, tag = "9")]
        pub(crate) variant_string_representation: Option<String>,
        /// The variant's legacy run slug (field 11).
        #[prost(string, optional, tag = "11")]
        pub(crate) legacy_slug: Option<String>,
    }

    /// One `ModelParameterValue`: an axis selection (fields 1, 2).
    #[derive(Clone, PartialEq, prost::Message)]
    pub(crate) struct ModelParameterValue {
        /// The parameter id (field 1).
        #[prost(string, tag = "1")]
        pub(crate) id: String,
        /// The selected value (field 2).
        #[prost(string, tag = "2")]
        pub(crate) value: String,
    }

    /// One `ModelParameterDefinition`: the axis and its typed values
    /// (fields 1, 4).
    #[derive(Clone, PartialEq, prost::Message)]
    pub(crate) struct ModelParameterDefinition {
        /// The parameter id (field 1).
        #[prost(string, tag = "1")]
        pub(crate) id: String,
        /// The typed definition (field 4).
        #[prost(message, optional, tag = "4")]
        pub(crate) parameter_type: Option<ModelParameterType>,
    }

    /// One `ModelParameterDefinition.ModelParameterType` (fields 1, 2).
    #[derive(Clone, PartialEq, prost::Message)]
    pub(crate) struct ModelParameterType {
        /// The boolean arm (field 1).
        #[prost(message, optional, tag = "1")]
        pub(crate) boolean_parameter: Option<BooleanParameterDefinition>,
        /// The enum arm (field 2).
        #[prost(message, optional, tag = "2")]
        pub(crate) enum_parameter: Option<EnumParameterDefinition>,
    }

    /// One `BooleanParameterDefinition` (field 1).
    #[derive(Clone, PartialEq, prost::Message)]
    pub(crate) struct BooleanParameterDefinition {
        /// The allowed values (field 1).
        #[prost(message, repeated, tag = "1")]
        pub(crate) values: Vec<BooleanParameterValue>,
    }

    /// One boolean `BooleanParameterValue`: fields 1 and 6. The declared
    /// cost flag stays unread — dal pricing has no Cursor multiplier
    /// consumer, and a subscription multiplier is not a USD rate.
    #[derive(Clone, PartialEq, prost::Message)]
    pub(crate) struct BooleanParameterValue {
        /// The value spelling (field 1).
        #[prost(string, tag = "1")]
        pub(crate) value: String,
        /// Whether an admin allowlist blocks the value (field 6).
        #[prost(bool, optional, tag = "6")]
        pub(crate) blocked_by_admin_allowlist: Option<bool>,
    }

    /// One `EnumParameterDefinition` (field 1).
    #[derive(Clone, PartialEq, prost::Message)]
    pub(crate) struct EnumParameterDefinition {
        /// The allowed values (field 1).
        #[prost(message, repeated, tag = "1")]
        pub(crate) values: Vec<EnumParameterValue>,
    }

    /// One enum `EnumParameterValue`: fields 1 and 4.
    #[derive(Clone, PartialEq, prost::Message)]
    pub(crate) struct EnumParameterValue {
        /// The value spelling (field 1).
        #[prost(string, tag = "1")]
        pub(crate) value: String,
        /// Whether an admin allowlist blocks the value (field 4).
        #[prost(bool, optional, tag = "4")]
        pub(crate) blocked_by_admin_allowlist: Option<bool>,
    }
}

/// Encodes the `GetUsableModelsRequest` dal sends: no custom ids.
#[must_use]
pub(crate) fn usable_request() -> Vec<u8> {
    GetUsableModelsRequest {
        custom_model_ids: Vec::new(),
    }
    .encode_to_vec()
}

/// Encodes the `AvailableModelsRequest` dal sends: the four pinned flags of
/// the reference discovery, exactly as it sets them.
#[must_use]
pub(crate) fn available_request() -> Vec<u8> {
    aiserver::AvailableModelsRequest {
        include_long_context_models: true,
        use_model_parameters: Some(true),
        do_not_use_markdown: Some(true),
        use_cloud_agent_effort_modes: Some(true),
    }
    .encode_to_vec()
}

/// Extracts the first message payload of a Connect unary body.
///
/// The pinned `decodeConnectUnaryBody`: 5-byte frames (flag byte, big-endian
/// length), a compressed frame is refused, an end-stream frame is skipped,
/// and the first plain frame wins. A body that parses as no frame at all is
/// returned whole, because edges variously answer bare protobuf.
fn unary_frame(payload: &[u8]) -> Option<&[u8]> {
    let mut offset = 0_usize;
    while offset + 5 <= payload.len() {
        let flags = payload[offset];
        let length = u32::from_be_bytes([
            payload[offset + 1],
            payload[offset + 2],
            payload[offset + 3],
            payload[offset + 4],
        ]);
        let frame_end = offset + 5 + usize::try_from(length).ok()?;
        if frame_end > payload.len() {
            return None;
        }
        if flags & 0b0000_0001 != 0 {
            return None;
        }
        if flags & 0b0000_0010 == 0 {
            return Some(&payload[offset + 5..frame_end]);
        }
        offset = frame_end;
    }
    None
}

/// Decodes one unary reply, envelope first, whole-body fallback second.
///
/// A missing or undecodable reply is `None`: every RPC of the join is
/// optional in exactly the reference's sense, and only a join with no model
/// list at all fails.
fn unary_message<M: Message + Default>(payload: Option<&[u8]>) -> Option<M> {
    let framed = unary_frame(payload?).unwrap_or(payload?);
    M::decode(framed).ok()
}

/// Joins the two model-list replies into catalog rows.
///
/// `usable` and `available` are the model-list bodies (`None` when the RPC
/// failed or answered nothing decodable); the `failure` is the first
/// model-list error, returned when neither list decoded so the caller falls
/// back to the cache and typed ids. When neither list failed to fetch but
/// both failed to decode, the typed protocol error is built here, never on
/// a successful join.
///
/// # Errors
///
/// Returns the `failure` when neither model list decoded.
pub(crate) fn decode_cursor_models(
    provider: &ProviderEntry,
    usable: Option<&[u8]>,
    available: Option<&[u8]>,
    failure: Option<ProviderError>,
) -> Result<Vec<CatalogEntry>, ProviderError> {
    let usable = usable.and_then(|bytes| unary_message::<GetUsableModelsResponse>(Some(bytes)));
    let available =
        available.and_then(|bytes| unary_message::<aiserver::AvailableModelsResponse>(Some(bytes)));

    if usable.is_none() && available.is_none() {
        return Err(failure.unwrap_or_else(|| {
            super::decode::protocol(Family::Cursor, "cursor catalog decoded no model list")
        }));
    }

    let builtins = super::builtin::built_in_entries();
    // Every id any decoded rich model claims, unfiltered: a usable slug one
    // of those ids already represents is never published twice (the
    // reference's `richCatalogIds` retention set).
    let rich_ids = rich_claimed_ids(available.as_ref());
    // The ids the usable list names, when it decoded: the entitlement filter
    // the reference applies to rich variants.
    let usable_ids = usable_entitlements(usable.as_ref());

    let mut rows = Vec::new();
    // Every row id published so far; the first spelling of an id wins.
    let mut claimed: HashSet<String> = HashSet::new();

    if let Some(available) = &available {
        for details in &available.models {
            let Some(base) = non_empty(&details.name) else {
                continue;
            };
            // The reference's three model-level filters; every one is a wire
            // fact, and each removal keeps out a route dal would always
            // refuse or that Cursor itself hides from agents.
            if details.supports_agent == Some(false)
                || details.is_chat_only == Some(true)
                || details.requires_data_retention == Some(true)
            {
                continue;
            }

            let model_display = client_display(details, base);
            let mut published = 0_usize;
            for variant in &details.variants {
                if admin_blocked(details, variant) {
                    continue;
                }
                // dal routes the exact wire spelling: a variant without a
                // wire-named id has no slug dal could send.
                let Some(id) = variant
                    .legacy_slug
                    .as_deref()
                    .and_then(non_empty)
                    .or_else(|| {
                        variant
                            .variant_string_representation
                            .as_deref()
                            .and_then(non_empty)
                    })
                else {
                    continue;
                };
                if let Some(entitled) = &usable_ids
                    && !std::iter::once(id)
                        .chain(variant.legacy_slug.as_deref())
                        .chain(std::iter::once(base))
                        .chain(details.id_aliases.iter().map(String::as_str))
                        .any(|candidate| entitled.contains(candidate))
                {
                    continue;
                }
                if !claimed.insert(String::from(id)) {
                    continue;
                }
                let mut entry = verified_row(provider, &builtins, id);
                entry.display = super::decode::sanitize_identifier(
                    variant
                        .display_name
                        .as_deref()
                        .and_then(non_empty)
                        .unwrap_or(model_display),
                );
                entry.context_window = variant_context(details, variant);
                entry.image_input = details.supports_images.unwrap_or(entry.image_input);
                rows.push(entry);
                published += 1;
            }

            if published == 0 {
                // No variant survived: the bare model, when the account can
                // run it under any of its wire-named spellings.
                let entitled = usable_ids.as_ref().is_none_or(|ids| {
                    std::iter::once(base)
                        .chain(details.legacy_slugs.iter().map(String::as_str))
                        .chain(details.id_aliases.iter().map(String::as_str))
                        .any(|candidate| ids.contains(candidate))
                });
                if entitled && claimed.insert(String::from(base)) {
                    let mut entry = verified_row(provider, &builtins, base);
                    entry.display = super::decode::sanitize_identifier(model_display);
                    entry.context_window = details.context_token_limit.filter(|limit| *limit > 0);
                    entry.image_input = details.supports_images.unwrap_or(entry.image_input);
                    rows.push(entry);
                }
            }
        }
    }

    if let Some(usable) = &usable {
        for details in &usable.models {
            let Some(id) = non_empty(&details.model_id) else {
                continue;
            };
            // Rich rows carry `legacy_slug`; keep a usable slug only when no
            // decoded rich model represents it, so schema drift cannot hide
            // a model the run endpoint accepts.
            if rich_ids.contains(id) || !claimed.insert(String::from(id)) {
                continue;
            }
            let mut entry = verified_row(provider, &builtins, id);
            entry.display = super::decode::sanitize_identifier(&usable_display(details, id));
            rows.push(entry);
        }
    }

    rows.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(rows)
}

/// Every id any decoded rich model claims, unfiltered: a usable slug one of
/// those ids already represents is never published twice (the reference's
/// `richCatalogIds` retention set).
fn rich_claimed_ids(available: Option<&aiserver::AvailableModelsResponse>) -> HashSet<String> {
    let Some(response) = available else {
        return HashSet::new();
    };
    let mut ids = HashSet::new();
    for details in &response.models {
        for candidate in std::iter::once(details.name.as_str())
            .chain(details.legacy_slugs.iter().map(String::as_str))
            .chain(details.id_aliases.iter().map(String::as_str))
            .chain(
                details
                    .variants
                    .iter()
                    .filter_map(|variant| variant.legacy_slug.as_deref()),
            )
            .chain(
                details
                    .variants
                    .iter()
                    .filter_map(|variant| variant.variant_string_representation.as_deref()),
            )
        {
            if let Some(candidate) = non_empty(candidate) {
                ids.insert(String::from(candidate));
            }
        }
    }
    ids
}

/// The ids the usable list names, when it decoded: the entitlement filter
/// the reference applies to rich variants.
fn usable_entitlements(usable: Option<&GetUsableModelsResponse>) -> Option<HashSet<String>> {
    Some(
        usable?
            .models
            .iter()
            .filter_map(|details| non_empty(&details.model_id))
            .map(String::from)
            .collect(),
    )
}

/// The first non-empty trimmed line of `value`, or `None`.
fn non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// The model-level display name: picker name, server name, then base id.
fn client_display<'a>(
    details: &'a aiserver::AvailableModelsModelDetails,
    base: &'a str,
) -> &'a str {
    details
        .client_display_name
        .as_deref()
        .and_then(non_empty)
        .or_else(|| details.server_model_name.as_deref().and_then(non_empty))
        .unwrap_or(base)
}

/// The usable row's display name: display name, short name, display id,
/// aliases, then the id itself (the reference's `pickModelDisplayName`).
fn usable_display(details: &ModelDetails, id: &str) -> String {
    for candidate in [
        details.display_name.as_str(),
        details.display_name_short.as_str(),
    ]
    .into_iter()
    .chain(std::iter::once(details.display_model_id.as_str()))
    .chain(details.aliases.iter().map(String::as_str))
    {
        if let Some(candidate) = non_empty(candidate) {
            return String::from(candidate);
        }
    }
    String::from(id)
}

/// The context limit one rich row carries: the variant's declared `context`
/// parameter when it parses, else the wire limit for the variant's mode.
///
/// No other signal participates: a row the wire gives no number for keeps
/// `None` and resolves like a typed id.
fn variant_context(
    details: &aiserver::AvailableModelsModelDetails,
    variant: &aiserver::ModelVariantConfig,
) -> Option<u32> {
    variant
        .parameter_values
        .iter()
        .find(|parameter| parameter.id.trim() == "context")
        .and_then(|parameter| parse_context_window(&parameter.value))
        .or_else(|| {
            let limit = if variant.is_max_mode == Some(true) {
                details.context_token_limit_for_max_mode
            } else {
                details.context_token_limit
            };
            limit.filter(|limit| *limit > 0)
        })
}

/// Parses a wire-declared context size: `200000`, `250k`, `2m`, in any
/// ASCII case (the reference's `parseCursorContextWindow`).
fn parse_context_window(value: &str) -> Option<u32> {
    let text = value.trim();
    let (digits, multiplier) = match text.bytes().next_back()? {
        b'k' | b'K' => (&text[..text.len() - 1], 1_000.0_f64),
        b'm' | b'M' => (&text[..text.len() - 1], 1_000_000.0_f64),
        // Any other byte, including a non-ASCII one, is part of the number
        // text; the parse below refuses it.
        _ => (text, 1.0_f64),
    };
    let amount: f64 = digits.parse().ok()?;
    if !amount.is_finite() || amount <= 0.0 {
        return None;
    }
    // The float product saturates only past every u32; the `u32::try_from`
    // below is the real range check and refuses the saturated value.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the saturation ceiling is refused by the u32 range check"
    )]
    let scaled = (amount * multiplier).round() as u64;
    u32::try_from(scaled).ok()
}

/// Whether any of the variant's parameter values is blocked by an admin
/// allowlist (the reference's `isRichVariantBlocked`).
fn admin_blocked(
    details: &aiserver::AvailableModelsModelDetails,
    variant: &aiserver::ModelVariantConfig,
) -> bool {
    variant.parameter_values.iter().any(|parameter| {
        details
            .parameter_definitions
            .iter()
            .find(|definition| definition.id == parameter.id)
            .and_then(|definition| definition.parameter_type.as_ref())
            .is_some_and(|kind| match kind {
                aiserver::ModelParameterType {
                    boolean_parameter: Some(boolean),
                    ..
                } => boolean.values.iter().any(|value| {
                    value.value == parameter.value && value.blocked_by_admin_allowlist == Some(true)
                }),
                aiserver::ModelParameterType {
                    enum_parameter: Some(enum_parameter),
                    ..
                } => enum_parameter.values.iter().any(|value| {
                    value.value == parameter.value && value.blocked_by_admin_allowlist == Some(true)
                }),
                aiserver::ModelParameterType {
                    boolean_parameter: None,
                    enum_parameter: None,
                } => false,
            })
    })
}

/// The verified row one discovered id starts from: a checked-in row when the
/// catalog trusts one, else the family default with no fabricated limit.
fn verified_row(provider: &ProviderEntry, builtins: &[CatalogEntry], id: &str) -> CatalogEntry {
    let mut entry = super::resolve::capability_row(builtins, provider, id)
        .cloned()
        .unwrap_or_else(|| super::resolve::default_entry(provider, id));
    entry.id = super::decode::sanitize_identifier(id);
    entry.listing = super::Listing::Listed;
    entry
}
