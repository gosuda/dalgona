//! Live model-list fetch with typed-id fallback.
//!
//! One `ModelFetch` carries the client, credential, and cache directory for a
//! single provider refresh; failures degrade to cache then typed ids.

use std::{
    collections::{BTreeMap, HashMap},
    future::Future,
    time::Duration,
};

use dal_core::Family;
use futures::{StreamExt, stream};

use super::{
    CatalogEntry, CatalogFetch, CatalogSource, ModelFetch,
    cache::{CacheError, read_cache_async, write_cache_async},
    decode::{
        apply_ollama_show, decode_anthropic_page, decode_codex_models, decode_copilot_models,
        decode_devin_models, decode_gemini_page, decode_mistral_models, decode_ollama_models,
        decode_openai_models, decode_radius_models,
    },
};
use crate::{
    auth::credential::Credential,
    error::ProviderError,
    family::{
        copilot::{
            CopilotKey, GetWire, decode_endpoint, endpoint_discovery_request, models_request,
            needs_endpoint_discovery,
        },
        devin::{
            PATH_DISCOVERY,
            request::{UNARY_HEADERS, discovery_body},
        },
        google::gemini::API_KEY_HEADER,
        ollama::{
            Deployment, OllamaShow, SHOW_PATH, TAGS_PATH, decode_show, show_body, trim_api_suffix,
        },
        pi_messages::CONFIG_PATH as PI_MESSAGES_CONFIG_PATH,
    },
    http::{self, Exchange, NON_STREAM_TOTAL_TIMEOUT},
    provider::ProviderEntry,
};

/// Fetches provider rows, atomically updates `models.json`, and falls back to
/// cached rows after a live failure.
///
/// A typed-id fallback is represented by `CatalogSource::Typed` with no
/// fetched rows. Callers resolve a known provider/id through [`super::resolve`],
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
            let cache_error =
                write_cache_async(cache_path, fetch.provider.id.clone(), rows.clone())
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

async fn fetch_live<S, D>(
    fetch: &ModelFetch<'_>,
    sleep: &S,
) -> Result<Vec<CatalogEntry>, ProviderError>
where
    S: Fn(Duration) -> D,
    D: Future<Output = ()>,
{
    if is_copilot_entry(fetch.provider) {
        return fetch_copilot_models(fetch, sleep).await;
    }
    match fetch.provider.family {
        Family::Chat | Family::Responses => {
            let url = http::endpoint(fetch.provider.family, &fetch.provider.base_url, "models")?;
            let headers = fetch
                .provider
                .shape()
                .headers(fetch.provider, fetch.credential)?;
            let body = get_body(fetch, url, headers, sleep).await?;
            decode_openai_models(fetch.provider, &body)
        }
        Family::Anthropic => fetch_anthropic(fetch, sleep).await,
        Family::Codex => {
            let mut url = http::endpoint(Family::Codex, &fetch.provider.base_url, "models")?;
            url.query_pairs_mut()
                .append_pair("client_version", fetch.version);
            let headers = fetch
                .provider
                .shape()
                .headers(fetch.provider, fetch.credential)?;
            let body = get_body(fetch, url, headers, sleep).await?;
            decode_codex_models(fetch.provider, &body)
        }
        Family::Bedrock => Err(ProviderError::InvalidRequest {
            message: String::from("the bedrock family has no catalog discovery yet"),
        }),
        Family::Cursor => Err(ProviderError::InvalidRequest {
            message: String::from("the cursor family has no catalog discovery yet"),
        }),
        Family::Devin => fetch_devin(fetch, sleep).await,
        Family::Droid => Err(ProviderError::InvalidRequest {
            message: String::from("the droid family has no catalog discovery yet"),
        }),
        Family::GitLab => Err(ProviderError::InvalidRequest {
            message: String::from("the gitlab family has no catalog discovery yet"),
        }),
        Family::GoogleGenerativeAi => fetch_gemini(fetch, sleep).await,
        Family::GoogleVertex => Err(ProviderError::InvalidRequest {
            message: String::from(
                "dal does not list google-vertex models: Vertex publisher models have no list \
                 dal reads, so model ids resolve as typed rows",
            ),
        }),
        Family::GoogleGeminiCli => Err(ProviderError::InvalidRequest {
            message: String::from(
                "dal does not list google-gemini-cli models: the gemini CLI owns its model \
                 list, so model ids resolve as typed rows",
            ),
        }),
        Family::GoogleAntigravity => Err(ProviderError::InvalidRequest {
            message: String::from(
                "dal does not list google-antigravity models: the Cloud Code Assist roster \
                 needs a Google OAuth sign-in dal does not provide, so model ids resolve as \
                 typed rows",
            ),
        }),
        Family::Mistral => fetch_mistral(fetch, sleep).await,
        Family::Ollama => fetch_ollama(fetch, sleep).await,
        Family::PiMessages => fetch_pi_messages(fetch, sleep).await,
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
    let headers = fetch
        .provider
        .shape()
        .headers(fetch.provider, fetch.credential)?;
    let mut seen_cursors = std::collections::HashSet::new();
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
            return Err(super::decode::protocol(
                Family::Anthropic,
                "model list has_more is true without last_id",
            ));
        };
        if !seen_cursors.insert(next_cursor.clone()) {
            return Err(super::decode::protocol(
                Family::Anthropic,
                "model list repeated a last_id cursor",
            ));
        }
        cursor = Some(next_cursor);
    }
}

async fn fetch_devin<S, D>(
    fetch: &ModelFetch<'_>,
    sleep: &S,
) -> Result<Vec<CatalogEntry>, ProviderError>
where
    S: Fn(Duration) -> D,
    D: Future<Output = ()>,
{
    let Credential::ApiKey { key } = fetch.credential else {
        return Err(ProviderError::NoCredentials {
            provider: fetch.provider.id.to_string(),
        });
    };
    let url = http::endpoint(Family::Devin, &fetch.provider.base_url, PATH_DISCOVERY)?;
    let body = post_body(
        fetch,
        url,
        unary_headers(),
        discovery_body(key.expose()),
        sleep,
    )
    .await?;
    decode_devin_models(fetch.provider, &body)
}

/// How many `api/show` probes one refresh keeps in flight.
const OLLAMA_SHOW_CONCURRENCY: usize = 6;

/// Lists the server's models from `api/tags`, then probes every one with
/// `api/show` for its capabilities and context window.
///
/// The tag list is the discovery: when it fails the whole fetch fails and the
/// caller falls back to the cache. A probe that fails does not: the row keeps
/// what the last good refresh knew about it (read from the cache), else what
/// `api/tags` alone says, so one flaky probe never blanks a model's vision or
/// reasoning support and a server without `api/show` still lists its models.
async fn fetch_ollama<S, D>(
    fetch: &ModelFetch<'_>,
    sleep: &S,
) -> Result<Vec<CatalogEntry>, ProviderError>
where
    S: Fn(Duration) -> D,
    D: Future<Output = ()>,
{
    let base = trim_api_suffix(&fetch.provider.base_url);
    let tags_url = http::endpoint(Family::Ollama, base, TAGS_PATH)?;
    let show_url = http::endpoint(Family::Ollama, base, SHOW_PATH)?;
    let headers = fetch
        .provider
        .shape()
        .headers(fetch.provider, fetch.credential)?;
    let body = get_body(fetch, tags_url, headers.clone(), sleep).await?;
    let mut rows = decode_ollama_models(fetch.provider, &body)?;
    // One future per model, built in a loop rather than a closure returning
    // an async block: callers that spawn the refresh need the whole future to
    // be `Send` for every lifetime, which a closure-built future defeats.
    let mut pending = Vec::with_capacity(rows.len());
    for row in &rows {
        pending.push(probe_ollama(
            fetch,
            show_url.clone(),
            headers.clone(),
            &row.id,
            sleep,
        ));
    }
    let probes: Vec<Result<OllamaShow, ProviderError>> = stream::iter(pending)
        .buffered(OLLAMA_SHOW_CONCURRENCY)
        .collect()
        .await;
    let previous = if probes.iter().any(Result::is_err) {
        cached_rows(fetch).await
    } else {
        HashMap::new()
    };
    let deployment = Deployment::of(&fetch.provider.id, &fetch.provider.base_url);
    for (row, probe) in rows.iter_mut().zip(probes) {
        match probe {
            Ok(probe) => apply_ollama_show(row, &probe, deployment),
            Err(_) => {
                if let Some(last) = previous.get(&row.id) {
                    row.clone_from(last);
                }
            }
        }
    }
    Ok(rows)
}

async fn probe_ollama<S, D>(
    fetch: &ModelFetch<'_>,
    url: url::Url,
    headers: Vec<(String, String)>,
    model: &str,
    sleep: &S,
) -> Result<OllamaShow, ProviderError>
where
    S: Fn(Duration) -> D,
    D: Future<Output = ()>,
{
    let body = post_json(fetch, url, headers, show_body(model)?, sleep).await?;
    decode_show(&body)
}

/// The provider's rows from the last good refresh, by id; empty when there is
/// no readable cache.
async fn cached_rows(fetch: &ModelFetch<'_>) -> HashMap<Box<str>, CatalogEntry> {
    read_cache_async(fetch.cache_dir.join("models.json"))
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| entry.provider == fetch.provider.id)
        .map(|entry| (entry.id.clone(), entry))
        .collect()
}

/// Fetches the Radius gateway config: the reference resolves `/v1/config`
/// against the gateway URL, so the request goes to the origin of the
/// configured base and a path on that base never prefixes it.
async fn fetch_pi_messages<S, D>(
    fetch: &ModelFetch<'_>,
    sleep: &S,
) -> Result<Vec<CatalogEntry>, ProviderError>
where
    S: Fn(Duration) -> D,
    D: Future<Output = ()>,
{
    let base = fetch.provider.base_url.as_ref();
    // An unparsable base is reported by `endpoint` below.
    let origin = url::Url::parse(base).map_or_else(
        |_| base.to_owned(),
        |url| url.origin().ascii_serialization(),
    );
    let url = http::endpoint(Family::PiMessages, &origin, PI_MESSAGES_CONFIG_PATH)?;
    let headers = fetch
        .provider
        .shape()
        .headers(fetch.provider, fetch.credential)?;
    let body = get_body(fetch, url, headers, sleep).await?;
    decode_radius_models(fetch.provider, &body)
}

const MISTRAL_MODELS_PATH: &str = "models";

async fn fetch_mistral<S, D>(
    fetch: &ModelFetch<'_>,
    sleep: &S,
) -> Result<Vec<CatalogEntry>, ProviderError>
where
    S: Fn(Duration) -> D,
    D: Future<Output = ()>,
{
    let url = http::endpoint(
        Family::Mistral,
        &fetch.provider.base_url,
        MISTRAL_MODELS_PATH,
    )?;
    let headers = fetch
        .provider
        .shape()
        .headers(fetch.provider, fetch.credential)?;
    let body = get_body(fetch, url, headers, sleep).await?;
    decode_mistral_models(fetch.provider, &body)
}

const GEMINI_MODELS_PATH: &str = "models";

/// The page size one Gemini `models.list` request asks for.
const GEMINI_PAGE_SIZE: &str = "100";

/// The most pages one refresh reads. A list longer than this is cut here, not
/// failed: the rows read so far are the catalog.
const GEMINI_MAX_PAGES: usize = 25;

/// Lists the direct Gemini API's text-generation models, following
/// `nextPageToken` until a page names none, repeats one, or the page bound is
/// reached.
///
/// Rows merge by id across pages (a later page wins) and come back sorted by
/// id. A failed page fails the whole refresh, so the caller falls back to the
/// cache and never caches a partial list.
async fn fetch_gemini<S, D>(
    fetch: &ModelFetch<'_>,
    sleep: &S,
) -> Result<Vec<CatalogEntry>, ProviderError>
where
    S: Fn(Duration) -> D,
    D: Future<Output = ()>,
{
    let headers = gemini_headers(fetch)?;
    let list_url = http::endpoint(
        Family::GoogleGenerativeAi,
        &fetch.provider.base_url,
        GEMINI_MODELS_PATH,
    )?;
    let mut rows = BTreeMap::new();
    // Every token requested so far; the last one names the page to read next.
    let mut tokens: Vec<String> = Vec::new();
    for _ in 0..GEMINI_MAX_PAGES {
        let mut url = list_url.clone();
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("pageSize", GEMINI_PAGE_SIZE);
            if let Some(token) = tokens.last() {
                query.append_pair("pageToken", token);
            }
        }
        let body = get_body(fetch, url, headers.clone(), sleep).await?;
        let page = decode_gemini_page(fetch.provider, &body)?;
        for row in page.rows {
            rows.insert(row.id.clone(), row);
        }
        match page.next_page_token {
            Some(next) if !tokens.contains(&next) => tokens.push(next),
            _ => break,
        }
    }
    Ok(rows.into_values().collect())
}

/// The credential header of a direct Gemini model-list request.
///
/// It is the `x-goog-api-key` header the request adapter sends, never a `?key=`
/// query that would put the secret in a URL. A missing or blank key sends no
/// request, so the caller falls back to typed ids; an OAuth login belongs to
/// the other Google surfaces and is refused as the adapter refuses it.
fn gemini_headers(fetch: &ModelFetch<'_>) -> Result<Vec<(String, String)>, ProviderError> {
    match fetch.credential {
        Credential::ApiKey { key } if !key.expose().trim().is_empty() => Ok(vec![(
            String::from(API_KEY_HEADER),
            key.expose().to_owned(),
        )]),
        Credential::ApiKey { .. } | Credential::None => Err(ProviderError::NoCredentials {
            provider: fetch.provider.id.to_string(),
        }),
        Credential::OAuth(_) => Err(ProviderError::InvalidRequest {
            message: format!("{} does not accept an OAuth credential", fetch.provider.id),
        }),
    }
}

fn unary_headers() -> Vec<(String, String)> {
    UNARY_HEADERS
        .iter()
        .map(|(name, value)| (String::from(*name), String::from(*value)))
        .collect()
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
        sleep,
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

async fn post_body<S, D>(
    fetch: &ModelFetch<'_>,
    url: url::Url,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    sleep: &S,
) -> Result<Vec<u8>, ProviderError>
where
    S: Fn(Duration) -> D,
    D: Future<Output = ()>,
{
    let exchange = Exchange::Bytes {
        accept: "*/*",
        total: Some(NON_STREAM_TOTAL_TIMEOUT),
    };
    post_exchange(fetch, url, headers, body, exchange, sleep).await
}

/// Posts a JSON body and reads the JSON reply under the non-stream deadline.
async fn post_json<S, D>(
    fetch: &ModelFetch<'_>,
    url: url::Url,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    sleep: &S,
) -> Result<Vec<u8>, ProviderError>
where
    S: Fn(Duration) -> D,
    D: Future<Output = ()>,
{
    let exchange = Exchange::Json {
        total: NON_STREAM_TOTAL_TIMEOUT,
    };
    post_exchange(fetch, url, headers, body, exchange, sleep).await
}

async fn post_exchange<S, D>(
    fetch: &ModelFetch<'_>,
    url: url::Url,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    exchange: Exchange,
    sleep: &S,
) -> Result<Vec<u8>, ProviderError>
where
    S: Fn(Duration) -> D,
    D: Future<Output = ()>,
{
    let mut request = fetch.client.post(url);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let family = fetch.provider.family;
    let response = http::send(
        family,
        request.body(body),
        fetch.user_agent,
        exchange,
        sleep,
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

fn is_copilot_entry(provider: &ProviderEntry) -> bool {
    matches!(
        provider.id.as_ref(),
        "github-copilot" | "github-copilot-responses" | "github-copilot-anthropic"
    ) || provider.settings.integration.is_some()
        || is_copilot_domain(provider.base_url.as_ref())
}

fn is_copilot_domain(base: &str) -> bool {
    let host = url::Url::parse(base)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase));
    host.is_some_and(|host| host == "githubcopilot.com" || host.ends_with(".githubcopilot.com"))
}

pub(crate) async fn fetch_copilot_models<S, D>(
    fetch: &ModelFetch<'_>,
    sleep: &S,
) -> Result<Vec<CatalogEntry>, ProviderError>
where
    S: Fn(Duration) -> D,
    D: Future<Output = ()>,
{
    let Credential::ApiKey { key } = fetch.credential else {
        return Err(ProviderError::NoCredentials {
            provider: fetch.provider.id.to_string(),
        });
    };
    let mut key = CopilotKey::parse(key.expose());
    if needs_endpoint_discovery(&fetch.provider.base_url, &key) {
        let discovery = endpoint_discovery_request(&key)?;
        let body = copilot_get(fetch, discovery, sleep).await?;
        if let Some(endpoint) = decode_endpoint(&body) {
            key = key.with_api_endpoint(&endpoint);
        }
    }
    let wire = models_request(&fetch.provider.base_url, &key)?;
    let body = copilot_get(fetch, wire, sleep).await?;
    decode_copilot_models(fetch.provider, &body)
}

async fn copilot_get<S, D>(
    fetch: &ModelFetch<'_>,
    wire: GetWire,
    sleep: &S,
) -> Result<Vec<u8>, ProviderError>
where
    S: Fn(Duration) -> D,
    D: Future<Output = ()>,
{
    let url = wire.endpoint()?;
    let user_agent = wire.user_agent;
    let request = wire.into_request(fetch.client, url);
    let family = fetch.provider.family;
    let response = http::send(
        family,
        request,
        user_agent,
        Exchange::Json {
            total: NON_STREAM_TOTAL_TIMEOUT,
        },
        sleep,
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
