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
    #[serde(rename = "openRequests")]
    pub open: Vec<crate::request::Request>,
    /// File changes represented in the view.
    #[serde(rename = "fileChanges")]
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
    /// A short preview of the session's latest content.
    pub preview: Box<str>,
    /// The absolute workspace associated with the session.
    pub workspace: Workspace,
    /// The session's latest update time.
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub updated_at: jiff::Timestamp,
    /// The session's creation time.
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub created_at: jiff::Timestamp,
    /// Whether the session is archived.
    pub archived: bool,
    /// The last journal sequence associated with the session.
    pub last_seq: Seq,
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
pub struct Page<T> {
    /// Values in chronological page order.
    pub items: Vec<T>,
    /// The exclusive cursor for the next older page, when one exists.
    pub next_before: Option<EntryId>,
}

/// An entry in the session's visible history.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct EntryView {
    /// The entry's identity.
    #[serde(rename = "entryId")]
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

    use super::{PageReq, PageReqError, SessionInfo};
    use crate::id::{Seq, SessionId};
    use crate::workspace::Workspace;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

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
            created_at: jiff::Timestamp::UNIX_EPOCH,
            archived: false,
            last_seq: Seq::new(NonZeroU64::MIN),
        };
        let encoded = sonic_rs::to_string(&session)?;
        let decoded: SessionInfo = sonic_rs::from_str(&encoded)?;
        assert_eq!(decoded, session);
        Ok(())
    }
}
