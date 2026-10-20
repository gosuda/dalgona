use std::{
    future::{Ready, ready},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};

use dal_core::{Family, InferFailure, RawJson, ReplayCapture, ReplayRefusal, Usage};
use futures::{FutureExt, StreamExt, stream};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use super::*;
use crate::{
    auth::credential::SecretString,
    error::LimitError,
    http::{self, Exchange},
    stream::{ReplayPayload, StopReason, StreamEvent},
};

const FIXED_NOW: &str = "Mon, 15 Jul 2024 16:24:59 +0000";

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
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn notice_sink() -> NoticeSink {
    Arc::new(|_| {})
}

fn plan<C: Clock>(
    clock: C,
    max_retries: u32,
    permits: Arc<Semaphore>,
    cancel: CancellationToken,
) -> Plan<C> {
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
    rfc2822::parse(value)
        .expect("test timestamp parses")
        .timestamp()
}

async fn loopback_server(responses: Vec<String>) -> (String, tokio::task::JoinSet<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let address = listener.local_addr().expect("loopback address").to_string();
    let mut server = tokio::task::JoinSet::new();
    server.spawn(async move {
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
            socket
                .write_all(response.as_bytes())
                .await
                .expect("write response");
        }
        requests
    });
    (format!("http://{address}/"), server)
}
async fn stalled_loopback_server() -> (String, tokio::task::JoinSet<bool>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let address = listener.local_addr().expect("loopback address").to_string();
    let mut server = tokio::task::JoinSet::new();
    server.spawn(async move {
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
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: keep-alive\r\n\r\nfirst",
            )
            .await
            .expect("write partial body");
        socket.flush().await.expect("flush partial body");
        let mut byte = [0; 1];
        matches!(
            tokio::time::timeout(Duration::from_millis(250), socket.read(&mut byte)).await,
            Ok(Ok(0) | Err(_))
        )
    });
    (format!("http://{address}/"), server)
}

fn stalled_once(
    url: String,
    credential: &Credential,
    cancel: &CancellationToken,
) -> impl Future<Output = Result<Option<EventStream>, AttemptFailure>> + Send + use<> {
    let client = http::build_client();
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
    url: String,
    credential: &Credential,
    _cancel: &CancellationToken,
) -> impl Future<Output = Result<Option<u16>, AttemptFailure>> + Send + use<> {
    let client = http::build_client();
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
    let mut response = format!(
        "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
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

fn failure(status: u16, retry_after: Option<&str>, message: &str) -> AttemptFailure {
    AttemptFailure::Response {
        status,
        code: None,
        message: String::from(message),
        retry_after: retry_after.map(String::from),
    }
}

fn token_of(credential: &Credential) -> String {
    match credential {
        Credential::ApiKey { key } => key.expose().to_owned(),
        Credential::OAuth(oauth) => oauth.access_token.expose().to_owned(),
        Credential::None => String::new(),
    }
}

/// What a scripted `request` saw: attempts started, refreshes run, and the
/// credential token each attempt carried.
#[derive(Clone, Default)]
struct Counts {
    attempts: Arc<AtomicUsize>,
    refreshes: Arc<AtomicUsize>,
    tokens: Arc<Mutex<Vec<String>>>,
}

impl Counts {
    fn attempts(&self) -> usize {
        self.attempts.load(Ordering::SeqCst)
    }

    fn refreshes(&self) -> usize {
        self.refreshes.load(Ordering::SeqCst)
    }

    fn tokens(&self) -> Vec<String> {
        lock(&self.tokens).clone()
    }
}

/// Runs `request` with `script` deciding the outcome of each zero-based
/// attempt and `refresh` deciding the outcome of each OAuth refresh.
async fn run<C: Clock>(
    lifecycle_plan: Plan<C>,
    credential: Credential,
    counts: &Counts,
    script: impl Fn(usize) -> Result<Option<u16>, AttemptFailure> + Send,
    refresh: impl Fn() -> Result<Credential, ProviderError> + Send,
) -> Result<Option<u16>, ProviderError> {
    let attempts = Arc::clone(&counts.attempts);
    let tokens = Arc::clone(&counts.tokens);
    let refreshes = Arc::clone(&counts.refreshes);
    request(
        lifecycle_plan,
        credential,
        move |credential, _cancel| {
            let number = attempts.fetch_add(1, Ordering::SeqCst);
            lock(&tokens).push(token_of(credential));
            let outcome = script(number);
            async move { outcome }
        },
        move |_held| {
            refreshes.fetch_add(1, Ordering::SeqCst);
            let outcome = refresh();
            async move { outcome }
        },
    )
    .await
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "the refresh outcome type of the `run` helper"
)]
fn never_refreshed() -> Result<Credential, ProviderError> {
    Ok(oauth("unexpected"))
}

fn text(chunk: &str) -> StreamEvent {
    StreamEvent::TextDelta {
        text: String::from(chunk),
    }
}

fn stop() -> StreamEvent {
    StreamEvent::Stop {
        reason: StopReason::EndTurn,
    }
}

/// A lifecycle stream whose attempt `n` yields the events `script(n)`.
fn scripted_stream(
    permits: Arc<Semaphore>,
    cancel: CancellationToken,
    max_retries: u32,
    attempts: Arc<AtomicUsize>,
    script: impl Fn(usize) -> Vec<Result<StreamEvent, ProviderError>> + Send + 'static,
) -> EventStream {
    stream(
        plan(
            TestClock::new(timestamp(FIXED_NOW)),
            max_retries,
            permits,
            cancel,
        ),
        api_key(),
        move |_credential, _cancel| {
            let events = script(attempts.fetch_add(1, Ordering::SeqCst));
            async move { Ok(Some(EventStream::new(stream::iter(events), || {}))) }
        },
        |oauth| async move { Ok(Credential::OAuth(oauth)) },
    )
}

/// A stalled lifecycle stream whose attempt records, when it is closed, how
/// many permits were free at that moment.
fn stream_probing_permits(
    permits: &Arc<Semaphore>,
    cancel: CancellationToken,
    seen: &Arc<Mutex<Vec<usize>>>,
) -> EventStream {
    let attempt_permits = Arc::clone(permits);
    let attempt_seen = Arc::clone(seen);
    stream(
        plan(
            TestClock::new(timestamp(FIXED_NOW)),
            1,
            Arc::clone(permits),
            cancel,
        ),
        api_key(),
        move |_credential, _cancel| {
            let permits = Arc::clone(&attempt_permits);
            let seen = Arc::clone(&attempt_seen);
            async move {
                Ok(Some(EventStream::new(
                    stream::pending::<Result<StreamEvent, ProviderError>>(),
                    move || lock(&seen).push(permits.available_permits()),
                )))
            }
        },
        |oauth| async move { Ok(Credential::OAuth(oauth)) },
    )
}

/// A clock that records how many permits are free whenever it is asked to
/// sleep.
#[derive(Clone)]
struct PermitProbeClock {
    permits: Arc<Semaphore>,
    free_during_sleep: Arc<Mutex<Vec<usize>>>,
}

impl Clock for PermitProbeClock {
    type Sleep = Ready<()>;

    fn now(&self) -> Timestamp {
        timestamp(FIXED_NOW)
    }

    fn sleep(&self, _duration: Duration) -> Self::Sleep {
        lock(&self.free_during_sleep).push(self.permits.available_permits());
        ready(())
    }
}

/// A clock whose sleeps never finish.
#[derive(Clone, Copy)]
struct StalledClock;

impl Clock for StalledClock {
    type Sleep = std::future::Pending<()>;

    fn now(&self) -> Timestamp {
        timestamp(FIXED_NOW)
    }

    fn sleep(&self, _duration: Duration) -> Self::Sleep {
        std::future::pending()
    }
}

#[tokio::test]
async fn retries_a_loopback_503_then_returns_the_success() {
    let (url, mut server) =
        loopback_server(vec![response(503, &[], "busy"), response(200, &[], "ok")]).await;
    let clock = TestClock::new(timestamp("Mon, 15 Jul 2024 16:24:59 +0000"));
    let output = request(
        plan(
            clock.clone(),
            1,
            Arc::new(Semaphore::new(2)),
            CancellationToken::new(),
        ),
        api_key(),
        move |credential, cancel| get_once(url.clone(), credential, cancel),
        |oauth| async move { Ok(Credential::OAuth(oauth)) },
    )
    .await
    .expect("request succeeds");
    assert_eq!(output, Some(200));
    assert_eq!(clock.sleeps().len(), 1);
    assert_eq!(
        server
            .join_next()
            .await
            .expect("server completes")
            .expect("server completes")
            .len(),
        2
    );
}

#[tokio::test]
async fn retry_after_http_date_uses_the_controlled_clock() {
    let (url, mut server) = loopback_server(vec![
        response(
            429,
            &[("Retry-After", "Mon, 15 Jul 2024 16:25:02 GMT")],
            "slow down",
        ),
        response(200, &[], "ok"),
    ])
    .await;
    let clock = TestClock::new(timestamp("Mon, 15 Jul 2024 16:24:59 +0000"));
    let output = request(
        plan(
            clock.clone(),
            1,
            Arc::new(Semaphore::new(1)),
            CancellationToken::new(),
        ),
        api_key(),
        move |credential, cancel| get_once(url.clone(), credential, cancel),
        |oauth| async move { Ok(Credential::OAuth(oauth)) },
    )
    .await
    .expect("request succeeds");
    assert_eq!(output, Some(200));
    assert_eq!(clock.sleeps(), vec![Duration::from_secs(3)]);
    assert_eq!(
        server
            .join_next()
            .await
            .expect("server completes")
            .expect("server completes")
            .len(),
        2
    );
}

#[tokio::test]
async fn refuses_retry_after_above_budget_without_sleeping() {
    let (url, mut server) =
        loopback_server(vec![response(429, &[("Retry-After", "120")], "slow down")]).await;
    let clock = TestClock::new(timestamp("Mon, 15 Jul 2024 16:24:59 +0000"));
    let error = request(
        plan(
            clock.clone(),
            4,
            Arc::new(Semaphore::new(1)),
            CancellationToken::new(),
        ),
        api_key(),
        move |credential, cancel| get_once(url.clone(), credential, cancel),
        |oauth| async move { Ok(Credential::OAuth(oauth)) },
    )
    .await
    .expect_err("over-budget retry is refused");
    assert!(
        matches!(&error, ProviderError::RetryAfterTooLong { seconds: 120, message }
            if message == "slow down"),
        "{error:?}"
    );
    assert_eq!(clock.sleeps(), []);
    assert_eq!(
        server
            .join_next()
            .await
            .expect("server completes")
            .expect("server completes")
            .len(),
        1
    );
}

async fn exhausted_rate_limit(
    responses: Vec<String>,
    retries: u32,
) -> (ProviderError, Vec<Duration>) {
    let attempts = responses.len();
    let (url, mut server) = loopback_server(responses).await;
    let clock = TestClock::new(timestamp("Mon, 15 Jul 2024 16:24:59 +0000"));
    let error = request(
        plan(
            clock.clone(),
            retries,
            Arc::new(Semaphore::new(1)),
            CancellationToken::new(),
        ),
        api_key(),
        move |credential, cancel| get_once(url.clone(), credential, cancel),
        |oauth| async move { Ok(Credential::OAuth(oauth)) },
    )
    .await
    .expect_err("every attempt is rate limited");
    assert_eq!(
        server
            .join_next()
            .await
            .expect("server completes")
            .expect("server completes")
            .len(),
        attempts
    );
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

    let (error, sleeps) = exhausted_rate_limit(
        vec![
            response(429, &[], "first"),
            response(
                429,
                &[("Retry-After", "Mon, 15 Jul 2024 16:25:02 GMT")],
                "final",
            ),
        ],
        1,
    )
    .await;
    assert_eq!(sleeps.len(), 1);
    let expected = Duration::from_secs(3)
        .checked_sub(sleeps[0])
        .expect("first backoff is shorter than the 3 s date window");
    assert!(
        matches!(&error, ProviderError::RateLimited { retry_after: Some(wait), .. }
                if *wait == expected),
        "{error:?} sleeps={sleeps:?}"
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
        matches!(
            &error,
            ProviderError::RateLimited {
                retry_after: None,
                ..
            }
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_three_hundred_status_is_final_and_keeps_its_status() {
    let (url, mut server) = loopback_server(vec![response(302, &[], "redirect")]).await;
    let error = request(
        plan(
            TestClock::new(timestamp("Mon, 15 Jul 2024 16:24:59 +0000")),
            4,
            Arc::new(Semaphore::new(1)),
            CancellationToken::new(),
        ),
        api_key(),
        move |credential, cancel| get_once(url.clone(), credential, cancel),
        |oauth| async move { Ok(Credential::OAuth(oauth)) },
    )
    .await
    .expect_err("3xx is not a successful provider response");
    assert!(matches!(error, ProviderError::Status { status: 302, .. }));
    assert_eq!(
        server
            .join_next()
            .await
            .expect("server completes")
            .expect("server completes")
            .len(),
        1
    );
}

#[tokio::test]
async fn retries_a_refused_loopback_connection() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind temporary port");
    let address = listener.local_addr().expect("read temporary port");
    drop(listener);
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempt_count = Arc::clone(&attempts);
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
            get_once(url.clone(), credential, cancel)
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
    let (url, mut server) = loopback_server(vec![
        response(401, &[], "expired"),
        response(200, &[], "ok"),
    ])
    .await;
    let refreshes = Arc::new(AtomicUsize::new(0));
    let refresh_count = Arc::clone(&refreshes);
    let output = request(
        plan(
            TestClock::new(timestamp("Mon, 15 Jul 2024 16:24:59 +0000")),
            0,
            Arc::new(Semaphore::new(1)),
            CancellationToken::new(),
        ),
        oauth("old"),
        move |credential, cancel| get_once(url.clone(), credential, cancel),
        move |_held| {
            refresh_count.fetch_add(1, Ordering::SeqCst);
            async { Ok(oauth("new")) }
        },
    )
    .await
    .expect("request succeeds after refresh");
    assert_eq!(output, Some(200));
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    let requests = server
        .join_next()
        .await
        .expect("server completes")
        .expect("server completes");
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0]
            .to_ascii_lowercase()
            .contains("authorization: bearer old")
    );
    assert!(
        requests[1]
            .to_ascii_lowercase()
            .contains("authorization: bearer new")
    );
}

#[tokio::test]
async fn a_second_oauth_401_is_sign_in_expired() {
    let (url, mut server) = loopback_server(vec![
        response(401, &[], "expired"),
        response(401, &[], "still expired"),
    ])
    .await;
    let refreshes = Arc::new(AtomicUsize::new(0));
    let refresh_count = Arc::clone(&refreshes);
    let error = request(
        plan(
            TestClock::new(timestamp("Mon, 15 Jul 2024 16:24:59 +0000")),
            3,
            Arc::new(Semaphore::new(1)),
            CancellationToken::new(),
        ),
        oauth("old"),
        move |credential, cancel| get_once(url.clone(), credential, cancel),
        move |_held| {
            refresh_count.fetch_add(1, Ordering::SeqCst);
            async { Ok(oauth("new")) }
        },
    )
    .await
    .expect_err("a refreshed token's 401 is terminal");
    assert!(matches!(error, ProviderError::SignInExpired { .. }));
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(
        server
            .join_next()
            .await
            .expect("server completes")
            .expect("server completes")
            .len(),
        2
    );
}

#[tokio::test]
async fn api_key_401_is_typed_and_never_refreshes() {
    let (url, mut server) = loopback_server(vec![response(401, &[], "bad key")]).await;
    let refreshes = Arc::new(AtomicUsize::new(0));
    let refresh_count = Arc::clone(&refreshes);
    let error = request(
        plan(
            TestClock::new(timestamp("Mon, 15 Jul 2024 16:24:59 +0000")),
            3,
            Arc::new(Semaphore::new(1)),
            CancellationToken::new(),
        ),
        api_key(),
        move |credential, cancel| get_once(url.clone(), credential, cancel),
        move |_held| {
            refresh_count.fetch_add(1, Ordering::SeqCst);
            async { Ok(oauth("unexpected")) }
        },
    )
    .await
    .expect_err("API-key 401 is final");
    assert!(matches!(error, ProviderError::AuthRejected { .. }));
    assert_eq!(refreshes.load(Ordering::SeqCst), 0);
    assert_eq!(
        server
            .join_next()
            .await
            .expect("server completes")
            .expect("server completes")
            .len(),
        1
    );
}

#[tokio::test]
async fn retries_an_in_stream_overload_only_before_delivery() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempt_count = Arc::clone(&attempts);
    let mut events = stream(
        plan(
            TestClock::new(timestamp("Mon, 15 Jul 2024 16:24:59 +0000")),
            1,
            Arc::new(Semaphore::new(1)),
            CancellationToken::new(),
        ),
        api_key(),
        move |_credential, _cancel| {
            let first = attempt_count.fetch_add(1, Ordering::SeqCst) == 0;
            async move {
                let output: Vec<Result<StreamEvent, ProviderError>> = if first {
                    vec![Err(ProviderError::Overloaded)]
                } else {
                    vec![Ok(StreamEvent::Stop {
                        reason: StopReason::EndTurn,
                    })]
                };
                Ok(Some(EventStream::new(stream::iter(output), || {})))
            }
        },
        |oauth| async move { Ok(Credential::OAuth(oauth)) },
    );
    assert!(matches!(
        events.next().await,
        Some(Ok(StreamEvent::Stop { .. }))
    ));
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert!(events.next().await.is_none());
}

#[tokio::test]
async fn error_after_any_event_is_not_replayed() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempt_count = Arc::clone(&attempts);
    let mut events = stream(
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
                let output = vec![
                    Ok(StreamEvent::TextDelta {
                        text: String::from("partial"),
                    }),
                    Err(ProviderError::Overloaded),
                ];
                Ok(Some(EventStream::new(stream::iter(output), || {})))
            }
        },
        |oauth| async move { Ok(Credential::OAuth(oauth)) },
    );
    assert!(matches!(
        events.next().await,
        Some(Ok(StreamEvent::TextDelta { .. }))
    ));
    assert!(matches!(
        events.next().await,
        Some(Err(ProviderError::Overloaded))
    ));
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert!(events.next().await.is_none());
}

#[tokio::test]
async fn a_loopback_cut_after_text_is_not_replayed() {
    let (url, mut server) = loopback_server(vec![String::from(
        "HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nfirst",
    )])
    .await;
    let mut events = stream(
        plan(
            TestClock::new(timestamp("Mon, 15 Jul 2024 16:24:59 +0000")),
            5,
            Arc::new(Semaphore::new(1)),
            CancellationToken::new(),
        ),
        api_key(),
        move |credential, cancel| stalled_once(url.clone(), credential, cancel),
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
    assert_eq!(
        server
            .join_next()
            .await
            .expect("server completes")
            .expect("server completes")
            .len(),
        1
    );
}

#[tokio::test]
async fn cancellation_drops_the_stalled_attempt_without_emitting_a_terminal() {
    let cancel = CancellationToken::new();
    let closed = Arc::new(AtomicUsize::new(0));
    let closed_by_stream = Arc::clone(&closed);
    let mut events = stream(
        plan(
            TestClock::new(timestamp("Mon, 15 Jul 2024 16:24:59 +0000")),
            1,
            Arc::new(Semaphore::new(1)),
            cancel.clone(),
        ),
        api_key(),
        move |_credential, _cancel| {
            let closed = Arc::clone(&closed_by_stream);
            async move {
                Ok(Some(EventStream::new(
                    stream::pending::<Result<StreamEvent, ProviderError>>(),
                    move || {
                        closed.fetch_add(1, Ordering::SeqCst);
                    },
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
        plan(
            TestClock::new(timestamp("Mon, 15 Jul 2024 16:24:59 +0000")),
            1,
            Arc::new(Semaphore::new(1)),
            CancellationToken::new(),
        ),
        api_key(),
        move |_credential, _cancel| {
            let closed = Arc::clone(&closed_by_stream);
            async move {
                Ok(Some(EventStream::new(
                    stream::pending::<Result<StreamEvent, ProviderError>>(),
                    move || {
                        closed.fetch_add(1, Ordering::SeqCst);
                    },
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
    let (url, mut server) = stalled_loopback_server().await;
    let cancel = CancellationToken::new();
    let mut events = stream(
        plan(
            TestClock::new(timestamp("Mon, 15 Jul 2024 16:24:59 +0000")),
            1,
            Arc::new(Semaphore::new(1)),
            cancel.clone(),
        ),
        api_key(),
        move |credential, cancel| stalled_once(url.clone(), credential, cancel),
        |oauth| async move { Ok(Credential::OAuth(oauth)) },
    );
    assert!(matches!(
        events.next().await,
        Some(Ok(StreamEvent::TextDelta { text })) if text == "first"
    ));
    let cancelled_at = Instant::now();
    cancel.cancel();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), events.next())
            .await
            .is_err()
    );
    assert!(
        server
            .join_next()
            .await
            .expect("server observes socket close")
            .expect("server observes socket close")
    );
    assert!(cancelled_at.elapsed() < Duration::from_millis(250));
}

#[tokio::test]
async fn dropping_a_loopback_stream_closes_its_socket() {
    let (url, mut server) = stalled_loopback_server().await;
    let mut events = stream(
        plan(
            TestClock::new(timestamp("Mon, 15 Jul 2024 16:24:59 +0000")),
            1,
            Arc::new(Semaphore::new(1)),
            CancellationToken::new(),
        ),
        api_key(),
        move |credential, cancel| stalled_once(url.clone(), credential, cancel),
        |oauth| async move { Ok(Credential::OAuth(oauth)) },
    );
    assert!(matches!(
        events.next().await,
        Some(Ok(StreamEvent::TextDelta { text })) if text == "first"
    ));
    let dropped_at = Instant::now();
    drop(events);
    assert!(
        server
            .join_next()
            .await
            .expect("server observes socket close")
            .expect("server observes socket close")
    );
    assert!(dropped_at.elapsed() < Duration::from_millis(250));
}

#[tokio::test]
async fn two_hundred_requests_obey_the_configured_semaphore_cap() {
    let permits = Arc::new(Semaphore::new(4));
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..200 {
        let permits = Arc::clone(&permits);
        let active = Arc::clone(&active);
        let maximum = Arc::clone(&maximum);
        tasks.spawn(async move {
            request(
                Plan::new(
                    Family::Responses,
                    "openai",
                    "gpt-6",
                    0,
                    permits,
                    CancellationToken::new(),
                    notice_sink(),
                ),
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
        });
    }
    while let Some(joined) = tasks.join_next().await {
        assert!(matches!(
            joined.expect("request task completes"),
            Ok(Some(()))
        ));
    }
    assert_eq!(maximum.load(Ordering::SeqCst), 4);
}

#[test]
fn retry_after_date_parser_accepts_http_date_and_rejects_garbage() {
    let now = timestamp("Mon, 15 Jul 2024 16:24:59 +0000");
    assert_eq!(
        retry_after_seconds("Mon, 15 Jul 2024 16:25:02 GMT", now),
        Some(3.0)
    );
    assert_eq!(retry_after_seconds("not a date", now), None);
}

#[test]
fn hostile_retry_after_values_never_panic_or_shorten_the_wait() {
    let now = timestamp("Mon, 15 Jul 2024 16:24:59 +0000");
    let wait =
        |value: &str| crate::retry::delay_for_attempt(1, retry_after_seconds(value, now), 1.0);
    assert_eq!(wait("7"), Ok(Duration::from_secs(7)));
    assert_eq!(wait("2.5"), Ok(Duration::from_millis(2_500)));
    assert_eq!(wait("0"), Ok(Duration::ZERO));
    assert_eq!(wait("60.0"), Ok(Duration::from_secs(60)));
    assert_eq!(wait("60.2").unwrap_err().seconds, 61);
    assert_eq!(wait("  7  "), Ok(Duration::from_secs(7)));
    assert_eq!(wait("60"), Ok(Duration::from_secs(60)));
    assert_eq!(wait("61").unwrap_err().seconds, 61);
    assert_eq!(wait("-5"), Ok(Duration::ZERO));
    assert_eq!(wait("NaN"), Ok(Duration::from_secs(1)));
    assert_eq!(wait("inf").unwrap_err().seconds, u64::MAX);
    assert_eq!(wait("1e400").unwrap_err().seconds, u64::MAX);
    assert_eq!(wait(""), Ok(Duration::from_secs(1)));
    assert_eq!(wait("Mon, 15 Jul 2024 16:24:00 GMT"), Ok(Duration::ZERO));
    assert_eq!(
        wait("Mon, 15 Jul 2024 16:26:59 GMT").unwrap_err().seconds,
        120
    );
}

#[tokio::test]
async fn backoff_doubles_per_retry_and_attempts_stop_at_one_plus_max_retries() {
    let clock = TestClock::new(timestamp(FIXED_NOW));
    let counts = Counts::default();
    let notices = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut retrying = plan(
        clock.clone(),
        3,
        Arc::new(Semaphore::new(1)),
        CancellationToken::new(),
    );
    let sink = Arc::clone(&notices);
    retrying.notices = Arc::new(move |notice| lock(&sink).push(notice));
    let error = run(
        retrying,
        api_key(),
        &counts,
        |number| {
            if number > 20 {
                return Ok(None);
            }
            Err(failure(503, None, "busy"))
        },
        never_refreshed,
    )
    .await
    .expect_err("every attempt is a 503");
    assert!(
        matches!(&error, ProviderError::Status { family: Family::Responses, status: 503, message }
            if message == "busy"),
        "{error:?}"
    );
    assert_eq!(counts.attempts(), 4);
    let sleeps = clock.sleeps();
    assert_eq!(sleeps.len(), 3, "{sleeps:?}");
    for (index, wait) in sleeps.iter().enumerate() {
        let base = f64::from(1_u32 << index);
        let secs = wait.as_secs_f64();
        assert!(
            (base * 0.9 - 1e-6..=base * 1.1 + 1e-6).contains(&secs),
            "retry {index} waited {secs} s, expected about {base} s"
        );
    }
    let shown = lock(&notices).clone();
    assert_eq!(shown.len(), 3, "{shown:?}");
    for (index, notice) in shown.iter().enumerate() {
        assert!(notice.starts_with("Retrying in "), "{notice}");
        assert!(
            notice.ends_with(&format!("s (attempt {}/3).", index + 1)),
            "{notice}"
        );
    }
}

#[tokio::test]
async fn a_huge_configured_retry_count_still_ends_after_one_hundred_retries() {
    let clock = TestClock::new(timestamp(FIXED_NOW));
    let counts = Counts::default();
    let error = run(
        plan(
            clock.clone(),
            u32::MAX,
            Arc::new(Semaphore::new(1)),
            CancellationToken::new(),
        ),
        api_key(),
        &counts,
        |number| {
            if number > 500 {
                return Ok(None);
            }
            Err(failure(503, None, "busy"))
        },
        never_refreshed,
    )
    .await
    .expect_err("the retry budget is capped at 100");
    assert!(
        matches!(&error, ProviderError::Status { status: 503, .. }),
        "{error:?}"
    );
    assert_eq!(counts.attempts(), 101);
    let sleeps = clock.sleeps();
    assert_eq!(sleeps.len(), 100);
    assert!(sleeps.iter().all(|wait| *wait <= Duration::from_secs(60)));
}

#[tokio::test]
async fn a_request_keeps_its_permit_through_every_backoff_sleep() {
    let permits = Arc::new(Semaphore::new(1));
    let clock = PermitProbeClock {
        permits: Arc::clone(&permits),
        free_during_sleep: Arc::new(Mutex::new(Vec::new())),
    };
    let counts = Counts::default();
    let output = run(
        plan(
            clock.clone(),
            2,
            Arc::clone(&permits),
            CancellationToken::new(),
        ),
        api_key(),
        &counts,
        |number| {
            if number < 2 {
                return Err(failure(503, None, "busy"));
            }
            Ok(Some(200))
        },
        never_refreshed,
    )
    .await
    .expect("the third attempt succeeds");
    assert_eq!(output, Some(200));
    let free = lock(&clock.free_during_sleep).clone();
    assert_eq!(free, vec![0, 0], "the permit is held while sleeping");
    assert_eq!(permits.available_permits(), 1);
}

#[tokio::test]
async fn a_failed_refresh_ends_the_request_with_the_refresh_error_and_no_replay() {
    let permits = Arc::new(Semaphore::new(1));
    let clock = TestClock::new(timestamp(FIXED_NOW));
    let counts = Counts::default();
    let error = run(
        plan(
            clock.clone(),
            3,
            Arc::clone(&permits),
            CancellationToken::new(),
        ),
        oauth("old"),
        &counts,
        |_| Err(failure(401, None, "expired")),
        || {
            Err(ProviderError::TokenExchange {
                status: 400,
                message: String::from("invalid_grant"),
            })
        },
    )
    .await
    .expect_err("a rejected refresh is final");
    assert!(
        matches!(&error, ProviderError::TokenExchange { status: 400, message }
            if message == "invalid_grant"),
        "{error:?}"
    );
    assert_eq!(counts.attempts(), 1);
    assert_eq!(counts.refreshes(), 1);
    assert_eq!(counts.tokens(), ["old"]);
    assert_eq!(clock.sleeps(), []);
    assert_eq!(permits.available_permits(), 1);
}

#[tokio::test]
async fn a_refresh_does_not_spend_the_retry_budget() {
    let clock = TestClock::new(timestamp(FIXED_NOW));
    let counts = Counts::default();
    let output = run(
        plan(
            clock.clone(),
            1,
            Arc::new(Semaphore::new(1)),
            CancellationToken::new(),
        ),
        oauth("old"),
        &counts,
        |number| match number {
            0 => Err(failure(401, None, "expired")),
            1 => Err(failure(503, None, "busy")),
            _ => Ok(Some(200)),
        },
        || Ok(oauth("new")),
    )
    .await
    .expect("one refresh and one retry still fit in max_retries = 1");
    assert_eq!(output, Some(200));
    assert_eq!(counts.attempts(), 3);
    assert_eq!(counts.refreshes(), 1);
    assert_eq!(counts.tokens(), ["old", "new", "new"]);
    assert_eq!(clock.sleeps().len(), 1);
}

#[tokio::test]
async fn the_single_refresh_is_not_re_armed_by_a_later_retry() {
    let counts = Counts::default();
    let error = run(
        plan(
            TestClock::new(timestamp(FIXED_NOW)),
            2,
            Arc::new(Semaphore::new(1)),
            CancellationToken::new(),
        ),
        oauth("old"),
        &counts,
        |number| match number {
            1 => Err(failure(503, None, "busy")),
            _ => Err(failure(401, None, "expired")),
        },
        || Ok(oauth("new")),
    )
    .await
    .expect_err("the refreshed token's 401 is terminal");
    assert!(
        matches!(&error, ProviderError::SignInExpired { provider } if provider == "openai"),
        "{error:?}"
    );
    assert_eq!(counts.attempts(), 3);
    assert_eq!(counts.refreshes(), 1);
    assert_eq!(counts.tokens(), ["old", "new", "new"]);
}

#[tokio::test]
async fn an_over_budget_retry_after_keeps_the_server_message_on_every_path() {
    for max_retries in [0, 2] {
        for (header, seconds) in [("61", 61_u64), ("60.5", 61), ("120", 120)] {
            let clock = TestClock::new(timestamp(FIXED_NOW));
            let counts = Counts::default();
            let error = run(
                plan(
                    clock.clone(),
                    max_retries,
                    Arc::new(Semaphore::new(1)),
                    CancellationToken::new(),
                ),
                api_key(),
                &counts,
                |_| Err(failure(429, Some(header), "slow down")),
                never_refreshed,
            )
            .await
            .expect_err("a wait over 60 s is refused");
            assert!(
                matches!(&error, ProviderError::RetryAfterTooLong { seconds: got, message }
                    if *got == seconds && message == "slow down"),
                "retries {max_retries}, Retry-After {header}: {error:?}"
            );
            assert_eq!(counts.attempts(), 1);
            assert_eq!(clock.sleeps(), []);
        }
    }
}

#[tokio::test]
async fn a_sixty_second_retry_after_is_honored_exactly_not_refused() {
    let clock = TestClock::new(timestamp(FIXED_NOW));
    let counts = Counts::default();
    let output = run(
        plan(
            clock.clone(),
            1,
            Arc::new(Semaphore::new(1)),
            CancellationToken::new(),
        ),
        api_key(),
        &counts,
        |number| {
            if number == 0 {
                return Err(failure(429, Some("60"), "slow down"));
            }
            Ok(Some(200))
        },
        never_refreshed,
    )
    .await
    .expect("a 60 s wait is inside the budget");
    assert_eq!(output, Some(200));
    assert_eq!(clock.sleeps(), vec![Duration::from_secs(60)]);

    let exhausted = run(
        plan(
            TestClock::new(timestamp(FIXED_NOW)),
            0,
            Arc::new(Semaphore::new(1)),
            CancellationToken::new(),
        ),
        api_key(),
        &Counts::default(),
        |_| Err(failure(429, Some("60"), "slow down")),
        never_refreshed,
    )
    .await
    .expect_err("no retry is left");
    assert!(
        matches!(&exhausted, ProviderError::RateLimited { message, retry_after: Some(wait) }
            if message == "slow down" && *wait == Duration::from_secs(60)),
        "{exhausted:?}"
    );
}

#[tokio::test]
async fn a_provider_error_that_is_not_transient_is_final_on_the_first_attempt() {
    fn final_errors() -> Vec<ProviderError> {
        vec![
            ProviderError::Protocol {
                family: Family::Responses,
                detail: String::from("bad frame"),
            },
            ProviderError::Limit(LimitError::SseLine),
            ProviderError::PlainHttp {
                host: String::from("example.com"),
            },
            ProviderError::Quota {
                message: String::from("no credits"),
            },
            ProviderError::UnknownModel {
                reference: String::from("nope"),
            },
        ]
    }
    for index in 0..final_errors().len() {
        let expected = format!("{:?}", final_errors()[index]);
        let clock = TestClock::new(timestamp(FIXED_NOW));
        let counts = Counts::default();
        let error = run(
            plan(
                clock.clone(),
                5,
                Arc::new(Semaphore::new(1)),
                CancellationToken::new(),
            ),
            api_key(),
            &counts,
            move |_| Err(AttemptFailure::Provider(final_errors().swap_remove(index))),
            never_refreshed,
        )
        .await
        .expect_err("a non-transient provider error is final");
        assert_eq!(format!("{error:?}"), expected);
        assert_eq!(counts.attempts(), 1, "{expected}");
        assert_eq!(clock.sleeps(), [], "{expected}");
    }
}

#[tokio::test]
async fn a_closed_admission_semaphore_is_a_typed_transport_failure_without_an_attempt() {
    let permits = Arc::new(Semaphore::new(1));
    permits.close();
    let counts = Counts::default();
    let error = run(
        plan(
            TestClock::new(timestamp(FIXED_NOW)),
            3,
            Arc::clone(&permits),
            CancellationToken::new(),
        ),
        api_key(),
        &counts,
        |_| Ok(Some(200)),
        never_refreshed,
    )
    .await
    .expect_err("admission is closed");
    assert!(
        matches!(&error, ProviderError::Transport { family: Family::Responses, reason }
            if reason == "provider request admission is closed"),
        "{error:?}"
    );
    assert_eq!(counts.attempts(), 0);

    let attempts = Arc::new(AtomicUsize::new(0));
    let mut events = scripted_stream(
        permits,
        CancellationToken::new(),
        3,
        Arc::clone(&attempts),
        |_| vec![Ok(stop())],
    );
    let first = tokio::time::timeout(Duration::from_secs(2), events.next())
        .await
        .expect("a closed admission fails the stream instead of stalling it");
    assert!(
        matches!(&first, Some(Err(ProviderError::Transport { reason, .. }))
            if reason == "provider request admission is closed"),
        "{first:?}"
    );
    assert!(events.next().await.is_none());
    assert_eq!(attempts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cancelling_during_a_backoff_sleep_returns_without_another_attempt() {
    let cancel = CancellationToken::new();
    let permits = Arc::new(Semaphore::new(1));
    let counts = Counts::default();
    let canceller = {
        let cancel = cancel.clone();
        let attempts = Arc::clone(&counts.attempts);
        async move {
            while attempts.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
            cancel.cancel();
        }
    };
    let running = run(
        plan(StalledClock, 3, Arc::clone(&permits), cancel),
        api_key(),
        &counts,
        |_| Err(failure(503, None, "busy")),
        never_refreshed,
    );
    let (output, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(running, canceller)
    })
    .await
    .expect("cancellation ends a backoff sleep that never finishes");
    assert!(matches!(output, Ok(None)), "{output:?}");
    assert_eq!(counts.attempts(), 1);
    assert_eq!(permits.available_permits(), 1);
}

#[tokio::test]
async fn cancelling_during_an_oauth_refresh_returns_without_replaying_the_request() {
    let cancel = CancellationToken::new();
    let permits = Arc::new(Semaphore::new(1));
    let attempts = Arc::new(AtomicUsize::new(0));
    let refreshes = Arc::new(AtomicUsize::new(0));
    let canceller = {
        let cancel = cancel.clone();
        let refreshes = Arc::clone(&refreshes);
        async move {
            while refreshes.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
            cancel.cancel();
        }
    };
    let attempt_count = Arc::clone(&attempts);
    let refresh_count = Arc::clone(&refreshes);
    let running = request(
        plan(
            TestClock::new(timestamp(FIXED_NOW)),
            3,
            Arc::clone(&permits),
            cancel,
        ),
        oauth("old"),
        move |_credential, _cancel| {
            attempt_count.fetch_add(1, Ordering::SeqCst);
            async { Err::<Option<()>, _>(failure(401, None, "expired")) }
        },
        move |_held| {
            refresh_count.fetch_add(1, Ordering::SeqCst);
            std::future::pending::<Result<Credential, ProviderError>>()
        },
    );
    let (output, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(running, canceller)
    })
    .await
    .expect("cancellation ends a refresh that never finishes");
    assert!(matches!(output, Ok(None)), "{output:?}");
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(permits.available_permits(), 1);
}

#[tokio::test]
async fn every_event_kind_sets_the_delivery_barrier_so_a_later_error_is_not_replayed() {
    let kinds = vec![
        text("t"),
        StreamEvent::ReasoningDelta {
            text: String::from("r"),
        },
        StreamEvent::ToolCallStarted {
            id: String::from("c1"),
            name: String::from("read"),
        },
        StreamEvent::ToolArgsDelta {
            id: String::from("c1"),
            fragment: b"{".to_vec(),
        },
        StreamEvent::Replay {
            payload: ReplayPayload {
                family: Family::Responses,
                model: "gpt-6".into(),
                item: RawJson::parse("{}").expect("an empty object is JSON"),
            },
        },
        StreamEvent::ToolCallsDone { calls: Vec::new() },
        StreamEvent::MessageReplay {
            capture: ReplayCapture::Refused {
                reason: ReplayRefusal::NoRecords,
            },
        },
        StreamEvent::Usage {
            usage: Usage {
                input_tokens: 1,
                cached_input_tokens: 0,
                output_tokens: 1,
                reasoning_tokens: None,
                cache_write_tokens: 0,
                premium_requests: None,
                cost_usd: None,
            },
        },
    ];
    for kind in kinds {
        let attempts = Arc::new(AtomicUsize::new(0));
        let sent = kind.clone();
        let mut events = scripted_stream(
            Arc::new(Semaphore::new(1)),
            CancellationToken::new(),
            5,
            Arc::clone(&attempts),
            move |_| vec![Ok(sent.clone()), Err(ProviderError::Overloaded)],
        );
        let first = events.next().await;
        assert!(
            matches!(&first, Some(Ok(event)) if *event == kind),
            "{kind:?}: {first:?}"
        );
        let second = events.next().await;
        assert!(
            matches!(&second, Some(Err(ProviderError::Overloaded))),
            "{kind:?}: {second:?}"
        );
        assert!(events.next().await.is_none(), "{kind:?}");
        assert_eq!(attempts.load(Ordering::SeqCst), 1, "{kind:?}");
    }
}

#[tokio::test]
async fn nothing_follows_the_terminal_stop() {
    let tails: [fn() -> Result<StreamEvent, ProviderError>; 3] = [
        || Ok(text("late")),
        || Ok(stop()),
        || Err(ProviderError::Overloaded),
    ];
    for tail in tails {
        let permits = Arc::new(Semaphore::new(1));
        let attempts = Arc::new(AtomicUsize::new(0));
        let mut events = scripted_stream(
            Arc::clone(&permits),
            CancellationToken::new(),
            5,
            Arc::clone(&attempts),
            move |_| vec![Ok(text("hello")), Ok(stop()), tail()],
        );
        assert!(matches!(
            events.next().await,
            Some(Ok(StreamEvent::TextDelta { .. }))
        ));
        assert!(matches!(
            events.next().await,
            Some(Ok(StreamEvent::Stop { .. }))
        ));
        assert!(events.next().await.is_none());
        assert!(events.next().await.is_none());
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(permits.available_permits(), 1);
    }
}

#[tokio::test]
async fn the_permit_is_released_at_the_terminal_stop_before_the_stream_is_dropped() {
    let permits = Arc::new(Semaphore::new(1));
    let mut events = scripted_stream(
        Arc::clone(&permits),
        CancellationToken::new(),
        0,
        Arc::default(),
        |_| vec![Ok(text("hello")), Ok(stop())],
    );
    assert!(matches!(
        events.next().await,
        Some(Ok(StreamEvent::TextDelta { .. }))
    ));
    assert_eq!(
        permits.available_permits(),
        0,
        "held while the reply streams"
    );
    assert!(matches!(
        events.next().await,
        Some(Ok(StreamEvent::Stop { .. }))
    ));
    assert_eq!(
        permits.available_permits(),
        1,
        "the terminal event releases the permit"
    );
    assert!(events.next().await.is_none());
}

#[tokio::test]
async fn cancellation_releases_the_permit_while_the_stream_is_still_alive() {
    let permits = Arc::new(Semaphore::new(1));
    let cancel = CancellationToken::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut events = stream_probing_permits(&permits, cancel.clone(), &seen);
    assert!(events.next().now_or_never().is_none());
    assert_eq!(permits.available_permits(), 0);
    cancel.cancel();
    assert!(events.next().now_or_never().is_none());
    assert_eq!(permits.available_permits(), 1);
    assert_eq!(
        *lock(&seen),
        [0],
        "the attempt closed while the permit was held"
    );
}

#[tokio::test]
async fn dropping_the_stream_closes_the_attempt_before_releasing_its_permit() {
    let permits = Arc::new(Semaphore::new(1));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut events = stream_probing_permits(&permits, CancellationToken::new(), &seen);
    assert!(events.next().now_or_never().is_none());
    assert_eq!(permits.available_permits(), 0);
    drop(events);
    assert_eq!(permits.available_permits(), 1);
    assert_eq!(
        *lock(&seen),
        [0],
        "the socket must be gone before the permit frees"
    );
}

#[tokio::test]
async fn an_empty_stream_before_any_event_is_a_cut_that_is_retried_then_surfaced() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let mut events = scripted_stream(
        Arc::new(Semaphore::new(1)),
        CancellationToken::new(),
        1,
        Arc::clone(&attempts),
        |number| {
            if number == 0 {
                return Vec::new();
            }
            vec![Ok(stop())]
        },
    );
    assert!(matches!(
        events.next().await,
        Some(Ok(StreamEvent::Stop { .. }))
    ));
    assert!(events.next().await.is_none());
    assert_eq!(attempts.load(Ordering::SeqCst), 2);

    let attempts = Arc::new(AtomicUsize::new(0));
    let mut exhausted = scripted_stream(
        Arc::new(Semaphore::new(1)),
        CancellationToken::new(),
        0,
        Arc::clone(&attempts),
        |_| Vec::new(),
    );
    let Some(Err(error)) = exhausted.next().await else {
        panic!("an empty stream with no retry left must surface an error");
    };
    assert!(
        matches!(
            InferFailure::from(error),
            InferFailure::Retryable { message, .. }
                if message.contains("stream cut off before completion")
        ),
        "a cut is transient"
    );
    assert!(exhausted.next().await.is_none());
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_websocket_close_after_delivery_keeps_its_code_and_before_it_is_retried() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let mut events = scripted_stream(
        Arc::new(Semaphore::new(1)),
        CancellationToken::new(),
        5,
        Arc::clone(&attempts),
        |_| {
            vec![
                Ok(text("partial")),
                Err(ProviderError::WsClosed {
                    code: Some((1011, String::from("busy"))),
                }),
            ]
        },
    );
    assert!(matches!(
        events.next().await,
        Some(Ok(StreamEvent::TextDelta { .. }))
    ));
    let closed = events.next().await;
    assert!(
        matches!(&closed, Some(Err(ProviderError::WsClosed { code: Some((1011, reason)) }))
            if reason == "busy"),
        "{closed:?}"
    );
    assert!(events.next().await.is_none());
    assert_eq!(attempts.load(Ordering::SeqCst), 1);

    let attempts = Arc::new(AtomicUsize::new(0));
    let mut retried = scripted_stream(
        Arc::new(Semaphore::new(1)),
        CancellationToken::new(),
        1,
        Arc::clone(&attempts),
        |number| {
            if number == 0 {
                return vec![Err(ProviderError::WsClosed {
                    code: Some((1006, String::from("gone"))),
                })];
            }
            vec![Ok(stop())]
        },
    );
    assert!(matches!(
        retried.next().await,
        Some(Ok(StreamEvent::Stop { .. }))
    ));
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}
