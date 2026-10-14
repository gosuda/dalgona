//! Live model-list fetch with typed-id fallback.
//!
//! One `ModelFetch` carries the client, credential, and cache directory for a
//! single provider refresh; failures degrade to cache then typed ids.

use std::{future::Future, pin::pin, time::Duration};

use futures::future::{Either, select};

use dal_core::Family;

use super::{
    CatalogEntry, CatalogFetch, CatalogSource, ModelFetch,
    cache::{CacheError, read_cache_async, write_cache_async},
    decode::{decode_anthropic_page, decode_codex_models, decode_openai_models},
};
use crate::{
    error::{LimitError, ProviderError},
    http::{self, Exchange, NON_STREAM_TOTAL_TIMEOUT},
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
    }
}

// All limits apply to one logical refresh, not to individual HTTP requests.
const ANTHROPIC_CATALOG_MAX_PAGES: usize = 32;
const ANTHROPIC_CATALOG_MAX_ROWS: usize = 10_000;
const ANTHROPIC_CATALOG_MAX_BYTES: usize = 32 << 20;
const ANTHROPIC_CATALOG_MAX_CURSOR_BYTES: usize = 64 << 10;
const ANTHROPIC_CATALOG_TOTAL_TIMEOUT: Duration = Duration::from_secs(120);

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
    // A single timer also covers response reads and JSON decoding. Dropping
    // the losing future cancels the in-flight HTTP request and its page state.
    let deadline = pin!(sleep(ANTHROPIC_CATALOG_TOTAL_TIMEOUT));
    let pages = pin!(fetch_anthropic_pages(fetch, sleep, base_url, headers));
    match select(deadline, pages).await {
        Either::Left(((), _)) => Err(ProviderError::Limit(LimitError::CatalogTimeout)),
        Either::Right((result, _)) => result,
    }
}

async fn fetch_anthropic_pages<S, D>(
    fetch: &ModelFetch<'_>,
    sleep: &S,
    base_url: url::Url,
    headers: Vec<(String, String)>,
) -> Result<Vec<CatalogEntry>, ProviderError>
where
    S: Fn(Duration) -> D,
    D: Future<Output = ()>,
{
    let mut seen_cursors = std::collections::HashSet::new();
    let mut rows = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    let mut row_count = 0;
    let mut bytes = 0;
    let mut cursor_bytes = 0;

    loop {
        // Refuse the next request before issuing it if the page budget is spent.
        charge_catalog_budget(&mut pages, 1, ANTHROPIC_CATALOG_MAX_PAGES, LimitError::CatalogPages)?;
        let mut url = base_url.clone();
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("limit", "1000");
            if let Some(cursor) = &cursor {
                query.append_pair("after_id", cursor);
            }
        }
        let body = get_body(fetch, url, headers.clone(), sleep).await?;
        charge_catalog_budget(
            &mut bytes,
            body.len(),
            ANTHROPIC_CATALOG_MAX_BYTES,
            LimitError::CatalogBytes,
        )?;
        let page = decode_anthropic_page(fetch.provider, &body)?;
        charge_catalog_budget(
            &mut row_count,
            page.rows.len(),
            ANTHROPIC_CATALOG_MAX_ROWS,
            LimitError::CatalogRows,
        )?;
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
        charge_catalog_budget(
            &mut cursor_bytes,
            next_cursor.len(),
            ANTHROPIC_CATALOG_MAX_CURSOR_BYTES,
            LimitError::CatalogCursorBytes,
        )?;
        if !seen_cursors.insert(next_cursor.clone()) {
            return Err(super::decode::protocol(
                Family::Anthropic,
                "model list repeated a last_id cursor",
            ));
        }
        cursor = Some(next_cursor);
    }
}

// The counter changes only when its new total is representable and in budget.
fn charge_catalog_budget(
    used: &mut usize,
    additional: usize,
    maximum: usize,
    error: LimitError,
) -> Result<(), ProviderError> {
    let Some(total) = used.checked_add(additional).filter(|total| *total <= maximum) else {
        return Err(ProviderError::Limit(error));
    };
    *used = total;
    Ok(())
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

#[cfg(test)]
mod budget_tests {
    use super::*;

    #[test]
    fn anthropic_budgets_accept_the_boundary_and_reject_additional_usage() {
        for (maximum, reason) in [
            (ANTHROPIC_CATALOG_MAX_PAGES, LimitError::CatalogPages),
            (ANTHROPIC_CATALOG_MAX_ROWS, LimitError::CatalogRows),
            (ANTHROPIC_CATALOG_MAX_BYTES, LimitError::CatalogBytes),
            (
                ANTHROPIC_CATALOG_MAX_CURSOR_BYTES,
                LimitError::CatalogCursorBytes,
            ),
        ] {
            let mut used = 0;
            charge_catalog_budget(&mut used, maximum, maximum, reason)
                .expect("the exact budget is admitted");
            assert_eq!(used, maximum);
            assert!(matches!(
                charge_catalog_budget(&mut used, 1, maximum, reason),
                Err(ProviderError::Limit(actual)) if actual == reason
            ));
            assert_eq!(used, maximum);
        }
    }

    #[test]
    fn anthropic_budget_rejects_integer_overflow_without_mutation() {
        let mut used = 1;
        assert!(matches!(
            charge_catalog_budget(&mut used, usize::MAX, usize::MAX, LimitError::CatalogBytes),
            Err(ProviderError::Limit(LimitError::CatalogBytes))
        ));
        assert_eq!(used, 1);
    }
}
