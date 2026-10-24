//! Actor-owned materialized view projection.
//!
//! Every published update folds into this snapshot, so `view` serves paged
//! leaf entries, the branch outline, settings, usage, and changes without
//! touching the fold or the journal.

use std::collections::{BTreeMap, HashMap};

use dal_core::{
    ApprovalMode, AutoCompaction, EntryId, EntryKind, EntryView, ExtStatus, FileChange, Gen,
    JournalPart, Mode, ModelRoute, Page, PageReq, Request, Seq, SessionId, SessionInfo,
    SettingsView, Stats, ThinkingLevel, Timestamp, TreeBranch, TreeDelta, TreeOutline, TurnState,
    UpdateKind, Usage, UsageView, View, Workspace,
};

/// Maximum branches retained in the outline, nearest the leaf first.
const OUTLINE_CAP: usize = 256;
/// Maximum entry bytes per view page.
const PAGE_BYTE_CAP: usize = 262_144;
/// Session-list preview length in bytes.
const PREVIEW_LEN: usize = 200;

/// Arguments the agent supplies around the stored projection.
pub(crate) struct SnapshotArgs {
    /// The session generation.
    pub(crate) generation: Gen,
    /// The session identity.
    pub(crate) id: SessionId,
    /// The session workspace.
    pub(crate) workspace: Workspace,
    /// Requests awaiting a client answer, oldest first.
    pub(crate) open: Vec<Request>,
    /// Snapshot time.
    pub(crate) updated_at: Timestamp,
    /// Creation time, when known.
    pub(crate) created_at: Option<Timestamp>,
    /// Archived flag, when known.
    pub(crate) archived: Option<bool>,
    /// The requested entry page.
    pub(crate) page: PageReq,
}

/// Materialized client-facing session state.
pub(crate) struct Projection {
    entries: HashMap<EntryId, EntryView>,
    leaf: Option<EntryId>,
    branches: Vec<TreeBranch>,
    settings: SettingsView,
    /// The host default model captured at open; the session's effective
    /// route while no `/model` selection has folded. Kept out of
    /// `settings.model`, which carries explicit selections only.
    open_time_model: Option<ModelRoute>,
    usage: UsageView,
    changes: Vec<FileChange>,
    preview: Box<str>,
    turn: TurnState,
    /// The last terminal stop the turn driver published; `TurnState::Idle`
    /// alone cannot tell `end_turn` from `cancelled` (durable turn outcome).
    pub(crate) last_stop: Option<dal_core::Stop>,
    last_seq: Option<Seq>,
    steers_queued: u32,
    follow_ups_queued: u32,
    retries: u32,
    dropped_observations: u64,
    auto_compaction: bool,
    ext_statuses: BTreeMap<Box<str>, ExtStatus>,
}

impl Projection {
    /// Seeds settings from the host configuration captured at open; updates
    /// refine it. The model argument is the open-time host default, held
    /// apart from `settings.model`: that field means an explicit session
    /// selection and stays `None` until a `/model` entry folds.
    pub(crate) fn seed(
        model: Option<ModelRoute>,
        thinking: ThinkingLevel,
        approval: ApprovalMode,
        mode: Mode,
    ) -> Self {
        Self {
            entries: HashMap::new(),
            leaf: None,
            branches: Vec::new(),
            open_time_model: model,
            settings: SettingsView {
                model: None,
                thinking,
                approval,
                mode,
                name: None,
                scoped_models: Vec::new(),
            },
            usage: UsageView {
                usage: Usage {
                    input_tokens: 0,
                    cached_input_tokens: 0,
                    output_tokens: 0,
                    reasoning_tokens: None,
                    cache_write_tokens: 0,
                    cost_usd: None,
                    premium_requests: None,
                },
                context_tokens: 0,
                context_window: 0,
            },
            changes: Vec::new(),
            preview: Box::default(),
            turn: TurnState::Idle,
            last_stop: None,
            last_seq: None,
            steers_queued: 0,
            follow_ups_queued: 0,
            retries: 0,
            dropped_observations: 0,
            auto_compaction: false,
            ext_statuses: BTreeMap::new(),
        }
    }

    /// Borrows the current status of every extension that is not in the
    /// quiet, textless baseline state.
    pub(crate) fn ext_statuses(&self) -> &BTreeMap<Box<str>, ExtStatus> {
        &self.ext_statuses
    }

    /// The approval mode the session runs under now.
    pub(crate) fn approval(&self) -> ApprovalMode {
        self.settings.approval
    }

    /// Folds one published update kind into the snapshot.
    pub(crate) fn apply(&mut self, kind: &UpdateKind) {
        match kind {
            UpdateKind::Tree(TreeDelta { added, leaf }) => {
                for view in added {
                    if matches!(view.kind, EntryKind::User { .. }) {
                        self.preview = user_preview(&view.kind);
                    }
                    if let EntryKind::ToolResult { changes, .. } = &view.kind {
                        self.changes.extend(changes.iter().cloned());
                    }
                    self.entries.insert(view.id, view.clone());
                }
                if *leaf != self.leaf {
                    self.leaf = *leaf;
                    if let Some(leaf) = leaf {
                        self.branches.retain(|branch| branch.leaf != *leaf);
                        self.branches.insert(
                            0,
                            TreeBranch {
                                leaf: *leaf,
                                label: None,
                                preview: self.preview.clone(),
                            },
                        );
                        self.branches.truncate(OUTLINE_CAP);
                    }
                }
            }
            UpdateKind::Settings(settings) => {
                self.settings = settings.clone();
            }
            UpdateKind::Usage(usage) => {
                self.usage = usage.clone();
            }
            UpdateKind::ExtStatus(status) => {
                if super::status::is_baseline(status) {
                    self.ext_statuses.remove(&status.ext);
                } else {
                    self.ext_statuses.insert(status.ext.clone(), status.clone());
                }
            }
            UpdateKind::TurnStarted { turn, .. } => {
                self.turn = TurnState::Running { turn: *turn };
            }
            UpdateKind::TurnEnded { turn, stop } => {
                self.last_stop = Some(*stop);
                if matches!(self.turn, TurnState::Running { turn: held } if held == *turn) {
                    self.turn = TurnState::Idle;
                }
            }
            UpdateKind::JobSettled { job } => {
                if matches!(self.turn, TurnState::Compacting { job: held } if held == *job) {
                    self.turn = TurnState::Idle;
                }
            }
            _ => {}
        }
    }

    /// Marks a manual compaction job as the active turn state.
    pub(crate) fn set_compacting(&mut self, job: dal_core::JobId) {
        self.turn = TurnState::Compacting { job };
    }

    /// Records the latest published sequence.
    pub(crate) fn note_seq(&mut self, seq: Seq) {
        self.last_seq = Some(seq);
    }

    /// Refreshes fold-owned counters after a step.
    pub(crate) fn set_fold_stats(&mut self, steers: u32, follow_ups: u32, auto: bool) {
        self.steers_queued = steers;
        self.follow_ups_queued = follow_ups;
        self.auto_compaction = auto;
    }

    /// The effective settings for publication: the projection's current
    /// settings, returned unchanged.
    pub(crate) fn effective_settings(&self) -> SettingsView {
        self.settings.clone()
    }

    /// The route the session runs under: the folded explicit selection,
    /// else the open-time host default. Views keep reporting the
    /// selection only; turn code wants the route.
    pub(crate) fn effective_model(&self) -> Option<ModelRoute> {
        self.settings
            .model
            .clone()
            .or_else(|| self.open_time_model.clone())
    }

    /// Builds the client-facing snapshot with leaf paging applied.
    pub(crate) fn snapshot(&self, args: SnapshotArgs) -> View {
        let (entries, next_before) = self.page(&args);
        let seq = self.last_seq.unwrap_or(Seq::new(std::num::NonZeroU64::MIN));
        View {
            r#gen: args.generation,
            seq,
            session: SessionInfo {
                id: args.id,
                name: self.settings.name.clone(),
                preview: self.preview.clone(),
                workspace: args.workspace,
                updated_at: args.updated_at,
                created_at: args.created_at,
                archived: args.archived,
                last_seq: self.last_seq,
                message_count: None,
            },
            turn: self.turn,
            entries: Page {
                items: entries,
                next_before,
            },
            tree: TreeOutline {
                branches: self.branches.clone(),
            },
            settings: self.settings.clone(),
            open: args.open,
            changes: self.changes.clone(),
            usage: self.usage.clone(),
            stats: Stats {
                steers_queued: self.steers_queued,
                follow_ups_queued: self.follow_ups_queued,
                retries: self.retries,
                dropped_observations: self.dropped_observations,
                auto_compaction: if self.auto_compaction {
                    AutoCompaction::On
                } else {
                    AutoCompaction::Off
                },
            },
        }
    }

    pub(crate) fn leaf_entries(&self) -> Vec<EntryView> {
        self.leaf_path().into_iter().cloned().collect()
    }

    pub(crate) fn restore_fold(&mut self, fold: &dal_core::Session) {
        self.preview = Box::default();
        self.changes.clear();
        self.leaf = fold.leaf_entry();
        let mut usage = None;
        for entry in fold.leaf_entries() {
            if matches!(entry.kind, EntryKind::User { .. }) {
                self.preview = user_preview(&entry.kind);
            }
            if let EntryKind::ToolResult { changes, .. } = &entry.kind {
                self.changes.extend(changes.iter().cloned());
            }
            if let EntryKind::Assistant {
                usage: reported, ..
            } = &entry.kind
            {
                usage = Some(*reported);
            }
            self.entries.insert(
                entry.id,
                EntryView {
                    id: entry.id,
                    parent: entry.parent,
                    kind: entry.kind.clone(),
                },
            );
        }
        self.settings = fold.settings_view();
        self.usage.usage = usage.unwrap_or(Usage {
            input_tokens: 0,
            cached_input_tokens: 0,
            output_tokens: 0,
            reasoning_tokens: None,
            cache_write_tokens: 0,
            cost_usd: None,
            premium_requests: None,
        });
        self.usage.context_tokens = fold.tokens_since_last_compaction();
        self.usage.context_window = fold.limits().map_or(0, |limits| limits.window);
        if let Some(leaf) = self.leaf {
            self.branches.retain(|branch| branch.leaf != leaf);
            self.branches.insert(
                0,
                TreeBranch {
                    leaf,
                    label: fold.branch_label(leaf).map(Into::into),
                    preview: self.preview.clone(),
                },
            );
            self.branches.truncate(OUTLINE_CAP);
        }
        self.sync_turn(fold);
    }

    /// Applies linear appends incrementally; a leaf jump hydrates its path
    /// from the owner, including branches never materialized since reopen.
    pub(crate) fn apply_fold(&mut self, kind: &UpdateKind, fold: &dal_core::Session) {
        let hydrate = if let UpdateKind::Tree(TreeDelta { added, leaf }) = kind {
            let mut cursor = self.leaf;
            let mut linear = true;
            for entry in added {
                linear &= entry.parent == cursor;
                cursor = Some(entry.id);
            }
            !linear || cursor != *leaf
        } else {
            false
        };
        self.apply(kind);
        if hydrate {
            self.restore_fold(fold);
        }
        if let UpdateKind::Tree(_) = kind
            && let Some(leaf) = self.leaf
            && let Some(branch) = self.branches.iter_mut().find(|branch| branch.leaf == leaf)
        {
            branch.label = fold.branch_label(leaf).map(Into::into);
        }
        self.sync_turn(fold);
    }

    /// Opening hooks own the admission lane even before `TurnStarted` exists.
    pub(crate) fn sync_turn(&mut self, fold: &dal_core::Session) {
        self.turn = match fold.phase() {
            dal_core::Phase::Opening { turn, .. } | dal_core::Phase::Settling { turn, .. } => {
                TurnState::Settling { turn: *turn }
            }
            dal_core::Phase::Running { turn, .. } => TurnState::Running { turn: *turn },
            dal_core::Phase::Compacting { job: Some(job) } => TurnState::Compacting { job: *job },
            _ => TurnState::Idle,
        };
    }

    fn leaf_path(&self) -> Vec<&EntryView> {
        let mut path: Vec<&EntryView> = Vec::new();
        let mut cursor = self.leaf;
        while let Some(id) = cursor {
            let Some(entry) = self.entries.get(&id) else {
                break;
            };
            cursor = entry.parent;
            path.push(entry);
        }
        path.reverse();
        path
    }

    fn page(&self, args: &SnapshotArgs) -> (Vec<EntryView>, Option<EntryId>) {
        let mut path = self.leaf_path();
        if let Some(before) = args.page.before {
            if let Some(pos) = path.iter().position(|entry| entry.id == before) {
                path.truncate(pos);
            } else {
                path.clear();
            }
        }
        let limit = args.page.limit.get() as usize;
        let start = path.len().saturating_sub(limit);
        let window = &path[start..];
        let mut kept: Vec<EntryView> = Vec::new();
        let mut bytes = 0usize;
        for entry in window.iter().rev() {
            let size = entry_bytes(entry);
            if !kept.is_empty() && bytes.saturating_add(size) > PAGE_BYTE_CAP {
                break;
            }
            bytes = bytes.saturating_add(size);
            kept.push((*entry).clone());
        }
        let byte_limited = kept.len() < window.len();
        kept.reverse();
        let next_before = if start > 0 || byte_limited {
            kept.first().map(|entry| entry.id)
        } else {
            None
        };
        (kept, next_before)
    }
}

/// First 200 bytes of user text, cut on a character boundary.
fn user_preview(kind: &EntryKind) -> Box<str> {
    let EntryKind::User { parts } = kind else {
        return Box::default();
    };
    let mut text = String::new();
    for part in parts {
        if let JournalPart::Text { text: chunk } = part {
            text.push_str(chunk);
        }
    }
    let mut end = PREVIEW_LEN.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].into()
}

/// Heuristic serialized size of one entry for page byte budgets.
fn entry_bytes(entry: &EntryView) -> usize {
    const OVERHEAD: usize = 64;
    let payload = match &entry.kind {
        EntryKind::User { parts } | EntryKind::ToolResult { parts, .. } => {
            parts.iter().map(part_bytes).sum::<usize>()
        }
        EntryKind::Assistant {
            model,
            content,
            replay,
            ..
        } => {
            model.len()
                + content.iter().map(block_bytes).sum::<usize>()
                + replay.as_ref().map_or(0, |json| json.as_str().len())
        }
        EntryKind::Reminder { source, text } => source.len() + text.len(),
        EntryKind::Model { route } => match route {
            dal_core::ModelRoute::Api {
                model, provider, ..
            } => model.len() + provider.as_deref().map_or(0, str::len) + 16,
            dal_core::ModelRoute::Synthetic { id } | dal_core::ModelRoute::Harness { id } => {
                id.len()
            }
        },
        EntryKind::Thinking { .. } | EntryKind::Approval { .. } | EntryKind::Mode { .. } => 16,
        EntryKind::Compaction {
            summary,
            replay,
            parts,
            ..
        } => {
            summary.as_ref().map_or(0, |text| text.len())
                + replay.as_ref().map_or(0, |json| json.as_str().len())
                + parts.iter().map(part_bytes).sum::<usize>()
        }
        EntryKind::BranchSummary { summary, .. } => summary.len(),
    };
    OVERHEAD + payload
}

/// Heuristic serialized size of one journal part.
fn part_bytes(part: &JournalPart) -> usize {
    match part {
        JournalPart::Text { text } => text.len(),
        JournalPart::TextBlob { bytes, .. } | JournalPart::ImageBlob { bytes, .. } => {
            (*bytes).try_into().unwrap_or(usize::MAX)
        }
        JournalPart::Image { base64, .. } => base64.len(),
        JournalPart::Blob { bytes, .. } => (*bytes).try_into().unwrap_or(usize::MAX),
    }
}

/// Heuristic serialized size of one assistant block.
fn block_bytes(block: &dal_core::Block) -> usize {
    let replay_bytes = |replay: &Option<dal_core::RawJson>| {
        replay.as_ref().map_or(0, |replay| replay.as_str().len())
    };
    match block {
        dal_core::Block::Text { text, replay } => text.len() + replay_bytes(replay),
        dal_core::Block::Reasoning { text, replay } => text.len() + replay.as_str().len(),
        dal_core::Block::ToolCall {
            id: _id,
            name,
            input,
            replay,
        } => name.len() + input.as_str().len() + replay_bytes(replay),
    }
}
#[cfg(test)]
mod tests {
    use std::num::{NonZeroU32, NonZeroU64};

    use dal_core::{
        ApprovalMode, AssistantStop, Block, Entry, EntryId, EntryKind, EntryView, Family, Gen,
        JournalPart, Mode, ModelRoute, PageReq, RawJson, Record, Session, SessionId, Settings,
        ThinkingLevel, Timestamp, TreeDelta, UpdateKind, Usage, Workspace,
    };

    use super::{Projection, SnapshotArgs};

    fn entry_id(value: u64) -> EntryId {
        EntryId::new(NonZeroU64::new(value).expect("fixture id is nonzero"))
    }

    #[test]
    fn assistant_entry_replay_counts_against_the_page_byte_budget()
    -> Result<(), Box<dyn std::error::Error>> {
        // Each entry carries one short text block and a large entry-level
        // replay: only the entry-level payload can push two of them past the
        // page byte cap.
        let replay = RawJson::parse(&format!("\"{}\"", "r".repeat(150_000)))?;
        let entries = (1..=2)
            .map(|value| EntryView {
                id: entry_id(value),
                parent: (value > 1).then(|| entry_id(value - 1)),
                kind: EntryKind::Assistant {
                    api: Family::Responses,
                    model: "test-model".into(),
                    content: vec![Block::Text {
                        text: "short".into(),
                        replay: None,
                    }],
                    usage: Usage {
                        input_tokens: 0,
                        cached_input_tokens: 0,
                        output_tokens: 0,
                        reasoning_tokens: None,
                        cache_write_tokens: 0,
                        cost_usd: None,
                        premium_requests: None,
                    },
                    stop: AssistantStop::Done,
                    route: None,
                    replay: Some(replay.clone()),
                },
            })
            .collect::<Vec<_>>();
        let mut projection =
            Projection::seed(None, ThinkingLevel::Medium, ApprovalMode::Ask, Mode::Normal);
        projection.apply(&UpdateKind::Tree(TreeDelta {
            added: entries,
            leaf: Some(entry_id(2)),
        }));
        let root = tempfile::tempdir()?;
        let workspace = Workspace::new(root.path().to_path_buf())?;
        let session = SessionId::parse("018f0f62-3b00-7000-8000-000000000001")
            .expect("fixture session id is valid");
        let limit = NonZeroU32::new(PageReq::MAX_LIMIT).expect("page limit is nonzero");
        let page = |before| {
            projection
                .snapshot(SnapshotArgs {
                    generation: Gen::new(NonZeroU64::MIN),
                    id: session,
                    workspace: workspace.clone(),
                    open: Vec::new(),
                    updated_at: Timestamp::now(),
                    created_at: None,
                    archived: None,
                    page: PageReq { limit, before },
                })
                .entries
        };

        let newest = page(None);
        let newest_ids = newest
            .items
            .iter()
            .map(|entry| entry.id.to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            newest_ids,
            [entry_id(2).to_string()],
            "two 150 kB replays exceed the page byte cap"
        );
        assert_eq!(newest.next_before, Some(entry_id(2)));

        let oldest = page(newest.next_before);
        let oldest_ids = oldest
            .items
            .iter()
            .map(|entry| entry.id.to_string())
            .collect::<Vec<_>>();
        assert_eq!(oldest_ids, [entry_id(1).to_string()]);
        assert_eq!(oldest.next_before, None);
        Ok(())
    }

    #[test]
    fn byte_limited_pages_preserve_every_entry_in_leaf_order()
    -> Result<(), Box<dyn std::error::Error>> {
        let entries = (1..=3)
            .map(|value| EntryView {
                id: entry_id(value),
                parent: (value > 1).then(|| entry_id(value - 1)),
                kind: EntryKind::User {
                    parts: vec![JournalPart::Text {
                        text: "x".repeat(150_000).into_boxed_str(),
                    }],
                },
            })
            .collect::<Vec<_>>();
        let mut projection =
            Projection::seed(None, ThinkingLevel::Medium, ApprovalMode::Ask, Mode::Normal);
        let entry_count = entries.len();
        projection.apply(&UpdateKind::Tree(TreeDelta {
            added: entries,
            leaf: Some(entry_id(3)),
        }));
        let root = tempfile::tempdir()?;
        let workspace = Workspace::new(root.path().to_path_buf())?;
        let session = SessionId::parse("018f0f62-3b00-7000-8000-000000000001")
            .expect("fixture session id is valid");
        let limit = NonZeroU32::new(PageReq::MAX_LIMIT).expect("page limit is nonzero");
        let direct_ids = projection
            .leaf_entries()
            .iter()
            .map(|entry| entry.id.to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            direct_ids,
            [entry_id(1), entry_id(2), entry_id(3)].map(|entry| entry.to_string())
        );

        let mut before = None;
        let mut pages = Vec::new();
        for _ in 0..=entry_count {
            let view = projection.snapshot(SnapshotArgs {
                generation: Gen::new(NonZeroU64::MIN),
                id: session,
                workspace: workspace.clone(),
                open: Vec::new(),
                updated_at: Timestamp::now(),
                created_at: None,
                archived: None,
                page: PageReq { limit, before },
            });
            let next_before = view.entries.next_before;
            assert_ne!(next_before, before, "history pagination must advance");
            pages.push(view.entries.items);
            before = next_before;
            if before.is_none() {
                break;
            }
        }
        assert!(before.is_none(), "history pagination must terminate");

        let ids = pages
            .into_iter()
            .rev()
            .flatten()
            .map(|entry| entry.id.to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            [entry_id(1), entry_id(2), entry_id(3)].map(|entry| entry.to_string())
        );
        Ok(())
    }

    fn journal_entry(id: u64, kind: EntryKind) -> Entry {
        Entry {
            id: entry_id(id),
            parent: (id > 1).then(|| entry_id(id - 1)),
            at: Timestamp::UNIX_EPOCH,
            kind,
        }
    }

    fn user_record() -> Record {
        Record::User(journal_entry(
            1,
            EntryKind::User {
                parts: vec![JournalPart::Text {
                    text: "hello".into(),
                }],
            },
        ))
    }

    #[test]
    fn restore_fold_keeps_open_time_seed_until_explicit_selection() {
        let default = ModelRoute::Api {
            family: Family::Chat,
            model: "default-model".into(),
            provider: Some("acme".into()),
        };
        let seed = Settings {
            model: Some(default.clone()),
            thinking: ThinkingLevel::Medium,
            approval: ApprovalMode::Ask,
            name: None,
            mode: Mode::Normal,
        };
        let restored = |records: Vec<Record>| {
            let (fold, _) = Session::replay_with(seed.clone(), records, Timestamp::now())
                .expect("fixture journal replays");
            // The open-time host default seeds the projection, the way the
            // registry seeds `Shared` at open.
            let mut projection = Projection::seed(
                Some(default.clone()),
                ThinkingLevel::Medium,
                ApprovalMode::Ask,
                Mode::Normal,
            );
            projection.restore_fold(&fold);
            projection
        };

        let inheriting = restored(vec![
            user_record(),
            Record::Thinking(journal_entry(
                2,
                EntryKind::Thinking {
                    level: ThinkingLevel::Low,
                },
            )),
        ]);
        let inheriting_settings = inheriting.effective_settings();
        assert_eq!(
            inheriting_settings.model, None,
            "an inherited default is not a selection"
        );
        assert_eq!(
            inheriting.effective_model(),
            Some(default.clone()),
            "a journal without a Model entry keeps the open-time seed as the route"
        );
        assert_eq!(inheriting_settings.thinking, ThinkingLevel::Low);

        let selecting = restored(vec![
            user_record(),
            Record::Model(journal_entry(
                2,
                EntryKind::Model {
                    route: default.clone(),
                },
            )),
            Record::Approval(journal_entry(
                3,
                EntryKind::Approval {
                    mode: ApprovalMode::All,
                },
            )),
            Record::Mode(journal_entry(
                4,
                EntryKind::Mode {
                    mode: Mode::EvalOnly,
                },
            )),
            Record::Name {
                at: Timestamp::UNIX_EPOCH,
                name: Some("kept".into()),
            },
        ]);
        let selecting_settings = selecting.effective_settings();
        assert_eq!(selecting_settings.model, Some(default.clone()));
        assert_eq!(selecting.effective_model(), Some(default));
        let selecting = selecting_settings;
        assert_eq!(selecting.thinking, ThinkingLevel::Medium);
        assert_eq!(selecting.approval, ApprovalMode::All);
        assert_eq!(selecting.mode, Mode::EvalOnly);
        assert_eq!(selecting.name.as_deref(), Some("kept"));
    }
}
