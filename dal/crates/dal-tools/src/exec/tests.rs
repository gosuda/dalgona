use super::{
    ExecArgs, ExecEvent, ExecJob, ExecOutcome, ExecState, ExecTool, ExecTransition, ValidatedCall,
};
use dal_agent::{ProcResult, ProcStatus, ToolError, ext::ToolOutcome};
use dal_core::{CallId, JobOutcome};
use std::{
    path::Path,
    time::{Duration, Instant},
};

#[cfg(unix)]
use dal_agent::{Delivery, Env, Host, Product, SessionRef};
#[cfg(unix)]
use dal_core::{
    ClientId, Command, Config, ConfigProduct, Expect, PageReq, Part, UpdateKind, Workspace,
};
#[cfg(unix)]
use std::{collections::BTreeMap, ffi::OsString};

#[test]
fn decode_error_literals() {
    for (input, expected) in [
        (
            r#"{"command":"x","extra":true}"#,
            "exec: unknown argument \"extra\"",
        ),
        (r#"{"command":1}"#, "exec: command must be a string"),
        (r"{}", "exec: command must be a string"),
        ("[]", "exec: command must be a string"),
        (
            r#"{"command":"x","timeout_seconds":"1"}"#,
            "exec: timeout_seconds must be an integer",
        ),
        (
            r#"{"command":"x","timeout_seconds":1.5}"#,
            "exec: timeout_seconds must be an integer",
        ),
    ] {
        assert_eq!(super::decode(input).unwrap_err().to_string(), expected);
    }

    let root = tempfile::tempdir().expect("create a workspace");
    for command in ["", "   "] {
        let args = super::decode(&format!("{{\"command\":{command:?}}}"))
            .expect("the command field decodes");
        assert_eq!(
            super::validate(args, root.path()).unwrap_err().to_string(),
            "exec: command must not be empty"
        );
    }
    for seconds in [0, 86_401] {
        let args = super::decode(&format!(
            "{{\"command\":\"true\",\"timeout_seconds\":{seconds}}}"
        ))
        .expect("the timeout field decodes");
        assert_eq!(
            super::validate(args, root.path()).unwrap_err().to_string(),
            "exec: timeout_seconds must be between 1 and 86400"
        );
    }
}

#[test]
fn validate_rejects_empty_commands_and_invalid_timeouts() {
    let root = tempfile::tempdir().expect("create a workspace");
    for command in ["", " \t\n"] {
        let error = super::validate(
            ExecArgs {
                command: command.into(),
                cwd: None,
                timeout_seconds: None,
                foreground_s: None,
            },
            root.path(),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "exec: command must not be empty");
    }
    for seconds in [-1, 0, 86_401] {
        let error = super::validate(
            ExecArgs {
                command: "true".into(),
                cwd: None,
                timeout_seconds: Some(seconds),
                foreground_s: None,
            },
            root.path(),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "exec: timeout_seconds must be between 1 and 86400"
        );
    }
}

#[test]
fn foreground_s_overrides_the_window_inside_its_range() {
    let root = tempfile::tempdir().expect("create a workspace");
    let decoded = super::decode(r#"{"command":"true","foreground_s":"5"}"#).unwrap_err();
    assert_eq!(decoded.to_string(), "exec: foreground_s must be an integer");
    for seconds in [-1, 0, 86_401] {
        let args = super::decode(&format!(
            "{{\"command\":\"true\",\"foreground_s\":{seconds}}}"
        ))
        .expect("the foreground field decodes");
        assert_eq!(
            super::validate(args, root.path()).unwrap_err().to_string(),
            "exec: foreground_s must be between 1 and 86400"
        );
    }
    for (text, window) in [
        (r#"{"command":"true","foreground_s":1}"#, Some(1)),
        (r#"{"command":"true","foreground_s":86400}"#, Some(86_400)),
        (r#"{"command":"true"}"#, None),
    ] {
        let args = super::decode(text).expect("the arguments decode");
        let validated = super::validate(args, root.path()).expect("the arguments validate");
        assert_eq!(validated.foreground, window.map(Duration::from_secs));
    }
}

#[test]
fn validate_keeps_cwd_inside_the_canonical_workspace() {
    let root = tempfile::tempdir().expect("create a workspace");
    std::fs::create_dir_all(root.path().join("subdir")).expect("create the child directory");
    let validated = super::validate(
        ExecArgs {
            command: "pwd".into(),
            cwd: Some("subdir".into()),
            timeout_seconds: Some(1),
            foreground_s: None,
        },
        root.path(),
    )
    .expect("the workspace child exists");
    assert_eq!(
        validated.cwd,
        std::fs::canonicalize(root.path().join("subdir")).unwrap()
    );
    assert_eq!(validated.timeout, Some(Duration::from_secs(1)));
    assert_eq!(validated.timeout_seconds, Some(1));
}

#[test]
fn validate_rejects_absolute_and_escaping_cwds() {
    let parent = tempfile::tempdir().expect("create a temporary parent");
    let root = parent.path().join("workspace");
    let outside = parent.path().join("outside");
    std::fs::create_dir_all(&root).expect("create the workspace");
    std::fs::create_dir_all(&outside).expect("create an existing outside directory");
    for cwd in [root.to_string_lossy().into_owned(), "../outside".into()] {
        let error = super::validate(
            ExecArgs {
                command: "pwd".into(),
                cwd: Some(cwd),
                timeout_seconds: None,
                foreground_s: None,
            },
            &root,
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "exec: cwd must be relative to the workspace root"
        );
    }
}

#[test]
fn exec_job_transitions_match_the_table() {
    let at = Instant::now();
    let timeout = Duration::from_secs(3);
    let states = [
        ExecState::Queued,
        ExecState::Running,
        ExecState::Detached,
        ExecState::LadderPending,
        ExecState::Done,
    ];
    let events = [
        ExecEvent::Spawned { at },
        ExecEvent::Exit(ExecOutcome::Exited(0)),
        ExecEvent::TimeoutFire,
        ExecEvent::Cancel,
        ExecEvent::BudgetFire,
        ExecEvent::LadderComplete(ExecOutcome::Aborted),
    ];

    for state in states {
        for event in events {
            let expected = match (state, event) {
                (ExecState::Queued, ExecEvent::Spawned { .. }) => {
                    Some(ExecTransition::StartTimeout)
                }
                (ExecState::Queued, ExecEvent::Cancel) => {
                    Some(ExecTransition::EmitResult(ExecOutcome::Aborted))
                }
                (ExecState::Running, ExecEvent::Exit(outcome)) => {
                    Some(ExecTransition::EmitResult(outcome))
                }
                (ExecState::Running, ExecEvent::TimeoutFire) => {
                    Some(ExecTransition::Ladder(ExecOutcome::TimedOut))
                }
                (ExecState::Running | ExecState::Detached, ExecEvent::Cancel) => {
                    Some(ExecTransition::Ladder(ExecOutcome::Aborted))
                }
                (ExecState::Running, ExecEvent::BudgetFire) => Some(ExecTransition::Detach),
                (ExecState::Detached, ExecEvent::Exit(outcome)) => {
                    Some(ExecTransition::NoticeAndDone(outcome))
                }

                (ExecState::LadderPending, ExecEvent::LadderComplete(outcome)) => {
                    Some(ExecTransition::FinishAfterLadder(outcome))
                }
                _ => None,
            };
            let mut job = ExecJob::new(CallId::new("call-1"), "true".into(), Some(timeout));
            job.state = state;
            if expected.is_some() {
                assert_eq!(job.on(event), expected);
                if matches!(
                    (state, event),
                    (ExecState::Queued, ExecEvent::Spawned { .. })
                ) {
                    assert_eq!(job.deadline(), Some(at + timeout));
                    assert_eq!(job.state, ExecState::Running);
                }
                continue;
            }
            #[cfg(debug_assertions)]
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job.on(event))).is_err(),
                "illegal pair did not trigger the debug invariant: {state:?}, {event:?}"
            );
            #[cfg(not(debug_assertions))]
            assert_eq!(
                job.on(event),
                None,
                "illegal pair returned a transition: {state:?}, {event:?}"
            );
        }
    }
}

#[test]
fn final_text_appends_the_sandbox_note_once_from_capture_metadata() {
    let path = Path::new("/session/jobs/call.log");
    let text = super::final_text(
        Some("Permission denied"),
        path,
        ExecOutcome::Exited(1),
        None,
        true,
        true,
    );
    assert_eq!(text.matches("dalgon sandbox:").count(), 1);
    assert!(text.ends_with("add it to sandbox_writable in dal.toml."));
}

#[test]
fn final_text_omits_success_status_and_empty_preview() {
    let text = super::final_text(
        None,
        Path::new("/session/jobs/call.log"),
        ExecOutcome::Exited(0),
        None,
        false,
        false,
    );
    assert_eq!(text, "Full output: /session/jobs/call.log");
}

#[cfg(unix)]
fn write_exec_fixture(path: &Path, command: &str, timeout_seconds: u64) {
    let usage = sonic_rs::json!({
        "input_tokens": 1,
        "cached_input_tokens": 0,
        "output_tokens": 1,
        "reasoning_tokens": null,
        "cache_write_tokens": 0,
        "cost_usd": null
    });
    let first = sonic_rs::json!({
        "kind": "events",
        "events": [
            {"type": "tool_call_started", "id": "exec-call", "name": "exec"},
            {"type": "tool_calls_done", "calls": [{
                "id": "exec-call",
                "name": "exec",
                "args": {
                    "kind": "parsed",
                    "value": {"command": command, "timeout_seconds": timeout_seconds}
                }
            }]},
            {"type": "usage", "usage": usage},
            {"type": "stop", "reason": "tool_use"}
        ]
    });
    let second = sonic_rs::json!({
        "kind": "events",
        "events": [
            {"type": "text_delta", "text": "exec completed"},
            {"type": "tool_calls_done", "calls": []},
            {"type": "usage", "usage": usage},
            {"type": "stop", "reason": "end_turn"}
        ]
    });
    std::fs::write(
        path,
        format!(
            "{}\n{}\n",
            sonic_rs::to_string(&first).expect("encode first response"),
            sonic_rs::to_string(&second).expect("encode second response")
        ),
    )
    .expect("write scripted fixture");
}

#[cfg(unix)]
struct ExecRun {
    workspace: tempfile::TempDir,
    thread: dal_core::SessionId,
    settled: dal_core::ToolOutcomeView,
}

/// Runs one scripted exec call through a real host and returns its settled result.
#[cfg(unix)]
async fn run_scripted_exec(
    command: &str,
    timeout_seconds: u64,
    env_vars: BTreeMap<OsString, OsString>,
) -> ExecRun {
    let data = tempfile::tempdir().expect("create data root");
    let workspace_dir = tempfile::tempdir().expect("create workspace");
    let fixture_path = data.path().join("exec-fixture.jsonl");
    write_exec_fixture(&fixture_path, command, timeout_seconds);
    let user = format!(
        "model = \"openai/gpt-6\"\napproval = \"all\"\n[providers.scripted]\nfixture = {:?}\n",
        fixture_path.to_string_lossy()
    );
    let config =
        Config::load(ConfigProduct::Dalgon, data.path(), "", Some(&user)).expect("load config");
    let product = Product {
        name: "dal",
        data_root: data.path().to_path_buf(),
        defaults: "",
        extensions: vec![crate::extension(crate::ToolsConfig::default()).expect("tools extension")],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: env_vars,
        cwd: workspace_dir.path().to_path_buf(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await.expect("start host");
    let workspace = Workspace::new(workspace_dir.path().to_path_buf()).expect("workspace");
    let agent = host
        .open(
            SessionRef::Ephemeral { workspace },
            ClientId::new("exec-outcome-test"),
        )
        .await
        .expect("open session");
    let thread = agent
        .view(PageReq::default())
        .expect("read session view")
        .session
        .id;
    let mut updates = agent.subscribe(None).expect("subscribe to session");
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Run the scripted exec call.".into(),
            }],
        })
        .await
        .expect("submit prompt");
    assert!(
        matches!(reply, dal_core::Reply::Accepted { .. }),
        "got {reply:?}"
    );
    let settled = tokio::time::timeout(Duration::from_secs(20), async {
        let mut settled = None;
        loop {
            let Some(Delivery::Update(update)) = updates.next().await else {
                panic!("session ended before the exec turn ended");
            };
            match &update.kind {
                UpdateKind::ToolSettled { outcome, .. } => settled = Some(outcome.clone()),
                UpdateKind::TurnEnded { .. } => break,
                _ => {}
            }
        }
        settled
    })
    .await
    .expect("wait for exec turn")
    .expect("the exec call settled");
    let shutdown = host.shutdown(Duration::from_secs(1)).await;
    assert_eq!(shutdown.sessions_closed, 1);
    ExecRun {
        workspace: workspace_dir,
        thread,
        settled,
    }
}

#[cfg(unix)]
#[tokio::test]
async fn exec_child_gets_provenance_after_captured_environment_overlays() {
    let run = run_scripted_exec(
        r#"printf '%s\n' "$DAL_THREAD_ID" "$DAL_TOOL_CALL_ID" > provenance.txt"#,
        10,
        BTreeMap::from([
            (
                OsString::from("DAL_THREAD_ID"),
                OsString::from("captured-thread"),
            ),
            (
                OsString::from("DAL_TOOL_CALL_ID"),
                OsString::from("captured-call"),
            ),
        ]),
    )
    .await;
    assert!(!run.settled.is_error, "got {:?}", run.settled);
    let contents = tokio::fs::read_to_string(run.workspace.path().join("provenance.txt"))
        .await
        .expect("read provenance output");
    assert_eq!(contents, format!("{}\nexec-call\n", run.thread));
}

#[cfg(unix)]
#[tokio::test]
async fn exec_reports_a_nonzero_exit_as_an_error_with_the_code() {
    let run = run_scripted_exec("echo partial; exit 3", 10, BTreeMap::new()).await;
    assert!(run.settled.is_error, "got {:?}", run.settled);
    assert!(
        run.settled.text.contains("partial")
            && run.settled.text.contains("Full output: ")
            && run.settled.text.ends_with("\nCommand exited with code 3"),
        "got {:?}",
        run.settled.text
    );
}

#[cfg(unix)]
#[tokio::test]
async fn exec_reports_a_signalled_command_as_an_error_with_the_signal() {
    let run = run_scripted_exec("kill -KILL $$", 10, BTreeMap::new()).await;
    assert!(run.settled.is_error, "got {:?}", run.settled);
    assert!(
        run.settled
            .text
            .ends_with("\nCommand was killed by signal 9"),
        "got {:?}",
        run.settled.text
    );
}

#[cfg(unix)]
#[tokio::test]
async fn exec_reports_a_timed_out_command_as_an_error_with_the_limit() {
    let run = run_scripted_exec("sleep 60", 1, BTreeMap::new()).await;
    assert!(run.settled.is_error, "got {:?}", run.settled);
    assert!(
        run.settled
            .text
            .ends_with("\nCommand timed out after 1 seconds"),
        "got {:?}",
        run.settled.text
    );
}

fn settled(
    status: ProcStatus,
    timeout_seconds: Option<u64>,
    preview: &[u8],
) -> (ToolOutcome, ExecJob) {
    let timeout = timeout_seconds.map(Duration::from_secs);
    let mut job = ExecJob::new(CallId::new("call-1"), "cmd".into(), timeout);
    job.on(ExecEvent::Spawned { at: Instant::now() });
    let validated = ValidatedCall {
        command: "cmd".into(),
        cwd: Path::new("/workspace").to_path_buf(),
        timeout,
        timeout_seconds,
        foreground: None,
    };
    let result = ProcResult {
        status,
        outcome: JobOutcome::Lost,
        preview: preview.into(),
        log_path: Path::new("/session/jobs/call.log").to_path_buf(),
        stdout_prefix: Box::default(),
        stdout_prefix_overflowed: false,
        denial_seen: false,
        completion_tail: Box::default(),
    };
    let outcome = ExecTool::settle(&mut job, &validated, Ok(result), false);
    (outcome, job)
}

fn error_text(outcome: ToolOutcome) -> String {
    match outcome {
        ToolOutcome::Err(error) => error.to_string(),
        other => panic!("expected a tool error, got {other:?}"),
    }
}

#[test]
fn settle_maps_a_zero_exit_to_success_without_a_status_line() {
    let (outcome, job) = settled(ProcStatus::Exited { code: 0 }, None, b"done");
    assert!(matches!(job.state, ExecState::Done));
    let ToolOutcome::Ok(output) = outcome else {
        panic!("a zero exit must succeed, got {outcome:?}");
    };
    assert_eq!(
        output.to_string(),
        "done\nFull output: /session/jobs/call.log"
    );
}

#[test]
fn settle_maps_each_failed_terminal_status_to_its_own_error_text() {
    let preview = b"tail";
    let output = "tail\nFull output: /session/jobs/call.log";
    let cases = [
        (
            ProcStatus::Exited { code: 3 },
            None,
            "Command exited with code 3",
        ),
        (
            ProcStatus::Exited { code: -1 },
            None,
            "Command exited with code -1",
        ),
        (
            ProcStatus::Signaled { signal: 9 },
            None,
            "Command was killed by signal 9",
        ),
        (
            ProcStatus::TimedOut,
            Some(5),
            "Command timed out after 5 seconds",
        ),
        (ProcStatus::TimedOut, None, "Command timed out"),
        (ProcStatus::Cancelled, Some(5), "Command aborted"),
    ];
    for (status, timeout_seconds, line) in cases {
        let (outcome, job) = settled(status, timeout_seconds, preview);
        assert!(matches!(job.state, ExecState::Done), "{status:?}");
        assert_eq!(
            error_text(outcome),
            format!("{output}\n{line}"),
            "{status:?}"
        );
    }
}

#[test]
fn settle_passes_a_wait_failure_through_without_touching_the_job() {
    let mut job = ExecJob::new(CallId::new("call-1"), "cmd".into(), None);
    job.on(ExecEvent::Spawned { at: Instant::now() });
    let validated = ValidatedCall {
        command: "cmd".into(),
        cwd: Path::new("/workspace").to_path_buf(),
        timeout: None,
        timeout_seconds: None,
        foreground: None,
    };
    let outcome = ExecTool::settle(
        &mut job,
        &validated,
        Err(ToolError::message("wait failed")),
        false,
    );
    assert_eq!(error_text(outcome), "wait failed");
    assert!(matches!(job.state, ExecState::Running));
}
