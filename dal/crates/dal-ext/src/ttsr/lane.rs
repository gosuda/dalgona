//! The repetitive-turns lane: a cross-turn detector over settled replies.
//!
//! Each settled turn is reduced to a digest, the set of its lowercased
//! alphanumeric words of three or more characters taken from at most
//! [`LANE_INPUT_MAX_BYTES`] of the reply. The turn's similarity is its
//! greatest Jaccard similarity against the previous [`LANE_WINDOW`] digests
//! of the same session. A run of [`LANE_STREAK`] consecutive turns at
//! [`LANE_SIMILARITY`] or higher fires one `Remind` of the built-in rule
//! [`LANE_RULE`]; the lane then stays latched until a turn below the
//! threshold re-arms it.
//!
//! The lane reads only the settled reply, never in-flight deltas. Its state
//! is at most [`LANE_WINDOW`] bounded digests plus two counters per session,
//! held behind one mutex that is taken for synchronous map work only.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::BuildHasher;
use std::sync::{LazyLock, Mutex, PoisonError};

use dal_core::{SessionId, TurnId};

use super::value::{Name, Origin, RepeatMode, Rule, RuleAction, ScopeSpec, ToolScope, cut_utf8};

/// Least Jaccard similarity that counts a turn as a repeat.
pub const LANE_SIMILARITY: f64 = 0.55;

/// [`LANE_SIMILARITY`] as a fraction: 11 shared words over 20 unioned words.
const SIMILAR_NUM: usize = 11;
const SIMILAR_DEN: usize = 20;

/// Consecutive similar turns, the first included, that fire the lane.
pub const LANE_STREAK: usize = 3;

/// Previous turn digests kept and compared per session.
pub const LANE_WINDOW: usize = 8;

/// The built-in rule name, which is also the fire's `pattern` literal.
pub const LANE_RULE: &str = "repetitive-turns";

/// The built-in reminder body.
pub const LANE_BODY: &str = "You are repeating similar turns. Change approach or ask the user.";

/// Longest reply prefix, in UTF-8 bytes, that a digest is built from, so a
/// session's lane state never grows with the reply size.
pub const LANE_INPUT_MAX_BYTES: usize = 8 * 1024;

/// The extension name that owns the built-in rule; the reminder header shows
/// it as `plugin:ttsr`.
const LANE_OWNER: &str = "ttsr";

static RULE: LazyLock<Rule> = LazyLock::new(|| {
    let Some(name) = Name::parse(LANE_RULE) else {
        unreachable!("the built-in rule name matches the rule name grammar")
    };
    Rule {
        name,
        origin: Origin::Record {
            plugin: LANE_OWNER.into(),
        },
        description: None,
        // The rule names the lane's fires; it watches no stream content, so a
        // set build can never match it.
        conditions: Vec::new(),
        scope: ScopeSpec {
            text: false,
            thinking: false,
            tools: ToolScope::Tools(Vec::new()),
        },
        globs: None,
        agents: None,
        always_apply: false,
        report: false,
        enabled: true,
        interrupt_mode: None,
        // The latch owns repetition; an after-gap of one keeps the gate from
        // blocking the fire that follows a re-arm.
        repeat_mode: Some(RepeatMode::AfterGap),
        repeat_gap: Some(1),
        judge: None,
        body: LANE_BODY.into(),
    }
});

/// Returns the built-in `repetitive-turns` rule.
///
/// The rule never enters a rule set; it names the lane's fires on the shared
/// journal, gate, and text path.
#[must_use]
pub fn lane_rule() -> &'static Rule {
    &RULE
}

/// Reduces a settled reply to its digest: every maximal run of alphanumeric
/// characters of at least three characters, lowercased, taken from the first
/// [`LANE_INPUT_MAX_BYTES`] bytes.
#[must_use]
pub fn digest(reply: &str) -> HashSet<String> {
    let bounded = cut_utf8(reply, LANE_INPUT_MAX_BYTES);
    bounded
        .split(|character: char| !character.is_alphanumeric())
        .filter(|run| run.chars().nth(2).is_some())
        .map(str::to_lowercase)
        .collect()
}

/// Returns the words the two digests share.
fn shared_count<S: BuildHasher>(left: &HashSet<String, S>, right: &HashSet<String, S>) -> usize {
    let (small, large) = if left.len() <= right.len() {
        (left, right)
    } else {
        (right, left)
    };
    small.iter().filter(|word| large.contains(*word)).count()
}

/// Reports whether `shared` words over `union` words reach
/// [`LANE_SIMILARITY`]; two empty digests share no evidence and never do.
fn is_similar(shared: usize, union: usize) -> bool {
    union != 0 && shared * SIMILAR_DEN >= union * SIMILAR_NUM
}

/// Returns the Jaccard similarity of two digests; two empty digests share no
/// evidence and score zero. The lane decides with an exact fraction; this
/// value is for reports.
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    reason = "digest sizes far below 2^52 convert exactly"
)]
pub fn similarity<S: BuildHasher>(left: &HashSet<String, S>, right: &HashSet<String, S>) -> f64 {
    let shared = shared_count(left, right);
    let union = left.len() + right.len() - shared;
    if union == 0 {
        return 0.0;
    }
    shared as f64 / union as f64
}

/// One lane fire: a `Remind` of [`lane_rule`] on the settled turn. The parent
/// renders it with `FireText::new(lane_rule(), LANE_RULE, reply subject,
/// RuleAction::Remind)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LaneFire {
    /// The settled turn that completed the streak.
    pub turn: TurnId,
    /// The turn's greatest similarity against the window.
    pub similarity: f64,
}

impl LaneFire {
    /// Returns the fire's action, always [`RuleAction::Remind`].
    #[must_use]
    pub const fn action(self) -> RuleAction {
        RuleAction::Remind
    }
}

/// One session's lane state.
///
/// Invariants: `digests.len() <= LANE_WINDOW`; `1 <= streak <= LANE_STREAK`
/// once a turn is seen; `latched` implies a fire since the last re-arm.
#[derive(Debug, Default)]
pub struct LaneWindow {
    digests: VecDeque<HashSet<String>>,
    streak: usize,
    latched: bool,
    last: Option<TurnId>,
}

impl LaneWindow {
    /// Folds one settled turn into the window and returns its fire, if any.
    ///
    /// A settled event for a turn at or before the newest seen turn changes
    /// nothing, so a replay can never add a digest twice.
    #[must_use]
    pub fn observe(&mut self, turn: TurnId, digest: HashSet<String>) -> Option<LaneFire> {
        if self.last.is_some_and(|last| turn <= last) {
            return None;
        }
        self.last = Some(turn);
        let (similar, best) =
            self.digests
                .iter()
                .fold((false, 0.0), |(similar, best), previous| {
                    let shared = shared_count(&digest, previous);
                    let union = digest.len() + previous.len() - shared;
                    (
                        similar || is_similar(shared, union),
                        f64::max(best, similarity(&digest, previous)),
                    )
                });
        if similar {
            self.streak = (self.streak + 1).min(LANE_STREAK);
        } else {
            self.streak = 1;
            self.latched = false;
        }
        if self.digests.len() == LANE_WINDOW {
            self.digests.pop_front();
        }
        self.digests.push_back(digest);
        if self.streak < LANE_STREAK || self.latched {
            return None;
        }
        self.latched = true;
        Some(LaneFire {
            turn,
            similarity: best,
        })
    }
}

/// The lane windows of every open session, keyed by session id.
///
/// The mutex guards synchronous map work only and is never held across an
/// await; the digest is built before the lock is taken.
#[derive(Debug, Default)]
pub struct Lanes {
    windows: Mutex<HashMap<SessionId, LaneWindow>>,
}

impl Lanes {
    /// Creates an empty lane with no session state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Observes the settled reply of `turn` in `session`; call it once per
    /// settled event, with the reply text of the settled payload.
    #[must_use]
    pub fn observe(&self, session: SessionId, turn: TurnId, reply: &str) -> Option<LaneFire> {
        let digest = digest(reply);
        self.windows
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(session)
            .or_default()
            .observe(turn, digest)
    }

    /// Evicts the session's window at `session_end`; an unknown session is a
    /// no-op.
    pub fn end_session(&self, session: SessionId) {
        self.windows
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&session);
    }

    /// Returns the number of sessions holding lane state.
    #[must_use]
    pub fn session_count(&self) -> usize {
        self.windows
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::num::NonZeroU64;

    use dal_core::{SessionId, TurnId};

    use super::{
        LANE_BODY, LANE_INPUT_MAX_BYTES, LANE_RULE, LANE_SIMILARITY, LANE_WINDOW, LaneWindow,
        Lanes, digest, is_similar, lane_rule, similarity,
    };
    use crate::ttsr::value::{RuleAction, cut_utf8};

    const BUILD: [&str; 5] = [
        "I will run the cargo build again and check the failing test output.",
        "Let me run the cargo build again and check the failing test output now.",
        "I will run the cargo build again, then check the failing test output.",
        "Running the cargo build again to check the failing test output.",
        "I will run the cargo build again and check the failing test output once more.",
    ];
    const DOCS: &str = "The README describes installation steps for Linux and macOS users.";
    const PARSER: [&str; 3] = [
        "Now patch the parser module so tokens carry their byte spans.",
        "Next patch the parser module so tokens carry their byte spans too.",
        "Patch the parser module again so tokens carry their byte spans.",
    ];

    fn turn(value: u64) -> TurnId {
        TurnId::new(NonZeroU64::new(value).unwrap())
    }

    fn feed(lanes: &Lanes, session: SessionId, replies: &[&str]) -> Vec<u64> {
        replies
            .iter()
            .zip(1..)
            .filter_map(|(reply, index)| lanes.observe(session, turn(index), reply))
            .map(|fire| fire.turn.get())
            .collect()
    }

    fn words(list: &[&str]) -> HashSet<String> {
        list.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn fixtures_hold_their_similarity_premises() {
        for (index, left) in BUILD.iter().enumerate() {
            for right in &BUILD[index + 1..] {
                assert!(similarity(&digest(left), &digest(right)) >= LANE_SIMILARITY);
            }
            assert!(similarity(&digest(left), &digest(DOCS)) < LANE_SIMILARITY);
            for parser in PARSER {
                assert!(similarity(&digest(left), &digest(parser)) < LANE_SIMILARITY);
            }
        }
        for parser in PARSER {
            assert!(similarity(&digest(DOCS), &digest(parser)) < LANE_SIMILARITY);
        }
        assert!(similarity(&digest(PARSER[0]), &digest(PARSER[1])) >= LANE_SIMILARITY);
        assert!(similarity(&digest(PARSER[1]), &digest(PARSER[2])) >= LANE_SIMILARITY);
    }

    #[test]
    fn repetitive_turns_lane_fire() {
        let lanes = Lanes::new();
        let session = SessionId::new_v7();
        let replies = [
            BUILD[0], BUILD[1], BUILD[2], DOCS, PARSER[0], PARSER[1], PARSER[2],
        ];
        let fires: Vec<_> = replies
            .iter()
            .zip(1..)
            .filter_map(|(reply, index)| lanes.observe(session, turn(index), reply))
            .collect();

        assert_eq!(
            fires.iter().map(|fire| fire.turn.get()).collect::<Vec<_>>(),
            [3, 7]
        );
        for fire in &fires {
            assert!(fire.similarity >= LANE_SIMILARITY);
            assert_eq!(fire.action(), RuleAction::Remind);
        }
    }

    #[test]
    fn latch_suppresses_similar_turns_after_the_fire() {
        let lanes = Lanes::new();
        let session = SessionId::new_v7();
        assert_eq!(feed(&lanes, session, &BUILD), [3]);
    }

    #[test]
    fn dissimilar_turn_restarts_the_streak() {
        let lanes = Lanes::new();
        let session = SessionId::new_v7();
        let replies = [BUILD[0], BUILD[1], DOCS, BUILD[2], BUILD[3]];
        assert_eq!(feed(&lanes, session, &replies), [5]);
    }

    #[test]
    fn sessions_are_independent() {
        let lanes = Lanes::new();
        let first = SessionId::new_v7();
        let second = SessionId::new_v7();
        assert!(lanes.observe(first, turn(1), BUILD[0]).is_none());
        assert!(lanes.observe(first, turn(2), BUILD[1]).is_none());
        assert!(lanes.observe(second, turn(1), BUILD[0]).is_none());
        assert!(lanes.observe(first, turn(3), BUILD[2]).is_some());
        assert!(lanes.observe(second, turn(2), DOCS).is_none());
        assert!(lanes.observe(second, turn(3), BUILD[1]).is_none());
        assert_eq!(lanes.session_count(), 2);
    }

    #[test]
    fn end_session_evicts_and_restarts_clean() {
        let lanes = Lanes::new();
        let session = SessionId::new_v7();
        let other = SessionId::new_v7();
        assert_eq!(feed(&lanes, session, &BUILD[..3]), [3]);
        assert!(lanes.observe(other, turn(1), DOCS).is_none());

        lanes.end_session(session);
        assert_eq!(lanes.session_count(), 1);
        lanes.end_session(session);
        assert_eq!(lanes.session_count(), 1);

        assert_eq!(feed(&lanes, session, &BUILD[..3]), [3]);
        lanes.end_session(other);
        lanes.end_session(session);
        assert_eq!(lanes.session_count(), 0);
    }

    #[test]
    fn replayed_settled_events_change_nothing() {
        let mut window = LaneWindow::default();
        assert!(window.observe(turn(1), digest(BUILD[0])).is_none());
        assert!(window.observe(turn(2), digest(BUILD[1])).is_none());
        assert!(window.observe(turn(2), digest(BUILD[1])).is_none());
        // A stale replay of the first turn must not add a digest or advance
        // the streak.
        assert!(window.observe(turn(1), digest(BUILD[0])).is_none());
        assert_eq!(window.digests.len(), 2);
        assert_eq!(window.streak, 2);
        assert_eq!(
            window
                .observe(turn(3), digest(BUILD[2]))
                .map(|fire| fire.turn),
            Some(turn(3))
        );
    }

    #[test]
    fn window_holds_at_most_eight_digests() {
        let mut window = LaneWindow::default();
        for index in 1..=40 {
            let reply = format!("distinct{index} alpha{index} beta{index}");
            assert!(window.observe(turn(index), digest(&reply)).is_none());
            assert!(window.digests.len() <= LANE_WINDOW);
        }
        assert_eq!(window.digests.len(), LANE_WINDOW);
        assert!(
            window
                .digests
                .contains(&digest("distinct40 alpha40 beta40"))
        );
        assert!(
            !window
                .digests
                .contains(&digest("distinct32 alpha32 beta32"))
        );
    }

    #[test]
    fn empty_replies_never_fire() {
        let lanes = Lanes::new();
        let session = SessionId::new_v7();
        assert!(feed(&lanes, session, &["", "", "", "", "a b !?", ""]).is_empty());
    }

    #[test]
    fn digest_reads_at_most_the_input_bound() {
        let long = DOCS.repeat(300);
        assert!(long.len() > LANE_INPUT_MAX_BYTES);
        let cut = cut_utf8(&long, LANE_INPUT_MAX_BYTES);
        assert_eq!(cut.len(), LANE_INPUT_MAX_BYTES);
        assert_eq!(digest(&long), digest(cut));
        assert_eq!(
            digest("Straße ÜBER über, 日本語! ab été_x Run42 run42"),
            words(&["straße", "über", "日本語", "été", "run42"])
        );
        assert!(digest("a bc -- ?? 12").is_empty());
        assert!(digest("").is_empty());
    }

    #[test]
    fn threshold_is_inclusive_exactly() {
        assert!(is_similar(11, 20));
        assert!(!is_similar(10, 20));
        assert!(!is_similar(0, 0));

        // Turn 2 against turn 1 sits exactly at 11 shared over 20 unioned.
        let shared: Vec<String> = (0..11).map(|index| format!("shared{index}")).collect();
        let mut left: HashSet<String> = shared.iter().cloned().collect();
        let mut right = left.clone();
        left.extend((0..5).map(|index| format!("left{index}")));
        right.extend((0..4).map(|index| format!("right{index}")));
        let mut window = LaneWindow::default();
        assert!(window.observe(turn(1), left.clone()).is_none());
        assert!(window.observe(turn(2), right).is_none());
        assert_eq!(
            window.observe(turn(3), left).map(|fire| fire.turn),
            Some(turn(3))
        );

        // One fewer shared word breaks the run at turn 2.
        let mut left: HashSet<String> = shared.iter().take(10).cloned().collect();
        let mut right = left.clone();
        left.extend((0..5).map(|index| format!("left{index}")));
        right.extend((0..5).map(|index| format!("right{index}")));
        let mut window = LaneWindow::default();
        assert!(window.observe(turn(1), left.clone()).is_none());
        assert!(window.observe(turn(2), right).is_none());
        assert!(window.observe(turn(3), left).is_none());
    }

    #[test]
    fn built_in_rule_carries_the_behavior_text() {
        let rule = lane_rule();
        assert_eq!(rule.name.as_str(), LANE_RULE);
        assert_eq!(rule.body, LANE_BODY);
        assert!(rule.judge.is_none());
        assert!(!rule.report);
        assert!(rule.conditions.is_empty());
        assert!(!rule.scope.reaches_any());
    }
}
