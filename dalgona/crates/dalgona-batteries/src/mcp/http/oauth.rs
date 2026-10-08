// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Protected-resource discovery, native OAuth authorization, and token refresh.

use std::{net::IpAddr, time::Duration};

use dal_agent::ext::{Caller, Services};
use dal_core::{Answer, Question};
use reqwest::{Client, Response, StatusCode, Url, header::HeaderMap};
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
    client: &Client,
    target: &Url,
    challenge: &Challenge,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Discovery, McpError> {
    let resource = auth::canonical_resource(target);
    let resource_metadata = fetch_resource_metadata(
        client,
        target,
        challenge.resource_metadata.as_deref(),
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
    let auth_metadata =
        fetch_authorization_metadata(client, &issuer, &issuer_url, timeout, cancel).await?;
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

pub(crate) async fn refresh(
    client: &Client,
    discovery: &Discovery,
    record: &TokenRecord,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Option<TokenRecord>, McpError> {
    let Some(refresh_token) = record.refresh_token.as_deref() else {
        return Ok(None);
    };
    let params = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", record.client_id.as_str()),
        ("resource", discovery.resource.as_str()),
    ];
    let response = post_form(client, &discovery.token_endpoint, &params, timeout, cancel).await?;
    if !response.status().is_success() {
        return Ok(None);
    }
    let value = response_json(response, timeout, cancel).await?;
    let token = token_from_response(
        &value,
        &record.client_id,
        &record.scopes,
        Some(refresh_token),
    )?;
    Ok(Some(token))
}

pub(crate) struct AuthorizeSpec<'a> {
    pub(crate) client: &'a Client,
    pub(crate) target: &'a Url,
    pub(crate) discovery: &'a Discovery,
    pub(crate) existing: Option<&'a TokenRecord>,
    pub(crate) requested_scope: Option<&'a str>,
    pub(crate) client_version: &'a str,
    pub(crate) services: &'a dyn Services,
    pub(crate) who: &'a Caller,
    pub(crate) cancel: &'a CancellationToken,
}

struct FlowStart {
    listener: TcpListener,
    redirect_uri: String,
    client_id: String,
    scopes: Vec<String>,
    verifier: String,
    state: String,
    authorization_url: Url,
}

async fn begin_flow(spec: &AuthorizeSpec<'_>) -> Result<FlowStart, McpError> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| auth_error("could not open OAuth loopback listener"))?;
    let address = listener
        .local_addr()
        .map_err(|_| auth_error("could not read OAuth loopback address"))?;
    let redirect_uri = format!("http://127.0.0.1:{}/callback", address.port());
    let registered = match spec.discovery.registration_endpoint.as_ref() {
        Some(endpoint) => {
            register_client(
                spec.client,
                endpoint,
                &redirect_uri,
                spec.client_version,
                spec.cancel,
            )
            .await?
        }
        None => None,
    };
    let client_id = match registered {
        Some(client_id) => client_id,
        None => match spec.existing {
            Some(record) if !record.client_id.is_empty() => record.client_id.clone(),
            _ => ask_client_id(spec.services, spec.who, spec.cancel).await?,
        },
    };
    let scopes = requested_scopes(spec.existing, spec.requested_scope, &spec.discovery.scopes);
    let scope_value = scopes.join(" ");
    let verifier = format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    let challenge = auth::pkce_challenge(&verifier);
    let state = uuid::Uuid::new_v4().to_string();
    let mut authorization_url = spec.discovery.authorization_endpoint.clone();
    {
        let mut query = authorization_url.query_pairs_mut();
        query
            .append_pair("client_id", &client_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("resource", &spec.discovery.resource);
        if !scope_value.is_empty() {
            query.append_pair("scope", &scope_value);
        }
    }
    Ok(FlowStart {
        listener,
        redirect_uri,
        client_id,
        scopes,
        verifier,
        state,
        authorization_url,
    })
}

async fn exchange_code(
    spec: &AuthorizeSpec<'_>,
    code: &str,
    redirect_uri: &str,
    client_id: &str,
    verifier: &str,
    scopes: &[String],
) -> Result<TokenRecord, McpError> {
    let params = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", client_id),
        ("code_verifier", verifier),
        ("resource", spec.discovery.resource.as_str()),
    ];
    let response = post_form(
        spec.client,
        &spec.discovery.token_endpoint,
        &params,
        Duration::from_secs(15),
        spec.cancel,
    )
    .await?;
    if !response.status().is_success() {
        return Err(auth_error("OAuth token exchange was rejected"));
    }
    let value = response_json(response, Duration::from_secs(15), spec.cancel).await?;
    token_from_response(&value, client_id, scopes, None)
}

pub(crate) async fn authorize(spec: AuthorizeSpec<'_>) -> Result<TokenRecord, McpError> {
    if spec.cancel.is_cancelled() {
        return Err(McpError::NoAskFrontEnd);
    }
    let start = begin_flow(&spec).await?;
    let callback = await_callback(
        start.listener,
        &start.state,
        &spec.discovery.issuer,
        spec.discovery.require_issuer_parameter,
        spec.cancel,
    );
    tokio::pin!(callback);
    let prompt = spec.services.ask(
        spec.who,
        Question::Confirm {
            text: format!(
                "Authorize the MCP server at {}. Open this URL to continue: {}",
                spec.target.host_str().unwrap_or("server"),
                start.authorization_url
            )
            .into_boxed_str(),
        },
    );
    tokio::pin!(prompt);
    let callback_result = tokio::select! {
        () = spec.cancel.cancelled() => return Err(McpError::NoAskFrontEnd),
        result = &mut callback => result?,
        answer = &mut prompt => {
            match answer {
                Ok(Some(Answer::Value(value))) if value.decode_as::<bool>().unwrap_or(false) => {},
                Ok(Some(Answer::Approve | Answer::ApproveForSession)) => {},
                Ok(_) => return Err(auth_error("OAuth authorization was declined")),
                Err(dal_agent::error::ServiceError::Denied(_)) => return Err(McpError::NoAskFrontEnd),
                Err(dal_agent::error::ServiceError::Cancelled) if spec.cancel.is_cancelled() => return Err(McpError::NoAskFrontEnd),
                Err(_) => return Err(auth_error("OAuth authorization prompt failed")),
            }
            callback.await?
        }
    };
    exchange_code(
        &spec,
        &callback_result.code,
        &start.redirect_uri,
        &start.client_id,
        &start.verifier,
        &start.scopes,
    )
    .await
}

async fn fetch_resource_metadata(
    client: &Client,
    target: &Url,
    explicit: Option<&str>,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Value, McpError> {
    let candidates = resource_metadata_urls(target, explicit)?;
    for candidate in candidates {
        let value = fetch_json(client, &candidate, timeout, cancel).await?;
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
        let value = fetch_json(client, &candidate, timeout, cancel).await?;
        if let Some(value) = value {
            return Ok(value);
        }
    }
    Err(auth_error("authorization-server metadata is unavailable"))
}

async fn fetch_json(
    client: &Client,
    url: &Url,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Option<Value>, McpError> {
    let response = send(client.get(url.clone()).timeout(timeout), cancel, timeout).await?;
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
    let response = match send(request, cancel, Duration::from_secs(15)).await {
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
        Ok(_) => Err(McpError::NoAskFrontEnd),
        Err(dal_agent::error::ServiceError::Denied(_)) => Err(McpError::NoAskFrontEnd),
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
    send(request, cancel, timeout).await
}

async fn send(
    request: reqwest::RequestBuilder,
    cancel: &CancellationToken,
    timeout: Duration,
) -> Result<Response, McpError> {
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
                let _ = std::fmt::Write::write_fmt(encoded, format_args!("{other:02X}"));
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
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::TcpListener,
    };

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
        let cancel = CancellationToken::new();
        let (result, paths) = tokio::join!(
            discover(
                &client,
                &target,
                &challenge,
                Duration::from_secs(3),
                &cancel,
            ),
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
        let cancel = CancellationToken::new();
        let (result, _) = tokio::join!(
            discover(
                &client,
                &target,
                &challenge,
                Duration::from_secs(3),
                &cancel,
            ),
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

    #[test]
    fn form_encoding_preserves_standard_separators() {
        assert_eq!(
            urlencoded(&[("scope", "read write"), ("a", "x+y")]),
            "scope=read+write&a=x%2By"
        );
    }
}
