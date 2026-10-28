//! The device-code sign-in, also the fallback for hosts that cannot bind the
//! browser callback.
//!
//! Polling stays here so the authorization flow owns one deadline and one
//! cancellation token. The Codex JSON device flow ends in a grant for the
//! ordinary token exchange; the `WorkOS`, Radius, xAI, and Kimi form device
//! flows (RFC 8628) deliver the tokens directly, with no second exchange, and
//! the Meta flow delivers an identity token that the sign-in exchanges for
//! its API key. Polling owns no persistence.

use std::time::Duration;

use dal_core::Family;
use serde::Deserialize;
use tokio::time::{Instant, sleep, sleep_until, timeout_at};
use tokio_util::sync::CancellationToken;

use sonic_rs::JsonValueTrait;
use url::Url;

use crate::{Hook, OAuthCredential, ProviderError, SecretString};

use super::credential::oauth_expires_at;
use super::oauth::{
    DeviceUrls, LoginProgress, OAuthHttp, decode_json, error_message, post_form_with, post_json,
    report_progress, unix_now,
};

/// The device authorization result required by the ordinary token exchange.
///
/// The returned verifier is provided by the device endpoint. It is deliberately
/// not formatted with `Debug` or included in errors.
pub(crate) struct DeviceGrant {
    pub(crate) authorization_code: String,
    pub(crate) code_verifier: String,
}

/// The outcome of a device authorization: a grant for the ordinary token
/// exchange (Codex), the tokens themselves (`WorkOS`, Radius, xAI, and Kimi,
/// whose device poll completes the grant with no second exchange), or an
/// identity token that is not yet a credential (Meta, whose sign-in mints
/// the API key from it). The identity token is never formatted with `Debug`
/// or included in errors.
pub(crate) enum DeviceOutcome {
    Grant(DeviceGrant),
    Tokens(OAuthCredential),
    Identity(SecretString),
}

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_auth_id: String,
    #[serde(alias = "usercode")]
    user_code: String,
    #[serde(
        default = "default_poll_interval",
        deserialize_with = "deserialize_interval"
    )]
    interval: u64,
}

#[derive(Deserialize)]
struct DevicePollResponse {
    authorization_code: String,
    code_verifier: String,
}

const fn default_poll_interval() -> u64 {
    5
}

fn deserialize_interval<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Interval {
        Number(u64),
        Text(String),
    }

    match Interval::deserialize(deserializer)? {
        Interval::Number(seconds) => Ok(seconds),
        Interval::Text(seconds) => seconds
            .parse()
            .map_err(<D::Error as serde::de::Error>::custom),
    }
}

/// Request and poll a device code until approval, cancellation, or timeout.
///
/// `hook` selects the device flow and `client_id` is the row's effective
/// client id. [`Hook::CodexAccount`] runs the JSON device flow and returns a
/// [`DeviceOutcome::Grant`] for the ordinary token exchange;
/// [`Hook::WorkOsDevice`], [`Hook::RadiusGateway`], [`Hook::XaiDevice`], and
/// [`Hook::KimiCodeDevice`] run the form-encoded RFC 8628 device flow and
/// return [`DeviceOutcome::Tokens`] directly, with no second exchange;
/// [`Hook::MetaMuse`] runs the same flow and returns the
/// [`DeviceOutcome::Identity`] token its sign-in mints from. The requests of
/// a flow carry `urls.headers`. The remaining hooks offer no device sign-in,
/// so reaching them is a caller bug.
///
/// Both arms share the transport, progress, cancellation, and deadline
/// mechanisms. Device codes, tokens, and remote bodies never enter errors.
pub(crate) async fn run(
    http: &OAuthHttp<'_>,
    family: Family,
    hook: Hook,
    client_id: &str,
    urls: &DeviceUrls,
    deadline: Instant,
    progress: &(dyn Fn(LoginProgress) + Send + Sync),
    cancel: &CancellationToken,
) -> Result<DeviceOutcome, ProviderError> {
    match hook {
        Hook::CodexAccount => run_codex(http, family, client_id, urls, deadline, progress, cancel)
            .await
            .map(DeviceOutcome::Grant),
        Hook::WorkOsDevice => {
            run_rfc8628(
                http,
                family,
                Rfc8628::WorkOs,
                client_id,
                urls,
                deadline,
                progress,
                cancel,
            )
            .await
        }
        Hook::RadiusGateway => {
            run_rfc8628(
                http,
                family,
                Rfc8628::Radius,
                client_id,
                urls,
                deadline,
                progress,
                cancel,
            )
            .await
        }
        Hook::XaiDevice => {
            run_rfc8628(
                http,
                family,
                Rfc8628::Xai,
                client_id,
                urls,
                deadline,
                progress,
                cancel,
            )
            .await
        }
        Hook::MetaMuse => {
            run_rfc8628(
                http,
                family,
                Rfc8628::Meta,
                client_id,
                urls,
                deadline,
                progress,
                cancel,
            )
            .await
        }
        Hook::KimiCodeDevice => {
            run_rfc8628(
                http,
                family,
                Rfc8628::Kimi,
                client_id,
                urls,
                deadline,
                progress,
                cancel,
            )
            .await
        }
        Hook::ClaudeCode
        | Hook::GitLabDuo
        | Hook::OpenRouterKey
        | Hook::CursorPoll
        | Hook::DevinCliToken
        | Hook::GoogleCloudCodeAssist
        | Hook::GoogleAntigravity => Err(ProviderError::AuthWrite {
            reason: String::from("this sign-in has no device flow"),
        }),
    }
}

/// The safety margin added to the server's device-poll interval, matching the
/// reference headless flow: every poll waits the server interval plus three
/// seconds.
const CODEX_POLL_SAFETY_MARGIN: Duration = Duration::from_secs(3);

/// The upper bound on Codex device polls, matching the reference headless
/// flow: after this many pending answers the sign-in times out instead of
/// polling forever on server errors.
const CODEX_MAX_POLLS: u32 = 120;

/// The wait before a Codex device poll: the server interval, at least one
/// second, plus the reference safety margin.
fn codex_poll_interval(device_interval_secs: u64) -> Duration {
    Duration::from_secs(device_interval_secs.max(1)).saturating_add(CODEX_POLL_SAFETY_MARGIN)
}

/// Whether a Codex device-poll status means approval is still pending: the
/// remote service answers 403 and 404 while the user has not approved yet.
fn is_codex_pending(status: u16) -> bool {
    matches!(status, 403 | 404)
}

/// Request and poll a Codex device code until approval, cancellation, or
/// timeout.
///
/// The remote service's 403 and 404 poll responses both mean that approval is
/// pending. Polling waits the server-provided interval plus three seconds
/// between requests, stops after [`CODEX_MAX_POLLS`] pending answers, and is
/// bounded by `deadline`; cancellation drops the current request or sleep.
/// Empty device or grant fields fail the sign-in instead of polling or
/// exchanging with a blank code.
async fn run_codex(
    http: &OAuthHttp<'_>,
    family: Family,
    client_id: &str,
    urls: &DeviceUrls,
    deadline: Instant,
    progress: &(dyn Fn(LoginProgress) + Send + Sync),
    cancel: &CancellationToken,
) -> Result<DeviceGrant, ProviderError> {
    run_codex_with_limit(
        http,
        family,
        client_id,
        urls,
        deadline,
        progress,
        cancel,
        CODEX_MAX_POLLS,
    )
    .await
}

async fn run_codex_with_limit(
    http: &OAuthHttp<'_>,
    family: Family,
    client_id: &str,
    urls: &DeviceUrls,
    deadline: Instant,
    progress: &(dyn Fn(LoginProgress) + Send + Sync),
    cancel: &CancellationToken,
    max_polls: u32,
) -> Result<DeviceGrant, ProviderError> {
    let Some(page) = &urls.page else {
        return Err(ProviderError::AuthWrite {
            reason: String::from("the Codex device row names no verification page"),
        });
    };
    let start = post_json(
        http,
        family,
        &urls.usercode,
        &DeviceCodeRequest { client_id },
    )
    .await?;
    if !(200..300).contains(&start.status) {
        return Err(ProviderError::DeviceCode {
            status: start.status,
            message: error_message(&start.body, &[]),
        });
    }
    let device = decode_json::<DeviceCodeResponse>(&start.body).ok_or_else(|| {
        ProviderError::DeviceCode {
            status: start.status,
            message: String::from("invalid device-code response"),
        }
    })?;
    if device.device_auth_id.is_empty() || device.user_code.is_empty() {
        return Err(ProviderError::DeviceCode {
            status: start.status,
            message: String::from("invalid device-code response"),
        });
    }
    let interval = codex_poll_interval(device.interval);
    report_progress(
        progress,
        LoginProgress::ShowCode {
            url: page.as_str().to_owned(),
            code: device.user_code.clone(),
        },
        cancel,
    )?;

    for _ in 0..max_polls {
        wait_for_next_poll(deadline, interval, cancel).await?;
        let poll = post_json(
            http,
            family,
            &urls.poll,
            &DevicePollRequest {
                device_auth_id: &device.device_auth_id,
                user_code: &device.user_code,
            },
        )
        .await?;
        if is_codex_pending(poll.status) {
            continue;
        }
        if !(200..300).contains(&poll.status) {
            return Err(ProviderError::DeviceCode {
                status: poll.status,
                message: error_message(&poll.body, &[&device.device_auth_id, &device.user_code]),
            });
        }
        let grant = decode_json::<DevicePollResponse>(&poll.body).ok_or_else(|| {
            ProviderError::DeviceCode {
                status: poll.status,
                message: String::from("invalid device-token response"),
            }
        })?;
        if grant.authorization_code.is_empty() || grant.code_verifier.is_empty() {
            return Err(ProviderError::DeviceCode {
                status: poll.status,
                message: String::from("invalid device-token response"),
            });
        }
        return Ok(DeviceGrant {
            authorization_code: grant.authorization_code,
            code_verifier: grant.code_verifier,
        });
    }
    Err(ProviderError::LoginTimeout)
}

#[derive(serde::Serialize)]
struct DeviceCodeRequest<'a> {
    client_id: &'a str,
}

#[derive(serde::Serialize)]
struct DevicePollRequest<'a> {
    device_auth_id: &'a str,
    user_code: &'a str,
}

/// The extra wait added for a `slow_down` poll answer that names no interval
/// of its own (RFC 8628 §3.5).
const SLOW_DOWN_INCREMENT: Duration = Duration::from_secs(5);

/// The shortest wait between two polls.
const MINIMUM_INTERVAL: Duration = Duration::from_secs(1);

/// The poll interval RFC 8628 §3.2 prescribes when the authorization
/// response names none.
const RFC_DEFAULT_INTERVAL_SECS: u64 = 5;

/// The lifetime of a Kimi device code whose authorization response names no
/// positive `expires_in`: the pinned 15 minutes.
const KIMI_DEVICE_LIFETIME_SECS: u64 = 15 * 60;

/// The lifetime an xAI token answer without `expires_in` gets: the pinned
/// hour.
const XAI_TOKEN_LIFETIME_SECS: i64 = 3600;

/// The expiry of an xAI token answer counted from `now`: `expires_in`
/// seconds when the answer names a positive one, the pinned hour when it
/// names none, and `None` when it names one that is not positive (an invalid
/// answer). The reference also subtracts five minutes at this point; the
/// refresher's proactive window of the same length already refreshes that
/// early, so stored here the expiry is the token's own.
pub(crate) fn xai_expires_at(now: i64, expires_in: Option<i64>) -> Option<i64> {
    match expires_in {
        None => Some(now.saturating_add(XAI_TOKEN_LIFETIME_SECS)),
        Some(seconds) if seconds > 0 => Some(now.saturating_add(seconds)),
        Some(_) => None,
    }
}

/// Which RFC 8628 form flow runs. The flavors share the transport, progress,
/// cancellation, and deadline mechanisms and differ in what the authorization
/// request sends, what the authorization response must carry, which
/// verification page is shown, how the poll cadence moves, what the token
/// delivery holds, and how an expired or denied code is worded. Each
/// difference is one method below with no wildcard, so a new flavor must
/// choose every one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Rfc8628 {
    /// `WorkOS`: the response may omit the verification page (the row's page
    /// is shown) and its lifetime; an expired code times out.
    WorkOs,
    /// Radius: the response must name `verification_uri` and a positive
    /// `expires_in`, and an expired or denied code is a typed device-code
    /// failure.
    Radius,
    /// xAI: the request names the `referrer`; the response must name an
    /// `https` `verification_uri` and a positive `expires_in`, and the page
    /// shown is the `https` complete URI when there is one. The first poll
    /// waits one interval, `slow_down` follows the interval it names, and a
    /// token answer must carry an access and a refresh token and may omit
    /// its lifetime (one hour).
    Xai,
    /// Meta: the response names a complete or plain `http(s)` verification
    /// URI and may omit its lifetime. The first poll waits one interval,
    /// `slow_down` follows the interval it names, and the token answer holds
    /// only the identity token.
    Meta,
    /// Kimi Code: the response must name both an `http(s)` `verification_uri`
    /// and `verification_uri_complete`, and the complete one is shown; the
    /// code lives 15 minutes when the response names no lifetime. The first
    /// poll waits one interval, `slow_down` follows the interval it names, a
    /// server error ends the sign-in, and a token answer must carry an access
    /// token, a refresh token, and a positive lifetime.
    Kimi,
}

impl Rfc8628 {
    /// The `referrer` the authorization request names after the client and
    /// scopes, when the vendor asks for one.
    const fn referrer(self) -> Option<&'static str> {
        match self {
            Self::Xai => Some("pi"),
            Self::WorkOs | Self::Radius | Self::Meta | Self::Kimi => None,
        }
    }

    /// Whether the first poll waits one interval, as the reference clients
    /// that name the pinned flows do; `WorkOS` and Radius poll at once.
    const fn waits_before_first_poll(self) -> bool {
        match self {
            Self::Xai | Self::Meta | Self::Kimi => true,
            Self::WorkOs | Self::Radius => false,
        }
    }

    /// Whether a `slow_down` answer moves the interval to the positive one it
    /// names (falling back to the five-second increment), and an interval of
    /// zero in the authorization response means the RFC default instead of
    /// the shortest wait.
    const fn follows_server_interval(self) -> bool {
        match self {
            Self::Xai | Self::Meta | Self::Kimi => true,
            Self::WorkOs | Self::Radius => false,
        }
    }

    /// Whether a server error status ends the sign-in before the body is
    /// read, whatever code the body names.
    const fn fails_on_server_error(self) -> bool {
        match self {
            Self::Kimi => true,
            Self::WorkOs | Self::Radius | Self::Xai | Self::Meta => false,
        }
    }

    /// The poll interval the authorization response asks for.
    fn initial_interval(self, seconds: u64) -> Duration {
        let seconds = if self.follows_server_interval() && seconds == 0 {
            RFC_DEFAULT_INTERVAL_SECS
        } else {
            seconds
        };
        Duration::from_secs(seconds).max(MINIMUM_INTERVAL)
    }

    /// The interval after a `slow_down` answer that named `named`.
    fn slowed(self, current: Duration, named: Option<Duration>) -> Duration {
        match named.filter(|_| self.follows_server_interval()) {
            Some(interval) => interval,
            None => current.saturating_add(SLOW_DOWN_INCREMENT),
        }
    }

    /// The device-code lifetime in seconds that bounds polling: the response's
    /// own (a positive one for the flavors that insist on it), the pinned
    /// default when the flavor has one, else none, and the flow deadline
    /// alone applies.
    fn lifetime(self, expires_in: Option<u64>) -> Option<u64> {
        match self {
            Self::WorkOs | Self::Radius | Self::Xai => expires_in,
            Self::Meta => expires_in.filter(|seconds| *seconds > 0),
            Self::Kimi => expires_in
                .filter(|seconds| *seconds > 0)
                .or(Some(KIMI_DEVICE_LIFETIME_SECS)),
        }
    }

    /// The failure of an expired device code. `WorkOS` reports the expiry as
    /// a timeout; the others as a typed device-code failure that names the
    /// vendor.
    fn expired(self, status: u16) -> ProviderError {
        let message = match self {
            Self::WorkOs => return ProviderError::LoginTimeout,
            Self::Radius => "device authorization expired",
            Self::Xai => "xAI device code expired",
            Self::Meta => "Meta device authorization expired. Please restart login.",
            Self::Kimi => "Kimi Code device authorization expired. Please restart login.",
        };
        ProviderError::DeviceCode {
            status,
            message: String::from(message),
        }
    }

    /// The failure of a denied device code.
    fn denied(self, family: Family, status: u16) -> ProviderError {
        let message = match self {
            Self::WorkOs => {
                return ProviderError::Status {
                    family,
                    status,
                    message: String::from("device authorization was denied"),
                };
            }
            Self::Radius => "device authorization was denied",
            Self::Xai => "xAI device authorization was denied",
            Self::Meta => "Meta login was denied.",
            Self::Kimi => "Kimi Code login was denied.",
        };
        ProviderError::DeviceCode {
            status,
            message: String::from(message),
        }
    }

    /// Whether `code` is this flavor's second spelling of a denial: xAI's
    /// issuer may answer `authorization_denied`.
    fn denies(self, code: &str) -> bool {
        code == "access_denied" || (self == Self::Xai && code == "authorization_denied")
    }

    /// The outcome of a successful poll answer: the tokens, or for Meta the
    /// identity token. Anything that does not fit the flavor is an invalid
    /// token response that quotes nothing of the body.
    fn delivery(self, status: u16, body: &[u8]) -> Result<DeviceOutcome, ProviderError> {
        let invalid = || ProviderError::TokenExchange {
            status,
            message: String::from("invalid token response"),
        };
        let token = decode_json::<TokenDelivery>(body).ok_or_else(invalid)?;
        if token.access_token.is_empty() {
            return Err(invalid());
        }
        if self == Self::Meta {
            return Ok(DeviceOutcome::Identity(SecretString::from(
                token.access_token,
            )));
        }
        let refresh_token = token
            .refresh_token
            .filter(|token| !token.is_empty())
            .ok_or_else(invalid)?;
        let now = unix_now();
        let expires_at = match self {
            Self::WorkOs | Self::Radius => {
                oauth_expires_at(now, token.expires_in, &token.access_token)
            }
            Self::Xai => Some(xai_expires_at(now, token.expires_in).ok_or_else(invalid)?),
            Self::Kimi => match token.expires_in {
                Some(seconds) if seconds > 0 => Some(now.saturating_add(seconds)),
                Some(_) | None => return Err(invalid()),
            },
            Self::Meta => return Err(invalid()),
        };
        Ok(DeviceOutcome::Tokens(OAuthCredential {
            expires_at,
            access_token: SecretString::from(token.access_token),
            refresh_token: SecretString::from(refresh_token),
            id_token: None,
            account_id: None,
            project_id: None,
        }))
    }
}

/// `uri` as a normalized URL when its scheme is one of `schemes`. The page is
/// shown to the user and may be opened by a host, so no other scheme is
/// admitted.
fn uri_with_scheme(uri: Option<&str>, schemes: &[&str]) -> Option<String> {
    let url = Url::parse(nonempty_uri(uri)?).ok()?;
    schemes.contains(&url.scheme()).then(|| url.to_string())
}

/// `uri` as a normalized URL when it is an `https` URL.
fn https_uri(uri: Option<&str>) -> Option<String> {
    uri_with_scheme(uri, &["https"])
}

/// `uri` as a normalized URL when it is an `http` or `https` URL.
fn web_uri(uri: Option<&str>) -> Option<String> {
    uri_with_scheme(uri, &["https", "http"])
}

/// The interval a `slow_down` answer names: a finite positive number of
/// seconds, never below the shortest wait.
fn named_interval(value: Option<&sonic_rs::Value>) -> Option<Duration> {
    let seconds = value?.as_f64()?;
    if !seconds.is_finite() || seconds <= 0.0 {
        return None;
    }
    Duration::try_from_secs_f64(seconds)
        .ok()
        .map(|interval| interval.max(MINIMUM_INTERVAL))
}

/// The device authorization response (RFC 8628 §3.2): the code pair, the
/// verification URLs, the poll cadence, and the code lifetime. The field names
/// are the compiled `login "device-code"` response paths.
#[derive(Deserialize)]
struct DeviceAuthorization {
    device_code: String,
    user_code: String,
    #[serde(default)]
    verification_uri: Option<String>,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    #[serde(
        default = "default_poll_interval",
        deserialize_with = "deserialize_interval"
    )]
    interval: u64,
    #[serde(default)]
    expires_in: Option<u64>,
}

fn nonempty_uri(uri: Option<&str>) -> Option<&str> {
    uri.filter(|uri| !uri.is_empty())
}

impl DeviceAuthorization {
    /// The verification page to show for `flavor`, or `None` when the
    /// response is unusable for it: an empty code, for Radius a missing
    /// `verification_uri` or a missing or zero lifetime, and for the xAI,
    /// Meta, and Kimi flavors a page that is not a plain web URL of the
    /// scheme they admit.
    fn shown_url(&self, flavor: Rfc8628, urls: &DeviceUrls) -> Option<String> {
        if self.device_code.is_empty() || self.user_code.is_empty() {
            return None;
        }
        match flavor {
            Rfc8628::WorkOs => nonempty_uri(self.verification_uri_complete.as_deref())
                .or_else(|| nonempty_uri(self.verification_uri.as_deref()))
                .map(str::to_owned)
                .or_else(|| urls.page.as_ref().map(|page| page.as_str().to_owned())),
            Rfc8628::Radius => self
                .expires_in
                .filter(|seconds| *seconds > 0)
                .and_then(|_| nonempty_uri(self.verification_uri.as_deref()))
                .map(str::to_owned),
            Rfc8628::Xai => {
                let page = https_uri(self.verification_uri.as_deref())?;
                self.expires_in.filter(|seconds| *seconds > 0)?;
                match nonempty_uri(self.verification_uri_complete.as_deref()) {
                    Some(complete) => https_uri(Some(complete)),
                    None => Some(page),
                }
            }
            Rfc8628::Meta => web_uri(self.verification_uri_complete.as_deref())
                .or_else(|| web_uri(self.verification_uri.as_deref())),
            Rfc8628::Kimi => {
                web_uri(self.verification_uri.as_deref())?;
                web_uri(self.verification_uri_complete.as_deref())
            }
        }
    }
}

/// A poll answer carrying an RFC 8628 error code and, for `slow_down`, the
/// interval the issuer asks for. The interval is read leniently (any JSON
/// value) so an unexpected type never hides the error code.
#[derive(Deserialize)]
struct PollError {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    interval: Option<sonic_rs::Value>,
}

/// The token delivery: the access grant plus the refresh grant and an
/// optional lifetime. There is no ID token and no account id; the shape
/// mirrors the existing token responses without the Codex-only `id_token`.
/// Each flavor decides which of the other members it needs; Meta's answer
/// holds only the access member, its identity token.
#[derive(Deserialize)]
struct TokenDelivery {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

/// Request and poll an RFC 8628 device code until the grant completes,
/// cancellation, or timeout.
///
/// The start and poll bodies are form-encoded; the start body names the
/// client, the space-joined scopes for a row that asks for scopes, and the
/// flavor's `referrer` when it has one. Both requests carry `urls.headers`.
/// A 2xx poll answer without an error code delivers the tokens directly (or
/// Meta's identity token). `authorization_pending` waits the server interval;
/// `slow_down` adds five seconds, or for the flavors that follow the issuer
/// moves to the interval the answer names; an expired code ends the sign-in
/// (a timeout for `WorkOS`, a typed device-code failure for the others) and a
/// denial is a typed denial. Anything else is a device-code failure with the
/// device codes redacted. A flavor that waits before its first poll sleeps one
/// interval first. Polling stops at the earlier of `deadline` and the device
/// response lifetime; an in-flight poll past the device lifetime times out
/// without cancelling the caller.
async fn run_rfc8628(
    http: &OAuthHttp<'_>,
    family: Family,
    flavor: Rfc8628,
    client_id: &str,
    urls: &DeviceUrls,
    deadline: Instant,
    progress: &(dyn Fn(LoginProgress) + Send + Sync),
    cancel: &CancellationToken,
) -> Result<DeviceOutcome, ProviderError> {
    let start_body = {
        let mut form = url::form_urlencoded::Serializer::new(String::new());
        form.append_pair("client_id", client_id);
        if let Some(scope) = &urls.scope {
            form.append_pair("scope", scope);
        }
        if let Some(referrer) = flavor.referrer() {
            form.append_pair("referrer", referrer);
        }
        form.finish()
    };
    let start = post_form_with(http, family, &urls.usercode, &start_body, &urls.headers).await?;
    if !(200..300).contains(&start.status) {
        return Err(ProviderError::DeviceCode {
            status: start.status,
            message: error_message(&start.body, &[]),
        });
    }
    let invalid = || ProviderError::DeviceCode {
        status: start.status,
        message: String::from("invalid device-code response"),
    };
    let device = decode_json::<DeviceAuthorization>(&start.body).ok_or_else(invalid)?;
    let shown_url = device.shown_url(flavor, urls).ok_or_else(invalid)?;
    report_progress(
        progress,
        LoginProgress::ShowCode {
            url: shown_url,
            code: device.user_code.clone(),
        },
        cancel,
    )?;

    let poll_deadline = flavor
        .lifetime(device.expires_in)
        .and_then(|secs| Instant::now().checked_add(Duration::from_secs(secs)))
        .map_or(deadline, |expiry| expiry.min(deadline));
    let poll_body = {
        let mut form = url::form_urlencoded::Serializer::new(String::new());
        form.append_pair("grant_type", "urn:ietf:params:oauth:grant-type:device_code")
            .append_pair("client_id", client_id)
            .append_pair("device_code", &device.device_code);
        form.finish()
    };
    let mut interval = flavor.initial_interval(device.interval);
    if flavor.waits_before_first_poll() {
        wait_for_next_poll(poll_deadline, interval, cancel).await?;
    }
    loop {
        if cancel.is_cancelled() {
            return Err(ProviderError::LoginCancelled);
        }
        let poll = timeout_at(
            poll_deadline,
            post_form_with(http, family, &urls.poll, &poll_body, &urls.headers),
        )
        .await
        .map_err(|_| ProviderError::LoginTimeout)??;
        if flavor.fails_on_server_error() && poll.status >= 500 {
            return Err(ProviderError::DeviceCode {
                status: poll.status,
                message: format!("device token request failed with status {}", poll.status),
            });
        }
        let answer = decode_json::<PollError>(&poll.body);
        let (error, named) = answer.map_or((None, None), |body| (body.error, body.interval));
        if (200..300).contains(&poll.status) && error.is_none() {
            return flavor.delivery(poll.status, &poll.body);
        }
        match error.as_deref() {
            Some("authorization_pending") => {}
            Some("slow_down") => {
                interval = flavor.slowed(interval, named_interval(named.as_ref()));
            }
            Some("expired_token") => return Err(flavor.expired(poll.status)),
            Some(code) if flavor.denies(code) => {
                return Err(flavor.denied(family, poll.status));
            }
            _ => {
                return Err(ProviderError::DeviceCode {
                    status: poll.status,
                    message: error_message(&poll.body, &[&device.device_code, &device.user_code]),
                });
            }
        }
        wait_for_next_poll(poll_deadline, interval, cancel).await?;
    }
}

async fn wait_for_next_poll(
    deadline: Instant,
    interval: Duration,
    cancel: &CancellationToken,
) -> Result<(), ProviderError> {
    if Instant::now() >= deadline {
        return Err(ProviderError::LoginTimeout);
    }
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(ProviderError::LoginCancelled),
        () = sleep_until(deadline) => Err(ProviderError::LoginTimeout),
        () = sleep(interval) => Ok(()),
    }
}

#[cfg(test)]
mod tests;
