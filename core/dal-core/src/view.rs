//! Session snapshots, entry pages, and lightweight view projections.

use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};

use crate::config::{ApprovalMode, Mode};
use crate::id::{EntryId, Gen, JobId, Seq, SessionId, TurnId};
use crate::journal::EntryKind;
use crate::model::{ModelRoute, ThinkingLevel, Usage};
use crate::workspace::Workspace;

/// A complete client-facing snapshot of session state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct View {
    /// The session generation represented by this snapshot.
    pub r#gen: Gen,
    /// The last update sequence represented by this snapshot.
    pub seq: Seq,
    /// Session identity and metadata.
    pub session: SessionInfo,
    /// The current turn state.
    pub turn: TurnState,
    /// A page of entries on the active history branch.
    pub entries: Page<EntryView>,
    /// The visible branch outline.
    pub tree: TreeOutline,
    /// The current session settings.
    pub settings: SettingsView,
    /// Requests awaiting a client answer.
    pub open: Vec<crate::request::Request>,
    /// File changes represented in the view.
    pub changes: Vec<FileChange>,
    /// The latest token usage projection.
    pub usage: UsageView,
    /// Runtime counters and automatic compaction state.
    pub stats: Stats,
}

/// Identity, workspace, and timestamps for a session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    /// The session's identity.
    pub id: SessionId,
    /// Its optional display name.
    pub name: Option<Box<str>>,
    /// A short preview of the session's first user message.
    pub preview: Box<str>,
    /// The absolute workspace associated with the session.
    pub workspace: Workspace,
    /// The session's latest update time.
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub updated_at: jiff::Timestamp,
    /// The session's creation time, when its header can be read.
    #[serde(default)]
    #[cfg_attr(feature = "schema", schemars(with = "Option<String>"))]
    pub created_at: Option<jiff::Timestamp>,
    /// Whether the session is archived, when its journal can be read.
    #[serde(default)]
    pub archived: Option<bool>,
    /// The last journal sequence, when known by the active session.
    #[serde(default)]
    pub last_seq: Option<Seq>,
}

/// One row of a session listing: identity plus picker counts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    /// The session's identity.
    pub id: SessionId,
    /// Its optional display name.
    pub name: Option<Box<str>>,
    /// The number of messages on the visible leaf path.
    pub message_count: u64,
    /// The session's latest update time.
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub updated_at: jiff::Timestamp,
}

/// The current lifecycle state of a session turn.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "state",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum TurnState {
    /// No turn or compaction job is active.
    Idle,
    /// A turn is generating a response.
    Running {
        /// The active turn's identity.
        turn: TurnId,
    },
    /// A turn is being settled after generation stopped.
    Settling {
        /// The settling turn's identity.
        turn: TurnId,
    },
    /// A session compaction job is running.
    Compacting {
        /// The active compaction job's identity.
        job: JobId,
    },
}

/// A bounded request for one page of session entries.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct PageReq {
    /// The maximum number of entries requested.
    pub limit: NonZeroU32,
    /// The exclusive entry cursor for the preceding page.
    pub before: Option<EntryId>,
}

impl PageReq {
    /// The default page size.
    pub const DEFAULT_LIMIT: u32 = 200;

    /// The largest page size accepted by [`PageReq::new`].
    pub const MAX_LIMIT: u32 = 1_000;

    /// Creates a page request after checking the maximum page size.
    ///
    /// # Errors
    /// Returns [`PageReqError::LimitTooLarge`] when `limit` exceeds
    /// [`PageReq::MAX_LIMIT`].
    pub fn new(limit: NonZeroU32, before: Option<EntryId>) -> Result<Self, PageReqError> {
        if limit.get() > Self::MAX_LIMIT {
            return Err(PageReqError::LimitTooLarge {
                limit: limit.get(),
                max: Self::MAX_LIMIT,
            });
        }
        Ok(Self { limit, before })
    }
}

impl Default for PageReq {
    fn default() -> Self {
        let limit = match NonZeroU32::new(Self::DEFAULT_LIMIT) {
            Some(limit) => limit,
            None => NonZeroU32::MIN,
        };
        Self {
            limit,
            before: None,
        }
    }
}

/// A page size exceeds the maximum accepted by [`PageReq`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PageReqError {
    /// The requested nonzero limit is above the configured maximum.
    #[error("page limit {limit} exceeds maximum {max}")]
    LimitTooLarge {
        /// The rejected page size.
        limit: u32,
        /// The maximum supported page size.
        max: u32,
    },
}

/// A single page of values and the cursor for older values.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Page<T, Cursor = EntryId> {
    /// Values in chronological page order.
    pub items: Vec<T>,
    /// The exclusive cursor for the next older page, when one exists.
    pub next_before: Option<Cursor>,
}

/// An entry in the session's visible history.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct EntryView {
    /// The entry's identity.
    pub id: EntryId,
    /// The parent entry, when this is not a root entry.
    pub parent: Option<EntryId>,
    /// The entry's journal kind.
    pub kind: EntryKind,
}

/// The branch summary shown with the active history.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct TreeOutline {
    /// Branches ordered by distance from the active leaf.
    pub branches: Vec<TreeBranch>,
}

/// A branch endpoint and its display summary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct TreeBranch {
    /// The branch's leaf entry.
    pub leaf: EntryId,
    /// Its optional user-provided label.
    pub label: Option<Box<str>>,
    /// A short preview of the branch.
    pub preview: Box<str>,
}

/// The settings currently applied to a session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    /// The configured model route, when explicitly selected.
    pub model: Option<ModelRoute>,
    /// The selected reasoning intensity.
    pub thinking: ThinkingLevel,
    /// The selected tool approval policy.
    pub approval: ApprovalMode,
    /// The selected execution mode.
    pub mode: Mode,
    /// The optional session display name.
    pub name: Option<Box<str>>,
}

/// The counts of added and removed lines for one changed file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    /// The changed path relative to the workspace.
    pub path: Box<str>,
    /// The number of added lines.
    pub added: u64,
    /// The number of removed lines.
    pub removed: u64,
}

/// Token usage and context capacity for the current session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct UsageView {
    /// Normalized model token usage.
    pub usage: Usage,
    /// The number of tokens currently in context.
    pub context_tokens: u64,
    /// The context window size in tokens.
    pub context_window: u64,
}

/// Runtime counters and automatic compaction status.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    /// The number of queued steering messages.
    pub steers_queued: u32,
    /// The number of queued follow-up turns.
    pub follow_ups_queued: u32,
    /// The number of provider retries so far.
    pub retries: u32,
    /// The number of dropped stream observations.
    pub dropped_observations: u64,
    /// Whether automatic compaction is enabled for the session.
    pub auto_compaction: AutoCompaction,
}

/// Whether automatic context compaction is enabled.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AutoCompaction {
    /// Automatic context compaction is enabled.
    On,
    /// Automatic context compaction is disabled.
    Off,
}

/// Raw filters supplied for listing sessions.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ListQuery {
    /// Optional requested result limit; the store applies its bounds.
    pub limit: Option<u32>,
    /// Optional opaque pagination cursor.
    pub cursor: Option<Box<str>>,
    /// Optional text search query.
    pub search: Option<Box<str>>,
}

/// A session tree delta containing appended entries or a leaf move.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct TreeDelta {
    /// Entries appended to the visible tree.
    pub added: Vec<EntryView>,
    /// The new active leaf when it changed.
    pub leaf: Option<EntryId>,
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU32, NonZeroU64};
    use std::time::Duration;

    use serde::Deserialize;

    use super::{
        AutoCompaction, EntryView, FileChange, Page, PageReq, PageReqError, SessionInfo,
        SettingsView, Stats, TreeOutline, TurnState, UsageView, View,
    };
    use crate::config::{ApprovalMode, Mode};
    use crate::id::{EntryId, Gen, RequestId, Seq, SessionId, TurnId};
    use crate::journal::EntryKind;
    use crate::model::{ThinkingLevel, Usage};
    use crate::request::{Answer, Owner, Question, Request};
    use crate::workspace::Workspace;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ConsumerView {
        open: Vec<Request>,
        changes: Vec<FileChange>,
        entries: ConsumerPage,
        session: ConsumerSession,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ConsumerPage {
        items: Vec<ConsumerEntry>,
        next_before: Option<EntryId>,
    }

    #[derive(Deserialize)]
    struct ConsumerEntry {
        id: EntryId,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ConsumerSession {
        updated_at: jiff::Timestamp,
        created_at: jiff::Timestamp,
        last_seq: Seq,
    }

    #[test]
    fn view_pages_keep_bounds_and_session_identity() -> TestResult {
        assert_eq!(PageReq::default().limit.get(), PageReq::DEFAULT_LIMIT);
        let one = PageReq::new(NonZeroU32::MIN, None)?;
        assert_eq!(one.limit.get(), 1);
        assert_eq!(
            PageReq::new(NonZeroU32::new(1_001).expect("1001 is nonzero"), None),
            Err(PageReqError::LimitTooLarge {
                limit: 1_001,
                max: PageReq::MAX_LIMIT,
            })
        );

        let session = SessionInfo {
            id: SessionId::new_v7(),
            name: Some("test session".into()),
            preview: "latest content".into(),
            workspace: Workspace::new(std::env::temp_dir())?,
            updated_at: jiff::Timestamp::UNIX_EPOCH,
            created_at: Some(jiff::Timestamp::UNIX_EPOCH),
            archived: Some(false),
            last_seq: Some(Seq::new(NonZeroU64::MIN)),
        };
        let encoded = sonic_rs::to_string(&session)?;
        let decoded: SessionInfo = sonic_rs::from_str(&encoded)?;
        assert_eq!(decoded, session);
        Ok(())
    }

    #[test]
    fn view_round_trips_declared_camel_case_fields() -> TestResult {
        let entry_id = EntryId::new(NonZeroU64::new(11).expect("11 is nonzero"));
        let next_before = EntryId::new(NonZeroU64::new(10).expect("10 is nonzero"));
        let turn_id = TurnId::new(NonZeroU64::new(3).expect("3 is nonzero"));
        let open = vec![Request {
            id: RequestId::new_v7(),
            turn: Some(turn_id),
            owner: Owner::Core,
            question: Question::Text {
                prompt: "Continue?".into(),
                placeholder: Some("Answer".into()),
            },
            timeout: Duration::from_secs(30),
            default: Answer::Cancel,
        }];
        let changes = vec![FileChange {
            path: "src/main.rs".into(),
            added: 2,
            removed: 1,
        }];
        let session = SessionInfo {
            id: SessionId::new_v7(),
            name: Some("session".into()),
            preview: "latest content".into(),
            workspace: Workspace::new(std::env::temp_dir())?,
            updated_at: jiff::Timestamp::UNIX_EPOCH,
            created_at: Some(jiff::Timestamp::UNIX_EPOCH),
            archived: Some(false),
            last_seq: Some(Seq::new(NonZeroU64::new(4).expect("4 is nonzero"))),
        };
        let view = View {
            r#gen: Gen::new(NonZeroU64::new(2).expect("2 is nonzero")),
            seq: Seq::new(NonZeroU64::new(4).expect("4 is nonzero")),
            session,
            turn: TurnState::Running { turn: turn_id },
            entries: Page {
                items: vec![EntryView {
                    id: entry_id,
                    parent: None,
                    kind: EntryKind::User { parts: Vec::new() },
                }],
                next_before: Some(next_before),
            },
            tree: TreeOutline {
                branches: Vec::new(),
            },
            settings: SettingsView {
                model: None,
                thinking: ThinkingLevel::Off,
                approval: ApprovalMode::Ask,
                mode: Mode::Normal,
                name: None,
            },
            open,
            changes,
            usage: UsageView {
                usage: Usage {
                    input_tokens: 12,
                    cached_input_tokens: 4,
                    output_tokens: 5,
                    reasoning_tokens: None,
                    cache_write_tokens: 0,
                    cost_usd: None,
                },
                context_tokens: 17,
                context_window: 128_000,
            },
            stats: Stats {
                steers_queued: 0,
                follow_ups_queued: 0,
                retries: 0,
                dropped_observations: 0,
                auto_compaction: AutoCompaction::Off,
            },
        };

        let encoded = sonic_rs::to_string(&view)?;
        let decoded: View = sonic_rs::from_str(&encoded)?;
        assert_eq!(decoded, view);

        let consumer: ConsumerView = sonic_rs::from_str(&encoded)?;
        assert_eq!(consumer.open, view.open);
        assert_eq!(consumer.changes, view.changes);
        assert_eq!(consumer.entries.items[0].id, entry_id);
        assert_eq!(consumer.entries.next_before, Some(next_before));
        assert_eq!(consumer.session.updated_at, jiff::Timestamp::UNIX_EPOCH);
        assert_eq!(consumer.session.created_at, jiff::Timestamp::UNIX_EPOCH);
        assert_eq!(
            consumer.session.last_seq,
            view.session
                .last_seq
                .expect("healthy session has a journal sequence")
        );
        Ok(())
    }

    #[test]
    fn damaged_session_info_serializes_unknown_metadata_as_null() -> TestResult {
        let session = SessionInfo {
            id: SessionId::new_v7(),
            name: None,
            preview: "(damaged session file)".into(),
            workspace: Workspace::new(std::env::temp_dir())?,
            updated_at: jiff::Timestamp::UNIX_EPOCH,
            created_at: None,
            archived: None,
            last_seq: None,
        };
        let encoded = sonic_rs::to_string(&session)?;
        assert!(encoded.contains("\"createdAt\":null"));
        assert!(encoded.contains("\"archived\":null"));
        assert!(encoded.contains("\"lastSeq\":null"));
        let decoded: SessionInfo = sonic_rs::from_str(&encoded)?;
        assert_eq!(decoded, session);
        Ok(())
    }
}
