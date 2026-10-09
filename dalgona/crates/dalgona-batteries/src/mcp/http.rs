// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Streamable-HTTP POST client with issuer-bound OAuth authorization.

pub(crate) mod auth;
pub(crate) mod oauth;
pub(crate) mod protocol;

use std::{
    net::IpAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use dal_core::RawJson;
use reqwest::{
    Client, Response, StatusCode, Url,
    header::{HeaderMap, HeaderName, HeaderValue},
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
    tokens: Arc<Mutex<Option<token_auth::TokenFile>>>,
    verified_issuer: Mutex<Option<String>>,
    session_id: Mutex<Option<String>>,
    authorization: Mutex<token_auth::AuthorizationState>,
    refreshes: Arc<token_auth::RefreshCoordinator>,
}

/// Retry bounds for one request that keeps receiving 401.
#[derive(Default)]
struct AuthRetry {
    attempts: u32,
    stored_token: bool,
    refresh: bool,
    interactive: bool,
}

/// One pending HTTP post and its credential, replayed across auth retries.
struct HttpCall<'a> {
    body: &'a str,
    method: Option<&'a str>,
    name: Option<&'a str>,
    extra: &'a [(HeaderName, HeaderValue)],
    version: &'a str,
    token: Option<&'a str>,
}

/// Shared correlation state for one SSE event stream.
struct StreamContext<'a> {
    request_id: u64,
    original_id: u64,
    cancel: &'a CancellationToken,
    deadline: &'a mut CallDeadline,
    version: &'a str,
    token: Option<&'a str>,
}

/// Loop state for one JSON-RPC exchange across auth retries.
struct ExchangeState<'a> {
    deadline: CallDeadline,
    retry: AuthRetry,
    step_ups: u32,
    used_token: Option<String>,
    services: &'a dyn dal_agent::ext::Services,
    who: &'a dal_agent::ext::Caller,
    cancel: &'a CancellationToken,
}

/// One JSON-RPC exchange request as received by the HTTP transport.
pub(crate) struct ExchangeRequest<'a> {
    pub(crate) id: u64,
    pub(crate) ids: &'a AtomicU64,
    pub(crate) method: &'a str,
    pub(crate) params: &'a str,
    pub(crate) headers: &'a [HeaderAnnotation],
    pub(crate) arguments: Option<&'a RawJson>,
    pub(crate) version: &'a str,
    pub(crate) services: &'a dyn dal_agent::ext::Services,
    pub(crate) who: &'a dal_agent::ext::Caller,
    pub(crate) cancel: &'a CancellationToken,
}

impl ExchangeRequest<'_> {
    /// Builds the per-exchange retry state owned by the transport loop.
    fn state(&self, transport: &HttpTransport) -> ExchangeState<'_> {
        ExchangeState {
            deadline: CallDeadline::new(transport.call_timeout, transport.call_max),
            retry: AuthRetry::default(),
            step_ups: 0,
            used_token: None,
            services: self.services,
            who: self.who,
            cancel: self.cancel,
        }
    }
}

/// How a 401 was recovered.
enum Recovered {
    /// Resend with the credential now in the cache.
    Retry,
    /// The user authorized again; resend with a fresh call deadline.
    Reauthorized,
}

impl HttpTransport {
    /// Creates a streamable-HTTP client. Authentication begins after a 401.
    pub(crate) fn new(
        key: Key,
        url: Url,
        tokens_path: PathBuf,
        client_version: String,
        budgets: &Budgets,
        refreshes: Arc<token_auth::RefreshCoordinator>,
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
            tokens: Arc::new(Mutex::new(None)),
            verified_issuer: Mutex::new(None),
            session_id: Mutex::new(None),
            authorization: Mutex::new(token_auth::AuthorizationState::default()),
            refreshes,
        })
    }

    /// Posts one JSON-RPC method and returns its correlated response object.
    pub(crate) async fn exchange(
        &self,
        request: ExchangeRequest<'_>,
    ) -> Result<RawJson, TransportError> {
        let id = request.id;
        let mut state = request.state(self);
        let params_value = sonic_rs::from_str::<Value>(request.params)
            .map_err(|_| TransportError::Mcp(protocol_error("invalid MCP params".to_owned())))?;
        if params_value.as_object().is_none() {
            return Err(TransportError::Mcp(protocol_error(
                "MCP params must be a JSON object".to_owned(),
            )));
        }
        let extra = match request.arguments {
            Some(arguments) => parameter_headers(request.headers, arguments)
                .map_err(|error| TransportError::Mcp(protocol_error(error.to_string())))?,
            None => Vec::new(),
        };
        let tool_name = if request.method == "tools/call" {
            params_value
                .get("name")
                .and_then(JsonValueTrait::as_str)
                .map(protocol::escape_name)
        } else {
            None
        };
        let mut request_id = id;
        loop {
            if request.cancel.is_cancelled() {
                return Err(TransportError::Cancelled);
            }
            let body = request_body(
                request_id,
                request.method,
                request.params,
                request.version,
                &self.client_version,
            )
            .map_err(TransportError::Mcp)?;
            let token = self.bearer().await;
            state.used_token.clone_from(&token);
            let call = HttpCall {
                body: body.as_str(),
                method: Some(request.method),
                name: tool_name.as_deref(),
                extra: &extra,
                version: request.version,
                token: token.as_deref(),
            };
            let response = self
                .send_once(call, request.cancel, &state.deadline)
                .await?;
            self.capture_session(&response).await?;
            let status = response.status();
            if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                let retrying = self
                    .recover_status(response.headers(), status, &mut state)
                    .await?;
                if retrying {
                    request_id = request.ids.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                return Err(TransportError::Mcp(McpError::HttpAuth {
                    code: status.as_u16(),
                    n: state.retry.attempts,
                }));
            }
            let token = token.as_deref();
            return self
                .finish_exchange(response, id, request_id, request.version, token, &mut state)
                .await;
        }
    }

    /// Handles one 401 or 403 reply. Returns whether to resend the request.
    async fn recover_status(
        &self,
        response_headers: &reqwest::header::HeaderMap,
        status: StatusCode,
        state: &mut ExchangeState<'_>,
    ) -> Result<bool, TransportError> {
        let used_token = state.used_token.as_deref();
        if status == StatusCode::UNAUTHORIZED {
            let recovered = self
                .recover_unauthorized(
                    response_headers,
                    used_token,
                    &mut state.retry,
                    state.services,
                    state.who,
                    state.cancel,
                )
                .await?;
            if matches!(recovered, Recovered::Reauthorized) {
                state.deadline = CallDeadline::new(self.call_timeout, self.call_max);
            }
            return Ok(true);
        }
        let challenge = oauth::challenge(response_headers);
        if !challenge.insufficient_scope {
            return Ok(false);
        }
        if self.bearer().await.as_deref() != used_token && challenge.scope.is_none() {
            return Ok(true);
        }
        self.recover_step_up(
            &challenge,
            &mut state.step_ups,
            state.services,
            state.who,
            state.cancel,
        )
        .await?;
        state.deadline = CallDeadline::new(self.call_timeout, self.call_max);
        Ok(true)
    }

    /// Delivers a success reply, an event stream, or the final error status.
    async fn finish_exchange(
        &self,
        response: Response,
        id: u64,
        request_id: u64,
        version: &str,
        token: Option<&str>,
        state: &mut ExchangeState<'_>,
    ) -> Result<RawJson, TransportError> {
        let status = response.status();
        if status == StatusCode::ACCEPTED || status == StatusCode::NO_CONTENT {
            return Err(TransportError::Mcp(protocol_error(
                "MCP request completed without a response".to_owned(),
            )));
        }
        if !status.is_success() {
            return self
                .error_from_status(response, id, request_id, status, version, state)
                .await;
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if content_type.starts_with("text/event-stream") {
            let context = StreamContext {
                request_id,
                original_id: id,
                cancel: state.cancel,
                deadline: &mut state.deadline,
                version,
                token,
            };
            return self.read_event_stream(response, context).await;
        }
        let body = self
            .read_body(response, RESPONSE_MAX, state.cancel, &state.deadline)
            .await?;
        let text = std::str::from_utf8(&body).map_err(|_| {
            TransportError::Mcp(protocol_error("invalid MCP response encoding".to_owned()))
        })?;
        response_for_id(text, request_id, id).map_err(TransportError::Mcp)
    }

    /// Builds the final transport error from one non-retryable failed status.
    async fn error_from_status(
        &self,
        response: Response,
        id: u64,
        request_id: u64,
        status: StatusCode,
        version: &str,
        state: &ExchangeState<'_>,
    ) -> Result<RawJson, TransportError> {
        let body = self
            .read_body(response, ERROR_BODY_MAX, state.cancel, &state.deadline)
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
        Err(TransportError::Mcp(McpError::HttpAuth {
            code: status.as_u16(),
            n: state.retry.attempts,
        }))
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
        let mut state = ExchangeState {
            deadline: CallDeadline::new(self.call_timeout, self.call_timeout),
            retry: AuthRetry::default(),
            step_ups: 0,
            used_token: None,
            services,
            who,
            cancel,
        };
        loop {
            if cancel.is_cancelled() {
                return Err(TransportError::Cancelled);
            }
            let token = self.bearer().await;
            state.used_token.clone_from(&token);
            let call = HttpCall {
                body: body.as_str(),
                method: Some(method),
                name: None,
                extra: &[],
                version,
                token: token.as_deref(),
            };
            let response = self.send_once(call, cancel, &state.deadline).await?;
            self.capture_session(&response).await?;
            let status = response.status();
            if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                let retrying = self
                    .recover_status(response.headers(), status, &mut state)
                    .await?;
                if !retrying {
                    return Err(TransportError::Mcp(McpError::HttpAuth {
                        code: status.as_u16(),
                        n: state.retry.attempts,
                    }));
                }
                continue;
            }
            if status.is_success() || status == StatusCode::ACCEPTED {
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
                n: state.retry.attempts,
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
        call: HttpCall<'_>,
        cancel: &CancellationToken,
        deadline: &CallDeadline,
    ) -> Result<Response, TransportError> {
        if cancel.is_cancelled() {
            return Err(TransportError::Cancelled);
        }
        let session = self.session_id.lock().await.clone();
        let headers = outbound_headers(
            call.version,
            call.method,
            call.name,
            call.extra,
            session.as_deref(),
        )
        .map_err(TransportError::Mcp)?;
        let mut request = self
            .client
            .post(self.url.clone())
            .headers(headers)
            .body(call.body.to_owned());
        if let Some(token) = call.token {
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
        mut context: StreamContext<'_>,
    ) -> Result<RawJson, TransportError> {
        let mut parser = SseParser::default();
        loop {
            let chunk = tokio::select! {
                () = context.cancel.cancelled() => return Err(TransportError::Cancelled),
                chunk = tokio::time::timeout_at(context.deadline.expires, response.chunk()) => match chunk {
                    Ok(Ok(chunk)) => chunk,
                    Ok(Err(_)) => return Err(TransportError::Mcp(protocol_error("HTTP event stream read failed".to_owned()))),
                    Err(_) => return Err(context.deadline.timeout_error()),
                },
            };
            let Some(chunk) = chunk else {
                break;
            };
            for event in parser.push(&chunk).map_err(TransportError::Mcp)? {
                if let Some(reply) = self.process_event(&event, &mut context).await? {
                    return Ok(reply);
                }
            }
        }
        for event in parser.finish().map_err(TransportError::Mcp)? {
            if let Some(reply) = self.process_event(&event, &mut context).await? {
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
        context: &mut StreamContext<'_>,
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
                if token == Some(&format!("t-{}", context.request_id)) {
                    context.deadline.extend();
                }
                return Ok(None);
            }
            if value.get("id").is_some() {
                self.answer_server_request(&value, context.version, context.token, context.cancel)
                    .await;
            }
            return Ok(None);
        }
        if value.get("id").and_then(JsonValueTrait::as_u64) == Some(context.request_id) {
            return response_for_id(event, context.request_id, context.original_id)
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

    /// Recovers from one 401: adopts a newer credential, refreshes, or asks
    /// the user, within the bounds in `retry`.
    ///
    /// A refresh that fails with an error leaves no latch, so a later 401
    /// refreshes again. Only a refusal by the token endpoint or a declined
    /// prompt blocks further refreshes or prompts.
    async fn recover_unauthorized(
        &self,
        headers: &HeaderMap,
        used_token: Option<&str>,
        retry: &mut AuthRetry,
        services: &dyn dal_agent::ext::Services,
        who: &dal_agent::ext::Caller,
        cancel: &CancellationToken,
    ) -> Result<Recovered, TransportError> {
        let unauthorized = StatusCode::UNAUTHORIZED.as_u16();
        retry.attempts += 1;
        let challenge = oauth::challenge(headers);
        let mut authorization = self.authorization.lock().await;
        let current = self.bearer().await;
        if current.as_deref() != used_token {
            if used_token.is_some() && current.is_some() {
                *retry = AuthRetry::default();
                *authorization = token_auth::AuthorizationState::default();
            }
            return Ok(Recovered::Retry);
        }
        if retry.attempts > 3 {
            return Err(TransportError::Mcp(McpError::HttpAuth {
                code: unauthorized,
                n: retry.attempts - 1,
            }));
        }
        if authorization.cancelled {
            return Err(TransportError::Mcp(McpError::NoAskFrontEnd));
        }
        let discovery = self.discover(&challenge, cancel).await?;
        self.set_issuer(&discovery.issuer).await;
        let existing = self.record(&discovery.issuer, &discovery.resource).await?;
        let stored = existing
            .as_ref()
            .is_some_and(|record| !record.access_token.is_empty());
        if used_token.is_none() && !retry.stored_token && stored {
            retry.stored_token = true;
            return Ok(Recovered::Retry);
        }
        if !retry.refresh
            && !authorization.refresh_failed
            && let Some(record) = existing.as_ref()
            && record.refresh_token.is_some()
        {
            retry.refresh = true;
            if let Some(updated) = self.refresh_token(&discovery, record, cancel).await? {
                authorization.refresh_failed = used_token == Some(updated.access_token.as_str());
                return Ok(Recovered::Retry);
            }
            authorization.refresh_failed = true;
        }
        if retry.interactive {
            return Err(TransportError::Mcp(McpError::HttpAuth {
                code: unauthorized,
                n: retry.attempts,
            }));
        }
        retry.interactive = true;
        let updated = self
            .authorize(&discovery, existing.as_ref(), None, services, who, cancel)
            .await
            .inspect_err(|error| {
                if matches!(error, TransportError::Mcp(McpError::NoAskFrontEnd)) {
                    authorization.cancelled = true;
                }
            })?;
        self.persist(&discovery, updated).await?;
        *authorization = token_auth::AuthorizationState::default();
        Ok(Recovered::Reauthorized)
    }

    /// Prompts one step-up authorization for the credential.
    ///
    /// The refresh coordinator's keyed state is shared by every transport of
    /// the client, so concurrent step-ups prompt once, and a declined or
    /// cancelled prompt answers later challenges without asking again until
    /// a fresh token publishes.
    async fn recover_step_up(
        &self,
        challenge: &oauth::Challenge,
        step_ups: &mut u32,
        services: &dyn dal_agent::ext::Services,
        who: &dal_agent::ext::Caller,
        cancel: &CancellationToken,
    ) -> Result<(), TransportError> {
        if *step_ups >= STEPUP_MAX {
            return Err(TransportError::Mcp(McpError::StepUpLimit));
        }
        *step_ups += 1;
        let discovery = self.discover(challenge, cancel).await?;
        let key = token_auth::refresh_key(&discovery.issuer, &discovery.resource);
        let to_mcp = |error: TransportError| match error {
            TransportError::Mcp(error) => error,
            TransportError::Cancelled => McpError::NoAskFrontEnd,
        };
        let result = self
            .refreshes
            .interactive_section(&key, async || {
                let existing = self
                    .record(&discovery.issuer, &discovery.resource)
                    .await
                    .map_err(to_mcp)?;
                let updated = self
                    .authorize(
                        &discovery,
                        existing.as_ref(),
                        challenge.scope.as_deref(),
                        services,
                        who,
                        cancel,
                    )
                    .await
                    .map_err(to_mcp)?;
                self.persist(&discovery, updated).await.map_err(to_mcp)
            })
            .await;
        if matches!(result, Err(McpError::NoAskFrontEnd)) {
            let mut authorization = self.authorization.lock().await;
            authorization.cancelled = true;
        }
        result.map_err(TransportError::Mcp)?;
        let mut authorization = self.authorization.lock().await;
        *authorization = token_auth::AuthorizationState::default();
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

    async fn refresh_token(
        &self,
        discovery: &Discovery,
        record: &token_auth::TokenRecord,
        cancel: &CancellationToken,
    ) -> Result<Option<token_auth::TokenRecord>, TransportError> {
        let path = self.tokens_path.clone();
        let issuer = discovery.issuer.clone();
        let resource = discovery.resource.clone();
        let cache = Arc::clone(&self.tokens);
        let cache_issuer = issuer.clone();
        let cache_resource = resource.clone();
        let persist = move |record: token_auth::TokenRecord| async move {
            let cache_record = record.clone();
            let result = tokio::task::spawn_blocking(move || {
                token_auth::persist_token(&path, &issuer, &resource, record)
            })
            .await
            .map_err(|_| McpError::Auth {
                cause: "token persistence failed".to_owned(),
            })?;
            result?;
            let mut tokens = cache.lock().await;
            tokens
                .get_or_insert_with(token_auth::TokenFile::default)
                .tokens
                .entry(cache_issuer)
                .or_default()
                .insert(cache_resource, cache_record);
            Ok(())
        };
        let updated = oauth::refresh(
            &self.refreshes,
            &self.client,
            discovery,
            record,
            self.connect_timeout,
            cancel,
            persist,
        )
        .await
        .map_err(TransportError::Mcp)?;
        if let Some(record) = updated.as_ref() {
            self.remember(discovery, record.clone()).await;
        }
        Ok(updated)
    }

    /// Persists an interactively authorized record and publishes it to the
    /// refresh coordinator, so a transport still holding an older access
    /// token adopts it instead of a stale cached refresh result.
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
        let key = token_auth::refresh_key(&discovery.issuer, &discovery.resource);
        self.refreshes.publish(&key, record.clone()).await;
        self.remember(discovery, record).await;
        Ok(())
    }

    async fn remember(&self, discovery: &Discovery, record: token_auth::TokenRecord) {
        let mut tokens = self.load_tokens().await;
        tokens
            .tokens
            .entry(discovery.issuer.clone())
            .or_default()
            .insert(discovery.resource.clone(), record);
        *self.tokens.lock().await = Some(tokens);
        self.set_issuer(&discovery.issuer).await;
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
            oauth::authorize(oauth::AuthorizeSpec {
                client: &self.client,
                target: &self.url,
                discovery,
                existing,
                requested_scope: scope,
                client_version: &self.client_version,
                services,
                who,
                cancel,
            }),
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
