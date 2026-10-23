//! Host-owned provider resources and their lifetime.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
};

use dal_core::{Family, SessionId};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::{
    auth::{
        credential::{self, Credential, EnvSnapshot},
        refresh::{Refresher, TokenEndpoints},
    },
    catalog::ResolvedModel,
    error::ProviderError,
    family::{
        azure,
        bedrock::auth::{AwsCredentialResolver, SHARED_RESOLVE_TIMEOUT, expired},
        copilot::IdentityCache,
        gitlab::{DirectAccessCache, workflow::WorkflowSessions},
        google::{antigravity::AntigravitySession, shared::ResponseIdSlot},
    },
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

/// One Antigravity conversation: the envelope state the join owns per dal
/// session, plus the slot the stream decoder reports response ids through
/// for the next request's labels.
#[derive(Default)]
pub(super) struct AntigravitySlot {
    pub(super) session: AntigravitySession,
    pub(super) response: ResponseIdSlot,
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
    /// The one Copilot identity negotiation cache of this set; there is no
    /// process-global instance.
    pub(super) copilot_identities: IdentityCache,
    /// The Bedrock credential chain of this set: one resolver over the
    /// captured environment, with its own proxy-bypassed HTTP client. No
    /// process-global state.
    pub(super) aws_resolver: Arc<AwsCredentialResolver>,
    /// The one GitLab `direct_access` grant cache of this set; the refresh
    /// engine holds the same instance, so sign-in rotation invalidates the
    /// grants every live turn reads. There is no second cache.
    pub(super) gitlab_grants: Arc<DirectAccessCache>,
    /// The live GitLab workflow sockets of this set, keyed by session and
    /// provider; a turn takes its row, drives it, and returns it only while
    /// a tool action is pending.
    pub(super) gitlab_workflows: WorkflowSessions,
    /// The caller-created TOML deny-all policy the Gemini CLI child runs
    /// under, written once at build through the dal-store private-file
    /// boundary. Present only when a `google-gemini-cli` provider is
    /// configured.
    pub(super) gemini_policy: Option<PathBuf>,
    /// One Antigravity conversation per dal session, seeded from the
    /// session id; cleared by [`ProviderSet::end_session`].
    pub(super) antigravity: Mutex<HashMap<SessionId, AntigravitySlot>>,
}

pub(super) struct ProviderSlot {
    pub(super) entry: ProviderEntry,
    pub(super) client: http::LazyClient,
    pub(super) permits: Arc<Semaphore>,
}

/// The client, slot, and usage tables one provider set builds together.
type ProviderSlotMaps = (
    HashMap<Box<str>, http::LazyClient>,
    HashMap<Box<str>, ProviderSlot>,
    HashMap<Box<str>, Arc<UsageChecker>>,
);
/// Whether one configured provider currently holds a credential source.
///
/// The row reports the effective configured family of the entry, never a
/// built-in default: a Compatible-preset row overridden to another `api`
/// reports the override. Readiness performs no request, refresh, model
/// fetch, or cache write: a stored or captured bearer counts before the
/// ambient AWS source probe runs, and a provider with no source reports
/// `ready: false` instead of an error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderReadiness {
    /// The configured provider id.
    pub provider: Box<str>,
    /// The effective configured family of the row.
    pub family: Family,
    /// Whether the provider holds a credential source.
    pub ready: bool,
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
    /// configuration, or names the provider whose defaulted Azure-shaped entry
    /// names an invalid `AZURE_OPENAI_RESOURCE_NAME`. The auth file is loaded
    /// only when a credential is resolved.
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

    fn build_provider_slots(
        providers_list: &[ProviderEntry],
        user_agent: &str,
    ) -> Result<ProviderSlotMaps, ProviderError> {
        let mut clients: HashMap<Box<str>, http::LazyClient> = HashMap::new();
        let mut providers = HashMap::with_capacity(providers_list.len());
        let mut usage = HashMap::new();

        for provider in providers_list {
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
                        Arc::new(UsageChecker::new(client, &provider.base_url, user_agent)?),
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
        Ok((clients, providers, usage))
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
        // One shared effective endpoint per entry, resolved before any slot,
        // client, or kept configuration is built from it: the catalog and
        // inference paths both read the prepared entries, so no second copy
        // can go stale.
        let config = with_azure_endpoints(config, &env)?;
        let (clients, providers, usage) =
            Self::build_provider_slots(&config.providers, &user_agent)?;

        // One grant cache for the whole set: the refresh engine holds the
        // same instance, so GitLab sign-in rotation invalidates the grants
        // live proxy turns read. The sibling login module reaches it only
        // through `ProviderSet::gitlab_grants`, never through these fields.
        let gitlab_grants: Arc<DirectAccessCache> = Arc::default();
        let refresher = Arc::new(
            Refresher::new(
                user_agent.clone(),
                data_dir.join("auth.json"),
                endpoints,
                &env,
            )
            .with_gitlab_grants(Arc::clone(&gitlab_grants)),
        );
        // The Bedrock chain resolves over the captured environment with its
        // own HTTP client: the crate's shared per-origin client must never
        // carry ambient proxy policy into ECS/IMDS credential HTTP. The
        // dedicated client is built by `http::build_resolver_client`
        // (parent http.rs seam): crate timeouts, same-origin redirects, and
        // no ambient proxy.
        let aws_resolver = Arc::new(AwsCredentialResolver::new(
            env.clone(),
            http::build_resolver_client(),
            AwsCredentialResolver::home_dir_from_env(&env),
        ));
        // The Gemini CLI child runs headless under a caller-created TOML
        // deny-all policy. The file is published once here through the
        // dal-store atomic private-file boundary, never with an ad-hoc
        // plaintext write; the family module only passes the path through.
        let gemini_policy = config
            .providers
            .iter()
            .any(|provider| provider.family == Family::GoogleGeminiCli)
            .then(|| ensure_gemini_cli_policy(data_dir))
            .transpose()?;
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
                config,
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
                copilot_identities: IdentityCache::default(),
                aws_resolver,
                gitlab_grants,
                gitlab_workflows: WorkflowSessions::default(),
                gemini_policy,
                antigravity: Mutex::new(HashMap::new()),
            }),
        })
    }

    /// Replaces the HTTP client for one named provider before any shared
    /// references or [`Provider`] handles are created.
    ///
    /// # Pre-use condition
    ///
    /// Must be called before creating [`Provider`] handles or cloning any shared
    /// references to this [`ProviderSet`]. The internal set structure must be
    /// uniquely owned; once shared, replacement fails.
    ///
    /// # Scope
    ///
    /// Replaces only the inference HTTP client for the named provider slot.
    /// Catalog discovery clients, credentials, caches, and transport for other
    /// providers are not changed.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::InvalidRequest`] if this [`ProviderSet`] is already
    /// shared (provider handles or cloned references exist), or if `provider_id`
    /// does not name a configured provider.
    #[cfg(feature = "test-support")]
    pub fn set_test_client(
        &mut self,
        provider_id: &str,
        client: reqwest::Client,
    ) -> Result<(), ProviderError> {
        let inner = Arc::get_mut(&mut self.inner).ok_or_else(|| ProviderError::InvalidRequest {
            message: String::from(
                "cannot configure test client on shared ProviderSet; call set_test_client before creating provider handles or sharing references",
            ),
        })?;
        let slot =
            inner
                .providers
                .get_mut(provider_id)
                .ok_or_else(|| ProviderError::InvalidRequest {
                    message: format!("provider {provider_id} is not configured"),
                })?;
        slot.client = client.into();
        Ok(())
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

    /// Reports whether `provider_id` currently holds a credential source,
    /// without sending any request.
    ///
    /// Non-Bedrock rows reuse the credential-resolution presence rule: any
    /// resolved credential (including an explicit credential-less row) is
    /// ready, and only [`ProviderError::NoCredentials`] reads as `false`.
    /// Vertex rows also count a captured pre-minted bearer token, and Droid
    /// and Antigravity rows count only a stored OAuth login, because those
    /// adapters refuse an API key.
    /// Bedrock rows count the stored `auth.json` key and the captured
    /// `AWS_BEARER_TOKEN_BEDROCK` bearer first, then consult the set-owned
    /// ambient AWS credential-source probe. Nothing refreshes, spawns a
    /// credential helper, fetches models, writes a cache, or touches the
    /// network; the profile probe's unreadable-file `false` is the observed
    /// legacy behavior it keeps.
    ///
    /// # Errors
    ///
    /// Returns `InvalidRequest` when `provider_id` names no configured
    /// provider, [`ProviderError::Cancelled`] when `cancel` fires before or
    /// during the check (never `false` for cancellation), the existing AWS
    /// timeout transport error when the Bedrock probe deadline expires, and
    /// auth-store errors otherwise.
    pub async fn credential_source_available(
        &self,
        provider_id: &str,
        cancel: &CancellationToken,
    ) -> Result<bool, ProviderError> {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let slot =
            self.inner
                .providers
                .get(provider_id)
                .ok_or_else(|| ProviderError::InvalidRequest {
                    message: format!("provider {provider_id} is not configured"),
                })?;
        let entry = &slot.entry;
        let store = self.load_auth_store(cancel, entry.family).await?;
        let deadline = tokio::time::Instant::now() + SHARED_RESOLVE_TIMEOUT;
        self.readiness_for(entry, &store, cancel, deadline).await
    }

    /// Reports one readiness row per configured provider, in stable
    /// provider-id order, loading `auth.json` once for the batch.
    ///
    /// Each row follows [`Self::credential_source_available`]; the single
    /// caller-minted deadline binds every Bedrock probe instead of a fresh
    /// envelope per row. A cancelled batch reports
    /// [`ProviderError::Cancelled`], including a cancelled empty batch.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Cancelled`] when `cancel` fires, the Bedrock
    /// timeout transport error when a probe deadline expires, and auth-store
    /// errors otherwise. No row reports `false` for cancellation.
    pub async fn provider_readiness(
        &self,
        cancel: &CancellationToken,
    ) -> Result<Vec<ProviderReadiness>, ProviderError> {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let mut slots: Vec<&ProviderEntry> = self
            .inner
            .providers
            .values()
            .map(|slot| &slot.entry)
            .collect();
        slots.sort_by(|left, right| left.id.cmp(&right.id));
        let Some(&first) = slots.first() else {
            return Ok(Vec::new());
        };
        let store = self.load_auth_store(cancel, first.family).await?;
        let deadline = tokio::time::Instant::now() + SHARED_RESOLVE_TIMEOUT;
        let mut rows = Vec::with_capacity(slots.len());
        for entry in slots {
            let ready = self.readiness_for(entry, &store, cancel, deadline).await?;
            rows.push(ProviderReadiness {
                provider: entry.id.clone(),
                family: entry.family,
                ready,
            });
        }
        Ok(rows)
    }

    /// Loads the decoded `auth.json` off the async runtime, with
    /// cancellation racing the blocking read: winning cancellation reports
    /// [`ProviderError::Cancelled`] and the late result is dropped, the way
    /// the AWS file boundary abandons a blocked read past its wait.
    ///
    /// # Errors
    ///
    /// Returns the typed auth-store error when `auth.json` cannot be read,
    /// [`ProviderError::Cancelled`] when `cancel` fires first, and a
    /// family-attributed transport error when the blocking join itself fails
    /// outside cancellation.
    async fn load_auth_store(
        &self,
        cancel: &CancellationToken,
        family: Family,
    ) -> Result<credential::AuthStore, ProviderError> {
        let path = self.inner.data_dir.join("auth.json");
        let load = tokio::task::spawn_blocking(move || credential::AuthStore::load(path));
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(ProviderError::Cancelled),
            result = load => match result {
                Ok(store) => store,
                Err(_) if cancel.is_cancelled() => Err(ProviderError::Cancelled),
                Err(_) => Err(ProviderError::Transport {
                    family,
                    reason: String::from("auth store read failed"),
                }),
            },
        }
    }

    /// The shared single-row check behind [`Self::credential_source_available`]
    /// and [`Self::provider_readiness`], so one row can never drift from the
    /// batch: the caller threads the batch deadline in, never a fresh
    /// envelope per row.
    ///
    /// Cancellation and the shared deadline bind the Bedrock probe wait, but
    /// the probe itself reports plain availability: a `false` that arrives
    /// alongside a fired `cancel` or an elapsed `deadline` is re-raised as
    /// [`ProviderError::Cancelled`] or the existing AWS timeout, never
    /// returned as unavailable.
    async fn readiness_for(
        &self,
        entry: &ProviderEntry,
        store: &credential::AuthStore,
        cancel: &CancellationToken,
        deadline: tokio::time::Instant,
    ) -> Result<bool, ProviderError> {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        if entry.family == Family::GoogleVertex {
            // A Vertex row is ready when it holds a stored/env credential of
            // the usual kind, or a pre-minted bearer token in the captured
            // environment (gcloud's `print-access-token` workflow).
            let keyed = credential::resolve(entry, store, &self.inner.env).is_ok();
            return Ok(keyed || super::request::vertex_env_token(&self.inner.env).is_some());
        }
        if accepts_only_oauth(entry.family) {
            // The request adapter signs in with a stored OAuth login and
            // refuses an API key, so a key alone is not a usable source.
            return match credential::resolve(entry, store, &self.inner.env) {
                Ok(Credential::OAuth(_)) => Ok(true),
                Ok(Credential::ApiKey { .. } | Credential::None)
                | Err(ProviderError::NoCredentials { .. }) => Ok(false),
                Err(error) => Err(error),
            };
        }
        if entry.family != Family::Bedrock {
            return match credential::resolve(entry, store, &self.inner.env) {
                Ok(_) => Ok(true),
                Err(ProviderError::NoCredentials { .. }) => Ok(false),
                Err(error) => Err(error),
            };
        }
        if self
            .inner
            .aws_resolver
            .resolve_bearer_token(entry, store)
            .is_some()
        {
            return Ok(true);
        }
        let present = self
            .inner
            .aws_resolver
            .has_credential_source(None, cancel, deadline)
            .await;
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(expired());
        }
        Ok(present)
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
        // Bedrock authenticates through the AWS chain in `aws_resolver`,
        // never through a bound credential: a missing keyed credential binds
        // credential-less instead of refusing the route. The request adapter
        // resolves bearer-or-SigV4 per attempt under one caller envelope.
        // Vertex follows the same precedent: without a key, the adapter
        // reads a pre-minted bearer token from the captured environment and
        // refuses with `NoCredentials` itself when there is none.
        let credential = match self.credential(&resolved.provider) {
            Ok(credential) => credential,
            Err(ProviderError::NoCredentials { .. })
                if authenticates_ambiently(slot.entry.family) =>
            {
                Credential::None
            }
            Err(error) => return Err(error),
        };
        Provider::new(
            resolved,
            slot.entry.clone(),
            credential,
            slot.client.get(),
            self,
        )
    }

    /// The one `direct_access` grant cache of this set.
    ///
    /// The sibling login module cannot touch `inner` (it is `pub(super)` in
    /// `provider/`), so this crate-internal getter is its only access: it
    /// clones the same instance the refresh engine invalidates on sign-in
    /// rotation, never a second cache.
    pub(crate) fn gitlab_grants(&self) -> Arc<DirectAccessCache> {
        Arc::clone(&self.inner.gitlab_grants)
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

    /// Stops OAuth credential refreshes and waits for every refresh task still
    /// running, so none keeps the auth-file lock or commits tokens after the
    /// host that owns this set is gone. Later refreshes fail with
    /// [`ProviderError::Transport`].
    pub async fn shutdown(&self) {
        self.inner.refresher.shutdown().await;
    }

    /// Clears all WebSocket, GitLab workflow, once-per-session notice, and
    /// Antigravity state for `session`. A parked GitLab workflow row is
    /// dropped with its socket; the family contract sends no workflow stop
    /// from this synchronous path (see [`WorkflowSessions::end_session`]).
    pub fn end_session(&self, session: &SessionId) {
        self.inner.ws.end_session(session);
        self.inner.gitlab_workflows.end_session(session);
        drop(lock(&self.inner.notices).remove(session));
        drop(lock(&self.inner.antigravity).remove(session));
    }
}

/// Resolves the effective endpoint of every Azure-shaped entry whose table
/// named no `base_url`: the captured `AZURE_OPENAI_BASE_URL`, else the
/// canonical endpoint of the validated `AZURE_OPENAI_RESOURCE_NAME`, else the
/// row default the entry already carries. An explicit table URL always wins,
/// even when it equals the row default, so an invalid resource name the entry
/// never needs is never validated and can neither override the URL nor fail
/// the set. A blank variable names nothing. Entries without the Azure shape
/// are untouched, whatever the Azure variables hold.
///
/// The configuration stays pure: the environment here is the host's captured
/// snapshot, never ambient process state.
///
/// # Errors
///
/// [`ProviderError::InvalidRequest`] naming the provider when a defaulted
/// Azure-shaped entry names a resource that is not one DNS label.
fn with_azure_endpoints(
    config: &ProviderConfig,
    env: &EnvSnapshot,
) -> Result<ProviderConfig, ProviderError> {
    let mut prepared = config.clone();
    for entry in &mut prepared.providers {
        if !entry.settings.base_url_defaulted || !super::request::is_azure_entry(entry) {
            continue;
        }
        if let Some(base) = azure::base_url_from_env(&entry.id, env)? {
            entry.base_url = base.into();
        }
    }
    Ok(prepared)
}

impl std::fmt::Debug for ProviderSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderSet")
            .field("provider_count", &self.inner.config.providers.len())
            .field("client_count", &self.inner.clients.len())
            .finish_non_exhaustive()
    }
}

/// The caller-created TOML deny-all policy the Gemini CLI child runs under.
///
/// The exact CLI grammar witnessed by the family's `CliRequest` contract,
/// not an invented JSON shape and not a product policy: every tool denied,
/// reads included, with the read-only approval mode on the command line.
pub(crate) const GEMINI_CLI_DENY_ALL_POLICY: &str =
    "[[rule]]\ntoolName = \"*\"\ndecision = \"deny\"\npriority = 100\n";

/// The policy file name under the host data directory.
const GEMINI_CLI_POLICY_FILE: &str = "gemini-cli-policy.toml";

/// Publishes the Gemini CLI deny-all policy through the dal-store atomic
/// private-file boundary: the directory is created private and the file is
/// written atomically at mode `0600`, never with an ad-hoc plaintext write.
///
/// # Errors
///
/// Returns `Transport` when the directory or the atomic write fails; the
/// child never spawns without its policy file.
fn ensure_gemini_cli_policy(data_dir: &Path) -> Result<PathBuf, ProviderError> {
    dal_store::create_private_dir_all(data_dir).map_err(|error| ProviderError::Transport {
        family: Family::GoogleGeminiCli,
        reason: format!("creating the Gemini CLI policy directory failed: {error}"),
    })?;
    let path = data_dir.join(GEMINI_CLI_POLICY_FILE);
    dal_store::write_atomic(
        &path,
        GEMINI_CLI_DENY_ALL_POLICY.as_bytes(),
        dal_store::FileMode::Mode0600,
    )
    .map_err(|error| ProviderError::Transport {
        family: Family::GoogleGeminiCli,
        reason: format!("writing the Gemini CLI deny-all policy failed: {error}"),
    })?;
    Ok(path)
}

/// Whether a family authenticates outside the bound credential: Bedrock
/// through the AWS chain, Vertex through a captured bearer token. Such a
/// route binds credential-less when nothing is stored, and each attempt
/// refuses with `NoCredentials` itself when its own source is empty. The set
/// and [`Provider::new`] share this one rule.
pub(super) const fn authenticates_ambiently(family: Family) -> bool {
    matches!(family, Family::Bedrock | Family::GoogleVertex)
}

/// Whether a family's adapter accepts only a stored OAuth login and refuses
/// an API key: Droid (`droid::require_bearer`) and Antigravity.
pub(super) const fn accepts_only_oauth(family: Family) -> bool {
    matches!(family, Family::Droid | Family::GoogleAntigravity)
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
