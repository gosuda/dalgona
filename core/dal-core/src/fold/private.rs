//! Private-effect fold state: loop births, start/end pairing, terminal log.
//!
//! A synthetic private loop journals three non-tree records: one
//! [`Record::PrivateLoop`] birth certificate, one [`Record::PrivateStart`]
//! per effect before it runs, and one [`Record::PrivateEnd`] per effect
//! after it settles. This module owns the fold side of that protocol: the
//! birth registry, the pairing rows, and the append-only terminal log whose
//! index is the source-order identity of each terminal outcome.
//!
//! Two call sites share one validation owner. The live actor pre-validates
//! each transition with the non-mutating `check_private_*` methods BEFORE
//! the durable append and commits with `fold_private_*` only after the
//! receipt; replay folds already-durable records through the same pair and
//! maps any [`PrivateConflict`] to a contradiction instead. Repair
//! synthesizes its ends through the same applier, so no shape is validated
//! or applied in two places.
//!
//! Lookups are O(1) [`HashMap`] indexes; deterministic journal order lives
//! in vectors only, never in map iteration.

use std::collections::HashMap;

use super::Session;
use crate::ext::PrivateEffect;
use crate::id::{PrivateEffectId, PrivateLoopId, TurnId};
use crate::journal::{PrivateOutcome, Record};
use crate::request::Owner;

/// One born private loop: its owner and owning turn.
///
/// Every born loop keeps its row even when it never gains a start, so the
/// start/end checks can tell an unborn loop from a born one with no start.
/// The loop identity itself is the key of the birth index next to it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct PrivateLoopBirth {
    pub(super) who: Owner,
    pub(super) turn: Option<TurnId>,
}

/// Live pairing state for one started private effect, in journal order.
///
/// `terminal` is `None` while the effect is in flight and `Some(index)`
/// into the terminal log once its end folds. The outcome payload lives
/// exactly once in the terminal log; the row keeps only the index.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct PrivateRow {
    pub(super) r#loop: PrivateLoopId,
    pub(super) token: PrivateEffectId,
    pub(super) who: Owner,
    pub(super) turn: Option<TurnId>,
    pub(super) round: u32,
    pub(super) name: Box<str>,
    pub(super) terminal: Option<usize>,
}

/// One terminal private-effect fact, in end journal order.
///
/// The position in the terminal log is the source-order identity of the
/// outcome; it is never a permission token. Only folded ends enter this
/// log, so in-flight rows are invisible to publication by construction.
#[derive(Clone, Debug, PartialEq)]
pub struct PrivateTerminal {
    /// The loop identity.
    pub r#loop: PrivateLoopId,
    /// The effect identity.
    pub token: PrivateEffectId,
    /// The extension that owns the effect.
    pub who: Owner,
    /// The owning turn, when the loop runs inside one.
    pub turn: Option<TurnId>,
    /// The private round number.
    pub round: u32,
    /// The tool name.
    pub name: Box<str>,
    /// The terminal outcome.
    pub outcome: PrivateOutcome,
}

/// The six ways a private transition can contradict the folded history.
///
/// Defined once here. Replay maps every variant to a contradiction; the
/// live actor fails the write before appending on the same shapes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrivateConflict {
    /// A start or end naming a loop with no prior birth.
    UnbornLoop,
    /// A second birth for one loop id, childless or not.
    LoopBornTwice,
    /// A second start for one `(loop, token)`.
    EffectStartedTwice,
    /// A start whose owner or turn differs from its loop's birth.
    LoopMismatch,
    /// An end whose `(loop, token)` has no started row, including the
    /// cross-loop case where the token started under another loop.
    EndWithoutStart,
    /// A second end for one `(loop, token)`.
    EndedTwice,
}

impl PrivateConflict {
    /// The contradiction detail replay reports for this shape.
    #[must_use]
    pub const fn detail(self) -> &'static str {
        match self {
            Self::UnbornLoop => "private effect names a loop with no private birth",
            Self::LoopBornTwice => "private loop born more than once",
            Self::EffectStartedTwice => "private effect started more than once",
            Self::LoopMismatch => "private start disagrees with its loop birth on owner or turn",
            Self::EndWithoutStart => "private end has no matching private start",
            Self::EndedTwice => "private effect ended more than once",
        }
    }
}

/// One paired private-effect fact for inspection, borrowing the fold.
///
/// The production publication path reads the terminal log through
/// [`Session::private_terminals_since`], not this iterator; tests and
/// debugging use it to see in-flight rows alongside settled ones.
#[derive(Clone, Copy, Debug)]
pub struct PrivateFact<'a> {
    /// The loop identity.
    pub r#loop: PrivateLoopId,
    /// The effect identity.
    pub token: PrivateEffectId,
    /// The extension that owns the effect.
    pub who: &'a Owner,
    /// The owning turn, when the loop runs inside one.
    pub turn: Option<TurnId>,
    /// The private round number.
    pub round: u32,
    /// The tool name.
    pub name: &'a str,
    /// The terminal outcome, `None` while the effect is in flight.
    pub outcome: Option<&'a PrivateOutcome>,
}

/// Birth position by loop id: O(1) lookup with no history scan.
pub(super) type PrivateLoopIndex = HashMap<PrivateLoopId, usize>;

/// Pairing-row position by `(loop, token)`: O(1) lookup, no history scan.
pub(super) type PrivateRowIndex = HashMap<(PrivateLoopId, PrivateEffectId), usize>;

impl From<&PrivateTerminal> for PrivateEffect {
    fn from(terminal: &PrivateTerminal) -> Self {
        Self {
            r#loop: terminal.r#loop,
            token: terminal.token,
            turn: terminal.turn,
            round: terminal.round,
            name: terminal.name.clone(),
            outcome: terminal.outcome.clone(),
        }
    }
}

impl Session {
    /// Validates a loop birth without mutating the fold.
    ///
    /// The live actor runs this BEFORE the durable append; a rejection
    /// leaves the journal byte-identical.
    ///
    /// # Errors
    /// Returns [`PrivateConflict::LoopBornTwice`] when the loop id already
    /// has a birth, childless or not.
    pub fn check_private_loop(&self, r#loop: PrivateLoopId) -> Result<(), PrivateConflict> {
        if self.private_loop_index.contains_key(&r#loop) {
            return Err(PrivateConflict::LoopBornTwice);
        }
        Ok(())
    }

    /// Validates a private start without mutating the fold.
    ///
    /// The live actor runs this BEFORE the durable append; a rejection
    /// leaves the journal byte-identical.
    ///
    /// # Errors
    /// Returns [`PrivateConflict::UnbornLoop`] when the loop has no birth,
    /// [`PrivateConflict::LoopMismatch`] when `who` or `turn` differs from
    /// the birth pair, or [`PrivateConflict::EffectStartedTwice`] when
    /// `(loop, token)` already has a row.
    pub fn check_private_start(
        &self,
        r#loop: PrivateLoopId,
        token: PrivateEffectId,
        who: &Owner,
        turn: Option<TurnId>,
    ) -> Result<(), PrivateConflict> {
        let Some(birth) = self
            .private_loop_index
            .get(&r#loop)
            .map(|index| &self.private_loops[*index])
        else {
            return Err(PrivateConflict::UnbornLoop);
        };
        if &birth.who != who || birth.turn != turn {
            return Err(PrivateConflict::LoopMismatch);
        }
        if self.private_row_index.contains_key(&(r#loop, token)) {
            return Err(PrivateConflict::EffectStartedTwice);
        }
        Ok(())
    }

    /// Validates a private end without mutating the fold.
    ///
    /// The live actor runs this BEFORE the durable append; a rejection
    /// leaves the journal byte-identical.
    ///
    /// # Errors
    /// Returns [`PrivateConflict::UnbornLoop`] when the loop has no birth,
    /// [`PrivateConflict::EndWithoutStart`] when `(loop, token)` has no
    /// started row, or [`PrivateConflict::EndedTwice`] when the row
    /// already settled.
    pub fn check_private_end(
        &self,
        r#loop: PrivateLoopId,
        token: PrivateEffectId,
    ) -> Result<(), PrivateConflict> {
        if !self.private_loop_index.contains_key(&r#loop) {
            return Err(PrivateConflict::UnbornLoop);
        }
        match self.private_row_index.get(&(r#loop, token)) {
            None => Err(PrivateConflict::EndWithoutStart),
            Some(index) if self.private_rows[*index].terminal.is_some() => {
                Err(PrivateConflict::EndedTwice)
            }
            Some(_) => Ok(()),
        }
    }

    /// Commits a pre-validated loop birth after the durable receipt.
    ///
    /// The caller must have accepted [`Session::check_private_loop`]
    /// first; replay and repair uphold this through the same pair, so the
    /// birth is pushed unconditionally here.
    ///
    /// # Panics
    /// Panics when `loop` already has a birth, meaning the caller skipped
    /// the [`Session::check_private_loop`] acceptance gate.
    pub fn fold_private_loop(&mut self, r#loop: PrivateLoopId, who: Owner, turn: Option<TurnId>) {
        assert!(
            !self.private_loop_index.contains_key(&r#loop),
            "fold_private_loop needs a check_private_loop-accepted loop"
        );
        self.private_loop_index
            .insert(r#loop, self.private_loops.len());
        self.private_loops.push(PrivateLoopBirth { who, turn });
    }

    /// Commits a pre-validated private start after the durable receipt.
    ///
    /// The caller must have accepted [`Session::check_private_start`]
    /// first. The row starts in flight: no terminal is published until the
    /// matching end folds.
    ///
    /// # Panics
    /// Panics when the loop birth is missing or mismatched, or
    /// `(loop, token)` already has a row, meaning the caller skipped the
    /// [`Session::check_private_start`] acceptance gate.
    pub fn fold_private_start(
        &mut self,
        r#loop: PrivateLoopId,
        token: PrivateEffectId,
        who: &Owner,
        turn: Option<TurnId>,
        round: u32,
        name: &str,
    ) {
        let birth_matches = self.private_loop_index.get(&r#loop).is_some_and(|index| {
            let birth = &self.private_loops[*index];
            birth.who == *who && birth.turn == turn
        });
        assert!(
            birth_matches && !self.private_row_index.contains_key(&(r#loop, token)),
            "fold_private_start needs a check_private_start-accepted (loop, token)"
        );
        self.private_row_index
            .insert((r#loop, token), self.private_rows.len());
        self.private_rows.push(PrivateRow {
            r#loop,
            token,
            who: who.clone(),
            turn,
            round,
            name: name.into(),
            terminal: None,
        });
    }

    /// Commits a pre-validated private end after the durable receipt.
    ///
    /// The caller must have accepted [`Session::check_private_end`]
    /// first. The outcome is stored exactly once, in the appended
    /// terminal; the row keeps only the terminal index.
    ///
    /// # Panics
    /// Panics when `(loop, token)` has no started row or the row already
    /// settled, meaning the caller skipped the
    /// [`Session::check_private_end`] acceptance gate.
    pub fn fold_private_end(
        &mut self,
        r#loop: PrivateLoopId,
        token: PrivateEffectId,
        outcome: PrivateOutcome,
    ) {
        let index = self.private_row_index[&(r#loop, token)];
        assert!(
            self.private_rows[index].terminal.is_none(),
            "fold_private_end needs a check_private_end-accepted (loop, token)"
        );
        let row = &self.private_rows[index];
        let (r#loop, token, turn, round) = (row.r#loop, row.token, row.turn, row.round);
        let (who, name) = (row.who.clone(), row.name.clone());
        let terminal = self.private_terminals.len();
        self.private_terminals.push(PrivateTerminal {
            r#loop,
            token,
            who,
            turn,
            round,
            name,
            outcome,
        });
        self.private_rows[index].terminal = Some(terminal);
    }

    /// Returns the number of folded terminal outcomes.
    ///
    /// The O(1) publication cursor source: Shared advances from here and
    /// diffs through [`Session::private_terminals_since`].
    #[must_use]
    pub const fn private_terminal_len(&self) -> usize {
        self.private_terminals.len()
    }

    /// Borrows the terminal log from `since` onward.
    ///
    /// An O(1) delta slice in end journal order; empty exactly when
    /// `since` equals the folded length.
    ///
    /// # Panics
    /// Panics when `since` lies beyond the folded terminals. A publication
    /// cursor beyond the fold length is an internal invariant failure:
    /// Shared advances its cursor only from slices this method returns, so
    /// a larger cursor means a bookkeeping bug, and failing closed beats a
    /// silent empty read that would stall publication forever.
    #[must_use]
    pub fn private_terminals_since(&self, since: usize) -> &[PrivateTerminal] {
        assert!(
            since <= self.private_terminals.len(),
            "private publication cursor {since} beyond {} folded terminals",
            self.private_terminals.len()
        );
        &self.private_terminals[since..]
    }

    /// Borrows every pairing row in journal order, settled or in flight.
    ///
    /// Retained for tests and debugging; the production publication path
    /// uses [`Session::private_terminals_since`], not this iterator.
    pub fn private_facts(&self) -> impl Iterator<Item = PrivateFact<'_>> {
        self.private_rows.iter().map(|row| PrivateFact {
            r#loop: row.r#loop,
            token: row.token,
            who: &row.who,
            turn: row.turn,
            round: row.round,
            name: &row.name,
            outcome: row
                .terminal
                .map(|terminal| &self.private_terminals[terminal].outcome),
        })
    }

    /// Synthesizes one [`PrivateOutcome::Indeterminate`] end per unresolved
    /// start, in row (journal) order.
    ///
    /// Normal replay finish only: declared replay never calls this, so
    /// inspection attributes state to written records. Each synthesized
    /// end is pushed into the repair batch for persistence AND folded
    /// through [`Session::fold_private_end`], settling its row and
    /// publishing its terminal like any other end; a second open finds no
    /// unresolved row and mints nothing. No tool executes here.
    pub(super) fn repair_private_ends(&mut self, now: jiff::Timestamp, records: &mut Vec<Record>) {
        let mut index = 0;
        while index < self.private_rows.len() {
            let pending = if self.private_rows[index].terminal.is_none() {
                let row = &self.private_rows[index];
                Some((row.r#loop, row.token))
            } else {
                None
            };
            if let Some((r#loop, token)) = pending {
                records.push(Record::PrivateEnd {
                    at: now,
                    r#loop,
                    token,
                    outcome: PrivateOutcome::Indeterminate,
                });
                self.fold_private_end(r#loop, token, PrivateOutcome::Indeterminate);
            }
            index += 1;
        }
    }
}
