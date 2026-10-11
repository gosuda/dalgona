use super::helpers::entry_token_weight;
use super::types::{ManualCompletion, QueuedInput, Tree, TurnFlags, TurnTotals};
use super::{
    ApprovalMode, BTreeSet, CallId, CompactionReason, EntryId, EntryKind, Family, Limits, Mode,
    ModelRoute, NonZeroU64, Part, Phase, Policy, RawJson, RequestParams, Session, Settings,
    ThinkingLevel, TurnId, TurnSource,
};

impl Session {
    /// Returns the number of queued steer cells.
    #[must_use]
    pub fn steers_queued(&self) -> usize {
        self.queued_inputs
            .iter()
            .filter(|item| matches!(item, QueuedInput::Steer(_)))
            .count()
    }

    /// Returns the number of queued follow-up turns.
    #[must_use]
    pub fn follow_ups_queued(&self) -> usize {
        self.queued_inputs
            .iter()
            .filter(|item| matches!(item, QueuedInput::FollowUp { .. }))
            .count()
    }

    /// Whether automatic compaction is enabled and the breaker is closed.
    #[must_use]
    pub fn auto_compaction_on(&self) -> bool {
        self.limits
            .is_some_and(|limits| limits.compact.enabled && limits.compact.compactor_available)
            && !self.breaker_open
    }

    /// Returns the automatic context-compaction trigger when it is viable.
    #[must_use]
    pub fn should_compact(&self) -> Option<CompactionReason> {
        let limits = self.limits?;
        (limits.compact.compactor_available && self.threshold_compaction_due())
            .then_some(CompactionReason::Threshold)
    }

    pub(super) fn threshold_compaction_due(&self) -> bool {
        let Some(limits) = self.limits else {
            return false;
        };
        if !limits.compact.enabled || self.breaker_open || limits.window == 0 {
            return false;
        }
        let current_model = self.active_model.as_ref().or(self.settings.model.as_ref());
        let Some((model, tokens, compactions)) = &self.last_usage else {
            return false;
        };
        if current_model.is_some_and(|current| current != model) || *compactions != self.compactions
        {
            return false;
        }
        if *tokens < limits.compact.min_tokens {
            return false;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "usage and window are u64 counters; the configured trigger fraction is f64"
        )]
        let window = limits.window as f64;
        #[expect(
            clippy::cast_precision_loss,
            reason = "usage and window are u64 counters; the configured trigger fraction is f64"
        )]
        let tokens = *tokens as f64;
        tokens >= window * limits.compact.threshold
    }

    pub(super) fn compaction_cut_point(&self) -> Option<EntryId> {
        let limits = self.limits?;
        self.cut_point(limits.compact.keep_tokens)
    }

    /// Selects the nearest user boundary that retains at least `keep_tokens`.
    #[must_use]
    pub fn cut_point(&self, keep_tokens: u64) -> Option<EntryId> {
        let branch = self.tree.ancestors(self.tree.leaf);
        let mut candidate = branch.len().checked_sub(1)?;
        let mut retained = 0_u64;
        loop {
            let entry = self.tree.entries.get(&branch[candidate])?;
            retained = retained.saturating_add(entry_token_weight(entry));
            if retained >= keep_tokens || candidate == 0 {
                break;
            }
            candidate -= 1;
        }
        let boundary = (0..=candidate)
            .rev()
            .find(|index| {
                self.tree
                    .entries
                    .get(&branch[*index])
                    .is_some_and(|entry| matches!(&entry.kind, EntryKind::User { .. }))
            })
            .unwrap_or(candidate);
        Some(branch[boundary])
    }

    /// Whether the current leaf itself is a compaction boundary.
    #[must_use]
    pub fn compacted_at_leaf(&self) -> bool {
        self.tree
            .leaf
            .and_then(|id| self.tree.entries.get(&id))
            .is_some_and(|entry| matches!(&entry.kind, EntryKind::Compaction { .. }))
    }

    /// Returns same-model tokens since compaction, or an estimate from the
    /// active branch at the shared 3.5-characters-per-token rate; the branch
    /// projection tracks byte lengths, so bytes stand in for characters.
    #[must_use]
    pub fn tokens_since_last_compaction(&self) -> u64 {
        self.last_usage
            .as_ref()
            .filter(|(_, _, compactions)| *compactions == self.compactions)
            .map_or_else(
                || crate::tokens::estimate_tokens(self.projected_bytes),
                |(_, tokens, _)| *tokens,
            )
    }
    pub(super) fn measured_context_tokens(&self) -> Option<u64> {
        let current_model = self.active_model.as_ref().or(self.settings.model.as_ref());
        let (model, tokens, compactions) = self.last_usage.as_ref()?;
        if *compactions != self.compactions || current_model.is_some_and(|current| current != model)
        {
            return None;
        }
        Some(*tokens)
    }

    /// Returns a stable context-cache key for the session and compaction count.
    #[must_use]
    pub fn cache_key(&self) -> Box<str> {
        format!(
            "{}:{}",
            self.id
                .map_or_else(|| "ephemeral".into(), |id| id.to_string()),
            self.compactions
        )
        .into()
    }

    /// Builds the policy snapshot used for one dispatcher round.
    #[must_use]
    pub fn policy(&self, answerer_attached: bool) -> Policy {
        Policy {
            mode: self.settings.approval,
            answerer_attached,
            allow_always: self.allow_always.clone(),
        }
    }

    /// Returns current actor-supplied model/context limits.
    #[must_use]
    pub const fn limits(&self) -> Option<Limits> {
        self.limits
    }

    /// Returns the model route of the live turn, if any.
    #[must_use]
    pub fn active_model(&self) -> Option<&ModelRoute> {
        self.active_model.as_ref()
    }

    /// Returns the route the caller selected with `Command::SetModel`, if
    /// any. This is the requested route for new rounds; `active_model` is
    /// only the route that answered the live stream.
    #[must_use]
    pub fn requested_model(&self) -> Option<&ModelRoute> {
        self.settings.model.as_ref()
    }

    /// Returns the model family of the live turn, if any.
    #[must_use]
    pub fn active_family(&self) -> Option<Family> {
        self.active_family
    }

    /// Returns the current compaction count.
    #[must_use]
    pub const fn compactions(&self) -> u32 {
        self.compactions
    }

    /// Returns the ended jobs whose reports a journaled wake delivered.
    #[must_use]
    pub const fn delivered_jobs(&self) -> &std::collections::HashSet<crate::JobId> {
        &self.delivered_jobs
    }

    /// Returns the durable wake-run counter.
    #[must_use]
    pub const fn wake_run(&self) -> u32 {
        self.wake_run
    }
    /// Returns a rewritten tool argument value queued by a guarding hook.
    #[must_use]
    pub fn argument_override(&self, call: &CallId) -> Option<&RawJson> {
        self.argument_overrides
            .iter()
            .find(|(id, _)| id == call)
            .map(|(_, args)| args)
    }

    /// Returns the focus text of an active manual compaction.
    #[must_use]
    pub fn manual_compaction_focus(&self) -> Option<&str> {
        self.pending_manual_focus.as_deref()
    }
    pub(super) fn pop_steer(&mut self) -> Option<Vec<Part>> {
        let index = self
            .queued_inputs
            .iter()
            .position(|item| matches!(item, QueuedInput::Steer(_)))?;
        let QueuedInput::Steer(parts) = self.queued_inputs.remove(index) else {
            return None;
        };
        Some(parts)
    }

    pub(super) fn pop_follow_up(&mut self) -> Option<(TurnId, TurnSource)> {
        let index = self
            .queued_inputs
            .iter()
            .position(|item| matches!(item, QueuedInput::FollowUp { .. }))?;
        let QueuedInput::FollowUp { turn, source } = self.queued_inputs.remove(index) else {
            return None;
        };
        Some((turn, source))
    }

    pub(super) fn empty() -> Self {
        Self {
            phase: Phase::Idle,
            id: None,
            generation: None,
            next_turn: Some(TurnId::new(NonZeroU64::MIN)),
            next_entry: Some(EntryId::new(NonZeroU64::MIN)),
            last_turn: 0,
            tree: Tree::new(),
            settings: Settings {
                model: None,
                thinking: ThinkingLevel::Off,
                approval: ApprovalMode::Ask,
                name: None,
                mode: Mode::Normal,
            },
            request_params: RequestParams {
                thinking: ThinkingLevel::Off,
                effort: None,
                temperature: None,
                max_output_tokens: None,
            },
            allow_always: BTreeSet::new(),
            promoted: BTreeSet::new(),
            queued_inputs: Vec::new(),
            open_questions: Vec::new(),
            live_jobs: Vec::new(),
            ended_jobs: Vec::new(),
            delivered_jobs: std::collections::HashSet::new(),
            wake_run: 0,
            wake_attempt_turn: None,
            limits: None,
            last_usage: None,
            active_model: None,
            active_family: None,
            projected_bytes: 0,
            compactions: 0,
            turn_flags: TurnFlags::default(),
            turn_totals: TurnTotals::default(),
            auto_failures: 0,
            breaker_open: false,
            pending_compaction: None,
            pending_manual_focus: None,
            argument_overrides: Vec::new(),
            compaction_none_notified: false,
            manual_completion: ManualCompletion::default(),
            ext_rows: Vec::new(),
        }
    }
}
