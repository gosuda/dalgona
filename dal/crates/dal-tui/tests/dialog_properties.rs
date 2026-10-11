//! Approval preview bytes property: body text joins to escaped preview.

use dal_tui::width::escape;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn approval_bytes_property(preview in prop::collection::vec(prop::char::any(), 0..200)) {
        let preview: String = preview.into_iter().collect();
        let escaped = escape(&preview);
        let introduced_bel = escaped.contains('\u{7}') && !preview.contains('\u{7}');
        prop_assert!(!introduced_bel, "escape introduced BEL");
        prop_assert!(escape(&escaped).len() >= escaped.len(), "double escape shrank");
    }
}
