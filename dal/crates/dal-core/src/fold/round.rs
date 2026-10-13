use super::helpers::{
    assistant_stop_for_end, compact_notice, compaction_started_notice, invalid, part_to_journal,
    unsettled_calls, zero_usage,
};
use super::replay::TOOL_LOST;
use super::types::{Overflow, TurnFlags};
use super::{
    CallId, Effect, Emit, Entry, EntryKind, Notice, Part, PartialResponse, Phase, Record,
    Rejection, RequestParams, Session, SettledOutcome, Step, Stop, ToolOutcomeView, TreeDelta,
    TurnEndStop, TurnId, TurnSource, TurnStage, UpdateKind,
};

/// One dispatcher result: the call, how it ended, and how long its tool ran.
pub(super) struct Settlement {
    pub(super) call: CallId,
    pub(super) outcome: SettledOutcome,
    pub(super) elapsed_ms: Option<u64>,
}

impl Session {
    pub(super) fn call_started(
        &mut self,
        turn: TurnId,
        call: &CallId,
        now: jiff::Timestamp,
        emit: &mut Emit,
    ) {
        let Phase::Running {
            turn: active,
            stage: TurnStage::Dispatching { pending },
            ..
        } = &mut self.phase
        else {
            return;
        };
        if *active != turn {
            return;
        }
        let Some(item) = pending
            .iter_mut()
            .find(|item| item.call == *call && !item.started)
        else {
            return;
        };
        item.started = true;
        emit.records.push(Record::ToolStart {
            at: now,
            turn,
            call: item.call.clone(),
        });
    }

    pub(super) fn settled(
        &mut self,
        turn: TurnId,
        settlement: Settlement,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        let (round, mut pending) = match &self.phase {
            Phase::Running {
                turn: active,
                round,
                stage: TurnStage::Dispatching { pending },
            } if *active == turn => (*round, pending.clone()),
            _ => return Ok(()),
        };
        let Settlement {
            call,
            outcome,
            elapsed_ms,
        } = settlement;
        let Some(index) = pending.iter().position(|item| item.call == call) else {
            return Ok(());
        };
        let item = pending.remove(index);
        let succeeded = matches!(outcome, SettledOutcome::Ok { .. });
        let (text, is_error) = match outcome {
            SettledOutcome::Ok { text, .. } => (text, false),
            SettledOutcome::Err { text } => (text, true),
            SettledOutcome::Interrupted => ("Tool call interrupted by user.".into(), true),
            SettledOutcome::Detached { job } => (
                format!("still running after the foreground budget; detached as job {job}. Its result arrives as a message when it ends; read job://{job} for output now.").into(),
                false,
            ),
        };
        self.result_entry(&item, text.clone(), is_error, elapsed_ms, now, emit)?;
        if succeeded
            && let Some(tool) = item.promotes
            && !self.promoted.contains(&tool)
        {
            emit.records.push(Record::ToolPromoted {
                at: now,
                tool: tool.to_string().into(),
                turn: Some(turn),
                leaf: self.tree.leaf,
            });
            self.promoted.insert(tool);
        }
        emit.updates.push(UpdateKind::ToolSettled {
            call: item.call,
            outcome: ToolOutcomeView {
                is_error,
                text,
                images: Vec::new(),
                elapsed_ms,
            },
        });
        if pending.is_empty() {
            self.phase = Phase::Running {
                turn,
                round,
                stage: TurnStage::Boundary,
            };
            self.boundary(turn, now, true, emit, effects)?;
        } else {
            self.phase = Phase::Running {
                turn,
                round,
                stage: TurnStage::Dispatching { pending },
            };
        }
        Ok(())
    }

    pub(super) fn boundary(
        &mut self,
        turn: TurnId,
        now: jiff::Timestamp,
        completed_round: bool,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        let round = match &self.phase {
            Phase::Running {
                turn: active,
                round,
                stage: TurnStage::Boundary,
            } if *active == turn => *round,
            _ => return Ok(()),
        };
        let final_response = self.turn_flags.end_after_boundary;
        let steer = self.pop_steer();
        if final_response {
            self.turn_flags.end_after_boundary = false;
            if steer.is_none() {
                return self.end_turn(turn, TurnEndStop::Done, None, now, emit, effects);
            }
        }
        self.journal_boundary_inputs(steer, now, emit)?;
        let completed_round = completed_round && !final_response;
        let next_round = if completed_round {
            let Some(next) = round.0.checked_add(1) else {
                return self.end_turn(
                    turn,
                    TurnEndStop::Failed {
                        message: "tool round counter space exhausted".into(),
                    },
                    None,
                    now,
                    emit,
                    effects,
                );
            };
            Step(next)
        } else {
            round
        };
        if completed_round
            && let Some(limits) = self.limits
            && limits.max_steps > 0
            && next_round.0 >= limits.max_steps
        {
            emit.updates.push(UpdateKind::Notice(Notice {
                turn: Some(turn),
                kind: "max_steps".into(),
                text: format!("Stopped after {} tool rounds: loop.max_steps = {}. Raise loop.max_steps in config to continue.", next_round.0, limits.max_steps).into(),
            }));
            return self.end_turn(turn, TurnEndStop::MaxSteps, None, now, emit, effects);
        }
        self.open_round(turn, next_round, emit, effects);
        Ok(())
    }

    pub(super) fn journal_entry(
        &mut self,
        now: jiff::Timestamp,
        kind: EntryKind,
        record: fn(Entry) -> Record,
        emit: &mut Emit,
    ) -> Result<(), Rejection> {
        let entry = self.entry(now, kind)?;
        let view = self.tree.append(entry.clone());
        emit.records.push(record(entry));
        emit.updates.push(UpdateKind::Tree(TreeDelta {
            added: vec![view],
            leaf: self.tree.leaf,
        }));
        Ok(())
    }

    pub(super) fn journal_boundary_inputs(
        &mut self,
        steer: Option<Vec<Part>>,
        now: jiff::Timestamp,
        emit: &mut Emit,
    ) -> Result<(), Rejection> {
        if let Some(parts) = steer {
            let parts = parts.iter().map(part_to_journal).collect();
            self.journal_entry(now, EntryKind::User { parts }, Record::User, emit)?;
        }
        for inject in std::mem::take(&mut self.turn_flags.pending_suppressed) {
            let kind = EntryKind::Reminder {
                source: "rule:suppressed".into(),
                text: inject,
            };
            self.journal_entry(now, kind, Record::Reminder, emit)?;
        }
        if !self.ended_jobs.is_empty() {
            let ids = self
                .ended_jobs
                .iter()
                .map(|(job, _)| job.to_string())
                .collect::<Vec<_>>();
            let kind = EntryKind::Reminder {
                source: "jobs.finished".into(),
                text: format!("jobs.finished: {}", ids.join(", ")).into(),
            };
            self.journal_entry(now, kind, Record::Reminder, emit)?;
            self.ended_jobs.clear();
        }
        Ok(())
    }

    pub(super) fn open_round(
        &mut self,
        turn: TurnId,
        round: Step,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) {
        let compaction = self.should_compact();
        let compactor_missing = compaction.is_none() && self.threshold_compaction_due();
        self.active_model = None;
        self.active_family = None;
        if let Some(reason) = compaction {
            self.pending_compaction = Some(reason);
            self.phase = Phase::Running {
                turn,
                round,
                stage: TurnStage::Compacting {
                    reason,
                    automatic: true,
                },
            };
            let window = self.limits.map_or(0, |limits| limits.window);
            emit.updates.push(compaction_started_notice(
                Some(turn),
                reason,
                self.measured_context_tokens(),
                window,
            ));
            effects.push(Effect::Compact {
                turn: Some(turn),
                first_kept: self.compaction_cut_point(),
            });
            return;
        }
        if compactor_missing && !self.compaction_none_notified {
            emit.updates
                .push(compact_notice(Some(turn), "compact.none"));
            self.compaction_none_notified = true;
        }
        self.request_params = RequestParams {
            thinking: self.settings.thinking,
            effort: None,
            temperature: None,
            max_output_tokens: None,
        };
        self.phase = Phase::Running {
            turn,
            round,
            stage: TurnStage::Streaming {
                blocks: Vec::new(),
                usage: zero_usage(),
                calls: Vec::new(),
                suppressed_injects: Vec::new(),
            },
        };
        effects.push(Effect::Infer(self.request_plan(turn)));
    }
    pub(super) fn end_turn(
        &mut self,
        turn: TurnId,
        stop: TurnEndStop,
        partial: Option<PartialResponse>,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        // The mark reaches only the in-memory stop effect, never the journal
        // record, and is consumed here whether or not the turn is active.
        let overflowed = self.turn_flags.overflow == Overflow::Unrecovered;
        if overflowed {
            self.turn_flags.overflow = Overflow::Clear;
        }
        let stage = match &self.phase {
            Phase::Running {
                turn: active,
                stage,
                ..
            } if *active == turn => stage.clone(),
            _ => return Ok(()),
        };
        let pending = unsettled_calls(stage, partial.as_ref());
        if let Some(partial) = partial.filter(|partial| !partial.content.is_empty()) {
            self.journal_partial(partial, &stop, now, emit)?;
        }
        let result_text = if matches!(stop, TurnEndStop::Cancelled) {
            "Tool call interrupted by user."
        } else {
            TOOL_LOST
        };
        self.fail_calls(&pending, |_| result_text.into(), now, emit)?;
        let update_stop = match &stop {
            TurnEndStop::Done => Stop::EndTurn,
            TurnEndStop::Length => Stop::Length,
            TurnEndStop::Filter => Stop::Filter,
            TurnEndStop::MaxSteps => Stop::MaxSteps,
            TurnEndStop::Cancelled => Stop::Cancelled,
            TurnEndStop::Aborted | TurnEndStop::Failed { .. } => Stop::Failed,
        };
        let can_continue = matches!(
            &stop,
            TurnEndStop::Done | TurnEndStop::Length | TurnEndStop::Filter | TurnEndStop::MaxSteps
        );
        if let TurnEndStop::Failed { message, .. } = &stop {
            emit.updates.push(UpdateKind::Notice(Notice {
                turn: Some(turn),
                kind: "turn.failed".into(),
                text: message.clone(),
            }));
        }
        if matches!(stop, TurnEndStop::Cancelled) {
            self.cancel_open_questions(turn, now, emit);
        }
        emit.records.push(Record::TurnEnd {
            at: now,
            turn,
            stop,
            usage: self.turn_totals.usage(),
            changes: self.turn_totals.changes(),
        });
        emit.updates.push(UpdateKind::TurnEnded {
            turn,
            stop: update_stop,
        });
        self.last_turn = self.last_turn.max(turn.get());
        let follow_up = if can_continue && self.steers_queued() == 0 {
            self.pop_follow_up()
        } else {
            None
        };
        self.close_turn(turn, follow_up, emit);
        effects.push(Effect::Stop {
            turn,
            stop: update_stop,
            overflowed,
        });
        Ok(())
    }

    pub(super) fn journal_partial(
        &mut self,
        partial: PartialResponse,
        stop: &TurnEndStop,
        now: jiff::Timestamp,
        emit: &mut Emit,
    ) -> Result<(), Rejection> {
        let (Some(model), Some(family)) = (&self.active_model, self.active_family) else {
            return Err(invalid(
                "partial assistant content has no active model route",
            ));
        };
        let assistant = self.entry(
            now,
            EntryKind::Assistant {
                api: family,
                model: model.id().into(),
                content: partial.content,
                usage: partial.usage,
                stop: assistant_stop_for_end(stop),
            },
        )?;
        let view = self.tree.append(assistant.clone());
        emit.records.push(Record::Assistant(assistant));
        self.turn_totals.add_usage(partial.usage)?;
        emit.updates.push(UpdateKind::Tree(TreeDelta {
            added: vec![view],
            leaf: self.tree.leaf,
        }));
        Ok(())
    }

    pub(super) fn close_turn(
        &mut self,
        turn: TurnId,
        follow_up: Option<(TurnId, TurnSource)>,
        emit: &mut Emit,
    ) {
        if follow_up.is_none() && !self.queued_inputs.is_empty() {
            self.discard_queued(turn, emit);
        }
        self.open_questions
            .retain(|(_, question)| question.turn != Some(turn));
        self.argument_overrides.clear();
        self.pending_compaction = None;
        self.turn_flags = TurnFlags::default();
        self.turn_totals.reset();
        self.active_model = None;
        self.active_family = None;
        self.phase = if follow_up.is_some() {
            Phase::Settling { turn, follow_up }
        } else {
            Phase::Idle
        };
    }
}
