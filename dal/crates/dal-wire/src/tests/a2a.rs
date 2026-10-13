use std::num::NonZeroU64;
use std::time::Duration;

use dal_core::{
    Answer, Choice, Notice, Owner, Part, Preview, Question, Request, RequestId, SessionId, Stop,
    StreamChannel, TurnId, UpdateKind,
};
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};

use crate::a2a::{TaskEvent, TaskState, transition};
use crate::serve::a2a::card_base;
use crate::serve::a2a::error::{Fail, VERSION_TEXT, version_ok};
use crate::serve::a2a::events::{apply, failed_task_json, frame, step, task_json};
use crate::serve::a2a::parts::{APPROVAL_HINT, DATA_HINT, answer_for, prompt_parts, question_text};
use crate::serve::a2a::table::{
    A2aState, Framing, MAX_CONTEXTS, MAX_TASKS, TASK_LIMIT_TEXT, TaskKey, TaskRec,
};

fn turn(value: u64) -> TurnId {
    TurnId::new(NonZeroU64::new(value).expect("nonzero turn"))
}

fn key(value: u64) -> TaskKey {
    TaskKey {
        session: SessionId::new_v7(),
        turn: turn(value),
    }
}

fn json(text: &str) -> Value {
    sonic_rs::from_str(text).expect("test JSON")
}

fn request(question: Question, at: TurnId) -> Request {
    Request {
        id: RequestId::new_v7(),
        turn: Some(at),
        owner: Owner::Core,
        question,
        timeout: Duration::from_secs(60),
        default: Answer::Decline,
    }
}

fn select() -> Question {
    Question::Select {
        prompt: "Pick one".into(),
        options: vec![
            Choice {
                label: "a".into(),
                description: None,
            },
            Choice {
                label: "b".into(),
                description: None,
            },
        ],
        multi: false,
        preview: None,
    }
}

fn approval() -> Question {
    Question::Approval {
        tool: "exec".into(),
        preview: Preview {
            title: "git status".into(),
            body: String::new().into(),
            digest: None,
        },
        grant: None,
    }
}
#[test]
fn grant_question_text_includes_declared_server_details() {
    let question = Question::Grant {
        ext: "web".into(),
        origin: "user".into(),
        capabilities: vec!["mcp".into()],
        detail: Some("search: URL https://mcp.example/search".into()),
    };
    assert_eq!(
        question_text(&question),
        "Grant web: mcp\nsearch: URL https://mcp.example/search"
    );
}

#[test]
fn state_edges_match_the_plan_graph() {
    use TaskEvent as E;
    use TaskState as S;
    let states = [
        S::Submitted,
        S::Working,
        S::InputRequired,
        S::Completed,
        S::Rejected,
        S::Canceled,
        S::Failed,
    ];
    let events = [
        E::Start,
        E::RequestInput,
        E::Answer,
        E::Complete,
        E::Refuse,
        E::Cancel,
        E::Fail,
    ];
    let legal = [
        (S::Submitted, E::Start, S::Working),
        (S::Working, E::RequestInput, S::InputRequired),
        (S::InputRequired, E::Answer, S::Working),
        (S::Working, E::Complete, S::Completed),
        (S::Working, E::Refuse, S::Rejected),
        (S::Working, E::Cancel, S::Canceled),
        (S::InputRequired, E::Cancel, S::Canceled),
        (S::Working, E::Fail, S::Failed),
    ];
    for state in states {
        for event in events {
            let expected = legal
                .iter()
                .find(|(from, on, _)| *from == state && *on == event)
                .map(|(_, _, to)| *to);
            assert_eq!(transition(state, event), expected, "{state:?} on {event:?}");
        }
        if state.is_terminal() {
            assert!(
                events
                    .iter()
                    .all(|event| transition(state, *event).is_none())
            );
        }
    }
}

#[test]
fn error_map_has_rpc_and_rest_shapes() {
    let cases = [
        ("TASK_NOT_FOUND", -32001, 404, "NOT_FOUND"),
        ("TASK_NOT_CANCELABLE", -32002, 400, "FAILED_PRECONDITION"),
        ("UNSUPPORTED_OPERATION", -32004, 400, "UNIMPLEMENTED"),
        (
            "CONTENT_TYPE_NOT_SUPPORTED",
            -32005,
            400,
            "INVALID_ARGUMENT",
        ),
        ("VERSION_NOT_SUPPORTED", -32009, 400, "FAILED_PRECONDITION"),
        ("INVALID_ARGUMENT", -32602, 400, "INVALID_ARGUMENT"),
        ("INTERNAL", -32603, 500, "INTERNAL"),
    ];
    for (reason, code, http, status) in cases {
        let fail = Fail::new(reason, "boom");
        let rpc = fail.rpc_envelope(&Value::from(7));
        assert_eq!(
            rpc,
            json(&format!(
                r#"{{"jsonrpc":"2.0","id":7,"error":{{"code":{code},"message":"boom","data":[{{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"{reason}","domain":"a2a-protocol.org","metadata":{{}}}}]}}}}"#
            ))
        );
        let resp = fail.rest();
        assert_eq!(resp.status, http, "{reason}");
        assert_eq!(
            fail.rest_body(),
            json(&format!(
                r#"{{"error":{{"code":{http},"status":"{status}","message":"boom","details":[{{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"{reason}","domain":"a2a-protocol.org","metadata":{{}}}}]}}}}"#
            ))
        );
    }
    let version = Fail::version();
    assert_eq!(version.kind.code, -32009);
    assert_eq!(version.message, VERSION_TEXT);
}

#[test]
fn version_needs_one_point_zero() {
    assert!(version_ok(Some("1.0"), ""));
    assert!(!version_ok(Some("0.3"), "A2A-Version=1.0"));
    assert!(!version_ok(None, ""));
    assert!(version_ok(None, "x=1&A2A-Version=1.0"));
    assert!(!version_ok(None, "A2A-Version=0.3"));
}

#[test]
fn card_base_prefers_host_header() {
    let bind = "127.0.0.1".parse().expect("ip");
    assert_eq!(
        card_base(Some("localhost:7437"), bind, 9),
        "http://localhost:7437"
    );
    assert_eq!(card_base(Some("[::1]:7437"), bind, 9), "http://[::1]:7437");
    assert_eq!(card_base(Some("[::1]"), bind, 9), "http://[::1]:9");
    assert_eq!(card_base(None, bind, 7437), "http://127.0.0.1:7437");
    let v6 = "::1".parse().expect("ip");
    assert_eq!(card_base(None, v6, 7437), "http://[::1]:7437");
}

#[test]
fn task_ids_round_trip() {
    let task = key(42);
    assert_eq!(TaskKey::parse(&task.to_string()), Some(task));
    assert_eq!(TaskKey::parse("42"), None);
    assert_eq!(TaskKey::parse(&format!("{}.0", task.session)), None);
}

#[test]
fn task_table_caps_unfinished_tasks_and_evicts_terminal() {
    let mut table = A2aState::new();
    let keys: Vec<TaskKey> = (1..=u64::try_from(MAX_TASKS).expect("cap fits u64"))
        .map(key)
        .collect();
    for task in &keys {
        table
            .insert(TaskRec::started(*task, String::new()))
            .expect("room for task");
    }
    let refused = table
        .insert(TaskRec::started(key(5000), String::new()))
        .expect_err("table is full of unfinished tasks");
    assert_eq!(refused.kind.code, -32603);
    assert_eq!(refused.message, TASK_LIMIT_TEXT);
    assert_eq!(table.task_count(), MAX_TASKS);

    let second = keys[1];
    let task = table.task_mut(second).expect("second task");
    assert!(task.advance(TaskEvent::Complete));
    let extra = key(5001);
    table
        .insert(TaskRec::started(extra, String::new()))
        .expect("terminal task makes room");
    assert_eq!(table.task_count(), MAX_TASKS);
    assert!(table.task(second).is_none());
    assert!(table.task(keys[0]).is_some());
    assert!(table.task(extra).is_some());
}

#[test]
fn context_table_evicts_oldest_idle_context() {
    let mut table = A2aState::new();
    let sessions: Vec<SessionId> = (0..MAX_CONTEXTS).map(|_| SessionId::new_v7()).collect();
    for session in &sessions {
        assert_eq!(
            table.remember_context(*session),
            [] as [dal_core::SessionId; 0]
        );
    }
    let busy = TaskKey {
        session: sessions[0],
        turn: turn(1),
    };
    table
        .insert(TaskRec::started(busy, String::new()))
        .expect("insert live task");
    let fresh = SessionId::new_v7();
    assert_eq!(table.remember_context(fresh), vec![sessions[1]]);
    assert_eq!(table.context_count(), MAX_CONTEXTS);
    assert!(table.has_context(sessions[0]));
    assert!(!table.has_context(sessions[1]));
    assert!(table.has_context(fresh));
    assert_eq!(
        table.remember_context(fresh),
        [] as [dal_core::SessionId; 0]
    );
}

#[test]
fn parts_map_text_images_and_data() {
    let parts = prompt_parts(&json(
        r#"[{"text":"hi"},{"raw":"iVBORw==","mediaType":"image/png"},{"data":{"a": [1, 2]}}]"#,
    ))
    .expect("supported parts");
    assert_eq!(
        parts,
        vec![
            Part::Text { text: "hi".into() },
            Part::Image {
                mime: "image/png".into(),
                bytes: vec![0x89, b'P', b'N', b'G'].into(),
            },
            Part::Text {
                text: r#"{"a":[1,2]}"#.into(),
            },
        ]
    );
}

#[test]
fn parts_refuse_urls_and_other_media() {
    let url = prompt_parts(&json(
        r#"[{"url":"https://x/y.png","mediaType":"image/png"}]"#,
    ))
    .expect_err("url part");
    assert_eq!(
        (url.kind.code, url.message.as_str()),
        (-32602, "dalgon does not fetch part urls")
    );
    let pdf = prompt_parts(&json(r#"[{"raw":"JVBE","mediaType":"application/pdf"}]"#))
        .expect_err("pdf part");
    assert_eq!(
        (pdf.kind.code, pdf.message.as_str()),
        (
            -32005,
            "dalgon accepts text and images: application/pdf is not supported"
        )
    );
    assert_eq!(
        prompt_parts(&json("[]")).expect_err("empty").kind.code,
        -32602
    );
}

#[test]
fn answers_follow_the_question_shape() {
    let approve = answer_for(&approval(), &json(r#"[{"text":"approve"}]"#)).expect("approve");
    assert_eq!(approve, Answer::Approve);
    let data = answer_for(
        &approval(),
        &json(r#"[{"data":{"answer":"approve_for_session"}}]"#),
    )
    .expect("data approval");
    assert_eq!(data, Answer::ApproveForSession);
    let bad = answer_for(&approval(), &json(r#"[{"text":"yes"}]"#)).expect_err("bad text");
    assert_eq!(
        (bad.kind.code, bad.message.as_str()),
        (-32602, APPROVAL_HINT)
    );

    let value = answer_for(&select(), &json(r#"[{"data":{"answer":"b"}}]"#)).expect("select");
    assert_eq!(
        value,
        Answer::Value(dal_core::RawJson::parse(r#""b""#).expect("raw"))
    );
    let missing = answer_for(&select(), &json(r#"[{"text":"b"}]"#)).expect_err("no data part");
    assert_eq!(
        (missing.kind.code, missing.message.as_str()),
        (-32602, DATA_HINT)
    );
}

fn kinds_of(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|event| {
            let (name, body) = event
                .as_object()
                .and_then(|object| object.iter().next())
                .expect("one stream response member");
            match name {
                "artifactUpdate" => format!(
                    "artifact:{}:{}:{}",
                    body.pointer(&sonic_rs::pointer!["artifact", "parts", 0, "text"])
                        .and_then(|text| text.as_str())
                        .unwrap_or("?"),
                    body.get("append")
                        .and_then(sonic_rs::JsonValueTrait::as_bool)
                        .unwrap_or(false),
                    body.get("lastChunk")
                        .and_then(sonic_rs::JsonValueTrait::as_bool)
                        .unwrap_or(false),
                ),
                "statusUpdate" => {
                    let state = body
                        .pointer(["status", "state"])
                        .and_then(|state| state.as_str())
                        .unwrap_or("?");
                    let meta = body
                        .get("metadata")
                        .and_then(|meta| meta.as_object())
                        .and_then(|meta| meta.iter().next())
                        .map(|(name, text)| format!(":{name}={}", text.as_str().unwrap_or("?")))
                        .unwrap_or_default();
                    format!("status:{state}{meta}")
                }
                other => other.to_owned(),
            }
        })
        .collect()
}

#[test]
fn stream_events_follow_the_plan_order() {
    let task_key = key(3);
    let mut task = TaskRec::started(task_key, "hi".to_owned());
    let asked = request(select(), task_key.turn);
    let updates = vec![
        UpdateKind::Delta {
            turn: task_key.turn,
            channel: StreamChannel::Text,
            text: "Hel".into(),
        },
        UpdateKind::Delta {
            turn: turn(9),
            channel: StreamChannel::Text,
            text: "wake".into(),
        },
        UpdateKind::Delta {
            turn: task_key.turn,
            channel: StreamChannel::Thinking,
            text: "hmm".into(),
        },
        UpdateKind::Delta {
            turn: task_key.turn,
            channel: StreamChannel::Text,
            text: "lo".into(),
        },
        UpdateKind::RuleFired {
            turn: task_key.turn,
            rule: "no-sudo".into(),
        },
        UpdateKind::Notice(Notice {
            turn: None,
            kind: "status".into(),
            text: "indexing".into(),
        }),
        UpdateKind::RequestOpened(asked.clone()),
        UpdateKind::RequestResolved {
            id: asked.id,
            answer: Answer::Value(dal_core::RawJson::parse(r#""a""#).expect("raw")),
            by: dal_core::ClientId::new("a2a#1"),
        },
        UpdateKind::TurnEnded {
            turn: task_key.turn,
            stop: Stop::EndTurn,
        },
    ];
    let mut events = vec![sonic_rs::json!({"task": task_json(&task)})];
    let mut input_required = None;
    for update in &updates {
        let pending = task.request.as_ref().map(|open| open.id);
        if let Some(next) = step(task_key.turn, pending, update) {
            let emitted = apply(&mut task, next);
            if task.state == TaskState::InputRequired {
                input_required = emitted.first().cloned();
            }
            events.extend(emitted);
        }
    }
    assert_eq!(
        kinds_of(&events),
        vec![
            "task",
            "artifact:Hel:false:false",
            "artifact:lo:true:false",
            "status:TASK_STATE_WORKING:dal.notice=rule no-sudo fired",
            "status:TASK_STATE_WORKING:dal.status=indexing",
            "status:TASK_STATE_INPUT_REQUIRED",
            "status:TASK_STATE_WORKING",
            "artifact::true:true",
            "status:TASK_STATE_COMPLETED",
        ]
    );
    let question = input_required.expect("input-required update");
    assert_eq!(
        question
            .pointer(&sonic_rs::pointer![
                "statusUpdate",
                "status",
                "message",
                "parts",
                1,
                "data",
                "question",
                "type"
            ])
            .and_then(|kind| kind.as_str()),
        Some("select")
    );
    assert_completed_view(&task, task_key);
}

fn assert_completed_view(task: &TaskRec, task_key: TaskKey) {
    let view = task_json(task);
    assert_eq!(
        view.pointer(["status", "state"])
            .and_then(|state| state.as_str()),
        Some("TASK_STATE_COMPLETED")
    );
    assert_eq!(
        view.pointer(&sonic_rs::pointer!["artifacts", 0, "parts", 0, "text"])
            .and_then(|text| text.as_str()),
        Some("Hello")
    );
    assert_eq!(
        view.get("contextId").and_then(|id| id.as_str()),
        Some(task_key.session.to_string().as_str())
    );
    assert_eq!(
        view.pointer(&sonic_rs::pointer!["history", 0, "parts", 0, "text"])
            .and_then(|text| text.as_str()),
        Some("hi")
    );
}

#[test]
fn stop_reasons_map_to_terminal_states() {
    let cases = [
        (Stop::EndTurn, TaskState::Completed),
        (Stop::Length, TaskState::Completed),
        (Stop::MaxSteps, TaskState::Completed),
        (Stop::Filter, TaskState::Rejected),
        (Stop::Cancelled, TaskState::Canceled),
        (Stop::Failed, TaskState::Failed),
    ];
    for (stop, expected) in cases {
        let task_key = key(1);
        let mut task = TaskRec::started(task_key, String::new());
        let end = step(
            task_key.turn,
            None,
            &UpdateKind::TurnEnded {
                turn: task_key.turn,
                stop,
            },
        )
        .expect("end step");
        apply(&mut task, end);
        assert_eq!(task.state, expected, "{stop:?}");
    }
}

#[test]
fn cancel_while_input_required_is_canceled() {
    let task_key = key(2);
    let mut task = TaskRec::started(task_key, String::new());
    let asked = request(approval(), task_key.turn);
    let ask = step(task_key.turn, None, &UpdateKind::RequestOpened(asked)).expect("ask");
    apply(&mut task, ask);
    assert_eq!(task.state, TaskState::InputRequired);
    let end = step(
        task_key.turn,
        None,
        &UpdateKind::TurnEnded {
            turn: task_key.turn,
            stop: Stop::Cancelled,
        },
    )
    .expect("end");
    apply(&mut task, end);
    assert_eq!(task.state, TaskState::Canceled);
    assert!(task.request.is_none());
}

#[test]
fn json_rpc_frames_wrap_each_stream_response() {
    let response = sonic_rs::json!({"statusUpdate": {"taskId": "t"}});
    assert_eq!(
        frame(&Framing::Rest, response.clone()),
        "data: {\"statusUpdate\":{\"taskId\":\"t\"}}\n\n"
    );
    let wrapped = frame(&Framing::JsonRpc(Value::from("r1")), response);
    let body = wrapped
        .strip_prefix("data: ")
        .and_then(|rest| rest.strip_suffix("\n\n"))
        .expect("one SSE event");
    assert_eq!(
        json(body),
        json(r#"{"jsonrpc":"2.0","id":"r1","result":{"statusUpdate":{"taskId":"t"}}}"#)
    );
}

#[test]
fn submit_failure_renders_a_failed_task() {
    let session = SessionId::new_v7();
    let task = failed_task_json(session, "hi", "the host is shut down");
    assert_eq!(
        task.pointer(["status", "state"])
            .and_then(|state| state.as_str()),
        Some("TASK_STATE_FAILED")
    );
    assert_eq!(
        task.pointer(&sonic_rs::pointer!["status", "message", "parts", 0, "text"])
            .and_then(|text| text.as_str()),
        Some("the host is shut down")
    );
    assert_eq!(
        task.get("contextId").and_then(|id| id.as_str()),
        Some(session.to_string().as_str())
    );
    let id = task.get("id").and_then(|id| id.as_str()).expect("task id");
    assert_eq!(TaskKey::parse(id), None);
}
