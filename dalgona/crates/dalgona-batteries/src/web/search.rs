// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! The `web_search` tool: one Brave Web Search request.

use std::sync::Arc;

use dal_agent::ext::{ArgError, BoxFuture, RawValue, ToolCall, ToolCx, ToolOutcome, ToolOutput};
use dal_core::{Name, RegistrationError, ToolClass, ToolSpec, Workspace};
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};

use super::WebError;

pub(crate) const TOOL_NAME: &str = "web_search";
pub(crate) const TOOL_DESCRIPTION: &str =
    "Search the web with Brave. Returns up to count results with title, url, and snippet.";
pub(crate) const BRAVE_ENDPOINT: &str = "https://api.search.brave.com/res/v1/web/search";
pub(crate) const DEFAULT_COUNT: u32 = 5;
pub(crate) const MAX_QUERY_CHARS: usize = 400;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct WebSearchParams {
    /// The query. 1 to 400 characters.
    #[schemars(length(min = 1, max = 400))]
    pub query: String,
    /// Result count. 1 to 20; default 5.
    #[schemars(range(min = 1, max = 20))]
    pub count: Option<u32>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct SearchResult {
    pub query: String,
    pub provider: &'static str,
    pub results: Vec<SearchHit>,
}

#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// Projects a Brave reply onto the named result fields. Unknown fields are
/// ignored; malformed or missing `web.results` yields an empty result list.
pub(crate) fn brave_results(body: &Value) -> Vec<SearchHit> {
    let Some(results) = body
        .get("web")
        .and_then(|web| web.get("results"))
        .and_then(|results| results.as_array())
    else {
        return Vec::new();
    };

    results
        .iter()
        .filter_map(|item| {
            let url = item.get("url")?.as_str()?.to_owned();
            let title = item
                .get("title")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_owned();
            let snippet = item
                .get("description")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_owned();
            Some(SearchHit {
                title,
                url,
                snippet,
            })
        })
        .collect()
}

/// Builds the Brave search URL. `endpoint` is [`BRAVE_ENDPOINT`] in
/// production; tests pass the loopback replay URL for the same path.
fn search_url(endpoint: &str, query: &str, count: u32) -> Result<reqwest::Url, WebError> {
    let mut url = reqwest::Url::parse(endpoint).map_err(|error| WebError::BadUrl {
        reason: error.to_string(),
    })?;
    url.query_pairs_mut()
        .append_pair("q", query)
        .append_pair("count", &count.to_string());
    Ok(url)
}

/// Builds the single-hop `net` request for one search URL.
fn search_request(url: &reqwest::Url, api_key: &str) -> dal_core::ext::FetchRequest {
    dal_core::ext::FetchRequest {
        method: dal_core::ext::FetchMethod::Get,
        url: url.as_str().into(),
        headers: vec![
            ("X-Subscription-Token".into(), api_key.into()),
            ("User-Agent".into(), super::USER_AGENT.into()),
        ],
        body: Vec::new(),
    }
}

/// The validated result count, defaulting to [`DEFAULT_COUNT`].
fn effective_count(requested: Option<u32>) -> u32 {
    requested.unwrap_or(DEFAULT_COUNT)
}

/// Parses one search `net` response. A non-2xx status becomes `HttpStatus`;
/// a non-JSON body yields empty results, never an error.
fn parse_search_response(
    query: &str,
    final_url: &str,
    response: &dal_core::ext::FetchResponse,
    max_bytes: usize,
) -> Result<SearchResult, WebError> {
    let status = response.status();
    if !(200..300).contains(&status) {
        return Err(WebError::HttpStatus {
            tool: super::ToolLabel::Search,
            code: status,
            url: final_url.to_owned(),
        });
    }
    let body = response.read_bytes(max_bytes);
    let value: Value = sonic_rs::from_slice(body).unwrap_or_default();
    Ok(SearchResult {
        query: query.to_owned(),
        provider: "brave",
        results: brave_results(&value),
    })
}

/// The `web_search` tool shell. The `Tool` run pipeline waits for the
/// extension `env`/`net` service surface and `ToolCx`; this shell carries
/// the validated config until then.
pub(crate) struct SearchTool {
    cfg: super::WebConfig,
    name: Name,
    spec: Arc<ToolSpec>,
}

impl SearchTool {
    pub(crate) fn new(cfg: super::WebConfig) -> Result<Self, RegistrationError> {
        let (name, spec) = super::tool_spec(
            TOOL_NAME,
            TOOL_DESCRIPTION,
            &schemars::schema_for!(WebSearchParams),
        )?;
        Ok(Self { cfg, name, spec })
    }
}

impl dal_agent::ext::Tool for SearchTool {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &dal_core::ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _workspace: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let params: WebSearchParams = match sonic_rs::from_str(call.args.as_str()) {
                Ok(params) => params,
                Err(error) => {
                    return ToolOutcome::Err(dal_agent::ToolError::message(format!(
                        "{TOOL_NAME}: invalid input: {error}."
                    )));
                }
            };
            if params.query.trim().is_empty() || params.query.chars().count() > MAX_QUERY_CHARS {
                return ToolOutcome::Err(dal_agent::ToolError::message(format!(
                    "{TOOL_NAME}: query must be 1 to {MAX_QUERY_CHARS} characters."
                )));
            }
            let count = effective_count(params.count);
            let services = cx.services();
            let caller = cx.caller().clone();
            let api_key = match services.env(&caller, &self.cfg.api_key_env).await {
                Ok(Some(key)) if !key.is_empty() => key,
                Ok(None | Some(_)) => {
                    return ToolOutcome::Err(dal_agent::ToolError::message(
                        WebError::NotConfigured {
                            var: self.cfg.api_key_env.clone(),
                        }
                        .to_string(),
                    ));
                }
                Err(dal_agent::error::ServiceError::Denied(reason)) => {
                    return ToolOutcome::Err(dal_agent::ToolError::Denied(reason));
                }
                Err(error) => {
                    return super::tool_outcome(super::service_error(
                        super::ToolLabel::Search,
                        error,
                    ));
                }
            };
            let url = match search_url(BRAVE_ENDPOINT, &params.query, count) {
                Ok(url) => url,
                Err(error) => {
                    return ToolOutcome::Err(dal_agent::ToolError::message(error.to_string()));
                }
            };
            let response = match super::timed_net(
                services.as_ref(),
                &caller,
                search_request(&url, &api_key),
                super::ToolLabel::Search,
                self.cfg.timeout_secs,
            )
            .await
            {
                Ok(response) => response,
                Err(error) => return super::tool_outcome(error),
            };
            match parse_search_response(
                &params.query,
                url.as_str(),
                &response,
                self.cfg.max_body_bytes,
            ) {
                Ok(result) => match sonic_rs::to_string(&result) {
                    Ok(text) => ToolOutcome::Ok(Box::new(ToolOutput::from_text(text))),
                    Err(error) => ToolOutcome::Err(dal_agent::ToolError::message(format!(
                        "{TOOL_NAME}: {error}."
                    ))),
                },
                Err(error) => super::tool_outcome(error),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{SearchHit, brave_results};
    use sonic_rs::Value;

    #[test]
    fn returns_no_hits_for_missing_or_non_array_results() -> Result<(), Box<dyn std::error::Error>>
    {
        for document in ["{}", r#"{"web":{"results":"nope"}}"#] {
            let value: Value = sonic_rs::from_str(document)?;
            assert!(brave_results(&value).is_empty(), "{document}");
        }
        Ok(())
    }

    #[test]
    fn drops_hits_without_a_string_url() -> Result<(), Box<dyn std::error::Error>> {
        let value: Value =
            sonic_rs::from_str(r#"{"web":{"results":[{"title":"missing url"},{"url":3}]}}"#)?;

        assert_eq!(brave_results(&value).len(), 0);
        Ok(())
    }

    #[test]
    fn fills_missing_hit_text_and_ignores_unknown_fields() -> Result<(), Box<dyn std::error::Error>>
    {
        let value: Value = sonic_rs::from_str(
            r#"{"web":{"results":[{"url":"https://example.test/","unknown":true}]},"future":1}"#,
        )?;

        assert_eq!(
            brave_results(&value),
            [SearchHit {
                title: String::new(),
                url: "https://example.test/".to_owned(),
                snippet: String::new(),
            }]
        );
        Ok(())
    }
    #[test]
    fn builds_a_brave_url_with_query_and_count() {
        let url =
            super::search_url(super::BRAVE_ENDPOINT, "parse rust", 2).expect("valid search url");
        assert_eq!(url.path(), "/res/v1/web/search");
        let query: std::collections::HashMap<String, String> =
            url.query_pairs().into_owned().collect();
        assert_eq!(query.get("q").map(String::as_str), Some("parse rust"));
        assert_eq!(query.get("count").map(String::as_str), Some("2"));
    }

    #[test]
    fn builds_a_subscription_token_request() {
        let url = super::search_url("http://127.0.0.1:9/res/v1/web/search", "q", 1)
            .expect("valid loopback url");
        let request = super::search_request(&url, "k");
        assert!(matches!(request.method, dal_core::ext::FetchMethod::Get));
        assert!(request.url.as_ref().contains("q=q&count=1"));
        assert!(request.headers.iter().any(|(name, value)| {
            name.as_ref() == "X-Subscription-Token" && value.as_ref() == "k"
        }));
    }

    #[test]
    fn parses_a_brave_reply_and_ignores_unknown_keys() {
        let response = dal_core::ext::FetchResponse {
            status: 200,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: br#"{"web":{"results":[{"title":"a","url":"https://a.test/","description":"s","future":1}]},"future":2}"#.to_vec(),
        };
        let result = super::parse_search_response(
            "parse rust",
            "https://api.search.brave.com/res/v1/web/search",
            &response,
            2_097_152,
        )
        .expect("valid reply");
        assert_eq!(result.provider, "brave");
        assert_eq!(result.query, "parse rust");
        assert_eq!(result.results.len(), 1);
        assert_eq!(result.results[0].snippet, "s");
    }

    #[test]
    fn treats_non_json_search_bodies_as_empty_results() {
        let response = dal_core::ext::FetchResponse {
            status: 200,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: b"not json".to_vec(),
        };
        let result = super::parse_search_response("q", "http://x/", &response, 1024)
            .expect("non-json is empty");
        assert_eq!(result.results.len(), 0);
    }

    #[test]
    fn reports_search_status_with_the_final_url() {
        let response = dal_core::ext::FetchResponse {
            status: 429,
            headers: Vec::new(),
            body: Vec::new(),
        };
        assert_eq!(
            super::parse_search_response("q", "http://x/search", &response, 1024)
                .err()
                .map(|error| error.to_string())
                .as_deref(),
            Some("web_search got HTTP 429 for http://x/search")
        );
    }

    #[test]
    fn decodes_search_params_and_default_count() -> Result<(), Box<dyn std::error::Error>> {
        let params: super::WebSearchParams = sonic_rs::from_str(r#"{"query":"parse rust"}"#)?;
        assert_eq!(params.query, "parse rust");
        assert_eq!(params.count, None);
        assert_eq!(super::effective_count(params.count), 5);
        assert_eq!(super::effective_count(Some(2)), 2);
        Ok(())
    }
}
