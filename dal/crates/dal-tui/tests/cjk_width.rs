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
    use unicode_segmentation::UnicodeSegmentation;
    for text in [
        "日本語でお願いします",
        "한국어 테스트입니다",
        "👨‍👩‍👧‍👦 family 🇯🇵 flag 漢字",
        "a\u{301}\u{65e5}·\u{fe0f}",
        "\u{1112}\u{1161}\u{11ab}\u{1100}\u{116e}\u{11a8}\u{110b}\u{1165}",
    ] {
        let original: Vec<&str> = text.graphemes(true).collect();
        let wrapped = wrap(text, 3, N);
        // Rows lose nothing and reorder nothing: their concatenation is the
        // original text, and every row boundary lands on an original cluster
        // boundary — a cluster split across rows fails the pairwise check.
        assert_eq!(wrapped.concat(), text);
        let mut offset = 0_usize;
        for cluster in wrapped.iter().flat_map(|row| row.graphemes(true)) {
            assert_eq!(cluster, original[offset], "row split an original cluster");
            offset += 1;
        }
        assert_eq!(offset, original.len());
        // `take_cells` returns the exact concatenation of a prefix of the
        // original clusters — byte-identical, never a partial grapheme.
        let taken = take_cells(text, 3, N);
        let mut prefix = String::new();
        for cluster in &original {
            if prefix.len() + cluster.len() > taken.len() {
                break;
            }
            prefix.push_str(cluster);
        }
        assert_eq!(prefix, taken, "{text:?} truncated mid-cluster");
    }
}

#[test]
fn decomposed_hangul_measures_like_precomposed() {
    // NFD jamo and the equivalent NFC syllable are the same two-cell glyph.
    assert_eq!(width("한", N), 2);
    assert_eq!(width("\u{1112}\u{1161}\u{11ab}", N), 2);
    // LV syllables (no final) decompose identically.
    assert_eq!(width("\u{1112}\u{1161}", N), width("하", N));
    // A mixed NFD/NFC document counts cells per syllable, not per codepoint.
    assert_eq!(width("\u{1112}\u{1161}\u{11ab}국어", N), width("한국어", N));
    assert_eq!(width("\u{1112}\u{1161}\u{11ab}국어", N), 6);
}

/// The wrap layout of `text`: one `(row width, cluster count)` pair per row.
/// Decomposed and precomposed spellings must produce identical layouts even
/// though their emitted bytes differ.
fn layout(text: &str, cap: usize) -> Vec<(usize, usize)> {
    wrap(text, cap, N)
        .iter()
        .map(|row| {
            (
                width(row, N),
                unicode_segmentation::UnicodeSegmentation::graphemes(row.as_str(), true).count(),
            )
        })
        .collect()
}

#[test]
fn conjoining_jamo_chains_wrap_whole_syllables() {
    // 한국어 fully decomposed: three three-jamo clusters.
    let decomposed = "\u{1112}\u{1161}\u{11ab}\u{1100}\u{116e}\u{11a8}\u{110b}\u{1165}";
    // Decomposed input must wrap and truncate with the identical cell layout
    // as the precomposed spelling.
    assert_eq!(layout(decomposed, 4), layout("한국어", 4));
    assert_eq!(
        width(&take_cells(decomposed, 5, N), N),
        width(&take_cells("한국어", 5, N), N)
    );
    // Never return half a syllable.
    assert_eq!(take_cells(decomposed, 3, N), "\u{1112}\u{1161}\u{11ab}");
    assert_eq!(take_cells(decomposed, 1, N), "");
    // A sentence keeps whole-syllable rows: decomposed and composed agree.
    let sentence = "\u{1112}\u{1161}\u{11ab}\u{1100}\u{116e}\u{11a8}\u{110b}\u{1165}\u{1105}\u{1169} \u{1106}\u{1161}\u{11af}\u{1112}\u{1162}\u{110b}\u{116d}";
    assert_eq!(layout(sentence, 5), layout("한국어로 말해요", 5));
}

#[test]
fn jamo_fragments_fillers_and_archaic_blocks_compose() {
    // Illegal-but-joined jamo sequences stay one cluster (UAX #29 L × L, and
    // filler/arae-a chains): a wrap must move each whole or not at all.
    for cluster in ["\u{1100}\u{1100}", "\u{115f}\u{1161}", "\u{1100}\u{119e}"] {
        let rows = wrap(&format!("x{cluster}"), 2, N);
        assert_eq!(rows.concat(), format!("x{cluster}"));
        assert!(
            rows.iter().any(|row| row.contains(cluster)),
            "the joined jamo sequence must survive as one cluster: {rows:?}"
        );
        // The cluster measures two cells (one syllable glyph or stacked jamo).
        assert_eq!(width(cluster, N), 2, "{cluster:?}");
    }
    // Extended-B jungseong in an old-orthography chain composes identically.
    assert_eq!(width("\u{1105}\u{d7b2}", N), 2);
    // A lone jamo is wide on its own (EAW=W across the jamo blocks).
    assert_eq!(width("\u{1100}", N), 2);
    assert_eq!(width("\u{d7b2}", N), 2);
}

#[test]
fn compatibility_and_halfwidth_jamo_follow_their_widths() {
    // Hangul compatibility jamo (U+3130–318F) are wide letters.
    assert_eq!(width("\u{3131}\u{3134}\u{3137}", N), 6);
    // Halfwidth jamo (U+FFA0–FFDC) stay one cell each.
    assert_eq!(width("\u{ffa1}\u{ffa2}", N), 2);
    assert_eq!(
        wrap("\u{ffa1}\u{ffa2}\u{ffa3}", 2, N),
        ["\u{ffa1}\u{ffa2}", "\u{ffa3}"]
    );
    // Mixed halfwidth + syllable: the margin rule still moves a wide cluster whole.
    assert_eq!(wrap("\u{ffa1}한\u{ffa2}", 3, N), ["\u{ffa1}한", "\u{ffa2}"]);
}

#[test]
fn korean_wraps_syllable_clusters_at_the_margin() {
    // 5-cell cap: two syllables fit, a third moves whole; the trailing space
    // stays with its row.
    assert_eq!(
        wrap("한국어로 말해요", 5, N),
        ["한국", "어로 ", "말해", "요"]
    );
    // take_cells stops at a syllable boundary.
    assert_eq!(take_cells("한국어", 5, N), "한국");
    assert_eq!(take_cells("한국어", 3, N), "한");
}
