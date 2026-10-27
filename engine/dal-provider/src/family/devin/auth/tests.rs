//! Loopback proofs of the Devin CLI sign-in steps.
//!
//! Every exchange test points the sign-in step at a local HTTP server that
//! scripts the vendor's answers. No network beyond the loopback and no live
//! credentials.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use url::Url;

use super::{
    EXPIRY_SKEW_SECS, FALLBACK_LIFETIME_SECS, credential, exchange_code, token_expires_at,
};
use crate::{
    OAuthCredential, ProviderError, SecretString, auth::oauth::OAuthHttp,
    http::build_client_no_redirect,
};

/// The token path under the loopback base, as the row's token URL is moved.
const TOKEN_PATH: &str = "/devin/auth/cli/token";

/// A loopback server that answers each connection with the next scripted
/// reply and records every request.
struct Loopback {
    listener: tokio::net::TcpListener,
    replies: std::sync::Arc<std::sync::Mutex<Vec<(u16, String)>>>,
    captured: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl Loopback {
    /// Binds a loopback port; `replies` are served in order.
    fn start(replies: Vec<(u16, String)>) -> Self {
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("loopback bind");
        std_listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let listener = TcpListener::from_std(std_listener).expect("async listener");
        Self {
            listener,
            replies: std::sync::Arc::new(std::sync::Mutex::new(replies)),
            captured: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    /// The base URL of the server.
    fn base(&self) -> String {
        format!(
            "http://127.0.0.1:{}",
            self.listener.local_addr().expect("addr").port()
        )
    }

    /// Serves every scripted reply, one connection per reply, then returns.
    /// Runs beside the request future in `tokio::join!`; no task outlives the
    /// test.
    async fn serve_all(&self) {
        loop {
            let next = {
                let mut guard = self.replies.lock().expect("replies locked");
                if guard.is_empty() {
                    return;
                }
                guard.remove(0)
            };
            let Ok((mut stream, _)) = self.listener.accept().await else {
                return;
            };
            if read_and_answer(&mut stream, next, &self.captured)
                .await
                .is_err()
            {
                return;
            }
        }
    }

    /// Exchanges `code` and `verifier` at this server's token endpoint
    /// through the shared no-redirect OAuth client, serving the replies.
    async fn exchange(&self, code: &str, verifier: &str) -> Result<OAuthCredential, ProviderError> {
        let client = build_client_no_redirect();
        let http = OAuthHttp {
            client: &client,
            user_agent: "test-agent",
        };
        let url = Url::parse(&format!("{}{TOKEN_PATH}", self.base())).expect("token url");
        let secrets = [code, verifier];
        let (answer, ()) = tokio::join!(
            exchange_code(&http, &url, code, verifier, &secrets),
            self.serve_all(),
        );
        answer
    }

    /// Every request served so far.
    fn requests(&self) -> Vec<String> {
        self.captured.lock().expect("captures locked").clone()
    }
}

/// Reads one HTTP/1.1 request, records its raw text, and answers with
/// `next`, closing the connection. Lock guards drop before every await.
async fn read_and_answer(
    stream: &mut tokio::net::TcpStream,
    next: (u16, String),
    captured: &std::sync::Arc<std::sync::Mutex<Vec<String>>>,
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
    let declared = text
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);
    let head_end = text.find("\r\n\r\n").map_or(text.len(), |at| at + 4);
    let mut body = text.as_bytes()[head_end..].to_vec();
    while body.len() < declared {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    let mut whole = text.into_bytes();
    whole.truncate(head_end);
    whole.extend_from_slice(&body);
    {
        let mut guard = captured.lock().expect("captures locked");
        guard.push(String::from_utf8_lossy(&whole).into_owned());
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

/// A JWT whose payload claims `exp`.
fn jwt_with_exp(exp: i64) -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256"}"#);
    let payload = URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{exp}}}"#).as_bytes());
    format!("{header}.{payload}.sig")
}

#[tokio::test]
async fn exchange_posts_code_and_verifier_as_bare_json() {
    let token = jwt_with_exp(1_000_000);
    let server = Loopback::start(vec![(200, format!(r#"{{"token":"{token}"}}"#))]);
    let answer = server.exchange("the-code", "the-verifier").await;
    let OAuthCredential {
        access_token,
        refresh_token,
        expires_at,
        id_token,
        account_id,
        project_id,
    } = answer.expect("exchange succeeds");
    assert_eq!(access_token.expose(), token);
    assert_eq!(refresh_token.expose(), token);
    assert_eq!(expires_at, Some(1_000_000 - EXPIRY_SKEW_SECS));
    assert_eq!(id_token, None);
    assert_eq!(account_id, None);
    assert_eq!(project_id, None);
    let raw = &server.requests()[0];
    assert!(
        raw.starts_with(&format!("POST {TOKEN_PATH} ")),
        "request line: {raw}"
    );
    assert!(raw.contains("accept: application/json"));
    assert!(raw.contains("content-type: application/json"));
    assert_eq!(
        raw.split("\r\n\r\n").nth(1),
        Some(r#"{"code":"the-code","code_verifier":"the-verifier"}"#)
    );
}

#[tokio::test]
async fn exchange_reports_a_rejected_code() {
    let server = Loopback::start(vec![(400, r#"{"error":"bad_verify_code"}"#.to_owned())]);
    let answer = server.exchange("the-code", "the-verifier").await;
    match answer.expect_err("a rejected exchange fails") {
        ProviderError::TokenExchange { status, message } => {
            assert_eq!(status, 400);
            assert!(message.contains("bad_verify_code"));
        }
        other => panic!("expected TokenExchange, got {other:?}"),
    }
}

#[tokio::test]
async fn exchange_refuses_an_empty_token() {
    let server = Loopback::start(vec![(200, r#"{"token":""}"#.to_owned())]);
    let answer = server.exchange("the-code", "the-verifier").await;
    let error = answer.expect_err("an empty token fails");
    assert!(matches!(
        error,
        ProviderError::TokenExchange { status: 200, .. }
    ));
}

#[tokio::test]
async fn exchange_errors_never_echo_the_code_or_verifier() {
    let server = Loopback::start(vec![(
        400,
        r#"{"error_description":"code the-code with the-verifier is spent"}"#.to_owned(),
    )]);
    let error = server
        .exchange("the-code", "the-verifier")
        .await
        .expect_err("a rejected exchange fails");
    let text = format!("{error} {error:?}");
    assert!(
        !text.contains("the-code") && !text.contains("the-verifier"),
        "{text}"
    );
    assert!(text.contains("<redacted>"), "{text}");
}

#[test]
fn token_expiry_reads_the_claim_and_falls_back_to_one_year() {
    use base64::engine::general_purpose::URL_SAFE;
    assert_eq!(token_expires_at(&jwt_with_exp(2_000_000), 0), 1_999_700);
    let now = 70_000_i64;
    assert_eq!(
        token_expires_at("garbage", now),
        now + FALLBACK_LIFETIME_SECS
    );
    // A padded JWT decodes too.
    let payload = URL_SAFE.encode(format!(r#"{{"exp":{}}}"#, 3_000_000));
    let padded = format!("aGVhZA.{payload}.c2ln");
    assert_eq!(token_expires_at(&padded, 0), 3_000_000 - EXPIRY_SKEW_SECS);
}

#[test]
fn credential_stores_one_token_as_both_halves() {
    let token = jwt_with_exp(4_000_000);
    let stored = credential(SecretString::from(token.clone()));
    assert_eq!(stored.access_token.expose(), token);
    assert_eq!(stored.refresh_token.expose(), token);
    assert_eq!(stored.expires_at, Some(4_000_000 - EXPIRY_SKEW_SECS));
}
