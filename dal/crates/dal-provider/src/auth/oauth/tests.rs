use super::{
    LoginEndpoints, LoginFlow, LoginProgress, PASTE_HINT, callback_code, parse_pasted_code,
    pkce_challenge,
};
use crate::{AuthStore, Credential, OAuthCredential, ProviderError, SecretString};
use dal_core::Family;
use reqwest::Client;
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tokio_util::sync::CancellationToken;
use url::Url;

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "dal-provider-oauth-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        let _removed = fs::remove_dir_all(&path);
        assert!(fs::create_dir_all(&path).is_ok());
        Self(path)
    }

    fn auth_path(&self) -> PathBuf {
        self.0.join("auth.json")
    }

    fn cache_dir(&self) -> PathBuf {
        self.0.join("cache")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _removed = fs::remove_dir_all(&self.0);
    }
}

fn callback_port(url: &str) -> Option<u16> {
    let authorize_url = Url::parse(url).ok()?;
    let redirect = authorize_url
        .query_pairs()
        .find_map(|(key, value)| (key == "redirect_uri").then(|| value.into_owned()))?;
    Url::parse(&redirect).ok()?.port()
}

async fn read_test_request(stream: &mut TcpStream) -> Option<Vec<u8>> {
    let mut request = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        let count = stream.read(&mut chunk).await.ok()?;
        if count == 0 || count > (16_usize * 1024).saturating_sub(request.len()) {
            return None;
        }
        request.extend_from_slice(&chunk[..count]);
        let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let headers = std::str::from_utf8(&request[..header_end]).ok()?;
        let content_length = headers
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find_map(|(name, value)| {
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        if request.len() >= header_end + 4 + content_length {
            return Some(request);
        }
    }
}

async fn mock_once(
    status: u16,
    body: &'static str,
) -> Option<(Url, tokio::task::JoinSet<Option<Vec<u8>>>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await.ok()?;
    let address = listener.local_addr().ok()?;
    let base = Url::parse(&format!("http://{address}")).ok()?;
    let mut server = tokio::task::JoinSet::new();
    server.spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else {
                return None;
            };
            let request = read_test_request(&mut stream).await?;
            let response = format!(
                "HTTP/1.1 {status} Test\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.ok()?;
            stream.write_all(body.as_bytes()).await.ok()?;
            Some(request)
        });
    Some((base, server))
}

async fn mock_redirect(
    status: u16,
) -> Option<(Url, tokio::task::JoinSet<Option<Vec<u8>>>, TcpListener)> {
    let target = TcpListener::bind("127.0.0.1:0").await.ok()?;
    let target_address = target.local_addr().ok()?;
    let listener = TcpListener::bind("127.0.0.1:0").await.ok()?;
    let address = listener.local_addr().ok()?;
    let base = Url::parse(&format!("http://{address}")).ok()?;
    let location = format!("http://{target_address}/redirected");
    let mut server = tokio::task::JoinSet::new();
    server.spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else {
                return None;
            };
            let request = read_test_request(&mut stream).await?;
            let response = format!(
                "HTTP/1.1 {status} Redirect\r\nlocation: {location}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            );
            stream.write_all(response.as_bytes()).await.ok()?;
            Some(request)
        });
    Some((base, server, target))
}

async fn mock_device_poll_redirect()
-> Option<(Url, tokio::task::JoinSet<Option<Vec<Vec<u8>>>>, TcpListener)> {
    let target = TcpListener::bind("127.0.0.1:0").await.ok()?;
    let target_address = target.local_addr().ok()?;
    let listener = TcpListener::bind("127.0.0.1:0").await.ok()?;
    let address = listener.local_addr().ok()?;
    let base = Url::parse(&format!("http://{address}")).ok()?;
    let location = format!("http://{target_address}/redirected");
    let mut server = tokio::task::JoinSet::new();
    server.spawn(async move {
            let (start_request, start_stream) = {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return None;
                };
                let request = read_test_request(&mut stream).await?;
                let body = r#"{"device_auth_id":"device","usercode":"ABCD","interval":"1"}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.ok()?;
                stream.write_all(body.as_bytes()).await.ok()?;
                (request, stream)
            };
            drop(start_stream);
            let (poll_request, poll_stream) = {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return None;
                };
                let request = read_test_request(&mut stream).await?;
                let response = format!(
                    "HTTP/1.1 307 Temporary Redirect\r\nlocation: {location}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                );
                stream.write_all(response.as_bytes()).await.ok()?;
                (request, stream)
            };
            drop(poll_stream);
            Some(vec![start_request, poll_request])
        });
    Some((base, server, target))
}
async fn occupied_callback_port() -> Option<(TcpListener, u16)> {
    let listener = TcpListener::bind("127.0.0.1:0").await.ok()?;
    let port = listener.local_addr().ok()?.port();
    Some((listener, port))
}

fn loopback_endpoints(base: &str, codex_port: u16, claude_port: u16) -> Option<LoginEndpoints> {
    LoginEndpoints::loopback(base)
        .ok()
        .map(|endpoints| endpoints.with_callback_ports(codex_port, claude_port))
}

fn configured_flow<'a>(
    provider: &str,
    store: &'a mut AuthStore,
    dir: &TestDir,
    endpoints: LoginEndpoints,
) -> Result<LoginFlow<'a>, ProviderError> {
    LoginFlow::new(
        provider,
        store,
        Client::new(),
        "dalgon/test (linux test; x86_64)",
        dir.cache_dir(),
    )?
    .with_endpoints(endpoints)
}

#[test]
fn pkce_rfc7636_vector() {
    assert_eq!(
        pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
}

#[test]
fn pasted_state_mismatch_is_typed() {
    let result = parse_pasted_code("code123#wrong-state", "right-state");
    assert!(matches!(result, Err(crate::ProviderError::StateMismatch)));
}

#[test]
fn pasted_redirect_and_query_are_accepted() {
    assert_eq!(
        parse_pasted_code(
            "http://localhost/callback?code=code123&state=state",
            "state"
        )
        .ok()
        .as_deref(),
        Some("code123")
    );
    assert_eq!(
        parse_pasted_code("code123", "state").ok().as_deref(),
        Some("code123")
    );
    assert_eq!(
        PASTE_HINT,
        "Paste the redirect URL or the code shown in the browser."
    );
}

#[test]
fn endpoint_override_rejects_non_loopback_origins() {
    assert!(LoginEndpoints::loopback("http://127.0.0.1:8123").is_ok());
    assert!(LoginEndpoints::loopback("http://example.com").is_err());
}

/// Occupies one loopback port to stand in for the preferred callback port.
async fn held_loopback_port() -> (TcpListener, u16) {
    let holder = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("loopback bind succeeds");
    let port = holder.local_addr().expect("bound address").port();
    (holder, port)
}

#[tokio::test]
async fn an_occupied_preferred_port_falls_back_to_a_free_port() {
    let (holder, taken) = held_loopback_port().await;
    let bound = super::bind_callback_with_fallback(taken).await;
    let Ok((listener, port)) = bound else {
        panic!("an occupied preferred port must not force paste login: {bound:?}");
    };
    assert_ne!(port, taken);
    drop(listener);
    drop(holder);
}

#[tokio::test]
async fn a_free_preferred_port_stays_preferred_and_a_strict_bind_refuses() {
    let (holder, taken) = held_loopback_port().await;
    assert!(
        super::bind_callback(taken).await.is_err(),
        "the registered fixed port must stay strict for providers that register it"
    );
    let (listener, port) = super::bind_callback(0).await.expect("ephemeral binds");
    assert_ne!(port, 0);
    drop(listener);
    drop(holder);
}

#[tokio::test]
async fn occupied_claude_callback_advertises_a_bound_port() {
    let dir = TestDir::new();
    let blocker = occupied_callback_port().await;
    assert!(blocker.is_some());
    let Some((blocker, port)) = blocker else {
        return;
    };
    let endpoints = loopback_endpoints("http://127.0.0.1:1", 0, port);
    assert!(endpoints.is_some());
    let Some(endpoints) = endpoints else {
        return;
    };
    let mut store = AuthStore::empty(dir.auth_path());
    let flow = configured_flow("anthropic", &mut store, &dir, endpoints);
    assert!(flow.is_ok());
    let Some(mut flow) = flow.ok() else {
        return;
    };
    let observed = Arc::new(Mutex::new(None));
    let cancel = CancellationToken::new();
    let cancel_on_open = cancel.clone();
    let sink = Arc::clone(&observed);
    let progress = move |event| {
        if let LoginProgress::OpenUrl { url } = event {
            if let Ok(mut slot) = sink.lock() {
                *slot = callback_port(&url);
            }
            cancel_on_open.cancel();
        }
    };
    let result = flow.run(&progress, &cancel).await;
    assert!(matches!(result, Err(ProviderError::LoginCancelled)));
    drop(flow);
    drop(blocker);
    let advertised = observed.lock().ok().and_then(|slot| *slot);
    assert_ne!(
        advertised,
        Some(port),
        "the authorize URL must advertise the port actually bound, not the occupied one"
    );
    if let Some(advertised) = advertised {
        let rebound = TcpListener::bind(("127.0.0.1", advertised)).await;
        assert!(
            rebound.is_ok(),
            "advertised port {advertised} was not bound"
        );
    }
    assert!(!dir.auth_path().exists());
}

#[tokio::test]
async fn callback_state_mismatch_is_typed() {
    let bound = super::bind_callback(0).await;
    assert!(bound.is_ok());
    let Some((listener, port)) = bound.ok() else {
        return;
    };
    let mut callback = tokio::task::JoinSet::new();
    callback.spawn(callback_code(
        listener,
        "/callback",
        "expected-state",
        Family::Anthropic,
    ));
    let connection = TcpStream::connect(("127.0.0.1", port)).await;
    assert!(connection.is_ok());
    let Some(mut stream) = connection.ok() else {
        return;
    };
    let request =
        b"GET /callback?code=code123&state=wrong-state HTTP/1.1\r\nhost: localhost\r\n\r\n";
    let written = stream.write_all(request).await;
    assert!(written.is_ok());
    drop(stream);
    let result = callback.join_next().await.expect("callback completes");
    assert!(matches!(result, Ok(Err(ProviderError::StateMismatch))));
}

#[tokio::test]
async fn pasted_state_mismatch_does_not_write_auth_file() {
    let dir = TestDir::new();
    let blocker = occupied_callback_port().await;
    assert!(blocker.is_some());
    let Some((blocker, port)) = blocker else {
        return;
    };
    let endpoints = loopback_endpoints("http://127.0.0.1:1", 0, port);
    assert!(endpoints.is_some());
    let Some(endpoints) = endpoints else {
        return;
    };
    let mut store = AuthStore::empty(dir.auth_path());
    let flow = configured_flow("anthropic", &mut store, &dir, endpoints);
    assert!(flow.is_ok());
    let Some(mut flow) = flow.ok() else {
        return;
    };
    let sender = flow.take_paste_sender();
    assert!(sender.is_some());
    let paste = Arc::new(Mutex::new(sender));
    let paste_on_prompt = Arc::clone(&paste);
    let progress = move |event| {
        if let LoginProgress::OpenUrl { .. } = event
            && let Ok(mut sender) = paste_on_prompt.lock()
            && let Some(sender) = sender.take()
        {
            let _sent = sender.send(String::from("code123#wrong-state"));
        }
    };
    let result = flow.run(&progress, &CancellationToken::new()).await;
    assert!(matches!(result, Err(ProviderError::StateMismatch)));
    drop(flow);
    drop(blocker);
    assert!(store.credential("anthropic").is_none());
    assert!(!dir.auth_path().exists());
}

#[tokio::test]
async fn cancellation_and_timeout_release_callback_listener() {
    for cancelled in [false, true] {
        let dir = TestDir::new();
        let endpoints = loopback_endpoints("http://127.0.0.1:1", 0, 0);
        assert!(endpoints.is_some());
        let Some(endpoints) = endpoints else {
            return;
        };
        let mut store = AuthStore::empty(dir.auth_path());
        let flow = configured_flow("anthropic", &mut store, &dir, endpoints);
        assert!(flow.is_ok());
        let Some(mut flow) = flow.ok() else {
            return;
        };
        if !cancelled {
            flow.wait = Duration::from_millis(100);
        }
        let observed_port = Arc::new(Mutex::new(None));
        let port_sink = Arc::clone(&observed_port);
        let cancel = CancellationToken::new();
        let cancel_on_open = cancel.clone();
        let progress = move |event| {
            if let LoginProgress::OpenUrl { url } = event {
                if let Some(port) = callback_port(&url)
                    && let Ok(mut observed) = port_sink.lock()
                {
                    *observed = Some(port);
                }
                if cancelled {
                    cancel_on_open.cancel();
                }
            }
        };
        let result = flow.run(&progress, &cancel).await;
        if cancelled {
            assert!(matches!(result, Err(ProviderError::LoginCancelled)));
        } else {
            assert!(matches!(result, Err(ProviderError::LoginTimeout)));
        }
        drop(flow);
        let port = observed_port.lock().ok().and_then(|observed| *observed);
        assert!(port.is_some());
        if let Some(port) = port {
            let rebound = TcpListener::bind(("127.0.0.1", port)).await;
            assert!(rebound.is_ok());
        }
        assert!(!dir.auth_path().exists());
    }
}

#[tokio::test]
async fn occupied_codex_callback_falls_back_to_device_endpoint() {
    let server = mock_once(500, r#"{"error":"device unavailable"}"#).await;
    assert!(server.is_some());
    let Some((base, mut server)) = server else {
        return;
    };
    let blocker = occupied_callback_port().await;
    assert!(blocker.is_some());
    let Some((blocker, port)) = blocker else {
        return;
    };
    let endpoints = loopback_endpoints(base.as_str(), port, 0);
    assert!(endpoints.is_some());
    let Some(endpoints) = endpoints else {
        return;
    };
    let dir = TestDir::new();
    let mut store = AuthStore::empty(dir.auth_path());
    let flow = configured_flow("openai-codex", &mut store, &dir, endpoints);
    assert!(flow.is_ok());
    let Some(mut flow) = flow.ok() else {
        return;
    };
    let progress = |_| {};
    let result = flow.run(&progress, &CancellationToken::new()).await;
    assert!(matches!(
        result,
        Err(ProviderError::DeviceCode { status: 500, .. })
    ));
    drop(flow);
    drop(blocker);
    assert!(matches!(server.join_next().await, Some(Ok(_))));
    assert!(!dir.auth_path().exists());
}

#[tokio::test]
async fn token_endpoint_400_and_500_leave_auth_file_unchanged() {
    for status in [400, 500] {
        let server = mock_once(status, r#"{"error":"token rejected"}"#).await;
        assert!(server.is_some());
        let Some((base, mut server)) = server else {
            return;
        };
        let blocker = occupied_callback_port().await;
        assert!(blocker.is_some());
        let Some((blocker, port)) = blocker else {
            return;
        };
        let endpoints = loopback_endpoints(base.as_str(), 0, port);
        assert!(endpoints.is_some());
        let Some(endpoints) = endpoints else {
            return;
        };
        let dir = TestDir::new();
        let mut store = AuthStore::empty(dir.auth_path());
        let flow = configured_flow("anthropic", &mut store, &dir, endpoints);
        assert!(flow.is_ok());
        let Some(mut flow) = flow.ok() else {
            return;
        };
        let sender = flow.take_paste_sender();
        let paste = Arc::new(Mutex::new(sender));
        let paste_on_prompt = Arc::clone(&paste);
        let progress = move |event| {
            if let LoginProgress::OpenUrl { .. } = event
                && let Ok(mut sender) = paste_on_prompt.lock()
                && let Some(sender) = sender.take()
            {
                let _sent = sender.send(String::from("code123"));
            }
        };
        let result = flow.run(&progress, &CancellationToken::new()).await;
        assert!(matches!(
            &result,
            Err(ProviderError::TokenExchange {
                status: response_status,
                message,
            }) if *response_status == status && message.as_str() == "token rejected"
        ));
        if let Err(error) = &result {
            assert!(!error.to_string().contains("code123"));
        }
        drop(flow);
        drop(blocker);
        assert!(matches!(server.join_next().await, Some(Ok(_))));
        assert!(store.credential("anthropic").is_none());
        assert!(!dir.auth_path().exists());
    }
}

#[tokio::test]
async fn successful_claude_exchange_stores_complete_oauth_entry() {
    let server = mock_once(
        200,
        r#"{"access_token":"access","refresh_token":"refresh","expires_in":3600}"#,
    )
    .await;
    assert!(server.is_some());
    let Some((base, mut server)) = server else {
        return;
    };
    let blocker = occupied_callback_port().await;
    assert!(blocker.is_some());
    let Some((blocker, port)) = blocker else {
        return;
    };
    let endpoints = loopback_endpoints(base.as_str(), 0, port);
    assert!(endpoints.is_some());
    let Some(endpoints) = endpoints else {
        return;
    };
    let dir = TestDir::new();
    let mut store = AuthStore::empty(dir.auth_path());
    let flow = configured_flow("anthropic", &mut store, &dir, endpoints);
    assert!(flow.is_ok());
    let Some(mut flow) = flow.ok() else {
        return;
    };
    let sender = flow.take_paste_sender();
    let paste = Arc::new(Mutex::new(sender));
    let paste_on_prompt = Arc::clone(&paste);
    let progress = move |event| {
        if let LoginProgress::OpenUrl { .. } = event
            && let Ok(mut sender) = paste_on_prompt.lock()
            && let Some(sender) = sender.take()
        {
            let _sent = sender.send(String::from("code123"));
        }
    };
    let result = flow.run(&progress, &CancellationToken::new()).await;
    assert!(matches!(
        &result,
        Ok(Credential::OAuth(oauth))
            if oauth.access_token.expose() == "access"
                && oauth.refresh_token.expose() == "refresh"
                && oauth.expires_at.is_some()
    ));
    drop(flow);
    drop(blocker);
    assert!(matches!(server.join_next().await, Some(Ok(_))));
    assert!(dir.auth_path().is_file());
    let loaded = AuthStore::load(dir.auth_path());
    assert!(loaded.is_ok());
    let Some(loaded) = loaded.ok() else {
        return;
    };
    assert_eq!(loaded.credential("anthropic"), result.ok());
    assert_eq!(
        store.credential("anthropic"),
        loaded.credential("anthropic")
    );
}

#[tokio::test]
async fn codex_logout_revokes_refresh_token_and_removes_entry() {
    let server = mock_once(204, "").await;
    assert!(server.is_some());
    let Some((base, mut server)) = server else {
        return;
    };
    let endpoints = loopback_endpoints(base.as_str(), 0, 0);
    assert!(endpoints.is_some());
    let Some(endpoints) = endpoints else {
        return;
    };
    let dir = TestDir::new();
    let mut store = AuthStore::empty(dir.auth_path());
    let credential = Credential::OAuth(OAuthCredential {
        access_token: SecretString::from("access"),
        refresh_token: SecretString::from("refresh"),
        expires_at: Some(100),
        id_token: Some(String::from("id-token")),
        account_id: Some(String::from("account")),
    });
    assert!(store.set("openai-codex", credential).is_ok());
    assert!(store.store().is_ok());
    let result = super::logout_with(
        "openai-codex",
        &mut store,
        "dalgon/test (linux test; x86_64)",
        &endpoints,
    )
    .await;
    assert!(result.is_ok());
    assert!(store.credential("openai-codex").is_none());
    let request_result = server.join_next().await.expect("server completes");
    assert!(
        request_result
            .as_ref()
            .is_ok_and(std::option::Option::is_some)
    );
    let Some(request) = request_result.ok().flatten() else {
        return;
    };
    let request = String::from_utf8(request);
    assert!(request.is_ok());
    let Some(request) = request.ok() else {
        return;
    };
    assert!(request.starts_with("POST /oauth/revoke HTTP/1.1\r\n"));
    let body = request.split_once("\r\n\r\n").map_or("", |(_, body)| body);
    assert!(body.contains("\"token\":\"refresh\""));
    assert!(body.contains("\"token_type_hint\":\"refresh_token\""));
    assert!(body.contains("app_EMoamEEZ73f0CkXaXp7hrann"));
    let loaded = AuthStore::load(dir.auth_path());
    assert!(loaded.is_ok());
    let Some(loaded) = loaded.ok() else {
        return;
    };
    assert!(loaded.credential("openai-codex").is_none());
}

#[tokio::test]
async fn repeated_logout_without_entry_makes_no_revoke_request() {
    let listener = TcpListener::bind("127.0.0.1:0").await;
    assert!(listener.is_ok());
    let Some(listener) = listener.ok() else {
        return;
    };
    let address = listener.local_addr();
    assert!(address.is_ok());
    let Some(address) = address.ok() else {
        return;
    };
    let base = Url::parse(&format!("http://{address}"));
    assert!(base.is_ok());
    let Some(base) = base.ok() else {
        return;
    };
    let endpoints = loopback_endpoints(base.as_str(), 0, 0);
    assert!(endpoints.is_some());
    let Some(endpoints) = endpoints else {
        return;
    };
    let dir = TestDir::new();
    let mut store = AuthStore::empty(dir.auth_path());
    for _ in 0..2 {
        let result = super::logout_with(
            "openai-codex",
            &mut store,
            "dalgon/test (linux test; x86_64)",
            &endpoints,
        )
        .await;
        assert!(result.is_ok());
    }
    let accepted = tokio::time::timeout(Duration::from_millis(25), listener.accept()).await;
    assert!(accepted.is_err());
    assert!(!dir.auth_path().exists());
}

#[tokio::test]
async fn oauth_307_and_308_do_not_forward_codes_or_verifiers() {
    for status in [307, 308] {
        let server = mock_redirect(status).await;
        assert!(server.is_some());
        let Some((base, mut server, target)) = server else {
            return;
        };
        let blocker = occupied_callback_port().await;
        assert!(blocker.is_some());
        let Some((blocker, port)) = blocker else {
            return;
        };
        let endpoints = loopback_endpoints(base.as_str(), 0, port);
        assert!(endpoints.is_some());
        let Some(endpoints) = endpoints else {
            return;
        };
        let dir = TestDir::new();
        let mut store = AuthStore::empty(dir.auth_path());
        let flow = configured_flow("anthropic", &mut store, &dir, endpoints);
        assert!(flow.is_ok());
        let Some(mut flow) = flow.ok() else {
            return;
        };
        let sender = flow.take_paste_sender();
        let paste_on_prompt = Arc::new(Mutex::new(sender));
        let paste = Arc::clone(&paste_on_prompt);
        let progress = move |event| {
            if let LoginProgress::OpenUrl { .. } = event
                && let Ok(mut sender) = paste.lock()
                && let Some(sender) = sender.take()
            {
                let _sent = sender.send(String::from("code123"));
            }
        };
        let result = flow.run(&progress, &CancellationToken::new()).await;
        assert!(matches!(
            &result,
            Err(ProviderError::TokenExchange {
                status: response_status,
                ..
            }) if *response_status == status
        ));
        drop(flow);
        drop(blocker);
        assert!(store.credential("anthropic").is_none());
        assert!(!dir.auth_path().exists());

        let request_result = server.join_next().await.expect("server completes");
        assert!(
            request_result
                .as_ref()
                .is_ok_and(std::option::Option::is_some)
        );
        let Some(request) = request_result.ok().flatten() else {
            return;
        };
        let request = String::from_utf8(request);
        assert!(request.is_ok());
        let Some(request) = request.ok() else {
            return;
        };
        let body = request.split_once("\r\n\r\n").map_or("", |(_, body)| body);
        assert!(body.contains("\"code\":\"code123\""));
        assert!(body.contains("\"code_verifier\":\""));

        let redirected = tokio::time::timeout(Duration::from_millis(50), target.accept()).await;
        assert!(redirected.is_err());
    }
}

#[tokio::test]
async fn oauth_revoke_redirect_does_not_forward_refresh_token() {
    let server = mock_redirect(307).await;
    assert!(server.is_some());
    let Some((base, mut server, target)) = server else {
        return;
    };
    let endpoints = loopback_endpoints(base.as_str(), 0, 0);
    assert!(endpoints.is_some());
    let Some(endpoints) = endpoints else {
        return;
    };
    let dir = TestDir::new();
    let mut store = AuthStore::empty(dir.auth_path());
    let credential = Credential::OAuth(OAuthCredential {
        access_token: SecretString::from("access"),
        refresh_token: SecretString::from("refresh"),
        expires_at: Some(100),
        id_token: Some(String::from("id-token")),
        account_id: Some(String::from("account")),
    });
    assert!(store.set("openai-codex", credential).is_ok());
    assert!(store.store().is_ok());
    let result = super::logout_with(
        "openai-codex",
        &mut store,
        "dalgon/test (linux test; x86_64)",
        &endpoints,
    )
    .await;
    assert!(result.is_ok());
    assert!(store.credential("openai-codex").is_none());

    let request_result = server.join_next().await.expect("server completes");
    assert!(
        request_result
            .as_ref()
            .is_ok_and(std::option::Option::is_some)
    );
    let Some(request) = request_result.ok().flatten() else {
        return;
    };
    let request = String::from_utf8(request);
    assert!(request.is_ok());
    let Some(request) = request.ok() else {
        return;
    };
    assert!(request.starts_with("POST /oauth/revoke HTTP/1.1\r\n"));
    let body = request.split_once("\r\n\r\n").map_or("", |(_, body)| body);
    assert!(body.contains("\"token\":\"refresh\""));
    assert!(body.contains("\"token_type_hint\":\"refresh_token\""));
    let redirected = tokio::time::timeout(Duration::from_millis(50), target.accept()).await;
    assert!(redirected.is_err());
}

#[tokio::test]
async fn device_poll_redirect_does_not_forward_user_code() {
    let server = mock_device_poll_redirect().await;
    assert!(server.is_some());
    let Some((base, mut server, target)) = server else {
        return;
    };
    let blocker = occupied_callback_port().await;
    assert!(blocker.is_some());
    let Some((blocker, port)) = blocker else {
        return;
    };
    let endpoints = loopback_endpoints(base.as_str(), port, 0);
    assert!(endpoints.is_some());
    let Some(endpoints) = endpoints else {
        return;
    };
    let dir = TestDir::new();
    let mut store = AuthStore::empty(dir.auth_path());
    let flow = configured_flow("openai-codex", &mut store, &dir, endpoints);
    assert!(flow.is_ok());
    let Some(mut flow) = flow.ok() else {
        return;
    };
    let shown_code = Arc::new(Mutex::new(None));
    let code_sink = Arc::clone(&shown_code);
    let progress = move |event| {
        if let LoginProgress::ShowCode { code, .. } = event
            && let Ok(mut shown_code) = code_sink.lock()
        {
            *shown_code = Some(code);
        }
    };
    let result = flow.run(&progress, &CancellationToken::new()).await;
    assert!(matches!(
        result,
        Err(ProviderError::DeviceCode { status: 307, .. })
    ));
    drop(flow);
    drop(blocker);
    assert!(store.credential("openai-codex").is_none());
    assert!(!dir.auth_path().exists());

    let server_result = tokio::time::timeout(Duration::from_secs(3), server.join_next()).await;
    assert!(server_result.is_ok());
    let Some(Some(Ok(Some(requests)))) = server_result.ok() else {
        return;
    };
    assert_eq!(requests.len(), 2);
    let poll_request_bytes = requests.into_iter().nth(1);
    assert!(poll_request_bytes.is_some());
    let Some(poll_request_bytes) = poll_request_bytes else {
        return;
    };
    let poll_request = String::from_utf8(poll_request_bytes);
    assert!(poll_request.is_ok());
    let Some(poll_request) = poll_request.ok() else {
        return;
    };
    assert!(poll_request.contains("\"user_code\":\"ABCD\""));
    assert!(
        shown_code
            .lock()
            .is_ok_and(|code| code.as_deref() == Some("ABCD"))
    );
    let redirected = tokio::time::timeout(Duration::from_millis(50), target.accept()).await;
    assert!(redirected.is_err());
}

#[tokio::test]
async fn explicit_device_auth_shows_code_without_opening_browser_on_free_callback_port() {
    let server = mock_device_poll_redirect().await;
    assert!(server.is_some());
    let Some((base, mut server, target)) = server else {
        return;
    };
    let callback = TcpListener::bind("127.0.0.1:0").await;
    assert!(callback.is_ok());
    let Some(callback) = callback.ok() else {
        return;
    };
    let callback_port = callback.local_addr().ok().map(|address| address.port());
    assert!(callback_port.is_some());
    let Some(callback_port) = callback_port else {
        return;
    };
    drop(callback);
    let endpoints = loopback_endpoints(base.as_str(), callback_port, 0);
    assert!(endpoints.is_some());
    let Some(endpoints) = endpoints else {
        return;
    };
    let dir = TestDir::new();
    let mut store = AuthStore::empty(dir.auth_path());
    let flow = configured_flow("openai-codex", &mut store, &dir, endpoints);
    assert!(flow.is_ok());
    let Some(flow) = flow.ok() else {
        return;
    };
    let mut flow = flow.with_device_auth();
    let shown_code = Arc::new(Mutex::new(None));
    let shown_sink = Arc::clone(&shown_code);
    let opened_url = Arc::new(Mutex::new(None));
    let opened_sink = Arc::clone(&opened_url);
    let progress = move |event| match event {
        LoginProgress::ShowCode { code, .. } => {
            if let Ok(mut shown_code) = shown_sink.lock() {
                *shown_code = Some(code);
            }
        }
        LoginProgress::OpenUrl { url } => {
            if let Ok(mut opened_url) = opened_sink.lock() {
                *opened_url = Some(url);
            }
        }
        LoginProgress::AskPaste { .. } | LoginProgress::Exchanging => {}
    };

    let result = tokio::time::timeout(
        Duration::from_secs(3),
        flow.run(&progress, &CancellationToken::new()),
    )
    .await;
    assert!(matches!(
        result,
        Ok(Err(ProviderError::DeviceCode { status: 307, .. }))
    ));
    assert!(
        shown_code
            .lock()
            .is_ok_and(|code| code.as_deref() == Some("ABCD"))
    );
    assert!(opened_url.lock().is_ok_and(|url| url.is_none()));
    drop(flow);

    let server_result = tokio::time::timeout(Duration::from_secs(3), server.join_next()).await;
    assert!(server_result.is_ok());
    let Some(Ok(Some(requests))) = server_result.ok().flatten() else {
        return;
    };
    assert_eq!(requests.len(), 2);
    let redirected = tokio::time::timeout(Duration::from_millis(50), target.accept()).await;
    assert!(redirected.is_err());
}

#[test]
fn authorization_url_scopes_use_each_wire_encoding() {
    let claude_base = Url::parse("https://claude.ai/oauth/authorize");
    assert!(claude_base.is_ok());
    let Some(claude_base) = claude_base.ok() else {
        return;
    };
    let claude = super::authorize_url(
        Family::Anthropic,
        &crate::find("anthropic")
            .and_then(|def| def.oauth)
            .expect("claude row"),
        &claude_base,
        "http://localhost:53692/callback",
        "challenge",
        "state",
    );
    assert!(claude.is_ok());
    let Some(claude) = claude.ok() else {
        return;
    };
    let claude_query = claude.query().unwrap_or_default();
    assert!(claude_query.contains(
            "scope=org%3Acreate_api_key+user%3Aprofile+user%3Ainference+user%3Asessions%3Aclaude_code+user%3Amcp_servers+user%3Afile_upload"
        ));
    assert!(!claude_query.contains("user%3Amcp_servers%3Aclaude_code"));

    let codex_base = Url::parse("https://auth.openai.com/oauth/authorize");
    assert!(codex_base.is_ok());
    let Some(codex_base) = codex_base.ok() else {
        return;
    };
    let codex = super::authorize_url(
        Family::Codex,
        &crate::find("openai-codex")
            .and_then(|def| def.oauth)
            .expect("codex row"),
        &codex_base,
        "http://localhost:1455/auth/callback",
        "challenge",
        "state",
    );
    assert!(codex.is_ok());
    let Some(codex) = codex.ok() else {
        return;
    };
    let codex_query = codex.query().unwrap_or_default();
    assert!(codex_query.contains(
            "scope=openid%20profile%20email%20offline_access%20api.connectors.read%20api.connectors.invoke"
        ));
}

#[test]
fn authorization_urls_keep_each_vendor_parameter_order_and_encoding() {
    let url = |id: &str, base: &str, redirect: &str| {
        let family = crate::find(id).map(|def| def.family).expect("table row");
        let oauth = crate::find(id)
            .and_then(|def| def.oauth)
            .expect("oauth row");
        super::authorize_url(
            family,
            &oauth,
            &Url::parse(base).expect("base"),
            redirect,
            "challenge",
            "state",
        )
        .expect("authorize url")
        .to_string()
    };
    assert_eq!(
        url(
            "anthropic",
            "https://claude.ai/oauth/authorize",
            "http://localhost:53692/callback"
        ),
        "https://claude.ai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A53692%2Fcallback&scope=org%3Acreate_api_key+user%3Aprofile+user%3Ainference+user%3Asessions%3Aclaude_code+user%3Amcp_servers+user%3Afile_upload&code_challenge=challenge&code_challenge_method=S256&state=state"
    );
    assert_eq!(
        url(
            "openai-codex",
            "https://auth.openai.com/oauth/authorize",
            "http://localhost:1455/auth/callback"
        ),
        "https://auth.openai.com/oauth/authorize?response_type=code&client_id=app_EMoamEEZ73f0CkXaXp7hrann&redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback&scope=openid%20profile%20email%20offline_access%20api.connectors.read%20api.connectors.invoke&code_challenge=challenge&code_challenge_method=S256&state=state&id_token_add_organizations=true&codex_cli_simplified_flow=true&originator=dalgon"
    );
}

#[test]
fn login_run_future_is_send_for_rpc_handlers() {
    fn assert_send<T: Send>(_: T) {}
    let dir = TestDir::new();
    let mut store = AuthStore::empty(dir.auth_path());
    let mut flow = LoginFlow::new(
        "openai-codex",
        &mut store,
        Client::new(),
        "dalgon/test",
        dir.cache_dir(),
    )
    .expect("test flow builds");
    let progress = |_: LoginProgress| {};
    assert_send(flow.run(&progress, &CancellationToken::new()));
}
