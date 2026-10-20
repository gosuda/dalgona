//! The one runner for every sign-in method of the built-in providers.
//!
//! A host front end (terminal, CLI, RPC) never builds a [`LoginFlow`]
//! itself. It names a provider and a [`Method`], hands over a [`LoginIo`]
//! for progress, pasted input, and cancellation, and receives the credential
//! or a typed error. The durable `auth.json` write is always the last step,
//! so a cancelled or failed login leaves the file unchanged.

use std::{convert::Infallible, fmt, path::PathBuf, sync::Arc};

use tokio::{
    sync::{mpsc, oneshot},
    time::timeout,
};
use tokio_util::sync::CancellationToken;

use dal_core::Family;

use crate::{
    AuthStore, Credential, CredentialKind, EnvSnapshot, LoginEndpoints, LoginFlow, LoginProgress,
    PROVIDERS, ProviderDef, ProviderError, ProviderSet, SecretString,
    auth::oauth::{logout_with_grants, store_api_key_unless_cancelled_with_grants, unix_now},
    family::{
        copilot::{
            BUNDLED_POLICY_MODEL_IDS, CopilotKey, DeviceCode, Integration, LoginTarget, Pacing,
            Poll, access_token_request, decode_device_code, decode_endpoint, decode_poll,
            device_code_request, enable_model_policy, endpoint_discovery_request, login_target,
            stored_key,
        },
        gitlab::DirectAccessCache,
    },
    find,
    http::{Exchange, LOGIN_WAIT, OAUTH_TIMEOUT, build_client, read_body, send},
    provider::GITHUB_COPILOT,
};

/// How a user proves who they are to a provider.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Method {
    /// A pasted API key.
    ApiKey,
    /// An OAuth authorization in the user's browser, completed by a loopback
    /// callback or a pasted redirect.
    Browser,
    /// An OAuth device code entered at a verification URL.
    Device,
}

impl Method {
    /// The stable lowercase name (`api_key`, `browser`, `device`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ApiKey => "api_key",
            Self::Browser => "browser",
            Self::Device => "device",
        }
    }
}

impl fmt::Display for Method {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The providers dal signs in to, in display order (by id). Each row names the
/// methods it offers through [`ProviderDef::methods`]. Front ends and wires
/// read this list instead of keeping their own.
#[must_use]
pub fn login_providers() -> Vec<&'static ProviderDef> {
    let mut rows: Vec<_> = PROVIDERS
        .iter()
        .filter(|def| def.methods().next().is_some())
        .collect();
    rows.sort_by_key(|def| def.id);
    rows
}

/// Capacity of the progress channel [`LoginIo::channel`] builds. A flow
/// reports at most four events, so a reader that is a little slow never
/// loses one.
pub const PROGRESS_CAPACITY: usize = 8;

/// What a login reads from and reports to its front end.
#[derive(Debug)]
pub struct LoginIo {
    /// Receives each [`LoginProgress`] the flow reports. Its capacity must be
    /// at least [`PROGRESS_CAPACITY`]; a closed or full channel cancels the
    /// login, because the front end that should show the next step is gone.
    pub progress: mpsc::Sender<LoginProgress>,
    /// One pasted value: the redirect URL or code for a flow that asks, or
    /// the key for [`Method::ApiKey`]. Dropping the sender closes the paste
    /// path; a browser callback can still complete the flow.
    pub paste: Option<oneshot::Receiver<String>>,
    /// Cancels the login. Cancellation never reports success.
    pub cancel: CancellationToken,
}

impl LoginIo {
    /// Builds an io with a progress channel of [`PROGRESS_CAPACITY`] and
    /// returns the receiving end.
    #[must_use]
    pub fn channel(
        paste: Option<oneshot::Receiver<String>>,
        cancel: CancellationToken,
    ) -> (Self, mpsc::Receiver<LoginProgress>) {
        let (progress, events) = mpsc::channel(PROGRESS_CAPACITY);
        (
            Self {
                progress,
                paste,
                cancel,
            },
            events,
        )
    }
}

/// Where a login reads and writes: the credential file, the model cache, the
/// request identity, and the endpoints.
#[derive(Clone)]
pub struct LoginSite {
    auth_path: PathBuf,
    cache_dir: PathBuf,
    user_agent: String,
    endpoints: LoginEndpoints,
    gitlab_grants: Option<Arc<DirectAccessCache>>,
}

impl fmt::Debug for LoginSite {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoginSite")
            .field("auth_path", &self.auth_path)
            .field("cache_dir", &self.cache_dir)
            .field("user_agent", &self.user_agent)
            .field("endpoints", &self.endpoints)
            .field("gitlab_grants", &self.gitlab_grants.is_some())
            .finish_non_exhaustive()
    }
}

impl LoginSite {
    /// Creates a site that talks to the pinned production endpoints.
    ///
    /// `env` is the environment the binary captured at startup. A row that
    /// names a client-id override variable (`GitLab` Duo) signs in, exchanges,
    /// and revokes with that variable's non-blank value instead of its pinned
    /// id, and honors its redirect override the same way.
    ///
    /// # Errors
    /// Returns a typed transport error when a production endpoint cannot be
    /// constructed.
    pub fn new(
        auth_path: impl Into<PathBuf>,
        cache_dir: impl Into<PathBuf>,
        user_agent: impl Into<String>,
        env: &EnvSnapshot,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            auth_path: auth_path.into(),
            cache_dir: cache_dir.into(),
            user_agent: user_agent.into(),
            endpoints: LoginEndpoints::production(env)?,
            gitlab_grants: None,
        })
    }

    /// Replaces the production endpoints with literal-loopback ones, for
    /// replay servers.
    ///
    /// # Errors
    /// Returns a typed transport error when an endpoint leaves the allowed
    /// origins.
    pub fn with_endpoints(mut self, endpoints: LoginEndpoints) -> Result<Self, ProviderError> {
        endpoints.validate()?;
        self.endpoints = endpoints;
        Ok(self)
    }

    /// Attaches the host's shared GitLab `direct_access` grant cache.
    ///
    /// This clones the handle owned by [`ProviderSet`] so turns and login share
    /// one allocation. A site without a set keeps `None` and behaves as before.
    #[must_use]
    pub fn with_provider_set(mut self, set: &ProviderSet) -> Self {
        self.gitlab_grants = Some(set.gitlab_grants());
        self
    }

    /// The `auth.json` path.
    #[must_use]
    pub fn auth_path(&self) -> &std::path::Path {
        &self.auth_path
    }
}

/// Runs one sign-in and returns the stored credential.
///
/// The OAuth methods drive [`LoginFlow`] under its 15-minute deadline.
/// [`Method::ApiKey`] waits for one value on [`LoginIo::paste`] under the same
/// deadline, then stores it. Every path ends with the locked, atomic
/// `auth.json` write. Cancellation that arrives before that write commits
/// reports [`ProviderError::LoginCancelled`] and leaves the file unchanged;
/// once the write has committed, the stored credential is returned.
///
/// # Errors
/// Returns [`ProviderError::LoginInput`] when the provider does not offer the
/// method or the key is empty, [`ProviderError::LoginCancelled`] after
/// cancellation, [`ProviderError::LoginTimeout`] past the deadline, and the
/// flow's typed error otherwise. A failed login leaves `auth.json` unchanged:
/// an existing file keeps its bytes and an absent one stays absent.
pub async fn login(
    provider: &str,
    method: Method,
    io: LoginIo,
    site: &LoginSite,
) -> Result<Credential, ProviderError> {
    let offered = find(provider).is_some_and(|def| def.offers(method));
    if !offered {
        return Err(ProviderError::LoginInput {
            reason: format!("{provider} does not sign in with {method}."),
        });
    }
    match method {
        Method::ApiKey => store_key(provider, io, site).await,
        Method::Browser => run_flow(provider, method, io, site).await,
        Method::Device => {
            if provider == GITHUB_COPILOT.id {
                login_copilot_device(io, site).await
            } else {
                run_flow(provider, method, io, site).await
            }
        }
    }
}

async fn login_copilot_device(io: LoginIo, site: &LoginSite) -> Result<Credential, ProviderError> {
    let LoginIo {
        progress,
        paste,
        cancel,
    } = io;

    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(ProviderError::LoginCancelled),
        res = timeout(LOGIN_WAIT, execute_copilot_device_flow(&progress, paste, &cancel, site)) => {
            match res {
                Ok(inner) => inner,
                Err(_) => Err(ProviderError::LoginTimeout),
            }
        }
    }
}

async fn execute_copilot_device_flow(
    progress: &mpsc::Sender<LoginProgress>,
    paste: Option<oneshot::Receiver<String>>,
    cancel: &CancellationToken,
    site: &LoginSite,
) -> Result<Credential, ProviderError> {
    let target = resolve_copilot_login_target(progress, paste, cancel).await?;
    let client = build_client();

    let device_code = request_copilot_device_code(&client, &target, site, cancel).await?;
    emit_copilot_progress(
        progress,
        LoginProgress::ShowCode {
            url: device_code.verification_uri.to_string(),
            code: device_code.user_code.to_string(),
        },
        cancel,
    )?;

    let github_token =
        poll_copilot_access_token(&client, &target, &device_code, site, cancel).await?;
    emit_copilot_progress(progress, LoginProgress::Exchanging, cancel)?;

    let serialized = stored_key(&github_token, target.enterprise_domain.as_deref(), None)?;
    let mut copilot_key = CopilotKey::parse(serialized.expose());

    let discovered = discover_copilot_endpoint(&client, &copilot_key, site, cancel).await?;
    if let Some(endpoint) = discovered.as_deref() {
        copilot_key = copilot_key.with_api_endpoint(endpoint);
    }

    enable_copilot_models(&client, &copilot_key, site, cancel).await?;

    let final_key = stored_key(
        &github_token,
        target.enterprise_domain.as_deref(),
        discovered.as_deref(),
    )?;

    if cancel.is_cancelled() {
        return Err(ProviderError::LoginCancelled);
    }

    store_api_key_unless_cancelled_with_grants(
        site.auth_path.clone(),
        GITHUB_COPILOT.id,
        final_key.expose().to_owned(),
        cancel,
        site.gitlab_grants.clone(),
    )
    .await?;

    Ok(Credential::ApiKey { key: final_key })
}

fn emit_copilot_progress(
    progress: &mpsc::Sender<LoginProgress>,
    event: LoginProgress,
    cancel: &CancellationToken,
) -> Result<(), ProviderError> {
    if progress.try_send(event).is_err() {
        cancel.cancel();
        return Err(ProviderError::LoginCancelled);
    }
    Ok(())
}

async fn resolve_copilot_login_target(
    progress: &mpsc::Sender<LoginProgress>,
    paste: Option<oneshot::Receiver<String>>,
    cancel: &CancellationToken,
) -> Result<LoginTarget, ProviderError> {
    let Some(receiver) = paste else {
        return login_target("");
    };

    emit_copilot_progress(
        progress,
        LoginProgress::AskPaste {
            hint: String::from("GitHub Enterprise URL/domain (blank for github.com)"),
            default: Some(String::from("github.com")),
        },
        cancel,
    )?;

    let answer = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(ProviderError::LoginCancelled),
        pasted = receiver => {
            if let Ok(text) = pasted {
                text
            } else {
                cancel.cancel();
                return Err(ProviderError::LoginCancelled);
            }
        }
    };

    login_target(&answer)
}

async fn request_copilot_device_code(
    client: &reqwest::Client,
    target: &LoginTarget,
    site: &LoginSite,
    cancel: &CancellationToken,
) -> Result<DeviceCode, ProviderError> {
    let mut wire = device_code_request(target);
    if let Some(base) = site.endpoints.copilot().device.as_ref() {
        wire.base_url = base.as_str().trim_end_matches('/').to_owned();
    }
    let url = wire.endpoint()?;
    let user_agent = wire.user_agent;
    let req = wire.into_request(client, url);

    let send_op = send(
        Family::Chat,
        req,
        user_agent,
        Exchange::Json {
            total: OAUTH_TIMEOUT,
        },
        tokio::time::sleep,
    );

    let response = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(ProviderError::LoginCancelled),
        res = send_op => res?,
    };

    let status = response.status().as_u16();
    let body = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(ProviderError::LoginCancelled),
        res = read_body(Family::Chat, response) => res?,
    };

    if status != 200 {
        return Err(ProviderError::DeviceCode {
            status,
            message: format!("device code request failed with status {status}"),
        });
    }

    decode_device_code(&body)
}

async fn poll_copilot_access_token(
    client: &reqwest::Client,
    target: &LoginTarget,
    device_code: &DeviceCode,
    site: &LoginSite,
    cancel: &CancellationToken,
) -> Result<SecretString, ProviderError> {
    let deadline = tokio::time::Instant::now() + device_code.expires_in;
    let mut pacing = Pacing::new(device_code.interval);

    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ProviderError::LoginCancelled),
            () = tokio::time::sleep(pacing.wait()) => {}
        }

        if tokio::time::Instant::now() >= deadline {
            return Err(ProviderError::LoginTimeout);
        }

        let mut wire = access_token_request(target, device_code.device_code.expose());
        if let Some(base) = site.endpoints.copilot().device.as_ref() {
            wire.base_url = base.as_str().trim_end_matches('/').to_owned();
        }
        let url = wire.endpoint()?;
        let user_agent = wire.user_agent;
        let req = wire.into_request(client, url);

        let send_op = send(
            Family::Chat,
            req,
            user_agent,
            Exchange::Json {
                total: OAUTH_TIMEOUT,
            },
            tokio::time::sleep,
        );

        let response = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ProviderError::LoginCancelled),
            res = send_op => res?,
        };

        let body = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ProviderError::LoginCancelled),
            res = read_body(Family::Chat, response) => res?,
        };

        match decode_poll(&body)? {
            Poll::Token(token) => return Ok(token),
            Poll::Pending => {}
            Poll::SlowDown { interval } => pacing.slow_down(interval),
        }
    }
}

async fn discover_copilot_endpoint(
    client: &reqwest::Client,
    copilot_key: &CopilotKey,
    site: &LoginSite,
    cancel: &CancellationToken,
) -> Result<Option<String>, ProviderError> {
    let mut wire = endpoint_discovery_request(copilot_key)?;
    wire.base_url = site
        .endpoints
        .copilot()
        .discovery
        .as_str()
        .trim_end_matches('/')
        .to_owned();

    let url = wire.endpoint()?;
    let user_agent = wire.user_agent;
    let req = wire.into_request(client, url);

    let send_op = send(
        Family::Chat,
        req,
        user_agent,
        Exchange::Json {
            total: OAUTH_TIMEOUT,
        },
        tokio::time::sleep,
    );

    let response = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(ProviderError::LoginCancelled),
        res = send_op => res,
    };

    let Ok(response) = response else {
        return Ok(None);
    };

    if !response.status().is_success() {
        return Ok(None);
    }

    let body = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(ProviderError::LoginCancelled),
        res = read_body(Family::Chat, response) => match res {
            Ok(body) => body,
            Err(_) => return Ok(None),
        },
    };

    Ok(decode_endpoint(&body))
}

async fn enable_copilot_models(
    client: &reqwest::Client,
    copilot_key: &CopilotKey,
    site: &LoginSite,
    cancel: &CancellationToken,
) -> Result<(), ProviderError> {
    let configured_base = site
        .endpoints
        .copilot()
        .policy
        .as_ref()
        .map_or(GITHUB_COPILOT.base_url, |base| {
            base.as_str().trim_end_matches('/')
        });

    let integration = Integration::Chat;
    for model_id in BUNDLED_POLICY_MODEL_IDS {
        if cancel.is_cancelled() {
            return Err(ProviderError::LoginCancelled);
        }
        enable_one_copilot_model(
            client,
            configured_base,
            copilot_key,
            model_id,
            &integration,
            site,
            cancel,
        )
        .await?;
    }

    Ok(())
}

async fn enable_one_copilot_model(
    client: &reqwest::Client,
    configured_base: &str,
    copilot_key: &CopilotKey,
    model_id: &str,
    integration: &Integration,
    site: &LoginSite,
    cancel: &CancellationToken,
) -> Result<(), ProviderError> {
    let Ok(mut policy_wire) =
        enable_model_policy(configured_base, copilot_key, model_id, integration)
    else {
        return Ok(());
    };
    if let Some(base) = site.endpoints.copilot().policy.as_ref() {
        policy_wire.base_url = base.as_str().trim_end_matches('/').to_owned();
    }
    let Ok(url) = policy_wire.endpoint() else {
        return Ok(());
    };
    let user_agent = policy_wire.user_agent;
    let req = policy_wire.into_request(client, url);

    let send_op = send(
        Family::Chat,
        req,
        user_agent,
        Exchange::Json {
            total: OAUTH_TIMEOUT,
        },
        tokio::time::sleep,
    );

    let response = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(ProviderError::LoginCancelled),
        res = send_op => res,
    };

    match response {
        Ok(response) if response.status().is_success() => {}
        Ok(response) => {
            let status = response.status().as_u16();
            tracing::debug!(
                provider = GITHUB_COPILOT.id,
                model = model_id,
                status,
                "copilot model policy activation refused (best-effort)"
            );
        }
        Err(_) => {
            tracing::debug!(
                provider = GITHUB_COPILOT.id,
                model = model_id,
                "copilot model policy request failed (best-effort)"
            );
        }
    }

    Ok(())
}

async fn store_key(
    provider: &str,
    io: LoginIo,
    site: &LoginSite,
) -> Result<Credential, ProviderError> {
    let LoginIo { paste, cancel, .. } = io;
    let Some(paste) = paste else {
        return Err(ProviderError::LoginInput {
            reason: String::from("no API key was supplied."),
        });
    };
    let received = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(ProviderError::LoginCancelled),
        received = timeout(LOGIN_WAIT, paste) => received,
    };
    let key = match received {
        Ok(Ok(key)) => key.trim().to_owned(),
        Ok(Err(_)) => return Err(ProviderError::LoginCancelled),
        Err(_) => return Err(ProviderError::LoginTimeout),
    };
    if key.is_empty() {
        return Err(ProviderError::LoginInput {
            reason: String::from("the API key is empty."),
        });
    }
    if cancel.is_cancelled() {
        return Err(ProviderError::LoginCancelled);
    }
    // Single commit (inside-closure old+new invalidation at the real commit
    // point); pre-commit Err paths above leave the cache untouched.
    store_api_key_unless_cancelled_with_grants(
        site.auth_path.clone(),
        provider,
        key.clone(),
        &cancel,
        site.gitlab_grants.clone(),
    )
    .await?;
    Ok(Credential::ApiKey {
        key: SecretString::from(key),
    })
}

async fn forward_paste(
    flow_paste: Option<oneshot::Sender<String>>,
    paste: Option<oneshot::Receiver<String>>,
) -> Infallible {
    let Some(receiver) = paste else {
        drop(flow_paste);
        return std::future::pending::<Infallible>().await;
    };
    let Ok(text) = receiver.await else {
        drop(flow_paste);
        return std::future::pending::<Infallible>().await;
    };
    if let Some(sender) = flow_paste {
        let _flow_ended = sender.send(text);
    }
    std::future::pending::<Infallible>().await
}

async fn run_flow(
    provider: &str,
    method: Method,
    io: LoginIo,
    site: &LoginSite,
) -> Result<Credential, ProviderError> {
    let mut store = AuthStore::load(&site.auth_path)?;
    let mut flow = LoginFlow::new(
        provider,
        &mut store,
        build_client(),
        site.user_agent.clone(),
        site.cache_dir.clone(),
        site.endpoints.clone(),
    )?;
    if method == Method::Device {
        flow = flow.with_device_auth();
    }
    // Clone-only, no second allocation; `None` keeps standalone flows valid.
    flow = flow.with_gitlab_grants(site.gitlab_grants.clone());
    let flow_paste = flow.take_paste_sender();
    let LoginIo {
        progress,
        paste,
        cancel,
    } = io;
    let report = |event: LoginProgress| {
        if progress.try_send(event).is_err() {
            cancel.cancel();
        }
    };
    let forward = forward_paste(flow_paste, paste);
    tokio::select! {
        biased;
        result = flow.run(&report, &cancel) => result,
        never = forward => match never {},
    }
}

/// Secret-free state of one stored credential.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredCredential {
    /// The provider id.
    pub provider: Box<str>,
    /// The stored credential kind.
    pub kind: CredentialKind,
    /// Whether an OAuth access token is inside the proactive refresh window
    /// or past its expiry, so the next request refreshes it.
    pub expired: bool,
    /// The stored OAuth `expires_at`, in seconds since the Unix epoch, when
    /// the credential is an OAuth one that names an expiry.
    pub expires_at: Option<i64>,
}

/// Lists the credentials stored in `auth.json`, in stable provider order.
///
/// # Errors
/// Returns the typed auth-store error when `auth.json` cannot be read.
pub fn stored_credentials(site: &LoginSite) -> Result<Vec<StoredCredential>, ProviderError> {
    let store = AuthStore::load(&site.auth_path)?;
    let now = unix_now();
    Ok(store
        .status()
        .into_iter()
        .map(|status| {
            let credential = store.credential(&status.provider);
            let expired = matches!(
                &credential,
                Some(Credential::OAuth(oauth)) if oauth.expiring(now)
            );
            StoredCredential {
                provider: status.provider,
                kind: status.kind,
                expired,
                expires_at: match credential {
                    Some(Credential::OAuth(oauth)) => oauth.expires_at,
                    _ => None,
                },
            }
        })
        .collect())
}

/// Removes one provider's stored credential, or every stored credential when
/// `provider` is `None`, and returns the providers that had an entry.
///
/// An OAuth entry of a provider that publishes a revocation endpoint (Codex,
/// `GitLab` Duo) is revoked on a best-effort basis first; a provider with no
/// entry makes no request and is not listed.
///
/// # Errors
/// Returns the typed auth-store or lock error when the durable removal
/// fails.
pub async fn sign_out(
    provider: Option<&str>,
    site: &LoginSite,
) -> Result<Vec<Box<str>>, ProviderError> {
    let mut store = AuthStore::load(&site.auth_path)?;
    let targets: Vec<Box<str>> = store
        .status()
        .into_iter()
        .map(|status| status.provider)
        .filter(|id| provider.is_none_or(|wanted| wanted == &**id))
        .collect();
    for target in &targets {
        logout_with_grants(
            target,
            &mut store,
            &site.user_agent,
            &site.endpoints,
            site.gitlab_grants.as_ref(),
        )
        .await?;
    }
    Ok(targets)
}

#[cfg(test)]
mod tests;
