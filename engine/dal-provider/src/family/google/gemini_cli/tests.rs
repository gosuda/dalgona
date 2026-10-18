//! Witnesses for the Gemini CLI subprocess surface: prompt rendering,
//! stream-json decoding, refusal rules, and canned turns plus cancellation
//! through real subprocess fixtures (unix only). The fixture scripts speak the
//! pinned stream-json shapes; the decoder under test is the real one. Temporary
//! fixtures additionally witness stdin delivery, snapshot-only environment,
//! stderr secret redaction, and process-group termination of descendants.

use dal_core::{
    AssistantPart, CallId, ContextItem, ModelRoute, ModelToolSpec, Part, Purpose, RawJson,
    RequestParams, ThinkingLevel,
};
use futures::StreamExt;

use super::*;
use crate::auth::credential::EnvSnapshot;
#[cfg(unix)]
use std::path::{Path, PathBuf};

fn raw(text: &str) -> RawJson {
    RawJson::parse(text).unwrap()
}

fn request(context: Vec<ContextItem>) -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::GoogleGeminiCli,
            model: "gemini-3-pro".into(),
        },
        system: "Be brief.".into(),
        tools: Vec::new().into(),
        context: context.into(),
        params: RequestParams {
            thinking: ThinkingLevel::Off,
            effort: None,
            temperature: None,
            max_output_tokens: None,
        },
        cache_key: None,
    }
}

fn text(value: &str) -> Part {
    Part::Text { text: value.into() }
}

fn cli_env(vars: &[(&str, &str)]) -> EnvSnapshot {
    EnvSnapshot::test(vars).with_cwd(std::env::temp_dir())
}

#[test]
fn render_prompt_flattens_the_turn() {
    let request = request(vec![
        ContextItem::User {
            parts: vec![text("What time?")],
        },
        ContextItem::Assistant {
            source: dal_core::ReplaySource {
                family: Family::GoogleGeminiCli,
                model: "gemini-3-pro".into(),
            },
            replay: None,
            parts: vec![
                AssistantPart::Text {
                    text: "It is noon.".into(),
                    replay: None,
                },
                AssistantPart::ToolCall {
                    call: CallId::new("call_1"),
                    name: "clock".into(),
                    args: raw(r#"{"zone":"utc"}"#),
                    replay: None,
                },
            ],
        },
        ContextItem::ToolResult {
            call: CallId::new("call_1"),
            name: "clock".into(),
            is_error: false,
            parts: vec![text("12:00")],
        },
    ]);
    let prompt = render_prompt(&request).unwrap();
    assert_eq!(
        String::from_utf8(prompt).unwrap(),
        "System instructions:\nBe brief.\n\nConversation:\nuser:\nWhat time?\nassistant:\nIt is noon.\ntool call call_1 (clock): {\"zone\":\"utc\"}\ntool result for call_1 (clock):\n12:00\n"
    );
}

#[test]
fn render_prompt_rejects_blobs() {
    let request = request(vec![ContextItem::User {
        parts: vec![Part::Blob {
            blob_id: dal_core::BlobId::from_bytes(b"png-bytes"),
            mime: "image/png".into(),
            bytes: 4,
        }],
    }]);
    let error = render_prompt(&request).unwrap_err();
    assert!(matches!(error, ProviderError::UnresolvedBlob { .. }));
}

#[test]
fn spawn_refuses_dal_tools_before_touching_the_binary() {
    let mut request = request(vec![]);
    request.tools = vec![ModelToolSpec {
        name: "read".into(),
        description: "Read.".into(),
        parameters: raw(r#"{"type":"object"}"#),
        grammar: None,
        defer_loading: false,
    }]
    .into();
    let env = cli_env(&[]);
    // `spawn` returns `CliStream`, which carries no `Debug` impl, so the
    // refusal is pulled out with `err` instead of `unwrap_err`.
    let error = spawn(&CliRequest {
        request: &request,
        binary: Some("/nonexistent-binary"),
        policy_file: "/dev/null",
        env: &env,
    })
    .err()
    .expect("spawn must refuse dal tools");
    assert!(error.to_string().contains("without dal tools"), "{error}");
}

#[test]
fn spawn_requires_a_captured_working_directory() {
    let request = request(vec![ContextItem::User {
        parts: vec![text("Hi")],
    }]);
    let env = EnvSnapshot::test(&[]);
    let error = spawn(&CliRequest {
        request: &request,
        binary: Some("/nonexistent-binary"),
        policy_file: "/dev/null",
        env: &env,
    })
    .err()
    .expect("spawn must refuse a missing captured cwd");
    assert!(
        matches!(error, ProviderError::InvalidRequest { .. }),
        "{error}"
    );
    assert!(error.to_string().contains("working directory"), "{error}");
}

// ---------------------------------------------------------------------------
// Stream-json decoding
// ---------------------------------------------------------------------------

fn feed(lines: &[&str]) -> (Vec<StreamEvent>, Result<(), ProviderError>) {
    let mut decoder = CliDecoder::default();
    let mut out = Vec::new();
    for line in lines {
        if let Err(error) = decoder.feed_line(line, &mut out) {
            return (out, Err(error));
        }
    }
    let end = decoder.end();
    (out, end)
}

#[test]
fn assistant_messages_become_text_deltas() {
    let (events, end) = feed(&[
        r#"{"type":"init","timestamp":"2026-10-16T00:00:00Z","session_id":"s1","model":"gemini-3-pro"}"#,
        r#"{"type":"message","timestamp":"2026-10-16T00:00:00Z","role":"assistant","content":"Hello","delta":true}"#,
        r#"{"type":"message","timestamp":"2026-10-16T00:00:00Z","role":"user","content":"echo"}"#,
        r#"{"type":"result","timestamp":"2026-10-16T00:00:00Z","status":"success","stats":{"input_tokens":10,"output_tokens":7,"cached":3}}"#,
    ]);
    assert!(end.is_ok());
    assert_eq!(
        events,
        vec![
            StreamEvent::TextDelta {
                text: String::from("Hello"),
            },
            StreamEvent::ToolCallsDone { calls: Vec::new() },
            StreamEvent::Usage {
                usage: Usage {
                    input_tokens: 10,
                    cached_input_tokens: 3,
                    output_tokens: 7,
                    reasoning_tokens: None,
                    cache_write_tokens: 0,
                    cost_usd: None,
                    premium_requests: None,
                },
            },
            StreamEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ]
    );
}

#[test]
fn cli_tool_use_stays_visible_but_never_becomes_a_call() {
    let (events, _) = feed(&[
        r#"{"type":"tool_use","timestamp":"t","tool_name":"read_file","tool_id":"1","parameters":{}}"#,
        r#"{"type":"tool_result","timestamp":"t","tool_id":"1","status":"success","output":"x"}"#,
        r#"{"type":"result","timestamp":"t","status":"success"}"#,
    ]);
    assert_eq!(events.len(), 4);
    assert!(matches!(
        &events[0],
        StreamEvent::ReasoningDelta { text } if text.contains("read_file")
    ));
    assert!(matches!(
        &events[1],
        StreamEvent::ToolCallsDone { calls } if calls.is_empty()
    ));
}

#[test]
fn warnings_pass_and_errors_end_the_stream() {
    let (events, end) = feed(&[
        r#"{"type":"error","timestamp":"t","severity":"warning","message":"slow"}"#,
        r#"{"type":"result","timestamp":"t","status":"success"}"#,
    ]);
    assert!(end.is_ok());
    assert_eq!(events.len(), 3);

    let (_, end) =
        feed(&[r#"{"type":"error","timestamp":"t","severity":"error","message":"no auth"}"#]);
    let error = end.unwrap_err();
    assert!(error.to_string().contains("no auth"), "{error}");

    let (_, end) = feed(&[
        r#"{"type":"result","timestamp":"t","status":"error","error":{"type":"Fatal","message":"boom"}}"#,
    ]);
    let error = end.unwrap_err();
    assert!(error.to_string().contains("boom"), "{error}");
}

#[test]
fn authentication_failures_name_the_cli_owned_sign_in() {
    // The CLI reports its own sign-in failure as a typed result error; dal
    // owns no credential here, so the message points at the CLI's store and
    // the environment the child is given, never at `dalgon login`.
    let (_, end) = feed(&[
        r#"{"type":"result","timestamp":"t","status":"error","error":{"type":"FatalAuthenticationError","message":"Please set an Auth method"}}"#,
    ]);
    let error = end.unwrap_err();
    assert!(
        matches!(error, ProviderError::InvalidRequest { .. }),
        "{error:?}"
    );
    let text = error.to_string();
    assert!(text.contains("not authenticated"), "{text}");
    assert!(text.contains("Please set an Auth method"), "{text}");
    assert!(text.contains("GEMINI_API_KEY"), "{text}");
    assert_eq!(error.fix(), None, "{error:?}");

    let (_, end) = feed(&[
        r#"{"type":"result","timestamp":"t","status":"error","error":{"type":"UnauthorizedError","message":"401"}}"#,
    ]);
    let text = end.unwrap_err().to_string();
    assert!(text.contains("not authenticated"), "{text}");

    // Other typed errors keep the plain error rendering.
    let (_, end) = feed(&[
        r#"{"type":"result","timestamp":"t","status":"error","error":{"type":"FatalConfigError","message":"bad settings"}}"#,
    ]);
    let text = end.unwrap_err().to_string();
    assert!(text.contains("gemini-cli error: bad settings"), "{text}");
    assert!(!text.contains("not authenticated"), "{text}");
}

#[cfg(unix)]
#[tokio::test]
async fn exit_codes_without_a_result_keep_their_documented_meaning() {
    // The CLI exits 41 on a sign-in failure and 42 / 52 / 55 on input,
    // configuration, and untrusted-workspace failures when it could not emit
    // a result event first.
    for (code, needle) in [
        (41, "not authenticated"),
        (42, "rejected its input"),
        (52, "configuration error"),
        (55, "workspace is not trusted"),
    ] {
        let fixture = TempFixture::new("fake-exit.sh", &format!("#!/bin/sh\nexit {code}\n")).await;
        let env = EnvSnapshot::test(&[]).with_cwd(fixture.dir().to_path_buf());
        let request = request(vec![ContextItem::User {
            parts: vec![text("Hi")],
        }]);
        let stream = spawn(&CliRequest {
            request: &request,
            binary: Some(fixture.program()),
            policy_file: "/dev/null",
            env: &env,
        })
        .expect("exit fixture spawns");
        let events: Vec<_> = stream.into_events().collect().await;
        let error = events
            .into_iter()
            .find_map(Result::err)
            .expect("an exit without a result ends in one error");
        assert!(
            matches!(error, ProviderError::InvalidRequest { .. }),
            "exit {code}: {error:?}"
        );
        let text = error.to_string();
        assert!(text.contains(needle), "exit {code}: {text}");
        assert!(text.contains(&code.to_string()), "exit {code}: {text}");
    }
}

#[test]
fn unknown_events_are_ignored_and_bad_shapes_fail() {
    let (events, end) = feed(&[
        "",
        r#"{"type":"future","timestamp":"t","n":1}"#,
        r#"{"type":"result","timestamp":"t","status":"success"}"#,
    ]);
    assert!(end.is_ok());
    assert_eq!(events.len(), 3);

    let (_, end) = feed(&[r#"{"type":"message","timestamp":"t"}"#]);
    let error = end.unwrap_err();
    assert!(
        error.to_string().contains("message event is not valid"),
        "{error}"
    );

    let (_, end) = feed(&[
        r#"{"type":"result","timestamp":"t","status":"success"}"#,
        r#"{"type":"result","timestamp":"t","status":"success"}"#,
    ]);
    let error = end.unwrap_err();
    assert!(error.to_string().contains("second result"), "{error}");
}

#[test]
fn transcript_without_result_is_cut() {
    let (_, end) =
        feed(&[r#"{"type":"message","timestamp":"t","role":"assistant","content":"partial"}"#]);
    assert!(matches!(end.unwrap_err(), ProviderError::StreamCut));
}

// ---------------------------------------------------------------------------
// Fixture-binary witnesses (unix only)
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn fixture(name: &str) -> String {
    format!(
        "{}/src/family/google/gemini_cli/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    )
}

#[cfg(unix)]
#[test]
fn process_gone_is_a_missing_group_never_a_denied_signal() {
    use crate::family::process::is_process_gone;

    // `killpg` on a group whose last member was reaped reports ESRCH (3), which std leaves
    // uncategorized; EPERM (1) is a real failure and must stay visible.
    assert!(is_process_gone(&io::Error::from_raw_os_error(3)));
    assert!(is_process_gone(&io::Error::from(io::ErrorKind::NotFound)));
    assert!(!is_process_gone(&io::Error::from_raw_os_error(1)));
    assert!(!is_process_gone(&io::Error::from(
        io::ErrorKind::PermissionDenied
    )));
}

/// Writes `script` to `path` and marks it owner-executable without this process ever holding the
/// file open for writing.
///
/// `std::fs::write` leaves a close-on-exec write descriptor open in this process until it is
/// dropped, and close-on-exec closes only at `exec`. A sibling test thread that forks inside that
/// window carries the descriptor until its own `exec`, so the finished script then fails to
/// spawn with `ETXTBSY` ("Text file busy"). A `/bin/sh` that alone opens, writes, and chmods the
/// file, and is joined before this returns, keeps every write descriptor out of the test
/// process: no sibling fork can inherit one, and none survives the join.
#[cfg(unix)]
#[expect(
    clippy::disallowed_methods,
    reason = "test helper runs /bin/sh to write script cleanly without descriptor leak"
)]
async fn write_executable(path: &Path, script: &str) {
    let status = tokio::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("printf '%s' \"$1\" > \"$2\" && chmod 700 \"$2\"")
        .arg("fixture-writer")
        .arg(script)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .status()
        .await
        .expect("fixture writer spawns");
    assert!(status.success(), "fixture writer failed: {status}");
}

/// A temporary executable shell fixture, removed on drop.
#[cfg(unix)]
struct TempFixture {
    dir: PathBuf,
    program: String,
}

#[cfg(unix)]
impl TempFixture {
    async fn new(stem: &str, script: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};

        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let sequence = SEQUENCE.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "dal-gemini-cli-{stem}-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("fixture directory creates");
        let path = dir.join(stem);
        write_executable(&path, script).await;
        let program = path.to_str().expect("fixture path is UTF-8").to_owned();
        Self { dir, program }
    }

    fn program(&self) -> &str {
        &self.program
    }

    fn dir(&self) -> &Path {
        &self.dir
    }
}

#[cfg(unix)]
impl Drop for TempFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn canned_turn_completes_through_the_real_driver() {
    let request = request(vec![ContextItem::User {
        parts: vec![text("Hi")],
    }]);
    let env = cli_env(&[]);
    let stream = spawn(&CliRequest {
        request: &request,
        binary: Some(&fixture("fake-gemini.sh")),
        policy_file: "/dev/null",
        env: &env,
    })
    .unwrap();
    let events: Vec<_> = stream.into_events().collect().await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    let events: Vec<_> = events.into_iter().map(Result::unwrap).collect();
    assert_eq!(
        events,
        vec![
            StreamEvent::TextDelta {
                text: String::from("Hello"),
            },
            StreamEvent::TextDelta {
                text: String::from(" world"),
            },
            StreamEvent::ToolCallsDone { calls: Vec::new() },
            StreamEvent::Usage {
                usage: Usage {
                    input_tokens: 10,
                    cached_input_tokens: 0,
                    output_tokens: 7,
                    reasoning_tokens: None,
                    cache_write_tokens: 0,
                    cost_usd: None,
                    premium_requests: None,
                },
            },
            StreamEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ]
    );
}

#[cfg(unix)]
#[tokio::test]
async fn prompt_bytes_reach_the_child_stdin() {
    let fixture = TempFixture::new(
        "fake-input.sh",
        "#!/bin/sh\nif grep -q \"needle-token-7f3a\" /dev/stdin; then\n  printf '%s\\n' '{\"type\":\"result\",\"timestamp\":\"t\",\"status\":\"success\"}'\nelse\n  printf 'stdin did not carry the prompt\\n' >&2\n  exit 3\nfi\n",
    )
    .await;
    let env = EnvSnapshot::test(&[]).with_cwd(fixture.dir().to_path_buf());
    let request = request(vec![ContextItem::User {
        parts: vec![text("needle-token-7f3a")],
    }]);
    let stream = spawn(&CliRequest {
        request: &request,
        binary: Some(fixture.program()),
        policy_file: "/dev/null",
        env: &env,
    })
    .expect("input fixture spawns");
    let events: Vec<_> = stream.into_events().collect().await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert!(
        matches!(events.last(), Some(Ok(StreamEvent::Stop { .. }))),
        "{events:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "test reads ambient PATH as the no-inheritance witness"
)]
async fn child_sees_the_snapshot_not_the_ambient_environment() {
    // A POSIX shell initializes its own unexported PATH even after `env_clear`, so shell
    // variables prove nothing about the exec. The exported environment of a real child
    // (`/usr/bin/env`, absolute) is what the exec carried. These exports are shell-created;
    // every other name must come from the snapshot.
    const SHELL_CREATED: [&str; 4] = ["PWD", "OLDPWD", "SHLVL", "_"];
    assert!(
        std::env::var_os("PATH").is_some(),
        "PATH anchors the no-inheritance witness"
    );
    let fixture = TempFixture::new(
        "fake-env.sh",
        "#!/bin/sh\ncat > /dev/null\n{ pwd -P; /usr/bin/env; } > \"$GEMINI_CLI_LIFECYCLE_OUT\"\nprintf '%s\\n' '{\"type\":\"result\",\"timestamp\":\"t\",\"status\":\"success\"}'\n",
    )
    .await;
    let out = fixture.dir().join("child-env");
    let out_var = out.to_str().expect("output path is UTF-8").to_owned();
    let env = EnvSnapshot::test(&[
        ("GEMINI_CLI_LIFECYCLE_MARKER", "marker-present"),
        ("GEMINI_CLI_LIFECYCLE_OUT", out_var.as_str()),
    ])
    .with_cwd(fixture.dir().to_path_buf());
    let request = request(vec![ContextItem::User {
        parts: vec![text("Hi")],
    }]);
    let stream = spawn(&CliRequest {
        request: &request,
        binary: Some(fixture.program()),
        policy_file: "/dev/null",
        env: &env,
    })
    .expect("env fixture spawns");
    let events: Vec<_> = stream.into_events().collect().await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    let dump = std::fs::read_to_string(&out).expect("the child wrote its exec environment");
    let mut lines = dump.lines();
    let cwd = lines
        .next()
        .expect("the child reported its working directory");
    let child: std::collections::BTreeMap<&str, &str> =
        lines.filter_map(|line| line.split_once('=')).collect();
    // Names only in diagnostics: the ambient environment may carry secrets.
    let names: Vec<&str> = child.keys().copied().collect();
    assert_eq!(
        child.get("GEMINI_CLI_LIFECYCLE_MARKER"),
        Some(&"marker-present"),
        "snapshot variable missing from the exec environment; names: {names:?}"
    );
    assert!(
        !child.contains_key("PATH"),
        "ambient PATH reached the exec environment; names: {names:?}"
    );
    let leaked: Vec<&str> = names
        .iter()
        .copied()
        .filter(|name| {
            !SHELL_CREATED.iter().any(|created| created == name)
                && env.iter().all(|(snapshot, _)| snapshot != *name)
        })
        .collect();
    assert!(
        leaked.is_empty(),
        "names outside the snapshot reached the exec environment: {leaked:?}"
    );
    assert_eq!(
        Path::new(cwd)
            .canonicalize()
            .expect("the child working directory exists"),
        fixture
            .dir()
            .canonicalize()
            .expect("the fixture directory exists"),
        "the child did not run in the captured working directory"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn stderr_secrets_never_reach_the_error() {
    const SECRET: &str = "lifecycle-secret-9d2c";
    let fixture = TempFixture::new(
        "fake-secret.sh",
        "#!/bin/sh\nprintf '%s\\n' \"token=${GEMINI_CLI_LIFECYCLE_SECRET:-missing}\" 'hint: run gemini login' >&2\nexit 3\n",
    )
    .await;
    let env = EnvSnapshot::test(&[("GEMINI_CLI_LIFECYCLE_SECRET", SECRET)])
        .with_cwd(fixture.dir().to_path_buf());
    let request = request(vec![ContextItem::User {
        parts: vec![text("Hi")],
    }]);
    let stream = spawn(&CliRequest {
        request: &request,
        binary: Some(fixture.program()),
        policy_file: "/dev/null",
        env: &env,
    })
    .expect("secret fixture spawns");
    let events: Vec<_> = stream.into_events().collect().await;
    assert_eq!(events.len(), 1, "{events:?}");
    let error = events
        .into_iter()
        .find_map(Result::err)
        .expect("stderr failure ends in one error");
    assert!(matches!(error, ProviderError::Transport { .. }), "{error}");
    let text = error.to_string();
    assert!(text.contains("no result"), "{text}");
    assert!(text.contains("stderr bytes withheld"), "{text}");
    assert!(!text.contains("cleanup"), "{text}");
    assert!(!text.contains("hint: run gemini login"), "{text}");
    assert!(!text.contains(SECRET), "{text}");
}

#[cfg(unix)]
#[tokio::test]
async fn leader_exit_terminates_descendants_holding_the_pipes() {
    // The background sleep inherits stdout, so EOF can only arrive after the
    // group kill. Collecting the full turn proves the sweep ran.
    let fixture = TempFixture::new(
        "fake-descendant.sh",
        "#!/bin/sh\nsleep 30 &\nprintf '%s\\n' '{\"type\":\"init\",\"timestamp\":\"t\",\"session_id\":\"s1\"}' '{\"type\":\"message\",\"timestamp\":\"t\",\"role\":\"assistant\",\"content\":\"done\"}' '{\"type\":\"result\",\"timestamp\":\"t\",\"status\":\"success\"}'\n",
    )
    .await;
    let env = EnvSnapshot::test(&[]).with_cwd(fixture.dir().to_path_buf());
    let request = request(vec![ContextItem::User {
        parts: vec![text("Hi")],
    }]);
    let stream = spawn(&CliRequest {
        request: &request,
        binary: Some(fixture.program()),
        policy_file: "/dev/null",
        env: &env,
    })
    .expect("descendant fixture spawns");
    let events: Vec<_> = stream.into_events().collect().await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert!(
        matches!(events.last(), Some(Ok(StreamEvent::Stop { .. }))),
        "{events:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn silent_failure_names_the_status_without_a_tail() {
    let fixture = TempFixture::new("fake-silent.sh", "#!/bin/sh\nexit 3\n").await;
    let env = EnvSnapshot::test(&[]).with_cwd(fixture.dir().to_path_buf());
    let request = request(vec![ContextItem::User {
        parts: vec![text("Hi")],
    }]);
    let stream = spawn(&CliRequest {
        request: &request,
        binary: Some(fixture.program()),
        policy_file: "/dev/null",
        env: &env,
    })
    .expect("silent fixture spawns");
    let events: Vec<_> = stream.into_events().collect().await;
    assert_eq!(events.len(), 1, "{events:?}");
    let error = events
        .into_iter()
        .find_map(Result::err)
        .expect("silent failure ends in one error");
    assert!(matches!(error, ProviderError::Transport { .. }), "{error}");
    let text = error.to_string();
    assert!(text.contains("no result"), "{text}");
    assert!(!text.contains("no result:"), "{text}");
    assert!(!text.contains("cleanup"), "{text}");
}

#[cfg(unix)]
#[tokio::test]
async fn dropped_stream_kills_the_child() {
    let request = request(vec![ContextItem::User {
        parts: vec![text("Hi")],
    }]);
    let env = cli_env(&[]);
    let start = std::time::Instant::now();
    {
        let stream = spawn(&CliRequest {
            request: &request,
            binary: Some(&fixture("fake-slow.sh")),
            policy_file: "/dev/null",
            env: &env,
        })
        .unwrap();
        // The fixture sleeps 30s: returning promptly proves the drop killed
        // the child instead of reaping its natural exit.
        drop(stream.into_events());
    }
    assert!(
        start.elapsed() < std::time::Duration::from_secs(10),
        "dropping the stream did not kill the child promptly"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn dropped_stream_terminates_the_whole_group() {
    let fixture = TempFixture::new(
        "fake-group.sh",
        "#!/bin/sh\nheartbeat=\"$GEMINI_CLI_LIFECYCLE_HEARTBEAT\"\n: > \"$heartbeat\"\ncount=0\n(\n  while true; do\n    count=$((count + 1))\n    printf '%s' \"$count\" > \"$heartbeat\"\n    sleep 1\n  done\n) &\nsleep 30\n",
    )
    .await;
    let heartbeat = fixture.dir().join("heartbeat");
    let heartbeat_var = heartbeat
        .to_str()
        .expect("heartbeat path is UTF-8")
        .to_owned();
    let env = EnvSnapshot::test(&[("GEMINI_CLI_LIFECYCLE_HEARTBEAT", heartbeat_var.as_str())])
        .with_cwd(fixture.dir().to_path_buf());
    let request = request(vec![ContextItem::User {
        parts: vec![text("Hi")],
    }]);
    let stream = spawn(&CliRequest {
        request: &request,
        binary: Some(fixture.program()),
        policy_file: "/dev/null",
        env: &env,
    })
    .expect("group fixture spawns");
    let events = stream.into_events();
    let started = std::time::Instant::now();
    loop {
        let beat = std::fs::read_to_string(&heartbeat).unwrap_or_default();
        if !beat.is_empty() {
            break;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "the descendant never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    drop(events);
    std::thread::sleep(std::time::Duration::from_millis(300));
    let before = std::fs::read_to_string(&heartbeat).unwrap_or_default();
    std::thread::sleep(std::time::Duration::from_millis(2500));
    let after = std::fs::read_to_string(&heartbeat).unwrap_or_default();
    assert!(!before.is_empty(), "the descendant never started");
    assert_eq!(before, after, "the descendant survived the drop");
}

#[test]
fn invalid_result_status_is_protocol() {
    // A `result` with an unknown status is a grammar violation, not a silent
    // ignore: the terminal must stay a protocol error.
    let (_, end) = feed(&[
        r#"{"type":"result","timestamp":"t","status":"bogus"}"#,
    ]);
    let error = end.unwrap_err();
    assert!(
        matches!(error, ProviderError::Protocol { .. }),
        "{error:?}"
    );
    assert!(error.to_string().contains("not valid"), "{error}");
}

#[test]
fn undecodable_non_json_lines_are_ignored() {
    // Lines that decode as neither an event nor a known kind are tolerated;
    // only a known kind in the wrong shape fails.
    let (events, end) = feed(&[
        "this is not json at all",
        "",
        r#"{"type":"future","timestamp":"t","n":1}"#,
        r#"{"type":"message","timestamp":"t","role":"assistant","content":"Hi"}"#,
        r#"{"type":"result","timestamp":"t","status":"success"}"#,
    ]);
    assert!(end.is_ok(), "{end:?}");
    assert_eq!(events.len(), 4, "{events:?}");
    assert!(
        matches!(events.last(), Some(StreamEvent::Stop { .. })),
        "{events:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn exact_limit_line_is_accepted() {
    // A single line of exactly LINE_LIMIT bytes must not fault: the cap is
    // `> LIMIT`, so the boundary payload is still a tolerable garbage line
    // and the later result completes the turn.
    let fixture = TempFixture::new(
        "fake-exact-limit.sh",
        "#!/bin/sh\ncat > /dev/null\nhead -c 1048576 /dev/zero | tr '\\0' 'x'\necho\nprintf '%s\\n' '{\"type\":\"result\",\"timestamp\":\"t\",\"status\":\"success\"}'\n",
    )
    .await;
    let env = EnvSnapshot::test(&[]).with_cwd(fixture.dir().to_path_buf());
    let request = request(vec![ContextItem::User {
        parts: vec![text("Hi")],
    }]);
    let stream = spawn(&CliRequest {
        request: &request,
        binary: Some(fixture.program()),
        policy_file: "/dev/null",
        env: &env,
    })
    .expect("exact-limit fixture spawns");
    let events: Vec<_> = stream.into_events().collect().await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert!(
        matches!(events.last(), Some(Ok(StreamEvent::Stop { .. }))),
        "{events:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn over_limit_line_ends_the_stream() {
    // One byte over the cap must end the stream even when the crossing chunk
    // already carries the newline: before the fix this line was delivered as
    // ignorable garbage and the turn completed Ok.
    let fixture = TempFixture::new(
        "fake-over-limit.sh",
        "#!/bin/sh\ncat > /dev/null\nhead -c 1048577 /dev/zero | tr '\\0' 'x'\necho\nprintf '%s\\n' '{\"type\":\"result\",\"timestamp\":\"t\",\"status\":\"success\"}'\n",
    )
    .await;
    let env = EnvSnapshot::test(&[]).with_cwd(fixture.dir().to_path_buf());
    let request = request(vec![ContextItem::User {
        parts: vec![text("Hi")],
    }]);
    let stream = spawn(&CliRequest {
        request: &request,
        binary: Some(fixture.program()),
        policy_file: "/dev/null",
        env: &env,
    })
    .expect("over-limit fixture spawns");
    let events: Vec<_> = stream.into_events().collect().await;
    assert_eq!(events.len(), 1, "{events:?}");
    let error = events
        .into_iter()
        .find_map(Result::err)
        .expect("an over-long line ends in one error");
    assert!(
        matches!(error, ProviderError::Protocol { .. }),
        "{error:?}"
    );
    assert!(error.to_string().contains("exceeds"), "{error}");
}

#[cfg(unix)]
#[tokio::test]
async fn nonzero_exit_preserves_the_stdin_writer_cause() {
    // The child consumes stdin so the real writer succeeds; the injected
    // fault stands in for an EPIPE observed before the leader exit. The
    // terminal must name both the exit status and the write cause.
    let fixture = TempFixture::new(
        "fake-exit-writer.sh",
        "#!/bin/sh\ncat > /dev/null\nexit 3\n",
    )
    .await;
    let env = EnvSnapshot::test(&[]).with_cwd(fixture.dir().to_path_buf());
    let request = request(vec![ContextItem::User {
        parts: vec![text("Hi")],
    }]);
    let mut stream = spawn(&CliRequest {
        request: &request,
        binary: Some(fixture.program()),
        policy_file: "/dev/null",
        env: &env,
    })
    .expect("exit-writer fixture spawns");
    let mut out = Vec::new();
    let handled = stream.state.handle(
        CliMsg::StdinFault(String::from("Broken pipe (os error 32)")),
        &mut out,
    );
    assert!(handled.is_ok(), "{handled:?}");
    assert!(out.is_empty(), "{out:?}");
    let events: Vec<_> = stream.into_events().collect().await;
    let error = events
        .into_iter()
        .find_map(Result::err)
        .expect("a failed exit with a writer fault ends in one error");
    assert!(
        matches!(error, ProviderError::Transport { .. }),
        "{error:?}"
    );
    let text = error.to_string();
    assert!(text.contains("3"), "{text}");
    assert!(
        text.contains("writing the gemini prompt failed"),
        "{text}"
    );
    assert!(text.contains("Broken pipe"), "{text}");
}

#[cfg(unix)]
#[tokio::test]
async fn auth_exit_preserves_the_stdin_writer_cause() {
    // Same as above with a sign-in exit: the InvalidRequest kind (and its
    // retry routing) must survive the chained write cause.
    let fixture = TempFixture::new(
        "fake-auth-writer.sh",
        "#!/bin/sh\ncat > /dev/null\nexit 41\n",
    )
    .await;
    let env = EnvSnapshot::test(&[]).with_cwd(fixture.dir().to_path_buf());
    let request = request(vec![ContextItem::User {
        parts: vec![text("Hi")],
    }]);
    let mut stream = spawn(&CliRequest {
        request: &request,
        binary: Some(fixture.program()),
        policy_file: "/dev/null",
        env: &env,
    })
    .expect("auth-writer fixture spawns");
    let mut out = Vec::new();
    let handled = stream.state.handle(
        CliMsg::StdinFault(String::from("Broken pipe (os error 32)")),
        &mut out,
    );
    assert!(handled.is_ok(), "{handled:?}");
    let events: Vec<_> = stream.into_events().collect().await;
    let error = events
        .into_iter()
        .find_map(Result::err)
        .expect("an auth exit with a writer fault ends in one error");
    assert!(
        matches!(error, ProviderError::InvalidRequest { .. }),
        "{error:?}"
    );
    let text = error.to_string();
    assert!(text.contains("not authenticated"), "{text}");
    assert!(text.contains("41"), "{text}");
    assert!(
        text.contains("writing the gemini prompt failed"),
        "{text}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn non_utf8_stdout_ends_the_stream() {
    // A short non-UTF8 line must end the stream as a protocol error even
    // though a later result follows; the decoder never sees the bytes.
    let fixture = TempFixture::new(
        "fake-bad-utf8.sh",
        "#!/bin/sh\ncat > /dev/null\nprintf '\\377\\376\\n'\nprintf '%s\\n' '{\"type\":\"result\",\"timestamp\":\"t\",\"status\":\"success\"}'\n",
    )
    .await;
    let env = EnvSnapshot::test(&[]).with_cwd(fixture.dir().to_path_buf());
    let request = request(vec![ContextItem::User {
        parts: vec![text("Hi")],
    }]);
    let stream = spawn(&CliRequest {
        request: &request,
        binary: Some(fixture.program()),
        policy_file: "/dev/null",
        env: &env,
    })
    .expect("bad-utf8 fixture spawns");
    let events: Vec<_> = stream.into_events().collect().await;
    assert_eq!(events.len(), 1, "{events:?}");
    let error = events
        .into_iter()
        .find_map(Result::err)
        .expect("non-UTF8 stdout ends in one error");
    assert!(
        matches!(error, ProviderError::Protocol { .. }),
        "{error:?}"
    );
    assert!(error.to_string().contains("UTF-8"), "{error}");
}

#[test]
fn writer_cause_preserves_the_process_failure_kind() {
    let auth = with_writer_cause(
        ProviderError::InvalidRequest {
            message: String::from("gemini is not authenticated (exit status 41)"),
        },
        "Broken pipe (os error 32)",
    );
    assert!(
        matches!(auth, ProviderError::InvalidRequest { .. }),
        "{auth:?}"
    );
    assert!(auth.to_string().contains("41"), "{auth}");
    assert!(
        auth.to_string().contains("writing the gemini prompt failed"),
        "{auth}"
    );

    let transport = with_writer_cause(
        ProviderError::Transport {
            family: Family::GoogleGeminiCli,
            reason: String::from("gemini exited with status 3 and no result"),
        },
        "Broken pipe (os error 32)",
    );
    assert!(
        matches!(transport, ProviderError::Transport { .. }),
        "{transport:?}"
    );
    assert!(transport.to_string().contains("status 3"), "{transport}");
}

#[test]
fn render_prompt_ignores_text_tool_and_message_replay() {
    let assistant = |replay: fn() -> Option<RawJson>| ContextItem::Assistant {
        source: dal_core::ReplaySource {
            family: Family::GoogleGeminiCli,
            model: "gemini-3-pro".into(),
        },
        replay: replay(),
        parts: vec![
            AssistantPart::Text {
                text: "It is noon.".into(),
                replay: replay(),
            },
            AssistantPart::ToolCall {
                call: CallId::new("call_1"),
                name: "clock".into(),
                args: raw(r#"{"zone":"utc"}"#),
                replay: replay(),
            },
        ],
    };
    let payload = || Some(raw(r#"{"foreign":"REPLAY-MARKER"}"#));
    let plain = render_prompt(&request(vec![assistant(|| None)])).unwrap();
    let laden = render_prompt(&request(vec![assistant(payload)])).unwrap();
    assert_eq!(laden, plain);
    let prompt = String::from_utf8(laden).unwrap();
    assert!(!prompt.contains("REPLAY-MARKER"), "{prompt}");
}
