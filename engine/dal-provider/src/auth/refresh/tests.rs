use std::{
    cell::RefCell,
    fs,
    future::Future,
    io::ErrorKind,
    pin::pin,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use dal_core::Family;
use futures::future::{Either, join_all, select};
use sonic_rs::JsonValueTrait;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::Notify,
};

use super::engine::{effective_client_id, fresh_credential};
use super::*;
use crate::auth::credential::{AuthStore, Credential, EnvSnapshot, OAuthCredential, SecretString};
use crate::auth::oauth::unix_now;
use crate::{PROVIDERS, ProviderDef, find};

const OLD_ACCESS: &str = "old-access-secret";
const OLD_REFRESH: &str = "old-refresh-secret";
const NEW_ACCESS: &str = "new-access-secret";
const NEW_REFRESH: &str = "new-refresh-secret";
const NEW_TOKENS: &str = r#"{"access_token":"new-access-secret","refresh_token":"new-refresh-secret","expires_in":3600,"token_type":"Bearer"}"#;
/// A `WorkOS` authenticate answer: no `expires_in`, extra identity fields.
const WORKOS_TOKENS: &str = r#"{"access_token":"new-access-secret","refresh_token":"new-refresh-secret","user":{"id":"user_1","email":"a@b.c"},"organization_id":"org_internal"}"#;
const ORG_ANSWER: &str = r#"{"orgId":"org_1"}"#;
const CANCELLED: &str = "credential refresh cancelled: the refresher is shut down";

fn codex_def() -> &'static ProviderDef {
    find("openai-codex").expect("the table has the openai-codex row")
}

fn claude_def() -> &'static ProviderDef {
    find("anthropic").expect("the table has the anthropic row")
}

fn droid_def() -> &'static ProviderDef {
    find("factory-droid").expect("the table has the factory-droid row")
}

fn gitlab_def() -> &'static ProviderDef {
    find("gitlab-duo").expect("the table has the gitlab-duo row")
}

fn row_client_id(def: &ProviderDef) -> &'static str {
    def.oauth.expect("an OAuth row").client_id
}

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
    /// Reads the request, then closes the connection without answering.
    Close,
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

/// An OAuth credential of a row that carries neither ID token nor account.
fn plain(access: &str, refresh: &str, expires_at: Option<i64>) -> OAuthCredential {
    OAuthCredential {
        access_token: SecretString::from(access),
        refresh_token: SecretString::from(refresh),
        expires_at,
        id_token: None,
        account_id: None,
    }
}

fn seed(path: &Path, credential: OAuthCredential) {
    seed_as(path, "openai-codex", credential);
}

fn seed_as(path: &Path, id: &str, credential: OAuthCredential) {
    let mut store = AuthStore::empty(path);
    store
        .set(id, Credential::OAuth(credential))
        .expect("the row takes oauth");
    store.store().expect("seed auth.json");
}

fn stored(path: &Path) -> OAuthCredential {
    stored_as(path, "openai-codex")
}

fn stored_as(path: &Path, id: &str) -> OAuthCredential {
    match AuthStore::load(path).expect("reload").credential(id) {
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
    refresher_in(path, base, &EnvSnapshot::default())
}

fn refresher_in(path: &Path, base: &str, env: &EnvSnapshot) -> Refresher {
    let endpoints = TokenEndpoints::with_bases(base, base, base, base).expect("loopback base");
    Refresher::new("dalgon/test", path, endpoints, env)
}

/// A refresher whose Factory session check also goes to the loopback `base`.
fn droid_refresher(path: &Path, base: &str) -> Refresher {
    refresher(path, base).with_session_base(base)
}

/// The value of header `name` in a raw request head.
fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim())
}

fn form(body: &str) -> Vec<(String, String)> {
    url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect()
}

fn form_encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn pairs(entries: &[(&str, &str)]) -> Vec<(String, String)> {
    entries
        .iter()
        .map(|&(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
}

fn jwt(payload: &str) -> String {
    format!(
        "{}.{}.not-a-signature",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256"}"#),
        URL_SAFE_NO_PAD.encode(payload)
    )
}

/// A Droid refresh the shutdown cancelled fails like a refresh refused after
/// shutdown: a transport error with fixed text, never a token.
fn assert_cancelled(result: &Result<Credential, ProviderError>) {
    assert!(
        matches!(
            result,
            Err(ProviderError::Transport { family: Family::Droid, reason }) if reason == CANCELLED
        ),
        "{result:?}"
    );
}

fn assert_lock_released(auth: &Path) {
    let probe =
        dal_store::open_private_lock(&lock_path(auth).expect("lock path")).expect("open lock");
    probe.try_lock().expect("the file lock was released");
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
            Reply::Close => {}
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
        refresher.refresh(codex_def(), &held, RefreshReason::Expiring)
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
        codex_def().oauth.map(|oauth| oauth.client_id)
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
    let external = dal_store::open_private_lock(&lock_path(&dir.auth()).expect("lock path"))
        .expect("open lock");
    external.try_lock().expect("external lock");
    let client = async {
        let expiring = refresher.refresh(codex_def(), &held, RefreshReason::Expiring);
        let rejected = refresher.refresh(codex_def(), &held, RefreshReason::Rejected);
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
            .refresh(codex_def(), &held, RefreshReason::Expiring)
            .await;
        let forced = refresher
            .refresh(codex_def(), &held, RefreshReason::Rejected)
            .await;
        // A second 401 on the old token finds the changed token: no request.
        let again = refresher
            .refresh(codex_def(), &held, RefreshReason::Rejected)
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
    let client = refresher.refresh(codex_def(), &held, RefreshReason::Expiring);
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
    let client = refresher.refresh(codex_def(), &held, RefreshReason::Expiring);
    let (result, seen) = with_server(listener, replies, client).await;

    let error = result.expect_err("two 503 fail");
    assert!(
        matches!(
            &error,
            ProviderError::Status { family: Family::Codex, status: 503, message }
                if message == "backend down for <redacted>"
        ),
        "expected Status 503 with redacted message, got: {error:?}"
    );
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
        let client = refresher.refresh(codex_def(), &held, RefreshReason::Rejected);
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
            _ = refresher.refresh(codex_def(), &held, RefreshReason::Expiring) => {
                panic!("the refresh completed before cancellation")
            }
        }
        // The second caller must join while the first request is still waiting.
        let second = refresher.refresh(codex_def(), &held, RefreshReason::Expiring);
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
    let probe = dal_store::open_private_lock(&lock_path(&dir.auth()).expect("lock path"))
        .expect("open lock");
    probe.try_lock().expect("the file lock was released");
}

#[tokio::test]
async fn shutdown_drains_an_abandoned_in_flight_refresh_and_refuses_new_ones() {
    let dir = TestDir::new("shutdown");
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
            _ = refresher.refresh(codex_def(), &held, RefreshReason::Expiring) => {
                panic!("the refresh completed before the request reached the server")
            }
        }
        let mut drain = Box::pin(refresher.shutdown());
        futures::future::poll_fn(|cx| {
            assert!(drain.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), drain)
            .await
            .expect("shutdown drains the abandoned refresh");
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
    let ((), seen) = with_server(listener, replies, client).await;

    assert_eq!(seen.len(), 1);
    let on_disk = stored(&dir.auth());
    assert_eq!(on_disk.access_token.expose(), NEW_ACCESS);
    assert_eq!(on_disk.refresh_token.expose(), NEW_REFRESH);
    let error = refresher
        .refresh(codex_def(), &held, RefreshReason::Rejected)
        .await
        .expect_err("shutdown refuses new refreshes");
    assert!(
        matches!(error, ProviderError::Transport { .. }),
        "{error:?}"
    );
    assert_no_secret(&error, &[NEW_ACCESS, NEW_REFRESH]);
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
            _ = refresher.refresh(codex_def(), &held, RefreshReason::Expiring) => {
                panic!("the refresh completed before cancellation")
            }
        }
        release.notify_one();
        // The failed task holds the auth file lock until its future ends,
        // through the exhausted retry; wait for that before the next caller.
        let probe = dal_store::open_private_lock(&lock_path(&dir.auth()).expect("lock path"))
            .expect("open lock");
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
            .refresh(codex_def(), &held, RefreshReason::Expiring)
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
        endpoints.url(claude_def()).map(url::Url::as_str),
        Some("https://platform.claude.com/v1/oauth/token")
    );
    assert_eq!(
        endpoints.url(codex_def()).map(url::Url::as_str),
        Some("https://auth.openai.com/oauth/token")
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
    let fresh = fresh_credential(claude_def(), &stored, body, 1_000).expect("valid body");
    assert_eq!(fresh.access_token.expose(), "a2");
    assert_eq!(fresh.refresh_token.expose(), OLD_REFRESH);
    assert_eq!(fresh.expires_at, Some(1_060));
    assert_eq!((fresh.id_token, fresh.account_id), (None, None));
    let error = fresh_credential(claude_def(), &stored, br#"{"access_token":"#, 0)
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
            .refresh(codex_def(), &held, RefreshReason::Expiring)
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
        let error =
            fresh_credential(codex_def(), &stored, body, 0).expect_err("not a token response");
        assert!(matches!(error, ProviderError::Transport { .. }), "{body:?}");
        assert!(!error.to_string().contains("leaked-body-secret"), "{error}");
    }
}

#[test]
fn extreme_expires_in_saturates_and_negative_is_already_expired() {
    let stored = codex(OLD_ACCESS, OLD_REFRESH, Some(1));
    let at = |expires_in: i64| {
        let body = format!(r#"{{"access_token":"a","expires_in":{expires_in}}}"#);
        fresh_credential(codex_def(), &stored, body.as_bytes(), 1_000)
            .expect("valid body")
            .expires_at
    };
    assert_eq!(at(i64::MAX), Some(i64::MAX));
    assert_eq!(at(-100), Some(900));
    assert_eq!(at(0), Some(1_000));
}

/// One `GitLab` refresh of a stored `old-*` credential under `env`, answered
/// with `NEW_TOKENS`; returns the form body the loopback server received.
async fn gitlab_refresh_form(env: &EnvSnapshot) -> Vec<(String, String)> {
    let dir = TestDir::new("gitlab-env");
    let held = plain(OLD_ACCESS, OLD_REFRESH, Some(unix_now() + 10));
    seed_as(&dir.auth(), "gitlab-duo", held.clone());
    let (listener, base) = listen().await;
    let refresher = refresher_in(&dir.auth(), &base, env);
    let replies = vec![Reply::Json(200, String::from(NEW_TOKENS))];
    let client = refresher.refresh(gitlab_def(), &held, RefreshReason::Expiring);
    let (result, seen) = with_server(listener, replies, client).await;

    result.expect("the refresh succeeds");
    assert_eq!(seen.len(), 1);
    form(&seen[0].body)
}

#[tokio::test]
async fn gitlab_refresh_posts_a_form_and_counts_expiry_from_created_at() {
    let dir = TestDir::new("gitlab-form");
    let held = plain(OLD_ACCESS, OLD_REFRESH, Some(unix_now() + 10));
    seed_as(&dir.auth(), "gitlab-duo", held.clone());
    let (listener, base) = listen().await;
    let refresher = refresher(&dir.auth(), &base);
    // Issued 50 minutes ago: the lifetime counts from then, not from now.
    let issued = unix_now() - 3_000;
    let answer = format!(
        r#"{{"access_token":"{NEW_ACCESS}","token_type":"Bearer","expires_in":7200,"refresh_token":"{NEW_REFRESH}","scope":"api","created_at":{issued}}}"#
    );
    let client = refresher.refresh(gitlab_def(), &held, RefreshReason::Expiring);
    let (result, seen) = with_server(listener, vec![Reply::Json(200, answer)], client).await;

    assert_eq!(seen.len(), 1, "no session check outside a Droid refresh");
    let request = &seen[0];
    assert!(request.head.starts_with("POST /oauth/token HTTP/1.1"));
    assert_eq!(
        header(&request.head, "content-type"),
        Some("application/x-www-form-urlencoded")
    );
    assert_eq!(header(&request.head, "accept"), Some("application/json"));
    assert_eq!(
        form(&request.body),
        pairs(&[
            ("grant_type", "refresh_token"),
            ("client_id", row_client_id(gitlab_def())),
            ("refresh_token", OLD_REFRESH),
        ])
    );
    let fresh = oauth(result.expect("the form refresh succeeds"));
    assert_eq!(fresh.expires_at, Some(issued + 7_200));
    let on_disk = stored_as(&dir.auth(), "gitlab-duo");
    assert_eq!(on_disk.access_token.expose(), NEW_ACCESS);
    assert_eq!(on_disk.refresh_token.expose(), NEW_REFRESH);
    assert_eq!(on_disk.expires_at, Some(issued + 7_200));
    assert_eq!((on_disk.id_token, on_disk.account_id), (None, None));
    assert_lock_released(&dir.auth());
}

#[tokio::test]
async fn gitlab_client_id_override_is_captured_trimmed_and_blank_means_unset() {
    let row = row_client_id(gitlab_def());
    let cases = [
        (Some("custom-client"), "custom-client"),
        (Some("  padded-client\n"), "padded-client"),
        (Some(""), row),
        (Some("   "), row),
        (None, row),
    ];
    for (configured, expected) in cases {
        let env = configured.map_or_else(EnvSnapshot::default, |value| {
            EnvSnapshot::test(&[("GITLAB_CLIENT_ID", value)])
        });
        assert_eq!(
            gitlab_refresh_form(&env).await,
            pairs(&[
                ("grant_type", "refresh_token"),
                ("client_id", expected),
                ("refresh_token", OLD_REFRESH),
            ]),
            "{configured:?}"
        );
    }
}

#[tokio::test]
async fn json_rows_keep_their_json_body_and_row_client_id_under_any_environment() {
    let env = EnvSnapshot::test(&[("GITLAB_CLIENT_ID", "custom-client")]);
    for (id, def) in [("openai-codex", codex_def()), ("anthropic", claude_def())] {
        let dir = TestDir::new("json-rows");
        let held = if id == "openai-codex" {
            codex(OLD_ACCESS, OLD_REFRESH, Some(unix_now() + 10))
        } else {
            plain(OLD_ACCESS, OLD_REFRESH, Some(unix_now() + 10))
        };
        seed_as(&dir.auth(), id, held.clone());
        let (listener, base) = listen().await;
        let refresher = refresher_in(&dir.auth(), &base, &env);
        let replies = vec![Reply::Json(200, String::from(NEW_TOKENS))];
        let client = refresher.refresh(def, &held, RefreshReason::Expiring);
        let (result, seen) = with_server(listener, replies, client).await;

        result.expect("the refresh succeeds");
        assert_eq!(seen.len(), 1, "{id}: no session check");
        assert_eq!(
            header(&seen[0].head, "content-type"),
            Some("application/json"),
            "{id}"
        );
        let expected = format!(
            r#"{{"grant_type":"refresh_token","client_id":"{}","refresh_token":"{OLD_REFRESH}"}}"#,
            row_client_id(def)
        );
        assert_eq!(seen[0].body, expected, "{id}");
    }
}

#[test]
fn effective_client_ids_follow_the_row_data_and_the_snapshot() {
    let env = EnvSnapshot::test(&[("GITLAB_CLIENT_ID", " mine \n")]);
    for def in PROVIDERS {
        let Some(oauth) = def.oauth else { continue };
        let expected = if oauth.client_id_env == Some("GITLAB_CLIENT_ID") {
            "mine"
        } else {
            oauth.client_id
        };
        assert_eq!(&*effective_client_id(&oauth, &env), expected, "{}", def.id);
        assert_eq!(
            &*effective_client_id(&oauth, &EnvSnapshot::default()),
            oauth.client_id,
            "{}",
            def.id
        );
    }
}

#[test]
fn issuance_time_follows_created_at_only_when_it_is_an_integer() {
    let stored = plain(OLD_ACCESS, OLD_REFRESH, Some(1));
    let at = |access: &str, extra: &str| {
        let body = format!(r#"{{"access_token":"{access}"{extra}}}"#);
        fresh_credential(gitlab_def(), &stored, body.as_bytes(), 1_000)
            .expect("valid body")
            .expires_at
    };
    let lifetime = r#","expires_in":60"#;
    let with = |created_at: &str| format!(r#"{lifetime},"created_at":{created_at}"#);
    assert_eq!(at("a", &with("100")), Some(160));
    assert_eq!(at("a", lifetime), Some(1_060));
    assert_eq!(at("a", &with("null")), Some(1_060));
    assert_eq!(at("a", &with(r#""2025-01-01T00:00:00Z""#)), Some(1_060));
    assert_eq!(at("a", &with("1.5")), Some(1_060));
    assert_eq!(at("a", &with("18446744073709551615")), Some(1_060));
    assert_eq!(at("a", &with("9223372036854775807")), Some(i64::MAX));
    assert_eq!(at("a", &with("-9223372036854775808")), Some(i64::MIN + 60));
    // Without `expires_in` the JWT `exp` claim stands, whatever `created_at` says.
    let token = jwt(r#"{"exp":5000}"#);
    assert_eq!(at(&token, r#","created_at":100"#), Some(5_000));
    assert_eq!(at("opaque", r#","created_at":100"#), None);
}

#[tokio::test]
async fn gitlab_form_echo_is_redacted_in_its_encoded_form() {
    let dir = TestDir::new("gitlab-echo");
    let refresh_token = "old+refresh/secret==";
    let access_token = "old/access+secret";
    let held = plain(access_token, refresh_token, Some(unix_now() + 10));
    seed_as(&dir.auth(), "gitlab-duo", held.clone());
    let before = fs::read(dir.auth()).expect("read seed");
    let (listener, base) = listen().await;
    let refresher = refresher(&dir.auth(), &base);
    let (encoded_refresh, encoded_access) = (form_encode(refresh_token), form_encode(access_token));
    let echoed = format!(
        "bad request: refresh_token={encoded_refresh}&raw={refresh_token}&access={encoded_access}"
    );
    let answer = format!(r#"{{"error":"invalid_request","error_description":"{echoed}"}}"#);
    let client = refresher.refresh(gitlab_def(), &held, RefreshReason::Rejected);
    let (result, seen) = with_server(listener, vec![Reply::Json(400, answer)], client).await;

    assert_eq!(seen.len(), 1, "a bad request is not retried");
    assert!(
        seen[0]
            .body
            .contains(&format!("refresh_token={encoded_refresh}")),
        "the request carried the encoded token"
    );
    let error = result.expect_err("a bad request fails");
    assert!(
        matches!(
            &error,
            ProviderError::Status { family: Family::GitLab, status: 400, message }
                if message == "bad request: refresh_token=<redacted>&raw=<redacted>&access=<redacted>"
        ),
        "{error:?}"
    );
    assert_no_secret(
        &error,
        &[
            refresh_token,
            access_token,
            &encoded_refresh,
            &encoded_access,
        ],
    );
    assert_eq!(fs::read(dir.auth()).expect("read after"), before);
}

#[tokio::test]
async fn droid_refresh_posts_a_form_and_verifies_the_fresh_token_before_it_commits() {
    let dir = TestDir::new("droid-verify");
    let held = plain(OLD_ACCESS, OLD_REFRESH, Some(unix_now() + 10));
    seed_as(&dir.auth(), "factory-droid", held.clone());
    let before = fs::read(dir.auth()).expect("read seed");
    let (listener, base) = listen().await;
    let refresher = droid_refresher(&dir.auth(), &base);
    let started = Rc::new(Notify::new());
    let release = Rc::new(Notify::new());
    let client = async {
        let mid_flight = async {
            started.notified().await;
            assert_eq!(
                fs::read(dir.auth()).expect("read while verifying"),
                before,
                "nothing is committed before the session check answers"
            );
            release.notify_one();
        };
        let refresh = refresher.refresh(droid_def(), &held, RefreshReason::Expiring);
        let (result, ()) = tokio::join!(refresh, mid_flight);
        result
    };
    let replies = vec![
        Reply::Json(200, String::from(WORKOS_TOKENS)),
        Reply::JsonAfterNotify(
            Rc::clone(&started),
            Rc::clone(&release),
            200,
            String::from(ORG_ANSWER),
        ),
    ];
    let (result, seen) = with_server(listener, replies, client).await;

    assert_eq!(seen.len(), 2, "one exchange, one session check");
    let exchange = &seen[0];
    assert!(
        exchange
            .head
            .starts_with("POST /user_management/authenticate HTTP/1.1")
    );
    assert_eq!(
        header(&exchange.head, "content-type"),
        Some("application/x-www-form-urlencoded")
    );
    assert_eq!(
        form(&exchange.body),
        pairs(&[
            ("grant_type", "refresh_token"),
            ("client_id", row_client_id(droid_def())),
            ("refresh_token", OLD_REFRESH),
        ])
    );
    let check = &seen[1];
    assert!(check.head.starts_with("GET /api/cli/whoami HTTP/1.1"));
    assert_eq!(
        header(&check.head, "authorization"),
        Some("Bearer new-access-secret"),
        "the check verifies the fresh token, not the stored one"
    );
    assert_eq!(header(&check.head, "accept"), Some("application/json"));
    let fresh = oauth(result.expect("the refresh succeeds"));
    assert_eq!(fresh.access_token.expose(), NEW_ACCESS);
    let on_disk = stored_as(&dir.auth(), "factory-droid");
    assert_eq!(on_disk.access_token.expose(), NEW_ACCESS);
    assert_eq!(on_disk.refresh_token.expose(), NEW_REFRESH);
    assert_eq!(on_disk.expires_at, None, "the answer carried no lifetime");
    assert_eq!((on_disk.id_token, on_disk.account_id), (None, None));
    assert_lock_released(&dir.auth());
}

#[tokio::test]
async fn droid_session_check_follows_the_endpoint_set_without_an_override() {
    let dir = TestDir::new("droid-seam");
    let held = plain(OLD_ACCESS, OLD_REFRESH, Some(unix_now() + 10));
    seed_as(&dir.auth(), "factory-droid", held.clone());
    let (listener, base) = listen().await;
    let refresher = refresher(&dir.auth(), &base);
    let client = refresher.refresh(droid_def(), &held, RefreshReason::Expiring);
    let replies = vec![
        Reply::Json(200, String::from(WORKOS_TOKENS)),
        Reply::Json(200, String::from(ORG_ANSWER)),
    ];
    let (result, seen) = with_server(listener, replies, client).await;

    assert_eq!(
        seen.len(),
        2,
        "the exchange and the session check both reach the loopback endpoint set"
    );
    assert!(seen[1].head.starts_with("GET /api/cli/whoami HTTP/1.1"));
    let fresh = oauth(result.expect("the refresh succeeds"));
    assert_eq!(fresh.access_token.expose(), NEW_ACCESS);
}

#[tokio::test]
async fn a_rejection_message_is_redacted_then_bounded_to_one_line() {
    let dir = TestDir::new("bounded");
    let held = codex(OLD_ACCESS, OLD_REFRESH, Some(unix_now() + 10));
    seed(&dir.auth(), held.clone());
    let before = fs::read(dir.auth()).expect("read seed");
    let (listener, base) = listen().await;
    let refresher = refresher(&dir.auth(), &base);
    let page = format!("{OLD_REFRESH} {}\nsecond line", "x".repeat(5_000));
    let client = refresher.refresh(codex_def(), &held, RefreshReason::Rejected);
    let (result, seen) = with_server(listener, vec![Reply::Json(400, page)], client).await;

    assert_eq!(seen.len(), 1, "a bad request is not retried");
    let error = result.expect_err("a rejected request fails");
    let ProviderError::Status {
        status: 400,
        message,
        ..
    } = &error
    else {
        panic!("expected a 400 Status, got {error:?}");
    };
    assert!(message.starts_with("<redacted> xxx"), "{message}");
    assert!(message.len() <= 300, "{} bytes", message.len());
    assert!(!message.contains("second line"), "{message}");
    assert_no_secret(&error, &[OLD_REFRESH, OLD_ACCESS]);
    assert_eq!(fs::read(dir.auth()).expect("read after"), before);
}

#[tokio::test]
async fn droid_verification_failure_is_advisory_and_still_commits() {
    let hostile = format!(r#"{{"error":"denied {NEW_ACCESS} {OLD_REFRESH} org_1"}}"#);
    let failures = [
        ("denied", Reply::Json(403, hostile.clone())),
        ("server error", Reply::Json(500, hostile)),
        ("no organization", Reply::Json(200, String::from("{}"))),
        (
            "unreadable body",
            Reply::Json(200, String::from("not json leaked-body-secret")),
        ),
        ("connection dropped", Reply::Close),
    ];
    for (label, failure) in failures {
        let dir = TestDir::new("droid-advisory");
        let held = plain(OLD_ACCESS, OLD_REFRESH, Some(unix_now() + 10));
        seed_as(&dir.auth(), "factory-droid", held.clone());
        let (listener, base) = listen().await;
        let refresher = droid_refresher(&dir.auth(), &base);
        let replies = vec![Reply::Json(200, String::from(WORKOS_TOKENS)), failure];
        let client = refresher.refresh(droid_def(), &held, RefreshReason::Rejected);
        let (result, seen) = with_server(listener, replies, client).await;

        assert_eq!(seen.len(), 2, "{label}: the check is not retried");
        let fresh = oauth(result.unwrap_or_else(|error| panic!("{label}: {error:?}")));
        assert_eq!(fresh.access_token.expose(), NEW_ACCESS, "{label}");
        let on_disk = stored_as(&dir.auth(), "factory-droid");
        assert_eq!(on_disk.access_token.expose(), NEW_ACCESS, "{label}");
        assert_eq!(on_disk.refresh_token.expose(), NEW_REFRESH, "{label}");
        assert_lock_released(&dir.auth());
    }
}

/// The admitted side of the commit gate: a refresh whose commit has run
/// keeps its tokens across a later shutdown, and a refused refresh carries
/// the refusal text, not the cancellation text.
#[tokio::test]
async fn shutdown_after_a_committed_droid_refresh_keeps_the_tokens() {
    let dir = TestDir::new("droid-committed");
    let held = plain(OLD_ACCESS, OLD_REFRESH, Some(unix_now() + 10));
    seed_as(&dir.auth(), "factory-droid", held.clone());
    let (listener, base) = listen().await;
    let refresher = droid_refresher(&dir.auth(), &base);
    let replies = vec![
        Reply::Json(200, String::from(WORKOS_TOKENS)),
        Reply::Json(200, String::from(ORG_ANSWER)),
    ];
    let client = refresher.refresh(droid_def(), &held, RefreshReason::Expiring);
    let (result, seen) = with_server(listener, replies, client).await;

    let fresh = oauth(result.expect("the refresh commits"));
    assert_eq!(fresh.access_token.expose(), NEW_ACCESS);
    assert_eq!(seen.len(), 2, "one exchange, one session check");
    tokio::time::timeout(Duration::from_secs(5), refresher.shutdown())
        .await
        .expect("shutdown after the commit is prompt");
    let on_disk = stored_as(&dir.auth(), "factory-droid");
    assert_eq!(on_disk.access_token.expose(), NEW_ACCESS);
    assert_eq!(on_disk.refresh_token.expose(), NEW_REFRESH);
    let error = refresher
        .refresh(droid_def(), &held, RefreshReason::Rejected)
        .await
        .expect_err("shutdown refuses new refreshes");
    assert!(
        matches!(&error, ProviderError::Transport { reason, .. } if reason.contains("refused")),
        "{error:?}"
    );
    assert_no_secret(&error, &[NEW_ACCESS, NEW_REFRESH]);
}

#[tokio::test]
async fn shutdown_during_the_session_check_cancels_without_committing() {
    // The task is parked in the session check, before its commit is
    // admitted: it holds no commit gate, so the shutdown takes the gate,
    // publishes the cancel at once, and the check aborts.
    let dir = TestDir::new("droid-cancel");
    let held = plain(OLD_ACCESS, OLD_REFRESH, Some(unix_now() + 10));
    seed_as(&dir.auth(), "factory-droid", held.clone());
    let before = fs::read(dir.auth()).expect("read seed");
    let (listener, base) = listen().await;
    let refresher = droid_refresher(&dir.auth(), &base);
    let started = Rc::new(Notify::new());
    let release = Rc::new(Notify::new());
    let client = async {
        let stopper = async {
            started.notified().await;
            // The live caller holds its slot; only the cancel frees it.
            tokio::time::timeout(Duration::from_secs(5), refresher.shutdown())
                .await
                .expect("shutdown cancels the session check and drains");
        };
        let refresh = refresher.refresh(droid_def(), &held, RefreshReason::Expiring);
        let (result, ()) = tokio::join!(refresh, stopper);
        result
    };
    // The check is never answered: only the cancel can end it.
    let replies = vec![
        Reply::Json(200, String::from(WORKOS_TOKENS)),
        Reply::JsonAfterNotify(
            Rc::clone(&started),
            Rc::clone(&release),
            200,
            String::from(ORG_ANSWER),
        ),
    ];
    let (result, seen) = with_server(listener, replies, client).await;

    assert_eq!(seen.len(), 2);
    assert_cancelled(&result);
    assert_eq!(fs::read(dir.auth()).expect("read after"), before);
    assert_lock_released(&dir.auth());
    let error = refresher
        .refresh(droid_def(), &held, RefreshReason::Rejected)
        .await
        .expect_err("shutdown refuses new refreshes");
    assert!(
        matches!(error, ProviderError::Transport { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn shutdown_drains_an_abandoned_refresh_stuck_in_the_session_check_without_committing() {
    let dir = TestDir::new("droid-abandoned");
    let held = plain(OLD_ACCESS, OLD_REFRESH, Some(unix_now() + 10));
    seed_as(&dir.auth(), "factory-droid", held.clone());
    let before = fs::read(dir.auth()).expect("read seed");
    let (listener, base) = listen().await;
    let refresher = droid_refresher(&dir.auth(), &base);
    let started = Rc::new(Notify::new());
    let release = Rc::new(Notify::new());
    let client = async {
        tokio::select! {
            biased;
            () = started.notified() => {}
            _ = refresher.refresh(droid_def(), &held, RefreshReason::Expiring) => {
                panic!("the refresh completed before the session check was on the wire")
            }
        }
        tokio::time::timeout(Duration::from_secs(5), refresher.shutdown())
            .await
            .expect("shutdown cancels the abandoned task and drains");
    };
    let replies = vec![
        Reply::Json(200, String::from(WORKOS_TOKENS)),
        Reply::JsonAfterNotify(
            Rc::clone(&started),
            Rc::clone(&release),
            200,
            String::from(ORG_ANSWER),
        ),
    ];
    let ((), seen) = with_server(listener, replies, client).await;

    assert_eq!(seen.len(), 2);
    assert_eq!(fs::read(dir.auth()).expect("read after"), before);
    assert_lock_released(&dir.auth());
}

#[tokio::test]
async fn shutdown_during_the_exchange_sends_no_session_check_and_never_commits() {
    let dir = TestDir::new("droid-precancel");
    let held = plain(OLD_ACCESS, OLD_REFRESH, Some(unix_now() + 10));
    seed_as(&dir.auth(), "factory-droid", held.clone());
    let before = fs::read(dir.auth()).expect("read seed");
    let (listener, base) = listen().await;
    let refresher = droid_refresher(&dir.auth(), &base);
    let started = Rc::new(Notify::new());
    let release = Rc::new(Notify::new());
    let client = async {
        let stopper = async {
            started.notified().await;
            let mut drain = Box::pin(refresher.shutdown());
            // One poll runs the cancel; the drain then waits on the live caller.
            futures::future::poll_fn(|cx| {
                assert!(drain.as_mut().poll(cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            release.notify_one();
            tokio::time::timeout(Duration::from_secs(5), drain)
                .await
                .expect("shutdown drains the cancelled task");
        };
        let refresh = refresher.refresh(droid_def(), &held, RefreshReason::Expiring);
        let (result, ()) = tokio::join!(refresh, stopper);
        result
    };
    // The check must never be requested; a second answer would wait unused.
    let replies = vec![
        Reply::JsonAfterNotify(
            Rc::clone(&started),
            Rc::clone(&release),
            200,
            String::from(WORKOS_TOKENS),
        ),
        Reply::Json(200, String::from(ORG_ANSWER)),
    ];
    let (result, seen) = with_server(listener, replies, client).await;

    assert_eq!(
        seen.len(),
        1,
        "the cancelled task opened no second connection"
    );
    assert_cancelled(&result);
    assert_eq!(fs::read(dir.auth()).expect("read after"), before);
    assert_lock_released(&dir.auth());
}

#[tokio::test]
async fn shutdown_while_a_droid_refresh_waits_for_the_file_lock_sends_no_request() {
    let dir = TestDir::new("droid-lockwait");
    let held = plain(OLD_ACCESS, OLD_REFRESH, Some(unix_now() + 10));
    seed_as(&dir.auth(), "factory-droid", held.clone());
    let before = fs::read(dir.auth()).expect("read seed");
    let (listener, base) = listen().await;
    let refresher = droid_refresher(&dir.auth(), &base);
    // Another process holds the lock, so the refresh is already past its
    // shutdown check and waits for the file.
    let external = dal_store::open_private_lock(&lock_path(&dir.auth()).expect("lock path"))
        .expect("open lock");
    external.try_lock().expect("external lock");
    let client = async {
        let stopper = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let mut drain = Box::pin(refresher.shutdown());
            futures::future::poll_fn(|cx| {
                assert!(drain.as_mut().poll(cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            drop(external);
            tokio::time::timeout(Duration::from_secs(5), drain)
                .await
                .expect("shutdown drains the cancelled task");
        };
        let refresh = refresher.refresh(droid_def(), &held, RefreshReason::Expiring);
        let (result, ()) = tokio::join!(refresh, stopper);
        result
    };
    let (result, seen) = with_server(listener, Vec::new(), client).await;

    assert!(
        seen.is_empty(),
        "a refresh that would be discarded is not sent"
    );
    assert_cancelled(&result);
    assert_eq!(fs::read(dir.auth()).expect("read after"), before);
    assert_lock_released(&dir.auth());
}

const GITLAB_GRANT_BODY: &str = r#"{"token":"da-grant","headers":{"x-a":"b"}}"#;

/// Serves `direct_access` grants on `listener` for the whole test, counting
/// hits. The base URL stays fixed so every `ensure` shares one cache key.
async fn serve_grants(listener: TcpListener, hits: Arc<AtomicU32>, body: &'static str) {
    loop {
        let (stream, _) = listener.accept().await.expect("accept");
        let _ = read_request(&stream).await;
        hits.fetch_add(1, Ordering::SeqCst);
        write_response(&stream, 200, body).await;
    }
}

#[tokio::test]
async fn gitlab_refresh_success_invalidates_old_and_new_grants() {
    use crate::family::gitlab::DirectAccessCache;

    let dir = TestDir::new("gitlab-grant-invalidate");
    let held = plain(OLD_ACCESS, OLD_REFRESH, Some(unix_now() + 10));
    seed_as(&dir.auth(), "gitlab-duo", held.clone());

    // One fixed direct_access base for the whole test: a single cache key
    // per token. Grants for both the outgoing and the incoming token are
    // cached up front: the incoming one stands in for a grant another
    // sign-in cached under the token the refresh is about to rotate to.
    let (da_listener, da_base) = listen().await;
    let hits = Arc::new(AtomicU32::new(0));
    let grants = {
        let hits = Arc::clone(&hits);
        #[expect(
            clippy::disallowed_methods,
            reason = "test edge: serve_grants runs concurrently with test"
        )]
        let task =
            tokio::spawn(async move { serve_grants(da_listener, hits, GITLAB_GRANT_BODY).await });
        task
    };
    let http = crate::http::build_client();
    let cache = Arc::new(DirectAccessCache::default());
    for token in [OLD_ACCESS, NEW_ACCESS] {
        cache
            .ensure(&http, &da_base, token, "dalgon/test")
            .await
            .expect("seed a grant");
        cache
            .ensure(&http, &da_base, token, "dalgon/test")
            .await
            .expect("the grant is cached");
    }
    assert_eq!(hits.load(Ordering::SeqCst), 2);

    // The token endpoint answers with rotated tokens; the refresh commits.
    // GitLab sends no session check: exactly one exchange request.
    let (token_listener, token_base) = listen().await;
    let refresher = refresher(&dir.auth(), &token_base).with_gitlab_grants(Arc::clone(&cache));
    let client = refresher.refresh(gitlab_def(), &held, RefreshReason::Rejected);
    let (result, seen) = with_server(
        token_listener,
        vec![Reply::Json(200, String::from(NEW_TOKENS))],
        client,
    )
    .await;
    assert_eq!(
        seen.len(),
        1,
        "a GitLab refresh sends one exchange, no check"
    );
    let fresh = oauth(result.expect("the refresh succeeds"));
    assert_eq!(fresh.access_token.expose(), NEW_ACCESS);
    assert_eq!(
        stored_as(&dir.auth(), "gitlab-duo").access_token.expose(),
        NEW_ACCESS
    );

    // Both tokens' grants are gone: each refetches exactly once, then caches.
    cache
        .ensure(&http, &da_base, OLD_ACCESS, "dalgon/test")
        .await
        .expect("the old grant refetches");
    assert_eq!(hits.load(Ordering::SeqCst), 3);
    cache
        .ensure(&http, &da_base, NEW_ACCESS, "dalgon/test")
        .await
        .expect("the new grant refetches");
    assert_eq!(hits.load(Ordering::SeqCst), 4);
    cache
        .ensure(&http, &da_base, NEW_ACCESS, "dalgon/test")
        .await
        .expect("the new grant caches");
    assert_eq!(hits.load(Ordering::SeqCst), 4);
    grants.abort();
}

#[tokio::test]
async fn gitlab_refresh_failure_preserves_the_working_grant() {
    use crate::family::gitlab::DirectAccessCache;

    let dir = TestDir::new("gitlab-grant-preserve");
    let held = plain(OLD_ACCESS, OLD_REFRESH, Some(unix_now() + 10));
    seed_as(&dir.auth(), "gitlab-duo", held.clone());
    let before = fs::read(dir.auth()).expect("read seed");

    let (da_listener, da_base) = listen().await;
    let hits = Arc::new(AtomicU32::new(0));
    let grants = {
        let hits = Arc::clone(&hits);
        #[expect(
            clippy::disallowed_methods,
            reason = "test edge: serve_grants runs concurrently with test"
        )]
        let task =
            tokio::spawn(async move { serve_grants(da_listener, hits, GITLAB_GRANT_BODY).await });
        task
    };
    let http = crate::http::build_client();
    let cache = Arc::new(DirectAccessCache::default());
    cache
        .ensure(&http, &da_base, OLD_ACCESS, "dalgon/test")
        .await
        .expect("seed the working grant");
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    // The refresh token is dead: the refresh fails and commits nothing.
    let (token_listener, token_base) = listen().await;
    let refresher = refresher(&dir.auth(), &token_base).with_gitlab_grants(Arc::clone(&cache));
    let answer = String::from(
        r#"{"error":"invalid_grant","error_description":"the refresh token is dead"}"#,
    );
    let client = refresher.refresh(gitlab_def(), &held, RefreshReason::Rejected);
    let (result, seen) = with_server(token_listener, vec![Reply::Json(400, answer)], client).await;
    let error = result.expect_err("a dead refresh token fails");
    assert!(
        matches!(&error, ProviderError::SignInExpired { .. }),
        "{error:?}"
    );
    assert_eq!(seen.len(), 1, "a rejected refresh token is not retried");
    assert_eq!(fs::read(dir.auth()).expect("read after"), before);
    assert_no_secret(&error, &[OLD_ACCESS, OLD_REFRESH]);

    // The working grant survived: no refetch.
    cache
        .ensure(&http, &da_base, OLD_ACCESS, "dalgon/test")
        .await
        .expect("the working grant");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "a failed refresh invalidates nothing"
    );
    grants.abort();
}
