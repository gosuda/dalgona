//! Model reference resolution over fetched, cached, and built-in rows.
//!
//! Resolution order is fixed: aliases expand once, `dalgon/` ids take the
//! harness route, `<provider>/<id>` looks up that provider's rows, and bare
//! ids search every row. Unlisted ids on a typed-source provider fall back to
//! family defaults without fabricated limits. A row that serves several
//! wires is the one exception: an id no wire selects stays listed but
//! refuses typed ([`ResolveError::UnboundModel`]) instead of falling back,
//! and the resolved family is the selected wire's, not the row's.

use dal_core::{Family, ModelRoute, ThinkingLevel};

use super::compiled_temperature;
use super::{Catalog, CatalogEntry, CatalogSource, Listing, ResolvedModel, ToolSupport};
use crate::{
    error::ResolveError,
    family::{
        chat::Dialect,
        gitlab::{ProxyRoute, proxy_route},
    },
    provider::{
        OPENAI, Origin, ProviderEntry,
        wire::{self, ModelIdAlias, WireBase},
    },
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
/// On a row that serves several wires the selected wire's family stands in
/// for the row's, and an id no wire selects refuses typed
/// ([`ResolveError::UnboundModel`]) even though the row may still list it.
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
    let bound = wire::wire_for(entry, model);
    if entry.def.is_some_and(|def| !def.wires.is_empty()) && bound.is_none() {
        return Err(ResolveError::UnboundModel {
            provider: provider.to_owned(),
            model: model.to_owned(),
        });
    }
    if bound.map_or(entry.family, |bound| bound.family) != family {
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
    let bound = wire::wire_for(provider, id);
    // A row serving several wires binds only ids a wire selects: the id
    // stays listed in the catalog, but selection refuses typed and the
    // row's default family never substitutes.
    if provider.def.is_some_and(|def| !def.wires.is_empty()) && bound.is_none() {
        return Err(ResolveError::UnboundModel {
            provider: provider.id.to_string(),
            model: id.to_owned(),
        });
    }
    // An explicit `api` narrows the row to its configured family instead of
    // sitting decorative beside per-model wires.
    if let Some(bound) = bound
        && provider.origins.api == Origin::Explicit
        && bound.family != provider.family
    {
        return Err(ResolveError::ProviderFamilyMismatch {
            provider: provider.id.to_string(),
            model: id.to_owned(),
        });
    }
    // An explicit `base_url` is authoritative: a fixed-endpoint wire refuses
    // typed rather than silently replacing the configured host. A suffix
    // wire still applies; it carries a path fact, not a host. The BAI
    // Anthropic wire is the one exception: the pinned `bai.ts` derives
    // Anthropic from the explicit gateway base by stripping one trailing
    // `/v1` (Chat/Responses keep the base verbatim) and the request wire
    // re-adds `/v1/messages`, so the same operator host serves all three
    // wires and the explicit base wins over the bare-host default.
    if let Some(bound) = bound
        && matches!(bound.base, WireBase::Override(_))
        && provider.origins.base == Origin::Explicit
        && !(&*provider.id == "bai" && bound.family == Family::Anthropic)
    {
        return Err(ResolveError::FixedWireBase {
            provider: provider.id.to_string(),
            model: id.to_owned(),
        });
    }
    let family = bound.map_or(provider.family, |bound| bound.family);
    let alias = provider
        .def
        .map_or(ModelIdAlias::None, |def| def.model_id_alias);
    let listed = listing_candidates(id)
        .into_iter()
        .flatten()
        .find_map(|candidate| listed_row(&catalog.entries, provider, candidate, alias));
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
    bind_family_capabilities(&mut entry, family);
    let route = ModelRoute::Api {
        family,
        model: id.into(),
        provider: Some(provider.id.clone()),
    };
    Ok(ResolvedModel {
        provider: provider.id.clone(),
        route,
        entry,
    })
}

/// The catalog row one listing candidate names on this provider: the exact
/// spelling first, then — when the row aliases ids — the row whose id
/// differs only at dot/hyphen positions. Alias lookup never leaves the
/// provider's own rows, so a twin owned by another provider is never bound.
fn listed_row<'a>(
    rows: &'a [CatalogEntry],
    provider: &ProviderEntry,
    candidate: &str,
    alias: ModelIdAlias,
) -> Option<&'a CatalogEntry> {
    rows.iter()
        .find(|entry| entry.provider == provider.id && entry.id.as_ref() == candidate)
        .or_else(|| alias_row(rows, provider, candidate, alias))
}

/// The row whose id is the dot/hyphen twin of `candidate`, when the row
/// aliases ids. The exact spelling always wins above, so a row listing both
/// spellings is never shadowed by its own twin.
fn alias_row<'a>(
    rows: &'a [CatalogEntry],
    provider: &ProviderEntry,
    candidate: &str,
    alias: ModelIdAlias,
) -> Option<&'a CatalogEntry> {
    if alias != ModelIdAlias::DotHyphen {
        return None;
    }
    rows.iter().find(|entry| {
        entry.provider == provider.id
            && entry.id.as_ref() != candidate
            && dot_hyphen_twin(entry.id.as_ref(), candidate)
    })
}

/// Whether `a` and `b` are the same id up to dot/hyphen positions. The rule
/// is owned by the wire table ([`wire::dot_hyphen_twin`]) so selection and
/// listing agree; the queried id is never rewritten.
fn dot_hyphen_twin(a: &str, b: &str) -> bool {
    wire::dot_hyphen_twin(a, b)
}

/// Resolves a reference to the catalog rows a price lookup may consult.
///
/// Selection needs one winner, but pricing does not: an id several providers
/// list is ambiguous for resolution yet still has a rate on each listed row.
/// The list holds the resolved row when one exists, else every bare-search
/// match in catalog order — the same rows [`resolve`] already enumerates, so
/// alias expansion, listing candidates, hidden rows, and the configured-
/// provider check stay owned here instead of being re-derived by callers.
///
/// Returns entries as `(provider/id, live row price)` pairs; pricing
/// precedence (configured, live, compiled) is the caller's.
#[must_use]
pub fn price_candidates(
    catalog: &Catalog,
    aliases: &[(Box<str>, Box<str>)],
    reference: &str,
) -> Vec<(String, Option<dal_core::ModelPrice>)> {
    let reference = expand_alias(aliases, reference);
    if let Ok(resolved) = resolve_expanded(catalog, reference, None) {
        return vec![(
            format!("{}/{}", resolved.provider, resolved.entry.id),
            resolved.entry.price.clone(),
        )];
    }
    bare_matches(catalog, reference, None)
        .into_iter()
        .map(|entry| {
            (
                format!("{}/{}", entry.provider, entry.id),
                entry.price.clone(),
            )
        })
        .collect()
}

fn resolve_bare(
    catalog: &Catalog,
    id: &str,
    family: Option<Family>,
) -> Result<ResolvedModel, ResolveError> {
    let matches = bare_matches(catalog, id, family);
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

/// The bare-search match set: every catalog row a listing candidate names on
/// a configured provider, deduplicated per provider, in catalog order. An
/// empty family accepts the row's own family; a named family also accepts a
/// row bound to that family's wire unless an explicit `api` narrowed it.
fn bare_matches<'a>(
    catalog: &'a Catalog,
    id: &str,
    family: Option<Family>,
) -> Vec<&'a CatalogEntry> {
    let serves_family = |entry: &CatalogEntry| {
        catalog.providers.iter().any(|(provider, _)| {
            provider.id == entry.provider
                && family.is_none_or(|family| {
                    provider.family == family
                        // A wire family also serves the bare search, so an
                        // id bound to another family's wire is reachable
                        // unqualified — unless the user's explicit `api`
                        // narrowed the row to its configured family.
                        || (provider.origins.api != Origin::Explicit
                            && provider.def.is_some_and(|def| {
                                def.wires.iter().any(|bound| bound.family == family)
                            }))
                })
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
    matches
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
    let alias = provider
        .def
        .map_or(ModelIdAlias::None, |def| def.model_id_alias);
    for candidate in model_candidates(id).into_iter().flatten() {
        if let Some(row) = listed_row(rows, provider, candidate, alias) {
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
        tool_support: entry_tool_support(provider, id),
        custom_grammar: false,
        premium_multiplier: None,
        price: None,
    }
}

fn entry_thinking(provider: &ProviderEntry, id: &str) -> ThinkingSupport {
    // The wire the id binds decides the contract: a Chat-family row serving
    // an Anthropic-wire id needs the Anthropic default, and the chat dialect
    // hook applies only to Chat wires.
    let family = wire::wire_for(provider, id).map_or(provider.family, |bound| bound.family);
    if family == Family::GitLab {
        return gitlab_thinking(id);
    }
    if family == Family::Chat
        && let Some(support) = Dialect::thinking_support(&provider.id, id)
    {
        return support;
    }
    default_thinking(family)
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

fn entry_tool_support(provider: &ProviderEntry, id: &str) -> ToolSupport {
    let family = wire::wire_for(provider, id).map_or(provider.family, |bound| bound.family);
    entry_tool_support_for_family(family)
}

fn entry_tool_support_for_family(family: Family) -> ToolSupport {
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
        | Family::GoogleCloudCodeAssist
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
