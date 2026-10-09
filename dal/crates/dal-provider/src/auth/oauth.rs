//! Host-driven OAuth login for Codex and Claude.
//!
//! A flow owns PKCE and state, accepts either a loopback browser redirect or
//! one explicit paste value, and makes the durable credential write its last
//! authentication step. Network secrets are wrapped in [`SecretString`] as soon
//! as the token response is decoded; server messages are redacted before they
//! enter a typed error.

use std::{
    fmt,
    fmt::Write as _,
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
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

use crate::{
    AuthStore, AuthStyle, Credential, OAuthCredential, ProviderEntry, ProviderError, SecretString,
    Transport,
    auth::credential::{codex_identity, oauth_expires_at},
    http::{
        CONNECT_TIMEOUT, Exchange, LOGIN_WAIT, OAUTH_TIMEOUT, STREAM_IDLE_TIMEOUT, check_base_url,
        endpoint, read_body, send,
    },
};

/// `OpenAI`'s public Codex OAuth client id.
pub(crate) const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Anthropic's public Claude Code OAuth client id.
pub(crate) const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
/// `OpenAI`'s Codex OAuth token endpoint.
pub(crate) const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
/// Anthropic's Claude OAuth token endpoint.
pub(crate) const CLAUDE_TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
/// The login-flow originator sent to `OpenAI`'s authorization endpoint.
pub(crate) const CODEX_ORIGINATOR: &str = "dalgon";

const CODEX_AUTH_ORIGIN: &str = "https://auth.openai.com";
const CODEX_API_BASE: &str = "https://chatgpt.com/backend-api/codex";
const CLAUDE_AUTH_BASE: &str = "https://claude.ai/oauth";
const CLAUDE_TOKEN_BASE: &str = "https://platform.claude.com/v1";
const CODEX_DEVICE_REDIRECT: &str = "https://auth.openai.com/deviceauth/callback";
const CODEX_CALLBACK_PATH: &str = "/auth/callback";
const CLAUDE_CALLBACK_PATH: &str = "/callback";
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
    },
    /// The authorization code is being exchanged for durable credentials.
    Exchanging,
}

/// Exact prompt text for a host that needs a user-pasted authorization result.
pub const PASTE_HINT: &str = "Paste the redirect URL or the code shown in the browser.";

/// OAuth endpoints for the built-in Codex and Claude flows.
///
/// Production values are fixed to their pinned HTTPS origins. Integration tests
/// can use [`LoginEndpoints::loopback`] to direct every OAuth request to a
/// literal loopback server; arbitrary HTTPS hosts are never admitted because
/// the authorization code and verifier are sent to these endpoints.
#[derive(Clone, Debug)]
pub struct LoginEndpoints {
    pub(crate) codex_authorize: Url,
    pub(crate) codex_token: Url,
    pub(crate) codex_device_usercode: Url,
    pub(crate) codex_device_token: Url,
    pub(crate) codex_device_page: Url,
    pub(crate) codex_revoke: Url,
    pub(crate) codex_models_base: Url,
    pub(crate) claude_authorize: Url,
    pub(crate) claude_token: Url,
    codex_callback_port: u16,
    claude_callback_port: u16,
    loopback_override: bool,
}

impl LoginEndpoints {
    pub(crate) fn production() -> Result<Self, ProviderError> {
        let codex_auth = Url::parse(CODEX_AUTH_ORIGIN)
            .map_err(|error| endpoint_error(Family::Codex, error.to_string()))?;
        let codex_api = Url::parse(CODEX_API_BASE)
            .map_err(|error| endpoint_error(Family::Codex, error.to_string()))?;
        let claude_auth = Url::parse(CLAUDE_AUTH_BASE)
            .map_err(|error| endpoint_error(Family::Anthropic, error.to_string()))?;
        let claude_token_base = Url::parse(CLAUDE_TOKEN_BASE)
            .map_err(|error| endpoint_error(Family::Anthropic, error.to_string()))?;
        let endpoints = Self {
            codex_authorize: endpoint(Family::Codex, codex_auth.as_str(), "oauth/authorize")?,
            codex_token: endpoint(Family::Codex, codex_auth.as_str(), "oauth/token")?,
            codex_device_usercode: endpoint(
                Family::Codex,
                codex_auth.as_str(),
                "api/accounts/deviceauth/usercode",
            )?,
            codex_device_token: endpoint(
                Family::Codex,
                codex_auth.as_str(),
                "api/accounts/deviceauth/token",
            )?,
            codex_device_page: endpoint(Family::Codex, codex_auth.as_str(), "codex/device")?,
            codex_revoke: endpoint(Family::Codex, codex_auth.as_str(), "oauth/revoke")?,
            codex_models_base: codex_api,
            claude_authorize: endpoint(Family::Anthropic, claude_auth.as_str(), "authorize")?,
            claude_token: endpoint(Family::Anthropic, claude_token_base.as_str(), "oauth/token")?,
            codex_callback_port: 1455,
            claude_callback_port: 53692,
            loopback_override: false,
        };
        endpoints.validate()?;
        Ok(endpoints)
    }

    /// Builds an endpoint set whose network requests all target `base_url`.
    ///
    /// This test seam accepts only `http` or `https` on the literal loopback
    /// hosts `127.0.0.1`, `::1`, or `localhost`. The callback ports default to
    /// zero so each test can bind an available ephemeral port.
    ///
    /// # Errors
    /// Returns a typed transport error if `base_url` is invalid or not loopback.
    pub fn loopback(base_url: &str) -> Result<Self, ProviderError> {
        let base = Url::parse(base_url).map_err(|error| {
            endpoint_error(Family::Codex, format!("loopback base is invalid: {error}"))
        })?;
        check_base_url(Family::Codex, base_url)?;
        if !is_loopback(&base) {
            return Err(endpoint_error(
                Family::Codex,
                "test OAuth endpoints must use a literal loopback host",
            ));
        }
        let codex_authorize = endpoint(Family::Codex, base.as_str(), "oauth/authorize")?;
        let codex_token = endpoint(Family::Codex, base.as_str(), "oauth/token")?;
        let codex_device_usercode = endpoint(
            Family::Codex,
            base.as_str(),
            "api/accounts/deviceauth/usercode",
        )?;
        let codex_device_token = endpoint(
            Family::Codex,
            base.as_str(),
            "api/accounts/deviceauth/token",
        )?;
        let codex_device_page = endpoint(Family::Codex, base.as_str(), "codex/device")?;
        let codex_revoke = endpoint(Family::Codex, base.as_str(), "oauth/revoke")?;
        let codex_models_base = endpoint(Family::Codex, base.as_str(), "backend-api/codex")?;
        let claude_authorize =
            endpoint(Family::Anthropic, base.as_str(), "claude/oauth/authorize")?;
        let claude_token = endpoint(Family::Anthropic, base.as_str(), "claude/v1/oauth/token")?;
        Ok(Self {
            codex_authorize,
            codex_token,
            codex_device_usercode,
            codex_device_token,
            codex_device_page,
            codex_revoke,
            codex_models_base,
            claude_authorize,
            claude_token,
            codex_callback_port: 0,
            claude_callback_port: 0,
            loopback_override: true,
        })
    }

    /// Sets the loopback callback ports; zero asks the OS to choose a free port.
    #[must_use]
    pub fn with_callback_ports(mut self, codex: u16, claude: u16) -> Self {
        self.codex_callback_port = codex;
        self.claude_callback_port = claude;
        self
    }

    pub(crate) fn validate(&self) -> Result<(), ProviderError> {
        validate_endpoint(Family::Codex, &self.codex_authorize, "auth.openai.com")?;
        validate_endpoint(Family::Codex, &self.codex_token, "auth.openai.com")?;
        validate_endpoint(
            Family::Codex,
            &self.codex_device_usercode,
            "auth.openai.com",
        )?;
        validate_endpoint(Family::Codex, &self.codex_device_token, "auth.openai.com")?;
        validate_endpoint(Family::Codex, &self.codex_device_page, "auth.openai.com")?;
        validate_endpoint(Family::Codex, &self.codex_revoke, "auth.openai.com")?;
        validate_endpoint(Family::Codex, &self.codex_models_base, "chatgpt.com")?;
        validate_endpoint(Family::Anthropic, &self.claude_authorize, "claude.ai")?;
        validate_endpoint(Family::Anthropic, &self.claude_token, "platform.claude.com")?;
        Ok(())
    }
}

/// A single Codex or Claude sign-in attempt.
///
/// The store and cache paths are supplied by the host. The callback receives
/// browser redirects on the bound loopback listener; Codex falls back to device
/// polling when its port is busy. For Claude paste login, call
/// [`LoginFlow::take_paste_sender`] before `run` and send one value from the
/// host's input surface. Cancellation drops the flow's one-shot receiver; if
/// the host took the sender, it is disconnected and the host should drop it.
/// No background task survives cancellation.
pub struct LoginFlow<'a> {
    provider: LoginProvider,
    store: &'a mut AuthStore,
    client: Client,
    oauth_client: Client,
    user_agent: String,
    cache_dir: PathBuf,
    endpoints: LoginEndpoints,
    codex_provider: Option<ProviderEntry>,
    paste_sender: Option<oneshot::Sender<String>>,
    paste_receiver: Option<oneshot::Receiver<String>>,
    wait: Duration,
    device_auth: bool,
    started: bool,
}

impl fmt::Debug for LoginFlow<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginFlow")
            .field("provider", &self.provider)
            .field("auth_path", &self.store.path())
            .field("cache_dir", &self.cache_dir)
            .field("started", &self.started)
            .finish_non_exhaustive()
    }
}

impl<'a> LoginFlow<'a> {
    /// Creates a flow for `openai-codex` or `anthropic`.
    ///
    /// The supplied client is used only for the post-login catalog fetch.
    /// Credential-bearing OAuth calls use a private client with redirects
    /// disabled, so a 307/308 cannot forward codes, verifiers, or refresh tokens.
    /// `cache_dir` is the host's separate model-cache directory.
    ///
    /// # Errors
    /// Returns [`ProviderError::AuthWrite`] for an unsupported provider, or a
    /// transport error if production endpoints or the private OAuth client
    /// cannot be constructed.
    pub fn new(
        provider: &str,
        store: &'a mut AuthStore,
        client: Client,
        user_agent: impl Into<String>,
        cache_dir: impl Into<PathBuf>,
    ) -> Result<Self, ProviderError> {
        let provider = match provider {
            "openai-codex" => LoginProvider::Codex,
            "anthropic" => LoginProvider::Claude,
            _ => {
                return Err(ProviderError::AuthWrite {
                    reason: format!("{provider} has no OAuth login flow"),
                });
            }
        };
        let oauth_client = build_oauth_client(provider.family())?;
        let (paste_sender, paste_receiver) = match provider {
            LoginProvider::Claude => {
                let (sender, receiver) = oneshot::channel();
                (Some(sender), Some(receiver))
            }
            LoginProvider::Codex => (None, None),
        };
        Ok(Self {
            provider,
            store,
            client,
            oauth_client,
            user_agent: user_agent.into(),
            cache_dir: cache_dir.into(),
            endpoints: LoginEndpoints::production()?,
            codex_provider: None,
            paste_sender,
            paste_receiver,
            wait: LOGIN_WAIT,
            device_auth: false,
            started: false,
        })
    }

    /// Selects the Codex device-code flow without trying the browser callback.
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

    /// Uses the host's configured built-in Codex route for the post-login model fetch.
    ///
    /// For a loopback endpoint override, the override's model API base takes
    /// precedence so replay tests do not contact the live service.
    ///
    /// # Errors
    /// Returns [`ProviderError::AuthWrite`] when `provider` is not the built-in
    /// `openai-codex` Codex route, or a typed URL error for an invalid base URL.
    pub fn with_codex_provider(mut self, provider: ProviderEntry) -> Result<Self, ProviderError> {
        if provider.id.as_ref() != "openai-codex" || provider.family != Family::Codex {
            return Err(ProviderError::AuthWrite {
                reason: String::from("post-login model fetch requires the openai-codex route"),
            });
        }
        check_base_url(Family::Codex, &provider.base_url)?;
        self.codex_provider = Some(provider);
        Ok(self)
    }

    /// Takes the sole input sender for Claude paste login.
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
    /// apply through callback/device authorization and token exchange; once a
    /// successful exchange is ready to commit, the durable write completes as
    /// one locked atomic transaction. After commit, cancellation only stops the
    /// best-effort Codex model fetch and the committed credential is returned.
    ///
    /// # Errors
    /// Returns a typed login, transport, token-exchange, ID-token, or auth-store
    /// error. A state mismatch and every pre-commit error leave `auth.json`
    /// unchanged.
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
        if cancel.is_cancelled() {
            drop(self.paste_sender.take());
            drop(self.paste_receiver.take());
            return Err(ProviderError::LoginCancelled);
        }
        let credential = Credential::OAuth(oauth);
        if let Err(error) = self.persist(&credential).await {
            drop(self.paste_sender.take());
            drop(self.paste_receiver.take());
            return Err(error);
        }
        if self.provider == LoginProvider::Codex {
            tokio::select! {
                biased;
                () = cancel.cancelled() => {},
                () = self.fetch_codex_models_best_effort(&credential) => {},
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
        let verifier = new_pkce_verifier();
        let challenge = pkce_challenge(&verifier);
        match self.provider {
            LoginProvider::Codex => {
                self.authorize_codex(progress, cancel, deadline, &verifier, &challenge)
                    .await
            }
            LoginProvider::Claude => {
                self.authorize_claude(progress, cancel, &verifier, &challenge)
                    .await
            }
        }
    }

    async fn authorize_codex(
        &self,
        progress: &(dyn Fn(LoginProgress) + Send + Sync),
        cancel: &CancellationToken,
        deadline: Instant,
        verifier: &str,
        challenge: &str,
    ) -> Result<OAuthCredential, ProviderError> {
        if self.device_auth {
            return self
                .authorize_codex_device(progress, cancel, deadline)
                .await;
        }
        let state = new_state();
        let listener = bind_callback(self.endpoints.codex_callback_port).await;
        if let Ok((listener, port)) = listener {
            let redirect = codex_redirect(port);
            let url = codex_authorize_url(
                &self.endpoints.codex_authorize,
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
            let code = callback_code(listener, CODEX_CALLBACK_PATH, &state, Family::Codex).await?;
            report_progress(progress, LoginProgress::Exchanging, cancel)?;
            return self
                .exchange_codex(&code, verifier, &redirect, &[&state, &code, verifier])
                .await;
        }
        self.authorize_codex_device(progress, cancel, deadline)
            .await
    }

    async fn authorize_codex_device(
        &self,
        progress: &(dyn Fn(LoginProgress) + Send + Sync),
        cancel: &CancellationToken,
        deadline: Instant,
    ) -> Result<OAuthCredential, ProviderError> {
        let http = OAuthHttp {
            client: &self.oauth_client,
            user_agent: &self.user_agent,
        };
        let grant = super::device::run(&http, &self.endpoints, deadline, progress, cancel).await?;
        report_progress(progress, LoginProgress::Exchanging, cancel)?;
        self.exchange_codex(
            &grant.authorization_code,
            &grant.code_verifier,
            CODEX_DEVICE_REDIRECT,
            &[&grant.authorization_code, &grant.code_verifier],
        )
        .await
    }

    async fn authorize_claude(
        &mut self,
        progress: &(dyn Fn(LoginProgress) + Send + Sync),
        cancel: &CancellationToken,
        verifier: &str,
        challenge: &str,
    ) -> Result<OAuthCredential, ProviderError> {
        let listener = bind_callback_with_fallback(self.endpoints.claude_callback_port).await;
        let (listener, redirect) =
            self.claude_listener_redirect(listener, challenge, verifier, progress, cancel)?;
        report_progress(
            progress,
            LoginProgress::AskPaste {
                hint: String::from(PASTE_HINT),
            },
            cancel,
        )?;
        let receiver = self.paste_receiver.take();
        drop(self.paste_sender.take());
        let code = if let Some(listener) = listener {
            self.claude_callback_code(listener, receiver, verifier, cancel)
                .await?
        } else {
            let value = receive_paste(receiver, cancel).await?;
            parse_pasted_code(&value, verifier)?
        };
        report_progress(progress, LoginProgress::Exchanging, cancel)?;
        self.exchange_claude(&code, verifier, &redirect, &[&code, verifier])
            .await
    }

    fn claude_listener_redirect(
        &self,
        listener: std::io::Result<(tokio::net::TcpListener, u16)>,
        challenge: &str,
        verifier: &str,
        progress: &(dyn Fn(LoginProgress) + Send + Sync),
        cancel: &CancellationToken,
    ) -> Result<(Option<tokio::net::TcpListener>, String), ProviderError> {
        if let Ok((listener, port)) = listener {
            let redirect = claude_redirect(port);
            let url = claude_authorize_url(
                &self.endpoints.claude_authorize,
                &redirect,
                challenge,
                verifier,
            )?;
            report_progress(
                progress,
                LoginProgress::OpenUrl {
                    url: url.to_string(),
                },
                cancel,
            )?;
            return Ok((Some(listener), redirect));
        }
        let redirect = claude_redirect(self.endpoints.claude_callback_port);
        let url = claude_authorize_url(
            &self.endpoints.claude_authorize,
            &redirect,
            challenge,
            verifier,
        )?;
        report_progress(
            progress,
            LoginProgress::OpenUrl {
                url: url.to_string(),
            },
            cancel,
        )?;
        Ok((None, redirect))
    }

    async fn claude_callback_code(
        &self,
        listener: tokio::net::TcpListener,
        receiver: Option<tokio::sync::oneshot::Receiver<String>>,
        verifier: &str,
        cancel: &CancellationToken,
    ) -> Result<String, ProviderError> {
        let callback = callback_code(listener, CLAUDE_CALLBACK_PATH, verifier, Family::Anthropic);
        tokio::pin!(callback);
        tokio::select! {
            biased;
            result = &mut callback => result,
            pasted = receive_paste(receiver, cancel) => match pasted {
                Ok(value) => parse_pasted_code(&value, verifier),
                Err(error) if cancel.is_cancelled() => Err(error),
                Err(_) => callback.await,
            }
        }
    }

    async fn exchange_codex(
        &self,
        code: &str,
        verifier: &str,
        redirect: &str,
        secrets: &[&str],
    ) -> Result<OAuthCredential, ProviderError> {
        let http = OAuthHttp {
            client: &self.oauth_client,
            user_agent: &self.user_agent,
        };
        let body = {
            let mut form = url::form_urlencoded::Serializer::new(String::new());
            form.append_pair("grant_type", "authorization_code")
                .append_pair("client_id", CODEX_CLIENT_ID)
                .append_pair("code", code)
                .append_pair("code_verifier", verifier)
                .append_pair("redirect_uri", redirect);
            form.finish()
        };
        let response = post_form(&http, Family::Codex, &self.endpoints.codex_token, &body).await?;
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

    async fn exchange_claude(
        &self,
        code: &str,
        verifier: &str,
        redirect: &str,
        secrets: &[&str],
    ) -> Result<OAuthCredential, ProviderError> {
        let http = OAuthHttp {
            client: &self.oauth_client,
            user_agent: &self.user_agent,
        };
        let request = ClaudeTokenRequest {
            grant_type: "authorization_code",
            client_id: CLAUDE_CLIENT_ID,
            code,
            state: verifier,
            redirect_uri: redirect,
            code_verifier: verifier,
        };
        let response = post_json(
            &http,
            Family::Anthropic,
            &self.endpoints.claude_token,
            &request,
        )
        .await?;
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

    async fn persist(&mut self, credential: &Credential) -> Result<(), ProviderError> {
        let path = self.store.path().to_path_buf();
        let provider = self.provider.id();
        let credential = credential.clone();
        let lock = crate::auth::refresh::lock_auth_file(&path).await?;
        let updated = crate::auth::refresh::blocking(move || {
            let _lock = lock;
            let mut latest = AuthStore::load(&path)?;
            latest.set(provider, credential)?;
            latest.store()?;
            Ok(latest)
        })
        .await?;
        *self.store = updated;
        Ok(())
    }

    async fn fetch_codex_models_best_effort(&self, credential: &Credential) {
        let default_entry = ProviderEntry {
            id: Box::from("openai-codex"),
            family: Family::Codex,
            base_url: self.endpoints.codex_models_base.as_str().into(),
            transport: Transport::Https,
            key_env: None,
            auth: AuthStyle::Bearer,
            max_concurrent_requests: 1,
        };
        let entry = self
            .codex_provider
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LoginProvider {
    Codex,
    Claude,
}

impl LoginProvider {
    const fn id(self) -> &'static str {
        match self {
            Self::Codex => "openai-codex",
            Self::Claude => "anthropic",
        }
    }
    const fn family(self) -> Family {
        match self {
            Self::Codex => Family::Codex,
            Self::Claude => Family::Anthropic,
        }
    }
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
    let path = path.into();
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
            })?;
        Ok(())
    })
    .await?;
    let provider = provider.to_owned();
    let lock = crate::auth::refresh::lock_auth_file(&path).await?;
    crate::auth::refresh::blocking(move || {
        let _lock = lock;
        let mut store = AuthStore::load(&path)?;
        store.set(
            &provider,
            Credential::ApiKey {
                key: SecretString::from(key),
            },
        )?;
        store.store()
    })
    .await
}

/// Removes `provider`'s local credential, revoking Codex refresh tokens on a
/// best-effort basis before the atomic local write.
///
/// Repeated logout and a provider with no stored entry succeed without a remote
/// request. Claude and API-key credentials are removed locally; Codex OAuth
/// sends one revoke request to the pinned production endpoint. The revoke is
/// best effort: a network/status failure never prevents local logout.
///
/// # Errors
/// Returns an auth-store or lock error if the durable local removal fails.
pub async fn logout(provider: &str, store: &mut AuthStore) -> Result<(), ProviderError> {
    let user_agent = default_user_agent();
    logout_with(provider, store, &user_agent, &LoginEndpoints::production()?).await
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
    if let ("openai-codex", Credential::OAuth(oauth)) = (provider, &stored) {
        endpoints.validate()?;
        let request = RevokeRequest {
            token: oauth.refresh_token.expose(),
            token_type_hint: "refresh_token",
            client_id: CODEX_CLIENT_ID,
        };
        if let Ok(client) = build_oauth_client(Family::Codex) {
            let http = OAuthHttp {
                client: &client,
                user_agent,
            };
            match post_json(&http, Family::Codex, &endpoints.codex_revoke, &request).await {
                Ok(response) if (200..300).contains(&response.status) => {}
                Ok(response) => tracing::warn!(
                    status = response.status,
                    "Codex OAuth token revocation failed"
                ),
                Err(_) => tracing::warn!("Codex OAuth token revocation request failed"),
            }
        } else {
            tracing::warn!("Codex OAuth token revocation client failed");
        }
    }
    let updated = crate::auth::refresh::blocking(move || {
        let _lock = lock;
        latest.remove(&provider_owned);
        if latest.status().is_empty() {
            match std::fs::remove_file(&remove_path) {
                Ok(()) => {}
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
        Ok(latest)
    })
    .await?;
    *store = updated;
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

async fn post_form(
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
    }
    truncate_message(&safe)
}

fn truncate_message(message: &str) -> String {
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

#[derive(Serialize)]
struct ClaudeTokenRequest<'a> {
    grant_type: &'static str,
    client_id: &'static str,
    code: &'a str,
    state: &'a str,
    redirect_uri: &'a str,
    code_verifier: &'a str,
}

#[derive(Serialize)]
struct RevokeRequest<'a> {
    token: &'a str,
    token_type_hint: &'static str,
    client_id: &'static str,
}

fn codex_authorize_url(
    endpoint_url: &Url,
    redirect_uri: &str,
    challenge: &str,
    state: &str,
) -> Result<Url, ProviderError> {
    with_query(
        endpoint_url,
        &[
            ("response_type", "code"),
            ("client_id", CODEX_CLIENT_ID),
            ("redirect_uri", redirect_uri),
            (
                "scope",
                "openid profile email offline_access api.connectors.read api.connectors.invoke",
            ),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
            ("state", state),
            ("id_token_add_organizations", "true"),
            ("codex_cli_simplified_flow", "true"),
            ("originator", CODEX_ORIGINATOR),
        ],
        Family::Codex,
        QueryEncoding::Percent20,
    )
}

fn claude_authorize_url(
    endpoint_url: &Url,
    redirect_uri: &str,
    challenge: &str,
    state: &str,
) -> Result<Url, ProviderError> {
    with_query(
        endpoint_url,
        &[
            ("code", "true"),
            ("client_id", CLAUDE_CLIENT_ID),
            ("response_type", "code"),
            ("redirect_uri", redirect_uri),
            (
                "scope",
                "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload",
            ),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
            ("state", state),
        ],
        Family::Anthropic,
        QueryEncoding::Form,
    )
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

fn codex_redirect(port: u16) -> String {
    format!("http://localhost:{port}{CODEX_CALLBACK_PATH}")
}

fn claude_redirect(port: u16) -> String {
    format!("http://localhost:{port}{CLAUDE_CALLBACK_PATH}")
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

fn parse_pasted_code(input: &str, expected_state: &str) -> Result<String, ProviderError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(endpoint_error(
            Family::Anthropic,
            "pasted authorization code is empty",
        ));
    }
    if let Some((code, state)) = input.split_once('#') {
        if state != expected_state {
            return Err(ProviderError::StateMismatch);
        }
        return nonempty_code(code);
    }
    if let Ok(url) = Url::parse(input) {
        if url.query().is_some() {
            return parse_query(url.query().unwrap_or_default(), true, expected_state);
        }
        return Err(endpoint_error(
            Family::Anthropic,
            "pasted redirect URL has no code",
        ));
    }
    if input.starts_with('?') || input.contains('=') {
        return parse_query(input.trim_start_matches('?'), false, expected_state);
    }
    nonempty_code(input)
}

fn parse_query(
    query: &str,
    require_state: bool,
    expected_state: &str,
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
        .ok_or_else(|| endpoint_error(Family::Anthropic, "pasted authorization result has no code"))
}

fn nonempty_code(code: &str) -> Result<String, ProviderError> {
    let code = code.trim();
    if code.is_empty() {
        return Err(endpoint_error(
            Family::Anthropic,
            "pasted authorization code is empty",
        ));
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

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(host)) => host == "localhost",
        Some(url::Host::Ipv4(address)) => address == Ipv4Addr::LOCALHOST,
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

#[cfg(test)]
mod tests;
