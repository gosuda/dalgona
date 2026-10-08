use super::helpers::{
    compact_notice, entry_weight, invalid, parts_to_text, wrong_turn, zero_usage,
};
use super::types::{ManualCompletion, QueuedInput};
use super::{
    CallId, CancelScope, CompactionReason, CompactionSummary, Effect, Emit, Entry, EntryId,
    EntryKind, Expect, JournalPart, ModelRequestPlan, NonZeroU64, Notice, Output, PartialResponse,
    PendingCall, Phase, Record, Rejection, Reply, Session, SettingsView, Step, TreeDelta,
    TurnEndStop, TurnId, TurnSource, TurnStage, UpdateKind,
};

impl Session {
    pub(super) fn cancel(
        &mut self,
        scope: CancelScope,
        partial: Option<PartialResponse>,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        match scope {
            CancelScope::Job(_) => Ok(()),
            CancelScope::Turn(turn) => {
                if matches!(&self.phase, Phase::Opening { turn: active, .. } if *active == turn) {
                    self.phase = Phase::Idle;
                    effects.push(Effect::Reply(Ok(Reply::Done(Output::Nothing))));
                    return Ok(());
                }
                if matches!(&self.phase, Phase::Running { turn: active, .. } if *active == turn) {
                    self.end_turn(turn, TurnEndStop::Cancelled, partial, now, emit, effects)?;
                    effects.push(Effect::Reply(Ok(Reply::Done(Output::Nothing))));
                    return Ok(());
                }
                Err(wrong_turn(Expect::After(turn), self.turn_state()))
            }
        }
    }

    pub(super) fn compaction_settled(
        &mut self,
        turn: Option<TurnId>,
        outcome: Result<CompactionSummary, Box<str>>,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        let phase_matches = match (turn, &self.phase) {
            (None, Phase::Compacting { .. }) => true,
            (
                Some(turn),
                Phase::Running {
                    turn: active,
                    stage: TurnStage::Compacting { .. },
                    ..
                },
            ) => turn == *active,
            _ => false,
        };
        if !phase_matches {
            return Ok(());
        }
        let reason = self
            .pending_compaction
            .ok_or_else(|| invalid("compaction result has no pending request"))?;
        self.pending_compaction = None;
        if turn.is_none() {
            self.manual_completion.result_settled = true;
            self.manual_completion.job_settled = true;
        }
        match outcome {
            Ok(summary) if summary.tokens_after >= summary.tokens_before => {
                self.compaction_not_shrunk(turn, &summary, emit, effects);
                Ok(())
            }
            Ok(summary) => self.compaction_applied(turn, summary, now, emit, effects),
            Err(error) => {
                let Some(turn) = turn else {
                    emit.updates.push(UpdateKind::Notice(Notice {
                        turn: None,
                        kind: "compact.manual_failed".into(),
                        text: format!("Compaction failed: {error}").into(),
                    }));
                    self.finish_manual_compaction_if_ready();
                    effects.push(Effect::Reply(Ok(Reply::Done(super::Output::Nothing))));
                    return Ok(());
                };
                let message: Option<Box<str>> = (reason == CompactionReason::Overflow)
                    .then(|| format!("Context overflow recovery failed: {error}").into());
                emit.updates.push(UpdateKind::Notice(Notice {
                    turn: Some(turn),
                    kind: "compaction_ended".into(),
                    text: error,
                }));
                self.record_auto_failure(Some(turn), emit);
                let Some(message) = message else {
                    self.continue_after_compaction(turn, effects);
                    return Ok(());
                };
                self.end_turn(
                    turn,
                    TurnEndStop::Failed { message },
                    None,
                    now,
                    emit,
                    effects,
                )
            }
        }
    }

    pub(super) fn compaction_not_shrunk(
        &mut self,
        turn: Option<TurnId>,
        summary: &CompactionSummary,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) {
        let text: Box<str> = format!(
            "Compaction rejected: the summary did not shrink the context ({} to {} tokens). The old context is kept.",
            summary.tokens_before, summary.tokens_after,
        )
        .into();
        emit.updates.push(UpdateKind::Notice(Notice {
            turn,
            kind: "compaction_ended".into(),
            text: text.clone(),
        }));
        if let Some(turn) = turn {
            self.record_auto_failure(Some(turn), emit);
            self.continue_after_compaction(turn, effects);
        } else {
            emit.updates.push(UpdateKind::Notice(Notice {
                turn: None,
                kind: "compact.manual_failed".into(),
                text,
            }));
            self.finish_manual_compaction_if_ready();
            effects.push(Effect::Reply(Ok(Reply::Done(super::Output::Nothing))));
        }
    }

    pub(super) fn compaction_applied(
        &mut self,
        turn: Option<TurnId>,
        summary: CompactionSummary,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        let next_compactions = self
            .compactions
            .checked_add(1)
            .ok_or_else(|| invalid("compaction counter space exhausted"))?;
        let first_kept = summary.first_kept.or_else(|| {
            self.limits
                .and_then(|limits| self.cut_point(limits.compact.keep_tokens))
        });
        let success_text = format!(
            "Context compacted by {}: {} to {} tokens.",
            summary.compactor, summary.tokens_before, summary.tokens_after,
        );
        let entry = self.entry(
            now,
            EntryKind::Compaction {
                summary: summary.summary,
                first_kept,
                tokens_before: summary.tokens_before,
                replay: summary.replay,
                usage: summary.usage,
                parts: summary.parts,
                parts_tokens: summary.parts_tokens,
            },
        )?;
        let view = self.tree.append(entry.clone());
        emit.records.push(Record::Compaction(entry));
        for letter in summary.letters {
            let record = Record::Ext {
                at: now,
                ext: letter.ext.as_str().into(),
                kind: letter.kind,
                body: letter.body,
            };
            if let Record::Ext {
                ext, kind, body, ..
            } = &record
            {
                self.fold_ext(ext, kind, body);
            }
            emit.records.push(record);
        }
        if let Some(usage) = summary.usage {
            self.turn_totals.add_usage(usage)?;
        }
        emit.updates.push(UpdateKind::Tree(TreeDelta {
            added: vec![view],
            leaf: self.tree.leaf,
        }));
        emit.updates.push(UpdateKind::Notice(Notice {
            turn,
            kind: "compaction_ended".into(),
            text: success_text.into(),
        }));
        self.compactions = next_compactions;
        self.last_usage = None;
        self.auto_failures = 0;
        if let Some(turn) = turn {
            self.continue_after_compaction(turn, effects);
        } else {
            self.breaker_open = false;
            self.finish_manual_compaction_if_ready();
            effects.push(Effect::Reply(Ok(Reply::Done(super::Output::Nothing))));
        }
        Ok(())
    }

    pub(super) fn finish_manual_compaction_if_ready(&mut self) {
        if !self.manual_completion.job_settled
            || !self.manual_completion.result_settled
            || !matches!(&self.phase, Phase::Compacting { .. })
        {
            return;
        }
        self.pending_manual_focus = None;
        self.manual_completion = ManualCompletion::default();
        self.phase = Phase::Idle;
    }
    pub(super) fn record_auto_failure(&mut self, turn: Option<TurnId>, emit: &mut Emit) {
        if self.auto_failures < 3 {
            self.auto_failures += 1;
        }
        if self.auto_failures == 3 && !self.breaker_open {
            self.breaker_open = true;
            emit.updates.push(compact_notice(turn, "compact.breaker"));
        }
    }
    pub(super) fn continue_after_compaction(&mut self, turn: TurnId, effects: &mut Vec<Effect>) {
        self.phase = Phase::Running {
            turn,
            round: self.current_round(),
            stage: TurnStage::Streaming {
                blocks: Vec::new(),
                usage: zero_usage(),
                calls: Vec::new(),
                suppressed_injects: Vec::new(),
            },
        };
        effects.push(Effect::Infer(self.request_plan(turn)));
    }

    pub(super) fn fail_calls(
        &mut self,
        calls: &[PendingCall],
        render: impl Fn(&str) -> Box<str>,
        now: jiff::Timestamp,
        emit: &mut Emit,
    ) -> Result<(), Rejection> {
        for item in calls {
            self.result_entry(&item.call, &item.name, render(&item.name), true, now, emit)?;
        }
        Ok(())
    }

    pub(super) fn result_entry(
        &mut self,
        call: &CallId,
        name: &str,
        text: Box<str>,
        error: bool,
        now: jiff::Timestamp,
        emit: &mut Emit,
    ) -> Result<(), Rejection> {
        let entry = self.entry(
            now,
            EntryKind::ToolResult {
                call: call.clone(),
                name: name.into(),
                error,
                parts: vec![JournalPart::Text { text }],
                changes: Vec::new(),
            },
        )?;
        let view = self.tree.append(entry.clone());
        if let EntryKind::ToolResult { changes, .. } = &entry.kind {
            self.turn_totals.add_changes(changes)?;
        }
        emit.records.push(Record::ToolResult(entry));
        emit.updates.push(UpdateKind::Tree(TreeDelta {
            added: vec![view],
            leaf: self.tree.leaf,
        }));
        Ok(())
    }

    pub(super) fn discard_queued(&mut self, turn: TurnId, emit: &mut Emit) {
        if self.queued_inputs.is_empty() {
            return;
        }
        let texts = self
            .queued_inputs
            .drain(..)
            .map(|queued| match queued {
                QueuedInput::Steer(parts) => parts_to_text(&parts),
                QueuedInput::FollowUp { source, .. } => match source {
                    TurnSource::Prompt { content, .. }
                    | TurnSource::FollowUp { content, .. }
                    | TurnSource::Wake { content, .. } => parts_to_text(&content),
                },
            })
            .collect::<Vec<_>>();
        emit.updates.push(UpdateKind::Notice(Notice {
            turn: Some(turn),
            kind: "discarded".into(),
            text: texts.join("\n").into(),
        }));
    }

    pub(super) fn request_plan(&self, turn: TurnId) -> ModelRequestPlan {
        ModelRequestPlan {
            turn,
            params: self.request_params.clone(),
        }
    }

    pub(super) fn current_round(&self) -> Step {
        match &self.phase {
            Phase::Running { round, .. } => *round,
            _ => Step(0),
        }
    }
    pub(super) fn projected_bytes_on_branch(&self) -> u64 {
        let branch = self.tree.ancestors(self.tree.leaf);
        let start = branch
            .iter()
            .rposition(|id| {
                self.tree
                    .entries
                    .get(id)
                    .is_some_and(|entry| matches!(&entry.kind, EntryKind::Compaction { .. }))
            })
            .unwrap_or(0);
        branch
            .iter()
            .skip(start)
            .filter_map(|id| self.tree.entries.get(id))
            .map(entry_weight)
            .fold(0_u64, u64::saturating_add)
    }

    pub(super) fn settings_view(&self) -> SettingsView {
        SettingsView {
            model: self.settings.model.clone(),
            thinking: self.settings.thinking,
            approval: self.settings.approval,
            mode: self.settings.mode,
            name: self.settings.name.clone(),
        }
    }

    pub(super) fn allocate_turn(&mut self) -> Result<TurnId, Rejection> {
        let turn = self
            .next_turn
            .ok_or_else(|| invalid("turn id space exhausted"))?;
        self.next_turn = turn
            .get()
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .map(TurnId::new);
        Ok(turn)
    }

    pub(super) fn allocate_entry_id(&mut self) -> Result<EntryId, Rejection> {
        let entry = self
            .next_entry
            .ok_or_else(|| invalid("entry id space exhausted"))?;
        self.next_entry = entry
            .get()
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .map(EntryId::new);
        Ok(entry)
    }

    pub(super) fn entry(
        &mut self,
        at: jiff::Timestamp,
        kind: EntryKind,
    ) -> Result<Entry, Rejection> {
        let id = self.allocate_entry_id()?;
        Ok(self.entry_at(id, at, kind))
    }

    pub(super) fn entry_at(&mut self, id: EntryId, at: jiff::Timestamp, kind: EntryKind) -> Entry {
        let entry = Entry {
            id,
            parent: self.tree.leaf,
            at,
            kind,
        };
        self.projected_bytes = self.projected_bytes.saturating_add(entry_weight(&entry));
        entry
    }

    pub(super) fn replay_setting(&mut self, entry: &Entry) {
        match &entry.kind {
            EntryKind::Model { route } => self.settings.model = Some(route.clone()),
            EntryKind::Thinking { level } => {
                self.settings.thinking = *level;
                self.request_params.thinking = *level;
            }
            EntryKind::Approval { mode } => self.settings.approval = *mode,
            EntryKind::Mode { mode } => self.settings.mode = *mode,
            _ => {}
        }
    }
}
