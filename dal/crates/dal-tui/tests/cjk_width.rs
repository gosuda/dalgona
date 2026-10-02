//! Deterministic CJK and emoji defect-class coverage: terminal-truth cell
//! widths, cluster-safe wrap/truncate, and locale mode selection.

use dal_tui::width::{WidthMode, take_cells, width, wrap};

const N: WidthMode = WidthMode::Narrow;
const W: WidthMode = WidthMode::Cjk;

#[test]
fn east_asian_fullwidth_and_wide_letters_measure_two_cells() {
    assert_eq!(width("あいうえお", N), 10);
    assert_eq!(width("한국어", N), 6);
    assert_eq!(width("繁體中文", N), 8);
    // Fullwidth ASCII forms (FF01–FF60) are two-cell letters.
    assert_eq!(width("ＡＢＣ", N), 6);
    assert_eq!(width("１２３", N), 6);
}

#[test]
fn halfwidth_forms_measure_one_cell() {
    // Halfwidth katakana and hangul jamo (FF61–FFDC) fit single cells.
    assert_eq!(width("ｱｲｳ", N), 3);
    assert_eq!(width("ﾊﾝｶｸ", N), 4);
}

#[test]
fn hangul_jamo_compose_into_one_wide_cluster() {
    // Three conjoining jamo render as one two-cell syllable.
    assert_eq!(width("\u{1100}\u{1161}\u{11a8}", N), 2);
    // Extended jamo blocks compose identically.
    assert_eq!(width("\u{a960}\u{1175}", N), 2);
}

#[test]
fn regional_indicator_pair_is_one_flag() {
    // A flag is one two-cell glyph on every terminal; the lone RI inside it
    // must not be counted per-codepoint.
    assert_eq!(width("🇯🇵", N), 2);
    assert_eq!(width("🇫🇷", N), 2);
    // A lone regional indicator still renders as two boxed letters.
    assert_eq!(width("🇯", N), 2);
    // Two flags are two graphemes → four cells.
    assert_eq!(width("🇯🇵🇫🇷", N), 4);
}

#[test]
fn emoji_sequences_stay_one_cluster() {
    assert_eq!(width("👨‍👩‍👧", N), 2);
    assert_eq!(width("🏳️‍🌈", N), 2);
    assert_eq!(width("👍🏽", N), 2);
    // Keycap sequence: base + VS16 + combining enclosing keycap.
    assert_eq!(width("1\u{fe0f}\u{20e3}", N), 2);
    // Text-presentation override narrows the emoji back to one cell.
    assert_eq!(width("❤\u{fe0e}", N), 1);
}

#[test]
fn ambiguous_width_switches_with_locale_mode() {
    // The EastAsianWidth=Ambiguous set unicode-width 0.2 honors: Latin-1
    // symbols, arrows, box drawing, math operators, №. Greek and Cyrillic
    // letters are Neutral there, not Ambiguous, and stay one cell.
    for character in [
        '·', '±', '§', '×', '÷', '¡', '®', '°', '¶', '№', '←', '→', '─', '│', '■', '□', '●', '○',
        '♥', '♣', '♠', '≈', '≠', '≤', '≥', '∞', '∫', '√', '∑', '∏',
    ] {
        assert_eq!(width(&character.to_string(), N), 1, "{character} narrow");
        assert_eq!(width(&character.to_string(), W), 2, "{character} cjk");
    }
    for character in ['α', 'Ω', 'π', 'Д', 'ж'] {
        assert_eq!(
            width(&character.to_string(), W),
            1,
            "{character} stays narrow"
        );
    }
}

#[test]
fn wrap_never_splits_a_wide_cluster_at_the_margin() {
    // A two-cell cluster starting on the last odd cell wraps whole.
    assert_eq!(wrap("ab日", 4, N), ["ab日"]);
    assert_eq!(wrap("abc日", 4, N), ["abc", "日"]);
    // A combining cluster must not split its mark onto the next row.
    assert_eq!(wrap("xa\u{301}", 2, N), ["xa\u{301}"]);
    // One-cell caps carry lone wide clusters per row, never half-glyph.
    assert_eq!(wrap("日a日", 1, N), ["日", "a", "日"]);
}

#[test]
fn wrap_preserves_mixed_cjk_latin_cell_math() {
    // 日本語 = 6 cells, then "ab" = 2 cells at cap 8 exactly.
    assert_eq!(wrap("日本語ab", 8, N), ["日本語ab"]);
    // Greedy fill: the 8-cell row takes "ab" and only "c" spills.
    assert_eq!(wrap("日本語abc", 8, N), ["日本語ab", "c"]);
    // A wide cluster that cannot fit in the tail moves whole to the next row.
    assert_eq!(wrap("日本語a日", 8, N), ["日本語a", "日"]);
}

#[test]
fn take_cells_stops_before_an_incomplete_cluster() {
    assert_eq!(take_cells("日本語", 5, N), "日本");
    assert_eq!(take_cells("日本語", 4, N), "日本");
    assert_eq!(take_cells("日本語", 3, N), "日");
    assert_eq!(take_cells("日本語", 1, N), "");
    // Combining marks follow their base, never orphan onto the line.
    assert_eq!(take_cells("a\u{301}日", 2, N), "a\u{301}");
    // A flag is taken whole or not at all.
    assert_eq!(take_cells("🇯🇵ab", 2, N), "🇯🇵");
    assert_eq!(take_cells("🇯🇵ab", 1, N), "");
}

#[test]
fn cjk_locale_selects_wide_ambiguous_measurement() {
    assert_eq!(WidthMode::from_locale(["ja_JP.UTF-8", "", ""]), W);
    assert_eq!(WidthMode::from_locale(["zh_TW.Big5", "", ""]), W);
    assert_eq!(WidthMode::from_locale(["ko_KR.eucKR", "", ""]), W);
    assert_eq!(WidthMode::from_locale(["zh-Hans-CN", "", ""]), W);
    assert_eq!(WidthMode::from_locale(["en_US.UTF-8", "", ""]), N);
    assert_eq!(WidthMode::from_locale(["C.UTF-8", "", ""]), N);
    assert_eq!(WidthMode::from_locale(["vi_VN.UTF-8", "", ""]), N);
    // The first non-empty value wins; LC_ALL precedes LC_CTYPE then LANG.
    assert_eq!(WidthMode::from_locale(["", "", "ja_JP.UTF-8"]), W);
    assert_eq!(
        WidthMode::from_locale(["en_US.UTF-8", "ja_JP.UTF-8", ""]),
        N
    );
    assert_eq!(WidthMode::from_locale(["", "", ""]), N);
}

#[test]
fn grapheme_aware_text_stays_valid_through_wrap_and_take() {
    // Every emitted row must re-parse as valid UTF-8 with all clusters whole.
    for text in [
        "日本語でお願いします",
        "한국어 테스트입니다",
        "👨‍👩‍👧‍👦 family 🇯🇵 flag 漢字",
        "a\u{301}\u{65e5}·\u{fe0f}",
    ] {
        for row in wrap(text, 3, N) {
            assert!(row.is_char_boundary(row.len()), "{row:?}");
            assert_eq!(
                unicode_segmentation::UnicodeSegmentation::graphemes(row.as_str(), true)
                    .collect::<String>(),
                row
            );
        }
        let taken = take_cells(text, 3, N);
        assert!(taken.is_char_boundary(taken.len()));
    }
}
