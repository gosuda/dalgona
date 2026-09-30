//! Live model-list fetch with typed-id fallback.
//!
//! One `ModelFetch` carries the client, credential, and cache directory for a
//! single provider refresh; failures degrade to cache then typed ids.

use std::{future::Future, time::Duration};

use dal_core::Family;

use super::{
    CatalogEntry, CatalogFetch, CatalogSource, ModelFetch,
    cache::{CacheError, read_cache_async, write_cache_async},
    decode::{decode_anthropic_page, decode_codex_models, decode_openai_models},
};
use crate::{
    auth::credential::Credential,
    error::ProviderError,
    http::{self, Exchange, NON_STREAM_TOTAL_TIMEOUT},
    provider::{AuthStyle, ProviderEntry},
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
    match fetch.provider.family {
        Family::Chat | Family::Responses => {
            let url = http::endpoint(fetch.provider.family, &fetch.provider.base_url, "models")?;
            let headers = auth_headers(fetch.provider, fetch.credential)?;
            let body = get_body(fetch, url, headers, sleep).await?;
            decode_openai_models(fetch.provider, &body)
        }
        Family::Anthropic => fetch_anthropic(fetch, sleep).await,
        Family::Codex => {
            let mut url = http::endpoint(Family::Codex, &fetch.provider.base_url, "models")?;
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
