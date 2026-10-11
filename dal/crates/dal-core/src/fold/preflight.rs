use super::helpers::{invalid, wrong_turn};
use super::types::{MAX_STEERS, MAX_WAKE_RUN};
use super::{
    Answer, CallId, CancelScope, Command, Event, Expect, HookEvent, HookOutcome, HookVerdict, Name,
    Phase, Rejection, RequestId, Session, ToolCallVerdict, TurnId, TurnStage, TurnState,
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

    pub(super) fn entry_available(&self) -> Result<(), Rejection> {
        self.next_entry
            .map(|_| ())
            .ok_or_else(|| invalid("entry id space exhausted"))
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
            HookEvent::BeforeTurn => match &self.phase {
                Phase::Settling {
                    follow_up: Some((next_turn, _)),
                    ..
                } if *next_turn == turn => self.entry_available(),
                _ => Ok(()),
            },
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
                self.entry_available()
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
                Phase::Running { turn: active, .. } => Err(wrong_turn(
                    Expect::After(*turn),
                    TurnState::Running { turn: *active },
                )),
                Phase::Settling { turn: active, .. } => Err(wrong_turn(
                    Expect::After(*turn),
                    TurnState::Settling { turn: *active },
                )),
                _ => Err(wrong_turn(Expect::After(*turn), TurnState::Idle)),
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
            Phase::Running { turn: active, .. } => Err(wrong_turn(
                Expect::After(turn),
                TurnState::Running { turn: *active },
            )),
            Phase::Settling { turn: active, .. } => Err(wrong_turn(
                Expect::After(turn),
                TurnState::Settling { turn: *active },
            )),
            _ => Err(wrong_turn(Expect::After(turn), TurnState::Idle)),
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
