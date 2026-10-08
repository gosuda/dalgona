//! The judged lane: bool-verdict gating for judged rules.
//!
//! A judged rule carries a `judge` question from its record. On a pattern
//! match with an interrupting budget, the lane latches the rule for the turn
//! and one bool judgment call decides delivery: verdict `true` follows the
//! interrupt path, verdict `false` delivers nothing, and a judge error fails
//! open with one info log. A match on a reminding budget acts report-only
//! with no judge call, and at most `max_retries` waits happen per turn.
//!
//! The async verdict wait itself rides the judge handle once it lands; this
//! module owns the per-session lane state, the match/settle decisions, the
//! `[rules].judge` gate resolution, and the shared-context builder.

use std::collections::HashSet;

use dal_core::{JudgeMode, TurnId};

use super::texts::judge_auto_off_note;

/// A judged-lane event, in plan order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JudgedEvent {
    /// A judged rule matched while interrupts are allowed.
    MatchedInterrupts,
    /// A judged rule matched while only reminders are allowed.
    MatchedRemindersOnly,
    /// The judge answered the bool question.
    Verdict(bool),
    /// The judge call failed; the string is the cause.
    JudgeFailed(String),
}

/// What a pattern match on a judged rule means.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MatchDecision {
    /// Latch the rule, stop the stream, and await one verdict.
    JudgePending,
    /// Deliver as a report with no judge call.
    ReportOnly,
}

/// What a settled verdict means for delivery.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SettleDecision {
    /// Follow the interrupt path: journal, gate record, reminder, retry.
    Interrupt,
    /// Allow the turn on: no record, no reminder, gate record, retry.
    Allow,
}

/// The `[rules].judge` gate after session-start resolution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JudgeGate {
    /// Judged rules are watched.
    Enabled,
    /// Judged rules are dropped from the candidate set; the note, when
    /// present, goes to the session status.
    Disabled {
        /// The auto-off note, if the gate resolved off by itself.
        note: Option<String>,
    },
}

/// Session open fails with this exact text when `[rules].judge` is `"on"`
/// without a usable judge.
pub const JUDGE_ON_WITHOUT_CREDENTIALS: &str =
    "dalgon: config.toml: rules.judge \"on\" needs a judge model with usable credentials.";

/// Resolves the judged gate per the session-start rule.
///
/// `judge_ready` is the judge handle's readiness at session start;
/// `judged_count` is the number of judged rules in the set.
///
/// # Errors
///
/// Fails with [`JudgeGateError`] when `mode` is [`JudgeMode::On`] without a
/// ready judge handle.
pub fn gate_mode(
    mode: JudgeMode,
    judge_ready: bool,
    judged_count: usize,
) -> Result<JudgeGate, JudgeGateError> {
    match (mode, judge_ready) {
        (JudgeMode::Off, _) => Ok(JudgeGate::Disabled { note: None }),

        (JudgeMode::On, false) => Err(JudgeGateError),
        (JudgeMode::Auto, false) => Ok(JudgeGate::Disabled {
            note: Some(judge_auto_off_note(judged_count)),
        }),
        (_, true) => Ok(JudgeGate::Enabled),
    }
}

/// `[rules].judge = "on"` without a usable judge fails session open.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{JUDGE_ON_WITHOUT_CREDENTIALS}")]
pub struct JudgeGateError;

/// Per-session judged pending state: the rules latched this turn and the
/// bounded wait count.
#[derive(Debug, Default)]
pub struct JudgedLane {
    turn: Option<TurnId>,
    latched: HashSet<Box<str>>,
    stops: u32,
}

impl JudgedLane {
    /// Returns an empty lane for a session.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Notes a pattern match on a judged rule and answers what it means.
    ///
    /// `interrupts` is whether the watch budget allows interrupts;
    /// `max_retries` bounds the waits this turn. The rule latches for the
    /// turn on every match; only the first interrupting match within budget
    /// enters judge pending.
    pub fn matched(
        &mut self,
        rule: &str,
        turn: TurnId,
        interrupts: bool,
        max_retries: u32,
    ) -> MatchDecision {
        if self.turn != Some(turn) {
            self.turn = Some(turn);
            self.latched.clear();
            self.stops = 0;
        }
        if !self.latched.insert(rule.into()) {
            return MatchDecision::ReportOnly;
        }
        if !interrupts {
            return MatchDecision::ReportOnly;
        }
        if self.stops >= max_retries {
            return MatchDecision::ReportOnly;
        }
        self.stops += 1;
        MatchDecision::JudgePending
    }

    /// How many verdict waits this turn has consumed.
    #[must_use]
    pub fn stops(&self) -> u32 {
        self.stops
    }
}

/// Settles a judge verdict or failure into its delivery decision.
///
/// Match events carry no settle outcome and answer `None`.
#[must_use]
pub fn settle(event: &JudgedEvent) -> Option<SettleDecision> {
    match event {
        JudgedEvent::MatchedInterrupts | JudgedEvent::MatchedRemindersOnly => None,
        JudgedEvent::Verdict(true) => Some(SettleDecision::Interrupt),
        JudgedEvent::Verdict(false) | JudgedEvent::JudgeFailed(_) => Some(SettleDecision::Allow),
    }
}

/// Builds the judged shared context from the 4096-byte judge window tail,
/// the rule body, and the record's bool question.
#[must_use]
pub fn judge_shared(window: &str, body: &str, question: &str) -> String {
    format!("{window}\n\nRule:\n{body}\n\nQuestion:\n{question}")
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use super::*;

    fn turn(value: u64) -> TurnId {
        TurnId::new(NonZeroU64::new(value).expect("test turns are nonzero"))
    }

    #[test]
    fn gate_resolution_table() {
        assert_eq!(
            gate_mode(JudgeMode::Off, false, 2),
            Ok(JudgeGate::Disabled { note: None })
        );
        assert_eq!(gate_mode(JudgeMode::On, true, 0), Ok(JudgeGate::Enabled));
        assert_eq!(gate_mode(JudgeMode::On, false, 1), Err(JudgeGateError));
        assert_eq!(JudgeGateError.to_string(), JUDGE_ON_WITHOUT_CREDENTIALS);
        assert_eq!(gate_mode(JudgeMode::Auto, true, 3), Ok(JudgeGate::Enabled));
        assert_eq!(
            gate_mode(JudgeMode::Auto, false, 2),
            Ok(JudgeGate::Disabled {
                note: Some(
                    "rules: judge auto resolved to off: no judge model with usable credentials. 2 judged rules are not watched.".to_owned()
                ),
            })
        );
    }

    #[test]
    fn first_interrupting_match_pends_and_second_reports() {
        let mut lane = JudgedLane::new();
        assert_eq!(
            lane.matched("r", turn(1), true, 3),
            MatchDecision::JudgePending
        );
        assert_eq!(lane.stops(), 1);
        assert_eq!(
            lane.matched("r", turn(1), true, 3),
            MatchDecision::ReportOnly
        );
        assert_eq!(lane.stops(), 1);
    }

    #[test]
    fn reminding_match_reports_without_a_wait() {
        let mut lane = JudgedLane::new();
        assert_eq!(
            lane.matched("r", turn(1), false, 3),
            MatchDecision::ReportOnly
        );
        assert_eq!(lane.stops(), 0);
        assert_eq!(
            lane.matched("r", turn(1), true, 3),
            MatchDecision::ReportOnly
        );
        assert_eq!(lane.stops(), 0);
    }

    #[test]
    fn waits_cap_at_max_retries_then_report() {
        let mut lane = JudgedLane::new();
        assert_eq!(
            lane.matched("a", turn(2), true, 1),
            MatchDecision::JudgePending
        );
        assert_eq!(
            lane.matched("b", turn(2), true, 1),
            MatchDecision::ReportOnly
        );
        assert_eq!(lane.stops(), 1);
    }

    #[test]
    fn new_turn_rearms_latch_and_budget() {
        let mut lane = JudgedLane::new();
        assert_eq!(
            lane.matched("r", turn(1), true, 1),
            MatchDecision::JudgePending
        );
        assert_eq!(
            lane.matched("r", turn(2), true, 1),
            MatchDecision::JudgePending
        );
        assert_eq!(lane.stops(), 1);
    }

    #[test]
    fn settle_table() {
        assert_eq!(settle(&JudgedEvent::MatchedInterrupts), None);
        assert_eq!(settle(&JudgedEvent::MatchedRemindersOnly), None);
        assert_eq!(
            settle(&JudgedEvent::Verdict(true)),
            Some(SettleDecision::Interrupt)
        );
        assert_eq!(
            settle(&JudgedEvent::Verdict(false)),
            Some(SettleDecision::Allow)
        );
        assert_eq!(
            settle(&JudgedEvent::JudgeFailed("timeout".to_owned())),
            Some(SettleDecision::Allow)
        );
    }

    #[test]
    fn shared_context_order() {
        assert_eq!(
            judge_shared("window tail", "Stop that.", "Is it bad?"),
            "window tail\n\nRule:\nStop that.\n\nQuestion:\nIs it bad?"
        );
    }
}
