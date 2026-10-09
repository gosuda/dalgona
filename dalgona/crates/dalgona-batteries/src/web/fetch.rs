// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! The `web_fetch` tool: one http or https URL to Markdown.

use std::sync::Arc;

use dal_agent::ext::{ArgError, BoxFuture, ToolCall, ToolCx, ToolOutcome, ToolOutput};
use dal_core::{Name, RegistrationError, ToolClass, ToolSpec, Workspace};

use super::{ToolLabel, WebConfig, WebError};

pub(crate) const TOOL_NAME: &str = "web_fetch";
pub(crate) const TOOL_DESCRIPTION: &str = "Fetch one http or https URL and return its \
    content as Markdown. Follows at most the configured redirects, reads at most \
    max_bytes body bytes, converts HTML, and passes text and JSON through.";
pub(crate) const MIN_BODY_BYTES: u64 = 1024;
pub(crate) const MAX_URL_BYTES: usize = 2048;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct WebFetchParams {
    /// The URL. 1 to 2048 bytes; scheme http or https.
    #[schemars(length(min = 1, max = 2048))]
    pub url: String,
    /// Body read cap in bytes. Optional; clamped into `1024..=max_body_bytes`.
    pub max_bytes: Option<u64>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct FetchResult {
    pub url: String,
    pub final_url: String,
    pub status: u16,
    pub content_type: String,
    pub markdown: String,
    pub truncated: bool,
    pub raw_bytes: usize,
}

/// Validates in byte-length, parse, scheme order. The `Url` parser's message is
/// preserved verbatim for a parse failure.
fn validate_url(raw: &str) -> Result<reqwest::Url, WebError> {
    if raw.len() > MAX_URL_BYTES {
        return Err(WebError::BadUrl {
            reason: "longer than 2048 bytes".into(),
        });
    }
    let url = reqwest::Url::parse(raw).map_err(|error| WebError::BadUrl {
        reason: error.to_string(),
    })?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        other => Err(WebError::UnsupportedScheme {
            scheme: other.to_owned(),
        }),
    }
}

/// Clamps the requested read cap into `1024..=max_body_bytes`.
fn effective_max_bytes(requested: Option<u64>, config: &WebConfig) -> usize {
    // Dalgona's supported x86_64/aarch64 targets make this usize-to-u64
    // conversion lossless.
    let ceiling = config.max_body_bytes as u64;
    let bounded = requested.unwrap_or(ceiling).clamp(MIN_BODY_BYTES, ceiling);
    usize::try_from(bounded).unwrap_or(config.max_body_bytes)
}
/// The `web_fetch` tool.
pub(crate) struct FetchTool {
    cfg: WebConfig,
    name: Name,
    spec: Arc<ToolSpec>,
}

impl FetchTool {
    pub(crate) fn new(cfg: WebConfig) -> Result<Self, RegistrationError> {
        let (name, spec) = super::tool_spec(
            TOOL_NAME,
            TOOL_DESCRIPTION,
            &schemars::schema_for!(WebFetchParams),
        )?;
        Ok(Self { cfg, name, spec })
    }
}

impl dal_agent::ext::Tool for FetchTool {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &dal_core::ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(
        &self,
        _args: &dal_agent::ext::RawValue,
        _workspace: &Workspace,
    ) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let params: WebFetchParams = match decode_args(call.args.as_str()) {
                Ok(params) => params,
                Err(outcome) => return *outcome,
            };
            let mut url = match validate_url(&params.url) {
                Ok(url) => url,
                Err(error) => return outcome(error),
            };
            let max_bytes = effective_max_bytes(params.max_bytes, &self.cfg);
            let services = cx.services();
            let caller = cx.caller().clone();
            for _ in 0..=self.cfg.max_redirects {
                let request = fetch_request(&url);
                let response = match super::timed_net(
                    services.as_ref(),
                    &caller,
                    request,
                    ToolLabel::Fetch,
                    self.cfg.timeout_secs,
                )
                .await
                {
                    Ok(response) => response,
                    Err(error) => return outcome(error),
                };
                match redirect_target(&url, response.status(), response.header("location")) {
                    Ok(Some(next)) => url = next,
                    Ok(None) => {
                        return convert_result(
                            &params.url,
                            &url,
                            &response,
                            max_bytes,
                            self.cfg.max_markdown_bytes,
                        );
                    }
                    Err(error) => return outcome(error),
                }
            }
            outcome(WebError::TooManyRedirects {
                tool: super::ToolLabel::Fetch,
                caps: self.cfg.max_redirects,
            })
        })
    }
}

fn decode_args(args: &str) -> Result<WebFetchParams, Box<ToolOutcome>> {
    sonic_rs::from_str(args).map_err(|error| {
        Box::new(ToolOutcome::Err(dal_agent::ToolError::message(format!(
            "{TOOL_NAME}: invalid input: {error}."
        ))))
    })
}

fn convert_result(
    requested: &str,
    final_url: &reqwest::Url,
    response: &dal_core::ext::FetchResponse,
    max_bytes: usize,
    max_markdown_bytes: usize,
) -> ToolOutcome {
    match convert_response(
        requested,
        final_url,
        response,
        max_bytes,
        max_markdown_bytes,
    ) {
        Ok(result) => match sonic_rs::to_string(&result) {
            Ok(text) => ToolOutcome::Ok(Box::new(ToolOutput::from_text(text))),
            Err(error) => ToolOutcome::Err(dal_agent::ToolError::message(format!(
                "{TOOL_NAME}: {error}."
            ))),
        },
        Err(error) => outcome(error),
    }
}

fn outcome(error: WebError) -> ToolOutcome {
    super::tool_outcome(error)
}

/// Builds the single-hop `net` request for one fetch URL.
fn fetch_request(url: &reqwest::Url) -> dal_core::ext::FetchRequest {
    dal_core::ext::FetchRequest {
        method: dal_core::ext::FetchMethod::Get,
        url: url.as_str().into(),
        headers: vec![("User-Agent".into(), super::USER_AGENT.into())],
        body: Vec::new(),
    }
}

/// Resolves one redirect hop. Returns `Ok(None)` when the status is not a
/// redirect, `Ok(Some(next))` for a same-scheme http(s) hop, and a typed
/// error for a missing location, an unparsable target, an unsupported
/// scheme, or a target above the URL byte limit.
fn redirect_target(
    current: &reqwest::Url,
    status: u16,
    location: Option<&str>,
) -> Result<Option<reqwest::Url>, WebError> {
    if !(300..400).contains(&status) {
        return Ok(None);
    }
    let Some(location) = location else {
        return Err(WebError::HttpStatus {
            tool: super::ToolLabel::Fetch,
            code: status,
            url: current.as_str().to_owned(),
        });
    };
    let next = current
        .join(location.trim())
        .map_err(|error| WebError::BadUrl {
            reason: error.to_string(),
        })?;
    if next.as_str().len() > MAX_URL_BYTES {
        return Err(WebError::BadUrl {
            reason: "longer than 2048 bytes".into(),
        });
    }
    match next.scheme() {
        "http" | "https" => Ok(Some(next)),
        other => Err(WebError::UnsupportedScheme {
            scheme: other.to_owned(),
        }),
    }
}

/// Converts one final `net` response into the tool result. Redirect statuses
/// never reach this function; non-2xx statuses become `HttpStatus`.
fn convert_response(
    requested: &str,
    final_url: &reqwest::Url,
    response: &dal_core::ext::FetchResponse,
    max_bytes: usize,
    max_markdown_bytes: usize,
) -> Result<FetchResult, WebError> {
    let status = response.status();
    if !(200..300).contains(&status) {
        return Err(WebError::HttpStatus {
            tool: super::ToolLabel::Fetch,
            code: status,
            url: final_url.as_str().to_owned(),
        });
    }
    let content_type = response.header("content-type").unwrap_or("").to_owned();
    let body = response.read_bytes(max_bytes);
    let raw_bytes = body.len();
    let text = String::from_utf8_lossy(body).into_owned();
    let markdown = match super::html::classify_media_type(&content_type) {
        None => {
            return Err(WebError::UnsupportedContentType { content_type });
        }
        Some(super::html::MediaKind::PassThrough) => text,
        Some(super::html::MediaKind::Convert) => {
            super::html::to_markdown(&text).map_err(|error| WebError::Network {
                tool: super::ToolLabel::Fetch,
                cause: super::cap_bytes(&error.to_string(), 200).to_owned(),
            })?
        }
    };
    let (markdown, truncated) = super::html::cap_markdown(markdown, max_markdown_bytes);
    Ok(FetchResult {
        url: requested.to_owned(),
        final_url: final_url.as_str().to_owned(),
        status,
        content_type,
        markdown,
        truncated,
        raw_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::{MAX_URL_BYTES, effective_max_bytes, validate_url};
    use crate::web::test_config;

    #[test]
    fn rejects_ftp_urls_before_network_access() {
        let error = validate_url("ftp://example.com/x");

        assert_eq!(
            error.err().map(|error| error.to_string()).as_deref(),
            Some("unsupported URL scheme \"ftp\": only http and https are allowed")
        );
    }

    #[test]
    fn rejects_urls_above_the_byte_limit() {
        let url = "x".repeat(MAX_URL_BYTES + 1);
        let error = validate_url(&url);

        assert_eq!(
            error.err().map(|error| error.to_string()).as_deref(),
            Some("invalid URL: longer than 2048 bytes")
        );
    }

    #[test]
    fn accepts_urls_at_the_byte_limit() {
        let prefix = "http://example.test/";
        let url = format!("{prefix}{}", "x".repeat(MAX_URL_BYTES - prefix.len()));

        assert_eq!(url.len(), MAX_URL_BYTES);
        assert!(validate_url(&url).is_ok());
    }

    #[test]
    fn clamps_requested_body_limits() {
        let config = test_config();

        assert_eq!(effective_max_bytes(Some(5), &config), 1024);
        assert_eq!(effective_max_bytes(Some(999_999_999), &config), 2_097_152);
        assert_eq!(effective_max_bytes(None, &config), 2_097_152);
    }

    #[test]
    fn accepts_http_and_https_urls() {
        assert!(validate_url("http://example.test/").is_ok());
        assert!(validate_url("https://example.test/").is_ok());
    }

    #[test]
    fn builds_a_single_hop_get_request() {
        let url = validate_url("http://example.test/page").expect("valid fixture url");
        let request = super::fetch_request(&url);

        assert!(matches!(request.method, dal_core::ext::FetchMethod::Get));
        assert_eq!(request.url.as_ref(), "http://example.test/page");
        assert!(request.body.is_empty());
        assert!(request.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("User-Agent") && value.as_ref().starts_with("dalgona-web/")
        }));
    }

    #[test]
    fn follows_relative_redirects_and_rejects_ftp_targets() {
        let current = validate_url("http://example.test/a").expect("valid url");
        let next = super::redirect_target(&current, 302, Some("/b"))
            .expect("redirect resolves")
            .expect("redirect hop");
        assert_eq!(next.as_str(), "http://example.test/b");

        let current = validate_url("http://example.test/r").expect("valid url");
        let error = super::redirect_target(&current, 302, Some("ftp://host/x"));
        assert_eq!(
            error.err().map(|error| error.to_string()).as_deref(),
            Some("unsupported URL scheme \"ftp\": only http and https are allowed")
        );

        let current = validate_url("http://example.test/a").expect("valid url");
        let error = super::redirect_target(&current, 302, None);
        assert_eq!(
            error.err().map(|error| error.to_string()).as_deref(),
            Some("web_fetch got HTTP 302 for http://example.test/a")
        );

        let current = validate_url("http://example.test/a").expect("valid url");
        assert!(
            super::redirect_target(&current, 200, Some("/b"))
                .expect("non-redirect")
                .is_none()
        );
    }

    #[test]
    fn converts_html_and_passes_text_through() {
        let config = test_config();
        let max_bytes = super::effective_max_bytes(None, &config);
        let requested = "http://example.test/";
        let final_url = validate_url(requested).expect("valid url");

        let html = dal_core::ext::FetchResponse {
            status: 200,
            headers: vec![("Content-Type".into(), "text/html".into())],
            body: b"<h1>Hello</h1>".to_vec(),
        };
        let converted = super::convert_response(
            requested,
            &final_url,
            &html,
            max_bytes,
            config.max_markdown_bytes,
        )
        .expect("html converts");
        assert_eq!(converted.markdown, "# Hello");
        assert_eq!(converted.status, 200);
        assert_eq!(converted.final_url, requested);
        assert!(!converted.truncated);
        assert_eq!(converted.raw_bytes, b"<h1>Hello</h1>".len());

        let text = dal_core::ext::FetchResponse {
            status: 200,
            headers: vec![("Content-Type".into(), "text/plain".into())],
            body: b"abc".to_vec(),
        };
        let passed = super::convert_response(
            requested,
            &final_url,
            &text,
            max_bytes,
            config.max_markdown_bytes,
        )
        .expect("text passes through");
        assert_eq!(passed.markdown, "abc");
        assert_eq!(passed.content_type, "text/plain");
    }

    #[test]
    fn rejects_unsupported_media_and_error_statuses() {
        let config = test_config();
        let max_bytes = super::effective_max_bytes(None, &config);
        let requested = "http://example.test/";
        let final_url = validate_url(requested).expect("valid url");

        let image = dal_core::ext::FetchResponse {
            status: 200,
            headers: vec![("Content-Type".into(), "image/png".into())],
            body: vec![0, 1, 2],
        };
        assert_eq!(
            super::convert_response(
                requested,
                &final_url,
                &image,
                max_bytes,
                config.max_markdown_bytes
            )
            .err()
            .map(|error| error.to_string())
            .as_deref(),
            Some("unsupported content type \"image/png\": web_fetch converts HTML and text only")
        );

        let missing = dal_core::ext::FetchResponse {
            status: 404,
            headers: vec![("Content-Type".into(), "text/html".into())],
            body: b"no".to_vec(),
        };
        assert_eq!(
            super::convert_response(
                requested,
                &final_url,
                &missing,
                max_bytes,
                config.max_markdown_bytes
            )
            .err()
            .map(|error| error.to_string())
            .as_deref(),
            Some("web_fetch got HTTP 404 for http://example.test/")
        );
    }
    #[test]
    fn decodes_fetch_params() -> Result<(), Box<dyn std::error::Error>> {
        let params: super::WebFetchParams =
            sonic_rs::from_str(r#"{"url":"https://example.test/"}"#)?;
        assert_eq!(params.url, "https://example.test/");
        assert_eq!(params.max_bytes, None);
        Ok(())
    }
}
