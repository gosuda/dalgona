//! Host-owned provider resources and their lifetime.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
};

use dal_core::SessionId;
use tokio::sync::Semaphore;

use crate::{
    auth::{
        credential::{self, Credential, EnvSnapshot},
        refresh::{Refresher, TokenEndpoints},
    },
    catalog::ResolvedModel,
    error::ProviderError,
    http,
    provider::{Provider, ProviderConfig, ProviderEntry},
    scripted::Script,
    usage::UsageChecker,
    ws::WsSessions,
};

/// All external identity needed to render the provider user-agent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderIdentity {
    /// Application version.
    pub version: Box<str>,
    /// Operating-system name.
    pub os: Box<str>,
    /// Operating-system version.
    pub os_version: Box<str>,
    /// Target architecture.
    pub arch: Box<str>,
}

impl ProviderIdentity {
    /// Builds the user-agent used on every provider request.
    #[must_use]
    pub fn user_agent(&self) -> String {
        http::user_agent(&self.version, &self.os, &self.os_version, &self.arch)
    }
}

/// One host's provider clients, per-provider admission limits, and session state.
///
/// Providers hold a weak reference to this owner: dropping the set releases
/// its clients, WebSocket pool, refresh locks, and per-session notice state.
#[must_use]
pub struct ProviderSet {
    pub(super) inner: Arc<ProviderSetInner>,
}

pub(super) struct ProviderSetInner {
    pub(super) config: ProviderConfig,
    pub(super) identity: ProviderIdentity,
    pub(super) user_agent: Box<str>,
    pub(super) data_dir: PathBuf,
    pub(super) cache_dir: PathBuf,
    pub(super) env: EnvSnapshot,
    pub(super) clients: HashMap<Box<str>, http::LazyClient>,
    pub(super) providers: HashMap<Box<str>, ProviderSlot>,
    pub(super) ws: WsSessions,
    pub(super) notices: Mutex<HashMap<SessionId, crate::thinking::SessionNotices>>,
    pub(super) refresher: Arc<Refresher>,
    pub(super) usage: HashMap<Box<str>, Arc<UsageChecker>>,
    pub(super) scripted: Option<Script>,
}

pub(super) struct ProviderSlot {
    pub(super) entry: ProviderEntry,
    pub(super) client: http::LazyClient,
    pub(super) permits: Arc<Semaphore>,
}

impl ProviderSet {
    /// Builds a host provider set using the caller's captured environment.
    ///
    /// Each origin shares one reqwest client. Provider concurrency limits,
    /// WebSocket sessions, refresh ownership, model caches, and notices are
    /// scoped to this set; no process-global registry is used.
    ///
    /// # Errors
    /// Returns the URL admission or usage-endpoint error for invalid provider
    /// configuration. The auth file is loaded only when a credential is resolved.
    pub fn new(
        config: &ProviderConfig,
        identity: ProviderIdentity,
        env: EnvSnapshot,
        data_dir: &Path,
        cache_dir: &Path,
    ) -> Result<Self, ProviderError> {
        Self::build(
            config,
            identity,
            env,
            data_dir,
            cache_dir,
            TokenEndpoints::production(),
        )
    }

    pub(super) fn build(
        config: &ProviderConfig,
        identity: ProviderIdentity,
        env: EnvSnapshot,
        data_dir: &Path,
        cache_dir: &Path,
        endpoints: TokenEndpoints,
    ) -> Result<Self, ProviderError> {
        let user_agent = identity.user_agent().into_boxed_str();
        let mut clients: HashMap<Box<str>, http::LazyClient> = HashMap::new();
        let mut providers = HashMap::with_capacity(config.providers.len());
        let mut usage = HashMap::new();

        for provider in &config.providers {
            http::check_base_url(provider.family, &provider.base_url)?;
            let origin = origin_key(&provider.base_url, provider.family)?;
            let client = clients.entry(origin).or_default().clone();
            let concurrency = usize::try_from(provider.max_concurrent_requests)
                .ok()
                .filter(|limit| {
                    (1..=(super::config::MAX_CONCURRENT_REQUESTS as usize)).contains(limit)
                })
                .ok_or_else(|| ProviderError::InvalidRequest {
                    message: format!("provider {} has an invalid concurrency limit", provider.id),
                })?;
            if providers
                .insert(
                    provider.id.clone(),
                    ProviderSlot {
                        entry: provider.clone(),
                        client: client.clone(),
                        permits: Arc::new(Semaphore::new(concurrency)),
                    },
                )
                .is_some()
            {
                return Err(ProviderError::InvalidRequest {
                    message: format!("provider {} is configured more than once", provider.id),
                });
            }
            if provider.family == dal_core::Family::Codex
                && usage
                    .insert(
                        provider.id.clone(),
                        Arc::new(UsageChecker::new(client, &provider.base_url, &user_agent)?),
                    )
                    .is_some()
            {
                return Err(ProviderError::InvalidRequest {
                    message: format!(
                        "Codex provider {} is configured more than once",
                        provider.id
                    ),
                });
            }
        }

        let refresher = Arc::new(Refresher::new(
            user_agent.clone(),
            data_dir.join("auth.json"),
            endpoints,
        ));
        let scripted = match &config.scripted {
            None => None,
            Some(selection) => {
                let path = std::path::Path::new(selection.fixture.as_ref());
                let path = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    data_dir.join(path)
                };
                Some(Script::from_replay_file(&path).map_err(ProviderError::Script)?)
            }
        };
        Ok(Self {
            inner: Arc::new(ProviderSetInner {
                config: config.clone(),
                identity,
                user_agent,
                data_dir: data_dir.to_path_buf(),
                cache_dir: cache_dir.to_path_buf(),
                env,
                clients,
                providers,
                ws: WsSessions::new(),
                notices: Mutex::new(HashMap::new()),
                refresher,
                usage,
                scripted,
            }),
        })
    }

    /// Resolves one provider's credential from auth.json and the captured
    /// environment snapshot. The process environment is never read here.
    ///
    /// # Errors
    /// Returns `InvalidRequest` if `provider_id` names no configured
    /// provider, and auth-store or credential errors otherwise.
    pub fn credential(&self, provider_id: &str) -> Result<Credential, ProviderError> {
        let slot =
            self.inner
                .providers
                .get(provider_id)
                .ok_or_else(|| ProviderError::InvalidRequest {
                    message: format!("provider {provider_id} is not configured"),
                })?;
        let store = credential::AuthStore::load(self.inner.data_dir.join("auth.json"))?;
        credential::resolve(&slot.entry, &store, &self.inner.env)
    }

    /// Binds one resolved model to its configured provider and resolved
    /// credential.
    ///
    /// # Errors
    /// Returns `InvalidRequest` if the resolved route names no configured
    /// provider, and auth or provider-construction errors otherwise.
    pub fn provider(&self, resolved: ResolvedModel) -> Result<Provider, ProviderError> {
        if let Some(script) = self.scripted_provider() {
            return Ok(script);
        }
        let slot = self
            .inner
            .providers
            .get(&resolved.provider)
            .ok_or_else(|| ProviderError::InvalidRequest {
                message: format!("provider {} is not configured", resolved.provider),
            })?;
        let credential = self.credential(&resolved.provider)?;
        Provider::new(
            resolved,
            slot.entry.clone(),
            credential,
            slot.client.get(),
            self,
        )
    }

    /// Returns the shared replay script when `[providers.scripted]` is set.
    ///
    /// Clones share one step queue, so every provider built from the same
    /// set continues the same fixture instead of restarting it.
    #[must_use]
    pub fn scripted_provider(&self) -> Option<Provider> {
        self.inner.scripted.clone().map(Provider::Scripted)
    }

    /// Loads one source-aware catalog over every configured provider.
    ///
    /// Live rows are preferred, then each provider's cache, then typed-id
    /// fallback. Credential absence is preserved as a typed fallback rather
    /// than causing a request with invented authentication.
    ///
    /// # Errors
    /// Returns `InvalidRequest` if a provider is not initialized, and
    /// transport or credential-refresh errors otherwise.
    pub async fn catalog(&self) -> Result<crate::catalog::Catalog, ProviderError> {
        let mut sources = Vec::with_capacity(self.inner.config.providers.len());
        let mut entries = Vec::new();
        for provider in &self.inner.config.providers {
            let loaded = self.load_models(&provider.id).await?;
            let slot = self.inner.providers.get(&provider.id).ok_or_else(|| {
                ProviderError::InvalidRequest {
                    message: format!("provider {} is not initialized", provider.id),
                }
            })?;
            sources.push((slot.entry.clone(), loaded.source));
            entries.extend(loaded.entries);
        }
        Ok(crate::catalog::Catalog::with_sources(sources, entries))
    }

    /// Refreshes one provider's model list and reports how it was obtained.
    ///
    /// The outcome carries the live-request error and any cache failure, so a
    /// caller can tell a fresh list from a cached one. A provider with no
    /// credential is asked with none, which falls back to its cache.
    ///
    /// # Errors
    /// Returns `InvalidRequest` if `provider_id` is not configured, and
    /// credential-file, admission, or credential-refresh errors otherwise.
    pub async fn load_models(
        &self,
        provider_id: &str,
    ) -> Result<crate::catalog::CatalogFetch, ProviderError> {
        let slot =
            self.inner
                .providers
                .get(provider_id)
                .ok_or_else(|| ProviderError::InvalidRequest {
                    message: format!("provider {provider_id} is not initialized"),
                })?;
        let credential = match self.credential(provider_id) {
            Ok(credential) => credential,
            Err(ProviderError::NoCredentials { .. }) => Credential::None,
            Err(error) => return Err(error),
        };
        let permit = Arc::clone(&slot.permits)
            .acquire_owned()
            .await
            .map_err(|_| ProviderError::Transport {
                family: slot.entry.family,
                reason: String::from("provider model-list admission is closed"),
            })?;
        let refresh_cancel = tokio_util::sync::CancellationToken::new();
        let credential = super::transport::refresh_expiring(
            &self.inner.refresher,
            &slot.entry,
            &credential,
            &refresh_cancel,
        )
        .await?;
        let fetch = crate::catalog::ModelFetch {
            client: slot.client.get(),
            provider: &slot.entry,
            credential: &credential,
            cache_dir: &self.inner.cache_dir,
            user_agent: &self.inner.user_agent,
            version: &self.inner.identity.version,
        };
        let loaded = crate::catalog::load_models(&fetch, tokio::time::sleep).await;
        drop(permit);
        Ok(loaded)
    }

    /// Resolves a reference through a source-aware catalog and configured aliases.
    ///
    /// # Errors
    /// Returns `NoDefault` when `reference` is empty and no default model is
    /// configured, and the catalog resolution error otherwise.
    pub fn resolve(
        &self,
        catalog: &crate::catalog::Catalog,
        reference: &str,
    ) -> Result<ResolvedModel, crate::error::ResolveError> {
        let reference = if reference.is_empty() {
            self.inner
                .config
                .default_model
                .as_deref()
                .ok_or(crate::error::ResolveError::NoDefault)?
        } else {
            reference
        };
        crate::catalog::resolve(catalog, &self.inner.config.aliases, reference)
    }

    /// Clears all WebSocket and once-per-session notice state for `session`.
    pub fn end_session(&self, session: &SessionId) {
        self.inner.ws.end_session(session);
        drop(lock(&self.inner.notices).remove(session));
    }
}

impl std::fmt::Debug for ProviderSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderSet")
            .field("provider_count", &self.inner.config.providers.len())
            .field("client_count", &self.inner.clients.len())
            .finish_non_exhaustive()
    }
}

pub(super) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn origin_key(base: &str, family: dal_core::Family) -> Result<Box<str>, ProviderError> {
    let url = url::Url::parse(base).map_err(|error| ProviderError::Transport {
        family,
        reason: format!("provider base URL is invalid: {error}"),
    })?;
    let host = url.host_str().ok_or_else(|| ProviderError::Transport {
        family,
        reason: String::from("provider base URL has no host"),
    })?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| ProviderError::Transport {
            family,
            reason: String::from("provider base URL has no port"),
        })?;
    Ok(format!("{}://{host}:{port}", url.scheme()).into_boxed_str())
}
