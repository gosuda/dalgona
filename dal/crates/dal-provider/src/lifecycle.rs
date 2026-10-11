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
    #[cfg(test)]
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
            AttemptOutcome::Cancelled | AttemptOutcome::Result(Ok(None)) => return Ok(None),
            AttemptOutcome::Result(Ok(Some(value))) => return Ok(Some(value)),
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
                    let server_wait = retry_after
                        .is_some_and(|secs| !secs.is_nan())
                        .then_some(wait);
                    return Err(match error {
                        ProviderError::RateLimited { message, .. } => ProviderError::RateLimited {
                            message,
                            retry_after: server_wait,
                        },
                        error => error,
                    });
                }
                let delay = match retry::delay_for_attempt(attempt, retry_after, retry_jitter()) {
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
                    Err(error) => return Some(self.fail(error)),
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
                    AttemptOutcome::Cancelled | AttemptOutcome::Result(Ok(None)) => {
                        return self.cancel_pending().await;
                    }
                    AttemptOutcome::Result(Ok(Some(stream))) => self.current = Some(stream),
                    AttemptOutcome::Result(Err(failure)) => {
                        match self.lifecycle.recover(failure, &mut self.refresh).await {
                            Ok(Recovery::Retry) => continue,
                            Ok(Recovery::Cancelled) => return self.cancel_pending().await,
                            Err(error) => return Some(self.fail(error)),
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
                    match self
                        .lifecycle
                        .recover(AttemptFailure::Provider(error), &mut self.refresh)
                        .await
                    {
                        Ok(Recovery::Retry) => {}
                        Ok(Recovery::Cancelled) => return self.cancel_pending().await,
                        Err(error) => return Some(self.fail(error)),
                    }
                }
                StreamOutcome::Item(None) => {
                    self.current = None;
                    match self
                        .lifecycle
                        .recover(
                            AttemptFailure::Provider(ProviderError::StreamCut),
                            &mut self.refresh,
                        )
                        .await
                    {
                        Ok(Recovery::Retry) => {}
                        Ok(Recovery::Cancelled) => return self.cancel_pending().await,
                        Err(error) => return Some(self.fail(error)),
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

    fn fail(&mut self, error: ProviderError) -> Result<StreamEvent, ProviderError> {
        self.current = None;
        drop(self.lifecycle.permit.take());
        self.terminal = true;
        Err(error)
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
            if request.delivered
                && matches!(
                    &error,
                    ProviderError::StreamCut | ProviderError::WsClosed { .. }
                )
            {
                return (RetryDecision::Fail(error), None, None);
            }
            let (status, code, message) = match error {
                ProviderError::Transport { reason, .. } => (None, None, Cow::Owned(reason)),
                ProviderError::Status {
                    status, message, ..
                } => (Some(status), None, Cow::Owned(message)),
                ProviderError::Overloaded => (
                    None,
                    Some("overloaded_error"),
                    Cow::Borrowed("provider is overloaded"),
                ),
                ProviderError::RateLimited { message, .. } => {
                    (Some(429), None, Cow::Owned(message))
                }
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
mod tests;
