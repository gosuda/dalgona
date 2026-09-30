//! Committed contrast gate: WCAG oracle, shipped names, all 171 pairs.

use dal_tui::theme::{SHIPPED_THEMES, contrast_ratio, verify_contrast_gate};
use proptest::prelude::*;

#[test]
fn contrast_gate_oracle() {
    let black = opaline::OpalineColor::new(0, 0, 0);
    let white = opaline::OpalineColor::new(255, 255, 255);
    let gray = opaline::OpalineColor::new(119, 119, 119);
    assert!((contrast_ratio(black, white) - 21.0).abs() < 0.0001);
    assert!((contrast_ratio(gray, white) - 4.48).abs() < 0.02);
    let result = verify_contrast_gate();
    assert!(
        result.is_ok(),
        "contrast gate failed: {:?}",
        result.as_ref().err()
    );
    assert_eq!(result.ok(), Some(171));
}

#[test]
fn shipped_theme_names() {
    assert_eq!(SHIPPED_THEMES.len(), 9);
    for name in SHIPPED_THEMES {
        assert!(
            opaline::builtins::load_by_name(name).is_some(),
            "{name} missing"
        );
    }
    assert!(opaline::builtins::load_by_name("zzz").is_none());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn contrast_ratio_property(r in 0u8..=255, g in 0u8..=255, b in 0u8..=255) {
        let color = opaline::OpalineColor::new(r, g, b);
        let white = opaline::OpalineColor::new(255, 255, 255);
        let ratio = contrast_ratio(color, white);
        prop_assert!((1.0..=21.0).contains(&ratio));
        prop_assert!((contrast_ratio(white, color) - ratio).abs() < 1e-9);
    }
}
