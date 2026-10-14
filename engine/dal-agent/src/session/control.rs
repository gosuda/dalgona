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
    opening_shutdown: bool,
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
            opening_shutdown: false,
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
        if self.opening_shutdown {
            token.cancel();
        }
        self.opening = Some(RunningTurn {
            turn,
            token: token.clone(),
            phase: TurnPhase::Running,
        });
        token
    }

    /// Clears a completed opening slot. A cancelled token stays attached
    /// until `begin_turn` binds it to the driver, preserving cancellation.
    pub(crate) fn end_opening(&mut self, turn: TurnId) {
        if self
            .opening
            .as_ref()
            .is_some_and(|opening| opening.turn == turn && !opening.token.is_cancelled())
        {
            self.opening = None;
        }
    }

    /// Fires and clears any open opening token, regardless of turn.
    pub(crate) fn cancel_opening(&mut self) {
        self.opening_shutdown = false;
        if let Some(opening) = self.opening.take() {
            opening.token.cancel();
        }
    }

    /// Cancels an opening for shutdown while retaining its marker until the
    /// actor skips Guard and lets the normal shutdown request close the actor.
    pub(crate) fn cancel_opening_for_shutdown(&mut self) {
        self.opening_shutdown = true;
        if let Some(opening) = self.opening.as_ref() {
            opening.token.cancel();
        }
        if let Some(running) = self.running.as_ref() {
            running.token.cancel();
        }
    }

    /// Reports whether the opening for `turn` was cancelled by shutdown.
    pub(crate) fn opening_shutdown(&self, turn: TurnId) -> bool {
        self.opening_shutdown
            && self
                .opening
                .as_ref()
                .is_some_and(|opening| opening.turn == turn)
    }

    /// Starts a turn, resetting its phase mirror. A turn already holding an
    /// opening token keeps it: runs started by opening hooks stay bound to
    /// the token `cancel` fires.
    pub(crate) fn begin_turn(&mut self, turn: TurnId) -> CancellationToken {
        if let Some(opening) = self.opening.take_if(|opening| opening.turn == turn) {
            self.opening_shutdown = false;
            let token = opening.token.clone();
            self.running = Some(RunningTurn {
                turn,
                token: token.clone(),
                phase: TurnPhase::Running,
            });
            return token;
        }
        self.end_opening(turn);
        let token = CancellationToken::new();
        if self.opening_shutdown {
            token.cancel();
        }
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

    /// Clones the live token for `turn` in either phase: opening hooks run
    /// before the journal sees `TurnStart`, so an opening turn's token lives
    /// in the opening slot, not the running one. A turn that already ended
    /// (or never started) yields `None`: callers must fail closed rather
    /// than fall back to an unrelated session lifetime.
    pub(crate) fn token_for(&self, turn: TurnId) -> Option<CancellationToken> {
        if let Some(token) = self
            .running
            .as_ref()
            .filter(|running| running.turn == turn)
            .map(|running| running.token.clone())
        {
            return Some(token);
        }
        self.opening
            .as_ref()
            .filter(|opening| opening.turn == turn)
            .map(|opening| opening.token.clone())
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

    /// Reports true only when neither an opening nor a running turn exists.
    pub(crate) fn idle(&self) -> bool {
        self.running.is_none() && self.opening.is_none()
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
