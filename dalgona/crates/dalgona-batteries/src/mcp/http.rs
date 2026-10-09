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
        oauth::{Challenge, Discovery, NetworkPolicy, OAuthClient},
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

/// The method, protocol version, and service boundary one request runs under.
pub(crate) struct CallCx<'a> {
    pub method: &'a str,
    pub version: &'a str,
    pub services: &'a dyn dal_agent::ext::Services,
    pub who: &'a dal_agent::ext::Caller,
    pub cancel: &'a CancellationToken,
}

/// One request loop's retry and authentication ledger.
struct AuthLedger {
    request_id: u64,
    used_token: Option<String>,
    attempts: u32,
    step_ups: u32,
    stored_token_attempted: bool,
    refresh_attempted: bool,
    interactive_attempted: bool,
}

impl AuthLedger {
    fn new(request_id: u64) -> Self {
        Self {
            request_id,
            used_token: None,
            attempts: 0,
            step_ups: 0,
            stored_token_attempted: false,
            refresh_attempted: false,
            interactive_attempted: false,
        }
    }

    /// Forgets the recovery bounds spent on an older credential.
    fn forget_attempts(&mut self) {
        self.attempts = 0;
        self.stored_token_attempted = false;
        self.refresh_attempted = false;
        self.interactive_attempted = false;
    }
}

/// One pass over a rejected response.
enum AuthStep {
    /// Retry the request; `reset_deadline` is set when authorization completed.
    Retry { reset_deadline: bool },
    /// Surface this terminal failure.
    Fail(TransportError),
}

/// One POST's protocol version, bearer token, and deadline.
struct SendCx<'a> {
    version: &'a str,
    token: Option<String>,
    cancel: &'a CancellationToken,
    deadline: &'a CallDeadline,
}

/// Correlation and budget context for one streamed reply.
struct StreamCx<'a> {
    request_id: u64,
    original_id: u64,
    version: &'a str,
    token: Option<String>,
    cancel: &'a CancellationToken,
    deadline: &'a mut CallDeadline,
}

/// Streamable-HTTP transport state for one MCP server instance.
pub(crate) struct HttpTransport {
    key: Key,
    url: Url,
    client: Client,
    oauth: OAuthClient,
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
        let oauth_policy = NetworkPolicy::for_target(&url);
        let client = Client::builder()
            .connect_timeout(budgets.start)
            .redirect(Policy::none())
            .build()
            .map_err(|_| McpError::Start {
                key: key.display(),
                cause: "HTTP client creation failed".to_owned(),
            })?;
        let oauth_client = Client::builder()
            .connect_timeout(budgets.start)
            .redirect(Policy::none())
            .no_proxy()
            .dns_resolver(oauth_policy.resolver())
            .build()
            .map_err(|_| McpError::Start {
                key: key.display(),
                cause: "OAuth HTTP client creation failed".to_owned(),
            })?;
        let oauth = OAuthClient {
            client: oauth_client,
            policy: oauth_policy,
        };
        Ok(Self {
            key,
            url,
            client,
            oauth,
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
        id: u64,
        ids: &AtomicU64,
        params: &str,
        headers: &[HeaderAnnotation],
        arguments: Option<&RawJson>,
        call: &CallCx<'_>,
    ) -> Result<RawJson, TransportError> {
        let ExchangeHead { extra, tool_name } =
            exchange_preamble(params, headers, arguments, call.method)?;
        let mut deadline = CallDeadline::new(self.call_timeout, self.call_max);
        let mut ledger = AuthLedger::new(id);
        loop {
            if call.cancel.is_cancelled() {
                return Err(TransportError::Cancelled);
            }
            let body = request_body(
                ledger.request_id,
                call.method,
                params,
                call.version,
                &self.client_version,
            )
            .map_err(TransportError::Mcp)?;
            let send = self.send_frame(&mut ledger, call, &deadline).await;
            let response = self
                .send_once(
                    body.as_str(),
                    Some(call.method),
                    tool_name.as_deref(),
                    &extra,
                    &send,
                )
                .await?;
            self.capture_session(&response).await?;
            let status = response.status();
            if status == StatusCode::UNAUTHORIZED {
                match self.unauthorized(&response, &mut ledger, call, ids).await? {
                    AuthStep::Retry { reset_deadline } => {
                        if reset_deadline {
                            deadline = CallDeadline::new(self.call_timeout, self.call_max);
                        }
                        continue;
                    }
                    AuthStep::Fail(error) => return Err(error),
                }
            }
            if status == StatusCode::FORBIDDEN {
                match self.forbidden(&response, &mut ledger, call, ids).await? {
                    AuthStep::Retry { reset_deadline } => {
                        if reset_deadline {
                            deadline = CallDeadline::new(self.call_timeout, self.call_max);
                        }
                        continue;
                    }
                    AuthStep::Fail(error) => return Err(error),
                }
            }
            if status == StatusCode::ACCEPTED || status == StatusCode::NO_CONTENT {
                return Err(TransportError::Mcp(protocol_error(
                    "MCP request completed without a response".to_owned(),
                )));
            }
            if !status.is_success() {
                let body = self
                    .read_body(response, ERROR_BODY_MAX, call.cancel, &deadline)
                    .await?;
                let text = String::from_utf8_lossy(&body).into_owned();
                return error_status(
                    status,
                    call.version,
                    ledger.request_id,
                    id,
                    &text,
                    ledger.attempts,
                );
            }
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if content_type.starts_with("text/event-stream") {
                let mut stream = StreamCx {
                    request_id: ledger.request_id,
                    original_id: id,
                    version: call.version,
                    token: send.token.clone(),
                    cancel: call.cancel,
                    deadline: &mut deadline,
                };
                return self.read_event_stream(response, &mut stream).await;
            }
            let body = self
                .read_body(response, RESPONSE_MAX, call.cancel, &deadline)
                .await?;
            let text = std::str::from_utf8(&body).map_err(|_| {
                TransportError::Mcp(protocol_error("invalid MCP response encoding".to_owned()))
            })?;
            return response_for_id(text, ledger.request_id, id).map_err(TransportError::Mcp);
        }
    }

    /// Refreshes the bearer token and assembles this attempt's send context.
    async fn send_frame<'a>(
        &self,
        ledger: &mut AuthLedger,
        call: &CallCx<'a>,
        deadline: &'a CallDeadline,
    ) -> SendCx<'a> {
        let token = self.bearer().await;
        ledger.used_token.clone_from(&token);
        SendCx {
            version: call.version,
            token: ledger.used_token.clone(),
            cancel: call.cancel,
            deadline,
        }
    }

    /// Recovers from one 401: adopts a newer credential, refreshes, or asks
    /// the user, within the bounds in `ledger`.
    ///
    /// A refresh that fails with an error leaves no latch, so a later 401
    /// refreshes again. Only a refusal by the token endpoint or a declined
    /// prompt blocks further refreshes or prompts.
    async fn unauthorized(
        &self,
        response: &Response,
        ledger: &mut AuthLedger,
        call: &CallCx<'_>,
        ids: &AtomicU64,
    ) -> Result<AuthStep, TransportError> {
        ledger.attempts += 1;
        let challenge = oauth::challenge(response.headers());
        let mut authorization = self.authorization.lock().await;
        let current = self.bearer().await;
        if current != ledger.used_token {
            if ledger.used_token.is_some() && current.is_some() {
                ledger.forget_attempts();
                *authorization = token_auth::AuthorizationState::default();
            }
            ledger.request_id = ids.fetch_add(1, Ordering::Relaxed);
            return Ok(AuthStep::Retry {
                reset_deadline: false,
            });
        }
        if ledger.attempts > 3 {
            return Ok(AuthStep::Fail(TransportError::Mcp(McpError::HttpAuth {
                code: StatusCode::UNAUTHORIZED.as_u16(),
                n: ledger.attempts - 1,
            })));
        }
        if authorization.cancelled {
            return Ok(AuthStep::Fail(TransportError::Mcp(McpError::NoAskFrontEnd)));
        }
        let discovery = self.discover(&challenge, call.cancel).await?;
        self.set_issuer(&discovery.issuer).await;
        let existing = self.record(&discovery.issuer, &discovery.resource).await?;
        if ledger.used_token.is_none()
            && !ledger.stored_token_attempted
            && let Some(record) = existing.as_ref()
            && !record.access_token.is_empty()
        {
            ledger.stored_token_attempted = true;
            ledger.request_id = ids.fetch_add(1, Ordering::Relaxed);
            return Ok(AuthStep::Retry {
                reset_deadline: false,
            });
        }
        if !ledger.refresh_attempted
            && !authorization.refresh_failed
            && let Some(record) = existing.as_ref()
            && record.refresh_token.is_some()
        {
            ledger.refresh_attempted = true;
            if let Some(updated) = self.refresh_token(&discovery, record, call.cancel).await? {
                authorization.refresh_failed =
                    ledger.used_token.as_deref() == Some(updated.access_token.as_str());
                ledger.request_id = ids.fetch_add(1, Ordering::Relaxed);
                return Ok(AuthStep::Retry {
                    reset_deadline: false,
                });
            }
            authorization.refresh_failed = true;
        }
        if ledger.interactive_attempted {
            return Ok(AuthStep::Fail(TransportError::Mcp(McpError::HttpAuth {
                code: StatusCode::UNAUTHORIZED.as_u16(),
                n: ledger.attempts,
            })));
        }
        ledger.interactive_attempted = true;
        let updated = self
            .authorize(&discovery, existing.as_ref(), None, call)
            .await
            .inspect_err(|error| {
                if matches!(error, TransportError::Mcp(McpError::NoAskFrontEnd)) {
                    authorization.cancelled = true;
                }
            })?;
        self.persist(&discovery, updated).await?;
        *authorization = token_auth::AuthorizationState::default();
        ledger.request_id = ids.fetch_add(1, Ordering::Relaxed);
        Ok(AuthStep::Retry {
            reset_deadline: true,
        })
    }

    /// Runs the 403 insufficient-scope ladder toward one interactive step-up.
    async fn forbidden(
        &self,
        response: &Response,
        ledger: &mut AuthLedger,
        call: &CallCx<'_>,
        ids: &AtomicU64,
    ) -> Result<AuthStep, TransportError> {
        let challenge = oauth::challenge(response.headers());
        if !challenge.insufficient_scope {
            return Ok(AuthStep::Fail(TransportError::Mcp(McpError::HttpAuth {
                code: StatusCode::FORBIDDEN.as_u16(),
                n: ledger.attempts,
            })));
        }
        if self.bearer().await != ledger.used_token && challenge.scope.is_none() {
            ledger.request_id = ids.fetch_add(1, Ordering::Relaxed);
            return Ok(AuthStep::Retry {
                reset_deadline: false,
            });
        }
        self.recover_step_up(&challenge, &mut ledger.step_ups, call)
            .await?;
        ledger.request_id = ids.fetch_add(1, Ordering::Relaxed);
        Ok(AuthStep::Retry {
            reset_deadline: true,
        })
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
        call: &CallCx<'_>,
    ) -> Result<(), TransportError> {
        if *step_ups >= STEPUP_MAX {
            return Err(TransportError::Mcp(McpError::StepUpLimit));
        }
        *step_ups += 1;
        let discovery = self.discover(challenge, call.cancel).await?;
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
                        call,
                    )
                    .await
                    .map_err(to_mcp)?;
                self.persist(&discovery, updated).await.map_err(to_mcp)
            })
            .await;
        result.map_err(TransportError::Mcp)?;
        let mut authorization = self.authorization.lock().await;
        *authorization = token_auth::AuthorizationState::default();
        Ok(())
    }

    /// Posts a JSON-RPC notification, including authentication retries.
    pub(crate) async fn notify(
        &self,
        ids: &AtomicU64,
        call: &CallCx<'_>,
    ) -> Result<(), TransportError> {
        let body = notification_body(call.method, call.version, &self.client_version)
            .map_err(TransportError::Mcp)?;
        let mut deadline = CallDeadline::new(self.call_timeout, self.call_timeout);
        let mut ledger = AuthLedger::new(0);
        loop {
            if call.cancel.is_cancelled() {
                return Err(TransportError::Cancelled);
            }
            let token = self.bearer().await;
            ledger.used_token.clone_from(&token);
            let send = SendCx {
                version: call.version,
                token,
                cancel: call.cancel,
                deadline: &deadline,
            };
            let response = self
                .send_once(body.as_str(), Some(call.method), None, &[], &send)
                .await?;
            self.capture_session(&response).await?;
            let status = response.status();
            let step = match status {
                StatusCode::UNAUTHORIZED => {
                    Some(self.unauthorized(&response, &mut ledger, call, ids).await?)
                }
                StatusCode::FORBIDDEN => {
                    Some(self.forbidden(&response, &mut ledger, call, ids).await?)
                }
                _ => None,
            };
            match step {
                Some(AuthStep::Retry { reset_deadline }) => {
                    if reset_deadline {
                        deadline = CallDeadline::new(self.call_timeout, self.call_timeout);
                    }
                    continue;
                }
                Some(AuthStep::Fail(error)) => return Err(error),
                None => {}
            }
            if status.is_success() || status == StatusCode::ACCEPTED {
                return Ok(());
            }
            if matches!(
                status,
                StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
            ) {
                return Err(TransportError::Mcp(
                    if call.version == LEGACY_PROTOCOL_VERSION {
                        McpError::Auth {
                            cause: "unsupported HTTP+SSE-only transport".to_owned(),
                        }
                    } else {
                        McpError::Protocol {
                            code: -32601,
                            message: format!("HTTP endpoint returned {}", status.as_u16()),
                        }
                    },
                ));
            }
            return Err(TransportError::Mcp(McpError::HttpAuth {
                code: status.as_u16(),
                n: ledger.attempts,
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
        send: &SendCx<'_>,
    ) -> Result<Response, TransportError> {
        if send.cancel.is_cancelled() {
            return Err(TransportError::Cancelled);
        }
        let session = self.session_id.lock().await.clone();
        let headers = outbound_headers(send.version, method, name, extra, session.as_deref())
            .map_err(TransportError::Mcp)?;
        let mut request = self
            .client
            .post(self.url.clone())
            .headers(headers)
            .body(body.to_owned());
        if let Some(token) = send.token.as_deref() {
            request = request.bearer_auth(token);
        }
        tokio::select! {
            () = send.cancel.cancelled() => Err(TransportError::Cancelled),
            response = tokio::time::timeout_at(send.deadline.expires, request.send()) => match response {
                Ok(Ok(response)) => Ok(response),
                Ok(Err(_)) => Err(TransportError::Mcp(protocol_error(format!(
                    "HTTP request failed for {}",
                    self.key.display()
                )))),
                Err(_) => Err(send.deadline.timeout_error()),
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
        stream: &mut StreamCx<'_>,
    ) -> Result<RawJson, TransportError> {
        let mut parser = SseParser::default();
        loop {
            let chunk = tokio::select! {
                () = stream.cancel.cancelled() => return Err(TransportError::Cancelled),
                chunk = tokio::time::timeout_at(stream.deadline.expires, response.chunk()) => match chunk {
                    Ok(Ok(chunk)) => chunk,
                    Ok(Err(_)) => return Err(TransportError::Mcp(protocol_error("HTTP event stream read failed".to_owned()))),
                    Err(_) => return Err(stream.deadline.timeout_error()),
                },
            };
            let Some(chunk) = chunk else {
                break;
            };
            for event in parser.push(&chunk).map_err(TransportError::Mcp)? {
                if let Some(reply) = self.process_event(&event, stream).await? {
                    return Ok(reply);
                }
            }
        }
        for event in parser.finish().map_err(TransportError::Mcp)? {
            if let Some(reply) = self.process_event(&event, stream).await? {
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
        stream: &mut StreamCx<'_>,
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
                if token == Some(&format!("t-{}", stream.request_id)) {
                    stream.deadline.extend();
                }
                return Ok(None);
            }
            if value.get("id").is_some() {
                self.answer_server_request(&value, stream).await;
            }
            return Ok(None);
        }
        if value.get("id").and_then(JsonValueTrait::as_u64) == Some(stream.request_id) {
            return response_for_id(event, stream.request_id, stream.original_id)
                .map(Some)
                .map_err(TransportError::Mcp);
        }
        Ok(None)
    }

    async fn answer_server_request(&self, value: &Value, stream: &StreamCx<'_>) {
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
        let Ok(headers) = outbound_headers(stream.version, None, None, &[], session.as_deref())
        else {
            return;
        };
        let mut request = self
            .client
            .post(self.url.clone())
            .headers(headers)
            .body(body.as_str().to_owned());
        if let Some(token) = stream.token.as_deref() {
            request = request.bearer_auth(token);
        }
        let _ = tokio::select! {
            () = stream.cancel.cancelled() => None,
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
            &self.oauth,
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
            &self.oauth,
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
        call: &CallCx<'_>,
    ) -> Result<token_auth::TokenRecord, TransportError> {
        tokio::time::timeout(
            self.stepup_timeout,
            oauth::authorize(
                &self.oauth,
                &oauth::AuthorizePlan {
                    target: &self.url,
                    discovery,
                    existing,
                    requested_scope: scope,
                    client_version: &self.client_version,
                },
                call.services,
                call.who,
                call.cancel,
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

/// The per-request headers and escaped tool name one exchange resolves once.
struct ExchangeHead {
    extra: Vec<(HeaderName, HeaderValue)>,
    tool_name: Option<String>,
}

/// Validates params and derives headers plus the escaped tool name.
fn exchange_preamble(
    params: &str,
    headers: &[HeaderAnnotation],
    arguments: Option<&RawJson>,
    method: &str,
) -> Result<ExchangeHead, TransportError> {
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
    Ok(ExchangeHead { extra, tool_name })
}

/// Maps a non-success reply to the surfaced outcome or transport error.
fn error_status(
    status: StatusCode,
    version: &str,
    request_id: u64,
    id: u64,
    text: &str,
    attempts: u32,
) -> Result<RawJson, TransportError> {
    if recognizes_modern_error(text) {
        return response_for_id(text, request_id, id).map_err(TransportError::Mcp);
    }
    if matches!(
        status,
        StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
    ) {
        return Err(TransportError::Mcp(if version == LEGACY_PROTOCOL_VERSION {
            McpError::Auth {
                cause: "unsupported HTTP+SSE-only transport".to_owned(),
            }
        } else {
            McpError::Protocol {
                code: -32601,
                message: format!("HTTP endpoint returned {}", status.as_u16()),
            }
        }));
    }
    Err(TransportError::Mcp(McpError::HttpAuth {
        code: status.as_u16(),
        n: attempts,
    }))
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
