//! HTTP client, timeouts, byte limits, and the endpoint trust boundary.
//!
//! Every provider URL is built by [`endpoint`], every request leaves through
//! [`send`], and every non-streaming body is read through [`read_body`]. The
//! plain-HTTP rule holds at three points: the base URL, the final request URL,
//! and each redirect hop. Plain `http` is allowed only for the literal loopback
//! hosts `localhost`, `127.0.0.1`, and `::1`; nothing else reaches a socket.
//!
//! Timeout ownership, exactly:
//! - [`CONNECT_TIMEOUT`]: the client connect phase ([`build_client`]).
//! - [`STREAM_IDLE_TIMEOUT`]: the client read timeout ([`build_client`]); it
//!   resets after every body read, so it bounds the idle gap of a stream. It
//!   also caps the wait for response headers.
//! - [`RESPONSE_HEADER_TIMEOUT`]: reqwest has no header-only deadline, so
//!   [`send`] races the response head against a caller-supplied timer.
//! - Totals ([`NON_STREAM_TOTAL_TIMEOUT`], [`COMPACT_TIMEOUT`],
//!   [`USAGE_TIMEOUT`], [`OAUTH_TIMEOUT`]): the per-request deadline of
//!   [`Exchange::Json`], covering connect through the last body byte.
//! - [`LOGIN_WAIT`]: the sign-in flow's own wait for a callback; no request
//!   here enforces it.

use std::error::Error;
use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::pin::pin;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use dal_core::Family;
use futures::future::{Either, select};
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderValue, USER_AGENT};
use url::{Host, Url};

use crate::error::{LimitError, ProviderError};

/// Bound on TCP and TLS connection setup.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Bound from request start to the response status line and headers.
pub const RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(60);
/// Bound on the gap between two body reads of a response.
pub const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// Total bound on one non-streaming request.
pub const NON_STREAM_TOTAL_TIMEOUT: Duration = Duration::from_secs(120);
/// Total bound on one remote compaction request.
pub const COMPACT_TIMEOUT: Duration = Duration::from_secs(600);
/// Total bound on one account usage request.
pub const USAGE_TIMEOUT: Duration = Duration::from_secs(15);
/// Total bound on one OAuth token or device-code request.
pub const OAUTH_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a sign-in flow waits for the browser callback or device approval.
pub const LOGIN_WAIT: Duration = Duration::from_mins(15);
/// Largest WebSocket message accepted.
pub const WS_MESSAGE_LIMIT: usize = 16 << 20;
/// Largest non-streaming response body accepted.
pub const BODY_LIMIT: usize = 16 << 20;

/// Most redirect hops followed for one request.
const MAX_REDIRECTS: usize = 10;

/// The response shape of one request, which fixes its `accept` header and its
/// total deadline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Exchange {
    /// An SSE response: `accept: text/event-stream` and no total deadline; the
    /// stream is bounded by [`STREAM_IDLE_TIMEOUT`] between reads.
    Stream,
    /// One JSON response: `accept: application/json`, bounded end to end by
    /// `total`, one of the total timeout constants of this module.
    Json {
        /// Deadline from connect through the last body byte.
        total: Duration,
    },
}

/// Builds the one client a host keeps per provider set.
///
/// The client connects within [`CONNECT_TIMEOUT`], fails any read that stays
/// idle for [`STREAM_IDLE_TIMEOUT`], follows at most 10 redirects and only
/// within the origin of the request (a hop to another scheme, host, or port
/// never carries provider credentials), sends no `referer`, never
/// retries on its own (the request lifecycle owns every retry), and sets no
/// default headers. It keeps no cookies: the `cookies` feature is off, so
/// reqwest has no store to enable. reqwest's built-in `accept: */*` never goes
/// out, because [`send`] sets `accept` on every request.
///
/// # Panics
///
/// Panics when reqwest cannot initialize its TLS backend, which only a broken
/// build configuration causes.
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "the builder fails only when the compiled TLS backend cannot initialize"
)]
pub fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(STREAM_IDLE_TIMEOUT)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            match follow(attempt.url(), attempt.previous()) {
                Ok(()) => attempt.follow(),
                Err(refused) => attempt.error(refused),
            }
        }))
        .referer(false)
        .retry(reqwest::retry::never())
        .build()
        .expect("the reqwest TLS backend initializes")
}

/// A [`build_client`] client built on first use.
///
/// Building a client parses the platform trust roots, which costs tens of
/// milliseconds; a host that never sends a request (a terminal that has not yet
/// received a prompt) must not pay it at startup. Clones share one build.
#[derive(Clone, Debug, Default)]
pub(crate) struct LazyClient(Arc<OnceLock<reqwest::Client>>);

impl LazyClient {
    /// The shared client, built by [`build_client`] on the first call.
    pub(crate) fn get(&self) -> &reqwest::Client {
        self.0.get_or_init(build_client)
    }
}

impl From<reqwest::Client> for LazyClient {
    fn from(client: reqwest::Client) -> Self {
        Self(Arc::new(OnceLock::from(client)))
    }
}

/// Renders the `user-agent` value every request sends.
#[must_use]
pub fn user_agent(version: &str, os: &str, os_version: &str, arch: &str) -> String {
    format!("dalgon/{version} ({os} {os_version}; {arch})")
}

/// Builds the URL of one provider call: `base`, then exactly one `/`, then
/// `path`.
///
/// Trailing slashes of the base path are dropped and one leading `/` of `path`
/// is dropped, so `https://h/v1/` plus `responses` and `https://h/v1` plus
/// `/responses` both give `https://h/v1/responses`. `path` may end in a
/// `?query`. The result keeps the origin and path prefix of `base`.
///
/// # Errors
///
/// Every [`check_base_url`] failure, and [`ProviderError::Transport`] when
/// `path` is empty, carries a fragment, or holds a backslash or an empty,
/// `.`, or `..` segment.
pub fn endpoint(family: Family, base: &str, path: &str) -> Result<Url, ProviderError> {
    let base = admitted_base(family, base)?;
    let path = path.strip_prefix('/').unwrap_or(path);
    let segments = path.split_once('?').map_or(path, |(segments, _)| segments);
    if segments.is_empty() {
        return Err(transport(
            family,
            format!("endpoint path {path:?} is empty"),
        ));
    }
    if path.contains('#') {
        return Err(transport(
            family,
            format!("endpoint path {path:?} carries a fragment"),
        ));
    }
    if segments.contains('\\')
        || segments
            .split('/')
            .any(|segment| matches!(segment, "" | "." | ".."))
    {
        return Err(transport(
            family,
            format!("endpoint path {path:?} holds a backslash or an empty, `.`, or `..` segment"),
        ));
    }
    let prefix = base.path().trim_end_matches('/');
    let joined = format!("{}{prefix}/{path}", &base[..url::Position::BeforePath]);
    let url = Url::parse(&joined)
        .map_err(|error| transport(family, format!("endpoint {joined:?} is not a URL: {error}")))?;
    let keeps_prefix = url
        .path()
        .strip_prefix(prefix)
        .is_some_and(|rest| rest.starts_with('/'));
    if url.origin() != base.origin() || !keeps_prefix {
        return Err(transport(
            family,
            format!("endpoint path {path:?} escapes the base URL"),
        ));
    }
    Ok(url)
}

/// Checks a configured base URL without any network access.
///
/// `https` is accepted for any host. `http` is accepted only when the host is
/// `localhost`, `127.0.0.1`, or `::1`.
///
/// # Errors
///
/// [`ProviderError::PlainHttp`] for `http` on any other host;
/// [`ProviderError::Transport`] when `base` does not parse, uses another
/// scheme, has no host, carries user credentials, or carries a query or
/// fragment.
pub fn check_base_url(family: Family, base: &str) -> Result<(), ProviderError> {
    admitted_base(family, base).map(drop)
}

/// Sends one request and returns the response head, whatever its status.
///
/// Before any connection attempt the final URL passes the plain-HTTP rule. The
/// request then carries `user-agent: <user_agent>`, the `accept` value of
/// `exchange`, `content-type: application/json` when it has a body and names
/// no content type, and the total deadline of [`Exchange::Json`] (a stream has
/// none). `sleep` is the caller's timer, called once with
/// [`RESPONSE_HEADER_TIMEOUT`]; when it completes before the response head
/// arrives, the request is dropped. A timer that is already complete refuses
/// the request without touching the network.
///
/// # Errors
///
/// [`ProviderError::PlainHttp`] for a plain-HTTP non-loopback URL or redirect;
/// [`ProviderError::Transport`] for an invalid request or `user-agent`, a
/// refused redirect, a connect, TLS, or I/O failure, a timeout, or a missed
/// header deadline.
pub async fn send<S, D>(
    family: Family,
    request: reqwest::RequestBuilder,
    user_agent: &str,
    exchange: Exchange,
    sleep: S,
) -> Result<reqwest::Response, ProviderError>
where
    S: FnOnce(Duration) -> D,
    D: Future<Output = ()>,
{
    let (client, request) = prepare(family, request, user_agent, exchange)?;
    let deadline = pin!(sleep(RESPONSE_HEADER_TIMEOUT));
    let response = pin!(async move { client.execute(request).await });
    match select(deadline, response).await {
        Either::Left(((), _)) => Err(transport(
            family,
            format!(
                "no response headers within {} s",
                RESPONSE_HEADER_TIMEOUT.as_secs()
            ),
        )),
        Either::Right((Ok(response), _)) => Ok(response),
        Either::Right((Err(error), _)) => Err(from_reqwest(family, error)),
    }
}

/// Reads a whole non-streaming response body, at most [`BODY_LIMIT`] bytes.
///
/// # Errors
///
/// [`ProviderError::Limit`] with [`LimitError::Body`] as soon as the declared
/// length or the bytes read pass [`BODY_LIMIT`]; [`ProviderError::Transport`]
/// when reading fails or the total deadline passes.
pub async fn read_body(
    family: Family,
    mut response: reqwest::Response,
) -> Result<Vec<u8>, ProviderError> {
    let over = |length| u64::try_from(BODY_LIMIT).is_ok_and(|limit| length > limit);
    if response.content_length().is_some_and(over) {
        return Err(ProviderError::Limit(LimitError::Body));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| from_reqwest(family, error))?
    {
        append_capped(&mut body, &chunk)?;
    }
    Ok(body)
}

/// Appends `chunk` unless the body would pass [`BODY_LIMIT`].
fn append_capped(body: &mut Vec<u8>, chunk: &[u8]) -> Result<(), ProviderError> {
    if chunk.len() > BODY_LIMIT - body.len() {
        return Err(ProviderError::Limit(LimitError::Body));
    }
    body.extend_from_slice(chunk);
    Ok(())
}

/// Builds the request and applies every per-request rule of [`send`].
fn prepare(
    family: Family,
    request: reqwest::RequestBuilder,
    user_agent: &str,
    exchange: Exchange,
) -> Result<(reqwest::Client, reqwest::Request), ProviderError> {
    let (client, request) = request.build_split();
    let mut request = request.map_err(|error| from_reqwest(family, error))?;
    admit(family, request.url())?;
    let agent = HeaderValue::from_str(user_agent).map_err(|_| {
        transport(
            family,
            format!("user-agent {user_agent:?} is not a header value"),
        )
    })?;
    let has_body = request.body().is_some();
    let headers = request.headers_mut();
    headers.insert(USER_AGENT, agent);
    let (accept, total) = match exchange {
        Exchange::Stream => ("text/event-stream", None),
        Exchange::Json { total } => ("application/json", Some(total)),
    };
    headers.insert(ACCEPT, HeaderValue::from_static(accept));
    if has_body && !headers.contains_key(CONTENT_TYPE) {
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    }
    *request.timeout_mut() = total;
    Ok((client, request))
}

/// Parses `base` and applies the base-URL rules of [`check_base_url`].
fn admitted_base(family: Family, base: &str) -> Result<Url, ProviderError> {
    let url = Url::parse(base)
        .map_err(|error| transport(family, format!("base URL is not a URL: {error}")))?;
    admit(family, &url)?;
    if !url.username().is_empty() || url.password().is_some() {
        let host = url.host_str().unwrap_or_default();
        return Err(transport(
            family,
            format!("base URL for {host} must not carry credentials"),
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        let origin = url.origin().ascii_serialization();
        return Err(transport(
            family,
            format!("base URL for {origin} must not carry a query or fragment"),
        ));
    }
    Ok(url)
}

/// The plain-HTTP rule for any URL about to be requested.
fn admit(family: Family, url: &Url) -> Result<(), ProviderError> {
    match url.scheme() {
        "https" if url.host().is_some() => Ok(()),
        "http" if is_loopback(url) => Ok(()),
        "http" => Err(ProviderError::PlainHttp {
            host: url.host_str().unwrap_or_default().to_owned(),
        }),
        scheme => Err(transport(
            family,
            format!("URL scheme {scheme:?} is not https"),
        )),
    }
}

/// Whether the host of `url` is one of the literal loopback hosts. The URL
/// parser has already folded case and IPv4 spellings; any other name, even one
/// that resolves to loopback, is not accepted.
fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(name)) => name == "localhost",
        Some(Host::Ipv4(address)) => address == Ipv4Addr::LOCALHOST,
        Some(Host::Ipv6(address)) => address == Ipv6Addr::LOCALHOST,
        None => false,
    }
}

/// The redirect rule: a hop is followed when it is within [`MAX_REDIRECTS`]
/// and keeps the origin (scheme, host, port) of the hop before it. reqwest
/// strips only `authorization` and cookie headers when a redirect changes
/// host, so provider credentials in `x-api-key` or `chatgpt-account-id` would
/// reach a foreign server; a provider endpoint has no reason to redirect
/// elsewhere. A plain-HTTP hop off loopback stays a [`RefusedRedirect::PlainHttp`].
fn follow(next: &Url, previous: &[Url]) -> Result<(), RefusedRedirect> {
    if previous.len() > MAX_REDIRECTS {
        return Err(RefusedRedirect::TooMany);
    }
    match next.scheme() {
        "http" if !is_loopback(next) => Err(RefusedRedirect::PlainHttp {
            host: next.host_str().unwrap_or_default().to_owned(),
        }),
        "https" | "http"
            if previous
                .last()
                .is_some_and(|hop| hop.origin() == next.origin()) =>
        {
            Ok(())
        }
        _ => Err(RefusedRedirect::Target {
            origin: next.origin().ascii_serialization(),
        }),
    }
}

/// Why the redirect policy stopped a request; mapped back to
/// [`ProviderError`] by [`from_reqwest`].
#[derive(Debug)]
enum RefusedRedirect {
    PlainHttp { host: String },
    Target { origin: String },
    TooMany,
}

impl fmt::Display for RefusedRedirect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PlainHttp { host } => write!(f, "refusing plain http redirect to {host}"),
            Self::Target { origin } => write!(f, "refusing redirect to {origin}"),
            Self::TooMany => write!(f, "more than {MAX_REDIRECTS} redirects"),
        }
    }
}

impl Error for RefusedRedirect {}

/// Maps a reqwest failure to the typed provider error, recovering a refused
/// redirect from the source chain. The reason omits the URL.
pub(crate) fn from_reqwest(family: Family, error: reqwest::Error) -> ProviderError {
    let error = error.without_url();
    let mut reason = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        if let Some(RefusedRedirect::PlainHttp { host }) = cause.downcast_ref::<RefusedRedirect>() {
            return ProviderError::PlainHttp { host: host.clone() };
        }
        reason.push_str(": ");
        reason.push_str(&cause.to_string());
        source = cause.source();
    }
    transport(family, reason)
}

fn transport(family: Family, reason: String) -> ProviderError {
    ProviderError::Transport { family, reason }
}

#[cfg(test)]
mod tests;
