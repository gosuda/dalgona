use super::{
    Chooser, DenyReason, Deserialize, EntryId, Expect, FrontAction, JobId, Output, Serialize,
    Tagged, TurnId, de,
};

/// A successful response to a command.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
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
    /// The command completed with a payload.
    Done(Output),
    /// The client must show a picker.
    Choose {
        /// The picker to show.
        chooser: Chooser,
        /// The initial filter text.
        filter: Box<str>,
    },
    /// The client must run a front-end action.
    Front(FrontAction),
    /// A background job was started.
    Started(JobId),
}

#[derive(Serialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub(super) enum ReplyShape<'a> {
    Accepted {
        turn: &'a TurnId,
        message_id: &'a EntryId,
    },
    Queued,
    Done {
        output: &'a Output,
    },
    Choose {
        chooser: &'a Chooser,
        filter: &'a str,
    },
    Front {
        action: &'a FrontAction,
    },
    Started {
        job: &'a JobId,
    },
}

impl Serialize for Reply {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let shape = match self {
            Self::Accepted { turn, message_id } => ReplyShape::Accepted { turn, message_id },
            Self::Queued => ReplyShape::Queued,
            Self::Done(output) => ReplyShape::Done { output },
            Self::Choose { chooser, filter } => ReplyShape::Choose { chooser, filter },
            Self::Front(action) => ReplyShape::Front { action },
            Self::Started(job) => ReplyShape::Started { job },
        };
        shape.serialize(serializer)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AcceptedReplyFields {
    pub(super) turn: TurnId,
    pub(super) message_id: EntryId,
}

#[derive(Deserialize)]
pub(super) struct ChooseReplyFields {
    pub(super) chooser: Chooser,
    pub(super) filter: Box<str>,
}

#[derive(Deserialize)]
pub(super) struct DoneReplyFields {
    pub(super) output: Output,
}

#[derive(Deserialize)]
pub(super) struct FrontReplyFields {
    pub(super) action: FrontAction,
}

#[derive(Deserialize)]
pub(super) struct StartedReplyFields {
    pub(super) job: JobId,
}

impl<'de> Deserialize<'de> for Reply {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let tagged = Tagged::decode(
            deserializer,
            "type",
            &["accepted", "queued", "done", "choose", "front", "started"],
        )?;

        match tagged.kind() {
            "accepted" => {
                let wire: AcceptedReplyFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Accepted {
                    turn: wire.turn,
                    message_id: wire.message_id,
                })
            }
            "queued" => Ok(Self::Queued),
            "done" => {
                let wire: DoneReplyFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Done(wire.output))
            }
            "choose" => {
                let wire: ChooseReplyFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Choose {
                    chooser: wire.chooser,
                    filter: wire.filter,
                })
            }
            "front" => {
                let wire: FrontReplyFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Front(wire.action))
            }
            "started" => {
                let wire: StartedReplyFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Started(wire.job))
            }
            other => Err(de::Error::custom(format!("unknown reply type `{other}`"))),
        }
    }
}

// The schema feature reads this mirror enum's serde attributes; the
// runtime codec above keeps the `Accepted` wire shape byte-stable.
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
pub(super) enum ReplyFields {
    Accepted { turn: TurnId, message_id: EntryId },
    Queued,
    Done { output: Output },
    Choose { chooser: Chooser, filter: Box<str> },
    Front { action: FrontAction },
    Started { job: JobId },
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for Reply {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Reply".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        <ReplyFields as schemars::JsonSchema>::json_schema(generator)
    }
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
    /// The session no longer accepts commands.
    #[error("session is closed")]
    SessionClosed,
    /// A command that requires an idle session was submitted during a turn.
    #[error("command requires an idle session; a turn is running")]
    BusyTurn,
    /// A prompt was submitted while compaction was in progress.
    #[error(
        "Cannot submit a prompt while compaction is in progress. Wait for compaction to finish and retry."
    )]
    Compacting,
    /// A steer could not be queued because the bounded queue is full.
    #[error("steer queue is full (16); wait for the next step or cancel.")]
    SteerFull,
    /// A command was denied by its approval or availability policy.
    #[error("command denied: {reason:?}")]
    Denied {
        /// The reason the command was denied.
        reason: DenyReason,
    },
    /// A command failed a validation or availability check.
    #[error("invalid command: {reason}")]
    Invalid {
        /// The reason the command was rejected.
        reason: Box<str>,
    },
}
