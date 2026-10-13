// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use dal_core::{Part, ThinkingLevel, TurnId};
use dal_ext::judge::Judge;

pub(super) const CLASSIFY_WAIT_MS: u64 = 2_000;
pub(super) const THINKING_SHARED_BYTES: usize = 4_000;
pub(super) const DIGEST_ASSISTANT_BYTES: usize = 16_384;
pub(super) const DIGEST_TOOL_PREVIEWS: usize = 8;
pub(super) const DIGEST_PREVIEW_BYTES: usize = 4_096;
pub(super) const CLAIM_MIN_REPLY_BYTES: usize = 512;
pub(super) const CLAIM_REMINDER_BYTES: usize = 400;
pub(super) const SHARED_TEXT_BYTES: usize = 8_000;
pub(super) const CTX_SHARED_BYTES: usize = 4_000;
pub(super) const RANK_MAX_CANDIDATES: usize = 20;
pub(super) const RANK_SCORE_MAX: u8 = 3;
pub(super) const RANK_PROMPT_BYTES: usize = 200;
pub(super) const RANK_MEMO_ENTRIES: usize = 512;
pub(super) const DEDUP_SHINGLE_WORDS: usize = 8;
pub(super) const DEDUP_JACCARD_MILLI: u64 = 900;
pub(super) const DEDUP_WINDOW_TURNS: u64 = 50;
pub(super) const DEDUP_MAX_CALLS_PER_TURN: u64 = 4;
pub(super) const THINKING_BREAKER_STREAK: u32 = 3;

pub(super) const FEATURE_THINKING: &str = "thinking";
pub(super) const FEATURE_SEARCH_RANK: &str = "search-rank";
pub(super) const FEATURE_ASK_ANCHOR: &str = "ask-anchor";
pub(super) const FEATURE_CLAIM_CHECK: &str = "claim-check";
pub(super) const FEATURE_PROMPT_DEDUP: &str = "prompt-dedup";
pub(super) const CHANNEL_CLAIM_CHECK: &str = FEATURE_CLAIM_CHECK;

pub(super) const BREAKER_NOTICE: &str =
    "judged: auto-thinking disabled for this session after 3 failed classifications.";
pub(super) const CLAIM_REMINDER: &str = "Your previous reply stated facts with confidence. Treat them as claims, not evidence: verify each before you build on it, or say plainly that it is unverified.";

/// Returns the longest prefix of `text` no longer than `limit` bytes.
pub(super) fn cap_utf8(text: &str, limit: usize) -> &str {
    if text.len() <= limit {
        return text;
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Collapses whitespace runs to one ASCII space and trims both ends.
pub(super) fn normalize_ws(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    for word in text.split_whitespace() {
        if !normalized.is_empty() {
            normalized.push(' ');
        }
        normalized.push_str(word);
    }
    normalized
}

/// A bounded, hook-observed view of the current session's recent context.
pub(super) struct Digest {
    user: String,
    assistant: String,
    previews: VecDeque<String>,
}

impl Default for Digest {
    fn default() -> Self {
        Self {
            user: String::with_capacity(THINKING_SHARED_BYTES),
            assistant: String::with_capacity(DIGEST_ASSISTANT_BYTES),
            previews: VecDeque::with_capacity(DIGEST_TOOL_PREVIEWS),
        }
    }
}

impl Digest {
    pub(super) fn observe_user(&mut self, content: &[Part]) {
        self.user.clear();
        for part in content {
            let Part::Text { text } = part else {
                continue;
            };
            let remaining = THINKING_SHARED_BYTES.saturating_sub(self.user.len());
            if remaining == 0 {
                break;
            }
            self.user.push_str(cap_utf8(text, remaining));
        }
    }

    pub(super) fn observe_assistant_delta(&mut self, delta: &str) {
        if delta.len() >= DIGEST_ASSISTANT_BYTES {
            let mut start = delta.len() - DIGEST_ASSISTANT_BYTES;
            while !delta.is_char_boundary(start) {
                start += 1;
            }
            self.assistant.clear();
            self.assistant.push_str(&delta[start..]);
            return;
        }

        let overflow = self
            .assistant
            .len()
            .saturating_add(delta.len())
            .saturating_sub(DIGEST_ASSISTANT_BYTES);
        if overflow > 0 {
            let mut start = overflow;
            while !self.assistant.is_char_boundary(start) {
                start += 1;
            }
            self.assistant.drain(..start);
        }
        self.assistant.push_str(delta);
    }

    pub(super) fn observe_preview(&mut self, preview: &str) {
        if self.previews.len() == DIGEST_TOOL_PREVIEWS {
            self.previews.pop_front();
        }
        self.previews
            .push_back(cap_utf8(preview, DIGEST_PREVIEW_BYTES).to_owned());
    }

    pub(super) fn user_text(&self) -> &str {
        &self.user
    }

    pub(super) fn assistant_text(&self) -> &str {
        &self.assistant
    }

    /// Joins user text, assistant text, then previews within the byte budget.
    pub(super) fn render(&self, budget: usize) -> String {
        let mut rendered = String::new();
        let mut first = true;
        for text in std::iter::once(self.user.as_str())
            .chain(std::iter::once(self.assistant.as_str()))
            .chain(self.previews.iter().map(String::as_str))
        {
            if text.is_empty() {
                continue;
            }
            if !first {
                if budget.saturating_sub(rendered.len()) < 2 {
                    break;
                }
                rendered.push_str("\n\n");
            }
            let remaining = budget.saturating_sub(rendered.len());
            let excerpt = cap_utf8(text, remaining);
            rendered.push_str(excerpt);
            if excerpt.len() < text.len() {
                break;
            }
            first = false;
        }
        rendered
    }

    pub(super) fn reset_assistant(&mut self) {
        self.assistant.clear();
    }
}

/// Whether automatic thinking classification remains active for this session.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum Latch {
    #[default]
    Auto,
    Manual,
    Disabled,
}

impl Latch {
    pub(super) fn classification_armed(self) -> bool {
        self == Self::Auto
    }

    pub(super) fn observe_user_level(&mut self) {
        if *self == Self::Auto {
            *self = Self::Manual;
        }
    }

    pub(super) fn observe_breaker(&mut self) {
        if *self == Self::Auto {
            *self = Self::Disabled;
        }
    }
}

/// One delivered reminder and its cached shingles, recorded at delivery time.
pub(super) struct Injection {
    pub(super) kind: &'static str,
    pub(super) text: String,
    pub(super) turn: u64,
    pub(super) shingles: Vec<u64>,
}

/// One ranked path's memoized score, ordered from least to most recently used.
pub(super) struct LruMemo {
    map: HashMap<MemoKey, u8>,
    order: VecDeque<MemoKey>,
}

pub(super) type MemoKey = (Arc<str>, Arc<str>, Arc<str>);

impl Default for LruMemo {
    fn default() -> Self {
        Self {
            map: HashMap::with_capacity(RANK_MEMO_ENTRIES),
            order: VecDeque::with_capacity(RANK_MEMO_ENTRIES),
        }
    }
}

impl LruMemo {
    pub(super) fn get(&mut self, key: &MemoKey) -> Option<u8> {
        let value = *self.map.get(key)?;
        if let Some(position) = self.order.iter().position(|queued| queued == key)
            && let Some(recent) = self.order.remove(position)
        {
            self.order.push_back(recent);
        }
        Some(value)
    }

    pub(super) fn put(&mut self, key: MemoKey, value: u8) {
        if let Some(position) = self.order.iter().position(|queued| queued == &key) {
            self.order.remove(position);
            self.map.remove(&key);
        } else if self.map.len() == RANK_MEMO_ENTRIES
            && let Some(oldest) = self.order.pop_front()
        {
            self.map.remove(&oldest);
        }
        self.map.insert(key.clone(), value);
        self.order.push_back(key);
    }
}

/// Mutable state owned by one session's judged battery instance.
pub(super) struct SessionState {
    pub(super) is_child: bool,
    pub(super) latch: Latch,
    pub(super) breaker_streak: u32,
    pub(super) breaker_notice_sent: bool,
    pub(super) turn_no: u64,
    pub(super) digest: Digest,
    pub(super) judge: Option<Judge>,
    pub(super) turn_thinking: Option<(TurnId, ThinkingLevel)>,
    pub(super) parked: Option<String>,
    pub(super) injections: VecDeque<Injection>,
    pub(super) memo: LruMemo,
    pub(super) dedup_calls_turn: Option<TurnId>,
    pub(super) dedup_calls_this_turn: u64,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            is_child: false,
            latch: Latch::Auto,
            breaker_streak: 0,
            breaker_notice_sent: false,
            turn_no: 1,
            digest: Digest::default(),
            judge: None,
            turn_thinking: None,
            parked: None,
            injections: VecDeque::new(),
            memo: LruMemo::default(),
            dedup_calls_turn: None,
            dedup_calls_this_turn: 0,
        }
    }
}

impl SessionState {
    pub(super) fn record_injection(&mut self, kind: &'static str, text: &str) {
        self.injections.push_back(Injection {
            kind,
            text: text.to_owned(),
            turn: self.turn_no,
            shingles: super::dedup::shingles(text, DEDUP_SHINGLE_WORDS),
        });
    }
}
