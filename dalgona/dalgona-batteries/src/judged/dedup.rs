// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
use std::collections::HashSet;

use dal_core::{SessionId, TurnId};
use dal_ext::judge::{Gate, Judge, JudgeQuestion, Verdict};

use super::Battery;
use super::state::{
    CTX_SHARED_BYTES, DEDUP_JACCARD_MILLI, DEDUP_MAX_CALLS_PER_TURN, DEDUP_SHINGLE_WORDS,
    DEDUP_WINDOW_TURNS, FEATURE_PROMPT_DEDUP, Injection, SHARED_TEXT_BYTES,
};
use crate::text::byte_prefix;

/// The reminder admission decision made by the deterministic and judged filters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Admission {
    /// The reminder may be delivered.
    Admit,
    /// The reminder duplicates content already delivered in this session.
    Refuse,
}

/// A byte that never occurs in a valid UTF-8 word, so word boundaries hash distinctly.
const WORD_SEPARATOR: u8 = 0xff;

/// Returns the FNV-1a hash of every `width`-word shingle in `text`.
pub(super) fn shingles(text: &str, width: usize) -> Vec<u64> {
    if width == 0 {
        return Vec::new();
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() < width {
        return Vec::new();
    }
    (0..=words.len() - width)
        .map(|start| {
            let mut hash = 0xcbf2_9ce4_8422_2325_u64;
            for word in &words[start..start + width] {
                for byte in word.bytes() {
                    hash ^= u64::from(byte);
                    hash = hash.wrapping_mul(0x1000_0000_01b3);
                }
                hash ^= u64::from(WORD_SEPARATOR);
                hash = hash.wrapping_mul(0x1000_0000_01b3);
            }
            hash
        })
        .collect()
}

fn jaccard_milli(left: &[u64], right: &[u64]) -> u64 {
    let unique_left: HashSet<u64> = left.iter().copied().collect();
    let mut unique_right: HashSet<u64> = right.iter().copied().collect();
    let intersection = unique_left
        .iter()
        .filter(|shingle| unique_right.remove(shingle))
        .count();
    let union = unique_left.len() + unique_right.len();
    if union == 0 {
        return 0;
    }
    (intersection as u64 * 1_000) / union as u64
}

fn same_normalized_words(left: &str, right: &str) -> bool {
    left.split_whitespace().eq(right.split_whitespace())
}

/// The outstanding injection of `kind` closest to the candidate by shingle
/// Jaccard. The newest wins a tie, including the case where neither side has
/// a full shingle.
fn closest_outstanding<'a>(
    injections: &'a std::collections::VecDeque<Injection>,
    kind: &str,
    candidate: &[u64],
) -> Option<&'a Injection> {
    let mut best: Option<(u64, &Injection)> = None;
    for injection in injections
        .iter()
        .rev()
        .filter(|injection| injection.kind == kind)
    {
        let distance = jaccard_milli(candidate, &injection.shingles);
        if best.is_none_or(|(best_distance, _)| distance > best_distance) {
            best = Some((distance, injection));
        }
    }
    best.map(|(_, injection)| injection)
}

impl Battery {
    /// Applies exact and shingle-based duplicate checks to one reminder channel.
    pub fn prefilter(&self, session: SessionId, kind: &'static str, text: &str) -> Admission {
        let candidate_shingles = shingles(text, DEDUP_SHINGLE_WORDS);
        self.with_session(session, |state| {
            let current_turn = state.turn_no;
            state.injections.retain(|injection| {
                current_turn.saturating_sub(injection.turn) <= DEDUP_WINDOW_TURNS
            });
            let duplicate = state
                .injections
                .iter()
                .filter(|injection| injection.kind == kind)
                .rev()
                .any(|injection| {
                    same_normalized_words(&injection.text, text)
                        || jaccard_milli(&candidate_shingles, &injection.shingles)
                            >= DEDUP_JACCARD_MILLI
                });
            if duplicate {
                Admission::Refuse
            } else {
                Admission::Admit
            }
        })
    }

    /// The text of the outstanding injection of `kind` closest to `text`.
    fn closest_injection_text(&self, session: SessionId, kind: &str, text: &str) -> Option<String> {
        let candidate_shingles = shingles(text, DEDUP_SHINGLE_WORDS);
        self.with_session(session, |state| {
            closest_outstanding(&state.injections, kind, &candidate_shingles)
                .map(|injection| injection.text.clone())
        })
    }

    /// Runs the deterministic filter before the bounded judge-based reminder filter.
    pub async fn admit(
        &self,
        session: SessionId,
        kind: &'static str,
        text: &str,
        turn: Option<TurnId>,
        judge: &Judge,
    ) -> Admission {
        if self.prefilter(session, kind, text) == Admission::Refuse {
            return Admission::Refuse;
        }
        if !self.cfg.dedup || !matches!(judge.state(), Gate::Ready { .. }) {
            return Admission::Admit;
        }
        let Some(old) = self.closest_injection_text(session, kind, text) else {
            return Admission::Admit;
        };
        self.consult(session, &old, text, turn, judge).await
    }

    /// Judges a borderline reminder once, charging the cap to its owning session.
    pub async fn consult(
        &self,
        session: SessionId,
        old: &str,
        text: &str,
        turn: Option<TurnId>,
        judge: &Judge,
    ) -> Admission {
        if same_normalized_words(old, text) {
            return Admission::Refuse;
        }
        let should_consult = self.with_session(session, |state| {
            if state.dedup_calls_turn != turn {
                state.dedup_calls_turn = turn;
                state.dedup_calls_this_turn = 0;
            }
            if state.dedup_calls_this_turn >= DEDUP_MAX_CALLS_PER_TURN {
                return false;
            }
            state.dedup_calls_this_turn += 1;
            true
        });
        if !should_consult {
            return Admission::Admit;
        }

        let shared = format!(
            "Already injected:\n<<<OLD\n{}\nOLD>>>\n\nCandidate:\n<<<NEW\n{}\nNEW>>>",
            byte_prefix(old, SHARED_TEXT_BYTES),
            byte_prefix(text, CTX_SHARED_BYTES),
        );
        let Ok(question) = JudgeQuestion::bool(
            "Does the candidate text in the shared context repeat advice the model already has?",
        ) else {
            return Admission::Admit;
        };
        match judge
            .judge(FEATURE_PROMPT_DEDUP, &shared, question, turn, None)
            .await
        {
            Ok(Verdict::Bool(true)) => Admission::Refuse,
            _ => Admission::Admit,
        }
    }
}

#[cfg(test)]
mod tests {
    use dal_core::SessionId;

    use super::super::{Battery, JudgedConfig};
    use super::{Admission, jaccard_milli, same_normalized_words, shingles};

    const KIND: &str = "claim-check";

    fn words(count: usize) -> String {
        (0..count)
            .map(|index| format!("w{index}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn with_word(text: &str, index: usize, replacement: &str) -> String {
        text.split(' ')
            .enumerate()
            .map(|(position, word)| if position == index { replacement } else { word })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// A battery whose session injected `text` at turn `injected` and now sits at `now`.
    fn injected(text: &str, injected: u64, now: u64) -> (Battery, SessionId) {
        let battery = Battery::new(JudgedConfig::default());
        let session = SessionId::new_v7();
        battery.with_session(session, |state| {
            state.turn_no = injected;
            state.record_injection(KIND, text);
            state.turn_no = now;
        });
        (battery, session)
    }

    #[test]
    fn a_whitespace_only_duplicate_is_refused_with_no_judge() {
        let text = words(12);
        let (battery, session) = injected(&text, 1, 3);
        let respaced = text.replace(' ', " \n\t  ");
        assert_eq!(
            battery.prefilter(session, KIND, &respaced),
            Admission::Refuse
        );
    }

    #[test]
    fn a_duplicate_of_another_kind_is_admitted() {
        let text = words(12);
        let (battery, session) = injected(&text, 1, 3);
        assert_eq!(
            battery.prefilter(session, "other-kind", &text),
            Admission::Admit
        );
    }

    #[test]
    fn shingle_jaccard_refuses_at_ninety_percent_and_admits_below() {
        let text = words(30);
        let (battery, session) = injected(&text, 1, 2);
        let near = with_word(&text, 29, "changed");
        assert_eq!(
            battery.prefilter(session, KIND, &near),
            Admission::Refuse,
            "a changed tail word keeps 22 of 24 shingles"
        );
        let far = with_word(&text, 14, "changed");
        assert_eq!(
            battery.prefilter(session, KIND, &far),
            Admission::Admit,
            "a changed middle word keeps only 15 of 31 shingles"
        );
    }

    #[test]
    fn an_injection_expires_after_fifty_turns() {
        let text = words(12);
        let (battery, session) = injected(&text, 1, 51);
        assert_eq!(
            battery.prefilter(session, KIND, &text),
            Admission::Refuse,
            "fifty turns later it is still outstanding"
        );
        let (battery, session) = injected(&text, 1, 52);
        assert_eq!(
            battery.prefilter(session, KIND, &text),
            Admission::Admit,
            "at turn 52 the identical candidate is admitted without a consult"
        );
    }

    fn battery_with(injections: &[(&'static str, &str)]) -> (Battery, SessionId) {
        let battery = Battery::new(JudgedConfig::default());
        let session = SessionId::new_v7();
        battery.with_session(session, |state| {
            for (kind, text) in injections {
                state.record_injection(kind, text);
            }
        });
        (battery, session)
    }

    #[test]
    fn the_closest_outstanding_injection_beats_the_newest() {
        let near = words(20);
        let far = (100..120)
            .map(|index| format!("x{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        let (battery, session) = battery_with(&[(KIND, &near), (KIND, &far)]);
        let candidate = with_word(&near, 19, "changed");
        assert_eq!(
            battery
                .closest_injection_text(session, KIND, &candidate)
                .as_deref(),
            Some(near.as_str()),
            "similarity wins over recency"
        );
    }

    #[test]
    fn a_tie_goes_to_the_newest_injection() {
        let (battery, session) = battery_with(&[(KIND, "short one"), (KIND, "short two")]);
        assert_eq!(
            battery
                .closest_injection_text(session, KIND, "short")
                .as_deref(),
            Some("short two")
        );
    }

    #[test]
    fn other_kinds_are_never_the_closest() {
        let text = words(20);
        let (battery, session) = battery_with(&[("other-kind", &text)]);
        assert_eq!(battery.closest_injection_text(session, KIND, &text), None);
    }

    #[test]
    fn jaccard_uses_the_set_union() {
        let left = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let right = [1, 2, 3, 4, 5, 6, 7, 8, 9, 11];
        assert_eq!(jaccard_milli(&left, &right), 818);
    }

    #[test]
    fn shingle_hashes_preserve_word_boundaries() {
        assert_ne!(shingles("ab c", 2), shingles("a bc", 2));
    }

    #[test]
    fn fewer_words_than_the_shingle_width_has_no_shingles() {
        assert_eq!(shingles("one two", 8).len(), 0);
    }

    #[test]
    fn whitespace_normalization_matches_the_same_words() {
        assert!(same_normalized_words("one  two\nthree", "one two three"));
    }

    #[test]
    fn different_words_do_not_match_after_normalization() {
        assert!(!same_normalized_words("one two", "one three"));
    }
}
