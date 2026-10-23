//! The device-code sign-in, also the fallback for hosts that cannot bind the
//! browser callback.
//!
//! Polling stays here so the authorization flow owns one deadline and one
//! cancellation token. The Codex JSON device flow ends in a grant for the
//! ordinary token exchange; the `WorkOS` form device flow (RFC 8628) delivers
//! the tokens directly, with no second exchange. Polling owns no persistence.

use std::time::Duration;

use dal_core::Family;
use serde::Deserialize;
use tokio::time::{Instant, sleep, sleep_until, timeout_at};
use tokio_util::sync::CancellationToken;

use crate::{Hook, OAuthCredential, ProviderError, SecretString};

use super::credential::oauth_expires_at;
use super::oauth::{
    DeviceUrls, LoginProgress, OAuthHttp, decode_json, error_message, post_form, post_json,
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
/// exchange (Codex), or the tokens themselves (`WorkOS`, whose device poll
/// completes the grant with no second exchange).
pub(crate) enum DeviceOutcome {
    Grant(DeviceGrant),
    Tokens(OAuthCredential),
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
/// [`Hook::WorkOsDevice`] runs the form-encoded RFC 8628 device flow and
/// returns [`DeviceOutcome::Tokens`] directly, with no second exchange. The
/// remaining hooks offer no device sign-in, so reaching them is a caller bug.
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
        Hook::WorkOsDevice => run_workos(http, family, client_id, urls, deadline, progress, cancel)
            .await
            .map(DeviceOutcome::Tokens),
        Hook::ClaudeCode | Hook::GitLabDuo => Err(ProviderError::AuthWrite {
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
            url: urls.page.as_str().to_owned(),
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

/// The extra wait added for every `slow_down` poll answer (RFC 8628 §3.5).
const SLOW_DOWN_INCREMENT: Duration = Duration::from_secs(5);

/// The `WorkOS` device authorization response (RFC 8628 §3.2): the code pair,
/// the verification URLs, the poll cadence, and the code lifetime. The field
/// names are the compiled `login "device-code"` response paths.
#[derive(Deserialize)]
struct WorkOsDeviceCodeResponse {
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

/// A `WorkOS` poll answer carrying an RFC 8628 error code.
#[derive(Deserialize)]
struct WorkOsPollError {
    #[serde(default)]
    error: Option<String>,
}

/// The `WorkOS` token delivery: the access and refresh grants plus an optional
/// lifetime. There is no ID token and no account id; the shape mirrors the
/// existing token responses without the Codex-only `id_token`.
#[derive(Deserialize)]
struct WorkOsTokenResponse {
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    expires_in: Option<i64>,
}

/// Request and poll a `WorkOS` device code until the grant completes,
/// cancellation, or timeout.
///
/// The start and poll bodies are form-encoded. A 2xx poll answer without an
/// error code delivers the tokens directly. `authorization_pending` waits the
/// server interval, `slow_down` adds five seconds per answer, `expired_token`
/// times out, and `access_denied` is a typed denial. Anything else is a
/// device-code failure with the device codes redacted. Polling stops at the
/// earlier of `deadline` and the device response lifetime; an in-flight poll
/// past the device lifetime times out without cancelling the caller.
#[expect(
    clippy::too_many_lines,
    reason = "RFC 8628 form polling loop owns interval backoff, deadline, and cancellation"
)]
async fn run_workos(
    http: &OAuthHttp<'_>,
    family: Family,
    client_id: &str,
    urls: &DeviceUrls,
    deadline: Instant,
    progress: &(dyn Fn(LoginProgress) + Send + Sync),
    cancel: &CancellationToken,
) -> Result<OAuthCredential, ProviderError> {
    let start_body = {
        let mut form = url::form_urlencoded::Serializer::new(String::new());
        form.append_pair("client_id", client_id);
        form.finish()
    };
    let start = post_form(http, family, &urls.usercode, &start_body).await?;
    if !(200..300).contains(&start.status) {
        return Err(ProviderError::DeviceCode {
            status: start.status,
            message: error_message(&start.body, &[]),
        });
    }
    let device = decode_json::<WorkOsDeviceCodeResponse>(&start.body).ok_or_else(|| {
        ProviderError::DeviceCode {
            status: start.status,
            message: String::from("invalid device-code response"),
        }
    })?;
    if device.device_code.is_empty() || device.user_code.is_empty() {
        return Err(ProviderError::DeviceCode {
            status: start.status,
            message: String::from("invalid device-code response"),
        });
    }
    let shown_url = device
        .verification_uri_complete
        .as_deref()
        .filter(|uri| !uri.is_empty())
        .or_else(|| {
            device
                .verification_uri
                .as_deref()
                .filter(|uri| !uri.is_empty())
        })
        .unwrap_or_else(|| urls.page.as_str())
        .to_owned();
    report_progress(
        progress,
        LoginProgress::ShowCode {
            url: shown_url,
            code: device.user_code.clone(),
        },
        cancel,
    )?;

    let poll_deadline = device
        .expires_in
        .and_then(|secs| Instant::now().checked_add(Duration::from_secs(secs)))
        .map_or(deadline, |expiry| expiry.min(deadline));
    let poll_body = {
        let mut form = url::form_urlencoded::Serializer::new(String::new());
        form.append_pair("grant_type", "urn:ietf:params:oauth:grant-type:device_code")
            .append_pair("client_id", client_id)
            .append_pair("device_code", &device.device_code);
        form.finish()
    };
    let mut interval = Duration::from_secs(device.interval.max(1));
    loop {
        if cancel.is_cancelled() {
            return Err(ProviderError::LoginCancelled);
        }
        let poll = timeout_at(
            poll_deadline,
            post_form(http, family, &urls.poll, &poll_body),
        )
        .await
        .map_err(|_| ProviderError::LoginTimeout)??;
        let error = decode_json::<WorkOsPollError>(&poll.body).and_then(|body| body.error);
        if (200..300).contains(&poll.status) && error.is_none() {
            let token = decode_json::<WorkOsTokenResponse>(&poll.body).ok_or_else(|| {
                ProviderError::TokenExchange {
                    status: poll.status,
                    message: String::from("invalid token response"),
                }
            })?;
            if token.access_token.is_empty() || token.refresh_token.is_empty() {
                return Err(ProviderError::TokenExchange {
                    status: poll.status,
                    message: String::from("invalid token response"),
                });
            }
            return Ok(OAuthCredential {
                expires_at: oauth_expires_at(unix_now(), token.expires_in, &token.access_token),
                access_token: SecretString::from(token.access_token),
                refresh_token: SecretString::from(token.refresh_token),
                id_token: None,
                account_id: None,
            });
        }
        match error.as_deref() {
            Some("authorization_pending") => {}
            Some("slow_down") => {
                interval = interval.saturating_add(SLOW_DOWN_INCREMENT);
            }
            Some("expired_token") => return Err(ProviderError::LoginTimeout),
            Some("access_denied") => {
                return Err(ProviderError::Status {
                    family,
                    status: poll.status,
                    message: String::from("device authorization was denied"),
                });
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
