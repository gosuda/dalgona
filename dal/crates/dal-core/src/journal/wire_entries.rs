use super::wire::{
    AnswerWire, AssistantBlocksWire, AssistantStopWire, JobOutcomeWire, JournalPartsWire,
    OwnerWire, PurposeWire, TsWire, TurnEndStopWire, UsageWire,
};
use super::{
    ApprovalMode, Block, CallId, ClientId, EntryId, Family, FileChange, Gen, JobId, MailMode, Mode,
    Product, RawJson, RequestId, Serialize, SessionId, ThinkingLevel, TurnId, Workspace,
};

pub(super) struct BlockWire<'a>(pub(super) &'a Block);

impl Serialize for BlockWire<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Block::Text { text } => {
                #[derive(Serialize)]
                struct TextMember<'a> {
                    r#type: &'static str,
                    text: &'a str,
                    replay: Option<()>,
                }
                TextMember {
                    r#type: "text",
                    text: text.as_ref(),
                    replay: None,
                }
                .serialize(serializer)
            }
            Block::Reasoning { text, replay } => {
                #[derive(Serialize)]
                struct ReasoningMember<'a> {
                    r#type: &'static str,
                    text: &'a str,
                    replay: &'a RawJson,
                }
                ReasoningMember {
                    r#type: "reasoning",
                    text: text.as_ref(),
                    replay,
                }
                .serialize(serializer)
            }
            Block::ToolCall { id, name, input } => {
                #[derive(Serialize)]
                struct ToolCallMember<'a> {
                    r#type: &'static str,
                    id: &'a CallId,
                    name: &'a str,
                    input: &'a RawJson,
                }
                ToolCallMember {
                    r#type: "tool_call",
                    id,
                    name: name.as_ref(),
                    input,
                }
                .serialize(serializer)
            }
        }
    }
}

// Ordered member structs; serde writes fields in declaration order, so
// each matches the store's format-1 member table exactly.

#[derive(Serialize)]
pub(super) struct SessionWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) id: &'a SessionId,
    pub(super) at: TsWire<'a>,
    pub(super) workspace: &'a Workspace,
    pub(super) product: &'a Product,
    pub(super) from: Option<SourceWire>,
}

/// The `from` member: `null` or `{"session":"<id>","entry":<int or null>}`.
#[derive(Serialize)]
pub(super) struct SourceWire {
    pub(super) session: SessionId,
    pub(super) entry: Option<EntryId>,
}

#[derive(Serialize)]
pub(super) struct BootWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) at: TsWire<'a>,
    pub(super) r#gen: Gen,
    pub(super) version: &'a str,
}

#[derive(Serialize)]
pub(super) struct UserWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) id: EntryId,
    pub(super) parent: Option<EntryId>,
    pub(super) at: TsWire<'a>,
    pub(super) parts: JournalPartsWire<'a>,
}

#[derive(Serialize)]
pub(super) struct AssistantWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) id: EntryId,
    pub(super) parent: Option<EntryId>,
    pub(super) at: TsWire<'a>,
    pub(super) api: Family,
    pub(super) model: &'a str,
    pub(super) content: AssistantBlocksWire<'a>,
    pub(super) usage: UsageWire<'a>,
    pub(super) stop: AssistantStopWire<'a>,
}

#[derive(Serialize)]
pub(super) struct ToolResultWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) id: EntryId,
    pub(super) parent: Option<EntryId>,
    pub(super) at: TsWire<'a>,
    pub(super) call: &'a CallId,
    pub(super) name: &'a str,
    pub(super) error: bool,
    pub(super) parts: JournalPartsWire<'a>,
    pub(super) changes: &'a [FileChange],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) elapsed_ms: Option<u64>,
}

#[derive(Serialize)]
pub(super) struct ReminderWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) id: EntryId,
    pub(super) parent: Option<EntryId>,
    pub(super) at: TsWire<'a>,
    pub(super) source: &'a str,
    pub(super) text: &'a str,
}

#[derive(Serialize)]
pub(super) struct ModelWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) id: EntryId,
    pub(super) parent: Option<EntryId>,
    pub(super) at: TsWire<'a>,
    pub(super) api: Family,
    pub(super) model: &'a str,
}

#[derive(Serialize)]
pub(super) struct ModelRouteWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) id: EntryId,
    pub(super) parent: Option<EntryId>,
    pub(super) at: TsWire<'a>,
    pub(super) route: RouteWire<'a>,
}

/// The journal `route` member of a non-API model record:
/// `{"synthetic":{"id":..}}` or `{"harness":{"id":..}}`. API routes keep
/// the format-1 `api` and `model` members, so no family is invented here.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RouteWire<'a> {
    Synthetic { id: &'a str },
    Harness { id: &'a str },
}

#[derive(Serialize)]
pub(super) struct ThinkingWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) id: EntryId,
    pub(super) parent: Option<EntryId>,
    pub(super) at: TsWire<'a>,
    pub(super) level: ThinkingLevel,
}

#[derive(Serialize)]
pub(super) struct ApprovalWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) id: EntryId,
    pub(super) parent: Option<EntryId>,
    pub(super) at: TsWire<'a>,
    pub(super) mode: ModeWire<'a>,
}

#[derive(Serialize)]
pub(super) struct ModeValueWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) id: EntryId,
    pub(super) parent: Option<EntryId>,
    pub(super) at: TsWire<'a>,
    pub(super) mode: Mode,
}

/// The `mode` member's journal literals: `ask`, `auto-edit`, `yolo`.
///
/// `config::ApprovalMode` spells the same three states as `ask`,
/// `edits`, and `all` for its own surfaces, so the journal lens maps
/// rather than delegates.
pub(super) struct ModeWire<'a>(pub(super) &'a ApprovalMode);

impl Serialize for ModeWire<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(match self.0 {
            ApprovalMode::Ask => "ask",
            ApprovalMode::Edits => "auto-edit",
            ApprovalMode::All => "yolo",
        })
    }
}

#[derive(Serialize)]
pub(super) struct CompactionWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) id: EntryId,
    pub(super) parent: Option<EntryId>,
    pub(super) at: TsWire<'a>,
    pub(super) summary: &'a Option<Box<str>>,
    pub(super) first_kept: Option<EntryId>,
    pub(super) tokens_before: u64,
    pub(super) replay: &'a Option<RawJson>,
    pub(super) usage: Option<UsageWire<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) parts: Option<JournalPartsWire<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) parts_tokens: Option<u64>,
}

#[derive(Serialize)]
pub(super) struct BranchSummaryWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) id: EntryId,
    pub(super) parent: Option<EntryId>,
    pub(super) at: TsWire<'a>,
    pub(super) from: EntryId,
    pub(super) summary: &'a str,
}

#[derive(Serialize)]
pub(super) struct LeafWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) at: TsWire<'a>,
    pub(super) to: &'a Option<EntryId>,
}

#[derive(Serialize)]
pub(super) struct LabelWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) at: TsWire<'a>,
    pub(super) entry: EntryId,
    pub(super) label: &'a Option<Box<str>>,
}

#[derive(Serialize)]
pub(super) struct NameWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) at: TsWire<'a>,
    pub(super) name: &'a Option<Box<str>>,
}

#[derive(Serialize)]
pub(super) struct ArchiveWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) at: TsWire<'a>,
    pub(super) archived: bool,
}

#[derive(Serialize)]
pub(super) struct TurnStartWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) at: TsWire<'a>,
    pub(super) turn: TurnId,
}

#[derive(Serialize)]
pub(super) struct ToolStartWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) at: TsWire<'a>,
    pub(super) turn: TurnId,
    pub(super) call: &'a CallId,
}

#[derive(Serialize)]
pub(super) struct TurnEndWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) at: TsWire<'a>,
    pub(super) turn: TurnId,
    pub(super) stop: TurnEndStopWire<'a>,
    pub(super) usage: Option<UsageWire<'a>>,
    pub(super) changes: &'a [FileChange],
}

#[derive(Serialize)]
pub(super) struct RuleFiredWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) at: TsWire<'a>,
    pub(super) turn: TurnId,
    pub(super) rule: &'a str,
    pub(super) entry: EntryId,
}

#[derive(Serialize)]
pub(super) struct ResolvedWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) at: TsWire<'a>,
    pub(super) request: &'a RequestId,
    pub(super) answer: AnswerWire<'a>,
    pub(super) by: &'a ClientId,
    #[serde(skip_serializing_if = "is_false")]
    pub(super) was_default: bool,
}

/// The `was_default` member appears only when it is true.
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde skip_serializing_if predicates take references"
)]
pub(super) fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Serialize)]
pub(super) struct AllowAlwaysWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) at: TsWire<'a>,
    pub(super) tool: &'a str,
    pub(super) by: &'a ClientId,
}

#[derive(Serialize)]
pub(super) struct GrantGivenWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) at: TsWire<'a>,
    pub(super) ext: &'a str,
    pub(super) set: &'a [Box<str>],
    pub(super) scope: &'a str,
    pub(super) by: &'a ClientId,
}

#[derive(Serialize)]
pub(super) struct ScopedGrantWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) at: TsWire<'a>,
    pub(super) call: &'a CallId,
    pub(super) prefix: &'a [Box<str>],
    pub(super) roots: &'a [Box<str>],
    pub(super) job: &'a JobId,
    pub(super) by: &'a ClientId,
}

#[derive(Serialize)]
pub(super) struct ScopedGrantEndedWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) at: TsWire<'a>,
    pub(super) job: &'a JobId,
}

#[derive(Serialize)]
pub(super) struct BeforeRequestMutWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) at: TsWire<'a>,
    pub(super) turn: TurnId,
    pub(super) ext: &'a str,
    pub(super) field: &'a str,
    pub(super) old: &'a str,
    pub(super) new: &'a str,
}

#[derive(Serialize)]
pub(super) struct ToolPromotedWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) at: TsWire<'a>,
    pub(super) tool: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) turn: Option<TurnId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) leaf: Option<EntryId>,
}

#[derive(Serialize)]
pub(super) struct WakeAttemptWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) at: TsWire<'a>,
    pub(super) turn: TurnId,
    pub(super) count: u32,
    #[serde(skip_serializing_if = "<[JobId]>::is_empty")]
    pub(super) jobs: &'a [JobId],
}

#[derive(Serialize)]
pub(super) struct JobWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) at: TsWire<'a>,
    pub(super) job: &'a JobId,
    pub(super) event: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) kind: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) outcome: Option<JobOutcomeWire<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) by: Option<&'a ClientId>,
}

#[derive(Serialize)]
pub(super) struct ExtWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) at: TsWire<'a>,
    pub(super) ext: &'a str,
    pub(super) kind: &'a str,
    pub(super) body: &'a RawJson,
}

#[derive(Serialize)]
pub(super) struct MailRecordWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) at: TsWire<'a>,
    pub(super) from: &'a SessionId,
    pub(super) to: &'a SessionId,
    pub(super) mode: MailMode,
    pub(super) text: &'a str,
    pub(super) reply_to: &'a Option<Box<str>>,
}

#[derive(Serialize)]
pub(super) struct InferredWire<'a> {
    pub(super) r#type: &'static str,
    pub(super) at: TsWire<'a>,
    pub(super) who: OwnerWire<'a>,
    pub(super) purpose: PurposeWire<'a>,
    pub(super) usage: UsageWire<'a>,
}
