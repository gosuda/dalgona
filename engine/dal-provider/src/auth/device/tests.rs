use super::{DeviceCodeResponse, DeviceOutcome, decode_json, run, wait_for_next_poll};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use dal_core::Family;
use reqwest::Client;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::Instant,
};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{Hook, ProviderError};

use super::super::oauth::{DeviceUrls, LoginProgress, OAuthHttp, unix_now};

/// The pinned public WorkOS client id; the start body must name it verbatim.
const CLIENT_ID: &str = "client_01HNM792M5G5G1A2THWPXKFMXB";

#[test]
fn device_interval_accepts_number_and_string_and_usercode_alias() {
    let numeric = decode_json::<DeviceCodeResponse>(
        br#"{"device_auth_id":"device","user_code":"ABCD","interval":3}"#,
    );
    assert!(numeric.as_ref().is_some_and(|value| value.interval == 3));

    let text = decode_json::<DeviceCodeResponse>(
        br#"{"device_auth_id":"device","usercode":"ABCD","interval":"7"}"#,
    );
    assert!(
        text.as_ref()
            .is_some_and(|value| value.interval == 7 && value.user_code == "ABCD")
    );
    assert!(
        decode_json::<DeviceCodeResponse>(
            br#"{"device_auth_id":"device","user_code":"ABCD","interval":"bad"}"#
        )
        .is_none()
    );
}

#[tokio::test]
async fn pending_poll_delay_obeys_cancel_and_deadline() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    let cancelled = wait_for_next_poll(
        Instant::now() + Duration::from_secs(10),
        Duration::from_secs(60),
        &cancel,
    )
    .await;
    assert!(matches!(
        cancelled,
        Err(crate::ProviderError::LoginCancelled)
    ));

    let expired = wait_for_next_poll(
        Instant::now(),
        Duration::from_secs(1),
        &CancellationToken::new(),
    )
    .await;
    assert!(matches!(expired, Err(crate::ProviderError::LoginTimeout)));
}

async fn read_request(stream: &mut TcpStream) -> Option<Vec<u8>> {
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

/// Serves one scripted response per connection and captures every request.
async fn serve_script(script: Vec<(u16, &'static str)>) -> (Url, Arc<Mutex<Vec<Vec<u8>>>>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback bind succeeds");
    let base = Url::parse(&format!(
        "http://{}",
        listener.local_addr().expect("bound address")
    ))
    .expect("loopback base parses");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    #[expect(
        clippy::disallowed_methods,
        reason = "test edge: loopback script server runs concurrently with test"
    )]
    let _server = tokio::spawn(async move {
        for (status, body) in script {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let Some(request) = read_request(&mut stream).await else {
                return;
            };
            captured.lock().expect("requests lock").push(request);
            let head = format!(
                "HTTP/1.1 {status} Test\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            if stream.write_all(head.as_bytes()).await.is_err() {
                return;
            }
            if stream.write_all(body.as_bytes()).await.is_err() {
                return;
            }
        }
    });
    (base, requests)
}

fn device_urls(base: &Url) -> DeviceUrls {
    DeviceUrls {
        usercode: base
            .join("/user_management/authorize/device")
            .expect("device url"),
        poll: base
            .join("/user_management/authenticate")
            .expect("poll url"),
        page: base.join("/device").expect("page url"),
        redirect_uri: "",
    }
}

fn progress_sink() -> (
    Arc<Mutex<Vec<LoginProgress>>>,
    impl Fn(LoginProgress) + Send + Sync,
) {
    let events: Arc<Mutex<Vec<LoginProgress>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let progress = move |event: LoginProgress| {
        sink.lock().expect("progress lock").push(event);
    };
    (events, progress)
}

fn request_parts(request: &[u8]) -> (String, String) {
    let text = String::from_utf8_lossy(request).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").expect("request has a body");
    (head.to_owned(), body.to_owned())
}

fn shown_code(events: &[LoginProgress]) -> (String, String) {
    events
        .iter()
        .find_map(|event| match event {
            LoginProgress::ShowCode { url, code } => Some((url.clone(), code.clone())),
            _ => None,
        })
        .expect("a device code is shown")
}

const DEVICE_START: &str = r#"{"device_code":"ws-device-1","user_code":"ABCD-EFGH","verification_uri":"https://example.test/device","verification_uri_complete":"https://example.test/device?user_code=ABCD-EFGH","interval":1,"expires_in":300}"#;
const TOKEN_DELIVERY: &str =
    r#"{"access_token":"ws-access-1","refresh_token":"ws-refresh-1","expires_in":3600}"#;

#[tokio::test]
async fn workos_direct_tokens_complete_without_exchange() {
    let (base, requests) = serve_script(vec![(200, DEVICE_START), (200, TOKEN_DELIVERY)]).await;
    let urls = device_urls(&base);
    let client = Client::new();
    let http = OAuthHttp {
        client: &client,
        user_agent: "dalgon/test",
    };
    let (events, progress) = progress_sink();
    let before = unix_now();

    let outcome = run(
        &http,
        Family::Droid,
        Hook::WorkOsDevice,
        CLIENT_ID,
        &urls,
        Instant::now() + Duration::from_secs(30),
        &progress,
        &CancellationToken::new(),
    )
    .await;

    let Ok(DeviceOutcome::Tokens(credential)) = outcome else {
        panic!("WorkOS poll must deliver tokens directly");
    };
    assert_eq!(credential.access_token.expose(), "ws-access-1");
    assert_eq!(credential.refresh_token.expose(), "ws-refresh-1");
    assert!(
        credential
            .expires_at
            .is_some_and(|at| at >= before + 3600 && at <= unix_now() + 3600),
        "expiry follows the token lifetime"
    );
    assert!(credential.id_token.is_none());
    assert!(credential.account_id.is_none());

    let captured = requests.lock().expect("requests lock");
    assert_eq!(captured.len(), 2);
    let (start_head, start_body) = request_parts(&captured[0]);
    assert!(start_head.contains("POST"));
    assert!(
        start_head.contains("application/x-www-form-urlencoded"),
        "the start body is form-encoded"
    );
    assert_eq!(start_body, format!("client_id={CLIENT_ID}"));
    let (poll_head, poll_body) = request_parts(&captured[1]);
    assert!(poll_head.contains("POST"));
    assert!(
        poll_body.contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"),
        "the poll names the device-code grant"
    );
    assert!(poll_body.contains(&format!("client_id={CLIENT_ID}")));
    assert!(poll_body.contains("device_code=ws-device-1"));

    let events = events.lock().expect("progress lock");
    assert_eq!(
        shown_code(&events),
        (
            String::from("https://example.test/device?user_code=ABCD-EFGH"),
            String::from("ABCD-EFGH"),
        )
    );
}

#[tokio::test]
async fn workos_slow_down_backs_off_then_completes() {
    let (base, requests) = serve_script(vec![
        (200, DEVICE_START),
        (400, r#"{"error":"slow_down"}"#),
        (200, TOKEN_DELIVERY),
    ])
    .await;
    let urls = device_urls(&base);
    let client = Client::new();
    let http = OAuthHttp {
        client: &client,
        user_agent: "dalgon/test",
    };
    let (_, progress) = progress_sink();

    let started = std::time::Instant::now();
    let outcome = run(
        &http,
        Family::Droid,
        Hook::WorkOsDevice,
        CLIENT_ID,
        &urls,
        Instant::now() + Duration::from_secs(60),
        &progress,
        &CancellationToken::new(),
    )
    .await;
    let elapsed = started.elapsed();

    match &outcome {
        Ok(DeviceOutcome::Tokens(_)) => {}
        Ok(DeviceOutcome::Grant(_)) => {
            panic!("slow_down must retry, delivered Grant after {elapsed:?}")
        }
        Err(err) => panic!("slow_down must retry, not fail after {elapsed:?}: {err:?}"),
    }
    assert_eq!(requests.lock().expect("requests lock").len(), 3);
    assert!(
        elapsed >= Duration::from_secs(5),
        "slow_down must add five seconds, took {elapsed:?}"
    );
}

#[tokio::test]
async fn workos_cancel_aborts_before_completion() {
    let (base, requests) = serve_script(vec![
        (200, DEVICE_START),
        (400, r#"{"error":"authorization_pending"}"#),
    ])
    .await;
    let urls = device_urls(&base);
    let client = Client::new();
    let http = OAuthHttp {
        client: &client,
        user_agent: "dalgon/test",
    };
    let (_, progress) = progress_sink();
    let cancel = CancellationToken::new();
    cancel.cancel();

    let outcome = run(
        &http,
        Family::Droid,
        Hook::WorkOsDevice,
        CLIENT_ID,
        &urls,
        Instant::now() + Duration::from_secs(30),
        &progress,
        &cancel,
    )
    .await;

    assert!(matches!(outcome, Err(ProviderError::LoginCancelled)));
    assert_eq!(
        requests.lock().expect("requests lock").len(),
        1,
        "cancellation must stop the flow before polling"
    );
}

#[tokio::test]
async fn workos_device_expiry_times_out() {
    let expired_start = r#"{"device_code":"ws-device-old","user_code":"WXYZ-0000","verification_uri":"https://example.test/device","interval":1,"expires_in":0}"#;
    let (base, _) = serve_script(vec![
        (200, expired_start),
        (400, r#"{"error":"authorization_pending"}"#),
    ])
    .await;
    let urls = device_urls(&base);
    let client = Client::new();
    let http = OAuthHttp {
        client: &client,
        user_agent: "dalgon/test",
    };
    let (_, progress) = progress_sink();

    let outcome = run(
        &http,
        Family::Droid,
        Hook::WorkOsDevice,
        CLIENT_ID,
        &urls,
        Instant::now() + Duration::from_secs(30),
        &progress,
        &CancellationToken::new(),
    )
    .await;

    assert!(matches!(outcome, Err(ProviderError::LoginTimeout)));
}

#[tokio::test]
async fn workos_denied_is_typed_without_secrets() {
    let (base, _) = serve_script(vec![
        (
            200,
            r#"{"device_code":"ws-device-denied","user_code":"DENY-0000","verification_uri":"https://example.test/device","interval":1,"expires_in":300}"#,
        ),
        (
            400,
            r#"{"error":"access_denied","error_description":"the user said no"}"#,
        ),
    ])
    .await;
    let urls = device_urls(&base);
    let client = Client::new();
    let http = OAuthHttp {
        client: &client,
        user_agent: "dalgon/test",
    };
    let (_, progress) = progress_sink();

    let outcome = run(
        &http,
        Family::Droid,
        Hook::WorkOsDevice,
        CLIENT_ID,
        &urls,
        Instant::now() + Duration::from_secs(30),
        &progress,
        &CancellationToken::new(),
    )
    .await;

    let Err(error) = outcome else {
        panic!("a denied device poll must fail");
    };
    match error {
        ProviderError::Status {
            status, message, ..
        } => {
            assert_eq!(status, 400);
            assert_eq!(message, "device authorization was denied");
            assert!(!message.contains("ws-device-denied"));
            assert!(!message.contains("DENY-0000"));
        }
        _ => panic!("a denied device poll must be a Status"),
    }
}

#[tokio::test]
async fn workos_start_missing_fields_is_typed() {
    let (base, _) = serve_script(vec![(200, r#"{"device_code":"ws-device-partial"}"#)]).await;
    let urls = device_urls(&base);
    let client = Client::new();
    let http = OAuthHttp {
        client: &client,
        user_agent: "dalgon/test",
    };
    let (_, progress) = progress_sink();

    let outcome = run(
        &http,
        Family::Droid,
        Hook::WorkOsDevice,
        CLIENT_ID,
        &urls,
        Instant::now() + Duration::from_secs(30),
        &progress,
        &CancellationToken::new(),
    )
    .await;

    let Err(error) = outcome else {
        panic!("a field-poor device response must fail");
    };
    match error {
        ProviderError::DeviceCode { status, message } => {
            assert_eq!(status, 200);
            assert_eq!(message, "invalid device-code response");
            assert!(!message.contains("ws-device-partial"));
        }
        _ => panic!("a field-poor device response must be a DeviceCode"),
    }
}

#[tokio::test]
async fn workos_poll_without_grants_is_typed() {
    let (base, _) = serve_script(vec![
        (200, DEVICE_START),
        (200, r#"{"access_token":"ws-access-lonely"}"#),
    ])
    .await;
    let urls = device_urls(&base);
    let client = Client::new();
    let http = OAuthHttp {
        client: &client,
        user_agent: "dalgon/test",
    };
    let (_, progress) = progress_sink();

    let outcome = run(
        &http,
        Family::Droid,
        Hook::WorkOsDevice,
        CLIENT_ID,
        &urls,
        Instant::now() + Duration::from_secs(30),
        &progress,
        &CancellationToken::new(),
    )
    .await;

    let Err(error) = outcome else {
        panic!("a grant-poor token response must fail");
    };
    match error {
        ProviderError::TokenExchange { status, message } => {
            assert_eq!(status, 200);
            assert_eq!(message, "invalid token response");
            assert!(!message.contains("ws-access-lonely"));
        }
        _ => panic!("a grant-poor token response must be a TokenExchange"),
    }
}

#[tokio::test]
async fn workos_poll_failure_redacts_device_codes() {
    let (base, _) = serve_script(vec![
        (
            200,
            r#"{"device_code":"ws-device-redact","user_code":"ZZZZ-9999","verification_uri":"https://example.test/device","interval":1,"expires_in":300}"#,
        ),
        (
            500,
            r#"{"error":"boom for ws-device-redact / ZZZZ-9999"}"#,
        ),
    ])
    .await;
    let urls = device_urls(&base);
    let client = Client::new();
    let http = OAuthHttp {
        client: &client,
        user_agent: "dalgon/test",
    };
    let (_, progress) = progress_sink();

    let outcome = run(
        &http,
        Family::Droid,
        Hook::WorkOsDevice,
        CLIENT_ID,
        &urls,
        Instant::now() + Duration::from_secs(30),
        &progress,
        &CancellationToken::new(),
    )
    .await;

    let Err(error) = outcome else {
        panic!("a failed poll must fail");
    };
    match error {
        ProviderError::DeviceCode { status, message } => {
            assert_eq!(status, 500);
            assert!(
                message.contains("<redacted>"),
                "device codes must be redacted, got {message}"
            );
            assert!(!message.contains("ws-device-redact"));
            assert!(!message.contains("ZZZZ-9999"));
        }
        _ => panic!("a failed poll must be a DeviceCode"),
    }
}

#[tokio::test]
async fn codex_poll_failure_redacts_the_device_auth_id_and_user_code() {
    let (base, _) = serve_script(vec![
        (
            200,
            r#"{"device_auth_id":"codex-auth-id-77","user_code":"QQQQ-1111","interval":1}"#,
        ),
        (
            500,
            r#"{"error":"boom for codex-auth-id-77 / QQQQ-1111"}"#,
        ),
    ])
    .await;
    let urls = device_urls(&base);
    let client = Client::new();
    let http = OAuthHttp {
        client: &client,
        user_agent: "dalgon/test",
    };
    let (_, progress) = progress_sink();

    let outcome = run(
        &http,
        Family::Codex,
        Hook::CodexAccount,
        "app_test",
        &urls,
        Instant::now() + Duration::from_secs(30),
        &progress,
        &CancellationToken::new(),
    )
    .await;

    let Err(ProviderError::DeviceCode { status, message }) = outcome else {
        panic!("a failed Codex poll must be a DeviceCode");
    };
    assert_eq!(status, 500);
    assert_eq!(message, "boom for <redacted> / <redacted>");
}

#[tokio::test]
async fn workos_show_code_falls_back_through_page() {
    let token = r#"{"access_token":"ws-access-fb","refresh_token":"ws-refresh-fb"}"#;
    for (start, expected) in [
        (
            r#"{"device_code":"d1","user_code":"U1","verification_uri":"https://example.test/only","interval":1,"expires_in":300}"#,
            "https://example.test/only",
        ),
        (
            r#"{"device_code":"d2","user_code":"U2","interval":1,"expires_in":300}"#,
            "page",
        ),
    ] {
        let (base, _) = serve_script(vec![(200, start), (200, token)]).await;
        let urls = device_urls(&base);
        let client = Client::new();
        let http = OAuthHttp {
            client: &client,
            user_agent: "dalgon/test",
        };
        let (events, progress) = progress_sink();

        let outcome = run(
            &http,
            Family::Droid,
            Hook::WorkOsDevice,
            CLIENT_ID,
            &urls,
            Instant::now() + Duration::from_secs(30),
            &progress,
            &CancellationToken::new(),
        )
        .await;
        assert!(
            matches!(outcome, Ok(DeviceOutcome::Tokens(_))),
            "fallback display must not block completion"
        );

        let expected_url = if expected == "page" {
            urls.page.as_str().to_owned()
        } else {
            expected.to_owned()
        };
        let events = events.lock().expect("progress lock");
        assert_eq!(shown_code(&events).0, expected_url);
    }
}
