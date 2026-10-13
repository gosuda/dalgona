// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! The web battery: `web_fetch` and `web_search` as one dalgona extension.

use std::fmt;

mod fetch;
mod html;
mod search;

pub(crate) const EXTENSION_NAME: &str = "web";
pub(crate) const USER_AGENT: &str = concat!("dalgona-web/", env!("CARGO_PKG_VERSION"));
/// The text of the `dalgona://web` manual page.
pub const WEB_DOC: &str = "\
# web

Two model tools.

`web_fetch` fetches one http or https URL and returns the page as Markdown.
It follows at most `max_redirects` redirects (default 10), revalidates the
scheme after every hop, reads at most `max_bytes` body bytes (clamped into
1024..=`max_body_bytes`, default ceiling 2097152), converts `text/html` and
`application/xhtml+xml` to Markdown, passes `text/*` and `application/json`
through unchanged, and rejects every other media type. The Markdown is
capped at `max_markdown_bytes` (default 131072); a cut sets `truncated` and
appends a truncation marker.

`web_search` queries Brave Web Search. The query is 1 to 400 characters;
`count` is 1 to 20 and defaults to 5. Results carry `title`, `url`, and
`snippet`. The API key is read at call time from the environment variable
named by `api_key_env` (default `BRAVE_API_KEY`); without it the tool
reports that it is not configured.

Config: the `[plugin.web]` table with `enabled`, `provider` (`brave` is
the only value), `api_key_env`, `timeout_secs` (1 to 300, default 30),
`max_redirects` (0 to 20, default 10), `max_body_bytes` (default
2097152), and `max_markdown_bytes` (default 131072). The battery needs
the `net` and `env` permissions; a missing permission makes each call
fail with a denied error naming the service. Requests go through the
host `net` service only; loopback HTTP(S) URLs use that same service
and remain subject to the host's network policy.";

/// Config section `[plugin.web]`.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebConfig {
    /// Whether the entry registers this battery.
    pub enabled: bool,
    /// The search provider. `brave` is the only supported value.
    pub provider: String,
    /// The environment variable that contains the Brave API key.
    pub api_key_env: String,
    /// The deadline for one network request, in seconds.
    pub timeout_secs: u32,
    /// The maximum number of redirects followed by `web_fetch`.
    pub max_redirects: u32,
    /// The maximum number of response body bytes read by `web_fetch`.
    pub max_body_bytes: usize,
    /// The maximum number of Markdown bytes returned by `web_fetch`.
    pub max_markdown_bytes: usize,
}

/// A typed web configuration error for the Dalgona build boundary.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct WebConfigError {
    message: Box<str>,
    #[source]
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl WebConfigError {
    fn missing_section() -> Self {
        Self {
            message: "invalid [plugin.web] config: missing section".into(),
            source: None,
        }
    }

    fn from_source<E>(source: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        let message = format!("invalid [plugin.web] config: {source}");
        Self {
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum WebConfigInvalid {
    #[error("unknown provider {provider:?}; the only provider is \"brave\"")]
    UnknownProvider { provider: String },
    #[error("api_key_env must not be empty")]
    EmptyApiKeyEnv,
    #[error("timeout_secs must be 1 to 300")]
    Timeout,
    #[error("max_redirects must be 0 to 20")]
    Redirects,
    #[error("max_body_bytes must be at least 1024")]
    BodyBytes,
    #[error("max_markdown_bytes must be at least 1")]
    MarkdownBytes,
}

/// Parses and validates the effective `[plugin.web]` TOML section.
///
/// # Errors
///
/// Returns a [`WebConfigError`] when the section is missing, holds an unknown field or a
/// wrongly typed value, or fails the provider and numeric limits.
///
/// The caller layers defaults and user configuration before this function.
/// Unknown fields and invalid values retain the `invalid [plugin.web] config:`
/// prefix in the typed error's display.
pub fn parse_config(section: Option<&toml::Value>) -> Result<WebConfig, WebConfigError> {
    let Some(section) = section else {
        return Err(WebConfigError::missing_section());
    };
    // `toml::Value::try_into` consumes its value; the shared Config retains the
    // original section for the other battery constructors.
    let config: WebConfig = section
        .clone()
        .try_into()
        .map_err(WebConfigError::from_source)?;
    config.validate().map_err(WebConfigError::from_source)?;
    Ok(config)
}

impl WebConfig {
    /// Checks the provider and numeric limits at the constructor boundary.
    fn validate(&self) -> Result<(), WebConfigInvalid> {
        if self.provider != "brave" {
            return Err(WebConfigInvalid::UnknownProvider {
                provider: self.provider.clone(),
            });
        }
        if self.api_key_env.is_empty() {
            return Err(WebConfigInvalid::EmptyApiKeyEnv);
        }
        if !(1..=300).contains(&self.timeout_secs) {
            return Err(WebConfigInvalid::Timeout);
        }
        if self.max_redirects > 20 {
            return Err(WebConfigInvalid::Redirects);
        }
        if self.max_body_bytes < 1024 {
            return Err(WebConfigInvalid::BodyBytes);
        }
        if self.max_markdown_bytes == 0 {
            return Err(WebConfigInvalid::MarkdownBytes);
        }
        Ok(())
    }
}

/// Which tool an error names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolLabel {
    /// The `web_fetch` tool.
    Fetch,
    /// The `web_search` tool.
    Search,
}

impl fmt::Display for ToolLabel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Fetch => "web_fetch",
            Self::Search => "web_search",
        })
    }
}

/// An error produced by web configuration, search, or fetch.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub(crate) enum WebError {
    /// The URL could not be parsed.
    #[error("invalid URL: {reason}")]
    BadUrl { reason: String },
    /// The URL scheme is not allowed.
    #[error("unsupported URL scheme \"{scheme}\": only http and https are allowed")]
    UnsupportedScheme { scheme: String },
    /// The fetch response has an unsupported media type.
    #[error("unsupported content type \"{content_type}\": web_fetch converts HTML and text only")]
    UnsupportedContentType { content_type: String },
    /// A network request exceeded its configured deadline.
    #[error("{tool} timed out after {secs}s: {url}")]
    Timeout {
        tool: ToolLabel,
        secs: u32,
        url: String,
    },
    /// Fetch followed more redirects than configured.
    #[error("{tool} exceeded {caps} redirects")]
    TooManyRedirects { tool: ToolLabel, caps: u32 },
    /// The server returned a non-success status.
    #[error("{tool} got HTTP {code} for {url}")]
    HttpStatus {
        tool: ToolLabel,
        code: u16,
        url: String,
    },
    /// The network service failed.
    #[error("{tool} failed: {cause}")]
    Network { tool: ToolLabel, cause: String },
    /// Search has no configured API key.
    #[error("web_search is not configured: set the {var} environment variable")]
    NotConfigured { var: String },
    /// The host cancelled the network request.
    #[error("request cancelled")]
    Cancelled,
    /// The host denied one of the declared services.
    #[error(transparent)]
    Denied(Box<dal_agent::error::ServiceError>),
}

/// Maps a service failure. Denials and cancellation keep their distinct
/// outcomes; other failures become a `Network` error with a bounded cause.
pub(crate) fn service_error(tool: ToolLabel, error: dal_agent::error::ServiceError) -> WebError {
    match error {
        dal_agent::error::ServiceError::Cancelled => WebError::Cancelled,
        denial @ dal_agent::error::ServiceError::Denied(_) => WebError::Denied(Box::new(denial)),
        other => WebError::Network {
            tool,
            cause: cap_bytes(&other.to_string(), 200).to_owned(),
        },
    }
}

/// Converts a web error into the model outcome, preserving cancellation and denials.
pub(crate) fn tool_outcome(error: WebError) -> dal_agent::ext::ToolOutcome {
    match error {
        WebError::Cancelled => dal_agent::ext::ToolOutcome::Interrupted,
        WebError::Denied(denial) => match *denial {
            dal_agent::error::ServiceError::Denied(reason) => {
                dal_agent::ext::ToolOutcome::Err(dal_agent::ToolError::Denied(reason))
            }
            other => {
                dal_agent::ext::ToolOutcome::Err(dal_agent::ToolError::message(other.to_string()))
            }
        },
        error => dal_agent::ext::ToolOutcome::Err(dal_agent::ToolError::message(error.to_string())),
    }
}

/// Runs one `net` request under the configured per-request deadline.
pub(crate) async fn timed_net(
    services: &dyn dal_agent::ext::Services,
    caller: &dal_agent::ext::Caller,
    request: dal_core::ext::FetchRequest,
    tool: ToolLabel,
    timeout_secs: u32,
) -> Result<dal_core::ext::FetchResponse, WebError> {
    let url = request.url.to_string();
    let deadline = std::time::Duration::from_secs(u64::from(timeout_secs));
    match tokio::time::timeout(deadline, services.net(caller, request)).await {
        Ok(Ok(response)) => Ok(response),
        Ok(Err(error)) => Err(service_error(tool, error)),
        Err(_elapsed) => Err(WebError::Timeout {
            tool,
            secs: timeout_secs,
            url,
        }),
    }
}

/// Builds a tool name and spec from a derived parameter schema.
pub(crate) fn tool_spec(
    name: &str,
    description: &str,
    schema: &schemars::Schema,
) -> Result<(dal_core::Name, std::sync::Arc<dal_core::ToolSpec>), dal_core::ext::RegistrationError>
{
    let name = dal_core::Name::parse(name)?;
    let parameters = sonic_rs::to_string(schema)
        .ok()
        .and_then(|encoded| dal_core::RawJson::parse(&encoded).ok())
        .filter(dal_core::valid_tool_parameters)
        .ok_or(dal_core::ext::RegistrationError::InvalidParameters)?;
    let spec = std::sync::Arc::new(dal_core::ToolSpec {
        name: name.clone(),
        description: description.into(),
        parameters,
        grammar: None,
    });
    Ok((name, spec))
}

/// Cuts a string to at most `max` bytes on a character boundary.
pub(crate) fn cap_bytes(value: &str, max: usize) -> &str {
    if value.len() <= max {
        return value;
    }
    let mut cut = max;
    while !value.is_char_boundary(cut) {
        cut -= 1;
    }
    &value[..cut]
}
/// Builds the web battery extension. Registers the fetch and search tools at
/// model visibility. The entry decodes and validates `[plugin.web]` with
/// [`parse_config`] before calling this constructor, so this function does
/// not re-validate and performs no ambient reads. [`WEB_DOC`] carries the
/// exact text of the `dalgona://web` page.
///
/// # Errors
///
/// Returns a [`dal_core::ext::RegistrationError`] when a declared name, service, or tool schema
/// is invalid.
pub fn web(
    config: WebConfig,
) -> Result<dal_agent::ext::Extension, dal_core::ext::RegistrationError> {
    use std::sync::Arc;

    let inject = dal_core::ext::ServiceSet::from_names(["net", "env"])?;
    dal_agent::ext::ExtensionBuilder::new(EXTENSION_NAME, env!("CARGO_PKG_VERSION"), inject)?
        .with_origin(dal_core::Origin::Bundled, None)
        .tool(
            Arc::new(fetch::FetchTool::new(config.clone())?),
            dal_core::ext::Visibility::Model,
        )
        .tool(
            Arc::new(search::SearchTool::new(config)?),
            dal_core::ext::Visibility::Model,
        )
        .build()
}

#[cfg(test)]
pub(crate) fn test_config() -> WebConfig {
    WebConfig {
        enabled: true,
        provider: "brave".into(),
        api_key_env: "BRAVE_API_KEY".into(),
        timeout_secs: 30,
        max_redirects: 10,
        max_body_bytes: 2_097_152,
        max_markdown_bytes: 131_072,
    }
}

#[cfg(test)]
mod tests {
    use super::{WebConfig, cap_bytes, parse_config, test_config};

    fn validation_message(config: &WebConfig) -> Option<String> {
        config.validate().err().map(|error| error.to_string())
    }

    #[test]
    fn accepts_the_web_config_defaults() {
        assert!(test_config().validate().is_ok());
    }

    #[test]
    fn rejects_an_unknown_provider() {
        let mut config = test_config();
        config.provider = "google".into();

        assert_eq!(
            validation_message(&config).as_deref(),
            Some("unknown provider \"google\"; the only provider is \"brave\"")
        );
    }

    #[test]
    fn rejects_an_empty_api_key_name() {
        let mut config = test_config();
        config.api_key_env.clear();

        assert_eq!(
            validation_message(&config).as_deref(),
            Some("api_key_env must not be empty")
        );
    }

    #[test]
    fn rejects_timeout_values_outside_the_inclusive_range() {
        let mut config = test_config();
        config.timeout_secs = 0;
        assert_eq!(
            validation_message(&config).as_deref(),
            Some("timeout_secs must be 1 to 300")
        );

        let mut config = test_config();
        config.timeout_secs = 301;
        assert_eq!(
            validation_message(&config).as_deref(),
            Some("timeout_secs must be 1 to 300")
        );
    }

    #[test]
    fn rejects_too_many_redirects() {
        let mut config = test_config();
        config.max_redirects = 21;

        assert_eq!(
            validation_message(&config).as_deref(),
            Some("max_redirects must be 0 to 20")
        );
    }

    #[test]
    fn rejects_a_body_limit_below_the_minimum() {
        let mut config = test_config();
        config.max_body_bytes = 1023;

        assert_eq!(
            validation_message(&config).as_deref(),
            Some("max_body_bytes must be at least 1024")
        );
    }

    #[test]
    fn rejects_a_zero_markdown_limit() {
        let mut config = test_config();
        config.max_markdown_bytes = 0;

        assert_eq!(
            validation_message(&config).as_deref(),
            Some("max_markdown_bytes must be at least 1")
        );
    }

    #[test]
    fn rejects_missing_and_unknown_web_config() -> Result<(), Box<dyn std::error::Error>> {
        let missing = parse_config(None).err().map(|error| error.to_string());
        assert_eq!(
            missing.as_deref(),
            Some("invalid [plugin.web] config: missing section")
        );

        let section: toml::Value = toml::from_str(
            "enabled = true\nprovider = \"brave\"\napi_key_env = \"BRAVE_API_KEY\"\ntimeout_secs = 30\nmax_redirects = 10\nmax_body_bytes = 2097152\nmax_markdown_bytes = 131072\ntypo = true\n",
        )?;
        let invalid = parse_config(Some(&section))
            .err()
            .map(|error| error.to_string());
        assert!(invalid.as_deref().is_some_and(|message| {
            message.starts_with("invalid [plugin.web] config:") && message.contains("typo")
        }));
        Ok(())
    }

    #[test]
    fn parses_a_complete_web_section() -> Result<(), Box<dyn std::error::Error>> {
        let section: toml::Value = toml::from_str(
            "enabled = true\nprovider = \"brave\"\napi_key_env = \"BRAVE_API_KEY\"\ntimeout_secs = 30\nmax_redirects = 10\nmax_body_bytes = 2097152\nmax_markdown_bytes = 131072\n",
        )?;
        let config = parse_config(Some(&section)).map_err(std::io::Error::other)?;

        assert_eq!(config.provider, "brave");
        assert_eq!(config.max_body_bytes, 2_097_152);
        Ok(())
    }
    #[test]
    fn caps_error_causes_at_utf8_boundaries() {
        let cause = "é".repeat(10);

        assert_eq!(cap_bytes(&cause, 3), "é");
    }

    #[test]
    fn maps_service_failures_with_the_calling_tool_label() {
        use super::{ToolLabel, service_error};
        let denied = service_error(
            ToolLabel::Search,
            dal_agent::error::ServiceError::Denied(dal_agent::error::DenyReason::NotInjected),
        );
        assert!(matches!(denied, super::WebError::Denied(_)));

        let failed = service_error(
            ToolLabel::Search,
            dal_agent::error::ServiceError::failed(None, "boom"),
        );
        assert_eq!(failed.to_string(), "web_search failed: boom");
    }
}

#[cfg(test)]
pub(crate) mod replay {
    //! A loopback HTTP server with path fixtures and a request log.

    use std::{
        collections::HashMap,
        convert::Infallible,
        pin::Pin,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        task::{Context, Poll},
        time::Duration,
    };

    use hyper::{
        Request, Response,
        body::{Body, Bytes, Frame, Incoming},
        header::{CONNECTION, CONTENT_LENGTH, CONTENT_TYPE, LOCATION},
        server::conn::http1,
        service::service_fn,
    };
    use hyper_util::rt::TokioIo;
    use tokio::net::TcpListener;

    const STREAM_CONTENT_LENGTH: usize = 5_242_880;
    const STREAM_CHUNK_BYTES: usize = 64 * 1024;

    /// One response selected by request path.
    pub(crate) struct Fixture {
        /// Exact request path.
        pub path: &'static str,
        /// HTTP status code.
        pub status: u16,
        /// Response media type.
        pub content_type: &'static str,
        /// Static response bytes.
        pub body: Vec<u8>,
        /// Delay before the server sends the response headers.
        pub delay: Option<Duration>,
        /// Optional `Location` header.
        pub redirect_to: Option<&'static str>,
        /// Optional streaming body length; its declared length remains larger.
        pub stream_len: Option<usize>,
    }

    /// The request received by the replay server.
    pub(crate) struct RecordedRequest {
        /// HTTP method.
        pub method: String,
        /// Request path without its query.
        pub path: String,
    }

    struct StoredFixture {
        status: u16,
        content_type: &'static str,
        body: Bytes,
        delay: Option<Duration>,
        redirect_to: Option<&'static str>,
        stream_len: Option<usize>,
    }

    impl From<Fixture> for StoredFixture {
        fn from(fixture: Fixture) -> Self {
            Self {
                status: fixture.status,
                content_type: fixture.content_type,
                body: Bytes::from(fixture.body),
                delay: fixture.delay,
                redirect_to: fixture.redirect_to,
                stream_len: fixture.stream_len,
            }
        }
    }

    /// The loopback server and its observable request/body counters.
    pub(crate) struct Replay {
        /// Bound loopback address.
        pub addr: std::net::SocketAddr,
        /// Requests accepted by the server.
        pub requests: Arc<Mutex<Vec<RecordedRequest>>>,
        /// Response body bytes yielded by the server.
        pub bytes_served: Arc<AtomicUsize>,
        listener: TcpListener,
        fixtures: Arc<HashMap<&'static str, Arc<StoredFixture>>>,
    }

    impl Replay {
        /// Binds `127.0.0.1:0` and stores the response fixtures.
        pub(crate) async fn spawn(fixtures: Vec<Fixture>) -> Self {
            let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                .await
                .expect("bind replay server to an ephemeral loopback port");
            let addr = listener
                .local_addr()
                .expect("read the replay server's bound address");
            let fixtures: Arc<HashMap<&'static str, Arc<StoredFixture>>> = Arc::new(
                fixtures
                    .into_iter()
                    .map(|fixture| (fixture.path, Arc::new(StoredFixture::from(fixture))))
                    .collect(),
            );
            Self {
                addr,
                requests: Arc::new(Mutex::new(Vec::new())),
                bytes_served: Arc::new(AtomicUsize::new(0)),
                listener,
                fixtures,
            }
        }

        /// Builds a request URL using this server's ephemeral address.
        pub(crate) fn url(&self, path: &str) -> String {
            format!("http://{}{}", self.addr, path)
        }
        /// Serves `count` connections in order. Redirect chains need one
        /// connection per hop; tests drive this alongside the tool call with
        /// `tokio::join!`, so no background task or spawn is required.
        pub(crate) async fn serve_requests(&self, count: usize) -> Result<(), std::io::Error> {
            for _ in 0..count {
                self.serve_next().await?;
            }
            Ok(())
        }
        /// Accepts and serves one HTTP/1 connection. The test network service
        /// polls this future alongside its real loopback client request.
        pub(crate) async fn serve_next(&self) -> Result<(), std::io::Error> {
            let (stream, _) = self.listener.accept().await?;
            let fixtures = Arc::clone(&self.fixtures);
            let requests = Arc::clone(&self.requests);
            let bytes_served = Arc::clone(&self.bytes_served);
            let service = service_fn(move |request: Request<Incoming>| {
                let fixtures = Arc::clone(&fixtures);
                let requests = Arc::clone(&requests);
                let bytes_served = Arc::clone(&bytes_served);
                async move {
                    record_request(&requests, &request);
                    let path = request.uri().path();
                    let fixture = fixtures
                        .get(path)
                        .cloned()
                        .unwrap_or_else(|| Arc::new(missing_fixture()));
                    if let Some(delay) = fixture.delay {
                        tokio::time::sleep(delay).await;
                    }
                    response(&fixture, bytes_served)
                }
            });
            http1::Builder::new()
                .keep_alive(false)
                .serve_connection(TokioIo::new(stream), service)
                .await
                .map_err(std::io::Error::other)
        }
    }

    fn record_request(requests: &Mutex<Vec<RecordedRequest>>, request: &Request<Incoming>) {
        let recorded = RecordedRequest {
            method: request.method().as_str().to_owned(),
            path: request.uri().path().to_owned(),
        };
        requests
            .lock()
            .expect("replay request log is not poisoned")
            .push(recorded);
    }

    fn missing_fixture() -> StoredFixture {
        StoredFixture {
            status: 404,
            content_type: "text/plain",
            body: Bytes::from_static(b"missing fixture"),
            delay: None,
            redirect_to: None,
            stream_len: None,
        }
    }

    fn response(
        fixture: &StoredFixture,
        bytes_served: Arc<AtomicUsize>,
    ) -> Result<Response<ReplayBody>, hyper::http::Error> {
        let (body, content_length) = match fixture.stream_len {
            Some(stream_len) => (Bytes::from(vec![b'a'; stream_len]), STREAM_CONTENT_LENGTH),
            None => (Bytes::clone(&fixture.body), fixture.body.len()),
        };
        let mut builder = Response::builder()
            .status(fixture.status)
            .header(CONTENT_TYPE, fixture.content_type)
            .header(CONTENT_LENGTH, content_length.to_string())
            .header(CONNECTION, "close");
        if let Some(location) = fixture.redirect_to {
            builder = builder.header(LOCATION, location);
        }
        builder.body(ReplayBody {
            bytes: body,
            offset: 0,
            bytes_served,
        })
    }

    struct ReplayBody {
        bytes: Bytes,
        offset: usize,
        bytes_served: Arc<AtomicUsize>,
    }

    impl Body for ReplayBody {
        type Data = Bytes;
        type Error = Infallible;

        fn poll_frame(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            let this = self.get_mut();
            if this.offset == this.bytes.len() {
                return Poll::Ready(None);
            }
            let end = this
                .offset
                .saturating_add(STREAM_CHUNK_BYTES)
                .min(this.bytes.len());
            let chunk = this.bytes.slice(this.offset..end);
            this.offset = end;
            this.bytes_served.fetch_add(chunk.len(), Ordering::SeqCst);
            Poll::Ready(Some(Ok(Frame::data(chunk))))
        }
    }
    #[cfg(test)]
    mod tests {
        use super::{Fixture, Replay};
        use std::sync::atomic::Ordering;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        #[tokio::test]
        async fn serves_a_fixture_and_logs_the_request() {
            let replay = Replay::spawn(vec![Fixture {
                path: "/hello",
                status: 200,
                content_type: "text/html",
                body: b"<h1>Hello</h1>".to_vec(),
                delay: None,
                redirect_to: None,
                stream_len: None,
            }])
            .await;
            let addr = replay.addr;
            let (server, client) = tokio::join!(replay.serve_requests(1), async move {
                let mut stream = tokio::net::TcpStream::connect(addr)
                    .await
                    .expect("connect to replay");
                stream
                    .write_all(
                        b"GET /hello HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
                    )
                    .await
                    .expect("send request");
                let mut raw = Vec::new();
                stream.read_to_end(&mut raw).await.expect("read response");
                raw
            });
            server.expect("serve one request");
            let text = String::from_utf8_lossy(&client).into_owned();
            assert!(text.starts_with("HTTP/1.1 200"), "{text}");
            assert!(text.contains("<h1>Hello</h1>"), "{text}");
            let requests = replay.requests.lock().expect("request log is not poisoned");
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].method, "GET");
            assert_eq!(requests[0].path, "/hello");
            assert!(replay.bytes_served.load(Ordering::SeqCst) >= b"<h1>Hello</h1>".len());
            assert_eq!(replay.url("/hello"), format!("http://{addr}/hello"));
        }
    }
}
