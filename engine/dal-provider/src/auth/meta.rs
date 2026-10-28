//! The Meta Muse Code key mint.
//!
//! Meta splits identity from API access: the device grant answers an identity
//! token that the Model API does not accept, and the Muse Code key endpoint
//! exchanges it for an API key that lives about a day. The identity token is
//! stored as the credential's refresh token and the minted key as its access
//! token, so the ordinary refresh scheduler re-mints the key before it
//! expires. The identity token itself is not renewable (the issuer answers a
//! refresh grant with 404 and issues no refresh token), so a `401` or `403`
//! from the mint means the session is dead and only a fresh sign-in helps.
//!
//! One request shape serves the sign-in and every renewal: `POST` to the
//! row's token endpoint with the identity token as a bearer, the pinned
//! `x-api-version`, and an empty JSON object. A failed mint stores nothing,
//! so the previous credential stays byte-identical.

use reqwest::RequestBuilder;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{OAuthCredential, ProviderError, SecretString, auth::oauth::error_message};

/// The pinned `x-api-version` the mint requires.
const API_VERSION: &str = "1.0.0";

/// How long a minted key lives: the pinned 24 hours. The mint answers no
/// lifetime, so the stored expiry counts from the successful answer.
pub(crate) const KEY_LIFETIME_SECS: i64 = 24 * 60 * 60;

/// The Muse Code key mint body: an empty JSON object. A struct, not a raw
/// literal, so the request shape is typed at the call site.
#[derive(Serialize)]
struct MetaMintBody {}

/// The mint request for `identity_token`, to the row's mint endpoint `url`.
/// The identity token rides a sensitive bearer header and the body is the
/// typed [`MetaMintBody`]. The caller adds the `User-Agent` and `Accept`
/// its HTTP layer owns.
pub(crate) fn request(client: &reqwest::Client, url: &Url, identity_token: &str) -> RequestBuilder {
    client
        .post(url.clone())
        .bearer_auth(identity_token)
        .header("x-api-version", API_VERSION)
        .json(&MetaMintBody {})
}

/// What a mint answered, decided from its status and body alone.
#[derive(Debug)]
pub(crate) enum Outcome {
    /// The mint issued a key.
    Key(SecretString),
    /// `401` or `403`: the identity token no longer names a session.
    SessionExpired,
    /// The mint refused or failed in a way another attempt may cure.
    Refused(ProviderError),
}

#[derive(Deserialize)]
struct Minted {
    #[serde(default)]
    api_key: Option<SecretString>,
    #[serde(default)]
    action_url: Option<String>,
}

/// Classifies one mint answer. `family` names the provider in typed errors;
/// every message is redacted of each value in `secrets` (the identity token
/// and, on a renewal, the key being replaced) and bounded to one line.
pub(crate) fn outcome(
    family: dal_core::Family,
    status: u16,
    body: &[u8],
    secrets: &[&str],
) -> Outcome {
    if matches!(status, 401 | 403) {
        return Outcome::SessionExpired;
    }
    if !(200..300).contains(&status) {
        return Outcome::Refused(ProviderError::Status {
            family,
            status,
            message: error_message(body, secrets),
        });
    }
    let Some(minted) = crate::auth::oauth::decode_json::<Minted>(body) else {
        return Outcome::Refused(ProviderError::TokenExchange {
            status,
            message: String::from("invalid mint response"),
        });
    };
    if let Some(key) = minted.api_key.filter(|key| !key.expose().is_empty()) {
        Outcome::Key(key)
    } else {
        let setup = minted
            .action_url
            .as_deref()
            .and_then(trusted_action_url)
            .map(|url| format!(" Complete setup at {url}"))
            .unwrap_or_default();
        Outcome::Refused(ProviderError::TokenExchange {
            status,
            message: format!("Meta did not issue an API key.{setup}"),
        })
    }
}

/// The setup page a mint may name, when it is a plain `https` URL that
/// carries no credentials: it is shown to the user, never fetched.
fn trusted_action_url(raw: &str) -> Option<String> {
    let url = Url::parse(raw).ok()?;
    (url.scheme() == "https"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none())
    .then(|| url.to_string())
}

/// The credential a successful mint stores: the minted key as the access
/// token, the identity token kept as the renewal secret, and the pinned
/// lifetime counted from `now`.
pub(crate) fn credential(
    key: SecretString,
    identity_token: SecretString,
    now: i64,
) -> OAuthCredential {
    OAuthCredential {
        access_token: key,
        refresh_token: identity_token,
        expires_at: Some(now.saturating_add(KEY_LIFETIME_SECS)),
        id_token: None,
        account_id: None,
        project_id: None,
    }
}

#[cfg(test)]
mod tests;
