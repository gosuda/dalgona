//! ENG-01 shared-contract regression tests for the host seam.
//!
//! Proves call identity (`LoginId`), terminal outcomes (exactly one
//! `LoginFinished` per completed login), cancellation ordering, and
//! credential-ownership boundaries through `engine/dal-agent/src/host/`.
//! Approval enforcement lives in session dispatch and core `approval::gate`,
//! outside host/ file scope; this suite does not claim it (see report E4).

#![expect(
    clippy::expect_used,
    reason = "fixture setup must fail at its boundary"
)]

use std::time::Duration;

use dal_agent::login::{CancellationToken, LoginId, LoginIo, Method};
use dal_agent::{Env, Host, HostUpdate, Product};
use dal_core::{Config, ConfigProduct};
use dal_provider::ProviderError;
use tokio::sync::oneshot;

fn test_product(dir: &std::path::Path) -> Product {
    Product {
        name: "dal",
        data_root: dir.to_path_buf(),
        defaults: "",
        extensions: Vec::new(),
        bundled: Vec::new(),
    }
}

async fn test_host(dir: &std::path::Path) -> Host {
    let config = Config::load(ConfigProduct::Dalgon, dir, "", None).expect("config loads");
    Host::start(test_product(dir), config, Env::data_root(dir.to_path_buf()))
        .await
        .expect("host starts")
}

async fn finished_updates(subscription: &mut dal_agent::HostSubscription) -> Vec<HostUpdate> {
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

#[test]
fn login_ids_are_stable_call_identity() {
    assert_eq!(LoginId::new(1).get(), 1);
    assert_eq!(LoginId::new(41).get(), 41);
    assert_ne!(LoginId::new(1), LoginId::new(2));
}

#[tokio::test]
async fn register_cancel_finish_follow_lifecycle_ordering() {
    let dir = tempfile::tempdir().expect("tempdir");
    let host = test_host(dir.path()).await;

    let (first, _) = host.register_login();
    assert!(host.cancel_login(first));
    assert!(!host.cancel_login(first));

    let (second, _) = host.register_login();
    assert_ne!(first, second);
    host.finish_login(second);
    assert!(!host.cancel_login(second));

    assert!(!host.cancel_login(LoginId::new(999_999)));
}

#[tokio::test]
async fn failed_login_publishes_exactly_one_terminal_update_naming_its_attempt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let host = test_host(dir.path()).await;
    let mut subscription = host.subscribe();
    let (login, _) = host.register_login();
    let (io, _events) = LoginIo::channel(None, CancellationToken::new());
    let outcome = host.login_as(login, "openai", Method::Browser, io).await;
    assert!(matches!(
        outcome,
        Err(dal_agent::HostError::Provider(
            ProviderError::LoginInput { .. }
        ))
    ));
    assert!(!dir.path().join("auth.json").exists());
    let updates = finished_updates(&mut subscription).await;
    assert_eq!(updates.len(), 1);
    let HostUpdate::LoginFinished {
        login: back, ready, ..
    } = &updates[0]
    else {
        panic!("expected LoginFinished");
    };
    assert_eq!(*back, login);
    assert!(!ready);
    host.finish_login(login);
}

#[tokio::test]
async fn api_key_login_stores_credential_and_reports_no_secrets() {
    let dir = tempfile::tempdir().expect("tempdir");
    let host = test_host(dir.path()).await;
    let mut subscription = host.subscribe();
    let (key, pasted) = oneshot::channel();
    key.send(String::from("sk-eng01-proof")).expect("send key");
    let (io, _events) = LoginIo::channel(Some(pasted), CancellationToken::new());
    let outcome = host
        .login("openai", Method::ApiKey, io)
        .await
        .expect("api-key login stores");
    assert_eq!(outcome.account, None);
    assert!(dir.path().join("auth.json").exists());
    let stored = host.stored_credentials().await.expect("lists credentials");
    assert_eq!(stored.len(), 1);
    assert_eq!(&*stored[0].provider, "openai");
    for entry in &stored {
        let rendered = format!("{entry:?}");
        assert!(
            !rendered.contains("sk-eng01-proof"),
            "stored credential leaks secret: {rendered}"
        );
    }
    assert_eq!(finished_updates(&mut subscription).await.len(), 1);
}

#[tokio::test]
async fn logout_without_a_stored_credential_lists_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let host = test_host(dir.path()).await;
    let removed = host.logout(Some("openai")).await.expect("logout runs");
    assert_eq!(removed.len(), 0);
    let stored = host.stored_credentials().await.expect("lists credentials");
    assert_eq!(stored.len(), 0);
}

#[tokio::test]
async fn logout_removes_only_the_named_entry_and_keeps_env_untouched() {
    let dir = tempfile::tempdir().expect("tempdir");
    let host = test_host(dir.path()).await;
    let (key, pasted) = oneshot::channel();
    key.send(String::from("sk-eng01-proof")).expect("send key");
    let (io, _events) = LoginIo::channel(Some(pasted), CancellationToken::new());
    host.login("openai", Method::ApiKey, io)
        .await
        .expect("api-key login stores");
    let removed = host.logout(Some("openai")).await.expect("logout runs");
    assert_eq!(removed.len(), 1);
    let stored = host.stored_credentials().await.expect("lists credentials");
    assert_eq!(stored.len(), 0);
}
