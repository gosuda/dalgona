//! Public-surface regressions of the built-in Cursor and Devin sign-ins.
//!
//! Every test drives [`login`] against one loopback server through
//! [`LoginEndpoints::loopback`], then reloads `auth.json` from disk, and the
//! refresh tests hand the reloaded credential to a [`Refresher`] whose token
//! endpoints are moved to the same server. No network beyond the loopback
//! and no live credentials.

use std::{
    collections::HashMap,
    fs,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::{mpsc, oneshot},
    time::timeout,
};
use tokio_util::sync::CancellationToken;
use url::Url;

use super::{
    TestDir,
    server::{Reply, Request, Server},
};
use crate::{
    AuthStore, Credential, EnvSnapshot, LoginEndpoints, LoginIo, LoginProgress, LoginSite, Method,
    OAuthCredential, ProviderError, SecretString,
    auth::{
        oauth::pkce_challenge,
        refresh::{RefreshReason, Refresher, TokenEndpoints},
    },
    find, login, store_api_key,
};

const GUARD: Duration = Duration::from_secs(20);
const CURSOR_POLL: &str = "/cursor/auth/poll";
const CURSOR_TOKEN: &str = "/cursor/oauth/token";
const CURSOR_USABLE: &str = "/cursor/agent.v1.AgentService/GetUsableModels";
const CURSOR_AVAILABLE: &str = "/cursor/aiserver.v1.AiService/AvailableModels";
const CURSOR_MODEL: &str = "composer-test";
const DEVIN_TOKEN: &str = "/devin/auth/cli/token";
const CURSOR_CLIENT: &str = "KbZUR41cY7W6zRSdpSUJ7I7mLYBKOCmB";

/// A JWT whose payload claims `exp`, so the stored expiry is the claim's.
fn jwt(subject: &str, exp: i64) -> String {
    format!(
        "{}.{}.sig",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#),
        URL_SAFE_NO_PAD.encode(format!(r#"{{"sub":"{subject}","exp":{exp}}}"#))
    )
}

fn json(body: impl Into<String>) -> Reply {
    Reply::Json(200, body.into())
}

/// A seeded `auth.json` beside one loopback server.
struct Fixture {
    dir: TestDir,
    seeded: Vec<u8>,
}

impl Fixture {
    async fn new() -> Self {
        let dir = TestDir::new();
        store_api_key(dir.0.join("auth.json"), "openai", String::from("sk-seed"))
            .await
            .expect("seed auth.json");
        let seeded = fs::read(dir.0.join("auth.json")).expect("read seeded auth.json");
        Self { dir, seeded }
    }

    fn site(&self, server: &Server) -> LoginSite {
        let endpoints = LoginEndpoints::loopback(server.base(), &EnvSnapshot::default())
            .expect("loopback endpoints");
        self.dir
            .site()
            .with_endpoints(endpoints)
            .expect("admitted endpoints")
    }

    /// The loopback site with the Devin callback pinned to `port`.
    fn site_with_devin_port(&self, server: &Server, port: u16) -> LoginSite {
        let endpoints = LoginEndpoints::loopback(server.base(), &EnvSnapshot::default())
            .expect("loopback endpoints")
            .with_callback_port(crate::Hook::DevinCliToken, port);
        self.dir
            .site()
            .with_endpoints(endpoints)
            .expect("admitted endpoints")
    }

    fn auth_path(&self) -> std::path::PathBuf {
        self.dir.0.join("auth.json")
    }

    fn stored(&self, provider: &str) -> Option<Credential> {
        AuthStore::load(self.auth_path())
            .expect("reload auth.json")
            .credential(provider)
    }

    fn assert_untouched(&self) {
        assert_eq!(
            fs::read(self.auth_path()).expect("read auth.json"),
            self.seeded
        );
    }

    fn assert_private(path: &std::path::Path) {
        let metadata = fs::metadata(path).expect("metadata");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        }
        #[cfg(not(unix))]
        assert!(metadata.is_file());
    }

    /// A refresher over this `auth.json` whose Cursor token endpoint is the
    /// loopback row's and whose other rows point at the server root.
    fn refresher(&self, server: &Server) -> Refresher {
        let cursor = find("cursor").expect("the cursor row");
        let endpoints = TokenEndpoints::with_bases(
            server.base(),
            server.base(),
            server.base(),
            server.base(),
            server.base(),
        )
        .and_then(|endpoints| endpoints.with_base(cursor, &format!("{}/cursor", server.base())))
        .expect("loopback token endpoints");
        Refresher::new(
            "dal/test",
            self.auth_path(),
            endpoints,
            &EnvSnapshot::default(),
        )
    }
}

async fn next_event(events: &mut mpsc::Receiver<LoginProgress>) -> LoginProgress {
    timeout(GUARD, events.recv())
        .await
        .expect("a progress event within the guard")
        .expect("the login still reports progress")
}

async fn open_url(events: &mut mpsc::Receiver<LoginProgress>) -> Url {
    match next_event(events).await {
        LoginProgress::OpenUrl { url } => Url::parse(&url).expect("an absolute URL"),
        other => panic!("expected the login page, got {other:?}"),
    }
}

fn query(url: &Url) -> Vec<(String, String)> {
    url.query_pairs().into_owned().collect()
}

fn pairs(request: &Request) -> HashMap<String, String> {
    Url::parse(&format!("http://localhost{}", request.target))
        .expect("request target")
        .query_pairs()
        .into_owned()
        .collect()
}

fn oauth_of(credential: &Credential) -> &OAuthCredential {
    match credential {
        Credential::OAuth(oauth) => oauth,
        other => panic!("expected an OAuth credential, got {other:?}"),
    }
}

async fn visit(redirect_uri: &str, query: &str) -> String {
    let redirect = Url::parse(redirect_uri).expect("redirect uri");
    let port = redirect.port().expect("callback port");
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect to the callback listener");
    let request = format!(
        "GET {}?{query} HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\n\r\n",
        redirect.path()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("send the callback request");
    let mut reply = String::new();
    timeout(GUARD, stream.read_to_string(&mut reply))
        .await
        .expect("callback reply within the guard")
        .expect("read the callback reply");
    reply
}

/// A `GetUsableModelsResponse` whose repeated field 1 holds one
/// `ModelDetails` naming [`CURSOR_MODEL`], in its exact unary wire. Every
/// byte is below `0x80`, so the shared text-bodied test server carries it
/// unchanged.
fn cursor_model_reply() -> Reply {
    use prost::Message as _;
    let details = crate::family::cursor::proto::ModelDetails {
        model_id: String::from(CURSOR_MODEL),
        display_name: String::from("Composer Test"),
        ..Default::default()
    }
    .encode_to_vec();
    let mut body = vec![0x0a, u8::try_from(details.len()).expect("a short message")];
    body.extend_from_slice(&details);
    assert!(body.iter().all(u8::is_ascii), "the fixture stays ASCII");
    Reply::Json(200, String::from_utf8(body).expect("ASCII protobuf"))
}

/// A Cursor server: `pending` poll answers of `404`, then the token release
/// for `access`; the refresh endpoint answers `refresh` in order. The two
/// catalog RPCs answer the post-login discovery with real unary bodies: one
/// usable model and an empty (zero-length) rich `AvailableModelsResponse`.
fn cursor_server(
    pending: usize,
    access: String,
    refresh: Vec<Reply>,
) -> impl Fn(&Request) -> Reply + Send + Sync + 'static {
    let polls = AtomicUsize::new(0);
    let refreshes = Mutex::new(refresh.into_iter());
    move |request| match request.path() {
        CURSOR_POLL if polls.fetch_add(1, Ordering::SeqCst) < pending => {
            Reply::Json(404, String::from("{}"))
        }
        CURSOR_POLL => json(format!(
            r#"{{"accessToken":"{access}","refreshToken":"cursor-refresh"}}"#
        )),
        CURSOR_TOKEN => refreshes
            .lock()
            .expect("refresh replies")
            .next()
            .unwrap_or_else(|| Reply::Json(500, String::from("{}"))),
        CURSOR_USABLE => cursor_model_reply(),
        CURSOR_AVAILABLE => Reply::Json(200, String::new()),
        _ => Reply::Json(404, String::from("{}")),
    }
}

/// Drives one Cursor browser login and returns its result, the login page,
/// and the server's requests.
async fn cursor_login(
    fixture: &Fixture,
    server: &Server,
) -> (Result<Credential, ProviderError>, Url) {
    let site = fixture.site(server);
    let (io, mut events) = LoginIo::channel(None, CancellationToken::new());
    let (result, page) = tokio::join!(
        timeout(GUARD, login("cursor", Method::Browser, io, &site)),
        open_url(&mut events),
    );
    (result.expect("the login finished within the guard"), page)
}

#[tokio::test]
async fn cursor_browser_login_polls_the_released_tokens_and_stores_them_privately() {
    let fixture = Fixture::new().await;
    let access = jwt("auth0|user_1", 4_000_000_000);
    let server = Server::start(cursor_server(1, access.clone(), Vec::new()))
        .await
        .expect("start the test server");
    let (result, page) = cursor_login(&fixture, &server).await;

    let credential = result.expect("the Cursor login succeeds");
    let oauth = oauth_of(&credential);
    assert_eq!(oauth.access_token.expose(), access);
    assert_eq!(oauth.refresh_token.expose(), "cursor-refresh");
    assert_eq!(oauth.expires_at, Some(4_000_000_000 - 300));
    assert_eq!(
        (oauth.id_token.as_ref(), oauth.account_id.as_ref()),
        (None, None)
    );
    assert_eq!(fixture.stored("cursor"), Some(credential.clone()));
    assert_eq!(
        fixture.stored("openai"),
        Some(Credential::ApiKey {
            key: SecretString::from("sk-seed")
        }),
        "the commit keeps the other entries"
    );
    Fixture::assert_private(&fixture.auth_path());

    assert_eq!(page.path(), "/cursor/loginDeepControl");
    let names: Vec<String> = query(&page).into_iter().map(|(name, _)| name).collect();
    assert_eq!(names, ["challenge", "uuid", "mode", "redirectTarget"]);
    let page_query: HashMap<String, String> = query(&page).into_iter().collect();
    assert_eq!(page_query["mode"], "login");
    assert_eq!(page_query["redirectTarget"], "cli");

    let polls = server.requests_to(CURSOR_POLL);
    assert_eq!(polls.len(), 2, "one pending answer, then the release");
    for poll in &polls {
        assert_eq!(poll.method, "GET");
        let sent = pairs(poll);
        assert_eq!(sent["uuid"], page_query["uuid"]);
        assert_eq!(pkce_challenge(&sent["verifier"]), page_query["challenge"]);
        assert_eq!(poll.header("authorization"), None);
    }
    let bearer = format!("Bearer {access}");
    for path in [CURSOR_USABLE, CURSOR_AVAILABLE] {
        let calls = server.requests_to(path);
        assert_eq!(calls.len(), 1, "{path}: one post-login discovery call");
        assert_eq!(calls[0].method, "POST", "{path}");
        assert_eq!(
            calls[0].header("authorization"),
            Some(bearer.as_str()),
            "{path}"
        );
        assert_eq!(
            calls[0].header("content-type"),
            Some("application/proto"),
            "{path}"
        );
    }
    assert_eq!(
        server.requests().len(),
        polls.len() + 2,
        "no callback, code exchange, refresh, retry, or default-model request"
    );
    let cache = fs::read_to_string(fixture.dir.0.join("cache").join("models.json"))
        .expect("the post-login discovery cached the account's models");
    assert!(cache.contains(CURSOR_MODEL), "{cache}");
    assert!(!cache.contains(&access) && !cache.contains("cursor-refresh"));
}

#[tokio::test]
async fn a_reloaded_cursor_sign_in_refreshes_rotates_and_ends_on_should_logout() {
    let fixture = Fixture::new().await;
    let renewed = jwt("auth0|user_1", 4_100_000_000);
    let server = Server::start(cursor_server(
        0,
        jwt("auth0|user_1", 4_000_000_000),
        vec![
            json(format!(
                r#"{{"access_token":"{renewed}","refresh_token":"cursor-rotated"}}"#
            )),
            json(r#"{"access_token":"","shouldLogout":true}"#),
        ],
    ))
    .await
    .expect("start the test server");
    let (result, _page) = cursor_login(&fixture, &server).await;
    result.expect("the Cursor login succeeds");

    let cursor = find("cursor").expect("the cursor row");
    let Some(Credential::OAuth(held)) = fixture.stored("cursor") else {
        panic!("the reloaded store holds the Cursor sign-in");
    };
    let refresher = fixture.refresher(&server);
    let fresh = refresher
        .refresh(cursor, &held, RefreshReason::Rejected)
        .await
        .expect("the refresh succeeds");
    let fresh = oauth_of(&fresh).clone();
    assert_eq!(fresh.access_token.expose(), renewed);
    assert_eq!(fresh.refresh_token.expose(), "cursor-rotated");
    assert_eq!(fresh.expires_at, Some(4_100_000_000 - 300));
    assert_eq!(
        fixture.stored("cursor"),
        Some(Credential::OAuth(fresh.clone()))
    );
    Fixture::assert_private(&fixture.auth_path());
    let grants = server.requests_to(CURSOR_TOKEN);
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0].method, "POST");
    assert_eq!(grants[0].header("content-type"), Some("application/json"));
    assert_eq!(
        grants[0].body,
        format!(
            r#"{{"grant_type":"refresh_token","client_id":"{CURSOR_CLIENT}","refresh_token":"cursor-refresh"}}"#
        )
    );

    let committed = fs::read(fixture.auth_path()).expect("read auth.json");
    let ended = refresher
        .refresh(cursor, &fresh, RefreshReason::Rejected)
        .await
        .expect_err("a shouldLogout answer ends the sign-in");
    assert!(
        matches!(&ended, ProviderError::SignInExpired { provider } if provider == "cursor"),
        "{ended:?}"
    );
    assert_eq!(server.requests_to(CURSOR_TOKEN).len(), 2, "no retry");
    assert_eq!(
        fs::read(fixture.auth_path()).expect("read auth.json"),
        committed,
        "an ended session stores nothing"
    );
    refresher.shutdown().await;
}

#[tokio::test]
async fn cancelling_a_cursor_login_while_it_polls_writes_nothing() {
    let fixture = Fixture::new().await;
    let mut server = Server::start(|request| match request.path() {
        CURSOR_POLL => Reply::Hold,
        _ => Reply::Json(404, String::from("{}")),
    })
    .await
    .expect("start the test server");
    let site = fixture.site(&server);
    let cancel = CancellationToken::new();
    let (io, mut events) = LoginIo::channel(None, cancel.clone());
    let driver = async {
        open_url(&mut events).await;
        server.next_request_to(CURSOR_POLL).await;
        cancel.cancel();
    };
    let (result, ()) = tokio::join!(
        timeout(GUARD, login("cursor", Method::Browser, io, &site)),
        driver
    );
    let result = result.expect("the login finished within the guard");
    assert!(
        matches!(result, Err(ProviderError::LoginCancelled)),
        "{result:?}"
    );
    fixture.assert_untouched();
}

fn devin_server(token: String) -> impl Fn(&Request) -> Reply + Send + Sync + 'static {
    move |request| match request.path() {
        DEVIN_TOKEN => json(format!(r#"{{"token":"{token}"}}"#)),
        _ => Reply::Json(404, String::from("{}")),
    }
}

/// The authorize query of a Devin login page, checked against the pinned
/// shape: no client id, the standard grant, then `prompt`.
fn devin_authorization(page: &Url) -> HashMap<String, String> {
    assert_eq!(page.path(), "/devin/auth/cli/continue");
    let names: Vec<String> = query(page).into_iter().map(|(name, _)| name).collect();
    assert_eq!(
        names,
        [
            "response_type",
            "redirect_uri",
            "code_challenge",
            "code_challenge_method",
            "state",
            "prompt"
        ]
    );
    let sent: HashMap<String, String> = query(page).into_iter().collect();
    assert_eq!(sent["response_type"], "code");
    assert_eq!(sent["code_challenge_method"], "S256");
    assert_eq!(sent["prompt"], "select_account");
    let redirect = Url::parse(&sent["redirect_uri"]).expect("redirect uri");
    assert_eq!(redirect.host_str(), Some("127.0.0.1"));
    assert_eq!(redirect.path(), "/callback");
    assert_ne!(redirect.port(), Some(0));
    sent
}

/// Checks the one Devin exchange and the stored non-refreshing credential.
fn assert_devin_signed_in(
    fixture: &Fixture,
    server: &Server,
    sent: &HashMap<String, String>,
    code: &str,
    token: &str,
    credential: &Credential,
) {
    let exchanges = server.requests_to(DEVIN_TOKEN);
    assert_eq!(exchanges.len(), 1);
    assert_eq!(exchanges[0].method, "POST");
    assert_eq!(exchanges[0].header("accept"), Some("application/json"));
    assert_eq!(
        exchanges[0].header("content-type"),
        Some("application/json")
    );
    let body: HashMap<String, String> =
        sonic_rs::from_str(&exchanges[0].body).expect("JSON exchange body");
    assert_eq!(body.len(), 2, "only the code and the verifier");
    assert_eq!(body["code"], code);
    assert_eq!(
        pkce_challenge(&body["code_verifier"]),
        sent["code_challenge"]
    );
    let oauth = oauth_of(credential);
    assert_eq!(oauth.access_token.expose(), token);
    assert_eq!(oauth.refresh_token.expose(), token);
    assert_eq!(oauth.expires_at, Some(4_000_000_000 - 300));
    assert_eq!(
        (oauth.id_token.as_ref(), oauth.account_id.as_ref()),
        (None, None)
    );
    assert_eq!(fixture.stored("devin").as_ref(), Some(credential));
    Fixture::assert_private(&fixture.auth_path());
}

#[tokio::test]
async fn devin_browser_login_exchanges_the_callback_code_and_never_refreshes() {
    let fixture = Fixture::new().await;
    let token = jwt("devin-user", 4_000_000_000);
    let server = Server::start(devin_server(token.clone()))
        .await
        .expect("start the test server");
    let site = fixture.site(&server);
    let (io, mut events) = LoginIo::channel(None, CancellationToken::new());
    let driver = async {
        let sent = devin_authorization(&open_url(&mut events).await);
        assert!(matches!(
            next_event(&mut events).await,
            LoginProgress::AskPaste { .. }
        ));
        let reply = visit(
            &sent["redirect_uri"],
            &format!("code=devin-code&state={}", sent["state"]),
        )
        .await;
        assert!(reply.starts_with("HTTP/1.1 200"), "callback reply: {reply}");
        sent
    };
    let (result, sent) = tokio::join!(
        timeout(GUARD, login("devin", Method::Browser, io, &site)),
        driver
    );
    let credential = result
        .expect("the login finished within the guard")
        .expect("the Devin login succeeds");
    assert_devin_signed_in(&fixture, &server, &sent, "devin-code", &token, &credential);

    let devin = find("devin").expect("the devin row");
    let held = oauth_of(&credential).clone();
    let before = fs::read(fixture.auth_path()).expect("read auth.json");
    let refresher = fixture.refresher(&server);
    let refused = refresher
        .refresh(devin, &held, RefreshReason::Rejected)
        .await
        .expect_err("a Devin token has no refresh grant");
    assert!(
        matches!(&refused, ProviderError::SignInExpired { provider } if provider == "devin"),
        "{refused:?}"
    );
    assert_eq!(
        refresher
            .refresh(devin, &held, RefreshReason::Expiring)
            .await
            .expect("an unexpired token is used as is"),
        credential
    );
    assert_eq!(
        server.requests_to(DEVIN_TOKEN).len(),
        1,
        "no refresh request"
    );
    assert_eq!(
        fs::read(fixture.auth_path()).expect("read auth.json"),
        before
    );
    refresher.shutdown().await;
}

#[tokio::test]
async fn a_pasted_devin_code_with_its_state_completes_the_login() {
    let fixture = Fixture::new().await;
    let token = jwt("devin-user", 4_000_000_000);
    let server = Server::start(devin_server(token.clone()))
        .await
        .expect("start the test server");
    let site = fixture.site(&server);
    let (paste, receiver) = oneshot::channel();
    let (io, mut events) = LoginIo::channel(Some(receiver), CancellationToken::new());
    let driver = async {
        let sent = devin_authorization(&open_url(&mut events).await);
        assert!(matches!(
            next_event(&mut events).await,
            LoginProgress::AskPaste { .. }
        ));
        paste
            .send(format!("pasted-code#{}", sent["state"]))
            .expect("the login still reads the paste");
        sent
    };
    let (result, sent) = tokio::join!(
        timeout(GUARD, login("devin", Method::Browser, io, &site)),
        driver
    );
    let credential = result
        .expect("the login finished within the guard")
        .expect("the pasted Devin login succeeds");
    assert_devin_signed_in(&fixture, &server, &sent, "pasted-code", &token, &credential);
}

#[tokio::test]
async fn a_busy_devin_callback_port_keeps_the_registered_uri_and_completes_by_paste() {
    let fixture = Fixture::new().await;
    let token = jwt("devin-user", 4_000_000_000);
    let server = Server::start(devin_server(token.clone()))
        .await
        .expect("start the test server");
    let busy = std::net::TcpListener::bind("127.0.0.1:0").expect("hold a loopback port");
    let port = busy.local_addr().expect("held port").port();
    let site = fixture.site_with_devin_port(&server, port);
    let (paste, receiver) = oneshot::channel();
    let (io, mut events) = LoginIo::channel(Some(receiver), CancellationToken::new());
    let driver = async {
        let sent = devin_authorization(&open_url(&mut events).await);
        let redirect = Url::parse(&sent["redirect_uri"]).expect("redirect uri");
        assert_eq!(
            redirect.port(),
            Some(port),
            "the registered port is advertised, never another one"
        );
        assert!(matches!(
            next_event(&mut events).await,
            LoginProgress::AskPaste { .. }
        ));
        paste
            .send(format!("busy-code#{}", sent["state"]))
            .expect("the login still reads the paste");
        sent
    };
    let (result, sent) = tokio::join!(
        timeout(GUARD, login("devin", Method::Browser, io, &site)),
        driver
    );
    drop(busy);
    let credential = result
        .expect("the login finished within the guard")
        .expect("the pasted Devin login succeeds on a busy port");
    assert_devin_signed_in(&fixture, &server, &sent, "busy-code", &token, &credential);
}

#[tokio::test]
async fn a_pasted_devin_code_with_a_foreign_state_fails_before_any_exchange() {
    let fixture = Fixture::new().await;
    let server = Server::start(devin_server(jwt("devin-user", 4_000_000_000)))
        .await
        .expect("start the test server");
    let site = fixture.site(&server);
    let (paste, receiver) = oneshot::channel();
    let (io, mut events) = LoginIo::channel(Some(receiver), CancellationToken::new());
    let driver = async {
        devin_authorization(&open_url(&mut events).await);
        assert!(matches!(
            next_event(&mut events).await,
            LoginProgress::AskPaste { .. }
        ));
        paste
            .send(String::from("pasted-code#not-this-attempt"))
            .expect("the login still reads the paste");
    };
    let (result, ()) = tokio::join!(
        timeout(GUARD, login("devin", Method::Browser, io, &site)),
        driver
    );
    let result = result.expect("the login finished within the guard");
    assert!(
        matches!(result, Err(ProviderError::StateMismatch)),
        "{result:?}"
    );
    assert!(server.requests().is_empty());
    fixture.assert_untouched();
}

#[tokio::test]
async fn cancelling_a_devin_login_before_the_callback_writes_nothing() {
    let fixture = Fixture::new().await;
    let server = Server::start(|_| Reply::Hold)
        .await
        .expect("start the test server");
    let site = fixture.site(&server);
    let cancel = CancellationToken::new();
    let (_paste, receiver) = oneshot::channel::<String>();
    let (io, mut events) = LoginIo::channel(Some(receiver), cancel.clone());
    let driver = async {
        devin_authorization(&open_url(&mut events).await);
        cancel.cancel();
    };
    let (result, ()) = tokio::join!(
        timeout(GUARD, login("devin", Method::Browser, io, &site)),
        driver
    );
    let result = result.expect("the login finished within the guard");
    assert!(
        matches!(result, Err(ProviderError::LoginCancelled)),
        "{result:?}"
    );
    assert!(server.requests().is_empty());
    fixture.assert_untouched();
}

#[test]
fn the_sign_in_list_offers_cursor_and_devin_by_key_and_browser() {
    for id in ["cursor", "devin"] {
        let def = super::login_providers()
            .into_iter()
            .find(|def| def.id == id)
            .unwrap_or_else(|| panic!("{id} is listed for sign-in"));
        assert_eq!(
            def.methods().collect::<Vec<_>>(),
            [Method::ApiKey, Method::Browser],
            "{id}"
        );
    }
}
