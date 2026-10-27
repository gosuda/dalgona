//! The Cursor sign-in steps the built-in `cursor` row's
//! [`Hook::CursorPoll`](crate::Hook::CursorPoll) dispatches to: the
//! deep-control poll grant and the refresh answer.
//!
//! Coded against the oh-my-pi pin `a4dc3302`
//! (`packages/ai/src/registry/oauth/cursor.ts`, verified on disk under
//! `.references/oh-my-pi`), which the Cursor family module already follows
//! for the wire. The senpi pin agrees on the login URL, the poll endpoint,
//! and the poll pacing, and diverges only on the refresh endpoint: it posts
//! an empty JSON body to `/auth/exchange_user_api_key` with the refresh
//! token as a bearer header. The oh-my-pi pin governs here, as it does for
//! the family's wire.
//!
//! The grant has no loopback redirect, no client id on the authorize step,
//! and no `state`: the browser approves server-side and the CLI releases the
//! tokens by polling with the per-attempt `uuid` plus the PKCE `verifier`,
//! which are the only proof of the grant. The login URL carries
//! `challenge`, `uuid`, `mode=login`, and `redirectTarget=cli`, in that
//! order. The poll answers `404` while approval is pending, `200` with
//! `{ accessToken, refreshToken }` once the user approves, and the poll
//! retries transport failures and other statuses up to three consecutive
//! times before failing. The stored access token is a JWT: its `exp` claim
//! minus a five-minute skew is the expiry, with a one-hour fallback when the
//! claim is unreadable.
//!
//! Both steps ride the shared OAuth HTTP owner: the private no-redirect
//! client, bounded bodies, URL-free transport errors, and server messages
//! scrubbed of the verifier before they enter a typed error. The refresh
//! request itself is the shared refresher's JSON grant
//! (`grant_type`, `client_id`, `refresh_token`); only its answer is
//! Cursor's own, read by [`refreshed`].

use std::time::Duration;

use base64::{
    Engine,
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;

use dal_core::Family;

use crate::{
    OAuthCredential, ProviderError, SecretString,
    auth::oauth::{
        OAuthHttp, error_message, get_json, new_pkce_verifier, pkce_challenge, unix_now,
    },
};

/// The token release endpoint the poll asks; the loopback test seam moves it
/// with the row's other endpoints.
pub(crate) const POLL_URL: &str = "https://api2.cursor.sh/auth/poll";

/// The wait before the first poll and the growth base of the backoff.
pub(crate) const POLL_BASE_DELAY: Duration = Duration::from_secs(1);

/// The longest wait one poll attempt backs off to.
pub(crate) const POLL_MAX_DELAY: Duration = Duration::from_secs(10);

/// The multiplier one pending poll applies to the previous wait.
pub(crate) const POLL_BACKOFF_MULTIPLIER: f64 = 1.2;

/// The poll attempts one sign-in allows, roughly the reference's wall-clock
/// budget once the wait has reached its cap.
pub(crate) const POLL_MAX_ATTEMPTS: u32 = 150;

/// The consecutive failed attempts (transport errors and non-`404` statuses)
/// one poll tolerates before failing. A pending `404` resets the count.
pub(crate) const POLL_MAX_CONSECUTIVE_ERRORS: u32 = 3;

/// The seconds an expiry claim is pulled forward, so a token cannot die
/// mid-request.
pub(crate) const EXPIRY_SKEW_SECS: i64 = 300;

/// The fallback lifetime of an access token whose `exp` claim is unreadable.
pub(crate) const DEFAULT_LIFETIME_SECS: i64 = 3600;

/// The poll pacing of one sign-in. Production uses the pinned values; the
/// loopback tests shrink them so a pending-then-approved poll runs in
/// milliseconds.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PollPacing {
    /// The wait before the first poll.
    pub(crate) base: Duration,
    /// The longest wait one attempt backs off to.
    pub(crate) max: Duration,
    /// The multiplier one pending poll applies to the previous wait.
    pub(crate) multiplier: f64,
    /// The attempts one sign-in allows.
    pub(crate) max_attempts: u32,
    /// The consecutive failed attempts one poll tolerates.
    pub(crate) max_consecutive_errors: u32,
}

impl Default for PollPacing {
    fn default() -> Self {
        Self {
            base: POLL_BASE_DELAY,
            max: POLL_MAX_DELAY,
            multiplier: POLL_BACKOFF_MULTIPLIER,
            max_attempts: POLL_MAX_ATTEMPTS,
            max_consecutive_errors: POLL_MAX_CONSECUTIVE_ERRORS,
        }
    }
}

/// The per-attempt proof of one poll grant: the PKCE pair and the `uuid` the
/// login URL names.
#[derive(Clone, Debug)]
pub(crate) struct Prompt {
    /// The PKCE verifier; the poll sends it and the refresh never does.
    pub(crate) verifier: String,
    /// The `uuid` the login URL carries and the poll answers for.
    pub(crate) uuid: String,
    /// The challenge derived from the verifier.
    pub(crate) challenge: String,
}

impl Prompt {
    /// Mints one prompt: a fresh `uuid` and a fresh PKCE pair, drawn by the
    /// same generator every other sign-in uses.
    #[must_use]
    pub(crate) fn new() -> Self {
        let verifier = new_pkce_verifier();
        let challenge = pkce_challenge(&verifier);
        Self {
            verifier,
            uuid: Uuid::new_v4().to_string(),
            challenge,
        }
    }

    /// The login URL the browser opens: `page` (the row's authorize URL)
    /// with the reference's query, in its parameter order.
    #[must_use]
    pub(crate) fn login_url(&self, page: &Url) -> Url {
        let mut url = page.clone();
        url.query_pairs_mut()
            .clear()
            .append_pair("challenge", &self.challenge)
            .append_pair("uuid", &self.uuid)
            .append_pair("mode", "login")
            .append_pair("redirectTarget", "cli");
        url
    }
}

/// The token release the poll answers with once the user approves.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PollTokens {
    access_token: SecretString,
    refresh_token: SecretString,
}

/// The refresh answer. Cursor keeps the stored refresh token unless it
/// rotates one, and ends sessions it will not renew with a `200` that carries
/// an empty token and `shouldLogout`.
#[derive(Deserialize)]
struct RefreshResponse {
    #[serde(default)]
    access_token: Option<SecretString>,
    #[serde(default)]
    refresh_token: Option<SecretString>,
    #[serde(default, rename = "shouldLogout")]
    should_logout: Option<bool>,
}

/// Polls the token release endpoint `url` until the user approves the
/// browser request, and returns the credential to store.
///
/// The state machine is the reference's: every attempt sleeps first, a
/// pending answer is `404` (it resets the error budget and backs the wait
/// off), an approval is a `2xx` with both tokens, and every other status,
/// unreadable approval, or transport failure burns one of the
/// consecutive-error attempts without growing the wait. Cancellation always
/// wins the race and reports [`ProviderError::LoginCancelled`]; exhausted
/// attempts report [`ProviderError::LoginTimeout`].
///
/// # Errors
///
/// Returns [`ProviderError::LoginCancelled`] on cancellation,
/// [`ProviderError::LoginTimeout`] after `pacing.max_attempts` attempts, the
/// transport error after `pacing.max_consecutive_errors` consecutive
/// failures that ended in one, and [`ProviderError::TokenExchange`] when the
/// last failed attempt carried an HTTP status; its message is scrubbed of
/// the verifier.
pub(crate) async fn poll_tokens(
    http: &OAuthHttp<'_>,
    url: &Url,
    prompt: &Prompt,
    pacing: PollPacing,
    cancel: &CancellationToken,
) -> Result<OAuthCredential, ProviderError> {
    let mut url = url.clone();
    url.query_pairs_mut()
        .clear()
        .append_pair("uuid", &prompt.uuid)
        .append_pair("verifier", &prompt.verifier);
    let mut delay = pacing.base;
    let mut errors = 0_u32;
    for _ in 0..pacing.max_attempts {
        tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ProviderError::LoginCancelled),
            () = tokio::time::sleep(delay) => {}
        }
        let answer = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ProviderError::LoginCancelled),
            answer = get_json(http, Family::Cursor, &url) => answer,
        };
        let response = match answer {
            Ok(response) => response,
            Err(error) => {
                errors += 1;
                if errors >= pacing.max_consecutive_errors {
                    return Err(error);
                }
                continue;
            }
        };
        if response.status == 404 {
            errors = 0;
            // The multiplier is the pinned 1.2 and the base is positive, so
            // the growth cannot fail; the cap keeps a stray multiplier from
            // running the wait away.
            delay = delay.mul_f64(pacing.multiplier.max(1.0)).min(pacing.max);
            continue;
        }
        if (200..300).contains(&response.status)
            && let Ok(tokens) = sonic_rs::from_slice::<PollTokens>(&response.body)
            && !tokens.access_token.expose().is_empty()
            && !tokens.refresh_token.expose().is_empty()
        {
            return Ok(credential(tokens.access_token, tokens.refresh_token));
        }
        errors += 1;
        if errors >= pacing.max_consecutive_errors {
            return Err(ProviderError::TokenExchange {
                status: response.status,
                message: error_message(&response.body, &[prompt.verifier.as_str()]),
            });
        }
    }
    Err(ProviderError::LoginTimeout)
}

/// The credential a successful (`2xx`) refresh answer `body` stores for
/// `provider`, refreshed at `now`.
///
/// A `shouldLogout` answer means Cursor ended the session and only a fresh
/// sign-in recovers it. A rotated refresh token replaces the stored one; an
/// answer that names none keeps `stored`'s, as the Cursor IDE does. The
/// expiry is the new access token's own.
///
/// # Errors
///
/// [`ProviderError::SignInExpired`] for an ended session, and
/// [`ProviderError::Transport`] for an answer that is not the token shape or
/// carries no access token; the parser message is dropped because the body
/// holds tokens.
pub(crate) fn refreshed(
    provider: &str,
    stored: &OAuthCredential,
    body: &[u8],
    now: i64,
) -> Result<OAuthCredential, ProviderError> {
    let invalid = |detail: &str| ProviderError::Transport {
        family: Family::Cursor,
        reason: format!("the token endpoint answered with an invalid body: {detail}"),
    };
    let answer = sonic_rs::from_slice::<RefreshResponse>(body)
        .map_err(|_| invalid("not the token response shape"))?;
    if answer.should_logout == Some(true) {
        return Err(ProviderError::SignInExpired {
            provider: provider.to_owned(),
        });
    }
    let access_token = answer
        .access_token
        .filter(|token| !token.expose().is_empty())
        .ok_or_else(|| invalid("the access token is empty"))?;
    let refresh_token = answer
        .refresh_token
        .filter(|token| !token.expose().is_empty())
        .unwrap_or_else(|| stored.refresh_token.clone());
    Ok(OAuthCredential {
        expires_at: Some(token_expires_at(access_token.expose(), now)),
        access_token,
        refresh_token,
        id_token: None,
        account_id: None,
        project_id: None,
    })
}

/// Builds the stored credential of a completed poll grant: the token pair
/// and the access token's own expiry.
#[must_use]
pub(crate) fn credential(
    access_token: SecretString,
    refresh_token: SecretString,
) -> OAuthCredential {
    OAuthCredential {
        expires_at: Some(token_expires_at(access_token.expose(), unix_now())),
        access_token,
        refresh_token,
        id_token: None,
        account_id: None,
        project_id: None,
    }
}

/// The expiry of a Cursor access token in Unix seconds: the JWT `exp` claim
/// pulled forward by the skew, or one hour from `now` when the claim is
/// unreadable.
#[must_use]
pub(crate) fn token_expires_at(access_token: &str, now: i64) -> i64 {
    jwt_exp_secs(access_token).map_or(now + DEFAULT_LIFETIME_SECS, |exp| exp - EXPIRY_SKEW_SECS)
}

/// The `exp` claim of a JWT in Unix seconds, padded or unpadded base64url.
fn jwt_exp_secs(token: &str) -> Option<i64> {
    let payload = token.split('.').nth(1)?;
    for engine in [&URL_SAFE_NO_PAD, &URL_SAFE] {
        if let Ok(bytes) = engine.decode(payload)
            && let Ok(claims) = sonic_rs::from_slice::<JwtClaims>(&bytes)
            && let Some(exp) = claims.exp
        {
            return Some(exp);
        }
    }
    None
}

#[derive(Deserialize)]
struct JwtClaims {
    #[serde(default)]
    exp: Option<i64>,
}

#[cfg(test)]
mod tests;
