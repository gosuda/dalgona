use crate::journal::AssistantRoute;

use super::helpers::entry_weight;
use super::replay::{Replay, contradiction};
use super::types::{ManualCompletion, QuestionRef, QueuedInput, Tree, TurnFlags, TurnTotals};
use super::{
    ApprovalMode, BTreeSet, CallId, CompactionReason, DecodeError, Effect, Emit, EntryId,
    EntryKind, Event, Family, GateSeed, Gen, JobId, JobKind, JobOutcome, Limits, ModelRoute, Name,
    NonZeroU64, Phase, RawJson, Record, Rejection, ReplayError, RequestId, RequestParams, Settings,
    TurnId,
};

/// The complete, replayable state of one session.
#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    pub(super) phase: Phase,
    pub(super) id: Option<crate::id::SessionId>,
    pub(super) generation: Option<Gen>,
    /// The next turn id, or `None` once the turn id space is exhausted.
    pub(super) next_turn: Option<TurnId>,
    /// The next entry id, or `None` once the entry id space is exhausted.
    pub(super) next_entry: Option<EntryId>,
    pub(super) last_turn: u64,
    pub(super) tree: Tree,
    pub(super) settings: Settings,
    pub(super) request_params: RequestParams,
    pub(super) allow_always: BTreeSet<Name>,
    pub(super) promoted: BTreeSet<Name>,
    pub(super) queued_inputs: Vec<QueuedInput>,
    pub(super) open_questions: Vec<(RequestId, QuestionRef)>,
    pub(super) live_jobs: Vec<(JobId, Option<JobKind>)>,
    pub(super) ended_jobs: Vec<(JobId, JobOutcome)>,
    pub(super) delivered_jobs: std::collections::HashSet<JobId>,
    pub(super) wake_run: u32,
    pub(super) wake_attempt_turn: Option<TurnId>,
    pub(super) limits: Option<Limits>,
    pub(super) last_usage: Option<(ModelRoute, u64, u32)>,
    pub(super) active_model: Option<ModelRoute>,
    pub(super) active_family: Option<Family>,
    pub(super) projected_bytes: u64,
    pub(super) rule_facts: Vec<GateSeed>,
    pub(super) compactions: u32,
    pub(super) turn_flags: TurnFlags,
    pub(super) turn_totals: TurnTotals,
    pub(super) auto_failures: u32,
    pub(super) breaker_open: bool,
    pub(super) pending_compaction: Option<CompactionReason>,
    pub(super) pending_manual_focus: Option<Box<str>>,
    pub(super) argument_overrides: Vec<(CallId, RawJson)>,
    pub(super) compaction_none_notified: bool,
    pub(super) manual_completion: ManualCompletion,
    pub(super) ext_rows: Vec<super::ext::ExtRow>,
}

fn response_model(route: Option<AssistantRoute>, model: &str) -> Option<ModelRoute> {
    Some(match route? {
        AssistantRoute::Api(family) => ModelRoute::Api {
            family,
            model: model.into(),
        },
        AssistantRoute::Synthetic => ModelRoute::Synthetic { id: model.into() },
        AssistantRoute::Harness => ModelRoute::Harness { id: model.into() },
    })
}

impl Session {
    /// Reconstructs one deterministic session state and its crash-repair records.
    ///
    /// Empty input creates an ephemeral, unbooted session and returns a boot
    /// record for generation one. Decoding bytes is offered separately by
    /// [`Session::replay_lines`].
    ///
    /// # Errors
    /// Returns an unsupported-version, decode, or journal-contradiction error.
    pub fn replay(
        records: impl IntoIterator<Item = Record>,
        now: jiff::Timestamp,
    ) -> Result<(Self, Vec<Effect>), ReplayError> {
        let mut replay = Replay::new();
        for record in records {
            replay.record(&record)?;
        }
        replay.finish(now)
    }

    /// Reconstructs a session like [`Session::replay`], seeding the settings the
    /// host is configured with before the journal's records fold in.
    ///
    /// A journaled settings record overrides the seed, so explicit per-session
    /// choices survive reopen while unset fields follow the live configuration.
    ///
    /// # Errors
    /// Returns an unsupported-version, decode, or journal-contradiction error.
    pub fn replay_with(
        seed: Settings,
        records: impl IntoIterator<Item = Record>,
        now: jiff::Timestamp,
    ) -> Result<(Self, Vec<Effect>), ReplayError> {
        let mut replay = Replay::new();
        replay.session.request_params.thinking = seed.thinking;
        replay.session.settings = seed;
        for record in records {
            replay.record(&record)?;
        }
        replay.finish(now)
    }

    /// Returns the typed route of the latest assistant on the current branch.
    ///
    /// Compaction does not remove this identity. An assistant with unknown
    /// provenance returns `None`; an older response cannot supply its route.
    #[must_use]
    pub fn last_response_model(&self) -> Option<ModelRoute> {
        let mut cursor = self.tree.leaf;
        while let Some(id) = cursor {
            let entry = self.tree.entries.get(&id)?;
            if let EntryKind::Assistant { route, model, .. } = &entry.kind {
                return response_model(*route, model);
            }
            cursor = entry.parent;
        }
        None
    }

    pub(super) fn restore_branch_state(
        &mut self,
        leaf: Option<EntryId>,
    ) -> Result<(), ReplayError> {
        let branch = self.tree.ancestors(leaf);
        let mut compactions = 0_u32;
        let mut boundary = None;
        for (index, id) in branch.iter().enumerate() {
            if let EntryKind::Compaction { first_kept, .. } = &self.tree.entries[id].kind {
                compactions = compactions
                    .checked_add(1)
                    .ok_or_else(|| contradiction("compaction counter space exhausted"))?;
                boundary = Some((index, *first_kept));
            }
        }
        let projection_start = boundary.map_or(0, |(index, first_kept)| {
            first_kept
                .and_then(|first| branch[..index].iter().position(|id| *id == first))
                .unwrap_or(index)
        });
        self.projected_bytes = branch[projection_start..]
            .iter()
            .map(|id| entry_weight(&self.tree.entries[id]))
            .fold(0_u64, u64::saturating_add);
        let start = boundary.map_or(0, |(index, _)| index + 1);
        self.last_usage = branch
            .iter()
            .skip(start)
            .rev()
            .find_map(|id| match &self.tree.entries.get(id)?.kind {
                EntryKind::Assistant {
                    route,
                    model,
                    usage,
                    ..
                } => Some((route.as_ref(), model, usage)),
                _ => None,
            })
            .and_then(|(route, model, usage)| {
                let model = response_model(route.copied(), model)?;
                Some((model, usage.input_tokens, compactions))
            });
        self.compactions = compactions;
        self.tree.leaf = leaf;
        Ok(())
    }

    pub(super) fn next_generation(&self) -> Result<Gen, ReplayError> {
        self.generation
            .map_or(Some(1), |generation| generation.get().checked_add(1))
            .and_then(NonZeroU64::new)
            .map(Gen::new)
            .ok_or_else(|| contradiction("generation id space exhausted"))
    }

    /// Reconstructs the session exactly as the journal declares it, without
    /// the crash-recovery records [`Session::replay`] synthesizes: an open
    /// turn stays open, started jobs stay started, and the generation stays
    /// the last boot's. Journal inspection uses this so attribution maps to
    /// written records, not synthetic repairs.
    ///
    /// # Errors
    /// Same contract as [`Session::replay`].
    pub fn replay_declared(records: impl IntoIterator<Item = Record>) -> Result<Self, ReplayError> {
        let mut replay = Replay::new();
        for record in records {
            replay.record(&record)?;
        }
        replay.finish_declared()
    }

    /// Decodes lines and replays them, preserving unsupported-version errors.
    ///
    /// # Errors
    /// Returns a decode, unsupported-version, or contradiction error.
    pub fn replay_lines<'a>(
        lines: impl IntoIterator<Item = &'a [u8]>,
        now: jiff::Timestamp,
    ) -> Result<(Self, Vec<Effect>), ReplayError> {
        let mut records = Vec::new();
        for line in lines {
            let decoded = crate::journal::decode(line).map_err(|error| match error {
                DecodeError::UnsupportedVersion { found } => ReplayError::UnknownVersion { found },
                other => ReplayError::Decode(other),
            })?;
            records.push(decoded.record);
        }
        Self::replay(records, now)
    }

    /// Applies one event atomically; rejected events leave session state untouched.
    ///
    /// The caller owns and reuses `out`; no session clone is made per event.
    ///
    /// # Errors
    /// Returns a typed command rejection without applying state or effects.
    pub fn step(
        &mut self,
        event: Event,
        now: jiff::Timestamp,
        out: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        self.preflight(&event)?;
        out.clear();
        let mut emit = Emit::default();
        self.apply(event, now, &mut emit, out)?;
        if !emit.records.is_empty() || !emit.updates.is_empty() {
            out.insert(0, Effect::Emit(emit));
        }
        Ok(())
    }

    /// Borrows the current lifecycle phase.
    #[must_use]
    pub const fn phase(&self) -> &Phase {
        &self.phase
    }

    /// Returns the current tool-approval mode.
    #[must_use]
    pub const fn approval_mode(&self) -> ApprovalMode {
        self.settings.approval
    }

    /// Borrows the session-wide always-allowed tool set.
    #[must_use]
    pub const fn allow_always(&self) -> &BTreeSet<Name> {
        &self.allow_always
    }

    /// Borrows the deferred tools promoted to the model-visible tool list.
    #[must_use]
    pub const fn promoted(&self) -> &BTreeSet<Name> {
        &self.promoted
    }
}

/// An incremental fold over declared journal records.
///
/// Pushes records one at a time so a tool that attributes each record's
/// change keeps one running fold instead of replaying every prefix;
/// `session()` returns the session the pushed records declare, the same
/// answer [`Session::replay_declared`] gives for the same records.
pub struct DeclaredFold {
    replay: Replay,
}

impl Default for DeclaredFold {
    fn default() -> Self {
        Self::new()
    }
}

impl DeclaredFold {
    /// An empty fold.
    #[must_use]
    pub fn new() -> Self {
        Self {
            replay: Replay::new(),
        }
    }

    /// Folds one more declared record.
    ///
    /// # Errors
    /// Same contract as [`Session::replay_declared`].
    pub fn push(&mut self, record: &Record) -> Result<(), ReplayError> {
        self.replay.record(record)
    }

    /// The session the pushed records declare so far.
    ///
    /// # Errors
    /// Same contract as [`Session::replay_declared`].
    pub fn session(&mut self) -> Result<Session, ReplayError> {
        self.replay.declared_tail()?;
        Ok(self.replay.session.clone())
    }
}
