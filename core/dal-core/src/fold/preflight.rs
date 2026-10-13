use std::collections::HashSet;

use super::helpers::{invalid, unsettled_calls, wrong_turn};
use super::types::{MAX_INTERRUPTS, MAX_STEERS, MAX_WAKE_RUN, Overflow};
use super::{
    Answer, CallId, CancelScope, Command, Event, Expect, HookEvent, HookOutcome, HookVerdict,
    InferFailure, Name, PartialResponse, Phase, Rejection, RequestId, Session, Stop, StreamEvent,
    StreamVerdict, ToolCallVerdict, TurnId, TurnStage, TurnState,
};

impl Session {
    pub(super) fn preflight(&self, event: &Event) -> Result<(), Rejection> {
        if matches!(&self.phase, Phase::Closed) && !matches!(event, Event::Close) {
            return Err(Rejection::SessionClosed);
        }
        match event {
            Event::Command { cmd, .. } => self.preflight_command(cmd),
            Event::Steer { turn, .. } => self.preflight_steer(*turn),
            Event::Wake { .. } => self.preflight_wake(),
            Event::GrantResolved {
                request,
                answer: Answer::ApproveForSession,
                by: Some(_),
                was_default: false,
            } => self.preflight_session_grant(*request),
            Event::Guard {
                turn,
                call,
                extension,
                outcome,
            } => self.preflight_guard(*turn, call.is_some(), extension.is_some(), outcome),
            Event::StreamRetry { turn, reminder } => {
                self.preflight_stream_entries(*turn, usize::from(reminder.is_some()))
            }
            Event::StreamVerdict {
                turn,
                verdict: StreamVerdict::Interrupt { .. },
            } if self.turn_flags.interrupts < MAX_INTERRUPTS => {
                self.preflight_stream_entries(*turn, 1)
            }
            Event::StreamEnded {
                turn,
                result,
                partial,
                ..
            } => self.preflight_stream_end(*turn, result, partial.as_ref()),
            Event::Cancel {
                scope: CancelScope::Turn(turn),
                ..
            } => self.preflight_cancel(*turn),
            Event::Settled { turn, call, .. } => self.preflight_settled(*turn, call),
            Event::Inferred { usage, .. } if matches!(&self.phase, Phase::Running { .. }) => {
                let mut totals = self.turn_totals.clone();
                totals.add_usage(*usage)
            }
            Event::CompactionSettled {
                turn,
                outcome: Ok(summary),
            } if summary.tokens_after < summary.tokens_before => {
                self.preflight_compaction_entry(*turn)
            }
            _ => Ok(()),
        }
    }

    /// Checks that `count` entry ids remain, so a transition that journals
    /// several entries cannot fail after it has changed the phase.
    pub(super) fn entries_available(&self, count: u64) -> Result<(), Rejection> {
        if count == 0 {
            return Ok(());
        }
        self.next_entry
            .and_then(|next| next.get().checked_add(count.saturating_sub(1)))
            .map(|_| ())
            .ok_or_else(|| invalid("entry id space exhausted"))
    }

    /// Reports the legacy turn-bound rejection state for follow-up and steer
    /// commands. Job-backed compaction intentionally reports Idle here;
    /// prompt rejection uses `turn_state` and preserves Compacting.
    fn wrong_turn_after(&self, turn: TurnId) -> Rejection {
        let actual = match &self.phase {
            Phase::Running { turn: active, .. } => TurnState::Running { turn: *active },
            Phase::Settling { turn: active, .. } => TurnState::Settling { turn: *active },
            _ => TurnState::Idle,
        };
        wrong_turn(Expect::After(turn), actual)
    }

    pub(super) fn preflight_wake(&self) -> Result<(), Rejection> {
        if self.wake_run >= MAX_WAKE_RUN {
            return Err(Rejection::Denied {
                reason: crate::approval::DenyReason::WakeLimit,
            });
        }
        if matches!(&self.phase, Phase::Compacting { .. }) {
            return Err(Rejection::BusyTurn);
        }
        if self.next_turn.is_none()
            || matches!(&self.phase, Phase::Idle) && self.next_entry.is_none()
        {
            return Err(invalid("turn or entry id space exhausted"));
        }
        Ok(())
    }

    pub(super) fn preflight_session_grant(&self, request: RequestId) -> Result<(), Rejection> {
        let Some(tool) = self
            .open_questions
            .iter()
            .find(|(id, _)| *id == request)
            .and_then(|(_, question)| question.tool.as_deref())
        else {
            return Ok(());
        };
        Name::parse_mapped_tool(tool)
            .map(|_| ())
            .map_err(|_| invalid("approval tool name is invalid"))
    }

    pub(super) fn preflight_guard(
        &self,
        turn: TurnId,
        has_call: bool,
        has_extension: bool,
        outcome: &HookOutcome,
    ) -> Result<(), Rejection> {
        match outcome.event() {
            HookEvent::BeforeTurn => {
                let reminder = u64::from(
                    matches!(outcome.verdict(), HookVerdict::BeforeTurn(Some(text)) if !text.is_empty()),
                );
                match &self.phase {
                    Phase::Settling {
                        follow_up: Some((next_turn, _)),
                        ..
                    } if *next_turn == turn => self.entries_available(1 + reminder),
                    Phase::Opening { turn: active, .. } if *active == turn && reminder == 1 => {
                        self.entries_available(1)
                    }
                    _ => Ok(()),
                }
            }
            HookEvent::ToolCall if matches!(&self.phase, Phase::Running { turn: active, stage: TurnStage::Dispatching { .. }, .. } if *active == turn) => {
                match outcome.verdict() {
                    HookVerdict::ToolCall(ToolCallVerdict::Block { .. })
                        if !has_call || !has_extension =>
                    {
                        Err(invalid(
                            "tool-call block requires call and extension identities",
                        ))
                    }
                    HookVerdict::ToolCall(ToolCallVerdict::Rewrite { .. }) if !has_call => {
                        Err(invalid("tool-call rewrite requires a call identity"))
                    }
                    _ => Ok(()),
                }
            }
            _ => Ok(()),
        }
    }

    fn preflight_stream_end(
        &self,
        turn: TurnId,
        result: &Result<super::Inference, InferFailure>,
        partial: Option<&PartialResponse>,
    ) -> Result<(), Rejection> {
        let active = matches!(
            &self.phase,
            Phase::Running {
                turn: active,
                stage: TurnStage::Streaming { .. },
                ..
            } if *active == turn
        );
        if !active {
            return Ok(());
        }
        let failure = match result {
            Ok(inference) => return self.preflight_response_entries(inference),
            Err(failure) => failure,
        };
        let partial_entry = partial.is_some_and(|partial| !partial.content.is_empty());
        let (response_entries, drains_pending, ends_turn) = match failure {
            InferFailure::Cancelled => (usize::from(partial_entry), false, true),
            InferFailure::Retryable { .. } => (0, false, false),
            InferFailure::Overflow { .. }
                if self.turn_flags.overflow == Overflow::Clear
                    && self
                        .limits
                        .is_some_and(|limits| limits.compact.compactor_available) =>
            {
                (0, false, false)
            }
            _ => (usize::from(partial_entry), true, true),
        };
        let pending = if drains_pending {
            self.turn_flags.pending_reminders.len()
        } else {
            0
        };
        let tool_entries = if ends_turn {
            match &self.phase {
                Phase::Running { stage, .. } => unsettled_calls(stage.clone(), partial).len(),
                _ => 0,
            }
        } else {
            0
        };
        let count = pending
            .checked_add(response_entries)
            .and_then(|count| count.checked_add(tool_entries))
            .ok_or_else(|| invalid("too many stream-end entries"))?;
        let count = u64::try_from(count).map_err(|_| invalid("too many stream-end entries"))?;
        self.entries_available(count)
    }

    fn preflight_response_entries(&self, inference: &super::Inference) -> Result<(), Rejection> {
        let stop = inference.events.iter().rev().find_map(|event| match event {
            StreamEvent::Stop(stop) => Some(*stop),
            _ => None,
        });
        let calls = if stop == Some(Stop::EndTurn) {
            0
        } else {
            inference
                .events
                .iter()
                .filter_map(|event| match event {
                    StreamEvent::ToolCall { call, .. } => Some(call),
                    _ => None,
                })
                .collect::<HashSet<_>>()
                .len()
        };
        let response = usize::from(
            stop.is_some()
                || inference.events.iter().any(|event| match event {
                    StreamEvent::Delta {
                        channel: super::StreamChannel::Text,
                        text,
                    } => !text.is_empty(),
                    StreamEvent::Delta {
                        channel: super::StreamChannel::Thinking,
                        ..
                    }
                    | StreamEvent::ThinkingReplay { .. }
                    | StreamEvent::ToolCall { .. } => true,
                    _ => false,
                }),
        );
        let reminders = if stop == Some(Stop::Cancelled) {
            0
        } else {
            self.turn_flags.pending_reminders.len()
        };
        let boundary = stop == Some(Stop::Length) && calls > 0;
        let suppressed = if boundary {
            self.turn_flags.pending_suppressed.len()
        } else {
            0
        };
        let count = [
            response,
            calls,
            reminders,
            usize::from(boundary && self.steers_queued() > 0),
            suppressed,
            usize::from(boundary && !self.ended_jobs.is_empty()),
        ]
        .into_iter()
        .try_fold(0_usize, usize::checked_add)
        .ok_or_else(|| invalid("too many stream-end entries"))?;
        let count = u64::try_from(count).map_err(|_| invalid("too many stream-end entries"))?;
        self.entries_available(count)
    }

    fn preflight_stream_entries(&self, turn: TurnId, extra: usize) -> Result<(), Rejection> {
        let pending = match &self.phase {
            Phase::Running {
                turn: active,
                stage: TurnStage::Streaming { .. },
                ..
            } if *active == turn => self.turn_flags.pending_reminders.len(),
            _ => return Ok(()),
        };
        let count = pending
            .checked_add(extra)
            .ok_or_else(|| invalid("too many pending reminders"))?;
        let count = u64::try_from(count).map_err(|_| invalid("too many pending reminders"))?;
        self.entries_available(count)
    }

    pub(super) fn preflight_cancel(&self, turn: TurnId) -> Result<(), Rejection> {
        match &self.phase {
            Phase::Opening { turn: active, .. } | Phase::Running { turn: active, .. }
                if *active == turn =>
            {
                Ok(())
            }
            Phase::Settling { turn: active, .. } if *active == turn => Err(wrong_turn(
                Expect::After(turn),
                TurnState::Settling { turn: *active },
            )),
            Phase::Running { turn: active, .. } => Err(wrong_turn(
                Expect::After(turn),
                TurnState::Running { turn: *active },
            )),
            _ => Err(wrong_turn(Expect::After(turn), TurnState::Idle)),
        }
    }

    pub(super) fn preflight_settled(&self, turn: TurnId, call: &CallId) -> Result<(), Rejection> {
        match &self.phase {
            Phase::Running {
                turn: active,
                stage: TurnStage::Dispatching { pending },
                ..
            } if *active == turn
                && pending
                    .iter()
                    .any(|item| item.call == *call && item.started) =>
            {
                self.entries_available(1)
            }
            Phase::Running { turn: active, .. } if *active == turn => {
                Err(invalid("tool result was not pending"))
            }
            _ => Ok(()),
        }
    }

    pub(super) fn preflight_compaction_entry(&self, turn: Option<TurnId>) -> Result<(), Rejection> {
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
        if phase_matches && (self.next_entry.is_none() || self.compactions == u32::MAX) {
            Err(invalid("entry or compaction counter space exhausted"))
        } else {
            Ok(())
        }
    }
    pub(super) fn preflight_command(&self, cmd: &Command) -> Result<(), Rejection> {
        if let Command::Prompt { expect, content } = cmd {
            if matches!(&self.phase, Phase::Compacting { .. }) {
                return Err(Rejection::Compacting);
            }
            if matches!(&self.phase, Phase::Idle) {
                if content.is_empty() {
                    return Err(invalid("prompt content is empty."));
                }
                let expected = match expect {
                    Expect::Idle => true,
                    Expect::After(turn) => self.last_turn == turn.get(),
                };
                if !expected {
                    return Err(wrong_turn(*expect, TurnState::Idle));
                }
                if self.next_turn.is_none() || self.next_entry.is_none() {
                    return Err(invalid("turn or entry id space exhausted"));
                }
                return Ok(());
            }
            return Err(wrong_turn(*expect, self.turn_state()));
        }
        match cmd {
            Command::Steer { turn, .. } => self.preflight_steer(*turn),
            Command::FollowUp { turn, .. } => match &self.phase {
                Phase::Running { turn: active, .. }
                    if active == turn && self.next_turn.is_some() =>
                {
                    Ok(())
                }
                Phase::Running { turn: active, .. } if active == turn => {
                    Err(invalid("turn id space exhausted"))
                }
                _ => Err(self.wrong_turn_after(*turn)),
            },
            Command::Rename(_)
            | Command::Compact { .. }
            | Command::MoveLeaf(_)
            | Command::Fork(_)
            | Command::Clone
                if !matches!(&self.phase, Phase::Idle) =>
            {
                Err(Rejection::BusyTurn)
            }
            Command::MoveLeaf(id) if !self.tree.entries.contains_key(id) => {
                Err(invalid("unknown entry id"))
            }
            _ => Ok(()),
        }
    }

    pub(super) fn preflight_steer(&self, turn: TurnId) -> Result<(), Rejection> {
        match &self.phase {
            Phase::Running { turn: active, .. } if *active == turn => {
                if self.steers_queued() == MAX_STEERS {
                    Err(Rejection::SteerFull)
                } else {
                    Ok(())
                }
            }
            _ => Err(self.wrong_turn_after(turn)),
        }
    }

    pub(super) fn turn_state(&self) -> TurnState {
        match &self.phase {
            Phase::Idle
            | Phase::Opening { .. }
            | Phase::Closed
            | Phase::Compacting { job: None } => TurnState::Idle,
            Phase::Running { turn, .. } => TurnState::Running { turn: *turn },
            Phase::Settling { turn, .. } => TurnState::Settling { turn: *turn },
            Phase::Compacting { job: Some(job) } => TurnState::Compacting { job: *job },
        }
    }
}
