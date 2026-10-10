//! The host sign-in seam against a loopback OAuth server: real flows, real
//! `auth.json` files, no mock of the code under test.

use std::path::Path;
use std::time::Duration;

use dal_core::{Config, ConfigProduct};
use dal_provider::{
    AuthStore, Credential, CredentialKind, LoginIo, LoginProgress, Method, OAuthCredential,
    PASTE_HINT, ProviderError, SecretString,
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::error::HostError;
use crate::host::LoginId;
use crate::login_fake::{
    ACCESS_TOKEN, ACCOUNT_ID, FakeOAuth, REFRESH_TOKEN, TokenReply, USER_CODE, follow_authorize_url,
};
use crate::{Env, Host, HostSubscription, HostUpdate, Product};

async fn host_in(dir: &Path, server: &FakeOAuth) -> Host {
    let config = Config::load(ConfigProduct::Dalgon, dir, "", None).expect("config");
    let product = Product {
        name: "dal",
        data_root: dir.to_path_buf(),
        defaults: "",
        extensions: Vec::new(),
        bundled: Vec::new(),
    };
    let host = Host::start(product, config, Env::data_root(dir.to_path_buf()))
        .await
        .expect("host");
    host.set_login_endpoints(server.endpoints(0, 0).expect("endpoints"))
        .expect("loopback endpoints");
    host
}

fn auth_json(dir: &Path) -> std::path::PathBuf {
    dir.join("auth.json")
}

async fn finished(subscription: &mut HostSubscription) -> Vec<HostUpdate> {
    let mut updates = Vec::new();
    while let Ok(Some(update)) =
        tokio::time::timeout(Duration::from_millis(200), subscription.next()).await
    {
        if matches!(update, HostUpdate::LoginFinished { .. }) {
            updates.push(update);
        }
    }
    updates
}

fn oauth(expires_at: Option<i64>) -> Credential {
    Credential::OAuth(OAuthCredential {
        access_token: SecretString::from("stored-access"),
        refresh_token: SecretString::from("stored-refresh"),
        expires_at,
        id_token: Some(String::from("stored-id")),
        account_id: Some(String::from("stored-account")),
    })
}

#[tokio::test]
async fn codex_browser_login_completes_on_the_loopback_callback() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = FakeOAuth::start(TokenReply::Issue).await.expect("server");
    let host = host_in(dir.path(), &server).await;
    let mut subscription = host.subscribe();
    let (io, mut events) = LoginIo::channel(None, CancellationToken::new());
    let browser = async {
        let mut seen = Vec::new();
        while let Some(event) = events.recv().await {
            if let LoginProgress::OpenUrl { url } = &event {
                let page = follow_authorize_url(url, "auth-code")
                    .await
                    .expect("callback");
                assert!(page.contains("Sign-in complete"), "{page}");
            }
            seen.push(event);
        }
        seen
    };
    let (outcome, seen) = tokio::join!(host.login("openai-codex", Method::Browser, io), browser);
    let outcome = outcome.expect("login");
    assert_eq!(&*outcome.provider, "openai-codex");
    assert_eq!(outcome.account.as_deref(), Some(ACCOUNT_ID));
    assert!(matches!(
        seen.as_slice(),
        [LoginProgress::OpenUrl { url }, LoginProgress::Exchanging]
            if url.contains("/oauth/authorize") && url.contains("code_challenge_method=S256")
    ));
    let stored = AuthStore::load(auth_json(dir.path())).expect("store");
    assert!(matches!(
        stored.credential("openai-codex"),
        Some(Credential::OAuth(oauth))
            if oauth.access_token.expose() == ACCESS_TOKEN
                && oauth.refresh_token.expose() == REFRESH_TOKEN
                && oauth.account_id.as_deref() == Some(ACCOUNT_ID)
    ));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(auth_json(dir.path()))
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let token = server.requests_to("/oauth/token");
    assert_eq!(token.len(), 1);
    assert!(
        token[0].body.contains("code=auth-code"),
        "{}",
        token[0].body
    );
    assert_eq!(
        finished(&mut subscription).await,
        [HostUpdate::LoginFinished {
            login: LoginId::new(1),
            provider: "openai-codex".into(),
            ready: true,
            detail: None,
        }]
    );
}

#[tokio::test]
async fn claude_paste_completes_the_login() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = FakeOAuth::start(TokenReply::Issue).await.expect("server");
    let host = host_in(dir.path(), &server).await;
    let mut subscription = host.subscribe();
    let (paste, pasted) = oneshot::channel();
    let (io, mut events) = LoginIo::channel(Some(pasted), CancellationToken::new());
    let mut paste = Some(paste);
    let user = async {
        let mut seen = Vec::new();
        while let Some(event) = events.recv().await {
            if matches!(event, LoginProgress::AskPaste { .. })
                && let Some(paste) = paste.take()
            {
                paste.send(String::from("pasted-code")).expect("paste");
            }
            seen.push(event);
        }
        seen
    };
    let (outcome, seen) = tokio::join!(host.login("anthropic", Method::Browser, io), user);
    assert_eq!(outcome.expect("login").account, None);
    assert!(matches!(
        seen.as_slice(),
        [
            LoginProgress::OpenUrl { url },
            LoginProgress::AskPaste { hint },
            LoginProgress::Exchanging,
        ] if url.contains("/claude/oauth/authorize") && hint == PASTE_HINT
    ));
    let token = server.requests_to("/claude/v1/oauth/token");
    assert_eq!(token.len(), 1);
    assert!(token[0].body.contains(r#""code":"pasted-code""#));
    let stored = AuthStore::load(auth_json(dir.path())).expect("store");
    assert!(matches!(
        stored.credential("anthropic"),
        Some(Credential::OAuth(oauth)) if oauth.access_token.expose() == ACCESS_TOKEN
    ));
    assert_eq!(finished(&mut subscription).await.len(), 1);
}

#[tokio::test]
async fn codex_device_login_shows_the_code_then_stores_the_credential() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = FakeOAuth::start(TokenReply::Issue).await.expect("server");
    let host = host_in(dir.path(), &server).await;
    let (io, mut events) = LoginIo::channel(None, CancellationToken::new());
    let login = host.login("openai-codex", Method::Device, io);
    let collect = async {
        let mut seen = Vec::new();
        while let Some(event) = events.recv().await {
            seen.push(event);
        }
        seen
    };
    let (outcome, seen) = tokio::join!(login, collect);
    assert_eq!(outcome.expect("login").account.as_deref(), Some(ACCOUNT_ID));
    assert!(matches!(
        seen.as_slice(),
        [LoginProgress::ShowCode { url, code }, LoginProgress::Exchanging]
            if url.ends_with("/codex/device") && code == USER_CODE
    ));
    let exchange = server.requests_to("/oauth/token");
    assert_eq!(exchange.len(), 1);
    assert!(exchange[0].body.contains("code=device-code"));
    assert!(exchange[0].body.contains("code_verifier=device-verifier"));
    assert!(auth_json(dir.path()).is_file());
}

#[tokio::test]
async fn cancelling_a_pending_login_reports_cancellation_and_stores_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = FakeOAuth::start(TokenReply::Issue).await.expect("server");
    let host = host_in(dir.path(), &server).await;
    let mut subscription = host.subscribe();
    let cancel = CancellationToken::new();
    let (io, mut events) = LoginIo::channel(None, cancel.clone());
    let user = async {
        while let Some(event) = events.recv().await {
            if matches!(event, LoginProgress::OpenUrl { .. }) {
                cancel.cancel();
            }
        }
    };
    let (outcome, ()) = tokio::join!(host.login("openai-codex", Method::Browser, io), user);
    assert!(matches!(
        outcome,
        Err(HostError::Provider(ProviderError::LoginCancelled))
    ));
    assert!(!auth_json(dir.path()).exists());
    assert_eq!(server.requests_to("/oauth/token").len(), 0);
    assert_eq!(
        finished(&mut subscription).await,
        [HostUpdate::LoginFinished {
            login: LoginId::new(1),
            provider: "openai-codex".into(),
            ready: false,
            detail: Some("sign-in cancelled.".into()),
        }]
    );
}

#[tokio::test(start_paused = true)]
async fn a_login_nobody_completes_times_out_after_fifteen_minutes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = FakeOAuth::start(TokenReply::Issue).await.expect("server");
    let host = host_in(dir.path(), &server).await;
    let mut subscription = host.subscribe();
    let (io, mut events) = LoginIo::channel(None, CancellationToken::new());
    let idle = async { while events.recv().await.is_some() {} };
    let started = tokio::time::Instant::now();
    let (outcome, ()) = tokio::join!(host.login("openai-codex", Method::Browser, io), idle);
    assert!(matches!(
        outcome,
        Err(HostError::Provider(ProviderError::LoginTimeout))
    ));
    assert_eq!(started.elapsed(), Duration::from_mins(15));
    assert!(!auth_json(dir.path()).exists());
    assert_eq!(
        finished(&mut subscription).await,
        [HostUpdate::LoginFinished {
            login: LoginId::new(1),
            provider: "openai-codex".into(),
            ready: false,
            detail: Some("sign-in timed out after 15 minutes.".into()),
        }]
    );
}

#[tokio::test]
async fn a_rejected_exchange_reports_the_token_error_and_stores_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = FakeOAuth::start(TokenReply::Reject).await.expect("server");
    let host = host_in(dir.path(), &server).await;
    let mut subscription = host.subscribe();
    let (io, mut events) = LoginIo::channel(None, CancellationToken::new());
    let browser = async {
        while let Some(event) = events.recv().await {
            if let LoginProgress::OpenUrl { url } = &event {
                follow_authorize_url(url, "auth-code")
                    .await
                    .expect("callback");
            }
        }
    };
    let (outcome, ()) = tokio::join!(host.login("openai-codex", Method::Browser, io), browser);
    let error = outcome.expect_err("the exchange is rejected");
    assert!(matches!(
        &error,
        HostError::Provider(ProviderError::TokenExchange { status: 400, .. })
    ));
    let text = error.to_string();
    assert!(
        text.starts_with("sign-in failed: the token endpoint returned 400: "),
        "{text}"
    );
    assert!(!text.contains("auth-code"));
    assert!(!auth_json(dir.path()).exists());
    assert_eq!(
        finished(&mut subscription).await,
        [HostUpdate::LoginFinished {
            login: LoginId::new(1),
            provider: "openai-codex".into(),
            ready: false,
            detail: Some(text.into()),
        }]
    );
}

#[tokio::test]
async fn an_api_key_arrives_on_the_paste_channel_and_is_stored() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = FakeOAuth::start(TokenReply::Issue).await.expect("server");
    let host = host_in(dir.path(), &server).await;
    let mut subscription = host.subscribe();
    let (key, pasted) = oneshot::channel();
    key.send(String::from("sk-fake")).expect("send key");
    let (io, _events) = LoginIo::channel(Some(pasted), CancellationToken::new());
    let outcome = host
        .login("openai", Method::ApiKey, io)
        .await
        .expect("login");
    assert_eq!(outcome.account, None);
    let stored = host.stored_credentials().await.expect("credentials");
    assert_eq!(
        stored
            .iter()
            .map(|row| (&*row.provider, row.kind, row.expired))
            .collect::<Vec<_>>(),
        [("openai", CredentialKind::ApiKey, false)]
    );
    assert_eq!(finished(&mut subscription).await.len(), 1);
}

#[tokio::test]
async fn an_unsupported_method_fails_once_and_writes_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = FakeOAuth::start(TokenReply::Issue).await.expect("server");
    let host = host_in(dir.path(), &server).await;
    let mut subscription = host.subscribe();
    let (io, _events) = LoginIo::channel(None, CancellationToken::new());
    let outcome = host.login("openai", Method::Browser, io).await;
    assert!(matches!(
        outcome,
        Err(HostError::Provider(ProviderError::LoginInput { .. }))
    ));
    assert!(!auth_json(dir.path()).exists());
    assert!(matches!(
        finished(&mut subscription).await.as_slice(),
        [HostUpdate::LoginFinished { ready: false, .. }]
    ));
}

#[tokio::test]
async fn each_login_names_its_own_attempt_in_its_finished_update() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = FakeOAuth::start(TokenReply::Issue).await.expect("server");
    let host = host_in(dir.path(), &server).await;
    let mut subscription = host.subscribe();
    let (io, _events) = LoginIo::channel(None, CancellationToken::new());
    let _ = host.login("openai", Method::Browser, io).await;
    let (io, _events) = LoginIo::channel(None, CancellationToken::new());
    let _ = host
        .login_as(LoginId::new(77), "openai", Method::Browser, io)
        .await;
    let logins: Vec<LoginId> = finished(&mut subscription)
        .await
        .into_iter()
        .map(|update| match update {
            HostUpdate::LoginFinished { login, .. } => login,
            other => panic!("not a login update: {other:?}"),
        })
        .collect();
    assert_eq!(logins, [LoginId::new(1), LoginId::new(77)]);
}

#[tokio::test]
async fn logout_revokes_a_codex_refresh_token_and_removes_only_that_entry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = FakeOAuth::start(TokenReply::Issue).await.expect("server");
    let host = host_in(dir.path(), &server).await;
    let mut store = AuthStore::empty(auth_json(dir.path()));
    store.set("openai-codex", oauth(None)).expect("codex");
    store
        .set(
            "openai",
            Credential::ApiKey {
                key: SecretString::from("sk-keep"),
            },
        )
        .expect("openai");
    store.store().expect("store");

    let removed = host.logout(Some("openai-codex")).await.expect("logout");
    assert_eq!(removed, [Box::<str>::from("openai-codex")]);
    let revoke = server.requests_to("/oauth/revoke");
    assert_eq!(revoke.len(), 1);
    assert!(revoke[0].body.contains("stored-refresh"));
    let after = AuthStore::load(auth_json(dir.path())).expect("store");
    assert!(after.credential("openai-codex").is_none());
    assert!(after.credential("openai").is_some());

    let again = host.logout(Some("openai-codex")).await.expect("repeat");
    assert_eq!(again.len(), 0);
    assert_eq!(server.requests_to("/oauth/revoke").len(), 1);

    let all = host.logout(None).await.expect("all");
    assert_eq!(all, [Box::<str>::from("openai")]);
    assert!(!auth_json(dir.path()).exists());
}

#[tokio::test]
async fn stored_credentials_report_an_expiring_oauth_entry_as_expired() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = FakeOAuth::start(TokenReply::Issue).await.expect("server");
    let host = host_in(dir.path(), &server).await;
    let mut store = AuthStore::empty(auth_json(dir.path()));
    store
        .set("openai-codex", oauth(Some(1)))
        .expect("openai-codex");
    store.store().expect("store");
    let rows = host.stored_credentials().await.expect("rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(&*rows[0].provider, "openai-codex");
    assert!(rows[0].expired);
}
