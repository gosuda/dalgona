//! Host-driven OAuth login for the table's OAuth providers.
//!
//! A flow owns PKCE and state, accepts either a loopback browser redirect or
//! one explicit paste value, and makes the durable credential write its last
//! authentication step. Network secrets are wrapped in [`SecretString`] as soon
//! as the token response is decoded; server messages are redacted before they
//! enter a typed error.

use std::{
    fmt,
    fmt::Write as _,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use dal_core::Family;
use reqwest::{Client, header::CONTENT_TYPE};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    time::{Instant, sleep, timeout_at},
};
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;

use super::device::DeviceOutcome;
use crate::{
    AuthStore, AuthStyle, Completion, Credential, EnvSnapshot, Hook, OAuthCredential, OAuthSpec,
    PROVIDERS, ProviderDef, ProviderEntry, ProviderError, ProviderSettings, QueryValue, Redirect,
    SecretString, Transport,
    auth::credential::{codex_identity, oauth_expires_at, oauth_issued_at},
    family::gitlab::{DirectAccessCache, PROVIDER_ID as GITLAB_PROVIDER_ID},
    find,
    http::{
        CONNECT_TIMEOUT, Exchange, LOGIN_WAIT, OAUTH_TIMEOUT, STREAM_IDLE_TIMEOUT, check_base_url,
        endpoint, read_body, send,
    },
};

/// The login-flow originator sent to `OpenAI`'s authorization endpoint.
pub(crate) const CODEX_ORIGINATOR: &str = "dalgon";

const CALLBACK_REQUEST_LIMIT: usize = 16 * 1024;
const RESPONSE_HTML: &[u8] = b"<!doctype html><title>Sign-in complete</title><p>Sign-in complete. You can return to dalgon.</p>";
const RESPONSE_BAD_REQUEST: &[u8] = b"<!doctype html><title>Sign-in failed</title><p>Sign-in failed. Return to dalgon and try again.</p>";

/// Host-visible progress for a login flow.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LoginProgress {
    /// Open this authorization URL in a browser.
    OpenUrl {
        /// The complete authorization URL.
        url: String,
    },
    /// Show this device code and verification URL to the user.
    ShowCode {
        /// The verification URL.
        url: String,
        /// The short-lived code to enter at that URL.
        code: String,
    },
    /// Ask the user to paste the redirect URL or authorization code.
    AskPaste {
        /// Instructions for the host's input surface.
        hint: String,
        /// Fallback answer used when the user submits an empty line.
        ///
        /// When `None`, the prompt requires a nonempty answer and empty inputs
        /// are rejected or retried. When `Some(value)`, an empty submission
        /// accepts `value` as the answer.
        default: Option<String>,
    },
    /// The authorization code is being exchanged for durable credentials.
    Exchanging,
}

/// Exact prompt text for a host that needs a user-pasted authorization result.
pub const PASTE_HINT: &str = "Paste the redirect URL or the code shown in the browser.";

/// OAuth endpoints for the table's sign-in flows, one set per OAuth row.
///
/// Production values are the rows' pinned HTTPS URLs. Integration tests
/// can use [`LoginEndpoints::loopback`] to direct every OAuth request to a
/// literal loopback server; arbitrary HTTPS hosts are never admitted because
/// the authorization code and verifier are sent to these endpoints.
#[derive(Clone, Debug)]
pub struct LoginEndpoints {
    rows: Vec<RowEndpoints>,
    loopback_override: bool,
    copilot: CopilotEndpoints,
}

/// Private endpoint mapping for Copilot device login, discovery, and policy.
#[derive(Clone, Debug)]
pub(crate) struct CopilotEndpoints {
    /// In production: None (formats <https://{target.host}>).
    /// In loopback: `Some(loopback_base_url)`.
    pub(crate) device: Option<Url>,
    /// In production: <https://api.github.com>.
    /// In loopback: `loopback_base_url`.
    pub(crate) discovery: Url,
    /// In production: None.
    /// In loopback: `Some(loopback_base_url)`.
    pub(crate) policy: Option<Url>,
}

impl CopilotEndpoints {
    fn production() -> Result<Self, ProviderError> {
        let discovery = Url::parse("https://api.github.com")
            .map_err(|error| endpoint_error(Family::Chat, error.to_string()))?;
        Ok(Self {
            device: None,
            discovery,
            policy: None,
        })
    }

    fn loopback(base: &Url) -> Self {
        Self {
            device: Some(base.clone()),
            discovery: base.clone(),
            policy: Some(base.clone()),
        }
    }

    fn validate_production(&self) -> Result<(), ProviderError> {
        if self.discovery.scheme() != "https" || self.discovery.host_str() != Some("api.github.com")
        {
            return Err(endpoint_error(
                Family::Chat,
                "Copilot discovery endpoint must be https://api.github.com in production",
            ));
        }
        if self.device.is_some() || self.policy.is_some() {
            return Err(endpoint_error(
                Family::Chat,
                "Copilot override endpoints must be None in production",
            ));
        }
        Ok(())
    }

    fn validate_loopback(&self) -> Result<(), ProviderError> {
        if !is_loopback(&self.discovery) {
            return Err(endpoint_error(
                Family::Chat,
                "Copilot discovery base must use a literal loopback host in tests",
            ));
        }
        if let Some(device) = &self.device
            && !is_loopback(device)
        {
            return Err(endpoint_error(
                Family::Chat,
                "Copilot device base must use a literal loopback host in tests",
            ));
        }
        if let Some(policy) = &self.policy
            && !is_loopback(policy)
        {
            return Err(endpoint_error(
                Family::Chat,
                "Copilot policy base must use a literal loopback host in tests",
            ));
        }
        Ok(())
    }
}

/// The endpoints, effective client id, and callback port of one OAuth row.
#[derive(Clone, Debug)]
pub(crate) struct RowEndpoints {
    pub(crate) def: &'static ProviderDef,
    pub(crate) oauth: OAuthSpec,
    /// The client id every request of this sign-in carries: the trimmed value
    /// of the row's override variable in the captured environment when that
    /// is not blank, else the row's pinned id. The authorize URL, the code
    /// exchange, and the revoke request all read this one value.
    pub(crate) client_id: Box<str>,
    /// The redirect URI the captured environment supplies in place of the
    /// row's registered one, for the rows that honor one.
    redirect_override: Option<RedirectOverride>,
    pub(crate) authorize: Url,
    pub(crate) token: Url,
    pub(crate) device: Option<DeviceUrls>,
    pub(crate) revoke: Option<Url>,
    pub(crate) models_base: Option<Url>,
    /// The base the sign-in's session is verified against before it is
    /// stored; `None` for the rows that verify nothing.
    pub(crate) session_base: Option<Url>,
    pub(crate) callback_port: u16,
}

/// A redirect URI taken from the captured environment, kept as supplied
/// (trimmed) and parsed only when a browser sign-in needs it, so a malformed
/// value fails that one sign-in instead of every endpoint set.
///
/// `Debug` names the variable and never prints the value, as
/// [`EnvSnapshot`]'s does: the text is unvalidated until a sign-in parses it
/// and could still carry credentials.
#[derive(Clone)]
struct RedirectOverride {
    /// The environment variable that supplied the value.
    variable: &'static str,
    /// The value, advertised verbatim.
    uri: Box<str>,
}

impl fmt::Debug for RedirectOverride {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RedirectOverride")
            .field("variable", &self.variable)
            .finish_non_exhaustive()
    }
}

/// The device-code endpoints of one OAuth row.
#[derive(Clone, Debug)]
pub(crate) struct DeviceUrls {
    pub(crate) usercode: Url,
    pub(crate) poll: Url,
    pub(crate) page: Url,
    pub(crate) redirect_uri: &'static str,
}

impl RowEndpoints {
    /// Places every URL of the row with `locate` and resolves its client id
    /// and redirect override from the captured environment: production keeps
    /// the table's URLs, the loopback seam moves them under one local server.
    fn build(
        def: &'static ProviderDef,
        oauth: OAuthSpec,
        callback_port: u16,
        env: &EnvSnapshot,
        locate: &dyn Fn(&str) -> Result<Url, ProviderError>,
    ) -> Result<Self, ProviderError> {
        let device = oauth
            .device
            .map(|device| -> Result<DeviceUrls, ProviderError> {
                Ok(DeviceUrls {
                    usercode: locate(device.usercode_url)?,
                    poll: locate(device.poll_url)?,
                    page: locate(device.page_url)?,
                    redirect_uri: device.redirect_uri,
                })
            })
            .transpose()?;
        Ok(Self {
            def,
            oauth,
            client_id: effective_client_id(&oauth, env).into(),
            redirect_override: redirect_override(oauth.hook, env),
            authorize: locate(oauth.authorize_url)?,
            token: locate(oauth.token_url)?,
            device,
            revoke: oauth.revoke_url.map(locate).transpose()?,
            models_base: fetches_models(oauth.hook)
                .then(|| locate(def.base_url))
                .transpose()?,
            session_base: verifies_session(oauth.hook)
                .then(|| locate(def.base_url))
                .transpose()?,
            callback_port,
        })
    }

    fn validate(&self) -> Result<(), ProviderError> {
        let family = self.def.family;
        let oauth = &self.oauth;
        validate_endpoint(family, &self.authorize, &host_of(oauth.authorize_url))?;
        validate_endpoint(family, &self.token, &host_of(oauth.token_url))?;
        if let (Some(urls), Some(device)) = (&self.device, oauth.device) {
            validate_endpoint(family, &urls.usercode, &host_of(device.usercode_url))?;
            validate_endpoint(family, &urls.poll, &host_of(device.poll_url))?;
            validate_endpoint(family, &urls.page, &host_of(device.page_url))?;
        }
        if let (Some(url), Some(text)) = (&self.revoke, oauth.revoke_url) {
            validate_endpoint(family, url, &host_of(text))?;
        }
        for url in [&self.models_base, &self.session_base]
            .into_iter()
            .flatten()
        {
            validate_endpoint(family, url, &host_of(self.def.base_url))?;
        }
        Ok(())
    }
}

impl LoginEndpoints {
    /// The pinned production endpoints, with each row's client id and
    /// redirect override resolved from the captured environment `env`.
    pub(crate) fn production(env: &EnvSnapshot) -> Result<Self, ProviderError> {
        let mut rows = Vec::new();
        for def in PROVIDERS {
            let Some(oauth) = def.oauth else { continue };
            let locate = |text: &str| {
                Url::parse(text).map_err(|error| endpoint_error(def.family, error.to_string()))
            };
            rows.push(RowEndpoints::build(
                def,
                oauth,
                oauth.redirect.port,
                env,
                &locate,
            )?);
        }
        let endpoints = Self {
            rows,
            loopback_override: false,
            copilot: CopilotEndpoints::production()?,
        };
        endpoints.validate()?;
        Ok(endpoints)
    }

    /// Builds an endpoint set whose network requests all target `base_url`.
    ///
    /// This test seam accepts only `http` or `https` on the literal loopback
    /// hosts `127.0.0.1`, `::1`, or `localhost`. The callback ports default to
    /// zero so each test can bind an available ephemeral port. Each row's
    /// client id and redirect override come from the captured environment
    /// `env`, exactly as for the production endpoints.
    ///
    /// # Errors
    /// Returns a typed transport error if `base_url` is invalid or not loopback.
    pub fn loopback(base_url: &str, env: &EnvSnapshot) -> Result<Self, ProviderError> {
        let base = Url::parse(base_url).map_err(|error| {
            endpoint_error(Family::Chat, format!("loopback base is invalid: {error}"))
        })?;
        check_base_url(Family::Chat, base_url)?;
        if !is_loopback(&base) {
            return Err(endpoint_error(
                Family::Chat,
                "test OAuth endpoints must use a literal loopback host",
            ));
        }
        let mut rows = Vec::new();
        for def in PROVIDERS {
            let Some(oauth) = def.oauth else { continue };
            let prefix = loopback_prefix(oauth.hook);
            let locate = |production: &str| {
                let path = Url::parse(production)
                    .map_err(|error| endpoint_error(def.family, error.to_string()))?
                    .path()
                    .trim_start_matches('/')
                    .to_owned();
                endpoint(def.family, base_url, &loopback_path(prefix, &path))
            };
            rows.push(RowEndpoints::build(def, oauth, 0, env, &locate)?);
        }
        Ok(Self {
            rows,
            loopback_override: true,
            copilot: CopilotEndpoints::loopback(&base),
        })
    }

    /// Sets the loopback callback ports, one for the Codex sign-in and one for
    /// the Claude sign-in; zero asks the OS to choose a free port. The other
    /// rows keep the port they were built with.
    #[must_use]
    pub fn with_callback_ports(mut self, codex: u16, claude: u16) -> Self {
        for row in &mut self.rows {
            row.callback_port = match row.oauth.hook {
                Hook::CodexAccount => codex,
                Hook::ClaudeCode => claude,
                Hook::WorkOsDevice | Hook::GitLabDuo => row.callback_port,
            };
        }
        self
    }

    pub(crate) fn row(&self, def: &ProviderDef) -> Option<&RowEndpoints> {
        self.rows.iter().find(|row| row.def.id == def.id)
    }

    pub(crate) fn validate(&self) -> Result<(), ProviderError> {
        self.rows.iter().try_for_each(RowEndpoints::validate)?;
        if self.loopback_override {
            self.copilot.validate_loopback()
        } else {
            self.copilot.validate_production()
        }
    }

    pub(crate) fn copilot(&self) -> &CopilotEndpoints {
        &self.copilot
    }
}

/// A single OAuth sign-in attempt.
///
/// The store and cache paths are supplied by the host. The callback receives
/// browser redirects on the bound loopback listener; a callback-only row falls
/// back to device polling when its port is busy. For a paste row, call
/// [`LoginFlow::take_paste_sender`] before `run` and send one value from the
/// host's input surface. Cancellation drops the flow's one-shot receiver; if
/// the host took the sender, it is disconnected and the host should drop it.
/// No background task survives cancellation.
pub struct LoginFlow<'a> {
    def: &'static ProviderDef,
    store: &'a mut AuthStore,
    client: Client,
    oauth_client: Client,
    user_agent: String,
    cache_dir: PathBuf,
    endpoints: LoginEndpoints,
    gitlab_grants: Option<Arc<DirectAccessCache>>,
    models_provider: Option<ProviderEntry>,
    paste_sender: Option<oneshot::Sender<String>>,
    paste_receiver: Option<oneshot::Receiver<String>>,
    wait: Duration,
    device_auth: bool,
    started: bool,
    #[cfg(test)]
    commit_ready: Option<oneshot::Sender<()>>,
}

impl fmt::Debug for LoginFlow<'_> {
    // Intentionally omits `gitlab_grants`: the cache is keyed by raw tokens
    // and must never format.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginFlow")
            .field("provider", &self.def.id)
            .field("auth_path", &self.store.path())
            .field("cache_dir", &self.cache_dir)
            .field("started", &self.started)
            .finish_non_exhaustive()
    }
}

impl<'a> LoginFlow<'a> {
    /// Creates a flow for a table provider that has an OAuth row.
    ///
    /// The supplied client is used only for the post-login catalog fetch.
    /// Credential-bearing OAuth calls use a private client with redirects
    /// disabled, so a 307/308 cannot forward codes, verifiers, or refresh tokens.
    /// `cache_dir` is the host's separate model-cache directory. `endpoints`
    /// carries the pinned HTTPS or literal-loopback URLs and the effective
    /// client ids of every row; arbitrary remote hosts are rejected so
    /// authorization codes and verifiers cannot be sent to a caller-selected
    /// origin.
    ///
    /// # Errors
    /// Returns [`ProviderError::AuthWrite`] for an unsupported provider, or a
    /// transport error if an endpoint violates the origin allowlist or URL
    /// rules, or the private OAuth client cannot be constructed.
    pub fn new(
        provider: &str,
        store: &'a mut AuthStore,
        client: Client,
        user_agent: impl Into<String>,
        cache_dir: impl Into<PathBuf>,
        endpoints: LoginEndpoints,
    ) -> Result<Self, ProviderError> {
        let Some(def) = find(provider).filter(|def| def.oauth.is_some()) else {
            return Err(ProviderError::AuthWrite {
                reason: format!("{provider} has no OAuth login flow"),
            });
        };
        endpoints.validate()?;
        let oauth_client = build_oauth_client(def.family)?;
        let (paste_sender, paste_receiver) = oneshot::channel();
        Ok(Self {
            def,
            store,
            client,
            oauth_client,
            user_agent: user_agent.into(),
            cache_dir: cache_dir.into(),
            endpoints,
            gitlab_grants: None,
            models_provider: None,
            paste_sender: Some(paste_sender),
            paste_receiver: Some(paste_receiver),
            wait: LOGIN_WAIT,
            device_auth: false,
            started: false,
            #[cfg(test)]
            commit_ready: None,
        })
    }

    /// Selects the device-code flow without trying the browser callback.
    #[must_use]
    pub fn with_device_auth(mut self) -> Self {
        self.device_auth = true;
        self
    }

    /// Replaces production endpoints with pinned HTTPS or literal-loopback URLs.
    ///
    /// A loopback override is intended for replay servers; arbitrary remote
    /// hosts are rejected so authorization codes and verifiers cannot be sent
    /// to a caller-selected origin.
    ///
    /// # Errors
    /// Returns a typed transport error when an endpoint violates the origin
    /// allowlist or URL rules.
    pub fn with_endpoints(mut self, endpoints: LoginEndpoints) -> Result<Self, ProviderError> {
        endpoints.validate()?;
        self.endpoints = endpoints;
        Ok(self)
    }

    /// Attaches the shared GitLab grant cache for post-commit invalidation.
    ///
    /// `None` (standalone flows) disables invalidation and preserves existing
    /// behavior. The host supplies `Some` via `LoginSite::with_provider_set`.
    /// No allocation, no global, no default no-op: a missing handle means
    /// there is no associated provider cache, not a skipped step.
    pub(crate) fn with_gitlab_grants(mut self, grants: Option<Arc<DirectAccessCache>>) -> Self {
        self.gitlab_grants = grants;
        self
    }

    /// Uses the host's configured built-in route of this flow's provider for
    /// the post-login model fetch.
    ///
    /// For a loopback endpoint override, the override's model API base takes
    /// precedence so replay tests do not contact the live service.
    ///
    /// # Errors
    /// Returns [`ProviderError::AuthWrite`] when `provider` is not the flow's
    /// built-in route, or a typed URL error for an invalid base URL.
    pub fn with_codex_provider(mut self, provider: ProviderEntry) -> Result<Self, ProviderError> {
        if provider.id.as_ref() != self.def.id || provider.family != self.def.family {
            return Err(ProviderError::AuthWrite {
                reason: format!("post-login model fetch requires the {} route", self.def.id),
            });
        }
        check_base_url(self.def.family, &provider.base_url)?;
        self.models_provider = Some(provider);
        Ok(self)
    }

    /// Takes the sole input sender for paste login.
    ///
    /// The sender is available once. Sending one value completes the paste
    /// input. Dropping it closes the paste path; a bound browser callback can
    /// still complete the flow, but a paste-only flow returns
    /// [`ProviderError::LoginCancelled`].
    pub fn take_paste_sender(&mut self) -> Option<oneshot::Sender<String>> {
        self.paste_sender.take()
    }

    /// Runs the selected login and atomically stores the resulting credential.
    ///
    /// The progress callback runs on the host task and can render the same
    /// events for a CLI, TUI, or GUI. Cancellation and the 15-minute deadline
    /// apply through callback/device authorization and token exchange. A row
    /// that verifies its session (Factory Droid) checks the new grant against
    /// the vendor before the write; a refused or cancelled verification fails
    /// the login with nothing stored. Cancellation also applies while the
    /// auth-file lock is contended and is checked again under the lock before
    /// the write. After this commit point, the durable write completes as one
    /// locked atomic transaction. After commit, cancellation only stops the
    /// best-effort model fetch and the committed credential is returned.
    ///
    /// # Errors
    /// Returns a typed login, transport, token-exchange, ID-token, or auth-store
    /// error. A state mismatch and every pre-commit error, including a refused
    /// session verification, leave `auth.json` unchanged: an existing file keeps
    /// its bytes and an absent one stays absent.
    pub async fn run(
        &mut self,
        progress: &(dyn Fn(LoginProgress) + Send + Sync),
        cancel: &CancellationToken,
    ) -> Result<Credential, ProviderError> {
        if self.started {
            return Err(ProviderError::AuthWrite {
                reason: String::from("a login flow can only run once"),
            });
        }
        self.started = true;
        let deadline = Instant::now() + self.wait;
        let authorization = {
            let flow = self.authorize(progress, cancel, deadline);
            tokio::select! {
                biased;
                () = cancel.cancelled() => Err(ProviderError::LoginCancelled),
                outcome = timeout_at(deadline, flow) => match outcome {
                    Ok(result) => result,
                    Err(_) => Err(ProviderError::LoginTimeout),
                },
            }
        };
        let oauth = match authorization {
            Ok(oauth) => oauth,
            Err(error) => {
                drop(self.paste_sender.take());
                drop(self.paste_receiver.take());
                return Err(error);
            }
        };
        let verified = if cancel.is_cancelled() {
            Err(ProviderError::LoginCancelled)
        } else {
            self.verify_session(&oauth, cancel).await
        };
        if let Err(error) = verified {
            drop(self.paste_sender.take());
            drop(self.paste_receiver.take());
            return Err(error);
        }
        let credential = Credential::OAuth(oauth);
        #[cfg(test)]
        if let Some(commit_ready) = self.commit_ready.take() {
            let _ = commit_ready.send(());
        }
        if let Err(error) = self.persist(&credential, cancel).await {
            drop(self.paste_sender.take());
            drop(self.paste_receiver.take());
            return Err(error);
        }
        let models_base = self
            .endpoints
            .row(self.def)
            .and_then(|row| row.models_base.clone());
        if let Some(models_base) = models_base {
            tokio::select! {
                biased;
                () = cancel.cancelled() => {},
                () = self.fetch_models_best_effort(&models_base, &credential) => {},
            }
        }
        drop(self.paste_sender.take());
        drop(self.paste_receiver.take());
        Ok(credential)
    }

    async fn authorize(
        &mut self,
        progress: &(dyn Fn(LoginProgress) + Send + Sync),
        cancel: &CancellationToken,
        deadline: Instant,
    ) -> Result<OAuthCredential, ProviderError> {
        let Some(row) = self.endpoints.row(self.def).cloned() else {
            return Err(ProviderError::AuthWrite {
                reason: format!("{} has no OAuth endpoints", self.def.id),
            });
        };
        let verifier = new_pkce_verifier();
        let challenge = pkce_challenge(&verifier);
        match row.oauth.completion {
            Completion::Callback => {
                self.authorize_callback(&row, progress, cancel, deadline, &verifier, &challenge)
                    .await
            }
            Completion::PasteCode => {
                self.authorize_paste(&row, progress, cancel, &verifier, &challenge)
                    .await
            }
        }
    }

    /// The browser flow: the callback or a pasted redirect completes it; a
    /// busy port falls back to the device flow, and a row that offers no
    /// browser sign-in runs only the device flow.
    async fn authorize_callback(
        &mut self,
        row: &RowEndpoints,
        progress: &(dyn Fn(LoginProgress) + Send + Sync),
        cancel: &CancellationToken,
        deadline: Instant,
        verifier: &str,
        challenge: &str,
    ) -> Result<OAuthCredential, ProviderError> {
        if self.device_auth || !row.oauth.browser {
            return self.authorize_device(row, progress, cancel, deadline).await;
        }
        let state = flow_state(row.oauth.hook, verifier);
        let listener = bind_callback(row.callback_port).await;
        if let Ok((listener, port)) = listener {
            let redirect = redirect_uri(row.oauth.redirect, port);
            let url = authorize_url(
                row.def.family,
                &row.oauth,
                &row.client_id,
                &row.authorize,
                &redirect,
                challenge,
                &state,
            )?;
            report_progress(
                progress,
                LoginProgress::OpenUrl {
                    url: url.to_string(),
                },
                cancel,
            )?;
            report_progress(
                progress,
                LoginProgress::AskPaste {
                    hint: String::from(PASTE_HINT),
                    default: None,
                },
                cancel,
            )?;
            let receiver = self.paste_receiver.take();
            drop(self.paste_sender.take());
            let code = Self::paste_callback_code(
                row,
                row.oauth.redirect.path,
                listener,
                receiver,
                &state,
                cancel,
            )
            .await?;
            report_progress(progress, LoginProgress::Exchanging, cancel)?;
            return self
                .exchange(row, &code, verifier, &redirect, &[&state, &code, verifier])
                .await;
        }
        self.authorize_device(row, progress, cancel, deadline).await
    }

    async fn authorize_device(
        &self,
        row: &RowEndpoints,
        progress: &(dyn Fn(LoginProgress) + Send + Sync),
        cancel: &CancellationToken,
        deadline: Instant,
    ) -> Result<OAuthCredential, ProviderError> {
        let Some(device) = &row.device else {
            return Err(ProviderError::AuthWrite {
                reason: format!("{} has no device sign-in", row.def.id),
            });
        };
        let http = OAuthHttp {
            client: &self.oauth_client,
            user_agent: &self.user_agent,
        };
        let outcome = super::device::run(
            &http,
            row.def.family,
            row.oauth.hook,
            &row.client_id,
            device,
            deadline,
            progress,
            cancel,
        )
        .await?;
        match outcome {
            DeviceOutcome::Grant(grant) => {
                report_progress(progress, LoginProgress::Exchanging, cancel)?;
                self.exchange(
                    row,
                    &grant.authorization_code,
                    &grant.code_verifier,
                    device.redirect_uri,
                    &[&grant.authorization_code, &grant.code_verifier],
                )
                .await
            }
            DeviceOutcome::Tokens(credential) => Ok(credential),
        }
    }

    /// The paste browser flow: the callback or a pasted value completes it.
    async fn authorize_paste(
        &mut self,
        row: &RowEndpoints,
        progress: &(dyn Fn(LoginProgress) + Send + Sync),
        cancel: &CancellationToken,
        verifier: &str,
        challenge: &str,
    ) -> Result<OAuthCredential, ProviderError> {
        let state = flow_state(row.oauth.hook, verifier);
        let Landing {
            uri,
            path,
            listener,
        } = Landing::open(row).await?;
        let url = authorize_url(
            row.def.family,
            &row.oauth,
            &row.client_id,
            &row.authorize,
            &uri,
            challenge,
            &state,
        )?;
        report_progress(
            progress,
            LoginProgress::OpenUrl {
                url: url.to_string(),
            },
            cancel,
        )?;
        report_progress(
            progress,
            LoginProgress::AskPaste {
                hint: String::from(PASTE_HINT),
                default: None,
            },
            cancel,
        )?;
        let receiver = self.paste_receiver.take();
        drop(self.paste_sender.take());
        let code = if let Some(listener) = listener {
            Self::paste_callback_code(row, &path, listener, receiver, &state, cancel).await?
        } else {
            let value = receive_paste(receiver, cancel).await?;
            parse_pasted_code(&value, &state, row.def.family)?
        };
        report_progress(progress, LoginProgress::Exchanging, cancel)?;
        self.exchange(
            row,
            &code,
            verifier,
            &uri,
            &[code.as_str(), verifier, state.as_str()],
        )
        .await
    }

    async fn paste_callback_code(
        row: &RowEndpoints,
        path: &str,
        listener: tokio::net::TcpListener,
        receiver: Option<tokio::sync::oneshot::Receiver<String>>,
        state: &str,
        cancel: &CancellationToken,
    ) -> Result<String, ProviderError> {
        let callback = callback_code(listener, path, state, row.def.family);
        tokio::pin!(callback);
        tokio::select! {
            biased;
            result = &mut callback => result,
            pasted = receive_paste(receiver, cancel) => match pasted {
                Ok(value) => parse_pasted_code(&value, state, row.def.family),
                Err(error) if cancel.is_cancelled() => Err(error),
                Err(_) => callback.await,
            }
        }
    }

    async fn exchange(
        &self,
        row: &RowEndpoints,
        code: &str,
        verifier: &str,
        redirect: &str,
        secrets: &[&str],
    ) -> Result<OAuthCredential, ProviderError> {
        let http = OAuthHttp {
            client: &self.oauth_client,
            user_agent: &self.user_agent,
        };
        match row.oauth.hook {
            Hook::CodexAccount => {
                let body = code_exchange_form(&row.client_id, code, verifier, redirect);
                let response = post_form(&http, row.def.family, &row.token, &body).await?;
                let token = token_or_exchange_error::<CodexTokenResponse>(&response, secrets)?;
                validate_token_pair(&token.access_token, &token.refresh_token)?;
                let identity = codex_identity(&token.id_token).ok_or(ProviderError::NoAccountId)?;
                Ok(OAuthCredential {
                    expires_at: oauth_expires_at(unix_now(), token.expires_in, &token.access_token),
                    access_token: SecretString::from(token.access_token),
                    refresh_token: SecretString::from(token.refresh_token),
                    id_token: Some(token.id_token),
                    account_id: Some(identity.account_id),
                })
            }
            Hook::ClaudeCode => {
                let request = ClaudeTokenRequest {
                    grant_type: "authorization_code",
                    client_id: &row.client_id,
                    code,
                    state: verifier,
                    redirect_uri: redirect,
                    code_verifier: verifier,
                };
                let response = post_json(&http, row.def.family, &row.token, &request).await?;
                let token = token_or_exchange_error::<ClaudeTokenResponse>(&response, secrets)?;
                validate_token_pair(&token.access_token, &token.refresh_token)?;
                Ok(OAuthCredential {
                    expires_at: oauth_expires_at(unix_now(), token.expires_in, &token.access_token),
                    access_token: SecretString::from(token.access_token),
                    refresh_token: SecretString::from(token.refresh_token),
                    id_token: None,
                    account_id: None,
                })
            }
            Hook::GitLabDuo => {
                let body = code_exchange_form(&row.client_id, code, verifier, redirect);
                let response = post_form(&http, row.def.family, &row.token, &body).await?;
                let token = token_or_exchange_error::<GitLabTokenResponse>(&response, secrets)?;
                validate_token_pair(&token.access_token, &token.refresh_token)?;
                Ok(OAuthCredential {
                    expires_at: oauth_expires_at(
                        oauth_issued_at(token.created_at.as_ref(), unix_now()),
                        token.expires_in,
                        &token.access_token,
                    ),
                    access_token: SecretString::from(token.access_token),
                    refresh_token: SecretString::from(token.refresh_token),
                    id_token: None,
                    account_id: None,
                })
            }
            Hook::WorkOsDevice => Err(ProviderError::AuthWrite {
                reason: format!(
                    "{} signs in by device grant and has no authorization-code exchange",
                    row.def.id
                ),
            }),
        }
    }

    /// Checks the vendor's session endpoint against the new grant before it
    /// is stored, for the rows that verify one. Cancellation reports
    /// [`ProviderError::LoginCancelled`]; every other refusal fails the login.
    async fn verify_session(
        &self,
        oauth: &OAuthCredential,
        cancel: &CancellationToken,
    ) -> Result<(), ProviderError> {
        let Some(base) = self
            .endpoints
            .row(self.def)
            .and_then(|row| row.session_base.as_ref())
        else {
            return Ok(());
        };
        crate::family::droid::verify_session(
            &self.oauth_client,
            base.as_str(),
            oauth.access_token.expose(),
            cancel,
        )
        .await
    }

    async fn persist(
        &mut self,
        credential: &Credential,
        cancel: &CancellationToken,
    ) -> Result<(), ProviderError> {
        fn token_of(value: &Credential) -> Option<&str> {
            match value {
                Credential::ApiKey { key } => Some(key.expose()),
                Credential::OAuth(oauth) => Some(oauth.access_token.expose()),
                Credential::None => None,
            }
        }
        let path = self.store.path().to_path_buf();
        let provider = self.def.id;
        let credential = credential.clone();
        create_auth_dir(&path).await?;
        // Cloned (never allocated) into the commit closure so invalidation runs
        // at the real commit point: a future dropped after the durable write
        // but before the outer await still leaves the cache coherent.
        let grants = self.gitlab_grants.clone();
        let lock = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ProviderError::LoginCancelled),
            lock = crate::auth::refresh::lock_auth_file(&path) => lock?,
        };
        let cancel = cancel.clone();
        let updated = crate::auth::refresh::blocking(move || {
            let _lock = lock;
            let mut latest = AuthStore::load(&path)?;
            // Same-lock pre-commit snapshot for rotation hygiene.
            let previous = latest.credential(provider);
            latest.set(provider, credential)?;
            if cancel.is_cancelled() {
                return Err(ProviderError::LoginCancelled);
            }
            latest.store()?;
            // Post-commit, pre-return: GitLab only, no pre-commit invalidation,
            // no token formatting. New value always; replaced value when it
            // differs (first login has no previous).
            if provider == GITLAB_PROVIDER_ID
                && let Some(grants) = grants.as_ref()
            {
                let committed = latest.credential(provider);
                let new_token = committed.as_ref().and_then(token_of);
                let old_token = previous.as_ref().and_then(token_of);
                match (old_token, new_token) {
                    (Some(old), Some(new)) if old != new => {
                        grants.remove(old);
                        grants.remove(new);
                    }
                    (_, Some(new)) => {
                        grants.remove(new);
                    }
                    _ => {}
                }
            }
            Ok(latest)
        })
        .await?;
        *self.store = updated;
        Ok(())
    }

    async fn fetch_models_best_effort(&self, models_base: &Url, credential: &Credential) {
        let default_entry = ProviderEntry {
            id: Box::from(self.def.id),
            def: Some(self.def),
            family: self.def.family,
            base_url: models_base.as_str().into(),
            transport: Transport::Https,
            key_env: None,
            auth: AuthStyle::Bearer,
            max_concurrent_requests: 1,
            settings: ProviderSettings::default(),
        };
        let entry = self
            .models_provider
            .as_ref()
            .filter(|_| !self.endpoints.loopback_override)
            .unwrap_or(&default_entry);
        let fetch = crate::catalog::ModelFetch {
            client: &self.client,
            provider: entry,
            credential,
            cache_dir: &self.cache_dir,
            user_agent: &self.user_agent,
            version: env!("CARGO_PKG_VERSION"),
        };
        let _catalog = crate::catalog::load_models(&fetch, |duration| sleep(duration)).await;
    }
}

/// Creates the directory `auth.json` lives in (mode 0700 on Unix) when it is
/// missing, so the lock file beside the credential file can be opened on a
/// first sign-in. The credential file itself is not touched.
async fn create_auth_dir(path: &Path) -> Result<(), ProviderError> {
    let parent = path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| ProviderError::AuthWrite {
            reason: format!("{} names no parent directory", path.display()),
        })?;
    crate::auth::refresh::blocking(move || {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(0o700);
        }
        builder
            .create(&parent)
            .map_err(|error| ProviderError::AuthWrite {
                reason: format!("could not create {}: {error}", parent.display()),
            })
    })
    .await
}

/// Stores one API key while holding the cross-process auth-file lock.
///
/// # Errors
/// Returns an auth-store or lock error when the key cannot be written.
pub async fn store_api_key(
    path: impl Into<PathBuf>,
    provider: &str,
    key: String,
) -> Result<(), ProviderError> {
    store_api_key_unless_cancelled_with_grants(
        path.into(),
        provider,
        key,
        &CancellationToken::new(),
        None,
    )
    .await
}

/// Stores one API key unless `cancel` fires first, with an optional shared
/// GitLab grant cache (internal companion; single implementation, no duplicate
/// lock).
///
/// Pre-commit snapshot and post-commit invalidation both run inside the one
/// existing commit closure holding the auth-file lock: the previous credential
/// is read before `set`, and after `store()` succeeds the GitLab grant for the
/// replaced token (when different) plus the new key are removed. A future
/// dropped after the durable write but before the outer await still leaves the
/// cache coherent. Non-GitLab rows, pre-commit cancel/errors, and `None` caches
/// change nothing. Token values never enter Debug/error/log.
pub(crate) async fn store_api_key_unless_cancelled_with_grants(
    path: PathBuf,
    provider: &str,
    key: String,
    cancel: &CancellationToken,
    grants: Option<Arc<DirectAccessCache>>,
) -> Result<(), ProviderError> {
    create_auth_dir(&path).await?;
    let provider = provider.to_owned();
    let lock = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(ProviderError::LoginCancelled),
        lock = crate::auth::refresh::lock_auth_file(&path) => lock?,
    };
    let cancel = cancel.clone();
    crate::auth::refresh::blocking(move || {
        let _lock = lock;
        if cancel.is_cancelled() {
            return Err(ProviderError::LoginCancelled);
        }
        let mut store = AuthStore::load(&path)?;
        let previous = store.credential(&provider);
        store.set(
            &provider,
            Credential::ApiKey {
                key: SecretString::from(key.clone()),
            },
        )?;
        store.store()?;
        if provider.as_str() == GITLAB_PROVIDER_ID
            && let Some(grants) = grants.as_ref()
        {
            let old_token = previous.as_ref().and_then(|value| match value {
                Credential::ApiKey { key } => Some(key.expose()),
                Credential::OAuth(oauth) => Some(oauth.access_token.expose()),
                Credential::None => None,
            });
            match old_token {
                Some(old) if old != key.as_str() => {
                    grants.remove(old);
                    grants.remove(key.as_str());
                }
                _ => {
                    grants.remove(key.as_str());
                }
            }
        }
        Ok(())
    })
    .await
}

/// Removes `provider`'s local credential, revoking refresh tokens of providers
/// that publish a revocation endpoint on a best-effort basis before the atomic
/// local write.
///
/// Repeated logout and a provider with no stored entry succeed without a remote
/// request. API-key credentials and providers without a revocation endpoint are
/// removed locally; an OAuth credential of a provider with one sends one revoke
/// request to the pinned production endpoint, carrying the client id `env`
/// resolves for that provider. The revoke is
/// best effort: a network/status failure never prevents local logout.
/// # Errors
/// Returns an auth-store or lock error if the durable local removal fails.
pub async fn logout(
    provider: &str,
    store: &mut AuthStore,
    env: &EnvSnapshot,
) -> Result<(), ProviderError> {
    let user_agent = default_user_agent();
    logout_with_grants(
        provider,
        store,
        &user_agent,
        &LoginEndpoints::production(env)?,
        None,
    )
    .await
}

/// Logout with explicit user-agent and endpoint values, primarily for loopback
/// replay tests. The revocation request uses a private client that does not
/// follow redirects; only loopback test endpoints and pinned production
/// origins are accepted.
///
/// # Errors
/// Returns an auth-store, endpoint-validation, or lock error if local logout
/// cannot be completed. A failure of the best-effort revoke is logged and ignored.
pub async fn logout_with(
    provider: &str,
    store: &mut AuthStore,
    user_agent: &str,
    endpoints: &LoginEndpoints,
) -> Result<(), ProviderError> {
    logout_with_grants(provider, store, user_agent, endpoints, None).await
}

/// Logout with an optional shared GitLab grant cache (internal contract).
///
/// Public `logout`/`logout_with` signatures are unchanged and route here with
/// `None`, preserving standalone behavior. `sign_out` routes the site handle
/// here. Ordering: the best-effort revoke runs first and never blocks local
/// removal; the grant is removed AFTER the durable removal section commits
/// (including when the revoke failed), never before, using the exact stored
/// token snapshot taken under the existing auth lock. No-entry targets make
/// no request and invalidate nothing. Token values never enter Debug/error/log.
pub(crate) async fn logout_with_grants(
    provider: &str,
    store: &mut AuthStore,
    user_agent: &str,
    endpoints: &LoginEndpoints,
    grants: Option<&Arc<DirectAccessCache>>,
) -> Result<(), ProviderError> {
    let path = store.path().to_path_buf();
    let remove_path = path.clone();
    let provider_for_load = provider.to_owned();
    let lock = crate::auth::refresh::lock_auth_file(&path).await?;
    let (lock, mut latest, stored, provider_owned) = crate::auth::refresh::blocking(move || {
        let latest = AuthStore::load(&path)?;
        let stored = latest.credential(&provider_for_load);
        Ok((lock, latest, stored, provider_for_load))
    })
    .await?;
    let Some(stored) = stored else {
        *store = latest;
        return Ok(());
    };
    let revoke = find(provider).and_then(|def| {
        let row = endpoints.row(def)?;
        Some((def, row, row.revoke.as_ref()?))
    });
    if let (Some((def, row, url)), Credential::OAuth(token)) = (revoke, &stored) {
        endpoints.validate()?;
        if let Ok(client) = build_oauth_client(def.family) {
            let http = OAuthHttp {
                client: &client,
                user_agent,
            };
            match post_revoke(&http, row, url, token.refresh_token.expose()).await {
                Ok(response) if (200..300).contains(&response.status) => {}
                Ok(response) => tracing::warn!(
                    provider = def.id,
                    status = response.status,
                    "OAuth token revocation failed"
                ),
                Err(_) => {
                    tracing::warn!(provider = def.id, "OAuth token revocation request failed");
                }
            }
        } else {
            tracing::warn!(provider = def.id, "OAuth token revocation client failed");
        }
    }
    // Under-lock snapshot token moved into the removal closure so invalidation
    // runs at the real commit point (covers revoke failures and future-drop
    // after the durable write). GitLab only; no pre-commit invalidation.
    let old_token: Option<String> = if provider == GITLAB_PROVIDER_ID {
        match &stored {
            Credential::ApiKey { key } => Some(key.expose().to_owned()),
            Credential::OAuth(oauth) => Some(oauth.access_token.expose().to_owned()),
            Credential::None => None,
        }
    } else {
        None
    };
    // Owned clone: `blocking` requires `Send + 'static`, so the borrowed cache
    // handle cannot cross into the closure; the `Arc` clone is a refcount bump.
    let grants_owned: Option<Arc<DirectAccessCache>> = grants.cloned();
    let updated = crate::auth::refresh::blocking(move || {
        let _lock = lock;
        latest.remove(&provider_owned);
        if latest.status().is_empty() {
            match std::fs::remove_file(&remove_path) {
                Ok(()) => sync_auth_directory(&remove_path)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(ProviderError::AuthWrite {
                        reason: format!("could not remove {}: {error}", remove_path.display()),
                    });
                }
            }
        } else {
            latest.store()?;
        }
        if let (Some(grants), Some(old)) = (grants_owned.as_ref(), old_token.as_deref()) {
            grants.remove(old);
        }
        Ok(latest)
    })
    .await?;
    *store = updated;
    Ok(())
}

#[cfg_attr(
    windows,
    expect(
        clippy::unnecessary_wraps,
        reason = "directory sync fails only on POSIX"
    )
)]
fn sync_auth_directory(path: &Path) -> Result<(), ProviderError> {
    #[cfg(not(windows))]
    {
        let parent = path.parent().ok_or_else(|| ProviderError::AuthWrite {
            reason: format!("{} names no parent directory", path.display()),
        })?;
        #[cfg(all(test, unix))]
        if tests::take_dir_sync_fault(path) {
            return Err(ProviderError::AuthWrite {
                reason: format!(
                    "could not sync {}: injected directory sync failure",
                    parent.display()
                ),
            });
        }
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| ProviderError::AuthWrite {
                reason: format!("could not sync {}: {error}", parent.display()),
            })?;
    }
    #[cfg(windows)]
    let _ = path;
    Ok(())
}

/// Builds a private OAuth client for requests that carry codes or tokens.
///
/// Redirects are disabled entirely: a provider's 307/308 cannot forward an
/// authorization code, verifier, device code, or refresh token to another host.
fn build_oauth_client(family: Family) -> Result<Client, ProviderError> {
    Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(STREAM_IDLE_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .referer(false)
        .retry(reqwest::retry::never())
        .build()
        .map_err(|error| ProviderError::Transport {
            family,
            reason: format!("could not initialize secure OAuth client: {error}"),
        })
}

fn default_user_agent() -> String {
    crate::http::user_agent(
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        "unknown",
        std::env::consts::ARCH,
    )
}

/// HTTP inputs shared by the OAuth and device endpoints.
pub(crate) struct OAuthHttp<'a> {
    pub(crate) client: &'a Client,
    pub(crate) user_agent: &'a str,
}

/// One bounded JSON response from a token or device-code endpoint.
pub(crate) struct OAuthResponse {
    pub(crate) status: u16,
    pub(crate) body: Vec<u8>,
}

/// Emits one host-facing progress event unless cancellation has won the race.
pub(crate) fn report_progress(
    progress: &(dyn Fn(LoginProgress) + Send + Sync),
    event: LoginProgress,
    cancel: &CancellationToken,
) -> Result<(), ProviderError> {
    if cancel.is_cancelled() {
        return Err(ProviderError::LoginCancelled);
    }
    progress(event);
    if cancel.is_cancelled() {
        return Err(ProviderError::LoginCancelled);
    }
    Ok(())
}

/// The form body of a standard PKCE authorization-code exchange.
fn code_exchange_form(client_id: &str, code: &str, verifier: &str, redirect_uri: &str) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "authorization_code")
        .append_pair("client_id", client_id)
        .append_pair("code", code)
        .append_pair("code_verifier", verifier)
        .append_pair("redirect_uri", redirect_uri)
        .finish()
}

/// Sends one refresh-token revocation in the encoding the vendor reads:
/// `GitLab`'s authorization server takes form parameters and ignores a JSON
/// body; every other row with a revoke endpoint takes JSON.
async fn post_revoke(
    http: &OAuthHttp<'_>,
    row: &RowEndpoints,
    url: &Url,
    refresh_token: &str,
) -> Result<OAuthResponse, ProviderError> {
    let family = row.def.family;
    match row.oauth.hook {
        Hook::GitLabDuo => {
            let body = {
                let mut form = url::form_urlencoded::Serializer::new(String::new());
                form.append_pair("token", refresh_token)
                    .append_pair("token_type_hint", "refresh_token")
                    .append_pair("client_id", &row.client_id);
                form.finish()
            };
            post_form(http, family, url, &body).await
        }
        Hook::CodexAccount | Hook::ClaudeCode | Hook::WorkOsDevice => {
            let request = RevokeRequest {
                token: refresh_token,
                token_type_hint: "refresh_token",
                client_id: &row.client_id,
            };
            post_json(http, family, url, &request).await
        }
    }
}

pub(crate) async fn post_json<T: Serialize>(
    http: &OAuthHttp<'_>,
    family: Family,
    url: &Url,
    body: &T,
) -> Result<OAuthResponse, ProviderError> {
    let bytes =
        sonic_rs::to_vec(body).map_err(|error| endpoint_error(family, error.to_string()))?;
    send_body(
        http,
        family,
        http.client
            .post(url.clone())
            .header(CONTENT_TYPE, "application/json")
            .body(bytes),
    )
    .await
}

pub(crate) async fn post_form(
    http: &OAuthHttp<'_>,
    family: Family,
    url: &Url,
    body: &str,
) -> Result<OAuthResponse, ProviderError> {
    send_body(
        http,
        family,
        http.client
            .post(url.clone())
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(body.to_owned()),
    )
    .await
}

async fn send_body(
    http: &OAuthHttp<'_>,
    family: Family,
    request: reqwest::RequestBuilder,
) -> Result<OAuthResponse, ProviderError> {
    let response = send(
        family,
        request,
        http.user_agent,
        Exchange::Json {
            total: OAUTH_TIMEOUT,
        },
        sleep,
    )
    .await?;
    let status = response.status().as_u16();
    let body = read_body(family, response).await?;
    Ok(OAuthResponse { status, body })
}

pub(crate) fn decode_json<T: DeserializeOwned>(body: &[u8]) -> Option<T> {
    sonic_rs::from_slice(body).ok()
}

pub(crate) fn error_message(body: &[u8], secrets: &[&str]) -> String {
    #[derive(Deserialize)]
    struct ErrorBody {
        #[serde(default)]
        error_description: Option<String>,
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        error: Option<String>,
    }

    let parsed = decode_json::<ErrorBody>(body);
    let message = parsed
        .and_then(|value| value.error_description.or(value.message).or(value.error))
        .or_else(|| {
            let text = std::str::from_utf8(body).ok()?.trim();
            (!text.starts_with('{') && !text.starts_with('[')).then(|| text.to_owned())
        })
        .unwrap_or_else(|| String::from("request failed"));
    let mut safe = message;
    for secret in secrets.iter().copied().filter(|secret| !secret.is_empty()) {
        safe = safe.replace(secret, "<redacted>");
        // The same secret also reaches error bodies percent-encoded, the way
        // a form body or a URL query carries it.
        let encoded: String = url::form_urlencoded::byte_serialize(secret.as_bytes()).collect();
        if encoded != secret {
            safe = safe.replace(&encoded, "<redacted>");
        }
    }
    truncate_message(&safe)
}

/// The first line of `message`, at most 300 bytes, for an error value.
pub(crate) fn truncate_message(message: &str) -> String {
    let end = message.find(['\n', '\r']).unwrap_or(message.len());
    let line = &message[..end];
    if line.len() <= 300 {
        return line.to_owned();
    }
    let mut cut = 300;
    while !line.is_char_boundary(cut) {
        cut -= 1;
    }
    line[..cut].to_owned()
}

fn validate_token_pair(access_token: &str, refresh_token: &str) -> Result<(), ProviderError> {
    if access_token.is_empty() || refresh_token.is_empty() {
        return Err(ProviderError::TokenExchange {
            status: 200,
            message: String::from("invalid token response"),
        });
    }
    Ok(())
}

fn token_or_exchange_error<T: DeserializeOwned>(
    response: &OAuthResponse,
    secrets: &[&str],
) -> Result<T, ProviderError> {
    if !(200..300).contains(&response.status) {
        return Err(ProviderError::TokenExchange {
            status: response.status,
            message: error_message(&response.body, secrets),
        });
    }
    decode_json(&response.body).ok_or_else(|| ProviderError::TokenExchange {
        status: response.status,
        message: String::from("invalid token response"),
    })
}

#[derive(Deserialize)]
struct CodexTokenResponse {
    id_token: String,
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    expires_in: Option<i64>,
}

#[derive(Deserialize)]
struct ClaudeTokenResponse {
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    expires_in: Option<i64>,
}

#[derive(Deserialize)]
struct GitLabTokenResponse {
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    expires_in: Option<i64>,
    /// When the server issued the grant, in Unix seconds. The expiry counts
    /// from it when it is an integer and from the local clock otherwise; any
    /// other JSON type is ignored rather than failing the sign-in, exactly as
    /// a refresh reads it.
    #[serde(default)]
    created_at: Option<sonic_rs::Value>,
}

#[derive(Serialize)]
struct ClaudeTokenRequest<'a> {
    grant_type: &'static str,
    client_id: &'a str,
    code: &'a str,
    state: &'a str,
    redirect_uri: &'a str,
    code_verifier: &'a str,
}

#[derive(Serialize)]
struct RevokeRequest<'a> {
    token: &'a str,
    token_type_hint: &'static str,
    client_id: &'a str,
}

fn authorize_url(
    family: Family,
    oauth: &OAuthSpec,
    client_id: &str,
    endpoint_url: &Url,
    redirect_uri: &str,
    challenge: &str,
    state: &str,
) -> Result<Url, ProviderError> {
    let scope = oauth.scopes.join(" ");
    let pairs: Vec<(&str, &str)> = oauth
        .query
        .iter()
        .map(|(name, value)| {
            let text = match value {
                QueryValue::Text(text) => text,
                QueryValue::ClientId => client_id,
                QueryValue::RedirectUri => redirect_uri,
                QueryValue::Scope => scope.as_str(),
                QueryValue::Challenge => challenge,
                QueryValue::State => state,
            };
            (*name, text)
        })
        .collect();
    with_query(endpoint_url, &pairs, family, query_encoding(oauth.hook))
}

/// Whether sign-in is followed by a best-effort model-list fetch.
fn fetches_models(hook: Hook) -> bool {
    matches!(hook, Hook::CodexAccount)
}

/// Whether the new grant is checked against the vendor's session endpoint
/// before it is stored.
fn verifies_session(hook: Hook) -> bool {
    matches!(hook, Hook::WorkOsDevice)
}

/// The client id a row's requests carry: the trimmed value of its override
/// variable in the captured environment when that is not blank, else the
/// row's pinned id. A blank value counts as unset.
fn effective_client_id(oauth: &OAuthSpec, env: &EnvSnapshot) -> String {
    oauth
        .client_id_env
        .and_then(|name| env.get(name))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(oauth.client_id)
        .to_owned()
}

/// The variable that replaces a row's registered redirect URI, for the rows
/// that honor one.
const fn redirect_env(hook: Hook) -> Option<&'static str> {
    match hook {
        Hook::GitLabDuo => Some("GITLAB_REDIRECT_URI"),
        Hook::CodexAccount | Hook::ClaudeCode | Hook::WorkOsDevice => None,
    }
}

/// The redirect override `env` supplies for `hook`, when it is not blank.
fn redirect_override(hook: Hook, env: &EnvSnapshot) -> Option<RedirectOverride> {
    let variable = redirect_env(hook)?;
    let uri = env.get(variable)?.trim();
    (!uri.is_empty()).then(|| RedirectOverride {
        variable,
        uri: uri.into(),
    })
}

/// The path prefix that keeps the rows apart on one loopback test server.
const fn loopback_prefix(hook: Hook) -> &'static str {
    match hook {
        Hook::CodexAccount => "",
        Hook::ClaudeCode => "claude/",
        Hook::WorkOsDevice => "workos/",
        Hook::GitLabDuo => "gitlab/",
    }
}

/// The loopback path of a production `path` under a row's `prefix`; a bare
/// origin has no path of its own and lands on the prefix alone.
fn loopback_path(prefix: &str, path: &str) -> String {
    if path.is_empty() {
        return prefix.trim_end_matches('/').to_owned();
    }
    format!("{prefix}{path}")
}

/// How the vendor's authorization endpoint reads a space in a query value.
const fn query_encoding(hook: Hook) -> QueryEncoding {
    match hook {
        Hook::CodexAccount => QueryEncoding::Percent20,
        Hook::ClaudeCode | Hook::WorkOsDevice | Hook::GitLabDuo => QueryEncoding::Form,
    }
}

/// The `state` of one attempt: Claude reuses the PKCE verifier, the others
/// draw a fresh value.
fn flow_state(hook: Hook, verifier: &str) -> String {
    match hook {
        Hook::ClaudeCode => verifier.to_owned(),
        Hook::CodexAccount | Hook::WorkOsDevice | Hook::GitLabDuo => new_state(),
    }
}
#[derive(Clone, Copy)]
enum QueryEncoding {
    Form,
    Percent20,
}

fn with_query(
    url: &Url,
    pairs: &[(&str, &str)],
    family: Family,
    encoding: QueryEncoding,
) -> Result<Url, ProviderError> {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs.iter().copied())
        .finish();
    let query = match encoding {
        QueryEncoding::Form => query,
        QueryEncoding::Percent20 => query.replace('+', "%20"),
    };
    let mut result = url.clone();
    result.set_query(Some(&query));
    if result.host().is_none() {
        return Err(endpoint_error(family, "authorization URL has no host"));
    }
    Ok(result)
}

fn redirect_uri(redirect: Redirect, port: u16) -> String {
    format!("http://{}:{port}{}", redirect.host, redirect.path)
}

/// Where a browser sign-in receives its authorization result.
struct Landing {
    /// The `redirect_uri` the vendor is told and the code exchange repeats.
    uri: String,
    /// The callback path the listener serves.
    path: String,
    /// The bound loopback listener; `None` when only a pasted value can
    /// complete the sign-in.
    listener: Option<TcpListener>,
}

impl Landing {
    /// Opens the landing of `row`: the environment's redirect override when
    /// one is set, otherwise the registered redirect on its preferred port or,
    /// when that is busy, on any free loopback port.
    async fn open(row: &RowEndpoints) -> Result<Self, ProviderError> {
        if let Some(redirect) = &row.redirect_override {
            return Self::overridden(redirect).await;
        }
        let (listener, port) = match bind_callback_with_fallback(row.callback_port).await {
            Ok((listener, port)) => (Some(listener), port),
            Err(_) => (None, row.callback_port),
        };
        Ok(Self {
            uri: redirect_uri(row.oauth.redirect, port),
            path: row.oauth.redirect.path.to_owned(),
            listener,
        })
    }

    /// An explicit override is advertised verbatim and served only where it
    /// says: its loopback host, port, and path. A busy port, or a host this
    /// machine cannot serve, leaves the sign-in to a pasted value; no other
    /// port is ever tried.
    async fn overridden(redirect: &RedirectOverride) -> Result<Self, ProviderError> {
        let url = redirect.parse()?;
        let listener = match loopback_address(&url) {
            Some(address) => TcpListener::bind(address).await.ok(),
            None => None,
        };
        Ok(Self {
            uri: redirect.uri.to_string(),
            path: url.path().to_owned(),
            listener,
        })
    }
}

impl RedirectOverride {
    /// The override as a URL: an `http` or `https` URL that names a host and,
    /// when it names a port, a nonzero one, with no credentials and no
    /// fragment. The error names the variable, never its value.
    fn parse(&self) -> Result<Url, ProviderError> {
        Url::parse(&self.uri)
            .ok()
            .filter(|url| {
                matches!(url.scheme(), "http" | "https")
                    && url.host().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.fragment().is_none()
                    && url.port() != Some(0)
            })
            .ok_or_else(|| ProviderError::LoginInput {
                reason: format!(
                    "{} must be an http or https URL with a host, a nonzero port, no credentials, and no fragment.",
                    self.variable
                ),
            })
    }
}

/// The loopback socket that serves `url`, when this machine can: a plain
/// `http` URL on `localhost`, `127.0.0.1`, or `::1`. `localhost` binds the
/// IPv4 loopback, as the registered redirects do.
fn loopback_address(url: &Url) -> Option<SocketAddr> {
    if url.scheme() != "http" || !is_loopback(url) {
        return None;
    }
    let ip = match url.host()? {
        url::Host::Ipv6(address) => IpAddr::V6(address),
        url::Host::Domain(_) | url::Host::Ipv4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
    };
    Some(SocketAddr::new(ip, url.port_or_known_default()?))
}

fn host_of(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_default()
}

fn new_pkce_verifier() -> String {
    let mut bytes = [0_u8; 32];
    bytes[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    URL_SAFE_NO_PAD.encode(bytes)
}

fn new_state() -> String {
    let bytes = Uuid::new_v4().into_bytes();
    let mut state = String::with_capacity(32);
    for byte in bytes {
        let _written = write!(state, "{byte:02x}");
    }
    state
}

pub(crate) fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

pub(crate) fn unix_now() -> i64 {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    i64::try_from(seconds).unwrap_or(i64::MAX)
}

async fn bind_callback(port: u16) -> std::io::Result<(TcpListener, u16)> {
    bind_loopback(port).await
}

/// Binds the Claude callback listener: the preferred port when free, else
/// any free loopback port. The provider accepts a loopback redirect on any
/// port, so the authorize URL is built from the port actually bound.
async fn bind_callback_with_fallback(port: u16) -> std::io::Result<(TcpListener, u16)> {
    match bind_loopback(port).await {
        Ok(bound) => Ok(bound),
        Err(preferred_error) => bind_loopback(0).await.map_err(|_| preferred_error),
    }
}

async fn bind_loopback(port: u16) -> std::io::Result<(TcpListener, u16)> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let listener = TcpListener::bind(address).await?;
    let bound = listener.local_addr()?.port();
    Ok((listener, bound))
}

async fn callback_code(
    listener: TcpListener,
    path: &str,
    expected_state: &str,
    family: Family,
) -> Result<String, ProviderError> {
    loop {
        let (mut stream, _) =
            listener
                .accept()
                .await
                .map_err(|error| ProviderError::Transport {
                    family,
                    reason: format!("could not accept OAuth callback: {error}"),
                })?;
        let Ok(Ok(request)) = tokio::time::timeout(
            Duration::from_secs(2),
            read_callback_request(&mut stream, family),
        )
        .await
        else {
            continue;
        };
        let code = match parse_callback_target(&request, path, expected_state) {
            Ok(Some(code)) => code,
            Err(error @ (ProviderError::StateMismatch | ProviderError::LoginCancelled)) => {
                let _response =
                    callback_response(&mut stream, "400 Bad Request", RESPONSE_BAD_REQUEST).await;
                return Err(error);
            }
            Ok(None) | Err(_) => {
                let _response =
                    callback_response(&mut stream, "404 Not Found", RESPONSE_BAD_REQUEST).await;
                continue;
            }
        };
        let _response = callback_response(&mut stream, "200 OK", RESPONSE_HTML).await;
        return Ok(code);
    }
}

async fn callback_response(
    stream: &mut TcpStream,
    status: &str,
    body: &[u8],
) -> Result<(), std::io::Error> {
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.write_all(body).await
}

async fn read_callback_request(
    stream: &mut TcpStream,
    family: Family,
) -> Result<String, ProviderError> {
    let mut bytes = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 1024];
    loop {
        let count = stream
            .read(&mut chunk)
            .await
            .map_err(|error| ProviderError::Transport {
                family,
                reason: format!("could not read OAuth callback: {error}"),
            })?;
        if count == 0 {
            return Err(ProviderError::Transport {
                family,
                reason: String::from("OAuth callback ended before its request headers"),
            });
        }
        if count > CALLBACK_REQUEST_LIMIT.saturating_sub(bytes.len()) {
            return Err(ProviderError::Transport {
                family,
                reason: String::from("OAuth callback request exceeds 16 KiB"),
            });
        }
        bytes.extend_from_slice(&chunk[..count]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8(bytes).map_err(|_| ProviderError::Transport {
        family,
        reason: String::from("OAuth callback request is not UTF-8"),
    })
}

fn parse_callback_target(
    request: &str,
    expected_path: &str,
    expected_state: &str,
) -> Result<Option<String>, ProviderError> {
    let Some(line) = request.lines().next() else {
        return Ok(None);
    };
    let mut parts = line.split_ascii_whitespace();
    if parts.next() != Some("GET") {
        return Ok(None);
    }
    let Some(target) = parts.next() else {
        return Ok(None);
    };
    let Ok(url) = Url::parse(&format!("http://localhost{target}")) else {
        return Ok(None);
    };
    if url.path() != expected_path {
        return Ok(None);
    }
    let mut code = None;
    let mut state = None;
    let mut authorization_error = false;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" if code.is_none() => code = Some(value.into_owned()),
            "state" if state.is_none() => state = Some(value.into_owned()),
            "error" if !authorization_error => authorization_error = !value.is_empty(),
            "code" | "state" | "error" => return Ok(None),
            _ => {}
        }
    }
    if state.as_deref() != Some(expected_state) {
        return Err(ProviderError::StateMismatch);
    }
    if authorization_error {
        return Err(ProviderError::LoginCancelled);
    }
    Ok(code.filter(|value| !value.is_empty()))
}

fn parse_pasted_code(
    input: &str,
    expected_state: &str,
    family: Family,
) -> Result<String, ProviderError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(endpoint_error(family, "pasted authorization code is empty"));
    }
    if let Some((code, state)) = input.split_once('#') {
        if state != expected_state {
            return Err(ProviderError::StateMismatch);
        }
        return nonempty_code(code, family);
    }
    if let Ok(url) = Url::parse(input) {
        if url.query().is_some() {
            return parse_query(
                url.query().unwrap_or_default(),
                true,
                expected_state,
                family,
            );
        }
        return Err(endpoint_error(family, "pasted redirect URL has no code"));
    }
    if input.starts_with('?') || input.contains('=') {
        return parse_query(input.trim_start_matches('?'), false, expected_state, family);
    }
    nonempty_code(input, family)
}

fn parse_query(
    query: &str,
    require_state: bool,
    expected_state: &str,
    family: Family,
) -> Result<String, ProviderError> {
    let mut code = None;
    let mut state = None;
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            _ => {}
        }
    }
    if let Some(state) = state {
        if state != expected_state {
            return Err(ProviderError::StateMismatch);
        }
    } else if require_state {
        return Err(ProviderError::StateMismatch);
    }
    code.filter(|value| !value.is_empty())
        .ok_or_else(|| endpoint_error(family, "pasted authorization result has no code"))
}

fn nonempty_code(code: &str, family: Family) -> Result<String, ProviderError> {
    let code = code.trim();
    if code.is_empty() {
        return Err(endpoint_error(family, "pasted authorization code is empty"));
    }
    Ok(code.to_owned())
}

async fn receive_paste(
    receiver: Option<oneshot::Receiver<String>>,
    cancel: &CancellationToken,
) -> Result<String, ProviderError> {
    let Some(receiver) = receiver else {
        return Err(ProviderError::LoginCancelled);
    };
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(ProviderError::LoginCancelled),
        value = receiver => value.map_err(|_| ProviderError::LoginCancelled),
    }
}

fn endpoint_error(family: Family, reason: impl Into<String>) -> ProviderError {
    ProviderError::Transport {
        family,
        reason: reason.into(),
    }
}

fn validate_endpoint(family: Family, url: &Url, pinned_host: &str) -> Result<(), ProviderError> {
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(endpoint_error(
            family,
            "OAuth endpoint contains credentials or a fragment",
        ));
    }
    let origin = url.origin().ascii_serialization();
    check_base_url(family, &origin)?;
    if is_loopback(url) {
        return Ok(());
    }
    if url.scheme() == "https" && url.host_str() == Some(pinned_host) && url.port().is_none() {
        return Ok(());
    }
    Err(endpoint_error(
        family,
        format!("OAuth endpoint host must be loopback or pinned {pinned_host}"),
    ))
}

pub(crate) fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(host)) => host == "localhost",
        Some(url::Host::Ipv4(address)) => address == Ipv4Addr::LOCALHOST,
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

#[cfg(test)]
mod tests;
