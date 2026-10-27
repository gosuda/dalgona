//! The native Google Cloud Code Assist family through the real provider
//! lifecycle: sign-in, readiness, live model list, model selection, the plain
//! request envelope, tools, stream decode, errors, cancellation, replay
//! isolation, and the preserved local `google-gemini-cli` subprocess. Every
//! case runs offline against a scripted loopback server that records the
//! exact bytes it answered.

use dal_core::{AssistantPart, CallId, ReplaySource};

use super::*;

const NATIVE: &str = "google-cloud-code-assist";
const TOKEN: &str = "ya29-native";

/// Stores a Cloud Code Assist sign-in the way the login commits it: tokens
/// and the project discovery named.
fn stored_cloud_code(
    dir: &TestDir,
    id: &str,
    access: &str,
    project: &str,
) -> Result<(), Box<dyn Error>> {
    let mut auth = crate::auth::credential::AuthStore::empty(dir.path().join("auth.json"));
    auth.set(
        id,
        Credential::OAuth(OAuthCredential {
            access_token: SecretString::from(access),
            refresh_token: SecretString::from("stored-refresh-secret"),
            expires_at: None,
            id_token: None,
            account_id: None,
            project_id: Some(project.to_owned()),
        }),
    )?;
    auth.store()?;
    Ok(())
}

fn native_entry(base: String) -> ProviderEntry {
    entry(
        NATIVE,
        Family::GoogleCloudCodeAssist,
        base,
        None,
        1,
        Transport::Https,
    )
}

fn set_for(dir: &TestDir, rows: Vec<ProviderEntry>) -> Result<ProviderSet, ProviderError> {
    ProviderSet::new(
        &config(rows, 0),
        identity(),
        EnvSnapshot::test(&[]),
        dir.path(),
        dir.path(),
    )
}

fn native_request(model: &str, context: Vec<ContextItem>) -> ModelRequest {
    request(Family::GoogleCloudCodeAssist, model, context)
}

fn tool_spec() -> ModelToolSpec {
    ModelToolSpec {
        name: "read_file".into(),
        description: "Reads a file.".into(),
        parameters: RawJson::parse(r#"{"type":"object","properties":{"path":{"type":"string"}}}"#)
            .expect("a schema"),
        grammar: None,
        defer_loading: false,
    }
}

fn tool_call_reply() -> String {
    String::from(
        "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"name\":\"read_file\",\"args\":{\"path\":\"a.txt\"}}}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":3,\"candidatesTokenCount\":2,\"totalTokenCount\":5}}}\n\n",
    )
}

/// One open of `turn` against `provider`, collected whole.
async fn run_turn(
    provider: &Provider,
    turn: &ModelRequest,
) -> Result<Vec<StreamEvent>, Box<dyn Error>> {
    let stream = provider
        .open(
            SessionId::new_v7(),
            turn,
            &[],
            notice_sink(),
            &CancellationToken::new(),
        )
        .await?;
    Ok(collect(stream).await?)
}

fn head(captured: &[u8]) -> String {
    String::from_utf8_lossy(captured).to_ascii_lowercase()
}

fn body(captured: &[u8]) -> String {
    String::from_utf8_lossy(request_body(captured)).into_owned()
}

#[tokio::test]
async fn a_native_sign_in_runs_discovery_selection_a_tool_turn_and_the_summary_fallback()
-> TestResult {
    let dir = TestDir::new()?;
    let quota = json_reply(
        200,
        r#"{"buckets":[{"modelId":"gemini-3-flash-preview"},{"modelId":"chat_20706"}]}"#,
    );
    let (base, mut server_task) = server("", vec![quota, stream_reply(tool_call_reply())]).await?;
    stored_cloud_code(&dir, NATIVE, TOKEN, "stored-project-9")?;
    let set = set_for(&dir, vec![native_entry(base)])?;
    assert!(
        set.inner.gemini_policy.is_none(),
        "the native row never spawns a subprocess, so it publishes no deny-all policy"
    );
    assert!(
        set.credential_source_available(NATIVE, &CancellationToken::new())
            .await?,
        "a stored sign-in with its project is ready"
    );

    // Model selection reads the live quota list: the account's own wire ids.
    let catalog = set.catalog().await?;
    let listed: Vec<&str> = catalog
        .entries()
        .iter()
        .filter(|row| row.provider.as_ref() == NATIVE)
        .map(|row| row.id.as_ref())
        .collect();
    assert_eq!(listed, ["gemini-3-flash-preview"]);
    let resolved = set.resolve(&catalog, "google-cloud-code-assist/gemini-3-flash-preview")?;
    assert!(
        set.resolve(&catalog, "google-cloud-code-assist/chat_20706")
            .is_err(),
        "an id the account does not list is unknown on a live list"
    );
    assert_eq!(
        resolved.entry.tool_support,
        crate::catalog::ToolSupport::Any
    );
    assert_eq!(resolved.entry.context_window, None, "no invented limit");
    assert!(resolved.entry.price.is_none(), "no invented price");
    assert!(!resolved.entry.remote_compact);

    // One tool turn through the plain envelope.
    let provider = set.provider(resolved.clone())?;
    assert!(matches!(provider, Provider::Http(_)), "not the subprocess");
    let mut turn = native_request("gemini-3-flash-preview", vec![user_text("Read a.txt.")]);
    turn.model = resolved.route.clone();
    turn.tools = Arc::from([tool_spec()]);
    let events = run_turn(&provider, &turn).await?;
    assert!(
        events.iter().any(|event| matches!(
            event,
            StreamEvent::ToolCallStarted { name, .. } if name == "read_file"
        )),
        "{events:?}"
    );
    let arguments: Vec<u8> = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ToolArgsDelta { fragment, .. } => Some(fragment.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(String::from_utf8(arguments)?, r#"{"path":"a.txt"}"#);
    assert!(matches!(
        events.last(),
        Some(StreamEvent::Stop {
            reason: crate::stream::StopReason::ToolUse
        })
    ));

    // Remote compaction has no native endpoint: the caller summarizes locally.
    let mut compact = native_request("gemini-3-flash-preview", vec![user_text("Summarize.")]);
    compact.model = resolved.route;
    compact.purpose = Purpose::Compact;
    let outcome = provider
        .compact(
            SessionId::new_v7(),
            &compact,
            notice_sink(),
            &CancellationToken::new(),
        )
        .await?;
    assert_eq!(outcome, Some(CompactOutcome::Unsupported));

    let captured = server_task.join_next().await.expect("server completes")??;
    assert_quota_and_tool_turn_wire(&captured);
    Ok(())
}

/// The wire of one native sign-in pass: exactly the quota probe and the
/// tool turn arrived, both on the native headers with the stored project,
/// and the turn body is the plain CCA envelope with the tool declarations.
fn assert_quota_and_tool_turn_wire(captured: &[Vec<u8>]) {
    assert_eq!(captured.len(), 2, "the compaction sent nothing");
    let quota_head = head(&captured[0]);
    assert!(
        quota_head.starts_with("post /v1internal:retrieveuserquota "),
        "{quota_head}"
    );
    assert!(
        quota_head
            .contains("user-agent: geminicli/0.46.0/gemini-3.1-pro-preview (linux; x64; terminal)"),
        "{quota_head}"
    );
    assert_eq!(body(&captured[0]), r#"{"project":"stored-project-9"}"#);

    let turn_head = head(&captured[1]);
    assert!(
        turn_head.starts_with("post /v1internal:streamgeneratecontent?alt=sse "),
        "{turn_head}"
    );
    assert!(
        turn_head.contains(&format!("authorization: bearer {TOKEN}")),
        "{turn_head}"
    );
    assert!(
        turn_head.contains(
            "client-metadata: ideType=IDE_UNSPECIFIED,platform=PLATFORM_UNSPECIFIED,pluginType=GEMINI"
                .to_ascii_lowercase()
                .as_str()
        ),
        "{turn_head}"
    );
    assert!(
        turn_head
            .contains("user-agent: geminicli/0.46.0/gemini-3-flash-preview (linux; x64; terminal)"),
        "{turn_head}"
    );
    assert!(
        turn_head.contains("accept: text/event-stream"),
        "{turn_head}"
    );
    let turn_body = body(&captured[1]);
    assert!(
        turn_body.starts_with(
            r#"{"project":"stored-project-9","model":"gemini-3-flash-preview","request":{"#
        ),
        "the plain envelope names the stored project first: {turn_body}"
    );
    for foreign in [
        "requestId",
        "userAgent",
        "requestType",
        "labels",
        "sessionId",
    ] {
        assert!(
            !turn_body.contains(foreign),
            "{foreign} belongs to Antigravity: {turn_body}"
        );
    }
    assert!(turn_body.contains("functionDeclarations"), "{turn_body}");
    assert!(turn_body.contains(r#""name":"read_file""#), "{turn_body}");
}

#[tokio::test]
async fn a_provider_setting_never_replaces_the_stored_native_project() -> TestResult {
    let dir = TestDir::new()?;
    let (base, mut server_task) = server("", vec![stream_reply(google_cca_reply())]).await?;
    stored_cloud_code(&dir, NATIVE, TOKEN, "stored-project-1")?;
    let mut row = native_entry(base);
    row.settings.project = Some("settings-project".into());
    let set = set_for(&dir, vec![row])?;
    let provider = set.provider(resolved(
        NATIVE,
        Family::GoogleCloudCodeAssist,
        "gemini-3-flash",
    ))?;
    let events = run_turn(
        &provider,
        &native_request("gemini-3-flash", vec![user_text("Say hello.")]),
    )
    .await?;
    assert!(matches!(events.last(), Some(StreamEvent::Stop { .. })));
    let captured = server_task.join_next().await.expect("server completes")??;
    let actual_request_body = body(&captured[0]);
    assert!(
        actual_request_body.contains(r#""project":"stored-project-1""#),
        "{actual_request_body}"
    );
    assert!(
        !actual_request_body.contains("settings-project"),
        "{actual_request_body}"
    );
    Ok(())
}

#[tokio::test]
async fn a_native_row_without_a_project_or_with_only_a_key_is_not_ready_and_sends_nothing()
-> TestResult {
    // `auth.json` holds a project-less OAuth member only under a row whose
    // sign-in binds none, so the fixture reuses the WorkOS row's id: the
    // entry is the native family, the credential is a project-less sign-in.
    let dir = TestDir::new()?;
    stored_oauth(&dir, "factory-droid", TOKEN)?;
    let row = entry(
        "factory-droid",
        Family::GoogleCloudCodeAssist,
        String::from("http://127.0.0.1:1"),
        None,
        1,
        Transport::Https,
    );
    let set = set_for(&dir, vec![row])?;
    assert!(
        !set.credential_source_available("factory-droid", &CancellationToken::new())
            .await?,
        "an OAuth sign-in without a project is not ready"
    );
    let text = open_error_text(
        &set,
        "factory-droid",
        Family::GoogleCloudCodeAssist,
        "gemini-3-flash",
        vec![user_text("Say hello.")],
    )
    .await;
    assert!(
        text.contains("has no Google Cloud project for this sign-in")
            && text.contains("dalgon login factory-droid again"),
        "{text}"
    );
    assert!(!text.contains(TOKEN), "{text}");

    // An API key from the captured environment is not a native credential.
    let keyed_dir = TestDir::new()?;
    let mut keyed = native_entry(String::from("http://127.0.0.1:1"));
    keyed.key_env = Some("CCA_KEY".into());
    let keyed_set = ProviderSet::new(
        &config(vec![keyed], 0),
        identity(),
        EnvSnapshot::test(&[("CCA_KEY", "env-key-secret")]),
        keyed_dir.path(),
        keyed_dir.path(),
    )?;
    assert!(
        !keyed_set
            .credential_source_available(NATIVE, &CancellationToken::new())
            .await?,
        "a key alone is not a usable source for an OAuth-only family"
    );
    let text = open_error_text(
        &keyed_set,
        NATIVE,
        Family::GoogleCloudCodeAssist,
        "gemini-3-flash",
        vec![user_text("Say hello.")],
    )
    .await;
    assert!(
        text.contains("needs an OAuth sign-in, not an API key"),
        "{text}"
    );
    assert!(!text.contains("env-key-secret"), "{text}");

    // No credential at all never binds a provider.
    let empty_dir = TestDir::new()?;
    let empty = set_for(
        &empty_dir,
        vec![native_entry(String::from("http://127.0.0.1:1"))],
    )?;
    assert!(matches!(
        empty.provider(resolved(
            NATIVE,
            Family::GoogleCloudCodeAssist,
            "gemini-3-flash"
        )),
        Err(ProviderError::NoCredentials { .. })
    ));
    Ok(())
}

async fn native_error_text(status: u16, body: &str) -> String {
    let dir = TestDir::new().expect("temp dir");
    let (base, _server) = server("", vec![json_reply(status, body)])
        .await
        .expect("loopback");
    stored_cloud_code(&dir, NATIVE, TOKEN, "stored-project-1").expect("sign-in");
    let set = set_for(&dir, vec![native_entry(base)]).expect("provider set");
    open_error_text(
        &set,
        NATIVE,
        Family::GoogleCloudCodeAssist,
        "gemini-3-flash",
        vec![user_text("Say hello.")],
    )
    .await
}

#[tokio::test]
async fn native_errors_render_verification_keep_quota_typed_and_redact_the_sign_in() {
    let url = "https://accounts.example.com/verify/abc";
    let validation = format!(
        "{{\"error\":{{\"code\":400,\"status\":\"FAILED_PRECONDITION\",\"message\":\"verify {TOKEN} now\",\"details\":[{{\"reason\":\"VALIDATION_REQUIRED\",\"metadata\":{{\"validation_url\":\"{url}\"}}}}]}}}}"
    );
    let text = native_error_text(400, &validation).await;
    assert!(text.contains("Account verification required."), "{text}");
    assert!(
        text.contains(url) && text.contains("retry your request"),
        "{text}"
    );
    assert!(!text.contains(TOKEN) && !text.contains('@'), "{text}");

    // A plain error carrying the access token never echoes it.
    let plain = format!(
        "{{\"error\":{{\"code\":400,\"status\":\"FAILED_PRECONDITION\",\"message\":\"rejected {TOKEN} and stored-refresh-secret\"}}}}"
    );
    let text = native_error_text(400, &plain).await;
    assert!(!text.contains(TOKEN), "{text}");
    assert!(!text.contains("stored-refresh-secret"), "{text}");

    // Quota keeps precedence and its type over a verification URL.
    let quota = format!(
        "{{\"error\":{{\"code\":429,\"status\":\"RESOURCE_EXHAUSTED\",\"message\":\"quota will reset\",\"details\":[{{\"@type\":\"type.googleapis.com/google.rpc.ErrorInfo\",\"reason\":\"QUOTA_EXHAUSTED\"}},{{\"reason\":\"VALIDATION_REQUIRED\",\"metadata\":{{\"validation_url\":\"{url}\"}}}}]}}}}"
    );
    let text = native_error_text(429, &quota).await;
    assert!(
        !text.contains("Account verification required."),
        "quota must stay quota: {text}"
    );
}

#[tokio::test]
async fn cancelling_a_native_turn_closes_the_response_and_releases_its_permit() -> TestResult {
    let dir = TestDir::new()?;
    let (base, headers_received, mut server_task) = stalled_server().await?;
    stored_cloud_code(&dir, NATIVE, TOKEN, "stored-project-1")?;
    let set = set_for(&dir, vec![native_entry(base)])?;
    let provider = set.provider(resolved(
        NATIVE,
        Family::GoogleCloudCodeAssist,
        "gemini-3-flash",
    ))?;
    let cancel = CancellationToken::new();
    let stream = provider
        .open(
            SessionId::new_v7(),
            &native_request("gemini-3-flash", vec![user_text("wait")]),
            &[],
            notice_sink(),
            &cancel,
        )
        .await?;
    let mut consumer = tokio::task::JoinSet::new();
    consumer.spawn(async move {
        let mut stream = stream;
        stream.next().await
    });
    headers_received.await?;
    cancel.cancel();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), server_task.join_next())
            .await?
            .expect("server completes")??,
        "the client dropped the connection"
    );
    consumer.abort_all();
    while consumer.join_next().await.is_some() {}
    assert_eq!(
        set.inner
            .providers
            .get(NATIVE)
            .map(|slot| slot.permits.available_permits()),
        Some(1)
    );
    Ok(())
}

/// An assistant turn whose thinking carries a stored call signature, bound to
/// `family`.
fn signed_turn(family: Family) -> ContextItem {
    ContextItem::Assistant {
        source: ReplaySource {
            family,
            model: "gemini-2.5-pro".into(),
        },
        replay: None,
        parts: vec![
            AssistantPart::Thinking {
                text: "prior".into(),
                replay: Some(
                    RawJson::parse(r#"{"calls":{"call_1":"QUJD"}}"#).expect("a signature carrier"),
                ),
            },
            AssistantPart::ToolCall {
                call: CallId::new("call_1"),
                name: "read".into(),
                args: RawJson::parse("{}").expect("arguments"),
                replay: None,
            },
        ],
    }
}

#[tokio::test]
async fn a_stored_signature_replays_only_into_the_native_family() -> TestResult {
    for (source, replays) in [
        (Family::GoogleCloudCodeAssist, true),
        (Family::GoogleGeminiCli, false),
        (Family::GoogleAntigravity, false),
        (Family::GoogleVertex, false),
    ] {
        let dir = TestDir::new()?;
        let (base, mut server_task) = server("", vec![stream_reply(google_cca_reply())]).await?;
        stored_cloud_code(&dir, NATIVE, TOKEN, "stored-project-1")?;
        let set = set_for(&dir, vec![native_entry(base)])?;
        let provider = set.provider(resolved(
            NATIVE,
            Family::GoogleCloudCodeAssist,
            "gemini-2.5-pro",
        ))?;
        let turn = native_request(
            "gemini-2.5-pro",
            vec![user_text("Hi"), signed_turn(source), user_text("Go on.")],
        );
        run_turn(&provider, &turn).await?;
        let captured = server_task.join_next().await.expect("server completes")??;
        assert_eq!(
            body(&captured[0]).contains(r#""thoughtSignature":"QUJD""#),
            replays,
            "{source:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn the_native_and_local_gemini_families_never_serve_each_others_routes() -> TestResult {
    let dir = TestDir::new()?;
    let fixture = format!(
        "{}/src/family/google/gemini_cli/fixtures/fake-gemini.sh",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut local = entry(
        "gemini-test",
        Family::GoogleGeminiCli,
        String::from("https://example.com"),
        None,
        2,
        Transport::Https,
    );
    local.auth = AuthStyle::None;
    local.settings.binary = Some(fixture.into());
    stored_cloud_code(&dir, NATIVE, TOKEN, "stored-project-1")?;
    let set = ProviderSet::new(
        &config(
            vec![local, native_entry(String::from("http://127.0.0.1:1"))],
            0,
        ),
        identity(),
        EnvSnapshot::test(&[]).with_cwd(dir.path().to_path_buf()),
        dir.path(),
        dir.path(),
    )?;
    // The old entry keeps its deny-all policy; the native row adds none.
    let policy = set
        .inner
        .gemini_policy
        .clone()
        .expect("the subprocess entry publishes its policy");
    assert_eq!(
        std::fs::read_to_string(policy)?,
        super::super::set::GEMINI_CLI_DENY_ALL_POLICY
    );
    let old = set.provider(resolved(
        "gemini-test",
        Family::GoogleGeminiCli,
        "gemini-3-pro",
    ))?;
    let new = set.provider(resolved(
        NATIVE,
        Family::GoogleCloudCodeAssist,
        "gemini-3-pro",
    ))?;
    assert!(matches!(old, Provider::GeminiCli(_)));
    assert!(matches!(new, Provider::Http(_)));
    // A route of one family never reaches the other's provider.
    for (provider, family) in [
        (&new, Family::GoogleGeminiCli),
        (&old, Family::GoogleCloudCodeAssist),
    ] {
        let refused = open_failure(provider, family).await;
        assert!(
            refused.as_deref().is_some_and(
                |text| text.contains("request model does not match the bound provider route")
            ),
            "{family:?}: {refused:?}"
        );
    }
    Ok(())
}

/// The text of the refusal one open of a `family` route earns, if any.
async fn open_failure(provider: &Provider, family: Family) -> Option<String> {
    let turn = request(family, "gemini-3-pro", vec![user_text("Say hello.")]);
    let cancel = CancellationToken::new();
    provider
        .open(SessionId::new_v7(), &turn, &[], notice_sink(), &cancel)
        .await
        .err()
        .map(|error| error.to_string())
}

#[tokio::test]
async fn native_antigravity_uses_its_stored_project_and_an_explicit_setting_still_wins()
-> TestResult {
    for (setting, expected, absent) in [
        (None, "stored-ag-project", "explicit-ag-project"),
        (
            Some("explicit-ag-project"),
            "explicit-ag-project",
            "stored-ag-project",
        ),
    ] {
        let dir = TestDir::new()?;
        let (base, mut server_task) = server("", vec![stream_reply(google_cca_reply())]).await?;
        stored_cloud_code(&dir, "google-antigravity", "ya29-ag", "stored-ag-project")?;
        let mut row = entry(
            "google-antigravity",
            Family::GoogleAntigravity,
            base,
            None,
            1,
            Transport::Https,
        );
        row.settings.project = setting.map(Into::into);
        let set = set_for(&dir, vec![row])?;
        assert!(
            set.credential_source_available("google-antigravity", &CancellationToken::new())
                .await?
        );
        let provider = set.provider(resolved(
            "google-antigravity",
            Family::GoogleAntigravity,
            "gemini-3-flash-agent",
        ))?;
        let turn = request(
            Family::GoogleAntigravity,
            "gemini-3-flash-agent",
            vec![user_text("Say hello.")],
        );
        run_turn(&provider, &turn).await?;
        let captured = server_task.join_next().await.expect("server completes")??;
        let actual_request_body = body(&captured[0]);
        assert!(
            actual_request_body.contains(&format!(r#""project":"{expected}""#)),
            "{actual_request_body}"
        );
        assert!(
            !actual_request_body.contains(absent),
            "{actual_request_body}"
        );
        assert!(head(&captured[0]).contains("user-agent: antigravity/hub/"));
    }
    Ok(())
}
