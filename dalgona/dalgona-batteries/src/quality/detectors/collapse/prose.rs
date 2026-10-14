// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use std::collections::{HashSet, VecDeque};

use super::lines::append_fixed;
use super::{
    Fire, NEAR_ECHO_THRESHOLD, NEAR_LOOKBACK, NEAR_MIN_CHARS, NEAR_MIN_WORD_CHARS, NEAR_SIMILARITY,
    NEAR_WINDOW, PARA_CHARS_MIN, PARA_RING, PARA_THRESHOLD, PARA_WORD_CHARS_MIN, RETENTION,
    SAMPLE_CHARS, Scalar, make_fire,
};

#[derive(Clone, Copy)]
struct Paragraph {
    hash_a: u32,
    hash_b: u32,
    chars: usize,
    start: usize,
    sample: [u8; SAMPLE_CHARS],
    sample_len: usize,
}

struct TokenSet {
    start: usize,
    tokens: HashSet<String>,
}

struct Echo {
    anchor: usize,
    start: usize,
    echoed: bool,
}

pub(super) struct Prose {
    current_line: [u8; RETENTION],
    current_line_len: usize,
    paragraph: [u8; RETENTION],
    paragraph_len: usize,
    paragraph_chars: usize,
    paragraph_words: usize,
    paragraph_hash_a: u32,
    paragraph_hash_b: u32,
    paragraph_start: usize,
    paragraph_fenced: bool,
    inside_fence: Option<char>,
    exact: VecDeque<Paragraph>,
    near: VecDeque<TokenSet>,
    window: VecDeque<Echo>,
}

impl Prose {
    pub(super) fn new() -> Self {
        Self {
            current_line: [0; RETENTION],
            current_line_len: 0,
            paragraph: [0; RETENTION],
            paragraph_len: 0,
            paragraph_chars: 0,
            paragraph_words: 0,
            paragraph_hash_a: 0x811c_9dc5,
            paragraph_hash_b: 5381,
            paragraph_start: 0,
            paragraph_fenced: false,
            inside_fence: None,
            exact: VecDeque::with_capacity(PARA_RING),
            near: VecDeque::with_capacity(NEAR_LOOKBACK),
            window: VecDeque::with_capacity(NEAR_WINDOW),
        }
    }

    pub(super) fn reset_fragment(&mut self) {
        self.current_line_len = 0;
        self.reset_paragraph();
    }

    pub(super) fn feed(&mut self, entry: Scalar) -> Option<Fire> {
        if entry.value != '\n' {
            append_fixed(
                &mut self.current_line,
                &mut self.current_line_len,
                entry.value,
            );
            return None;
        }
        if !line_has_content(&self.current_line[..self.current_line_len]) {
            self.current_line_len = 0;
            return self.finish_paragraph();
        }
        self.fold_line(entry);
        None
    }

    fn fold_line(&mut self, newline: Scalar) {
        if self.paragraph_chars == 0 {
            self.paragraph_start = newline.offset.saturating_sub(self.current_line_len);
        }
        let text = std::str::from_utf8(&self.current_line[..self.current_line_len]).ok();
        let Some(text) = text else {
            self.reset_paragraph();
            self.current_line_len = 0;
            return;
        };
        for ch in text.chars() {
            update_hash(&mut self.paragraph_hash_a, &mut self.paragraph_hash_b, ch);
            self.paragraph_chars += 1;
            self.paragraph_words += usize::from(is_word_char(ch));
        }
        update_hash(&mut self.paragraph_hash_a, &mut self.paragraph_hash_b, '\n');
        self.paragraph_chars += 1;
        append_bytes(
            &mut self.paragraph,
            &mut self.paragraph_len,
            text.as_bytes(),
        );
        append_bytes(&mut self.paragraph, &mut self.paragraph_len, b"\n");
        let marker = fence_marker(text);
        self.paragraph_fenced |= self.inside_fence.is_some() || marker.is_some();
        if let Some(marker) = marker {
            self.inside_fence = if self.inside_fence == Some(marker) {
                None
            } else {
                Some(marker)
            };
        }
        self.current_line_len = 0;
    }

    fn finish_paragraph(&mut self) -> Option<Fire> {
        if self.paragraph_chars == 0 {
            return None;
        }
        let paragraph = Paragraph {
            hash_a: self.paragraph_hash_a,
            hash_b: self.paragraph_hash_b,
            chars: self.paragraph_chars,
            start: self.paragraph_start,
            sample: copy_sample(&self.paragraph, self.paragraph_len),
            sample_len: self.paragraph_len.min(SAMPLE_CHARS),
        };
        let exact = self.check_exact(paragraph);
        if exact.is_some() {
            self.reset_paragraph();
            return exact;
        }
        let near = self.check_near(paragraph);
        self.reset_paragraph();
        near
    }

    fn check_exact(&mut self, current: Paragraph) -> Option<Fire> {
        let eligible = !self.paragraph_fenced
            && self.paragraph_chars >= PARA_CHARS_MIN
            && self.paragraph_words >= PARA_WORD_CHARS_MIN;
        if !eligible {
            self.push_exact(current);
            return None;
        }
        let mut occurrences = 1;
        let mut first = None;
        let mut second = None;
        for previous in &self.exact {
            if !same_paragraph(*previous, current) {
                continue;
            }
            occurrences += 1;
            if first.is_none() {
                first = Some(*previous);
            } else if second.is_none() {
                second = Some(previous.start);
            }
        }
        self.push_exact(current);
        if occurrences < PARA_THRESHOLD {
            return None;
        }
        let first = first?;
        let second = second?;
        let sample = sample_text(first.sample, first.sample_len)?;
        make_fire(
            "paragraph_repeat",
            first.start,
            second,
            format!("paragraph repeated {occurrences} times: {sample}"),
        )
    }

    fn push_exact(&mut self, entry: Paragraph) {
        self.exact.push_back(entry);
        if self.exact.len() > PARA_RING {
            let _ = self.exact.pop_front();
        }
    }

    fn check_near(&mut self, current: Paragraph) -> Option<Fire> {
        let eligible = !self.paragraph_fenced
            && self.paragraph_chars >= NEAR_MIN_CHARS
            && self.paragraph_words >= NEAR_MIN_WORD_CHARS;
        if !eligible {
            return None;
        }
        let text = std::str::from_utf8(&self.paragraph[..self.paragraph_len]).ok()?;
        let normalized = super::super::repetitive_turns::normalize(text);
        let tokens = word_tokens(&normalized);
        if tokens.is_empty() {
            return None;
        }
        let (similarity, anchor) = self.best_match(&tokens);
        let echoed = similarity >= NEAR_SIMILARITY && anchor.is_some();
        self.near.push_back(TokenSet {
            start: current.start,
            tokens,
        });
        if self.near.len() > NEAR_LOOKBACK {
            let _ = self.near.pop_front();
        }
        self.window.push_back(Echo {
            anchor: anchor.unwrap_or(current.start),
            start: current.start,
            echoed,
        });
        if self.window.len() > NEAR_WINDOW {
            let _ = self.window.pop_front();
        }
        let echo_count = self.window.iter().filter(|entry| entry.echoed).count();
        if echo_count < NEAR_ECHO_THRESHOLD {
            return None;
        }
        let oldest = self.window.iter().find(|entry| entry.echoed)?;
        make_fire(
            "near_duplicate_paragraphs",
            oldest.anchor,
            oldest.start,
            format!(
                "{echo_count} of the last {} paragraphs repeat earlier text at Jaccard {similarity:.3}",
                self.window.len()
            ),
        )
    }

    fn best_match(&self, tokens: &HashSet<String>) -> (f64, Option<usize>) {
        let mut best = 0.0;
        let mut anchor = None;
        for previous in &self.near {
            let similarity = token_jaccard(tokens, &previous.tokens);
            if similarity > best {
                best = similarity;
                anchor = Some(previous.start);
            }
        }
        (best, anchor)
    }

    fn reset_paragraph(&mut self) {
        self.paragraph_len = 0;
        self.paragraph_chars = 0;
        self.paragraph_words = 0;
        self.paragraph_hash_a = 0x811c_9dc5;
        self.paragraph_hash_b = 5381;
        self.paragraph_start = 0;
        self.paragraph_fenced = false;
    }
}

fn line_has_content(line: &[u8]) -> bool {
    line.iter().any(|byte| !byte.is_ascii_whitespace())
}

fn same_paragraph(a: Paragraph, b: Paragraph) -> bool {
    a.chars == b.chars && a.hash_a == b.hash_a && a.hash_b == b.hash_b
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || (u32::from(ch) > 0x7f && !matches!(u32::from(ch), 0x2500..=0x259f))
}

fn update_hash(hash_a: &mut u32, hash_b: &mut u32, ch: char) {
    let mut buffer = [0; 4];
    for byte in ch.encode_utf8(&mut buffer).bytes() {
        *hash_a = (*hash_a ^ u32::from(byte)).wrapping_mul(0x0100_0193);
        *hash_b = hash_b.wrapping_mul(31).wrapping_add(u32::from(byte));
    }
}

fn fence_marker(line: &str) -> Option<char> {
    let trimmed = line.trim_start();
    if trimmed.starts_with("```") {
        return Some('`');
    }
    if trimmed.starts_with("~~~") {
        return Some('~');
    }
    None
}

fn append_bytes(target: &mut [u8; RETENTION], length: &mut usize, bytes: &[u8]) {
    let available = target.len().saturating_sub(*length);
    let mut count = available.min(bytes.len());
    if let Err(error) = std::str::from_utf8(&bytes[..count]) {
        count = error.valid_up_to();
    }
    target[*length..*length + count].copy_from_slice(&bytes[..count]);
    *length += count;
}

fn copy_sample(bytes: &[u8; RETENTION], length: usize) -> [u8; SAMPLE_CHARS] {
    let mut sample = [0; SAMPLE_CHARS];
    let mut count = length.min(SAMPLE_CHARS);
    if let Err(error) = std::str::from_utf8(&bytes[..count]) {
        count = error.valid_up_to();
    }
    sample[..count].copy_from_slice(&bytes[..count]);
    sample
}

fn sample_text(bytes: [u8; SAMPLE_CHARS], length: usize) -> Option<String> {
    Some(std::str::from_utf8(&bytes[..length]).ok()?.to_owned())
}

fn word_tokens(text: &str) -> HashSet<String> {
    text.split(|ch: char| !(ch.is_alphanumeric() || ch == '#'))
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

fn token_jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let intersection = a.iter().filter(|token| b.contains(*token)).count();
    let union = a.len() + b.len() - intersection;
    let Ok(intersection) = u16::try_from(intersection) else {
        return 0.0;
    };
    let Ok(union) = u16::try_from(union) else {
        return 0.0;
    };
    f64::from(intersection) / f64::from(union)
}
