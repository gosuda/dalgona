//! Bounded per-job output lines with monotonic sequence numbers.

use std::collections::VecDeque;

use dal_core::JobLine;

/// The most lines one job retains; older lines are counted as dropped.
pub(super) const RING_LINES: usize = 512;
/// The longest retained line; a longer line is cut at a character boundary.
const MAX_LINE_BYTES: usize = 4096;

/// The ring of the newest lines of one job, plus the unfinished line.
#[derive(Debug, Default)]
pub(super) struct LineRing {
    last_seq: u64,
    lines: VecDeque<(u64, Box<str>)>,
    partial: Vec<u8>,
}

/// One read of a ring after a cursor.
pub(super) struct LineRead {
    pub(super) lines: Vec<JobLine>,
    pub(super) next: u64,
    pub(super) dropped: u64,
}

impl LineRing {
    /// Appends output bytes, completing a line at every `\n`.
    pub(super) fn push(&mut self, bytes: &[u8]) {
        for chunk in bytes.split_inclusive(|byte| *byte == b'\n') {
            self.partial.extend_from_slice(chunk);
            if chunk.ends_with(b"\n") || self.partial.len() > MAX_LINE_BYTES * 2 {
                self.complete();
            }
        }
    }

    /// Completes the unfinished line, at the end of the job.
    pub(super) fn finish(&mut self) {
        if !self.partial.is_empty() {
            self.complete();
        }
    }

    fn complete(&mut self) {
        let mut raw = std::mem::take(&mut self.partial);
        while matches!(raw.last(), Some(b'\n' | b'\r')) {
            raw.pop();
        }
        let mut text = String::from_utf8_lossy(&raw).into_owned();
        if text.len() > MAX_LINE_BYTES {
            let mut cut = MAX_LINE_BYTES;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
        }
        self.last_seq += 1;
        if self.lines.len() == RING_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back((self.last_seq, text.into_boxed_str()));
    }

    /// Reads the lines after `after`, counting those the ring already lost.
    pub(super) fn read(&self, after: Option<u64>) -> LineRead {
        let after = after.unwrap_or(0);
        let first = self
            .lines
            .front()
            .map_or(self.last_seq + 1, |(seq, _)| *seq);
        let dropped = first.saturating_sub(after.saturating_add(1));
        let lines: Vec<JobLine> = self
            .lines
            .iter()
            .filter(|(seq, _)| *seq > after)
            .map(|(seq, text)| JobLine {
                seq: *seq,
                text: text.clone(),
            })
            .collect();
        let next = lines.last().map_or(after, |line| line.seq);
        LineRead {
            lines,
            next,
            dropped,
        }
    }
}
