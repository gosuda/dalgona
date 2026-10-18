//! Tiers 4–5 of the `SigV4` chain: ECS container credentials and EC2 `IMDSv2`.
//!
//! Endpoint trust:
//!
//! - The ECS relative URI is confined to `http://169.254.170.2` with an
//!   absolute path; backslashes and fragments are refused before URL parsing
//!   so the parser cannot normalize them away.
//! - A full ECS URI must be `https`, or plain HTTP to a local/metadata host
//!   (loopback, private, and link-local ranges, checked below).
//! - The IMDS endpoint defaults to the link-local address (IPv6 when
//!   `AWS_EC2_METADATA_SERVICE_ENDPOINT_MODE=ipv6`); an override follows the
//!   same scheme rule as the full ECS URI, so plain HTTP to a non-local
//!   host is refused without a connection attempt.
//! - Redirects ride the shared client's same-origin policy (a hop off the
//!   request origin never carries the `authorization`, metadata-token, or
//!   SSO headers). STS and SSO endpoints are always `https` with an exact
//!   constructed host, so no redirect can leave AWS.
//! - Loopback HTTP serves the test servers; every other plain-HTTP host is
//!   refused without a connection attempt.

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use url::Url;

use super::{AwsCredential, IMDS_PROBE_TIMEOUT, ResolveCtx, guard_step, parse_expiration_seconds};
use crate::{EnvSnapshot, ProviderError};

/// ECS task credentials base: the relative URI resolves against this origin.
const ECS_BASE: &str = "http://169.254.170.2/";
/// The exact host a relative ECS URI is confined to.
const ECS_HOST: &str = "169.254.170.2";

/// Default `IMDSv2` base over IPv4.
const IMDS_IPV4_BASE: &str = "http://169.254.169.254/";
/// Default `IMDSv2` base when endpoint mode selects IPv6.
const IMDS_IPV6_BASE: &str = "http://[fd00:ec2::254]/";

/// IMDS token TTL requested on the PUT: 21600 seconds.
const IMDS_TOKEN_TTL_SECONDS: &str = "21600";

/// The IMDS availability predicate, used ONLY by `has_credential_source`:
/// the tier is unavailable when `AWS_EC2_METADATA_DISABLED=true`, available
/// when an endpoint override is set or the DMI/sysfs probe confirms an EC2
/// host. Resolution itself probes whenever it is not disabled (see below).
pub(crate) fn imds_source_present(env: &EnvSnapshot) -> bool {
    if env
        .get("AWS_EC2_METADATA_DISABLED")
        .map(str::to_lowercase)
        .as_deref()
        == Some("true")
    {
        return false;
    }
    if env
        .get("AWS_EC2_METADATA_SERVICE_ENDPOINT")
        .is_some_and(|endpoint| !endpoint.is_empty())
    {
        return true;
    }
    ec2_host()
}

/// The DMI/hypervisor EC2 host probe: Xen instances tag hypervisor/DMI
/// UUIDs with an `ec2` prefix; Nitro instances expose the instance id in
/// `board_asset_tag` (`i-…`) and `Amazon EC2` in the vendor fields. Missing
/// or unreadable files are inconclusive, never fatal; values match
/// lowercased.
pub(crate) fn ec2_host() -> bool {
    type HostCheck = (&'static str, fn(&str) -> bool);
    const CHECKS: [HostCheck; 5] = [
        ("/sys/hypervisor/uuid", |value| value.starts_with("ec2")),
        ("/sys/devices/virtual/dmi/id/product_uuid", |value| {
            value.starts_with("ec2")
        }),
        ("/sys/devices/virtual/dmi/id/board_asset_tag", |value| {
            value.starts_with("ec2") || value.starts_with("i-")
        }),
        ("/sys/devices/virtual/dmi/id/sys_vendor", |value| {
            value.contains("amazon ec2")
        }),
        ("/sys/devices/virtual/dmi/id/bios_vendor", |value| {
            value.contains("amazon ec2")
        }),
    ];
    CHECKS.iter().any(|(path, matches)| {
        std::fs::read_to_string(path).is_ok_and(|value| matches(&value.trim().to_lowercase()))
    })
}

/// Whether `host` is local or cloud-metadata: `localhost` names, IPv4
/// loopback/unspecified, RFC 1918 private, and link-local ranges (covering
/// IMDS and ECS), plus IPv6 loopback, unspecified, link-local, and
/// unique-local (covering the IPv6 IMDS address). Names that merely resolve
/// to loopback are NOT accepted; no DNS is consulted.
pub(crate) fn is_local_or_metadata_host(host: &str) -> bool {
    let lower = host.to_lowercase();
    if lower == "localhost" || lower.ends_with(".localhost") || lower == "metadata.google.internal"
    {
        return true;
    }
    let ip = lower
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(&lower);
    if let Some((a, b)) = ipv4_first_two(ip) {
        if a == 127 || a == 10 || a == 0 {
            return true;
        }
        if a == 169 && b == 254 {
            return true;
        }
        if a == 192 && b == 168 {
            return true;
        }
        if a == 172 && (16..=31).contains(&b) {
            return true;
        }
        return false;
    }
    if ip == "::1" || ip == "::" {
        return true;
    }
    let bytes: Vec<&str> = ip.split(':').collect();
    if let Some(first) = bytes.first() {
        let group = first.to_lowercase();
        if group.len() == 4
            && group.starts_with("fe")
            && matches!(&group[2..3], "8" | "9" | "a" | "b")
        {
            return true;
        }
        if group.len() == 4 && (group.starts_with('f') && matches!(&group[1..2], "c" | "d")) {
            return true;
        }
    }
    false
}

/// The first two octets of a dotted-quad IPv4 literal, or `None` when `ip`
/// is not one. Single-digit groups are not required: one to three digits
/// each, exactly four groups.
fn ipv4_first_two(ip: &str) -> Option<(u16, u16)> {
    let mut groups = ip.split('.');
    let first = groups.next()?;
    let second = groups.next()?;
    if groups.count() != 2 {
        return None;
    }
    let parse = |group: &str| {
        (!group.is_empty() && group.len() <= 3 && group.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| group.parse::<u16>().ok())?
    };
    Some((parse(first)?, parse(second)?))
}

/// Tier 4: ECS / container credentials. Returns `None` when neither URI
/// variable is set. A set URI that fails parses, admission, transport, or
/// the required response keys raises; only the transport kind travels, never
/// body text or the token.
pub(crate) async fn resolve_container_credentials(
    ctx: &ResolveCtx<'_>,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<Option<AwsCredential>, ProviderError> {
    let relative = ctx
        .env
        .get("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI")
        .filter(|uri| !uri.is_empty());
    let full = ctx
        .env
        .get("AWS_CONTAINER_CREDENTIALS_FULL_URI")
        .filter(|uri| !uri.is_empty());
    let endpoint = match (relative, full) {
        (None, None) => return Ok(None),
        (Some(relative), _) => ecs_relative_endpoint(relative)?,
        (None, Some(full)) => ecs_full_endpoint(full)?,
    };
    let authorization = container_authorization(ctx, cancel, deadline).await?;
    let mut request = ctx.client.get(endpoint);
    if let Some(token) = authorization {
        let value = reqwest::header::HeaderValue::from_str(&token).map_err(|_| {
            transport_error("the AWS container authorization token is not a header value")
        })?;
        request = request.header("authorization", value);
    }
    let (status, bytes) = send(request, cancel, deadline).await?;
    if !(200..300).contains(&status) {
        return Err(transport_error(&format!(
            "the AWS container credential endpoint failed: HTTP {status}"
        )));
    }
    let response: ContainerResponse = sonic_rs::from_slice(&bytes)
        .map_err(|_| transport_error("the AWS container credential response is not valid JSON"))?;
    let credential = response.credential().ok_or_else(|| {
        transport_error("the AWS container credential response is missing AccessKeyId/SecretAccessKey/Token/Expiration")
    })?;
    Ok(Some(credential))
}

/// Resolves the ECS relative URI against the fixed link-local base.
pub(crate) fn ecs_relative_endpoint(relative: &str) -> Result<Url, ProviderError> {
    if !relative.starts_with('/') || relative.starts_with("//") {
        return Err(ProviderError::InvalidRequest {
            message: String::from(
                "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI must be a single-host absolute path",
            ),
        });
    }
    if relative.contains('\\') || relative.contains('#') {
        return Err(ProviderError::InvalidRequest {
            message: String::from(
                "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI must be a single-host absolute path",
            ),
        });
    }
    let url =
        Url::parse(&format!("{ECS_BASE}{}", relative.trim_start_matches('/'))).map_err(|_| {
            ProviderError::InvalidRequest {
                message: String::from(
                    "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI must be a single-host absolute path",
                ),
            }
        })?;
    if url.scheme() != "http" || url.host_str() != Some(ECS_HOST) {
        return Err(ProviderError::InvalidRequest {
            message: String::from(
                "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI must be a single-host absolute path",
            ),
        });
    }
    Ok(url)
}

/// Admits a full ECS URI: `https` anywhere, else plain HTTP to a
/// local/metadata host. The URI text never enters an error.
pub(crate) fn ecs_full_endpoint(full: &str) -> Result<Url, ProviderError> {
    let url = Url::parse(full).map_err(|_| ProviderError::InvalidRequest {
        message: String::from("AWS_CONTAINER_CREDENTIALS_FULL_URI is invalid"),
    })?;
    match url.scheme() {
        "https" if url.host().is_some() => Ok(url),
        "http" if url.host_str().is_some_and(is_local_or_metadata_host) => Ok(url),
        _ => Err(ProviderError::InvalidRequest {
            message: String::from(
                "AWS_CONTAINER_CREDENTIALS_FULL_URI must use HTTPS or a local metadata host",
            ),
        }),
    }
}

/// The ECS `authorization` header value: the token variable, else the
/// trimmed token file read over the async file boundary. Empty reads as
/// absent; the token text never enters an error.
async fn container_authorization(
    ctx: &ResolveCtx<'_>,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<Option<String>, ProviderError> {
    if let Some(token) = ctx
        .env
        .get("AWS_CONTAINER_AUTHORIZATION_TOKEN")
        .filter(|token| !token.is_empty())
    {
        return Ok(Some(token.to_owned()));
    }
    let file = ctx
        .env
        .get("AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE")
        .filter(|file| !file.is_empty());
    let Some(file) = file else {
        return Ok(None);
    };
    let path = super::profile::join_cwd(ctx.env, std::path::Path::new(file));
    let bytes = super::read_shared_bytes_async(path, cancel, deadline)
        .await?
        .ok_or_else(|| {
            transport_error("unable to read the AWS container authorization token file")
        })?;
    Ok(Some(String::from_utf8_lossy(&bytes).trim().to_owned()).filter(|token| !token.is_empty()))
}

/// The ECS credential JSON: all four members required, `Expiration` parsed,
/// unknown members ignored for forward compatibility.
#[derive(serde::Deserialize)]
struct ContainerResponse {
    #[serde(rename = "AccessKeyId")]
    access_key_id: Option<String>,
    #[serde(rename = "SecretAccessKey")]
    secret_access_key: Option<String>,
    #[serde(rename = "Token")]
    token: Option<String>,
    #[serde(rename = "Expiration")]
    expiration: Option<String>,
}

impl ContainerResponse {
    fn credential(self) -> Option<AwsCredential> {
        let access_key_id = self.access_key_id.filter(|key| !key.is_empty())?;
        let secret_access_key = self.secret_access_key.filter(|key| !key.is_empty())?;
        let token = self.token.filter(|token| !token.is_empty())?;
        let expires_at = self
            .expiration
            .as_deref()
            .and_then(parse_expiration_seconds)?;
        Some(AwsCredential {
            access_key_id: access_key_id.into(),
            secret_access_key: secret_access_key.into(),
            session_token: Some(token.into()),
            expires_at: Some(expires_at),
        })
    }
}

/// Tier 5: EC2 `IMDSv2`, probed whenever it is not disabled:
/// `AWS_EC2_METADATA_DISABLED=true` skips, anything else probes with
/// one-second per-request timers. Every probe failure soft-fails to `None`:
/// a host without metadata simply has no tier-5 credentials. Cancellation
/// and the shared deadline still raise.
pub(crate) async fn resolve_imds_credentials(
    ctx: &ResolveCtx<'_>,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<Option<AwsCredential>, ProviderError> {
    if ctx
        .env
        .get("AWS_EC2_METADATA_DISABLED")
        .map(str::to_lowercase)
        .as_deref()
        == Some("true")
    {
        return Ok(None);
    }
    let base = imds_base_url(ctx.env)?;
    let token = imds_probe(
        ctx.client.put(
            base.join("latest/api/token")
                .map_err(|_| transport_error("the AWS IMDS endpoint is invalid"))?,
        ),
        |request| {
            request.header(
                "x-aws-ec2-metadata-token-ttl-seconds",
                IMDS_TOKEN_TTL_SECONDS,
            )
        },
        cancel,
        deadline,
    )
    .await?;
    let token = match token {
        Some((status, bytes)) if (200..300).contains(&status) => {
            let token = String::from_utf8(bytes).unwrap_or_default();
            let token = token.trim().to_owned();
            (!token.is_empty()).then_some(token)
        }
        _ => None,
    };
    let Some(token) = token else {
        return Ok(None);
    };
    let value = reqwest::header::HeaderValue::from_str(&token).ok();
    let Some(token_header) = value else {
        return Ok(None);
    };
    let role = imds_probe(
        ctx.client.get(
            base.join("latest/meta-data/iam/security-credentials/")
                .map_err(|_| transport_error("the AWS IMDS endpoint is invalid"))?,
        ),
        |request| request.header("x-aws-ec2-metadata-token", token_header.clone()),
        cancel,
        deadline,
    )
    .await?;
    let role = match role {
        Some((status, bytes)) if (200..300).contains(&status) => {
            let role = String::from_utf8(bytes).unwrap_or_default();
            let role = role.trim().to_owned();
            (!role.is_empty()).then_some(role)
        }
        _ => None,
    };
    let Some(role) = role else {
        return Ok(None);
    };
    let mut url = base;
    url.path_segments_mut()
        .map_err(|()| transport_error("the AWS IMDS endpoint is invalid"))?
        .extend(["latest", "meta-data", "iam", "security-credentials", &role]);
    let credential = imds_probe(
        ctx.client.get(url),
        |request| request.header("x-aws-ec2-metadata-token", token_header.clone()),
        cancel,
        deadline,
    )
    .await?;
    let body = match credential {
        Some((status, bytes)) if (200..300).contains(&status) => bytes,
        _ => return Ok(None),
    };
    Ok(imds_credential(&body))
}

/// One IMDS probe request under the per-request probe timer. `Ok(None)` on
/// timer expiry or transport failure; cancellation and the shared deadline
/// raise through [`super::cancelled`] and [`super::expired`].
async fn imds_probe(
    request: reqwest::RequestBuilder,
    headers: impl FnOnce(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<Option<(u16, Vec<u8>)>, ProviderError> {
    let work = async {
        let request = headers(request);
        let response = request
            .send()
            .await
            .map_err(|error| crate::http::from_reqwest(dal_core::Family::Bedrock, error))?;
        let status = response.status().as_u16();
        let bytes = crate::http::read_body(dal_core::Family::Bedrock, response).await?;
        Ok::<(u16, Vec<u8>), ProviderError>((status, bytes))
    };
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(super::cancelled()),
        () = tokio::time::sleep_until(deadline) => Err(super::expired()),
        result = tokio::time::timeout(IMDS_PROBE_TIMEOUT, work) => match result {
            Ok(Ok(outcome)) => Ok(Some(outcome)),
            Ok(Err(_)) | Err(_) => Ok(None),
        },
    }
}

/// The IMDS credential JSON: all four members required and `Expiration`
/// parsed, else the probe reads as absent.
fn imds_credential(body: &[u8]) -> Option<AwsCredential> {
    #[derive(serde::Deserialize)]
    struct ImdsResponse {
        #[serde(rename = "AccessKeyId")]
        access_key_id: Option<String>,
        #[serde(rename = "SecretAccessKey")]
        secret_access_key: Option<String>,
        #[serde(rename = "Token")]
        token: Option<String>,
        #[serde(rename = "Expiration")]
        expiration: Option<String>,
    }
    let response: ImdsResponse = sonic_rs::from_slice(body).ok()?;
    Some(AwsCredential {
        access_key_id: response.access_key_id.filter(|key| !key.is_empty())?.into(),
        secret_access_key: response
            .secret_access_key
            .filter(|key| !key.is_empty())?
            .into(),
        session_token: Some(response.token.filter(|token| !token.is_empty())?.into()),
        expires_at: Some(
            response
                .expiration
                .as_deref()
                .and_then(parse_expiration_seconds)?,
        ),
    })
}

/// The IMDS base: the override, else IPv6 or IPv4 link-local per endpoint
/// mode. The override follows the full-URI scheme rule; its text never
/// enters an error.
pub(crate) fn imds_base_url(env: &EnvSnapshot) -> Result<Url, ProviderError> {
    if let Some(endpoint) = env
        .get("AWS_EC2_METADATA_SERVICE_ENDPOINT")
        .filter(|endpoint| !endpoint.is_empty())
    {
        let mut url = Url::parse(endpoint).map_err(|_| ProviderError::InvalidRequest {
            message: String::from(
                "AWS_EC2_METADATA_SERVICE_ENDPOINT must use HTTPS or a local metadata host",
            ),
        })?;
        match url.scheme() {
            "https" if url.host().is_some() => {}
            "http" if url.host_str().is_some_and(is_local_or_metadata_host) => {}
            _ => {
                return Err(ProviderError::InvalidRequest {
                    message: String::from(
                        "AWS_EC2_METADATA_SERVICE_ENDPOINT must use HTTPS or a local metadata host",
                    ),
                });
            }
        }
        if !url.path().ends_with('/') {
            url.set_path(&format!("{}/", url.path()));
        }
        return Ok(url);
    }
    let ipv6 = env
        .get("AWS_EC2_METADATA_SERVICE_ENDPOINT_MODE")
        .map(str::to_lowercase)
        .as_deref()
        == Some("ipv6");
    Url::parse(if ipv6 { IMDS_IPV6_BASE } else { IMDS_IPV4_BASE })
        .map_err(|_| transport_error("the AWS IMDS endpoint is invalid"))
}

/// Sends one tier request under the shared guard and returns the status plus
/// the capped body.
async fn send(
    request: reqwest::RequestBuilder,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<(u16, Vec<u8>), ProviderError> {
    guard_step(cancel, deadline, async {
        let response = request
            .send()
            .await
            .map_err(|error| crate::http::from_reqwest(dal_core::Family::Bedrock, error))?;
        let status = response.status().as_u16();
        let bytes = crate::http::read_body(dal_core::Family::Bedrock, response).await?;
        Ok::<(u16, Vec<u8>), ProviderError>((status, bytes))
    })
    .await
}

/// A transport error with a static reason: no URL, body, token, or header
/// value travels.
fn transport_error(reason: &str) -> ProviderError {
    ProviderError::Transport {
        family: dal_core::Family::Bedrock,
        reason: String::from(reason),
    }
}
