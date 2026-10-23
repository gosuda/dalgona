//! Tier 1–3 of the `SigV4` chain: static environment keys, web identity /
//! OIDC federation, and the shared profile files.
//!
//! File discovery honors `AWS_SHARED_CREDENTIALS_FILE` and `AWS_CONFIG_FILE`
//! over `~/.aws/credentials` and `~/.aws/config` under the captured home
//! directory. Both texts parse into one merged set, config first, with
//! credentials keys winning the merge. Every error names the profile and
//! the failure class; values from files, helpers, and the wire never enter
//! an error message.

use std::path::{Path, PathBuf};

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use url::Url;

use super::super::sts::{StsOperation, decode_credentials, decode_error_code};
use super::{
    FILE_SESSION_CREDS_TTL_SECS, now_unix, parse_expiration_seconds, validate_region_label,
};
// NOTE: `super::super::profile` is `family::bedrock::profile`, the
// shared-file parser; it is not this module.
use super::super::process::{
    CredentialProcessRequest, is_windows_batch_script, run_credential_process,
    split_credential_process_argv,
};
use super::super::profile::{ProfileSection, ProfileSet};
use super::{AwsCredential, ResolveCtx, guard_step};
use crate::{EnvSnapshot, ProviderError};

/// STS `AssumeRole` request version: `2011-06-15`.
const STS_VERSION: &str = "2011-06-15";

/// `SigV4` service name of the STS exchange, scoped through
/// `SignedRequest::service`.
const STS_SERVICE: &str = "sts";

/// STS endpoint DNS suffix for the Beijing and Ningxia regions.
const CN_SUFFIX: &str = "amazonaws.com.cn";
/// STS endpoint DNS suffix everywhere else.
const STANDARD_SUFFIX: &str = "amazonaws.com";

/// The shared-credentials file: `AWS_SHARED_CREDENTIALS_FILE`, else
/// `~/.aws/credentials` when a home directory was captured.
pub(crate) fn credentials_path(env: &EnvSnapshot, home: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = env
        .get("AWS_SHARED_CREDENTIALS_FILE")
        .filter(|p| !p.is_empty())
    {
        return Some(join_cwd(env, Path::new(path)));
    }
    home.map(|home| home.join(".aws").join("credentials"))
}

/// The shared-config file: `AWS_CONFIG_FILE`, else `~/.aws/config` when a
/// home directory was captured.
pub(crate) fn config_path(env: &EnvSnapshot, home: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = env.get("AWS_CONFIG_FILE").filter(|p| !p.is_empty()) {
        return Some(join_cwd(env, Path::new(path)));
    }
    home.map(|home| home.join(".aws").join("config"))
}

/// Resolves a relative path against the edge-captured working directory;
/// absolute paths pass through unchanged. Every file the chain reads by a
/// configured name goes through this, never through the process directory.
pub(super) fn join_cwd(env: &EnvSnapshot, path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_owned();
    }
    match env.cwd() {
        Some(cwd) => cwd.join(path),
        None => path.to_owned(),
    }
}

/// Parses the two shared texts into one merged set: config first, then
/// credentials, later keys winning.
pub(crate) fn parse_set(
    config: &str,
    credentials: &str,
) -> Result<ProfileSet, super::super::profile::ProfileError> {
    ProfileSet::parse(config, credentials)
}

/// Tier 1: static environment credentials. A session token rides along when
/// set; without one the credential never expires.
pub(crate) fn read_env_credentials(env: &EnvSnapshot) -> Option<AwsCredential> {
    let access_key_id = env.get("AWS_ACCESS_KEY_ID").filter(|key| !key.is_empty())?;
    let secret_access_key = env
        .get("AWS_SECRET_ACCESS_KEY")
        .filter(|key| !key.is_empty())?;
    Some(AwsCredential {
        access_key_id: access_key_id.into(),
        secret_access_key: secret_access_key.into(),
        session_token: env
            .get("AWS_SESSION_TOKEN")
            .filter(|token| !token.is_empty())
            .map(Into::into),
        expires_at: None,
    })
}

/// Tier 2: web identity / OIDC federation from `AWS_WEB_IDENTITY_TOKEN_FILE`
/// plus `AWS_ROLE_ARN`. An absent pair yields `None`; a present pair that
/// fails raises.
pub(crate) async fn resolve_web_identity(
    ctx: &ResolveCtx<'_>,
    region: &str,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<Option<AwsCredential>, ProviderError> {
    let token_file = ctx
        .env
        .get("AWS_WEB_IDENTITY_TOKEN_FILE")
        .filter(|file| !file.is_empty());
    let role_arn = ctx.env.get("AWS_ROLE_ARN").filter(|arn| !arn.is_empty());
    let (Some(token_file), Some(role_arn)) = (token_file, role_arn) else {
        return Ok(None);
    };
    let session_name = ctx
        .env
        .get("AWS_ROLE_SESSION_NAME")
        .filter(|name| !name.is_empty());
    let endpoints = StsEndpoints::production(region)?;
    let token = read_token_file(ctx.env, token_file, cancel, deadline).await?;
    let credential = resolve_web_identity_at(
        ctx,
        &endpoints,
        role_arn,
        &token,
        session_name,
        cancel,
        deadline,
    )
    .await?;
    Ok(Some(credential))
}

/// The web-identity exchange against explicit endpoints: production from the
/// wrapper above, loopback from tests.
pub(crate) async fn resolve_web_identity_at(
    ctx: &ResolveCtx<'_>,
    endpoints: &StsEndpoints,
    role_arn: &str,
    token: &str,
    session_name: Option<&str>,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<AwsCredential, ProviderError> {
    assume_role_with_web_identity(
        ctx,
        endpoints,
        role_arn,
        token,
        session_name,
        cancel,
        deadline,
    )
    .await
}

/// Reads and trims a web-identity token file over the async file boundary;
/// a relative path resolves against the captured working directory.
/// Refusals and empty files are typed errors that name the file kind, never
/// its path or contents.
async fn read_token_file(
    env: &EnvSnapshot,
    path: &str,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<String, ProviderError> {
    let bytes = super::read_shared_bytes_async(join_cwd(env, Path::new(path)), cancel, deadline)
        .await?
        .ok_or_else(|| ProviderError::Transport {
            family: dal_core::Family::Bedrock,
            reason: String::from("unable to read the AWS web identity token file"),
        })?;
    let token = String::from_utf8_lossy(&bytes).trim().to_owned();
    if token.is_empty() {
        return Err(ProviderError::InvalidRequest {
            message: String::from("the AWS web identity token file is empty"),
        });
    }
    Ok(token)
}

/// Tier 3: the shared profile files. Returns `None` when the selected profile
/// holds no credential source; role chains recurse with cycle detection.
pub(crate) async fn resolve_profile_credentials(
    ctx: &ResolveCtx<'_>,
    profile: &str,
    load_shared_config: bool,
    region: &str,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<Option<AwsCredential>, ProviderError> {
    let credentials = read_profile_text(
        credentials_path(ctx.env, ctx.home_dir).as_ref(),
        cancel,
        deadline,
    )
    .await?;
    let config = if load_shared_config {
        read_profile_text(
            config_path(ctx.env, ctx.home_dir).as_ref(),
            cancel,
            deadline,
        )
        .await?
    } else {
        String::new()
    };
    if credentials.is_empty() && config.is_empty() {
        return Ok(None);
    }
    let set = parse_set(&config, &credentials).map_err(|error| ProviderError::InvalidRequest {
        message: format!("the AWS shared config is invalid: {error}"),
    })?;
    let endpoints = StsEndpoints::production(region)?;
    Box::pin(resolve_profile_credentials_at(
        ctx, &set, &endpoints, region, profile, cancel, deadline,
    ))
    .await
}

/// The profile dispatch against an explicit set and endpoints: production
/// from the wrapper above, a parsed set plus loopback endpoints from tests.
pub(crate) async fn resolve_profile_credentials_at(
    ctx: &ResolveCtx<'_>,
    set: &ProfileSet,
    endpoints: &StsEndpoints,
    region: &str,
    profile: &str,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<Option<AwsCredential>, ProviderError> {
    let mut seen = Vec::new();
    Box::pin(resolve_profile_chain(
        ctx, set, endpoints, region, profile, cancel, deadline, &mut seen,
    ))
    .await
}

/// Reads one shared profile text over the async file boundary: a missing
/// path or a failed read degrades to empty so a half-present setup falls
/// through to the next tier instead of failing a host that holds other
/// sources.
async fn read_profile_text(
    path: Option<&PathBuf>,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<String, ProviderError> {
    let Some(path) = path else {
        return Ok(String::new());
    };
    let bytes = super::read_shared_bytes_async(path.clone(), cancel, deadline).await?;
    Ok(bytes
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default())
}

/// One visited profile on a role chain, from the target down to the terminal
/// base. Frames borrow the set; the walk is synchronous, so no future ever
/// captures them.
enum ChainFrame<'a> {
    /// A `role_arn` profile, exchanged after its base resolves.
    Role {
        name: &'a str,
        section: &'a ProfileSection,
        base: RoleBase<'a>,
    },
    /// Static keys: terminal base.
    Static(&'a ProfileSection),
    /// SSO dispatch: terminal base.
    Sso {
        name: &'a str,
        section: &'a ProfileSection,
    },
    /// External process: terminal base.
    Process { name: &'a str, command: &'a str },
    /// No credential source: terminal, resolves to `None`.
    Missing,
}

/// How a role profile derives its base credentials.
enum RoleBase<'a> {
    /// Self-contained web-identity exchange.
    WebIdentity { token_file: &'a str },
    /// Another profile; the walk continues there.
    SourceProfile { name: &'a str },
    /// A `credential_source` directive.
    CredentialSource { name: &'a str },
}

/// Walks one role chain synchronously into frames, target first. Cycle
/// detection reuses the caller's visited-identity path; every refusal below
/// matches the former recursive wording exactly.
fn walk_chain<'a>(
    set: &'a ProfileSet,
    profile: &'a str,
    seen: &mut Vec<String>,
) -> Result<Vec<ChainFrame<'a>>, ProviderError> {
    let mut frames = Vec::new();
    let mut current = profile;
    loop {
        if seen.iter().any(|visited| visited == current) {
            return Err(ProviderError::InvalidRequest {
                message: format!("AWS profile role chain contains a cycle at '{current}'"),
            });
        }
        seen.push(current.to_owned());
        let Some(section) = set.profile(current) else {
            frames.push(ChainFrame::Missing);
            break;
        };
        if section.get("role_arn").is_some_and(|arn| !arn.is_empty()) {
            if let Some(token_file) = section
                .get("web_identity_token_file")
                .filter(|f| !f.is_empty())
            {
                frames.push(ChainFrame::Role {
                    name: current,
                    section,
                    base: RoleBase::WebIdentity { token_file },
                });
                break;
            }
            if section
                .get("mfa_serial")
                .is_some_and(|serial| !serial.is_empty())
            {
                return Err(ProviderError::InvalidRequest {
                    message: format!(
                        "AWS profile '{current}' requires MFA (mfa_serial), which is not supported for non-interactive credential resolution"
                    ),
                });
            }
            if let Some(source) = section.get("source_profile").filter(|s| !s.is_empty()) {
                frames.push(ChainFrame::Role {
                    name: current,
                    section,
                    base: RoleBase::SourceProfile { name: source },
                });
                current = source;
                continue;
            }
            if let Some(source) = section.get("credential_source").filter(|s| !s.is_empty()) {
                frames.push(ChainFrame::Role {
                    name: current,
                    section,
                    base: RoleBase::CredentialSource { name: source },
                });
                break;
            }
            return Err(ProviderError::InvalidRequest {
                message: format!(
                    "AWS profile '{current}' sets role_arn without source_profile, credential_source, or web_identity_token_file"
                ),
            });
        }
        if section
            .get("aws_access_key_id")
            .is_some_and(|key| !key.is_empty())
            && section
                .get("aws_secret_access_key")
                .is_some_and(|key| !key.is_empty())
        {
            frames.push(ChainFrame::Static(section));
            break;
        }
        if section
            .get("sso_account_id")
            .is_some_and(|id| !id.is_empty())
            && section
                .get("sso_role_name")
                .is_some_and(|role| !role.is_empty())
        {
            frames.push(ChainFrame::Sso {
                name: current,
                section,
            });
            break;
        }
        if let Some(command) = section
            .get("credential_process")
            .filter(|cmd| !cmd.is_empty())
        {
            frames.push(ChainFrame::Process {
                name: current,
                command,
            });
            break;
        }
        frames.push(ChainFrame::Missing);
        break;
    }
    Ok(frames)
}

/// Resolves one profile, following `role_arn` chains. `seen` holds the
/// visited profile identities; revisiting one is a cycle, refused before any
/// network call.
#[expect(
    clippy::too_many_lines,
    reason = "profile chain resolver tracks visited set, static keys, and role assumptions"
)]
async fn resolve_profile_chain(
    ctx: &ResolveCtx<'_>,
    set: &ProfileSet,
    endpoints: &StsEndpoints,
    region: &str,
    profile: &str,
    cancel: &CancellationToken,
    deadline: Instant,
    seen: &mut Vec<String>,
) -> Result<Option<AwsCredential>, ProviderError> {
    // Iterative descent: the walk above collects frames synchronously, then
    // the base resolves bottom-up with no recursion (async recursion would
    // need a boxed future per hop). Frames borrow the set, which outlives
    // every await below.
    let frames = walk_chain(set, profile, seen)?;
    let Some(bottom) = frames.last() else {
        unreachable!("the walk always yields at least one frame")
    };
    let base = match bottom {
        ChainFrame::Missing => None,
        ChainFrame::Static(section) => read_profile_static(section),
        ChainFrame::Sso { name, section } => {
            super::sso::resolve_sso_credentials(section, set, ctx, name, cancel, deadline).await?
        }
        ChainFrame::Process { name, command } => Some(
            Box::pin(read_credential_process(
                ctx, name, command, cancel, deadline,
            ))
            .await?,
        ),
        ChainFrame::Role {
            section,
            base: RoleBase::WebIdentity { token_file },
            ..
        } => {
            let token = read_token_file(ctx.env, token_file, cancel, deadline).await?;
            Some(
                assume_role_with_web_identity(
                    ctx,
                    endpoints,
                    section.get("role_arn").unwrap_or_default(),
                    &token,
                    section
                        .get("role_session_name")
                        .filter(|name| !name.is_empty()),
                    cancel,
                    deadline,
                )
                .await?,
            )
        }
        ChainFrame::Role {
            name,
            section,
            base: RoleBase::CredentialSource { name: source },
        } => {
            let Some(base) = resolve_credential_source(ctx, source, cancel, deadline).await? else {
                return Err(ProviderError::InvalidRequest {
                    message: format!(
                        "AWS profile '{name}' credential_source '{source}' produced no credentials"
                    ),
                });
            };
            Some(
                sts_assume_role(
                    ctx,
                    endpoints,
                    &base,
                    region,
                    section.get("role_arn").unwrap_or_default(),
                    section
                        .get("role_session_name")
                        .filter(|name| !name.is_empty()),
                    section
                        .get("duration_seconds")
                        .filter(|value| !value.is_empty()),
                    section.get("external_id").filter(|value| !value.is_empty()),
                    cancel,
                    deadline,
                )
                .await?,
            )
        }
        ChainFrame::Role {
            base: RoleBase::SourceProfile { .. },
            ..
        } => {
            unreachable!("the walk never ends on a source_profile frame")
        }
    };
    let mut base = match (base, frames.len()) {
        (Some(base), _) => base,
        (None, 1) => return Ok(None),
        (None, _) => match &frames[frames.len() - 2] {
            ChainFrame::Role {
                name: parent,
                base: RoleBase::SourceProfile { name: source },
                ..
            } => {
                return Err(ProviderError::InvalidRequest {
                    message: format!(
                        "AWS profile '{parent}' references source_profile '{source}', which has no usable credentials"
                    ),
                });
            }
            _ => unreachable!("only a source_profile frame can sit above an empty base"),
        },
    };
    for frame in frames.iter().rev().skip(1) {
        let ChainFrame::Role {
            section,
            base: RoleBase::SourceProfile { .. },
            ..
        } = frame
        else {
            unreachable!("the walk continues past source_profile frames only")
        };
        base = sts_assume_role(
            ctx,
            endpoints,
            &base,
            region,
            section.get("role_arn").unwrap_or_default(),
            section
                .get("role_session_name")
                .filter(|name| !name.is_empty()),
            section
                .get("duration_seconds")
                .filter(|value| !value.is_empty()),
            section.get("external_id").filter(|value| !value.is_empty()),
            cancel,
            deadline,
        )
        .await?;
    }
    Ok(Some(base))
}

/// Profile static keys. A session token caps the cache at five minutes so
/// in-place rotations are picked up.
fn read_profile_static(section: &ProfileSection) -> Option<AwsCredential> {
    let access_key_id = section
        .get("aws_access_key_id")
        .filter(|key| !key.is_empty())?;
    let secret_access_key = section
        .get("aws_secret_access_key")
        .filter(|key| !key.is_empty())?;
    let session_token = section
        .get("aws_session_token")
        .filter(|token| !token.is_empty());
    Some(AwsCredential {
        access_key_id: access_key_id.into(),
        secret_access_key: secret_access_key.into(),
        session_token: session_token.map(Into::into),
        expires_at: session_token.map(|_| now_unix() + FILE_SESSION_CREDS_TTL_SECS),
    })
}

/// The base credentials named by a `credential_source` directive. Unknown
/// directives are typed refusals; the EC2 source uses the same IMDS probe
/// as tier 5.
async fn resolve_credential_source(
    ctx: &ResolveCtx<'_>,
    source: &str,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<Option<AwsCredential>, ProviderError> {
    match source {
        "Environment" => Ok(read_env_credentials(ctx.env)),
        "Ec2InstanceMetadata" => {
            super::metadata::resolve_imds_credentials(ctx, cancel, deadline).await
        }
        "EcsContainer" => {
            super::metadata::resolve_container_credentials(ctx, cancel, deadline).await
        }
        _ => Err(ProviderError::InvalidRequest {
            message: format!("unsupported AWS credential_source '{source}'"),
        }),
    }
}

/// Builds one web-identity form body. The serializer scope ends here, so no
/// non-`Send` encoder state crosses an await.
fn web_identity_form(role_arn: &str, session: Option<&str>, token: &str) -> Vec<u8> {
    let mut form = url::form_urlencoded::Serializer::new(String::new());
    form.append_pair("Action", "AssumeRoleWithWebIdentity");
    form.append_pair("Version", STS_VERSION);
    form.append_pair("RoleArn", role_arn);
    form.append_pair("RoleSessionName", &session_name(session));
    form.append_pair("WebIdentityToken", token);
    form.finish().into_bytes()
}

/// Builds one `AssumeRole` form body; same scope discipline as above.
fn assume_role_form(
    role_arn: &str,
    session: Option<&str>,
    duration_seconds: Option<&str>,
    external_id: Option<&str>,
) -> Vec<u8> {
    let mut form = url::form_urlencoded::Serializer::new(String::new());
    form.append_pair("Action", "AssumeRole");
    form.append_pair("Version", STS_VERSION);
    form.append_pair("RoleArn", role_arn);
    form.append_pair("RoleSessionName", &session_name(session));
    if let Some(seconds) = duration_seconds {
        form.append_pair("DurationSeconds", seconds);
    }
    if let Some(id) = external_id {
        form.append_pair("ExternalId", id);
    }
    form.finish().into_bytes()
}

/// Exchanges a web-identity token for role credentials via STS
/// `AssumeRoleWithWebIdentity`: the env chain and `role_arn` plus
/// `web_identity_token_file` profiles share this exchange.
async fn assume_role_with_web_identity(
    ctx: &ResolveCtx<'_>,
    endpoints: &StsEndpoints,
    role_arn: &str,
    token: &str,
    session: Option<&str>,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<AwsCredential, ProviderError> {
    let body = web_identity_form(role_arn, session, token);
    let text = post_sts(
        ctx,
        endpoints,
        None,
        &body,
        "AssumeRoleWithWebIdentity",
        cancel,
        deadline,
    )
    .await?;
    decode_credentials(&text, StsOperation::AssumeRoleWithWebIdentity).map_err(|error| {
        ProviderError::Transport {
            family: dal_core::Family::Bedrock,
            reason: format!("the AWS web identity response is invalid: {error}"),
        }
    })
}

/// Exchanges base credentials for a target role via STS `AssumeRole`,
/// signed with the base credentials for the `sts` service.
async fn sts_assume_role(
    ctx: &ResolveCtx<'_>,
    endpoints: &StsEndpoints,
    base: &AwsCredential,
    region: &str,
    role_arn: &str,
    session: Option<&str>,
    duration_seconds: Option<&str>,
    external_id: Option<&str>,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<AwsCredential, ProviderError> {
    let body = assume_role_form(role_arn, session, duration_seconds, external_id);
    let text = post_sts(
        ctx,
        endpoints,
        Some((base, region)),
        &body,
        "AssumeRole",
        cancel,
        deadline,
    )
    .await?;
    decode_credentials(&text, StsOperation::AssumeRole).map_err(|error| ProviderError::Transport {
        family: dal_core::Family::Bedrock,
        reason: format!("the AWS AssumeRole response is invalid: {error}"),
    })
}

/// The default STS role session name: the configured name, else `omp-<pid>`.
fn session_name(configured: Option<&str>) -> String {
    configured.map_or_else(|| format!("omp-{}", std::process::id()), str::to_owned)
}

/// POSTs one unsigned or SigV4-signed STS form and returns the response text.
/// Non-2xx statuses map to the decoded STS error code (a static spelling),
/// never to body text; a non-UTF-8 body reports the status alone.
async fn post_sts(
    ctx: &ResolveCtx<'_>,
    endpoints: &StsEndpoints,
    signed_base: Option<(&AwsCredential, &str)>,
    body: &[u8],
    action: &'static str,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<String, ProviderError> {
    let response = guard_step(cancel, deadline, async {
        let mut request = ctx
            .client
            .post(endpoints.base.clone())
            .header("content-type", "application/x-www-form-urlencoded");
        if let Some((base, region)) = signed_base {
            request = sign_sts_request(request, base, region, &endpoints.host, body);
        }
        let response = request
            .body(body.to_owned())
            .send()
            .await
            .map_err(|error| crate::http::from_reqwest(dal_core::Family::Bedrock, error))?;
        let status = response.status().as_u16();
        let bytes = crate::http::read_body(dal_core::Family::Bedrock, response).await?;
        Ok::<(u16, Vec<u8>), ProviderError>((status, bytes))
    })
    .await?;
    if !(200..300).contains(&response.0) {
        return Err(sts_failure(action, response.0, &response.1));
    }
    String::from_utf8(response.1).map_err(|_| ProviderError::Transport {
        family: dal_core::Family::Bedrock,
        reason: format!("AWS STS {action} failed: HTTP {}", response.0),
    })
}

/// Maps a failed STS exchange to a redacted transport error: the HTTP status
/// plus the decoded STS error code spelling. Service message text, request
/// ids, and raw bodies never enter the error.
fn sts_failure(action: &'static str, status: u16, body: &[u8]) -> ProviderError {
    let code = std::str::from_utf8(body)
        .ok()
        .and_then(|text| decode_error_code(text).ok())
        .map_or_else(|| String::from("Unrecognized"), |code| format!("{code:?}"));
    ProviderError::Transport {
        family: dal_core::Family::Bedrock,
        reason: format!("AWS STS {action} failed: HTTP {status} ({code})"),
    }
}

/// Signs one STS POST with base credentials for the `sts` service. `host` is
/// the `host` header value the request travels with, port included, so the
/// signature covers what the transport sends. The family keeps one
/// canonical-request implementation: the same signer as the Converse
/// request, scoped to `sts`.
fn sign_sts_request(
    request: reqwest::RequestBuilder,
    base: &AwsCredential,
    region: &str,
    host: &str,
    body: &[u8],
) -> reqwest::RequestBuilder {
    let signed = super::super::sigv4::SignedRequest {
        method: "POST",
        host,
        path: "/",
        query: None,
        headers: &[(
            "content-type",
            String::from("application/x-www-form-urlencoded"),
        )],
        region,
        credential: base,
        date: super::super::AmzDate::from_unix(now_unix()),
        service: STS_SERVICE,
    }
    .sign(body);
    let mut request = request
        .header("x-amz-date", signed.amz_date)
        .header("x-amz-content-sha256", signed.content_sha256)
        .header("authorization", signed.authorization);
    if let Some(token) = signed.security_token {
        request = request.header("x-amz-security-token", token);
    }
    request
}

/// The validated STS endpoint of one resolution: `https://sts.{region}.…/`,
/// host-pinned after parsing so a hostile region spelling cannot redirect
/// the exchange.
#[derive(Clone, Debug)]
pub(crate) struct StsEndpoints {
    base: Url,
    host: String,
}

impl StsEndpoints {
    /// The production endpoint for `region`: China regions use the `.cn`
    /// suffix. The region spelling is validated before interpolation, and
    /// the built URL's host must equal the interpolated host exactly.
    pub(crate) fn production(region: &str) -> Result<Self, ProviderError> {
        validate_region_label(region, "STS")?;
        let suffix = if region.starts_with("cn-") {
            CN_SUFFIX
        } else {
            STANDARD_SUFFIX
        };
        let host = format!("sts.{region}.{suffix}");
        let base =
            Url::parse(&format!("https://{host}/")).map_err(|_| ProviderError::Transport {
                family: dal_core::Family::Bedrock,
                reason: String::from("the AWS STS endpoint is invalid"),
            })?;
        if base.host_str() != Some(host.as_str()) {
            return Err(ProviderError::Transport {
                family: dal_core::Family::Bedrock,
                reason: String::from("the AWS STS endpoint is invalid"),
            });
        }
        Ok(Self { base, host })
    }

    /// A loopback endpoint for tests. Plain HTTP is admitted only for a
    /// literal loopback host; anything else is refused without a request.
    #[cfg(test)]
    pub(crate) fn loopback(url: &Url) -> Result<Self, ProviderError> {
        let name = url.host_str().unwrap_or_default();
        let loopback = matches!(name, "localhost" | "127.0.0.1" | "::1");
        if let (true, "http", Some(port)) = (loopback, url.scheme(), url.port()) {
            let mut base = url.clone();
            base.set_path("/");
            // The `host` header of a non-default port carries the port.
            let host = format!("{name}:{port}");
            return Ok(Self { base, host });
        }
        Err(ProviderError::InvalidRequest {
            message: String::from("test STS endpoints must use a literal loopback host"),
        })
    }
}

/// The external-process tier: tokenizes `command` with the platform grammar,
/// refuses batch scripts (shell-required, unauthorized), runs the helper with
/// the captured environment pair slice and the edge-captured directory, and
/// parses the strict Version-1 envelope. Stdout and stderr text never enter
/// an error; the raw buffer drops after parsing.
async fn read_credential_process(
    ctx: &ResolveCtx<'_>,
    profile: &str,
    command: &str,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<AwsCredential, ProviderError> {
    let parts = split_credential_process_argv(command, profile)?;
    if parts.is_empty() {
        return Err(ProviderError::InvalidRequest {
            message: format!("AWS credential_process for profile '{profile}' is empty"),
        });
    }
    let program: &str = &parts[0];
    let args: &[Box<str>] = &parts[1..];
    if is_windows_batch_script(program) {
        return Err(ProviderError::InvalidRequest {
            message: format!(
                "AWS profile '{profile}' credential_process names a batch script; batch scripts require cmd.exe and no shell path is authorized"
            ),
        });
    }
    if (program.contains('/') || program.contains('\\')) && !Path::new(program).is_absolute() {
        return Err(ProviderError::InvalidRequest {
            message: format!(
                "AWS profile '{profile}' credential_process names a relative path; use an absolute path or a command found on PATH"
            ),
        });
    }
    let cwd = ctx.env.cwd().ok_or_else(|| ProviderError::InvalidRequest {
        message: format!(
            "AWS profile '{profile}' credential_process has no captured working directory"
        ),
    })?;
    // The helper inherits the full captured snapshot, borrowed once with no
    // env override; the runner applies exactly this slice over env_clear().
    // No ambient read, no narrowing, no values in errors or logs.
    let pairs: Vec<(&str, &str)> = ctx.env.iter().collect();
    let request = CredentialProcessRequest {
        program,
        args,
        cwd,
        env: &pairs,
        profile,
    };
    let stdout = Box::pin(run_credential_process(&request, cancel, deadline)).await?;
    parse_process_envelope(profile, &stdout)
}

/// The strict credential-process envelope: `Version` 1 with non-empty
/// `AccessKeyId` and `SecretAccessKey`. Unknown members are ignored so the
/// helper format stays forward-compatible. `Expiration` is optional: a
/// missing or unparseable one on a credential with a session token caps the
/// cache at [`FILE_SESSION_CREDS_TTL_SECS`], like a rotated key in a shared
/// file, and the credential still signs inside that window; without a token
/// it reads as a long-lived key.
fn parse_process_envelope(profile: &str, stdout: &[u8]) -> Result<AwsCredential, ProviderError> {
    #[derive(serde::Deserialize)]
    struct Envelope {
        #[serde(rename = "Version")]
        version: Option<sonic_rs::Value>,
        #[serde(rename = "AccessKeyId")]
        access_key_id: Option<String>,
        #[serde(rename = "SecretAccessKey")]
        secret_access_key: Option<String>,
        #[serde(rename = "SessionToken")]
        session_token: Option<String>,
        #[serde(rename = "Expiration")]
        expiration: Option<String>,
    }
    let envelope: Envelope =
        sonic_rs::from_slice(stdout).map_err(|_| ProviderError::Transport {
            family: dal_core::Family::Bedrock,
            reason: format!(
                "AWS credential_process for profile '{profile}' did not emit valid JSON"
            ),
        })?;
    if !version_is_one(envelope.version.as_ref()) {
        return Err(ProviderError::Transport {
            family: dal_core::Family::Bedrock,
            reason: format!(
                "AWS credential_process for profile '{profile}' returned invalid credential envelope (expected Version 1 with AccessKeyId and SecretAccessKey)"
            ),
        });
    }
    let access_key_id = envelope.access_key_id.filter(|key| !key.is_empty());
    let secret_access_key = envelope.secret_access_key.filter(|key| !key.is_empty());
    let (Some(access_key_id), Some(secret_access_key)) = (access_key_id, secret_access_key) else {
        return Err(ProviderError::Transport {
            family: dal_core::Family::Bedrock,
            reason: format!(
                "AWS credential_process for profile '{profile}' returned invalid credential envelope (expected Version 1 with AccessKeyId and SecretAccessKey)"
            ),
        });
    };
    let session_token = envelope.session_token.filter(|token| !token.is_empty());
    let expires_at = envelope
        .expiration
        .as_deref()
        .and_then(parse_expiration_seconds)
        .or_else(|| {
            session_token
                .is_some()
                .then(|| now_unix() + FILE_SESSION_CREDS_TTL_SECS)
        });
    Ok(AwsCredential {
        access_key_id: access_key_id.into(),
        secret_access_key: secret_access_key.into(),
        session_token: session_token.map(Into::into),
        expires_at,
    })
}

/// Whether the envelope `Version` member is exactly the number 1. Any other
/// shape, including a string `"1"`, is refused without echoing the value.
fn version_is_one(version: Option<&sonic_rs::Value>) -> bool {
    use sonic_rs::JsonValueTrait;
    version.and_then(JsonValueTrait::as_i64) == Some(1)
}

/// Whether `profile` terminates in a usable credential source: static keys,
/// SSO, `credential_process`, or a `role_arn` chain. MFA-gated roles read
/// unusable; `seen` guards `source_profile` cycles by visited identity.
pub(crate) fn profile_has_source(
    set: &ProfileSet,
    profile: &str,
    env: &EnvSnapshot,
    seen: &mut Vec<String>,
) -> bool {
    if seen.iter().any(|visited| visited == profile) {
        return false;
    }
    seen.push(profile.to_owned());
    let Some(section) = set.profile(profile) else {
        seen.pop();
        return false;
    };
    if section.get("role_arn").is_some_and(|arn| !arn.is_empty()) {
        let usable = role_source_usable(set, section, env, seen);
        seen.pop();
        return usable;
    }
    let usable = section
        .get("aws_access_key_id")
        .is_some_and(|key| !key.is_empty())
        && section
            .get("aws_secret_access_key")
            .is_some_and(|key| !key.is_empty())
        || section
            .get("credential_process")
            .is_some_and(|cmd| !cmd.is_empty())
        || sso_source_usable(set, section);
    seen.pop();
    usable
}

/// Whether a `role_arn` profile has a usable base: web identity material, a
/// non-MFA recursive source, or a satisfiable `credential_source`.
fn role_source_usable(
    set: &ProfileSet,
    section: &ProfileSection,
    env: &EnvSnapshot,
    seen: &mut Vec<String>,
) -> bool {
    if section
        .get("web_identity_token_file")
        .is_some_and(|file| !file.is_empty())
    {
        return true;
    }
    if section
        .get("mfa_serial")
        .is_some_and(|serial| !serial.is_empty())
    {
        return false;
    }
    if let Some(source) = section.get("source_profile").filter(|s| !s.is_empty()) {
        return profile_has_source(set, source, env, seen);
    }
    match section.get("credential_source").unwrap_or_default() {
        "Environment" => {
            env.get("AWS_ACCESS_KEY_ID")
                .is_some_and(|key| !key.is_empty())
                && env
                    .get("AWS_SECRET_ACCESS_KEY")
                    .is_some_and(|key| !key.is_empty())
        }
        "EcsContainer" => {
            env.get("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI")
                .is_some_and(|uri| !uri.is_empty())
                || env
                    .get("AWS_CONTAINER_CREDENTIALS_FULL_URI")
                    .is_some_and(|uri| !uri.is_empty())
        }
        "Ec2InstanceMetadata" => {
            env.get("AWS_EC2_METADATA_DISABLED")
                .map(str::to_lowercase)
                .as_deref()
                != Some("true")
        }
        _ => false,
    }
}

/// Whether an SSO profile names an account, a role, and a reachable token
/// source: legacy `sso_start_url`/`sso_region` on the profile, or an
/// `sso_session` block carrying both.
fn sso_source_usable(set: &ProfileSet, section: &ProfileSection) -> bool {
    if section.get("sso_account_id").is_none_or(str::is_empty)
        || section.get("sso_role_name").is_none_or(str::is_empty)
    {
        return false;
    }
    if section
        .get("sso_start_url")
        .is_some_and(|url| !url.is_empty())
        && section
            .get("sso_region")
            .is_some_and(|region| !region.is_empty())
    {
        return true;
    }
    let session = section
        .get("sso_session")
        .filter(|name| !name.is_empty())
        .and_then(|name| set.sso_session(name));
    session.is_some_and(|session| {
        session
            .get("sso_start_url")
            .is_some_and(|url| !url.is_empty())
            && session
                .get("sso_region")
                .is_some_and(|region| !region.is_empty())
    })
}
