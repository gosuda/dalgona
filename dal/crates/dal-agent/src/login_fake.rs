//! A loopback OAuth server for tests that run real sign-in flows.
//!
//! One [`FakeOAuth`] answers the token, device-code, revoke, and model
//! endpoints of [`LoginEndpoints::loopback`] over plain HTTP on `127.0.0.1`,
//! records every request, and can play the browser by following an
//! authorization URL to its loopback redirect. It is test support only: it
//! serves invented tokens and never leaves the machine.

use std::io;
use std::sync::{Arc, Mutex};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use dal_provider::{LoginEndpoints, ProviderError};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;

/// The account id the fake Codex ID token names.
pub const ACCOUNT_ID: &str = "acct-fake";
/// The access token the fake token endpoints issue.
pub const ACCESS_TOKEN: &str = "fake-access-token";
/// The refresh token the fake token endpoints issue.
pub const REFRESH_TOKEN: &str = "fake-refresh-token";
/// The device code the fake device endpoint shows.
pub const USER_CODE: &str = "ABCD-EFGH";

/// One request the server received.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Recorded {
    /// The HTTP method.
    pub method: String,
    /// The request path.
    pub path: String,
    /// The request body as text.
    pub body: String,
}

/// How the fake token endpoints answer.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TokenReply {
    /// Issue the fake token pair.
    #[default]
    Issue,
    /// Answer `400` with an `invalid_grant` body.
    Reject,
}

/// A running fake OAuth server; dropping it stops it.
pub struct FakeOAuth {
    base: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
    _tasks: JoinSet<()>,
}

impl FakeOAuth {
    /// Starts a server that answers token requests as `reply` says.
    ///
    /// # Errors
    /// Returns the bind error when no loopback port is available.
    pub async fn start(reply: TokenReply) -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let mut tasks = JoinSet::new();
        tasks.spawn(serve(listener, Arc::clone(&requests), reply));
        Ok(Self {
            base: format!("http://127.0.0.1:{port}"),
            requests,
            _tasks: tasks,
        })
    }

    /// The server's origin, without a trailing slash.
    #[must_use]
    pub fn base(&self) -> &str {
        &self.base
    }

    /// Endpoints that send every request to this server. Port `0` asks the OS
    /// for a free callback port.
    ///
    /// # Errors
    /// Returns the endpoint error when the base URL is not a loopback origin.
    pub fn endpoints(
        &self,
        codex_callback_port: u16,
        claude_callback_port: u16,
    ) -> Result<LoginEndpoints, ProviderError> {
        Ok(LoginEndpoints::loopback(&self.base)?
            .with_callback_ports(codex_callback_port, claude_callback_port))
    }

    /// Every request received so far, in arrival order.
    #[must_use]
    pub fn requests(&self) -> Vec<Recorded> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The requests whose path is `path`.
    #[must_use]
    pub fn requests_to(&self, path: &str) -> Vec<Recorded> {
        self.requests()
            .into_iter()
            .filter(|request| request.path == path)
            .collect()
    }
}

/// Plays the browser: follows the `redirect_uri` of an authorization URL with
/// `code` and the URL's own `state`, and returns the callback page text.
///
/// # Errors
/// Returns an error when the URL carries no loopback redirect, or the
/// callback listener cannot be reached or read.
pub async fn follow_authorize_url(authorize_url: &str, code: &str) -> io::Result<String> {
    let invalid = |what: &str| io::Error::other(format!("authorize url has no {what}"));
    let url = reqwest::Url::parse(authorize_url).map_err(io::Error::other)?;
    let query = |name: &str| {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
            .ok_or_else(|| invalid(name))
    };
    let mut redirect = reqwest::Url::parse(&query("redirect_uri")?).map_err(io::Error::other)?;
    let port = redirect.port().ok_or_else(|| invalid("redirect port"))?;
    redirect.set_query(None);
    redirect
        .query_pairs_mut()
        .append_pair("code", code)
        .append_pair("state", &query("state")?);
    let target = format!(
        "{}?{}",
        redirect.path(),
        redirect.query().unwrap_or_default()
    );
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
    let request = format!("GET {target} HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    Ok(String::from_utf8_lossy(&response).into_owned())
}

async fn serve(listener: TcpListener, requests: Arc<Mutex<Vec<Recorded>>>, reply: TokenReply) {
    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        let Some(request) = read_request(&mut stream).await else {
            continue;
        };
        let (status, body) = answer(&request, reply);
        requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(request);
        let response = format!(
            "HTTP/1.1 {status} Fake\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;
    }
}

async fn read_request(stream: &mut TcpStream) -> Option<Recorded> {
    let mut data = Vec::new();
    let mut chunk = [0_u8; 2048];
    let header_end = loop {
        let count = stream.read(&mut chunk).await.ok()?;
        if count == 0 {
            return None;
        }
        data.extend_from_slice(&chunk[..count]);
        if let Some(position) = data.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let head = String::from_utf8_lossy(&data[..header_end]).into_owned();
    let length = head
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while data.len() < header_end + length {
        let count = stream.read(&mut chunk).await.ok()?;
        if count == 0 {
            break;
        }
        data.extend_from_slice(&chunk[..count]);
    }
    let mut first = head.lines().next()?.split_whitespace();
    Some(Recorded {
        method: first.next()?.to_owned(),
        path: first.next()?.to_owned(),
        body: String::from_utf8_lossy(&data[header_end..]).into_owned(),
    })
}

fn answer(request: &Recorded, reply: TokenReply) -> (u16, String) {
    let token_path = matches!(
        request.path.as_str(),
        "/oauth/token" | "/claude/v1/oauth/token"
    );
    if token_path && reply == TokenReply::Reject {
        return (
            400,
            String::from(r#"{"error":"invalid_grant","error_description":"code was rejected"}"#),
        );
    }
    match request.path.as_str() {
        "/oauth/token" => (200, codex_tokens()),
        "/claude/v1/oauth/token" => (
            200,
            format!(
                r#"{{"access_token":"{ACCESS_TOKEN}","refresh_token":"{REFRESH_TOKEN}","expires_in":3600}}"#
            ),
        ),
        "/api/accounts/deviceauth/usercode" => (
            200,
            format!(r#"{{"device_auth_id":"dev-1","usercode":"{USER_CODE}","interval":"1"}}"#),
        ),
        "/api/accounts/deviceauth/token" => (
            200,
            String::from(
                r#"{"authorization_code":"device-code","code_challenge":"unused","code_verifier":"device-verifier"}"#,
            ),
        ),
        "/oauth/revoke" => (200, String::from("{}")),
        _ => (404, String::from(r#"{"error":"not found"}"#)),
    }
}

fn codex_tokens() -> String {
    let encode = |text: &str| URL_SAFE_NO_PAD.encode(text);
    let id_token = format!(
        "{}.{}.{}",
        encode(r#"{"alg":"none"}"#),
        encode(&format!(
            r#"{{"https://api.openai.com/auth":{{"chatgpt_account_id":"{ACCOUNT_ID}"}}}}"#
        )),
        encode("signature"),
    );
    format!(
        r#"{{"id_token":"{id_token}","access_token":"{ACCESS_TOKEN}","refresh_token":"{REFRESH_TOKEN}","expires_in":3600}}"#
    )
}
