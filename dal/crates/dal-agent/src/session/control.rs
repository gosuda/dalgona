//! Turn bypass: cancel around the command channel.
//!
//! Cancel fires the turn token before the actor touches any channel;
//! a mismatch changes nothing. An opening turn has no journal start
//! yet, so the cell tracks the opening token alongside the running one
//! and `cancel` hits whichever matches.

use dal_core::TurnId;
use tokio_util::sync::CancellationToken;

use crate::error::{ActualTurn, AgentError, ExpectedTurn, TurnPhase};

/// Mirror of the running turn.
pub(crate) struct ControlCell {
    running: Option<RunningTurn>,
    opening: Option<RunningTurn>,
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
            opening: None,
            last_ended: None,
        }
    }

    /// Mints the opening-phase token for `turn`, replacing any stale one.
    ///
    /// The actor binds this token while `Opening` hooks run, before the
    /// journal sees `TurnStart`; a cancel landing in that window fires it
    /// without waiting for the turn to start.
    pub(crate) fn begin_opening(&mut self, turn: TurnId) -> CancellationToken {
        let token = CancellationToken::new();
        self.opening = Some(RunningTurn {
            turn,
            token: token.clone(),
            phase: TurnPhase::Running,
        });
        token
    }

    /// Clears the opening slot when it still names `turn`.
    pub(crate) fn end_opening(&mut self, turn: TurnId) {
        if self.opening.as_ref().is_some_and(|o| o.turn == turn) {
            self.opening = None;
        }
    }

    /// Fires and clears any open opening token, regardless of turn.
    pub(crate) fn cancel_opening(&mut self) {
        if let Some(opening) = self.opening.take() {
            opening.token.cancel();
        }
    }

    /// Starts a turn, resetting its token and phase mirror.
    pub(crate) fn begin_turn(&mut self, turn: TurnId) -> CancellationToken {
        self.end_opening(turn);
        let token = CancellationToken::new();
        self.running = Some(RunningTurn {
            turn,
            token: token.clone(),
            phase: TurnPhase::Running,
        });
        token
    }

    /// Fires the turn's live token, then leaves the actor to settle it.
    ///
    /// A turn in its opening phase fires the opening token; a running turn
    /// fires the running token. A turn that already ended reports
    /// `WrongTurn` and changes nothing.
    pub(crate) fn cancel(&self, turn: TurnId) -> Result<(), AgentError> {
        let mut fired = false;
        if let Some(opening) = self.opening.as_ref().filter(|o| o.turn == turn) {
            opening.token.cancel();
            fired = true;
        }
        if let Some(running) = self.running.as_ref().filter(|r| r.turn == turn) {
            running.token.cancel();
            fired = true;
        }
        if fired {
            return Ok(());
        }
        Err(AgentError::WrongTurn {
            expected: ExpectedTurn::Turn(turn),
            actual: self.actual(),
        })
    }

    /// Clones the running turn's token when `turn` holds the session.
    ///
    /// The driver's per-turn state binds this token so `cancel` preempts a
    /// live stream out-of-band — `Effect::Stop` only ever reaches the driver
    /// after `infer` returns, so a queued effect can never interrupt it.
    pub(crate) fn token(&self, turn: TurnId) -> Option<CancellationToken> {
        self.running
            .as_ref()
            .filter(|running| running.turn == turn)
            .map(|running| running.token.clone())
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
