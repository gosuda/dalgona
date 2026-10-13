// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use std::collections::VecDeque;

use super::{
    Fire, LINE_CYCLES_MIN, LINE_PERIOD_MAX, LINE_REPEATED_CHARS_MIN, LINE_RING, RETENTION,
    SAMPLE_CHARS, Scalar, make_fire,
};

#[derive(Clone, Copy)]
struct Entry {
    hash_a: u32,
    hash_b: u32,
    chars: usize,
    width: usize,
    start: usize,
    eligible: bool,
    sample: [u8; SAMPLE_CHARS],
    sample_len: usize,
}

pub(super) struct Lines {
    line: Buffer,
    ring: VecDeque<Entry>,
    matches: [usize; LINE_PERIOD_MAX + 1],
    repeated_chars: [usize; LINE_PERIOD_MAX + 1],
    block_start: [usize; LINE_PERIOD_MAX + 1],
    cycle_width: [usize; LINE_PERIOD_MAX + 1],
}

impl Lines {
    pub(super) fn new() -> Self {
        Self {
            line: Buffer::new(0),
            ring: VecDeque::with_capacity(LINE_RING),
            matches: [0; LINE_PERIOD_MAX + 1],
            repeated_chars: [0; LINE_PERIOD_MAX + 1],
            block_start: [0; LINE_PERIOD_MAX + 1],
            cycle_width: [0; LINE_PERIOD_MAX + 1],
        }
    }

    pub(super) fn feed(&mut self, entry: Scalar) -> Option<Fire> {
        if entry.value != '\n' {
            self.line.push(entry);
            return None;
        }
        let current = self.line.finish(entry);
        self.ring.push_back(current);
        if self.ring.len() > LINE_RING {
            let _ = self.ring.pop_front();
        }
        self.check_cycles(current)
    }

    fn check_cycles(&mut self, current: Entry) -> Option<Fire> {
        for period in 1..=LINE_PERIOD_MAX {
            let Some(previous_index) = self.ring.len().checked_sub(period + 1) else {
                self.reset(period);
                continue;
            };
            let previous = self.ring[previous_index];
            if !same_line(previous, current) {
                self.reset(period);
                continue;
            }
            self.advance(period, previous, current);
            let enough_matches = self.matches[period] >= (LINE_CYCLES_MIN - 1) * period;
            if !enough_matches || self.repeated_chars[period] < LINE_REPEATED_CHARS_MIN {
                continue;
            }
            return self.fire(period, current);
        }
        None
    }

    fn advance(&mut self, period: usize, previous: Entry, current: Entry) {
        if self.matches[period] > 0 {
            self.matches[period] += 1;
            self.repeated_chars[period] += current.chars;
            return;
        }
        self.block_start[period] = previous.start;
        self.cycle_width[period] = self
            .ring
            .iter()
            .skip(self.ring.len().saturating_sub(period + 1))
            .take(period)
            .map(|line| line.width)
            .sum();
        self.repeated_chars[period] = self
            .ring
            .iter()
            .skip(self.ring.len().saturating_sub(period + 1))
            .take(period + 1)
            .map(|line| line.chars)
            .sum();
        self.matches[period] = 1;
    }

    fn fire(&self, period: usize, current: Entry) -> Option<Fire> {
        let sample = std::str::from_utf8(&current.sample[..current.sample_len]).ok()?;
        let cycles = self.matches[period] / period + 1;
        make_fire(
            "line_cycle",
            self.block_start[period],
            self.block_start[period].saturating_add(self.cycle_width[period]),
            format!("line cycle period {period} over {cycles} cycles: {sample}"),
        )
    }

    fn reset(&mut self, period: usize) {
        self.matches[period] = 0;
        self.repeated_chars[period] = 0;
    }
}

struct Buffer {
    text: [u8; RETENTION],
    text_len: usize,
    hash_a: u32,
    hash_b: u32,
    chars: usize,
    width: usize,
    words: usize,
    content: bool,
    start: usize,
}

impl Buffer {
    fn new(start: usize) -> Self {
        Self {
            text: [0; RETENTION],
            text_len: 0,
            hash_a: 0x811c_9dc5,
            hash_b: 5381,
            chars: 0,
            width: 0,
            words: 0,
            content: false,
            start,
        }
    }

    fn push(&mut self, entry: Scalar) {
        update_hash(&mut self.hash_a, &mut self.hash_b, entry.value);
        self.chars += 1;
        self.width += entry.width;
        self.content |= !entry.value.is_ascii_whitespace();
        self.words += usize::from(is_word_char(entry.value));
        append_fixed(&mut self.text, &mut self.text_len, entry.value);
    }

    fn finish(&mut self, newline: Scalar) -> Entry {
        let entry = Entry {
            hash_a: self.hash_a,
            hash_b: self.hash_b,
            chars: self.chars,
            width: self.width + newline.width,
            start: self.start,
            eligible: self.content && self.words > 0,
            sample: copy_sample(&self.text, self.text_len),
            sample_len: self.text_len.min(SAMPLE_CHARS),
        };
        *self = Self::new(newline.offset.saturating_add(newline.width));
        entry
    }
}

fn same_line(a: Entry, b: Entry) -> bool {
    a.eligible && b.eligible && a.chars == b.chars && a.hash_a == b.hash_a && a.hash_b == b.hash_b
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

pub(super) fn append_fixed(target: &mut [u8], length: &mut usize, ch: char) {
    let mut buffer = [0; 4];
    let encoded = ch.encode_utf8(&mut buffer).as_bytes();
    if *length + encoded.len() > target.len() {
        return;
    }
    target[*length..*length + encoded.len()].copy_from_slice(encoded);
    *length += encoded.len();
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
