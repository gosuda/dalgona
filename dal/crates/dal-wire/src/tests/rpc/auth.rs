//! `auth/status`, `auth/login`, and `auth/logout` over the real RPC loop and
//! the real remote client, against a loopback fake OAuth server.

use std::time::Duration;

use dal_agent::login_fake::{FakeOAuth, TokenReply, USER_CODE, follow_authorize_url};
use dal_provider::{AuthStore, Credential, OAuthCredential, SecretString};
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};

use super::super::support::{Rig, Rpc, assert_error, initialize, result, rig};
use super::with_rpc;

async fn fake_rig(reply: TokenReply) -> (Rig, FakeOAuth) {
    let rig = rig(&[]).await;
    let fake = FakeOAuth::start(reply).await.expect("fake oauth server");
    rig.host
        .set_login_endpoints(fake.endpoints(0, 0).expect("endpoints"))
        .expect("loopback endpoints");
    (rig, fake)
}

fn oauth(expires_at: i64, with_identity: bool) -> Credential {
    Credential::OAuth(OAuthCredential {
        access_token: SecretString::from("stored-access"),
        refresh_token: SecretString::from("stored-refresh"),
        expires_at: Some(expires_at),
        id_token: with_identity.then(|| String::from("stored-id")),
        account_id: with_identity.then(|| String::from("stored-account")),
    })
}

/// An expiry (year 2096) no refresh window reaches during a test.
const FAR_FUTURE: i64 = 4_000_000_000;

fn store(rig: &Rig, entries: &[(&str, Credential)]) {
    let mut store = AuthStore::empty(rig.data().join("auth.json"));
    for (provider, credential) in entries {
        store.set(provider, credential.clone()).expect("set");
    }
    store.store().expect("store");
}

/// Reads frames until the `login_finished` host update and returns its body.
async fn login_finished(rpc: &mut Rpc) -> Value {
    loop {
        let frame = rpc.next().await;
        if frame["method"].as_str() == Some("host/update")
            && frame["params"]["update"]["type"].as_str() == Some("login_finished")
        {
            return frame["params"]["update"].clone();
        }
    }
}

#[tokio::test]
async fn a_browser_login_returns_pending_then_the_ready_host_update() {
    let (rig, fake) = fake_rig(TokenReply::Issue).await;
    let auth = rig.data().join("auth.json");
    with_rpc(&rig, |mut rpc| async move {
        initialize(&mut rpc).await;
        rpc.call(1, "host/subscribe", sonic_rs::json!({})).await;
        let reply = rpc
            .call(
                2,
                "auth/login",
                sonic_rs::json!({"provider": "openai-codex", "method": "browser"}),
            )
            .await;
        let pending = result(&reply);
        assert_eq!(pending["state"].as_str(), Some("pending"));
        assert!(pending.get("userCode").is_none(), "{pending}");
        let url = pending["url"].as_str().expect("authorize url").to_owned();
        assert!(url.contains("/oauth/authorize"), "{url}");
        assert!(!auth.exists(), "nothing is stored before the callback");

        let page = follow_authorize_url(&url, "auth-code")
            .await
            .expect("callback");
        assert!(page.contains("Sign-in complete"), "{page}");
        assert_eq!(
            login_finished(&mut rpc).await,
            sonic_rs::json!({"type": "login_finished", "provider": "openai-codex", "state": "ready"})
        );
        rpc.assert_quiet(Duration::from_millis(300)).await;
        assert!(auth.is_file());
        assert_eq!(fake.requests_to("/oauth/token").len(), 1);

        let status = rpc.call(3, "auth/status", sonic_rs::json!({})).await;
        let rows = result(&status)["providers"].as_array().expect("rows");
        let row = rows
            .iter()
            .find(|row| row["provider"].as_str() == Some("openai-codex"))
            .expect("codex row");
        assert_eq!(row["state"].as_str(), Some("ready"));
        assert_eq!(row["detail"].as_str(), Some("oauth"));
    })
    .await;
}

#[tokio::test]
async fn a_device_login_returns_the_user_code_then_the_ready_host_update() {
    let (rig, _fake) = fake_rig(TokenReply::Issue).await;
    with_rpc(&rig, |mut rpc| async move {
        initialize(&mut rpc).await;
        rpc.call(1, "host/subscribe", sonic_rs::json!({})).await;
        let reply = rpc
            .call(
                2,
                "auth/login",
                sonic_rs::json!({"provider": "openai-codex", "method": "device"}),
            )
            .await;
        let pending = result(&reply);
        assert_eq!(pending["state"].as_str(), Some("pending"));
        assert_eq!(pending["userCode"].as_str(), Some(USER_CODE));
        assert!(
            pending["url"]
                .as_str()
                .is_some_and(|url| url.ends_with("/codex/device")),
            "{pending}"
        );
        let finished = login_finished(&mut rpc).await;
        assert_eq!(finished["state"].as_str(), Some("ready"), "{finished}");
    })
    .await;
}

#[tokio::test]
async fn a_failed_login_reports_failed_with_the_token_error_detail() {
    let (rig, _fake) = fake_rig(TokenReply::Reject).await;
    let auth = rig.data().join("auth.json");
    with_rpc(&rig, |mut rpc| async move {
        initialize(&mut rpc).await;
        rpc.call(1, "host/subscribe", sonic_rs::json!({})).await;
        let reply = rpc
            .call(
                2,
                "auth/login",
                sonic_rs::json!({"provider": "openai-codex", "method": "browser"}),
            )
            .await;
        let url = result(&reply)["url"].as_str().expect("url").to_owned();
        follow_authorize_url(&url, "auth-code")
            .await
            .expect("callback");
        let finished = login_finished(&mut rpc).await;
        assert_eq!(finished["state"].as_str(), Some("failed"), "{finished}");
        assert!(
            finished["detail"].as_str().is_some_and(
                |detail| detail.starts_with("sign-in failed: the token endpoint returned 400")
            ),
            "{finished}"
        );
        assert!(!auth.exists());
    })
    .await;
}

#[tokio::test]
async fn login_params_are_validated_before_any_flow_starts() {
    let (rig, fake) = fake_rig(TokenReply::Issue).await;
    with_rpc(&rig, |mut rpc| async move {
        initialize(&mut rpc).await;
        let reply = rpc
            .call(
                1,
                "auth/login",
                sonic_rs::json!({"provider": "x", "method": "api_key"}),
            )
            .await;
        assert_error(
            &reply,
            -32602,
            r#"invalid params for auth/login: method "api_key" needs apiKey"#,
        );
        let reply = rpc
            .call(
                2,
                "auth/login",
                sonic_rs::json!({"provider": "openai", "method": "browser"}),
            )
            .await;
        assert_error(
            &reply,
            -32602,
            "invalid params for auth/login: openai does not sign in with browser",
        );
        let reply = rpc
            .call(
                3,
                "auth/login",
                sonic_rs::json!({"provider": "nobody", "method": "device"}),
            )
            .await;
        assert_error(
            &reply,
            -32602,
            r#"invalid params for auth/login: unknown provider "nobody""#,
        );
        let reply = rpc
            .call(
                4,
                "auth/login",
                sonic_rs::json!({"provider": "openai", "method": "pigeon"}),
            )
            .await;
        assert_error(
            &reply,
            -32602,
            r#"invalid params for auth/login: unknown login method "pigeon""#,
        );
        assert_eq!(fake.requests(), [] as [dal_agent::login_fake::Recorded; 0]);
    })
    .await;
}

#[tokio::test]
async fn an_api_key_login_stores_the_key_and_answers_ready() {
    let (rig, _fake) = fake_rig(TokenReply::Issue).await;
    let auth = rig.data().join("auth.json");
    with_rpc(&rig, |mut rpc| async move {
        initialize(&mut rpc).await;
        let reply = rpc
            .call(
                1,
                "auth/login",
                sonic_rs::json!({"provider": "anthropic", "method": "api_key", "apiKey": "sk-ant-test"}),
            )
            .await;
        assert_eq!(result(&reply), &sonic_rs::json!({"state": "ready"}));
        let stored = AuthStore::load(&auth).expect("auth.json");
        assert!(matches!(
            stored.credential("anthropic"),
            Some(Credential::ApiKey { key }) if key.expose() == "sk-ant-test"
        ));
    })
    .await;
}

#[tokio::test]
async fn logout_removes_one_provider_or_all_and_revokes_codex() {
    let (rig, fake) = fake_rig(TokenReply::Issue).await;
    store(
        &rig,
        &[
            ("openai-codex", oauth(FAR_FUTURE, true)),
            (
                "openai",
                Credential::ApiKey {
                    key: SecretString::from("sk-keep"),
                },
            ),
        ],
    );
    let auth = rig.data().join("auth.json");
    with_rpc(&rig, |mut rpc| async move {
        initialize(&mut rpc).await;
        let reply = rpc
            .call(
                1,
                "auth/logout",
                sonic_rs::json!({"provider": "openai-codex"}),
            )
            .await;
        assert_eq!(
            result(&reply),
            &sonic_rs::json!({"removed": ["openai-codex"]})
        );
        assert_eq!(fake.requests_to("/oauth/revoke").len(), 1);
        let reply = rpc
            .call(2, "auth/logout", sonic_rs::json!({"provider": "anthropic"}))
            .await;
        assert_eq!(result(&reply), &sonic_rs::json!({"removed": []}));
        let reply = rpc
            .call(3, "auth/logout", sonic_rs::json!({"provider": "nobody"}))
            .await;
        assert_error(
            &reply,
            -32602,
            r#"invalid params for auth/logout: unknown provider "nobody""#,
        );
        let reply = rpc.call(4, "auth/logout", sonic_rs::json!({})).await;
        assert_eq!(result(&reply), &sonic_rs::json!({"removed": ["openai"]}));
        assert!(!auth.exists());
    })
    .await;
}

#[tokio::test]
async fn status_reports_expired_for_a_stored_oauth_credential_inside_the_refresh_window() {
    let (rig, _fake) = fake_rig(TokenReply::Issue).await;
    store(
        &rig,
        &[
            ("openai-codex", oauth(1, true)),
            ("anthropic", oauth(FAR_FUTURE, false)),
        ],
    );
    with_rpc(&rig, |mut rpc| async move {
        initialize(&mut rpc).await;
        let status = rpc.call(1, "auth/status", sonic_rs::json!({})).await;
        let rows = result(&status)["providers"].as_array().expect("rows");
        let state_of = |provider: &str| {
            rows.iter()
                .find(|row| row["provider"].as_str() == Some(provider))
                .map(|row| {
                    (
                        row["state"].as_str().map(str::to_owned),
                        row["detail"].as_str().map(str::to_owned),
                    )
                })
                .expect("row")
        };
        assert_eq!(
            state_of("openai-codex"),
            (Some("expired".into()), Some("oauth".into()))
        );
        assert_eq!(
            state_of("anthropic"),
            (Some("ready".into()), Some("oauth".into()))
        );
        assert_eq!(state_of("openai"), (Some("ready".into()), None));
    })
    .await;
}

#[cfg(unix)]
#[tokio::test]
async fn the_remote_client_decodes_login_logout_and_status_from_the_real_server() {
    use crate::remote::{
        RemoteEndpoint, RemoteHost, RemoteHostUpdate, RemoteLogin, RemoteLoginMethod,
    };

    let (rig, _fake) = fake_rig(TokenReply::Issue).await;
    store(&rig, &[("anthropic", oauth(1, false))]);
    let socket_dir = rig.dir.path().join("sock");
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&socket_dir)
            .expect("private socket directory");
    }
    let socket = socket_dir.join("rpc.sock");
    let serve_host = rig.host.clone();
    let server = crate::serve_local(&socket, None, None, move |transport| {
        let host = serve_host.clone();
        Box::pin(crate::serve_rpc(host, transport))
    });
    let client = async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !socket.exists() {
            assert!(tokio::time::Instant::now() < deadline, "socket appears");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let host = RemoteHost::connect(RemoteEndpoint::LocalSocket(socket.clone()))
            .await
            .expect("connect");
        let rows = host.auth_status().await.expect("status");
        let anthropic = rows
            .iter()
            .find(|row| row.provider == "anthropic")
            .expect("anthropic row");
        assert_eq!(anthropic.state, "expired");
        assert_eq!(anthropic.detail.as_deref(), Some("oauth"));

        let mut updates = host.subscribe().await.expect("subscribe");
        let pending = host
            .login("openai-codex", RemoteLoginMethod::Browser)
            .await
            .expect("login");
        let RemoteLogin::Pending { url, user_code } = pending else {
            panic!("a browser login is pending, got {pending:?}");
        };
        assert!(user_code.is_none());
        follow_authorize_url(&url, "auth-code")
            .await
            .expect("callback");
        loop {
            let update = tokio::time::timeout(Duration::from_secs(10), updates.next())
                .await
                .expect("update in time")
                .expect("update");
            if let RemoteHostUpdate::LoginFinished {
                provider,
                ready,
                detail,
            } = update
            {
                assert_eq!(provider, "openai-codex");
                assert!(ready);
                assert_eq!(detail, None);
                break;
            }
        }
        assert_eq!(
            host.logout(Some("openai-codex")).await.expect("logout"),
            ["openai-codex"]
        );
        assert_eq!(host.logout(None).await.expect("logout all"), ["anthropic"]);
    };
    tokio::select! {
        outcome = server => panic!("the local server ended: {outcome:?}"),
        () = client => {}
    }
}

#[tokio::test]
async fn closing_the_connection_cancels_a_pending_login() {
    let (rig, _fake) = fake_rig(TokenReply::Issue).await;
    let auth = rig.data().join("auth.json");
    let mut updates = rig.host.subscribe();
    with_rpc(&rig, |mut rpc| async move {
        initialize(&mut rpc).await;
        let reply = rpc
            .call(
                1,
                "auth/login",
                sonic_rs::json!({"provider": "openai-codex", "method": "browser"}),
            )
            .await;
        assert_eq!(result(&reply)["state"].as_str(), Some("pending"));
    })
    .await;
    let finished = loop {
        let update = tokio::time::timeout(Duration::from_secs(10), updates.next())
            .await
            .expect("update in time")
            .expect("host open");
        if let dal_agent::HostUpdate::LoginFinished { .. } = update {
            break update;
        }
    };
    assert_eq!(
        finished,
        dal_agent::HostUpdate::LoginFinished {
            provider: "openai-codex".into(),
            ready: false,
            detail: Some("sign-in cancelled.".into()),
        }
    );
    assert!(!auth.exists());
}
