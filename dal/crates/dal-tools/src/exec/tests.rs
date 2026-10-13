use super::{ExecArgs, ExecEvent, ExecJob, ExecOutcome, ExecState, ExecTransition};
use dal_core::CallId;
use std::{
    path::Path,
    time::{Duration, Instant},
};

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
