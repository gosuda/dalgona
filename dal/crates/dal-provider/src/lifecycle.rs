//! One owner for provider request attempts, retry decisions, refresh, and admission.
//!
//! A family adapter supplies exactly one attempt and preserves HTTP rejection
//! metadata in [`AttemptFailure::Response`]. This module delegates timeout
//! enforcement to `http::send` and its `Exchange` (the client owns connect and
//! idle bounds); it owns the attempt counter, retry delay, OAuth refresh, and
//! provider semaphore permit. WebSocket session fallback is a separate Codex
//! contract and is not routed through this HTTP lifecycle.

use std::{borrow::Cow, future::Future, sync::Arc, time::Duration};

use dal_core::Family;
use futures::stream;
use jiff::{Timestamp, fmt::rfc2822};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    auth::credential::{Credential, OAuthCredential},
    error::ProviderError,
    retry::{self, RequestState, RetryDecision},
    stream::{EventStream, NoticeSink, StreamEvent},
};

const MAX_CONFIGURED_RETRIES: u32 = 100;

/// Clock and sleep source used by the lifecycle. The default implementation
/// uses Tokio time; tests can provide a deterministic implementation.
pub(crate) trait Clock: Clone + Send + Sync + 'static {
    /// Future returned by [`Clock::sleep`].
    type Sleep: Future<Output = ()> + Send;

    /// Current wall-clock instant, used only for HTTP-date `Retry-After`.
    fn now(&self) -> Timestamp;

    /// Waits for `duration` on this clock.
    fn sleep(&self, duration: Duration) -> Self::Sleep;
}

/// The production clock backed by system time and Tokio's timer.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TokioClock;

impl Clock for TokioClock {
    type Sleep = tokio::time::Sleep;

    fn now(&self) -> Timestamp {
        Timestamp::now()
    }

    fn sleep(&self, duration: Duration) -> Self::Sleep {
        tokio::time::sleep(duration)
    }
}

/// Inputs shared by the single-attempt factory and the lifecycle.
///
/// `permits` is the per-provider semaphore from provider configuration. The
/// lifecycle holds one permit from admission through the terminal event, retry
/// sleeps included. The configured retry count is the number of attempts after
/// the initial attempt and is capped at the provider config maximum of 100.
pub(crate) struct Plan<C = TokioClock> {
    pub(crate) family: Family,
    pub(crate) provider: Box<str>,
    pub(crate) model: Box<str>,
    pub(crate) max_retries: u32,
    pub(crate) permits: Arc<Semaphore>,
    pub(crate) cancel: CancellationToken,
    pub(crate) notices: NoticeSink,
    clock: C,
}

impl Plan<TokioClock> {
    /// Creates a lifecycle plan using Tokio time.
    pub(crate) fn new(
        family: Family,
        provider: impl Into<Box<str>>,
        model: impl Into<Box<str>>,
        max_retries: u32,
        permits: Arc<Semaphore>,
        cancel: CancellationToken,
        notices: NoticeSink,
    ) -> Self {
        Self {
            family,
            provider: provider.into(),
            model: model.into(),
            max_retries: max_retries.min(MAX_CONFIGURED_RETRIES),
            permits,
            cancel,
            notices,
            clock: TokioClock,
        }
    }
}

impl<C: Clock> Plan<C> {
    fn with_clock<D: Clock>(self, clock: D) -> Plan<D> {
        Plan {
            family: self.family,
            provider: self.provider,
            model: self.model,
            max_retries: self.max_retries,
            permits: self.permits,
            cancel: self.cancel,
            notices: self.notices,
            clock,
        }
    }
}

/// Failure facts from one family-specific attempt.
///
/// HTTP adapters must preserve `status`, provider `code`, response `message`,
/// and the raw `Retry-After` header instead of reducing them to a display
/// string. Stream decoders can return a typed provider error after decoding.
#[derive(Debug)]
pub(crate) enum AttemptFailure {
    /// An error already mapped by a transport or stream decoder.
    Provider(ProviderError),
    /// An HTTP response that the family adapter rejected.
    Response {
        /// HTTP response status code, including 3xx responses not followed.
        status: u16,
        /// Provider error code or type, when present in the body.
        code: Option<String>,
        /// Provider message, kept verbatim for `retry::classify`.
        message: String,
        /// Raw `Retry-After` header: delta-seconds or an HTTP date.
        retry_after: Option<String>,
    },
}

impl From<ProviderError> for AttemptFailure {
    fn from(error: ProviderError) -> Self {
        Self::Provider(error)
    }
}

/// Runs one non-streaming request with bounded retries and one OAuth refresh.
///
/// `attempt` must perform one request only. Use `http::send` with
/// `Exchange::Json` for its connect, header, and total-request deadlines. A
/// cancelled request returns `Ok(None)`; a provider failure is returned as a
/// typed `ProviderError` after the configured retry budget is exhausted.
pub(crate) async fn request<T, A, AF, R, RF, C>(
    plan: Plan<C>,
    credential: Credential,
    mut attempt: A,
    mut refresh: R,
) -> Result<Option<T>, ProviderError>
where
    T: Send,
    A: FnMut(&Credential, &CancellationToken) -> AF + Send,
    AF: Future<Output = Result<Option<T>, AttemptFailure>> + Send,
    R: FnMut(OAuthCredential) -> RF + Send,
    RF: Future<Output = Result<Credential, ProviderError>> + Send,
    C: Clock,
{
    let mut lifecycle = Lifecycle::new(plan, credential);
    match lifecycle.acquire().await? {
        Acquire::Ready => {}
        Acquire::Cancelled => return Ok(None),
    }

    loop {
        let cancel = lifecycle.plan.cancel.clone();
        let outcome = tokio::select! {
            biased;
            () = cancel.cancelled() => AttemptOutcome::Cancelled,
            result = attempt(&lifecycle.credential, &cancel) => AttemptOutcome::Result(result),
        };
        match outcome {
            AttemptOutcome::Cancelled => return Ok(None),
            AttemptOutcome::Result(Ok(Some(value))) => return Ok(Some(value)),
            AttemptOutcome::Result(Ok(None)) => return Ok(None),
            AttemptOutcome::Result(Err(failure)) => {
                match lifecycle.recover(failure, &mut refresh).await? {
                    Recovery::Retry => {}
                    Recovery::Cancelled => return Ok(None),
                }
            }
        }
    }
}

/// Runs a streaming request and returns its neutral, terminal-guarded stream.
///
/// The factory performs exactly one connection/response attempt. Its stream
/// may be retried only while no neutral event has reached the consumer. Any
/// forwarded event sets the delivery barrier, including reasoning, replay,
/// call-start, tool-argument, usage, and terminal events. Cancellation emits
/// no terminal event: it drops the active attempt and remains pending until the
/// caller drops the stream.
pub(crate) fn stream<A, AF, R, RF, C>(
    plan: Plan<C>,
    credential: Credential,
    attempt: A,
    refresh: R,
) -> EventStream
where
    A: FnMut(&Credential, &CancellationToken) -> AF + Send + 'static,
    AF: Future<Output = Result<Option<EventStream>, AttemptFailure>> + Send + 'static,
    R: FnMut(OAuthCredential) -> RF + Send + 'static,
    RF: Future<Output = Result<Credential, ProviderError>> + Send + 'static,
    C: Clock,
{
    let state = StreamLifecycle {
        lifecycle: Lifecycle::new(plan, credential),
        attempt,
        refresh,
        current: None,
        terminal: false,
        _attempt_future: std::marker::PhantomData,
    };
    let source = stream::unfold(state, |mut state| async move {
        state.next().await.map(|event| (event, state))
    });
    EventStream::new(source, || {})
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Acquire {
    Ready,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Recovery {
    Retry,
    Cancelled,
}

enum AttemptOutcome<T> {
    Result(Result<Option<T>, AttemptFailure>),
    Cancelled,
}

struct Lifecycle<C> {
    plan: Plan<C>,
    credential: Credential,
    retries: u32,
    delivered: bool,
    refreshed: bool,
    permit: Option<OwnedSemaphorePermit>,
}

impl<C: Clock> Lifecycle<C> {
    fn new(plan: Plan<C>, credential: Credential) -> Self {
        Self {
            plan,
            credential,
            retries: 0,
            delivered: false,
            refreshed: false,
            permit: None,
        }
    }

    async fn acquire(&mut self) -> Result<Acquire, ProviderError> {
        if self.permit.is_some() {
            return Ok(Acquire::Ready);
        }
        let permits = Arc::clone(&self.plan.permits);
        let cancel = &self.plan.cancel;
        tokio::select! {
            biased;
            () = cancel.cancelled() => Ok(Acquire::Cancelled),
            result = permits.acquire_owned() => match result {
                Ok(permit) => {
                    self.permit = Some(permit);
                    Ok(Acquire::Ready)
                }
                Err(_) => Err(ProviderError::Transport {
                    family: self.plan.family,
                    reason: String::from("provider request admission is closed"),
                }),
            },
        }
    }

    async fn recover<R, RF>(
        &mut self,
        failure: AttemptFailure,
        refresh: &mut R,
    ) -> Result<Recovery, ProviderError>
    where
        R: FnMut(OAuthCredential) -> RF,
        RF: Future<Output = Result<Credential, ProviderError>> + Send,
    {
        let state = RequestState {
            family: self.plan.family,
            provider: &self.plan.provider,
            model: &self.plan.model,
            oauth: matches!(&self.credential, Credential::OAuth(_)),
            refreshed: self.refreshed,
            delivered: self.delivered,
        };
        let (decision, retry_after, message) = classify_failure(failure, &state);
        match decision {
            RetryDecision::Fail(error) => {
                drop(self.permit.take());
                Err(error)
            }
            RetryDecision::RefreshOnce => {
                let Credential::OAuth(held) = &self.credential else {
                    drop(self.permit.take());
                    return Err(ProviderError::AuthRejected {
                        provider: String::from(self.plan.provider.as_ref()),
                    });
                };
                self.refreshed = true;
                let cancel = self.plan.cancel.clone();
                let outcome = tokio::select! {
                    biased;
                    () = cancel.cancelled() => RefreshOutcome::Cancelled,
                    result = refresh(held.clone()) => RefreshOutcome::Result(result),
                };
                match outcome {
                    RefreshOutcome::Cancelled => Ok(Recovery::Cancelled),
                    RefreshOutcome::Result(Ok(credential)) => {
                        self.credential = credential;
                        Ok(Recovery::Retry)
                    }
                    RefreshOutcome::Result(Err(error)) => {
                        drop(self.permit.take());
                        Err(error)
                    }
                }
            }
            RetryDecision::Retry(error) => {
                let retry_after = retry_after
                    .as_deref()
                    .and_then(|value| retry_after_seconds(value, self.plan.clock.now()));
                let attempt = self.retries.saturating_add(1);
                if self.retries >= self.plan.max_retries {
                    drop(self.permit.take());
                    let wait = match retry::delay_for_attempt(attempt, retry_after, 1.0) {
                        Ok(wait) => wait,
                        Err(too_long) => {
                            return Err(too_long.into_error(message.unwrap_or_default()));
                        }
                    };
                    // Only this final response's own usable wait becomes the hint.
                    let server_wait = retry_after.is_some_and(|secs| !secs.is_nan()).then_some(wait);
                    return Err(match error {
                        ProviderError::RateLimited { message, .. } => ProviderError::RateLimited {
                            message,
                            retry_after: server_wait,
                        },
                        error => error,
                    });
                }
                let delay =
                    match retry::delay_for_attempt(attempt, retry_after, retry_jitter()) {
                        Ok(delay) => delay,
                        Err(too_long) => {
                            drop(self.permit.take());
                            return Err(too_long.into_error(message.unwrap_or_default()));
                        }
                    };
                (self.plan.notices)(format!(
                    "Retrying in {:.1}s (attempt {attempt}/{}).",
                    delay.as_secs_f64(),
                    self.plan.max_retries
                ));
                let cancel = self.plan.cancel.clone();
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => Ok(Recovery::Cancelled),
                    () = self.plan.clock.sleep(delay) => {
                        self.retries = self.retries.saturating_add(1);
                        Ok(Recovery::Retry)
                    }
                }
            }
        }
    }
}

enum RefreshOutcome {
    Result(Result<Credential, ProviderError>),
    Cancelled,
}

struct StreamLifecycle<A, AF, R, RF, C> {
    lifecycle: Lifecycle<C>,
    attempt: A,
    refresh: R,
    current: Option<EventStream>,
    terminal: bool,
    _attempt_future: std::marker::PhantomData<fn() -> (AF, RF)>,
}

impl<A, AF, R, RF, C> StreamLifecycle<A, AF, R, RF, C>
where
    A: FnMut(&Credential, &CancellationToken) -> AF + Send,
    AF: Future<Output = Result<Option<EventStream>, AttemptFailure>> + Send,
    R: FnMut(OAuthCredential) -> RF + Send,
    RF: Future<Output = Result<Credential, ProviderError>> + Send,
    C: Clock,
{
    async fn next(&mut self) -> Option<Result<StreamEvent, ProviderError>> {
        if self.terminal {
            return None;
        }
        loop {
            if self.current.is_none() {
                match self.lifecycle.acquire().await {
                    Ok(Acquire::Ready) => {}
                    Ok(Acquire::Cancelled) => return self.cancel_pending().await,
                    Err(error) => return self.fail(error),
                }
                let cancel = self.lifecycle.plan.cancel.clone();
                let outcome = tokio::select! {
                    biased;
                    () = cancel.cancelled() => AttemptOutcome::Cancelled,
                    result = (self.attempt)(&self.lifecycle.credential, &cancel) => {
                        AttemptOutcome::Result(result)
                    }
                };
                match outcome {
                    AttemptOutcome::Cancelled => return self.cancel_pending().await,
                    AttemptOutcome::Result(Ok(Some(stream))) => self.current = Some(stream),
                    AttemptOutcome::Result(Ok(None)) => return self.cancel_pending().await,
                    AttemptOutcome::Result(Err(failure)) => {
                        match self.lifecycle.recover(failure, &mut self.refresh).await {
                            Ok(Recovery::Retry) => continue,
                            Ok(Recovery::Cancelled) => return self.cancel_pending().await,
                            Err(error) => return self.fail(error),
                        }
                    }
                }
            }

            let cancel = self.lifecycle.plan.cancel.clone();
            let next = {
                let Some(current) = self.current.as_mut() else {
                    continue;
                };
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => StreamOutcome::Cancelled,
                    event = current.next() => StreamOutcome::Item(event),
                }
            };
            match next {
                StreamOutcome::Cancelled => return self.cancel_pending().await,
                StreamOutcome::Item(Some(Ok(event))) => {
                    self.lifecycle.delivered = true;
                    if matches!(&event, StreamEvent::Stop { .. }) {
                        drop(self.lifecycle.permit.take());
                        self.terminal = true;
                    }
                    return Some(Ok(event));
                }
                StreamOutcome::Item(Some(Err(error))) => {
                    self.current = None;
                    match self.lifecycle.recover(AttemptFailure::Provider(error), &mut self.refresh).await {
                        Ok(Recovery::Retry) => continue,
                        Ok(Recovery::Cancelled) => return self.cancel_pending().await,
                        Err(error) => return self.fail(error),
                    }
                }
                StreamOutcome::Item(None) => {
                    self.current = None;
                    match self.lifecycle.recover(
                        AttemptFailure::Provider(ProviderError::StreamCut),
                        &mut self.refresh,
                    ).await {
                        Ok(Recovery::Retry) => continue,
                        Ok(Recovery::Cancelled) => return self.cancel_pending().await,
                        Err(error) => return self.fail(error),
                    }
                }
            }
        }
    }

    async fn cancel_pending(&mut self) -> Option<Result<StreamEvent, ProviderError>> {
        self.current = None;
        drop(self.lifecycle.permit.take());
        self.terminal = true;
        std::future::pending().await
    }

    fn fail(&mut self, error: ProviderError) -> Option<Result<StreamEvent, ProviderError>> {
        self.current = None;
        drop(self.lifecycle.permit.take());
        self.terminal = true;
        Some(Err(error))
    }
}

impl<A, AF, R, RF, C> Drop for StreamLifecycle<A, AF, R, RF, C> {
    fn drop(&mut self) {
        // The socket must be gone before its permit admits another request.
        drop(self.current.take());
        drop(self.lifecycle.permit.take());
    }
}

enum StreamOutcome {
    Item(Option<Result<StreamEvent, ProviderError>>),
    Cancelled,
}

fn classify_failure(
    failure: AttemptFailure,
    request: &RequestState<'_>,
) -> (RetryDecision, Option<String>, Option<String>) {
    match failure {
        AttemptFailure::Response {
            status,
            code,
            message,
            retry_after,
        } => (
            retry::classify(Some(status), code.as_deref(), &message, request),
            retry_after,
            Some(message),
        ),
        AttemptFailure::Provider(error) => {
            if request.delivered && matches!(&error, ProviderError::StreamCut | ProviderError::WsClosed { .. }) {
                return (RetryDecision::Fail(error), None, None);
            }
            let (status, code, message) = match error {
                ProviderError::Transport { reason, .. } => (None, None, Cow::Owned(reason)),
                ProviderError::Status { status, message, .. } => {
                    (Some(status), None, Cow::Owned(message))
                }
                ProviderError::Overloaded => {
                    (None, Some("overloaded_error"), Cow::Borrowed("provider is overloaded"))
                }
                ProviderError::RateLimited { message, .. } => (Some(429), None, Cow::Owned(message)),
                ProviderError::StreamCut => (
                    None,
                    None,
                    Cow::Borrowed("stream cut off before completion"),
                ),
                ProviderError::WsClosed { .. } => (
                    None,
                    None,
                    Cow::Borrowed("WebSocket closed before response.completed"),
                ),
                error => return (RetryDecision::Fail(error), None, None),
            };
            (retry::classify(status, code, &message, request), None, None)
        }
    }
}

fn retry_after_seconds(value: &str, now: Timestamp) -> Option<f64> {
    if let Ok(seconds) = value.trim().parse::<f64>() {
        return Some(seconds);
    }
    let date = rfc2822::parse(value.trim()).ok()?.timestamp();
    Some(date.duration_since(now).as_secs_f64())
}

fn retry_jitter() -> f64 {
    let bytes = Uuid::new_v4().into_bytes();
    let sample = u32::from_be_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
    0.9 + 0.2 * (f64::from(sample) / f64::from(u32::MAX))
}

#[cfg(test)]
mod tests {
    use std::{
        future::{Ready, ready},
        sync::{Mutex, atomic::{AtomicUsize, Ordering}},
        time::Instant,
    };

    use dal_core::Family;
    use futures::{StreamExt, stream};
    use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::TcpListener};

    use super::*;
    use crate::{
        auth::credential::SecretString,
        http::{self, Exchange},
        stream::{StopReason, StreamEvent},
    };

    #[derive(Clone)]
    struct TestClock {
        now: Arc<Mutex<Timestamp>>,
        sleeps: Arc<Mutex<Vec<Duration>>>,
    }

    impl TestClock {
        fn new(now: Timestamp) -> Self {
            Self {
                now: Arc::new(Mutex::new(now)),
                sleeps: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn sleeps(&self) -> Vec<Duration> {
            lock(&self.sleeps).clone()
        }
    }

    impl Clock for TestClock {
        type Sleep = Ready<()>;

        fn now(&self) -> Timestamp {
            *lock(&self.now)
        }

        fn sleep(&self, duration: Duration) -> Self::Sleep {
            lock(&self.sleeps).push(duration);
            let delta = jiff::SignedDuration::from_secs_f64(duration.as_secs_f64());
            let mut now = lock(&self.now);
            *now = now.checked_add(delta).unwrap_or(*now);
            ready(())
        }
    }

    fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn notice_sink() -> NoticeSink {
        Arc::new(|_| {})
    }

    fn plan<C: Clock>(clock: C, max_retries: u32, permits: Arc<Semaphore>, cancel: CancellationToken) -> Plan<C> {
        Plan::new(
            Family::Responses,
            "openai",
            "gpt-6",
            max_retries,
            permits,
            cancel,
            notice_sink(),
        )
        .with_clock(clock)
    }

    fn timestamp(value: &str) -> Timestamp {
        rfc2822::parse(value).expect("test timestamp parses").timestamp()
    }

    async fn loopback_server(responses: Vec<String>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind loopback");
        let address = listener.local_addr().expect("loopback address").to_string();
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for response in responses {
                let (mut socket, _) = listener.accept().await.expect("accept request");
                let mut received = Vec::new();
                let mut chunk = [0; 1024];
                while !received.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    let count = socket.read(&mut chunk).await.expect("read request");
                    if count == 0 {
                        break;
                    }
                    received.extend_from_slice(&chunk[..count]);
                }
                requests.push(String::from_utf8_lossy(&received).into_owned());
                socket.write_all(response.as_bytes()).await.expect("write response");
            }
            requests
        });
        (format!("http://{address}/"), server)
    }
    async fn stalled_loopback_server() -> (String, tokio::task::JoinHandle<bool>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind loopback");
        let address = listener.local_addr().expect("loopback address").to_string();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept stalled request");
            let mut received = Vec::new();
            let mut chunk = [0; 1024];
            while !received.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let count = socket.read(&mut chunk).await.expect("read stalled request");
                if count == 0 {
                    return false;
                }
                received.extend_from_slice(&chunk[..count]);
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: keep-alive\r\n\r\nfirst")
                .await
                .expect("write partial body");
            socket.flush().await.expect("flush partial body");
            let mut byte = [0; 1];
            matches!(
                tokio::time::timeout(Duration::from_millis(250), socket.read(&mut byte)).await,
                Ok(Ok(0)) | Ok(Err(_))
            )
        });
        (format!("http://{address}/"), server)
    }

    fn stalled_once(
        client: reqwest::Client,
        url: String,
        credential: &Credential,
        cancel: &CancellationToken,
    ) -> impl Future<Output = Result<Option<EventStream>, AttemptFailure>> + Send + use<> {
        let mut request = client.get(url);
        match credential {
            Credential::ApiKey { key } => request = request.bearer_auth(key.expose()),
            Credential::OAuth(oauth) => {
                request = request.bearer_auth(oauth.access_token.expose());
            }
            Credential::None => {}
        }
        let cancel = cancel.clone();
        async move {
            let response = tokio::select! {
                biased;
                () = cancel.cancelled() => return Ok(None),
                response = http::send(
                    Family::Responses,
                    request,
                    "lifecycle-test/1",
                    Exchange::Stream,
                    tokio::time::sleep,
                ) => response.map_err(AttemptFailure::Provider)?,
            };
            let events = response.bytes_stream().map(|chunk| match chunk {
                Ok(bytes) => Ok(StreamEvent::TextDelta {
                    text: String::from_utf8_lossy(&bytes).into_owned(),
                }),
                Err(_) => Err(ProviderError::StreamCut),
            });
            Ok(Some(EventStream::new(events, || {})))
        }
    }

    fn get_once(
        client: reqwest::Client,
        url: String,
        credential: &Credential,
        _cancel: &CancellationToken,
    ) -> impl Future<Output = Result<Option<u16>, AttemptFailure>> + Send + use<> {
        let mut request = client.get(url);
        match credential {
            Credential::ApiKey { key } => request = request.bearer_auth(key.expose()),
            Credential::OAuth(oauth) => {
                request = request.bearer_auth(oauth.access_token.expose());
            }
            Credential::None => {}
        }
        async move {
            let response = http::send(
                Family::Responses,
                request,
                "lifecycle-test/1",
                Exchange::Json {
                    total: http::NON_STREAM_TOTAL_TIMEOUT,
                },
                tokio::time::sleep,
            )
            .await
            .map_err(AttemptFailure::Provider)?;
            let status = response.status().as_u16();
            if response.status().is_success() {
                return Ok(Some(status));
            }
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .map(String::from);
            let body = http::read_body(Family::Responses, response)
                .await
                .map_err(AttemptFailure::Provider)?;
            let message = String::from_utf8_lossy(&body).into_owned();
            Err(AttemptFailure::Response {
                status,
                code: None,
                message,
                retry_after,
            })
        }
    }

    fn response(status: u16, headers: &[(&str, &str)], body: &str) -> String {
        let mut response = format!("HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
        for (name, value) in headers {
            response.push_str(name);
            response.push_str(": ");
            response.push_str(value);
            response.push_str("\r\n");
        }
        response.push_str("\r\n");
        response.push_str(body);
        response
    }

    fn api_key() -> Credential {
        Credential::ApiKey {
            key: SecretString::from("api-key"),
        }
    }

    fn oauth(token: &str) -> Credential {
        Credential::OAuth(OAuthCredential {
            access_token: SecretString::from(token),
            refresh_token: SecretString::from("refresh"),
            expires_at: None,
            id_token: None,
            account_id: None,
        })
    }

    #[tokio::test]
    async fn retries_a_loopback_503_then_returns_the_success() {
        let (url, server) = loopback_server(vec![
            response(503, &[], "busy"),
            response(200, &[], "ok"),
        ])
        .await;
        let clock = TestClock::new(timestamp("Mon, 15 Jul 2024 16:24:59 +0000"));
        let client = http::build_client();
        let output = request(
            plan(clock.clone(), 1, Arc::new(Semaphore::new(2)), CancellationToken::new()),
            api_key(),
            move |credential, cancel| get_once(client.clone(), url.clone(), credential, cancel),
            |oauth| async move { Ok(Credential::OAuth(oauth)) },
        )
        .await
        .expect("request succeeds");
        assert_eq!(output, Some(200));
        assert_eq!(clock.sleeps().len(), 1);
        assert_eq!(server.await.expect("server completes").len(), 2);
    }

    #[tokio::test]
    async fn retry_after_http_date_uses_the_controlled_clock() {
        let (url, server) = loopback_server(vec![
            response(429, &[("Retry-After", "Mon, 15 Jul 2024 16:25:02 GMT")], "slow down"),
            response(200, &[], "ok"),
        ])
        .await;
        let clock = TestClock::new(timestamp("Mon, 15 Jul 2024 16:24:59 +0000"));
        let client = http::build_client();
        let output = request(
            plan(clock.clone(), 1, Arc::new(Semaphore::new(1)), CancellationToken::new()),
            api_key(),
            move |credential, cancel| get_once(client.clone(), url.clone(), credential, cancel),
            |oauth| async move { Ok(Credential::OAuth(oauth)) },
        )
        .await
        .expect("request succeeds");
        assert_eq!(output, Some(200));
        assert_eq!(clock.sleeps(), vec![Duration::from_secs(3)]);
        assert_eq!(server.await.expect("server completes").len(), 2);
    }

    #[tokio::test]
    async fn refuses_retry_after_above_budget_without_sleeping() {
        let (url, server) = loopback_server(vec![response(429, &[("Retry-After", "120")], "slow down")]).await;
        let clock = TestClock::new(timestamp("Mon, 15 Jul 2024 16:24:59 +0000"));
        let client = http::build_client();
        let error = request(
            plan(clock.clone(), 4, Arc::new(Semaphore::new(1)), CancellationToken::new()),
            api_key(),
            move |credential, cancel| get_once(client.clone(), url.clone(), credential, cancel),
            |oauth| async move { Ok(Credential::OAuth(oauth)) },
        )
        .await
        .expect_err("over-budget retry is refused");
        assert!(matches!(error, ProviderError::RetryAfterTooLong { seconds: 120, .. }));
        assert!(clock.sleeps().is_empty());
        assert_eq!(server.await.expect("server completes").len(), 1);
    }

    async fn exhausted_rate_limit(responses: Vec<String>, retries: u32) -> (ProviderError, Vec<Duration>) {
        let attempts = responses.len();
        let (url, server) = loopback_server(responses).await;
        let clock = TestClock::new(timestamp("Mon, 15 Jul 2024 16:24:59 +0000"));
        let client = http::build_client();
        let error = request(
            plan(clock.clone(), retries, Arc::new(Semaphore::new(1)), CancellationToken::new()),
            api_key(),
            move |credential, cancel| get_once(client.clone(), url.clone(), credential, cancel),
            |oauth| async move { Ok(Credential::OAuth(oauth)) },
        )
        .await
        .expect_err("every attempt is rate limited");
        assert_eq!(server.await.expect("server completes").len(), attempts);
        (error, clock.sleeps())
    }

    #[tokio::test]
    async fn exhausted_rate_limit_keeps_only_the_final_retry_after() {
        let (error, sleeps) = exhausted_rate_limit(
            vec![
                response(429, &[("Retry-After", "5")], "first"),
                response(429, &[("Retry-After", "2")], "final"),
            ],
            1,
        )
        .await;
        assert_eq!(sleeps, vec![Duration::from_secs(5)]);
        assert!(
            matches!(&error, ProviderError::RateLimited { message, retry_after: Some(wait) }
                if message == "final" && *wait == Duration::from_secs(2)),
            "{error:?}"
        );

        let (error, _) = exhausted_rate_limit(
            vec![
                response(429, &[], "first"),
                response(429, &[("Retry-After", "Mon, 15 Jul 2024 16:25:02 GMT")], "final"),
            ],
            1,
        )
        .await;
        assert!(
            matches!(&error, ProviderError::RateLimited { retry_after: Some(wait), .. }
                if *wait == Duration::from_secs(3)),
            "{error:?}"
        );

        let (error, sleeps) = exhausted_rate_limit(
            vec![
                response(429, &[("Retry-After", "4")], "first"),
                response(429, &[], "final"),
            ],
            1,
        )
        .await;
        assert_eq!(sleeps, vec![Duration::from_secs(4)]);
        assert!(
            matches!(&error, ProviderError::RateLimited { retry_after: None, .. }),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn a_three_hundred_status_is_final_and_keeps_its_status() {
        let (url, server) = loopback_server(vec![response(302, &[], "redirect")]).await;
        let client = http::build_client();
        let error = request(
            plan(
                TestClock::new(timestamp("Mon, 15 Jul 2024 16:24:59 +0000")),
                4,
                Arc::new(Semaphore::new(1)),
                CancellationToken::new(),
            ),
            api_key(),
            move |credential, cancel| get_once(client.clone(), url.clone(), credential, cancel),
            |oauth| async move { Ok(Credential::OAuth(oauth)) },
        )
        .await
        .expect_err("3xx is not a successful provider response");
        assert!(matches!(
            error,
            ProviderError::Status { status: 302, .. }
        ));
        assert_eq!(server.await.expect("server completes").len(), 1);
    }

    #[tokio::test]
    async fn retries_a_refused_loopback_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind temporary port");
        let address = listener.local_addr().expect("read temporary port");
        drop(listener);
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempt_count = Arc::clone(&attempts);
        let client = http::build_client();
        let url = format!("http://{address}/");
        let error = request(
            plan(
                TestClock::new(timestamp("Mon, 15 Jul 2024 16:24:59 +0000")),
                1,
                Arc::new(Semaphore::new(1)),
                CancellationToken::new(),
            ),
            api_key(),
            move |credential, cancel| {
                attempt_count.fetch_add(1, Ordering::SeqCst);
                get_once(client.clone(), url.clone(), credential, cancel)
            },
            |oauth| async move { Ok(Credential::OAuth(oauth)) },
        )
        .await
        .expect_err("repeated connect refusal is typed");
        assert!(matches!(error, ProviderError::Transport { .. }));
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn quota_code_is_not_retried_even_when_429_is_retryable() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempt_count = Arc::clone(&attempts);
        let error = request(
            plan(
                TestClock::new(timestamp("Mon, 15 Jul 2024 16:24:59 +0000")),
                5,
                Arc::new(Semaphore::new(1)),
                CancellationToken::new(),
            ),
            api_key(),
            move |_credential, _cancel| {
                attempt_count.fetch_add(1, Ordering::SeqCst);
                async {
                    Err::<Option<()>, _>(AttemptFailure::Response {
                        status: 429,
                        code: Some(String::from("insufficient_quota")),
                        message: String::from("no credits"),
                        retry_after: None,
                    })
                }
            },
            |oauth| async move { Ok(Credential::OAuth(oauth)) },
        )
        .await
        .expect_err("quota failure is terminal");
        assert!(matches!(error, ProviderError::Quota { .. }));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }


    #[tokio::test]
    async fn refreshes_an_oauth_401_once_before_replaying() {
        let (url, server) = loopback_server(vec![
            response(401, &[], "expired"),
            response(200, &[], "ok"),
        ])
        .await;
        let refreshes = Arc::new(AtomicUsize::new(0));
        let refresh_count = Arc::clone(&refreshes);
        let client = http::build_client();
        let output = request(
            plan(TestClock::new(timestamp("Mon, 15 Jul 2024 16:24:59 +0000")), 0, Arc::new(Semaphore::new(1)), CancellationToken::new()),
            oauth("old"),
            move |credential, cancel| get_once(client.clone(), url.clone(), credential, cancel),
            move |_held| {
                refresh_count.fetch_add(1, Ordering::SeqCst);
                async { Ok(oauth("new")) }
            },
        )
        .await
        .expect("request succeeds after refresh");
        assert_eq!(output, Some(200));
        assert_eq!(refreshes.load(Ordering::SeqCst), 1);
        let requests = server.await.expect("server completes");
        assert_eq!(requests.len(), 2);
        assert!(requests[0].to_ascii_lowercase().contains("authorization: bearer old"));
        assert!(requests[1].to_ascii_lowercase().contains("authorization: bearer new"));
    }

    #[tokio::test]
    async fn a_second_oauth_401_is_sign_in_expired() {
        let (url, server) = loopback_server(vec![
            response(401, &[], "expired"),
            response(401, &[], "still expired"),
        ])
        .await;
        let refreshes = Arc::new(AtomicUsize::new(0));
        let refresh_count = Arc::clone(&refreshes);
        let client = http::build_client();
        let error = request(
            plan(
                TestClock::new(timestamp("Mon, 15 Jul 2024 16:24:59 +0000")),
                3,
                Arc::new(Semaphore::new(1)),
                CancellationToken::new(),
            ),
            oauth("old"),
            move |credential, cancel| get_once(client.clone(), url.clone(), credential, cancel),
            move |_held| {
                refresh_count.fetch_add(1, Ordering::SeqCst);
                async { Ok(oauth("new")) }
            },
        )
        .await
        .expect_err("a refreshed token's 401 is terminal");
        assert!(matches!(error, ProviderError::SignInExpired { .. }));
        assert_eq!(refreshes.load(Ordering::SeqCst), 1);
        assert_eq!(server.await.expect("server completes").len(), 2);
    }

    #[tokio::test]
    async fn api_key_401_is_typed_and_never_refreshes() {
        let (url, server) = loopback_server(vec![response(401, &[], "bad key")]).await;
        let refreshes = Arc::new(AtomicUsize::new(0));
        let refresh_count = Arc::clone(&refreshes);
        let client = http::build_client();
        let error = request(
            plan(TestClock::new(timestamp("Mon, 15 Jul 2024 16:24:59 +0000")), 3, Arc::new(Semaphore::new(1)), CancellationToken::new()),
            api_key(),
            move |credential, cancel| get_once(client.clone(), url.clone(), credential, cancel),
            move |_held| {
                refresh_count.fetch_add(1, Ordering::SeqCst);
                async { Ok(oauth("unexpected")) }
            },
        )
        .await
        .expect_err("API-key 401 is final");
        assert!(matches!(error, ProviderError::AuthRejected { .. }));
        assert_eq!(refreshes.load(Ordering::SeqCst), 0);
        assert_eq!(server.await.expect("server completes").len(), 1);
    }

    #[tokio::test]
    async fn retries_an_in_stream_overload_only_before_delivery() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempt_count = Arc::clone(&attempts);
        let mut events = stream(
            plan(TestClock::new(timestamp("Mon, 15 Jul 2024 16:24:59 +0000")), 1, Arc::new(Semaphore::new(1)), CancellationToken::new()),
            api_key(),
            move |_credential, _cancel| {
                let first = attempt_count.fetch_add(1, Ordering::SeqCst) == 0;
                async move {
                    let output: Vec<Result<StreamEvent, ProviderError>> = if first {
                        vec![Err(ProviderError::Overloaded)]
                    } else {
                        vec![Ok(StreamEvent::Stop { reason: StopReason::EndTurn })]
                    };
                    Ok(Some(EventStream::new(stream::iter(output), || {})))
                }
            },
            |oauth| async move { Ok(Credential::OAuth(oauth)) },
        );
        assert!(matches!(events.next().await, Some(Ok(StreamEvent::Stop { .. }))));
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert!(events.next().await.is_none());
    }

    #[tokio::test]
    async fn error_after_any_event_is_not_replayed() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempt_count = Arc::clone(&attempts);
        let mut events = stream(
            plan(TestClock::new(timestamp("Mon, 15 Jul 2024 16:24:59 +0000")), 5, Arc::new(Semaphore::new(1)), CancellationToken::new()),
            api_key(),
            move |_credential, _cancel| {
                attempt_count.fetch_add(1, Ordering::SeqCst);
                async {
                    let output = vec![
                        Ok(StreamEvent::TextDelta { text: String::from("partial") }),
                        Err(ProviderError::Overloaded),
                    ];
                    Ok(Some(EventStream::new(stream::iter(output), || {})))
                }
            },
            |oauth| async move { Ok(Credential::OAuth(oauth)) },
        );
        assert!(matches!(events.next().await, Some(Ok(StreamEvent::TextDelta { .. }))));
        assert!(matches!(events.next().await, Some(Err(ProviderError::Overloaded))));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(events.next().await.is_none());
    }

    #[tokio::test]
    async fn a_loopback_cut_after_text_is_not_replayed() {
        let (url, server) = loopback_server(vec![String::from(
            "HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nfirst",
        )])
        .await;
        let client = http::build_client();
        let mut events = stream(
            plan(
                TestClock::new(timestamp("Mon, 15 Jul 2024 16:24:59 +0000")),
                5,
                Arc::new(Semaphore::new(1)),
                CancellationToken::new(),
            ),
            api_key(),
            move |credential, cancel| stalled_once(client.clone(), url.clone(), credential, cancel),
            |oauth| async move { Ok(Credential::OAuth(oauth)) },
        );
        assert!(matches!(
            events.next().await,
            Some(Ok(StreamEvent::TextDelta { text })) if text == "first"
        ));
        assert!(matches!(
            events.next().await,
            Some(Err(ProviderError::StreamCut))
        ));
        assert!(events.next().await.is_none());
        assert_eq!(server.await.expect("server completes").len(), 1);
    }

    #[tokio::test]
    async fn cancellation_drops_the_stalled_attempt_without_emitting_a_terminal() {
        let cancel = CancellationToken::new();
        let closed = Arc::new(AtomicUsize::new(0));
        let closed_by_stream = Arc::clone(&closed);
        let mut events = stream(
            plan(TestClock::new(timestamp("Mon, 15 Jul 2024 16:24:59 +0000")), 1, Arc::new(Semaphore::new(1)), cancel.clone()),
            api_key(),
            move |_credential, _cancel| {
                let closed = Arc::clone(&closed_by_stream);
                async move {
                    Ok(Some(EventStream::new(
                        stream::pending::<Result<StreamEvent, ProviderError>>(),
                        move || { closed.fetch_add(1, Ordering::SeqCst); },
                    )))
                }
            },
            |oauth| async move { Ok(Credential::OAuth(oauth)) },
        );
        let first = tokio::time::timeout(Duration::from_millis(20), events.next()).await;
        assert!(first.is_err(), "stalled stream unexpectedly yielded");
        let cancelled_at = Instant::now();
        cancel.cancel();
        let no_terminal = tokio::time::timeout(Duration::from_millis(20), events.next()).await;
        assert!(no_terminal.is_err(), "cancellation emitted an event");
        assert_eq!(closed.load(Ordering::SeqCst), 1);
        assert!(cancelled_at.elapsed() < Duration::from_millis(250));
    }

    #[tokio::test]
    async fn dropping_the_outer_stream_drops_its_active_attempt() {
        let closed = Arc::new(AtomicUsize::new(0));
        let closed_by_stream = Arc::clone(&closed);
        let events = stream(
            plan(TestClock::new(timestamp("Mon, 15 Jul 2024 16:24:59 +0000")), 1, Arc::new(Semaphore::new(1)), CancellationToken::new()),
            api_key(),
            move |_credential, _cancel| {
                let closed = Arc::clone(&closed_by_stream);
                async move {
                    Ok(Some(EventStream::new(
                        stream::pending::<Result<StreamEvent, ProviderError>>(),
                        move || { closed.fetch_add(1, Ordering::SeqCst); },
                    )))
                }
            },
            |oauth| async move { Ok(Credential::OAuth(oauth)) },
        );
        let mut events = events;
        let _ = tokio::time::timeout(Duration::from_millis(20), events.next()).await;
        drop(events);
        assert_eq!(closed.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn cancelling_a_loopback_stream_closes_its_socket_without_a_terminal() {
        let (url, server) = stalled_loopback_server().await;
        let cancel = CancellationToken::new();
        let client = http::build_client();
        let mut events = stream(
            plan(
                TestClock::new(timestamp("Mon, 15 Jul 2024 16:24:59 +0000")),
                1,
                Arc::new(Semaphore::new(1)),
                cancel.clone(),
            ),
            api_key(),
            move |credential, cancel| stalled_once(client.clone(), url.clone(), credential, cancel),
            |oauth| async move { Ok(Credential::OAuth(oauth)) },
        );
        assert!(matches!(
            events.next().await,
            Some(Ok(StreamEvent::TextDelta { text })) if text == "first"
        ));
        let cancelled_at = Instant::now();
        cancel.cancel();
        assert!(tokio::time::timeout(Duration::from_millis(20), events.next())
            .await
            .is_err());
        assert!(server.await.expect("server observes socket close"));
        assert!(cancelled_at.elapsed() < Duration::from_millis(250));
    }

    #[tokio::test]
    async fn dropping_a_loopback_stream_closes_its_socket() {
        let (url, server) = stalled_loopback_server().await;
        let client = http::build_client();
        let mut events = stream(
            plan(
                TestClock::new(timestamp("Mon, 15 Jul 2024 16:24:59 +0000")),
                1,
                Arc::new(Semaphore::new(1)),
                CancellationToken::new(),
            ),
            api_key(),
            move |credential, cancel| stalled_once(client.clone(), url.clone(), credential, cancel),
            |oauth| async move { Ok(Credential::OAuth(oauth)) },
        );
        assert!(matches!(
            events.next().await,
            Some(Ok(StreamEvent::TextDelta { text })) if text == "first"
        ));
        let dropped_at = Instant::now();
        drop(events);
        assert!(server.await.expect("server observes socket close"));
        assert!(dropped_at.elapsed() < Duration::from_millis(250));
    }

    #[tokio::test]
    async fn two_hundred_requests_obey_the_configured_semaphore_cap() {
        let permits = Arc::new(Semaphore::new(4));
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::with_capacity(200);
        for _ in 0..200 {
            let permits = Arc::clone(&permits);
            let active = Arc::clone(&active);
            let maximum = Arc::clone(&maximum);
            tasks.push(tokio::spawn(async move {
                request(
                    Plan::new(Family::Responses, "openai", "gpt-6", 0, permits, CancellationToken::new(), notice_sink()),
                    api_key(),
                    move |_credential, _cancel| {
                        let active = Arc::clone(&active);
                        let maximum = Arc::clone(&maximum);
                        async move {
                            let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                            maximum.fetch_max(current, Ordering::SeqCst);
                            tokio::time::sleep(Duration::from_millis(1)).await;
                            active.fetch_sub(1, Ordering::SeqCst);
                            Ok::<_, AttemptFailure>(Some(()))
                        }
                    },
                    |oauth| async move { Ok(Credential::OAuth(oauth)) },
                )
                .await
            }));
        }
        for task in tasks {
            assert!(matches!(
                task.await.expect("request task completes"),
                Ok(Some(()))
            ));
        }
        assert_eq!(maximum.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn retry_after_date_parser_accepts_http_date_and_rejects_garbage() {
        let now = timestamp("Mon, 15 Jul 2024 16:24:59 +0000");
        assert_eq!(retry_after_seconds("Mon, 15 Jul 2024 16:25:02 GMT", now), Some(3.0));
        assert_eq!(retry_after_seconds("not a date", now), None);
    }
}
