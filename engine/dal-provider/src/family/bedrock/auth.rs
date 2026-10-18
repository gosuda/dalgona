//! Bedrock dual-mode auth: a bearer-token bypass ahead of the `SigV4`
//! five-tier credential chain.
//!
//! Precedence: a stored provider API key or `AWS_BEARER_TOKEN_BEDROCK`
//! emits `Authorization: Bearer <token>` and skips `SigV4` entirely (a
//! request-level bearer option, when the caller holds one, wins before
//! this); otherwise credentials resolve through static environment keys,
//! web identity / OIDC, the shared profile files (static, `source_profile`
//! role chaining, SSO, `credential_process`), ECS container credentials,
//! and EC2 `IMDSv2`.
//!
//! The resolver reads no ambient state. It resolves from the captured
//! [`EnvSnapshot`], sends through the captured [`reqwest::Client`], and runs
//! helpers in the edge-captured working directory. Cancellation and the
//! single absolute resolution deadline thread through every nested call.
//! File reads run on the blocking pool: abandoning the wait on cancel or
//! deadline does not stop a blocked read, which runs to completion.

pub(crate) mod metadata;
pub(crate) mod profile;
pub(crate) mod sso;

#[cfg(test)]
mod tests;

use std::{
    collections::HashMap,
    future::Future,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use dal_core::Family;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::AwsCredential;
use crate::{AuthStore, EnvSnapshot, ProviderEntry, ProviderError, SecretString};

/// Seconds of clock skew absorbed before a cached expiry: a credential that
/// dies within this window is re-resolved instead of served.
pub(crate) const REFRESH_SKEW_SECS: i64 = 60;

/// Cache cap for credentials that carry a session token but no expiry:
/// session keys in a shared file and the output of a `credential_process`
/// that omits `Expiration`. Tools like `aws-vault` rotate such keys in
/// place; caching them forever would serve stale keys after rotation. The
/// cap is also the expiry the signer sees, so a key that names no lifetime
/// still signs while it is cached.
pub(crate) const FILE_SESSION_CREDS_TTL_SECS: i64 = 5 * 60;

/// The single shared resolution envelope: 30 seconds bounding one
/// credential resolution. The caller owns the absolute deadline: it passes
/// `Instant::now() + SHARED_RESOLVE_TIMEOUT` into the resolve entry points,
/// and every nested HTTP, SSO, IMDS, and subprocess call races that same
/// instant. No other timeout constant exists on this path; the IMDS
/// per-probe bound below caps each metadata probe inside the envelope, it
/// is not a second envelope.
pub(crate) const SHARED_RESOLVE_TIMEOUT: Duration = Duration::from_secs(30);

/// Per-request bound on one `IMDSv2` probe request. Each of the token, role,
/// and credential fetches carries its own one-second timer inside the
/// shared envelope.
pub(crate) const IMDS_PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// Proactive SSO access-token refresh skew: a cached token this close to
/// `expiresAt` refreshes before the federation call so a request in flight
/// cannot age out.
pub(crate) const SSO_TOKEN_REFRESH_SKEW_SECS: i64 = 60;

/// Reads one file fully, with no size or type gate: no pre-open check can
/// make a read of an env-controlled path cancellable or nonblocking (a
/// FIFO blocks in open itself). The async tiers run this on the blocking
/// pool under cancel/deadline select (see [`read_shared_bytes_async`]); a
/// missing path or a failed read returns `None` and the caller applies its
/// tier rule. Directories fail the read itself and read as absent.
pub(crate) fn read_shared_bytes(path: &Path) -> Option<Vec<u8>> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut out = Vec::new();
    {
        use std::io::Read as _;
        file.read_to_end(&mut out).ok()?;
    }
    Some(out)
}

/// The async boundary for file reads on the resolution path: the blocking
/// read runs on the blocking pool while cancellation and the shared
/// deadline bind the wait. Abandoning the wait does NOT stop a blocked
/// read; the pool thread runs to completion and its result is dropped.
/// Every read site states this; nothing here claims otherwise.
pub(crate) async fn read_shared_bytes_async(
    path: PathBuf,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<Option<Vec<u8>>, ProviderError> {
    let outcome = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(cancelled()),
        () = tokio::time::sleep_until(deadline) => return Err(expired()),
        result = tokio::task::spawn_blocking(move || read_shared_bytes(&path)) => result,
    };
    outcome.map_err(|_| ProviderError::Transport {
        family: Family::Bedrock,
        reason: String::from("an AWS shared file read failed"),
    })
}

/// The `AWS_BEARER_TOKEN_BEDROCK` environment variable, the third bearer
/// source after an explicit option and the stored provider key.
const BEARER_TOKEN_ENV: &str = "AWS_BEARER_TOKEN_BEDROCK";

/// The fallback region when neither an explicit region, the environment, nor
/// the active shared-config profile names one.
const DEFAULT_REGION: &str = "us-east-1";

/// The credential a Bedrock request carries: either a bearer token that
/// bypasses `SigV4`, or a `SigV4` credential from the five-tier chain.
#[derive(Clone, Debug)]
pub(crate) enum BedrockAuth {
    /// `Authorization: Bearer <token>`; `SigV4` signing is skipped entirely.
    Bearer(SecretString),
    /// `SigV4`-signed with this credential.
    SigV4(AwsCredential),
}

/// The per-request inputs of `SigV4` credential resolution. The region is the
/// already-resolved region (see [`AwsCredentialResolver::resolve_region`]);
/// the profile is the explicit override, defaulting through `AWS_PROFILE` to
/// `default` inside resolution.
#[derive(Clone, Debug)]
pub(crate) struct AwsResolveOptions {
    /// Named profile override; `None` selects via `AWS_PROFILE`/`default`.
    pub(crate) profile: Option<String>,
    /// Resolved region; part of the cache key.
    pub(crate) region: String,
}

/// The shared inputs of the credential tiers: captured environment, shared
/// client, and captured home, all borrowed from the resolver.
pub(crate) struct ResolveCtx<'a> {
    pub(crate) env: &'a EnvSnapshot,
    pub(crate) client: &'a reqwest::Client,
    pub(crate) home_dir: Option<&'a Path>,
}

/// Drives one tier step under structured cancellation: caller cancellation
/// wins first, then the single shared resolution deadline, then the work.
/// Dropping the losing branch drops its future and aborts its in-flight
/// request; a blocked file read on the pool runs to completion past the
/// wait.
pub(crate) async fn guard_step<T>(
    cancel: &CancellationToken,
    deadline: Instant,
    work: impl Future<Output = Result<T, ProviderError>>,
) -> Result<T, ProviderError> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(cancelled()),
        () = tokio::time::sleep_until(deadline) => Err(expired()),
        result = work => result,
    }
}
/// Cache key: one entry per profile, region, and shared-config mode.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct AwsCacheKey {
    profile: String,
    region: String,
    load_shared_config: bool,
}

/// One cached resolution: the credential plus its absolute expiry in unix
/// seconds, or `None` for a non-expiring static credential.
#[derive(Clone, Debug)]
struct AwsCacheEntry {
    cred: AwsCredential,
    expires_at: Option<i64>,
}

/// The per-provider-set Bedrock credential resolver.
///
/// Constructed once per provider set from the captured environment, one
/// shared HTTP client, and the captured home directory. The cache is
/// process-wide per set: no global singleton exists. Concurrent cold
/// resolutions for one key serialize on a per-key slot and re-check the
/// cache, so one key spawns one fetch storm, never N.
#[derive(Debug)]
pub(crate) struct AwsCredentialResolver {
    env: EnvSnapshot,
    client: reqwest::Client,
    home_dir: Option<PathBuf>,
    cache: Mutex<HashMap<AwsCacheKey, AwsCacheEntry>>,
    slots: Mutex<HashMap<AwsCacheKey, Arc<tokio::sync::Mutex<()>>>>,
}

impl AwsCredentialResolver {
    /// Builds a resolver over explicitly captured inputs. `home_dir` is the
    /// captured home directory (see [`Self::home_dir_from_env`]); `None`
    /// disables `~/.aws` discovery while explicit file overrides still work.
    #[must_use]
    pub(crate) fn new(
        env: EnvSnapshot,
        client: reqwest::Client,
        home_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            env,
            client,
            home_dir,
            cache: Mutex::new(HashMap::new()),
            slots: Mutex::new(HashMap::new()),
        }
    }

    /// Derives the captured home directory from the snapshot alone, never
    /// from ambient state: `HOME` on Unix, `USERPROFILE` (else
    /// `HOMEDRIVE`+`HOMEPATH`) on Windows. The composition edge calls this on
    /// the snapshot it already captured and passes the result to [`Self::new`].
    #[must_use]
    pub(crate) fn home_dir_from_env(env: &EnvSnapshot) -> Option<PathBuf> {
        #[cfg(windows)]
        {
            if let Some(profile) = env.get("USERPROFILE").filter(|v| !v.is_empty()) {
                return Some(PathBuf::from(profile));
            }
            match (
                env.get("HOMEDRIVE").filter(|v| !v.is_empty()),
                env.get("HOMEPATH").filter(|v| !v.is_empty()),
            ) {
                (Some(drive), Some(path)) => Some(PathBuf::from(format!("{drive}{path}"))),
                _ => None,
            }
        }
        #[cfg(not(windows))]
        {
            env.get("HOME")
                .filter(|home| !home.is_empty())
                .map(PathBuf::from)
        }
    }

    /// The selected shared-credentials profile: the explicit override, else
    /// `AWS_PROFILE`, else `default`. An empty override counts as unset.
    #[must_use]
    pub(crate) fn resolve_profile_name(explicit: Option<&str>, env: &EnvSnapshot) -> String {
        explicit
            .filter(|name| !name.is_empty())
            .or_else(|| env.get("AWS_PROFILE").filter(|name| !name.is_empty()))
            .unwrap_or("default")
            .to_owned()
    }

    /// Whether the shared config file participates in profile and region
    /// resolution. An explicit profile (argument or `AWS_PROFILE`) enables
    /// it; the implicit default profile follows the `AWS_SDK_LOAD_CONFIG`
    /// opt-in (`1`/`true`, case-insensitive).
    #[must_use]
    pub(crate) fn should_load_shared_config(explicit: Option<&str>, env: &EnvSnapshot) -> bool {
        if explicit.is_some_and(|name| !name.is_empty())
            || env.get("AWS_PROFILE").is_some_and(|name| !name.is_empty())
        {
            return true;
        }
        matches!(
            env.get("AWS_SDK_LOAD_CONFIG").map(str::to_lowercase),
            Some(value) if value == "1" || value == "true"
        )
    }

    /// The region precedence shared by transports and credential exchanges:
    /// explicit region, `AWS_REGION`, `AWS_DEFAULT_REGION`, the region of the
    /// active shared-config profile, else `us-east-1`. Empty values count as
    /// unset. A profile region is read only when
    /// [`Self::should_load_shared_config`] admits the config file; an
    /// unreadable or unparseable config degrades to the next tier. Async so
    /// the config read rides the
    /// file boundary instead of stalling the runtime; the caller's
    /// cancellation and deadline bind the read, never a minted envelope.
    #[must_use]
    pub(crate) async fn resolve_region(
        &self,
        explicit: Option<&str>,
        profile: Option<&str>,
        cancel: &CancellationToken,
        deadline: Instant,
    ) -> String {
        if let Some(region) = explicit.filter(|region| !region.trim().is_empty()) {
            return region.to_owned();
        }
        for name in ["AWS_REGION", "AWS_DEFAULT_REGION"] {
            if let Some(region) = self
                .env
                .get(name)
                .filter(|region| !region.trim().is_empty())
            {
                return region.to_owned();
            }
        }
        if let Some(region) = self.profile_region(profile, cancel, deadline).await {
            return region;
        }
        String::from(DEFAULT_REGION)
    }

    /// The `region` key of the active shared-config profile, when the config
    /// file participates. Any file or parse failure yields `None`.
    async fn profile_region(
        &self,
        profile: Option<&str>,
        cancel: &CancellationToken,
        deadline: Instant,
    ) -> Option<String> {
        if !Self::should_load_shared_config(profile, &self.env) {
            return None;
        }
        let text = self
            .read_shared_file(
                profile::config_path(&self.env, self.home_dir.as_deref()),
                cancel,
                deadline,
            )
            .await?;
        let set = profile::parse_set(&text, "").ok()?;
        let name = Self::resolve_profile_name(profile, &self.env);
        let region = set.profile(&name)?.get("region")?;
        (!region.trim().is_empty()).then(|| region.to_owned())
    }

    /// Whether this set can authenticate Bedrock at all: bearer material, a
    /// static or web-identity environment source, container credentials, a
    /// live IMDS source, or a profile that terminates in a usable credential
    /// source. The profile probe mirrors the resolver dispatch (static keys,
    /// SSO, `credential_process`, `role_arn` chaining; MFA-gated roles read
    /// unusable) so availability never diverges from resolution. Async so
    /// the profile-file reads ride the file boundary instead of stalling
    /// the runtime; the caller's cancellation and deadline bind the probe.
    #[must_use]
    pub(crate) async fn has_credential_source(
        &self,
        profile: Option<&str>,
        cancel: &CancellationToken,
        deadline: Instant,
    ) -> bool {
        if self
            .env
            .get("AWS_ACCESS_KEY_ID")
            .is_some_and(|key| !key.is_empty())
            && self
                .env
                .get("AWS_SECRET_ACCESS_KEY")
                .is_some_and(|key| !key.is_empty())
        {
            return true;
        }
        if self
            .env
            .get(BEARER_TOKEN_ENV)
            .is_some_and(|token| !token.is_empty())
        {
            return true;
        }
        if self
            .env
            .get("AWS_WEB_IDENTITY_TOKEN_FILE")
            .is_some_and(|file| !file.is_empty())
            && self
                .env
                .get("AWS_ROLE_ARN")
                .is_some_and(|arn| !arn.is_empty())
        {
            return true;
        }
        if self
            .env
            .get("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI")
            .is_some_and(|uri| !uri.is_empty())
            || self
                .env
                .get("AWS_CONTAINER_CREDENTIALS_FULL_URI")
                .is_some_and(|uri| !uri.is_empty())
        {
            return true;
        }
        if metadata::imds_source_present(&self.env) {
            return true;
        }
        self.profile_has_credential_source(profile, cancel, deadline)
            .await
    }

    /// The profile half of [`Self::has_credential_source`]: whether the
    /// selected profile terminates in static keys, SSO, `credential_process`,
    /// or a `role_arn` chain. Unreadable files and unparseable shapes read
    /// as unavailable.
    async fn profile_has_credential_source(
        &self,
        profile: Option<&str>,
        cancel: &CancellationToken,
        deadline: Instant,
    ) -> bool {
        let name = Self::resolve_profile_name(profile, &self.env);
        let load_shared_config = Self::should_load_shared_config(profile, &self.env);
        let credentials = self
            .read_shared_file(
                profile::credentials_path(&self.env, self.home_dir.as_deref()),
                cancel,
                deadline,
            )
            .await
            .unwrap_or_default();
        let config = if load_shared_config {
            self.read_shared_file(
                profile::config_path(&self.env, self.home_dir.as_deref()),
                cancel,
                deadline,
            )
            .await
            .unwrap_or_default()
        } else {
            String::new()
        };
        let Ok(set) = profile::parse_set(&config, &credentials) else {
            return false;
        };
        let mut seen = Vec::new();
        profile::profile_has_source(&set, &name, &self.env, &mut seen)
    }

    /// The bearer token for `provider`, when one exists: the stored
    /// `auth.json` API key, else `AWS_BEARER_TOKEN_BEDROCK`. An empty value
    /// counts as unset. The request-level bearer option stays caller-side:
    /// no secret config field exists, and this resolver adds none.
    #[must_use]
    pub(crate) fn resolve_bearer_token(
        &self,
        provider: &ProviderEntry,
        store: &AuthStore,
    ) -> Option<SecretString> {
        if let Some(crate::Credential::ApiKey { key }) = store.credential(&provider.id)
            && !key.expose().is_empty()
        {
            return Some(key);
        }
        self.env
            .get(BEARER_TOKEN_ENV)
            .filter(|token| !token.is_empty())
            .map(SecretString::from)
    }

    /// Resolves the auth a Bedrock request carries. A bearer token wins and
    /// bypasses `SigV4`; otherwise the five-tier chain resolves `SigV4`
    /// credentials, or [`ProviderError::NoCredentials`] names the provider.
    ///
    /// # Errors
    ///
    /// [`ProviderError::NoCredentials`] when no bearer source and no chain
    /// tier yields credentials; cancellation, deadline, transport, and typed
    /// profile errors from the chain below.
    pub(crate) async fn resolve_bedrock_auth(
        &self,
        provider: &ProviderEntry,
        store: &AuthStore,
        options: &AwsResolveOptions,
        cancel: &CancellationToken,
        deadline: Instant,
    ) -> Result<BedrockAuth, ProviderError> {
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        if let Some(token) = self.resolve_bearer_token(provider, store) {
            return Ok(BedrockAuth::Bearer(token));
        }
        let credential =
            Box::pin(self.resolve_credentials(&provider.id, options, cancel, deadline)).await?;
        Ok(BedrockAuth::SigV4(credential))
    }

    /// Resolves `SigV4` credentials through the five-tier chain with
    /// process-wide per-set caching: a fresh entry serves until 60 s before
    /// its expiry. Concurrent cold resolutions for one key serialize on a
    /// per-key slot and re-check the cache, so one key performs one fetch.
    /// Failures are never cached.
    ///
    /// # Errors
    ///
    /// [`ProviderError::NoCredentials`] when no tier yields credentials;
    /// cancellation, deadline, transport, and typed profile errors.
    pub(crate) async fn resolve_credentials(
        &self,
        provider_id: &str,
        options: &AwsResolveOptions,
        cancel: &CancellationToken,
        deadline: Instant,
    ) -> Result<AwsCredential, ProviderError> {
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        let key = AwsCacheKey {
            profile: Self::resolve_profile_name(options.profile.as_deref(), &self.env),
            region: options.region.clone(),
            load_shared_config: Self::should_load_shared_config(
                options.profile.as_deref(),
                &self.env,
            ),
        };
        let now = now_unix();
        if let Some(hit) = self
            .cache
            .lock()
            .map_or(None, |cache| cache.get(&key).cloned())
            && hit
                .expires_at
                .is_none_or(|expires| expires.saturating_sub(REFRESH_SKEW_SECS) > now)
        {
            return Ok(hit.cred);
        }
        // A waiter queued behind another resolution of the same key obeys
        // its own cancellation and deadline, not the holder's: the wait is
        // a tier step like any other.
        let slot = self.slot_for(&key);
        let _guard = guard_step(cancel, deadline, async { Ok(slot.lock().await) }).await?;
        let now = now_unix();
        if let Some(hit) = self
            .cache
            .lock()
            .map_or(None, |cache| cache.get(&key).cloned())
            && hit
                .expires_at
                .is_none_or(|expires| expires.saturating_sub(REFRESH_SKEW_SECS) > now)
        {
            return Ok(hit.cred);
        }
        let credential = Box::pin(self.resolve_fresh(provider_id, &key, cancel, deadline)).await?;
        if let Ok(cache) = self.cache.lock().as_deref_mut() {
            cache.insert(
                key.clone(),
                AwsCacheEntry {
                    expires_at: credential.expires_at,
                    cred: credential.clone(),
                },
            );
        }
        self.slots.lock().map(|mut slots| slots.remove(&key)).ok();
        Ok(credential)
    }

    /// Drops the cache entry for one profile and region, so the next
    /// resolution re-resolves. The Bedrock provider calls this on HTTP
    /// 401/403 so rotated credentials are picked up instead of served stale.
    pub(crate) fn invalidate(&self, profile: Option<&str>, region: &str) {
        let key = AwsCacheKey {
            profile: Self::resolve_profile_name(profile, &self.env),
            region: region.to_owned(),
            load_shared_config: Self::should_load_shared_config(profile, &self.env),
        };
        self.cache.lock().map(|mut cache| cache.remove(&key)).ok();
    }

    /// One uncached pass through the five tiers, first hit wins.
    async fn resolve_fresh(
        &self,
        provider_id: &str,
        key: &AwsCacheKey,
        cancel: &CancellationToken,
        deadline: Instant,
    ) -> Result<AwsCredential, ProviderError> {
        // 1. Static environment keys.
        if let Some(credential) = profile::read_env_credentials(&self.env) {
            return Ok(credential);
        }
        let ctx = ResolveCtx {
            env: &self.env,
            client: &self.client,
            home_dir: self.home_dir.as_deref(),
        };
        // 2. Web identity / OIDC federation.
        if let Some(credential) =
            profile::resolve_web_identity(&ctx, &key.region, cancel, deadline).await?
        {
            return Ok(credential);
        }
        // 3. Shared profile files.
        if let Some(credential) = Box::pin(profile::resolve_profile_credentials(
            &ctx,
            &key.profile,
            key.load_shared_config,
            &key.region,
            cancel,
            deadline,
        ))
        .await?
        {
            return Ok(credential);
        }
        // 4. ECS / container credentials.
        if let Some(credential) =
            metadata::resolve_container_credentials(&ctx, cancel, deadline).await?
        {
            return Ok(credential);
        }
        // 5. EC2 IMDSv2, gated on an explicit endpoint or an EC2 host probe.
        if let Some(credential) = metadata::resolve_imds_credentials(&ctx, cancel, deadline).await?
        {
            return Ok(credential);
        }
        Err(ProviderError::NoCredentials {
            provider: provider_id.to_owned(),
        })
    }

    /// The single-flight slot for `key`, created on first use.
    fn slot_for(&self, key: &AwsCacheKey) -> Arc<tokio::sync::Mutex<()>> {
        self.slots
            .lock()
            .map(|mut slots| slots.entry(key.clone()).or_default().clone())
            .unwrap_or_default()
    }

    /// Reads one shared file over the async boundary; a missing path or a
    /// failed read degrades to empty so a half-present setup falls through
    /// to the next tier.
    async fn read_shared_file(
        &self,
        path: Option<PathBuf>,
        cancel: &CancellationToken,
        deadline: Instant,
    ) -> Option<String> {
        let path = path?;
        read_shared_bytes_async(path, cancel, deadline)
            .await
            .ok()
            .flatten()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// The shared cancellation refusal: caller cancellation raises a retryable
/// transport error, never a credential-shaped one.
pub(crate) fn cancelled() -> ProviderError {
    ProviderError::Transport {
        family: Family::Bedrock,
        reason: String::from("AWS credential resolution cancelled"),
    }
}

/// The shared deadline refusal: the single resolution envelope elapsed.
pub(crate) fn expired() -> ProviderError {
    ProviderError::Transport {
        family: Family::Bedrock,
        reason: String::from("AWS credential resolution timed out after 30 seconds"),
    }
}

/// Current unix time in seconds, the clock every expiry compares against.
pub(crate) fn now_unix() -> i64 {
    jiff::Timestamp::now().as_second()
}

/// Parses an AWS `Expiration` instant (RFC 3339, as STS, ECS, IMDS, and the
/// SSO federation call emit it) into unix seconds. Anything else yields
/// `None` and the caller applies its tier's missing-expiry rule.
pub(crate) fn parse_expiration_seconds(text: &str) -> Option<i64> {
    text.parse::<jiff::Timestamp>()
        .ok()
        .map(jiff::Timestamp::as_second)
}

/// Validates a region-shaped label before it is interpolated into a service
/// hostname. AWS region names are lowercase ASCII letters, digits, and
/// hyphens; anything else is refused before URL construction.
pub(crate) fn validate_region_label(value: &str, what: &'static str) -> Result<(), ProviderError> {
    let valid = !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if valid {
        Ok(())
    } else {
        Err(ProviderError::InvalidRequest {
            message: format!("the AWS {what} region is invalid"),
        })
    }
}
