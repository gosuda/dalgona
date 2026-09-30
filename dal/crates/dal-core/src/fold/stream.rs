use super::helpers::{
    CompletedResponse, InferredResponse, StreamEnd, assistant_stop, blocks_from_inference,
    compact_notice, compaction_started_notice, invalid, resolution_failure, truncated_args,
    zero_usage,
};
use super::{
    CompactionReason, Effect, Emit, EntryKind, InferFailure, PartialResponse, PendingCall, Phase,
    PlannedCall, Record, Rejection, ResolvedCall, Session, Stop, StreamEvent, TreeDelta,
    TurnEndStop, TurnId, TurnStage, UpdateKind, plan,
};

impl Session {
    pub(super) fn stream(
        &mut self,
        turn: TurnId,
        event: StreamEvent,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        if !matches!(&self.phase, Phase::Running { turn: active, stage: TurnStage::Streaming { .. }, .. } if *active == turn)
        {
            return Ok(());
        }
        match event {
            StreamEvent::Delta { channel, text } => effects.push(Effect::Delta {
                turn,
                channel,
                text,
            }),
            StreamEvent::ToolCall { call, name, args } => {
                emit.updates.push(UpdateKind::ToolStarted {
                    call,
                    tool: name,
                    args,
                });
            }
            StreamEvent::Usage(_) | StreamEvent::Stop(_) | StreamEvent::ThinkingReplay { .. } => {}
            StreamEvent::Compaction { .. } => {
                return Err(invalid(
                    "native compaction outcome appeared in a turn stream",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn stream_ended(
        &mut self,
        turn: TurnId,
        end: StreamEnd,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        if !matches!(&self.phase, Phase::Running { turn: active, stage: TurnStage::Streaming { .. }, .. } if *active == turn)
        {
            return Ok(());
        }
        let StreamEnd {
            model,
            family,
            result,
            partial,
        } = end;
        self.active_model = Some(model.clone());
        self.active_family = Some(family);
        match result {
            Ok(inference) => {
                let response = CompletedResponse {
                    model,
                    family,
                    inference,
                };
                self.response_completed(turn, response, now, emit, effects)
            }
            Err(failure) => self.stream_failed(turn, failure, partial, now, emit, effects),
        }
    }

    pub(super) fn stream_failed(
        &mut self,
        turn: TurnId,
        failure: InferFailure,
        partial: Option<PartialResponse>,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        let message: Box<str> = match failure {
            InferFailure::Retryable { .. } => return Ok(()),
            InferFailure::Cancelled => {
                return self.end_turn(turn, TurnEndStop::Cancelled, partial, now, emit, effects);
            }
            InferFailure::Overflow { message, .. } => {
                if !self.turn_flags.overflowed && self.overflow_compaction(turn, emit, effects) {
                    return Ok(());
                }
                format!("Context overflow recovery failed: {message}").into()
            }
            InferFailure::Fatal { message, .. } => message,
            error @ (InferFailure::SyntheticCycle { .. } | InferFailure::SyntheticDepth { .. }) => {
                error.to_string().into()
            }
        };
        self.end_turn(
            turn,
            TurnEndStop::Failed { message },
            partial,
            now,
            emit,
            effects,
        )
    }

    /// Starts overflow compaction; returns `false` when no compactor can run.
    pub(super) fn overflow_compaction(
        &mut self,
        turn: TurnId,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> bool {
        let Some(limits) = self
            .limits
            .filter(|limits| limits.compact.compactor_available)
        else {
            if !self.compaction_none_notified {
                emit.updates
                    .push(compact_notice(Some(turn), "compact.none"));
                self.compaction_none_notified = true;
            }
            return false;
        };
        self.turn_flags.overflowed = true;
        self.pending_compaction = Some(CompactionReason::Overflow);
        self.phase = Phase::Running {
            turn,
            round: self.current_round(),
            stage: TurnStage::Compacting {
                reason: CompactionReason::Overflow,
                automatic: true,
            },
        };
        emit.updates.push(compaction_started_notice(
            Some(turn),
            CompactionReason::Overflow,
            self.measured_context_tokens(),
            limits.window,
        ));
        effects.push(Effect::Compact {
            turn: Some(turn),
            first_kept: self.compaction_cut_point(),
        });
        true
    }

    pub(super) fn response_completed(
        &mut self,
        turn: TurnId,
        response: CompletedResponse,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        self.turn_flags.overflowed = false;
        let InferredResponse {
            blocks,
            calls,
            usage,
            stop,
        } = blocks_from_inference(response.inference)?;
        let Some(stop) = stop else {
            let partial = Some(PartialResponse {
                content: blocks,
                usage: usage.unwrap_or(zero_usage()),
            });
            let message = "provider inference ended without a stop event".into();
            return self.end_turn(
                turn,
                TurnEndStop::Failed { message },
                partial,
                now,
                emit,
                effects,
            );
        };
        let usage = usage.unwrap_or(zero_usage());
        let assistant = self.entry(
            now,
            EntryKind::Assistant {
                api: response.family,
                model: response.model.id().into(),
                content: blocks,
                usage,
                stop: assistant_stop(stop),
            },
        )?;
        self.last_usage = Some((response.model, usage.input_tokens, self.compactions));
        let view = self.tree.append(assistant.clone());
        emit.records.push(Record::Assistant(assistant));
        self.turn_totals.add_usage(usage)?;
        let mut pending: Vec<PendingCall> = Vec::new();
        for (call, name) in calls {
            if pending.iter().all(|item| item.call != call) {
                pending.push(PendingCall {
                    call,
                    name,
                    started: false,
                    promotes: None,
                });
            }
        }
        emit.updates.push(UpdateKind::Tree(TreeDelta {
            added: vec![view],
            leaf: self.tree.leaf,
        }));
        emit.updates.push(UpdateKind::Usage(crate::view::UsageView {
            usage,
            context_tokens: usage.input_tokens,
            context_window: self.limits.map_or(0, |limits| limits.window),
        }));
        self.response_stopped(turn, stop, pending, now, emit, effects)
    }

    pub(super) fn response_stopped(
        &mut self,
        turn: TurnId,
        stop: Stop,
        pending: Vec<PendingCall>,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        let round = self.current_round();
        let end = match stop {
            Stop::Length if !pending.is_empty() => {
                self.fail_calls(&pending, truncated_args, now, emit)?;
                self.phase = Phase::Running {
                    turn,
                    round,
                    stage: TurnStage::Boundary,
                };
                return self.boundary(turn, now, true, emit, effects);
            }
            Stop::Filter => {
                self.fail_calls(&pending, truncated_args, now, emit)?;
                self.phase = Phase::Running {
                    turn,
                    round,
                    stage: TurnStage::Boundary,
                };
                TurnEndStop::Filter
            }
            Stop::Length => TurnEndStop::Length,
            Stop::EndTurn => {
                let stage = if pending.is_empty() {
                    self.turn_flags.end_after_boundary = true;
                    TurnStage::Boundary
                } else {
                    TurnStage::Resolving { pending }
                };
                self.phase = Phase::Running { turn, round, stage };
                return Ok(());
            }
            Stop::Cancelled | Stop::Failed | Stop::MaxSteps => {
                self.phase = Phase::Running {
                    turn,
                    round,
                    stage: TurnStage::Resolving { pending },
                };
                match stop {
                    Stop::Cancelled => TurnEndStop::Cancelled,
                    Stop::MaxSteps => TurnEndStop::MaxSteps,
                    _ => TurnEndStop::Failed {
                        message: "provider ended with a failed stop".into(),
                    },
                }
            }
        };
        self.end_turn(turn, end, None, now, emit, effects)
    }

    pub(super) fn resolved(
        &mut self,
        turn: TurnId,
        calls: &[ResolvedCall],
        answerer_attached: bool,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        let expected = match &self.phase {
            Phase::Running {
                turn: active,
                stage: TurnStage::Resolving { pending },
                ..
            } if *active == turn => pending.clone(),
            _ => return Ok(()),
        };
        if calls
            .iter()
            .any(|call| !expected.iter().any(|item| item.call == call.call))
            || expected
                .iter()
                .any(|item| !calls.iter().any(|call| call.call == item.call))
        {
            return Err(invalid(
                "resolved calls do not match the pending response calls",
            ));
        }
        let mut planned = Vec::new();
        let mut promotes = Vec::new();
        for item in &expected {
            let mut matching = calls.iter().filter(|call| call.call == item.call);
            let Some(call) = matching.next() else {
                continue;
            };
            if let Some(text) = resolution_failure(call, matching.next().is_some()) {
                self.result_entry(&item.call, &item.name, text, true, now, emit)?;
                continue;
            }
            let Ok(class) = &call.result else {
                continue;
            };
            planned.push(PlannedCall {
                call: item.call.clone(),
                name: call.name.clone(),
                class: class.clone(),
            });
            promotes.push(call.promoted);
        }
        let units = plan(&planned, &self.policy(answerer_attached));
        let pending = planned
            .into_iter()
            .zip(promotes)
            .map(|(call, promoted)| PendingCall {
                call: call.call,
                name: call.name.to_string().into(),
                started: false,
                promotes: promoted.then_some(call.name),
            })
            .collect::<Vec<_>>();
        let round = self.current_round();
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
            effects.push(Effect::Dispatch { turn, units });
        }
        Ok(())
    }
}
