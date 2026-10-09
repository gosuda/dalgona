// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
use std::sync::Arc;

use dal_agent::ext::BoxFuture;
use dal_ext::judge::{Gate, JudgeQuestion, Verdict};
use dal_tools::{Rerank, RerankCall, RerankCandidate, RerankOutput};

use super::Battery;
use super::state::{
    FEATURE_SEARCH_RANK, RANK_MAX_CANDIDATES, RANK_PROMPT_BYTES, RANK_SCORE_MAX, cap_utf8,
    normalize_ws,
};

/// The judged reranker supplied to dal's search tools.
pub struct JudgedRerank {
    battery: Arc<Battery>,
}

impl JudgedRerank {
    /// Creates a reranker over the judged battery's session state.
    pub fn new(battery: Arc<Battery>) -> Self {
        Self { battery }
    }
}

impl Rerank for JudgedRerank {
    fn rerank<'a>(&'a self, call: RerankCall<'a>) -> BoxFuture<'a, RerankOutput> {
        Box::pin(async move {
            let Some(order) = identity_order(call.candidates.len()) else {
                return RerankOutput {
                    order: Vec::new(),
                    note: None,
                };
            };
            if !self.battery.cfg.ranking || call.candidates.len() <= 2 {
                return RerankOutput { order, note: None };
            }
            let Some(judge) = self.battery.judge_session(call.session) else {
                return RerankOutput { order, note: None };
            };
            if !matches!(judge.state(), Gate::Ready { .. }) {
                return RerankOutput { order, note: None };
            }

            let take = call.candidates.len().min(RANK_MAX_CANDIDATES);
            let mode: Arc<str> = Arc::from(call.mode);
            let query: Arc<str> = Arc::from(normalize_ws(call.query));
            let key = |index: usize| {
                (
                    Arc::clone(&mode),
                    Arc::clone(&query),
                    Arc::from(call.candidates[index].path.as_ref()),
                )
            };
            let cached = self.battery.with_session(call.session, |state| {
                (0..take)
                    .map(|index| state.memo.get(&key(index)))
                    .collect::<Vec<_>>()
            });

            let mut fresh_keys = Vec::with_capacity(take);
            let mut questions = Vec::with_capacity(take);
            for (index, score) in cached.into_iter().enumerate() {
                if score.is_some() {
                    continue;
                }
                let candidate: &RerankCandidate = &call.candidates[index];
                let Ok(question) = JudgeQuestion::score(
                    cap_utf8(&candidate.display, RANK_PROMPT_BYTES),
                    RANK_SCORE_MAX,
                ) else {
                    continue;
                };
                fresh_keys.push((index, key(index)));
                questions.push(question);
            }

            if !questions.is_empty() {
                let shared = format!(
                    "Search query: {}\nMode: {}\nScore each candidate: 3 = it would change the next action, 2 = it supports it, 1 = it is ambient, 0 = it is irrelevant.",
                    call.query, call.mode
                );
                if let Ok(verdicts) = judge
                    .judge_batch(FEATURE_SEARCH_RANK, &shared, questions, call.turn, None)
                    .await
                    && verdicts.len() == fresh_keys.len()
                {
                    self.battery.with_session(call.session, |state| {
                        for ((_, memo_key), verdict) in fresh_keys.iter().zip(verdicts) {
                            if let Verdict::Score(score) = verdict {
                                state.memo.put(memo_key.clone(), score);
                            }
                        }
                    });
                }
            }

            let scored = self.battery.with_session(call.session, |state| {
                (0..take)
                    .map(|index| state.memo.get(&key(index)))
                    .collect::<Vec<_>>()
            });
            let ranked = rank_order(&scored, call.candidates.len());
            let changed = ranked.iter().enumerate().any(|(index, &ranked_index)| {
                usize::try_from(ranked_index)
                    .ok()
                    .is_none_or(|value| value != index)
            });
            let rescored = scored.iter().filter(|score| score.is_some()).count();
            let note = changed.then(|| {
                Box::from(format!(
                    "judged: re-ranked by relevance; {rescored} of {} candidates rescored",
                    call.candidates.len()
                ))
            });
            RerankOutput {
                order: ranked,
                note,
            }
        })
    }
}

fn identity_order(len: usize) -> Option<Vec<u32>> {
    let mut order = Vec::with_capacity(len);
    for index in 0..len {
        order.push(u32::try_from(index).ok()?);
    }
    Some(order)
}

fn rank_order(scored: &[Option<u8>], total: usize) -> Vec<u32> {
    let Some(mut order) = identity_order(total) else {
        return Vec::new();
    };
    let score_of = |index: usize| {
        if index < scored.len() {
            scored[index].unwrap_or(1)
        } else {
            1
        }
    };
    order.sort_by(|left, right| {
        let left_index = usize::try_from(*left).unwrap_or(usize::MAX);
        let right_index = usize::try_from(*right).unwrap_or(usize::MAX);
        score_of(right_index)
            .cmp(&score_of(left_index))
            .then(left.cmp(right))
    });
    order
}

impl std::fmt::Debug for JudgedRerank {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JudgedRerank")
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::rank_order;

    #[test]
    fn scores_sort_descending_and_keep_equal_indices_stable() {
        assert_eq!(
            rank_order(&[Some(1), Some(3), Some(3), Some(0), Some(2)], 5),
            vec![1, 2, 4, 0, 3]
        );
    }

    #[test]
    fn unscored_tail_keeps_ambient_order() {
        assert_eq!(
            rank_order(&[Some(3), Some(0), None], 5),
            vec![0, 2, 3, 4, 1]
        );
    }

    #[test]
    fn empty_candidates_have_an_empty_permutation() {
        assert!(rank_order(&[], 0).is_empty());
    }
}
