use std::num::NonZeroU64;
use std::path::PathBuf;
use std::time::Duration;

use super::{Answer, AnswerValue, CallGrant, Choice, JobEnd, Owner, Preview, Question, Request};
use crate::id::{CallId, JobId, RequestId, TurnId};
use crate::raw::RawJson;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn preview() -> Preview {
    Preview {
        title: "Patch".into(),
        body: "Update the selected files.".into(),
        digest: Some([0x5a; 32]),
    }
}

fn call_grant() -> CallGrant {
    CallGrant {
        argv_prefix: "git".into(),
        roots: vec![PathBuf::from("/workspace"), PathBuf::from("/tmp")],
        until: JobEnd(JobId::new_v7()),
    }
}

fn questions() -> [Question; 5] {
    [
        Question::Approval {
            tool: "exec".into(),
            preview: preview(),
            grant: Some(call_grant()),
            call: Some(CallId::new("call-1")),
        },
        Question::Grant {
            ext: "web".into(),
            origin: "user".into(),
            capabilities: vec!["net".into(), "docs".into()],
            detail: None,
        },
        Question::Select {
            prompt: "Choose a branch".into(),
            options: vec![
                Choice {
                    label: "main".into(),
                    description: None,
                },
                Choice {
                    label: "release".into(),
                    description: Some("Stable branch".into()),
                },
            ],
            multi: false,
            preview: Some(preview()),
        },
        Question::Confirm {
            text: "Continue?".into(),
        },
        Question::Text {
            prompt: "Commit message".into(),
            placeholder: Some("Describe the change".into()),
        },
    ]
}

#[test]
fn answer_variants_round_trip_with_snake_case_tags() -> TestResult {
    let variants = [
        (Answer::Approve, r#"{"type":"approve"}"#),
        (
            Answer::ApproveForSession,
            r#"{"type":"approve_for_session"}"#,
        ),
        (Answer::Decline, r#"{"type":"decline"}"#),
        (Answer::Cancel, r#"{"type":"cancel"}"#),
    ];
    for (answer, expected) in variants {
        let encoded = sonic_rs::to_string(&answer)?;
        assert_eq!(encoded, expected);
        assert_eq!(sonic_rs::from_str::<Answer>(&encoded)?, answer);
    }

    let value = Answer::Value(RawJson::parse(r#"["one", 2]"#)?);
    let encoded = sonic_rs::to_string(&value)?;
    assert_eq!(encoded, r#"{"type":"value","value":["one", 2]}"#);
    assert_eq!(sonic_rs::from_str::<Answer>(&encoded)?, value);
    Ok(())
}

#[test]
fn answer_value_wraps_raw_payload_transparently() -> TestResult {
    let raw = RawJson::parse(r#"{"selected": ["a", "b"]}"#)?;
    let value = AnswerValue::from(raw);
    let encoded = sonic_rs::to_string(&value)?;
    assert_eq!(encoded, r#"{"selected": ["a", "b"]}"#);
    let decoded = sonic_rs::from_str::<AnswerValue>(&encoded)?;
    assert_eq!(decoded, value);
    assert_eq!(decoded.as_raw(), value.as_raw());
    assert_eq!(value.into_raw(), decoded.into_raw());
    Ok(())
}

#[test]
fn value_payload_round_trips_every_json_shape_and_keeps_raw_text() -> TestResult {
    let payloads = [
        "null",
        "true",
        "false",
        "-0.0",
        "1e+03",
        "9007199254740993123456789",
        "0",
        r#""""#,
        r#""line\nbreak""#,
        r#""日本語 🎯""#,
        "[]",
        r#"["label", 9007199254740993123456789]"#,
        "{}",
        r#"{ "z" : 1e+03, "a": 9007199254740993123456789 }"#,
    ];

    for payload in payloads {
        // The tag sits after the payload to prove it is found by member
        // scan rather than by position.
        let wire = format!(r#"{{"value":{payload},"type":"value"}}"#);
        let decoded = sonic_rs::from_str::<Answer>(&wire)?;
        let Answer::Value(raw) = &decoded else {
            return Err("expected a value answer".into());
        };
        assert_eq!(raw.as_str(), payload, "raw text changed for {payload}");

        let bytes = sonic_rs::to_vec(&decoded)?;
        let encoded = std::str::from_utf8(&bytes)?;
        assert!(encoded.contains(payload), "payload changed: {encoded}");
        assert_eq!(sonic_rs::from_slice::<Answer>(&bytes)?, decoded);
    }
    Ok(())
}

#[test]
fn owner_question_and_request_round_trip_every_shape() -> TestResult {
    let owners = [
        Owner::Core,
        Owner::Extension {
            name: "web".into(),
            origin: "bundled".into(),
        },
    ];
    for owner in owners {
        let encoded = sonic_rs::to_string(&owner)?;
        assert_eq!(sonic_rs::from_str::<Owner>(&encoded)?, owner);
    }

    for question in questions() {
        let encoded = sonic_rs::to_string(&question)?;
        assert_eq!(sonic_rs::from_str::<Question>(&encoded)?, question);

        let request = Request {
            id: RequestId::new_v7(),
            turn: Some(TurnId::new(NonZeroU64::MIN)),
            owner: Owner::Extension {
                name: "web".into(),
                origin: "bundled".into(),
            },
            question,
            timeout: Duration::from_millis(1_234),
            default: Answer::Value(RawJson::parse(r#"{ "choice" : "main", "n":1e+03 }"#)?),
        };
        let bytes = sonic_rs::to_vec(&request)?;
        let decoded = sonic_rs::from_slice::<Request>(&bytes)?;
        assert_eq!(decoded, request);
    }
    Ok(())
}

#[test]
fn approval_call_id_is_optional_on_the_wire() -> TestResult {
    let without = Question::Approval {
        tool: "exec".into(),
        preview: preview(),
        grant: None,
        call: None,
    };
    let encoded = sonic_rs::to_string(&without)?;
    assert!(
        !encoded.contains("call"),
        "absent call is omitted: {encoded}"
    );
    assert_eq!(sonic_rs::from_str::<Question>(&encoded)?, without);

    let legacy = r#"{"type":"approval","tool":"exec","preview":{"title":"Patch","body":"","digest":null},"grant":null}"#;
    assert_eq!(
        sonic_rs::from_str::<Question>(legacy)?,
        Question::Approval {
            tool: "exec".into(),
            preview: Preview {
                title: "Patch".into(),
                body: "".into(),
                digest: None,
            },
            grant: None,
            call: None,
        }
    );

    let with = Question::Approval {
        tool: "exec".into(),
        preview: preview(),
        grant: None,
        call: Some(CallId::new("provider-call-7")),
    };
    let encoded = sonic_rs::to_string(&with)?;
    assert!(encoded.contains(r#""call":"provider-call-7""#), "{encoded}");
    assert_eq!(sonic_rs::from_str::<Question>(&encoded)?, with);
    Ok(())
}

#[test]
fn tagged_decoders_reject_unknown_and_duplicate_discriminators() {
    let answers = [
        (
            r#"{"type":"future_answer"}"#,
            "unknown `type` variant `future_answer`",
        ),
        (
            r#"{"type":"approve","type":"decline"}"#,
            "duplicate field `type`",
        ),
        (
            r#"{"type":"value","value":1,"type":"value"}"#,
            "duplicate field `type`",
        ),
        (r#"{"type":"value"}"#, "missing field `value`"),
        (
            r#"{"type":"value","value":1,"value":2}"#,
            "duplicate field `value`",
        ),
        (r#""approve""#, "expected a JSON object"),
    ];
    for (input, reason) in answers {
        let Some(error) = sonic_rs::from_str::<Answer>(input).err() else {
            panic!("accepted invalid answer {input}");
        };
        assert!(
            error.to_string().contains(reason),
            "wrong rejection for {input}: {error}"
        );
    }

    let questions = [
        (
            r#"{"type":"future_question"}"#,
            "unknown `type` variant `future_question`",
        ),
        (
            r#"{"type":"confirm","type":"text","text":"Continue?"}"#,
            "duplicate field `type`",
        ),
        (r#"{"type":"confirm"}"#, "missing field `text`"),
        ("[1,2]", "expected a JSON object"),
    ];
    for (input, reason) in questions {
        let Some(error) = sonic_rs::from_str::<Question>(input).err() else {
            panic!("accepted invalid question {input}");
        };
        assert!(
            error.to_string().contains(reason),
            "wrong rejection for {input}: {error}"
        );
    }

    let owners = [
        (
            r#"{"type":"future_owner"}"#,
            "unknown `type` variant `future_owner`",
        ),
        (
            r#"{"type":"core","type":"extension","name":"web","origin":"user"}"#,
            "duplicate field `type`",
        ),
        (
            r#"{"type":"extension","name":"web"}"#,
            "missing field `origin`",
        ),
    ];
    for (input, reason) in owners {
        let Some(error) = sonic_rs::from_str::<Owner>(input).err() else {
            panic!("accepted invalid owner {input}");
        };
        assert!(
            error.to_string().contains(reason),
            "wrong rejection for {input}: {error}"
        );
    }
}

#[test]
fn request_decode_names_the_nested_unknown_tag() -> TestResult {
    let id = sonic_rs::to_string(&RequestId::new_v7())?;
    let turn = sonic_rs::to_string(&TurnId::new(NonZeroU64::MIN))?;
    let request = |question: &str, answer: &str| {
        format!(
            r#"{{"id":{id},"turn":{turn},"owner":{{"type":"core"}},"question":{question},"timeout":{{"secs":1,"nanos":0}},"default":{answer}}}"#
        )
    };

    let Some(bad_question) = sonic_rs::from_str::<Request>(&request(
        r#"{"type":"future_question"}"#,
        r#"{"type":"cancel"}"#,
    ))
    .err() else {
        panic!("accepted unknown nested question tag");
    };
    assert!(
        bad_question
            .to_string()
            .contains("unknown `type` variant `future_question`"),
        "wrong rejection: {bad_question}"
    );

    let Some(bad_answer) = sonic_rs::from_str::<Request>(&request(
        r#"{"type":"confirm","text":"Continue?"}"#,
        r#"{"type":"future_answer"}"#,
    ))
    .err() else {
        panic!("accepted unknown nested answer tag");
    };
    assert!(
        bad_answer
            .to_string()
            .contains("unknown `type` variant `future_answer`"),
        "wrong rejection: {bad_answer}"
    );
    Ok(())
}
