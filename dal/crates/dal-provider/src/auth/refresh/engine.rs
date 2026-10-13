//! The serialized refresh engine: one refresher per `auth.json`.
//!
//! The commit is the last step; its cancellation check is the linearization
//! point. All file I/O runs on the blocking pool through the sibling lock.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use sonic_rs::JsonValueTrait;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use super::{
    PERMANENT_CODES, PROACTIVE_WINDOW_SECS, RETRY_DELAY, blocking,
    endpoints::{OAuthProvider, RefreshReason, TokenEndpoints},
    lock_auth_file,
};
use crate::{
    auth::{
        credential::{
            AuthStore, Credential, OAuthCredential, SecretString, codex_identity, oauth_expires_at,
        },
        oauth::unix_now,
    },
    error::ProviderError,
    http::{CONNECT_TIMEOUT, Exchange, OAUTH_TIMEOUT, read_body, send},
};

/// Refreshes OAuth credentials stored in one `auth.json`, at most one refresh
/// per credential key at a time across every refresher of that file.
///
/// A host keeps one `Refresher` per `auth.json`; dropping it drops its
/// mutexes. It holds no secret itself.
#[derive(Debug)]
pub struct Refresher {
    client: reqwest::Client,
    user_agent: Box<str>,
    auth_path: PathBuf,
    endpoints: TokenEndpoints,
    anthropic: Arc<Mutex<()>>,
    openai_codex: Arc<Mutex<()>>,
}

impl Refresher {
    /// A refresher for the `auth.json` at `auth_path` that sends refresh
    /// requests with `user_agent` to `endpoints`.
    ///
    /// It builds its own client, which follows no redirect: a refresh body
    /// carries the refresh token, and reqwest resends a body on a 307 or 308
    /// to whatever host the redirect names. A 3xx answer is a
    /// [`ProviderError::Status`] instead.
    ///
    /// # Panics
    ///
    /// Panics when reqwest cannot initialize its TLS backend, which only a
    /// broken build configuration causes.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "the builder fails only when the compiled TLS backend cannot initialize"
    )]
    pub fn new(
        user_agent: impl Into<Box<str>>,
        auth_path: impl Into<PathBuf>,
        endpoints: TokenEndpoints,
    ) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .referer(false)
            .retry(reqwest::retry::never())
            .build()
            .expect("the reqwest TLS backend initializes");
        Self {
            client,
            user_agent: user_agent.into(),
            auth_path: auth_path.into(),
            endpoints,
            anthropic: Arc::new(Mutex::new(())),
            openai_codex: Arc::new(Mutex::new(())),
        }
    }

    /// The `auth.json` path this refresher reloads and writes.
    #[must_use]
    pub fn auth_path(&self) -> &Path {
        &self.auth_path
    }

    /// Returns the credential to use in place of `held`, refreshing it when
    /// needed.
    ///
    /// With [`RefreshReason::Expiring`] and `held` outside the proactive
    /// window (or without expiry) this returns `held` at once, taking no lock
    /// and touching no file. Otherwise it takes the per-key mutex and then
    /// `auth.json.lock`, reloads `auth.json`, and returns the stored
    /// credential when its access token differs from `held` or, for
    /// `Expiring`, when it is outside the window. Only then does it send the
    /// refresh request (at most two attempts) and commit the new tokens with
    /// one atomic rename before returning them. A stored entry that is no
    /// longer an OAuth sign-in (an API key) is returned as is.
    ///
    /// Cancelling the returned future before the commit check drops any
    /// in-flight request, releases both locks, and leaves `auth.json`
    /// unchanged, even when the token endpoint already answered. A commit
    /// already past its check finishes its one atomic rename, then both
    /// locks release.
    ///
    /// # Errors
    ///
    /// - [`ProviderError::SignInExpired`] when the token endpoint rejects the
    ///   refresh token (`invalid_grant`, `refresh_token_expired`,
    ///   `refresh_token_reused`, `refresh_token_invalidated`); sent once.
    /// - [`ProviderError::Transport`] or [`ProviderError::Status`] after a
    ///   transient failure (transport, 408, 429, 5xx) repeated on the retry,
    ///   or at once for another non-success status or an invalid token body.
    /// - [`ProviderError::NoCredentials`] when `auth.json` no longer has the
    ///   entry.
    /// - The [`AuthStore::load`] errors for the reloaded file, and
    ///   [`ProviderError::AuthWrite`] when `auth.json.lock` cannot be opened,
    ///   stays held by another process past the lock wait, or the commit
    ///   fails (the previous file then stays in place).
    /// - [`ProviderError::PlainHttp`] and [`ProviderError::Limit`] from the
    ///   HTTP layer, without retry.
    pub async fn refresh(
        &self,
        provider: OAuthProvider,
        held: &OAuthCredential,
        reason: RefreshReason,
    ) -> Result<Credential, ProviderError> {
        if reason == RefreshReason::Expiring && !expiring(held, unix_now()) {
            return Ok(Credential::OAuth(held.clone()));
        }
        let cancel = CancellationToken::new();
        // Fires when this future is dropped, cancelling a commit not yet begun.
        let _cancel_on_drop = cancel.clone().drop_guard();
        let key = Arc::clone(self.key_mutex(provider)).lock_owned().await;
        let file = lock_auth_file(&self.auth_path).await?;
        let path = self.auth_path.clone();
        let mut store = blocking(move || AuthStore::load(path)).await?;
        let stored = match store.credential(provider.id()) {
            Some(Credential::OAuth(stored)) => stored,
            Some(other @ Credential::ApiKey { .. }) => return Ok(other),
            Some(Credential::None) | None => {
                return Err(ProviderError::NoCredentials {
                    provider: provider.id().to_owned(),
                });
            }
        };
        if stored.access_token != held.access_token
            || (reason == RefreshReason::Expiring && !expiring(&stored, unix_now()))
        {
            return Ok(Credential::OAuth(stored));
        }
        let fresh = self.exchange(provider, &stored).await?;
        let committed = Credential::OAuth(fresh.clone());
        let id = provider.id();
        blocking(move || {
            // The guards live until the commit ends or is refused.
            let _locks = (key, file);
            commit(&cancel, &mut store, id, committed)
        })
        .await?;
        tracing::debug!(provider = id, "stored refreshed OAuth tokens");
        Ok(Credential::OAuth(fresh))
    }

    const fn key_mutex(&self, provider: OAuthProvider) -> &Arc<Mutex<()>> {
        match provider {
            OAuthProvider::Anthropic => &self.anthropic,
            OAuthProvider::OpenAiCodex => &self.openai_codex,
        }
    }
}
impl Refresher {
    /// Sends the refresh request, retrying once after [`RETRY_DELAY`] on a
    /// transient failure.
    async fn exchange(
        &self,
        provider: OAuthProvider,
        stored: &OAuthCredential,
    ) -> Result<OAuthCredential, ProviderError> {
        let body = sonic_rs::to_vec(&RefreshBody {
            grant_type: "refresh_token",
            client_id: provider.client_id(),
            refresh_token: stored.refresh_token.expose(),
        })
        .map_err(|_| ProviderError::Transport {
            family: provider.family(),
            reason: String::from("could not encode the token refresh request"),
        })?;
        let mut retried = false;
        loop {
            match self.attempt(provider, stored, body.clone()).await {
                Ok(fresh) => return Ok(fresh),
                Err(Failure::Transient(_)) if !retried => {
                    retried = true;
                    tracing::debug!(
                        provider = provider.id(),
                        "token refresh failed transiently; retrying once"
                    );
                    tokio::time::sleep(RETRY_DELAY).await;
                }
                Err(Failure::Transient(error) | Failure::Final(error)) => return Err(error),
            }
        }
    }

    /// One refresh request and the classification of its outcome.
    async fn attempt(
        &self,
        provider: OAuthProvider,
        stored: &OAuthCredential,
        body: Vec<u8>,
    ) -> Result<OAuthCredential, Failure> {
        let family = provider.family();
        let request = self
            .client
            .post(self.endpoints.url(provider).clone())
            .body(body);
        let exchange = Exchange::Json {
            total: OAUTH_TIMEOUT,
        };
        let response = send(
            family,
            request,
            &self.user_agent,
            exchange,
            tokio::time::sleep,
        )
        .await
        .map_err(Failure::from_http)?;
        let status = response.status();
        let bytes = read_body(family, response)
            .await
            .map_err(Failure::from_http)?;
        if status.is_success() {
            fresh_credential(provider, stored, &bytes, unix_now()).map_err(Failure::Final)
        } else {
            Err(rejection(provider, status.as_u16(), &bytes, stored))
        }
    }
}

/// Writes `credential` for `id` unless `cancel` has fired. The check is the
/// commit's linearization point: after a cancellation nothing is written;
/// past the check the atomic rename of [`AuthStore::store`] runs to its end.
pub(crate) fn commit(
    cancel: &CancellationToken,
    store: &mut AuthStore,
    id: &str,
    credential: Credential,
) -> Result<(), ProviderError> {
    if cancel.is_cancelled() {
        return Err(ProviderError::AuthWrite {
            reason: String::from("the refresh was cancelled before the commit"),
        });
    }
    store.set(id, credential)?;
    store.store()
}

/// The outcome of one failed refresh attempt.
enum Failure {
    /// Worth the single retry.
    Transient(ProviderError),
    /// Returned at once.
    Final(ProviderError),
}

impl Failure {
    fn from_http(error: ProviderError) -> Self {
        match error {
            ProviderError::Transport { .. } => Self::Transient(error),
            other => Self::Final(other),
        }
    }
}

#[derive(Serialize)]
struct RefreshBody<'a> {
    grant_type: &'static str,
    client_id: &'static str,
    refresh_token: &'a str,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: SecretString,
    #[serde(default)]
    refresh_token: Option<SecretString>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    id_token: Option<SecretString>,
}

/// Whether `credential` is inside the proactive window at `now`; a credential
/// without expiry refreshes only after a 401.
fn expiring(credential: &OAuthCredential, now: i64) -> bool {
    credential
        .expires_at
        .is_some_and(|at| at.saturating_sub(now) <= PROACTIVE_WINDOW_SECS)
}

/// The new credential from a successful token response. A response without
/// a refresh token or ID token keeps the stored one; the Codex account id
/// follows the ID token when it names one.
pub(crate) fn fresh_credential(
    provider: OAuthProvider,
    stored: &OAuthCredential,
    body: &[u8],
    now: i64,
) -> Result<OAuthCredential, ProviderError> {
    let invalid = |detail: &str| ProviderError::Transport {
        family: provider.family(),
        reason: format!("the token endpoint answered with an invalid body: {detail}"),
    };
    // The parser message may quote the body, which holds tokens: drop it.
    let tokens = sonic_rs::from_slice::<TokenResponse>(body)
        .map_err(|_| invalid("not the token response shape"))?;
    if tokens.access_token.expose().is_empty() {
        return Err(invalid("the access token is empty"));
    }
    let expires_at = oauth_expires_at(now, tokens.expires_in, tokens.access_token.expose());
    let refresh_token = tokens
        .refresh_token
        .filter(|token| !token.expose().is_empty())
        .unwrap_or_else(|| stored.refresh_token.clone());
    let (id_token, account_id) = match provider {
        OAuthProvider::Anthropic => (None, None),
        OAuthProvider::OpenAiCodex => {
            let id_token = tokens
                .id_token
                .map(|token| token.expose().to_owned())
                .or_else(|| stored.id_token.clone());
            let account_id = id_token
                .as_deref()
                .and_then(codex_identity)
                .map(|identity| identity.account_id)
                .or_else(|| stored.account_id.clone());
            (id_token, account_id)
        }
    };
    Ok(OAuthCredential {
        access_token: tokens.access_token,
        refresh_token,
        expires_at,
        id_token,
        account_id,
    })
}

/// Classifies a non-success token response.
fn rejection(
    provider: OAuthProvider,
    status: u16,
    body: &[u8],
    stored: &OAuthCredential,
) -> Failure {
    let value = sonic_rs::from_slice::<sonic_rs::Value>(body).ok();
    let permanent = value.as_ref().is_some_and(|value| {
        error_codes(value)
            .into_iter()
            .flatten()
            .any(|code| PERMANENT_CODES.contains(&code))
    });
    if (400..500).contains(&status) && permanent {
        return Failure::Final(ProviderError::SignInExpired {
            provider: provider.id().to_owned(),
        });
    }
    let error = ProviderError::Status {
        family: provider.family(),
        status,
        message: redact(&error_message(value.as_ref(), body), stored),
    };
    if matches!(status, 408 | 429 | 500..=599) {
        Failure::Transient(error)
    } else {
        Failure::Final(error)
    }
}

/// The error codes a token endpoint may use: OAuth `{"error":"<code>"}`,
/// `OpenAI` `{"error":{"code":..,"type":..}}`, and a top-level `code`.
fn error_codes(value: &sonic_rs::Value) -> [Option<&str>; 4] {
    let error = value.get("error");
    [
        error.and_then(JsonValueTrait::as_str),
        error
            .and_then(|error| error.get("code"))
            .and_then(JsonValueTrait::as_str),
        error
            .and_then(|error| error.get("type"))
            .and_then(JsonValueTrait::as_str),
        value.get("code").and_then(JsonValueTrait::as_str),
    ]
}

/// The human message of an error body: `error_description`, then
/// `error.message`, then the first error code, then the raw text.
fn error_message(value: Option<&sonic_rs::Value>, body: &[u8]) -> String {
    let field = value.and_then(|value| {
        value
            .get("error_description")
            .and_then(JsonValueTrait::as_str)
            .or_else(|| {
                value
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(JsonValueTrait::as_str)
            })
            .or_else(|| error_codes(value).into_iter().flatten().next())
    });
    field.map_or_else(|| String::from_utf8_lossy(body).into_owned(), str::to_owned)
}

/// Replaces any echo of the stored secrets in a server message.
fn redact(message: &str, stored: &OAuthCredential) -> String {
    let mut message = message.to_owned();
    for secret in [&stored.refresh_token, &stored.access_token] {
        let secret = secret.expose();
        if !secret.is_empty() {
            message = message.replace(secret, "<redacted>");
        }
    }
    message
}
