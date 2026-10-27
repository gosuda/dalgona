//! Captured-environment Google Application Default Credentials for Vertex AI.
//!
//! The resolver intentionally owns only ephemeral access tokens. ADC JSON and
//! private keys are read at the named path, exchanged, and dropped; no vendor
//! credential fields are added to `auth.json`.
//!
//! Source order for one bearer: an explicit captured env token wins outright
//! and never touches files, metadata, or the cache; else the explicit
//! `GOOGLE_APPLICATION_CREDENTIALS` file (a hard choice: missing, unreadable,
//! or malformed is a typed failure, never a fallback); else the user ADC file
//! derived from the captured `HOME`/`APPDATA`; else the ambient metadata
//! server over the one pinned plain-HTTP origin. The cache keeps at most one
//! token and refreshes it a skew window before expiry.

use std::{
    io::{ErrorKind, Read as _},
    path::{Path, PathBuf},
    sync::{Mutex, PoisonError},
    time::Duration,
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

use dal_core::Family;

use crate::{
    auth::credential::EnvSnapshot,
    error::ProviderError,
    http::{self, Exchange},
};

const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const METADATA_URL: &str =
    "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token";
const METADATA_HEADER: &str = "Metadata-Flavor";
const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
const JWT_GRANT: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";
const REFRESH_SKEW_SECS: i64 = 60;
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(30);
const METADATA_TIMEOUT: Duration = Duration::from_secs(2);
const ADC_AGENT: &str = "dalgon/adc";

/// One bearer credential file: the two exchangeable ADC forms, everything
/// else a typed unsupported failure. No `Debug`: the variants carry secrets.
#[derive(Deserialize)]
#[serde(tag = "type")]
enum AdcFile {
    #[serde(rename = "authorized_user")]
    AuthorizedUser {
        client_id: String,
        client_secret: String,
        refresh_token: String,
    },
    #[serde(rename = "service_account")]
    ServiceAccount {
        client_email: String,
        private_key: String,
        private_key_id: Option<String>,
    },
    #[serde(other)]
    Unsupported,
}

/// One token endpoint or metadata reply.
#[derive(Deserialize)]
struct TokenReply {
    access_token: String,
    expires_in: i64,
}

/// One cached ADC token: opaque bearer plus absolute expiry, nothing else.
#[derive(Clone)]
struct Cached {
    token: Box<str>,
    expires_at: i64,
}

/// Which local source the captured environment selects for this resolver.
/// The explicit GAC variable is authoritative; the resolver is built once per
/// set from the captured snapshot, so the selection never changes.
enum Selected {
    /// `GOOGLE_APPLICATION_CREDENTIALS`: hard source, typed failures only.
    Gac(PathBuf),
    /// The user ADC file when it may exist; absence falls through to
    /// metadata, not to an error.
    User(PathBuf),
    /// No file: the ambient metadata server, over the one pinned origin.
    Metadata,
}

/// One provider-set-owned ADC resolver. The cache is deliberately not global.
pub(crate) struct Resolver {
    env: EnvSnapshot,
    user_path: Option<PathBuf>,
    client: reqwest::Client,
    cache: Mutex<Option<Cached>>,
    inflight: AsyncMutex<()>,
    #[cfg(any(test, feature = "test-support"))]
    endpoints: Endpoints,
}

/// The test-only endpoint bundle; production always sends the pinned
/// constants and this type does not exist without the test seam.
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone)]
struct Endpoints {
    token: String,
    metadata: String,
}

impl Resolver {
    pub(crate) fn new(env: EnvSnapshot, client: reqwest::Client) -> Self {
        let user_path = user_adc_path(&env);
        Self {
            env,
            user_path,
            client,
            cache: Mutex::new(None),
            inflight: AsyncMutex::new(()),
            #[cfg(any(test, feature = "test-support"))]
            endpoints: Endpoints {
                token: TOKEN_URL.to_owned(),
                metadata: METADATA_URL.to_owned(),
            },
        }
    }

    /// Points the OAuth and metadata requests at loopback fakes. Test seam
    /// only: production code cannot name this method.
    #[cfg(test)]
    pub(crate) fn with_endpoints(mut self, token: String, metadata: String) -> Self {
        self.endpoints = Endpoints { token, metadata };
        self
    }

    /// Resolves one Vertex bearer token.
    ///
    /// An explicit captured env token returns directly and bypasses files,
    /// metadata, and the cache. Otherwise the cache serves a token that is
    /// still outside the refresh skew; a single in-flight acquisition covers
    /// concurrent callers, each racing its own cancellation against the
    /// shared future. A caller cancelled while waiting on the lock exits
    /// promptly and never cancels another caller's acquisition; the released
    /// lock lets a later caller resolve independently.
    ///
    /// # Errors
    ///
    /// [`ProviderError::Cancelled`] when the caller's token fires, typed file
    /// errors for a selected but broken ADC file, [`ProviderError::TokenExchange`]
    /// for a rejected exchange, [`ProviderError::NoCredentials`] when only an
    /// unreachable metadata source remains, and transport errors for timeouts
    /// and malformed replies.
    pub(crate) async fn resolve(
        &self,
        cancel: &CancellationToken,
        provider: &str,
    ) -> Result<Box<str>, ProviderError> {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        if let Some(token) = self.explicit_token() {
            return Ok(token.into());
        }
        let deadline = tokio::time::Instant::now() + RESOLVE_TIMEOUT;
        if let Some(token) = fresh_cached(&self.cache) {
            return Ok(token);
        }
        let _inflight = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ProviderError::Cancelled),
            guard = self.inflight.lock() => guard,
        };
        if let Some(token) = fresh_cached(&self.cache) {
            return Ok(token);
        }
        let minted = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ProviderError::Cancelled),
            minted = self.acquire(cancel, deadline, provider) => minted?,
        };
        if let Ok(mut cache) = self.cache.lock() {
            *cache = Some(minted.clone());
        }
        Ok(minted.token)
    }

    /// The side-effect-free readiness probe for the ADC source: the same
    /// usable-local-source rule the stored OAuth and AWS branches apply.
    ///
    /// Ready means a usable local source exists: a strictly parsed and
    /// field-validated ADC file (`authorized_user` fields, or a
    /// `service_account` whose PEM parses as an RSA key) or an already
    /// acquired unexpired token. Mere path presence is never ready: the
    /// probe loads and validates through the exact private-file read the
    /// exchange uses, so a selected file that is missing, unreadable,
    /// malformed, or unsupported reports its typed file error instead of a
    /// verdict. No probe ever touches metadata, the network, or the cache:
    /// nothing is minted, inserted, or faked.
    ///
    /// # Errors
    ///
    /// [`ProviderError::Cancelled`] when the caller's token fires, and the
    /// typed file error of a selected but unreadable or malformed ADC file.
    pub(crate) async fn readiness(
        &self,
        cancel: &CancellationToken,
    ) -> Result<bool, ProviderError> {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        if fresh_cached(&self.cache).is_some() {
            return Ok(true);
        }
        match self.selected_source()? {
            Selected::Gac(path) => match self.load_file(&path, cancel).await? {
                // A validated file is a usable source right now.
                Some(_) => Ok(true),
                // The explicit GAC variable is a hard source choice.
                None => Err(missing_file(&path)),
            },
            // Absence means metadata would be the source; readiness never
            // probes ambient metadata, so that source stays unknown.
            Selected::User(path) => Ok(self.load_file(&path, cancel).await?.is_some()),
            Selected::Metadata => Ok(false),
        }
    }

    /// The first nonblank explicit bearer in the captured snapshot, if any.
    fn explicit_token(&self) -> Option<&str> {
        ["GOOGLE_CLOUD_ACCESS_TOKEN", "CLOUDSDK_AUTH_ACCESS_TOKEN"]
            .iter()
            .find_map(|name| self.env.get(name).filter(|value| nonblank(value)))
    }

    /// The one source the captured environment selects, resolving a relative
    /// GAC value against the captured cwd only.
    fn selected_source(&self) -> Result<Selected, ProviderError> {
        if let Some(value) = self
            .env
            .get("GOOGLE_APPLICATION_CREDENTIALS")
            .filter(|value| nonblank(value))
        {
            return Ok(Selected::Gac(resolve_named_path(value, self.env.cwd())?));
        }
        Ok(match self.user_path.as_deref() {
            Some(path) => Selected::User(path.to_path_buf()),
            None => Selected::Metadata,
        })
    }

    /// One acquisition: read the selected source, exchange, and return the
    /// cacheable token with its absolute expiry. Errors are never cached.
    async fn acquire(
        &self,
        cancel: &CancellationToken,
        deadline: tokio::time::Instant,
        provider: &str,
    ) -> Result<Cached, ProviderError> {
        match self.selected_source()? {
            Selected::Gac(path) => {
                let adc = self.load_file(&path, cancel).await?;
                let Some(adc) = adc else {
                    return Err(missing_file(&path));
                };
                let reply = self.exchange(adc, &path, cancel, deadline).await?;
                cached_file_token(&path, reply)
            }
            Selected::User(path) => {
                let adc = self.load_file(&path, cancel).await?;
                if let Some(adc) = adc {
                    let reply = self.exchange(adc, &path, cancel, deadline).await?;
                    cached_file_token(&path, reply)
                } else {
                    let reply = self.metadata(cancel, provider, deadline).await?;
                    cached_metadata_token(reply)
                }
            }
            Selected::Metadata => {
                let reply = self.metadata(cancel, provider, deadline).await?;
                cached_metadata_token(reply)
            }
        }
    }

    /// Reads and strictly decodes one ADC file off the async worker: the
    /// blocking open, identity check, and parse run in the blocking pool and
    /// the caller races its cancellation against the bounded join.
    async fn load_file(
        &self,
        path: &Path,
        cancel: &CancellationToken,
    ) -> Result<Option<AdcFile>, ProviderError> {
        let path = path.to_path_buf();
        let load = tokio::task::spawn_blocking(move || load_adc_file(&path));
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(ProviderError::Cancelled),
            result = load => match result {
                Ok(inner) => inner,
                Err(_) if cancel.is_cancelled() => Err(ProviderError::Cancelled),
                Err(_) => Err(ProviderError::Transport {
                    family: Family::GoogleVertex,
                    reason: String::from("Google ADC file read task failed"),
                }),
            },
        }
    }

    /// Exchanges a parsed ADC file for a token reply at the OAuth endpoint.
    /// The whole request, body read, and parse race the caller's
    /// cancellation and the shared resolution deadline.
    async fn exchange(
        &self,
        adc: AdcFile,
        source: &Path,
        cancel: &CancellationToken,
        deadline: tokio::time::Instant,
    ) -> Result<TokenReply, ProviderError> {
        let body = match adc {
            AdcFile::AuthorizedUser {
                client_id,
                client_secret,
                refresh_token,
            } => format!(
                "client_id={}&client_secret={}&refresh_token={}&grant_type=refresh_token",
                form(&client_id),
                form(&client_secret),
                form(&refresh_token),
            ),
            AdcFile::ServiceAccount {
                client_email,
                private_key,
                private_key_id,
            } => {
                let assertion = sign_blocking(
                    source,
                    &client_email,
                    &private_key,
                    private_key_id.as_deref(),
                    cancel,
                )
                .await?;
                format!(
                    "grant_type={}&assertion={}",
                    form(JWT_GRANT),
                    form(&assertion)
                )
            }
            AdcFile::Unsupported => {
                return Err(ProviderError::InvalidRequest {
                    message: String::from("unsupported Vertex ADC credential type"),
                });
            }
        };
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(resolution_timeout());
        }
        let url = {
            #[cfg(any(test, feature = "test-support"))]
            {
                self.endpoints.token.clone()
            }
            #[cfg(not(any(test, feature = "test-support")))]
            {
                TOKEN_URL.to_owned()
            }
        };
        let response = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ProviderError::Cancelled),
            response = http::send(
                Family::GoogleVertex,
                self.client
                    .post(url)
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(body),
                ADC_AGENT,
                Exchange::Json {
                    total: remaining.min(http::OAUTH_TIMEOUT),
                },
                tokio::time::sleep,
            ) => response?,
        };
        if !response.status().is_success() {
            return Err(ProviderError::TokenExchange {
                status: response.status().as_u16(),
                message: String::from("Google ADC token exchange failed"),
            });
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(resolution_timeout());
        }
        let bytes = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ProviderError::Cancelled),
            bytes = http::read_body(Family::GoogleVertex, response) => bytes?,
        };
        if tokio::time::Instant::now() >= deadline {
            return Err(resolution_timeout());
        }
        sonic_rs::from_slice(&bytes).map_err(|_| ProviderError::TokenExchange {
            status: 200,
            message: String::from("Google ADC token response was invalid"),
        })
    }

    /// Reads the ambient metadata token over the one pinned plain-HTTP
    /// origin: no redirect away from the request URL, the `Metadata-Flavor:
    /// Google` request header and response check, a 2-second cap raced with
    /// the caller's cancellation, and a bounded body read.
    async fn metadata(
        &self,
        cancel: &CancellationToken,
        provider: &str,
        deadline: tokio::time::Instant,
    ) -> Result<TokenReply, ProviderError> {
        let url = {
            #[cfg(any(test, feature = "test-support"))]
            {
                self.endpoints.metadata.clone()
            }
            #[cfg(not(any(test, feature = "test-support")))]
            {
                METADATA_URL.to_owned()
            }
        };
        let parsed = metadata_admitted(&url)?;
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(resolution_timeout());
        }
        let mut request = self
            .client
            .get(parsed.clone())
            .header("user-agent", ADC_AGENT)
            .header(METADATA_HEADER, "Google")
            .build()
            .map_err(|error| http::from_reqwest(Family::GoogleVertex, error))?;
        *request.timeout_mut() = Some(remaining.min(METADATA_TIMEOUT));
        let response = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ProviderError::Cancelled),
            response = self.client.execute(request) => response
                .map_err(|error| http::from_reqwest(Family::GoogleVertex, error))?,
        };
        if response.url() != &parsed {
            return Err(ProviderError::Transport {
                family: Family::GoogleVertex,
                reason: String::from("refusing a redirect away from the Google metadata origin"),
            });
        }
        if !response.status().is_success() {
            return Err(ProviderError::NoCredentials {
                provider: provider.to_owned(),
            });
        }
        let flavored = response
            .headers()
            .get(METADATA_HEADER)
            .and_then(|value| value.to_str().ok())
            == Some("Google");
        if !flavored {
            return Err(ProviderError::Transport {
                family: Family::GoogleVertex,
                reason: String::from(
                    "Google metadata response lacks the Metadata-Flavor: Google header",
                ),
            });
        }
        let bytes = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ProviderError::Cancelled),
            bytes = http::read_body(Family::GoogleVertex, response) => bytes?,
        };
        if tokio::time::Instant::now() >= deadline {
            return Err(resolution_timeout());
        }
        sonic_rs::from_slice(&bytes).map_err(|_| ProviderError::Transport {
            family: Family::GoogleVertex,
            reason: String::from("Google metadata response was invalid"),
        })
    }
}

/// The one transport gate for the ambient metadata source: production only
/// ever sends the exact pinned Google metadata origin, and a test build may
/// point the same request at a literal loopback endpoint and nowhere else.
/// No other plain-HTTP origin and no arbitrary URL reaches the socket here.
fn metadata_admitted(url: &str) -> Result<url::Url, ProviderError> {
    let parsed = url::Url::parse(url).map_err(|_| ProviderError::Transport {
        family: Family::GoogleVertex,
        reason: String::from("Google metadata endpoint is not a URL"),
    })?;
    if url == METADATA_URL {
        return Ok(parsed);
    }
    #[cfg(any(test, feature = "test-support"))]
    {
        if is_loopback_url(&parsed) {
            return Ok(parsed);
        }
    }
    Err(ProviderError::Transport {
        family: Family::GoogleVertex,
        reason: String::from("refusing a Google metadata endpoint that is not the pinned origin"),
    })
}

/// Whether `url` is plain HTTP to one of the literal loopback hosts.
#[cfg(any(test, feature = "test-support"))]
fn is_loopback_url(url: &url::Url) -> bool {
    use std::net::{Ipv4Addr, Ipv6Addr};
    url.scheme() == "http"
        && match url.host() {
            Some(url::Host::Domain(name)) => name == "localhost",
            Some(url::Host::Ipv4(address)) => address == Ipv4Addr::LOCALHOST,
            Some(url::Host::Ipv6(address)) => address == Ipv6Addr::LOCALHOST,
            None => false,
        }
}

/// The typed failure for a missing explicit GAC file: a hard source choice,
/// never a silent fall-through.
fn missing_file(path: &Path) -> ProviderError {
    ProviderError::AuthFileInvalid {
        path: path.to_path_buf(),
        message: String::from("ADC file cannot be read"),
    }
}

fn resolution_timeout() -> ProviderError {
    ProviderError::Transport {
        family: Family::GoogleVertex,
        reason: String::from("Google ADC resolution timed out"),
    }
}

/// The cached token, when one is still serviceable: outside the refresh
/// skew, never an expired or zero-lifetime one.
fn fresh_cached(cache: &Mutex<Option<Cached>>) -> Option<Box<str>> {
    let cache = cache.lock().unwrap_or_else(PoisonError::into_inner);
    let cached = cache.as_ref()?;
    (cached.expires_at > unix_now().saturating_add(REFRESH_SKEW_SECS)).then(|| cached.token.clone())
}

/// Validates one token reply for a file source; the failure names the file.
fn cached_file_token(path: &Path, reply: TokenReply) -> Result<Cached, ProviderError> {
    if !usable_reply(&reply) {
        return Err(ProviderError::AuthFileInvalid {
            path: path.to_path_buf(),
            message: String::from("token response has an invalid access token or expiry"),
        });
    }
    Ok(cached_token(reply))
}

/// Validates one token reply for the metadata source; the failure is a
/// transport-class error because the source is the network, not a file.
fn cached_metadata_token(reply: TokenReply) -> Result<Cached, ProviderError> {
    if !usable_reply(&reply) {
        return Err(ProviderError::Transport {
            family: Family::GoogleVertex,
            reason: String::from("Google metadata token response was invalid"),
        });
    }
    Ok(cached_token(reply))
}

fn usable_reply(reply: &TokenReply) -> bool {
    nonblank(&reply.access_token) && reply.expires_in > 0
}

fn cached_token(reply: TokenReply) -> Cached {
    Cached {
        token: reply.access_token.into_boxed_str(),
        expires_at: unix_now().saturating_add(reply.expires_in),
    }
}

/// Reads and strictly decodes one ADC file: `Ok(None)` for an absent file
/// (the caller decides whether absence is a hard failure or a fall-through),
/// a typed file error for anything present but broken.
fn load_adc_file(path: &Path) -> Result<Option<AdcFile>, ProviderError> {
    let bytes = match read_private(path) {
        Ok(bytes) => bytes,
        Err(ReadError::Absent) => return Ok(None),
        Err(ReadError::Rejected(message)) => {
            return Err(ProviderError::AuthFileInvalid {
                path: path.to_path_buf(),
                message: message.into(),
            });
        }
    };
    let adc: AdcFile =
        sonic_rs::from_slice(&bytes).map_err(|_| ProviderError::AuthFileInvalid {
            path: path.to_path_buf(),
            message: String::from("ADC JSON is invalid"),
        })?;
    validate_adc(&adc, path)?;
    Ok(Some(adc))
}

/// Strict field validation at the selected source boundary: the required
/// fields of each form, the usable PEM for a service account, and a typed
/// refusal for every other `type`. No key material reaches any message.
fn validate_adc(adc: &AdcFile, path: &Path) -> Result<(), ProviderError> {
    let invalid = |message: &str| ProviderError::AuthFileInvalid {
        path: path.to_path_buf(),
        message: message.to_owned(),
    };
    match adc {
        AdcFile::AuthorizedUser {
            client_id,
            client_secret,
            refresh_token,
        } => {
            if nonblank(client_id) && nonblank(client_secret) && nonblank(refresh_token) {
                return Ok(());
            }
            Err(invalid(
                "authorized_user needs nonblank client_id, client_secret, and refresh_token",
            ))
        }
        AdcFile::ServiceAccount {
            client_email,
            private_key,
            ..
        } => {
            if !nonblank(client_email) {
                return Err(invalid("service_account needs a nonblank client_email"));
            }
            parse_signing_key(private_key)
                .map(|_| ())
                .map_err(|_| invalid("service_account private key is not a usable RSA PEM"))?;
            Ok(())
        }
        AdcFile::Unsupported => Err(invalid("unsupported ADC credential type for Vertex")),
    }
}

/// Why one private-file read was refused; absence is its own outcome.
enum ReadError {
    Absent,
    Rejected(&'static str),
}

/// Reads one ADC file with the `auth.json` private-file discipline: the
/// initial symlink/regular check, the `dal_store` open that also applies the
/// Windows DACL helper, the opened handle verified against the initial
/// metadata (`same_file`), the owner-only permission probe, and the bytes
/// read from that one handle. No second open, no partial identity.
fn read_private(path: &Path) -> Result<Vec<u8>, ReadError> {
    let link = match std::fs::symlink_metadata(path) {
        Ok(link) => link,
        Err(error) if error.kind() == ErrorKind::NotFound => return Err(ReadError::Absent),
        Err(_) => return Err(ReadError::Rejected("ADC file cannot be read")),
    };
    if link.file_type().is_symlink() {
        return Err(ReadError::Rejected("ADC path is a symbolic link"));
    }
    let mut file = match dal_store::open_private_file(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Err(ReadError::Absent),
        Err(error) if error.kind() == ErrorKind::PermissionDenied => {
            return Err(ReadError::Rejected("ADC file permissions are not private"));
        }
        Err(_) => return Err(ReadError::Rejected("ADC file cannot be read")),
    };
    let opened = file
        .metadata()
        .map_err(|_| ReadError::Rejected("ADC file cannot be read"))?;
    if !same_file(&link, &opened) {
        return Err(ReadError::Rejected("ADC path changed while opening"));
    }
    if !opened.is_file() {
        return Err(ReadError::Rejected("ADC path is not a regular file"));
    }
    let private = dal_store::file_is_private(&file)
        .map_err(|_| ReadError::Rejected("ADC file permissions are not private"))?;
    if !private {
        return Err(ReadError::Rejected("ADC file permissions are not private"));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|_| ReadError::Rejected("ADC file cannot be read"))?;
    Ok(bytes)
}

/// The `auth.json` open-handle identity check, the exact `AuthStore`
/// discipline: the opened file must be the inode the initial metadata
/// described (POSIX dev+ino; elsewhere the platform offers no comparison,
/// as in `credential::same_file`).
#[cfg(unix)]
fn same_file(link: &std::fs::Metadata, opened: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    link.dev() == opened.dev() && link.ino() == opened.ino()
}

#[cfg(not(unix))]
fn same_file(link: &std::fs::Metadata, opened: &std::fs::Metadata) -> bool {
    let _ = (link, opened);
    true
}

/// Signs one RS256 JWT bearer assertion off the async worker (RSA private
/// operations run in the blocking pool, raced with the caller's
/// cancellation).
async fn sign_blocking(
    source: &Path,
    email: &str,
    pem: &str,
    key_id: Option<&str>,
    cancel: &CancellationToken,
) -> Result<String, ProviderError> {
    let (source, email, pem, key_id) = (
        source.to_path_buf(),
        email.to_owned(),
        pem.to_owned(),
        key_id.map(str::to_owned),
    );
    let job = tokio::task::spawn_blocking(move || {
        sign_assertion(&email, &pem, key_id.as_deref(), &source)
    });
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(ProviderError::Cancelled),
        result = job => match result {
            Ok(inner) => inner,
            Err(_) if cancel.is_cancelled() => Err(ProviderError::Cancelled),
            Err(_) => Err(ProviderError::Transport {
                family: Family::GoogleVertex,
                reason: String::from("Vertex service-account signing task failed"),
            }),
        },
    }
}

/// Builds and signs `header.payload` with the installed rustls/aws-lc stack:
/// PEM parse, `any_supported_type`, and exactly `RSA_PKCS1_SHA256`. Every
/// failure is a typed file error naming the source; none echoes key bytes.
fn sign_assertion(
    email: &str,
    pem: &str,
    key_id: Option<&str>,
    source: &Path,
) -> Result<String, ProviderError> {
    use rustls::SignatureScheme;
    use rustls::crypto::aws_lc_rs::sign::any_supported_type;
    let invalid = |message: &str| ProviderError::AuthFileInvalid {
        path: source.to_path_buf(),
        message: message.to_owned(),
    };
    let key = parse_signing_key(pem)
        .map_err(|_| invalid("service_account private key is not a usable RSA PEM"))?;
    let signer = any_supported_type(&key)
        .map_err(|_| invalid("service_account private key is unsupported"))?;
    let signer = signer
        .choose_scheme(&[SignatureScheme::RSA_PKCS1_SHA256])
        .ok_or_else(|| invalid("service_account key does not support RS256"))?;
    let kid = match key_id {
        Some(kid) if nonblank(kid) => json_string(kid).ok_or_else(|| {
            invalid("service_account private_key_id could not be encoded as JSON")
        })?,
        _ => String::new(),
    };
    let email = json_string(email)
        .ok_or_else(|| invalid("service_account client_email could not be encoded as JSON"))?;
    let now = unix_now();
    let header = if kid.is_empty() {
        String::from(r#"{"alg":"RS256","typ":"JWT"}"#)
    } else {
        format!(r#"{{"alg":"RS256","typ":"JWT","kid":{kid}}}"#)
    };
    let claims = format!(
        r#"{{"iss":{email},"scope":{},"aud":{},"iat":{now},"exp":{}}}"#,
        json_string(CLOUD_PLATFORM_SCOPE).unwrap_or_default(),
        json_string(TOKEN_URL).unwrap_or_default(),
        now.saturating_add(3600),
    );
    let input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header.as_bytes()),
        URL_SAFE_NO_PAD.encode(claims.as_bytes()),
    );
    let signature = signer
        .sign(input.as_bytes())
        .map_err(|_| invalid("Vertex service-account assertion could not be signed"))?;
    Ok(format!("{}.{}", input, URL_SAFE_NO_PAD.encode(signature)))
}

/// Parses a PKCS#8 or PKCS#1 RSA PEM with the installed pki-types
/// constructors; the error never carries key material.
fn parse_signing_key(
    pem: &str,
) -> Result<rustls::pki_types::PrivateKeyDer<'static>, ProviderError> {
    use rustls::pki_types::pem::PemObject;
    if let Ok(key) = rustls::pki_types::PrivatePkcs8KeyDer::from_pem_slice(pem.as_bytes()) {
        return Ok(rustls::pki_types::PrivateKeyDer::Pkcs8(key));
    }
    if let Ok(key) = rustls::pki_types::PrivatePkcs1KeyDer::from_pem_slice(pem.as_bytes()) {
        return Ok(rustls::pki_types::PrivateKeyDer::Pkcs1(key));
    }
    Err(ProviderError::InvalidRequest {
        message: String::from("Vertex service-account private key is invalid"),
    })
}

/// Encodes one string as a JSON string literal, or nothing when the encoder
/// refuses (callers turn that into a typed file error).
fn json_string(value: &str) -> Option<String> {
    sonic_rs::to_string(value)
        .ok()
        .filter(|encoded| !encoded.is_empty())
}

fn nonblank(value: &str) -> bool {
    !value.trim().is_empty()
}

fn unix_now() -> i64 {
    jiff::Timestamp::now().as_second()
}

fn form(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

/// Resolves a captured `GOOGLE_APPLICATION_CREDENTIALS` value against the
/// captured cwd only; without a captured cwd a relative path is a typed
/// invalid file, never an ambient-cwd read.
fn resolve_named_path(value: &str, cwd: Option<&Path>) -> Result<PathBuf, ProviderError> {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        return Ok(path);
    }
    cwd.map(|cwd| cwd.join(&path))
        .ok_or_else(|| ProviderError::AuthFileInvalid {
            path,
            message: String::from("relative ADC path requires captured cwd"),
        })
}

/// The user ADC path from the captured snapshot: the Windows spelling reads
/// `APPDATA`, every other platform reads `HOME`. A blank captured value
/// derives nothing.
fn user_adc_path(env: &EnvSnapshot) -> Option<PathBuf> {
    if cfg!(windows) {
        user_adc_path_windows(env)
    } else {
        user_adc_path_posix(env)
    }
}

/// `${APPDATA}/gcloud/application_default_credentials.json`.
fn user_adc_path_windows(env: &EnvSnapshot) -> Option<PathBuf> {
    env.get("APPDATA")
        .filter(|value| nonblank(value))
        .map(|value| PathBuf::from(value).join("gcloud/application_default_credentials.json"))
}

/// `${HOME}/.config/gcloud/application_default_credentials.json`.
fn user_adc_path_posix(env: &EnvSnapshot) -> Option<PathBuf> {
    env.get("HOME")
        .filter(|value| nonblank(value))
        .map(|value| {
            PathBuf::from(value).join(".config/gcloud/application_default_credentials.json")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::Mutex as AsyncTestMutex;

    /// A throwaway RSA test key (PKCS#8 DER, base64), generated once with
    /// openssl for this fixture; no runtime openssl dependency and no real
    /// credential.
    const TEST_RSA_PKCS8_DER_B64: &str = "MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQDin5WZ5sTPAG0sqHWZxwH8RT3xCsjMy8il+5VP5qupbPQF2+tRoaLnatRELWLvbNpbTFoVPGuoqPYWSMl902G12RF3sd+eI3bd+ZFY0mInCmlGvTrRYovf+Tbvp2IeBXwEK79Bfxdo7ekDFY/BuzT5cuMZ7svUvNtCPwmUX79UrOSZLqw5v//p6CXw/nLEyIe7ZBPHvYYX4WDxSsojJbLQlvDNabQEA5rNNQ9iz8r5v1h+CB3j2o3hr3J4Msb5P8mIt0xFbky2ueL4PNQeCpLYKF6+cuw3oB90cQoETwJ8SNlOuvlMG3ki8pSk4aEVLH2zGKnBKAFKEbUAxCzFUDwPAgMBAAECggEALCKvVItMfhITsRRX3uySlup3T0ft5nCzsZvHLwn6GaOF/NQhRsMnglFcaLC8KWv2NLyweJ1NTEonrxxa3X369BsD219RcS9v+Xge1gEXK4pMCYJV/6xlGGglSlQ7eCRSTE+mzl8tlHptHHQpDK5keUZmM0k9+j8Tppm3SGXDUf6CLvpmKo9N5bw08iDg/k0mBIT7gBPhdMlkhDSgEfxFMs3diQQtwO4IBTFi+IG4j4NqVbOl3qIIM/dd7tEw084/XfZeeLCPlOATM8BIv/hIPREXoFzmDfU2gd4A0rQmvJutQMP076W/VM0IhTEA9t0j3ClQm5a/0wtdrCmWn1GjCQKBgQDxjcAtgLgNw5aoo2Igy0QPrJ/YX37E/TZYTMz95aHMQv7x1bqXpFdqKg9QXz+6ok8Zlei6sKir1Cg+p2He8s6tDLi3czLBH16LUZgylD5C/geWuQrdT8MDIGFJmvni+CWjwLMFZbWTLXXrm8JmX7FMUvMsu/CKlAOJDlPF4xzZdwKBgQDwLT8TsDn9TPn57o2V9HXPkFiVrvWLfOlJA56tEbapYx8QeEQzWNjy2LtrcA7smt8I4V0aIEC39n21FKMQDzSQrnax9pDIbQ2UP0nkafTnTAQlU/10tXmEcZNSrzZYts3CIHblIsQ6SdcXfHC6hqpnLEZoJx29+n8o1myJALPYKQKBgGEYEGJ59gGaV+nLWrtB2UbPpaTMpeE6wuh1XoTEE+hOGbpNpeYkafrDk0pELxKvAUqQhm7cacA/1DlzAJUkVk0a6vjadlFIcbFgh+P00J4PDC/ai3JwCDwufMygXceM5VxQLeWCf5z8TgT5pAeQeWzrftHBbavWWSFzP9swMj17AoGBAOdhn6/3CZbWrsiyomKB1XG2zTC/Ej1khFZcD2UKeKAtJwM16fZRtQgX9k8l8KrO9OnYO1ptEgxogmiOTh2tmGV+PklqkccVEIzuf+B2l4KxABl6FeZ24m/O/zAcLk4/JnDL198dpE/HTVmKAAwiqk2lINs8O/IHPrP4By3hqGEpAoGBAMX5w/06SVy+BgsPFyUBQYBKJtImOuVOJEZjkLS+heM1hOCzoTgyxXE1QNSoolDnnCERXp863ozLZaRMcya5jlLVwjFshV6imNflkhW+xqfqMCvGPOnBMxhaKKXGJoxNg9fS3mSb34SkgYYCR+2MxTKkWv4utll3sTSg0eYdxPnP";

    const AUTHORIZED_USER_JSON: &[u8] =
        br#"{"type":"authorized_user","client_id":"gac-client","client_secret":"gac-secret","refresh_token":"gac-refresh"}"#;

    const TOKEN_OK: &str = r#"{"access_token":"adc-token","expires_in":3600}"#;

    /// An endpoint nothing can answer: connections fail fast, so a test that
    /// wrongly sends there fails instead of hanging.
    fn unused_endpoint(path: &str) -> String {
        String::from("http://[::1]:9") + path
    }

    struct CapturedRequest {
        method: String,
        target: String,
        headers: Vec<(String, String)>,
        body: String,
    }

    /// Whether the fake reply carries the Google metadata flavor header.
    enum Flavor {
        Google,
        Absent,
    }

    struct Reply {
        delay: Duration,
        status: u16,
        flavor: Flavor,
        body: &'static str,
    }

    fn token_reply(expires_in: i64) -> Reply {
        Reply {
            delay: Duration::ZERO,
            status: 200,
            flavor: Flavor::Absent,
            body: match expires_in {
                3600 => TOKEN_OK,
                _ => r#"{"access_token":"adc-token","expires_in":30}"#,
            },
        }
    }

    fn delayed(expires_in: i64) -> Reply {
        Reply {
            delay: Duration::from_millis(400),
            ..token_reply(expires_in)
        }
    }

    /// Serves one scripted reply per connection over IPv6 loopback, records
    /// every request (method, target, headers, body), and closes cleanly.
    /// Owns its bounded accept loop in the returned task set: every caller
    /// drains it, so no worker outlives the test.
    async fn serve(
        replies: Vec<Reply>,
    ) -> (
        tokio::task::JoinSet<()>,
        String,
        Arc<AsyncTestMutex<Vec<CapturedRequest>>>,
    ) {
        let listener = TcpListener::bind("[::1]:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let captured: Arc<AsyncTestMutex<Vec<CapturedRequest>>> = Arc::default();
        let mut workers = tokio::task::JoinSet::new();
        let worker = Arc::clone(&captured);
        workers.spawn(async move {
            for reply in replies {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                if let Some(request) = read_http_request(&mut stream).await {
                    worker.lock().await.push(request);
                }
                tokio::time::sleep(reply.delay).await;
                let flavor = match reply.flavor {
                    Flavor::Google => "metadata-flavor: Google\r\n",
                    Flavor::Absent => "",
                };
                let response = format!(
                    "HTTP/1.1 {} {}\r\ncontent-type: application/json\r\n{flavor}content-length: {}\r\nconnection: close\r\n\r\n{}",
                    reply.status,
                    status_text(reply.status),
                    reply.body.len(),
                    reply.body,
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        (workers, format!("http://[::1]:{port}"), captured)
    }

    fn status_text(status: u16) -> &'static str {
        match status {
            200 => "OK",
            401 => "Unauthorized",
            404 => "Not Found",
            429 => "Too Many Requests",
            _ => "Internal Server Error",
        }
    }

    async fn read_http_request(stream: &mut tokio::net::TcpStream) -> Option<CapturedRequest> {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 2048];
        let head_end = loop {
            let count = stream.read(&mut buffer).await.ok()?;
            if count == 0 {
                return None;
            }
            bytes.extend_from_slice(&buffer[..count]);
            if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break end;
            }
            if bytes.len() > 64 * 1024 {
                return None;
            }
        };
        let head = String::from_utf8_lossy(&bytes[..head_end]).into_owned();
        let mut lines = head.lines();
        let request_line = lines.next()?;
        let mut fields = request_line.split_whitespace();
        let method = fields.next()?;
        let target = fields.next()?;
        let headers: Vec<(String, String)> = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
            .collect();
        let length: usize = headers
            .iter()
            .find(|(name, _)| name == "content-length")
            .and_then(|(_, value)| value.parse().ok())
            .unwrap_or(0);
        let body_start = head_end + 4;
        while bytes.len() < body_start + length {
            let count = stream.read(&mut buffer).await.ok()?;
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
        let body = String::from_utf8_lossy(&bytes[body_start..]).into_owned();
        Some(CapturedRequest {
            method: method.to_owned(),
            target: target.to_owned(),
            headers,
            body,
        })
    }

    /// Writes a private regular file in the per-test temp area.
    fn private_file(name: &str, contents: &[u8]) -> PathBuf {
        let path = temp_path(name);
        let _ = fs::remove_file(&path);
        fs::write(&path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        path
    }

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("dal-vertex-adc-{}-{name}", std::process::id()))
    }

    fn gac_env(path: &Path) -> EnvSnapshot {
        EnvSnapshot::test(&[("GOOGLE_APPLICATION_CREDENTIALS", path.to_str().unwrap())])
    }

    /// The PEM the service-account fixture carries, rebuilt from the DER.
    fn test_rsa_pem() -> String {
        let mut pem = String::from("-----BEGIN PRIVATE KEY-----\n");
        let bytes = TEST_RSA_PKCS8_DER_B64.as_bytes();
        for chunk in bytes.chunks(64) {
            pem.push_str(std::str::from_utf8(chunk).unwrap());
            pem.push('\n');
        }
        pem.push_str("-----END PRIVATE KEY-----\n");
        pem
    }

    fn service_account_json() -> String {
        format!(
            r#"{{"type":"service_account","client_email":"sa@test.invalid","private_key":"{}","private_key_id":"test-kid-1"}}"#,
            test_rsa_pem().replace('\n', "\\n"),
        )
    }

    #[derive(Deserialize)]
    struct JwtHeader {
        alg: String,
        kid: Option<String>,
    }

    #[derive(Deserialize)]
    struct JwtClaims {
        iss: String,
        scope: String,
        aud: String,
        iat: i64,
        exp: i64,
    }

    #[tokio::test]
    async fn vertex_adc_authorized_user_refreshes_and_caches() {
        let path = private_file("authorized-user.json", AUTHORIZED_USER_JSON);
        // One exchange serves a long-lived token; the second resolve must be
        // a cache hit, and the captured Vertex-independent request carries
        // the exact refresh form.
        let (mut workers, endpoint, seen) = serve(vec![token_reply(3600)]).await;
        let resolver = Resolver::new(gac_env(&path), reqwest::Client::new())
            .with_endpoints(endpoint, unused_endpoint("/metadata"));
        let cancel = CancellationToken::new();
        let token = resolver.resolve(&cancel, "google-vertex").await.unwrap();
        assert_eq!(&*token, "adc-token");
        let again = resolver.resolve(&cancel, "google-vertex").await.unwrap();
        assert_eq!(&*again, "adc-token");
        let requests = seen.lock().await;
        assert_eq!(requests.len(), 1, "the second resolve must be cached");
        let request = &requests[0];
        assert_eq!(request.method, "POST");
        assert!(
            request
                .headers
                .iter()
                .any(|(name, value)| name == "content-type"
                    && value.starts_with("application/x-www-form-urlencoded")),
            "the exchange is form-encoded"
        );
        assert!(request.body.contains("client_id=gac-client"));
        assert!(request.body.contains("client_secret=gac-secret"));
        assert!(request.body.contains("refresh_token=gac-refresh"));
        assert!(request.body.contains("grant_type=refresh_token"));
        drop(requests);
        // A token expiring inside the 60 s skew is never served twice: each
        // resolve refreshes, proving the skew boundary without wall-clock
        // sleeps.
        let (mut skew_workers, skew_endpoint, skew_seen) =
            serve(vec![token_reply(30), token_reply(30)]).await;
        let skew = Resolver::new(gac_env(&path), reqwest::Client::new())
            .with_endpoints(skew_endpoint, unused_endpoint("/metadata"));
        let _ = skew.resolve(&cancel, "google-vertex").await.unwrap();
        let _ = skew.resolve(&cancel, "google-vertex").await.unwrap();
        assert_eq!(
            skew_seen.lock().await.len(),
            2,
            "a token inside the skew window must refresh, never be reused"
        );
        while skew_workers.join_next().await.is_some() {}
        while workers.join_next().await.is_some() {}
        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn vertex_adc_service_account_signs_rs256_jwt() {
        let path = private_file("service-account.json", service_account_json().as_bytes());
        let (mut workers, endpoint, seen) = serve(vec![Reply {
            body: r#"{"access_token":"service-token","expires_in":3600}"#,
            ..token_reply(3600)
        }])
        .await;
        let resolver = Resolver::new(gac_env(&path), reqwest::Client::new())
            .with_endpoints(endpoint, unused_endpoint("/metadata"));
        let cancel = CancellationToken::new();
        let token = resolver.resolve(&cancel, "google-vertex").await.unwrap();
        assert_eq!(&*token, "service-token");
        let requests = seen.lock().await;
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert!(request.body.starts_with(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion="
        ));
        let jwt = request.body.split("assertion=").nth(1).unwrap();
        let segments: Vec<&str> = jwt.split('.').collect();
        assert_eq!(segments.len(), 3, "a compact JWT has three segments");
        let header: JwtHeader =
            sonic_rs::from_slice(&URL_SAFE_NO_PAD.decode(segments[0]).unwrap()).unwrap();
        assert_eq!(header.alg, "RS256");
        assert_eq!(header.kid.as_deref(), Some("test-kid-1"));
        let claims: JwtClaims =
            sonic_rs::from_slice(&URL_SAFE_NO_PAD.decode(segments[1]).unwrap()).unwrap();
        assert_eq!(claims.iss, "sa@test.invalid");
        assert_eq!(claims.scope, CLOUD_PLATFORM_SCOPE);
        assert_eq!(claims.aud, TOKEN_URL);
        let now = unix_now();
        assert!(
            claims.iat <= now + 5 && claims.iat >= now - 300,
            "iat {} must be the current time {now}",
            claims.iat,
        );
        assert_eq!(claims.exp - claims.iat, 3600);
        let signature = URL_SAFE_NO_PAD.decode(segments[2]).unwrap();
        assert_eq!(
            signature.len(),
            256,
            "RS256 over a 2048-bit key signs 256 bytes"
        );
        drop(requests);
        let again = resolver.resolve(&cancel, "google-vertex").await.unwrap();
        assert_eq!(&*again, "service-token");
        assert_eq!(seen.lock().await.len(), 1, "the signed assertion is cached");
        while workers.join_next().await.is_some() {}
        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn vertex_adc_source_precedence() {
        let cancel = CancellationToken::new();
        // The explicit env bearer bypasses files, metadata, and the cache:
        // the GAC names a missing file, yet the token resolves with zero
        // requests, twice.
        let missing = temp_path("precedence-missing.json");
        let _ = fs::remove_file(&missing);
        let env = EnvSnapshot::test(&[
            ("GOOGLE_APPLICATION_CREDENTIALS", missing.to_str().unwrap()),
            ("GOOGLE_CLOUD_ACCESS_TOKEN", "env-bearer"),
        ]);
        let resolver = Resolver::new(env, reqwest::Client::new())
            .with_endpoints(unused_endpoint("/token"), unused_endpoint("/metadata"));
        let token = resolver.resolve(&cancel, "google-vertex").await.unwrap();
        assert_eq!(&*token, "env-bearer");
        let again = resolver.resolve(&cancel, "google-vertex").await.unwrap();
        assert_eq!(&*again, "env-bearer");
        // The alias order: the gcloud override variable answers only when the
        // cloud alias is absent or blank.
        let alias = EnvSnapshot::test(&[
            ("GOOGLE_CLOUD_ACCESS_TOKEN", "  "),
            ("CLOUDSDK_AUTH_ACCESS_TOKEN", "sdk-bearer"),
        ]);
        let alias_resolver = Resolver::new(alias, reqwest::Client::new());
        let aliased = alias_resolver
            .resolve(&cancel, "google-vertex")
            .await
            .unwrap();
        assert_eq!(&*aliased, "sdk-bearer");
        // The GAC file beats the user ADC file: both exist, only the GAC
        // client reaches the token endpoint.
        let gac = private_file("precedence-gac.json", AUTHORIZED_USER_JSON);
        let home = temp_path("precedence-home");
        let user = home.join(".config/gcloud/application_default_credentials.json");
        fs::create_dir_all(user.parent().unwrap()).unwrap();
        fs::write(
            &user,
            br#"{"type":"authorized_user","client_id":"user-client","client_secret":"u","refresh_token":"u"}"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&user, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let (mut workers, endpoint, seen) = serve(vec![token_reply(3600)]).await;
        let both = EnvSnapshot::test(&[
            ("GOOGLE_APPLICATION_CREDENTIALS", gac.to_str().unwrap()),
            ("HOME", home.to_str().unwrap()),
        ]);
        let resolver = Resolver::new(both, reqwest::Client::new())
            .with_endpoints(endpoint, unused_endpoint("/metadata"));
        let token = resolver.resolve(&cancel, "google-vertex").await.unwrap();
        assert_eq!(&*token, "adc-token");
        assert!(
            seen.lock().await[0].body.contains("client_id=gac-client"),
            "the GAC file must win over the user ADC file"
        );
        // An absent GAC file is a hard typed failure even when a user file
        // exists: no fallback, no metadata, no request.
        let (mut dead_workers, dead, dead_seen) = serve(vec![]).await;
        let hard = EnvSnapshot::test(&[
            ("GOOGLE_APPLICATION_CREDENTIALS", missing.to_str().unwrap()),
            ("HOME", home.to_str().unwrap()),
        ]);
        let resolver = Resolver::new(hard, reqwest::Client::new())
            .with_endpoints(dead, unused_endpoint("/metadata"));
        let error = resolver
            .resolve(&cancel, "google-vertex")
            .await
            .unwrap_err();
        assert!(
            matches!(&error, ProviderError::AuthFileInvalid { path, .. } if path == &missing),
            "absent GAC must be a typed hard failure, got {error:?}"
        );
        assert_eq!(dead_seen.lock().await.len(), 0);
        while dead_workers.join_next().await.is_some() {}
        while workers.join_next().await.is_some() {}
        let _ = fs::remove_file(gac);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn vertex_adc_user_path_uses_captured_home_or_appdata() {
        // The pure derivations, both platform spellings.
        let posix = EnvSnapshot::test(&[("HOME", "/home/tester")]);
        assert_eq!(
            user_adc_path_posix(&posix),
            Some(PathBuf::from(
                "/home/tester/.config/gcloud/application_default_credentials.json"
            ))
        );
        let windows_root = r"C:\Users\tester";
        let windows = EnvSnapshot::test(&[("APPDATA", windows_root)]);
        assert_eq!(
            user_adc_path_windows(&windows),
            Some(PathBuf::from(windows_root).join("gcloud/application_default_credentials.json")),
        );
        // A blank captured value derives nothing, on either spelling.
        assert_eq!(
            user_adc_path_posix(&EnvSnapshot::test(&[("HOME", " ")])),
            None
        );
        assert_eq!(
            user_adc_path_windows(&EnvSnapshot::test(&[("APPDATA", "")])),
            None
        );
    }

    #[test]
    fn vertex_adc_derivation_never_reads_the_ambient_process_environment() {
        // Whatever the process environment carries, a captured snapshot
        // without HOME or APPDATA derives nothing: the derivation consults
        // only captured state.
        let bare = EnvSnapshot::test(&[]);
        assert_eq!(user_adc_path(&bare), None);
        let resolver = Resolver::new(bare, reqwest::Client::new());
        assert!(matches!(
            resolver.selected_source().unwrap(),
            Selected::Metadata
        ));
        // The captured authority decides on its own: with both home
        // spellings captured, the user file is the selected source, with no
        // ambient read anywhere in the derivation.
        let captured =
            EnvSnapshot::test(&[("HOME", "/captured-home"), ("APPDATA", r"C:\captured-home")]);
        let resolver = Resolver::new(captured, reqwest::Client::new());
        assert!(matches!(
            resolver.selected_source().unwrap(),
            Selected::User(_)
        ));
    }

    #[tokio::test]
    async fn vertex_adc_user_path_resolves_from_captured_home_and_cwd() {
        // The user file at the captured HOME-derived path resolves; no GAC.
        let home = temp_path("captured-home");
        let user = home.join(".config/gcloud/application_default_credentials.json");
        fs::create_dir_all(user.parent().unwrap()).unwrap();
        fs::write(&user, AUTHORIZED_USER_JSON).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&user, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let (mut workers, endpoint, seen) = serve(vec![token_reply(3600)]).await;
        let env = EnvSnapshot::test(&[("HOME", home.to_str().unwrap())]);
        let resolver = Resolver::new(env, reqwest::Client::new())
            .with_endpoints(endpoint, unused_endpoint("/metadata"));
        let cancel = CancellationToken::new();
        let token = resolver.resolve(&cancel, "google-vertex").await.unwrap();
        assert_eq!(&*token, "adc-token");
        assert!(seen.lock().await[0].body.contains("client_id=gac-client"));
        // A relative GAC path resolves only against the captured cwd.
        let dir = temp_path("captured-cwd");
        fs::create_dir_all(&dir).unwrap();
        let relative = dir.join("adc-relative.json");
        fs::write(&relative, AUTHORIZED_USER_JSON).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&relative, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let without_cwd =
            EnvSnapshot::test(&[("GOOGLE_APPLICATION_CREDENTIALS", "adc-relative.json")]);
        let resolver = Resolver::new(without_cwd, reqwest::Client::new());
        let error = resolver
            .resolve(&cancel, "google-vertex")
            .await
            .unwrap_err();
        assert!(matches!(
            &error,
            ProviderError::AuthFileInvalid { message, .. }
                if message.contains("relative ADC path requires captured cwd")
        ));
        let with_cwd =
            EnvSnapshot::test(&[("GOOGLE_APPLICATION_CREDENTIALS", "adc-relative.json")])
                .with_cwd(dir.clone());
        let (mut cwd_workers, cwd_endpoint, _cwd_seen) = serve(vec![token_reply(3600)]).await;
        let resolver = Resolver::new(with_cwd, reqwest::Client::new())
            .with_endpoints(cwd_endpoint, unused_endpoint("/metadata"));
        let token = resolver.resolve(&cancel, "google-vertex").await.unwrap();
        assert_eq!(&*token, "adc-token");
        while cwd_workers.join_next().await.is_some() {}
        while workers.join_next().await.is_some() {}
        let _ = fs::remove_dir_all(home);
        let _ = fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn vertex_adc_metadata_success_and_header() {
        // No files: the metadata source answers a loopback fake that demands
        // and returns the Google flavor header. No live metadata hostname is
        // contacted: the endpoint is this test's own [::1] listener.
        let (mut workers, endpoint, seen) = serve(vec![Reply {
            flavor: Flavor::Google,
            ..token_reply(3600)
        }])
        .await;
        assert!(
            endpoint.starts_with("http://[::1]:"),
            "the fake must be loopback, got {endpoint}"
        );
        let resolver = Resolver::new(EnvSnapshot::test(&[]), reqwest::Client::new())
            .with_endpoints(unused_endpoint("/token"), format!("{endpoint}/metadata"));
        let cancel = CancellationToken::new();
        let token = resolver.resolve(&cancel, "google-vertex").await.unwrap();
        assert_eq!(&*token, "adc-token");
        let requests = seen.lock().await;
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.method, "GET");
        assert_eq!(request.target, "/metadata");
        assert!(
            request
                .headers
                .iter()
                .any(|(name, value)| name == "metadata-flavor" && value == "Google"),
            "the metadata request must carry the Google flavor header"
        );
        // A response without the flavor header is refused: the source is
        // never trusted on status alone.
        drop(requests);
        let (mut plain_workers, plain, _plain_seen) = serve(vec![Reply {
            flavor: Flavor::Absent,
            ..token_reply(3600)
        }])
        .await;
        let resolver = Resolver::new(EnvSnapshot::test(&[]), reqwest::Client::new())
            .with_endpoints(unused_endpoint("/token"), format!("{plain}/metadata"));
        let error = resolver
            .resolve(&cancel, "google-vertex")
            .await
            .unwrap_err();
        assert!(
            matches!(&error, ProviderError::Transport { reason, .. }
                if reason.contains("Metadata-Flavor")),
            "a flavorless metadata reply must be refused, got {error:?}"
        );
        while workers.join_next().await.is_some() {}
        while plain_workers.join_next().await.is_some() {}
    }

    #[tokio::test]
    async fn vertex_adc_metadata_failure_is_typed_and_not_fake_ready() {
        let cancel = CancellationToken::new();
        // A non-2xx metadata answer is a typed no-credentials failure.
        let (mut missing_workers, missing, missing_seen) = serve(vec![Reply {
            flavor: Flavor::Google,
            status: 404,
            ..token_reply(3600)
        }])
        .await;
        let resolver = Resolver::new(EnvSnapshot::test(&[]), reqwest::Client::new())
            .with_endpoints(unused_endpoint("/token"), format!("{missing}/metadata"));
        let error = resolver
            .resolve(&cancel, "google-vertex")
            .await
            .unwrap_err();
        assert!(
            matches!(&error, ProviderError::NoCredentials { provider } if provider == "google-vertex"),
            "got {error:?}"
        );
        assert_eq!(missing_seen.lock().await.len(), 1);
        // A malformed metadata body is a typed transport failure.
        let (mut broken_workers, broken, _broken_seen) = serve(vec![Reply {
            flavor: Flavor::Google,
            body: r#"{"access_token":"#,
            ..token_reply(3600)
        }])
        .await;
        let resolver = Resolver::new(EnvSnapshot::test(&[]), reqwest::Client::new())
            .with_endpoints(unused_endpoint("/token"), format!("{broken}/metadata"));
        let error = resolver
            .resolve(&cancel, "google-vertex")
            .await
            .unwrap_err();
        assert!(
            matches!(&error, ProviderError::Transport { reason, .. }
                if reason.contains("metadata response was invalid")),
            "got {error:?}"
        );
        // Readiness for a metadata-only source is false and sends nothing:
        // no probe may read ambient metadata or mint a token.
        let (mut silent_workers, silent, silent_seen) = serve(vec![]).await;
        let resolver = Resolver::new(EnvSnapshot::test(&[]), reqwest::Client::new())
            .with_endpoints(unused_endpoint("/token"), format!("{silent}/metadata"));
        assert!(!resolver.readiness(&cancel).await.unwrap());
        assert_eq!(silent_seen.lock().await.len(), 0);
        while missing_workers.join_next().await.is_some() {}
        while broken_workers.join_next().await.is_some() {}
        while silent_workers.join_next().await.is_some() {}
    }

    #[tokio::test]
    async fn vertex_adc_readiness_counts_a_strictly_valid_local_source() {
        let cancel = CancellationToken::new();
        // A strictly parsed, field-validated file is a usable local source:
        // ready before any mint, with zero HTTP (the fake serves nothing,
        // and the seen log proves no request left the process).
        let path = private_file("ready-authorized-user.json", AUTHORIZED_USER_JSON);
        let (mut silent_workers, silent, silent_seen) = serve(vec![]).await;
        let resolver = Resolver::new(gac_env(&path), reqwest::Client::new())
            .with_endpoints(silent, unused_endpoint("/metadata"));
        assert!(resolver.readiness(&cancel).await.unwrap());
        assert_eq!(silent_seen.lock().await.len(), 0, "readiness sends nothing");
        // A user-ADC file at the captured HOME counts the same way; a
        // missing user file stays unknown because metadata is never probed.
        let home = temp_path("ready-home");
        let user = home.join(".config/gcloud/application_default_credentials.json");
        fs::create_dir_all(user.parent().unwrap()).unwrap();
        fs::write(&user, AUTHORIZED_USER_JSON).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&user, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let resolver = Resolver::new(
            EnvSnapshot::test(&[("HOME", home.to_str().unwrap())]),
            reqwest::Client::new(),
        )
        .with_endpoints(unused_endpoint("/token"), unused_endpoint("/metadata"));
        assert!(resolver.readiness(&cancel).await.unwrap());
        // A cached token stays a ready source.
        let (mut workers, endpoint, seen) = serve(vec![token_reply(3600)]).await;
        let resolver = Resolver::new(gac_env(&path), reqwest::Client::new())
            .with_endpoints(endpoint, unused_endpoint("/metadata"));
        let _ = resolver.resolve(&cancel, "google-vertex").await.unwrap();
        assert!(resolver.readiness(&cancel).await.unwrap());
        assert_eq!(seen.lock().await.len(), 1);
        // A malformed selected file is a typed error, never readiness.
        let malformed = private_file(
            "ready-malformed.json",
            br#"{"type":"authorized_user","refresh_token":"only"}"#,
        );
        let resolver = Resolver::new(gac_env(&malformed), reqwest::Client::new())
            .with_endpoints(unused_endpoint("/token"), unused_endpoint("/metadata"));
        let error = resolver.readiness(&cancel).await.unwrap_err();
        assert!(
            matches!(&error, ProviderError::AuthFileInvalid { .. }),
            "{error:?}"
        );
        // A missing explicit GAC file is a typed hard failure, not false.
        let missing = temp_path("ready-missing.json");
        let _ = fs::remove_file(&missing);
        let resolver = Resolver::new(gac_env(&missing), reqwest::Client::new());
        let error = resolver.readiness(&cancel).await.unwrap_err();
        assert!(
            matches!(&error, ProviderError::AuthFileInvalid { .. }),
            "{error:?}"
        );
        while workers.join_next().await.is_some() {}
        while silent_workers.join_next().await.is_some() {}
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(malformed);
    }

    #[tokio::test]
    async fn vertex_adc_cancel_promptly_at_every_stage() {
        // Pre-cancelled: no request at all.
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let resolver = Resolver::new(EnvSnapshot::test(&[]), reqwest::Client::new())
            .with_endpoints(unused_endpoint("/token"), unused_endpoint("/metadata"));
        let error = resolver
            .resolve(&cancelled, "google-vertex")
            .await
            .unwrap_err();
        assert!(matches!(error, ProviderError::Cancelled));
        // During a pending token exchange: the delayed reply never lands.
        let path = private_file("cancel-authorized-user.json", AUTHORIZED_USER_JSON);
        let (mut workers, endpoint, seen) = serve(vec![delayed(3600)]).await;
        let resolver = Arc::new(
            Resolver::new(gac_env(&path), reqwest::Client::new())
                .with_endpoints(endpoint, unused_endpoint("/metadata")),
        );
        let cancel = CancellationToken::new();
        let mut callers = tokio::task::JoinSet::new();
        callers.spawn({
            let resolver = Arc::clone(&resolver);
            let cancel = cancel.clone();
            async move { resolver.resolve(&cancel, "google-vertex").await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();
        let started = std::time::Instant::now();
        let error = callers.join_next().await.unwrap().unwrap().unwrap_err();
        assert!(matches!(error, ProviderError::Cancelled));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "cancellation must be prompt, took {:?}",
            started.elapsed()
        );
        drop(seen);
        while callers.join_next().await.is_some() {}
        while workers.join_next().await.is_some() {}
        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn vertex_adc_cancelled_lock_waiter_releases_and_later_caller_resolves() {
        // Caller A drives the shared acquisition under the in-flight lock;
        // the fake holds the response until the test releases it, so the
        // lock is provably held when caller B waits on it and is cancelled:
        // B exits promptly, A's acquisition then completes and caches, and
        // caller C resolves from that cache with no second exchange. The
        // coordination uses barriers and yields, never wall-clock sleeps.
        let path = private_file("lock-authorized-user.json", AUTHORIZED_USER_JSON);
        let listener = TcpListener::bind("[::1]:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests: Arc<AsyncTestMutex<Vec<CapturedRequest>>> = Arc::default();
        let sink = Arc::clone(&requests);
        let (seen_tx, seen_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let mut server_workers = tokio::task::JoinSet::new();
        server_workers.spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            if let Some(request) = read_http_request(&mut stream).await {
                sink.lock().await.push(request);
            }
            let _ = seen_tx.send(());
            // The response stays held until the test releases it.
            let _ = release_rx.await;
            let body = r#"{"access_token":"adc-token","expires_in":3600}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len(),
            );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.shutdown().await;
        });
        let resolver = Arc::new(
            Resolver::new(gac_env(&path), reqwest::Client::new())
                .with_endpoints(format!("http://[::1]:{port}"), unused_endpoint("/metadata")),
        );
        let first_cancel = CancellationToken::new();
        let mut first = tokio::task::JoinSet::new();
        first.spawn({
            let resolver = Arc::clone(&resolver);
            async move { resolver.resolve(&first_cancel, "google-vertex").await }
        });
        // The captured request means A owns the in-flight lock with its
        // exchange parked on the held response.
        seen_rx.await.unwrap();
        let second_cancel = CancellationToken::new();
        let mut second = tokio::task::JoinSet::new();
        second.spawn({
            let resolver = Arc::clone(&resolver);
            let task_cancel = second_cancel.clone();
            async move { resolver.resolve(&task_cancel, "google-vertex").await }
        });
        // Yield, never sleep: B runs its prelude (no explicit token, no
        // cached token) and parks on the lock wait.
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        let started = std::time::Instant::now();
        second_cancel.cancel();
        let error = tokio::time::timeout(Duration::from_secs(2), second.join_next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(matches!(error, ProviderError::Cancelled));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "a cancelled lock waiter must exit promptly, took {:?}",
            started.elapsed()
        );
        // Only now does A's acquisition finish and cache the token.
        let _ = release_tx.send(());
        let token = tokio::time::timeout(Duration::from_secs(2), first.join_next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(&*token, "adc-token");
        let later = resolver
            .resolve(&CancellationToken::new(), "google-vertex")
            .await
            .unwrap();
        assert_eq!(&*later, "adc-token");
        assert_eq!(
            requests.lock().await.len(),
            1,
            "the later caller must reuse the completed acquisition"
        );
        while first.join_next().await.is_some() {}
        while second.join_next().await.is_some() {}
        while server_workers.join_next().await.is_some() {}
        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn vertex_adc_metadata_cancel_during_pending_reply() {
        let (mut workers, endpoint, _seen) = serve(vec![Reply {
            flavor: Flavor::Google,
            ..delayed(3600)
        }])
        .await;
        let resolver = Arc::new(
            Resolver::new(EnvSnapshot::test(&[]), reqwest::Client::new())
                .with_endpoints(unused_endpoint("/token"), format!("{endpoint}/metadata")),
        );
        let cancel = CancellationToken::new();
        let mut callers = tokio::task::JoinSet::new();
        callers.spawn({
            let resolver = Arc::clone(&resolver);
            let cancel = cancel.clone();
            async move { resolver.resolve(&cancel, "google-vertex").await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();
        let started = std::time::Instant::now();
        let error = callers.join_next().await.unwrap().unwrap().unwrap_err();
        assert!(matches!(error, ProviderError::Cancelled));
        assert!(started.elapsed() < Duration::from_secs(2));
        while callers.join_next().await.is_some() {}
        while workers.join_next().await.is_some() {}
    }

    #[tokio::test]
    async fn vertex_adc_exchange_401_and_429_are_typed_without_caching() {
        let path = private_file("status-authorized-user.json", AUTHORIZED_USER_JSON);
        let cancel = CancellationToken::new();
        // A rejected exchange is a typed status failure and never cached:
        // the next resolve attempts fresh, and nothing invalidates or
        // replays on its own.
        let (mut rejected_workers, rejected, rejected_seen) = serve(vec![Reply {
            status: 401,
            body: r#"{"error":"invalid_grant"}"#,
            ..token_reply(3600)
        }])
        .await;
        let resolver = Resolver::new(gac_env(&path), reqwest::Client::new())
            .with_endpoints(rejected, unused_endpoint("/metadata"));
        let error = resolver
            .resolve(&cancel, "google-vertex")
            .await
            .unwrap_err();
        assert!(
            matches!(&error, ProviderError::TokenExchange { status: 401, .. }),
            "got {error:?}"
        );
        assert_eq!(rejected_seen.lock().await.len(), 1, "no retry on 401");
        // A rate-limit answer stays a typed exchange failure: no acquisition
        // storm, exactly one request per caller.
        let (mut throttled_workers, throttled, throttled_seen) = serve(vec![Reply {
            status: 429,
            body: r#"{"error":"rate"}"#,
            ..token_reply(3600)
        }])
        .await;
        let resolver = Resolver::new(gac_env(&path), reqwest::Client::new())
            .with_endpoints(throttled, unused_endpoint("/metadata"));
        let error = resolver
            .resolve(&cancel, "google-vertex")
            .await
            .unwrap_err();
        assert!(
            matches!(&error, ProviderError::TokenExchange { status: 429, .. }),
            "got {error:?}"
        );
        assert_eq!(throttled_seen.lock().await.len(), 1, "no retry on 429");
        while rejected_workers.join_next().await.is_some() {}
        while throttled_workers.join_next().await.is_some() {}
        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn vertex_adc_file_errors_redact_contents() {
        let cancel = CancellationToken::new();
        // A malformed file that carries marker secrets: the typed error names
        // the path and class and never a file value.
        let markers = private_file(
            "redact.json",
            br#"{"type":"authorized_user","client_id":"MARKER-CLIENT-ID","client_secret":"MARKER-SECRET-VALUE""#,
        );
        let resolver = Resolver::new(gac_env(&markers), reqwest::Client::new())
            .with_endpoints(unused_endpoint("/token"), unused_endpoint("/metadata"));
        let error = resolver
            .resolve(&cancel, "google-vertex")
            .await
            .unwrap_err();
        let text = format!("{error}\n{error:?}");
        assert!(
            matches!(&error, ProviderError::AuthFileInvalid { .. }),
            "{error:?}"
        );
        assert!(!text.contains("MARKER-CLIENT-ID"));
        assert!(!text.contains("MARKER-SECRET-VALUE"));
        // A symlink is refused by class, never followed for contents.
        let target = private_file("redact-target.json", AUTHORIZED_USER_JSON);
        let link = temp_path("redact-link.json");
        let _ = fs::remove_file(&link);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&target, &link).unwrap();
        let resolver = Resolver::new(gac_env(&link), reqwest::Client::new())
            .with_endpoints(unused_endpoint("/token"), unused_endpoint("/metadata"));
        let error = resolver
            .resolve(&cancel, "google-vertex")
            .await
            .unwrap_err();
        let text = format!("{error}\n{error:?}");
        assert!(text.contains("symbolic link"), "got {error:?}");
        assert!(!text.contains("gac-secret"), "the target must not be read");
        // A broadly readable file is refused as not private.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let loose = private_file("redact-loose.json", AUTHORIZED_USER_JSON);
            fs::set_permissions(&loose, fs::Permissions::from_mode(0o644)).unwrap();
            let resolver = Resolver::new(gac_env(&loose), reqwest::Client::new())
                .with_endpoints(unused_endpoint("/token"), unused_endpoint("/metadata"));
            let error = resolver
                .resolve(&cancel, "google-vertex")
                .await
                .unwrap_err();
            let text = format!("{error}\n{error:?}");
            assert!(text.contains("not private"), "got {error:?}");
            assert!(!text.contains("gac-secret"));
            let _ = fs::remove_file(loose);
        }
        // An unsupported credential type is a typed failure, never a
        // fallback to metadata.
        let impersonated = private_file(
            "redact-impersonated.json",
            br#"{"type":"impersonated_service_account","source_credentials":"MARKER-DELEGATE"}"#,
        );
        let resolver = Resolver::new(gac_env(&impersonated), reqwest::Client::new())
            .with_endpoints(unused_endpoint("/token"), unused_endpoint("/metadata"));
        let error = resolver
            .resolve(&cancel, "google-vertex")
            .await
            .unwrap_err();
        let text = format!("{error}\n{error:?}");
        assert!(
            text.contains("unsupported ADC credential type"),
            "got {error:?}"
        );
        assert!(!text.contains("MARKER-DELEGATE"));
        // A service account with a broken PEM is refused before any network
        // use, and the key text never surfaces.
        let broken_key = private_file(
            "redact-key.json",
            br#"{"type":"service_account","client_email":"sa@test.invalid","private_key":"MARKER-PEM-BYTES"}"#,
        );
        let resolver = Resolver::new(gac_env(&broken_key), reqwest::Client::new())
            .with_endpoints(unused_endpoint("/token"), unused_endpoint("/metadata"));
        let error = resolver
            .resolve(&cancel, "google-vertex")
            .await
            .unwrap_err();
        let text = format!("{error}\n{error:?}");
        assert!(!text.contains("MARKER-PEM-BYTES"));
        assert!(text.contains("not a usable RSA PEM"), "got {error:?}");
        let _ = fs::remove_file(markers);
        let _ = fs::remove_file(target);
        let _ = fs::remove_file(link);
        let _ = fs::remove_file(impersonated);
        let _ = fs::remove_file(broken_key);
    }

    #[test]
    fn service_account_malformed_key_is_rejected_without_network() {
        let path = private_file(
            "malformed-service-account.json",
            br#"{"type":"service_account","client_email":"service@example.invalid","private_key":"not-a-pem"}"#,
        );
        let resolver = Resolver::new(gac_env(&path), reqwest::Client::new());
        let error = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(resolver.readiness(&CancellationToken::new()))
            .unwrap_err();
        assert!(
            matches!(&error, ProviderError::AuthFileInvalid { .. }),
            "{error:?}"
        );
        let _ = fs::remove_file(path);
    }
}
