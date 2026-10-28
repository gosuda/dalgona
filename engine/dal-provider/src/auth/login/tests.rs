use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU32, Ordering},
};

mod google_login;
mod native_login;
mod oauth_login;
mod server;

use tokio::{sync::oneshot, task::JoinSet};
use tokio_util::sync::CancellationToken;

use super::{LoginIo, LoginSite, Method, login, login_providers, sign_out, stored_credentials};
use crate::{
    AuthStore, Credential, CredentialKind, EnvSnapshot, LoginEndpoints, OAuthCredential,
    ProviderError, ProviderIdentity, SecretString,
};

/// The host platform every test site captures: fixed, so a request that
/// names it is deterministic.
fn test_identity() -> ProviderIdentity {
    ProviderIdentity {
        version: "0.0.0-test".into(),
        os: "linux".into(),
        os_version: String::new().into(),
        arch: "x86_64".into(),
    }
}

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
        LoginSite::new(
            self.0.join("auth.json"),
            self.0.join("cache"),
            &test_identity(),
            &EnvSnapshot::default(),
        )
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
fn the_provider_list_names_the_sign_in_providers_in_display_order() {
    // The per-provider method matrix — including the invariant that every
    // provider offers at least one method — is owned by
    // provider/def/tests.rs::methods_follow_the_row. This test owns only the
    // login surface: which providers are listed, in display order for front ends.
    // No count and no literal roster: the table grows with each preset. The
    // login surface owns only that every listed row offers a sign-in method,
    // that the list stays in display order, and that the vendor rows are kept.
    let listed = login_providers();
    for def in &listed {
        assert!(
            def.methods().next().is_some(),
            "{} is listed but offers no sign-in method",
            def.id
        );
    }
    let ids: Vec<_> = listed.iter().map(|def| def.id).collect();
    assert!(
        ids.windows(2).all(|pair| pair[0] <= pair[1]),
        "the sign-in list stays in display order: {ids:?}"
    );
    for vendor in ["anthropic", "openai", "openai-codex"] {
        assert!(
            ids.contains(&vendor),
            "the sign-in list keeps its {vendor} row"
        );
    }
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
async fn cancelling_while_the_auth_file_lock_is_held_reports_cancellation_and_writes_nothing() {
    let dir = TestDir::new();
    let guard = crate::auth::refresh::lock_auth_file(&dir.0.join("auth.json"))
        .await
        .expect("hold the auth-file lock");
    let (sender, receiver) = oneshot::channel::<String>();
    sender
        .send(String::from("sk-held"))
        .expect("receiver is alive");
    let cancel = CancellationToken::new();
    let io = LoginIo::channel(Some(receiver), cancel.clone()).0;
    let site = dir.site();
    let mut attempt = JoinSet::new();
    attempt.spawn(async move { login("openai", Method::ApiKey, io, &site).await });
    tokio::select! {
        biased;
        () = cancel.cancelled() => panic!("cancel nobody fired"),
        () = tokio::time::sleep(std::time::Duration::from_millis(250)) => {}
        joined = attempt.join_next() => panic!("login finished before cancellation: {joined:?}"),
    }
    cancel.cancel();
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), attempt.join_next())
        .await
        .expect("the cancelled login returns promptly")
        .expect("the login task is still tracked")
        .expect("the login task runs");
    assert!(matches!(result, Err(ProviderError::LoginCancelled)));
    assert!(!dir.0.join("auth.json").exists());
    drop(guard);
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
        project_id: None,
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

mod copilot {
    use super::server::{Reply, Request, Server};
    use super::*;
    use crate::LoginProgress;

    fn json_reply(status: u16, body: impl Into<String>) -> Reply {
        Reply::Json(status, body.into())
    }

    /// One string member of the structured key envelope a sign-in stores.
    fn envelope_member(key_text: &str, name: &str) -> Option<String> {
        use sonic_rs::JsonValueTrait as _;
        let envelope = sonic_rs::from_str::<sonic_rs::Value>(key_text)
            .expect("the stored key is a JSON envelope");
        envelope
            .get(name)
            .and_then(|member| member.as_str())
            .map(str::to_owned)
    }

    struct AbortOnDrop(tokio::task::JoinHandle<()>);

    impl Drop for AbortOnDrop {
        fn drop(&mut self) {
            self.0.abort();
        }
    }

    const EXPECTED_BUNDLED_POLICY_IDS: [&str; 54] = [
        "claude-fable-5",
        "claude-fable-5.1",
        "claude-haiku-4.5",
        "claude-haiku-5.5",
        "claude-opus-4.5",
        "claude-opus-4.6",
        "claude-opus-4.7",
        "claude-opus-4.8",
        "claude-opus-5",
        "claude-opus-5.5",
        "claude-sonnet-4",
        "claude-sonnet-4.5",
        "claude-sonnet-4.6",
        "claude-sonnet-5",
        "claude-sonnet-5.5",
        "gemini-2.5-pro",
        "gemini-3-flash-preview",
        "gemini-3-pro-preview",
        "gemini-3.1-pro-preview",
        "gemini-3.5-flash",
        "gemini-3.6-flash",
        "gemini-3.7-flash",
        "gemini-3.8-flash",
        "gpt-4.1",
        "gpt-4o",
        "gpt-5",
        "gpt-5-mini",
        "gpt-5.1",
        "gpt-5.1-codex",
        "gpt-5.1-codex-max",
        "gpt-5.1-codex-mini",
        "gpt-5.2",
        "gpt-5.2-codex",
        "gpt-5.3-codex",
        "gpt-5.4",
        "gpt-5.4-mini",
        "gpt-5.4-nano",
        "gpt-5.5",
        "gpt-5.6-luna",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-6-astra",
        "gpt-6-luna",
        "gpt-6-sol",
        "gpt-6.1-sol",
        "grok-4.5",
        "grok-4.6",
        "grok-4.7",
        "grok-code-fast-1",
        "kimi-k2.7-code",
        "kimi-k3",
        "mai-code-1-flash-picker",
        "mai-code-1.1-flash",
        "raptor-mini",
    ];

    async fn start_copilot_device_server() -> Server {
        Server::start(|request: &Request| match request.path() {
            "/login/device/code" => {
                let form = request.form();
                let client_id = form
                    .iter()
                    .find(|(k, _)| k == "client_id")
                    .map(|(_, v)| v.as_str());
                assert_eq!(client_id, Some("Ov23li8tweQw6odWQebz"));
                json_reply(
                    200,
                    r#"{
                        "device_code": "copilot-dev-code-1",
                        "user_code": "ABCD-EFGH",
                        "verification_uri": "https://github.com/login/device",
                        "interval": 1,
                        "expires_in": 900
                    }"#,
                )
            }
            "/login/oauth/access_token" => {
                let form = request.form();
                let code = form
                    .iter()
                    .find(|(k, _)| k == "device_code")
                    .map(|(_, v)| v.as_str());
                assert_eq!(code, Some("copilot-dev-code-1"));
                json_reply(
                    200,
                    r#"{
                        "access_token": "token-test-12345",
                        "token_type": "bearer",
                        "scope": "read:user"
                    }"#,
                )
            }
            "/copilot_internal/user" => {
                assert_eq!(
                    request.header("authorization"),
                    Some("token token-test-12345")
                );
                json_reply(
                    200,
                    r#"{
                        "endpoints": {
                            "api": "https://api.githubcopilot.com"
                        }
                    }"#,
                )
            }
            path if path.starts_with("/models") => {
                assert_eq!(
                    request.header("authorization"),
                    Some("Bearer token-test-12345")
                );
                if path == "/models" {
                    json_reply(200, r#"{"data":[{"id":"claude-3.5-sonnet"}]}"#)
                } else {
                    json_reply(200, r#"{"state":"enabled"}"#)
                }
            }
            _ => json_reply(404, "not found"),
        })
        .await
        .expect("start server")
    }

    #[tokio::test]
    async fn copilot_device_login_public_success_persists_api_key_envelope() {
        let dir = TestDir::new();
        let server = start_copilot_device_server().await;

        let endpoints = LoginEndpoints::loopback(server.base(), &EnvSnapshot::default())
            .expect("loopback endpoints");
        let site = dir.site().with_endpoints(endpoints).expect("site");

        let cancel = CancellationToken::new();
        let (io, mut progress_rx) = LoginIo::channel(None, cancel);

        let credential = login("github-copilot", Method::Device, io, &site)
            .await
            .expect("device login succeeds");

        let key = match credential {
            Credential::ApiKey { key } => key,
            other => panic!("expected ApiKey credential, got {other:?}"),
        };

        let key_text = key.expose();
        assert_eq!(
            envelope_member(key_text, "token").as_deref(),
            Some("token-test-12345")
        );
        assert_eq!(
            envelope_member(key_text, "apiEndpoint").as_deref(),
            Some("https://api.githubcopilot.com"),
            "the discovered account endpoint is stored"
        );
        assert_eq!(
            envelope_member(key_text, "enterpriseUrl"),
            None,
            "a public GitHub sign-in names no enterprise host"
        );

        let store = AuthStore::load(dir.0.join("auth.json")).expect("load auth.json");
        assert_eq!(
            store.credential("github-copilot"),
            Some(Credential::ApiKey { key })
        );

        let mut events = Vec::new();
        while let Ok(event) = progress_rx.try_recv() {
            events.push(event);
        }
        assert!(
            events.iter().any(
                |e| matches!(e, LoginProgress::ShowCode { code, .. } if &**code == "ABCD-EFGH")
            ),
            "must emit ShowCode with user_code: {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, LoginProgress::Exchanging)),
            "must emit Exchanging progress: {events:?}"
        );
    }

    #[tokio::test]
    async fn copilot_device_login_cancelled_during_poll_leaves_auth_untouched() {
        let dir = TestDir::new();
        let mut seed = AuthStore::empty(dir.0.join("auth.json"));
        seed.set(
            "openai",
            Credential::ApiKey {
                key: SecretString::from("sk-seed"),
            },
        )
        .expect("set");
        seed.store().expect("store");
        let seeded_bytes = fs::read(dir.0.join("auth.json")).expect("read seed");

        let cancel = CancellationToken::new();
        let cancel_for_server = cancel.clone();

        let server = Server::start(move |request: &Request| match request.path() {
            "/login/device/code" => json_reply(
                200,
                r#"{
                        "device_code": "dev-code-cancel",
                        "user_code": "CANCEL-1234",
                        "verification_uri": "https://github.com/login/device",
                        "interval": 1,
                        "expires_in": 900
                    }"#,
            ),
            "/login/oauth/access_token" => {
                cancel_for_server.cancel();
                json_reply(200, r#"{"error":"authorization_pending"}"#)
            }
            _ => json_reply(404, "not found"),
        })
        .await
        .expect("start server");

        let endpoints = LoginEndpoints::loopback(server.base(), &EnvSnapshot::default())
            .expect("loopback endpoints");
        let site = dir.site().with_endpoints(endpoints).expect("site");

        let (io, _rx) = LoginIo::channel(None, cancel);

        let result = login("github-copilot", Method::Device, io, &site).await;
        assert!(
            matches!(result, Err(ProviderError::LoginCancelled)),
            "expected LoginCancelled, got: {result:?}"
        );

        let current_bytes = fs::read(dir.0.join("auth.json")).expect("read auth.json");
        assert_eq!(
            current_bytes, seeded_bytes,
            "auth.json must remain byte-for-byte identical"
        );
    }

    #[tokio::test]
    async fn copilot_device_login_enterprise_target_selection() {
        let dir = TestDir::new();
        let server = Server::start(|request: &Request| match request.path() {
            "/login/device/code" => {
                let form = request.form();
                let client_id = form
                    .iter()
                    .find(|(k, _)| k == "client_id")
                    .map(|(_, v)| v.as_str());
                assert_eq!(client_id, Some("Ov23ctDVkRmgkPke0Mmm"));
                json_reply(
                    200,
                    r#"{
                        "device_code": "ent-code-1",
                        "user_code": "ENT-1234",
                        "verification_uri": "https://github.myenterprise.com/login/device",
                        "interval": 1,
                        "expires_in": 900
                    }"#,
                )
            }
            "/login/oauth/access_token" => {
                let form = request.form();
                let client_id = form
                    .iter()
                    .find(|(k, _)| k == "client_id")
                    .map(|(_, v)| v.as_str());
                assert_eq!(client_id, Some("Ov23ctDVkRmgkPke0Mmm"));
                json_reply(
                    200,
                    r#"{
                        "access_token": "token-ent-67890",
                        "token_type": "bearer",
                        "scope": "read:user"
                    }"#,
                )
            }
            "/copilot_internal/user" => json_reply(
                200,
                r#"{
                        "endpoints": {
                            "api": "https://copilot-api.github.myenterprise.com"
                        }
                    }"#,
            ),
            path if path.starts_with("/models") => json_reply(200, r#"{"state":"enabled"}"#),
            _ => json_reply(404, "not found"),
        })
        .await
        .expect("start server");

        let endpoints = LoginEndpoints::loopback(server.base(), &EnvSnapshot::default())
            .expect("loopback endpoints");
        let site = dir.site().with_endpoints(endpoints).expect("site");

        let (paste_tx, paste_rx) = oneshot::channel();
        paste_tx
            .send("github.myenterprise.com".to_string())
            .expect("send paste");

        let cancel = CancellationToken::new();
        let (io, _rx) = LoginIo::channel(Some(paste_rx), cancel);

        let credential = login("github-copilot", Method::Device, io, &site)
            .await
            .expect("enterprise login succeeds");

        let key = match credential {
            Credential::ApiKey { key } => key,
            other => panic!("expected ApiKey, got {other:?}"),
        };
        let key_text = key.expose();
        assert_eq!(
            envelope_member(key_text, "token").as_deref(),
            Some("token-ent-67890")
        );
        assert_eq!(
            envelope_member(key_text, "enterpriseUrl").as_deref(),
            Some("github.myenterprise.com"),
            "the pasted enterprise host is stored by itself"
        );
        assert_eq!(
            envelope_member(key_text, "apiEndpoint").as_deref(),
            Some("https://copilot-api.github.myenterprise.com"),
            "the discovered enterprise endpoint is stored by itself"
        );
    }

    #[tokio::test]
    async fn copilot_device_login_policy_refusal_handled_per_reference_without_losing_cancellation()
    {
        let dir = TestDir::new();
        let server = Server::start(|request: &Request| match request.path() {
            "/login/device/code" => json_reply(
                200,
                r#"{
                        "device_code": "policy-code-1",
                        "user_code": "POL-1234",
                        "verification_uri": "https://github.com/login/device",
                        "interval": 1,
                        "expires_in": 900
                    }"#,
            ),
            "/login/oauth/access_token" => json_reply(
                200,
                r#"{
                        "access_token": "token-policy-test",
                        "token_type": "bearer",
                        "scope": "read:user"
                    }"#,
            ),
            "/copilot_internal/user" => json_reply(
                200,
                r#"{
                        "endpoints": {
                            "api": "https://api.githubcopilot.com"
                        }
                    }"#,
            ),
            "/models" => json_reply(
                200,
                r#"{"data":[{"id":"claude-3.5-sonnet"},{"id":"grok-beta"}]}"#,
            ),
            path if path.starts_with("/models/") => {
                json_reply(400, r#"{"error":"model_not_supported"}"#)
            }
            _ => json_reply(404, "not found"),
        })
        .await
        .expect("start server");

        let endpoints = LoginEndpoints::loopback(server.base(), &EnvSnapshot::default())
            .expect("loopback endpoints");
        let site = dir.site().with_endpoints(endpoints).expect("site");

        // 1. Policy failure is best-effort and login succeeds:
        let cancel = CancellationToken::new();
        let (io, _rx) = LoginIo::channel(None, cancel);

        let credential = login("github-copilot", Method::Device, io, &site)
            .await
            .expect("login should succeed despite model policy 400 failure");
        assert!(matches!(credential, Credential::ApiKey { .. }));

        // 2. Cancellation during policy phase is never lost/swallowed:
        let cancel2 = CancellationToken::new();
        cancel2.cancel();
        let (io2, _rx2) = LoginIo::channel(None, cancel2);
        let result2 = login("github-copilot", Method::Device, io2, &site).await;
        let Err(ProviderError::LoginCancelled) = result2 else {
            panic!("cancellation must never be swallowed")
        };
    }

    async fn start_truncated_discovery_peer() -> (String, AbortOnDrop) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback peer");
        let base = format!("http://{}", listener.local_addr().expect("peer addr"));
        #[expect(
            clippy::disallowed_methods,
            reason = "test edge: loopback server runs in the background of login test"
        )]
        let peer = AbortOnDrop(tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                #[expect(
                    clippy::disallowed_methods,
                    reason = "test edge: handle connection in the background of login test"
                )]
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    let mut chunk = [0_u8; 1024];
                    let target = loop {
                        let Ok(count) = stream.read(&mut chunk).await else {
                            return;
                        };
                        if count == 0 {
                            return;
                        }
                        bytes.extend_from_slice(&chunk[..count]);
                        let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n")
                        else {
                            if bytes.len() > 64 * 1024 {
                                return;
                            }
                            continue;
                        };
                        let Ok(head) = std::str::from_utf8(&bytes[..end]) else {
                            return;
                        };
                        let mut lines = head.lines();
                        let Some(first) = lines.next() else {
                            return;
                        };
                        let mut start = first.split_whitespace();
                        let (Some(_method), Some(target)) = (start.next(), start.next()) else {
                            return;
                        };
                        let length: usize = lines
                            .filter_map(|line| line.split_once(':'))
                            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                            .and_then(|(_, value)| value.trim().parse().ok())
                            .unwrap_or(0);
                        if bytes.len() < end + 4 + length {
                            continue;
                        }
                        break target.to_owned();
                    };
                    let path = target.split('?').next().unwrap_or_default().to_owned();
                    if path == "/copilot_internal/user" {
                        let partial = b"{\"endpoints\":";
                        let head = "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 400\r\nconnection: close\r\n\r\n";
                        let _written = stream.write_all(head.as_bytes()).await;
                        let _written = stream.write_all(partial).await;
                        let _closed = stream.shutdown().await;
                        return;
                    }
                    let (status, body) = match path.as_str() {
                        "/login/device/code" => (
                            200,
                            "{\"device_code\":\"trunc-code-1\",\"user_code\":\"TRUNC-1\",\"verification_uri\":\"https://github.com/login/device\",\"interval\":1,\"expires_in\":900}",
                        ),
                        "/login/oauth/access_token" => (
                            200,
                            "{\"access_token\":\"token-trunc-test\",\"token_type\":\"bearer\",\"scope\":\"read:user\"}",
                        ),
                        "/models" => (200, "{\"data\":[]}"),
                        _ if path.starts_with("/models/") => (200, "{\"ok\":true}"),
                        _ => (404, "not found"),
                    };
                    let head = format!(
                        "HTTP/1.1 {status} Test\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        body.len()
                    );
                    let _written = stream.write_all(head.as_bytes()).await;
                    let _written = stream.write_all(body.as_bytes()).await;
                    let _closed = stream.shutdown().await;
                });
            }
        }));
        (base, peer)
    }

    #[tokio::test]
    async fn copilot_discovery_truncated_body_is_best_effort_and_persists_without_endpoint() {
        use std::time::Duration;

        let dir = TestDir::new();
        let (base, _peer) = start_truncated_discovery_peer().await;

        let endpoints =
            LoginEndpoints::loopback(&base, &EnvSnapshot::default()).expect("loopback endpoints");
        let site = dir.site().with_endpoints(endpoints).expect("site");
        let cancel = CancellationToken::new();
        let (io, _rx) = LoginIo::channel(None, cancel);
        let credential = tokio::time::timeout(
            Duration::from_secs(30),
            login("github-copilot", Method::Device, io, &site),
        )
        .await
        .expect("login returns without hanging")
        .expect("truncated discovery body is best-effort");
        let key = match credential {
            Credential::ApiKey { key } => key,
            other => panic!("expected ApiKey, got {other:?}"),
        };
        let key_text = key.expose();
        assert!(
            key_text.contains("token-trunc-test"),
            "stored key must contain token: {key_text}"
        );
        assert!(
            !key_text.contains("apiEndpoint"),
            "truncated discovery must omit the endpoint: {key_text}"
        );
    }

    #[tokio::test]
    async fn copilot_signin_issues_no_live_models_get_and_posts_unique_policies() {
        use std::time::Duration;

        let dir = TestDir::new();
        let server = Server::start(|request: &Request| match request.path() {
            "/login/device/code" => json_reply(
                200,
                r#"{
                    "device_code": "nolist-code-1",
                    "user_code": "NOLIST-1",
                    "verification_uri": "https://github.com/login/device",
                    "interval": 1,
                    "expires_in": 900
                }"#,
            ),
            "/login/oauth/access_token" => json_reply(
                200,
                r#"{
                    "access_token": "token-nolist-test",
                    "token_type": "bearer",
                    "scope": "read:user"
                }"#,
            ),
            "/copilot_internal/user" => json_reply(
                200,
                r#"{
                    "endpoints": {
                        "api": "https://api.githubcopilot.com"
                    }
                }"#,
            ),
            "/models" => json_reply(200, r#"{"data":[]}"#),
            path if path.starts_with("/models/") && path.ends_with("/policy") => {
                json_reply(200, r#"{"ok":true}"#)
            }
            _ => json_reply(404, "not found"),
        })
        .await
        .expect("start server");

        let endpoints = LoginEndpoints::loopback(server.base(), &EnvSnapshot::default())
            .expect("loopback endpoints");
        let site = dir.site().with_endpoints(endpoints).expect("site");
        let cancel = CancellationToken::new();
        let (io, _rx) = LoginIo::channel(None, cancel);
        let credential = tokio::time::timeout(
            Duration::from_secs(60),
            login("github-copilot", Method::Device, io, &site),
        )
        .await
        .expect("login returns without hanging")
        .expect("login succeeds");
        assert!(matches!(credential, Credential::ApiKey { .. }));
        assert!(
            server.requests_to("/models").is_empty(),
            "sign-in must use bundled policy IDs, not a live /models GET"
        );
        let mut policy_ids: Vec<String> = server
            .requests()
            .into_iter()
            .filter_map(|request| {
                let path = request.path().to_owned();
                let suffix = path.strip_prefix("/models/")?.strip_suffix("/policy")?;
                (!suffix.is_empty()
                    && suffix.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
                    }))
                .then(|| suffix.to_owned())
            })
            .collect();
        policy_ids.sort();
        let mut expected: Vec<String> = EXPECTED_BUNDLED_POLICY_IDS
            .iter()
            .map(ToString::to_string)
            .collect();
        expected.sort();
        assert_eq!(
            policy_ids, expected,
            "sign-in must activate exactly the bundled policy set, once each"
        );
    }

    #[tokio::test]
    async fn copilot_cancel_during_epilogue_leaves_auth_untouched() {
        use std::time::Duration;

        let dir = TestDir::new();
        let mut seed = AuthStore::empty(dir.0.join("auth.json"));
        seed.set(
            "openai",
            Credential::ApiKey {
                key: SecretString::from("sk-seed"),
            },
        )
        .expect("set");
        seed.store().expect("store");
        let seeded_bytes = fs::read(dir.0.join("auth.json")).expect("read seed");

        let server = Server::start(|request: &Request| match request.path() {
            "/login/device/code" => json_reply(
                200,
                r#"{
                    "device_code": "epilogue-code-1",
                    "user_code": "EPILOG-1",
                    "verification_uri": "https://github.com/login/device",
                    "interval": 1,
                    "expires_in": 900
                }"#,
            ),
            "/login/oauth/access_token" => json_reply(
                200,
                r#"{
                    "access_token": "token-epilogue-test",
                    "token_type": "bearer",
                    "scope": "read:user"
                }"#,
            ),
            "/copilot_internal/user" => json_reply(
                200,
                r#"{
                    "endpoints": {
                        "api": "https://api.githubcopilot.com"
                    }
                }"#,
            ),
            "/models" => json_reply(200, r#"{"data":[]}"#),
            path if path.starts_with("/models/") => Reply::Hold,
            _ => json_reply(404, "not found"),
        })
        .await
        .expect("start server");

        let endpoints = LoginEndpoints::loopback(server.base(), &EnvSnapshot::default())
            .expect("loopback endpoints");
        let site = dir.site().with_endpoints(endpoints).expect("site");
        let cancel = CancellationToken::new();
        let watcher_cancel = cancel.clone();
        let (io, mut events) = LoginIo::channel(None, cancel.clone());
        #[expect(
            clippy::disallowed_methods,
            reason = "test edge: event watcher task monitors login progress in the background"
        )]
        let _watcher = AbortOnDrop(tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if matches!(event, LoginProgress::Exchanging) {
                    watcher_cancel.cancel();
                    return;
                }
            }
        }));
        let result = tokio::time::timeout(
            Duration::from_secs(60),
            login("github-copilot", Method::Device, io, &site),
        )
        .await
        .expect("login returns without hanging");
        assert!(
            matches!(result, Err(ProviderError::LoginCancelled)),
            "expected LoginCancelled, got: {result:?}"
        );
        let current_bytes = fs::read(dir.0.join("auth.json")).expect("read auth.json");
        assert_eq!(
            current_bytes, seeded_bytes,
            "auth.json must remain byte-for-byte identical"
        );
    }
}
