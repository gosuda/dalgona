//! `auth/status`, `auth/login`, and `auth/logout` over the real RPC loop and
//! the real remote client, against a loopback fake OAuth server.

use std::time::Duration;

use dal_agent::login_fake::{FakeOAuth, TokenReply, USER_CODE, follow_authorize_url};
use dal_provider::{AuthStore, Credential, OAuthCredential, SecretString};
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};

use super::super::support::{
    Rig, Rpc, assert_error, assert_invalid_params, initialize, result, rig,
};
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
        let login_id = pending["loginId"].as_u64().expect("login id");
        assert!(url.contains("/oauth/authorize"), "{url}");
        assert!(!auth.exists(), "nothing is stored before the callback");

        let page = follow_authorize_url(&url, "auth-code")
            .await
            .expect("callback");
        assert!(page.contains("Sign-in complete"), "{page}");
        assert_eq!(
            login_finished(&mut rpc).await,
            sonic_rs::json!({"type": "login_finished", "loginId": login_id, "provider": "openai-codex", "state": "ready"})
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
async fn logout_with_a_malformed_provider_removes_nothing() {
    let rig = rig(&[]).await;
    store(
        &rig,
        &[
            (
                "openai",
                Credential::ApiKey {
                    key: SecretString::from("sk-first"),
                },
            ),
            (
                "anthropic",
                Credential::ApiKey {
                    key: SecretString::from("sk-second"),
                },
            ),
        ],
    );
    let auth = rig.data().join("auth.json");
    let before = std::fs::read(&auth).expect("auth.json");
    with_rpc(&rig, |mut rpc| async move {
        initialize(&mut rpc).await;
        let malformed = [
            sonic_rs::json!({"provider": 123}),
            sonic_rs::json!({"provider": ["openai"]}),
            sonic_rs::json!({"provider": {"id": "openai"}}),
            sonic_rs::json!({"provider": null}),
            sonic_rs::json!({"provider": true}),
        ];
        for (id, params) in (1..).zip(malformed) {
            let reply = rpc.call(id, "auth/logout", params).await;
            assert_invalid_params(&reply, "auth/logout", "provider");
        }
        for (id, params) in (10..).zip([sonic_rs::json!([1]), sonic_rs::json!("openai")]) {
            let reply = rpc.call(id, "auth/logout", params).await;
            assert_invalid_params(&reply, "auth/logout", "object");
        }
        assert_eq!(std::fs::read(&auth).expect("auth.json"), before);
        let stored = AuthStore::load(&auth).expect("auth.json");
        assert!(matches!(
            stored.credential("openai"),
            Some(Credential::ApiKey { key }) if key.expose() == "sk-first"
        ));
        assert!(matches!(
            stored.credential("anthropic"),
            Some(Credential::ApiKey { key }) if key.expose() == "sk-second"
        ));
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
                ..
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
            login: dal_agent::login::LoginId::new(1),
            provider: "openai-codex".into(),
            ready: false,
            detail: Some("sign-in cancelled.".into()),
        }
    );
    assert!(!auth.exists());
}

async fn cancel_reused_request_ids(rig: &Rig) {
    with_rpc(rig, |mut rpc| async move {
        initialize(&mut rpc).await;
        let first = rpc
            .call(
                2,
                "auth/login",
                sonic_rs::json!({"provider": "openai-codex", "method": "browser"}),
            )
            .await;
        let first_id = result(&first)["loginId"].as_u64().expect("first login id");
        let second = rpc
            .call(
                2,
                "auth/login",
                sonic_rs::json!({"provider": "openai-codex", "method": "browser"}),
            )
            .await;
        let second_id = result(&second)["loginId"]
            .as_u64()
            .expect("second login id");
        assert_ne!(
            first_id, second_id,
            "reused request ids must not alias logins"
        );

        for (request_id, login_id) in [(3, first_id), (4, second_id)] {
            let reply = rpc
                .call(
                    request_id,
                    "auth/cancel",
                    sonic_rs::json!({"loginId": login_id}),
                )
                .await;
            assert_eq!(result(&reply), &sonic_rs::json!({"cancelled": true}));
        }
    })
    .await;
}

async fn disconnect_reused_request_id(rig: &Rig) {
    let mut updates = rig.host.subscribe();
    with_rpc(rig, |mut rpc| async move {
        initialize(&mut rpc).await;
        rpc.call(1, "host/subscribe", sonic_rs::json!({})).await;
        let first = rpc
            .call(
                2,
                "auth/login",
                sonic_rs::json!({"provider": "openai-codex", "method": "browser"}),
            )
            .await;
        let first_id = result(&first)["loginId"].as_u64().expect("first login id");
        let first_url = result(&first)["url"]
            .as_str()
            .expect("first authorize url")
            .to_owned();
        let second = rpc
            .call(
                2,
                "auth/login",
                sonic_rs::json!({"provider": "openai-codex", "method": "browser"}),
            )
            .await;
        let second_id = result(&second)["loginId"]
            .as_u64()
            .expect("second login id");
        assert_eq!(second_id, 4);
        assert_ne!(
            first_id, second_id,
            "reused request ids must not alias logins"
        );

        follow_authorize_url(&first_url, "auth-code")
            .await
            .expect("first callback");
        let finished = login_finished(&mut rpc).await;
        assert_eq!(finished["loginId"].as_u64(), Some(first_id), "{finished}");
        assert_eq!(finished["state"].as_str(), Some("ready"), "{finished}");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let mut request_id = 3;
        loop {
            let reply = rpc
                .call(
                    request_id,
                    "auth/cancel",
                    sonic_rs::json!({"loginId": first_id}),
                )
                .await;
            if result(&reply) == &sonic_rs::json!({"cancelled": false}) {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "completed login forgets its id"
            );
            request_id += 1;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;

    let finished = loop {
        let update = tokio::time::timeout(Duration::from_secs(10), updates.next())
            .await
            .expect("update in time")
            .expect("host open");
        if let dal_agent::HostUpdate::LoginFinished { login, .. } = &update
            && login.get() == 4
        {
            break update;
        }
    };
    assert_eq!(
        finished,
        dal_agent::HostUpdate::LoginFinished {
            login: dal_agent::login::LoginId::new(4),
            provider: "openai-codex".into(),
            ready: false,
            detail: Some("sign-in cancelled.".into()),
        }
    );

    with_rpc(rig, |mut rpc| async move {
        initialize(&mut rpc).await;
        for (request_id, login_id) in [(1, 3), (2, 4)] {
            let reply = rpc
                .call(
                    request_id,
                    "auth/cancel",
                    sonic_rs::json!({"loginId": login_id}),
                )
                .await;
            assert_eq!(
                result(&reply),
                &sonic_rs::json!({"cancelled": false}),
                "login {login_id} must be gone after completion or connection close"
            );
        }
    })
    .await;
}

#[tokio::test]
async fn reused_request_id_keeps_logins_independent_through_cancellation_and_disconnect() {
    let (rig, _fake) = fake_rig(TokenReply::Issue).await;
    cancel_reused_request_ids(&rig).await;
    disconnect_reused_request_id(&rig).await;
}

#[tokio::test]
async fn cancel_before_completion_cancels_the_waiter_and_reports_true() {
    let (rig, _fake) = fake_rig(TokenReply::Issue).await;
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
        let login_id = pending["loginId"].as_u64().expect("login id");
        let reply = rpc
            .call(3, "auth/cancel", sonic_rs::json!({"loginId": login_id}))
            .await;
        assert_eq!(result(&reply), &sonic_rs::json!({"cancelled": true}));
        let finished = login_finished(&mut rpc).await;
        assert_eq!(finished["state"].as_str(), Some("failed"), "{finished}");
        assert_eq!(
            finished["detail"].as_str(),
            Some("sign-in cancelled."),
            "{finished}"
        );
        assert!(!auth.exists(), "a cancelled login stores nothing");
        let reply = rpc
            .call(4, "auth/cancel", sonic_rs::json!({"loginId": login_id}))
            .await;
        assert_eq!(result(&reply), &sonic_rs::json!({"cancelled": false}));
    })
    .await;
}

#[tokio::test]
async fn cancel_after_completion_reports_false() {
    let (rig, _fake) = fake_rig(TokenReply::Issue).await;
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
        let login_id = pending["loginId"].as_u64().expect("login id");
        let url = pending["url"].as_str().expect("authorize url").to_owned();
        follow_authorize_url(&url, "auth-code")
            .await
            .expect("callback");
        let finished = login_finished(&mut rpc).await;
        assert_eq!(finished["state"].as_str(), Some("ready"), "{finished}");
        // The finished login forgets its id just after the update is
        // published, so poll until the id is gone.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let mut id = 3;
        loop {
            let reply = rpc
                .call(id, "auth/cancel", sonic_rs::json!({"loginId": login_id}))
                .await;
            if result(&reply) == &sonic_rs::json!({"cancelled": false}) {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "a finished login forgets its id"
            );
            id += 1;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
}

#[tokio::test]
async fn cancel_with_an_unknown_or_malformed_id_is_false_or_invalid_params() {
    let (rig, _fake) = fake_rig(TokenReply::Issue).await;
    with_rpc(&rig, |mut rpc| async move {
        initialize(&mut rpc).await;
        let reply = rpc
            .call(1, "auth/cancel", sonic_rs::json!({"loginId": 999_999}))
            .await;
        assert_eq!(result(&reply), &sonic_rs::json!({"cancelled": false}));
        let reply = rpc.call(2, "auth/cancel", sonic_rs::json!({})).await;
        assert_error(
            &reply,
            -32602,
            "invalid params for auth/cancel: missing member `loginId`",
        );
        let reply = rpc
            .call(3, "auth/cancel", sonic_rs::json!({"loginId": "1"}))
            .await;
        assert_error(
            &reply,
            -32602,
            "invalid params for auth/cancel: member `loginId` must be an integer",
        );
        let reply = rpc
            .call(4, "auth/cancel", sonic_rs::json!({"loginId": -1}))
            .await;
        assert_error(
            &reply,
            -32602,
            "invalid params for auth/cancel: member `loginId` must be a positive integer",
        );
        let reply = rpc.call(5, "auth/cancel", sonic_rs::json!([])).await;
        assert_invalid_params(&reply, "auth/cancel", "object");
    })
    .await;
}

#[cfg(unix)]
#[tokio::test]
async fn the_remote_client_cancels_a_pending_login() {
    use crate::remote::{
        CancellableLogin, RemoteEndpoint, RemoteHost, RemoteHostUpdate, RemoteLoginMethod,
    };

    let (rig, _fake) = fake_rig(TokenReply::Issue).await;
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
        let mut updates = host.subscribe().await.expect("subscribe");
        let login = host
            .login_cancellable("openai-codex", RemoteLoginMethod::Browser)
            .await
            .expect("login");
        let CancellableLogin::Pending { login_id, url, .. } = login else {
            panic!("a browser login is pending, got {login:?}");
        };
        assert!(url.contains("/oauth/authorize"), "{url}");
        assert!(host.cancel_login(login_id).await.expect("cancel"));
        loop {
            let update = tokio::time::timeout(Duration::from_secs(10), updates.next())
                .await
                .expect("update in time")
                .expect("update");
            if let RemoteHostUpdate::LoginFinished {
                provider,
                ready,
                detail,
                login_id: finished_id,
            } = update
            {
                assert_eq!(provider, "openai-codex");
                assert!(!ready);
                assert_eq!(detail.as_deref(), Some("sign-in cancelled."));
                assert_eq!(finished_id, login_id);
                break;
            }
        }
        assert!(!host.cancel_login(login_id).await.expect("recancel"));
    };
    tokio::select! {
        outcome = server => panic!("the local server ended: {outcome:?}"),
        () = client => {}
    }
}

#[test]
fn bespoke_arms_prefix_a_bare_invalid_params_error_like_guarded_ones() {
    use crate::rpc::{host_error, normalize_invalid_params};

    let bare = host_error(dal_agent::HostError::Store(
        dal_store::StoreError::Invalid {
            reason: "the record is not writable".into(),
        },
    ));
    assert_eq!(bare.code, -32602);
    assert_eq!(bare.message, "the record is not writable");
    for method in ["auth/login", "session/subscribe"] {
        let shaped = normalize_invalid_params(method, bare.clone());
        assert_eq!(shaped.code, -32602);
        assert_eq!(
            shaped.message,
            format!("invalid params for {method}: the record is not writable")
        );
        let again = normalize_invalid_params(method, shaped.clone());
        assert_eq!(again.message, shaped.message, "a prefixed error is kept");
    }
    let internal = host_error(dal_agent::HostError::Closed);
    assert_eq!(
        normalize_invalid_params("auth/login", internal.clone()).message,
        internal.message
    );
}
