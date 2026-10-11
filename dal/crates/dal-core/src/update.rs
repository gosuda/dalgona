//! Sequenced updates delivered to session subscribers.

use serde::{Deserialize, Serialize};

use crate::id::{BlobId, CallId, ClientId, Gen, JobId, RequestId, Seq, TurnId};
use crate::model::{Stop, StreamChannel};
use crate::raw::{RawJson, Tagged};
use crate::request::{Answer, Request};
use crate::view::{SettingsView, TreeDelta, UsageView};

/// One sequenced state change emitted by a session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Update {
    /// The session generation containing this update.
    pub r#gen: Gen,
    /// The update's sequence number within its generation.
    pub seq: Seq,
    /// The state change carried by this update.
    pub kind: UpdateKind,
}

/// The kind of state change carried by an [`Update`].
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum UpdateKind {
    /// A turn began for the given cause.
    TurnStarted {
        /// The newly started turn.
        turn: TurnId,
        /// The event that started the turn.
        cause: TurnCause,
    },
    /// Text or tool arguments arrived from the model stream.
    Delta {
        /// The turn producing the streamed content.
        turn: TurnId,
        /// The shared model channel. The seam lists `text`/`reasoning` and
        /// tool arguments, while this serde type tags its channel variants.
        channel: StreamChannel,
        /// Newly streamed content.
        text: Box<str>,
    },
    /// A tool call began.
    ToolStarted {
        /// The call's provider identity.
        #[serde(rename = "callId")]
        call: CallId,
        /// The tool name.
        tool: Box<str>,
        /// The unmodified tool arguments.
        args: RawJson,
    },
    /// A tool call produced progress text.
    ToolProgress {
        /// The progressing call's identity.
        #[serde(rename = "callId")]
        call: CallId,
        /// The latest progress text.
        tail: Box<str>,
    },
    /// A tool call completed.
    ToolSettled {
        /// The completed call's identity.
        #[serde(rename = "callId")]
        call: CallId,
        /// The result projected for clients.
        outcome: ToolOutcomeView,
    },
    /// A client-facing request was opened.
    RequestOpened(
        /// The newly opened request.
        Request,
    ),
    /// A client-facing request was resolved.
    RequestResolved {
        /// The resolved request's identity.
        #[serde(rename = "requestId")]
        id: RequestId,
        /// The answer supplied by the client.
        answer: Answer,
        /// The client that supplied the answer.
        by: ClientId,
    },
    /// A registered rule matched during the turn.
    RuleFired {
        /// The turn in which the rule matched.
        turn: TurnId,
        /// The matched rule's name.
        rule: Box<str>,
    },
    /// A user-visible notice was emitted.
    Notice(
        /// The notice payload.
        Notice,
    ),
    /// A turn ended with the given stop reason.
    TurnEnded {
        /// The completed turn.
        turn: TurnId,
        /// Why the turn ended.
        stop: Stop,
    },
    /// A background job began.
    JobStarted {
        /// The started job's identity.
        job: JobId,
    },
    /// A background job settled.
    JobSettled {
        /// The settled job's identity.
        job: JobId,
    },
    /// Session settings changed.
    Settings(
        /// The current settings projection.
        SettingsView,
    ),
    /// The session tree changed.
    Tree(
        /// The appended entry or active-leaf change.
        TreeDelta,
    ),
    /// Token usage was updated.
    Usage(
        /// The usage projection.
        UsageView,
    ),
    /// An extension's transient status changed; this update is not journaled
    /// and is not restored by session replay.
    ExtStatus(
        /// The extension's current status.
        ExtStatus,
    ),
    /// An unrecognized update kind with no retained payload.
    #[serde(other)]
    Unknown,
}

// Updates decode through the raw tagged carrier: `tool_started` carries
// `RawJson`, `request_opened` and `settings` carry types that read raw
// member text themselves, and none of that survives serde's internally
// tagged content buffering. Unknown tags decode to `Unknown` rather
// than failing, preserving the old `#[serde(other)]` leniency.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TurnStartedFields {
    turn: TurnId,
    cause: TurnCause,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeltaUpdateFields {
    turn: TurnId,
    channel: StreamChannel,
    text: Box<str>,
}

#[derive(Deserialize)]
struct ToolStartedFields {
    #[serde(rename = "callId")]
    call: CallId,
    tool: Box<str>,
    args: RawJson,
}

#[derive(Deserialize)]
struct ToolProgressFields {
    #[serde(rename = "callId")]
    call: CallId,
    tail: Box<str>,
}

#[derive(Deserialize)]
struct ToolSettledFields {
    #[serde(rename = "callId")]
    call: CallId,
    outcome: ToolOutcomeView,
}

#[derive(Deserialize)]
struct RequestResolvedFields {
    #[serde(rename = "requestId")]
    id: RequestId,
    answer: Answer,
    by: ClientId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuleFiredFields {
    turn: TurnId,
    rule: Box<str>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TurnEndedFields {
    turn: TurnId,
    stop: Stop,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExtStatusFields {
    ext: Box<str>,
    state: Box<str>,
    #[serde(default)]
    text: Option<Box<str>>,
}

fn decode_ext_status<E: serde::de::Error>(raw: &str) -> Result<UpdateKind, E> {
    let fields: ExtStatusFields = sonic_rs::from_str(raw).map_err(serde::de::Error::custom)?;
    let state = match fields.state.as_ref() {
        "busy" => ExtState::Busy,
        "quiet" => ExtState::Quiet,
        _ => return Ok(UpdateKind::Unknown),
    };
    Ok(UpdateKind::ExtStatus(ExtStatus {
        ext: fields.ext,
        state,
        text: fields.text,
    }))
}

#[derive(Deserialize)]
struct JobUpdateFields {
    job: JobId,
}

impl<'de> Deserialize<'de> for UpdateKind {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let tagged = Tagged::decode_any(deserializer, "type")?;
        Ok(match tagged.kind() {
            "turn_started" => {
                let fields: TurnStartedFields =
                    sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?;
                Self::TurnStarted {
                    turn: fields.turn,
                    cause: fields.cause,
                }
            }
            "delta" => {
                let fields: DeltaUpdateFields =
                    sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?;
                Self::Delta {
                    turn: fields.turn,
                    channel: fields.channel,
                    text: fields.text,
                }
            }
            "tool_started" => {
                let fields: ToolStartedFields =
                    sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?;
                Self::ToolStarted {
                    call: fields.call,
                    tool: fields.tool,
                    args: fields.args,
                }
            }
            "tool_progress" => {
                let fields: ToolProgressFields =
                    sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?;
                Self::ToolProgress {
                    call: fields.call,
                    tail: fields.tail,
                }
            }
            "tool_settled" => {
                let fields: ToolSettledFields =
                    sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?;
                Self::ToolSettled {
                    call: fields.call,
                    outcome: fields.outcome,
                }
            }
            "request_opened" => Self::RequestOpened(
                sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?,
            ),
            "request_resolved" => {
                let fields: RequestResolvedFields =
                    sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?;
                Self::RequestResolved {
                    id: fields.id,
                    answer: fields.answer,
                    by: fields.by,
                }
            }
            "rule_fired" => {
                let fields: RuleFiredFields =
                    sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?;
                Self::RuleFired {
                    turn: fields.turn,
                    rule: fields.rule,
                }
            }
            "notice" => {
                Self::Notice(sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?)
            }
            "turn_ended" => {
                let fields: TurnEndedFields =
                    sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?;
                Self::TurnEnded {
                    turn: fields.turn,
                    stop: fields.stop,
                }
            }
            "job_started" => {
                let fields: JobUpdateFields =
                    sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?;
                Self::JobStarted { job: fields.job }
            }
            "job_settled" => {
                let fields: JobUpdateFields =
                    sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?;
                Self::JobSettled { job: fields.job }
            }
            "settings" => {
                Self::Settings(sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?)
            }
            "tree" => {
                Self::Tree(sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?)
            }
            "usage" => {
                Self::Usage(sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?)
            }
            "ext_status" => decode_ext_status::<D::Error>(tagged.raw())?,
            _ => Self::Unknown,
        })
    }
}

/// The event that started a turn.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum TurnCause {
    /// A user prompt started the turn.
    User,
    /// A wake command started the turn.
    Wake,
    /// A queued follow-up started the turn.
    FollowUp,
}

/// A completed tool result projected for update subscribers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ToolOutcomeView {
    /// Whether the tool result represents an error.
    pub is_error: bool,
    /// The text result returned by the tool.
    pub text: Box<str>,
    /// Images included with the result.
    pub images: Vec<BlobId>,
    /// Milliseconds the tool ran on a monotonic clock, approval waits
    /// excluded; absent when the call never ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
}

/// Whether an extension's status source is still working.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ExtState {
    /// The extension has work in flight.
    Busy,
    /// The extension has nothing in flight.
    Quiet,
}

/// The current transient status of one extension's status kind.
///
/// Status values are live observations, not durable session state; clients
/// must not expect them to survive restart or appear in a replayed view.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ExtStatus {
    /// The extension owning the status kind.
    pub ext: Box<str>,
    /// Whether the extension is busy or quiet.
    pub state: ExtState,
    /// The status text, when the extension supplies one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<Box<str>>,
}

impl std::fmt::Display for ExtStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.text, self.state) {
            (Some(text), _) => write!(formatter, "{}: {text}", self.ext),
            (None, ExtState::Busy) => write!(formatter, "{}: busy", self.ext),
            (None, ExtState::Quiet) => write!(formatter, "{}: quiet", self.ext),
        }
    }
}

impl ExtStatus {
    /// Reports whether the extension has nothing in flight.
    #[must_use]
    pub const fn is_quiet(&self) -> bool {
        matches!(self.state, ExtState::Quiet)
    }
}

/// A notice emitted to clients.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Notice {
    /// The associated turn, when the notice is turn-scoped.
    pub turn: Option<TurnId>,
    /// The stable notice kind.
    pub kind: Box<str>,
    /// The human-readable notice text.
    pub text: Box<str>,
}

#[cfg(test)]
mod tests {
    use super::{ExtState, TurnCause, Update, UpdateKind};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn ext_status_round_trips_and_ignores_unknown_state() -> TestResult {
        let wire = r#"{"gen":1,"seq":5,"kind":{"type":"ext_status","ext":"focus","state":"busy","text":"indexing","future":1}}"#;
        let update: Update = sonic_rs::from_str(wire)?;
        let UpdateKind::ExtStatus(status) = &update.kind else {
            panic!("expected ext_status, got {:?}", update.kind);
        };
        assert_eq!(status.ext.as_ref(), "focus");
        assert_eq!(status.state, ExtState::Busy);
        assert_eq!(status.text.as_deref(), Some("indexing"));
        assert!(!status.is_quiet());

        let quiet = UpdateKind::ExtStatus(super::ExtStatus {
            ext: "focus".into(),
            state: ExtState::Quiet,
            text: None,
        });
        let text = sonic_rs::to_string(&quiet)?;
        assert_eq!(
            text,
            r#"{"type":"ext_status","ext":"focus","state":"quiet"}"#
        );

        let bad =
            r#"{"gen":1,"seq":6,"kind":{"type":"ext_status","ext":"focus","state":"dozing"}}"#;
        let unknown: Update = sonic_rs::from_str(bad)?;
        assert!(matches!(unknown.kind, UpdateKind::Unknown));
        Ok(())
    }

    #[test]
    fn update_ignores_unknown_fields_and_unknown_kind() -> TestResult {
        let known: Update = sonic_rs::from_str(
            r#"{"gen":1,"seq":1,"kind":{"type":"turn_started","turn":1,"cause":"user","futureKindField":true},"futureUpdateField":{"unused":true}}"#,
        )?;
        assert!(matches!(
            known.kind,
            UpdateKind::TurnStarted {
                turn: _,
                cause: TurnCause::User
            }
        ));

        let unknown: Update = sonic_rs::from_str(
            r#"{"gen":1,"seq":2,"kind":{"type":"future_kind","payload":{"value":7}}}"#,
        )?;
        assert!(matches!(unknown.kind, UpdateKind::Unknown));
        Ok(())
    }

    #[test]
    fn raw_args_and_nested_blocks_survive_tagged_decoding() -> TestResult {
        use crate::journal::EntryKind;
        use crate::view::TreeDelta;

        // `args` keeps its exact bytes; the wire surface relies on raw
        // members that serde's tagged buffering cannot carry.
        let started: Update = sonic_rs::from_str(
            r#"{"gen":1,"seq":3,"kind":{"type":"tool_started","callId":"call_1","tool":"read","args":{"b": 2, "a":1e+02}}}"#,
        )?;
        let UpdateKind::ToolStarted { args, .. } = started.kind else {
            panic!("expected tool_started, got {:?}", started.kind);
        };
        assert_eq!(args.as_str(), r#"{"b": 2, "a":1e+02}"#);

        // A nested EntryView drives EntryKind -> Block -> RawJson three
        // tag seams deep; buffering at any one of them once failed.
        let tree: Update = sonic_rs::from_str(
            r#"{"gen":1,"seq":4,"kind":{"type":"tree","added":[{"id":1,"parent":null,"kind":{"type":"assistant","api":"anthropic","model":"m","content":[{"type":"reasoning","text":"t","replay":{"sig":"abc"}}],"usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null},"stop":{"type":"done"}}}],"leaf":1}}"#,
        )?;
        let UpdateKind::Tree(TreeDelta { added, leaf }) = tree.kind else {
            panic!("expected tree, got {:?}", tree.kind);
        };
        assert_eq!(leaf.map(crate::id::EntryId::get), Some(1));
        let [entry] = added.as_slice() else {
            panic!("expected one added entry");
        };
        let EntryKind::Assistant { content, .. } = &entry.kind else {
            panic!("expected assistant entry");
        };
        let [crate::journal::Block::Reasoning { replay, .. }] = content.as_slice() else {
            panic!("expected one reasoning block");
        };
        assert_eq!(replay.as_str(), r#"{"sig":"abc"}"#);
        Ok(())
    }
}
