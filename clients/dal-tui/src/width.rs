//! Grapheme-safe terminal measurement, wrapping, and control escaping.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

/// Locale-selected terminal width policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WidthMode {
    /// Ambiguous East Asian characters occupy one cell.
    #[default]
    Narrow,
    /// Ambiguous East Asian characters occupy two cells.
    Cjk,
}

impl WidthMode {
    /// Selects CJK width when the first configured locale starts with `ja`, `zh`, or `ko`.
    #[must_use]
    pub fn from_locale(values: [&str; 3]) -> Self {
        values
            .into_iter()
            .find(|value| !value.is_empty())
            .filter(|value| {
                let language = value.split(['_', '-', '.']).next().unwrap_or_default();
                matches!(language, "ja" | "zh" | "ko")
            })
            .map_or(Self::Narrow, |_| Self::Cjk)
    }
}

/// Returns the measured cell width of all grapheme clusters in `text`.
#[must_use]
pub fn width(text: &str, mode: WidthMode) -> usize {
    text.graphemes(true)
        .map(|cluster| cluster_width(cluster, mode))
        .sum()
}

fn cluster_width(cluster: &str, mode: WidthMode) -> usize {
    let mut regional_indicators = 0;
    let mut jamo = 0;
    let mut has_zwj = false;
    let mut has_vs16 = false;
    let mut has_keycap = false;
    let mut max_width = 0;

    for character in cluster.chars() {
        let scalar = u32::from(character);
        if (0x1f1e6..=0x1f1ff).contains(&scalar) {
            regional_indicators += 1;
        }
        if is_hangul_jamo(scalar) {
            jamo += 1;
        }
        has_zwj |= scalar == 0x200d;
        has_vs16 |= scalar == 0xfe0f;
        has_keycap |= scalar == 0x20e3;

        let character_width = match mode {
            WidthMode::Narrow => character.width().unwrap_or(0),
            WidthMode::Cjk => character.width_cjk().unwrap_or(0),
        };
        max_width = max_width.max(character_width);
    }

    if regional_indicators > 0 {
        // A cluster carries at most two indicators, and a paired flag is
        // one two-cell glyph — counting them per-codepoint reports 4
        // cells for what every terminal draws in 2.
        return 2;
    }
    if has_zwj || has_vs16 || has_keycap || jamo > 0 {
        // A cluster holding any conjoining jamo is a syllable-region glyph:
        // two cells whether it is a complete syllable, an archaic chain, or a
        // lone jamo unicode-width reports as zero-width (Extended-B measures
        // `None`, which would desync the cursor against the drawn cell).
        return 2;
    }
    max_width
}

fn is_hangul_jamo(scalar: u32) -> bool {
    (0x1100..=0x11ff).contains(&scalar)
        || (0xa960..=0xa97c).contains(&scalar)
        || (0xd7b0..=0xd7fb).contains(&scalar)
}

/// Wraps text without splitting grapheme clusters or ending a row with half a wide
/// cluster, preferring word boundaries over mid-word breaks.
///
/// Whitespace between words marks a break: a row ends on the last whitespace run
/// that still fits and the next row starts at the following word, so the emitted
/// rows join back to the input except for `\n`, which is consumed as a hard break.
/// No-break spaces (U+00A0, U+2007, U+202F) are not break opportunities. A run of
/// non-whitespace longer than `cap` still breaks at the cell cap inside the word,
/// because no better boundary exists. An individual cluster wider than `cap`
/// occupies its own row because a terminal cannot represent that cluster within
/// fewer cells without changing its content.
#[must_use]
pub fn wrap(text: &str, cap: usize, mode: WidthMode) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    wrap_ranges(text, cap, mode)
        .into_iter()
        .map(|(start, end)| text[start..end].to_owned())
        .collect()
}

/// Wraps `text` like [`wrap`] but returns the byte range of each row instead of
/// allocating the row strings. Callers that carry styled spans on the text use the
/// ranges to move them onto the emitted rows. Ranges cover every byte: joining the
/// slices always yields `text` again except that `\n` is consumed as a hard break.
#[must_use]
pub(crate) fn wrap_ranges(text: &str, cap: usize, mode: WidthMode) -> Vec<(usize, usize)> {
    if text.is_empty() {
        return vec![(0, 0)];
    }

    let cap = cap.max(1);
    let mut rows = Vec::new();
    // `start..end` bounds committed text and `cells` counts its width plus an
    // in-flight whitespace run; `end` only advances over non-whitespace and
    // marks the first byte of the current whitespace run while one is open.
    // `boundary` remembers the last word resume point: `limit` is that word's
    // first byte and `base` the row's cell count there, so closing a row at
    // `limit` keeps the whitespace on it and the next row restarts with
    // `cells - base` cells already committed.
    let mut start = 0;
    let mut end = 0;
    let mut cells = 0;
    let mut in_ws = false;
    let mut boundary: Option<(usize, usize)> = None;

    for (index, cluster) in text.grapheme_indices(true) {
        if cluster == "\n" {
            // `end` stops at the last non-whitespace, but the run's bytes belong to
            // this row: emit through `index` so joining the ranges loses nothing.
            rows.push((start, if in_ws { index } else { end }));
            start = index + 1;
            end = start;
            cells = 0;
            in_ws = false;
            boundary = None;
            continue;
        }

        let cluster_width = cluster_width(cluster, mode);
        let breakable = cluster
            .chars()
            .all(|c| c.is_whitespace() && !matches!(c, '\u{00a0}' | '\u{2007}' | '\u{202f}'));
        if breakable {
            if cells + cluster_width > cap && index > start {
                // The whitespace run alone does not fit on this row: split it,
                // never emitting an empty row; `end` tracks the tail run's start.
                rows.push((start, index));
                start = index;
                end = index;
                cells = 0;
                boundary = None;
            }
            if !in_ws {
                end = index;
            }
            in_ws = true;
            cells += cluster_width;
            continue;
        }
        if in_ws {
            in_ws = false;
            if end > start {
                // The run ended and committed content precedes it: the row may
                // close here, keeping the whitespace, and resume at this word's
                // first byte. With no content before the run a break would emit
                // a whitespace-only row, so the word just joins it.
                boundary = Some((index, cells));
            }
        }

        let over = |cells| {
            cells + cluster_width > cap || (cluster_width == 2 && cells == cap.saturating_sub(1))
        };
        if let Some((limit, base)) = boundary.filter(|(limit, _)| *limit > start)
            && over(cells)
        {
            // The word overflows but the boundary still fits: close the row at the
            // word's start, keeping the whitespace; the word heads the next row.
            rows.push((start, limit));
            start = limit;
            cells -= base;
            boundary = None;
        }
        if index > start && over(cells) {
            // No usable boundary: break at the cell cap, inside the word if
            // needed. The row just needs bytes — including any pending
            // whitespace run, which ends at `index`.
            rows.push((start, index));
            start = index;
            cells = 0;
        }
        end = index + cluster.len();
        cells += cluster_width;
        if cluster_width > cap {
            rows.push((start, end));
            start = end;
            cells = 0;
        }
    }

    // Pending whitespace still belongs to the last row: `end` only advances over
    // non-whitespace, so the byte range runs to the end of the text.
    if start < text.len() || rows.is_empty() || text.ends_with('\n') {
        let last = if in_ws { text.len() } else { end };
        rows.push((start, last.max(start)));
    }
    rows
}

/// Returns the longest grapheme prefix that fits within `max` cells.
#[must_use]
pub fn take_cells(text: &str, max: usize, mode: WidthMode) -> String {
    let mut result = String::new();
    let mut cells = 0;
    for cluster in text.graphemes(true) {
        let cluster_width = cluster_width(cluster, mode);
        if cells + cluster_width > max {
            break;
        }
        result.push_str(cluster);
        cells += cluster_width;
    }
    result
}

/// Makes control characters visible before they reach the terminal or width calculator.
#[must_use]
pub fn escape(text: &str) -> String {
    dal_core::terminal::escape(text).into_owned()
}

#[cfg(test)]
mod tests {
    use super::{WidthMode, escape, take_cells, width, wrap};

    #[test]
    fn literals_follow_terminal_width_rules() {
        let narrow = WidthMode::Narrow;
        assert_eq!(width("a", narrow), 1);
        assert_eq!(width("日漢字", narrow), 6);
        assert_eq!(width("가", narrow), 2);
        assert_eq!(width("\u{1100}\u{1161}\u{11a8}", narrow), 2);
        assert_eq!(width("a\u{301}", narrow), 1);
        assert_eq!(width("👨‍🌾", narrow), 2);
        assert_eq!(width("#\u{fe0f}\u{20e3}", narrow), 2);
        assert_eq!(width("❤", narrow), 1);
        assert_eq!(width("❤\u{fe0f}", narrow), 2);
        assert_eq!(width("❤\u{fe0e}", narrow), 1);
        assert_eq!(width("🇫🇷", narrow), 2);
        assert_eq!(width("🇫", narrow), 2);
        assert_eq!(width("·±", narrow), 2);
        assert_eq!(width("·±", WidthMode::Cjk), 4);
    }

    #[test]
    fn control_characters_are_visible_before_measurement() {
        let escaped = escape("\t\u{7}");
        assert_eq!(escaped, "\\t\\a");
        assert_eq!(width(&escaped, WidthMode::Narrow), 4);
    }

    #[test]
    fn bidi_controls_escape_to_visible_code_points() {
        let payload = "a\u{61c}\u{200e}\u{200f}\u{202a}\u{202b}\u{202c}\u{202d}\u{202e}\u{2066}\u{2067}\u{2068}\u{2069}b";
        let escaped = escape(payload);
        assert_eq!(
            escaped,
            "a\\u{61c}\\u{200e}\\u{200f}\\u{202a}\\u{202b}\\u{202c}\\u{202d}\\u{202e}\\u{2066}\\u{2067}\\u{2068}\\u{2069}b"
        );
        assert!(escaped.is_ascii(), "no bidi control survives: {escaped}");
    }

    #[test]
    fn arabic_and_hebrew_text_survive_escape_untouched() {
        let text = "مرحبا שלום http://example.test/المستخدم?x=1";
        assert_eq!(escape(text), text);
    }

    #[test]
    fn wrapping_keeps_wide_clusters_together() {
        assert_eq!(wrap("a日b", 3, WidthMode::Narrow), ["a日", "b"]);
    }

    #[test]
    fn wrapping_prefers_word_boundaries_over_mid_word_breaks() {
        // Boundary breaks keep the whitespace at the end of the row and the next
        // row starts at the word, so the emitted ranges join back byte for byte.
        assert_eq!(
            wrap("aaa bbb cc", 4, WidthMode::Narrow),
            ["aaa ", "bbb ", "cc"]
        );
        assert_eq!(
            wrap("call compute alpha beta", 11, WidthMode::Narrow),
            ["call ", "compute ", "alpha beta"]
        );
    }

    #[test]
    fn unbroken_runs_still_break_at_the_cell_cap() {
        assert_eq!(
            wrap("xx xxxxxxy", 4, WidthMode::Narrow),
            ["xx ", "xxxx", "xxy"]
        );
    }

    #[test]
    fn no_break_spaces_stay_glued_to_their_words() {
        // UAX #14: NBSP (also U+2007 and U+202F) is not a break opportunity, so a
        // row too small for the pair breaks mid-word at the cap instead.
        assert_eq!(
            wrap("a\u{a0}bcd", 2, WidthMode::Narrow),
            ["a\u{a0}", "bc", "d"]
        );
    }

    #[test]
    fn a_wide_space_at_the_row_start_never_yields_an_empty_row() {
        assert_eq!(wrap("\u{3000}", 1, WidthMode::Narrow), ["\u{3000}"]);
        // A whitespace run wider than the cap still splits at the cap.
        assert_eq!(
            wrap("ab\u{3000}\u{3000}c", 2, WidthMode::Narrow),
            ["ab", "\u{3000}", "\u{3000}", "c"]
        );
    }

    #[test]
    fn truncation_stops_before_an_incomplete_cluster() {
        assert_eq!(take_cells("ab日", 2, WidthMode::Narrow), "ab");
    }
}
