//! Serialized OAuth token refresh over `auth.json`.
//!
//! One [`Refresher`] owns one `auth.json` path. Inside a process each OAuth
//! credential key (`anthropic`, `openai-codex`) has one async mutex; across
//! processes an exclusive advisory lock on the sibling `auth.json.lock`
//! (`std::fs::File::try_lock`, polled without blocking the runtime)
//! serializes every refresher of the same file. Holding both, the refresher
//! reloads `auth.json` and decides from the reloaded entry:
//!
//! - the stored access token differs from the one the caller holds: another
//!   caller or process already refreshed, so the stored credential is used and
//!   no request is sent;
//! - [`RefreshReason::Expiring`] and the stored token has more than
//!   [`PROACTIVE_WINDOW_SECS`] left (or no expiry at all): the stored
//!   credential is used and no request is sent;
//! - otherwise one refresh request goes to the token endpoint, retried once
//!   after [`RETRY_DELAY`] on a transient failure and never after a rejected
//!   refresh token, and the new tokens are committed by the atomic rename of
//!   [`AuthStore::store`].
//!
//! The commit is the last step. Each locked refresh owns a
//! [`CancellationToken`] that fires when its future is dropped; the commit
//! runs on the blocking pool owning both lock guards and first checks that
//! token. That check is the commit's linearization point: a cancellation
//! before it sends no write, leaves `auth.json` byte-for-byte unchanged, and
//! releases both locks; once past it, the single atomic rename runs to its
//! end before the locks release, so the file is never half-written and no
//! write starts after a cancellation. All file I/O runs on the blocking pool.
//! Nothing here reads the environment or keeps process-global state, and no
//! error text or log line carries a token.

use std::{
    fs::{File, OpenOptions, TryLockError},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use dal_core::Family;
use serde::{Deserialize, Serialize};
use sonic_rs::JsonValueTrait;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{
    auth::{
        credential::{
            AuthStore, Credential, OAuthCredential, SecretString, codex_identity,
            oauth_expires_at,
        },
        oauth::{CLAUDE_CLIENT_ID, CLAUDE_TOKEN_URL, CODEX_CLIENT_ID, CODEX_TOKEN_URL, unix_now},
    },
    error::ProviderError,
    http::{CONNECT_TIMEOUT, Exchange, OAUTH_TIMEOUT, endpoint, read_body, send},
};

/// Seconds before `expires_at` at which a proactive refresh starts.
pub const PROACTIVE_WINDOW_SECS: i64 = 300;

/// Wait between the first transient refresh failure and the single retry.
pub const RETRY_DELAY: Duration = Duration::from_secs(1);

/// Poll interval while another process holds `auth.json.lock`.
const LOCK_POLL: Duration = Duration::from_millis(20);

/// Longest wait for `auth.json.lock`: one full refresh by the holder (two
/// attempts bounded by [`OAUTH_TIMEOUT`] plus the retry delay) with slack for
/// its file writes.
const LOCK_WAIT: Duration =
    Duration::from_secs(2 * OAUTH_TIMEOUT.as_secs() + RETRY_DELAY.as_secs() + 5);

/// Token endpoint paths under a replay or test base, matching the paths of
/// [`CLAUDE_TOKEN_URL`] and [`CODEX_TOKEN_URL`].
const ANTHROPIC_TOKEN_PATH: &str = "v1/oauth/token";
const CODEX_TOKEN_PATH: &str = "oauth/token";

/// Token-endpoint error codes that mean the refresh token is dead; the user
/// must sign in again and no retry can help.
const PERMANENT_CODES: [&str; 4] = [
    "invalid_grant",
    "refresh_token_expired",
    "refresh_token_reused",
    "refresh_token_invalidated",
];

/// An `auth.json` credential key that holds an OAuth sign-in.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OAuthProvider {
    /// The `anthropic` entry (Claude sign-in).
    Anthropic,
    /// The `openai-codex` entry (`ChatGPT` sign-in).
    OpenAiCodex,
}

impl OAuthProvider {
    /// The provider for an `auth.json` key, when that key can hold an OAuth
    /// sign-in.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "anthropic" => Some(Self::Anthropic),
            "openai-codex" => Some(Self::OpenAiCodex),
            _ => None,
        }
    }

    /// The `auth.json` key and provider id.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAiCodex => "openai-codex",
        }
    }

    /// The API family whose errors name refresh transport failures.
    #[must_use]
    pub const fn family(self) -> Family {
        match self {
            Self::Anthropic => Family::Anthropic,
            Self::OpenAiCodex => Family::Codex,
        }
    }

    const fn client_id(self) -> &'static str {
        match self {
            Self::Anthropic => CLAUDE_CLIENT_ID,
            Self::OpenAiCodex => CODEX_CLIENT_ID,
        }
    }
}

/// Why a caller asks for a refresh.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefreshReason {
    /// Before a request: refresh only inside the proactive window.
    Expiring,
    /// After a 401 with the held token: refresh even inside the window,
    /// unless the stored token already changed.
    Rejected,
}

/// The token endpoint URL of each OAuth provider.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TokenEndpoints {
    anthropic: Url,
    openai_codex: Url,
}

impl TokenEndpoints {
    /// The production endpoints: `https://platform.claude.com/v1/oauth/token`
    /// and `https://auth.openai.com/oauth/token`.
    ///
    /// # Panics
    ///
    /// Never: both are constant `https` URLs.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "the production token endpoints are constant https URLs"
    )]
    pub fn production() -> Self {
        Self {
            anthropic: Url::parse(CLAUDE_TOKEN_URL).expect("the Claude token URL parses"),
            openai_codex: Url::parse(CODEX_TOKEN_URL).expect("the Codex token URL parses"),
        }
    }

    /// Endpoints under other bases, for tests and replay servers: the
    /// Anthropic endpoint is `<anthropic_base>/v1/oauth/token`, the Codex
    /// endpoint `<codex_base>/oauth/token`.
    ///
    /// # Errors
    ///
    /// Every [`endpoint`] failure: [`ProviderError::PlainHttp`] for plain
    /// `http` on a non-loopback host and [`ProviderError::Transport`] for a
    /// base that is not an admissible URL.
    pub fn with_bases(anthropic_base: &str, codex_base: &str) -> Result<Self, ProviderError> {
        Ok(Self {
            anthropic: endpoint(Family::Anthropic, anthropic_base, ANTHROPIC_TOKEN_PATH)?,
            openai_codex: endpoint(Family::Codex, codex_base, CODEX_TOKEN_PATH)?,
        })
    }

    /// The token endpoint of `provider`.
    #[must_use]
    pub const fn url(&self, provider: OAuthProvider) -> &Url {
        match provider {
            OAuthProvider::Anthropic => &self.anthropic,
            OAuthProvider::OpenAiCodex => &self.openai_codex,
        }
    }
}

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
        let response = send(family, request, &self.user_agent, exchange, tokio::time::sleep)
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
fn commit(
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
fn fresh_credential(
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
fn rejection(provider: OAuthProvider, status: u16, body: &[u8], stored: &OAuthCredential) -> Failure {
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
        error.and_then(|error| error.get("code")).and_then(JsonValueTrait::as_str),
        error.and_then(|error| error.get("type")).and_then(JsonValueTrait::as_str),
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

/// Takes the exclusive advisory lock on the `auth.json.lock` sibling of
/// `auth_path`, polling without blocking the runtime. Dropping the returned
/// file releases the lock; the lock file stays. Every writer of `auth.json`
/// (refresh, login, logout) holds this lock across its reload, `set`, and
/// `store`, so none overwrites another's entry.
pub(crate) async fn lock_auth_file(auth_path: &Path) -> Result<File, ProviderError> {
    let lock_path = lock_path(auth_path)?;
    let open_path = lock_path.clone();
    let file = blocking(move || {
        open_lock_file(&open_path).map_err(|error| ProviderError::AuthWrite {
            reason: format!("could not open {}: {error}", open_path.display()),
        })
    })
    .await?;
    let deadline = tokio::time::Instant::now() + LOCK_WAIT;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(LOCK_POLL).await;
            }
            Err(TryLockError::WouldBlock) => {
                return Err(ProviderError::AuthWrite {
                    reason: format!(
                        "{} stayed locked by another process for {} s",
                        lock_path.display(),
                        LOCK_WAIT.as_secs()
                    ),
                });
            }
            Err(TryLockError::Error(error)) => {
                return Err(ProviderError::AuthWrite {
                    reason: format!("could not lock {}: {error}", lock_path.display()),
                });
            }
        }
    }
}

/// Runs file work on the blocking pool; the work runs to its end even when
/// the awaiting future is dropped.
pub(crate) async fn blocking<T, F>(work: F) -> Result<T, ProviderError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, ProviderError> + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| ProviderError::AuthWrite {
            reason: format!("the auth.json task failed: {error}"),
        })?
}

fn lock_path(auth_path: &Path) -> Result<PathBuf, ProviderError> {
    let Some(name) = auth_path.file_name() else {
        return Err(ProviderError::AuthWrite {
            reason: format!("{} names no file", auth_path.display()),
        });
    };
    let mut lock_name = name.to_os_string();
    lock_name.push(".lock");
    Ok(auth_path.with_file_name(lock_name))
}

fn open_lock_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use std::{
        cell::RefCell,
        rc::Rc,
        fs,
        io::ErrorKind,
        pin::pin,
        sync::atomic::{AtomicU32, Ordering},
        time::Instant,
    };

    use futures::future::{Either, join_all, select};
    use tokio::{
        net::{TcpListener, TcpStream},
        sync::Notify,
    };

    use super::*;

    const OLD_ACCESS: &str = "old-access-secret";
    const OLD_REFRESH: &str = "old-refresh-secret";
    const NEW_ACCESS: &str = "new-access-secret";
    const NEW_REFRESH: &str = "new-refresh-secret";
    const NEW_TOKENS: &str = r#"{"access_token":"new-access-secret","refresh_token":"new-refresh-secret","expires_in":3600,"token_type":"Bearer"}"#;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "dal-provider-refresh-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }

        fn auth(&self) -> PathBuf {
            self.0.join("auth.json")
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    enum Reply {
        Json(u16, String),
        /// Holds the connection open without answering, then signals.
        Stall(Rc<Notify>),
    }

    struct Seen {
        at: Instant,
        head: String,
        body: String,
    }

    fn codex(access: &str, refresh: &str, expires_at: Option<i64>) -> OAuthCredential {
        OAuthCredential {
            access_token: SecretString::from(access),
            refresh_token: SecretString::from(refresh),
            expires_at,
            id_token: Some(String::from("old-id-token")),
            account_id: Some(String::from("acct-1")),
        }
    }

    fn seed(path: &Path, credential: OAuthCredential) {
        let mut store = AuthStore::empty(path);
        store
            .set("openai-codex", Credential::OAuth(credential))
            .expect("codex takes oauth");
        store.store().expect("seed auth.json");
    }

    fn stored(path: &Path) -> OAuthCredential {
        match AuthStore::load(path).expect("reload").credential("openai-codex") {
            Some(Credential::OAuth(credential)) => credential,
            other => panic!("expected an OAuth entry, got {other:?}"),
        }
    }

    fn oauth(credential: Credential) -> OAuthCredential {
        match credential {
            Credential::OAuth(credential) => credential,
            other => panic!("expected OAuth, got {other:?}"),
        }
    }

    fn refresher(path: &Path, base: &str) -> Refresher {
        let endpoints = TokenEndpoints::with_bases(base, base).expect("loopback base");
        Refresher::new("dalgon/test", path, endpoints)
    }

    async fn listen() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        (listener, format!("http://127.0.0.1:{port}"))
    }

    async fn read_request(stream: &TcpStream) -> (String, String) {
        let mut data = Vec::new();
        let mut chunk = [0_u8; 4096];
        loop {
            if let Some(end) = data.windows(4).position(|window| window == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&data[..end]).into_owned();
                let length = head
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .map_or(0, |(_, value)| value.trim().parse().expect("content length"));
                if data.len() >= end + 4 + length {
                    let body = String::from_utf8_lossy(&data[end + 4..end + 4 + length]);
                    return (head, body.into_owned());
                }
            }
            stream.readable().await.expect("readable");
            match stream.try_read(&mut chunk) {
                Ok(0) => panic!("client closed before the request ended"),
                Ok(read) => data.extend_from_slice(&chunk[..read]),
                Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                Err(error) => panic!("read request: {error}"),
            }
        }
    }

    async fn write_response(stream: &TcpStream, status: u16, body: &str) {
        let text = format!(
            "HTTP/1.1 {status} Scripted\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let mut bytes = text.as_bytes();
        while !bytes.is_empty() {
            stream.writable().await.expect("writable");
            match stream.try_write(bytes) {
                Ok(written) => bytes = &bytes[written..],
                Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                Err(error) => panic!("write response: {error}"),
            }
        }
    }

    async fn serve(listener: TcpListener, replies: Vec<Reply>, seen: &RefCell<Vec<Seen>>) {
        let mut stalled = Vec::new();
        for reply in replies {
            let (stream, _) = listener.accept().await.expect("accept");
            let (head, body) = read_request(&stream).await;
            seen.borrow_mut().push(Seen {
                at: Instant::now(),
                head,
                body,
            });
            match reply {
                Reply::Json(status, body) => write_response(&stream, status, &body).await,
                Reply::Stall(signal) => {
                    stalled.push(stream);
                    signal.notify_one();
                }
            }
        }
        std::future::pending::<()>().await;
    }

    /// Runs `client` on this task while the loopback server answers with
    /// `replies` in order; returns the client output and every request seen.
    async fn with_server<T>(
        listener: TcpListener,
        replies: Vec<Reply>,
        client: impl Future<Output = T>,
    ) -> (T, Vec<Seen>) {
        let seen = RefCell::new(Vec::new());
        let output = {
            let server = pin!(serve(listener, replies, &seen));
            match select(pin!(client), server).await {
                Either::Left((output, _)) => output,
                Either::Right(((), _)) => panic!("the server stopped"),
            }
        };
        (output, seen.into_inner())
    }

    fn assert_no_secret(error: &ProviderError, secrets: &[&str]) {
        let texts = [format!("{error}"), format!("{error:?}")];
        for text in &texts {
            for secret in secrets {
                assert!(!text.contains(secret), "{text:?} leaks a secret");
            }
        }
    }

    #[tokio::test]
    async fn sixty_four_callers_across_two_refreshers_send_one_request() {
        let dir = TestDir::new("race");
        let held = codex(OLD_ACCESS, OLD_REFRESH, Some(unix_now() + 10));
        seed(&dir.auth(), held.clone());
        let (listener, base) = listen().await;
        // Two refreshers of one file stand in for two processes: separate
        // key mutexes, one advisory lock per open of auth.json.lock.
        let first = refresher(&dir.auth(), &base);
        let second = refresher(&dir.auth(), &base);
        let callers = (0..64).map(|index| {
            let refresher = if index % 2 == 0 { &first } else { &second };
            refresher.refresh(OAuthProvider::OpenAiCodex, &held, RefreshReason::Expiring)
        });
        let replies = vec![Reply::Json(200, String::from(NEW_TOKENS))];
        let (results, seen) = with_server(listener, replies, join_all(callers)).await;

        assert_eq!(seen.len(), 1);
        assert!(seen[0].head.starts_with("POST /oauth/token HTTP/1.1"));
        let body = sonic_rs::from_str::<sonic_rs::Value>(&seen[0].body).expect("JSON body");
        assert_eq!(body.get("grant_type").and_then(JsonValueTrait::as_str), Some("refresh_token"));
        assert_eq!(body.get("client_id").and_then(JsonValueTrait::as_str), Some(CODEX_CLIENT_ID));
        assert_eq!(body.get("refresh_token").and_then(JsonValueTrait::as_str), Some(OLD_REFRESH));
        for result in results {
            let fresh = oauth(result.expect("every caller gets the new token"));
            assert_eq!(fresh.access_token.expose(), NEW_ACCESS);
        }
        let on_disk = stored(&dir.auth());
        assert_eq!(on_disk.access_token.expose(), NEW_ACCESS);
        assert_eq!(on_disk.refresh_token.expose(), NEW_REFRESH);
        assert_eq!(on_disk.id_token.as_deref(), Some("old-id-token"));
        assert_eq!(on_disk.account_id.as_deref(), Some("acct-1"));
        let expires_at = on_disk.expires_at.expect("expires_in sets expires_at");
        assert!((expires_at - unix_now() - 3600).abs() <= 5);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.auth()).expect("stat").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[tokio::test]
    async fn external_writer_before_the_lock_prevents_any_request() {
        let dir = TestDir::new("guarded");
        let held = codex(OLD_ACCESS, OLD_REFRESH, Some(unix_now() + 10));
        seed(&dir.auth(), held.clone());
        let (listener, base) = listen().await;
        let refresher = refresher(&dir.auth(), &base);
        // Another process holds the lock while it writes a fresh token.
        let external = open_lock_file(&lock_path(&dir.auth()).expect("lock path")).expect("open lock");
        external.try_lock().expect("external lock");
        let client = async {
            let expiring = refresher.refresh(OAuthProvider::OpenAiCodex, &held, RefreshReason::Expiring);
            let rejected = refresher.refresh(OAuthProvider::OpenAiCodex, &held, RefreshReason::Rejected);
            let writer = async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                seed(&dir.auth(), codex(NEW_ACCESS, NEW_REFRESH, Some(unix_now() + 3600)));
                drop(external);
            };
            let (expiring, rejected, ()) = tokio::join!(expiring, rejected, writer);
            (expiring, rejected)
        };
        let ((expiring, rejected), seen) = with_server(listener, Vec::new(), client).await;

        assert!(seen.is_empty());
        for result in [expiring, rejected] {
            assert_eq!(oauth(result.expect("stored token")).access_token.expose(), NEW_ACCESS);
        }
    }

    #[tokio::test]
    async fn forced_refresh_ignores_the_window_only_for_an_unchanged_token() {
        let dir = TestDir::new("forced");
        let held = codex(OLD_ACCESS, OLD_REFRESH, Some(unix_now() + 3600));
        seed(&dir.auth(), held.clone());
        let (listener, base) = listen().await;
        let refresher = refresher(&dir.auth(), &base);
        let client = async {
            let early = refresher
                .refresh(OAuthProvider::OpenAiCodex, &held, RefreshReason::Expiring)
                .await;
            let forced = refresher
                .refresh(OAuthProvider::OpenAiCodex, &held, RefreshReason::Rejected)
                .await;
            // A second 401 on the old token finds the changed token: no request.
            let again = refresher
                .refresh(OAuthProvider::OpenAiCodex, &held, RefreshReason::Rejected)
                .await;
            (early, forced, again)
        };
        let replies = vec![Reply::Json(200, String::from(NEW_TOKENS))];
        let ((early, forced, again), seen) = with_server(listener, replies, client).await;

        assert_eq!(seen.len(), 1);
        assert_eq!(oauth(early.expect("fresh token")).access_token.expose(), OLD_ACCESS);
        assert_eq!(oauth(forced.expect("forced refresh")).access_token.expose(), NEW_ACCESS);
        assert_eq!(oauth(again.expect("changed token")).access_token.expose(), NEW_ACCESS);
    }

    #[tokio::test]
    async fn transient_failure_retries_once_after_one_second() {
        let dir = TestDir::new("transient");
        let held = codex(OLD_ACCESS, OLD_REFRESH, Some(unix_now() + 10));
        seed(&dir.auth(), held.clone());
        let (listener, base) = listen().await;
        let refresher = refresher(&dir.auth(), &base);
        let replies = vec![
            Reply::Json(503, String::from(r#"{"error":"temporarily_unavailable"}"#)),
            Reply::Json(200, String::from(NEW_TOKENS)),
        ];
        let client = refresher.refresh(OAuthProvider::OpenAiCodex, &held, RefreshReason::Expiring);
        let (result, seen) = with_server(listener, replies, client).await;

        assert_eq!(oauth(result.expect("retry succeeds")).access_token.expose(), NEW_ACCESS);
        assert_eq!(seen.len(), 2);
        assert!(seen[1].at.duration_since(seen[0].at) >= RETRY_DELAY);
        assert_eq!(stored(&dir.auth()).access_token.expose(), NEW_ACCESS);
    }

    #[tokio::test]
    async fn repeated_transient_failure_gives_status_without_secrets() {
        let dir = TestDir::new("twice");
        let held = codex(OLD_ACCESS, OLD_REFRESH, Some(unix_now() + 10));
        seed(&dir.auth(), held.clone());
        let before = fs::read(dir.auth()).expect("read seed");
        let (listener, base) = listen().await;
        let refresher = refresher(&dir.auth(), &base);
        let echo = format!("backend down for {OLD_REFRESH}");
        let replies = vec![Reply::Json(503, echo.clone()), Reply::Json(503, echo)];
        let client = refresher.refresh(OAuthProvider::OpenAiCodex, &held, RefreshReason::Expiring);
        let (result, seen) = with_server(listener, replies, client).await;

        let error = result.expect_err("two 503 fail");
        assert!(matches!(
            &error,
            ProviderError::Status { family: Family::Codex, status: 503, message }
                if message == "backend down for <redacted>"
        ));
        assert_no_secret(&error, &[OLD_REFRESH, OLD_ACCESS]);
        assert_eq!(seen.len(), 2);
        assert_eq!(fs::read(dir.auth()).expect("read after"), before);
    }

    #[tokio::test]
    async fn rejected_refresh_token_is_sign_in_expired_after_one_request() {
        for body in [
            r#"{"error":"invalid_grant","error_description":"bad"}"#,
            r#"{"error":{"message":"expired","type":"invalid_request_error","code":"refresh_token_expired"}}"#,
            r#"{"error":{"code":"refresh_token_reused"}}"#,
            r#"{"error":"refresh_token_invalidated"}"#,
        ] {
            let dir = TestDir::new("permanent");
            let held = codex(OLD_ACCESS, OLD_REFRESH, Some(unix_now() + 10));
            seed(&dir.auth(), held.clone());
            let before = fs::read(dir.auth()).expect("read seed");
            let (listener, base) = listen().await;
            let refresher = refresher(&dir.auth(), &base);
            let replies = vec![Reply::Json(400, String::from(body))];
            let client = refresher.refresh(OAuthProvider::OpenAiCodex, &held, RefreshReason::Rejected);
            let (result, seen) = with_server(listener, replies, client).await;

            let error = result.expect_err("rejected refresh token");
            assert_eq!(error.to_string(), "openai-codex sign-in expired: the refresh token was rejected.");
            assert_no_secret(&error, &[OLD_REFRESH, OLD_ACCESS]);
            assert_eq!(seen.len(), 1, "{body}");
            assert_eq!(fs::read(dir.auth()).expect("read after"), before);
        }
    }

    #[tokio::test]
    async fn cancelled_refresh_releases_locks_and_keeps_the_prior_credential() {
        let dir = TestDir::new("cancel");
        let held = codex(OLD_ACCESS, OLD_REFRESH, Some(unix_now() + 10));
        seed(&dir.auth(), held.clone());
        let before = fs::read(dir.auth()).expect("read seed");
        let (listener, base) = listen().await;
        let refresher = refresher(&dir.auth(), &base);
        let stalled = Rc::new(Notify::new());
        let client = async {
            // Cancel only once the server holds the request: mid-request.
            tokio::select! {
                _ = refresher.refresh(OAuthProvider::OpenAiCodex, &held, RefreshReason::Expiring) => {
                    panic!("the stalled refresh must not finish");
                }
                () = stalled.notified() => {}
            }
            assert_eq!(fs::read(dir.auth()).expect("read after cancel"), before);
            let probe = open_lock_file(&lock_path(&dir.auth()).expect("lock path")).expect("open lock");
            probe.try_lock().expect("the file lock was released");
            drop(probe);
            refresher
                .refresh(OAuthProvider::OpenAiCodex, &held, RefreshReason::Expiring)
                .await
        };
        let replies = vec![Reply::Stall(Rc::clone(&stalled)), Reply::Json(200, String::from(NEW_TOKENS))];
        let (result, seen) = with_server(listener, replies, client).await;

        assert_eq!(oauth(result.expect("next caller refreshes")).access_token.expose(), NEW_ACCESS);
        assert_eq!(seen.len(), 2);
        assert_eq!(stored(&dir.auth()).access_token.expose(), NEW_ACCESS);
    }

    #[test]
    fn cancelled_commit_writes_nothing() {
        let dir = TestDir::new("commit");
        let held = codex(OLD_ACCESS, OLD_REFRESH, Some(unix_now() + 10));
        seed(&dir.auth(), held);
        let before = fs::read(dir.auth()).expect("read seed");
        let mut store = AuthStore::load(dir.auth()).expect("load");
        let cancel = CancellationToken::new();
        cancel.cancel();
        let fresh = Credential::OAuth(codex(NEW_ACCESS, NEW_REFRESH, Some(unix_now() + 3600)));
        let error = commit(&cancel, &mut store, "openai-codex", fresh.clone())
            .expect_err("a cancelled commit is refused");
        assert!(matches!(error, ProviderError::AuthWrite { .. }));
        assert_eq!(fs::read(dir.auth()).expect("read after"), before);

        commit(&CancellationToken::new(), &mut store, "openai-codex", fresh).expect("live commit");
        assert_eq!(stored(&dir.auth()).access_token.expose(), NEW_ACCESS);
    }

    #[test]
    fn production_endpoints_are_the_documented_urls() {
        let endpoints = TokenEndpoints::production();
        assert_eq!(
            endpoints.url(OAuthProvider::Anthropic).as_str(),
            "https://platform.claude.com/v1/oauth/token"
        );
        assert_eq!(
            endpoints.url(OAuthProvider::OpenAiCodex).as_str(),
            "https://auth.openai.com/oauth/token"
        );
    }

    #[test]
    fn anthropic_refresh_keeps_no_codex_identity_and_old_refresh_token_when_omitted() {
        let stored = OAuthCredential {
            access_token: SecretString::from(OLD_ACCESS),
            refresh_token: SecretString::from(OLD_REFRESH),
            expires_at: Some(1),
            id_token: None,
            account_id: None,
        };
        let body = br#"{"access_token":"a2","expires_in":60}"#;
        let fresh = fresh_credential(OAuthProvider::Anthropic, &stored, body, 1_000).expect("valid body");
        assert_eq!(fresh.access_token.expose(), "a2");
        assert_eq!(fresh.refresh_token.expose(), OLD_REFRESH);
        assert_eq!(fresh.expires_at, Some(1_060));
        assert_eq!((fresh.id_token, fresh.account_id), (None, None));
        let error = fresh_credential(OAuthProvider::Anthropic, &stored, br#"{"access_token":"#, 0)
            .expect_err("truncated body");
        assert!(matches!(error, ProviderError::Transport { family: Family::Anthropic, .. }));
    }
}
