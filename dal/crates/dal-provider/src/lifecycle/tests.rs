use std::{
    future::{Ready, ready},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};

use dal_core::Family;
use futures::{StreamExt, stream};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

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
    assert!(matches!(
        error,
        ProviderError::RetryAfterTooLong { seconds: 120, .. }
    ));
    assert!(clock.sleeps().is_empty());
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
