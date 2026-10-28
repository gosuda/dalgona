//! The Devin CLI sign-in exchange the built-in `devin` row's
//! [`Hook::DevinCliToken`](crate::Hook::DevinCliToken) dispatches to.
//!
//! Coded against the pins on disk: the oh-my-pi compiled auth rule for
//! `devin` (`packages/catalog/src/compat/rules.json`, from
//! `auth/devin.kdl`) and the senpi implementation
//! (`packages/ai/src/auth/oauth/devin.ts`, `devin-token.ts`,
//! `devin-callback.ts`), which agree. The browser half is the generic PKCE
//! flow driven by the row: the authorize page with an S256 challenge, a
//! per-attempt `state`, and `prompt=select_account`, no `client_id` (the
//! vendor registers none), a callback on `127.0.0.1:59653/callback`, and a
//! pasted code or `code#state` when the browser is on another machine. Only
//! the exchange is Devin's own and deliberately non-standard: a JSON body of
//! only `code` and `code_verifier`, answered with `{ "token": <jwt> }`. The
//! one token is both the stored access and refresh credential, its expiry is
//! the JWT `exp` claim minus a five-minute skew, and the fallback when the
//! claim is unreadable is the reference's one-year window. There is no
//! refresh grant: Devin re-issues tokens through a fresh sign-in.
//!
//! The token is spent against the Cascade model host (`server.codeium.com`),
//! never against `api.devin.ai`: that host is the login host only, so the
//! stored credential carries no endpoint override.

use base64::{
    Engine,
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
};
use serde::{Deserialize, Serialize};
use url::Url;

use dal_core::Family;

use crate::{
    OAuthCredential, ProviderError, SecretString,
    auth::oauth::{OAuthHttp, post_json, token_or_exchange_error, unix_now},
};

/// The seconds an expiry claim is pulled forward, so a token cannot die
/// mid-request.
pub(crate) const EXPIRY_SKEW_SECS: i64 = 300;

/// The fallback lifetime of a token whose `exp` claim is unreadable: the
/// reference's one-year window for Devin's long-lived CLI tokens.
pub(crate) const FALLBACK_LIFETIME_SECS: i64 = 31_536_000;

/// The exchange answer: one token serving as both access and refresh
/// credential.
#[derive(Deserialize)]
struct TokenAnswer {
    token: SecretString,
}

/// The exchange request body: the code and the PKCE verifier, and nothing
/// else — no `grant_type`, no `client_id`, no redirect.
#[derive(Serialize)]
struct ExchangeBody<'a> {
    code: &'a str,
    code_verifier: &'a str,
}

/// Exchanges one authorization code for the CLI token at the row's token
/// endpoint `url`.
///
/// The POST carries exactly `{"code": …, "code_verifier": …}` as JSON, with
/// `Accept` and `Content-Type` `application/json`, through the shared OAuth
/// client that follows no redirect: a redirect must never carry an
/// authorization code or verifier to another host. A refusal's server
/// message is scrubbed of `secrets` and bounded before it enters the error.
///
/// # Errors
///
/// Returns [`ProviderError::TokenExchange`] for a non-`2xx` answer, an
/// unparseable answer, or an answer with an empty token, and the transport
/// errors of the shared OAuth client.
pub(crate) async fn exchange_code(
    http: &OAuthHttp<'_>,
    url: &Url,
    code: &str,
    verifier: &str,
    secrets: &[&str],
) -> Result<OAuthCredential, ProviderError> {
    let body = ExchangeBody {
        code,
        code_verifier: verifier,
    };
    let response = post_json(http, Family::Devin, url, &body).await?;
    let answer = token_or_exchange_error::<TokenAnswer>(&response, secrets)?;
    if answer.token.expose().is_empty() {
        return Err(ProviderError::TokenExchange {
            status: response.status,
            message: String::from("invalid token response"),
        });
    }
    Ok(credential(answer.token))
}

/// Builds the stored credential of a completed exchange: one token as both
/// the access and the refresh credential, with the token's own expiry.
#[must_use]
pub(crate) fn credential(token: SecretString) -> OAuthCredential {
    OAuthCredential {
        expires_at: Some(token_expires_at(token.expose(), unix_now())),
        access_token: token.clone(),
        refresh_token: token,
        id_token: None,
        account_id: None,
        project_id: None,
    }
}

/// The expiry of a CLI token in Unix seconds: the JWT `exp` claim pulled
/// forward by the skew, or one year from `now` when the claim is unreadable.
#[must_use]
pub(crate) fn token_expires_at(token: &str, now: i64) -> i64 {
    jwt_exp_secs(token).map_or(now + FALLBACK_LIFETIME_SECS, |exp| exp - EXPIRY_SKEW_SECS)
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
