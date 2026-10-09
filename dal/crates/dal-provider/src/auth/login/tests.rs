use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU32, Ordering},
};

mod oauth_login;
mod server;

use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::{LoginIo, LoginSite, Method, login, login_providers, sign_out, stored_credentials};
use crate::{AuthStore, Credential, CredentialKind, OAuthCredential, ProviderError, SecretString};

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "dal-provider-login-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _removed = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create test dir");
        Self(path)
    }

    fn site(&self) -> LoginSite {
        LoginSite::new(self.0.join("auth.json"), self.0.join("cache"), "dal/test")
            .expect("production endpoints")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _removed = fs::remove_dir_all(&self.0);
    }
}

fn api_key_io(key: &str, cancel: CancellationToken) -> LoginIo {
    let (sender, receiver) = oneshot::channel();
    sender.send(key.to_owned()).expect("receiver is alive");
    LoginIo::channel(Some(receiver), cancel).0
}

#[test]
fn the_provider_list_names_the_methods_each_provider_offers() {
    let listed: Vec<_> = login_providers()
        .iter()
        .map(|def| (def.id, def.methods().collect::<Vec<_>>()))
        .collect();
    assert_eq!(
        listed,
        [
            ("anthropic", vec![Method::ApiKey, Method::Browser]),
            ("openai", vec![Method::ApiKey]),
            ("openai-codex", vec![Method::Browser, Method::Device]),
        ]
    );
}

#[tokio::test]
async fn a_method_the_provider_does_not_offer_is_refused_before_any_write() {
    let dir = TestDir::new();
    for (provider, method) in [
        ("openai", Method::Browser),
        ("openai-codex", Method::ApiKey),
        ("anthropic", Method::Device),
        ("nobody", Method::ApiKey),
    ] {
        let io = api_key_io("sk-test", CancellationToken::new());
        let result = login(provider, method, io, &dir.site()).await;
        assert!(
            matches!(&result, Err(ProviderError::LoginInput { reason }) if reason.contains(method.as_str())),
            "{provider} {method}: {result:?}"
        );
    }
    assert!(!dir.0.join("auth.json").exists());
}

#[tokio::test]
async fn an_api_key_is_stored_with_mode_0600_and_returned() {
    let dir = TestDir::new();
    let io = api_key_io("  sk-test \n", CancellationToken::new());
    let credential = login("openai", Method::ApiKey, io, &dir.site())
        .await
        .expect("login");
    assert_eq!(
        credential,
        Credential::ApiKey {
            key: SecretString::from("sk-test")
        }
    );
    let store = AuthStore::load(dir.0.join("auth.json")).expect("load");
    assert_eq!(store.credential("openai"), Some(credential));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(dir.0.join("auth.json"))
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[tokio::test]
async fn an_empty_api_key_is_refused_and_writes_nothing() {
    let dir = TestDir::new();
    let io = api_key_io("   ", CancellationToken::new());
    let result = login("openai", Method::ApiKey, io, &dir.site()).await;
    assert!(matches!(result, Err(ProviderError::LoginInput { .. })));
    assert!(!dir.0.join("auth.json").exists());
}

#[tokio::test]
async fn cancelling_while_the_key_is_awaited_reports_cancellation_and_writes_nothing() {
    let dir = TestDir::new();
    let cancel = CancellationToken::new();
    let (_keep_open, receiver) = oneshot::channel::<String>();
    let io = LoginIo::channel(Some(receiver), cancel.clone()).0;
    cancel.cancel();
    let result = login("anthropic", Method::ApiKey, io, &dir.site()).await;
    assert!(matches!(result, Err(ProviderError::LoginCancelled)));
    assert!(!dir.0.join("auth.json").exists());
}

#[tokio::test]
async fn a_dropped_key_sender_is_a_cancellation() {
    let dir = TestDir::new();
    let (sender, receiver) = oneshot::channel::<String>();
    drop(sender);
    let io = LoginIo::channel(Some(receiver), CancellationToken::new()).0;
    let result = login("openai", Method::ApiKey, io, &dir.site()).await;
    assert!(matches!(result, Err(ProviderError::LoginCancelled)));
}

fn oauth(expires_at: Option<i64>) -> Credential {
    Credential::OAuth(OAuthCredential {
        access_token: SecretString::from("access"),
        refresh_token: SecretString::from("refresh"),
        expires_at,
        id_token: None,
        account_id: None,
    })
}

#[test]
fn stored_credentials_flag_oauth_inside_the_refresh_window() {
    let dir = TestDir::new();
    let now = super::unix_now();
    let mut store = AuthStore::empty(dir.0.join("auth.json"));
    store
        .set("anthropic", oauth(Some(now + 299)))
        .expect("set anthropic");
    store
        .set(
            "openai",
            Credential::ApiKey {
                key: SecretString::from("sk"),
            },
        )
        .expect("set openai");
    store.store().expect("store");
    let rows = stored_credentials(&dir.site()).expect("rows");
    let summary: Vec<_> = rows
        .iter()
        .map(|row| (&*row.provider, row.kind, row.expired))
        .collect();
    assert_eq!(
        summary,
        [
            ("openai", CredentialKind::ApiKey, false),
            ("anthropic", CredentialKind::OAuth, true),
        ]
    );

    let mut fresh = AuthStore::empty(dir.0.join("auth.json"));
    fresh
        .set("anthropic", oauth(Some(now + 3600)))
        .expect("set fresh");
    fresh.store().expect("store fresh");
    assert!(
        !stored_credentials(&dir.site())
            .expect("rows")
            .iter()
            .any(|row| row.expired)
    );
}

#[tokio::test]
async fn sign_out_removes_one_or_all_and_lists_only_stored_providers() {
    let dir = TestDir::new();
    let mut store = AuthStore::empty(dir.0.join("auth.json"));
    store.set("anthropic", oauth(None)).expect("set anthropic");
    store
        .set(
            "openai",
            Credential::ApiKey {
                key: SecretString::from("sk"),
            },
        )
        .expect("set openai");
    store.store().expect("store");

    assert_eq!(
        sign_out(Some("openai-codex"), &dir.site())
            .await
            .expect("absent"),
        [] as [std::boxed::Box<str>; 0]
    );
    let removed = sign_out(Some("openai"), &dir.site()).await.expect("one");
    assert_eq!(removed, [Box::<str>::from("openai")]);
    let removed = sign_out(None, &dir.site()).await.expect("all");
    assert_eq!(removed, [Box::<str>::from("anthropic")]);
    assert!(!dir.0.join("auth.json").exists());
    assert_eq!(
        sign_out(None, &dir.site()).await.expect("empty"),
        [] as [std::boxed::Box<str>; 0]
    );
}
