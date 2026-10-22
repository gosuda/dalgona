use super::{
    LoginEndpoints, LoginFlow, LoginProgress, PASTE_HINT, callback_code, parse_pasted_code,
    pkce_challenge,
};
use crate::{AuthStore, Credential, EnvSnapshot, OAuthCredential, ProviderError, SecretString};
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
    LoginEndpoints::loopback(base, &EnvSnapshot::default())
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
        endpoints,
    )
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
    let result = parse_pasted_code("code123#wrong-state", "right-state", Family::Anthropic);
    assert!(matches!(result, Err(crate::ProviderError::StateMismatch)));
}

#[test]
fn pasted_redirect_and_query_are_accepted() {
    assert_eq!(
        parse_pasted_code(
            "http://localhost/callback?code=code123&state=state",
            "state",
            Family::Anthropic
        )
        .ok()
        .as_deref(),
        Some("code123")
    );
    assert_eq!(
        parse_pasted_code("code123", "state", Family::Anthropic)
            .ok()
            .as_deref(),
        Some("code123")
    );
    assert_eq!(
        PASTE_HINT,
        "Paste the redirect URL or the code shown in the browser."
    );
}

#[test]
fn a_pasted_result_needs_a_matching_state_and_a_code_in_every_form() {
    let parse = |input: &str| parse_pasted_code(input, "right-state", Family::Anthropic);
    for (input, code) in [
        ("code123#right-state", "code123"),
        (
            "http://localhost/callback?code=code123&state=right-state",
            "code123",
        ),
        ("?code=code123&state=right-state", "code123"),
    ] {
        assert_eq!(parse(input).ok().as_deref(), Some(code), "{input}");
    }
    // A wrong or absent state is refused whichever form carried it: the state
    // is the only proof that the pasted result answers this login.
    for input in [
        "code123#wrong-state",
        "code123#",
        "http://localhost/callback?code=code123&state=wrong-state",
        "http://localhost/callback?code=code123",
        "?code=code123&state=wrong-state",
        "code=code123&state=wrong-state",
    ] {
        let result = parse(input);
        assert!(
            matches!(result, Err(ProviderError::StateMismatch)),
            "{input}: {result:?}"
        );
    }
    // A matching state without a code, or a blank paste, is no login either.
    for input in [
        "#right-state",
        "http://localhost/callback?state=right-state",
        "http://localhost/callback?code=&state=right-state",
        "   ",
    ] {
        let result = parse(input);
        assert!(
            matches!(result, Err(ProviderError::Transport { .. })),
            "{input}: {result:?}"
        );
    }
}

#[test]
fn an_endpoint_override_must_name_exactly_a_loopback_host() {
    let env = EnvSnapshot::default();
    for base in [
        "http://127.0.0.1:8123",
        "http://localhost:8123",
        "http://[::1]:8123",
        "HTTP://LOCALHOST:8123",
    ] {
        assert!(
            LoginEndpoints::loopback(base, &env).is_ok(),
            "{base} is a loopback host"
        );
    }
    // Host confusion: the override receives authorization codes and tokens,
    // so a name that only starts or ends like a loopback host must be refused.
    // A plain-HTTP base is stopped by the transport rule first.
    for base in [
        "http://localhost.evil.test:8123",
        "http://127.0.0.1.evil.test:8123",
        "http://notlocalhost:8123",
        "http://localhost@evil.test:8123",
        "http://127.0.0.1@evil.test:8123",
        "http://0.0.0.0:8123",
    ] {
        let result = LoginEndpoints::loopback(base, &env);
        assert!(
            matches!(
                result,
                Err(ProviderError::PlainHttp { .. } | ProviderError::Transport { .. })
            ),
            "{base} is not a loopback host: {:?}",
            result.err()
        );
    }
    // An https base passes the transport rule, so only the loopback test
    // itself stands between a look-alike host and the override.
    for base in [
        "https://example.test",
        "https://localhost.evil.test:8123",
        "https://127.0.0.1.evil.test:8123",
        "https://notlocalhost:8123",
        "https://0.0.0.0:8123",
    ] {
        let result = LoginEndpoints::loopback(base, &env);
        assert!(
            matches!(result, Err(ProviderError::Transport { .. })),
            "{base} is not a loopback host: {:?}",
            result.err()
        );
    }
}

#[test]
fn endpoint_override_rejects_non_loopback_origins() {
    let env = EnvSnapshot::default();
    assert!(LoginEndpoints::loopback("http://127.0.0.1:8123", &env).is_ok());
    assert!(LoginEndpoints::loopback("http://example.com", &env).is_err());
}

/// Occupies one loopback port to stand in for the preferred callback port.
async fn held_loopback_port() -> (TcpListener, u16) {
    let holder = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("loopback bind succeeds");
    let port = holder.local_addr().expect("bound address").port();
    (holder, port)
}

/// Binds `address`, retrying briefly while a stray loopback connection still
/// holds the port. The invariant is that the owner released it; the wait only
/// covers the release-to-close window a connecting peer can hold open after
/// the listener drops.
async fn bind_soon(address: (&str, u16)) -> std::io::Result<TcpListener> {
    let mut delay = Duration::from_millis(10);
    for _ in 0..20 {
        match TcpListener::bind(address).await {
            Ok(listener) => return Ok(listener),
            Err(error) => {
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_millis(160));
                if delay >= Duration::from_secs(2) {
                    return Err(error);
                }
            }
        }
    }
    TcpListener::bind(address).await
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
        let rebound = bind_soon(("127.0.0.1", advertised)).await;
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
            let rebound = bind_soon(("127.0.0.1", port)).await;
            assert!(rebound.is_ok(), "released callback port was not bound");
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
    assert!(
        matches!(result, Err(ProviderError::DeviceCode { status: 307, .. })),
        "expected DeviceCode {{ status: 307, .. }}, got: {result:?}"
    );
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

    // The specified Codex poll cadence waits the server interval plus the
    // three-second safety margin before the first poll, so the window must
    // cover the mock's one-second interval plus that margin.
    let result = tokio::time::timeout(
        Duration::from_secs(15),
        flow.run(&progress, &CancellationToken::new()),
    )
    .await;
    assert!(
        matches!(
            result,
            Ok(Err(ProviderError::DeviceCode { status: 307, .. }))
        ),
        "expected Ok(Err(DeviceCode {{ status: 307, .. }})), got: {result:?}"
    );
    assert!(
        shown_code
            .lock()
            .is_ok_and(|code| code.as_deref() == Some("ABCD"))
    );
    assert!(opened_url.lock().is_ok_and(|url| url.is_none()));
    drop(flow);

    let server_result = tokio::time::timeout(Duration::from_secs(15), server.join_next()).await;
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
    let claude_row = crate::find("anthropic")
        .and_then(|def| def.oauth)
        .expect("claude row");
    let claude = super::authorize_url(
        Family::Anthropic,
        &claude_row,
        claude_row.client_id,
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
    let codex_row = crate::find("openai-codex")
        .and_then(|def| def.oauth)
        .expect("codex row");
    let codex = super::authorize_url(
        Family::Codex,
        &codex_row,
        codex_row.client_id,
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
            oauth.client_id,
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
    for provider in ["openai-codex", "factory-droid", "gitlab-duo"] {
        let mut store = AuthStore::empty(dir.auth_path());
        let mut flow = LoginFlow::new(
            provider,
            &mut store,
            Client::new(),
            "dalgon/test",
            dir.cache_dir(),
            LoginEndpoints::production(&EnvSnapshot::default()).expect("production endpoints"),
        )
        .expect("test flow builds");
        let progress = |_: LoginProgress| {};
        assert_send(flow.run(&progress, &CancellationToken::new()));
    }
}

#[tokio::test]
async fn cancelling_after_the_commit_signal_while_the_auth_lock_is_held_writes_nothing() {
    let (base, mut server) = mock_once(
        200,
        r#"{"access_token":"access","refresh_token":"refresh","expires_in":3600}"#,
    )
    .await
    .expect("token server starts");
    let endpoints = loopback_endpoints(base.as_str(), 0, 0).expect("loopback endpoints");
    let dir = TestDir::new();
    let path = dir.auth_path();
    let mut store = AuthStore::empty(&path);
    store
        .set(
            "openai",
            Credential::ApiKey {
                key: SecretString::from("seed"),
            },
        )
        .expect("seed credential");
    store.store().expect("save seed");
    let before = fs::read(&path).expect("seed bytes");
    let mut flow = configured_flow("anthropic", &mut store, &dir, endpoints).expect("flow builds");
    let paste = Mutex::new(flow.take_paste_sender());
    let progress = |event| {
        if let LoginProgress::OpenUrl { .. } = event
            && let Some(sender) = paste.lock().expect("paste lock").take()
        {
            sender.send(String::from("code123")).expect("deliver code");
        }
    };
    let (commit_ready, commit_signal) = tokio::sync::oneshot::channel();
    flow.commit_ready = Some(commit_ready);
    let cancel = CancellationToken::new();
    let lock = crate::auth::refresh::lock_auth_file(&path)
        .await
        .expect("hold the auth lock");
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let (result, ()) = tokio::join!(flow.run(&progress, &cancel), async {
            commit_signal.await.expect("exchange reaches commit");
            cancel.cancel();
            drop(lock);
        });
        result
    })
    .await
    .expect("login settles without a leaked lock");
    drop(flow);
    assert!(matches!(server.join_next().await, Some(Ok(Some(_)))));
    assert!(
        matches!(result, Err(ProviderError::LoginCancelled)),
        "cancelled commit must fail: {result:?}"
    );
    assert_eq!(fs::read(&path).expect("auth bytes"), before);
    assert!(store.credential("anthropic").is_none());
}

#[cfg(unix)]
static DIR_SYNC_FAULTS: std::sync::LazyLock<Mutex<Vec<PathBuf>>> =
    std::sync::LazyLock::new(|| Mutex::new(Vec::new()));

#[cfg(unix)]
pub(super) fn take_dir_sync_fault(path: &std::path::Path) -> bool {
    let mut faults = DIR_SYNC_FAULTS.lock().expect("fault lock");
    let Some(index) = faults.iter().position(|fault| fault == path) else {
        return false;
    };
    faults.swap_remove(index);
    true
}

#[cfg(unix)]
struct DirSyncFault(PathBuf);

#[cfg(unix)]
impl DirSyncFault {
    fn arm(path: PathBuf) -> Self {
        DIR_SYNC_FAULTS
            .lock()
            .expect("fault lock")
            .push(path.clone());
        Self(path)
    }
}

#[cfg(unix)]
impl Drop for DirSyncFault {
    fn drop(&mut self) {
        DIR_SYNC_FAULTS
            .lock()
            .expect("fault lock")
            .retain(|path| path != &self.0);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn logout_of_the_last_credential_reports_a_failed_directory_sync() {
    let dir = TestDir::new();
    let path = dir.auth_path();
    let mut store = AuthStore::empty(&path);
    store
        .set(
            "openai",
            Credential::ApiKey {
                key: SecretString::from("seed"),
            },
        )
        .expect("seed credential");
    store.store().expect("save seed");
    let _fault = DirSyncFault::arm(path.clone());
    let result = super::logout("openai", &mut store, &EnvSnapshot::default()).await;
    assert!(
        matches!(result, Err(ProviderError::AuthWrite { .. })),
        "directory sync failure must not report successful logout: {result:?}"
    );
    assert!(!path.exists(), "unlink precedes the failed sync");
    assert!(store.credential("openai").is_some());
    assert!(
        AuthStore::load(&path)
            .expect("reload store")
            .credential("openai")
            .is_none()
    );
}

/// One answer the route server gives to a request line without its version.
struct Route {
    key: &'static str,
    status: u16,
    body: String,
    /// Read the request and never answer it.
    hold: bool,
    /// Answer with the request body quoted into an `error_description`.
    echo: bool,
}

impl Route {
    fn json(key: &'static str, status: u16, body: impl Into<String>) -> Self {
        Self {
            key,
            status,
            body: body.into(),
            hold: false,
            echo: false,
        }
    }

    /// Answers `{"error_description":"<the request body>"}`, so an exchange
    /// failure can quote exactly what was sent.
    fn json_echo(key: &'static str, status: u16) -> Self {
        Self {
            key,
            status,
            body: String::new(),
            hold: false,
            echo: true,
        }
    }

    fn held(key: &'static str) -> Self {
        Self {
            key,
            status: 200,
            body: String::from("{}"),
            hold: true,
            echo: false,
        }
    }
}

/// One request as the route server saw it.
#[derive(Clone)]
struct Seen {
    key: String,
    head: String,
    body: String,
    /// The `auth.json` bytes at the moment the request arrived; `None` while
    /// the file did not exist.
    auth: Option<Vec<u8>>,
}

/// A loopback server that answers by request line and records every request.
/// Its one task is owned by the `JoinSet`, which aborts it on drop.
struct RouteServer {
    base: Url,
    seen: Arc<Mutex<Vec<Seen>>>,
    _task: tokio::task::JoinSet<()>,
}

async fn write_json(stream: &mut TcpStream, status: u16, body: &str) {
    let head = format!(
        "HTTP/1.1 {status} Test\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    let _head = stream.write_all(head.as_bytes()).await;
    let _body = stream.write_all(body.as_bytes()).await;
}

async fn route_server(routes: Vec<Route>, auth_path: PathBuf) -> RouteServer {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("route server binds");
    let address = listener.local_addr().expect("route server address");
    let base = Url::parse(&format!("http://{address}")).expect("route server base");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    let mut task = tokio::task::JoinSet::new();
    task.spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let Some(request) = read_test_request(&mut stream).await else {
                continue;
            };
            let text = String::from_utf8_lossy(&request).into_owned();
            let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_str(), ""));
            let key = head
                .lines()
                .next()
                .and_then(|line| line.rsplit_once(' '))
                .map_or("", |(key, _version)| key);
            record.lock().expect("seen lock").push(Seen {
                key: key.to_owned(),
                head: head.to_owned(),
                body: body.to_owned(),
                auth: fs::read(&auth_path).ok(),
            });
            let Some(route) = routes.iter().find(|route| route.key == key) else {
                write_json(&mut stream, 404, "{}").await;
                continue;
            };
            if route.hold {
                std::future::pending::<()>().await;
            }
            let body = if route.echo {
                format!("{{\"error\":\"invalid_grant\",\"error_description\":\"{body}\"}}")
            } else {
                route.body.clone()
            };
            write_json(&mut stream, route.status, &body).await;
        }
    });
    RouteServer {
        base,
        seen,
        _task: task,
    }
}

impl RouteServer {
    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().expect("seen lock").clone()
    }

    fn keys(&self) -> Vec<String> {
        self.seen().into_iter().map(|request| request.key).collect()
    }

    /// The one request made to `key`.
    fn only(&self, key: &str) -> Seen {
        let mut matching: Vec<Seen> = self
            .seen()
            .into_iter()
            .filter(|request| request.key == key)
            .collect();
        assert_eq!(
            matching.len(),
            1,
            "{key} must be requested exactly once: {:?}",
            self.keys()
        );
        matching.remove(0)
    }

    /// Waits until a request for `key` has arrived; callers bound it with a
    /// timeout.
    async fn wait_for(&self, key: &str) {
        while !self.keys().iter().any(|seen| seen == key) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}

fn form_fields(body: &str) -> Vec<(String, String)> {
    url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect()
}

fn field<'a>(fields: &'a [(String, String)], name: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

fn query_fields(url: &str) -> Vec<(String, String)> {
    Url::parse(url)
        .expect("authorize URL parses")
        .query_pairs()
        .into_owned()
        .collect()
}

fn is_form(request: &Seen) -> bool {
    request
        .head
        .to_ascii_lowercase()
        .contains("content-type: application/x-www-form-urlencoded")
}

fn pinned_client_id(provider: &str) -> &'static str {
    crate::find(provider)
        .and_then(|def| def.oauth)
        .map(|oauth| oauth.client_id)
        .expect("provider with OAuth")
}

/// Stores one API key, so the file exists before a failing login, and returns
/// its bytes.
fn seed_openai_key(store: &mut AuthStore) -> Vec<u8> {
    store
        .set(
            "openai",
            Credential::ApiKey {
                key: SecretString::from("seed"),
            },
        )
        .expect("seed credential");
    store.store().expect("save seed");
    fs::read(store.path()).expect("seed bytes")
}

/// An existing file keeps its exact bytes; an absent one stays absent.
fn assert_store_untouched(dir: &TestDir, before: Option<&[u8]>) {
    match before {
        Some(bytes) => assert_eq!(fs::read(dir.auth_path()).expect("auth bytes"), bytes),
        None => assert!(!dir.auth_path().exists(), "an absent store stays absent"),
    }
}

async fn free_loopback_port() -> u16 {
    let (holder, port) = held_loopback_port().await;
    drop(holder);
    port
}

/// Plays the browser: one `GET` to the loopback callback, returning the
/// status line.
async fn browser_get(port: u16, target: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("callback listener accepts");
    let request = format!("GET {target} HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("callback request written");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("callback response read");
    String::from_utf8_lossy(&response)
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned()
}

fn opened_url(event: &LoginProgress) -> Option<String> {
    match event {
        LoginProgress::OpenUrl { url } => Some(url.clone()),
        LoginProgress::ShowCode { .. }
        | LoginProgress::AskPaste { .. }
        | LoginProgress::Exchanging => None,
    }
}

const WORKOS_START: &str = "POST /workos/user_management/authorize/device";
const WORKOS_POLL: &str = "POST /workos/user_management/authenticate";
const WORKOS_WHOAMI: &str = "GET /workos/api/cli/whoami";
const GITLAB_TOKEN: &str = "POST /gitlab/oauth/token";
const GITLAB_REVOKE: &str = "POST /gitlab/oauth/revoke";
const WORKOS_DEVICE: &str = r#"{"device_code":"device-code-secret","user_code":"WDJB-MJHT","verification_uri":"https://workos.example/device","verification_uri_complete":"https://workos.example/device?user_code=WDJB-MJHT","expires_in":300,"interval":1}"#;
const WORKOS_TOKENS: &str =
    r#"{"access_token":"workos-access","refresh_token":"workos-refresh","expires_in":3600}"#;

fn workos_routes(whoami: Route) -> Vec<Route> {
    vec![
        Route::json(WORKOS_START, 200, WORKOS_DEVICE),
        Route::json(WORKOS_POLL, 200, WORKOS_TOKENS),
        whoami,
    ]
}

/// What `auth.json` holds before a sign-in starts.
#[derive(Clone, Copy, Eq, PartialEq)]
enum StartingStore {
    /// No file at all.
    Absent,
    /// A file holding one API key.
    Seeded,
}

/// One finished Factory Droid sign-in against a route server.
struct DroidRun {
    result: Result<Credential, ProviderError>,
    server: RouteServer,
    dir: TestDir,
    store: AuthStore,
    /// The seed file's bytes when the run began with an existing store.
    before: Option<Vec<u8>>,
}

/// Signs in to Factory Droid without asking for the device flow: the row
/// offers no browser sign-in, so the flow itself must pick the device grant.
async fn droid_login(whoami: Route, start: StartingStore) -> DroidRun {
    let dir = TestDir::new();
    let server = route_server(workos_routes(whoami), dir.auth_path()).await;
    let mut store = AuthStore::empty(dir.auth_path());
    let before = (start == StartingStore::Seeded).then(|| seed_openai_key(&mut store));
    let endpoints = LoginEndpoints::loopback(server.base.as_str(), &EnvSnapshot::default())
        .expect("loopback endpoints");
    let mut flow =
        configured_flow("factory-droid", &mut store, &dir, endpoints).expect("droid flow");
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        flow.run(&|_: LoginProgress| {}, &CancellationToken::new()),
    )
    .await
    .expect("login settles");
    drop(flow);
    DroidRun {
        result,
        server,
        dir,
        store,
        before,
    }
}

#[tokio::test]
async fn workos_tokens_are_verified_then_stored_without_a_second_exchange() {
    for start in [StartingStore::Absent, StartingStore::Seeded] {
        let started = super::unix_now();
        let run = droid_login(
            Route::json(WORKOS_WHOAMI, 200, r#"{"orgId":"org_1"}"#),
            start,
        )
        .await;
        let finished = super::unix_now();
        let credential = run.result.expect("device tokens complete the login");
        let Credential::OAuth(oauth) = &credential else {
            panic!("a device login stores an OAuth credential: {credential:?}");
        };
        assert_eq!(oauth.access_token.expose(), "workos-access");
        assert_eq!(oauth.refresh_token.expose(), "workos-refresh");
        assert_eq!(oauth.id_token, None);
        assert_eq!(oauth.account_id, None);
        let expires_at = oauth.expires_at.expect("expires_in sets an expiry");
        assert!((started + 3600..=finished + 3600).contains(&expires_at));

        // The poll delivered the tokens: nothing is exchanged afterwards, and
        // the one extra request is the session check.
        assert_eq!(
            run.server.keys(),
            [WORKOS_START, WORKOS_POLL, WORKOS_WHOAMI]
        );
        let device_start = run.server.only(WORKOS_START);
        assert!(is_form(&device_start), "device start is form-encoded");
        let start_fields = form_fields(&device_start.body);
        assert_eq!(
            field(&start_fields, "client_id"),
            Some(pinned_client_id("factory-droid"))
        );
        let poll = run.server.only(WORKOS_POLL);
        assert!(is_form(&poll), "device poll is form-encoded");
        let poll_fields = form_fields(&poll.body);
        assert_eq!(
            field(&poll_fields, "grant_type"),
            Some("urn:ietf:params:oauth:grant-type:device_code")
        );
        assert_eq!(
            field(&poll_fields, "device_code"),
            Some("device-code-secret")
        );
        assert!(
            field(&poll_fields, "code").is_none() && field(&poll_fields, "code_verifier").is_none(),
            "no authorization-code exchange is built for a direct grant"
        );

        // Verification precedes persistence: when whoami arrived the file was
        // still the seed (or absent), and the bearer was the new access token.
        let whoami = run.server.only(WORKOS_WHOAMI);
        assert_eq!(whoami.auth, run.before);
        assert!(
            whoami
                .head
                .to_ascii_lowercase()
                .contains("authorization: bearer workos-access")
        );

        let stored = AuthStore::load(run.dir.auth_path()).expect("stored file loads");
        assert_eq!(stored.credential("factory-droid"), Some(credential.clone()));
        assert_eq!(run.store.credential("factory-droid"), Some(credential));
        assert_eq!(stored.credential("openai").is_some(), run.before.is_some());
    }
}

/// Signs in while whoami answers `status` with a hostile body, and checks the
/// refusal is typed, quotes nothing, and leaves the store as it was.
async fn assert_whoami_refusal(status: u16, start: StartingStore) {
    let hostile = r#"{"message":"hostile org-secret body"}"#;
    let run = droid_login(Route::json(WORKOS_WHOAMI, status, hostile), start).await;
    let refusal = run.result.as_ref().expect_err("a refused session fails");
    assert!(
        matches!(
            refusal,
            ProviderError::Status { family: Family::Droid, status: refused, .. }
                if *refused == status
        ),
        "whoami {status} must fail the login: {refusal:?}"
    );
    let shown = format!("{refusal:?} {refusal}");
    for secret in ["hostile", "org-secret", "workos-access", "workos-refresh"] {
        assert!(!shown.contains(secret), "{secret} leaked into {shown}");
    }
    assert_store_untouched(&run.dir, run.before.as_deref());
    assert!(run.store.credential("factory-droid").is_none());
    assert_eq!(
        run.store.credential("openai").is_some(),
        run.before.is_some()
    );
    assert_eq!(
        run.server.keys(),
        [WORKOS_START, WORKOS_POLL, WORKOS_WHOAMI],
        "the grant was issued before the verification refused it"
    );
}

#[tokio::test]
async fn a_refused_session_verification_fails_the_login_and_leaves_the_store_untouched() {
    for start in [StartingStore::Absent, StartingStore::Seeded] {
        assert_whoami_refusal(403, start).await;
        assert_whoami_refusal(500, start).await;
    }
}

#[tokio::test]
async fn a_session_without_an_organization_is_refused_before_persistence() {
    for start in [StartingStore::Absent, StartingStore::Seeded] {
        let run = droid_login(Route::json(WORKOS_WHOAMI, 200, "{}"), start).await;
        assert!(
            matches!(
                &run.result,
                Err(ProviderError::TokenExchange { status: 200, .. })
            ),
            "an unresolved organization fails the login: {:?}",
            run.result
        );
        assert_store_untouched(&run.dir, run.before.as_deref());
        assert!(run.store.credential("factory-droid").is_none());
    }
}

#[tokio::test]
async fn cancelling_during_session_verification_is_cancellation_and_stores_nothing() {
    for seeded in [false, true] {
        let dir = TestDir::new();
        let server = route_server(workos_routes(Route::held(WORKOS_WHOAMI)), dir.auth_path()).await;
        let mut store = AuthStore::empty(dir.auth_path());
        let before = seeded.then(|| seed_openai_key(&mut store));
        let endpoints = LoginEndpoints::loopback(server.base.as_str(), &EnvSnapshot::default())
            .expect("loopback endpoints");
        let mut flow =
            configured_flow("factory-droid", &mut store, &dir, endpoints).expect("droid flow");
        let cancel = CancellationToken::new();
        let progress = |_: LoginProgress| {};
        let (result, ()) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(flow.run(&progress, &cancel), async {
                server.wait_for(WORKOS_WHOAMI).await;
                cancel.cancel();
            })
        })
        .await
        .expect("a cancelled verification settles");
        drop(flow);
        assert!(
            matches!(result, Err(ProviderError::LoginCancelled)),
            "cancellation during whoami stays cancellation: {result:?}"
        );
        assert_store_untouched(&dir, before.as_deref());
        assert!(store.credential("factory-droid").is_none());
    }
}

#[tokio::test]
async fn cancelling_at_the_device_code_prompt_stops_before_any_poll() {
    let dir = TestDir::new();
    let server = route_server(
        workos_routes(Route::json(WORKOS_WHOAMI, 200, r#"{"orgId":"org_1"}"#)),
        dir.auth_path(),
    )
    .await;
    let mut store = AuthStore::empty(dir.auth_path());
    let endpoints = LoginEndpoints::loopback(server.base.as_str(), &EnvSnapshot::default())
        .expect("loopback endpoints");
    let mut flow = configured_flow("factory-droid", &mut store, &dir, endpoints).expect("flow");
    let cancel = CancellationToken::new();
    let shown = Mutex::new(None);
    let progress = |event: LoginProgress| {
        if let LoginProgress::ShowCode { url, code } = event {
            *shown.lock().expect("shown lock") = Some((url, code));
            cancel.cancel();
        }
    };
    let result = flow.run(&progress, &cancel).await;
    drop(flow);
    assert!(matches!(result, Err(ProviderError::LoginCancelled)));
    let (url, code) = shown
        .into_inner()
        .expect("shown lock")
        .expect("the device code was shown");
    assert_eq!(code, "WDJB-MJHT");
    assert_eq!(url, "https://workos.example/device?user_code=WDJB-MJHT");
    assert_eq!(server.keys(), [WORKOS_START]);
    assert!(!dir.auth_path().exists());
}

fn gitlab_grant(extra: &str) -> String {
    format!(
        r#"{{"access_token":"gitlab-access","refresh_token":"gitlab-refresh","token_type":"Bearer"{extra}}}"#
    )
}

/// The events and result of one `GitLab` sign-in whose pasted value is sent
/// when the authorize URL is shown.
struct PasteRun {
    result: Result<Credential, ProviderError>,
    events: Vec<LoginProgress>,
}

impl PasteRun {
    fn url(&self) -> String {
        self.events
            .iter()
            .find_map(opened_url)
            .expect("an authorize URL was shown")
    }
}

async fn paste_login(flow: &mut LoginFlow<'_>, pasted: &str) -> PasteRun {
    let sender = Mutex::new(flow.take_paste_sender());
    let events = Mutex::new(Vec::new());
    let progress = |event: LoginProgress| {
        if matches!(event, LoginProgress::OpenUrl { .. })
            && let Some(sender) = sender.lock().expect("paste lock").take()
        {
            let _sent = sender.send(pasted.to_owned());
        }
        events.lock().expect("events lock").push(event);
    };
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        flow.run(&progress, &CancellationToken::new()),
    )
    .await
    .expect("login settles");
    PasteRun {
        result,
        events: events.into_inner().expect("events lock"),
    }
}

#[tokio::test]
async fn gitlab_authorize_url_carries_the_pinned_client_scope_and_registered_redirect() {
    let dir = TestDir::new();
    let mut store = AuthStore::empty(dir.auth_path());
    let before = seed_openai_key(&mut store);
    let endpoints = LoginEndpoints::loopback("http://127.0.0.1:1", &EnvSnapshot::default())
        .expect("loopback endpoints");
    let mut flow = configured_flow("gitlab-duo", &mut store, &dir, endpoints).expect("flow");
    let cancel = CancellationToken::new();
    let opened = Mutex::new(None);
    let progress = |event: LoginProgress| {
        if let LoginProgress::OpenUrl { url } = event {
            *opened.lock().expect("opened lock") = Some(url);
            cancel.cancel();
        }
    };
    let result = flow.run(&progress, &cancel).await;
    drop(flow);
    assert!(matches!(result, Err(ProviderError::LoginCancelled)));
    let url = opened
        .into_inner()
        .expect("opened lock")
        .expect("an authorize URL was shown");
    assert_eq!(
        Url::parse(&url).expect("URL").path(),
        "/gitlab/oauth/authorize"
    );
    let query = query_fields(&url);
    assert_eq!(
        field(&query, "client_id"),
        Some(pinned_client_id("gitlab-duo"))
    );
    assert_eq!(field(&query, "response_type"), Some("code"));
    assert_eq!(field(&query, "scope"), Some("api"));
    assert_eq!(field(&query, "code_challenge_method"), Some("S256"));
    assert_eq!(field(&query, "code_challenge").map(str::len), Some(43));
    assert!(field(&query, "state").is_some_and(|state| !state.is_empty()));
    let redirect = field(&query, "redirect_uri").expect("redirect_uri");
    assert!(redirect.starts_with("http://localhost:") && redirect.ends_with("/callback"));
    assert_eq!(
        fs::read(dir.auth_path()).expect("auth bytes"),
        before,
        "a cancelled login leaves the existing file untouched"
    );
}

#[test]
fn effective_client_ids_follow_the_captured_environment() {
    let client = |entries: &[(&str, &str)], provider: &str| {
        let endpoints =
            LoginEndpoints::production(&EnvSnapshot::test(entries)).expect("production endpoints");
        let def = crate::find(provider).expect("table row");
        endpoints.row(def).expect("OAuth row").client_id.to_string()
    };
    assert_eq!(client(&[], "gitlab-duo"), pinned_client_id("gitlab-duo"));
    assert_eq!(
        client(&[("GITLAB_CLIENT_ID", "  override-client\t")], "gitlab-duo"),
        "override-client"
    );
    assert_eq!(
        client(&[("GITLAB_CLIENT_ID", "  ")], "gitlab-duo"),
        pinned_client_id("gitlab-duo"),
        "a blank override counts as unset"
    );
    for provider in ["anthropic", "openai-codex", "factory-droid"] {
        assert_eq!(
            client(&[("GITLAB_CLIENT_ID", "override-client")], provider),
            pinned_client_id(provider),
            "{provider} names no override variable"
        );
    }
}

#[test]
fn production_and_loopback_rows_place_the_new_vendors_apart() {
    let env = EnvSnapshot::default();
    let production = LoginEndpoints::production(&env).expect("production endpoints");
    let row = |endpoints: &LoginEndpoints, provider: &str| {
        endpoints
            .row(crate::find(provider).expect("table row"))
            .expect("OAuth row")
            .clone()
    };
    let gitlab = row(&production, "gitlab-duo");
    assert_eq!(
        gitlab.authorize.as_str(),
        "https://gitlab.com/oauth/authorize"
    );
    assert_eq!(gitlab.token.as_str(), "https://gitlab.com/oauth/token");
    assert_eq!(
        gitlab.revoke.as_ref().map(Url::as_str),
        Some("https://gitlab.com/oauth/revoke")
    );
    let droid = row(&production, "factory-droid");
    assert_eq!(
        droid.session_base.as_ref().map(Url::as_str),
        Some("https://api.factory.ai/")
    );

    let loopback = LoginEndpoints::loopback("http://127.0.0.1:9", &env).expect("loopback");
    let paths: Vec<String> = ["openai-codex", "anthropic", "factory-droid", "gitlab-duo"]
        .into_iter()
        .map(|provider| row(&loopback, provider).token.path().to_owned())
        .collect();
    assert_eq!(
        paths,
        [
            "/oauth/token",
            "/claude/v1/oauth/token",
            "/workos/user_management/authenticate",
            "/gitlab/oauth/token",
        ],
        "each row owns its token path on one loopback server"
    );
    assert_eq!(
        row(&loopback, "factory-droid")
            .session_base
            .as_ref()
            .map(Url::as_str),
        Some("http://127.0.0.1:9/workos")
    );
}

/// A finished `GitLab` exchange: the stored credential, the exchange form, and
/// the authorize URL.
async fn gitlab_exchange(
    env: &EnvSnapshot,
    grant: String,
) -> (Credential, Vec<(String, String)>, String) {
    let dir = TestDir::new();
    let server = route_server(vec![Route::json(GITLAB_TOKEN, 200, grant)], dir.auth_path()).await;
    let mut store = AuthStore::empty(dir.auth_path());
    let endpoints =
        LoginEndpoints::loopback(server.base.as_str(), env).expect("loopback endpoints");
    let mut flow = configured_flow("gitlab-duo", &mut store, &dir, endpoints).expect("flow");
    let run = paste_login(&mut flow, "code123").await;
    drop(flow);
    let url = run.url();
    let credential = run.result.expect("the exchange completes the login");
    let request = server.only(GITLAB_TOKEN);
    assert!(is_form(&request), "the GitLab exchange is form-encoded");
    assert!(!request.body.starts_with('{'), "not a JSON body");
    let stored = AuthStore::load(dir.auth_path()).expect("stored file loads");
    assert_eq!(stored.credential("gitlab-duo"), Some(credential.clone()));
    (credential, form_fields(&request.body), url)
}

#[tokio::test]
async fn gitlab_exchange_sends_the_effective_client_and_verifier_as_a_form() {
    let pinned = pinned_client_id("gitlab-duo");
    let padded = " override-client ";
    let cases: [(&[(&str, &str)], &str); 3] = [
        (&[("GITLAB_CLIENT_ID", padded)], "override-client"),
        (&[("GITLAB_CLIENT_ID", "   ")], pinned),
        (&[], pinned),
    ];
    for (entries, expected) in cases {
        let (credential, form, url) = gitlab_exchange(
            &EnvSnapshot::test(entries),
            gitlab_grant(r#","expires_in":7200,"created_at":1700000000"#),
        )
        .await;
        let query = query_fields(&url);
        assert_eq!(field(&query, "client_id"), Some(expected), "authorize");
        assert_eq!(field(&form, "client_id"), Some(expected), "exchange");
        assert_eq!(field(&form, "grant_type"), Some("authorization_code"));
        assert_eq!(field(&form, "code"), Some("code123"));
        assert_eq!(field(&form, "redirect_uri"), field(&query, "redirect_uri"));
        let verifier = field(&form, "code_verifier").expect("code_verifier");
        assert_eq!(
            Some(pkce_challenge(verifier).as_str()),
            field(&query, "code_challenge"),
            "the exchange proves the verifier behind the authorize challenge"
        );
        let Credential::OAuth(oauth) = credential else {
            panic!("a GitLab login stores an OAuth credential");
        };
        assert_eq!(oauth.access_token.expose(), "gitlab-access");
        assert_eq!(oauth.refresh_token.expose(), "gitlab-refresh");
        assert_eq!(oauth.id_token, None);
        assert_eq!(oauth.account_id, None);
        assert_eq!(oauth.expires_at, Some(1_700_007_200));
    }
}

#[tokio::test]
async fn gitlab_expiry_counts_from_created_at_only_when_the_response_supplies_it() {
    let expires_at = async |extra: &str| {
        let (credential, _, _) =
            gitlab_exchange(&EnvSnapshot::default(), gitlab_grant(extra)).await;
        let Credential::OAuth(oauth) = credential else {
            panic!("a GitLab login stores an OAuth credential");
        };
        oauth.expires_at
    };
    let started = super::unix_now();
    let local = expires_at(r#","expires_in":7200"#).await.expect("expiry");
    let finished = super::unix_now();
    assert!(
        (started + 7200..=finished + 7200).contains(&local),
        "without created_at the expiry counts from the local clock"
    );
    assert_eq!(
        expires_at(r#","expires_in":7200,"created_at":1000"#).await,
        Some(8200),
        "created_at is the issuance base"
    );
    assert_eq!(
        expires_at(r#","created_at":1000"#).await,
        None,
        "created_at alone names no expiry"
    );
}

#[tokio::test]
async fn a_gitlab_exchange_ignores_a_created_at_that_is_not_an_integer() {
    // A refresh reads the same field the same way, so a grant that one accepts
    // the other never refuses.
    for created_at in [
        r#""2025-01-01T00:00:00Z""#,
        "1.5",
        "null",
        "true",
        "18446744073709551615",
    ] {
        let started = super::unix_now();
        let (credential, _, _) = gitlab_exchange(
            &EnvSnapshot::default(),
            gitlab_grant(&format!(r#","expires_in":7200,"created_at":{created_at}"#)),
        )
        .await;
        let finished = super::unix_now();
        let Credential::OAuth(oauth) = credential else {
            panic!("a GitLab login stores an OAuth credential");
        };
        let expires_at = oauth.expires_at.expect("expires_in names an expiry");
        assert!(
            (started + 7200..=finished + 7200).contains(&expires_at),
            "{created_at}: an unusable created_at counts from the local clock, got {expires_at}"
        );
    }
}

#[tokio::test]
async fn a_first_login_creates_the_missing_data_directory() {
    let dir = TestDir::new();
    let auth_path = dir.0.join("fresh").join("data").join("auth.json");
    let server = route_server(
        vec![Route::json(
            GITLAB_TOKEN,
            200,
            gitlab_grant(r#","expires_in":7200"#),
        )],
        auth_path.clone(),
    )
    .await;
    let mut store = AuthStore::empty(auth_path.clone());
    let endpoints = LoginEndpoints::loopback(server.base.as_str(), &EnvSnapshot::default())
        .expect("loopback endpoints");
    let mut flow = configured_flow("gitlab-duo", &mut store, &dir, endpoints).expect("flow");
    let run = paste_login(&mut flow, "code123").await;
    drop(flow);
    run.result
        .expect("a login into a missing data directory completes");
    let stored = AuthStore::load(&auth_path).expect("the stored file loads");
    assert!(
        matches!(stored.credential("gitlab-duo"), Some(Credential::OAuth(_))),
        "the credential reached the new directory"
    );
}

#[test]
fn a_redirect_override_value_never_formats_in_debug_output() {
    let uri = "http://user:hunter2@localhost:8080/callback";
    let env = EnvSnapshot::test(&[("GITLAB_REDIRECT_URI", uri)]);
    let endpoints = LoginEndpoints::production(&env).expect("production endpoints");
    let shown = format!("{endpoints:?}");
    assert!(!shown.contains("hunter2"), "{shown}");
    assert!(!shown.contains(uri), "{shown}");
    assert!(
        shown.contains("GITLAB_REDIRECT_URI"),
        "the variable stays named: {shown}"
    );
}

#[tokio::test]
async fn a_rejected_gitlab_exchange_redacts_the_code_and_leaves_the_store_untouched() {
    for seeded in [false, true] {
        let dir = TestDir::new();
        let server = route_server(
            vec![Route::json(
                GITLAB_TOKEN,
                400,
                r#"{"error":"invalid_grant","error_description":"code123 is not accepted"}"#,
            )],
            dir.auth_path(),
        )
        .await;
        let mut store = AuthStore::empty(dir.auth_path());
        let before = seeded.then(|| seed_openai_key(&mut store));
        let endpoints = LoginEndpoints::loopback(server.base.as_str(), &EnvSnapshot::default())
            .expect("loopback endpoints");
        let mut flow = configured_flow("gitlab-duo", &mut store, &dir, endpoints).expect("flow");
        let run = paste_login(&mut flow, "code123").await;
        drop(flow);
        assert!(
            matches!(
                &run.result,
                Err(ProviderError::TokenExchange { status: 400, message })
                    if message.as_str() == "<redacted> is not accepted"
            ),
            "{:?}",
            run.result
        );
        assert_store_untouched(&dir, before.as_deref());
        assert!(store.credential("gitlab-duo").is_none());
    }
}

#[tokio::test]
async fn cancelling_a_gitlab_exchange_stores_nothing() {
    let dir = TestDir::new();
    let server = route_server(vec![Route::held(GITLAB_TOKEN)], dir.auth_path()).await;
    let mut store = AuthStore::empty(dir.auth_path());
    let before = seed_openai_key(&mut store);
    let endpoints = LoginEndpoints::loopback(server.base.as_str(), &EnvSnapshot::default())
        .expect("loopback endpoints");
    let mut flow = configured_flow("gitlab-duo", &mut store, &dir, endpoints).expect("flow");
    let cancel = CancellationToken::new();
    let sender = Mutex::new(flow.take_paste_sender());
    let progress = |event: LoginProgress| {
        if matches!(event, LoginProgress::OpenUrl { .. })
            && let Some(sender) = sender.lock().expect("paste lock").take()
        {
            let _sent = sender.send(String::from("code123"));
        }
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(flow.run(&progress, &cancel), async {
            server.wait_for(GITLAB_TOKEN).await;
            cancel.cancel();
        })
    })
    .await
    .expect("a cancelled exchange settles");
    drop(flow);
    assert!(matches!(result, Err(ProviderError::LoginCancelled)));
    assert_eq!(fs::read(dir.auth_path()).expect("auth bytes"), before);
    assert!(store.credential("gitlab-duo").is_none());
}

#[tokio::test]
async fn a_redirect_override_is_advertised_verbatim_and_served_at_its_own_port_and_path() {
    let dir = TestDir::new();
    let server = route_server(
        vec![Route::json(
            GITLAB_TOKEN,
            200,
            gitlab_grant(r#","expires_in":7200"#),
        )],
        dir.auth_path(),
    )
    .await;
    let port = free_loopback_port().await;
    let advertised = format!("http://localhost:{port}/gitlab/cb");
    let padded = format!("  {advertised}  ");
    let env = EnvSnapshot::test(&[("GITLAB_REDIRECT_URI", padded.as_str())]);
    let mut store = AuthStore::empty(dir.auth_path());
    let endpoints =
        LoginEndpoints::loopback(server.base.as_str(), &env).expect("loopback endpoints");
    let mut flow = configured_flow("gitlab-duo", &mut store, &dir, endpoints).expect("flow");
    let events = Mutex::new(Vec::new());
    let progress = |event: LoginProgress| events.lock().expect("events lock").push(event);
    let cancel = CancellationToken::new();
    let browser = async {
        let url = loop {
            let shown = events
                .lock()
                .expect("events lock")
                .iter()
                .find_map(opened_url);
            if let Some(url) = shown {
                break url;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        let query = query_fields(&url);
        let state = field(&query, "state").expect("state").to_owned();
        let default_path = browser_get(port, &format!("/callback?code=wrong&state={state}")).await;
        let override_path =
            browser_get(port, &format!("/gitlab/cb?code=code123&state={state}")).await;
        (url, default_path, override_path)
    };
    let (result, (url, default_path, override_path)) =
        tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(flow.run(&progress, &cancel), browser)
        })
        .await
        .expect("the callback completes the login");
    drop(flow);
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(
        field(&query_fields(&url), "redirect_uri"),
        Some(advertised.as_str())
    );
    assert!(
        default_path.contains("404"),
        "the registered path is not served under an override: {default_path}"
    );
    assert!(
        override_path.contains("200"),
        "the override path is served: {override_path}"
    );
    let form = form_fields(&server.only(GITLAB_TOKEN).body);
    assert_eq!(field(&form, "redirect_uri"), Some(advertised.as_str()));
    assert_eq!(field(&form, "code"), Some("code123"));
}

#[tokio::test]
async fn a_redirect_override_that_this_machine_cannot_serve_leaves_the_login_to_paste() {
    let (holder, busy) = held_loopback_port().await;
    let overrides = [
        // The override's own port is taken: no other port is tried.
        format!("http://127.0.0.1:{busy}/cb"),
        // Not loopback: nothing local can receive it.
        String::from("https://app.example/gitlab/callback"),
        // TLS cannot be terminated by the loopback listener.
        format!("https://localhost:{busy}/cb"),
    ];
    for advertised in overrides {
        let dir = TestDir::new();
        let server = route_server(
            vec![Route::json(
                GITLAB_TOKEN,
                200,
                gitlab_grant(r#","expires_in":7200"#),
            )],
            dir.auth_path(),
        )
        .await;
        let env = EnvSnapshot::test(&[("GITLAB_REDIRECT_URI", advertised.as_str())]);
        let mut store = AuthStore::empty(dir.auth_path());
        let endpoints =
            LoginEndpoints::loopback(server.base.as_str(), &env).expect("loopback endpoints");
        let mut flow = configured_flow("gitlab-duo", &mut store, &dir, endpoints).expect("flow");
        let run = paste_login(&mut flow, "code123").await;
        drop(flow);
        assert!(run.result.is_ok(), "{advertised}: {:?}", run.result);
        assert!(
            run.events
                .iter()
                .any(|event| matches!(event, LoginProgress::AskPaste { .. })),
            "{advertised} is paste-only"
        );
        assert_eq!(
            field(&query_fields(&run.url()), "redirect_uri"),
            Some(advertised.as_str()),
            "the advertised value never changes"
        );
        let form = form_fields(&server.only(GITLAB_TOKEN).body);
        assert_eq!(field(&form, "redirect_uri"), Some(advertised.as_str()));
    }
    drop(holder);
}

#[tokio::test]
async fn a_busy_override_port_is_paste_only_not_a_fallback_listener() {
    // With no paste input the paste-only flow ends at once; a fallback
    // listener would instead keep waiting for a callback nobody can send.
    let (holder, busy) = held_loopback_port().await;
    let advertised = format!("http://127.0.0.1:{busy}/cb");
    let env = EnvSnapshot::test(&[("GITLAB_REDIRECT_URI", advertised.as_str())]);
    let dir = TestDir::new();
    let mut store = AuthStore::empty(dir.auth_path());
    let endpoints = LoginEndpoints::loopback("http://127.0.0.1:1", &env).expect("loopback");
    let mut flow = configured_flow("gitlab-duo", &mut store, &dir, endpoints).expect("flow");
    let opened = Mutex::new(None);
    let progress = |event: LoginProgress| {
        if let LoginProgress::OpenUrl { url } = event {
            *opened.lock().expect("opened lock") = Some(url);
        }
    };
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        flow.run(&progress, &CancellationToken::new()),
    )
    .await
    .expect("a paste-only login does not wait on a listener");
    drop(flow);
    assert!(matches!(result, Err(ProviderError::LoginCancelled)));
    let url = opened
        .into_inner()
        .expect("opened lock")
        .expect("an authorize URL was shown");
    assert_eq!(
        field(&query_fields(&url), "redirect_uri"),
        Some(advertised.as_str())
    );
    assert!(!dir.auth_path().exists());
    drop(holder);
}

#[tokio::test]
async fn a_malformed_redirect_override_fails_that_login_before_any_request() {
    // Each value with the part of it that must never reach the error text.
    let refused = [
        ("not a url", "not a url"),
        ("ftp://localhost:9/cb", "ftp://"),
        ("http://localhost:0/cb", "localhost"),
        ("http://user:secretpw@localhost:9/cb", "secretpw"),
        ("http://localhost:9/cb#leaky", "leaky"),
    ];
    for (bad, leak) in refused {
        let dir = TestDir::new();
        let server = route_server(Vec::new(), dir.auth_path()).await;
        let env = EnvSnapshot::test(&[("GITLAB_REDIRECT_URI", bad)]);
        let endpoints =
            LoginEndpoints::loopback(server.base.as_str(), &env).expect("other rows still build");
        let mut store = AuthStore::empty(dir.auth_path());
        let mut flow = configured_flow("gitlab-duo", &mut store, &dir, endpoints).expect("flow");
        let run = paste_login(&mut flow, "code123").await;
        drop(flow);
        let error = match run.result {
            Err(error @ ProviderError::LoginInput { .. }) => error,
            other => panic!("{bad} must be refused as input: {other:?}"),
        };
        let shown = error.to_string();
        assert!(shown.contains("GITLAB_REDIRECT_URI"), "{shown}");
        assert!(!shown.contains(leak), "{leak} leaked into {shown}");
        assert!(server.keys().is_empty(), "no request precedes the refusal");
        assert!(!dir.auth_path().exists());
    }
}

#[test]
fn error_messages_scrub_raw_and_percent_encoded_secrets() {
    let quoted = r#"{"error_description":"grant co%3Ade and verifier V-x_y~z failed"}"#;
    let message = super::error_message(quoted.as_bytes(), &["co:de", "V-x_y~z"]);
    assert_eq!(message, "grant <redacted> and verifier <redacted> failed");
}

#[tokio::test]
async fn a_gitlab_exchange_error_never_quotes_the_code_verifier_or_state() {
    let dir = TestDir::new();
    let server = route_server(vec![Route::json_echo(GITLAB_TOKEN, 400)], dir.auth_path()).await;
    let mut store = AuthStore::empty(dir.auth_path());
    let endpoints = LoginEndpoints::loopback(server.base.as_str(), &EnvSnapshot::default())
        .expect("loopback endpoints");
    let mut flow = configured_flow("gitlab-duo", &mut store, &dir, endpoints).expect("flow");
    let run = paste_login(&mut flow, "code123").await;
    drop(flow);
    let request = server.only(GITLAB_TOKEN);
    let form = form_fields(&request.body);
    assert!(
        field(&form, "state").is_none(),
        "the GitLab exchange form sends no state"
    );
    let verifier = field(&form, "code_verifier")
        .expect("code_verifier")
        .to_owned();
    assert!(
        request.body.contains(&verifier) && request.body.contains("code123"),
        "the server echoed the exchange body: {}",
        request.body
    );
    let message = match run.result {
        Err(ProviderError::TokenExchange {
            status: 400,
            message,
        }) => message,
        other => panic!("an echoed exchange failure is a token-exchange error: {other:?}"),
    };
    for secret in ["code123", verifier.as_str()] {
        assert!(!message.contains(secret), "{secret} leaked into {message}");
    }
    let encoded: String = url::form_urlencoded::byte_serialize(verifier.as_bytes()).collect();
    assert!(!message.contains(&encoded));
    assert_store_untouched(&dir, None);
}

#[tokio::test]
async fn a_gitlab_paste_failure_names_the_gitlab_family() {
    let dir = TestDir::new();
    let mut store = AuthStore::empty(dir.auth_path());
    let before = seed_openai_key(&mut store);
    let endpoints = LoginEndpoints::loopback("http://127.0.0.1:1", &EnvSnapshot::default())
        .expect("loopback endpoints");
    let mut flow = configured_flow("gitlab-duo", &mut store, &dir, endpoints).expect("flow");
    let run = paste_login(&mut flow, "   ").await;
    drop(flow);
    assert!(
        matches!(
            &run.result,
            Err(ProviderError::Transport {
                family: Family::GitLab,
                reason
            }) if reason.contains("empty")
        ),
        "{:?}",
        run.result
    );
    assert_eq!(fs::read(dir.auth_path()).expect("auth bytes"), before);
}

#[tokio::test]
async fn gitlab_revoke_is_a_form_with_the_effective_client_and_never_blocks_logout() {
    type RevokeCase<'a> = (&'a [(&'a str, &'a str)], &'a str, u16);
    let pinned = pinned_client_id("gitlab-duo");
    let padded = " override-client ";
    let cases: [RevokeCase<'_>; 3] = [
        (&[], pinned, 200),
        (&[("GITLAB_CLIENT_ID", padded)], "override-client", 400),
        (&[("GITLAB_CLIENT_ID", "  ")], pinned, 503),
    ];
    for (entries, expected, status) in cases {
        let dir = TestDir::new();
        let server = route_server(
            vec![Route::json(GITLAB_REVOKE, status, "{}")],
            dir.auth_path(),
        )
        .await;
        let mut store = AuthStore::empty(dir.auth_path());
        store
            .set(
                "gitlab-duo",
                Credential::OAuth(OAuthCredential {
                    access_token: SecretString::from("gitlab-access"),
                    refresh_token: SecretString::from("gitlab refresh/token"),
                    expires_at: Some(100),
                    id_token: None,
                    account_id: None,
                }),
            )
            .expect("seed credential");
        store.store().expect("save seed");
        let env = EnvSnapshot::test(entries);
        let endpoints =
            LoginEndpoints::loopback(server.base.as_str(), &env).expect("loopback endpoints");
        let result = super::logout_with("gitlab-duo", &mut store, "dalgon/test", &endpoints).await;
        assert!(result.is_ok(), "revoke status {status} never blocks logout");
        let request = server.only(GITLAB_REVOKE);
        assert!(is_form(&request), "Doorkeeper reads form parameters");
        assert!(!request.body.starts_with('{'), "not a JSON body");
        let form = form_fields(&request.body);
        assert_eq!(field(&form, "token"), Some("gitlab refresh/token"));
        assert_eq!(field(&form, "token_type_hint"), Some("refresh_token"));
        assert_eq!(field(&form, "client_id"), Some(expected));
        assert!(store.credential("gitlab-duo").is_none());
        assert!(!dir.auth_path().exists(), "the last entry removes the file");
    }
}
