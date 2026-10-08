use super::{ExecArgs, ExecEvent, ExecJob, ExecOutcome, ExecState, ExecTransition};
use dal_core::CallId;
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
fn write_exec_fixture(path: &Path, command: &str) {
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
                    "value": {"command": command, "timeout_seconds": 10}
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
#[tokio::test]
async fn exec_child_gets_provenance_after_captured_environment_overlays() {
    let data = tempfile::tempdir().expect("create data root");
    let workspace_dir = tempfile::tempdir().expect("create workspace");
    let fixture_path = data.path().join("exec-provenance.jsonl");
    write_exec_fixture(
        &fixture_path,
        r#"printf '%s\n' "$DAL_THREAD_ID" "$DAL_TOOL_CALL_ID" > provenance.txt"#,
    );
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
        vars: BTreeMap::from([
            (
                OsString::from("DAL_THREAD_ID"),
                OsString::from("captured-thread"),
            ),
            (
                OsString::from("DAL_TOOL_CALL_ID"),
                OsString::from("captured-call"),
            ),
        ]),
        cwd: workspace_dir.path().to_path_buf(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await.expect("start host");
    let workspace = Workspace::new(workspace_dir.path().to_path_buf()).expect("workspace");
    let agent = host
        .open(
            SessionRef::Ephemeral { workspace },
            ClientId::new("exec-provenance-test"),
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
    assert!(matches!(reply, dal_core::Reply::Accepted { .. }));
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let Some(Delivery::Update(update)) = updates.next().await else {
                panic!("session ended before the exec turn ended");
            };
            if matches!(update.kind, UpdateKind::TurnEnded { .. }) {
                break;
            }
        }
    })
    .await
    .expect("wait for exec turn");
    let contents = tokio::fs::read_to_string(workspace_dir.path().join("provenance.txt"))
        .await
        .expect("read provenance output");
    assert_eq!(contents, format!("{thread}\nexec-call\n"));
    let shutdown = host.shutdown(Duration::from_secs(1)).await;
    assert_eq!(shutdown.sessions_closed, 1);
}
