//! Loopback proofs of the Cursor sign-in steps.
//!
//! Every test points the sign-in steps at a local HTTP server that
//! scripts the vendor's answers: pending polls, approvals, refresh grants,
//! and ended sessions. No network beyond the loopback and no live
//! credentials.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::Digest;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

use url::Url;

use super::{
    DEFAULT_LIFETIME_SECS, EXPIRY_SKEW_SECS, PollPacing, Prompt, credential, poll_tokens,
    refreshed, token_expires_at,
};
use crate::{
    OAuthCredential, ProviderError, SecretString, auth::oauth::OAuthHttp,
    http::build_client_no_redirect,
};

/// One scripted answer: the status and the raw JSON body.
fn reply(status: u16, body: &str) -> (u16, String) {
    (status, String::from(body))
}

/// A captured request.
#[derive(Clone, Debug)]
struct Captured {
    method: String,
    target: String,
    headers: String,
}

impl Captured {
    fn header(&self, name: &str) -> Option<String> {
        for line in self.headers.lines() {
            if let Some((key, value)) = line.split_once(':')
                && key.trim().eq_ignore_ascii_case(name)
            {
                return Some(value.trim().to_owned());
            }
        }
        None
    }

    fn query_pairs(&self) -> Vec<(String, String)> {
        let query = self.target.split_once('?').map_or("", |(_, query)| query);
        url::form_urlencoded::parse(query.as_bytes())
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect()
    }

    fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or_default()
    }
}

/// A loopback server that answers each connection with the next scripted
/// reply and records every request.
struct Scripted {
    base: String,
    replies: Arc<Mutex<VecDeque<(u16, String)>>>,
    captured: Arc<Mutex<Vec<Captured>>>,
    tasks: JoinSet<()>,
}

impl Drop for Scripted {
    fn drop(&mut self) {
        self.tasks.abort_all();
    }
}

impl Scripted {
    /// Binds a loopback port and queues `replies` in answer order.
    fn start(replies: Vec<(u16, String)>) -> Self {
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("loopback bind");
        std_listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let listener = TcpListener::from_std(std_listener).expect("async listener");
        let port = listener.local_addr().expect("local addr").port();
        let mut server = Self {
            base: format!("http://127.0.0.1:{port}"),
            replies: Arc::new(Mutex::new(replies.into())),
            captured: Arc::new(Mutex::new(Vec::new())),
            tasks: JoinSet::new(),
        };
        server.spawn_accept(listener);
        server
    }

    /// Keeps one accept in flight at a time; each answer closes the
    /// connection, so every poll attempt arrives as a fresh one.
    fn spawn_accept(&mut self, listener: TcpListener) {
        let replies = Arc::clone(&self.replies);
        let captured = Arc::clone(&self.captured);
        self.tasks.spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let next = {
                    let guard = replies.lock().expect("reply queue locked");
                    guard.front().cloned()
                };
                let Some(next) = next else { break };
                {
                    let mut guard = replies.lock().expect("reply queue locked");
                    guard.pop_front();
                }
                if read_and_answer(&mut stream, next, &captured).await.is_err() {
                    break;
                }
            }
        });
    }

    /// Every request served so far, in arrival order.
    fn requests(&self) -> Vec<Captured> {
        self.captured.lock().expect("captures locked").clone()
    }

    /// The poll endpoint on this server.
    fn poll_url(&self) -> Url {
        Url::parse(&format!("{}/auth/poll", self.base)).expect("poll url")
    }
}

/// Polls `server` through the shared no-redirect OAuth client.
async fn poll(
    server: &Scripted,
    prompt: &Prompt,
    pacing: PollPacing,
    cancel: &CancellationToken,
) -> Result<OAuthCredential, ProviderError> {
    let client = build_client_no_redirect();
    let http = OAuthHttp {
        client: &client,
        user_agent: "test-agent",
    };
    poll_tokens(&http, &server.poll_url(), prompt, pacing, cancel).await
}

/// A stored session to refresh.
fn stored(refresh_token: &str) -> OAuthCredential {
    credential(
        SecretString::from("old-access"),
        SecretString::from(refresh_token),
    )
}

/// Reads one HTTP/1.1 request, records it, and answers with `next`, closing
/// the connection afterwards. Lock guards are dropped before every await.
async fn read_and_answer(
    stream: &mut TcpStream,
    next: (u16, String),
    captured: &Arc<Mutex<Vec<Captured>>>,
) -> std::io::Result<()> {
    let mut raw = Vec::new();
    let mut chunk = [0_u8; 4096];
    while !raw.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..read]);
    }
    let text = String::from_utf8_lossy(&raw).into_owned();
    let mut parts = text.split("\r\n\r\n");
    let head = parts.next().unwrap_or_default();
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default().to_owned();
    let headers: String = lines.collect::<Vec<_>>().join("\r\n");
    let declared = head
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);
    let mut body = parts.next().unwrap_or_default().as_bytes().to_vec();
    while body.len() < declared {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    let mut fields = request_line.split(' ');
    let method = fields.next().unwrap_or_default().to_owned();
    let target = fields.next().unwrap_or_default().to_owned();
    {
        let mut guard = captured.lock().expect("captures locked");
        guard.push(Captured {
            method,
            target,
            headers,
        });
    }
    let (status, answer) = next;
    let phrase = if status == 200 { "OK" } else { "Status" };
    let response = format!(
        "HTTP/1.1 {status} {phrase}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{answer}",
        answer.len(),
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

/// Millisecond pacing so a scripted poll finishes instantly.
fn fast_pacing(attempts: u32) -> PollPacing {
    PollPacing {
        base: Duration::from_millis(1),
        max: Duration::from_millis(2),
        multiplier: 1.2,
        max_attempts: attempts,
        max_consecutive_errors: 3,
    }
}

/// A JWT whose payload claims `exp`.
fn jwt_with_exp(exp: i64) -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
    let payload = URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{exp}}}"#).as_bytes());
    format!("{header}.{payload}.sig")
}

#[tokio::test]
async fn login_url_carries_challenge_uuid_mode_and_target_in_order() {
    let prompt = Prompt::new();
    let page = Url::parse("https://cursor.com/loginDeepControl").expect("page");
    let url = prompt.login_url(&page);
    assert_eq!(
        url.as_str().split_once('?').map(|(base, _)| base),
        Some("https://cursor.com/loginDeepControl")
    );
    let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    let names: Vec<&str> = pairs.iter().map(|(key, _)| key.as_str()).collect();
    assert_eq!(names, ["challenge", "uuid", "mode", "redirectTarget"]);
    let by_name = |name: &str| {
        pairs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };
    assert_eq!(by_name("mode").as_deref(), Some("login"));
    assert_eq!(by_name("redirectTarget").as_deref(), Some("cli"));
    assert_eq!(
        by_name("challenge").as_deref(),
        Some(prompt.challenge.as_str())
    );
    assert_eq!(by_name("uuid").as_deref(), Some(prompt.uuid.as_str()));
}

#[tokio::test]
async fn prompt_mints_decodable_pkce_pair_and_uuid() {
    let prompt = Prompt::new();
    assert_eq!(prompt.verifier.len(), 43);
    assert_ne!(prompt.verifier, Prompt::new().verifier);
    uuid::Uuid::parse_str(&prompt.uuid).expect("uuid v4");
    let expected = URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(prompt.verifier.as_bytes()));
    assert_eq!(prompt.challenge, expected);
}

#[tokio::test]
async fn poll_returns_tokens_after_pending_answers() {
    let server = Scripted::start(vec![
        reply(404, "{}"),
        reply(404, "{}"),
        reply(200, r#"{"accessToken":"acc","refreshToken":"ref"}"#),
    ]);
    let prompt = Prompt::new();
    let tokens = poll(&server, &prompt, fast_pacing(10), &CancellationToken::new())
        .await
        .expect("poll succeeds");
    assert_eq!(tokens.access_token.expose(), "acc");
    assert_eq!(tokens.refresh_token.expose(), "ref");
    assert_eq!((tokens.id_token, tokens.account_id), (None, None));
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    let first = &requests[0];
    assert_eq!(first.method, "GET");
    assert_eq!(first.path(), "/auth/poll");
    let pairs = first.query_pairs();
    assert_eq!(
        pairs,
        vec![
            (String::from("uuid"), prompt.uuid.clone()),
            (String::from("verifier"), prompt.verifier.clone()),
        ]
    );
    assert_eq!(first.header("accept").as_deref(), Some("application/json"));
}

#[tokio::test]
async fn poll_fails_after_consecutive_server_errors() {
    let server = Scripted::start(vec![reply(500, "{}"), reply(500, "{}"), reply(500, "{}")]);
    let prompt = Prompt::new();
    let error = poll(&server, &prompt, fast_pacing(10), &CancellationToken::new())
        .await
        .expect_err("three errors fail the poll");
    match error {
        ProviderError::TokenExchange { status, .. } => assert_eq!(status, 500),
        other => panic!("expected TokenExchange, got {other:?}"),
    }
    // One attempt per scripted answer: the third error is the last.
    assert_eq!(server.requests().len(), 3);
}

#[tokio::test]
async fn poll_times_out_when_every_attempt_is_pending() {
    let server = Scripted::start(vec![reply(404, "{}"); 6]);
    let prompt = Prompt::new();
    let error = poll(&server, &prompt, fast_pacing(4), &CancellationToken::new())
        .await
        .expect_err("exhausted attempts time out");
    assert!(matches!(error, ProviderError::LoginTimeout));
    assert_eq!(server.requests().len(), 4);
}

#[tokio::test]
async fn cancelled_poll_reports_login_cancelled() {
    let server = Scripted::start(vec![reply(404, "{}")]);
    let prompt = Prompt::new();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let error = poll(&server, &prompt, fast_pacing(5), &cancel)
        .await
        .expect_err("a cancelled poll fails");
    assert!(matches!(error, ProviderError::LoginCancelled));
    assert!(
        server.requests().is_empty(),
        "a cancelled poll sends nothing"
    );
}

#[tokio::test]
async fn poll_errors_never_echo_the_verifier() {
    let prompt = Prompt::new();
    let echo = format!(r#"{{"error":"bad verifier {}"}}"#, prompt.verifier);
    let server = Scripted::start(vec![
        reply(400, &echo),
        reply(400, &echo),
        reply(400, &echo),
    ]);
    let error = poll(&server, &prompt, fast_pacing(10), &CancellationToken::new())
        .await
        .expect_err("rejected polls fail");
    let text = format!("{error} {error:?}");
    assert!(!text.contains(&prompt.verifier), "{text}");
    assert!(text.contains("<redacted>"), "{text}");
}

#[tokio::test]
async fn an_approval_without_tokens_burns_an_attempt() {
    let server = Scripted::start(vec![
        reply(200, r#"{"accessToken":""}"#),
        reply(200, r#"{"accessToken":"acc","refreshToken":"ref"}"#),
    ]);
    let prompt = Prompt::new();
    let tokens = poll(&server, &prompt, fast_pacing(10), &CancellationToken::new())
        .await
        .expect("the second answer approves");
    assert_eq!(tokens.access_token.expose(), "acc");
    assert_eq!(server.requests().len(), 2);
}

#[test]
fn refresh_keeps_the_stored_refresh_token_without_rotation() {
    let access = jwt_with_exp(1_000_000);
    let answer = format!(r#"{{"access_token":"{access}"}}"#);
    let fresh = refreshed("cursor", &stored("stored-refresh"), answer.as_bytes(), 0)
        .expect("refresh succeeds");
    assert_eq!(fresh.access_token.expose(), access);
    assert_eq!(fresh.refresh_token.expose(), "stored-refresh");
    assert_eq!(fresh.expires_at, Some(1_000_000 - EXPIRY_SKEW_SECS));
    assert_eq!((fresh.id_token, fresh.account_id), (None, None));
}

#[test]
fn refresh_stores_a_rotated_refresh_token() {
    let access = jwt_with_exp(2_000_000);
    let answer = format!(r#"{{"access_token":"{access}","refresh_token":"rotated"}}"#);
    let fresh = refreshed("cursor", &stored("stored-refresh"), answer.as_bytes(), 0)
        .expect("refresh succeeds");
    assert_eq!(fresh.refresh_token.expose(), "rotated");
}

#[test]
fn refresh_reports_a_session_cursor_ended() {
    let error = refreshed(
        "cursor",
        &stored("stored-refresh"),
        br#"{"access_token":"","shouldLogout":true}"#,
        0,
    )
    .expect_err("an ended session fails the refresh");
    match error {
        ProviderError::SignInExpired { provider } => assert_eq!(provider, "cursor"),
        other => panic!("expected SignInExpired, got {other:?}"),
    }
}

#[test]
fn refresh_refuses_an_answer_without_an_access_token() {
    for body in [&br#"{"access_token":""}"#[..], br"{}", br"not json"] {
        let error = refreshed("cursor", &stored("stored-refresh"), body, 0)
            .expect_err("no access token fails the refresh");
        assert!(
            matches!(error, ProviderError::Transport { .. }),
            "{error:?}"
        );
        assert!(!format!("{error:?}").contains("stored-refresh"));
    }
}

#[tokio::test]
async fn token_expiry_reads_the_jwt_claim_with_a_fallback() {
    assert_eq!(token_expires_at(&jwt_with_exp(1_000_000), 0), 999_700);
    let now = 50_000_i64;
    assert_eq!(
        token_expires_at("not-a-jwt", now),
        now + DEFAULT_LIFETIME_SECS
    );
}

#[tokio::test]
async fn credential_carries_the_token_pair_and_expiry() {
    let access = jwt_with_exp(3_000_000);
    let oauth = credential(
        SecretString::from(access.clone()),
        SecretString::from("ref"),
    );
    assert_eq!(oauth.access_token.expose(), access);
    assert_eq!(oauth.refresh_token.expose(), "ref");
    assert_eq!(oauth.expires_at, Some(3_000_000 - EXPIRY_SKEW_SECS));
    assert_eq!(oauth.id_token, None);
    assert_eq!(oauth.account_id, None);
}
