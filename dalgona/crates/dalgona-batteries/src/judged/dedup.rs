use std::collections::HashSet;

use dal_core::{SessionId, TurnId};
use dal_ext::judge::{Gate, Judge, JudgeQuestion, Verdict};

use super::Battery;
use super::state::{
    CTX_SHARED_BYTES, DEDUP_JACCARD_MILLI, DEDUP_MAX_CALLS_PER_TURN, DEDUP_SHINGLE_WORDS,
    DEDUP_WINDOW_TURNS, FEATURE_PROMPT_DEDUP, SHARED_TEXT_BYTES, cap_utf8,
};

/// The reminder admission decision made by the deterministic and judged filters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Admission {
    /// The reminder may be delivered.
    Admit,
    /// The reminder duplicates content already delivered in this session.
    Refuse,
}

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
                // 0xff cannot occur in a valid UTF-8 word, so boundaries remain distinct.
                hash ^= 0xff;
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
        let old = self.with_session(session, |state| {
            state
                .injections
                .iter()
                .rev()
                .find(|injection| injection.kind == kind)
                .map(|injection| injection.text.clone())
        });
        let Some(old) = old else {
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
            cap_utf8(old, SHARED_TEXT_BYTES),
            cap_utf8(text, CTX_SHARED_BYTES),
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
    use super::{jaccard_milli, same_normalized_words, shingles};

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
        assert!(shingles("one two", 8).is_empty());
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
