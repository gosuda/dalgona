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

/// Wraps text without splitting grapheme clusters or ending a row with half a wide cluster.
///
/// An individual cluster wider than `cap` occupies its own row because a terminal cannot
/// represent that cluster within fewer cells without changing its content.
#[must_use]
pub fn wrap(text: &str, cap: usize, mode: WidthMode) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }

    let cap = cap.max(1);
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut row_width = 0;

    for cluster in text.graphemes(true) {
        if cluster == "\n" {
            rows.push(std::mem::take(&mut row));
            row_width = 0;
            continue;
        }

        let cluster_width = cluster_width(cluster, mode);
        if !row.is_empty()
            && (row_width + cluster_width > cap
                || (cluster_width == 2 && row_width == cap.saturating_sub(1)))
        {
            rows.push(std::mem::take(&mut row));
            row_width = 0;
        }
        row.push_str(cluster);
        row_width += cluster_width;
        if cluster_width > cap {
            rows.push(std::mem::take(&mut row));
            row_width = 0;
        }
    }

    if !row.is_empty() || rows.is_empty() || text.ends_with('\n') {
        rows.push(row);
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

/// Reports whether a character is a Unicode bidirectional-control code that
/// can reorder surrounding text in a bidi terminal: the Arabic letter mark,
/// the direction marks, the embeddings and overrides, and the isolates.
pub(crate) fn is_bidi_control(character: char) -> bool {
    matches!(
        character,
        '\u{61c}' | '\u{200e}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

/// Makes control characters visible before they reach the terminal or width calculator.
#[must_use]
pub fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\t' => escaped.push_str("\\t"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\u{7}' => escaped.push_str("\\a"),
            character if character.is_control() || is_bidi_control(character) => {
                use std::fmt::Write as _;
                let _ = write!(escaped, "\\u{{{:x}}}", u32::from(character));
            }
            character => escaped.push(character),
        }
    }
    escaped
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
        assert!(
            !escaped.chars().any(super::is_bidi_control),
            "no bidi control survives: {escaped}"
        );
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
    fn truncation_stops_before_an_incomplete_cluster() {
        assert_eq!(take_cells("ab日", 2, WidthMode::Narrow), "ab");
    }
}
