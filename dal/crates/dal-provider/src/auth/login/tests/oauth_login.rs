use std::{
    collections::HashMap,
    fs,
    future::Future,
    sync::atomic::{AtomicUsize, Ordering},
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
    AuthStore, Credential, LoginEndpoints, LoginIo, LoginProgress, LoginSite, Method,
    OAuthCredential, ProviderError, SecretString,
    auth::{
        oauth::{pkce_challenge, unix_now},
        refresh::lock_auth_file,
    },
    find, login, store_api_key,
};

const GUARD: Duration = Duration::from_secs(20);
const CODE_PATH: &str = "/api/accounts/deviceauth/usercode";
const POLL_PATH: &str = "/api/accounts/deviceauth/token";
const CODEX_TOKEN_PATH: &str = "/oauth/token";
const CODEX_MODELS_PATH: &str = "/backend-api/codex/models";
const CLAUDE_TOKEN_PATH: &str = "/claude/v1/oauth/token";
const MODELS: &str = r#"{"models":[{"slug":"codex-test","display_name":"Codex Test","visibility":"list","context_window":200000}]}"#;

fn id_token(account: &str) -> String {
    let claims = format!(
        r#"{{"https://api.openai.com/auth":{{"chatgpt_account_id":"{account}","chatgpt_user_id":"user-1"}}}}"#
    );
    format!(
        "{}.{}.not-a-signature",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256"}"#),
        URL_SAFE_NO_PAD.encode(claims)
    )
}

fn codex_tokens() -> String {
    format!(
        r#"{{"id_token":"{}","access_token":"codex-access","refresh_token":"codex-refresh","expires_in":3600}}"#,
        id_token("acct-1")
    )
}

fn claude_tokens() -> String {
    String::from(
        r#"{"access_token":"claude-access","refresh_token":"claude-refresh","expires_in":7200}"#,
    )
}

fn json(body: impl Into<String>) -> Reply {
    Reply::Json(200, body.into())
}

fn client_id(provider: &str) -> &'static str {
    find(provider)
        .and_then(|def| def.oauth.as_ref())
        .map(|oauth| oauth.client_id)
        .expect("provider with OAuth")
}

struct Fixture {
    dir: TestDir,
    server: Server,
    seeded: Vec<u8>,
}

impl Fixture {
    async fn new(handler: impl Fn(&Request) -> Reply + Send + Sync + 'static) -> Self {
        let dir = TestDir::new();
        store_api_key(dir.0.join("auth.json"), "openai", String::from("sk-seed"))
            .await
            .expect("seed auth.json");
        let seeded = fs::read(dir.0.join("auth.json")).expect("read seeded auth.json");
        Self {
            dir,
            server: Server::start(handler).await.expect("start the test server"),
            seeded,
        }
    }

    fn site(&self) -> LoginSite {
        let endpoints = LoginEndpoints::loopback(self.server.base()).expect("loopback endpoints");
        self.dir
            .site()
            .with_endpoints(endpoints)
            .expect("admitted endpoints")
    }

    fn auth_json(&self) -> Vec<u8> {
        fs::read(self.dir.0.join("auth.json")).expect("read auth.json")
    }

    fn stored(&self, provider: &str) -> Option<Credential> {
        AuthStore::load(self.dir.0.join("auth.json"))
            .expect("load auth.json")
            .credential(provider)
    }

    fn assert_untouched(&self) {
        assert_eq!(self.auth_json(), self.seeded);
    }
}

struct Authorization {
    redirect_uri: String,
    state: String,
    challenge: String,
}

fn authorization(url: &str) -> Authorization {
    let url = Url::parse(url).expect("authorization url");
    let pairs: HashMap<String, String> = url.query_pairs().into_owned().collect();
    Authorization {
        redirect_uri: pairs["redirect_uri"].clone(),
        state: pairs["state"].clone(),
        challenge: pairs["code_challenge"].clone(),
    }
}

async fn next_event(events: &mut mpsc::Receiver<LoginProgress>) -> LoginProgress {
    timeout(GUARD, events.recv())
        .await
        .expect("a progress event within the guard")
        .expect("the login still reports progress")
}

async fn authorization_event(events: &mut mpsc::Receiver<LoginProgress>) -> Authorization {
    match next_event(events).await {
        LoginProgress::OpenUrl { url } => authorization(&url),
        other => panic!("expected the authorization URL, got {other:?}"),
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

async fn drive<D: Future>(
    provider: &str,
    method: Method,
    io: LoginIo,
    site: &LoginSite,
    driver: D,
) -> (Result<Credential, ProviderError>, D::Output) {
    let (result, outcome) = tokio::join!(timeout(GUARD, login(provider, method, io, site)), driver);
    (
        result.expect("the login finished within the guard"),
        outcome,
    )
}

fn oauth_of(credential: &Credential) -> &OAuthCredential {
    match credential {
        Credential::OAuth(oauth) => oauth,
        other => panic!("expected an OAuth credential, got {other:?}"),
    }
}

fn body_map(request: &Request) -> HashMap<String, String> {
    sonic_rs::from_str(&request.body).expect("JSON request body")
}

fn codex_server() -> impl Fn(&Request) -> Reply + Send + Sync + 'static {
    |request| match request.path() {
        CODEX_TOKEN_PATH => json(codex_tokens()),
        CODEX_MODELS_PATH => json(MODELS),
        _ => Reply::Json(404, String::from("{}")),
    }
}

#[tokio::test]
async fn codex_browser_login_stores_the_complete_account_bound_credential() {
    let fixture = Fixture::new(codex_server()).await;
    let site = fixture.site();
    let (io, mut events) = LoginIo::channel(None, CancellationToken::new());
    let driver = async {
        let auth = authorization_event(&mut events).await;
        let reply = visit(
            &auth.redirect_uri,
            &format!("code=auth-code&state={}", auth.state),
        )
        .await;
        assert!(reply.starts_with("HTTP/1.1 200"), "callback reply: {reply}");
        assert!(matches!(
            next_event(&mut events).await,
            LoginProgress::Exchanging
        ));
        auth
    };
    let (result, auth) = drive("openai-codex", Method::Browser, io, &site, driver).await;

    let credential = result.expect("the login succeeds");
    let oauth = oauth_of(&credential);
    assert_eq!(oauth.access_token.expose(), "codex-access");
    assert_eq!(oauth.refresh_token.expose(), "codex-refresh");
    assert_eq!(oauth.id_token.as_deref(), Some(id_token("acct-1").as_str()));
    assert_eq!(oauth.account_id.as_deref(), Some("acct-1"));
    let now = unix_now();
    assert!(
        oauth
            .expires_at
            .is_some_and(|at| (now + 3500..=now + 3700).contains(&at))
    );
    assert_eq!(fixture.stored("openai-codex"), Some(credential.clone()));
    assert_eq!(
        fixture.stored("openai"),
        Some(Credential::ApiKey {
            key: SecretString::from("sk-seed")
        })
    );

    let exchange = fixture.server.requests_to(CODEX_TOKEN_PATH);
    assert_eq!(exchange.len(), 1);
    assert_eq!(exchange[0].method, "POST");
    assert!(
        exchange[0]
            .header("content-type")
            .is_some_and(|kind| kind.starts_with("application/x-www-form-urlencoded"))
    );
    assert_eq!(
        exchange[0].field("grant_type").as_deref(),
        Some("authorization_code")
    );
    assert_eq!(
        exchange[0].field("client_id").as_deref(),
        Some(client_id("openai-codex"))
    );
    assert_eq!(exchange[0].field("code").as_deref(), Some("auth-code"));
    assert_eq!(
        exchange[0].field("redirect_uri").as_deref(),
        Some(auth.redirect_uri.as_str())
    );
    let verifier = exchange[0].field("code_verifier").expect("code_verifier");
    assert_eq!(pkce_challenge(&verifier), auth.challenge);

    let models = fixture.server.requests_to(CODEX_MODELS_PATH);
    assert_eq!(models.len(), 1);
    assert_eq!(
        models[0].header("authorization"),
        Some("Bearer codex-access")
    );
    assert_eq!(models[0].header("chatgpt-account-id"), Some("acct-1"));
    let cached = fs::read_to_string(fixture.dir.0.join("cache").join("models.json"))
        .expect("the post-login model fetch fills the cache");
    assert!(cached.contains("codex-test"));
}

#[tokio::test]
async fn cancelling_a_codex_login_during_the_token_exchange_leaves_auth_json_unchanged() {
    let mut fixture = Fixture::new(|request| match request.path() {
        CODEX_TOKEN_PATH => Reply::Hold,
        _ => Reply::Json(404, String::from("{}")),
    })
    .await;
    let site = fixture.site();
    let cancel = CancellationToken::new();
    let (io, mut events) = LoginIo::channel(None, cancel.clone());
    let server = &mut fixture.server;
    let driver = async {
        let auth = authorization_event(&mut events).await;
        visit(
            &auth.redirect_uri,
            &format!("code=auth-code&state={}", auth.state),
        )
        .await;
        server.next_request_to(CODEX_TOKEN_PATH).await;
        cancel.cancel();
    };
    let (result, ()) = drive("openai-codex", Method::Browser, io, &site, driver).await;

    assert!(matches!(result, Err(ProviderError::LoginCancelled)));
    assert!(fixture.server.requests_to(CODEX_MODELS_PATH).is_empty());
    fixture.assert_untouched();
}

#[tokio::test]
async fn cancelling_after_the_commit_returns_the_stored_credential() {
    let mut fixture = Fixture::new(|request| match request.path() {
        CODEX_TOKEN_PATH => json(codex_tokens()),
        CODEX_MODELS_PATH => Reply::Hold,
        _ => Reply::Json(404, String::from("{}")),
    })
    .await;
    let site = fixture.site();
    let cancel = CancellationToken::new();
    let (io, mut events) = LoginIo::channel(None, cancel.clone());
    let server = &mut fixture.server;
    let driver = async {
        let auth = authorization_event(&mut events).await;
        visit(
            &auth.redirect_uri,
            &format!("code=auth-code&state={}", auth.state),
        )
        .await;
        server.next_request_to(CODEX_MODELS_PATH).await;
        let committed = AuthStore::load(fixture.dir.0.join("auth.json"))
            .expect("load auth.json")
            .credential("openai-codex");
        cancel.cancel();
        committed
    };
    let (result, committed) = drive("openai-codex", Method::Browser, io, &site, driver).await;

    let credential = result.expect("a committed login is not reported as cancelled");
    assert_eq!(committed.as_ref(), Some(&credential));
    assert_eq!(fixture.stored("openai-codex"), Some(credential));
}

#[tokio::test]
async fn a_login_commit_keeps_entries_another_writer_stored_while_it_waited_for_the_lock() {
    let mut fixture = Fixture::new(codex_server()).await;
    let site = fixture.site();
    let path = fixture.dir.0.join("auth.json");
    let lock = lock_auth_file(&path).await.expect("hold the auth lock");
    let (io, mut events) = LoginIo::channel(None, CancellationToken::new());
    let server = &mut fixture.server;
    let driver = async {
        let auth = authorization_event(&mut events).await;
        visit(
            &auth.redirect_uri,
            &format!("code=auth-code&state={}", auth.state),
        )
        .await;
        server.next_request_to(CODEX_TOKEN_PATH).await;
        let mut newer = AuthStore::load(&path).expect("load under the lock");
        newer
            .set(
                "anthropic",
                Credential::ApiKey {
                    key: SecretString::from("sk-newer"),
                },
            )
            .expect("set the newer entry");
        newer.store().expect("store the newer entry");
        drop(lock);
    };
    let (result, ()) = drive("openai-codex", Method::Browser, io, &site, driver).await;

    let credential = result.expect("the login commits after the lock is released");
    assert_eq!(fixture.stored("openai-codex"), Some(credential));
    assert_eq!(
        fixture.stored("anthropic"),
        Some(Credential::ApiKey {
            key: SecretString::from("sk-newer")
        })
    );
    assert_eq!(
        fixture.stored("openai"),
        Some(Credential::ApiKey {
            key: SecretString::from("sk-seed")
        })
    );
}

#[tokio::test]
async fn a_progress_consumer_that_is_gone_cancels_a_browser_login_before_any_request() {
    let fixture = Fixture::new(codex_server()).await;
    let site = fixture.site();
    let cancel = CancellationToken::new();
    let (io, events) = LoginIo::channel(None, cancel.clone());
    drop(events);
    let (result, ()) = drive("openai-codex", Method::Browser, io, &site, async {}).await;

    assert!(matches!(result, Err(ProviderError::LoginCancelled)));
    assert!(cancel.is_cancelled());
    assert!(fixture.server.requests().is_empty());
    fixture.assert_untouched();
}

#[tokio::test]
async fn a_progress_consumer_that_leaves_mid_login_stops_it_before_the_token_exchange() {
    let fixture = Fixture::new(codex_server()).await;
    let site = fixture.site();
    let cancel = CancellationToken::new();
    let (io, mut events) = LoginIo::channel(None, cancel.clone());
    let driver = async {
        let auth = authorization_event(&mut events).await;
        drop(events);
        visit(
            &auth.redirect_uri,
            &format!("code=auth-code&state={}", auth.state),
        )
        .await;
    };
    let (result, ()) = drive("openai-codex", Method::Browser, io, &site, driver).await;

    assert!(matches!(result, Err(ProviderError::LoginCancelled)));
    assert!(cancel.is_cancelled());
    assert!(fixture.server.requests_to(CODEX_TOKEN_PATH).is_empty());
    fixture.assert_untouched();
}

fn device_server(polls: AtomicUsize) -> impl Fn(&Request) -> Reply + Send + Sync + 'static {
    move |request| match request.path() {
        CODE_PATH => json(r#"{"device_auth_id":"dev-1","user_code":"WXYZ-1234","interval":1}"#),
        POLL_PATH => match polls.fetch_add(1, Ordering::SeqCst) {
            0 => Reply::Json(403, String::from(r#"{"error":"pending"}"#)),
            1 => Reply::Json(404, String::from(r#"{"error":"pending"}"#)),
            _ => json(r#"{"authorization_code":"device-code","code_verifier":"device-verifier"}"#),
        },
        CODEX_TOKEN_PATH => json(codex_tokens()),
        CODEX_MODELS_PATH => json(MODELS),
        _ => Reply::Json(404, String::from("{}")),
    }
}

#[tokio::test]
async fn device_login_waits_through_pending_polls_then_stores_the_exchanged_credential() {
    let fixture = Fixture::new(device_server(AtomicUsize::new(0))).await;
    let site = fixture.site();
    let (io, mut events) = LoginIo::channel(None, CancellationToken::new());
    let base = fixture.server.base().to_owned();
    let driver = async {
        let first = next_event(&mut events).await;
        let second = next_event(&mut events).await;
        (first, second)
    };
    let (result, (first, second)) = drive("openai-codex", Method::Device, io, &site, driver).await;

    assert_eq!(
        first,
        LoginProgress::ShowCode {
            url: format!("{base}/codex/device"),
            code: String::from("WXYZ-1234"),
        }
    );
    assert_eq!(second, LoginProgress::Exchanging);
    let credential = result.expect("the device login succeeds");
    let oauth = oauth_of(&credential);
    assert_eq!(oauth.access_token.expose(), "codex-access");
    assert_eq!(oauth.account_id.as_deref(), Some("acct-1"));
    assert_eq!(fixture.stored("openai-codex"), Some(credential));

    let start = fixture.server.requests_to(CODE_PATH);
    assert_eq!(start.len(), 1);
    assert_eq!(body_map(&start[0])["client_id"], client_id("openai-codex"));
    let polls = fixture.server.requests_to(POLL_PATH);
    assert_eq!(polls.len(), 3, "two pending answers, then the grant");
    for poll in &polls {
        let body = body_map(poll);
        assert_eq!(body["device_auth_id"], "dev-1");
        assert_eq!(body["user_code"], "WXYZ-1234");
    }
    let exchange = fixture.server.requests_to(CODEX_TOKEN_PATH);
    assert_eq!(exchange.len(), 1);
    assert_eq!(exchange[0].field("code").as_deref(), Some("device-code"));
    assert_eq!(
        exchange[0].field("code_verifier").as_deref(),
        Some("device-verifier")
    );
    assert_eq!(
        exchange[0].field("redirect_uri").as_deref(),
        find("openai-codex")
            .and_then(|def| def.oauth.as_ref())
            .and_then(|oauth| oauth.device.as_ref())
            .map(|device| device.redirect_uri)
    );
}

#[tokio::test]
async fn cancelling_a_device_login_while_it_waits_to_poll_writes_nothing() {
    let fixture = Fixture::new(device_server(AtomicUsize::new(0))).await;
    let site = fixture.site();
    let cancel = CancellationToken::new();
    let (io, mut events) = LoginIo::channel(None, cancel.clone());
    let driver = async {
        assert!(matches!(
            next_event(&mut events).await,
            LoginProgress::ShowCode { .. }
        ));
        cancel.cancel();
    };
    let (result, ()) = drive("openai-codex", Method::Device, io, &site, driver).await;

    assert!(matches!(result, Err(ProviderError::LoginCancelled)));
    assert!(fixture.server.requests_to(CODEX_TOKEN_PATH).is_empty());
    fixture.assert_untouched();
}

#[tokio::test]
async fn a_closed_progress_channel_stops_a_device_login_after_the_code_request() {
    let fixture = Fixture::new(device_server(AtomicUsize::new(0))).await;
    let site = fixture.site();
    let (io, events) = LoginIo::channel(None, CancellationToken::new());
    drop(events);
    let (result, ()) = drive("openai-codex", Method::Device, io, &site, async {}).await;

    assert!(matches!(result, Err(ProviderError::LoginCancelled)));
    assert_eq!(fixture.server.requests_to(CODE_PATH).len(), 1);
    assert!(fixture.server.requests_to(POLL_PATH).is_empty());
    fixture.assert_untouched();
}

fn claude_server() -> impl Fn(&Request) -> Reply + Send + Sync + 'static {
    |request| match request.path() {
        CLAUDE_TOKEN_PATH => json(claude_tokens()),
        _ => Reply::Json(404, String::from("{}")),
    }
}

fn assert_claude_exchange(fixture: &Fixture, auth: &Authorization, code: &str) {
    let exchange = fixture.server.requests_to(CLAUDE_TOKEN_PATH);
    assert_eq!(exchange.len(), 1);
    let body = body_map(&exchange[0]);
    assert_eq!(body["grant_type"], "authorization_code");
    assert_eq!(body["client_id"], client_id("anthropic"));
    assert_eq!(body["code"], code);
    assert_eq!(body["state"], auth.state);
    assert_eq!(body["redirect_uri"], auth.redirect_uri);
    assert_eq!(pkce_challenge(&body["code_verifier"]), auth.challenge);
    let credential = fixture.stored("anthropic").expect("stored credential");
    let oauth = oauth_of(&credential);
    assert_eq!(oauth.access_token.expose(), "claude-access");
    assert_eq!(oauth.refresh_token.expose(), "claude-refresh");
    assert_eq!(oauth.id_token, None);
    assert_eq!(oauth.account_id, None);
}

#[tokio::test]
async fn a_pasted_value_reaches_the_claude_flow_and_completes_the_login() {
    let fixture = Fixture::new(claude_server()).await;
    let site = fixture.site();
    let (paste, receiver) = oneshot::channel();
    let (io, mut events) = LoginIo::channel(Some(receiver), CancellationToken::new());
    let driver = async {
        let auth = authorization_event(&mut events).await;
        assert!(matches!(
            next_event(&mut events).await,
            LoginProgress::AskPaste { .. }
        ));
        paste
            .send(format!("paste-code#{}", auth.state))
            .expect("the login still reads the paste");
        assert!(matches!(
            next_event(&mut events).await,
            LoginProgress::Exchanging
        ));
        auth
    };
    let (result, auth) = drive("anthropic", Method::Browser, io, &site, driver).await;

    let credential = result.expect("the pasted login succeeds");
    assert_eq!(fixture.stored("anthropic"), Some(credential));
    assert_claude_exchange(&fixture, &auth, "paste-code");
}

#[tokio::test]
async fn a_dropped_paste_sender_leaves_the_browser_callback_able_to_finish_the_login() {
    let fixture = Fixture::new(claude_server()).await;
    let site = fixture.site();
    let (paste, receiver) = oneshot::channel::<String>();
    let (io, mut events) = LoginIo::channel(Some(receiver), CancellationToken::new());
    let driver = async {
        let auth = authorization_event(&mut events).await;
        assert!(matches!(
            next_event(&mut events).await,
            LoginProgress::AskPaste { .. }
        ));
        drop(paste);
        let reply = visit(
            &auth.redirect_uri,
            &format!("code=callback-code&state={}", auth.state),
        )
        .await;
        assert!(reply.starts_with("HTTP/1.1 200"), "callback reply: {reply}");
        assert!(matches!(
            next_event(&mut events).await,
            LoginProgress::Exchanging
        ));
        auth
    };
    let (result, auth) = drive("anthropic", Method::Browser, io, &site, driver).await;

    let credential = result.expect("the callback completes the login");
    assert_eq!(fixture.stored("anthropic"), Some(credential));
    assert_claude_exchange(&fixture, &auth, "callback-code");
}

#[tokio::test]
async fn a_closed_host_paste_receiver_closes_the_flow_paste_channel() {
    let (flow_sender, flow_receiver) = oneshot::channel::<String>();
    let (paste_sender, paste_receiver) = oneshot::channel::<String>();
    let forward = super::super::forward_paste(Some(flow_sender), Some(paste_receiver));
    tokio::pin!(forward);
    drop(paste_sender);

    let flow_closed = timeout(GUARD, async {
        tokio::select! {
            result = flow_receiver => result,
            never = &mut forward => match never {},
        }
    })
    .await
    .expect("the flow paste channel closes within the guard");
    assert!(flow_closed.is_err());
}

#[tokio::test]
async fn cancelling_a_claude_login_that_waits_for_input_writes_nothing() {
    let fixture = Fixture::new(claude_server()).await;
    let site = fixture.site();
    let cancel = CancellationToken::new();
    let (_paste, receiver) = oneshot::channel::<String>();
    let (io, mut events) = LoginIo::channel(Some(receiver), cancel.clone());
    let driver = async {
        authorization_event(&mut events).await;
        assert!(matches!(
            next_event(&mut events).await,
            LoginProgress::AskPaste { .. }
        ));
        cancel.cancel();
    };
    let (result, ()) = drive("anthropic", Method::Browser, io, &site, driver).await;

    assert!(matches!(result, Err(ProviderError::LoginCancelled)));
    assert!(fixture.server.requests().is_empty());
    fixture.assert_untouched();
}

#[tokio::test]
async fn a_full_progress_channel_stops_a_claude_login_before_it_asks_for_input() {
    let fixture = Fixture::new(claude_server()).await;
    let site = fixture.site();
    let cancel = CancellationToken::new();
    let (progress, events) = mpsc::channel(1);
    let (_paste, receiver) = oneshot::channel::<String>();
    let io = LoginIo {
        progress,
        paste: Some(receiver),
        cancel: cancel.clone(),
    };
    let (result, ()) = drive("anthropic", Method::Browser, io, &site, async {}).await;

    assert!(matches!(result, Err(ProviderError::LoginCancelled)));
    assert!(cancel.is_cancelled());
    assert!(fixture.server.requests().is_empty());
    fixture.assert_untouched();
    drop(events);
}
