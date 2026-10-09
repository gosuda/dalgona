//! Model reference resolution over fetched, cached, and built-in rows.
//!
//! Resolution order is fixed: aliases expand once, `dalgon/` ids take the
//! harness route, `<provider>/<id>` looks up that provider's rows, and bare
//! ids search every row. Unlisted ids on a typed-source provider fall back to
//! family defaults without fabricated limits.

use dal_core::{Family, ModelRoute, ThinkingLevel};

use super::compiled_temperature;
use super::{Catalog, CatalogEntry, CatalogSource, Listing, ResolvedModel, ToolSupport};
use crate::{error::ResolveError, provider::ProviderEntry, thinking::ThinkingSupport};

/// Resolves a provider alias, qualified reference, harness mode, or bare id.
///
/// References are split at their first slash, preserving any remaining slash
/// in the provider-native model id. Aliases are expanded once before parsing.
///
/// # Errors
///
/// Returns [`ResolveError::UnknownModel`] when no provider/model pair
/// matches, or [`ResolveError::AmbiguousModel`] when a bare id matches
/// several providers.
pub fn resolve(
    catalog: &Catalog,
    aliases: &[(Box<str>, Box<str>)],
    reference: &str,
) -> Result<ResolvedModel, ResolveError> {
    let expanded = aliases
        .iter()
        .find(|(name, _)| name.as_ref() == reference)
        .map_or(reference, |(_, target)| target.as_ref());
    resolve_expanded(catalog, expanded)
}

/// Resolves the model a route names.
///
/// A route keeps its API family but not the provider prefix its model was
/// configured with, so `openai-responses/gpt-6` becomes the bare id `gpt-6`.
/// A catalog built without a model list resolves only provider-qualified
/// ids. The bare id therefore resolves first, and an unknown bare id retries
/// under the family's own provider prefix. Synthetic and harness routes
/// resolve by their id.
///
/// # Errors
/// Returns the bare-id error when both spellings fail.
pub fn resolve_route(
    catalog: &Catalog,
    aliases: &[(Box<str>, Box<str>)],
    route: &ModelRoute,
) -> Result<ResolvedModel, ResolveError> {
    let ModelRoute::Api { family, model } = route else {
        return resolve(catalog, aliases, route.id());
    };
    let bare = resolve(catalog, aliases, model);
    if !matches!(bare, Err(ResolveError::UnknownModel { .. })) {
        return bare;
    }
    let prefix = match family {
        Family::Chat => "openai-chat",
        Family::Responses => "openai-responses",
        Family::Codex => "openai-codex",
        Family::Anthropic => "anthropic",
    };
    resolve(catalog, aliases, &format!("{prefix}/{model}")).or(bare)
}

fn resolve_expanded(catalog: &Catalog, reference: &str) -> Result<ResolvedModel, ResolveError> {
    if reference.starts_with("dalgon/") {
        return resolve_harness(reference);
    }
    if let Some((provider, id)) = reference.split_once('/') {
        if let Some(provider_entry) = catalog
            .providers
            .iter()
            .find(|(candidate, _)| candidate.id.as_ref() == provider)
            .map(|(provider, _)| provider)
        {
            return resolve_qualified(catalog, provider_entry, id, reference);
        }
        if let Some((owner, family)) = family_provider(provider)
            && let Some(provider_entry) = catalog
                .providers
                .iter()
                .find(|(candidate, _)| candidate.id.as_ref() == owner)
                .map(|(provider, _)| provider)
                .filter(|provider_entry| provider_entry.family == family)
        {
            return resolve_qualified(catalog, provider_entry, id, reference);
        }
        return resolve_unknown_provider(catalog, reference);
    }
    resolve_bare(catalog, reference)
}

/// Maps a family-qualified route prefix to the provider that serves it.
fn family_provider(prefix: &str) -> Option<(&'static str, Family)> {
    match prefix {
        "openai-responses" => Some(("openai", Family::Responses)),
        "openai-chat" => Some(("openai", Family::Chat)),
        _ => None,
    }
}

fn resolve_harness(reference: &str) -> Result<ResolvedModel, ResolveError> {
    let route = ModelRoute::harness(reference.to_owned()).map_err(|_| unknown_model(reference))?;
    let entry = CatalogEntry {
        provider: "dalgon".into(),
        id: reference.into(),
        display: reference.into(),
        listing: Listing::Listed,
        context_window: None,
        max_output: None,
        thinking: default_thinking(Family::Responses),
        image_input: false,
        image_profile: None,
        remote_compact: false,
        supports_reasoning_summaries: false,
        temperature_allowed: false,
        display_supported: false,
        tool_support: ToolSupport::None,
        custom_grammar: false,
    };
    Ok(ResolvedModel {
        provider: "dalgon".into(),
        route,
        entry,
    })
}

fn resolve_qualified(
    catalog: &Catalog,
    provider: &ProviderEntry,
    id: &str,
    reference: &str,
) -> Result<ResolvedModel, ResolveError> {
    if id.is_empty() {
        return Err(unknown_model(reference));
    }
    let listed = listing_candidates(id)
        .into_iter()
        .flatten()
        .find_map(|candidate| {
            catalog
                .entries
                .iter()
                .find(|entry| entry.provider == provider.id && entry.id.as_ref() == candidate)
        });
    let mut entry = if let Some(entry) = listed {
        entry.clone()
    } else {
        if has_hidden_entry_elsewhere(catalog, provider, id)
            || catalog.source_for(&provider.id) != CatalogSource::Typed
        {
            return Err(unknown_model(reference));
        }
        typed_entry(catalog, provider, id)
    };
    bind_family_capabilities(&mut entry, provider.family);
    let route = ModelRoute::Api {
        family: provider.family,
        model: id.into(),
    };
    Ok(ResolvedModel {
        provider: provider.id.clone(),
        route,
        entry,
    })
}

fn resolve_unknown_provider(
    catalog: &Catalog,
    reference: &str,
) -> Result<ResolvedModel, ResolveError> {
    resolve_bare(catalog, reference)
}

fn resolve_bare(catalog: &Catalog, id: &str) -> Result<ResolvedModel, ResolveError> {
    let mut matches: Vec<&CatalogEntry> = Vec::new();
    for candidate in listing_candidates(id).into_iter().flatten() {
        for entry in &catalog.entries {
            if entry.id.as_ref() == candidate
                && !matches
                    .iter()
                    .any(|matched| matched.provider == entry.provider)
            {
                matches.push(entry);
            }
        }
        if !matches.is_empty() {
            break;
        }
    }
    match matches.as_slice() {
        [] => Err(unknown_model(id)),
        [entry] => {
            let provider = catalog
                .providers
                .iter()
                .find(|(provider, _)| provider.id == entry.provider)
                .map(|(provider, _)| provider)
                .ok_or_else(|| unknown_model(id))?;
            resolve_qualified(catalog, provider, id, id)
        }
        _ => {
            let candidates = matches
                .iter()
                .map(|entry| format!("{}/{}", entry.provider, entry.id))
                .collect::<Vec<_>>()
                .join(", ");
            Err(ResolveError::AmbiguousModel {
                id: id.to_owned(),
                candidates,
            })
        }
    }
}
pub(crate) fn typed_entry(catalog: &Catalog, provider: &ProviderEntry, id: &str) -> CatalogEntry {
    if let Some(row) = capability_row(&catalog.entries, provider, id) {
        let mut entry = row.clone();
        entry.provider.clone_from(&provider.id);
        entry.id = id.into();
        entry.listing = Listing::Listed;
        return entry;
    }
    default_entry(provider, id)
}

pub(crate) fn capability_row<'a>(
    rows: &'a [CatalogEntry],
    provider: &ProviderEntry,
    id: &str,
) -> Option<&'a CatalogEntry> {
    for candidate in model_candidates(id).into_iter().flatten() {
        if let Some(row) = rows
            .iter()
            .find(|entry| entry.provider == provider.id && entry.id.as_ref() == candidate)
        {
            return Some(row);
        }
        if is_openai_family(provider.family)
            && let Some(row) = rows
                .iter()
                .find(|entry| entry.provider.as_ref() == "openai" && entry.id.as_ref() == candidate)
        {
            return Some(row);
        }
    }
    None
}

fn has_hidden_entry_elsewhere(catalog: &Catalog, provider: &ProviderEntry, id: &str) -> bool {
    listing_candidates(id)
        .into_iter()
        .flatten()
        .any(|candidate| {
            catalog.entries.iter().any(|entry| {
                entry.provider != provider.id
                    && entry.id.as_ref() == candidate
                    && entry.listing == Listing::Hidden
            })
        })
}

fn listing_candidates(id: &str) -> [Option<&str>; 3] {
    let date = id.rsplit_once('-').and_then(|(base, suffix)| {
        (suffix.len() == 8 && suffix.bytes().all(|byte| byte.is_ascii_digit())).then_some(base)
    });
    let latest = id.strip_suffix("-latest");
    [Some(id), date, latest]
}

fn model_candidates(id: &str) -> [Option<&str>; 4] {
    let [exact, date, latest] = listing_candidates(id);
    let vendor = id
        .split_once('/')
        .and_then(|(_, remainder)| (!remainder.is_empty()).then_some(remainder));
    [exact, date, latest, vendor]
}

fn unknown_model(reference: &str) -> ResolveError {
    ResolveError::UnknownModel {
        reference: reference.to_owned(),
    }
}

fn is_openai_family(family: Family) -> bool {
    matches!(family, Family::Chat | Family::Responses)
}

pub(crate) fn default_entry(provider: &ProviderEntry, id: &str) -> CatalogEntry {
    CatalogEntry {
        provider: provider.id.clone(),
        id: id.into(),
        display: id.into(),
        listing: Listing::Listed,
        context_window: None,
        max_output: None,
        thinking: default_thinking(provider.family),
        image_input: false,
        image_profile: None,
        remote_compact: false,
        supports_reasoning_summaries: false,
        temperature_allowed: compiled_temperature(provider.id.as_ref(), id),
        display_supported: false,
        tool_support: ToolSupport::Any,
        custom_grammar: false,
    }
}

pub(crate) fn bind_family_capabilities(entry: &mut CatalogEntry, family: Family) {
    if family == Family::Chat {
        entry.remote_compact = false;
    }
}

fn default_thinking(family: Family) -> ThinkingSupport {
    match family {
        Family::Chat | Family::Responses | Family::Codex => ThinkingSupport::OpenAi {
            accepted: vec![
                ThinkingLevel::Minimal,
                ThinkingLevel::Low,
                ThinkingLevel::Medium,
                ThinkingLevel::High,
                ThinkingLevel::Xhigh,
                ThinkingLevel::Max,
            ],
            none_supported: true,
        },
        Family::Anthropic => ThinkingSupport::UnknownAdaptive,
    }
}
