//! Model reference resolution over fetched, cached, and built-in rows.
//!
//! Resolution order is fixed: aliases expand once, `dalgon/` ids take the
//! harness route, `<provider>/<id>` looks up that provider's rows, and bare
//! ids search every row. Unlisted ids on a typed-source provider fall back to
//! family defaults without fabricated limits.

use dal_core::{Family, ModelRoute, ThinkingLevel};

use super::compiled_temperature;
use super::{Catalog, CatalogEntry, CatalogSource, Listing, ResolvedModel, ToolSupport};
use crate::{
    error::ResolveError,
    family::gitlab::{ProxyRoute, proxy_route},
    provider::{OPENAI, ProviderEntry},
    thinking::ThinkingSupport,
};

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
    resolve_expanded(catalog, expand_alias(aliases, reference), None)
}

/// Resolves the model a route names.
///
/// A route that names its provider selects exactly that provider's row:
/// aliases, the bare-id search, and the family retry never run, so another
/// provider listing the same native id is never bound in its place. When the
/// provider is not configured or serves another API family the route fails
/// typed ([`ResolveError::UnknownProvider`],
/// [`ResolveError::ProviderFamilyMismatch`]) instead of resolving elsewhere.
///
/// A route without a provider keeps its API family but not the provider
/// prefix its model was configured with, so `openai-responses/gpt-6`
/// becomes the bare id `gpt-6`. A catalog built without a model list
/// resolves only provider-qualified ids. The bare id therefore resolves
/// first. When it is unknown, or ambiguous between providers of the family,
/// the route retries under the catalog's single configured owner for that
/// family. Among several providers of one family, a Chat or Responses route
/// retries under `openai`, the provider the `openai-chat/` and
/// `openai-responses/` references name, when that row is configured, serves
/// the family and resolves the id. That includes an id several providers
/// list. A retry resolves like a qualified id, so a catalog that lists
/// models still rejects an id its owner does not list. Every other case
/// keeps the bare error, so custom providers that share a family and an id
/// stay ambiguous. Synthetic and harness routes resolve by their id.
///
/// # Errors
/// Returns the bare-id error when both spellings fail, or when the only
/// match belongs to a different API family than the route; for a route that
/// names its provider, the typed provider error or the model error of that
/// provider's own row.
pub fn resolve_route(
    catalog: &Catalog,
    aliases: &[(Box<str>, Box<str>)],
    route: &ModelRoute,
) -> Result<ResolvedModel, ResolveError> {
    let ModelRoute::Api {
        family,
        model,
        provider,
    } = route
    else {
        return resolve(catalog, aliases, route.id());
    };
    if let Some(provider) = provider {
        return resolve_provider_route(catalog, *family, provider, model);
    }
    let in_family = |resolved: ResolvedModel| {
        matches!(&resolved.route, ModelRoute::Api { family: found, .. } if found == family)
            .then_some(resolved)
    };
    let bare = resolve_expanded(catalog, expand_alias(aliases, model), Some(*family))
        .and_then(|resolved| in_family(resolved).ok_or_else(|| unknown_model(model)));
    if !matches!(
        bare,
        Err(ResolveError::UnknownModel { .. } | ResolveError::AmbiguousModel { .. })
    ) {
        return bare;
    }
    // Every family retries under the catalog's single configured owner for
    // that family when there is one; a lone custom Chat or Responses owner
    // serves typed routes without guessing. When several owners share the
    // family, Chat and Responses retry under `openai`, the owner the
    // `openai-chat/` and `openai-responses/` references name, when that row
    // serves the family; the retry also settles an id several owners list.
    // Other families, and several owners without that row, keep the bare
    // error.
    let owner =
        single_family_owner(catalog, *family).or_else(|| canonical_family_owner(catalog, *family));
    let Some(owner) = owner else {
        return bare;
    };
    if model.is_empty() {
        return bare;
    }
    match resolve(catalog, aliases, &format!("{owner}/{model}"))
        .ok()
        .and_then(in_family)
    {
        Some(resolved) => Ok(resolved),
        None => bare,
    }
}

/// Resolves the exact row a provider-scoped route names.
///
/// The provider is looked up by id before anything else; the native id is
/// never alias-expanded or searched across providers, and a provider that
/// serves another family than the route is refused rather than reinterpreted.
fn resolve_provider_route(
    catalog: &Catalog,
    family: Family,
    provider: &str,
    model: &str,
) -> Result<ResolvedModel, ResolveError> {
    let Some(entry) = catalog
        .providers
        .iter()
        .find(|(candidate, _)| candidate.id.as_ref() == provider)
        .map(|(entry, _)| entry)
    else {
        return Err(ResolveError::UnknownProvider {
            provider: provider.to_owned(),
            model: model.to_owned(),
        });
    };
    if entry.family != family {
        return Err(ResolveError::ProviderFamilyMismatch {
            provider: provider.to_owned(),
            model: model.to_owned(),
        });
    }
    resolve_qualified(catalog, entry, model, &format!("{provider}/{model}"))
}

fn expand_alias<'a>(aliases: &'a [(Box<str>, Box<str>)], reference: &'a str) -> &'a str {
    aliases
        .iter()
        .find(|(name, _)| name.as_ref() == reference)
        .map_or(reference, |(_, target)| target.as_ref())
}

fn resolve_expanded(
    catalog: &Catalog,
    reference: &str,
    family: Option<Family>,
) -> Result<ResolvedModel, ResolveError> {
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
        if let Some((owner, owner_family)) = family_provider(provider)
            && let Some(provider_entry) = catalog
                .providers
                .iter()
                .find(|(candidate, _)| candidate.id.as_ref() == owner)
                .map(|(provider, _)| provider)
                .filter(|provider_entry| provider_entry.family == owner_family)
        {
            return resolve_qualified(catalog, provider_entry, id, reference);
        }
        return resolve_bare(catalog, reference, family);
    }
    resolve_bare(catalog, reference, family)
}

/// The catalog's only configured provider serving `family`, when exactly one does.
///
/// Route retry must never guess among several same-family providers: `ollama`
/// and `ollama-cloud` share a family but not an endpoint, so a family-wide
/// guess would bind cloud ids to the local row. It must also not prefer a
/// built-in row the catalog does not configure. Zero or several owners mean no
/// census retry; only `canonical_family_owner` may still name one.
fn single_family_owner(catalog: &Catalog, family: Family) -> Option<&str> {
    let mut owners = catalog
        .providers
        .iter()
        .filter(|(provider, _)| provider.family == family)
        .map(|(provider, _)| provider.id.as_ref());
    let only = owners.next()?;
    owners.next().is_none().then_some(only)
}

/// The configured catalog entry for the family's canonical owner, when the
/// route grammar names one. `openai-responses` and `openai-chat` both bind
/// to `openai`; the family check rejects a custom `openai` row that no
/// longer serves the requested family.
fn canonical_family_owner(catalog: &Catalog, family: Family) -> Option<&str> {
    let prefix = match family {
        Family::Chat => "openai-chat",
        Family::Responses => "openai-responses",
        _ => return None,
    };
    let (owner, owner_family) = family_provider(prefix)?;
    catalog
        .providers
        .iter()
        .find(|(candidate, _)| candidate.id.as_ref() == owner && candidate.family == owner_family)
        .map(|(provider, _)| provider.id.as_ref())
}

/// Maps a family-qualified route prefix to the provider that serves it.
/// `openai-chat` resolves to the `openai` row when that row serves the Chat
/// family; the family filter rejects a custom `openai` rebind.
fn family_provider(prefix: &str) -> Option<(&'static str, Family)> {
    match prefix {
        "openai-responses" => Some((OPENAI.id, Family::Responses)),
        "openai-chat" => Some((OPENAI.id, Family::Chat)),
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
        premium_multiplier: None,
        price: None,
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
        let mut entry = entry.clone();
        entry.id = id.into();
        entry
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
        provider: Some(provider.id.clone()),
    };
    Ok(ResolvedModel {
        provider: provider.id.clone(),
        route,
        entry,
    })
}

fn resolve_bare(
    catalog: &Catalog,
    id: &str,
    family: Option<Family>,
) -> Result<ResolvedModel, ResolveError> {
    let serves_family = |entry: &CatalogEntry| {
        family.is_none_or(|family| {
            catalog
                .providers
                .iter()
                .any(|(provider, _)| provider.id == entry.provider && provider.family == family)
        })
    };
    let mut matches: Vec<&CatalogEntry> = Vec::new();
    for candidate in listing_candidates(id).into_iter().flatten() {
        for entry in &catalog.entries {
            if entry.id.as_ref() == candidate
                && serves_family(entry)
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
            && let Some(row) = rows.iter().find(|entry| {
                entry.provider.as_ref() == OPENAI.id && entry.id.as_ref() == candidate
            })
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
        thinking: entry_thinking(provider, id),
        image_input: false,
        image_profile: None,
        remote_compact: false,
        supports_reasoning_summaries: false,
        temperature_allowed: compiled_temperature(provider.id.as_ref(), id),
        display_supported: false,
        tool_support: entry_tool_support(provider.family),
        custom_grammar: false,
        premium_multiplier: None,
        price: None,
    }
}

fn entry_thinking(provider: &ProviderEntry, id: &str) -> ThinkingSupport {
    if provider.family == Family::GitLab {
        return gitlab_thinking(id);
    }
    default_thinking(provider.family)
}

/// Thinking support for one GitLab model id, matching the proxy route the
/// request will take rather than the shared family label.
///
/// The Anthropic route lowers through the Anthropic encoder and needs the
/// Anthropic contract; the Responses and Completions routes lower through
/// the `OpenAI` encoders and need the `OpenAI` ladder. This mirrors
/// [`crate::family::gitlab::proxy_route`], the same function the request
/// path uses to pick its encoder.
fn gitlab_thinking(id: &str) -> ThinkingSupport {
    match proxy_route(id) {
        ProxyRoute::Anthropic => ThinkingSupport::UnknownAdaptive,
        ProxyRoute::Responses | ProxyRoute::Completions => ThinkingSupport::OpenAi {
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
    }
}

fn entry_tool_support(family: Family) -> ToolSupport {
    if family == Family::GoogleGeminiCli {
        return ToolSupport::None;
    }
    ToolSupport::Any
}

pub(crate) fn bind_family_capabilities(entry: &mut CatalogEntry, family: Family) {
    if family == Family::Chat {
        entry.remote_compact = false;
    }
    if family == Family::GoogleGeminiCli {
        entry.tool_support = ToolSupport::None;
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
        // Bedrock speaks the Converse wire with the Anthropic thinking
        // fragment (`additionalModelRequestFields.thinking`); an OpenAI
        // effort has no member there and `bedrock::request_body` rejects it
        // as `ForeignThinking`. Typed Bedrock rows therefore carry the
        // Anthropic contract, matching the Anthropic family default.
        Family::Anthropic | Family::Bedrock => ThinkingSupport::UnknownAdaptive,
        Family::Cursor
        | Family::Devin
        | Family::Droid
        | Family::GitLab
        | Family::GoogleGenerativeAi
        | Family::GoogleVertex
        | Family::GoogleGeminiCli
        | Family::GoogleAntigravity
        | Family::Ollama
        | Family::PiMessages => ThinkingSupport::OpenAi {
            accepted: Vec::new(),
            none_supported: true,
        },
        // `reasoning_effort: none` only exists on the models that reason, and
        // a card says whether one does; a row without that knowledge sends no
        // reasoning control at all instead of one the model may reject.
        Family::Mistral => ThinkingSupport::OpenAi {
            accepted: Vec::new(),
            none_supported: false,
        },
    }
}
