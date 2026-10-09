//! The one runner for every sign-in method of the built-in providers.
//!
//! A host front end (terminal, CLI, RPC) never builds a [`LoginFlow`]
//! itself. It names a provider and a [`Method`], hands over a [`LoginIo`]
//! for progress, pasted input, and cancellation, and receives the credential
//! or a typed error. The durable `auth.json` write is always the last step,
//! so a cancelled or failed login leaves the file unchanged.

use std::{convert::Infallible, fmt, path::PathBuf};

use tokio::{
    sync::{mpsc, oneshot},
    time::timeout,
};
use tokio_util::sync::CancellationToken;

use crate::{
    AuthStore, Credential, CredentialKind, LoginEndpoints, LoginFlow, LoginProgress, ProviderError,
    SecretString,
    auth::oauth::{logout_with, store_api_key, unix_now},
    http::{LOGIN_WAIT, build_client},
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

const LOGIN_PROVIDERS: &[(&str, &[Method])] = &[
    ("anthropic", &[Method::ApiKey, Method::Browser]),
    ("openai", &[Method::ApiKey]),
    ("openai-codex", &[Method::Browser, Method::Device]),
];

/// The providers dal signs in to, in display order, with the methods each
/// one offers. Front ends and wires read this list instead of keeping their
/// own.
#[must_use]
pub fn login_providers() -> &'static [(&'static str, &'static [Method])] {
    LOGIN_PROVIDERS
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
#[derive(Clone, Debug)]
pub struct LoginSite {
    auth_path: PathBuf,
    cache_dir: PathBuf,
    user_agent: String,
    endpoints: LoginEndpoints,
}

impl LoginSite {
    /// Creates a site that talks to the pinned production endpoints.
    ///
    /// # Errors
    /// Returns a typed transport error when a production endpoint cannot be
    /// constructed.
    pub fn new(
        auth_path: impl Into<PathBuf>,
        cache_dir: impl Into<PathBuf>,
        user_agent: impl Into<String>,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            auth_path: auth_path.into(),
            cache_dir: cache_dir.into(),
            user_agent: user_agent.into(),
            endpoints: LoginEndpoints::production()?,
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
/// `auth.json` write.
///
/// # Errors
/// Returns [`ProviderError::LoginInput`] when the provider does not offer the
/// method or the key is empty, [`ProviderError::LoginCancelled`] after
/// cancellation, [`ProviderError::LoginTimeout`] past the deadline, and the
/// flow's typed error otherwise. A failed login leaves `auth.json` unchanged.
pub async fn login(
    provider: &str,
    method: Method,
    io: LoginIo,
    site: &LoginSite,
) -> Result<Credential, ProviderError> {
    let offered = LOGIN_PROVIDERS
        .iter()
        .any(|(id, methods)| *id == provider && methods.contains(&method));
    if !offered {
        return Err(ProviderError::LoginInput {
            reason: format!("{provider} does not sign in with {method}."),
        });
    }
    match method {
        Method::ApiKey => store_key(provider, io, site).await,
        Method::Browser | Method::Device => run_flow(provider, method, io, site).await,
    }
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
    store_api_key(&site.auth_path, provider, key.clone()).await?;
    Ok(Credential::ApiKey {
        key: SecretString::from(key),
    })
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
    )?
    .with_endpoints(site.endpoints.clone())?;
    if method == Method::Device {
        flow = flow.with_device_auth();
    }
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
    let forward = async move {
        if let (Some(sender), Some(receiver)) = (flow_paste, paste)
            && let Ok(text) = receiver.await
        {
            let _flow_ended = sender.send(text);
        }
        std::future::pending::<Infallible>().await
    };
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
            let expired = matches!(
                store.credential(&status.provider),
                Some(Credential::OAuth(oauth)) if oauth.expiring(now)
            );
            StoredCredential {
                provider: status.provider,
                kind: status.kind,
                expired,
            }
        })
        .collect())
}

/// Removes one provider's stored credential, or every stored credential when
/// `provider` is `None`, and returns the providers that had an entry.
///
/// A Codex OAuth entry is revoked on a best-effort basis first; a provider
/// with no entry makes no request and is not listed.
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
        logout_with(target, &mut store, &site.user_agent, &site.endpoints).await?;
    }
    Ok(targets)
}

#[cfg(test)]
mod tests;
