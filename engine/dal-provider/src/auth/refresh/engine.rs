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
        device::xai_expires_at,
        kimi::{ClientIdentity, ClientIdentitySource},
        meta,
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
    /// The source of the Kimi client identity the Kimi Code refresh carries.
    /// Set once by [`Refresher::with_client_identity`]; `None` until the
    /// provider set shares its source, and a Kimi Code refresh without one is
    /// refused before any request: it never goes out unnamed.
    client_identity: Option<Arc<ClientIdentitySource>>,
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
/// row's own id; `None` for a row whose vendor registers no client. The
/// login side resolves the identical value, so a token issued to a
/// configured application is refreshed as that application.
pub(crate) fn effective_client_id(oauth: &OAuthSpec, env: &EnvSnapshot) -> Option<Box<str>> {
    oauth
        .client_id_env
        .and_then(|name| env.get(name))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or(oauth.client_id)
        .map(Into::into)
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

/// How a refresh request is sent: a JSON or form grant at the token
/// endpoint, or Meta's key mint, which sends no grant at all.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BodyEncoding {
    Json,
    Form,
    /// The Muse Code key mint: the identity token is the bearer, the body is
    /// an empty JSON object, and the answer is a new API key rather than a
    /// token response. There is no `grant_type` and no client id.
    Mint,
}

impl BodyEncoding {
    /// The encoding the hook's token endpoint takes for a refresh; `None` for
    /// a hook that never refreshes (a durable API key, or a Devin token with
    /// no refresh grant). No wildcard: a new hook must choose.
    fn of(hook: Hook) -> Option<Self> {
        match hook {
            Hook::ClaudeCode | Hook::CodexAccount | Hook::CursorPoll => Some(Self::Json),
            Hook::WorkOsDevice
            | Hook::GitLabDuo
            | Hook::RadiusGateway
            | Hook::GoogleCloudCodeAssist
            | Hook::GoogleAntigravity
            | Hook::XaiDevice
            | Hook::KimiCodeDevice => Some(Self::Form),
            Hook::MetaMuse => Some(Self::Mint),
            Hook::OpenRouterKey | Hook::DevinCliToken => None,
        }
    }

    /// The refresh body. A form body carries the row's client secret, when it
    /// registers one, between the client id and the refresh token, as the
    /// reference's declarative refresh orders it; no row with a JSON refresh
    /// registers a secret.
    fn body(self, client: ClientAuth<'_>, refresh_token: &str) -> Result<Vec<u8>, sonic_rs::Error> {
        match self {
            // The mint request builds its own body.
            Self::Mint => Ok(Vec::new()),
            Self::Json => sonic_rs::to_vec(&RefreshBody {
                grant_type: "refresh_token",
                client_id: client.id,
                refresh_token,
            }),
            Self::Form => {
                let mut form = url::form_urlencoded::Serializer::new(String::new());
                form.append_pair("grant_type", "refresh_token")
                    .append_pair("client_id", client.id);
                if let Some(secret) = client.secret {
                    form.append_pair("client_secret", secret);
                }
                form.append_pair("refresh_token", refresh_token);
                Ok(form.finish().into_bytes())
            }
        }
    }
}

/// The OAuth client a refresh authenticates as: the row's effective id and,
/// for an installed-application client, its registered secret.
#[derive(Clone, Copy)]
struct ClientAuth<'a> {
    id: &'a str,
    secret: Option<&'a str>,
}

/// How a refresh request names itself: the host's `User-Agent`, or, for a
/// sign-in that names the vendor's client (Kimi Code), the client's own
/// `User-Agent` and identity headers in its place.
#[derive(Clone, Copy)]
struct Naming<'a> {
    user_agent: &'a str,
    identity: Option<&'a ClientIdentity>,
}

impl<'a> Naming<'a> {
    /// The `User-Agent` the request sends.
    fn agent(self) -> &'a str {
        self.identity
            .map_or(self.user_agent, ClientIdentity::user_agent)
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
            client_identity: None,
            slots: PROVIDERS
                .iter()
                .filter_map(|def| {
                    let oauth = def.oauth.as_ref()?;
                    // A row that never refreshes (`OpenRouter`'s durable key,
                    // Devin's token) has nothing to refresh, and a refreshing
                    // row sends its client id; a row with neither gets no
                    // slot.
                    if !oauth.hook.refreshes() {
                        return None;
                    }
                    let client_id = effective_client_id(oauth, env)?;
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

    /// Shares the provider set's Kimi client-identity source with this
    /// refresher. The Kimi Code refresh names itself with the identity the
    /// source derives (its `User-Agent` and `X-Msh-*` headers), the same one
    /// every request of the set sends.
    #[must_use]
    pub(crate) fn with_client_identity(mut self, source: Arc<ClientIdentitySource>) -> Self {
        self.client_identity = Some(source);
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
    /// A `GitLab Duo`, Factory Droid, Radius, xAI, Kimi Code, or Cloud Code
    /// Assist refresh sends a form body (the Cloud Code Assist one carries the
    /// row's registered client secret and keeps the stored Google Cloud
    /// project; the Kimi Code one names the Kimi client identity and fails
    /// at once for a `401` or `403`); the others send JSON. A Meta refresh
    /// is a new key mint with the stored identity token as the bearer, not a
    /// grant: it stores the new key with the same identity token and a
    /// 24-hour expiry only after the mint answered a key, and a refused mint
    /// leaves `auth.json` as it was. A Cursor answer is read by the family's own decoder: a
    /// `200` that names `shouldLogout` ends the sign-in with
    /// [`ProviderError::SignInExpired`] and stores nothing. A row with no
    /// refresh grant (Devin) gets [`ProviderError::SignInExpired`] without a
    /// request. After the exchange a Droid refresh checks the fresh access
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
        let (Some(oauth), Some(url), Some(slot), Some(encoding)) = (
            provider.oauth,
            self.endpoints.url(provider),
            self.slot(provider),
            provider
                .oauth
                .and_then(|oauth| BodyEncoding::of(oauth.hook)),
        ) else {
            return Err(ProviderError::SignInExpired {
                provider: provider.id.to_owned(),
            });
        };
        // A refresh that names the client carries the identity the provider
        // set gave this refresher; without one it is refused before any
        // request or lock, never sent unnamed.
        let client_identity = if oauth.hook.sends_client_identity() {
            let Some(source) = &self.client_identity else {
                return Err(ProviderError::AuthWrite {
                    reason: format!(
                        "{} refresh needs the client identity; this refresher was given none",
                        provider.id
                    ),
                });
            };
            Some(source.get().await?)
        } else {
            None
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
        let client_secret = oauth.client_secret;
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
                Naming {
                    user_agent: &user_agent,
                    identity: client_identity.as_deref(),
                },
                &url,
                provider,
                ClientAuth {
                    id: &client_id,
                    secret: client_secret,
                },
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
        http_client: &reqwest::Client,
        naming: Naming<'_>,
        url: &Url,
        provider: &ProviderDef,
        client_auth: ClientAuth<'_>,
        encoding: BodyEncoding,
        stored: &OAuthCredential,
    ) -> Result<OAuthCredential, ProviderError> {
        let body = encoding
            .body(client_auth, stored.refresh_token.expose())
            .map_err(|_| ProviderError::Transport {
                family: provider.family,
                reason: String::from("could not encode the token refresh request"),
            })?;
        let mut retried = false;
        loop {
            let attempt = Self::attempt(
                http_client,
                naming,
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
        naming: Naming<'_>,
        url: &Url,
        provider: &ProviderDef,
        stored: &OAuthCredential,
        encoding: BodyEncoding,
        body: Vec<u8>,
    ) -> Result<OAuthCredential, Failure> {
        let family = provider.family;
        let mut request = match encoding {
            BodyEncoding::Mint => meta::request(client, url, stored.refresh_token.expose()),
            BodyEncoding::Json => client.post(url.clone()).body(body),
            BodyEncoding::Form => client
                .post(url.clone())
                .body(body)
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded"),
        };
        // A request that names the client sends the client's identity
        // headers (and its `User-Agent`, through `naming`).
        if let Some(identity) = naming.identity {
            for (name, value) in identity.headers() {
                request = request.header(*name, value.as_str());
            }
        }
        let exchange = Exchange::Json {
            total: OAUTH_TIMEOUT,
        };
        let response = send(
            family,
            request,
            naming.agent(),
            exchange,
            tokio::time::sleep,
        )
        .await
        .map_err(Failure::from_http)?;
        let status = response.status();
        let bytes = read_body(family, response)
            .await
            .map_err(Failure::from_http)?;
        if encoding == BodyEncoding::Mint {
            return minted(provider, status.as_u16(), &bytes, stored);
        }
        if status.is_success() {
            let fresh = if provider
                .oauth
                .is_some_and(|oauth| oauth.hook == Hook::CursorPoll)
            {
                crate::family::cursor::auth::refreshed(provider.id, stored, &bytes, unix_now())
            } else {
                fresh_credential(provider, stored, &bytes, unix_now())
            };
            fresh.map_err(Failure::Final)
        } else {
            Err(rejection(provider, status.as_u16(), &bytes, stored))
        }
    }
}

/// The outcome of one key mint: the new credential, a dead session (the
/// stored identity token no longer names one), or a refusal that is worth
/// the single retry when the status says the mint may recover. Every message
/// is redacted of the identity token and of the key being replaced.
fn minted(
    provider: &ProviderDef,
    status: u16,
    body: &[u8],
    stored: &OAuthCredential,
) -> Result<OAuthCredential, Failure> {
    match meta::outcome(
        provider.family,
        status,
        body,
        &[stored.refresh_token.expose(), stored.access_token.expose()],
    ) {
        meta::Outcome::Key(key) => Ok(meta::credential(
            key,
            stored.refresh_token.clone(),
            unix_now(),
        )),
        meta::Outcome::SessionExpired => Err(Failure::Final(ProviderError::SignInExpired {
            provider: provider.id.to_owned(),
        })),
        meta::Outcome::Refused(error) => Err(
            if matches!(
                &error,
                ProviderError::Status {
                    status: 408 | 429 | 500..=599,
                    ..
                }
            ) {
                Failure::Transient(error)
            } else {
                Failure::Final(error)
            },
        ),
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
/// a refresh token or ID token keeps the stored one (Google never rotates its
/// refresh token); the Codex account id follows the ID token when it names
/// one, and a Cloud Code Assist sign-in keeps its stored Google Cloud
/// project. The lifetime counts from the
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
    let hook = provider.oauth.map(|oauth| oauth.hook);
    // A Kimi Code answer always rotates: it names a refresh token and a
    // positive lifetime, or it is not a token answer.
    if hook == Some(Hook::KimiCodeDevice)
        && (tokens
            .refresh_token
            .as_ref()
            .is_none_or(|token| token.expose().is_empty())
            || !matches!(tokens.expires_in, Some(seconds) if seconds > 0))
    {
        return Err(invalid("the answer names no refresh token or no lifetime"));
    }
    let issued_at = oauth_issued_at(tokens.created_at.as_ref(), now);
    // An xAI answer without a lifetime lives one hour, whatever its JWT
    // claims; a lifetime that is not positive is no token answer.
    let expires_at = if hook == Some(Hook::XaiDevice) {
        Some(
            xai_expires_at(issued_at, tokens.expires_in)
                .ok_or_else(|| invalid("the lifetime is not positive"))?,
        )
    } else {
        oauth_expires_at(issued_at, tokens.expires_in, tokens.access_token.expose())
    };
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
        // The project is minted once, at sign-in, and Google's refresh answer
        // never names it: the stored one is kept exactly as it was.
        project_id: stored.project_id.clone(),
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
    // The Kimi Code token endpoint answers 401 or 403 for a dead credential.
    let unauthorized = matches!(status, 401 | 403)
        && provider
            .oauth
            .is_some_and(|oauth| oauth.hook == Hook::KimiCodeDevice);
    if ((400..500).contains(&status) && permanent) || unauthorized {
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
