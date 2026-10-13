// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use std::collections::{HashSet, VecDeque};

const HISTORY: usize = 6;
const RESTORE: usize = 8;
const MIN_CHARS: usize = 40;
const JACCARD_FIRE: f64 = 0.55;
const STREAK_FIRE: u32 = 3;
const MAX_TEXT_BYTES: usize = 64 * 1024;

#[derive(Debug)]
struct Turn {
    grams: HashSet<String>,
}

/// Rolling window of settled turn texts that detects a streak of near-identical turns.
#[derive(Debug, Default)]
pub struct TurnHistory {
    texts: VecDeque<Turn>,
    streak: u32,
    latched: bool,
}

/// A repetitive-turn verdict.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fire {
    /// Similarity of the settled turn to the one before it.
    pub jaccard: f64,
}

impl TurnHistory {
    /// Rebuilds the window from the most recent journaled turn texts.
    #[must_use]
    pub fn restore(texts: &[String]) -> Self {
        let mut history = Self::default();
        let keep_from = texts.len().saturating_sub(RESTORE);
        for text in &texts[keep_from..] {
            history.commit_inner(text);
        }
        history
    }

    /// Adds a settled turn; fires once when the third similar turn in a row arrives.
    #[must_use]
    pub fn commit(&mut self, settled_text: &str) -> Option<Fire> {
        self.commit_inner(settled_text)
    }

    /// Clears the window and the streak.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    fn commit_inner(&mut self, text: &str) -> Option<Fire> {
        let normalized = normalize(text);
        if normalized.chars().count() < MIN_CHARS {
            return None;
        }

        let grams = word_trigrams(&normalized);
        let similarity = match self.texts.back() {
            Some(previous) => jaccard(&grams, &previous.grams)?,
            None => 0.0,
        };
        self.streak = if similarity >= JACCARD_FIRE {
            self.streak.saturating_add(1)
        } else {
            self.latched = false;
            1
        };

        self.texts.push_back(Turn { grams });
        if self.texts.len() > HISTORY {
            self.texts.pop_front();
        }
        if self.latched || self.streak < STREAK_FIRE {
            return None;
        }
        self.latched = true;
        Some(Fire {
            jaccard: similarity,
        })
    }
}

#[must_use]
pub(crate) fn normalize(text: &str) -> String {
    let end = bounded_end(text);
    let chars: Vec<char> = text[..end].chars().collect();
    let mut output = NormalizedText {
        value: String::with_capacity(end),
        pending_space: false,
    };
    let mut index = 0;

    while index < chars.len() {
        let ch = chars[index];
        if !is_hex(ch) {
            append_scalar(&mut output, ch);
            index += 1;
            continue;
        }
        let run_end = hex_run_end(&chars, index);
        if is_hex_identifier(&chars, index, run_end) {
            output.push_non_space('#');
        } else {
            append_unmatched_hex(&mut output, &chars[index..run_end]);
        }
        index = run_end;
    }
    output.value
}

fn bounded_end(text: &str) -> usize {
    let mut end = text.len().min(MAX_TEXT_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

fn append_scalar(output: &mut NormalizedText, ch: char) {
    if ch.is_ascii_digit() {
        output.push_non_space('#');
        return;
    }
    if ch.is_whitespace() {
        output.push_space();
        return;
    }
    output.push_lowercase(ch);
}

fn hex_run_end(chars: &[char], mut index: usize) -> usize {
    while index < chars.len() && is_hex(chars[index]) {
        index += 1;
    }
    index
}

fn is_hex_identifier(chars: &[char], start: usize, end: usize) -> bool {
    end - start >= 7
        && (start == 0 || !chars[start - 1].is_alphanumeric())
        && (end == chars.len() || !chars[end].is_alphanumeric())
}

fn append_unmatched_hex(output: &mut NormalizedText, chars: &[char]) {
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if !ch.is_ascii_digit() {
            output.push_lowercase(ch);
            index += 1;
            continue;
        }
        output.push_non_space('#');
        index = digit_run_end(chars, index + 1);
    }
}

fn digit_run_end(chars: &[char], mut index: usize) -> usize {
    while index < chars.len() && chars[index].is_ascii_digit() {
        index += 1;
    }
    index
}

fn is_hex(ch: char) -> bool {
    ch.is_ascii_hexdigit()
}

#[derive(Debug)]
struct NormalizedText {
    value: String,
    pending_space: bool,
}

impl NormalizedText {
    fn push_non_space(&mut self, ch: char) {
        if self.pending_space {
            self.value.push(' ');
            self.pending_space = false;
        }
        self.value.push(ch);
    }

    fn push_space(&mut self) {
        if !self.value.is_empty() {
            self.pending_space = true;
        }
    }

    fn push_lowercase(&mut self, ch: char) {
        for lower in ch.to_lowercase() {
            self.push_non_space(lower);
        }
    }
}

fn word_trigrams(text: &str) -> HashSet<String> {
    let mut grams = HashSet::new();
    let mut words = text
        .split(|ch: char| !(ch.is_alphanumeric() || ch == '#'))
        .filter(|word| !word.is_empty());
    let Some(mut first) = words.next() else {
        return grams;
    };
    let Some(mut second) = words.next() else {
        return grams;
    };

    for third in words {
        grams.insert(format!("{first} {second} {third}"));
        first = second;
        second = third;
    }
    grams
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> Option<f64> {
    if a.is_empty() || b.is_empty() {
        return Some(0.0);
    }
    let intersection = a.iter().filter(|gram| b.contains(*gram)).count();
    let union = a.len() + b.len() - intersection;
    let intersection = u16::try_from(intersection).ok()?;
    let union = u16::try_from(union).ok()?;
    Some(f64::from(intersection) / f64::from(union))
}
