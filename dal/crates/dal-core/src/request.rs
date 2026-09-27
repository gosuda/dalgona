//! Request, question, answer, and call-scoped grant values.
//!
//! These are data only: a drop resolves nothing, timers and question/answer
//! pairing belong to the request broker, and `Request.timeout` stays a
//! [`Duration`] here — the protocol adapter owns the integer-millisecond
//! wire member.
//!
//! Answer encoding is the adjacent shape named by the wire plan,
//! `{"type":"value","value":any}`: a naive internally-tagged derive would
//! flatten object payloads and reject primitive or array payloads, so the
//! unit answers carry only the tag and `Value` carries its raw JSON under
//! the `value` member. Decoding goes through [`raw::Tagged`], never serde
//! content buffering, so a value payload keeps its exact interior bytes,
//! member order, and number spelling.

use std::time::Duration;

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

use crate::id::{JobId, RequestId, TurnId};
use crate::raw::{RawJson, Tagged};

/// A preview shown with a question or approval request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Preview {
    /// The preview title.
    pub title: Box<str>,
    /// The preview body.
    pub body: Box<str>,
    /// The digest of the previewed content, when supplied.
    pub digest: Option<[u8; 32]>,
}

/// One choice offered by a selection question.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Choice {
    /// The answer value and display label of this choice.
    pub label: Box<str>,
    /// Additional display text for this choice.
    pub description: Option<Box<str>>,
}

/// A call-scoped grant for running one argv prefix inside selected roots.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CallGrant {
    /// The argv prefix covered by the grant.
    pub argv_prefix: Box<str>,
    /// The filesystem roots covered by the grant.
    pub roots: Vec<std::path::PathBuf>,
    /// The job whose completion ends the grant.
    pub until: JobEnd,
}

/// The job whose completion ends a call-scoped grant.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct JobEnd(
    /// The job whose completion ends the grant.
    pub JobId,
);

/// Identifies who opened a question.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Owner {
    /// The core opened the question.
    Core,
    /// An extension opened the question.
    Extension {
        /// The extension name.
        name: Box<str>,
        /// The extension origin.
        origin: Box<str>,
    },
}

#[derive(Deserialize)]
struct ExtensionOwnerFields {
    name: Box<str>,
    origin: Box<str>,
}

impl<'de> Deserialize<'de> for Owner {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let tagged = Tagged::decode(deserializer, "type", &["core", "extension"])?;
        match tagged.kind() {
            "core" => Ok(Self::Core),
            "extension" => {
                let fields: ExtensionOwnerFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Extension {
                    name: fields.name,
                    origin: fields.origin,
                })
            }
            other => Err(de::Error::custom(format!(
                "unknown request owner type `{other}`"
            ))),
        }
    }
}

/// A question presented to a client.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Question {
    /// Ask whether to run a tool, optionally with a call-scoped grant.
    Approval {
        /// The tool being approved.
        tool: Box<str>,
        /// The tool operation preview.
        preview: Preview,
        /// A grant that applies only to this call, when present.
        grant: Option<CallGrant>,
    },
    /// Ask the user to grant an extension its declared capabilities.
    Grant {
        /// The extension requesting capabilities.
        ext: Box<str>,
        /// The extension origin.
        origin: Box<str>,
        /// The requested capabilities.
        capabilities: Vec<Box<str>>,
    },
    /// Ask the client to select one or more offered choices.
    Select {
        /// The selection prompt.
        prompt: Box<str>,
        /// The available choices, in display order.
        options: Vec<Choice>,
        /// Whether the client may select more than one choice.
        multi: bool,
        /// Additional preview content, when present.
        preview: Option<Preview>,
    },
    /// Ask for a yes-or-no confirmation.
    Confirm {
        /// The confirmation text.
        text: Box<str>,
    },
    /// Ask for free-form text.
    Text {
        /// The text prompt.
        prompt: Box<str>,
        /// Placeholder text for the input.
        placeholder: Option<Box<str>>,
    },
}

#[derive(Deserialize)]
struct ApprovalQuestionFields {
    tool: Box<str>,
    preview: Preview,
    grant: Option<CallGrant>,
}

#[derive(Deserialize)]
struct GrantQuestionFields {
    ext: Box<str>,
    origin: Box<str>,
    capabilities: Vec<Box<str>>,
}

#[derive(Deserialize)]
struct SelectQuestionFields {
    prompt: Box<str>,
    options: Vec<Choice>,
    multi: bool,
    preview: Option<Preview>,
}

#[derive(Deserialize)]
struct ConfirmQuestionFields {
    text: Box<str>,
}

#[derive(Deserialize)]
struct TextQuestionFields {
    prompt: Box<str>,
    placeholder: Option<Box<str>>,
}

impl<'de> Deserialize<'de> for Question {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let tagged = Tagged::decode(
            deserializer,
            "type",
            &["approval", "grant", "select", "confirm", "text"],
        )?;
        match tagged.kind() {
            "approval" => {
                let fields: ApprovalQuestionFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Approval {
                    tool: fields.tool,
                    preview: fields.preview,
                    grant: fields.grant,
                })
            }
            "grant" => {
                let fields: GrantQuestionFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Grant {
                    ext: fields.ext,
                    origin: fields.origin,
                    capabilities: fields.capabilities,
                })
            }
            "select" => {
                let fields: SelectQuestionFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Select {
                    prompt: fields.prompt,
                    options: fields.options,
                    multi: fields.multi,
                    preview: fields.preview,
                })
            }
            "confirm" => {
                let fields: ConfirmQuestionFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Confirm { text: fields.text })
            }
            "text" => {
                let fields: TextQuestionFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Text {
                    prompt: fields.prompt,
                    placeholder: fields.placeholder,
                })
            }
            other => Err(de::Error::custom(format!(
                "unknown question type `{other}`"
            ))),
        }
    }
}

/// An answer to an approval, grant, selection, confirmation, or text question.
///
/// `Value` encodes as `{"type":"value","value":<payload>}` with the payload
/// kept as raw JSON, so every valid JSON answer shape — string, array,
/// object, number, boolean, or null — round-trips without re-encoding.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Answer {
    /// Approve this call, including its call-scoped grant when present.
    Approve,
    /// Approve this call with its session-level approval meaning.
    ApproveForSession,
    /// Decline the requested action and its grant.
    Decline,
    /// Cancel the question.
    Cancel,
    /// An arbitrary JSON answer value, retained without re-encoding.
    Value(RawJson),
}

#[derive(Deserialize)]
struct ValueAnswerFields {
    value: RawJson,
}

impl<'de> Deserialize<'de> for Answer {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let tagged = Tagged::decode(
            deserializer,
            "type",
            &[
                "approve",
                "approve_for_session",
                "decline",
                "cancel",
                "value",
            ],
        )?;
        match tagged.kind() {
            "approve" => Ok(Self::Approve),
            "approve_for_session" => Ok(Self::ApproveForSession),
            "decline" => Ok(Self::Decline),
            "cancel" => Ok(Self::Cancel),
            "value" => {
                let fields: ValueAnswerFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Value(fields.value))
            }
            other => Err(de::Error::custom(format!("unknown answer type `{other}`"))),
        }
    }
}

/// A pending question and its fail-closed default answer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Request {
    /// The request identity.
    pub id: RequestId,
    /// The turn that opened the request, when it belongs to a turn.
    pub turn: Option<TurnId>,
    /// The owner that opened the request.
    pub owner: Owner,
    /// The question presented to the client.
    pub question: Question,
    /// How long the request may remain open.
    pub timeout: Duration,
    /// The answer selected when the request resolves without a client answer.
    pub default: Answer,
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;
    use std::path::PathBuf;
    use std::time::Duration;

    use super::{Answer, CallGrant, Choice, JobEnd, Owner, Preview, Question, Request};
    use crate::id::{JobId, RequestId, TurnId};
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
            },
            Question::Grant {
                ext: "web".into(),
                origin: "user".into(),
                capabilities: vec!["net".into(), "docs".into()],
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
}
