//! Turn bypass: cancel around the command channel.
//!
//! Cancel fires the turn token before the actor touches any channel;
//! a mismatch changes nothing. An opening turn has no journal start
//! yet, so the cell tracks the opening token alongside the running one
//! and `cancel` hits whichever matches.

use std::sync::Arc;

use dal_core::TurnId;
use tokio_util::sync::CancellationToken;

use super::ResolutionInbox;
use crate::error::{ActualTurn, AgentError, ExpectedTurn, TurnPhase};

/// Mirror of the running turn.
pub(crate) struct ControlCell {
    running: Option<RunningTurn>,
    opening: Option<RunningTurn>,
    opening_shutdown: bool,
    last_ended: Option<TurnId>,
    /// The actor-owned resolution inbox: a classified cancel claims the
    /// turn's reported requests before the token fires, under the cell
    /// mutex → inbox → broker lock order.
    inbox: Arc<ResolutionInbox>,
}

struct RunningTurn {
    turn: TurnId,
    token: CancellationToken,
    phase: TurnPhase,
}

impl ControlCell {
    /// An idle cell with no running or ended turn, bound to the session's
    /// resolution inbox for cancel classification.
    pub(crate) fn new(inbox: Arc<ResolutionInbox>) -> Self {
        Self {
            running: None,
            opening: None,
            opening_shutdown: false,
            last_ended: None,
            inbox,
        }
    }

    /// Classifies the turn's open requests through the inbox, then fires the
    /// token. An already-fired token classifies nothing: its cancel was
    /// classified earlier, or the token was minted pre-cancelled under
    /// shutdown, so a later cancel must add no claims.
    fn fire_cancel(&self, turn: TurnId, token: &CancellationToken) {
        if token.is_cancelled() {
            return;
        }
        self.inbox.cancel_turn(turn);
        token.cancel();
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
    /// actor skips Guard and lets the normal shutdown request close the
    /// actor. Both the opening and the running turn classify here, before
    /// any lifecycle fire, so their reported requests journal `Cancelled`.
    pub(crate) fn cancel_opening_for_shutdown(&mut self) {
        self.opening_shutdown = true;
        if let Some(opening) = self.opening.as_ref() {
            self.fire_cancel(opening.turn, &opening.token);
        }
        if let Some(running) = self.running.as_ref() {
            self.fire_cancel(running.turn, &running.token);
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

    /// Classifies the turn's open requests through the inbox, then fires its
    /// live token, and leaves the actor to settle it.
    ///
    /// A turn in its opening phase fires the opening token; a running turn
    /// fires the running token. A turn whose token is already fired matches
    /// but classifies and fires nothing. A turn that already ended reports
    /// `WrongTurn` and changes nothing.
    pub(crate) fn cancel(&self, turn: TurnId) -> Result<(), AgentError> {
        let mut matched = false;
        if let Some(opening) = self.opening.as_ref().filter(|o| o.turn == turn) {
            matched = true;
            self.fire_cancel(turn, &opening.token);
        }
        if let Some(running) = self.running.as_ref().filter(|r| r.turn == turn) {
            matched = true;
            self.fire_cancel(turn, &running.token);
        }
        if matched {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Broker;
    use dal_core::{Owner, Question};
    use std::time::Duration;

    fn fixture() -> (
        ControlCell,
        std::sync::Arc<Broker>,
        std::sync::Arc<ResolutionInbox>,
    ) {
        let broker = std::sync::Arc::new(Broker::new());
        let inbox = std::sync::Arc::new(ResolutionInbox::new(std::sync::Arc::clone(&broker)));
        let cell = ControlCell::new(std::sync::Arc::clone(&inbox));
        (cell, broker, inbox)
    }

    fn open_on(broker: &Broker, turn: TurnId) -> dal_core::RequestId {
        broker
            .open(
                Owner::Core,
                Question::Text {
                    prompt: "proceed?".into(),
                    placeholder: None,
                },
                turn,
                tokio::time::Instant::now() + Duration::from_secs(30),
            )
            .0
            .id
    }

    fn turn(n: u64) -> TurnId {
        TurnId::new(std::num::NonZeroU64::new(n).unwrap())
    }

    /// A user cancel classifies the reported requests once through the
    /// inbox, then fires the token; a second cancel adds no claims.
    #[test]
    fn cancel_classifies_once_and_fires_the_token() {
        let (mut cell, broker, inbox) = fixture();
        let id = turn(1);
        let token = cell.begin_turn(id);
        let request = open_on(&broker, id);
        assert!(broker.report_opened(request).is_none());
        cell.cancel(id).expect("the running turn matches");
        assert!(token.is_cancelled());
        let claim = inbox.take().expect("the cancel claims the request");
        assert_eq!(claim.request.id, request);
        assert_eq!(claim.cause, dal_core::ResolutionCause::Cancelled);
        cell.cancel(id).expect("the turn still matches");
        assert!(inbox.take().is_none(), "a second cancel claims nothing");
    }

    /// A cancel naming no registered turn changes nothing and reports
    /// `WrongTurn`.
    #[test]
    fn cancel_of_an_unregistered_turn_is_wrong_turn() {
        let (mut cell, broker, inbox) = fixture();
        cell.begin_turn(turn(1));
        assert!(matches!(
            cell.cancel(turn(2)),
            Err(AgentError::WrongTurn { .. })
        ));
        assert!(inbox.take().is_none());
        assert_eq!(broker.open_requests().len(), 0);
    }

    /// Shutdown classification covers both the opening and the running turn
    /// before any lifecycle fire, and stays single-shot.
    #[test]
    fn shutdown_classification_covers_both_slots_once() {
        let (mut cell, broker, inbox) = fixture();
        let opening_turn = turn(1);
        let running_turn = turn(2);
        let opening_token = cell.begin_opening(opening_turn);
        let running_token = cell.begin_turn(running_turn);
        let opening_request = open_on(&broker, opening_turn);
        let running_request = open_on(&broker, running_turn);
        assert!(broker.report_opened(opening_request).is_none());
        assert!(broker.report_opened(running_request).is_none());
        cell.cancel_opening_for_shutdown();
        assert!(opening_token.is_cancelled());
        assert!(running_token.is_cancelled());
        let claims: Vec<dal_core::RequestId> = std::iter::from_fn(|| inbox.take())
            .map(|claim| claim.request.id)
            .collect();
        assert_eq!(claims, vec![opening_request, running_request]);
        cell.cancel_opening_for_shutdown();
        assert!(inbox.take().is_none(), "shutdown classifies once");
    }

    /// Tokens minted after the shutdown marker fire pre-cancelled, and a
    /// later cancel matches the slot without classifying anything.
    #[test]
    fn post_marker_tokens_cancel_without_classification() {
        let (mut cell, _broker, inbox) = fixture();
        cell.begin_turn(turn(1));
        cell.cancel_opening_for_shutdown();
        let late_token = cell.begin_turn(turn(2));
        assert!(late_token.is_cancelled(), "the mint honours the marker");
        cell.cancel(turn(2)).expect("the registered turn matches");
        assert!(inbox.take().is_none(), "a dead token classifies nothing");
    }
}
