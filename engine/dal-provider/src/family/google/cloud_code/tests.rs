//! Witnesses for the Cloud Code Assist project lifecycle and the native
//! request envelope: exact request bodies and headers, tier and project
//! selection, the bounded operation polling, cancellation and deadline
//! propagation, and the explicit thinking-off spelling. Every server is a
//! loopback fixture on an ephemeral port; nothing contacts a vendor.

use std::{
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use dal_core::{ContextItem, ModelRoute, Part, Purpose, RequestParams};
use jiff::Timestamp;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Mutex,
    task::JoinSet,
};

use super::*;
use crate::thinking::{BeforeRequestPatch, Effort, RequestLimits, ThinkingSupport, plan};

const TOKEN: &str = "tok-secret-1";

const CLI_METADATA: &str = r#""metadata":{"ideType":"IDE_UNSPECIFIED","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI"}"#;

const FREE_DEFAULT: &str = r#"{"allowedTiers":[{"id":"free-tier","isDefault":true}]}"#;

// -- loopback fixture -----------------------------------------------------------

#[derive(Clone)]
enum Step {
    Reply(u16, String),
    Stall,
}

fn ok(body: &str) -> Step {
    Step::Reply(200, body.to_owned())
}

fn refuse(status: u16, body: &str) -> Step {
    Step::Reply(status, body.to_owned())
}

struct Seen {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

struct Fake {
    base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    _server: JoinSet<()>,
}

impl Fake {
    async fn targets(&self) -> Vec<String> {
        self.seen
            .lock()
            .await
            .iter()
            .map(|seen| format!("{} {}", seen.method, seen.target))
            .collect()
    }

    async fn count(&self) -> usize {
        self.seen.lock().await.len()
    }

    async fn body(&self, index: usize) -> String {
        self.seen.lock().await[index].body.clone()
    }
}

/// Serves one scripted step per connection and records every request. The
/// listener closes when the script ends, so a request past the script fails
/// to connect instead of hanging.
async fn serve(script: Vec<Step>) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
    let worker = Arc::clone(&seen);
    let mut server = JoinSet::new();
    server.spawn(async move {
        for step in script {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            if let Some(request) = read_request(&mut stream).await {
                worker.lock().await.push(request);
            }
            match step {
                Step::Reply(status, body) => {
                    let head = format!(
                        "HTTP/1.1 {status} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        reason(status),
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes()).await;
                    let _ = stream.write_all(body.as_bytes()).await;
                    let _ = stream.shutdown().await;
                }
                Step::Stall => std::future::pending::<()>().await,
            }
        }
    });
    Fake {
        base,
        seen,
        _server: server,
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "Internal Server Error",
    }
}

async fn read_request(stream: &mut TcpStream) -> Option<Seen> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 2048];
    let head_end = loop {
        let count = stream.read(&mut buffer).await.ok()?;
        if count == 0 {
            return None;
        }
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break end;
        }
        if bytes.len() > 64 * 1024 {
            return None;
        }
    };
    let head = String::from_utf8_lossy(&bytes[..head_end]).into_owned();
    let mut lines = head.lines();
    let (method, rest) = lines.next()?.split_once(' ')?;
    let target = rest.split(' ').next()?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    let body_start = head_end + 4;
    while bytes.len() < body_start + length {
        let count = stream.read(&mut buffer).await.ok()?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    Some(Seen {
        method: method.to_owned(),
        target,
        headers,
        body: String::from_utf8_lossy(&bytes[body_start..]).into_owned(),
    })
}

// -- clocks -------------------------------------------------------------------------

/// Records each pause and returns at once, so 23 pauses cost no time.
#[derive(Clone, Default)]
struct RecordingClock {
    pauses: Arc<StdMutex<Vec<Duration>>>,
}

impl RecordingClock {
    fn pauses(&self) -> Vec<Duration> {
        self.pauses.lock().unwrap().clone()
    }
}

impl Clock for RecordingClock {
    type Sleep = std::future::Ready<()>;

    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn sleep(&self, duration: Duration) -> Self::Sleep {
        self.pauses.lock().unwrap().push(duration);
        std::future::ready(())
    }
}

/// Cancels the run the moment a pause starts and never wakes.
#[derive(Clone)]
struct CancelOnPause {
    cancel: CancellationToken,
}

impl Clock for CancelOnPause {
    type Sleep = std::future::Pending<()>;

    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn sleep(&self, _duration: Duration) -> Self::Sleep {
        self.cancel.cancel();
        std::future::pending()
    }
}

// -- runners ------------------------------------------------------------------------

fn identity(os: &str, arch: &str) -> ProviderIdentity {
    ProviderIdentity {
        version: "1.2.3".into(),
        os: os.into(),
        os_version: "test".into(),
        arch: arch.into(),
    }
}

fn soon() -> tokio::time::Instant {
    tokio::time::Instant::now() + Duration::from_secs(60)
}

async fn run<C: Clock>(
    surface: Surface<'_>,
    fake: &Fake,
    env: &EnvSnapshot,
    clock: &C,
    cancel: &CancellationToken,
    deadline: tokio::time::Instant,
) -> Result<String, ProviderError> {
    let client = crate::http::build_client();
    let identity = identity("linux", "x86_64");
    discover_project(&Discovery {
        surface,
        client: &client,
        endpoint: &fake.base,
        access_token: TOKEN,
        env,
        identity: &identity,
        clock,
        cancel,
        deadline,
    })
    .await
}

async fn gemini(
    fake: &Fake,
    env: &EnvSnapshot,
    clock: &RecordingClock,
) -> Result<String, ProviderError> {
    run(
        Surface::GeminiCli,
        fake,
        env,
        clock,
        &CancellationToken::new(),
        soon(),
    )
    .await
}

async fn antigravity(fake: &Fake, clock: &RecordingClock) -> Result<String, ProviderError> {
    run(
        Surface::Antigravity {
            client_version: "9.9.9",
        },
        fake,
        &EnvSnapshot::default(),
        clock,
        &CancellationToken::new(),
        soon(),
    )
    .await
}

fn status_of(error: ProviderError) -> (u16, String) {
    match error {
        ProviderError::Status {
            status, message, ..
        } => (status, message),
        other => panic!("expected a status error, got {other:?}"),
    }
}

fn invalid_message(error: ProviderError) -> String {
    match error {
        ProviderError::InvalidRequest { message } => message,
        other => panic!("expected an invalid request, got {other:?}"),
    }
}

fn transport_reason(error: ProviderError) -> String {
    match error {
        ProviderError::Transport { reason, .. } => reason,
        other => panic!("expected a transport error, got {other:?}"),
    }
}

// -- environment project ---------------------------------------------------------------

#[test]
fn env_project_prefers_the_project_name_then_the_id_name() {
    let both = EnvSnapshot::test(&[
        ("GOOGLE_CLOUD_PROJECT_ID", "from-id"),
        ("GOOGLE_CLOUD_PROJECT", "first"),
    ]);
    assert_eq!(env_project(&both), Some("first"));
    let blank_first = EnvSnapshot::test(&[
        ("GOOGLE_CLOUD_PROJECT", "   "),
        ("GOOGLE_CLOUD_PROJECT_ID", "from-id"),
    ]);
    assert_eq!(env_project(&blank_first), Some("from-id"));
    let empty_first = EnvSnapshot::test(&[
        ("GOOGLE_CLOUD_PROJECT", ""),
        ("GOOGLE_CLOUD_PROJECT_ID", "from-id"),
    ]);
    assert_eq!(env_project(&empty_first), Some("from-id"));
    assert_eq!(env_project(&EnvSnapshot::default()), None);
    // Other Google project variables belong to Vertex and never answer here.
    assert_eq!(
        env_project(&EnvSnapshot::test(&[("GCP_PROJECT", "vertex")])),
        None
    );
}

// -- Gemini CLI discovery ---------------------------------------------------------------

#[tokio::test]
async fn current_tier_answers_with_the_server_project_and_cli_identity() {
    let fake = serve(vec![ok(
        r#"{"cloudaicompanionProject":"srv-proj","currentTier":{"id":"standard-tier"}}"#,
    )])
    .await;
    let project = gemini(&fake, &EnvSnapshot::default(), &RecordingClock::default())
        .await
        .unwrap();
    assert_eq!(project, "srv-proj");
    let seen = fake.seen.lock().await;
    assert_eq!(seen.len(), 1);
    let request = &seen[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.target, "/v1internal:loadCodeAssist");
    assert_eq!(request.body, format!("{{{CLI_METADATA}}}"));
    assert_eq!(request.header("authorization"), Some("Bearer tok-secret-1"));
    assert_eq!(request.header("client-metadata"), Some(CLIENT_METADATA));
    assert_eq!(
        request.header("user-agent"),
        Some("GeminiCLI/0.46.0/gemini-3.1-pro-preview (linux; x64; terminal)")
    );
    assert_eq!(request.header("content-type"), Some("application/json"));
}

#[tokio::test]
async fn env_project_rides_the_load_body_and_answers_a_projectless_tier() {
    let fake = serve(vec![ok(r#"{"currentTier":{"id":"standard-tier"}}"#)]).await;
    let env = EnvSnapshot::test(&[
        ("GOOGLE_CLOUD_PROJECT", "   "),
        ("GOOGLE_CLOUD_PROJECT_ID", "env-proj"),
    ]);
    let project = gemini(&fake, &env, &RecordingClock::default())
        .await
        .unwrap();
    assert_eq!(project, "env-proj");
    assert_eq!(
        fake.body(0).await,
        r#"{"cloudaicompanionProject":"env-proj","metadata":{"ideType":"IDE_UNSPECIFIED","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI","duetProject":"env-proj"}}"#
    );
}

#[tokio::test]
async fn current_tier_without_any_project_names_the_remedy() {
    let fake = serve(vec![ok(r#"{"currentTier":{"id":"standard-tier"}}"#)]).await;
    let error = gemini(&fake, &EnvSnapshot::default(), &RecordingClock::default())
        .await
        .unwrap_err();
    let message = invalid_message(error);
    let names: std::collections::BTreeSet<_> = message
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .collect();
    assert!(names.contains("GOOGLE_CLOUD_PROJECT"), "{message}");
    assert!(names.contains("GOOGLE_CLOUD_PROJECT_ID"), "{message}");
    assert_eq!(fake.count().await, 1, "no onboarding for an onboarded tier");
}

#[tokio::test]
async fn vpc_sc_refusal_reads_as_standard_tier() {
    let refusal = r#"{"error":{"code":403,"message":"blocked","details":[{"reason":"SECURITY_POLICY_VIOLATED"}]}}"#;
    let with_env = serve(vec![refuse(403, refusal)]).await;
    let env = EnvSnapshot::test(&[("GOOGLE_CLOUD_PROJECT", "vpc-proj")]);
    let project = gemini(&with_env, &env, &RecordingClock::default())
        .await
        .unwrap();
    assert_eq!(project, "vpc-proj");
    assert_eq!(with_env.count().await, 1);

    let without_env = serve(vec![refuse(403, refusal)]).await;
    let error = gemini(
        &without_env,
        &EnvSnapshot::default(),
        &RecordingClock::default(),
    )
    .await
    .unwrap_err();
    assert!(invalid_message(error).contains("GOOGLE_CLOUD_PROJECT"));
    assert_eq!(
        without_env.count().await,
        1,
        "no onboarding on the VPC path"
    );
}

#[tokio::test]
async fn a_refused_load_keeps_the_status_and_hides_the_token() {
    let fake = serve(vec![refuse(
        500,
        r#"{"error":{"code":500,"status":"INTERNAL","message":"boom tok-secret-1"}}"#,
    )])
    .await;
    let error = gemini(&fake, &EnvSnapshot::default(), &RecordingClock::default())
        .await
        .unwrap_err();
    let (status, message) = status_of(error);
    assert_eq!(status, 500);
    assert_eq!(message, "loadCodeAssist failed: boom <redacted>");
}

#[tokio::test]
async fn validation_required_renders_the_fixed_verification_text() {
    let body = r#"{"error":{"code":403,"message":"verify","details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"VALIDATION_REQUIRED","metadata":{"validation_url":"https://accounts.google.com/verify?id=1"}}]}}"#;
    let fake = serve(vec![refuse(403, body)]).await;
    let error = gemini(&fake, &EnvSnapshot::default(), &RecordingClock::default())
        .await
        .unwrap_err();
    let (status, message) = status_of(error);
    assert_eq!(status, 403);
    assert_eq!(
        message,
        "Account verification required. Visit https://accounts.google.com/verify?id=1 to continue, then sign in again."
    );
}

#[tokio::test]
async fn free_tier_onboards_without_the_project_then_polls_to_done() {
    let fake = serve(vec![
        ok(FREE_DEFAULT),
        ok(r#"{"name":"operations/op-1","done":false}"#),
        ok(r#"{"done":false}"#),
        ok(r#"{"done":true,"response":{"cloudaicompanionProject":{"id":"lro-proj"}}}"#),
    ])
    .await;
    let env = EnvSnapshot::test(&[("GOOGLE_CLOUD_PROJECT", "env-proj")]);
    let clock = RecordingClock::default();
    let project = gemini(&fake, &env, &clock).await.unwrap();
    assert_eq!(project, "lro-proj");
    assert_eq!(
        fake.targets().await,
        vec![
            "POST /v1internal:loadCodeAssist",
            "POST /v1internal:onboardUser",
            "GET /v1internal/operations/op-1",
            "GET /v1internal/operations/op-1",
        ]
    );
    assert_eq!(
        fake.body(1).await,
        format!(r#"{{"tierId":"free-tier",{CLI_METADATA}}}"#),
        "the free tier never carries the environment project"
    );
    assert_eq!(
        clock.pauses(),
        vec![Duration::from_secs(5)],
        "the first poll is immediate and each later poll waits five seconds"
    );
}

#[tokio::test]
async fn legacy_tier_onboards_with_the_environment_project() {
    let fake = serve(vec![
        ok(r#"{"allowedTiers":[]}"#),
        ok(r#"{"done":true,"response":{"cloudaicompanionProject":{"id":"srv-proj"}}}"#),
    ])
    .await;
    let env = EnvSnapshot::test(&[("GOOGLE_CLOUD_PROJECT", "env-proj")]);
    let clock = RecordingClock::default();
    let project = gemini(&fake, &env, &clock).await.unwrap();
    assert_eq!(project, "srv-proj");
    assert_eq!(
        fake.body(1).await,
        r#"{"tierId":"legacy-tier","metadata":{"ideType":"IDE_UNSPECIFIED","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI","duetProject":"env-proj"},"cloudaicompanionProject":"env-proj"}"#
    );
    assert!(
        clock.pauses().is_empty(),
        "a done operation is never polled"
    );
    assert_eq!(fake.count().await, 2);
}

#[tokio::test]
async fn a_done_operation_without_a_project_falls_back_to_the_environment() {
    let fake = serve(vec![ok(FREE_DEFAULT), ok(r#"{"done":true}"#)]).await;
    let env = EnvSnapshot::test(&[("GOOGLE_CLOUD_PROJECT_ID", "env-proj")]);
    let project = gemini(&fake, &env, &RecordingClock::default())
        .await
        .unwrap();
    assert_eq!(project, "env-proj");

    let bare = serve(vec![ok(FREE_DEFAULT), ok(r#"{"done":true}"#)]).await;
    let error = gemini(&bare, &EnvSnapshot::default(), &RecordingClock::default())
        .await
        .unwrap_err();
    let message = invalid_message(error);
    assert!(message.contains("could not discover"), "{message}");
    assert!(message.contains("GOOGLE_CLOUD_PROJECT_ID"), "{message}");
}

#[tokio::test]
async fn an_unfinished_unnamed_operation_is_not_a_success_even_with_an_env_project() {
    let fake = serve(vec![ok(FREE_DEFAULT), ok(r#"{"done":false}"#)]).await;
    let env = EnvSnapshot::test(&[("GOOGLE_CLOUD_PROJECT", "env-proj")]);
    let clock = RecordingClock::default();
    let error = gemini(&fake, &env, &clock).await.unwrap_err();
    assert!(
        transport_reason(error).contains("unfinished operation without a name"),
        "the environment project must not stand in for a provisioning that never ran"
    );
    assert_eq!(fake.count().await, 2, "nothing to poll");
    assert_eq!(clock.pauses(), [] as [std::time::Duration; 0]);

    let named_project = serve(vec![
        ok(FREE_DEFAULT),
        ok(r#"{"done":false,"response":{"cloudaicompanionProject":{"id":"lro-proj"}}}"#),
    ])
    .await;
    let project = gemini(&named_project, &env, &RecordingClock::default())
        .await
        .unwrap();
    assert_eq!(project, "lro-proj", "a project the operation names is used");
}

#[tokio::test]
async fn the_flagged_default_tier_is_the_one_onboarded() {
    let fake = serve(vec![
        ok(r#"{"allowedTiers":[{"id":"standard-tier"},{"id":"enterprise-tier","isDefault":true}]}"#),
        ok(r#"{"done":true}"#),
    ])
    .await;
    let env = EnvSnapshot::test(&[("GOOGLE_CLOUD_PROJECT", "env-proj")]);
    gemini(&fake, &env, &RecordingClock::default())
        .await
        .unwrap();
    assert!(
        fake.body(1)
            .await
            .starts_with(r#"{"tierId":"enterprise-tier","#)
    );
}

#[tokio::test]
async fn a_paid_tier_without_an_environment_project_is_refused_before_onboarding() {
    let fake = serve(vec![ok(
        r#"{"allowedTiers":[{"id":"standard-tier","isDefault":true}]}"#,
    )])
    .await;
    let error = gemini(&fake, &EnvSnapshot::default(), &RecordingClock::default())
        .await
        .unwrap_err();
    assert!(invalid_message(error).contains("GOOGLE_CLOUD_PROJECT"));
    assert_eq!(fake.count().await, 1);
}

#[tokio::test]
async fn provisioning_polls_stop_at_twenty_four_with_five_second_pauses() {
    let mut script = vec![
        ok(FREE_DEFAULT),
        ok(r#"{"name":"operations/op-9","done":false}"#),
    ];
    script.extend(std::iter::repeat_n(ok(r#"{"done":false}"#), 24));
    let fake = serve(script).await;
    let clock = RecordingClock::default();
    let error = gemini(&fake, &EnvSnapshot::default(), &clock)
        .await
        .unwrap_err();
    let reason = transport_reason(error);
    assert!(reason.contains("24 polls"), "{reason}");
    assert_eq!(
        fake.count().await,
        26,
        "load, onboard, and exactly 24 polls"
    );
    assert_eq!(clock.pauses(), vec![Duration::from_secs(5); 23]);
}

#[tokio::test]
async fn a_refused_poll_keeps_its_status() {
    let fake = serve(vec![
        ok(FREE_DEFAULT),
        ok(r#"{"name":"operations/op-2","done":false}"#),
        refuse(500, r#"{"error":{"message":"gone"}}"#),
    ])
    .await;
    let error = gemini(&fake, &EnvSnapshot::default(), &RecordingClock::default())
        .await
        .unwrap_err();
    let (status, message) = status_of(error);
    assert_eq!(status, 500);
    assert_eq!(message, "operation poll failed: gone");
}

#[tokio::test]
async fn an_operation_name_that_is_not_a_path_is_never_requested() {
    for name in ["../escape", "ops?x=1", "a#b", "a b", "a//b", "%2e%2e/x"] {
        let fake = serve(vec![
            ok(FREE_DEFAULT),
            ok(&format!(r#"{{"name":"{name}","done":false}}"#)),
        ])
        .await;
        let error = gemini(&fake, &EnvSnapshot::default(), &RecordingClock::default())
            .await
            .unwrap_err();
        let reason = transport_reason(error);
        assert!(
            reason.contains("operation name") || reason.contains("endpoint path"),
            "{name}: {reason}"
        );
        assert_eq!(fake.count().await, 2, "{name}: no poll request was sent");
    }
}

#[tokio::test]
async fn a_reply_that_is_not_json_is_a_transport_error() {
    let fake = serve(vec![ok("not json")]).await;
    let error = gemini(&fake, &EnvSnapshot::default(), &RecordingClock::default())
        .await
        .unwrap_err();
    assert!(
        transport_reason(error).contains("loadCodeAssist returned a body that is not valid JSON")
    );
}

// -- cancellation and deadline ----------------------------------------------------------

#[tokio::test]
async fn cancellation_ends_a_stalled_request() {
    let fake = serve(vec![Step::Stall]).await;
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    let clock = RecordingClock::default();
    let env = EnvSnapshot::default();
    let (outcome, ()) = tokio::join!(
        tokio::time::timeout(
            Duration::from_secs(20),
            run(Surface::GeminiCli, &fake, &env, &clock, &cancel, soon()),
        ),
        async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            trigger.cancel();
        }
    );
    assert!(matches!(outcome.unwrap(), Err(ProviderError::Cancelled)));
}

#[tokio::test]
async fn a_cancelled_token_sends_nothing() {
    let fake = serve(vec![ok(r#"{"currentTier":{}}"#)]).await;
    let cancel = CancellationToken::new();
    cancel.cancel();
    let outcome = run(
        Surface::GeminiCli,
        &fake,
        &EnvSnapshot::default(),
        &RecordingClock::default(),
        &cancel,
        soon(),
    )
    .await;
    assert!(matches!(outcome, Err(ProviderError::Cancelled)));
    assert_eq!(fake.count().await, 0);
}

#[tokio::test]
async fn cancellation_during_the_poll_pause_stops_before_the_next_poll() {
    let fake = serve(vec![
        ok(FREE_DEFAULT),
        ok(r#"{"name":"operations/op-3","done":false}"#),
        ok(r#"{"done":false}"#),
    ])
    .await;
    let cancel = CancellationToken::new();
    let clock = CancelOnPause {
        cancel: cancel.clone(),
    };
    let outcome = run(
        Surface::GeminiCli,
        &fake,
        &EnvSnapshot::default(),
        &clock,
        &cancel,
        soon(),
    )
    .await;
    assert!(matches!(outcome, Err(ProviderError::Cancelled)));
    assert_eq!(fake.count().await, 3, "no poll after the cancelled pause");
}

#[tokio::test]
async fn the_deadline_bounds_a_stalled_request() {
    let fake = serve(vec![Step::Stall]).await;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(150);
    let env = EnvSnapshot::default();
    let clock = RecordingClock::default();
    let cancel = CancellationToken::new();
    let outcome = tokio::time::timeout(
        Duration::from_secs(20),
        run(Surface::GeminiCli, &fake, &env, &clock, &cancel, deadline),
    )
    .await
    .unwrap();
    assert!(transport_reason(outcome.unwrap_err()).contains("deadline"));
}

#[tokio::test]
async fn a_passed_deadline_sends_nothing() {
    let fake = serve(vec![ok(r#"{"currentTier":{}}"#)]).await;
    let deadline = tokio::time::Instant::now();
    let outcome = run(
        Surface::GeminiCli,
        &fake,
        &EnvSnapshot::default(),
        &RecordingClock::default(),
        &CancellationToken::new(),
        deadline,
    )
    .await;
    assert!(transport_reason(outcome.unwrap_err()).contains("deadline"));
    assert_eq!(fake.count().await, 0);
}

// -- Antigravity discovery --------------------------------------------------------------

#[tokio::test]
async fn antigravity_onboards_the_free_tier_then_reads_the_project_again() {
    let fake = serve(vec![
        ok(FREE_DEFAULT),
        ok(r#"{"name":"op/1","done":false}"#),
        ok(r#"{"done":true,"response":{"@type":"t","cloudaicompanionProject":"ignored"}}"#),
        ok(r#"{"cloudaicompanionProject":"ag-proj","currentTier":{"id":"free-tier"},"paidTier":{"id":"free-tier"}}"#),
    ])
    .await;
    let clock = RecordingClock::default();
    let project = antigravity(&fake, &clock).await.unwrap();
    assert_eq!(project, "ag-proj");
    assert_eq!(
        fake.targets().await,
        vec![
            "POST /v1internal:loadCodeAssist",
            "POST /v1internal:onboardUser",
            "GET /v1internal/op/1",
            "POST /v1internal:loadCodeAssist",
        ]
    );
    assert_eq!(
        fake.body(0).await,
        r#"{"metadata":{"ideType":"ANTIGRAVITY"}}"#
    );
    assert_eq!(
        fake.body(1).await,
        r#"{"tierId":"free-tier","metadata":{"ideType":"ANTIGRAVITY"}}"#
    );
    assert_eq!(clock.pauses(), vec![Duration::from_secs(1)]);
    let seen = fake.seen.lock().await;
    assert_eq!(
        seen[0].header("user-agent"),
        Some("antigravity/hub/9.9.9 (aidev_client; os_type=darwin; arch=arm64; cl=963137146)")
    );
    assert_eq!(seen[0].header("client-metadata"), None);
    assert_eq!(seen[0].header("authorization"), Some("Bearer tok-secret-1"));
}

#[tokio::test]
async fn antigravity_restates_the_project_when_the_first_load_has_no_paid_tier() {
    let fake = serve(vec![
        ok(r#"{"cloudaicompanionProject":"p1","currentTier":{"id":"x"}}"#),
        ok(r#"{"cloudaicompanionProject":"p1","currentTier":{"id":"x"},"paidTier":{"id":"x"}}"#),
        ok(r#"{"cloudaicompanionProject":"p1","currentTier":{"id":"x"},"paidTier":{"id":"x"}}"#),
    ])
    .await;
    let project = antigravity(&fake, &RecordingClock::default())
        .await
        .unwrap();
    assert_eq!(project, "p1");
    assert_eq!(fake.count().await, 3);
    assert_eq!(
        fake.body(1).await,
        r#"{"cloudaicompanionProject":"p1","metadata":{"ideType":"ANTIGRAVITY"}}"#
    );
}

#[tokio::test]
async fn antigravity_reports_a_refused_free_tier_with_its_url() {
    let fake = serve(vec![ok(
        r#"{"allowedTiers":[{"id":"standard-tier"}],"ineligibleTiers":[{"tierId":"free-tier","reasonMessage":"Not available in your region","validationUrl":"https://example.com/verify"}]}"#,
    )])
    .await;
    let error = antigravity(&fake, &RecordingClock::default())
        .await
        .unwrap_err();
    assert_eq!(
        invalid_message(error),
        "Not available in your region\nhttps://example.com/verify"
    );
    assert_eq!(fake.count().await, 1);
}

#[tokio::test]
async fn antigravity_reports_a_failed_onboarding_operation() {
    let fake = serve(vec![
        ok(FREE_DEFAULT),
        ok(r#"{"done":true,"error":{"code":9,"message":"quota"}}"#),
    ])
    .await;
    let error = antigravity(&fake, &RecordingClock::default())
        .await
        .unwrap_err();
    assert_eq!(
        invalid_message(error),
        "onboardUser operation failed: 9: quota"
    );
}

#[tokio::test]
async fn antigravity_polls_at_most_thirty_times_one_second_apart() {
    let mut script = vec![ok(FREE_DEFAULT), ok(r#"{"name":"op/2","done":false}"#)];
    script.extend(std::iter::repeat_n(ok(r#"{"done":false}"#), 30));
    let fake = serve(script).await;
    let clock = RecordingClock::default();
    let error = antigravity(&fake, &clock).await.unwrap_err();
    assert!(transport_reason(error).contains("30 polls"));
    assert_eq!(fake.count().await, 32);
    assert_eq!(clock.pauses(), vec![Duration::from_secs(1); 30]);
}

// -- catalog project read ---------------------------------------------------------------

async fn plain_project(
    fake: &Fake,
    cancel: &CancellationToken,
) -> Result<Option<String>, ProviderError> {
    let client = crate::http::build_client();
    let identity = identity("linux", "x86_64");
    let env = EnvSnapshot::test(&[("GOOGLE_CLOUD_PROJECT", "never-sent")]);
    let clock = RecordingClock::default();
    load_project_id(&Discovery {
        surface: Surface::GeminiCli,
        client: &client,
        endpoint: &fake.base,
        access_token: TOKEN,
        env: &env,
        identity: &identity,
        clock: &clock,
        cancel,
        deadline: soon(),
    })
    .await
}

#[tokio::test]
async fn load_project_id_reads_a_string_or_an_id_object_and_sends_no_env_project() {
    let named = serve(vec![ok(r#"{"cloudaicompanionProject":"str-proj"}"#)]).await;
    assert_eq!(
        plain_project(&named, &CancellationToken::new())
            .await
            .unwrap(),
        Some(String::from("str-proj"))
    );
    assert_eq!(named.body(0).await, format!("{{{CLI_METADATA}}}"));

    let object = serve(vec![ok(r#"{"cloudaicompanionProject":{"id":"obj-proj"}}"#)]).await;
    assert_eq!(
        plain_project(&object, &CancellationToken::new())
            .await
            .unwrap(),
        Some(String::from("obj-proj"))
    );
}

#[tokio::test]
async fn load_project_id_is_none_on_failure_and_an_error_on_cancellation() {
    let refused = serve(vec![refuse(500, "{}")]).await;
    assert_eq!(
        plain_project(&refused, &CancellationToken::new())
            .await
            .unwrap(),
        None
    );
    let garbled = serve(vec![ok("nope")]).await;
    assert_eq!(
        plain_project(&garbled, &CancellationToken::new())
            .await
            .unwrap(),
        None
    );
    let idle = serve(vec![ok("{}")]).await;
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        plain_project(&idle, &cancel).await,
        Err(ProviderError::Cancelled)
    ));
}

// -- native request encoding ------------------------------------------------------------

fn request(model: &str, level: ThinkingLevel) -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::GoogleCloudCodeAssist,
            model: model.into(),
            provider: None,
        },
        system: "Be brief.".into(),
        tools: Vec::new().into(),
        context: vec![ContextItem::User {
            parts: vec![Part::Text { text: "Hi".into() }],
        }]
        .into(),
        params: RequestParams {
            thinking: level,
            effort: None,
            temperature: None,
            max_output_tokens: None,
        },
        cache_key: None,
    }
}

fn limits() -> RequestLimits {
    RequestLimits {
        max_tokens: 65_536,
        max_output: 65_536,
        temperature_allowed: true,
    }
}

fn levels(accepted: &[ThinkingLevel], none_supported: bool) -> ThinkingSupport {
    ThinkingSupport::OpenAi {
        accepted: accepted.to_vec(),
        none_supported,
    }
}

/// Builds the wire the way the request adapter would: the planner composes
/// the plan from the requested level and the capability row.
fn wire_for(
    model: &str,
    level: ThinkingLevel,
    caps: &ThinkingSupport,
    patch: &BeforeRequestPatch,
) -> GoogleWire {
    let request = request(model, level);
    let composed = plan(level, patch, caps, limits());
    build(&NativeRequest {
        request: &request,
        thinking: &composed,
        provider: "google-gemini-cli",
        access_token: TOKEN,
        project: "proj-1",
        identity: &identity("macos", "aarch64"),
        image_input: false,
    })
    .unwrap()
}

fn body_text(wire: &GoogleWire) -> String {
    String::from_utf8(wire.body.clone()).unwrap()
}

#[test]
fn build_writes_exactly_the_three_member_envelope() {
    let wire = wire_for(
        "gemini-2.5-pro",
        ThinkingLevel::High,
        &levels(&[ThinkingLevel::High], true),
        &BeforeRequestPatch::default(),
    );
    let body = body_text(&wire);
    assert_eq!(
        body,
        r#"{"project":"proj-1","model":"gemini-2.5-pro","request":{"contents":[{"role":"user","parts":[{"text":"Hi"}]}],"systemInstruction":{"parts":[{"text":"Be brief."}]},"generationConfig":{"thinkingConfig":{"includeThoughts":true,"thinkingLevel":"HIGH"}}}}"#
    );
    for foreign in [
        "requestId",
        "userAgent",
        "requestType",
        "labels",
        "sessionId",
    ] {
        assert!(
            !body.contains(foreign),
            "{foreign} is Antigravity-only: {body}"
        );
    }
}

#[test]
fn build_sends_the_cli_identity_to_the_configured_base() {
    let wire = wire_for(
        "gemini-2.5-pro",
        ThinkingLevel::Off,
        &levels(&[], true),
        &BeforeRequestPatch::default(),
    );
    assert_eq!(wire.path, "v1internal:streamGenerateContent?alt=sse");
    assert_eq!(wire.base_override, None);
    assert!(wire.fallback.is_none());
    assert_eq!(
        wire.headers,
        vec![
            ("authorization", String::from("Bearer tok-secret-1")),
            ("client-metadata", String::from(CLIENT_METADATA)),
        ]
    );
    assert_eq!(
        wire.user_agent.as_deref(),
        Some("GeminiCLI/0.46.0/gemini-2.5-pro (darwin; arm64; terminal)")
    );
}

#[test]
fn build_refuses_a_foreign_route_and_a_blank_project() {
    let composed = plan(
        ThinkingLevel::Off,
        &BeforeRequestPatch::default(),
        &levels(&[], true),
        limits(),
    );
    let identity = identity("linux", "x86_64");
    let input = |request: &ModelRequest, project: &'static str| {
        build(&NativeRequest {
            request,
            thinking: &composed,
            provider: "gemini-work",
            access_token: TOKEN,
            project,
            identity: &identity,
            image_input: false,
        })
    };
    // Every other family is foreign, the local Gemini CLI subprocess included.
    for family in Family::ALL
        .into_iter()
        .filter(|family| *family != Family::GoogleCloudCodeAssist)
    {
        let mut foreign = request("gemini-2.5-pro", ThinkingLevel::Off);
        foreign.model = ModelRoute::Api {
            family,
            model: "gemini-2.5-pro".into(),
            provider: None,
        };
        assert!(
            matches!(
                input(&foreign, "proj-1"),
                Err(ProviderError::InvalidRequest { .. })
            ),
            "{family:?}"
        );
    }
    let own = request("gemini-2.5-pro", ThinkingLevel::Off);
    let message = invalid_message(input(&own, "  ").unwrap_err());
    assert_eq!(
        message,
        "gemini-work has no Google Cloud project for this sign-in; run dalgon login gemini-work again."
    );
}

// A stored thought signature is bound to the family that produced it, so only
// a Cloud Code Assist message replays into a native request; the local Gemini
// CLI and every other family's signature never crosses.
#[test]
fn stored_signatures_replay_only_from_the_native_family() {
    let composed = plan(
        ThinkingLevel::Off,
        &BeforeRequestPatch::default(),
        &levels(&[], true),
        limits(),
    );
    let identity = identity("linux", "x86_64");
    for family in Family::ALL {
        let signed = ContextItem::Assistant {
            source: dal_core::ReplaySource {
                family,
                model: "gemini-2.5-pro".into(),
            },
            replay: None,
            parts: vec![
                dal_core::AssistantPart::Thinking {
                    text: "prior".into(),
                    replay: Some(
                        dal_core::RawJson::parse(r#"{"calls":{"call_1":"QUJD"}}"#).unwrap(),
                    ),
                },
                dal_core::AssistantPart::ToolCall {
                    call: dal_core::CallId::new("call_1"),
                    name: "read".into(),
                    args: dal_core::RawJson::parse("{}").unwrap(),
                    replay: None,
                },
            ],
        };
        let request = ModelRequest {
            context: vec![signed].into(),
            ..request("gemini-2.5-pro", ThinkingLevel::Off)
        };
        let wire = build(&NativeRequest {
            request: &request,
            thinking: &composed,
            provider: "gemini-work",
            access_token: TOKEN,
            project: "proj-1",
            identity: &identity,
            image_input: false,
        })
        .unwrap();
        let body = body_text(&wire);
        assert_eq!(
            body.contains(r#""thoughtSignature":"QUJD""#),
            family == Family::GoogleCloudCodeAssist,
            "{family:?}: {body}"
        );
    }
}

#[test]
fn compat_follows_the_pinned_classes() {
    let claude = compat("claude-sonnet-4-6");
    assert!(claude.part_ids && claude.legacy_parameters && claude.replay_thinking);
    assert_eq!(claude.skip, SignaturePolicy::Never);
    assert_eq!(claude.tool_mode, ToolMode::Auto);

    let three = compat("gemini-3.1-pro-preview");
    assert!(!three.part_ids && !three.legacy_parameters && three.nested_tool_images);
    assert_eq!(three.skip, SignaturePolicy::FirstCall);

    let two = compat("gemini-2.5-pro");
    assert!(!two.part_ids && !two.nested_tool_images);
    assert_eq!(two.skip, SignaturePolicy::Never);

    let open = compat("gpt-oss-120b-medium");
    assert!(open.part_ids && !open.legacy_parameters);
}

#[test]
fn user_agent_reports_node_platform_names() {
    assert_eq!(
        user_agent("m", &identity("windows", "x86_64")),
        "GeminiCLI/0.46.0/m (win32; x64; terminal)"
    );
    assert_eq!(
        user_agent("m", &identity("linux", "aarch64")),
        "GeminiCLI/0.46.0/m (linux; arm64; terminal)"
    );
    assert_eq!(
        user_agent("m", &identity("freebsd", "riscv64")),
        "GeminiCLI/0.46.0/m (freebsd; riscv64; terminal)"
    );
}

// -- explicit thinking-off --------------------------------------------------------------

const OFF_LEVEL: &str =
    r#""generationConfig":{"thinkingConfig":{"includeThoughts":false,"thinkingLevel":"MINIMAL"}}"#;
const OFF_BUDGET: &str =
    r#""generationConfig":{"thinkingConfig":{"includeThoughts":false,"thinkingBudget":0}}"#;

/// The ids the pinned catalog flags `suppress-when-off` for the
/// `google-gemini-cli` provider.
const FLAGGED: [&str; 7] = [
    "gemini-3.5-flash-extra-low",
    "gemini-3.5-flash-low",
    "gemini-3-flash-agent",
    "gemini-3.1-pro-low",
    "gemini-pro-agent",
    "gemini-3-pro-low",
    "gemini-3-pro-high",
];

#[test]
fn explicit_off_on_every_flagged_id_names_the_minimal_level() {
    for model in FLAGGED {
        let wire = wire_for(
            model,
            ThinkingLevel::Off,
            &levels(&[ThinkingLevel::Low, ThinkingLevel::High], true),
            &BeforeRequestPatch::default(),
        );
        assert!(
            body_text(&wire).contains(OFF_LEVEL),
            "{model}: {}",
            body_text(&wire)
        );
    }
}

#[test]
fn a_budget_row_on_a_flagged_id_names_a_zero_budget() {
    let wire = wire_for(
        "gemini-3.5-flash-low",
        ThinkingLevel::Off,
        &ThinkingSupport::Budget { can_disable: true },
        &BeforeRequestPatch::default(),
    );
    assert!(
        body_text(&wire).contains(OFF_BUDGET),
        "{}",
        body_text(&wire)
    );
}

#[test]
fn explicit_off_on_an_unflagged_model_never_names_a_suppression() {
    // The row accepts off, so the planner expresses it; the pinned catalog
    // does not flag these ids, and the backend rejects an unsupported
    // suppression (a level that cannot be minimal, a budget that floors).
    for model in [
        "gemini-3.1-pro-preview",
        "gemini-3.7-flash",
        "gemini-2.5-pro",
        "gemini-2.5-flash",
        "gemini-2.0-flash",
        "claude-sonnet-4-6",
        "gpt-oss-120b-medium",
    ] {
        let effort = wire_for(
            model,
            ThinkingLevel::Off,
            &levels(&[ThinkingLevel::Low, ThinkingLevel::High], true),
            &BeforeRequestPatch::default(),
        );
        let body = body_text(&effort);
        assert!(!body.contains("thinkingConfig"), "{model}: {body}");
        // A budget row keeps the shared mapping of a disabled budget plan
        // (`includeThoughts: false` alone); the suppression spellings never
        // appear.
        let budget = wire_for(
            model,
            ThinkingLevel::Off,
            &ThinkingSupport::Budget { can_disable: true },
            &BeforeRequestPatch::default(),
        );
        let body = body_text(&budget);
        assert!(!body.contains("MINIMAL"), "{model}: {body}");
        assert!(!body.contains("thinkingBudget"), "{model}: {body}");
        assert!(!body.contains("thinkingLevel"), "{model}: {body}");
    }
}

#[test]
fn a_level_the_row_clamped_to_off_stays_omitted() {
    // Medium was requested; the row accepts no levels, so the planner clamps
    // to off. That is the default, not an explicit off, even on a flagged id.
    let wire = wire_for(
        "gemini-3.1-pro-low",
        ThinkingLevel::Medium,
        &levels(&[], true),
        &BeforeRequestPatch::default(),
    );
    assert!(
        !body_text(&wire).contains("thinkingConfig"),
        "{}",
        body_text(&wire)
    );
}

#[test]
fn a_row_that_cannot_disable_keeps_the_member_omitted() {
    let effort_row = wire_for(
        "gemini-3.1-pro-low",
        ThinkingLevel::Off,
        &levels(&[ThinkingLevel::Low], false),
        &BeforeRequestPatch::default(),
    );
    assert!(!body_text(&effort_row).contains("thinkingConfig"));
    let budget_row = wire_for(
        "gemini-3.5-flash-low",
        ThinkingLevel::Off,
        &ThinkingSupport::Budget { can_disable: false },
        &BeforeRequestPatch::default(),
    );
    assert!(!body_text(&budget_row).contains("thinkingConfig"));
}

#[test]
fn a_hook_effort_that_raises_the_level_is_not_an_off() {
    let patch = BeforeRequestPatch {
        effort: Some(Effort::High),
        temperature: None,
    };
    let wire = wire_for(
        "gemini-3.1-pro-low",
        ThinkingLevel::Off,
        &levels(&[ThinkingLevel::Low, ThinkingLevel::High], true),
        &patch,
    );
    let body = body_text(&wire);
    assert!(
        body.contains(r#""thinkingConfig":{"includeThoughts":true,"thinkingLevel":"HIGH"}"#),
        "{body}"
    );
    assert!(!body.contains("MINIMAL"), "{body}");
}

#[test]
fn a_requested_level_on_a_flagged_id_is_not_suppressed() {
    let wire = wire_for(
        "gemini-3.1-pro-low",
        ThinkingLevel::High,
        &levels(&[ThinkingLevel::Low, ThinkingLevel::High], true),
        &BeforeRequestPatch::default(),
    );
    let body = body_text(&wire);
    assert!(
        body.contains(r#""thinkingConfig":{"includeThoughts":true,"thinkingLevel":"HIGH"}"#),
        "{body}"
    );
}
