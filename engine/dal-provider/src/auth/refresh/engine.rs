//! The serialized refresh engine: one refresher per `auth.json`.
//!
//! The per-key flight slot owns one refresh task from the request through
//! persistence. The task holds the auth file lock; the slot serializes
//! callers. A caller can drop its wait before the task starts, but cannot
//! interrupt a request that may have rotated the stored refresh token.
//! [`Refresher::shutdown`] is the host's way to end the engine: it refuses new
//! refreshes and waits for every task still in a slot.
//!
//! A Factory Droid refresh adds one step between the token exchange and the
//! commit: the Factory session check. Its failure is advisory, but the
//! shutdown cancel is not: a cancelled task commits nothing and fails like the
//! refusal of a refresh after shutdown, with [`ProviderError::Transport`].
//! The cancel is published under the refresher's commit gate, so it is
//! linearized against the whole commit critical section: either it is
//! published before a Droid commit is admitted, and the task's in-section
//! check stores nothing, or the admitted commit runs to its atomic rename
//! before the cancel can be published at all.

use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
    },
};

use dal_core::Family;
use reqwest::header::CONTENT_TYPE;
use serde::{Deserialize, Serialize};
use sonic_rs::JsonValueTrait;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use url::Url;

use super::{
    PERMANENT_CODES, RETRY_DELAY, blocking,
    endpoints::{RefreshReason, TokenEndpoints},
    lock_auth_file,
};
use crate::{
    Hook, OAuthSpec, PROVIDERS, ProviderDef,
    auth::{
        credential::{
            AuthStore, Credential, EnvSnapshot, OAuthCredential, SecretString, codex_identity,
            oauth_expires_at, oauth_issued_at,
        },
        oauth::{truncate_message, unix_now},
    },
    error::ProviderError,
    family::{droid::verify_session, gitlab::DirectAccessCache},
    http::{CONNECT_TIMEOUT, Exchange, OAUTH_TIMEOUT, read_body, send},
};

/// Refreshes OAuth credentials stored in one `auth.json`, at most one refresh
/// per credential key at a time across every refresher of that file.
///
/// A host keeps one `Refresher` per `auth.json` and calls
/// [`Refresher::shutdown`] before dropping it, because a refresh task that
/// outlives its owner would keep the auth-file lock and still commit. It holds
/// no secret itself: the client ids it keeps are public application ids.
pub struct Refresher {
    client: LazyLock<reqwest::Client, fn() -> reqwest::Client>,
    user_agent: Box<str>,
    auth_path: PathBuf,
    endpoints: TokenEndpoints,
    slots: Vec<(&'static str, RefreshSlot)>,
    closed: AtomicBool,
    /// Fires at shutdown. Only the Factory session check observes it.
    cancel: CancellationToken,
    /// Serializes the publication of the shutdown cancel against the commit
    /// critical section of a Droid refresh. Shared with its session checks.
    commit_gate: Arc<Mutex<()>>,
    /// A test override of the Factory API base the session check uses. Absent,
    /// the check goes to the endpoint set's base, which is the row's own in
    /// production and moves with the other test endpoints.
    session_base: Option<Box<str>>,
    /// The shared GitLab Duo `direct_access` cache. Set once by
    /// [`Refresher::with_gitlab_grants`]; `None` until the provider set
    /// shares its cache. A committed GitLab Duo refresh drops the old and
    /// new tokens' grants through it; no other hook touches it.
    gitlab_grants: Option<Arc<DirectAccessCache>>,
}

impl std::fmt::Debug for Refresher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Refresher")
            .field("user_agent", &self.user_agent)
            .field("auth_path", &self.auth_path)
            .field("endpoints", &self.endpoints)
            .field("slots", &self.slots)
            .field("closed", &self.closed)
            .field("session_base", &self.session_base)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct RefreshSlot {
    /// The id the row's sign-in sent at construction time: the configured
    /// override when the environment snapshot had one, else the row's.
    client_id: Box<str>,
    key: Mutex<()>,
    in_flight: Mutex<Option<tokio::task::JoinHandle<Result<Credential, ProviderError>>>>,
}

impl RefreshSlot {
    fn new(client_id: Box<str>) -> Self {
        Self {
            client_id,
            key: Mutex::new(()),
            in_flight: Mutex::new(None),
        }
    }
}

/// The client id a sign-in of `oauth` uses under `env`: the row's override
/// variable when the snapshot holds a nonblank value (trimmed), else the
/// row's own id. The login side resolves the identical value, so a token
/// issued to a configured application is refreshed as that application.
pub(crate) fn effective_client_id(oauth: &OAuthSpec, env: &EnvSnapshot) -> Box<str> {
    oauth
        .client_id_env
        .and_then(|name| env.get(name))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(oauth.client_id)
        .into()
}

/// An owned advisory lock on `auth.json.lock`.
///
/// Explicit release via [`LockFile::unlock`] synchronously removes the flock
/// from the open-file description and returns any error. [`Drop`] provides
/// cancellation-safe release on early task exit, abort, or failure with a warning.
#[derive(Debug)]
struct LockFile {
    file: Option<File>,
}

impl LockFile {
    fn new(file: File) -> Self {
        Self { file: Some(file) }
    }

    /// Explicitly unlocks the file lock, taking ownership to avoid duplicate unlock on drop.
    fn unlock(&mut self) -> Result<(), ProviderError> {
        if let Some(file) = self.file.take() {
            file.unlock().map_err(|error| ProviderError::AuthWrite {
                reason: format!("could not unlock auth file: {error}"),
            })?;
        }
        Ok(())
    }
}

impl Drop for LockFile {
    fn drop(&mut self) {
        if let Some(file) = self.file.take()
            && let Err(error) = file.unlock()
        {
            tracing::warn!(%error, "could not unlock auth file during drop");
        }
    }
}

/// How a refresh request body is encoded on the wire.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BodyEncoding {
    Json,
    Form,
}

impl BodyEncoding {
    /// The encoding the hook's token endpoint takes. No wildcard: a new hook
    /// must choose.
    fn of(hook: Hook) -> Self {
        match hook {
            Hook::ClaudeCode | Hook::CodexAccount => Self::Json,
            Hook::WorkOsDevice | Hook::GitLabDuo => Self::Form,
        }
    }

    fn body(self, client_id: &str, refresh_token: &str) -> Result<Vec<u8>, sonic_rs::Error> {
        match self {
            Self::Json => sonic_rs::to_vec(&RefreshBody {
                grant_type: "refresh_token",
                client_id,
                refresh_token,
            }),
            Self::Form => Ok(url::form_urlencoded::Serializer::new(String::new())
                .append_pair("grant_type", "refresh_token")
                .append_pair("client_id", client_id)
                .append_pair("refresh_token", refresh_token)
                .finish()
                .into_bytes()),
        }
    }
}

/// The Factory session check one Droid refresh runs between its token
/// exchange and its commit, and the shutdown cancel that ends the refresh.
struct SessionCheck {
    family: Family,
    base: Box<str>,
    cancel: CancellationToken,
}

impl SessionCheck {
    /// The failure of a refresh the shutdown cancelled. It is the error a
    /// refresh refused after shutdown gets, not `LoginCancelled`: the caller
    /// is a request, not a sign-in, and a transport error is what a request
    /// retries or reports.
    fn cancelled(&self) -> ProviderError {
        ProviderError::Transport {
            family: self.family,
            reason: String::from("credential refresh cancelled: the refresher is shut down"),
        }
    }

    /// Fails once the refresher is shut down. The task asks before it sends
    /// the exchange, so a request that would rotate the refresh token is
    /// never sent only to be discarded, and inside the commit gate right
    /// before the commit, where a published cancel is observed without a
    /// race: the cancel itself is published only by a shutdown that holds
    /// the same gate.
    fn live(&self) -> Result<(), ProviderError> {
        if self.cancel.is_cancelled() {
            Err(self.cancelled())
        } else {
            Ok(())
        }
    }

    /// Verifies the fresh access token. A failure is advisory: it is logged
    /// without any response text and the refresh goes on to commit, because
    /// the stored identity does not depend on the answer. Cancellation is
    /// not advisory: it fails the task, so a cancelled refresh never
    /// persists.
    async fn run(
        &self,
        client: &reqwest::Client,
        provider: &str,
        fresh: &OAuthCredential,
    ) -> Result<(), ProviderError> {
        match verify_session(
            client,
            &self.base,
            fresh.access_token.expose(),
            &self.cancel,
        )
        .await
        {
            Ok(()) => Ok(()),
            Err(ProviderError::LoginCancelled) => Err(self.cancelled()),
            Err(error) => {
                tracing::warn!(
                    provider,
                    %error,
                    "the Factory session check failed after refresh; keeping the refreshed tokens"
                );
                Ok(())
            }
        }
    }
}

/// Builds the redirect-free client one [`Refresher`] uses.
///
/// # Panics
///
/// Panics when reqwest cannot initialize its TLS backend, which only a broken
/// build configuration causes.
#[expect(
    clippy::expect_used,
    reason = "the builder fails only when the compiled TLS backend cannot initialize"
)]
fn refresh_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .referer(false)
        .retry(reqwest::retry::never())
        .build()
        .expect("the reqwest TLS backend initializes")
}

impl Refresher {
    /// A refresher for the `auth.json` at `auth_path` that sends refresh
    /// requests with `user_agent` to `endpoints`.
    ///
    /// `env` is the process environment the binary captured at startup. Each
    /// OAuth row's effective client id is resolved from it here, once, and
    /// kept: a row with an override variable (`GitLab Duo`) refreshes as the
    /// configured application, the same one its sign-in used. Nothing reads
    /// the process environment after construction.
    ///
    /// Its client is built by the first refresh, not here, because building
    /// one parses the platform trust roots. It follows no redirect: a refresh
    /// body carries the refresh token, and reqwest resends a body on a 307 or
    /// 308 to whatever host the redirect names. A 3xx answer is a
    /// [`ProviderError::Status`] instead.
    #[must_use]
    pub fn new(
        user_agent: impl Into<Box<str>>,
        auth_path: impl Into<PathBuf>,
        endpoints: TokenEndpoints,
        env: &EnvSnapshot,
    ) -> Self {
        Self {
            client: LazyLock::new(refresh_client),
            user_agent: user_agent.into(),
            auth_path: auth_path.into(),
            endpoints,
            closed: AtomicBool::new(false),
            cancel: CancellationToken::new(),
            commit_gate: Arc::new(Mutex::new(())),
            session_base: None,
            gitlab_grants: None,
            slots: PROVIDERS
                .iter()
                .filter_map(|def| {
                    let client_id = effective_client_id(def.oauth.as_ref()?, env);
                    Some((def.id, RefreshSlot::new(client_id)))
                })
                .collect(),
        }
    }

    /// Sends the Factory session check to `base` instead of the endpoint
    /// set's, for tests that serve the check apart from the token endpoint. A
    /// plain-`http` base must be loopback, like every other endpoint.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_session_base(mut self, base: impl Into<Box<str>>) -> Self {
        self.session_base = Some(base.into());
        self
    }

    /// Shares the provider set's GitLab Duo `direct_access` cache with this
    /// refresher. After a GitLab Duo refresh commits, the owned task drops
    /// the old and new tokens' grants so the next turn fetches with the
    /// rotated credential.
    #[must_use]
    pub(crate) fn with_gitlab_grants(mut self, grants: Arc<DirectAccessCache>) -> Self {
        self.gitlab_grants = Some(grants);
        self
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
    /// Dropping the returned future while it waits for either lock abandons
    /// that wait. The per-key slot keeps an in-flight task so a later caller
    /// awaits the same refresh. That task sends the request and persists its
    /// result before releasing the auth file lock, even if the first caller
    /// drops its wait. A finished task's success is handed to the next
    /// caller; its failure is dropped, so the next caller starts a fresh
    /// refresh instead of receiving the old error. The HTTP exchange remains
    /// bounded by [`OAUTH_TIMEOUT`].
    ///
    /// A `GitLab Duo` or Factory Droid refresh sends a form body; the others
    /// send JSON. After the exchange a Droid refresh checks the fresh access
    /// token against the Factory session endpoint before it commits. That
    /// check is advisory: a failed answer is logged and the tokens are still
    /// committed. [`Refresher::shutdown`] cancels it under the commit gate,
    /// and a cancelled refresh commits nothing and returns
    /// [`ProviderError::Transport`].
    ///
    /// # Errors
    ///
    /// - [`ProviderError::SignInExpired`] when the token endpoint rejects the
    ///   refresh token (`invalid_grant`, `refresh_token_expired`,
    ///   `refresh_token_reused`, `refresh_token_invalidated`); sent once.
    /// - [`ProviderError::Transport`] or [`ProviderError::Status`] after a
    ///   transient failure (transport, 408, 429, 5xx) repeated on the retry,
    ///   or at once for another non-success status or an invalid token body.
    /// - [`ProviderError::Transport`] also when [`Refresher::shutdown`]
    ///   cancelled a Droid refresh before its commit was admitted; the
    ///   commit gate keeps that cancellation from racing a commit that has
    ///   been admitted, which then always finishes. Nothing was stored.
    /// - [`ProviderError::NoCredentials`] when `auth.json` no longer has the
    ///   entry.
    /// - The [`AuthStore::load`] errors for the reloaded file, and
    ///   [`ProviderError::AuthWrite`] when `auth.json.lock` cannot be opened,
    ///   stays held by another process past the lock wait, or the commit
    ///   fails (the previous file then stays in place).
    /// - [`ProviderError::PlainHttp`] and [`ProviderError::Limit`] from the
    ///   HTTP layer, without retry.
    #[expect(
        clippy::too_many_lines,
        reason = "refresh sequence coordinates in-memory slot lock, token exchange, file reload, and commit"
    )]
    pub async fn refresh(
        &self,
        provider: &'static ProviderDef,
        held: &OAuthCredential,
        reason: RefreshReason,
    ) -> Result<Credential, ProviderError> {
        if reason == RefreshReason::Expiring && !held.expiring(unix_now()) {
            return Ok(Credential::OAuth(held.clone()));
        }
        let (Some(oauth), Some(url), Some(slot)) = (
            provider.oauth,
            self.endpoints.url(provider),
            self.slot(provider),
        ) else {
            return Err(ProviderError::SignInExpired {
                provider: provider.id.to_owned(),
            });
        };
        let key = slot.key.lock().await;
        let mut in_flight = slot.in_flight.lock().await;
        if in_flight
            .as_ref()
            .is_some_and(tokio::task::JoinHandle::is_finished)
        {
            match await_refresh(&mut in_flight).await {
                Ok(credential) => {
                    drop(key);
                    return Ok(credential);
                }
                // The finished task's failure is stale: nobody received it.
                // await_refresh cleared the slot, so this caller starts the
                // fresh refresh below, still holding the per-key guard to
                // keep the fresh attempt single-flight.
                Err(_) => {
                    tracing::debug!(
                        provider = provider.id,
                        "the joined refresh had already failed; starting a fresh one"
                    );
                }
            }
        }
        if in_flight.is_some() {
            drop(key);
            return await_refresh(&mut in_flight).await;
        }
        if self.closed.load(Ordering::Acquire) {
            return Err(ProviderError::Transport {
                family: provider.family,
                reason: String::from("credential refresh refused: the refresher is shut down"),
            });
        }
        let mut file = LockFile::new(lock_auth_file(&self.auth_path).await?);
        let path = self.auth_path.clone();
        let mut store = blocking(move || AuthStore::load(path)).await?;
        let stored = match store.credential(provider.id) {
            Some(Credential::OAuth(stored)) => stored,
            Some(other @ Credential::ApiKey { .. }) => {
                file.unlock()?;
                return Ok(other);
            }
            Some(Credential::None) | None => {
                return Err(ProviderError::NoCredentials {
                    provider: provider.id.to_owned(),
                });
            }
        };
        if stored.access_token != held.access_token
            || (reason == RefreshReason::Expiring && !stored.expiring(unix_now()))
        {
            file.unlock()?;
            return Ok(Credential::OAuth(stored));
        }
        let client = (*self.client).clone();
        let user_agent = self.user_agent.clone();
        let url = url.clone();
        let id = provider.id;
        let client_id = slot.client_id.clone();
        let encoding = BodyEncoding::of(oauth.hook);
        let session_check = (oauth.hook == Hook::WorkOsDevice).then(|| SessionCheck {
            family: provider.family,
            base: self
                .session_base
                .clone()
                .or_else(|| self.endpoints.session_base(provider).map(Box::from))
                .unwrap_or_else(|| Box::from(provider.base_url)),
            cancel: self.cancel.clone(),
        });
        // Only a Droid task serializes its commit against the shutdown
        // cancel; the other rows keep their exact behavior.
        let commit_gate = (oauth.hook == Hook::WorkOsDevice).then(|| Arc::clone(&self.commit_gate));
        let gitlab_grants = self.gitlab_grants.clone();
        let invalidate_gitlab = oauth.hook == Hook::GitLabDuo;
        #[expect(
            clippy::disallowed_methods,
            reason = "the refresh task is stored in the per-key slot and must persist a rotated token after caller cancellation"
        )]
        let task = tokio::spawn(async move {
            if let Some(check) = &session_check {
                check.live()?;
            }
            let fresh = Self::exchange(
                &client,
                &user_agent,
                &url,
                provider,
                &client_id,
                encoding,
                &stored,
            )
            .await?;
            if let Some(check) = &session_check {
                check.run(&client, id, &fresh).await?;
            }
            // Admission of the commit: from here to the end of the blocking
            // work the shutdown cancel cannot be published, so the in-section
            // check and the atomic rename below are one critical section.
            let admitted = match &commit_gate {
                Some(gate) => Some(gate.lock().await),
                None => None,
            };
            let committed = Credential::OAuth(fresh.clone());
            // The post-commit invalidation rides inside the blocking closure
            // so it shares the commit's atomic point: the closure runs to
            // its end even if the awaiting task is dropped, and no shutdown
            // can land between the durable rename and the invalidation.
            // `stored` is dead after the exchange, so its token moves while
            // the still-needed fresh token clones. Other hooks build no
            // tuple and keep their exact behavior.
            let gitlab_invalidation = if invalidate_gitlab {
                gitlab_grants
                    .map(|grants| (grants, stored.access_token, fresh.access_token.clone()))
            } else {
                None
            };
            blocking(move || {
                // The file lock lives until the request and commit both finish.
                let mut file = file;
                if let Some(check) = &session_check {
                    check.live()?;
                }
                commit(&mut store, id, committed)?;
                if let Some((grants, old_token, new_token)) = gitlab_invalidation {
                    grants.remove(old_token.expose());
                    grants.remove(new_token.expose());
                }
                // The commit is durable. A failed unlock cannot undo it, and
                // the lock is released when the file closes, so the failure is
                // logged and the refresh still succeeds.
                if let Err(error) = file.unlock() {
                    tracing::warn!(
                        provider = id,
                        %error,
                        "could not release the auth file lock after the commit"
                    );
                }
                Ok(())
            })
            .await?;
            drop(admitted);
            tracing::debug!(provider = id, "stored refreshed OAuth tokens");
            Ok::<Credential, ProviderError>(Credential::OAuth(fresh))
        });
        *in_flight = Some(task);
        drop(key);
        await_refresh(&mut in_flight).await
    }

    /// Stops new refreshes and waits until every refresh task that is still
    /// in a slot has persisted or failed, so none outlives the host.
    ///
    /// A [`Refresher::refresh`] that needs a new request after this call
    /// starts fails with [`ProviderError::Transport`]; one that joins a task
    /// already in flight still receives its result, except that a Droid task
    /// that has not committed ends with [`ProviderError::Transport`] and
    /// keeps the stored credential: the shutdown cancel is published under
    /// the commit gate, so a commit that has been admitted always finishes,
    /// and one that has not stores nothing. The wait covers a task
    /// whose callers all dropped their waits, and is safe to drop and call
    /// again: the slot keeps the task until it is observed finished.
    pub async fn shutdown(&self) {
        self.closed.store(true, Ordering::Release);
        // Published under the commit gate: a Droid task inside its commit
        // critical section finishes it (the atomic rename is durable) before
        // the cancel exists, and one that has not been admitted yet observes
        // the cancel in its in-section check and stores nothing. A task
        // parked in the session check holds no gate, so taking it is prompt
        // and the cancel still ends the wait at once. Before the drain: a
        // live caller holds its slot while it waits, so the cancel is what
        // lets a task parked in the session check end.
        let gate = self.commit_gate.lock().await;
        self.cancel.cancel();
        drop(gate);
        for (_, slot) in &self.slots {
            let mut in_flight = slot.in_flight.lock().await;
            if let Some(task) = in_flight.as_mut() {
                let _outcome = task.await;
            }
            *in_flight = None;
        }
    }

    fn slot(&self, provider: &ProviderDef) -> Option<&RefreshSlot> {
        self.slots
            .iter()
            .find(|(id, _)| *id == provider.id)
            .map(|(_, slot)| slot)
    }
}
impl Refresher {
    /// Sends the refresh request, retrying once after [`RETRY_DELAY`] on a
    /// transient failure.
    async fn exchange(
        client: &reqwest::Client,
        user_agent: &str,
        url: &Url,
        provider: &ProviderDef,
        client_id: &str,
        encoding: BodyEncoding,
        stored: &OAuthCredential,
    ) -> Result<OAuthCredential, ProviderError> {
        let body = encoding
            .body(client_id, stored.refresh_token.expose())
            .map_err(|_| ProviderError::Transport {
                family: provider.family,
                reason: String::from("could not encode the token refresh request"),
            })?;
        let mut retried = false;
        loop {
            let attempt = Self::attempt(
                client,
                user_agent,
                url,
                provider,
                stored,
                encoding,
                body.clone(),
            );
            match attempt.await {
                Ok(fresh) => return Ok(fresh),
                Err(Failure::Transient(_)) if !retried => {
                    retried = true;
                    tracing::debug!(
                        provider = provider.id,
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
        client: &reqwest::Client,
        user_agent: &str,
        url: &Url,
        provider: &ProviderDef,
        stored: &OAuthCredential,
        encoding: BodyEncoding,
        body: Vec<u8>,
    ) -> Result<OAuthCredential, Failure> {
        let family = provider.family;
        let mut request = client.post(url.clone()).body(body);
        if encoding == BodyEncoding::Form {
            request = request.header(CONTENT_TYPE, "application/x-www-form-urlencoded");
        }
        let exchange = Exchange::Json {
            total: OAUTH_TIMEOUT,
        };
        let response = send(family, request, user_agent, exchange, tokio::time::sleep)
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

async fn await_refresh(
    in_flight: &mut Option<tokio::task::JoinHandle<Result<Credential, ProviderError>>>,
) -> Result<Credential, ProviderError> {
    let result = {
        let Some(task) = in_flight.as_mut() else {
            return Err(ProviderError::AuthWrite {
                reason: String::from("the refresh task is missing"),
            });
        };
        match task.await {
            Ok(result) => result,
            Err(error) => Err(ProviderError::AuthWrite {
                reason: format!("the OAuth refresh task failed: {error}"),
            }),
        }
    };
    *in_flight = None;
    result
}

/// Writes `credential` for `id` and atomically stores the updated auth file.
fn commit(store: &mut AuthStore, id: &str, credential: Credential) -> Result<(), ProviderError> {
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
    client_id: &'a str,
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
    /// When the endpoint issued the token, in Unix seconds (`GitLab`'s
    /// Doorkeeper sends it beside `expires_in`). Any other JSON type is
    /// ignored rather than failing the refresh of a row that never used it.
    #[serde(default)]
    created_at: Option<sonic_rs::Value>,
}

/// The new credential from a successful token response. A response without
/// a refresh token or ID token keeps the stored one; the Codex account id
/// follows the ID token when it names one. The lifetime counts from the
/// response's `created_at` when it carries an integer one, else from `now`;
/// `expires_in` absent falls back to the access-token JWT `exp` claim, then
/// to no expiry.
pub(crate) fn fresh_credential(
    provider: &ProviderDef,
    stored: &OAuthCredential,
    body: &[u8],
    now: i64,
) -> Result<OAuthCredential, ProviderError> {
    let invalid = |detail: &str| ProviderError::Transport {
        family: provider.family,
        reason: format!("the token endpoint answered with an invalid body: {detail}"),
    };
    // The parser message may quote the body, which holds tokens: drop it.
    let tokens = sonic_rs::from_slice::<TokenResponse>(body)
        .map_err(|_| invalid("not the token response shape"))?;
    if tokens.access_token.expose().is_empty() {
        return Err(invalid("the access token is empty"));
    }
    let issued_at = oauth_issued_at(tokens.created_at.as_ref(), now);
    let expires_at = oauth_expires_at(issued_at, tokens.expires_in, tokens.access_token.expose());
    let refresh_token = tokens
        .refresh_token
        .filter(|token| !token.expose().is_empty())
        .unwrap_or_else(|| stored.refresh_token.clone());
    let (id_token, account_id) = if provider
        .oauth
        .is_some_and(|oauth| oauth.hook == Hook::CodexAccount)
    {
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
    } else {
        (None, None)
    };
    Ok(OAuthCredential {
        access_token: tokens.access_token,
        refresh_token,
        expires_at,
        id_token,
        account_id,
    })
}

/// Classifies a non-success token response. The server message is redacted
/// first and then bounded to one line of at most 300 bytes, as a sign-in
/// error is, so a proxy's error page never rides into the error value.
fn rejection(
    provider: &ProviderDef,
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
            provider: provider.id.to_owned(),
        });
    }
    let error = ProviderError::Status {
        family: provider.family,
        status,
        message: truncate_message(&redact(&error_message(value.as_ref(), body), stored)),
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

/// Replaces any echo of the stored secrets in a server message, as stored and
/// as a form body carries them: a server that echoes a form request returns
/// the percent-encoded token.
fn redact(message: &str, stored: &OAuthCredential) -> String {
    let mut message = message.to_owned();
    for secret in [&stored.refresh_token, &stored.access_token] {
        let secret = secret.expose();
        if secret.is_empty() {
            continue;
        }
        message = message.replace(secret, "<redacted>");
        let encoded = url::form_urlencoded::byte_serialize(secret.as_bytes()).collect::<String>();
        if encoded != secret {
            message = message.replace(&encoded, "<redacted>");
        }
    }
    message
}
