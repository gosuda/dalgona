//! Commands, replies, and rejections at the session-control boundary.

use serde::{Deserialize, Serialize, de};

use crate::raw::Tagged;

use crate::config::ApprovalMode;
use crate::content::Part;
use crate::id::{EntryId, JobId, SessionId, TurnId};
use crate::model::{ModelRoute, ThinkingLevel};

/// A command submitted to a session actor.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Start a prompt when the session matches the expected state.
    Prompt {
        /// The session state required before accepting the prompt.
        expect: Expect,
        /// The user content for the prompt.
        content: Vec<Part>,
    },
    /// Add input to the currently running turn.
    Steer {
        /// The turn that receives the input.
        turn: TurnId,
        /// The content to queue for the next request.
        content: Vec<Part>,
    },
    /// Queue input to run after the named turn ends.
    FollowUp {
        /// The turn that must still be running when the follow-up is accepted.
        turn: TurnId,
        /// The content to queue for the next turn.
        content: Vec<Part>,
    },
    /// Cancel a running turn or job.
    Cancel {
        /// The turn or job to cancel.
        scope: CancelScope,
    },
    /// Select the model route for subsequent requests.
    SetModel(
        /// The model route to use.
        ModelRoute,
    ),
    /// Select the reasoning intensity for subsequent requests.
    SetThinking(
        /// The requested reasoning intensity.
        ThinkingLevel,
    ),
    /// Select the approval policy for subsequent tool calls.
    SetApproval(
        /// The approval policy to use.
        ApprovalMode,
    ),
    /// Start context compaction while the session is idle.
    Compact {
        /// Optional focus text for the compaction request.
        focus: Option<Box<str>>,
    },
    /// Move the session's active leaf to an existing entry.
    MoveLeaf(
        /// The entry to make the active leaf.
        EntryId,
    ),
    /// Create a child session rooted at an existing entry.
    Fork(
        /// The entry at which to fork.
        EntryId,
    ),
    /// Clone the current session into a new session.
    Clone,
    /// Set the session's display name.
    Rename(
        /// The new session name.
        Box<str>,
    ),
    /// Run a named extension operation.
    Run {
        /// The registered operation name.
        name: Box<str>,
        /// The operation's argument text.
        args: Box<str>,
    },
}

#[derive(Serialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
enum CommandShape<'a> {
    Prompt {
        expect: &'a Expect,
        #[serde(rename = "parts")]
        content: &'a [Part],
    },
    Steer {
        turn: &'a TurnId,
        #[serde(rename = "parts")]
        content: &'a [Part],
    },
    FollowUp {
        turn: &'a TurnId,
        #[serde(rename = "parts")]
        content: &'a [Part],
    },
    Cancel {
        scope: &'a CancelScope,
    },
    SetModel {
        model: &'a ModelRoute,
    },
    SetThinking {
        level: &'a ThinkingLevel,
    },
    SetApproval {
        mode: &'a ApprovalMode,
    },
    Compact {
        #[serde(rename = "instructions", skip_serializing_if = "Option::is_none")]
        focus: Option<&'a str>,
    },
    MoveLeaf {
        entry: &'a EntryId,
    },
    Fork {
        entry: &'a EntryId,
    },
    Clone,
    Rename {
        name: &'a str,
    },
    Run {
        name: &'a str,
        args: &'a str,
    },
}

// The schema feature reads this mirror enum's serde attributes;
// runtime decode goes through the tagged carrier because `SetModel`
// carries a `ModelRoute`, which needs its raw member text.
#[expect(
    dead_code,
    reason = "schemars reads the variants' serde attributes; nothing constructs them"
)]
#[derive(Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
enum CommandFields {
    Prompt {
        #[serde(default)]
        expect: Expect,
        #[serde(rename = "parts")]
        content: Vec<Part>,
    },
    Steer {
        turn: TurnId,
        #[serde(rename = "parts")]
        content: Vec<Part>,
    },
    FollowUp {
        turn: TurnId,
        #[serde(rename = "parts")]
        content: Vec<Part>,
    },
    Cancel {
        scope: CancelScope,
    },
    SetModel {
        model: ModelRoute,
    },
    SetThinking {
        level: ThinkingLevel,
    },
    SetApproval {
        mode: ApprovalMode,
    },
    Compact {
        #[serde(rename = "instructions")]
        focus: Option<Box<str>>,
    },
    MoveLeaf {
        entry: EntryId,
    },
    Fork {
        entry: EntryId,
    },
    Clone,
    Rename {
        name: Box<str>,
    },
    Run {
        name: Box<str>,
        args: Box<str>,
    },
}

impl Serialize for Command {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let shape = match self {
            Self::Prompt { expect, content } => CommandShape::Prompt { expect, content },
            Self::Steer { turn, content } => CommandShape::Steer { turn, content },
            Self::FollowUp { turn, content } => CommandShape::FollowUp { turn, content },
            Self::Cancel { scope } => CommandShape::Cancel { scope },
            Self::SetModel(route) => CommandShape::SetModel { model: route },
            Self::SetThinking(level) => CommandShape::SetThinking { level },
            Self::SetApproval(mode) => CommandShape::SetApproval { mode },
            Self::Compact { focus } => CommandShape::Compact {
                focus: focus.as_deref(),
            },
            Self::MoveLeaf(entry) => CommandShape::MoveLeaf { entry },
            Self::Fork(entry) => CommandShape::Fork { entry },
            Self::Clone => CommandShape::Clone,
            Self::Rename(name) => CommandShape::Rename { name },
            Self::Run { name, args } => CommandShape::Run { name, args },
        };
        shape.serialize(serializer)
    }
}

// Command decodes through the raw tagged carrier rather than a derived
// internally tagged enum: `SetModel` carries a `ModelRoute`, which reads
// its own raw text, and serde's tagged buffering cannot hand raw bytes
// to a nested member.
#[derive(Deserialize)]
struct PromptCommandFields {
    #[serde(default)]
    expect: Expect,
    #[serde(rename = "parts")]
    content: Vec<Part>,
}

#[derive(Deserialize)]
struct TurnCommandFields {
    turn: TurnId,
    #[serde(rename = "parts")]
    content: Vec<Part>,
}

#[derive(Deserialize)]
struct ScopeCommandFields {
    scope: CancelScope,
}

#[derive(Deserialize)]
struct ModelCommandFields {
    model: ModelRoute,
}

#[derive(Deserialize)]
struct LevelCommandFields {
    level: ThinkingLevel,
}

#[derive(Deserialize)]
struct ModeCommandFields {
    mode: ApprovalMode,
}

#[derive(Deserialize)]
struct CompactCommandFields {
    #[serde(rename = "instructions")]
    focus: Option<Box<str>>,
}

#[derive(Deserialize)]
struct EntryCommandFields {
    entry: EntryId,
}

#[derive(Deserialize)]
struct NameCommandFields {
    name: Box<str>,
}

#[derive(Deserialize)]
struct RunCommandFields {
    name: Box<str>,
    args: Box<str>,
}

impl<'de> Deserialize<'de> for Command {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let tagged = Tagged::decode(
            deserializer,
            "type",
            &[
                "prompt",
                "steer",
                "follow_up",
                "cancel",
                "set_model",
                "set_thinking",
                "set_approval",
                "compact",
                "move_leaf",
                "fork",
                "clone",
                "rename",
                "run",
            ],
        )?;

        match tagged.kind() {
            "prompt" => {
                let wire: PromptCommandFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Prompt {
                    expect: wire.expect,
                    content: wire.content,
                })
            }
            "steer" => {
                let wire: TurnCommandFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Steer {
                    turn: wire.turn,
                    content: wire.content,
                })
            }
            "follow_up" => {
                let wire: TurnCommandFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::FollowUp {
                    turn: wire.turn,
                    content: wire.content,
                })
            }
            "cancel" => {
                let wire: ScopeCommandFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Cancel { scope: wire.scope })
            }
            "set_model" => {
                let wire: ModelCommandFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::SetModel(wire.model))
            }
            "set_thinking" => {
                let wire: LevelCommandFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::SetThinking(wire.level))
            }
            "set_approval" => {
                let wire: ModeCommandFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::SetApproval(wire.mode))
            }
            "compact" => {
                let wire: CompactCommandFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Compact { focus: wire.focus })
            }
            "move_leaf" => {
                let wire: EntryCommandFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::MoveLeaf(wire.entry))
            }
            "fork" => {
                let wire: EntryCommandFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Fork(wire.entry))
            }
            "clone" => Ok(Self::Clone),
            "rename" => {
                let wire: NameCommandFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Rename(wire.name))
            }
            "run" => {
                let wire: RunCommandFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Run {
                    name: wire.name,
                    args: wire.args,
                })
            }
            other => Err(de::Error::custom(format!("unknown command type `{other}`"))),
        }
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for Command {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Command".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        <CommandFields as schemars::JsonSchema>::json_schema(generator)
    }
}

/// The session state expected by a prompt command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Expect {
    /// Require the session to be idle.
    #[default]
    Idle,
    /// Require the session to be after the specified turn.
    After(
        /// The turn whose completion is required.
        TurnId,
    ),
}

/// The running object targeted by a cancellation command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancelScope {
    /// Cancel a running turn.
    Turn(
        /// The running turn to cancel.
        TurnId,
    ),
    /// Cancel a live job.
    Job(
        /// The job to cancel.
        JobId,
    ),
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
enum CancelScopeShape {
    Turn { turn: TurnId },
    Job { job: JobId },
}

impl Serialize for CancelScope {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Turn(turn) => CancelScopeShape::Turn { turn: *turn },
            Self::Job(job) => CancelScopeShape::Job { job: *job },
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CancelScope {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match CancelScopeShape::deserialize(deserializer)? {
            CancelScopeShape::Turn { turn } => Self::Turn(turn),
            CancelScopeShape::Job { job } => Self::Job(job),
        })
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for CancelScope {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "CancelScope".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        <CancelScopeShape as schemars::JsonSchema>::json_schema(generator)
    }
}

/// A successful response to a command.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Reply {
    /// A prompt was accepted and its user entry was appended.
    Accepted {
        /// The newly started turn.
        turn: TurnId,
        /// The user message entry created for the prompt.
        message_id: EntryId,
    },
    /// Input was queued for a running or future turn.
    Queued,
    /// The command completed without a payload.
    Done,
    /// A run handler returned Markdown text.
    Text {
        /// The Markdown response.
        markdown: Box<str>,
    },
    /// A new session was created.
    Session {
        /// The created session's identity.
        session_id: SessionId,
    },
    /// A background job was started.
    Job {
        /// The started job's identity.
        job: JobId,
    },
}

/// A command rejected without changing session state.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum Rejection {
    /// A turn-scoped command targeted a different session state.
    #[error("turn mismatch")]
    WrongTurn {
        /// The state required by the command.
        expected: Expect,
        /// The session's actual turn state.
        actual: crate::view::TurnState,
    },
    /// A command failed a validation or availability check.
    #[error("invalid command: {reason}")]
    Invalid {
        /// The reason the command was rejected.
        reason: Box<str>,
    },
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use super::{CancelScope, Command, Expect};
    use crate::config::ApprovalMode;
    use crate::content::Part;
    use crate::id::{EntryId, JobId, TurnId};
    use crate::model::{ModelRoute, ThinkingLevel};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn command_variants_round_trip_with_wire_fields() -> TestResult {
        let turn = TurnId::new(NonZeroU64::MIN);
        let job = JobId::new_v7();
        let entry = EntryId::new(NonZeroU64::MIN);
        let route = ModelRoute::synthetic("test/model")?;
        let commands = [
            Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text {
                    text: "hello".into(),
                }],
            },
            Command::Prompt {
                expect: Expect::After(turn),
                content: Vec::new(),
            },
            Command::Steer {
                turn,
                content: Vec::new(),
            },
            Command::FollowUp {
                turn,
                content: Vec::new(),
            },
            Command::Cancel {
                scope: CancelScope::Turn(turn),
            },
            Command::Cancel {
                scope: CancelScope::Job(job),
            },
            Command::SetModel(route),
            Command::SetThinking(ThinkingLevel::High),
            Command::SetApproval(ApprovalMode::Edits),
            Command::Compact {
                focus: Some("preserve the API".into()),
            },
            Command::MoveLeaf(entry),
            Command::Fork(entry),
            Command::Clone,
            Command::Rename("renamed".into()),
            Command::Run {
                name: "inspect".into(),
                args: "--status".into(),
            },
        ];

        for command in commands {
            let encoded = sonic_rs::to_string(&command)?;
            let decoded: Command = sonic_rs::from_str(&encoded)?;
            assert_eq!(decoded, command, "round-trip failed for {encoded}");
        }

        assert_eq!(
            sonic_rs::to_string(&Command::SetThinking(ThinkingLevel::High))?,
            r#"{"type":"set_thinking","level":"high"}"#
        );
        assert_eq!(
            sonic_rs::to_string(&Command::Cancel {
                scope: CancelScope::Turn(turn)
            })?,
            r#"{"type":"cancel","scope":{"type":"turn","turn":1}}"#
        );
        Ok(())
    }
}
