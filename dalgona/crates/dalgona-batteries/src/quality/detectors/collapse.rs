// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

mod lines;
mod prose;

use std::collections::VecDeque;

use lines::Lines;
use prose::Prose;

pub(crate) const TAIL_RING: usize = 512;
pub(crate) const DOMINANT_CJK_RUN: usize = 224;
pub(crate) const DOMINANT_PUNCT_RUN: usize = 256;
pub(crate) const DOMINANT_GAP_MAX: usize = 32;
pub(crate) const DOMINANT_RATIO: usize = 8;
pub(crate) const NEWLINE_FLOOD_CHARS: usize = 320;
pub(crate) const NEWLINE_FLOOD_COUNT: usize = 8;
pub(crate) const GENERIC_FLOOD_CHARS: usize = 480;
pub(crate) const PERIOD_MIN: usize = 2;
pub(crate) const PERIOD_MAX: usize = 64;
pub(crate) const PERIOD_SPAN_MIN: usize = 256;
pub(crate) const PERIOD_REPS_MIN: usize = 8;
pub(crate) const LINE_RING: usize = 16;
pub(crate) const LINE_PERIOD_MAX: usize = 4;
pub(crate) const LINE_CYCLES_MIN: usize = 6;
pub(crate) const LINE_REPEATED_CHARS_MIN: usize = 384;
pub(crate) const RETENTION: usize = 512;
pub(crate) const PARA_CHARS_MIN: usize = 64;
pub(crate) const PARA_WORD_CHARS_MIN: usize = 24;
pub(crate) const PARA_THRESHOLD: usize = 3;
pub(crate) const PARA_RING: usize = 64;
pub(crate) const NEAR_MIN_CHARS: usize = 64;
pub(crate) const NEAR_MIN_WORD_CHARS: usize = 24;
pub(crate) const NEAR_SIMILARITY: f64 = 0.5;
pub(crate) const NEAR_LOOKBACK: usize = 32;
pub(crate) const NEAR_WINDOW: usize = 12;
pub(crate) const NEAR_ECHO_THRESHOLD: usize = 8;
pub(crate) const SAMPLE_CHARS: usize = 80;

/// Which stream the detector is reading.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Source {
    /// Model prose or reasoning text.
    Prose,
    /// Tool-call argument text.
    Tool,
}

/// A collapse verdict with the stream offsets it covers.
#[derive(Clone, Debug, PartialEq)]
pub struct Fire {
    /// Stable machine name of the pattern that matched.
    pub reason: &'static str,
    /// Byte offset where the anomaly begins.
    pub anomaly_start_offset: u64,
    /// Byte offset where the garbage span begins.
    pub garbage_start_offset: u64,
    /// Human-readable description of the match.
    pub detail: String,
}

#[derive(Clone, Copy)]
pub(crate) struct Scalar {
    pub(crate) value: char,
    pub(crate) offset: usize,
    pub(crate) width: usize,
}

#[derive(Clone, Copy, Default)]
struct PeriodState {
    matched: usize,
    block_start: usize,
    checked: bool,
}

/// Incremental collapse detector over one output stream.
pub struct State {
    source: Source,
    offset: usize,
    tail: VecDeque<Scalar>,
    latched: bool,
    run_char: Option<char>,
    run_threshold: usize,
    run_count: usize,
    run_gap: usize,
    run_start: usize,
    run_width: usize,
    whitespace_count: usize,
    newline_count: usize,
    whitespace_start: usize,
    periods: [PeriodState; PERIOD_MAX + 1],
    lines: Lines,
    prose: Prose,
}

impl State {
    /// Creates an unlatched detector reading `source`.
    #[must_use]
    pub fn new(source: Source) -> Self {
        Self {
            source,
            offset: 0,
            tail: VecDeque::with_capacity(TAIL_RING),
            latched: false,
            run_char: None,
            run_threshold: 0,
            run_count: 0,
            run_gap: 0,
            run_start: 0,
            run_width: 0,
            whitespace_count: 0,
            newline_count: 0,
            whitespace_start: 0,
            periods: [PeriodState::default(); PERIOD_MAX + 1],
            lines: Lines::new(),
            prose: Prose::new(),
        }
    }

    /// Switches the stream source, dropping any partial prose fragment.
    pub fn set_source(&mut self, source: Source) {
        if self.source == source {
            return;
        }
        self.source = source;
        self.prose.reset_fragment();
    }

    /// Whether the detector has already fired and now ignores input.
    #[must_use]
    pub fn latched(&self) -> bool {
        self.latched
    }

    /// Consumes a streamed delta; returns the first collapse pattern, then latches.
    #[must_use]
    pub fn feed(&mut self, delta: &str) -> Option<Fire> {
        if self.latched {
            return None;
        }
        for (relative, value) in delta.char_indices() {
            let entry = Scalar {
                value,
                offset: self.offset.saturating_add(relative),
                width: value.len_utf8(),
            };
            self.push_tail(entry);
            if let Some(fire) = self.update_dominant(entry) {
                return Some(self.latch(fire, delta.len()));
            }
            if let Some(fire) = self.update_whitespace(entry) {
                return Some(self.latch(fire, delta.len()));
            }
            if let Some(fire) = self.update_periods(entry) {
                return Some(self.latch(fire, delta.len()));
            }
            if let Some(fire) = self.lines.feed(entry) {
                return Some(self.latch(fire, delta.len()));
            }
            if self.source == Source::Prose
                && let Some(fire) = self.prose.feed(entry)
            {
                return Some(self.latch(fire, delta.len()));
            }
        }
        self.offset = self.offset.saturating_add(delta.len());
        None
    }

    fn latch(&mut self, fire: Fire, delta_len: usize) -> Fire {
        self.latched = true;
        self.offset = self.offset.saturating_add(delta_len);
        fire
    }

    fn push_tail(&mut self, entry: Scalar) {
        self.tail.push_back(entry);
        if self.tail.len() > TAIL_RING {
            let _ = self.tail.pop_front();
        }
    }

    fn update_dominant(&mut self, entry: Scalar) -> Option<Fire> {
        if entry.value.is_ascii_whitespace() {
            return self.extend_dominant_gap();
        }
        let Some(threshold) = dominant_threshold(entry.value) else {
            self.clear_dominant_run();
            return None;
        };
        if self.run_char != Some(entry.value) {
            self.start_dominant_run(entry, threshold);
            return None;
        }
        self.run_count += 1;
        if self.run_count < self.run_threshold {
            return None;
        }
        make_fire(
            "dominant_run",
            self.run_start,
            self.run_start.saturating_add(self.run_width),
            format!(
                "dominant run U+{:04X} repeated {} times",
                u32::from(entry.value),
                self.run_count
            ),
        )
    }

    fn extend_dominant_gap(&mut self) -> Option<Fire> {
        self.run_char?;
        self.run_gap += 1;
        if self.run_gap <= DOMINANT_GAP_MAX
            && self.run_gap.saturating_mul(DOMINANT_RATIO) <= self.run_count
        {
            return None;
        }
        self.clear_dominant_run();
        None
    }

    fn start_dominant_run(&mut self, entry: Scalar, threshold: usize) {
        self.run_char = Some(entry.value);
        self.run_threshold = threshold;
        self.run_count = 1;
        self.run_gap = 0;
        self.run_start = entry.offset;
        self.run_width = entry.width;
    }

    fn clear_dominant_run(&mut self) {
        self.run_char = None;
        self.run_threshold = 0;
        self.run_count = 0;
        self.run_gap = 0;
    }

    fn update_whitespace(&mut self, entry: Scalar) -> Option<Fire> {
        if !entry.value.is_ascii_whitespace() {
            self.whitespace_count = 0;
            self.newline_count = 0;
            return None;
        }
        if self.whitespace_count == 0 {
            self.whitespace_start = entry.offset;
        }
        self.whitespace_count += 1;
        self.newline_count += usize::from(entry.value == '\n');
        let newline_flood = self.whitespace_count >= NEWLINE_FLOOD_CHARS
            && self.newline_count >= NEWLINE_FLOOD_COUNT;
        if !newline_flood && self.whitespace_count < GENERIC_FLOOD_CHARS {
            return None;
        }
        make_fire(
            "whitespace_flood",
            self.whitespace_start,
            self.whitespace_start.saturating_add(entry.width),
            format!(
                "whitespace flood of {} characters with {} newlines",
                self.whitespace_count, self.newline_count
            ),
        )
    }

    fn update_periods(&mut self, entry: Scalar) -> Option<Fire> {
        for period in PERIOD_MIN..=PERIOD_MAX {
            let previous = self.tail_back(period);
            if previous.is_none_or(|scalar| scalar.value != entry.value) {
                self.periods[period] = PeriodState::default();
                continue;
            }
            let previous = previous?;
            let state = &mut self.periods[period];
            if state.matched == 0 {
                state.block_start = previous.offset;
            }
            state.matched += 1;
            if state.checked || !period_ready(state, period) {
                continue;
            }
            state.checked = true;
            let block_start = state.block_start;
            let matched = state.matched;
            if !self.period_unit_eligible(period) {
                continue;
            }
            let first_width = self.period_first_width(period);
            return make_fire(
                "short_period",
                block_start,
                block_start.saturating_add(first_width),
                format!("short period {period} spans {matched} scalars"),
            );
        }
        None
    }

    fn period_unit_eligible(&self, period: usize) -> bool {
        let Some(start) = self.tail.len().checked_sub(period) else {
            return false;
        };
        let unit = self.tail.iter().skip(start);
        if unit.clone().all(|entry| entry.value.is_ascii_whitespace()) {
            return false;
        }
        let decorative = self
            .tail
            .iter()
            .skip(start)
            .all(|entry| entry.value.is_ascii_whitespace() || is_decorative(entry.value));
        let base64 = self.tail.iter().skip(start).all(|entry| {
            entry.value.is_ascii_alphanumeric() || matches!(entry.value, '+' | '/' | '=')
        });
        let numeric = self.tail.iter().skip(start).all(|entry| {
            entry.value.is_ascii_whitespace()
                || entry.value.is_ascii_hexdigit()
                || matches!(entry.value, '.' | ',' | ':' | ';' | '-' | '/' | '|')
        });
        let code = self
            .tail
            .iter()
            .skip(start)
            .all(|entry| entry.value.is_ascii_whitespace() || is_code_punctuation(entry.value));
        !(decorative || base64 || numeric || code)
    }

    fn period_first_width(&self, period: usize) -> usize {
        let start = self.tail.len().saturating_sub(period);
        self.tail.iter().skip(start).map(|entry| entry.width).sum()
    }

    fn tail_back(&self, back: usize) -> Option<Scalar> {
        self.tail
            .get(self.tail.len().checked_sub(back + 1)?)
            .copied()
    }
}

fn period_ready(state: &PeriodState, period: usize) -> bool {
    state.matched >= PERIOD_SPAN_MIN && state.matched >= PERIOD_REPS_MIN * period
}

fn dominant_threshold(ch: char) -> Option<usize> {
    if is_box_drawing(ch) {
        return None;
    }
    if !ch.is_ascii() {
        return Some(DOMINANT_CJK_RUN);
    }
    if matches!(ch, '!' | '$' | '%' | '&' | '?' | '@') {
        return Some(DOMINANT_PUNCT_RUN);
    }
    None
}

fn is_decorative(ch: char) -> bool {
    if is_box_drawing(ch) {
        return true;
    }
    ch.is_ascii()
        && ch.is_ascii_graphic()
        && !ch.is_ascii_alphanumeric()
        && !matches!(ch, '!' | '$' | '%' | '&' | '?' | '@')
}

fn is_box_drawing(ch: char) -> bool {
    matches!(u32::from(ch), 0x2500..=0x259f)
}

fn is_code_punctuation(ch: char) -> bool {
    "{}()[]<>,;.:=+-*/&|^%$#@~`'\"\\_".contains(ch)
}

#[must_use]
pub(crate) fn make_fire(
    reason: &'static str,
    anomaly: usize,
    garbage: usize,
    detail: String,
) -> Option<Fire> {
    let anomaly_start_offset = u64::try_from(anomaly).ok()?;
    let garbage_start_offset = u64::try_from(garbage).ok()?;
    Some(Fire {
        reason,
        anomaly_start_offset,
        garbage_start_offset,
        detail,
    })
}
