//! Turn bypass: cancel around the command channel.
//!
//! Cancel fires the turn token before the actor touches any channel;
//! a mismatch changes nothing.

use dal_core::TurnId;
use tokio_util::sync::CancellationToken;

use crate::error::{ActualTurn, AgentError, ExpectedTurn, TurnPhase};

/// Mirror of the running turn.
pub(crate) struct ControlCell {
    running: Option<RunningTurn>,
    last_ended: Option<TurnId>,
}

struct RunningTurn {
    turn: TurnId,
    token: CancellationToken,
    phase: TurnPhase,
}

impl ControlCell {
    /// An idle cell with no running or ended turn.
    pub(crate) fn new() -> Self {
        Self {
            running: None,
            last_ended: None,
        }
    }

    /// Starts a turn, resetting its token and phase mirror.
    pub(crate) fn begin_turn(&mut self, turn: TurnId) -> CancellationToken {
        let token = CancellationToken::new();
        self.running = Some(RunningTurn {
            turn,
            token: token.clone(),
            phase: TurnPhase::Running,
        });
        token
    }

    /// Fires the turn token, then leaves the actor to settle the turn.
    ///
    /// A turn that already ended reports `WrongTurn` and changes nothing.
    pub(crate) fn cancel(&self, turn: TurnId) -> Result<(), AgentError> {
        let Some(running) = self.running.as_ref().filter(|r| r.turn == turn) else {
            return Err(AgentError::WrongTurn {
                expected: ExpectedTurn::Turn(turn),
                actual: self.actual(),
            });
        };
        running.token.cancel();
        Ok(())
    }

    /// Ends the running turn when it is the one named.
    pub(crate) fn end_turn(&mut self, turn: TurnId) {
        if self.running.as_ref().is_some_and(|r| r.turn == turn) {
            self.running = None;
            self.last_ended = Some(turn);
        }
    }

    /// The running turn, when one holds the session.
    pub(crate) fn running(&self) -> Option<TurnId> {
        self.running.as_ref().map(|running| running.turn)
    }

    fn actual(&self) -> ActualTurn {
        match self.running.as_ref() {
            Some(running) => ActualTurn::Turn {
                turn: running.turn,
                phase: running.phase,
            },
            None => match self.last_ended {
                Some(turn) => ActualTurn::Turn {
                    turn,
                    phase: TurnPhase::Ended,
                },
                None => ActualTurn::Idle,
            },
        }
    }
}
