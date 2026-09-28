//! Provider model lists, reference resolution, and the checked-in price snapshot.

use std::{
    collections::HashSet,
    future::Future,
    path::{Path, PathBuf},
    time::Duration,
};

use dal_core::{Family, ModelPrice, ModelRoute, ThinkingLevel};
use serde::{Deserialize, Serialize};

use crate::{
    auth::credential::Credential,
    error::{ProviderError, ResolveError},
    http::{self, Exchange, NON_STREAM_TOTAL_TIMEOUT},
    provider::{AuthStyle, ProviderEntry},
    thinking::{Effort, ThinkingSupport},
};

/// One provider model and the capabilities known for it.
#[derive(Clone, Debug)]
pub struct CatalogEntry {
    /// The provider id used in model references.
    pub provider: Box<str>,
    /// The provider-native model id.
    pub id: Box<str>,
    /// The display name supplied by the provider or built-in table.
    pub display: Box<str>,
    /// Whether the provider's model list exposes this model.
    pub listing: Listing,
    /// Known input-context limit, in tokens.
    pub context_window: Option<u32>,
    /// Known output limit, in tokens.
    pub max_output: Option<u32>,
    /// Supported reasoning shape for the provider family.
    pub thinking: ThinkingSupport,
    /// Whether the model accepts image input.
    pub image_input: bool,
    /// Whether the model supports the family's remote-compaction endpoint.
    pub remote_compact: bool,
    /// Whether the Codex catalog reports reasoning-summary support.
    pub supports_reasoning_summaries: bool,
    /// Provider tool routes permitted by the model's catalog capabilities.
    pub tool_support: ToolSupport,
    /// Whether verified model metadata permits sending a temperature parameter.
    pub temperature_allowed: bool,
    /// Whether the provider model metadata confirms adaptive thinking display support.
    pub display_supported: bool,
}

/// Visibility in a provider model listing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Listing {
    /// The provider makes the model visible in its model list.
    Listed,
    /// The model is routable but hidden from ordinary listings.
    Hidden,
}

/// Model-level tool capability constrained by the provider API family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolSupport {
    /// The model does not accept tools on any supported route.
    None,
    /// The model accepts tools on any supported route.
    Any,
    /// The model accepts tools through Responses-style routes only.
    ResponsesOnly,
    /// Responses-style routes accept tools; Chat accepts them only with
    /// reasoning disabled.
    ChatWhenNoReasoning,
}

impl ToolSupport {
    /// Reports whether tools are supported for one resolved route and level.
    #[must_use]
    pub const fn allows(self, family: Family, thinking: ThinkingLevel) -> bool {
        match self {
            Self::None => false,
            Self::Any => true,
            Self::ResponsesOnly => matches!(family, Family::Responses | Family::Codex),
            Self::ChatWhenNoReasoning => match family {
                Family::Chat => matches!(thinking, ThinkingLevel::Off),
                Family::Responses | Family::Codex => true,
                Family::Anthropic => false,
            },
        }
    }
}

/// A reference resolved to its provider route and catalog metadata.
#[derive(Clone, Debug)]
pub struct ResolvedModel {
    /// The provider id used in the reference.
    pub provider: Box<str>,
    /// The API or built-in harness route.
    pub route: ModelRoute,
    /// The selected model row, or its typed-id fallback.
    pub entry: CatalogEntry,
}

/// The provider model catalog used by resolution.
#[derive(Clone, Debug)]
pub struct Catalog {
    providers: Vec<(ProviderEntry, CatalogSource)>,
    entries: Vec<CatalogEntry>,
}

impl Catalog {
    /// Builds a catalog with an explicit load source for every configured provider.
    ///
    /// `entries` contains fetched or cached rows; proven built-in rows are
    /// added unless a provider/id row is already present.
    #[must_use]
    pub fn with_sources(
        providers: Vec<(ProviderEntry, CatalogSource)>,
        mut entries: Vec<CatalogEntry>,
    ) -> Self {
        for builtin in built_in_entries() {
            if !entries
                .iter()
                .any(|entry| entry.provider == builtin.provider && entry.id == builtin.id)
            {
                entries.push(builtin);
            }
        }
        Self { providers, entries }
    }

    /// Returns the rows available for resolution, including built-ins.
    #[must_use]
    pub fn entries(&self) -> &[CatalogEntry] {
        &self.entries
    }

    fn source_for(&self, provider: &str) -> CatalogSource {
        self.providers
            .iter()
            .find(|(candidate, _)| candidate.id.as_ref() == provider)
            .map_or(CatalogSource::Typed, |(_, source)| *source)
    }
}

/// Identifies how a model list was obtained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogSource {
    /// A provider request succeeded; updating the cache was attempted.
    Live,
    /// The provider request failed and a valid on-disk cache was used.
    Cache,
    /// Neither a live list nor usable cached rows were available; resolution
    /// can still create typed ids for configured providers.
    Typed,
}

/// Rows and fallback evidence returned by [`load_models`].
#[derive(Debug)]
#[must_use]
pub struct CatalogFetch {
    /// Rows for the requested provider from the live list or its cache; empty
    /// when `source` is `Typed`.
    pub entries: Vec<CatalogEntry>,
    /// The source selected by the live/cache/typed fallback sequence.
    pub source: CatalogSource,
    /// The live request or decode error, when live loading failed.
    pub live_error: Option<ProviderError>,
    /// A cache write/read failure, when one occurred.
    pub cache_error: Option<Box<str>>,
    /// Provider whose live list was requested and whose cache fallback was read.
    pub provider: Box<str>,
}

/// Inputs for one provider model-list refresh.
#[derive(Debug)]
pub struct ModelFetch<'a> {
    /// The host's HTTP client.
    pub client: &'a reqwest::Client,
    /// The configured provider and its family/base URL.
    pub provider: &'a ProviderEntry,
    /// The credential to use for the model-list request.
    pub credential: &'a Credential,
    /// The dal cache directory; `models.json` is created inside it.
    pub cache_dir: &'a Path,
    /// The per-request user-agent value.
    pub user_agent: &'a str,
    /// The client version sent to the Codex catalog endpoint.
    pub version: &'a str,
}

/// Fetches provider rows, atomically updates `models.json`, and falls back to
/// cached rows after a live failure.
///
/// A typed-id fallback is represented by `CatalogSource::Typed` with no
/// fetched rows. Callers resolve a known provider/id through [`resolve`],
/// which creates the family-default entry without a fabricated context limit.
///
/// Live and cache failures are stored in [`CatalogFetch`] so the caller can
/// continue with typed ids after login.
pub async fn load_models<S, D>(fetch: &ModelFetch<'_>, sleep: S) -> CatalogFetch
where
    S: Fn(Duration) -> D,
    D: Future<Output = ()>,
{
    let cache_path = fetch.cache_dir.join("models.json");
    match fetch_live(fetch, &sleep).await {
        Ok(rows) => {
            let cache_error = write_cache_async(
                cache_path,
                fetch.provider.id.clone(),
                rows.clone(),
            )
            .await
            .err()
            .map(|error| error.to_string().into_boxed_str());
            CatalogFetch {
                entries: rows,
                source: CatalogSource::Live,
                live_error: None,
                cache_error,
                provider: fetch.provider.id.clone(),
            }
        }
        Err(live_error) => match read_cache_async(cache_path).await {
            Ok(entries) => {
                let entries = entries
                    .into_iter()
                    .filter(|entry| entry.provider == fetch.provider.id)
                    .collect::<Vec<_>>();
                let source = if entries.is_empty() {
                    CatalogSource::Typed
                } else {
                    CatalogSource::Cache
                };
                CatalogFetch {
                    entries,
                    source,
                    live_error: Some(live_error),
                    cache_error: None,
                    provider: fetch.provider.id.clone(),
                }
            }
            Err(CacheError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                CatalogFetch {
                    entries: Vec::new(),
                    source: CatalogSource::Typed,
                    live_error: Some(live_error),
                    cache_error: None,
                    provider: fetch.provider.id.clone(),
                }
            }
            Err(cache_error) => CatalogFetch {
                entries: Vec::new(),
                source: CatalogSource::Typed,
                live_error: Some(live_error),
                cache_error: Some(cache_error.to_string().into_boxed_str()),
                provider: fetch.provider.id.clone(),
            },
        },
    }
}

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
        return resolve_unknown_provider(catalog, reference);
    }
    resolve_bare(catalog, reference)
}

fn resolve_harness(reference: &str) -> Result<ResolvedModel, ResolveError> {
    let route = ModelRoute::harness(reference.to_owned())
        .map_err(|_| unknown_model(reference))?;
    let entry = CatalogEntry {
        provider: "dalgon".into(),
        id: reference.into(),
        display: reference.into(),
        listing: Listing::Listed,
        context_window: None,
        max_output: None,
        thinking: default_thinking(Family::Responses),
        image_input: false,
        remote_compact: false,
        supports_reasoning_summaries: false,
        temperature_allowed: false,
        display_supported: false,
        tool_support: ToolSupport::None,
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
            catalog.entries.iter().find(|entry| {
                entry.provider == provider.id && entry.id.as_ref() == candidate
            })
        });
    let mut entry = match listed {
        Some(entry) => entry.clone(),
        None => {
            if has_hidden_entry_elsewhere(catalog, provider, id)
                || catalog.source_for(&provider.id) != CatalogSource::Typed
            {
                return Err(unknown_model(reference));
            }
            typed_entry(catalog, provider, id)
        }
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

fn typed_entry(catalog: &Catalog, provider: &ProviderEntry, id: &str) -> CatalogEntry {
    if let Some(row) = capability_row(&catalog.entries, provider, id) {
        let mut entry = row.clone();
        entry.provider.clone_from(&provider.id);
        entry.id = id.into();
        entry.listing = Listing::Listed;
        return entry;
    }
    default_entry(provider, id)
}

fn capability_row<'a>(
    rows: &'a [CatalogEntry],
    provider: &ProviderEntry,
    id: &str,
) -> Option<&'a CatalogEntry> {
    for candidate in model_candidates(id).into_iter().flatten() {
        if let Some(row) = rows.iter().find(|entry| {
            entry.provider == provider.id
                && entry.id.as_ref() == candidate
        }) {
            return Some(row);
        }
        if is_openai_family(provider.family) {
            if let Some(row) = rows.iter().find(|entry| {
                entry.provider.as_ref() == "openai"
                    && entry.id.as_ref() == candidate
            }) {
                return Some(row);
            }
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
        (suffix.len() == 8 && suffix.bytes().all(|byte| byte.is_ascii_digit()))
            .then_some(base)
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

fn default_entry(provider: &ProviderEntry, id: &str) -> CatalogEntry {
    CatalogEntry {
        provider: provider.id.clone(),
        id: id.into(),
        display: id.into(),
        listing: Listing::Listed,
        context_window: None,
        max_output: None,
        thinking: default_thinking(provider.family),
        image_input: false,
        remote_compact: false,
        supports_reasoning_summaries: false,
        temperature_allowed: compiled_temperature(provider.id.as_ref(), id),
        display_supported: false,
        tool_support: ToolSupport::Any,
    }
}

fn bind_family_capabilities(entry: &mut CatalogEntry, family: Family) {
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

/// Returns built-in model rows whose capabilities have a source-backed note.
#[must_use]
pub fn built_in_entries() -> Vec<CatalogEntry> {
    use ThinkingLevel::{High, Low, Max, Medium, Xhigh};

    // `prices/models.dev.json` marks each built-in OpenAI row below as not
    // supporting temperature.
    let gpt_56_levels = vec![Low, Medium, High, Xhigh, Max];
    let gpt_6_levels = vec![Low, Medium, High, Xhigh, Max];
    // Verified 2026-09-28: https://developers.openai.com/api/docs/models/gpt-6-astra
    // states 922,000 maximum input and 128,000 maximum output tokens. The model
    // page lists Responses tools; provider-layer change 16 limits it to Responses.
    let astra = openai_entry(
        "gpt-6-astra",
        "GPT-6 Astra",
        922_000,
        128_000,
        gpt_6_levels.clone(),
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
        gpt_6_levels.clone(),
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
        gpt_6_levels,
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
        gpt_56_levels.clone(),
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
        gpt_56_levels.clone(),
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
        gpt_56_levels.clone(),
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
        gpt_56_levels,
        true,
        ToolSupport::Any,
    );
    // Provenance: `.references/codex/codex-rs/models-manager/models.json`, the
    // `gpt-6-luna` row (`context_window` 272,000, `tool_mode` code-only,
    // `supports_reasoning_summaries` true); D-51 assigns its limits to Reserve.
    // D-45's default 95% yields the 258,400 input window; no output cap is given.
    let reserve = CatalogEntry {
        provider: "openai-codex".into(),
        id: "gpt-reserve".into(),
        display: "Luna Reserve".into(),
        listing: Listing::Hidden,
        context_window: Some(258_400),
        max_output: None,
        thinking: ThinkingSupport::OpenAi {
            accepted: vec![
                ThinkingLevel::Low,
                ThinkingLevel::Medium,
                ThinkingLevel::High,
                ThinkingLevel::Xhigh,
                ThinkingLevel::Max,
            ],
            none_supported: false,
        },
        image_input: true,
        remote_compact: true,
        supports_reasoning_summaries: true,
        tool_support: ToolSupport::Any,
        temperature_allowed: false,
        display_supported: false,
    };
    vec![
        astra,
        sol,
        luna,
        gpt_56,
        gpt_56_sol,
        gpt_56_luna,
        gpt_56_terra,
        reserve,
    ]
}

/// OpenAI Responses rows use the remote compaction endpoint described by D-47.
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
        remote_compact: true,
        supports_reasoning_summaries: false,
        tool_support,
        temperature_allowed: false,
        display_supported: false,
    }
}

async fn fetch_live<S, D>(
    fetch: &ModelFetch<'_>,
    sleep: &S,
) -> Result<Vec<CatalogEntry>, ProviderError>
where
    S: Fn(Duration) -> D,
    D: Future<Output = ()>,
{
    match fetch.provider.family {
        Family::Chat | Family::Responses => {
            let url = http::endpoint(fetch.provider.family, &fetch.provider.base_url, "models")?;
            let headers = auth_headers(fetch.provider, fetch.credential)?;
            let body = get_body(fetch, url, headers, sleep).await?;
            decode_openai_models(fetch.provider, &body)
        }
        Family::Anthropic => fetch_anthropic(fetch, sleep).await,
        Family::Codex => {
            let mut url =
                http::endpoint(Family::Codex, &fetch.provider.base_url, "models")?;
            url.query_pairs_mut()
                .append_pair("client_version", fetch.version);
            let headers = codex_headers(fetch.provider, fetch.credential)?;
            let body = get_body(fetch, url, headers, sleep).await?;
            decode_codex_models(fetch.provider, &body)
        }
    }
}

async fn fetch_anthropic<S, D>(
    fetch: &ModelFetch<'_>,
    sleep: &S,
) -> Result<Vec<CatalogEntry>, ProviderError>
where
    S: Fn(Duration) -> D,
    D: Future<Output = ()>,
{
    let base_url = http::endpoint(Family::Anthropic, &fetch.provider.base_url, "v1/models")?;
    let headers = anthropic_headers(fetch.provider, fetch.credential)?;
    let mut seen_cursors = HashSet::new();
    let mut rows = Vec::new();
    let mut cursor: Option<String> = None;

    loop {
        let mut url = base_url.clone();
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("limit", "1000");
            if let Some(cursor) = &cursor {
                query.append_pair("after_id", cursor);
            }
        }
        let body = get_body(fetch, url, headers.clone(), sleep).await?;
        let page = decode_anthropic_page(fetch.provider, &body)?;
        rows.extend(page.rows);
        if !page.has_more {
            return Ok(rows);
        }
        let Some(next_cursor) = page.last_id else {
            return Err(protocol(
                Family::Anthropic,
                "model list has_more is true without last_id",
            ));
        };
        if !seen_cursors.insert(next_cursor.clone()) {
            return Err(protocol(
                Family::Anthropic,
                "model list repeated a last_id cursor",
            ));
        }
        cursor = Some(next_cursor);
    }
}

async fn get_body<S, D>(
    fetch: &ModelFetch<'_>,
    url: url::Url,
    headers: Vec<(String, String)>,
    sleep: &S,
) -> Result<Vec<u8>, ProviderError>
where
    S: Fn(Duration) -> D,
    D: Future<Output = ()>,
{
    let mut request = fetch.client.get(url);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let family = fetch.provider.family;
    let response = http::send(
        family,
        request,
        fetch.user_agent,
        Exchange::Json {
            total: NON_STREAM_TOTAL_TIMEOUT,
        },
        |duration| sleep(duration),
    )
    .await?;
    let status = response.status();
    if !status.is_success() {
        return Err(ProviderError::Status {
            family,
            status: status.as_u16(),
            message: String::new(),
        });
    }
    http::read_body(family, response).await
}

fn auth_headers(
    provider: &ProviderEntry,
    credential: &Credential,
) -> Result<Vec<(String, String)>, ProviderError> {
    match credential {
        Credential::ApiKey { key } => Ok(vec![(
            match provider.auth {
                AuthStyle::Bearer => String::from("authorization"),
                AuthStyle::XApiKey => String::from("x-api-key"),
            },
            match provider.auth {
                AuthStyle::Bearer => format!("Bearer {}", key.expose()),
                AuthStyle::XApiKey => key.expose().to_owned(),
            },
        )]),
        Credential::OAuth(oauth) => Ok(vec![(
            String::from("authorization"),
            format!("Bearer {}", oauth.access_token.expose()),
        )]),
        Credential::None => Err(no_credentials(provider)),
    }
}

fn anthropic_headers(
    provider: &ProviderEntry,
    credential: &Credential,
) -> Result<Vec<(String, String)>, ProviderError> {
    let mut headers = auth_headers(provider, credential)?;
    headers.push((
        String::from("anthropic-version"),
        String::from("2023-06-01"),
    ));
    if matches!(credential, Credential::OAuth(_)) {
        headers.push((
            String::from("anthropic-beta"),
            String::from("claude-code-20250219,oauth-2025-04-20"),
        ));
        headers.push((String::from("x-app"), String::from("cli")));
    }
    Ok(headers)
}

fn codex_headers(
    provider: &ProviderEntry,
    credential: &Credential,
) -> Result<Vec<(String, String)>, ProviderError> {
    let Credential::OAuth(oauth) = credential else {
        return Err(no_credentials(provider));
    };
    let Some(account_id) = oauth.account_id.as_ref() else {
        return Err(ProviderError::NoAccountId);
    };
    Ok(vec![
        (
            String::from("authorization"),
            format!("Bearer {}", oauth.access_token.expose()),
        ),
        (String::from("chatgpt-account-id"), account_id.clone()),
        (String::from("originator"), String::from("dalgon")),
    ])
}

fn no_credentials(provider: &ProviderEntry) -> ProviderError {
    ProviderError::NoCredentials {
        provider: provider.id.to_string(),
    }
}

fn decode_openai_models(
    provider: &ProviderEntry,
    bytes: &[u8],
) -> Result<Vec<CatalogEntry>, ProviderError> {
    let response: OpenAiModels = decode_json(provider.family, bytes, "OpenAI model list")?;
    let builtins = built_in_entries();
    Ok(response
        .data
        .into_iter()
        .map(|model| {
            let base = capability_row(&builtins, provider, &model.id)
                .cloned()
                .unwrap_or_else(|| default_entry(provider, &model.id));
            let mut entry = CatalogEntry {
                provider: provider.id.clone(),
                id: model.id.clone().into_boxed_str(),
                display: base.display,
                listing: Listing::Listed,
                context_window: base.context_window,
                max_output: base.max_output,
                thinking: base.thinking,
                image_input: base.image_input,
                remote_compact: base.remote_compact,
                supports_reasoning_summaries: base.supports_reasoning_summaries,
                tool_support: base.tool_support,
                temperature_allowed: base.temperature_allowed,
                display_supported: base.display_supported,
            };
            bind_family_capabilities(&mut entry, provider.family);
            entry
        })
        .collect())
}


fn decode_codex_models(
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
                id: model.slug.clone().into_boxed_str(),
                display: model
                    .display_name
                    .unwrap_or_else(|| model.slug.clone())
                    .into_boxed_str(),
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
                remote_compact: true,
                supports_reasoning_summaries: model.supports_reasoning_summaries,
                tool_support: ToolSupport::Any,
                temperature_allowed: compiled_temperature(provider.id.as_ref(), &model.slug),
                display_supported: false,
            })
        })
        .collect()
}

fn decode_anthropic_page(
    provider: &ProviderEntry,
    bytes: &[u8],
) -> Result<AnthropicPage, ProviderError> {
    let response: AnthropicModels =
        decode_json(Family::Anthropic, bytes, "Anthropic model list")?;
    let rows = response
        .data
        .into_iter()
        .map(|model| {
            let capabilities = model.capabilities.unwrap_or_default();
            let thinking = anthropic_thinking(&capabilities);
            CatalogEntry {
                provider: provider.id.clone(),
                id: model.id.clone().into_boxed_str(),
                display: model
                    .display_name
                    .unwrap_or_else(|| model.id.clone())
                    .into_boxed_str(),
                listing: Listing::Listed,
                context_window: model.max_input_tokens,
                max_output: model.max_tokens,
                thinking,
                image_input: capabilities
                    .image_input
                    .is_some_and(|support| support.supported),
                remote_compact: capabilities
                    .context_management
                    .and_then(|context| context.compact_20260112)
                    .is_some_and(|support| support.supported),
                supports_reasoning_summaries: false,
                tool_support: ToolSupport::Any,
                temperature_allowed: compiled_temperature(provider.id.as_ref(), &model.id),
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

fn parse_anthropic_effort(effort: &str) -> Option<Effort> {
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

fn protocol(family: Family, detail: impl Into<String>) -> ProviderError {
    ProviderError::Protocol {
        family,
        detail: detail.into(),
    }
}

#[derive(Debug, thiserror::Error)]
enum CacheError {
    #[error("cache I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("models cache JSON is invalid: {0}")]
    Json(#[from] sonic_rs::Error),
    #[error("models cache could not be serialized: {0}")]
    Serialize(sonic_rs::Error),
    #[error("models cache version {0} is not supported")]
    Version(u32),
    #[error("{0}")]
    Invalid(Box<str>),
    #[error(transparent)]
    Store(#[from] dal_store::StoreError),
    #[error("cache task failed: {0}")]
    Worker(#[from] tokio::task::JoinError),
}

fn read_cache(path: &Path) -> Result<Vec<CatalogEntry>, CacheError> {
    let bytes = std::fs::read(path)?;
    let cache: CachedCatalog = sonic_rs::from_slice(&bytes)?;
    if cache.version != CACHE_VERSION {
        return Err(CacheError::Version(cache.version));
    }
    cache
        .entries
        .into_iter()
        .map(CachedEntry::into_catalog_entry)
        .collect()
}

async fn read_cache_async(path: PathBuf) -> Result<Vec<CatalogEntry>, CacheError> {
    tokio::task::spawn_blocking(move || read_cache(&path)).await?
}

fn encode_cache(entries: &[CatalogEntry]) -> Result<Vec<u8>, CacheError> {
    sonic_rs::to_vec(&CachedCatalog {
        version: CACHE_VERSION,
        entries: entries.iter().map(CachedEntry::from).collect(),
    })
    .map_err(CacheError::Serialize)
}

async fn write_cache_async(
    path: PathBuf,
    provider: Box<str>,
    entries: Vec<CatalogEntry>,
) -> Result<(), CacheError> {
    tokio::task::spawn_blocking(move || write_cache(&path, &provider, entries)).await?
}

fn write_cache(path: &Path, provider: &str, fresh_rows: Vec<CatalogEntry>) -> Result<(), CacheError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let lock_path = path.with_file_name("models.json.lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    lock_file.lock()?;

    let (mut entries, read_error) = match read_cache(path) {
        Ok(entries) => (entries, None),
        Err(CacheError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            (Vec::new(), None)
        }
        Err(error) => (Vec::new(), Some(error)),
    };
    entries.retain(|entry| entry.provider.as_ref() != provider);
    entries.extend(fresh_rows);
    let bytes = encode_cache(&entries)?;
    dal_store::write_atomic(path, &bytes, dal_store::FileMode::Mode0600)?;
    if let Some(error) = read_error {
        return Err(error);
    }
    Ok(())
}


const CACHE_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct CachedCatalog {
    version: u32,
    entries: Vec<CachedEntry>,
}

#[derive(Serialize, Deserialize)]
struct CachedEntry {
    provider: String,
    id: String,
    display: String,
    hidden: bool,
    context_window: Option<u32>,
    max_output: Option<u32>,
    thinking: CachedThinking,
    image_input: bool,
    remote_compact: bool,
    #[serde(default)]
    supports_reasoning_summaries: bool,
    #[serde(default)]
    tool_support: CachedToolSupport,
    #[serde(default)]
    temperature_allowed: Option<bool>,
    #[serde(default)]
    display_supported: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CachedThinking {
    OpenAi {
        accepted: Vec<ThinkingLevel>,
        none_supported: bool,
    },
    Adaptive {
        can_disable: bool,
        #[serde(default)]
        accepted: Vec<String>,
    },
    Budget {
        can_disable: bool,
    },
    UnknownAdaptive,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CachedToolSupport {
    None,
    Any,
    ResponsesOnly,
    ChatWhenNoReasoning,
}

impl Default for CachedToolSupport {
    fn default() -> Self {
        Self::Any
    }
}

impl From<ToolSupport> for CachedToolSupport {
    fn from(support: ToolSupport) -> Self {
        match support {
            ToolSupport::None => Self::None,
            ToolSupport::Any => Self::Any,
            ToolSupport::ResponsesOnly => Self::ResponsesOnly,
            ToolSupport::ChatWhenNoReasoning => Self::ChatWhenNoReasoning,
        }
    }
}

impl From<CachedToolSupport> for ToolSupport {
    fn from(support: CachedToolSupport) -> Self {
        match support {
            CachedToolSupport::None => Self::None,
            CachedToolSupport::Any => Self::Any,
            CachedToolSupport::ResponsesOnly => Self::ResponsesOnly,
            CachedToolSupport::ChatWhenNoReasoning => Self::ChatWhenNoReasoning,
        }
    }
}

impl From<&CatalogEntry> for CachedEntry {
    fn from(entry: &CatalogEntry) -> Self {
        let thinking = match &entry.thinking {
            ThinkingSupport::OpenAi {
                accepted,
                none_supported,
            } => CachedThinking::OpenAi {
                accepted: accepted.clone(),
                none_supported: *none_supported,
            },
            ThinkingSupport::Adaptive {
                can_disable,
                accepted,
            } => CachedThinking::Adaptive {
                can_disable: *can_disable,
                accepted: accepted
                    .iter()
                    .map(|effort| effort.as_str().to_owned())
                    .collect(),
            },
            ThinkingSupport::Budget { can_disable } => CachedThinking::Budget {
                can_disable: *can_disable,
            },
            ThinkingSupport::UnknownAdaptive => CachedThinking::UnknownAdaptive,
        };
        Self {
            provider: entry.provider.to_string(),
            id: entry.id.to_string(),
            display: entry.display.to_string(),
            hidden: entry.listing == Listing::Hidden,
            context_window: entry.context_window,
            max_output: entry.max_output,
            thinking,
            image_input: entry.image_input,
            remote_compact: entry.remote_compact,
            supports_reasoning_summaries: entry.supports_reasoning_summaries,
            tool_support: CachedToolSupport::from(entry.tool_support),
            temperature_allowed: Some(entry.temperature_allowed),
            display_supported: entry.display_supported,
        }
    }
}

impl CachedEntry {
    fn into_catalog_entry(self) -> Result<CatalogEntry, CacheError> {
        if self.provider.is_empty() || self.id.is_empty() || self.display.is_empty() {
            return Err(CacheError::Invalid(
                "models cache contains an empty provider, id, or display name".into(),
            ));
        }
        let temperature_allowed = self
            .temperature_allowed
            .unwrap_or_else(|| compiled_temperature(&self.provider, &self.id));
        let thinking = match self.thinking {
            CachedThinking::OpenAi {
                accepted,
                none_supported,
            } => ThinkingSupport::OpenAi {
                accepted,
                none_supported,
            },
            CachedThinking::Adaptive {
                can_disable,
                accepted,
            } => ThinkingSupport::Adaptive {
                can_disable,
                accepted: accepted
                    .iter()
                    .filter_map(|effort| parse_anthropic_effort(effort))
                    .collect(),
            },
            CachedThinking::Budget { can_disable } => ThinkingSupport::Budget { can_disable },
            CachedThinking::UnknownAdaptive => ThinkingSupport::UnknownAdaptive,
        };
        Ok(CatalogEntry {
            provider: self.provider.into_boxed_str(),
            id: self.id.into_boxed_str(),
            display: self.display.into_boxed_str(),
            listing: if self.hidden {
                Listing::Hidden
            } else {
                Listing::Listed
            },
            context_window: self.context_window,
            max_output: self.max_output,
            thinking,
            image_input: self.image_input,
            remote_compact: self.remote_compact,
            supports_reasoning_summaries: self.supports_reasoning_summaries,
            tool_support: self.tool_support.into(),
            temperature_allowed,
            display_supported: self.display_supported,
        })
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

struct AnthropicPage {
    rows: Vec<CatalogEntry>,
    has_more: bool,
    last_id: Option<String>,
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

/// Compiled models.dev pricing for one exact provider/model id.
pub(crate) struct PriceRow {
    pub(crate) model: &'static str,
    pub(crate) input: Option<f64>,
    pub(crate) cached_input: Option<f64>,
    pub(crate) output: Option<f64>,
    pub(crate) reasoning: Option<f64>,
}

include!("prices_generated.rs");

/// Returns whether the exact provider/model pair is marked for temperature in
/// the compiled models.dev snapshot.
#[must_use]
pub fn compiled_temperature(provider: &str, id: &str) -> bool {
    TEMPERATURE_ROWS.binary_search(&(provider, id)).is_ok()
}

/// Returns the compiled USD-per-million-token rates for an exact provider/model id.
///
/// Missing models have no compiled price. A missing rate in an otherwise
/// priced row contributes zero rather than inventing a different rate.
#[must_use]
pub fn compiled_price(model: &str) -> Option<ModelPrice> {
    let index = PRICE_ROWS.binary_search_by_key(&model, |row| row.model).ok()?;
    let row = &PRICE_ROWS[index];
    Some(ModelPrice {
        input: row.input.unwrap_or(0.0),
        cached_input: row.cached_input.unwrap_or(0.0),
        output: row.output.unwrap_or(0.0),
        reasoning: row.reasoning.unwrap_or(0.0),
    })
}

/// Identifies the source and date of the checked-in price snapshot.
#[must_use]
pub const fn price_source() -> (&'static str, &'static str, &'static str) {
    (PRICE_SOURCE, PRICE_SOURCE_URL, PRICE_FETCHED_AT)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::{BufRead, BufReader, Write},
        net::{TcpListener, TcpStream},
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, Instant},
    };

    use super::*;
    use crate::{
        auth::credential::{OAuthCredential, SecretString},
        provider::Transport,
    };

    fn provider(id: &str, family: Family, base_url: &str) -> ProviderEntry {
        ProviderEntry {
            id: id.into(),
            family,
            base_url: base_url.into(),
            transport: Transport::Https,
            key_env: None,
            auth: AuthStyle::Bearer,
            max_concurrent_requests: 1,
        }
    }

    fn row(provider: &str, id: &str, context_window: Option<u32>) -> CatalogEntry {
        CatalogEntry {
            provider: provider.into(),
            id: id.into(),
            display: id.into(),
            listing: Listing::Listed,
            context_window,
            max_output: None,
            thinking: ThinkingSupport::UnknownAdaptive,
            image_input: false,
            remote_compact: false,
            supports_reasoning_summaries: false,
            tool_support: ToolSupport::Any,
            temperature_allowed: false,
            display_supported: false,
        }
    }

    #[test]
    fn aliases_expand_once_without_alias_chains() {
        let catalog = Catalog::with_sources(
            vec![(
                provider("openai", Family::Responses, "https://example.test/v1"),
                CatalogSource::Cache,
            )],
            vec![row("openai", "model", Some(42))],
        );
        let aliases = vec![
            (Box::<str>::from("first"), Box::<str>::from("second")),
            (
                Box::<str>::from("second"),
                Box::<str>::from("openai/model"),
            ),
        ];
        assert_eq!(
            resolve(&catalog, &aliases, "first")
                .unwrap_err()
                .to_string(),
            "unknown model second"
        );
    }

    #[test]
    fn qualified_cache_miss_is_unknown_but_typed_source_accepts_new_ids() {
        let cached = Catalog::with_sources(
            vec![(
                provider("openai", Family::Responses, "https://api.openai.com/v1"),
                CatalogSource::Cache,
            )],
            vec![row("openai", "known-model", Some(10_000))],
        );
        assert_eq!(
            resolve(&cached, &[], "openai/some-new-id")
                .unwrap_err()
                .to_string(),
            "unknown model openai/some-new-id"
        );
        assert_eq!(
            resolve(&cached, &[], "opneai/gpt-6-luna")
                .unwrap_err()
                .to_string(),
            "unknown model opneai/gpt-6-luna"
        );

        let typed = Catalog::with_sources(
            vec![(
                provider("openai", Family::Responses, "https://api.openai.com/v1"),
                CatalogSource::Typed,
            )],
            Vec::new(),
        );
        let resolved = resolve(&typed, &[], "openai/some-new-id")
            .expect("provider without a list accepts typed ids");
        assert_eq!(resolved.entry.context_window, None);
    }

    #[test]
    fn bare_id_collision_names_candidates_in_catalog_order() {
        let catalog = Catalog::with_sources(
            vec![
                (
                    provider("one", Family::Responses, "https://one.test/v1"),
                    CatalogSource::Cache,
                ),
                (
                    provider("two", Family::Anthropic, "https://two.test"),
                    CatalogSource::Cache,
                ),
            ],
            vec![row("one", "shared", None), row("two", "shared", None)],
        );
        assert_eq!(
            resolve(&catalog, &[], "shared").unwrap_err().to_string(),
            "model id shared matches several providers: one/shared, two/shared"
        );
    }

    #[test]
    fn first_slash_splits_provider_and_preserves_vendor_path() {
        let catalog = Catalog::with_sources(
            vec![(
                provider("zenmux", Family::Chat, "https://zenmux.test/v1"),
                CatalogSource::Cache,
            )],
            vec![row("zenmux", "openai/gpt-5.6-luna", None)],
        );
        let resolved = resolve(&catalog, &[], "zenmux/openai/gpt-5.6-luna")
            .expect("qualified z-mux model resolves");
        assert_eq!(resolved.route.id(), "openai/gpt-5.6-luna");
        assert_eq!(resolved.entry.context_window, None);
        let aliases = [(Box::<str>::from("fast"), Box::<str>::from("zenmux/openai/gpt-5.6-luna"))];
        assert_eq!(
            resolve(&catalog, &aliases, "fast")
                .expect("configured alias resolves once")
                .route
                .id(),
            "openai/gpt-5.6-luna"
        );
    }

    #[test]
    fn typed_vendor_segment_uses_last_capability_candidate() {
        let catalog = Catalog::with_sources(
            vec![(
                provider("zenmux", Family::Chat, "https://zenmux.test/v1"),
                CatalogSource::Typed,
            )],
            Vec::new(),
        );
        let resolved = resolve(&catalog, &[], "zenmux/openai/gpt-5.6-luna")
            .expect("typed vendor-prefixed id uses its built-in capability row");
        assert_eq!(resolved.entry.context_window, Some(922_000));
    }

    #[test]
    fn date_suffix_uses_the_base_capability_row() {
        let catalog = Catalog::with_sources(
            vec![(
                provider("anthropic", Family::Anthropic, "https://api.anthropic.com"),
                CatalogSource::Cache,
            )],
            vec![row("anthropic", "claude-sonnet-5", Some(200_000))],
        );
        let resolved = resolve(&catalog, &[], "claude-sonnet-5-20260101")
            .expect("dated bare id matches its base row");
        assert_eq!(resolved.route.id(), "claude-sonnet-5-20260101");
        assert_eq!(resolved.entry.id.as_ref(), "claude-sonnet-5");
        assert_eq!(resolved.entry.context_window, Some(200_000));
    }

    #[test]
    fn capability_lookup_prefers_exact_row_without_context_window() {
        let provider = provider("anthropic", Family::Anthropic, "https://api.anthropic.com");
        let rows = [
            row("anthropic", "claude-sonnet-5", Some(200_000)),
            row("anthropic", "claude-sonnet-5-20260101", None),
        ];
        let matched = capability_row(&rows, &provider, "claude-sonnet-5-20260101")
            .expect("exact row with unknown context wins over the base row");
        assert_eq!(matched.id.as_ref(), "claude-sonnet-5-20260101");
        assert_eq!(matched.context_window, None);
    }

    #[test]
    fn typed_resolution_uses_exact_compiled_temperature_pair() {
        let catalog = Catalog::with_sources(
            vec![
                (
                    provider("anthropic", Family::Anthropic, "https://api.anthropic.com"),
                    CatalogSource::Typed,
                ),
                (
                    provider("zenmux", Family::Anthropic, "https://zenmux.test"),
                    CatalogSource::Typed,
                ),
            ],
            Vec::new(),
        );
        let supported = resolve(&catalog, &[], "anthropic/claude-haiku-4-5")
            .expect("known compiled model resolves as typed");
        let wrong_provider = resolve(&catalog, &[], "zenmux/claude-haiku-4-5")
            .expect("known provider accepts typed ids");
        let unknown = resolve(&catalog, &[], "anthropic/claude-haiku-5")
            .expect("unknown model ids remain routable");
        assert!(supported.entry.temperature_allowed);
        assert!(!wrong_provider.entry.temperature_allowed);
        assert!(!unknown.entry.temperature_allowed);
    }

    #[test]
    fn reserve_is_a_hidden_codex_row_with_luna_limits() {
        let catalog = Catalog::with_sources(
            vec![
                (
                    provider(
                        "openai-codex",
                        Family::Codex,
                        "https://chatgpt.com/backend-api/codex",
                    ),
                    CatalogSource::Typed,
                ),
                (
                    provider("openai", Family::Responses, "https://api.openai.com/v1"),
                    CatalogSource::Typed,
                ),
                (
                    provider("anthropic", Family::Anthropic, "https://api.anthropic.com"),
                    CatalogSource::Typed,
                ),
            ],
            Vec::new(),
        );
        let reserve = resolve(&catalog, &[], "gpt-reserve")
            .expect("hidden reserve row resolves");
        assert_eq!(reserve.provider.as_ref(), "openai-codex");
        assert_eq!(reserve.entry.listing, Listing::Hidden);
        assert_eq!(reserve.entry.display.as_ref(), "Luna Reserve");
        assert_eq!(reserve.entry.context_window, Some(258_400));
        assert!(reserve.entry.supports_reasoning_summaries);
        assert_eq!(
            resolve(&catalog, &[], "openai/gpt-reserve")
                .unwrap_err()
                .to_string(),
            "unknown model openai/gpt-reserve"
        );
        assert_eq!(
            resolve(&catalog, &[], "anthropic/gpt-reserve")
                .unwrap_err()
                .to_string(),
            "unknown model anthropic/gpt-reserve"
        );
    }

    #[test]
    fn built_in_tool_support_is_api_and_effort_specific() {
        let entries = built_in_entries();
        let find = |id: &str| {
            entries
                .iter()
                .find(|entry| entry.id.as_ref() == id)
                .expect("built-in row exists")
        };
        let astra = find("gpt-6-astra").tool_support;
        assert_eq!(astra, ToolSupport::ResponsesOnly);
        assert!(!astra.allows(Family::Chat, ThinkingLevel::Off));
        assert!(astra.allows(Family::Responses, ThinkingLevel::High));

        let sol = find("gpt-6-sol").tool_support;
        assert_eq!(sol, ToolSupport::ChatWhenNoReasoning);
        assert!(sol.allows(Family::Chat, ThinkingLevel::Off));
        assert!(!sol.allows(Family::Chat, ThinkingLevel::High));
        assert!(sol.allows(Family::Responses, ThinkingLevel::High));

        let gpt_56 = find("gpt-5.6-luna").tool_support;
        assert_eq!(gpt_56, ToolSupport::Any);
        assert!(gpt_56.allows(Family::Chat, ThinkingLevel::High));
    }

    #[test]
    fn openai_decoder_uses_exact_compiled_temperature_capability() {
        let provider = provider("openai", Family::Chat, "https://api.openai.com/v1");
        let rows = decode_openai_models(
            &provider,
            br#"{"data":[{"id":"gpt-4o"},{"id":"gpt-6-sol"}]}"#,
        )
        .expect("OpenAI model rows decode");
        assert!(rows[0].temperature_allowed);
        assert!(!rows[1].temperature_allowed);
    }

    #[test]
    fn codex_decoder_hides_non_listed_rows_and_applies_window_percent() {
        let provider = provider(
            "openai-codex",
            Family::Codex,
            "https://chatgpt.com/backend-api/codex",
        );
        let bytes = br#"{"models":[{"slug":"gpt-6-luna","display_name":"GPT-6 Luna","visibility":"list","context_window":1000,"effective_context_window_percent":95,"supports_reasoning_summaries":true,"supported_reasoning_levels":["low",{"effort":"high"}],"input_modalities":["text","image"]},{"slug":"gpt-reserve","display_name":"Luna Reserve","visibility":"hide","context_window":1000}]}"#;
        let rows = decode_codex_models(&provider, bytes).expect("Codex rows decode");
        assert_eq!(rows[0].context_window, Some(950));
        assert_eq!(rows[0].listing, Listing::Listed);
        assert!(rows[0].image_input);
        assert!(rows[0].supports_reasoning_summaries);
        assert_eq!(rows[1].listing, Listing::Hidden);
        assert_eq!(rows[1].context_window, Some(950));
        assert!(!rows[1].supports_reasoning_summaries);
    }

    #[test]
    fn anthropic_decoder_reads_limits_and_capabilities() {
        let provider = provider(
            "anthropic",
            Family::Anthropic,
            "https://api.anthropic.com",
        );
        let bytes = br#"{"data":[{"id":"claude-sonnet-5","display_name":"Claude Sonnet 5","max_input_tokens":200000,"max_tokens":8192,"capabilities":{"image_input":{"supported":true},"thinking":{"types":{"adaptive":{"supported":true},"disabled":{"supported":true}}},"effort":{"supported":true,"high":{"supported":true},"low":{"supported":true},"max":{"supported":true},"medium":{"supported":true},"xhigh":{"supported":true}},"context_management":{"compact_20260112":{"supported":true}}}}],"has_more":true,"last_id":"claude-sonnet-5"}"#;
        let page = decode_anthropic_page(&provider, bytes).expect("Anthropic page decodes");
        assert!(page.has_more);
        assert_eq!(page.last_id.as_deref(), Some("claude-sonnet-5"));
        assert_eq!(page.rows[0].context_window, Some(200_000));
        assert_eq!(page.rows[0].max_output, Some(8192));
        assert!(page.rows[0].image_input);
        assert!(page.rows[0].remote_compact);
        if let ThinkingSupport::Adaptive {
            can_disable,
            accepted,
        } = &page.rows[0].thinking
        {
            assert!(*can_disable);
            assert_eq!(
                accepted,
                &[
                    Effort::Low,
                    Effort::Medium,
                    Effort::High,
                    Effort::Xhigh,
                    Effort::Max,
                ]
            );
        } else {
            panic!("page did not report adaptive thinking");
        }
    }

    #[test]
    fn anthropic_decoder_uses_exact_compiled_temperature_capability() {
        let provider = provider("anthropic", Family::Anthropic, "https://api.anthropic.com");
        let bytes = br#"{"data":[{"id":"claude-haiku-4-5"},{"id":"claude-sonnet-5"}],"has_more":false,"last_id":"claude-sonnet-5"}"#;
        let page = decode_anthropic_page(&provider, bytes)
            .expect("Anthropic model rows decode without extra capability fields");
        assert!(page.rows[0].temperature_allowed);
        assert!(!page.rows[1].temperature_allowed);
    }

    #[test]
    fn anthropic_effort_rows_keep_only_reported_supported_levels() {
        let provider = provider(
            "anthropic",
            Family::Anthropic,
            "https://api.anthropic.com",
        );
        let bytes = br#"{"data":[{"id":"partial","capabilities":{"thinking":{"types":{"adaptive":{"supported":true}}},"effort":{"supported":true,"low":{"supported":true},"medium":{"supported":false},"high":{"supported":true},"xhigh":{"supported":true},"max":{"supported":false}}}},{"id":"disabled","capabilities":{"thinking":{"types":{"adaptive":{"supported":true}}},"effort":{"supported":false,"low":{"supported":true},"medium":{"supported":true}}}}],"has_more":false,"last_id":"disabled"}"#;
        let page = decode_anthropic_page(&provider, bytes).expect("Anthropic effort rows decode");
        let ThinkingSupport::Adaptive { accepted, .. } = &page.rows[0].thinking else {
            panic!("first row should have known adaptive thinking");
        };
        assert_eq!(
            accepted.as_slice(),
            &[Effort::Low, Effort::High, Effort::Xhigh]
        );
        let ThinkingSupport::Adaptive { accepted, .. } = &page.rows[1].thinking else {
            panic!("second row should remain known adaptive");
        };
        assert!(accepted.is_empty());
    }


    #[tokio::test]
    async fn anthropic_fetch_paginates_and_atomically_caches_rows() {
        let directory = TestDir::new();
        let listener =
            TcpListener::bind(("127.0.0.1", 0)).expect("bind catalog replay listener");
        listener
            .set_nonblocking(true)
            .expect("make catalog listener nonblocking");
        let port = listener.local_addr().expect("read listener port").port();
        let server = serve_anthropic_pages(
            listener,
            [
                r#"{"data":[{"id":"claude-one","max_input_tokens":200000,"max_tokens":8192}],"has_more":true,"last_id":"claude-one"}"#,
                r#"{"data":[{"id":"claude-two","max_input_tokens":300000,"max_tokens":8192}],"has_more":false,"last_id":"claude-two"}"#,
            ],
        );
        let mut provider = provider(
            "anthropic",
            Family::Anthropic,
            &format!("http://127.0.0.1:{port}"),
        );
        provider.auth = AuthStyle::XApiKey;
        let credential = Credential::ApiKey {
            key: SecretString::from(String::from("test-key")),
        };
        let client = reqwest::Client::new();
        let fetch = ModelFetch {
            client: &client,
            provider: &provider,
            credential: &credential,
            cache_dir: directory.path(),
            user_agent: "dalgon/test",
            version: "test",
        };

        let fetched = load_models(&fetch, |_| std::future::pending::<()>()).await;
        let requests = server.join().expect("serve both model pages");
        assert_eq!(fetched.source, CatalogSource::Live);
        assert_eq!(fetched.entries.len(), 2);
        assert!(requests[0].contains("limit=1000"));
        assert!(requests[1].contains("after_id=claude-one"));
        let cached = read_cache_async(directory.path().join("models.json"))
            .await
            .expect("read atomically published cache");
        assert_eq!(
            cached
                .iter()
                .map(|entry| entry.id.as_ref())
                .collect::<Vec<_>>(),
            ["claude-one", "claude-two"]
        );
    }
    #[tokio::test]
    async fn failed_live_fetch_uses_atomic_cache_then_typed_default_has_unknown_window() {
        let directory = TestDir::new();
        let port = closed_loopback_port();
        let provider = provider(
            "openai-codex",
            Family::Codex,
            &format!("http://127.0.0.1:{port}/backend-api/codex"),
        );
        let mut cached = row("openai-codex", "gpt-6-luna", Some(902_500));
        cached.supports_reasoning_summaries = true;
        let cache_path = directory.path().join("models.json");
        let oauth = Credential::OAuth(OAuthCredential {
            access_token: SecretString::from(String::from("token")),
            refresh_token: SecretString::from(String::from("refresh")),
            expires_at: None,
            id_token: None,
            account_id: Some(String::from("account")),
        });
        let client = reqwest::Client::new();
        let fetch = ModelFetch {
            client: &client,
            provider: &provider,
            credential: &oauth,
            cache_dir: directory.path(),
            user_agent: "dalgon/test",
            version: "test",
        };
        let other_provider_row = row("anthropic", "claude-sonnet-5", Some(200_000));
        write_cache_async(
            cache_path.clone(),
            "anthropic".into(),
            vec![other_provider_row],
        )
        .await
        .expect("write another provider's cache row");
        let other_provider_cache = load_models(&fetch, |_| std::future::pending::<()>()).await;
        assert_eq!(other_provider_cache.source, CatalogSource::Typed);
        assert!(other_provider_cache.entries.is_empty());
        assert!(matches!(
            other_provider_cache.live_error.as_ref(),
            Some(ProviderError::Transport { .. })
        ));

        write_cache_async(cache_path.clone(), "openai-codex".into(), vec![cached])
            .await
            .expect("write Codex cache row");
        let all_cached = read_cache_async(cache_path.clone())
            .await
            .expect("read merged provider cache");
        let cached_providers: Vec<_> = all_cached
            .iter()
            .map(|entry| entry.provider.as_ref())
            .collect();
        assert_eq!(cached_providers.len(), 2);
        assert!(cached_providers.contains(&"anthropic"));
        assert!(cached_providers.contains(&"openai-codex"));
        let cached_result = load_models(&fetch, |_| std::future::pending::<()>()).await;
        assert_eq!(cached_result.source, CatalogSource::Cache);
        assert_eq!(cached_result.entries[0].provider.as_ref(), "openai-codex");
        assert_eq!(cached_result.entries[0].context_window, Some(902_500));
        assert!(cached_result.entries[0].supports_reasoning_summaries);
        assert!(!cached_result.entries[0].temperature_allowed);
        assert!(!cached_result.entries[0].display_supported);

        fs::remove_file(cache_path).expect("remove cache");
        let typed_result = load_models(&fetch, |_| std::future::pending::<()>()).await;
        assert_eq!(typed_result.source, CatalogSource::Typed);
        assert!(typed_result.entries.is_empty());
        let catalog = Catalog::with_sources(
            vec![(provider, typed_result.source)],
            typed_result.entries,
        );
        let typed = resolve(&catalog, &[], "openai-codex/some-new-id")
            .expect("known provider accepts a typed model id");
        assert_eq!(typed.route.id(), "some-new-id");
        assert_eq!(typed.entry.context_window, None);
        assert!(!typed.entry.temperature_allowed);
        assert!(!typed.entry.display_supported);
        assert!(matches!(
            typed.entry.thinking,
            ThinkingSupport::OpenAi {
                none_supported: true,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn version_one_cache_without_optional_capabilities_defaults_false() {
        let directory = TestDir::new();
        let cache_path = directory.path().join("models.json");
        fs::write(
            &cache_path,
            br#"{"version":1,"entries":[{"provider":"anthropic","id":"unknown","display":"unknown","hidden":false,"context_window":null,"max_output":null,"thinking":{"kind":"unknown_adaptive"},"image_input":false,"remote_compact":false},{"provider":"anthropic","id":"claude-haiku-4-5","display":"Claude Haiku 4.5","hidden":false,"context_window":null,"max_output":null,"thinking":{"kind":"unknown_adaptive"},"image_input":false,"remote_compact":false}]}"#,
        )
        .expect("write version-one cache");
        let entries = read_cache_async(cache_path)
            .await
            .expect("read version-one cache");
        assert_eq!(entries.len(), 2);
        assert!(!entries[0].temperature_allowed);
        assert!(entries[1].temperature_allowed);
        assert!(!entries[0].display_supported && !entries[1].display_supported);
    }

    #[tokio::test]
    async fn cached_capability_flags_survive_resolution() {
        let directory = TestDir::new();
        let cache_path = directory.path().join("models.json");
        let mut entry = row("anthropic", "synthetic-capability-model", Some(32_000));
        entry.temperature_allowed = true;
        entry.display_supported = true;
        write_cache_async(cache_path.clone(), "anthropic".into(), vec![entry])
            .await
            .expect("write capability cache");
        let entries = read_cache_async(cache_path)
            .await
            .expect("read capability cache");
        let catalog = Catalog::with_sources(
            vec![(
                provider("anthropic", Family::Anthropic, "https://api.anthropic.com"),
                CatalogSource::Cache,
            )],
            entries,
        );
        let resolved = resolve(&catalog, &[], "anthropic/synthetic-capability-model")
            .expect("cached model resolves");
        assert!(resolved.entry.temperature_allowed && resolved.entry.display_supported);
    }


    fn serve_anthropic_pages(
        listener: TcpListener,
        pages: [&'static str; 2],
    ) -> std::thread::JoinHandle<Vec<String>> {
        std::thread::spawn(move || {
            let mut request_lines = Vec::with_capacity(pages.len());
            for body in pages {
                let mut stream = accept_loopback(&listener);
                stream
                    .set_nonblocking(false)
                    .expect("make replay socket blocking");
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .expect("set replay socket read timeout");
                let mut reader =
                    BufReader::new(stream.try_clone().expect("clone replay socket"));
                let mut request_line = String::new();
                assert_ne!(
                    reader.read_line(&mut request_line).expect("read request line"),
                    0
                );
                let mut header_line = String::new();
                loop {
                    header_line.clear();
                    let bytes = reader
                        .read_line(&mut header_line)
                        .expect("read request header");
                    if bytes == 0 || header_line == "\r\n" {
                        break;
                    }
                }
                drop(reader);
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("write replay response");
                request_lines.push(request_line);
            }
            request_lines
        })
    }

    fn accept_loopback(listener: &TcpListener) -> TcpStream {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match listener.accept() {
                Ok((stream, _)) => return stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("catalog replay listener failed: {error}"),
            }
        }
    }
    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "dal-provider-catalog-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).expect("create catalog test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn closed_loopback_port() -> u16 {
        let listener =
            std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback port");
        listener.local_addr().expect("read loopback port").port()
    }

    #[test]
    fn exact_priced_model_and_missing_model_have_distinct_costs() {
        use dal_core::Usage;

        let row = PRICE_ROWS
            .iter()
            .find(|row| row.input.is_some() || row.output.is_some())
            .expect("snapshot has a priced row");
        let price = compiled_price(row.model).expect("snapshot model resolves");
        let usage = Usage {
            input_tokens: 1_000_000,
            cached_input_tokens: 0,
            output_tokens: 0,
            reasoning_tokens: None,
            cache_write_tokens: 0,
            cost_usd: None,
        };
        assert_eq!(usage.cost_usd(None, Some(&price)), Some(price.input));
        assert!(compiled_price("missing/model").is_none());
        assert!(compiled_price(&format!("{}-unlisted", row.model)).is_none());
    }

    #[test]
    fn generated_table_remains_sorted_for_binary_search() {
        assert!(PRICE_ROWS.windows(2).all(|pair| pair[0].model < pair[1].model));
        assert_eq!(price_source().0, "models.dev");
    }
}
