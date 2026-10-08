use std::{
    cell::RefCell,
    fs,
    future::Future,
    io::ErrorKind,
    pin::pin,
    rc::Rc,
    sync::atomic::{AtomicU32, Ordering},
    time::Instant,
};

use dal_core::Family;
use futures::future::{Either, join_all, select};
use sonic_rs::JsonValueTrait;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::Notify,
};

use super::engine::fresh_credential;
use super::*;
use crate::auth::credential::{AuthStore, Credential, OAuthCredential, SecretString};
use crate::auth::oauth::{CODEX_CLIENT_ID, unix_now};

const OLD_ACCESS: &str = "old-access-secret";
const OLD_REFRESH: &str = "old-refresh-secret";
const NEW_ACCESS: &str = "new-access-secret";
const NEW_REFRESH: &str = "new-refresh-secret";
const NEW_TOKENS: &str = r#"{"access_token":"new-access-secret","refresh_token":"new-refresh-secret","expires_in":3600,"token_type":"Bearer"}"#;

struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "dal-provider-refresh-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create test directory");
        Self(path)
    }

    fn auth(&self) -> PathBuf {
        self.0.join("auth.json")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

enum Reply {
    Json(u16, String),
    /// Waits after reading the request before answering.
    JsonAfterNotify(Rc<Notify>, Rc<Notify>, u16, String),
}

struct Seen {
    at: Instant,
    head: String,
    body: String,
}

fn codex(access: &str, refresh: &str, expires_at: Option<i64>) -> OAuthCredential {
    OAuthCredential {
        access_token: SecretString::from(access),
        refresh_token: SecretString::from(refresh),
        expires_at,
        id_token: Some(String::from("old-id-token")),
        account_id: Some(String::from("acct-1")),
    }
}

fn seed(path: &Path, credential: OAuthCredential) {
    let mut store = AuthStore::empty(path);
    store
        .set("openai-codex", Credential::OAuth(credential))
        .expect("codex takes oauth");
    store.store().expect("seed auth.json");
}

fn stored(path: &Path) -> OAuthCredential {
    match AuthStore::load(path)
        .expect("reload")
        .credential("openai-codex")
    {
        Some(Credential::OAuth(credential)) => credential,
        other => panic!("expected an OAuth entry, got {other:?}"),
    }
}

fn oauth(credential: Credential) -> OAuthCredential {
    match credential {
        Credential::OAuth(credential) => credential,
        other => panic!("expected OAuth, got {other:?}"),
    }
}

fn refresher(path: &Path, base: &str) -> Refresher {
    let endpoints = TokenEndpoints::with_bases(base, base).expect("loopback base");
    Refresher::new("dalgon/test", path, endpoints)
}

async fn listen() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let port = listener.local_addr().expect("local addr").port();
    (listener, format!("http://127.0.0.1:{port}"))
}

async fn read_request(stream: &TcpStream) -> (String, String) {
    let mut data = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        if let Some(end) = data.windows(4).position(|window| window == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&data[..end]).into_owned();
            let length = head
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .map_or(0, |(_, value)| {
                    value.trim().parse().expect("content length")
                });
            if data.len() >= end + 4 + length {
                let body = String::from_utf8_lossy(&data[end + 4..end + 4 + length]);
                return (head, body.into_owned());
            }
        }
        stream.readable().await.expect("readable");
        match stream.try_read(&mut chunk) {
            Ok(0) => panic!("client closed before the request ended"),
            Ok(read) => data.extend_from_slice(&chunk[..read]),
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            Err(error) => panic!("read request: {error}"),
        }
    }
}

async fn write_response(stream: &TcpStream, status: u16, body: &str) {
    let text = format!(
        "HTTP/1.1 {status} Scripted\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut bytes = text.as_bytes();
    while !bytes.is_empty() {
        stream.writable().await.expect("writable");
        match stream.try_write(bytes) {
            Ok(written) => bytes = &bytes[written..],
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            Err(error) => panic!("write response: {error}"),
        }
    }
}

async fn serve(listener: TcpListener, replies: Vec<Reply>, seen: &RefCell<Vec<Seen>>) {
    for reply in replies {
        let (stream, _) = listener.accept().await.expect("accept");
        let (head, body) = read_request(&stream).await;
        seen.borrow_mut().push(Seen {
            at: Instant::now(),
            head,
            body,
        });
        match reply {
            Reply::Json(status, body) => write_response(&stream, status, &body).await,
            Reply::JsonAfterNotify(started, release, status, body) => {
                started.notify_one();
                release.notified().await;
                write_response(&stream, status, &body).await;
            }
        }
    }
    std::future::pending::<()>().await;
}

/// Runs `client` on this task while the loopback server answers with
/// `replies` in order; returns the client output and every request seen.
async fn with_server<T>(
    listener: TcpListener,
    replies: Vec<Reply>,
    client: impl Future<Output = T>,
) -> (T, Vec<Seen>) {
    let seen = RefCell::new(Vec::new());
    let output = {
        let server = pin!(serve(listener, replies, &seen));
        match select(pin!(client), server).await {
            Either::Left((output, _)) => output,
            Either::Right(((), _)) => panic!("the server stopped"),
        }
    };
    (output, seen.into_inner())
}

fn assert_no_secret(error: &ProviderError, secrets: &[&str]) {
    let texts = [format!("{error}"), format!("{error:?}")];
    for text in &texts {
        for secret in secrets {
            assert!(!text.contains(secret), "{text:?} leaks a secret");
        }
    }
}

#[tokio::test]
async fn sixty_four_callers_across_two_refreshers_send_one_request() {
    let dir = TestDir::new("race");
    let held = codex(OLD_ACCESS, OLD_REFRESH, Some(unix_now() + 10));
    seed(&dir.auth(), held.clone());
    let (listener, base) = listen().await;
    // Two refreshers of one file stand in for two processes: separate
    // key mutexes, one advisory lock per open of auth.json.lock.
    let first = refresher(&dir.auth(), &base);
    let second = refresher(&dir.auth(), &base);
    let callers = (0..64).map(|index| {
        let refresher = if index % 2 == 0 { &first } else { &second };
        refresher.refresh(OAuthProvider::OpenAiCodex, &held, RefreshReason::Expiring)
    });
    let replies = vec![Reply::Json(200, String::from(NEW_TOKENS))];
    let (results, seen) = with_server(listener, replies, join_all(callers)).await;

    assert_eq!(seen.len(), 1);
    assert!(seen[0].head.starts_with("POST /oauth/token HTTP/1.1"));
    let body = sonic_rs::from_str::<sonic_rs::Value>(&seen[0].body).expect("JSON body");
    assert_eq!(
        body.get("grant_type").and_then(JsonValueTrait::as_str),
        Some("refresh_token")
    );
    assert_eq!(
        body.get("client_id").and_then(JsonValueTrait::as_str),
        Some(CODEX_CLIENT_ID)
    );
    assert_eq!(
        body.get("refresh_token").and_then(JsonValueTrait::as_str),
        Some(OLD_REFRESH)
    );
    for result in results {
        let fresh = oauth(result.expect("every caller gets the new token"));
        assert_eq!(fresh.access_token.expose(), NEW_ACCESS);
    }
    let on_disk = stored(&dir.auth());
    assert_eq!(on_disk.access_token.expose(), NEW_ACCESS);
    assert_eq!(on_disk.refresh_token.expose(), NEW_REFRESH);
    assert_eq!(on_disk.id_token.as_deref(), Some("old-id-token"));
    assert_eq!(on_disk.account_id.as_deref(), Some("acct-1"));
    let expires_at = on_disk.expires_at.expect("expires_in sets expires_at");
    assert!((expires_at - unix_now() - 3600).abs() <= 5);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(dir.auth()).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[tokio::test]
async fn external_writer_before_the_lock_prevents_any_request() {
    let dir = TestDir::new("guarded");
    let held = codex(OLD_ACCESS, OLD_REFRESH, Some(unix_now() + 10));
    seed(&dir.auth(), held.clone());
    let (listener, base) = listen().await;
    let refresher = refresher(&dir.auth(), &base);
    // Another process holds the lock while it writes a fresh token.
    let external = open_lock_file(&lock_path(&dir.auth()).expect("lock path")).expect("open lock");
    external.try_lock().expect("external lock");
    let client = async {
        let expiring =
            refresher.refresh(OAuthProvider::OpenAiCodex, &held, RefreshReason::Expiring);
        let rejected =
            refresher.refresh(OAuthProvider::OpenAiCodex, &held, RefreshReason::Rejected);
        let writer = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            seed(
                &dir.auth(),
                codex(NEW_ACCESS, NEW_REFRESH, Some(unix_now() + 3600)),
            );
            drop(external);
        };
        let (expiring, rejected, ()) = tokio::join!(expiring, rejected, writer);
        (expiring, rejected)
    };
    let ((expiring, rejected), seen) = with_server(listener, Vec::new(), client).await;

    assert!(seen.is_empty());
    for result in [expiring, rejected] {
        assert_eq!(
            oauth(result.expect("stored token")).access_token.expose(),
            NEW_ACCESS
        );
    }
}

#[tokio::test]
async fn forced_refresh_ignores_the_window_only_for_an_unchanged_token() {
    let dir = TestDir::new("forced");
    let held = codex(OLD_ACCESS, OLD_REFRESH, Some(unix_now() + 3600));
    seed(&dir.auth(), held.clone());
    let (listener, base) = listen().await;
    let refresher = refresher(&dir.auth(), &base);
    let client = async {
        let early = refresher
            .refresh(OAuthProvider::OpenAiCodex, &held, RefreshReason::Expiring)
            .await;
        let forced = refresher
            .refresh(OAuthProvider::OpenAiCodex, &held, RefreshReason::Rejected)
            .await;
        // A second 401 on the old token finds the changed token: no request.
        let again = refresher
            .refresh(OAuthProvider::OpenAiCodex, &held, RefreshReason::Rejected)
            .await;
        (early, forced, again)
    };
    let replies = vec![Reply::Json(200, String::from(NEW_TOKENS))];
    let ((early, forced, again), seen) = with_server(listener, replies, client).await;

    assert_eq!(seen.len(), 1);
    assert_eq!(
        oauth(early.expect("fresh token")).access_token.expose(),
        OLD_ACCESS
    );
    assert_eq!(
        oauth(forced.expect("forced refresh")).access_token.expose(),
        NEW_ACCESS
    );
    assert_eq!(
        oauth(again.expect("changed token")).access_token.expose(),
        NEW_ACCESS
    );
}

#[tokio::test]
async fn transient_failure_retries_once_after_one_second() {
    let dir = TestDir::new("transient");
    let held = codex(OLD_ACCESS, OLD_REFRESH, Some(unix_now() + 10));
    seed(&dir.auth(), held.clone());
    let (listener, base) = listen().await;
    let refresher = refresher(&dir.auth(), &base);
    let replies = vec![
        Reply::Json(503, String::from(r#"{"error":"temporarily_unavailable"}"#)),
        Reply::Json(200, String::from(NEW_TOKENS)),
    ];
    let client = refresher.refresh(OAuthProvider::OpenAiCodex, &held, RefreshReason::Expiring);
    let (result, seen) = with_server(listener, replies, client).await;

    assert_eq!(
        oauth(result.expect("retry succeeds")).access_token.expose(),
        NEW_ACCESS
    );
    assert_eq!(seen.len(), 2);
    assert!(seen[1].at.duration_since(seen[0].at) >= RETRY_DELAY);
    assert_eq!(stored(&dir.auth()).access_token.expose(), NEW_ACCESS);
}

#[tokio::test]
async fn repeated_transient_failure_gives_status_without_secrets() {
    let dir = TestDir::new("twice");
    let held = codex(OLD_ACCESS, OLD_REFRESH, Some(unix_now() + 10));
    seed(&dir.auth(), held.clone());
    let before = fs::read(dir.auth()).expect("read seed");
    let (listener, base) = listen().await;
    let refresher = refresher(&dir.auth(), &base);
    let echo = format!("backend down for {OLD_REFRESH}");
    let replies = vec![Reply::Json(503, echo.clone()), Reply::Json(503, echo)];
    let client = refresher.refresh(OAuthProvider::OpenAiCodex, &held, RefreshReason::Expiring);
    let (result, seen) = with_server(listener, replies, client).await;

    let error = result.expect_err("two 503 fail");
    assert!(matches!(
        &error,
        ProviderError::Status { family: Family::Codex, status: 503, message }
            if message == "backend down for <redacted>"
    ));
    assert_no_secret(&error, &[OLD_REFRESH, OLD_ACCESS]);
    assert_eq!(seen.len(), 2);
    assert_eq!(fs::read(dir.auth()).expect("read after"), before);
}

#[tokio::test]
async fn rejected_refresh_token_is_sign_in_expired_after_one_request() {
    for body in [
        r#"{"error":"invalid_grant","error_description":"bad"}"#,
        r#"{"error":{"message":"expired","type":"invalid_request_error","code":"refresh_token_expired"}}"#,
        r#"{"error":{"code":"refresh_token_reused"}}"#,
        r#"{"error":"refresh_token_invalidated"}"#,
    ] {
        let dir = TestDir::new("permanent");
        let held = codex(OLD_ACCESS, OLD_REFRESH, Some(unix_now() + 10));
        seed(&dir.auth(), held.clone());
        let before = fs::read(dir.auth()).expect("read seed");
        let (listener, base) = listen().await;
        let refresher = refresher(&dir.auth(), &base);
        let replies = vec![Reply::Json(400, String::from(body))];
        let client = refresher.refresh(OAuthProvider::OpenAiCodex, &held, RefreshReason::Rejected);
        let (result, seen) = with_server(listener, replies, client).await;

        let error = result.expect_err("rejected refresh token");
        assert_eq!(
            error.to_string(),
            "openai-codex sign-in expired: the refresh token was rejected."
        );
        assert_no_secret(&error, &[OLD_REFRESH, OLD_ACCESS]);
        assert_eq!(seen.len(), 1, "{body}");
        assert_eq!(fs::read(dir.auth()).expect("read after"), before);
    }
}

#[tokio::test]
async fn cancelled_refresh_persists_rotated_credential() {
    let dir = TestDir::new("cancel");
    let held = codex(OLD_ACCESS, OLD_REFRESH, Some(unix_now() + 10));
    seed(&dir.auth(), held.clone());
    let (listener, base) = listen().await;
    let refresher = refresher(&dir.auth(), &base);
    let started = Rc::new(Notify::new());
    let release = Rc::new(Notify::new());
    let client = async {
        tokio::select! {
            biased;
            () = started.notified() => {}
            _ = refresher.refresh(OAuthProvider::OpenAiCodex, &held, RefreshReason::Expiring) => {
                panic!("the refresh completed before cancellation")
            }
        }
        // The second caller must join while the first request is still waiting.
        let second = refresher.refresh(OAuthProvider::OpenAiCodex, &held, RefreshReason::Expiring);
        tokio::pin!(second);
        futures::future::poll_fn(|cx| {
            let _ = second.as_mut().poll(cx);
            std::task::Poll::Ready(())
        })
        .await;
        release.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(1), second)
            .await
            .expect("the second caller waits for the in-flight refresh")
            .expect("the persisted refresh is reused");
        let fresh = oauth(result);
        assert_eq!(fresh.access_token.expose(), NEW_ACCESS);
        Credential::OAuth(fresh)
    };
    let replies = vec![
        Reply::JsonAfterNotify(
            Rc::clone(&started),
            Rc::clone(&release),
            200,
            String::from(NEW_TOKENS),
        ),
        Reply::Json(500, String::from(r#"{"error":"duplicate refresh"}"#)),
    ];
    let (result, seen) = with_server(listener, replies, client).await;

    assert_eq!(oauth(result).access_token.expose(), NEW_ACCESS);
    assert_eq!(seen.len(), 1);
    let on_disk = stored(&dir.auth());
    assert_eq!(on_disk.access_token.expose(), NEW_ACCESS);
    assert_eq!(on_disk.refresh_token.expose(), NEW_REFRESH);
    let probe = open_lock_file(&lock_path(&dir.auth()).expect("lock path")).expect("open lock");
    probe.try_lock().expect("the file lock was released");
}

#[tokio::test]
async fn failed_in_flight_refresh_gives_a_later_caller_a_fresh_attempt() {
    let dir = TestDir::new("stale");
    let held = codex(OLD_ACCESS, OLD_REFRESH, Some(unix_now() + 10));
    seed(&dir.auth(), held.clone());
    let (listener, base) = listen().await;
    let refresher = refresher(&dir.auth(), &base);
    let started = Rc::new(Notify::new());
    let release = Rc::new(Notify::new());
    let client = async {
        // The only caller drops its wait once the request is on the wire;
        // the in-flight task then fails alone.
        tokio::select! {
            biased;
            () = started.notified() => {}
            _ = refresher.refresh(OAuthProvider::OpenAiCodex, &held, RefreshReason::Expiring) => {
                panic!("the refresh completed before cancellation")
            }
        }
        release.notify_one();
        // The failed task holds the auth file lock until its future ends,
        // through the exhausted retry; wait for that before the next caller.
        let probe = open_lock_file(&lock_path(&dir.auth()).expect("lock path")).expect("open lock");
        loop {
            match probe.try_lock() {
                Ok(()) => break,
                Err(fs::TryLockError::WouldBlock) => tokio::time::sleep(LOCK_POLL).await,
                Err(error) => panic!("probe the auth file lock: {error}"),
            }
        }
        drop(probe);
        // The server now succeeds. The new caller must run a fresh refresh
        // instead of receiving the finished task's stale error.
        let result = refresher
            .refresh(OAuthProvider::OpenAiCodex, &held, RefreshReason::Expiring)
            .await
            .expect("a finished failed task must be retried fresh");
        let fresh = oauth(result);
        assert_eq!(fresh.access_token.expose(), NEW_ACCESS);
        Credential::OAuth(fresh)
    };
    let hiccup = String::from(r#"{"error":"server hiccup"}"#);
    let replies = vec![
        Reply::JsonAfterNotify(
            Rc::clone(&started),
            Rc::clone(&release),
            500,
            hiccup.clone(),
        ),
        Reply::Json(500, hiccup),
        Reply::Json(200, String::from(NEW_TOKENS)),
    ];
    let (result, seen) = with_server(listener, replies, client).await;

    assert_eq!(oauth(result).access_token.expose(), NEW_ACCESS);
    assert_eq!(seen.len(), 3, "two failed attempts, then one fresh one");
    let body = sonic_rs::from_str::<sonic_rs::Value>(&seen[2].body).expect("JSON body");
    assert_eq!(
        body.get("refresh_token").and_then(JsonValueTrait::as_str),
        Some(OLD_REFRESH),
        "the fresh attempt retries the stored refresh token"
    );
    let on_disk = stored(&dir.auth());
    assert_eq!(on_disk.access_token.expose(), NEW_ACCESS);
    assert_eq!(on_disk.refresh_token.expose(), NEW_REFRESH);
}

#[test]
fn production_endpoints_are_the_documented_urls() {
    let endpoints = TokenEndpoints::production();
    assert_eq!(
        endpoints.url(OAuthProvider::Anthropic).as_str(),
        "https://platform.claude.com/v1/oauth/token"
    );
    assert_eq!(
        endpoints.url(OAuthProvider::OpenAiCodex).as_str(),
        "https://auth.openai.com/oauth/token"
    );
}

#[test]
fn anthropic_refresh_keeps_no_codex_identity_and_old_refresh_token_when_omitted() {
    let stored = OAuthCredential {
        access_token: SecretString::from(OLD_ACCESS),
        refresh_token: SecretString::from(OLD_REFRESH),
        expires_at: Some(1),
        id_token: None,
        account_id: None,
    };
    let body = br#"{"access_token":"a2","expires_in":60}"#;
    let fresh =
        fresh_credential(OAuthProvider::Anthropic, &stored, body, 1_000).expect("valid body");
    assert_eq!(fresh.access_token.expose(), "a2");
    assert_eq!(fresh.refresh_token.expose(), OLD_REFRESH);
    assert_eq!(fresh.expires_at, Some(1_060));
    assert_eq!((fresh.id_token, fresh.account_id), (None, None));
    let error = fresh_credential(OAuthProvider::Anthropic, &stored, br#"{"access_token":"#, 0)
        .expect_err("truncated body");
    assert!(matches!(
        error,
        ProviderError::Transport {
            family: Family::Anthropic,
            ..
        }
    ));
}

#[tokio::test]
async fn the_proactive_window_holds_for_skewed_and_extreme_expiry() {
    let now = unix_now();
    let cases = [
        (Some(i64::MIN), true),
        (Some(now - 100_000), true),
        (Some(now), true),
        (Some(now + PROACTIVE_WINDOW_SECS - 5), true),
        (Some(now + PROACTIVE_WINDOW_SECS + 5), false),
        (Some(i64::MAX), false),
        (None, false),
    ];
    for (expires_at, inside) in cases {
        let dir = TestDir::new("window");
        seed(&dir.auth(), codex(NEW_ACCESS, NEW_REFRESH, Some(i64::MAX)));
        let held = codex(OLD_ACCESS, OLD_REFRESH, expires_at);
        let credential = refresher(&dir.auth(), "http://127.0.0.1:1")
            .refresh(OAuthProvider::OpenAiCodex, &held, RefreshReason::Expiring)
            .await
            .expect("no request is needed");
        let expected = if inside { NEW_ACCESS } else { OLD_ACCESS };
        assert_eq!(
            oauth(credential).access_token.expose(),
            expected,
            "{expires_at:?}"
        );
    }
}

#[test]
fn hostile_token_responses_are_typed_errors_and_never_echo_the_body() {
    let stored = codex(OLD_ACCESS, OLD_REFRESH, Some(1));
    let hostile: [&[u8]; 8] = [
        br#"{"access_token":""}"#,
        br#"{"access_token":"a","expires_in":"3600"}"#,
        br#"{"expires_in":3600}"#,
        b"leaked-body-secret <html>",
        b"\xFF\xFE leaked-body-secret",
        b"[]",
        b"null",
        b"",
    ];
    for body in hostile {
        let error = fresh_credential(OAuthProvider::OpenAiCodex, &stored, body, 0)
            .expect_err("not a token response");
        assert!(matches!(error, ProviderError::Transport { .. }), "{body:?}");
        assert!(!error.to_string().contains("leaked-body-secret"), "{error}");
    }
}

#[test]
fn extreme_expires_in_saturates_and_negative_is_already_expired() {
    let stored = codex(OLD_ACCESS, OLD_REFRESH, Some(1));
    let at = |expires_in: i64| {
        let body = format!(r#"{{"access_token":"a","expires_in":{expires_in}}}"#);
        fresh_credential(OAuthProvider::OpenAiCodex, &stored, body.as_bytes(), 1_000)
            .expect("valid body")
            .expires_at
    };
    assert_eq!(at(i64::MAX), Some(i64::MAX));
    assert_eq!(at(-100), Some(900));
    assert_eq!(at(0), Some(1_000));
}
