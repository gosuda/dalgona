//! Text windows over file bytes: line selection, budgets, cuts, and rendering.

use dal_core::SourceRow;

use super::{Lines, MAX_LIMIT};
use crate::tag8;

/// Longest displayed line, in bytes, before the `...` cut marker.
const MAX_LINE_BYTES: usize = 2000;
/// Budget for the result text of one window, footer or tag line included.
pub(crate) const MAX_OUTPUT_BYTES: usize = 50 * 1024;
/// One displayed line. `was_truncated` marks a line cut at
/// [`MAX_LINE_BYTES`]; its text ends with the `...` marker and it is never
/// recorded as seen.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReadLine {
    pub(crate) number: u64,
    pub(crate) text: String,
    pub(crate) was_truncated: bool,
}

/// A rendered text window over one file.
#[derive(Debug)]
pub(crate) struct ReadWindow {
    pub(crate) lines: Vec<ReadLine>,
    /// Line count from the first pass.
    pub(crate) total_lines: u64,
    /// Present when the window stops before the file is complete.
    pub(crate) next_offset: Option<u64>,
    /// Contiguous displayed intervals, in order.
    pub(crate) intervals: Vec<(u64, u64)>,
    /// Bytes of the second pass, which the window displays.
    pub(crate) bytes: Vec<u8>,
}

/// The captured source behind one text window: its tag path, the bytes the
/// window displays, and the displayed rows.
#[derive(Debug)]
pub(crate) struct Source {
    pub(crate) path: String,
    pub(crate) bytes: Vec<u8>,
    pub(crate) rows: Box<[SourceRow]>,
    pub(crate) truncated: bool,
}

/// Displayed lines as source rows; a cut line is kept but never complete,
/// and a complete row drops the CR of a CRLF terminator.
pub(crate) fn source_rows(lines: Vec<ReadLine>) -> Box<[SourceRow]> {
    lines
        .into_iter()
        .map(|line| {
            let mut text = line.text;
            if !line.was_truncated && text.ends_with('\r') {
                text.pop();
            }
            SourceRow {
                line: line.number,
                text: text.into(),
                complete: !line.was_truncated,
            }
        })
        .collect()
}
/// Selects, cuts, and budgets the displayed lines of `bytes`.
pub(crate) fn window(bytes: Vec<u8>, total: u64, lines: &Lines, reserve: usize) -> ReadWindow {
    let from;
    let (ranges, max_lines): (&[(u64, u64)], u64) = match lines {
        Lines::From { offset, limit } => {
            from = [(*offset, offset.saturating_add(limit - 1))];
            (from.as_slice(), *limit)
        }
        Lines::Select(ranges) => (ranges.as_slice(), MAX_LIMIT),
    };
    let budget = MAX_OUTPUT_BYTES.saturating_sub(reserve);
    let shown = select_lines(&bytes, total, ranges, max_lines, budget);
    let displayed = contiguous(shown.iter().map(|line| line.number));
    let complete = match lines {
        Lines::From { .. } => total == 0 || shown.last().is_some_and(|last| last.number >= total),
        Lines::Select(_) => total == 0 || displayed == [(1, total)],
    };
    let next_offset = if complete {
        None
    } else {
        shown
            .last()
            .map(|last| last.number + 1)
            .or_else(|| match lines {
                Lines::From { offset, .. } => Some(*offset),
                Lines::Select(ranges) => ranges.first().map(|(first, _)| *first),
            })
    };
    let intervals = contiguous(
        shown
            .iter()
            .filter(|line| !line.was_truncated)
            .map(|line| line.number),
    );
    ReadWindow {
        lines: shown,
        total_lines: total,
        next_offset,
        intervals,
        bytes,
    }
}

/// Decodes `bytes` lossily as one sequence and keeps the lines inside
/// `ranges`, stopping at `max_lines` or `budget` bytes after a whole line.
fn select_lines(
    bytes: &[u8],
    total: u64,
    ranges: &[(u64, u64)],
    max_lines: u64,
    budget: usize,
) -> Vec<ReadLine> {
    let decoded = String::from_utf8_lossy(bytes);
    let body = decoded.strip_suffix('\n').unwrap_or(&decoded);
    let pieces = (!decoded.is_empty()).then(|| body.split('\n'));
    let mut shown: Vec<ReadLine> = Vec::new();
    let mut used = 0_usize;
    let mut range = 0_usize;
    for (number, raw) in (1_u64..).zip(pieces.into_iter().flatten()) {
        if number > total {
            break;
        }
        while ranges.get(range).is_some_and(|&(_, last)| last < number) {
            range += 1;
        }
        let Some(&(first, _)) = ranges.get(range) else {
            break;
        };
        if number < first {
            continue;
        }
        if shown.len() as u64 == max_lines {
            break;
        }
        let (text, was_truncated) = cut_line(raw);
        let entry = number.to_string().len() + text.len() + 2;
        if !shown.is_empty() && used + entry > budget {
            break;
        }
        used += entry;
        shown.push(ReadLine {
            number,
            text,
            was_truncated,
        });
    }
    shown
}

/// Groups ascending line numbers into inclusive contiguous intervals.
fn contiguous(numbers: impl Iterator<Item = u64>) -> Vec<(u64, u64)> {
    let mut intervals: Vec<(u64, u64)> = Vec::new();
    for number in numbers {
        match intervals.last_mut() {
            Some(interval) if interval.1 + 1 == number => interval.1 = number,
            _ => intervals.push((number, number)),
        }
    }
    intervals
}

/// Cuts a line longer than [`MAX_LINE_BYTES`] at the last character
/// boundary not past that byte and appends `...`.
fn cut_line(raw: &str) -> (String, bool) {
    if raw.len() <= MAX_LINE_BYTES {
        return (raw.to_owned(), false);
    }
    let mut end = MAX_LINE_BYTES;
    while !raw.is_char_boundary(end) {
        end -= 1;
    }
    (format!("{}...", &raw[..end]), true)
}

/// Numbered lines, then the continuation footer or the whole-file tag line.
pub(crate) fn render(window: &ReadWindow, path: &str) -> String {
    let mut out = String::new();
    for line in &window.lines {
        out.push_str(&line.number.to_string());
        out.push('\t');
        out.push_str(&line.text);
        out.push('\n');
    }
    match (
        window.next_offset,
        window.lines.first(),
        window.lines.last(),
    ) {
        (Some(next), Some(first), Some(last)) => out.push_str(&format!(
            "[Showing lines {}-{} of {}. Use :{next} to continue.]",
            first.number, last.number, window.total_lines
        )),
        (Some(next), None, None) => out.push_str(&format!(
            "[No lines at or after {next}; file has {} lines.]",
            window.total_lines
        )),
        _ => out.push_str(&format!("[{path}#{}]", tag8("whole", &window.bytes))),
    }
    out
}

#[cfg(test)]
mod tests {
    use dal_core::SourceRow;

    use super::{Lines, source_rows, window};

    #[test]
    fn source_rows_keep_exact_complete_lines_and_mark_cut_lines() {
        let long = "x".repeat(2100);
        let bytes = format!("one\r\n{long}\nthree").into_bytes();
        let shown = window(
            bytes,
            3,
            &Lines::From {
                offset: 1,
                limit: 3,
            },
            0,
        );
        let rows = source_rows(shown.lines);
        let cut = format!("{}...", &long[..2000]);
        assert_eq!(
            *rows,
            [
                SourceRow {
                    line: 1,
                    text: "one".into(),
                    complete: true
                },
                SourceRow {
                    line: 2,
                    text: cut.as_str().into(),
                    complete: false
                },
                SourceRow {
                    line: 3,
                    text: "three".into(),
                    complete: true
                },
            ]
        );
    }
}
