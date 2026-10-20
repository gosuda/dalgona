//! The SSO tier: cached SSO tokens plus the portal federation exchange.
//!
//! Two profile shapes resolve here: legacy `sso_start_url` + `sso_region`
//! directly on the profile, and `sso_session` referencing an
//! `[sso-session …]` block that fills the gaps. The cached token comes from
//! `~/.aws/sso/cache/*.json`, matched on `startUrl` by scanning file
//! contents: no filename hash is needed because the session name only selects
//! the start URL the caller already resolved.
//!
//! Storage goes through the declared `dal-store` boundary: reads via
//! `open_private_file` (symlink and permission refused), refresh writeback
//! via `create_private_dir_all` plus atomic `write_atomic` at mode 0600. A
//! failed writeback is logged and ignored — the in-memory token still serves
//! this run. Token, secret, and response text never enter an error or a log
//! line.

use sonic_rs::JsonValueTrait;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use url::Url;

use super::super::profile::ProfileSection;
use super::super::profile::ProfileSet;
use super::{AwsCredential, ResolveCtx, SSO_TOKEN_REFRESH_SKEW_SECS, guard_step, now_unix};
use crate::ProviderError;

/// SSO OIDC token endpoint path under the regional OIDC base.
const OIDC_TOKEN_PATH: &str = "token";

/// Resolves SSO role credentials for one profile section. Returns `None`
/// when the profile names no usable token source (missing start URL or SSO
/// region); a named-but-unusable token raises with `aws sso login` guidance.
/// `profile` names the profile in errors; URLs and tokens never do.
pub(crate) async fn resolve_sso_credentials(
    section: &ProfileSection,
    set: &ProfileSet,
    ctx: &ResolveCtx<'_>,
    profile: &str,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<Option<AwsCredential>, ProviderError> {
    let mut start_url = section.get("sso_start_url").filter(|url| !url.is_empty());
    let mut sso_region = section
        .get("sso_region")
        .filter(|region| !region.is_empty());
    if let Some(session) = section
        .get("sso_session")
        .filter(|name| !name.is_empty())
        .and_then(|name| set.sso_session(name))
    {
        if start_url.is_none() {
            start_url = session.get("sso_start_url").filter(|url| !url.is_empty());
        }
        if sso_region.is_none() {
            sso_region = session
                .get("sso_region")
                .filter(|region| !region.is_empty());
        }
    }
    let (Some(start_url), Some(sso_region)) = (start_url, sso_region) else {
        return Ok(None);
    };
    let account_id = section.get("sso_account_id").filter(|id| !id.is_empty());
    let role_name = section.get("sso_role_name").filter(|role| !role.is_empty());
    let (Some(account_id), Some(role_name)) = (account_id, role_name) else {
        return Ok(None);
    };
    super::validate_region_label(sso_region, "SSO")?;
    let endpoints = SsoEndpoints::production(sso_region)?;
    federation_flow(
        ctx, profile, start_url, account_id, role_name, &endpoints, cancel, deadline,
    )
    .await
    .map(Some)
}

/// Scans the SSO cache over the async file boundary; cancellation and the
/// shared deadline bind the wait, with the same abandonment honesty as
/// every other file read (a blocked scan thread runs to completion).
async fn load_sso_cached_token_async(
    ctx: &ResolveCtx<'_>,
    start_url: &str,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<Option<(sonic_rs::Value, String)>, ProviderError> {
    let Some(home) = ctx.home_dir else {
        return Ok(None);
    };
    let dir = home.join(".aws").join("sso").join("cache");
    let start_url = start_url.to_owned();
    let outcome = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(super::cancelled()),
        () = tokio::time::sleep_until(deadline) => return Err(super::expired()),
        result = tokio::task::spawn_blocking(move || scan_sso_cache(&dir, &start_url)) => result,
    };
    outcome.map_err(|_| ProviderError::Transport {
        family: dal_core::Family::Bedrock,
        reason: String::from("unable to read the AWS SSO cache"),
    })?
}

/// The blocking SSO cache scan: directory entries in sorted order, each
/// `.json` file opened through the private-file boundary and content-matched
/// on `startUrl`. Unreadable or unparseable files skip; a missing directory
/// reads as absent.
///
/// Several files can carry one `startUrl`: the CLI writes one token per
/// legacy profile and one per `sso-session`, and a login under one leaves
/// the others behind. The match that expires last wins, so a stale leftover
/// never shadows the token the CLI keeps refreshed; a token that names no
/// parseable expiry ranks below any that does, and equal expiries keep the
/// earlier name.
fn scan_sso_cache(
    dir: &std::path::Path,
    start_url: &str,
) -> Result<Option<(sonic_rs::Value, String)>, ProviderError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(ProviderError::Transport {
                family: dal_core::Family::Bedrock,
                reason: String::from("unable to read the AWS SSO cache"),
            });
        }
    };
    let mut names: Vec<String> = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else { continue };
        if entry
            .path()
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            names.push(name);
        }
    }
    names.sort();
    let mut best: Option<(Option<i64>, sonic_rs::Value, String)> = None;
    for name in names {
        let path = dir.join(&name);
        let Ok(mut file) = dal_store::open_private_file(&path) else {
            continue;
        };
        let mut text = String::new();
        if std::io::Read::read_to_string(&mut file, &mut text).is_err() {
            continue;
        }
        let Ok(value) = sonic_rs::from_str::<sonic_rs::Value>(&text) else {
            continue;
        };
        if value.get("startUrl").and_then(JsonValueTrait::as_str) != Some(start_url) {
            continue;
        }
        let expires_at = value
            .get("expiresAt")
            .and_then(JsonValueTrait::as_str)
            .and_then(super::parse_expiration_seconds);
        if best.as_ref().is_none_or(|(held, ..)| expires_at > *held) {
            best = Some((expires_at, value, name));
        }
    }
    Ok(best.map(|(_, value, name)| (value, name)))
}

/// The SSO token-load, refresh, and federation flow against explicit
/// endpoints: production from the wrapper above, loopback from tests.
pub(crate) async fn federation_flow(
    ctx: &ResolveCtx<'_>,
    profile: &str,
    start_url: &str,
    account_id: &str,
    role_name: &str,
    endpoints: &SsoEndpoints,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<AwsCredential, ProviderError> {
    let cached = load_sso_cached_token_async(ctx, start_url, cancel, deadline).await?;
    let Some((mut token, file)) = cached else {
        return Err(ProviderError::InvalidRequest {
            message: format!(
                "AWS SSO token for profile '{profile}' not found in ~/.aws/sso/cache; run 'aws sso login' first"
            ),
        });
    };
    let mut access_token = token
        .get("accessToken")
        .and_then(JsonValueTrait::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_owned);
    if access_token.is_none() {
        return Err(ProviderError::InvalidRequest {
            message: format!(
                "AWS SSO token for profile '{profile}' not found in ~/.aws/sso/cache; run 'aws sso login' first"
            ),
        });
    }
    let expires_at = token
        .get("expiresAt")
        .and_then(JsonValueTrait::as_str)
        .and_then(super::parse_expiration_seconds);
    let now = now_unix();
    if expires_at.is_some_and(|expires| expires - SSO_TOKEN_REFRESH_SKEW_SECS <= now) {
        if let Some(refreshed) =
            refresh_sso_token(ctx, &mut token, &file, endpoints, cancel, deadline).await?
        {
            access_token = Some(refreshed);
        } else if expires_at.is_some_and(|expires| expires <= now) {
            return Err(ProviderError::InvalidRequest {
                message: format!(
                    "AWS SSO token for profile '{profile}' has expired; run 'aws sso login' to refresh"
                ),
            });
        }
    }
    let Some(access_token) = access_token else {
        return Err(ProviderError::InvalidRequest {
            message: format!(
                "AWS SSO token for profile '{profile}' has expired; run 'aws sso login' to refresh"
            ),
        });
    };
    federation_credentials(
        ctx,
        endpoints,
        account_id,
        role_name,
        &access_token,
        cancel,
        deadline,
    )
    .await
}

/// Exchanges a cached refresh grant for a fresh access token via SSO OIDC
/// `CreateToken`, the transparent refresh the AWS CLI performs. Returns
/// `None` when refresh is impossible (no grant material, an expired client
/// registration, a rejected exchange, or a tokenless answer) so the caller
/// falls back to the `aws sso login` remedy instead of surfacing a network
/// error. Unknown token members survive: the parsed value mutates in place
/// and serializes back whole.
async fn refresh_sso_token(
    ctx: &ResolveCtx<'_>,
    token: &mut sonic_rs::Value,
    file: &str,
    endpoints: &SsoEndpoints,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<Option<String>, ProviderError> {
    #[derive(serde::Serialize)]
    struct RefreshRequest<'a> {
        #[serde(rename = "clientId")]
        client_id: &'a str,
        #[serde(rename = "clientSecret")]
        client_secret: &'a str,
        #[serde(rename = "grantType")]
        grant_type: &'a str,
        #[serde(rename = "refreshToken")]
        refresh_token: &'a str,
    }

    #[derive(serde::Deserialize)]
    struct RefreshResponse {
        #[serde(rename = "accessToken")]
        access_token: Option<String>,
        #[serde(rename = "expiresIn")]
        expires_in: Option<i64>,
        #[serde(rename = "refreshToken")]
        refresh_token: Option<String>,
    }

    let material = refresh_material(token);
    let Some(material) = material else {
        return Ok(None);
    };

    if token
        .get("registrationExpiresAt")
        .and_then(JsonValueTrait::as_str)
        .and_then(super::parse_expiration_seconds)
        .is_some_and(|expires| expires <= now_unix())
    {
        return Ok(None);
    }
    let body = sonic_rs::to_string(&RefreshRequest {
        client_id: material.client_id,
        client_secret: material.client_secret,
        grant_type: "refresh_token",
        refresh_token: material.refresh_token,
    })
    .map_err(|_| ProviderError::Transport {
        family: dal_core::Family::Bedrock,
        reason: String::from("the AWS SSO token refresh request is invalid"),
    })?;
    let url = endpoints
        .oidc
        .join(OIDC_TOKEN_PATH)
        .map_err(|_| ProviderError::Transport {
            family: dal_core::Family::Bedrock,
            reason: String::from("the AWS SSO OIDC endpoint is invalid"),
        })?;
    // Cancellation and the deadline propagate; every transport or body
    // failure degrades to the login remedy.
    let answer = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(super::cancelled()),
        () = tokio::time::sleep_until(deadline) => return Err(super::expired()),
        result = refresh_exchange(ctx, url, body) => result,
    };
    let Ok((status, bytes)) = answer else {
        return Ok(None);
    };
    if !(200..300).contains(&status) {
        return Ok(None);
    }
    let response: RefreshResponse =
        sonic_rs::from_slice(&bytes).map_err(|_| ProviderError::Transport {
            family: dal_core::Family::Bedrock,
            reason: String::from("the AWS SSO token refresh response is invalid"),
        })?;
    let access_token = response.access_token.filter(|token| !token.is_empty());
    let Some(access_token) = access_token else {
        return Ok(None);
    };
    let expires_at = now_unix().saturating_add(response.expires_in.unwrap_or(0));
    persist_refreshed_token(
        ctx,
        token,
        file,
        &access_token,
        expires_at,
        response.refresh_token,
    );
    Ok(Some(access_token))
}

/// POSTs one OIDC refresh exchange and returns the status plus the capped
/// body. Any failure shape maps to a static transport error the caller
/// degrades to the login remedy.
async fn refresh_exchange(
    ctx: &ResolveCtx<'_>,
    url: Url,
    body: String,
) -> Result<(u16, Vec<u8>), ProviderError> {
    let response = ctx
        .client
        .post(url)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .map_err(|_| ProviderError::Transport {
            family: dal_core::Family::Bedrock,
            reason: String::from("the AWS SSO token refresh request failed"),
        })?;
    let status = response.status().as_u16();
    let bytes = crate::http::read_body(dal_core::Family::Bedrock, response).await?;
    Ok((status, bytes))
}

/// The refresh grant material of a cached token: all three members must be
/// present and non-empty.
fn refresh_material(token: &sonic_rs::Value) -> Option<RefreshMaterial<'_>> {
    Some(RefreshMaterial {
        refresh_token: token
            .get("refreshToken")
            .and_then(JsonValueTrait::as_str)
            .filter(|token| !token.is_empty())?,
        client_id: token
            .get("clientId")
            .and_then(JsonValueTrait::as_str)
            .filter(|id| !id.is_empty())?,
        client_secret: token
            .get("clientSecret")
            .and_then(JsonValueTrait::as_str)
            .filter(|secret| !secret.is_empty())?,
    })
}

struct RefreshMaterial<'a> {
    refresh_token: &'a str,
    client_id: &'a str,
    client_secret: &'a str,
}

/// Writes a refreshed token back to the cache file it came from: the parsed
/// value mutates in place (unknown members preserved), serializes whole, and
/// publishes atomically at mode 0600. A failed write logs the filename and
/// is ignored; the in-memory token still serves this run.
fn persist_refreshed_token(
    ctx: &ResolveCtx<'_>,
    token: &mut sonic_rs::Value,
    file: &str,
    access_token: &str,
    expires_at: i64,
    refresh_token: Option<String>,
) {
    use sonic_rs::JsonValueMutTrait;
    if let Some(object) = token.as_object_mut() {
        object.insert("accessToken", sonic_rs::Value::from(access_token));
        object.insert(
            "expiresAt",
            sonic_rs::Value::from(iso_z(expires_at).as_str()),
        );
        if let Some(refresh_token) = refresh_token.filter(|token| !token.is_empty()) {
            object.insert(
                "refreshToken",
                sonic_rs::Value::from(refresh_token.as_str()),
            );
        }
    }
    let Ok(bytes) = sonic_rs::to_string(&token) else {
        return;
    };
    let Some(home) = ctx.home_dir else {
        return;
    };
    let dir = home.join(".aws").join("sso").join("cache");
    if dal_store::create_private_dir_all(&dir).is_err() {
        tracing::debug!(
            file,
            "the AWS SSO cache directory is not writable; keeping the refreshed token in memory"
        );
        return;
    }
    if dal_store::write_atomic(
        &dir.join(file),
        bytes.as_bytes(),
        dal_store::FileMode::Mode0600,
    )
    .is_err()
    {
        tracing::debug!(
            file,
            "the AWS SSO token refresh was not persisted; keeping the refreshed token in memory"
        );
    }
}

/// Formats unix seconds as a whole-second UTC instant, the shape the AWS CLI
/// writes to `expiresAt`.
fn iso_z(secs: i64) -> String {
    let zoned = jiff::Timestamp::from_second(secs)
        .unwrap_or(jiff::Timestamp::UNIX_EPOCH)
        .to_zoned(jiff::tz::TimeZone::UTC);
    zoned.strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Calls the portal federation endpoint for one account and role. Non-2xx
/// statuses report the status alone; the strict `roleCredentials` envelope
/// requires all members, with a numeric millisecond `expiration`.
#[derive(serde::Deserialize)]
struct FederationResponse {
    #[serde(rename = "roleCredentials")]
    role_credentials: Option<RoleCredentials>,
}

#[derive(serde::Deserialize)]
struct RoleCredentials {
    #[serde(rename = "accessKeyId")]
    access_key_id: Option<String>,
    #[serde(rename = "secretAccessKey")]
    secret_access_key: Option<String>,
    #[serde(rename = "sessionToken")]
    session_token: Option<String>,
    #[serde(rename = "expiration")]
    expiration: Option<sonic_rs::Value>,
}

async fn federation_credentials(
    ctx: &ResolveCtx<'_>,
    endpoints: &SsoEndpoints,
    account_id: &str,
    role_name: &str,
    access_token: &str,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<AwsCredential, ProviderError> {
    let mut url = endpoints.portal.clone();
    url.path_segments_mut()
        .map_err(|()| ProviderError::Transport {
            family: dal_core::Family::Bedrock,
            reason: String::from("the AWS SSO portal endpoint is invalid"),
        })?
        .extend(["federation", "credentials"]);
    url.query_pairs_mut()
        .append_pair("account_id", account_id)
        .append_pair("role_name", role_name);
    let token = reqwest::header::HeaderValue::from_str(access_token).map_err(|_| {
        ProviderError::Transport {
            family: dal_core::Family::Bedrock,
            reason: String::from("the AWS SSO access token is not a header value"),
        }
    })?;
    let (status, bytes) = guard_step(cancel, deadline, async {
        let response = ctx
            .client
            .get(url)
            .header("x-amz-sso_bearer_token", token)
            .send()
            .await
            .map_err(|error| crate::http::from_reqwest(dal_core::Family::Bedrock, error))?;
        let status = response.status().as_u16();
        let bytes = crate::http::read_body(dal_core::Family::Bedrock, response).await?;
        Ok::<(u16, Vec<u8>), ProviderError>((status, bytes))
    })
    .await?;
    if !(200..300).contains(&status) {
        return Err(ProviderError::Transport {
            family: dal_core::Family::Bedrock,
            reason: format!("AWS SSO GetRoleCredentials failed: HTTP {status}"),
        });
    }
    let response: FederationResponse =
        sonic_rs::from_slice(&bytes).map_err(|_| ProviderError::Transport {
            family: dal_core::Family::Bedrock,
            reason: String::from("the AWS SSO GetRoleCredentials response is invalid"),
        })?;
    let role = response.role_credentials.filter(|role| {
        role.access_key_id
            .as_deref()
            .is_some_and(|key| !key.is_empty())
            && role
                .secret_access_key
                .as_deref()
                .is_some_and(|key| !key.is_empty())
            && role
                .session_token
                .as_deref()
                .is_some_and(|token| !token.is_empty())
            && role
                .expiration
                .as_ref()
                .and_then(JsonValueTrait::as_i64)
                .is_some()
    });
    let Some(role) = role else {
        return Err(ProviderError::Transport {
            family: dal_core::Family::Bedrock,
            reason: String::from(
                "the AWS SSO GetRoleCredentials response is missing roleCredentials",
            ),
        });
    };
    Ok(AwsCredential {
        access_key_id: role.access_key_id.unwrap_or_default().into(),
        secret_access_key: role.secret_access_key.unwrap_or_default().into(),
        session_token: role.session_token.map(Into::into),
        expires_at: role
            .expiration
            .as_ref()
            .and_then(JsonValueTrait::as_i64)
            .map(|millis| millis / 1000),
    })
}

/// The validated SSO endpoints of one resolution: the portal federation base
/// and the OIDC token base, both `https` with exact constructed hosts.
#[derive(Clone, Debug)]
pub(crate) struct SsoEndpoints {
    portal: Url,
    oidc: Url,
}

impl SsoEndpoints {
    /// The production endpoints for one SSO region.
    pub(crate) fn production(sso_region: &str) -> Result<Self, ProviderError> {
        let portal = Url::parse(&format!("https://portal.sso.{sso_region}.amazonaws.com/"))
            .map_err(|_| ProviderError::Transport {
                family: dal_core::Family::Bedrock,
                reason: String::from("the AWS SSO portal endpoint is invalid"),
            })?;
        let oidc =
            Url::parse(&format!("https://oidc.{sso_region}.amazonaws.com/")).map_err(|_| {
                ProviderError::Transport {
                    family: dal_core::Family::Bedrock,
                    reason: String::from("the AWS SSO OIDC endpoint is invalid"),
                }
            })?;
        let expected_portal = format!("portal.sso.{sso_region}.amazonaws.com");
        let expected_oidc = format!("oidc.{sso_region}.amazonaws.com");
        if portal.host_str() != Some(expected_portal.as_str())
            || oidc.host_str() != Some(expected_oidc.as_str())
        {
            return Err(ProviderError::Transport {
                family: dal_core::Family::Bedrock,
                reason: String::from("the AWS SSO endpoint is invalid"),
            });
        }
        Ok(Self { portal, oidc })
    }

    /// Loopback endpoints for tests: plain HTTP admitted only for literal
    /// loopback hosts, mirroring the STS test seam.
    #[cfg(test)]
    pub(crate) fn loopback(portal: &Url, oidc: &Url) -> Result<Self, ProviderError> {
        fn admitted(url: &Url) -> bool {
            url.scheme() == "http"
                && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"))
                && url.port().is_some()
        }
        if admitted(portal) && admitted(oidc) {
            return Ok(Self {
                portal: portal.clone(),
                oidc: oidc.clone(),
            });
        }
        Err(ProviderError::InvalidRequest {
            message: String::from("test SSO endpoints must use a literal loopback host"),
        })
    }
}
