// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Protected-resource discovery, native OAuth authorization, and token refresh.

use std::fmt::Write as _;
use std::io;
use std::{
    future::Future,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    time::Duration,
};

use dal_agent::ext::{Caller, Services};
use dal_core::{Answer, Question};
use reqwest::{
    Client, Response, StatusCode, Url,
    dns::{Addrs, Name, Resolve, Resolving},
    header::HeaderMap,
};
use serde::Serialize;
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tokio_util::sync::CancellationToken;

use crate::mcp::{
    McpError,
    http::auth::{self, TokenRecord},
};

const OAUTH_BODY_MAX: usize = 64 * 1024;
const CALLBACK_REQUEST_MAX: usize = 8192;
const CALLBACK_PATH: &str = "/callback";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AddressClass {
    Global,
    Loopback,
    Private,
    LinkLocal,
    Other,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct NetworkPolicy {
    target_class: AddressClass,
}

impl NetworkPolicy {
    pub(crate) fn for_target(target: &Url) -> Self {
        let target_class = target
            .host_str()
            .map(|host| {
                host.strip_prefix('[')
                    .and_then(|host| host.strip_suffix(']'))
                    .unwrap_or(host)
            })
            .and_then(|host| host.parse::<IpAddr>().ok())
            .map(classify_address)
            .or_else(|| {
                target
                    .host_str()
                    .filter(|host| host.eq_ignore_ascii_case("localhost"))
                    .map(|_| AddressClass::Loopback)
            })
            .unwrap_or(AddressClass::Global);
        Self { target_class }
    }

    fn allows(self, address: IpAddr) -> bool {
        let class = classify_address(address);
        class == AddressClass::Global || class == self.target_class
    }

    pub(crate) fn check_url(self, url: &Url) -> Result<(), McpError> {
        let Some(host) = url.host_str() else {
            return Err(auth_error("OAuth endpoint has no host"));
        };
        let host = host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(host);
        if let Ok(address) = host.parse::<IpAddr>() {
            if !self.allows(address) {
                return Err(auth_error(
                    "OAuth endpoint targets a disallowed network address",
                ));
            }
        } else if host.eq_ignore_ascii_case("localhost")
            && !self.allows(IpAddr::V4(Ipv4Addr::LOCALHOST))
        {
            return Err(auth_error(
                "OAuth endpoint targets a disallowed network address",
            ));
        }
        Ok(())
    }

    pub(crate) fn resolver(self) -> GuardedResolver {
        GuardedResolver { policy: self }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct OAuthClient {
    pub(crate) client: Client,
    pub(crate) policy: NetworkPolicy,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct GuardedResolver {
    policy: NetworkPolicy,
}

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let policy = self.policy;
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let addresses = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)?
                .collect::<Vec<_>>();
            if addresses.is_empty() || addresses.iter().any(|address| !policy.allows(address.ip()))
            {
                return Err(Box::new(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "OAuth endpoint resolved to a disallowed network address",
                ))
                    as Box<dyn std::error::Error + Send + Sync>);
            }
            Ok(Box::new(addresses.into_iter()) as Addrs)
        })
    }
}

fn classify_address(address: IpAddr) -> AddressClass {
    match address {
        IpAddr::V4(address) => classify_ipv4(address),
        IpAddr::V6(address) => classify_ipv6(address),
    }
}

fn classify_ipv4(address: Ipv4Addr) -> AddressClass {
    let octets = address.octets();
    if address.is_loopback() {
        AddressClass::Loopback
    } else if octets[0] == 169 && octets[1] == 254 {
        AddressClass::LinkLocal
    } else if address.is_private() || (octets[0] == 100 && (64..=127).contains(&octets[1])) {
        AddressClass::Private
    } else if address.is_unspecified()
        || address.is_multicast()
        || octets[0] >= 224
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
        || (octets[0] == 198 && octets[1] == 18)
        || (octets[0] == 198 && octets[1] == 19)
        || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
        || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113)
    {
        AddressClass::Other
    } else {
        AddressClass::Global
    }
}

fn classify_ipv6(address: Ipv6Addr) -> AddressClass {
    if address.is_loopback() {
        return AddressClass::Loopback;
    }
    if let Some(mapped) = address.to_ipv4_mapped() {
        return classify_ipv4(mapped);
    }
    let segments = address.segments();
    if segments[0] & 0xffc0 == 0xfe80 {
        AddressClass::LinkLocal
    } else if segments[0] & 0xfe00 == 0xfc00 {
        AddressClass::Private
    } else if address.is_unspecified()
        || (segments[0] == 0
            && segments[1] == 0
            && segments[2] == 0
            && segments[3] == 0
            && segments[4] == 0)
        || address.is_multicast()
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
    {
        AddressClass::Other
    } else {
        AddressClass::Global
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Challenge {
    pub(crate) resource_metadata: Option<String>,
    pub(crate) scope: Option<String>,
    pub(crate) insufficient_scope: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Discovery {
    pub(crate) issuer: String,
    pub(crate) resource: String,
    authorization_endpoint: Url,
    token_endpoint: Url,
    registration_endpoint: Option<Url>,
    scopes: Vec<String>,
    require_issuer_parameter: bool,
}

pub(crate) fn challenge(headers: &HeaderMap) -> Challenge {
    let mut parsed = Challenge::default();
    for value in headers.get_all(reqwest::header::WWW_AUTHENTICATE) {
        let Ok(value) = value.to_str() else {
            continue;
        };
        if parsed.resource_metadata.is_none() {
            parsed.resource_metadata = auth::challenge_param(value, "resource_metadata");
        }
        if parsed.scope.is_none() {
            parsed.scope = auth::challenge_param(value, "scope");
        }
        if !parsed.insufficient_scope {
            parsed.insufficient_scope = auth::challenge_param(value, "error")
                .is_some_and(|error| error.eq_ignore_ascii_case("insufficient_scope"));
        }
    }
    parsed
}

pub(crate) async fn discover(
    oauth: &OAuthClient,
    target: &Url,
    challenge: &Challenge,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Discovery, McpError> {
    let resource = auth::canonical_resource(target);
    let resource_metadata = fetch_resource_metadata(
        &oauth.client,
        target,
        challenge.resource_metadata.as_deref(),
        &oauth.policy,
        timeout,
        cancel,
    )
    .await?;
    validate_resource_binding(&resource_metadata, &resource)?;
    let issuer = resource_metadata
        .get("authorization_servers")
        .and_then(|value| value.as_array())
        .and_then(|values| values.first())
        .and_then(JsonValueTrait::as_str)
        .ok_or_else(|| auth_error("missing authorization server issuer"))?
        .to_owned();
    let issuer_url = parse_issuer(&issuer)?;
    let auth_metadata = fetch_authorization_metadata(
        &oauth.client,
        &issuer,
        &issuer_url,
        &oauth.policy,
        timeout,
        cancel,
    )
    .await?;
    let returned_issuer = auth_metadata
        .get("issuer")
        .and_then(JsonValueTrait::as_str)
        .ok_or(McpError::IssuerMismatch)?;
    if returned_issuer != issuer {
        return Err(McpError::IssuerMismatch);
    }
    if let Some(methods) = auth_metadata
        .get("code_challenge_methods_supported")
        .and_then(|value| value.as_array())
        && !methods.iter().any(|method| method.as_str() == Some("S256"))
    {
        return Err(auth_error(
            "authorization server does not support PKCE S256",
        ));
    }
    let authorization_endpoint = endpoint_field(&auth_metadata, "authorization_endpoint")?;
    let token_endpoint = endpoint_field(&auth_metadata, "token_endpoint")?;
    let registration_endpoint = auth_metadata
        .get("registration_endpoint")
        .and_then(JsonValueTrait::as_str)
        .map(parse_endpoint)
        .transpose()?;
    let scopes = resource_metadata
        .get("scopes_supported")
        .or_else(|| auth_metadata.get("scopes_supported"))
        .and_then(|value| value.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(JsonValueTrait::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let require_issuer_parameter = auth_metadata
        .get("authorization_response_iss_parameter_supported")
        .and_then(JsonValueTrait::as_bool)
        .unwrap_or(false);
    Ok(Discovery {
        issuer,
        resource,
        authorization_endpoint,
        token_endpoint,
        registration_endpoint,
        scopes,
        require_issuer_parameter,
    })
}

pub(crate) async fn refresh<P, Fut>(
    coordinator: &auth::RefreshCoordinator,
    oauth: &OAuthClient,
    discovery: &Discovery,
    record: &TokenRecord,
    timeout: Duration,
    cancel: &CancellationToken,
    persist: P,
) -> Result<Option<TokenRecord>, McpError>
where
    P: FnOnce(TokenRecord) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), McpError>> + Send + 'static,
{
    let Some(refresh_token) = record.refresh_token.as_deref() else {
        return Ok(None);
    };
    if cancel.is_cancelled() {
        return Err(McpError::NoAskFrontEnd);
    }
    let key = auth::refresh_key(&discovery.issuer, &discovery.resource);
    let client = oauth.client.clone();
    let policy = oauth.policy;
    let discovery = discovery.clone();
    let record = record.clone();
    let refresh_token = refresh_token.to_owned();
    let operation = move || async move {
        let params = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
            ("client_id", record.client_id.as_str()),
            ("resource", discovery.resource.as_str()),
        ];
        let cancel = CancellationToken::new();
        let response = post_form(
            &client,
            &policy,
            &discovery.token_endpoint,
            &params,
            timeout,
            &cancel,
        )
        .await?;
        if !response.status().is_success() {
            return refresh_refusal(response.status());
        }
        let value = response_json(response, timeout, &cancel).await?;
        let token = token_from_response(
            &value,
            &record.client_id,
            &record.scopes,
            Some(refresh_token.as_str()),
        )?;
        Ok(Some(token))
    };
    let result = tokio::select! {
        () = cancel.cancelled() => Err(McpError::NoAskFrontEnd),
        result = coordinator.run(&key, record.access_token.as_str(), operation) => result,
    }?;
    // Persist only the slot's winner, under the coordinator's commit lock:
    // an interactive login published during or after the flight supersedes
    // the flight's token and must not be overwritten.
    match result {
        Some(settled) => coordinator.commit(&key, settled, persist).await.map(Some),
        None => Ok(None),
    }
}

/// Maps a failed token-endpoint status for a refresh.
///
/// A refusal that invalidates the credential (400, 401, 403, 404, and other
/// client errors) reports `None` and the caller falls back to interactive
/// authorization. A server error, a request timeout (408), or a rate limit
/// (429) is transient: it reports an error so a later request may refresh
/// again with the same credential.
fn refresh_refusal(status: StatusCode) -> Result<Option<TokenRecord>, McpError> {
    let transient = matches!(
        status,
        StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_MANY_REQUESTS
    );
    if status.is_client_error() && !transient {
        return Ok(None);
    }
    Err(auth_error(&format!(
        "OAuth token endpoint is unavailable (HTTP {}); try again later",
        status.as_u16()
    )))
}

/// The endpoint, discovery document, and token history one authorization runs over.
pub(crate) struct AuthorizePlan<'a> {
    pub target: &'a Url,
    pub discovery: &'a Discovery,
    pub existing: Option<&'a TokenRecord>,
    pub requested_scope: Option<&'a str>,
    pub client_version: &'a str,
}

pub(crate) async fn authorize(
    oauth: &OAuthClient,
    plan: &AuthorizePlan<'_>,
    services: &dyn Services,
    who: &Caller,
    cancel: &CancellationToken,
) -> Result<TokenRecord, McpError> {
    let AuthorizePlan {
        target,
        discovery,
        existing,
        requested_scope,
        client_version: _,
    } = *plan;
    if cancel.is_cancelled() {
        return Err(McpError::NoAskFrontEnd);
    }

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| auth_error("could not open OAuth loopback listener"))?;
    let address = listener
        .local_addr()
        .map_err(|_| auth_error("could not read OAuth loopback address"))?;
    let redirect_uri = format!("http://127.0.0.1:{}/callback", address.port());
    let client_id = registered_or_stored_client_id(
        &oauth.client,
        &oauth.policy,
        plan,
        &redirect_uri,
        services,
        who,
        cancel,
    )
    .await?;
    let scopes = requested_scopes(existing, requested_scope, &discovery.scopes);
    let scope_value = scopes.join(" ");
    let verifier = format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    let challenge = auth::pkce_challenge(&verifier);
    let state = uuid::Uuid::new_v4().to_string();
    let authorization_url = authorization_url(
        discovery,
        &client_id,
        &scope_value,
        &redirect_uri,
        &challenge,
        &state,
    );
    let callback = await_callback(
        listener,
        &state,
        &discovery.issuer,
        discovery.require_issuer_parameter,
        cancel,
    );
    let prompt = services.ask(
        who,
        Question::Confirm {
            text: format!(
                "Authorize the MCP server at {}. Open this URL to continue: {}",
                target.host_str().unwrap_or("server"),
                authorization_url
            )
            .into_boxed_str(),
        },
    );
    let callback_result = await_consent(callback, prompt, cancel).await?;
    let params = [
        ("grant_type", "authorization_code"),
        ("code", callback_result.code.as_str()),
        ("redirect_uri", redirect_uri.as_str()),
        ("client_id", client_id.as_str()),
        ("code_verifier", verifier.as_str()),
        ("resource", discovery.resource.as_str()),
    ];
    let response = post_form(
        &oauth.client,
        &oauth.policy,
        &discovery.token_endpoint,
        &params,
        Duration::from_secs(15),
        cancel,
    )
    .await?;
    if !response.status().is_success() {
        return Err(auth_error("OAuth token exchange was rejected"));
    }
    let value = response_json(response, Duration::from_secs(15), cancel).await?;
    token_from_response(&value, &client_id, &scopes, None)
}

async fn await_consent<C, P>(
    callback: C,
    prompt: P,
    cancel: &CancellationToken,
) -> Result<Callback, McpError>
where
    C: Future<Output = Result<Callback, McpError>>,
    P: Future<Output = Result<Option<Answer>, dal_agent::error::ServiceError>>,
{
    tokio::pin!(callback);
    tokio::pin!(prompt);
    tokio::select! {
        () = cancel.cancelled() => Err(McpError::NoAskFrontEnd),
        result = &mut callback => {
            let callback = result?;
            let answer = tokio::select! {
                () = cancel.cancelled() => return Err(McpError::NoAskFrontEnd),
                answer = &mut prompt => answer,
            };
            confirm_answer(answer)?;
            Ok(callback)
        }
        answer = &mut prompt => {
            confirm_answer(answer)?;
            callback.await
        }
    }
}

fn confirm_answer(
    answer: Result<Option<Answer>, dal_agent::error::ServiceError>,
) -> Result<(), McpError> {
    match answer {
        Ok(Some(Answer::Value(value))) if value.decode_as::<bool>().unwrap_or(false) => Ok(()),
        Ok(Some(Answer::Approve | Answer::ApproveForSession)) => Ok(()),
        Ok(Some(Answer::Decline | Answer::Cancel))
        | Err(
            dal_agent::error::ServiceError::Denied(_) | dal_agent::error::ServiceError::Cancelled,
        ) => Err(McpError::NoAskFrontEnd),
        Ok(None | Some(_)) => Err(auth_error("OAuth authorization was declined")),
        Err(_) => Err(auth_error("OAuth authorization prompt failed")),
    }
}
/// Resolves the OAuth client identifier: dynamic registration, a stored
/// record, or a user prompt, in that order.
async fn registered_or_stored_client_id(
    client: &Client,
    policy: &NetworkPolicy,
    plan: &AuthorizePlan<'_>,
    redirect_uri: &str,
    services: &dyn Services,
    who: &Caller,
    cancel: &CancellationToken,
) -> Result<String, McpError> {
    let registered = match plan.discovery.registration_endpoint.as_ref() {
        Some(endpoint) => {
            register_client(
                client,
                policy,
                endpoint,
                redirect_uri,
                plan.client_version,
                cancel,
            )
            .await?
        }
        None => None,
    };
    match registered {
        Some(client_id) => Ok(client_id),
        None => match plan.existing {
            Some(record) if !record.client_id.is_empty() => Ok(record.client_id.clone()),
            _ => ask_client_id(services, who, cancel).await,
        },
    }
}

/// Builds the authorization endpoint URL with PKCE and resource parameters.
fn authorization_url(
    discovery: &Discovery,
    client_id: &str,
    scope_value: &str,
    redirect_uri: &str,
    challenge: &str,
    state: &str,
) -> Url {
    let mut url = discovery.authorization_endpoint.clone();
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("client_id", client_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("state", state)
            .append_pair("code_challenge", challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("resource", &discovery.resource);
        if !scope_value.is_empty() {
            query.append_pair("scope", scope_value);
        }
    }
    url
}

async fn fetch_resource_metadata(
    client: &Client,
    target: &Url,
    explicit: Option<&str>,
    policy: &NetworkPolicy,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Value, McpError> {
    let candidates = resource_metadata_urls(target, explicit)?;
    for candidate in candidates {
        let value = fetch_json(client, policy, &candidate, timeout, cancel).await?;
        if let Some(value) = value {
            return Ok(value);
        }
    }
    Err(auth_error("protected-resource metadata is unavailable"))
}

fn resource_metadata_urls(target: &Url, explicit: Option<&str>) -> Result<Vec<Url>, McpError> {
    let mut candidates = Vec::new();
    if let Some(explicit) = explicit {
        candidates.push(parse_endpoint(explicit)?);
    }
    let origin = target.origin().ascii_serialization();
    let path = target.path().trim_matches('/');
    if !path.is_empty() {
        candidates.push(parse_endpoint(&format!(
            "{origin}/.well-known/oauth-protected-resource/{path}"
        ))?);
    }
    candidates.push(parse_endpoint(&format!(
        "{origin}/.well-known/oauth-protected-resource"
    ))?);
    candidates.dedup();
    Ok(candidates)
}

async fn fetch_authorization_metadata(
    client: &Client,
    issuer: &str,
    issuer_url: &Url,
    policy: &NetworkPolicy,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Value, McpError> {
    let origin = issuer_url.origin().ascii_serialization();
    let path = issuer_url.path().trim_matches('/');
    let inserted = if path.is_empty() {
        format!("{origin}/.well-known/oauth-authorization-server")
    } else {
        format!("{origin}/.well-known/oauth-authorization-server/{path}")
    };
    let oidc = format!(
        "{}/.well-known/openid-configuration",
        issuer.trim_end_matches('/')
    );
    for candidate in [inserted, oidc] {
        let candidate = parse_endpoint(&candidate)?;
        let value = fetch_json(client, policy, &candidate, timeout, cancel).await?;
        if let Some(value) = value {
            return Ok(value);
        }
    }
    Err(auth_error("authorization-server metadata is unavailable"))
}

async fn fetch_json(
    client: &Client,
    policy: &NetworkPolicy,
    url: &Url,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Option<Value>, McpError> {
    let response = send(
        client.get(url.clone()).timeout(timeout),
        policy,
        cancel,
        timeout,
    )
    .await?;
    if matches!(
        response.status(),
        StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
    ) {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(auth_error("OAuth metadata request was rejected"));
    }
    response_json(response, timeout, cancel).await.map(Some)
}

fn validate_resource_binding(metadata: &Value, resource: &str) -> Result<(), McpError> {
    let reported = metadata
        .get("resource")
        .and_then(JsonValueTrait::as_str)
        .ok_or_else(|| auth_error("protected-resource metadata omitted its resource"))?;
    let reported = Url::parse(reported)
        .map_err(|_| auth_error("protected-resource metadata has an invalid resource"))?;
    if !reported.username().is_empty()
        || reported.password().is_some()
        || reported.query().is_some()
        || reported.fragment().is_some()
        || auth::canonical_resource(&reported) != resource
    {
        return Err(auth_error(
            "protected-resource metadata does not match the MCP server",
        ));
    }
    Ok(())
}

fn parse_issuer(issuer: &str) -> Result<Url, McpError> {
    let url = parse_endpoint(issuer)?;
    if url.query().is_some() {
        return Err(auth_error("authorization issuer cannot contain a query"));
    }
    Ok(url)
}

fn endpoint_field(metadata: &Value, name: &str) -> Result<Url, McpError> {
    let endpoint = metadata
        .get(name)
        .and_then(JsonValueTrait::as_str)
        .ok_or_else(|| auth_error("authorization-server metadata omitted a required endpoint"))?;
    parse_endpoint(endpoint)
}

fn parse_endpoint(endpoint: &str) -> Result<Url, McpError> {
    let url =
        Url::parse(endpoint).map_err(|_| auth_error("OAuth metadata contains an invalid URL"))?;
    let loopback = url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
    });
    if !matches!(url.scheme(), "https" | "http")
        || (url.scheme() == "http" && !loopback)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(auth_error("OAuth endpoint must use HTTPS or loopback HTTP"));
    }
    Ok(url)
}

async fn register_client(
    client: &Client,
    policy: &NetworkPolicy,
    endpoint: &Url,
    redirect_uri: &str,
    client_version: &str,
    cancel: &CancellationToken,
) -> Result<Option<String>, McpError> {
    #[derive(Serialize)]
    struct Registration<'a> {
        redirect_uris: [&'a str; 1],
        client_name: &'a str,
        client_uri: &'a str,
        application_type: &'a str,
        token_endpoint_auth_method: &'a str,
        grant_types: [&'a str; 2],
        response_types: [&'a str; 1],
    }
    let client_name = format!("Dalgona MCP client {client_version}");
    let request = Registration {
        redirect_uris: [redirect_uri],
        client_name: &client_name,
        client_uri: "https://github.com/",
        application_type: "native",
        token_endpoint_auth_method: "none",
        grant_types: ["authorization_code", "refresh_token"],
        response_types: ["code"],
    };
    let body = sonic_rs::to_string(&request)
        .map_err(|_| auth_error("OAuth client registration request could not be encoded"))?;
    let request = client
        .post(endpoint.clone())
        .timeout(Duration::from_secs(15))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body);
    let response = match send(request, policy, cancel, Duration::from_secs(15)).await {
        Ok(response) if response.status().is_success() => response,
        Ok(_) => return Ok(None),
        Err(error) => return Err(error),
    };
    let value = response_json(response, Duration::from_secs(15), cancel).await?;
    let client_id = value
        .get("client_id")
        .and_then(JsonValueTrait::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    Ok(client_id)
}

async fn ask_client_id(
    services: &dyn Services,
    who: &Caller,
    cancel: &CancellationToken,
) -> Result<String, McpError> {
    let answer = tokio::select! {
        () = cancel.cancelled() => return Err(McpError::NoAskFrontEnd),
        answer = services.ask(who, Question::Text {
            prompt: "The authorization server does not support dynamic client registration. Enter its native OAuth client ID.".into(),
            placeholder: Some("client id".into()),
        }) => answer,
    };
    match answer {
        Ok(Some(Answer::Value(value))) => {
            let client_id = value
                .decode_as::<String>()
                .map_err(|_| auth_error("OAuth client ID must be text"))?;
            if client_id.trim().is_empty() {
                return Err(auth_error("OAuth client ID cannot be empty"));
            }
            Ok(client_id.trim().to_owned())
        }
        Ok(None | Some(_)) | Err(dal_agent::error::ServiceError::Denied(_)) => {
            Err(McpError::NoAskFrontEnd)
        }
        Err(dal_agent::error::ServiceError::Cancelled) if cancel.is_cancelled() => {
            Err(McpError::NoAskFrontEnd)
        }
        Err(_) => Err(auth_error("OAuth client ID prompt failed")),
    }
}

#[derive(Clone, Debug)]
struct Callback {
    code: String,
}

async fn await_callback(
    listener: TcpListener,
    expected_state: &str,
    issuer: &str,
    require_issuer_parameter: bool,
    cancel: &CancellationToken,
) -> Result<Callback, McpError> {
    loop {
        let (mut stream, _) = tokio::select! {
            () = cancel.cancelled() => return Err(McpError::NoAskFrontEnd),
            result = listener.accept() => result.map_err(|_| auth_error("OAuth callback listener failed"))?,
        };
        let Some(target) = read_callback_target(&mut stream, cancel).await? else {
            write_callback(&mut stream, 400).await;
            continue;
        };
        let Ok(url) = Url::parse(&format!("http://127.0.0.1{target}")) else {
            write_callback(&mut stream, 400).await;
            continue;
        };
        if url.path() != CALLBACK_PATH {
            write_callback(&mut stream, 404).await;
            continue;
        }
        let query = url.query_pairs().into_owned().collect::<Vec<_>>();
        let state = query
            .iter()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.as_str());
        if state != Some(expected_state) {
            write_callback(&mut stream, 400).await;
            continue;
        }
        let returned_issuer = query
            .iter()
            .find(|(key, _)| key == "iss")
            .map(|(_, value)| value.as_str());
        if returned_issuer.is_some_and(|value| value != issuer)
            || (require_issuer_parameter && returned_issuer.is_none())
        {
            write_callback(&mut stream, 400).await;
            return Err(McpError::IssuerMismatch);
        }
        if query.iter().any(|(key, _)| key == "error") {
            write_callback(&mut stream, 400).await;
            return Err(auth_error("OAuth authorization was rejected"));
        }
        let code = query
            .iter()
            .find(|(key, _)| key == "code")
            .map(|(_, value)| value.clone())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| auth_error("OAuth callback omitted its authorization code"))?;
        write_callback(&mut stream, 200).await;
        return Ok(Callback { code });
    }
}

async fn read_callback_target(
    stream: &mut TcpStream,
    cancel: &CancellationToken,
) -> Result<Option<String>, McpError> {
    let mut request = Vec::with_capacity(512);
    let mut byte = [0_u8; 1];
    while request.len() < CALLBACK_REQUEST_MAX {
        let count = tokio::select! {
            () = cancel.cancelled() => return Err(McpError::NoAskFrontEnd),
            result = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut byte)) => match result {
                Ok(Ok(count)) => count,
                Ok(Err(_)) | Err(_) => return Ok(None),
            },
        };
        if count == 0 {
            return Ok(None);
        }
        request.push(byte[0]);
        if request.ends_with(b"\r\n") {
            break;
        }
    }
    if request.len() >= CALLBACK_REQUEST_MAX {
        return Ok(None);
    }
    let line = std::str::from_utf8(&request).map_err(|_| auth_error("invalid OAuth callback"))?;
    let mut fields = line.split_ascii_whitespace();
    if fields.next() != Some("GET") {
        return Ok(None);
    }
    Ok(fields.next().map(str::to_owned))
}

async fn write_callback(stream: &mut TcpStream, status: u16) {
    let (reason, body) = if status == 200 {
        ("OK", "Authorization complete. You may close this window.")
    } else if status == 404 {
        ("Not Found", "Callback path not found.")
    } else {
        ("Bad Request", "Authorization callback rejected.")
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
}

async fn post_form(
    client: &Client,
    policy: &NetworkPolicy,
    endpoint: &Url,
    params: &[(&str, &str)],
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Response, McpError> {
    let request = client
        .post(endpoint.clone())
        .timeout(timeout)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(urlencoded(params));
    send(request, policy, cancel, timeout).await
}

async fn send(
    request: reqwest::RequestBuilder,
    policy: &NetworkPolicy,
    cancel: &CancellationToken,
    timeout: Duration,
) -> Result<Response, McpError> {
    let request_for_check = request
        .try_clone()
        .ok_or_else(|| auth_error("OAuth request could not be cloned"))?;
    let request_for_check = request_for_check
        .build()
        .map_err(|_| auth_error("OAuth request could not be built"))?;
    policy.check_url(request_for_check.url())?;
    tokio::select! {
        () = cancel.cancelled() => Err(McpError::NoAskFrontEnd),
        result = tokio::time::timeout(timeout, request.send()) => match result {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_)) => Err(auth_error("OAuth network request failed")),
            Err(_) => Err(auth_error("OAuth network request timed out")),
        },
    }
}

async fn response_json(
    mut response: Response,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Value, McpError> {
    let mut body = Vec::new();
    loop {
        let chunk = tokio::select! {
            () = cancel.cancelled() => return Err(McpError::NoAskFrontEnd),
            chunk = tokio::time::timeout(timeout, response.chunk()) => match chunk {
                Ok(Ok(chunk)) => chunk,
                Ok(Err(_)) => return Err(auth_error("OAuth response read failed")),
                Err(_) => return Err(auth_error("OAuth response read timed out")),
            },
        };
        let Some(chunk) = chunk else {
            break;
        };
        if body.len().saturating_add(chunk.len()) > OAUTH_BODY_MAX {
            return Err(auth_error("OAuth response exceeds the size limit"));
        }
        body.extend_from_slice(&chunk);
    }
    sonic_rs::from_slice(&body).map_err(|_| auth_error("OAuth response is invalid JSON"))
}

fn token_from_response(
    value: &Value,
    client_id: &str,
    scopes: &[String],
    previous_refresh: Option<&str>,
) -> Result<TokenRecord, McpError> {
    let access_token = value
        .get("access_token")
        .and_then(JsonValueTrait::as_str)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| auth_error("OAuth token response omitted an access token"))?
        .to_owned();
    if value
        .get("token_type")
        .and_then(JsonValueTrait::as_str)
        .is_some_and(|token_type| !token_type.eq_ignore_ascii_case("bearer"))
    {
        return Err(auth_error(
            "OAuth server returned an unsupported token type",
        ));
    }
    let refresh_token = value
        .get("refresh_token")
        .and_then(JsonValueTrait::as_str)
        .map(str::to_owned)
        .or_else(|| previous_refresh.map(str::to_owned));
    let scopes = value
        .get("scope")
        .and_then(JsonValueTrait::as_str)
        .map_or_else(
            || scopes.to_vec(),
            |scope| scope.split_ascii_whitespace().map(str::to_owned).collect(),
        );
    Ok(TokenRecord {
        client_id: client_id.to_owned(),
        access_token,
        refresh_token,
        scopes,
    })
}

fn requested_scopes(
    existing: Option<&TokenRecord>,
    requested: Option<&str>,
    supported: &[String],
) -> Vec<String> {
    let mut scopes = Vec::new();
    if let Some(existing) = existing {
        scopes.extend(existing.scopes.iter().cloned());
    }
    if let Some(requested) = requested {
        scopes.extend(requested.split_ascii_whitespace().map(str::to_owned));
    }
    if scopes.is_empty() {
        scopes.extend(supported.iter().cloned());
    }
    scopes.sort();
    scopes.dedup();
    scopes
}

fn urlencoded(params: &[(&str, &str)]) -> String {
    let mut encoded = String::new();
    for (index, (key, value)) in params.iter().enumerate() {
        if index > 0 {
            encoded.push('&');
        }
        form_encode(key, &mut encoded);
        encoded.push('=');
        form_encode(value, &mut encoded);
    }
    encoded
}

fn form_encode(value: &str, encoded: &mut String) {
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'*' => {
                encoded.push(byte as char);
            }
            b' ' => encoded.push('+'),
            other => {
                encoded.push('%');
                let _ = write!(encoded, "{other:02X}");
            }
        }
    }
}

fn auth_error(cause: &str) -> McpError {
    McpError::Auth {
        cause: cause.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::TcpListener,
    };

    /// Serves exactly `expected` token requests, then returns.
    async fn token_server(listener: TcpListener, expected: usize, count: Arc<AtomicUsize>) {
        for _ in 0..expected {
            let (stream, _) = listener.accept().await.expect("token request");
            count.fetch_add(1, Ordering::Relaxed);
            let mut reader = BufReader::new(stream);
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).await.expect("token headers");
                if line == "\r\n" {
                    break;
                }
            }
            let body = r#"{"access_token":"new-access","refresh_token":"new-refresh","token_type":"Bearer"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            reader
                .get_mut()
                .write_all(response.as_bytes())
                .await
                .expect("token response");
        }
    }

    async fn metadata_server(listener: TcpListener, bodies: Vec<String>) -> Vec<String> {
        let mut paths = Vec::with_capacity(bodies.len());
        for body in bodies {
            let (stream, _) = listener.accept().await.expect("metadata request");
            let mut reader = BufReader::new(stream);
            let mut headers = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).await.expect("metadata headers");
                if headers.is_empty() {
                    paths.push(line.trim().to_owned());
                }
                headers.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            reader
                .get_mut()
                .write_all(response.as_bytes())
                .await
                .expect("metadata response");
        }
        paths
    }

    async fn discovery_fixture() -> (Url, TcpListener) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("metadata listener");
        let address = listener.local_addr().expect("metadata address");
        (
            Url::parse(&format!("http://{address}/mcp")).expect("target URL"),
            listener,
        )
    }

    #[tokio::test]
    async fn discovery_rejects_authorization_server_issuer_mismatch() {
        let (target, listener) = discovery_fixture().await;
        let issuer = format!("{}/issuer", target.origin().ascii_serialization());
        let resource_metadata = format!(
            "{{\"resource\":\"{}\",\"authorization_servers\":[\"{issuer}\"]}}",
            auth::canonical_resource(&target)
        );
        let wrong_authorization_metadata = format!(
            "{{\"issuer\":\"{}/attacker\",\"authorization_endpoint\":\"{issuer}/authorize\",\"token_endpoint\":\"{issuer}/token\",\"code_challenge_methods_supported\":[\"S256\"]}}",
            target.origin().ascii_serialization()
        );
        let server = metadata_server(
            listener,
            vec![resource_metadata, wrong_authorization_metadata],
        );
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("metadata client");
        let challenge = Challenge::default();
        let oauth = OAuthClient {
            client,
            policy: NetworkPolicy::for_target(&target),
        };
        let cancel = CancellationToken::new();
        let (result, paths) = tokio::join!(
            discover(&oauth, &target, &challenge, Duration::from_secs(3), &cancel,),
            server,
        );
        assert!(matches!(result, Err(McpError::IssuerMismatch)));
        assert!(paths[0].contains("/.well-known/oauth-protected-resource/mcp"));
        assert!(paths[1].contains("/.well-known/oauth-authorization-server/issuer"));
    }

    #[tokio::test]
    async fn discovery_accepts_exact_issuer_and_resource_binding() {
        let (target, listener) = discovery_fixture().await;
        let issuer = format!("{}/issuer", target.origin().ascii_serialization());
        let resource_metadata = format!(
            "{{\"resource\":\"{}\",\"authorization_servers\":[\"{issuer}\"],\"scopes_supported\":[\"files:read\"]}}",
            auth::canonical_resource(&target)
        );
        let authorization_metadata = format!(
            "{{\"issuer\":\"{issuer}\",\"authorization_endpoint\":\"{issuer}/authorize\",\"token_endpoint\":\"{issuer}/token\",\"registration_endpoint\":\"{issuer}/register\",\"code_challenge_methods_supported\":[\"S256\"],\"authorization_response_iss_parameter_supported\":true}}"
        );
        let server = metadata_server(listener, vec![resource_metadata, authorization_metadata]);
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("metadata client");
        let challenge = Challenge::default();
        let oauth = OAuthClient {
            client,
            policy: NetworkPolicy::for_target(&target),
        };
        let cancel = CancellationToken::new();
        let (result, _) = tokio::join!(
            discover(&oauth, &target, &challenge, Duration::from_secs(3), &cancel,),
            server,
        );
        let discovered = result.expect("bound metadata");
        assert_eq!(discovered.issuer, issuer);
        assert_eq!(discovered.resource, auth::canonical_resource(&target));
        assert!(discovered.require_issuer_parameter);
        assert_eq!(discovered.scopes, vec!["files:read"]);
    }

    #[test]
    fn parses_bearer_challenge_parameters() {
        let mut headers = HeaderMap::new();
        headers.append(
            reqwest::header::WWW_AUTHENTICATE,
            "Bearer error=\"insufficient_scope\", scope=\"read write\", resource_metadata=\"https://auth.example/meta\""
                .parse()
                .expect("valid challenge"),
        );
        let parsed = challenge(&headers);
        assert!(parsed.insufficient_scope);
        assert_eq!(parsed.scope.as_deref(), Some("read write"));
        assert_eq!(
            parsed.resource_metadata.as_deref(),
            Some("https://auth.example/meta")
        );
    }

    #[tokio::test]
    async fn concurrent_refreshes_issue_one_token_request() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("token listener");
        let address = listener.local_addr().expect("token address");
        let token_endpoint =
            Url::parse(&format!("http://{address}/token")).expect("token endpoint");
        let count = Arc::new(AtomicUsize::new(0));
        let server = token_server(listener, 1, Arc::clone(&count));
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("OAuth client");
        let discovery = Discovery {
            issuer: format!("http://{address}/issuer"),
            resource: format!("http://{address}/mcp"),
            authorization_endpoint: token_endpoint.clone(),
            token_endpoint,
            registration_endpoint: None,
            scopes: Vec::new(),
            require_issuer_parameter: false,
        };
        let record = TokenRecord {
            client_id: "client".to_owned(),
            access_token: "old-access".to_owned(),
            refresh_token: Some("old-refresh".to_owned()),
            scopes: Vec::new(),
        };
        let cancel = CancellationToken::new();
        let oauth = OAuthClient {
            client,
            policy: NetworkPolicy::for_target(&discovery.token_endpoint),
        };
        let coordinator = Arc::new(auth::RefreshCoordinator::new());
        let mut calls = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let oauth = oauth.clone();
            let discovery = discovery.clone();
            let record = record.clone();
            let cancel = cancel.clone();
            let coordinator = Arc::clone(&coordinator);
            calls.spawn(async move {
                refresh(
                    &coordinator,
                    &oauth,
                    &discovery,
                    &record,
                    Duration::from_secs(3),
                    &cancel,
                    |_| async { Ok::<(), McpError>(()) },
                )
                .await
            });
        }
        let drain = async {
            while let Some(result) = calls.join_next().await {
                let refreshed = result.expect("refresh task").expect("refresh response");
                assert!(refreshed.is_some());
            }
        };
        tokio::join!(server, drain);
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn callback_first_waits_for_affirmative_confirmation() {
        let cancel = CancellationToken::new();
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .expect("callback client");
        // A real loopback callback completes first; a later decline must
        // still refuse the exchange.
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("callback listener");
        let port = listener.local_addr().expect("callback address").port();
        let callback = await_callback(
            listener,
            "state-1",
            "https://issuer.example",
            false,
            &cancel,
        );
        let (fired, callback) = tokio::join!(
            client
                .get(format!(
                    "http://127.0.0.1:{port}/callback?state=state-1&code=code-1"
                ))
                .send(),
            callback,
        );
        assert!(fired.expect("callback request").status().is_success());
        let callback = callback.expect("validated callback");
        assert_eq!(callback.code, "code-1");
        let declined = await_consent(
            async { Ok::<Callback, McpError>(callback) },
            async { Ok::<Option<Answer>, dal_agent::error::ServiceError>(Some(Answer::Decline)) },
            &cancel,
        )
        .await;
        assert!(matches!(declined, Err(McpError::NoAskFrontEnd)));

        // The same ordering with approval confirms the retained callback.
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("callback listener");
        let port = listener.local_addr().expect("callback address").port();
        let callback = await_callback(
            listener,
            "state-2",
            "https://issuer.example",
            false,
            &cancel,
        );
        let (fired, callback) = tokio::join!(
            client
                .get(format!(
                    "http://127.0.0.1:{port}/callback?state=state-2&code=code-2"
                ))
                .send(),
            callback,
        );
        assert!(fired.expect("callback request").status().is_success());
        let approved = await_consent(
            async { Ok::<Callback, McpError>(callback.expect("validated callback")) },
            async { Ok::<Option<Answer>, dal_agent::error::ServiceError>(Some(Answer::Approve)) },
            &cancel,
        )
        .await
        .expect("approval");
        assert_eq!(approved.code, "code-2");
    }

    #[tokio::test]
    async fn consent_cancellation_settles_without_exchange() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result =
            await_consent::<_, _>(std::future::pending(), std::future::pending(), &cancel).await;
        assert!(matches!(result, Err(McpError::NoAskFrontEnd)));
    }

    #[tokio::test]
    async fn guarded_requests_enforce_literal_address_at_send_boundary() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("counter listener");
        let address = listener.local_addr().expect("counter address");
        let count = Arc::new(AtomicUsize::new(0));
        let server = token_server(listener, 1, Arc::clone(&count));
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .expect("guarded test client");
        let endpoint = Url::parse(&format!("http://{address}/token")).expect("endpoint");
        let local = NetworkPolicy::for_target(&endpoint);
        let cancel = CancellationToken::new();
        let (response, ()) = tokio::join!(
            send(
                client.get(endpoint.clone()),
                &local,
                &cancel,
                Duration::from_secs(3),
            ),
            server,
        );
        let response = response.expect("local boundary is allowed");
        assert!(response.status().is_success());
        let public = NetworkPolicy::for_target(
            &Url::parse("https://mcp.example.test/mcp").expect("public target"),
        );
        let rejected = send(
            client.get(endpoint),
            &public,
            &CancellationToken::new(),
            Duration::from_secs(3),
        )
        .await;
        assert!(rejected.is_err());
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn network_policy_preserves_local_boundary_and_rejects_public_to_local() {
        let public = NetworkPolicy::for_target(
            &Url::parse("https://mcp.example.test/mcp").expect("public target"),
        );
        let local = NetworkPolicy::for_target(
            &Url::parse("http://127.0.0.1:9000/mcp").expect("local target"),
        );
        let loopback = Url::parse("http://127.0.0.1:9001/.well-known").expect("loopback URL");
        assert!(public.check_url(&loopback).is_err());
        assert!(local.check_url(&loopback).is_ok());
        let private = Url::parse("https://10.0.0.7/.well-known").expect("private URL");
        assert!(public.check_url(&private).is_err());
        let local_v6 =
            NetworkPolicy::for_target(&Url::parse("https://[::1]/mcp").expect("IPv6 target"));
        assert!(
            local_v6
                .check_url(&Url::parse("https://[::1]/meta").expect("IPv6 URL"))
                .is_ok()
        );
        assert!(
            public
                .check_url(&Url::parse("https://[::1]/meta").expect("IPv6 loopback URL"))
                .is_err()
        );
        assert!(
            public
                .check_url(&Url::parse("https://[::ffff:127.0.0.1]/meta").expect("mapped URL"))
                .is_err()
        );
    }

    #[tokio::test]
    async fn guarded_resolver_refuses_non_global_names_for_public_targets() {
        use reqwest::dns::Resolve as _;
        let public = NetworkPolicy::for_target(
            &Url::parse("https://mcp.example.test/mcp").expect("public target"),
        );
        let refused = public
            .resolver()
            .resolve("localhost".parse().expect("dns name"))
            .await;
        assert!(refused.is_err());
        let local = NetworkPolicy::for_target(
            &Url::parse("http://127.0.0.1:9000/mcp").expect("local target"),
        );
        let allowed = local
            .resolver()
            .resolve("localhost".parse().expect("dns name"))
            .await
            .expect("loopback resolution");
        assert!(
            allowed
                .into_iter()
                .all(|address| address.ip().is_loopback())
        );
    }

    async fn gated_token_server(
        listener: TcpListener,
        count: Arc<AtomicUsize>,
        gate: tokio::sync::oneshot::Receiver<()>,
    ) {
        let (stream, _) = listener.accept().await.expect("token request");
        count.fetch_add(1, Ordering::Relaxed);
        let mut reader = BufReader::new(stream);
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).await.expect("token headers");
            if line == "\r\n" {
                break;
            }
        }
        gate.await.expect("release gate");
        let body = r#"{"access_token":"stale-access","refresh_token":"stale-refresh","token_type":"Bearer"}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        reader
            .get_mut()
            .write_all(response.as_bytes())
            .await
            .expect("token response");
    }

    #[tokio::test]
    async fn a_superseded_refresh_does_not_persist_its_stale_token() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("token listener");
        let address = listener.local_addr().expect("token address");
        let count = Arc::new(AtomicUsize::new(0));
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
        let server = gated_token_server(listener, Arc::clone(&count), gate_rx);
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("OAuth client");
        let discovery = Discovery {
            issuer: format!("http://{address}/issuer"),
            resource: format!("http://{address}/mcp"),
            authorization_endpoint: Url::parse(&format!("http://{address}/authorize"))
                .expect("authorize endpoint"),
            token_endpoint: Url::parse(&format!("http://{address}/token")).expect("token endpoint"),
            registration_endpoint: None,
            scopes: Vec::new(),
            require_issuer_parameter: false,
        };
        let oauth = OAuthClient {
            client,
            policy: NetworkPolicy::for_target(&discovery.token_endpoint),
        };
        let record = TokenRecord {
            client_id: "client".to_owned(),
            access_token: "old-access".to_owned(),
            refresh_token: Some("old-refresh".to_owned()),
            scopes: Vec::new(),
        };
        let coordinator = Arc::new(auth::RefreshCoordinator::new());
        let persisted = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let key = auth::refresh_key(&discovery.issuer, &discovery.resource);
        let cancel = CancellationToken::new();
        let log = Arc::clone(&persisted);
        let refreshing = refresh(
            &coordinator,
            &oauth,
            &discovery,
            &record,
            Duration::from_secs(5),
            &cancel,
            move |token| async move {
                log.lock()
                    .expect("persist log")
                    .push(token.access_token.clone());
                Ok::<(), McpError>(())
            },
        );
        let publisher = async {
            tokio::time::timeout(Duration::from_secs(5), async {
                while count.load(Ordering::Relaxed) == 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the refresh flight reached the token endpoint");
            coordinator
                .publish(
                    &key,
                    TokenRecord {
                        client_id: "client".to_owned(),
                        access_token: "interactive-access".to_owned(),
                        refresh_token: None,
                        scopes: Vec::new(),
                    },
                    |_| async { Ok::<(), McpError>(()) },
                )
                .await
                .expect("interactive record persists");
            gate_tx.send(()).expect("release the flight");
        };
        let (updated, (), ()) = tokio::join!(refreshing, publisher, server);
        let updated = updated.expect("refresh response");
        assert_eq!(
            updated.as_ref().map(|token| token.access_token.as_str()),
            Some("interactive-access"),
            "the caller adopts the published winner, not the superseded flight"
        );
        assert_eq!(
            persisted.lock().expect("persist log").as_slice(),
            &["interactive-access".to_owned()],
            "only the settled winner persists"
        );
    }

    #[test]
    fn refresh_refusal_keeps_transient_statuses_retryable() {
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::NOT_FOUND,
        ] {
            assert!(
                matches!(refresh_refusal(status), Ok(None)),
                "{status} invalidates the credential"
            );
        }
        for status in [
            StatusCode::REQUEST_TIMEOUT,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            assert!(
                matches!(refresh_refusal(status), Err(McpError::Auth { .. })),
                "{status} is transient"
            );
        }
    }

    #[test]
    fn form_encoding_preserves_standard_separators() {
        assert_eq!(
            urlencoded(&[("scope", "read write"), ("a", "x+y")]),
            "scope=read+write&a=x%2By"
        );
    }
}
