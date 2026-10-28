//! Public-surface regressions of the native Google Cloud Code Assist sign-ins
//! (`google-cloud-code-assist`, `google-antigravity`).
//!
//! Every test drives [`login`] against one loopback server through
//! [`LoginEndpoints::loopback`]: the authorize page is read from the progress
//! events, the browser is played by a request to the loopback callback, and
//! the token, `loadCodeAssist`, `onboardUser`, and operation endpoints all
//! answer from the same server. `auth.json` is reloaded from
//! disk after each login, and the refresh tests hand the reloaded credential
//! to a [`Refresher`] whose token endpoint moved to the same server. No
//! network beyond the loopback and no live credentials.

use std::{collections::HashMap, fs, time::Duration};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::mpsc,
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
        oauth::unix_now,
        refresh::{RefreshReason, Refresher, TokenEndpoints},
    },
    family::google::antigravity,
    find, login, store_api_key,
};

const GUARD: Duration = Duration::from_secs(20);

const CLI_TOKEN: &str = "/cloud-code-assist/token";
const CLI_AUTHORIZE: &str = "/cloud-code-assist/o/oauth2/v2/auth";
const CLI_LOAD: &str = "/cloud-code-assist/v1internal:loadCodeAssist";
const CLI_ONBOARD: &str = "/cloud-code-assist/v1internal:onboardUser";
const CLI_OPERATION: &str = "/cloud-code-assist/v1internal/operations/op-1";

const AG_TOKEN: &str = "/antigravity/token";
const AG_LOAD: &str = "/antigravity/v1internal:loadCodeAssist";

const ACCESS: &str = "ya29.fresh-access";
const REFRESH: &str = "1//fresh-refresh";

/// Google's token answer: both tokens, a lifetime, and the fields the engine
/// ignores.
fn token_reply() -> Reply {
    json(format!(
        r#"{{"access_token":"{ACCESS}","refresh_token":"{REFRESH}","expires_in":3599,"scope":"openid","token_type":"Bearer","id_token":"ignored.id.token"}}"#
    ))
}

fn json(body: impl Into<String>) -> Reply {
    Reply::Json(200, body.into())
}

fn not_found() -> Reply {
    Reply::Json(404, String::from("{}"))
}

/// A Cloud Code Assist server whose `loadCodeAssist` names no tier, so the free tier
/// is onboarded through a long-running operation that the first poll finishes.
fn onboarding_server() -> impl Fn(&Request) -> Reply + Send + Sync + 'static {
    move |request| match (request.method.as_str(), request.path()) {
        ("POST", CLI_TOKEN) => token_reply(),
        ("POST", CLI_LOAD) => json(r#"{"allowedTiers":[{"id":"free-tier","isDefault":true}]}"#),
        ("POST", CLI_ONBOARD) => json(r#"{"name":"operations/op-1","done":false}"#),
        ("GET", CLI_OPERATION) => json(
            r#"{"name":"operations/op-1","done":true,"response":{"cloudaicompanionProject":{"id":"provisioned-project"}}}"#,
        ),
        _ => not_found(),
    }
}

/// A Cloud Code Assist server whose `loadCodeAssist` answers `load`; every other
/// discovery route is absent, so a request to one fails the sign-in.
fn load_only_server(load: Reply) -> impl Fn(&Request) -> Reply + Send + Sync + 'static {
    let load = std::sync::Mutex::new(Some(load));
    move |request| match (request.method.as_str(), request.path()) {
        ("POST", CLI_TOKEN) => token_reply(),
        ("POST", CLI_LOAD) => load
            .lock()
            .expect("load reply")
            .take()
            .unwrap_or_else(not_found),
        _ => not_found(),
    }
}

/// An Antigravity server whose account is already onboarded with a project.
fn antigravity_server() -> impl Fn(&Request) -> Reply + Send + Sync + 'static {
    move |request| match (request.method.as_str(), request.path()) {
        ("POST", AG_TOKEN) => token_reply(),
        ("POST", AG_LOAD) => json(
            r#"{"cloudaicompanionProject":"ag-project","currentTier":{"id":"free-tier"},"paidTier":{"id":"free-tier"},"allowedTiers":[{"id":"free-tier","isDefault":true}]}"#,
        ),
        _ => not_found(),
    }
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

    fn site(&self, server: &Server, env: &EnvSnapshot) -> LoginSite {
        let endpoints = LoginEndpoints::loopback(server.base(), env).expect("loopback endpoints");
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
        other => panic!("expected the authorize page, got {other:?}"),
    }
}

fn query(url: &Url) -> Vec<(String, String)> {
    url.query_pairs().into_owned().collect()
}

fn oauth_of(credential: &Credential) -> &OAuthCredential {
    match credential {
        Credential::OAuth(oauth) => oauth,
        other => panic!("expected an OAuth credential, got {other:?}"),
    }
}

/// Plays the browser: one request to the loopback callback.
async fn visit(redirect_uri: &str, query: &str) -> String {
    let redirect = Url::parse(redirect_uri).expect("redirect uri");
    let port = redirect.port().expect("callback port");
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect to the callback listener");
    let request = format!(
        "GET {}?{query} HTTP/1.1\r\nhost: 127.0.0.1\r\nconnection: close\r\n\r\n",
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

/// What one browser login produced: the result, the authorize page, and the
/// progress events that followed the page.
struct Run {
    result: Result<Credential, ProviderError>,
    page: Url,
    events: Vec<LoginProgress>,
}

/// Drives one browser login of `provider` to its code exchange: reads the
/// authorize page, follows its redirect with `code` and the page's own
/// `state`, and collects the rest of the progress.
async fn browser_login(provider: &str, site: &LoginSite) -> Run {
    let (io, mut events) = LoginIo::channel(None, CancellationToken::new());
    let driver = async {
        let page = open_url(&mut events).await;
        let pairs: HashMap<String, String> = query(&page).into_iter().collect();
        let paste = next_event(&mut events).await;
        let callback = visit(
            &pairs["redirect_uri"],
            &format!("code=the-code&state={}", pairs["state"]),
        )
        .await;
        assert!(callback.contains("Sign-in complete"), "{callback}");
        (page, vec![paste])
    };
    let (result, (page, mut seen)) = tokio::join!(
        timeout(GUARD, login(provider, Method::Browser, io, site)),
        driver
    );
    while let Ok(event) = events.try_recv() {
        seen.push(event);
    }
    Run {
        result: result.expect("the login finished within the guard"),
        page,
        events: seen,
    }
}

fn names(pairs: &[(String, String)]) -> Vec<&str> {
    pairs.iter().map(|(name, _)| name.as_str()).collect()
}

fn row_client(provider: &str) -> (&'static str, &'static str) {
    let oauth = find(provider)
        .and_then(|def| def.oauth)
        .expect("a Cloud Code Assist row");
    (
        oauth.client_id.expect("a registered client id"),
        oauth.client_secret.expect("a registered client secret"),
    )
}

/// Asserts the authorize-page shape of one Cloud Code Assist browser
/// sign-in: the pinned path, the pinned parameter order, the registered
/// client id, and a fresh random `state`. Returns the parsed query,
/// carrying the loopback `redirect_uri` the token request must echo.
fn assert_authorize_page(authorize: &Url, client_id: &str) -> HashMap<String, String> {
    let pairs = query(authorize);
    assert_eq!(authorize.path(), CLI_AUTHORIZE);
    assert_eq!(
        names(&pairs),
        [
            "client_id",
            "response_type",
            "redirect_uri",
            "scope",
            "state",
            "access_type",
            "prompt"
        ]
    );
    let page: HashMap<String, String> = pairs.into_iter().collect();
    assert_eq!(page["client_id"], client_id);
    assert_eq!(page["response_type"], "code");
    assert_eq!(page["access_type"], "offline");
    assert_eq!(page["prompt"], "consent");
    assert_eq!(
        page["scope"],
        "https://www.googleapis.com/auth/cloud-platform \
         https://www.googleapis.com/auth/userinfo.email \
         https://www.googleapis.com/auth/userinfo.profile"
    );
    assert!(
        page["redirect_uri"].starts_with("http://127.0.0.1:")
            && page["redirect_uri"].ends_with("/oauth2callback"),
        "{}",
        page["redirect_uri"]
    );
    assert_eq!(page["state"].len(), 32, "a fresh random state");
    assert!(page["state"].bytes().all(|byte| byte.is_ascii_hexdigit()));
    page
}

/// Asserts the token-exchange request of one Cloud Code Assist browser
/// sign-in: the installed-application form carries the registered client
/// secret and the pasted code, echoes the authorize page's redirect, and
/// sends no PKCE verifier.
fn assert_token_request(token: &Request, client_id: &str, client_secret: &str, redirect_uri: &str) {
    assert_eq!(token.method, "POST");
    assert_eq!(
        token.header("content-type"),
        Some("application/x-www-form-urlencoded")
    );
    let form = token.form();
    assert_eq!(
        form.iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        [
            "grant_type",
            "client_id",
            "client_secret",
            "code",
            "redirect_uri"
        ],
        "no PKCE verifier is sent"
    );
    assert_eq!(
        token.field("grant_type").as_deref(),
        Some("authorization_code")
    );
    assert_eq!(token.field("client_id").as_deref(), Some(client_id));
    assert_eq!(token.field("client_secret").as_deref(), Some(client_secret));
    assert_eq!(token.field("code").as_deref(), Some("the-code"));
    assert_eq!(token.field("redirect_uri").as_deref(), Some(redirect_uri));
}

#[tokio::test]
async fn cloud_code_assist_login_exchanges_discovers_the_project_and_stores_it_privately() {
    let fixture = Fixture::new().await;
    let server = Server::start(onboarding_server())
        .await
        .expect("start the test server");
    let site = fixture.site(&server, &EnvSnapshot::default());
    let before = unix_now();
    let run = browser_login("google-cloud-code-assist", &site).await;
    let after = unix_now();

    let credential = run.result.expect("the Cloud Code Assist login succeeds");
    let oauth = oauth_of(&credential);
    assert_eq!(oauth.access_token.expose(), ACCESS);
    assert_eq!(oauth.refresh_token.expose(), REFRESH);
    assert!(
        oauth
            .expires_at
            .is_some_and(|at| (before + 3599..=after + 3599).contains(&at)),
        "{:?}",
        oauth.expires_at
    );
    assert_eq!(
        (oauth.id_token.as_ref(), oauth.account_id.as_ref()),
        (None, None),
        "the ignored ID token never reaches the credential"
    );
    assert_eq!(oauth.project_id.as_deref(), Some("provisioned-project"));
    assert_eq!(
        fixture.stored("google-cloud-code-assist"),
        Some(credential.clone())
    );
    assert_eq!(
        fixture.stored("openai"),
        Some(Credential::ApiKey {
            key: SecretString::from("sk-seed")
        }),
        "the commit keeps the other entries"
    );
    Fixture::assert_private(&fixture.auth_path());

    let (client_id, client_secret) = row_client("google-cloud-code-assist");
    let page = assert_authorize_page(&run.page, client_id);

    assert_token_request(
        &server.requests_to(CLI_TOKEN)[0],
        client_id,
        client_secret,
        page["redirect_uri"].as_str(),
    );

    let bearer = format!("Bearer {ACCESS}");
    let metadata =
        r#""ideType":"IDE_UNSPECIFIED","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI""#;
    let load = &server.requests_to(CLI_LOAD)[0];
    assert_eq!(load.header("authorization"), Some(bearer.as_str()));
    assert_eq!(
        load.header("client-metadata"),
        Some("ideType=IDE_UNSPECIFIED,platform=PLATFORM_UNSPECIFIED,pluginType=GEMINI")
    );
    assert_eq!(
        load.header("user-agent"),
        Some("GeminiCLI/0.46.0/gemini-3.1-pro-preview (linux; x64; terminal)"),
        "the platform comes from the captured identity"
    );
    assert_eq!(load.body, format!(r#"{{"metadata":{{{metadata}}}}}"#));
    let onboard = &server.requests_to(CLI_ONBOARD)[0];
    assert_eq!(
        onboard.body,
        format!(r#"{{"tierId":"free-tier","metadata":{{{metadata}}}}}"#)
    );
    assert_eq!(server.requests_to(CLI_OPERATION).len(), 1);

    let order: Vec<String> = server
        .requests()
        .iter()
        .map(|request| request.path().to_owned())
        .collect();
    assert_eq!(
        order,
        [CLI_TOKEN, CLI_LOAD, CLI_ONBOARD, CLI_OPERATION],
        "no refresh, retry, or model request"
    );
    assert!(
        matches!(
            run.events.as_slice(),
            [LoginProgress::AskPaste { .. }, LoginProgress::Exchanging]
        ),
        "discovery stays under the one Exchanging event: {:?}",
        run.events
    );
    let text = fs::read_to_string(fixture.auth_path()).expect("read auth.json");
    assert!(
        text.contains(r#""project_id":"provisioned-project""#),
        "{text}"
    );
}

#[tokio::test]
async fn cloud_code_assist_login_uses_the_captured_environment_project_for_an_onboarded_account() {
    let fixture = Fixture::new().await;
    let server = Server::start(load_only_server(json(
        r#"{"currentTier":{"id":"standard-tier"}}"#,
    )))
    .await
    .expect("start the test server");
    let env = EnvSnapshot::test(&[("GOOGLE_CLOUD_PROJECT", "env-project")]);
    let site = fixture.site(&server, &env);
    let run = browser_login("google-cloud-code-assist", &site).await;

    let credential = run
        .result
        .expect("the login succeeds on the captured project");
    assert_eq!(
        oauth_of(&credential).project_id.as_deref(),
        Some("env-project")
    );
    assert_eq!(fixture.stored("google-cloud-code-assist"), Some(credential));
    let load = &server.requests_to(CLI_LOAD)[0];
    assert_eq!(
        load.body,
        r#"{"cloudaicompanionProject":"env-project","metadata":{"ideType":"IDE_UNSPECIFIED","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI","duetProject":"env-project"}}"#
    );
    assert!(
        server.requests_to(CLI_ONBOARD).is_empty(),
        "an onboarded account is never onboarded again"
    );
}

#[tokio::test]
async fn an_account_that_needs_a_project_fails_with_the_remedy_and_stores_nothing() {
    let fixture = Fixture::new().await;
    let server = Server::start(load_only_server(json(
        r#"{"currentTier":{"id":"standard-tier"}}"#,
    )))
    .await
    .expect("start the test server");
    let site = fixture.site(&server, &EnvSnapshot::default());
    let run = browser_login("google-cloud-code-assist", &site).await;

    let Err(ProviderError::InvalidRequest { message }) = &run.result else {
        panic!("the missing project must be refused: {:?}", run.result);
    };
    let variables: std::collections::BTreeSet<_> = message
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .collect();
    assert!(variables.contains("GOOGLE_CLOUD_PROJECT"));
    assert!(variables.contains("GOOGLE_CLOUD_PROJECT_ID"));
    let remedy = message
        .split_whitespace()
        .filter_map(|word| reqwest::Url::parse(word.trim_end_matches('.')).ok())
        .find(|url| matches!(url.scheme(), "http" | "https"))
        .expect("the refusal gives a documentation link");
    assert_eq!(remedy.scheme(), "https");
    assert_eq!(remedy.host_str(), Some("geminicli.com"));
    assert!(server.requests_to(CLI_ONBOARD).is_empty());
    fixture.assert_untouched();
}

#[tokio::test]
async fn an_account_verification_refusal_names_the_url_and_action_never_the_account() {
    let fixture = Fixture::new().await;
    let refusal = Reply::Json(
        403,
        String::from(
            r#"{"error":{"code":403,"message":"verify","details":[{"reason":"VALIDATION_REQUIRED","metadata":{"validation_url":"https://accounts.google.com/verify?x=1"}}]}}"#,
        ),
    );
    let server = Server::start(load_only_server(refusal))
        .await
        .expect("start the test server");
    let site = fixture.site(&server, &EnvSnapshot::default());
    let run = browser_login("google-cloud-code-assist", &site).await;

    assert!(
        matches!(
            &run.result,
            Err(ProviderError::Status { status: 403, message, .. })
                if message == "Account verification required. Visit https://accounts.google.com/verify?x=1 to continue, then sign in again."
        ),
        "{:?}",
        run.result
    );
    fixture.assert_untouched();
}

#[tokio::test]
async fn antigravity_login_uses_its_own_client_callback_and_discovery() {
    let fixture = Fixture::new().await;
    let server = Server::start(antigravity_server())
        .await
        .expect("start the test server");
    let site = fixture.site(&server, &EnvSnapshot::default());
    let run = browser_login("google-antigravity", &site).await;

    let credential = run.result.expect("the Antigravity login succeeds");
    let oauth = oauth_of(&credential);
    assert_eq!(oauth.access_token.expose(), ACCESS);
    assert_eq!(oauth.refresh_token.expose(), REFRESH);
    assert_eq!(oauth.project_id.as_deref(), Some("ag-project"));
    assert_eq!(
        fixture.stored("google-antigravity"),
        Some(credential.clone())
    );
    assert_eq!(fixture.stored("google-cloud-code-assist"), None);
    Fixture::assert_private(&fixture.auth_path());

    let (client_id, client_secret) = row_client("google-antigravity");
    let page: HashMap<String, String> = query(&run.page).into_iter().collect();
    assert_eq!(page["client_id"], client_id);
    assert!(
        page["redirect_uri"].ends_with("/oauth-callback"),
        "{}",
        page["redirect_uri"]
    );
    assert!(page["scope"].ends_with(
        "https://www.googleapis.com/auth/cclog https://www.googleapis.com/auth/experimentsandconfigs"
    ));
    let token = &server.requests_to(AG_TOKEN)[0];
    assert_eq!(token.field("client_id").as_deref(), Some(client_id));
    assert_eq!(token.field("client_secret").as_deref(), Some(client_secret));
    assert_eq!(token.field("code_verifier"), None);

    let loads = server.requests_to(AG_LOAD);
    assert_eq!(loads.len(), 2, "the pinned load, then the project re-read");
    for load in &loads {
        assert_eq!(load.body, r#"{"metadata":{"ideType":"ANTIGRAVITY"}}"#);
        assert_eq!(
            load.header("user-agent"),
            Some(antigravity::user_agent(antigravity::CLIENT_VERSION).as_str())
        );
        assert_eq!(load.header("client-metadata"), None);
    }
    assert!(
        server
            .requests_to("/antigravity/v1internal:onboardUser")
            .is_empty()
    );
}

#[tokio::test]
async fn cancelling_during_project_discovery_stops_the_login_and_stores_nothing() {
    let fixture = Fixture::new().await;
    let mut server = Server::start(load_only_server(Reply::Hold))
        .await
        .expect("start the test server");
    let site = fixture.site(&server, &EnvSnapshot::default());
    let cancel = CancellationToken::new();
    let (io, mut events) = LoginIo::channel(None, cancel.clone());
    let driver = async {
        let page = open_url(&mut events).await;
        let pairs: HashMap<String, String> = query(&page).into_iter().collect();
        let _paste = next_event(&mut events).await;
        let _callback = visit(
            &pairs["redirect_uri"],
            &format!("code=the-code&state={}", pairs["state"]),
        )
        .await;
        let held = server.next_request_to(CLI_LOAD).await;
        assert_eq!(held.method, "POST");
        cancel.cancel();
    };
    let (result, ()) = tokio::join!(
        timeout(
            GUARD,
            login("google-cloud-code-assist", Method::Browser, io, &site)
        ),
        driver
    );
    let result = result.expect("the login finished within the guard");
    assert!(
        matches!(result, Err(ProviderError::LoginCancelled)),
        "{result:?}"
    );
    fixture.assert_untouched();
}

#[tokio::test]
async fn a_flow_given_no_platform_identity_refuses_before_the_browser_opens() {
    let fixture = Fixture::new().await;
    let server = Server::start(onboarding_server())
        .await
        .expect("start the test server");
    let endpoints = LoginEndpoints::loopback(server.base(), &EnvSnapshot::default())
        .expect("loopback endpoints");
    let mut store = AuthStore::load(fixture.auth_path()).expect("load auth.json");
    let mut flow = crate::LoginFlow::new(
        "google-cloud-code-assist",
        &mut store,
        reqwest::Client::new(),
        "dal/test",
        fixture.dir.0.join("cache"),
        endpoints,
    )
    .expect("the flow builds");
    let events = std::sync::Mutex::new(Vec::new());
    let report = |event: LoginProgress| events.lock().expect("events").push(event);
    let result = flow.run(&report, &CancellationToken::new()).await;

    assert!(
        matches!(
            &result,
            Err(ProviderError::AuthWrite { reason }) if reason.contains("platform identity")
        ),
        "{result:?}"
    );
    assert!(
        events.lock().expect("events").is_empty(),
        "no authorize page was reported"
    );
    assert!(server.requests().is_empty());
    fixture.assert_untouched();
}

/// Seeds `auth.json` with a signed-in Cloud Code Assist credential and returns the
/// stored value.
fn seed_cloud_code(fixture: &Fixture, provider: &str, project: &str) -> OAuthCredential {
    let credential = OAuthCredential {
        access_token: SecretString::from("ya29.old-access"),
        refresh_token: SecretString::from("1//old-refresh"),
        expires_at: Some(unix_now() + 10),
        id_token: None,
        account_id: None,
        project_id: Some(project.to_owned()),
    };
    let mut store = AuthStore::load(fixture.auth_path()).expect("load auth.json");
    store
        .set(provider, Credential::OAuth(credential.clone()))
        .expect("the row takes a project credential");
    store.store().expect("save auth.json");
    credential
}

fn refresher(fixture: &Fixture, server: &Server, provider: &str, prefix: &str) -> Refresher {
    let def = find(provider).expect("the row");
    let endpoints = TokenEndpoints::production()
        .with_base(def, &format!("{}{prefix}", server.base()))
        .expect("loopback token endpoint");
    Refresher::new(
        "dal/test",
        fixture.auth_path(),
        endpoints,
        &EnvSnapshot::default(),
    )
}

#[tokio::test]
async fn a_refresh_sends_the_client_secret_and_keeps_the_stored_project_and_refresh_token() {
    for (provider, prefix, token_path, project) in [
        (
            "google-cloud-code-assist",
            "/cloud-code-assist",
            CLI_TOKEN,
            "stored-cli-project",
        ),
        (
            "google-antigravity",
            "/antigravity",
            AG_TOKEN,
            "stored-ag-project",
        ),
    ] {
        let fixture = Fixture::new().await;
        let held = seed_cloud_code(&fixture, provider, project);
        let server = Server::start(move |request: &Request| {
            if request.path() == token_path {
                json(
                    r#"{"access_token":"ya29.new-access","expires_in":3600,"token_type":"Bearer"}"#,
                )
            } else {
                not_found()
            }
        })
        .await
        .expect("start the test server");
        let def = find(provider).expect("the row");
        let before = unix_now();
        let refresher = refresher(&fixture, &server, provider, prefix);
        let granted = refresher
            .refresh(def, &held, RefreshReason::Rejected)
            .await
            .expect("the refresh succeeds");
        refresher.shutdown().await;
        let after = unix_now();

        let oauth = oauth_of(&granted);
        assert_eq!(oauth.access_token.expose(), "ya29.new-access", "{provider}");
        assert_eq!(
            oauth.refresh_token.expose(),
            "1//old-refresh",
            "{provider}: Google never rotates the refresh token"
        );
        assert_eq!(oauth.project_id.as_deref(), Some(project), "{provider}");
        assert!(
            oauth
                .expires_at
                .is_some_and(|at| (before + 3600..=after + 3600).contains(&at)),
            "{provider}"
        );
        assert_eq!(
            fixture.stored(provider),
            Some(granted.clone()),
            "{provider}"
        );

        let (client_id, client_secret) = row_client(provider);
        let request = &server.requests()[0];
        assert_eq!(request.path(), token_path, "{provider}");
        assert_eq!(
            request.header("content-type"),
            Some("application/x-www-form-urlencoded"),
            "{provider}"
        );
        assert_eq!(
            request.form(),
            [
                ("grant_type", "refresh_token"),
                ("client_id", client_id),
                ("client_secret", client_secret),
                ("refresh_token", "1//old-refresh"),
            ]
            .map(|(name, value)| (name.to_owned(), value.to_owned())),
            "{provider}: the reference's declarative refresh body"
        );
        assert_eq!(
            server.requests().len(),
            1,
            "{provider}: no discovery on refresh"
        );
    }
}

#[tokio::test]
async fn a_refresh_that_rotates_the_refresh_token_still_keeps_the_project() {
    let fixture = Fixture::new().await;
    let held = seed_cloud_code(&fixture, "google-cloud-code-assist", "stored-cli-project");
    let server = Server::start(|request: &Request| {
        if request.path() == CLI_TOKEN {
            json(r#"{"access_token":"ya29.new-access","refresh_token":"1//rotated","expires_in":60}"#)
        } else {
            not_found()
        }
    })
    .await
    .expect("start the test server");
    let def = find("google-cloud-code-assist").expect("the row");
    let refresher = refresher(
        &fixture,
        &server,
        "google-cloud-code-assist",
        "/cloud-code-assist",
    );
    let granted = refresher
        .refresh(def, &held, RefreshReason::Rejected)
        .await
        .expect("the refresh succeeds");
    refresher.shutdown().await;
    let oauth = oauth_of(&granted);
    assert_eq!(oauth.refresh_token.expose(), "1//rotated");
    assert_eq!(oauth.project_id.as_deref(), Some("stored-cli-project"));
    assert_eq!(fixture.stored("google-cloud-code-assist"), Some(granted));
}
