// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Streamable-HTTP POST client with issuer-bound OAuth authorization.

pub(crate) mod auth;
pub(crate) mod oauth;
pub(crate) mod protocol;

use std::{
    net::IpAddr,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use dal_core::RawJson;
use reqwest::{
    Client, Response, StatusCode, Url,
    header::{HeaderName, HeaderValue},
    redirect::Policy,
};
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};
use tokio::{sync::Mutex, time::Instant};
use tokio_util::sync::CancellationToken;

use crate::mcp::{
    Budgets, McpError, STEPUP_MAX, TransportError,
    http::{
        auth as token_auth,
        oauth::{Challenge, Discovery},
        protocol::{
            LEGACY_PROTOCOL_VERSION, PROTOCOL_VERSION, notification_body, outbound_headers,
            protocol_error, recognizes_modern_error, request_body, session_from,
        },
    },
    tools::{HeaderAnnotation, Key, parameter_headers},
};

const RESPONSE_MAX: usize = 8 * 1024 * 1024;
const ERROR_BODY_MAX: usize = 64 * 1024;
const SESSION_ID_MAX: usize = 4096;

#[derive(Clone, Debug)]
struct CallDeadline {
    started: Instant,
    expires: Instant,
    initial: Duration,
    cap: Duration,
}

impl CallDeadline {
    fn new(initial: Duration, cap: Duration) -> Self {
        let started = Instant::now();
        Self {
            started,
            expires: started + initial,
            initial,
            cap,
        }
    }

    fn extend(&mut self) {
        let maximum = self.started + self.cap;
        self.expires = (Instant::now() + self.initial).min(maximum);
    }

    fn timeout_error(&self) -> TransportError {
        TransportError::Mcp(McpError::Timeout {
            n: self.initial.as_secs(),
        })
    }
}

/// Streamable-HTTP transport state for one MCP server instance.
pub(crate) struct HttpTransport {
    key: Key,
    url: Url,
    client: Client,
    tokens_path: PathBuf,
    client_version: String,
    connect_timeout: Duration,
    call_timeout: Duration,
    call_max: Duration,
    stepup_timeout: Duration,
    shutdown_grace: Duration,
    tokens: Mutex<Option<token_auth::TokenFile>>,
    verified_issuer: Mutex<Option<String>>,
    session_id: Mutex<Option<String>>,
    authorization: Mutex<()>,
}

impl HttpTransport {
    /// Creates a streamable-HTTP client. Authentication begins after a 401.
    pub(crate) fn new(
        key: Key,
        url: Url,
        tokens_path: PathBuf,
        client_version: String,
        budgets: &Budgets,
    ) -> Result<Self, McpError> {
        validate_endpoint(&url)?;
        let client = Client::builder()
            .connect_timeout(budgets.start)
            .redirect(Policy::none())
            .build()
            .map_err(|_| McpError::Start {
                key: key.display(),
                cause: "HTTP client creation failed".to_owned(),
            })?;
        Ok(Self {
            key,
            url,
            client,
            tokens_path,
            client_version,
            connect_timeout: budgets.start,
            call_timeout: budgets.call,
            call_max: budgets.call_max,
            stepup_timeout: budgets.stepup,
            shutdown_grace: budgets.shutdown_grace,
            tokens: Mutex::new(None),
            verified_issuer: Mutex::new(None),
            session_id: Mutex::new(None),
            authorization: Mutex::new(()),
        })
    }

    /// Posts one JSON-RPC method and returns its correlated response object.
    pub(crate) async fn exchange(
        &self,
        id: u64,
        ids: &AtomicU64,
        method: &str,
        params: &str,
        headers: &[HeaderAnnotation],
        arguments: Option<&RawJson>,
        version: &str,
        services: &dyn dal_agent::ext::Services,
        who: &dal_agent::ext::Caller,
        cancel: &CancellationToken,
    ) -> Result<RawJson, TransportError> {
        let params_value = sonic_rs::from_str::<Value>(params)
            .map_err(|_| TransportError::Mcp(protocol_error("invalid MCP params".to_owned())))?;
        if params_value.as_object().is_none() {
            return Err(TransportError::Mcp(protocol_error(
                "MCP params must be a JSON object".to_owned(),
            )));
        }
        let extra = match arguments {
            Some(arguments) => parameter_headers(headers, arguments)
                .map_err(|error| TransportError::Mcp(protocol_error(error.to_string())))?,
            None => Vec::new(),
        };
        let tool_name = if method == "tools/call" {
            params_value
                .get("name")
                .and_then(JsonValueTrait::as_str)
                .map(protocol::escape_name)
        } else {
            None
        };
        let mut deadline = CallDeadline::new(self.call_timeout, self.call_max);
        let mut request_id = id;
        let mut used_token = None;
        let mut auth_attempts = 0_u32;
        let mut step_ups = 0_u32;
        let mut stored_token_attempted = false;
        let mut refresh_attempted = false;
        let mut interactive_attempted = false;
        loop {
            if cancel.is_cancelled() {
                return Err(TransportError::Cancelled);
            }
            let body = request_body(request_id, method, params, version, &self.client_version)
                .map_err(TransportError::Mcp)?;
            let token = self.bearer().await;
            used_token.clone_from(&token);
            let response = self
                .send_once(
                    body.as_str(),
                    Some(method),
                    tool_name.as_deref(),
                    &extra,
                    version,
                    token.as_deref(),
                    cancel,
                    &deadline,
                )
                .await?;
            self.capture_session(&response).await?;
            let status = response.status();
            if status == StatusCode::UNAUTHORIZED {
                if auth_attempts >= 3 {
                    return Err(TransportError::Mcp(McpError::HttpAuth {
                        code: status.as_u16(),
                        n: auth_attempts,
                    }));
                }
                auth_attempts += 1;
                let challenge = oauth::challenge(response.headers());
                let _authorization = self.authorization.lock().await;
                if self.bearer().await != used_token {
                    request_id = ids.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                let discovery = self.discover(&challenge, cancel).await?;
                self.set_issuer(&discovery.issuer).await;
                let existing = self.record(&discovery.issuer, &discovery.resource).await?;
                if used_token.is_none()
                    && !stored_token_attempted
                    && let Some(record) = existing.as_ref()
                    && !record.access_token.is_empty()
                {
                    stored_token_attempted = true;
                    request_id = ids.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                if !refresh_attempted
                    && let Some(record) = existing.as_ref()
                    && record.refresh_token.is_some()
                {
                    refresh_attempted = true;
                    if let Some(updated) = oauth::refresh(
                        &self.client,
                        &discovery,
                        record,
                        self.connect_timeout,
                        cancel,
                    )
                    .await?
                    {
                        self.persist(&discovery, updated).await?;
                        request_id = ids.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                }
                if interactive_attempted {
                    return Err(TransportError::Mcp(McpError::HttpAuth {
                        code: status.as_u16(),
                        n: auth_attempts,
                    }));
                }
                interactive_attempted = true;
                refresh_attempted = true;
                let updated = self
                    .authorize(&discovery, existing.as_ref(), None, services, who, cancel)
                    .await?;
                self.persist(&discovery, updated).await?;
                deadline = CallDeadline::new(self.call_timeout, self.call_max);
                request_id = ids.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            if status == StatusCode::FORBIDDEN {
                let challenge = oauth::challenge(response.headers());
                if !challenge.insufficient_scope {
                    return Err(TransportError::Mcp(McpError::HttpAuth {
                        code: status.as_u16(),
                        n: auth_attempts,
                    }));
                }
                if step_ups >= STEPUP_MAX {
                    return Err(TransportError::Mcp(McpError::StepUpLimit));
                }
                step_ups += 1;
                let _authorization = self.authorization.lock().await;
                if self.bearer().await != used_token && challenge.scope.is_none() {
                    request_id = ids.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                let discovery = self.discover(&challenge, cancel).await?;
                self.set_issuer(&discovery.issuer).await;
                let existing = self.record(&discovery.issuer, &discovery.resource).await?;
                let updated = self
                    .authorize(
                        &discovery,
                        existing.as_ref(),
                        challenge.scope.as_deref(),
                        services,
                        who,
                        cancel,
                    )
                    .await?;
                self.persist(&discovery, updated).await?;
                deadline = CallDeadline::new(self.call_timeout, self.call_max);
                request_id = ids.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            if status == StatusCode::ACCEPTED || status == StatusCode::NO_CONTENT {
                return Err(TransportError::Mcp(protocol_error(
                    "MCP request completed without a response".to_owned(),
                )));
            }
            if !status.is_success() {
                let body = self
                    .read_body(response, ERROR_BODY_MAX, cancel, &deadline)
                    .await?;
                let text = String::from_utf8_lossy(&body).into_owned();
                if recognizes_modern_error(&text) {
                    return response_for_id(&text, request_id, id).map_err(TransportError::Mcp);
                }
                if matches!(
                    status,
                    StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
                ) {
                    if version == LEGACY_PROTOCOL_VERSION {
                        return Err(TransportError::Mcp(McpError::Auth {
                            cause: "unsupported HTTP+SSE-only transport".to_owned(),
                        }));
                    }
                    return Err(TransportError::Mcp(McpError::Protocol {
                        code: -32601,
                        message: format!("HTTP endpoint returned {}", status.as_u16()),
                    }));
                }
                return Err(TransportError::Mcp(McpError::HttpAuth {
                    code: status.as_u16(),
                    n: auth_attempts,
                }));
            }
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if content_type.starts_with("text/event-stream") {
                return self
                    .read_event_stream(
                        response,
                        request_id,
                        id,
                        cancel,
                        &mut deadline,
                        version,
                        token.as_deref(),
                    )
                    .await;
            }
            let body = self
                .read_body(response, RESPONSE_MAX, cancel, &deadline)
                .await?;
            let text = std::str::from_utf8(&body).map_err(|_| {
                TransportError::Mcp(protocol_error("invalid MCP response encoding".to_owned()))
            })?;
            return response_for_id(text, request_id, id).map_err(TransportError::Mcp);
        }
    }

    /// Posts a JSON-RPC notification, including authentication retries.
    pub(crate) async fn notify(
        &self,
        _ids: &AtomicU64,
        method: &str,
        version: &str,
        services: &dyn dal_agent::ext::Services,
        who: &dal_agent::ext::Caller,
        cancel: &CancellationToken,
    ) -> Result<(), TransportError> {
        let body = notification_body(method, version, &self.client_version)
            .map_err(TransportError::Mcp)?;
        let deadline = CallDeadline::new(self.call_timeout, self.call_timeout);
        let mut auth_attempts = 0_u32;
        let mut step_ups = 0_u32;
        let mut interactive_attempted = false;
        loop {
            if cancel.is_cancelled() {
                return Err(TransportError::Cancelled);
            }
            let token = self.bearer().await;
            let response = self
                .send_once(
                    body.as_str(),
                    Some(method),
                    None,
                    &[],
                    version,
                    token.as_deref(),
                    cancel,
                    &deadline,
                )
                .await?;
            self.capture_session(&response).await?;
            if response.status() == StatusCode::UNAUTHORIZED {
                if auth_attempts >= 3 {
                    return Err(TransportError::Mcp(McpError::HttpAuth {
                        code: 401,
                        n: auth_attempts,
                    }));
                }
                auth_attempts += 1;
                let challenge = oauth::challenge(response.headers());
                let _authorization = self.authorization.lock().await;
                if self.bearer().await != token {
                    continue;
                }
                let discovery = self.discover(&challenge, cancel).await?;
                self.set_issuer(&discovery.issuer).await;
                let existing = self.record(&discovery.issuer, &discovery.resource).await?;
                if token.is_none()
                    && let Some(record) = existing.as_ref()
                    && !record.access_token.is_empty()
                {
                    continue;
                }
                if let Some(record) = existing.as_ref()
                    && record.refresh_token.is_some()
                    && let Some(updated) = oauth::refresh(
                        &self.client,
                        &discovery,
                        record,
                        self.connect_timeout,
                        cancel,
                    )
                    .await?
                {
                    self.persist(&discovery, updated).await?;
                    continue;
                }
                if interactive_attempted {
                    return Err(TransportError::Mcp(McpError::HttpAuth {
                        code: 401,
                        n: auth_attempts,
                    }));
                }
                interactive_attempted = true;
                let updated = self
                    .authorize(&discovery, existing.as_ref(), None, services, who, cancel)
                    .await?;
                self.persist(&discovery, updated).await?;
                continue;
            }
            if response.status() == StatusCode::FORBIDDEN {
                let challenge = oauth::challenge(response.headers());
                if !challenge.insufficient_scope {
                    return Err(TransportError::Mcp(McpError::HttpAuth {
                        code: 403,
                        n: auth_attempts,
                    }));
                }
                if step_ups >= STEPUP_MAX {
                    return Err(TransportError::Mcp(McpError::StepUpLimit));
                }
                step_ups += 1;
                let _authorization = self.authorization.lock().await;
                let discovery = self.discover(&challenge, cancel).await?;
                self.set_issuer(&discovery.issuer).await;
                let existing = self.record(&discovery.issuer, &discovery.resource).await?;
                let updated = self
                    .authorize(
                        &discovery,
                        existing.as_ref(),
                        challenge.scope.as_deref(),
                        services,
                        who,
                        cancel,
                    )
                    .await?;
                self.persist(&discovery, updated).await?;
                continue;
            }
            if response.status().is_success() || response.status() == StatusCode::ACCEPTED {
                return Ok(());
            }
            if matches!(
                response.status(),
                StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
            ) && version == LEGACY_PROTOCOL_VERSION
            {
                return Err(TransportError::Mcp(McpError::Auth {
                    cause: "unsupported HTTP+SSE-only transport".to_owned(),
                }));
            }
            if matches!(
                response.status(),
                StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
            ) {
                return Err(TransportError::Mcp(McpError::Protocol {
                    code: -32601,
                    message: format!("HTTP endpoint returned {}", response.status().as_u16()),
                }));
            }
            return Err(TransportError::Mcp(McpError::HttpAuth {
                code: response.status().as_u16(),
                n: auth_attempts,
            }));
        }
    }

    /// Deletes an established streamable-HTTP session when the server supports it.
    pub(crate) async fn shutdown(&self) -> Result<(), McpError> {
        let Some(session) = self.session_id.lock().await.clone() else {
            return Ok(());
        };
        let version = PROTOCOL_VERSION;
        let headers = outbound_headers(version, None, None, &[], Some(&session))?;
        let mut request = self.client.delete(self.url.clone()).headers(headers);
        if let Some(token) = self.bearer().await {
            request = request.bearer_auth(token);
        }
        let response = tokio::time::timeout(self.shutdown_grace, request.send())
            .await
            .map_err(|_| protocol_error("HTTP session shutdown timed out".to_owned()))?
            .map_err(|_| protocol_error("HTTP session shutdown failed".to_owned()))?;
        if response.status().is_success()
            || matches!(
                response.status(),
                StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
            )
        {
            *self.session_id.lock().await = None;
            return Ok(());
        }
        Err(McpError::HttpAuth {
            code: response.status().as_u16(),
            n: 0,
        })
    }

    async fn send_once(
        &self,
        body: &str,
        method: Option<&str>,
        name: Option<&str>,
        extra: &[(HeaderName, HeaderValue)],
        version: &str,
        token: Option<&str>,
        cancel: &CancellationToken,
        deadline: &CallDeadline,
    ) -> Result<Response, TransportError> {
        if cancel.is_cancelled() {
            return Err(TransportError::Cancelled);
        }
        let session = self.session_id.lock().await.clone();
        let headers = outbound_headers(version, method, name, extra, session.as_deref())
            .map_err(TransportError::Mcp)?;
        let mut request = self
            .client
            .post(self.url.clone())
            .headers(headers)
            .body(body.to_owned());
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        tokio::select! {
            () = cancel.cancelled() => Err(TransportError::Cancelled),
            response = tokio::time::timeout_at(deadline.expires, request.send()) => match response {
                Ok(Ok(response)) => Ok(response),
                Ok(Err(_)) => Err(TransportError::Mcp(protocol_error(format!(
                    "HTTP request failed for {}",
                    self.key.display()
                )))),
                Err(_) => Err(deadline.timeout_error()),
            },
        }
    }

    async fn read_body(
        &self,
        mut response: Response,
        maximum: usize,
        cancel: &CancellationToken,
        deadline: &CallDeadline,
    ) -> Result<Vec<u8>, TransportError> {
        let mut body = Vec::new();
        loop {
            let chunk = tokio::select! {
                () = cancel.cancelled() => return Err(TransportError::Cancelled),
                chunk = tokio::time::timeout_at(deadline.expires, response.chunk()) => match chunk {
                    Ok(Ok(chunk)) => chunk,
                    Ok(Err(_)) => return Err(TransportError::Mcp(protocol_error("HTTP response read failed".to_owned()))),
                    Err(_) => return Err(deadline.timeout_error()),
                },
            };
            let Some(chunk) = chunk else {
                return Ok(body);
            };
            if body.len().saturating_add(chunk.len()) > maximum {
                return Err(TransportError::Mcp(protocol_error(
                    "HTTP response exceeds the size limit".to_owned(),
                )));
            }
            body.extend_from_slice(&chunk);
        }
    }

    async fn read_event_stream(
        &self,
        mut response: Response,
        request_id: u64,
        original_id: u64,
        cancel: &CancellationToken,
        deadline: &mut CallDeadline,
        version: &str,
        token: Option<&str>,
    ) -> Result<RawJson, TransportError> {
        let mut parser = SseParser::default();
        loop {
            let chunk = tokio::select! {
                () = cancel.cancelled() => return Err(TransportError::Cancelled),
                chunk = tokio::time::timeout_at(deadline.expires, response.chunk()) => match chunk {
                    Ok(Ok(chunk)) => chunk,
                    Ok(Err(_)) => return Err(TransportError::Mcp(protocol_error("HTTP event stream read failed".to_owned()))),
                    Err(_) => return Err(deadline.timeout_error()),
                },
            };
            let Some(chunk) = chunk else {
                break;
            };
            for event in parser.push(&chunk).map_err(TransportError::Mcp)? {
                if let Some(reply) = self
                    .process_event(
                        &event,
                        request_id,
                        original_id,
                        cancel,
                        deadline,
                        version,
                        token,
                    )
                    .await?
                {
                    return Ok(reply);
                }
            }
        }
        for event in parser.finish().map_err(TransportError::Mcp)? {
            if let Some(reply) = self
                .process_event(
                    &event,
                    request_id,
                    original_id,
                    cancel,
                    deadline,
                    version,
                    token,
                )
                .await?
            {
                return Ok(reply);
            }
        }
        Err(TransportError::Mcp(protocol_error(
            "HTTP event stream ended without a response".to_owned(),
        )))
    }

    async fn process_event(
        &self,
        event: &str,
        request_id: u64,
        original_id: u64,
        cancel: &CancellationToken,
        deadline: &mut CallDeadline,
        version: &str,
        token: Option<&str>,
    ) -> Result<Option<RawJson>, TransportError> {
        if event.is_empty() || event == "[DONE]" {
            return Ok(None);
        }
        let raw = RawJson::parse(event).map_err(|_| {
            TransportError::Mcp(protocol_error(
                "invalid JSON in HTTP event stream".to_owned(),
            ))
        })?;
        let value = raw.decode_as::<Value>().map_err(|_| {
            TransportError::Mcp(protocol_error("invalid JSON-RPC event".to_owned()))
        })?;
        if let Some(method) = value.get("method").and_then(JsonValueTrait::as_str) {
            if method == "notifications/progress" {
                let token = value
                    .get("params")
                    .and_then(|params| params.get("_meta"))
                    .and_then(|meta| meta.get("progressToken"))
                    .and_then(JsonValueTrait::as_str);
                if token == Some(&format!("t-{request_id}")) {
                    deadline.extend();
                }
                return Ok(None);
            }
            if value.get("id").is_some() {
                self.answer_server_request(&value, version, token, cancel)
                    .await;
            }
            return Ok(None);
        }
        if value.get("id").and_then(JsonValueTrait::as_u64) == Some(request_id) {
            return response_for_id(event, request_id, original_id)
                .map(Some)
                .map_err(TransportError::Mcp);
        }
        Ok(None)
    }

    async fn answer_server_request(
        &self,
        value: &Value,
        version: &str,
        token: Option<&str>,
        cancel: &CancellationToken,
    ) {
        let Some(id) = value.get("id") else {
            return;
        };
        let Ok(id) = sonic_rs::to_string(id) else {
            return;
        };
        let Ok(method_name) =
            sonic_rs::to_string("client does not support server-initiated requests")
        else {
            return;
        };
        let Ok(body) = RawJson::parse(&format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"error\":{{\"code\":-32601,\"message\":{method_name}}}}}"
        )) else {
            return;
        };
        let session = self.session_id.lock().await.clone();
        let Ok(headers) = outbound_headers(version, None, None, &[], session.as_deref()) else {
            return;
        };
        let mut request = self
            .client
            .post(self.url.clone())
            .headers(headers)
            .body(body.as_str().to_owned());
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        let _ = tokio::select! {
            () = cancel.cancelled() => None,
            response = request.send() => response.ok(),
        };
    }

    async fn capture_session(&self, response: &Response) -> Result<(), TransportError> {
        let Some(session) = session_from(response) else {
            return Ok(());
        };
        if session.len() > SESSION_ID_MAX || HeaderValue::from_str(&session).is_err() {
            return Err(TransportError::Mcp(protocol_error(
                "invalid MCP session identifier".to_owned(),
            )));
        }
        *self.session_id.lock().await = Some(session);
        Ok(())
    }

    async fn bearer(&self) -> Option<String> {
        let issuer = self.verified_issuer.lock().await.clone()?;
        let resource = token_auth::canonical_resource(&self.url);
        let tokens = self.load_tokens().await;
        token_auth::record_for(&tokens, &issuer, &resource)
            .map(|record| record.access_token.clone())
            .filter(|token| !token.is_empty())
    }

    async fn load_tokens(&self) -> token_auth::TokenFile {
        if let Some(tokens) = self.tokens.lock().await.as_ref() {
            return tokens.clone();
        }
        let path = self.tokens_path.clone();
        let loaded = tokio::task::spawn_blocking(move || token_auth::read_tokens(&path))
            .await
            .unwrap_or_default();
        *self.tokens.lock().await = Some(loaded.clone());
        loaded
    }

    async fn record(
        &self,
        issuer: &str,
        resource: &str,
    ) -> Result<Option<token_auth::TokenRecord>, TransportError> {
        let tokens = self.load_tokens().await;
        Ok(token_auth::record_for(&tokens, issuer, resource).cloned())
    }

    async fn set_issuer(&self, issuer: &str) {
        *self.verified_issuer.lock().await = Some(issuer.to_owned());
    }

    async fn persist(
        &self,
        discovery: &Discovery,
        record: token_auth::TokenRecord,
    ) -> Result<(), TransportError> {
        let path = self.tokens_path.clone();
        let issuer = discovery.issuer.clone();
        let resource = discovery.resource.clone();
        let stored = record.clone();
        tokio::task::spawn_blocking(move || {
            token_auth::persist_token(&path, &issuer, &resource, stored)
        })
        .await
        .map_err(|_| {
            TransportError::Mcp(McpError::Auth {
                cause: "token persistence failed".to_owned(),
            })
        })?
        .map_err(TransportError::Mcp)?;
        let mut tokens = self.load_tokens().await;
        tokens
            .tokens
            .entry(discovery.issuer.clone())
            .or_default()
            .insert(discovery.resource.clone(), record);
        *self.tokens.lock().await = Some(tokens);
        self.set_issuer(&discovery.issuer).await;
        Ok(())
    }

    async fn discover(
        &self,
        challenge: &Challenge,
        cancel: &CancellationToken,
    ) -> Result<Discovery, TransportError> {
        oauth::discover(
            &self.client,
            &self.url,
            challenge,
            self.connect_timeout,
            cancel,
        )
        .await
        .map_err(TransportError::Mcp)
    }

    async fn authorize(
        &self,
        discovery: &Discovery,
        existing: Option<&token_auth::TokenRecord>,
        scope: Option<&str>,
        services: &dyn dal_agent::ext::Services,
        who: &dal_agent::ext::Caller,
        cancel: &CancellationToken,
    ) -> Result<token_auth::TokenRecord, TransportError> {
        tokio::time::timeout(
            self.stepup_timeout,
            oauth::authorize(
                &self.client,
                &self.url,
                &self.tokens_path,
                discovery,
                existing,
                scope,
                &self.client_version,
                services,
                who,
                cancel,
            ),
        )
        .await
        .map_err(|_| {
            TransportError::Mcp(McpError::Timeout {
                n: self.stepup_timeout.as_secs(),
            })
        })?
        .map_err(TransportError::Mcp)
    }
}

fn validate_endpoint(url: &Url) -> Result<(), McpError> {
    if !matches!(url.scheme(), "https" | "http")
        || (url.scheme() == "http" && !is_loopback(url))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(McpError::Start {
            key: "HTTP server".to_owned(),
            cause: "HTTP endpoint must use HTTPS or loopback HTTP and must not contain credentials or a fragment".to_owned(),
        });
    }
    Ok(())
}

fn is_loopback(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn response_for_id(text: &str, attempt_id: u64, original_id: u64) -> Result<RawJson, McpError> {
    let value = sonic_rs::from_str::<Value>(text)
        .map_err(|_| protocol_error("invalid JSON-RPC response".to_owned()))?;
    if let Some(items) = value.as_array() {
        for item in items {
            if item.get("id").and_then(JsonValueTrait::as_u64) == Some(attempt_id) {
                return normalize_reply(item, original_id);
            }
        }
        return Err(protocol_error(
            "HTTP response did not match its request id".to_owned(),
        ));
    }
    if value.get("id").and_then(JsonValueTrait::as_u64) != Some(attempt_id) {
        return Err(protocol_error(
            "HTTP response did not match its request id".to_owned(),
        ));
    }
    normalize_reply(&value, original_id)
}

fn normalize_reply(value: &Value, id: u64) -> Result<RawJson, McpError> {
    let body = if let Some(error) = value.get("error") {
        let error = sonic_rs::to_string(error)
            .map_err(|_| protocol_error("invalid JSON-RPC error response".to_owned()))?;
        format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"error\":{error}}}")
    } else if let Some(result) = value.get("result") {
        let result = sonic_rs::to_string(result)
            .map_err(|_| protocol_error("invalid JSON-RPC result response".to_owned()))?;
        format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{result}}}")
    } else {
        return Err(protocol_error(
            "JSON-RPC response has no result or error".to_owned(),
        ));
    };
    RawJson::parse(&body)
        .map_err(|_| protocol_error("invalid correlated JSON-RPC response".to_owned()))
}

#[derive(Default)]
struct SseParser {
    line: Vec<u8>,
    data: Vec<String>,
    bytes: usize,
}

impl SseParser {
    fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>, McpError> {
        self.bytes = self.bytes.saturating_add(chunk.len());
        if self.bytes > RESPONSE_MAX {
            return Err(protocol_error(
                "HTTP event stream exceeds the size limit".to_owned(),
            ));
        }
        let mut events = Vec::new();
        for byte in chunk {
            if *byte == b'\n' {
                self.consume_line(&mut events)?;
            } else {
                self.line.push(*byte);
            }
        }
        Ok(events)
    }

    fn finish(&mut self) -> Result<Vec<String>, McpError> {
        let mut events = Vec::new();
        if !self.line.is_empty() {
            self.consume_line(&mut events)?;
        }
        self.finish_event(&mut events);
        Ok(events)
    }

    fn consume_line(&mut self, events: &mut Vec<String>) -> Result<(), McpError> {
        if self.line.last() == Some(&b'\r') {
            self.line.pop();
        }
        let line = std::str::from_utf8(&self.line)
            .map_err(|_| protocol_error("invalid UTF-8 in HTTP event stream".to_owned()))?;
        if line.is_empty() {
            self.finish_event(events);
        } else if let Some(value) = line.strip_prefix("data:") {
            let value = value.strip_prefix(' ').unwrap_or(value);
            self.data.push(value.to_owned());
        }
        self.line.clear();
        Ok(())
    }

    fn finish_event(&mut self, events: &mut Vec<String>) {
        if !self.data.is_empty() {
            events.push(self.data.join("\n"));
            self.data.clear();
        }
    }
}

#[cfg(test)]
mod tests;
