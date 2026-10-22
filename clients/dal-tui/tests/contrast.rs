//! Committed contrast gate: floor sensitivity plus shipped-name resolution.

use dal_tui::theme::{
    Fill, OVERRIDES, Role, SHIPPED_THEMES, builtin_id, contrast_ratio, fill_name, load,
    token_color, token_name,
};
use dal_tui::{ColorMode, ThemeRequest};
use proptest::prelude::*;
use ratatui::style::Color;

const CORE_ROLES: [Role; 6] = [
    Role::Text,
    Role::Dim,
    Role::Accent,
    Role::Success,
    Role::Warning,
    Role::Error,
];
const ALL_ROLES: [Role; 7] = [
    Role::Text,
    Role::Dim,
    Role::Faint,
    Role::Accent,
    Role::Success,
    Role::Warning,
    Role::Error,
];
const FILLS: [Fill; 3] = [Fill::Canvas, Fill::Surface, Fill::Selected];

/// The exact gate foreground for one shipped pair, overrides included.
fn gate_foreground(name: &str, role: Role, fill: Fill) -> Option<opaline::OpalineColor> {
    let resolved = load(
        &ThemeRequest::Named(name.into()),
        ColorMode::Truecolor,
        None,
        None,
    )
    .ok()?;
    let Color::Rgb(red, green, blue) = resolved.color_on(role, fill) else {
        return None;
    };
    Some(opaline::OpalineColor::new(red, green, blue))
}

fn gate_background(name: &str, fill: Fill) -> Option<opaline::OpalineColor> {
    opaline::builtins::load_by_name(builtin_id(name)).map(|theme| theme.color(fill_name(fill)))
}

fn with_channel(color: opaline::OpalineColor, channel: usize, value: u8) -> opaline::OpalineColor {
    match channel {
        0 => opaline::OpalineColor::new(value, color.g, color.b),
        1 => opaline::OpalineColor::new(color.r, value, color.b),
        _ => opaline::OpalineColor::new(color.r, color.g, value),
    }
}

#[test]
fn contrast_floor_reachable_by_a_one_channel_edit() {
    let marginal = SHIPPED_THEMES
        .iter()
        .flat_map(|name| {
            CORE_ROLES.iter().flat_map(move |role| {
                FILLS.iter().filter_map(move |fill| {
                    let foreground = gate_foreground(name, *role, *fill)?;
                    let background = gate_background(name, *fill)?;
                    Some((name, *role, *fill, foreground, background))
                })
            })
        })
        .chain(SHIPPED_THEMES.iter().filter_map(|name| {
            let foreground = gate_foreground(name, Role::Faint, Fill::Canvas)?;
            let background = gate_background(name, Fill::Canvas)?;
            Some((name, Role::Faint, Fill::Canvas, foreground, background))
        }))
        .map(|(name, role, fill, foreground, background)| {
            (
                name,
                role,
                fill,
                foreground,
                background,
                contrast_ratio(foreground, background),
            )
        })
        .find(|(_, _, _, _, _, ratio)| (4.5..6.0).contains(ratio));
    let Some((name, role, fill, foreground, background, base)) = marginal else {
        panic!("no shipped gate pair sits near the 4.5 floor");
    };
    let crossing = (0..3).flat_map(|channel| {
        (0..=255u8).map(move |value| {
            let edited = with_channel(foreground, channel, value);
            (channel, value, edited)
        })
    });
    let Some((channel, value, edited)) = crossing
        .map(|(channel, value, edited)| {
            (channel, value, edited, contrast_ratio(edited, background))
        })
        .find(|(_, _, _, ratio)| *ratio < 4.5)
        .map(|(channel, value, edited, _)| (channel, value, edited))
    else {
        panic!("{name} {role:?} on {fill:?} holds {base:.2} across every one-channel edit");
    };
    let untouched: Vec<u8> = [foreground.r, foreground.g, foreground.b]
        .into_iter()
        .enumerate()
        .filter(|(index, _)| *index != channel)
        .map(|(_, channel_value)| channel_value)
        .collect();
    let kept: Vec<u8> = [edited.r, edited.g, edited.b]
        .into_iter()
        .enumerate()
        .filter(|(index, _)| *index != channel)
        .map(|(_, channel_value)| channel_value)
        .collect();
    assert_eq!(untouched, kept, "exactly one channel moved");
    assert!(
        contrast_ratio(edited, background) < base,
        "the edit moves toward the background"
    );
    let _ = value;
}

#[test]
fn shipped_names_resolve_to_their_builtin_token_sets() {
    assert_eq!(SHIPPED_THEMES.len(), 9);
    for name in SHIPPED_THEMES {
        let builtin = opaline::builtins::load_by_name(builtin_id(name))
            .unwrap_or_else(|| panic!("{name} builtin is missing"));
        let resolved = load(
            &ThemeRequest::Named(name.into()),
            ColorMode::Truecolor,
            None,
            None,
        )
        .unwrap_or_else(|error| panic!("{name} loads: {error}"));
        assert_eq!(resolved.name(), Some(name));
        for role in ALL_ROLES {
            assert!(
                builtin.has_token(token_name(role)),
                "{name} builtin lacks {}",
                token_name(role)
            );
            for fill in FILLS {
                assert!(
                    builtin.has_token(fill_name(fill)),
                    "{name} builtin lacks {}",
                    fill_name(fill)
                );
                let overridden = OVERRIDES
                    .iter()
                    .any(|item| item.theme == name && item.role == role && item.fill == fill);
                if overridden {
                    continue;
                }
                assert_eq!(
                    resolved.color_on(role, fill),
                    token_color(&builtin, role).into(),
                    "{name} {role:?} on {fill:?} differs from its builtin"
                );
            }
        }
    }
    assert!(
        load(
            &ThemeRequest::Named("zzz".into()),
            ColorMode::Truecolor,
            None,
            None
        )
        .is_err(),
        "unknown theme names still fail"
    );
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

/// Each luminance coefficient is pinned by a pure channel: swapping the red
/// and blue weights (0.2126/0.0722) trades the 5.25 and 2.44 below, and any
/// change to the green weight moves 15.30. The WCAG 4.5 floor is pinned from
/// both sides by two greys one step apart, so a moved floor or a broken
/// threshold comparison flips one of the two assertions.
#[test]
fn contrast_ratio_pins_each_channel_weight_and_the_4_5_edge() {
    let black = opaline::OpalineColor::new(0, 0, 0);
    let white = opaline::OpalineColor::new(255, 255, 255);
    let close = |actual: f64, expected: f64| (actual - expected).abs() < 0.01;
    assert!(
        close(
            contrast_ratio(opaline::OpalineColor::new(255, 0, 0), black),
            5.25
        ),
        "red weight moved"
    );
    assert!(
        close(
            contrast_ratio(opaline::OpalineColor::new(0, 255, 0), black),
            15.30
        ),
        "green weight moved"
    );
    assert!(
        close(
            contrast_ratio(opaline::OpalineColor::new(0, 0, 255), black),
            2.44
        ),
        "blue weight moved"
    );
    let just_below = contrast_ratio(opaline::OpalineColor::new(119, 119, 119), white);
    let just_above = contrast_ratio(opaline::OpalineColor::new(118, 118, 118), white);
    assert!(
        (just_below - 4.48).abs() < 0.02,
        "grey #777 no longer measures 4.48: {just_below}"
    );
    assert!(
        (just_above - 4.54).abs() < 0.02,
        "grey #767676 no longer measures 4.54: {just_above}"
    );
    assert!(just_below < 4.5, "#777 must sit below the floor");
    assert!(just_above > 4.5, "#767676 must sit above the floor");
}
