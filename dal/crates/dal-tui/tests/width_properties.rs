//! Property tests for the one width function: additivity, wrap safety, CJK scope.

use dal_tui::width::{WidthMode, take_cells, width, wrap};
use proptest::prelude::*;
use unicode_segmentation::UnicodeSegmentation;

fn alphabet() -> impl Strategy<Value = char> {
    prop_oneof![
        Just('a'),
        Just(' '),
        Just('日'),
        Just('漢'),
        Just('가'),
        Just('\u{0301}'),
        Just('❤'),
        Just('\u{fe0f}'),
        Just('·'),
        Just('±'),
        Just('#'),
        Just('🇫'),
    ]
}

fn text_strategy() -> impl Strategy<Value = String> {
    prop::collection::vec(alphabet(), 0..40).prop_map(|chars| chars.into_iter().collect())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn width_additivity(s in text_strategy(), mode in prop_oneof![Just(WidthMode::Narrow), Just(WidthMode::Cjk)]) {
        let clusters: Vec<&str> = s.graphemes(true).collect();
        if clusters.is_empty() {
            prop_assert_eq!(width(&s, mode), 0);
        } else {
            let at: usize = clusters.len() / 2;
            let a: String = clusters[..at].concat();
            let b: String = clusters[at..].concat();
            prop_assert_eq!(width(&s, mode), width(&a, mode) + width(&b, mode));
        }
    }

    #[test]
    fn wrap_safety(s in text_strategy(), cap in 1usize..120, cjk in prop::bool::ANY) {
        let mode = if cjk { WidthMode::Cjk } else { WidthMode::Narrow };
        let rows = wrap(&s, cap, mode);
        prop_assert!(!rows.is_empty());
        let joined: String = rows.concat();
        prop_assert_eq!(joined.as_str(), s.as_str());
        for row in &rows {
            let row_clusters: Vec<&str> = row.graphemes(true).collect();
            if row_clusters.len() == 1 {
                continue;
            }
            prop_assert!(
                width(row, mode) <= cap,
                "row {row:?} measures {} over cap {cap}",
                width(row, mode)
            );
        }
        for row in &rows {
            let row_width = width(row, mode);
            for cluster in row.graphemes(true) {
                let cluster_width = width(cluster, mode);
                if cluster_width == 2 {
                    prop_assert!(
                        row_width <= cap || row.graphemes(true).count() == 1,
                        "wide cluster split or overflow in {row:?}"
                    );
                }
            }
        }
        let _ = take_cells(&s, cap, mode);
    }

    #[test]
    fn cjk_ambiguous_only(s in text_strategy()) {
        for cluster in s.graphemes(true) {
            let narrow = width(cluster, WidthMode::Narrow);
            let cjk = width(cluster, WidthMode::Cjk);
            if narrow == cjk {
                continue;
            }
            for scalar in cluster.chars() {
                // Zero-width scalars (combining marks, joiners, selectors)
                // inherit the base scalar's width; they are never ambiguous
                // themselves and carry no cells of their own.
                if width(&scalar.to_string(), WidthMode::Narrow) == 0
                    && width(&scalar.to_string(), WidthMode::Cjk) == 0
                {
                    continue;
                }
                let scalar = u32::from(scalar);
                prop_assert!(
                    is_ambiguous_excerpt(scalar),
                    "width differs for non-ambiguous scalar U+{scalar:04X} in {cluster:?}"
                );
            }
        }
    }
}

/// Committed excerpt of EastAsianWidth.txt Ambiguous ranges covering the test alphabet.
fn is_ambiguous_excerpt(scalar: u32) -> bool {
    matches!(
        scalar,
        0x00A1
            | 0x00A4
            | 0x00A7..=0x00A8
            | 0x00AA
            | 0x00AD..=0x00AE
            | 0x00B0..=0x00B1
            | 0x00B2..=0x00B3
            | 0x00B7
            | 0x00BB
            | 0x00BF
    )
}
