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
        /// Additional detail about the exact capability declaration.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<Box<str>>,
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
    #[serde(default)]
    detail: Option<Box<str>>,
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
                    detail: fields.detail,
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

/// An arbitrary JSON answer value returned by the `ask` service.
///
/// This is the service-layer counterpart to [`Answer::Value`]: the broker
/// owns the timeout and the fail-closed default, so the service surface
/// carries only the value itself, retained without re-encoding.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct AnswerValue(pub RawJson);

impl AnswerValue {
    /// Borrows the underlying raw JSON payload.
    #[must_use]
    pub fn as_raw(&self) -> &RawJson {
        &self.0
    }

    /// Converts into the underlying raw JSON payload.
    #[must_use]
    pub fn into_raw(self) -> RawJson {
        self.0
    }
}

impl From<RawJson> for AnswerValue {
    fn from(value: RawJson) -> Self {
        Self(value)
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
mod tests;
