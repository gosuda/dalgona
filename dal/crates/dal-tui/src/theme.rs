//! Shipped opaline themes, semantic terminal roles, and contrast checks.

use opaline::{OpalineColor, Theme as OpalineTheme, builtins, names::tokens};
use ratatui::style::{Color, Modifier, Style};

use crate::{ColorMode, ThemeRequest, TuiError};

/// Exact built-in themes shipped by dal.
pub const SHIPPED_THEMES: [&str; 9] = [
    "flexoki-dark",
    "flexoki-light",
    "github-dark",
    "github-light",
    "kanagawa-wave",
    "catppuccin-mocha",
    "rose-pine",
    "ayu-mirage",
    "catppuccin-latte",
];
/// One same-palette color substitution that satisfies a measured contrast pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoleOverride {
    /// The shipped theme to which this substitution applies.
    pub theme: &'static str,
    /// The foreground role being replaced.
    pub role: Role,
    /// The measured background fill.
    pub fill: Fill,
    /// A shipped token or named same-palette contrast correction.
    pub token: &'static str,
}

/// Committed contrast substitutions, measured with each theme as one unit.
pub const OVERRIDES: &[RoleOverride] = &[
    RoleOverride {
        theme: "flexoki-dark",
        role: Role::Accent,
        fill: Fill::Selected,
        token: "orange_light",
    },
    RoleOverride {
        theme: "flexoki-dark",
        role: Role::Success,
        fill: Fill::Selected,
        token: "green_light",
    },
    RoleOverride {
        theme: "flexoki-dark",
        role: Role::Error,
        fill: Fill::Canvas,
        token: "red_light",
    },
    RoleOverride {
        theme: "flexoki-dark",
        role: Role::Error,
        fill: Fill::Surface,
        token: "red_light",
    },
    RoleOverride {
        theme: "flexoki-dark",
        role: Role::Error,
        fill: Fill::Selected,
        token: "fg_200",
    },
    RoleOverride {
        theme: "flexoki-light",
        role: Role::Accent,
        fill: Fill::Selected,
        token: "orange_700",
    },
    RoleOverride {
        theme: "flexoki-light",
        role: Role::Success,
        fill: Fill::Canvas,
        token: "green_800",
    },
    RoleOverride {
        theme: "flexoki-light",
        role: Role::Success,
        fill: Fill::Surface,
        token: "green_800",
    },
    RoleOverride {
        theme: "flexoki-light",
        role: Role::Success,
        fill: Fill::Selected,
        token: "green_800",
    },
    RoleOverride {
        theme: "flexoki-light",
        role: Role::Warning,
        fill: Fill::Canvas,
        token: "yellow_800",
    },
    RoleOverride {
        theme: "flexoki-light",
        role: Role::Warning,
        fill: Fill::Surface,
        token: "yellow_800",
    },
    RoleOverride {
        theme: "flexoki-light",
        role: Role::Warning,
        fill: Fill::Selected,
        token: "yellow_800",
    },
    RoleOverride {
        theme: "github-dark",
        role: Role::Dim,
        fill: Fill::Canvas,
        token: "fg_muted_contrast",
    },
    RoleOverride {
        theme: "github-dark",
        role: Role::Dim,
        fill: Fill::Surface,
        token: "fg_muted_contrast",
    },
    RoleOverride {
        theme: "github-dark",
        role: Role::Dim,
        fill: Fill::Selected,
        token: "fg_muted_contrast",
    },
    RoleOverride {
        theme: "github-dark",
        role: Role::Accent,
        fill: Fill::Selected,
        token: "blue3",
    },
    RoleOverride {
        theme: "github-dark",
        role: Role::Success,
        fill: Fill::Selected,
        token: "green3",
    },
    RoleOverride {
        theme: "github-dark",
        role: Role::Error,
        fill: Fill::Canvas,
        token: "red3",
    },
    RoleOverride {
        theme: "github-dark",
        role: Role::Error,
        fill: Fill::Surface,
        token: "red3",
    },
    RoleOverride {
        theme: "github-dark",
        role: Role::Error,
        fill: Fill::Selected,
        token: "red3",
    },
    RoleOverride {
        theme: "github-dark",
        role: Role::Faint,
        fill: Fill::Canvas,
        token: "fg_muted",
    },
    RoleOverride {
        theme: "kanagawa-wave",
        role: Role::Accent,
        fill: Fill::Selected,
        token: "spring_violet2",
    },
    RoleOverride {
        theme: "kanagawa-wave",
        role: Role::Faint,
        fill: Fill::Canvas,
        token: "fuji_gray",
    },
    RoleOverride {
        theme: "catppuccin-mocha",
        role: Role::Accent,
        fill: Fill::Selected,
        token: "lavender",
    },
    RoleOverride {
        theme: "catppuccin-mocha",
        role: Role::Error,
        fill: Fill::Selected,
        token: "rosewater",
    },
    RoleOverride {
        theme: "rose-pine",
        role: Role::Dim,
        fill: Fill::Selected,
        token: "text",
    },
    RoleOverride {
        theme: "rose-pine",
        role: Role::Faint,
        fill: Fill::Canvas,
        token: "muted",
    },
    RoleOverride {
        theme: "rose-pine",
        role: Role::Error,
        fill: Fill::Selected,
        token: "rose",
    },
    RoleOverride {
        theme: "ayu-mirage",
        role: Role::Dim,
        fill: Fill::Canvas,
        token: "fg_default",
    },
    RoleOverride {
        theme: "ayu-mirage",
        role: Role::Dim,
        fill: Fill::Surface,
        token: "fg_default",
    },
    RoleOverride {
        theme: "ayu-mirage",
        role: Role::Dim,
        fill: Fill::Selected,
        token: "fg_default",
    },
    RoleOverride {
        theme: "ayu-mirage",
        role: Role::Faint,
        fill: Fill::Canvas,
        token: "fg_muted",
    },
    RoleOverride {
        theme: "catppuccin-latte",
        role: Role::Text,
        fill: Fill::Selected,
        token: "text_800",
    },
    RoleOverride {
        theme: "catppuccin-latte",
        role: Role::Dim,
        fill: Fill::Selected,
        token: "text_800",
    },
    RoleOverride {
        theme: "catppuccin-latte",
        role: Role::Faint,
        fill: Fill::Canvas,
        token: "overlay2",
    },
    RoleOverride {
        theme: "catppuccin-latte",
        role: Role::Accent,
        fill: Fill::Surface,
        token: "mauve_800",
    },
    RoleOverride {
        theme: "catppuccin-latte",
        role: Role::Accent,
        fill: Fill::Selected,
        token: "mauve_800",
    },
    RoleOverride {
        theme: "catppuccin-latte",
        role: Role::Success,
        fill: Fill::Canvas,
        token: "green_900",
    },
    RoleOverride {
        theme: "catppuccin-latte",
        role: Role::Success,
        fill: Fill::Surface,
        token: "green_900",
    },
    RoleOverride {
        theme: "catppuccin-latte",
        role: Role::Success,
        fill: Fill::Selected,
        token: "green_900",
    },
    RoleOverride {
        theme: "catppuccin-latte",
        role: Role::Warning,
        fill: Fill::Canvas,
        token: "yellow_900",
    },
    RoleOverride {
        theme: "catppuccin-latte",
        role: Role::Warning,
        fill: Fill::Surface,
        token: "yellow_900",
    },
    RoleOverride {
        theme: "catppuccin-latte",
        role: Role::Warning,
        fill: Fill::Selected,
        token: "yellow_900",
    },
    RoleOverride {
        theme: "catppuccin-latte",
        role: Role::Error,
        fill: Fill::Surface,
        token: "red_900",
    },
    RoleOverride {
        theme: "catppuccin-latte",
        role: Role::Error,
        fill: Fill::Selected,
        token: "red_900",
    },
];

/// A missing built-in theme or a failed semantic color pair.
#[derive(Debug, thiserror::Error)]
pub enum ContrastError {
    /// A shipped theme name did not resolve to an embedded opaline theme.
    #[error("built-in theme {name} is missing")]
    BuiltinMissing {
        /// The missing shipped name.
        name: Box<str>,
    },
    /// A foreground/background pair did not meet its measured floor.
    #[error(transparent)]
    Pair(#[from] ContrastFailure),
}

/// A failed role/background contrast pair.
#[derive(Debug, Clone, PartialEq)]
pub struct ContrastFailure {
    /// Theme whose pair failed.
    pub theme: Box<str>,
    /// Foreground semantic role.
    pub role: Role,
    /// Background fill.
    pub fill: Fill,
    /// Measured WCAG ratio.
    pub ratio: f64,
    /// Required minimum ratio.
    pub minimum: f64,
    /// Foreground color in hexadecimal.
    pub foreground: Box<str>,
    /// Background color in hexadecimal.
    pub background: Box<str>,
}

impl std::fmt::Display for ContrastFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} {:?} on {:?}: ratio {:.2} ({}, {}) is below {:.1}",
            self.theme,
            self.role,
            self.fill,
            self.ratio,
            self.foreground,
            self.background,
            self.minimum
        )
    }
}

impl std::error::Error for ContrastFailure {}

/// Runs the shipped-theme contrast gate and returns the number of checked pairs.
///
/// # Errors
/// Returns the first missing theme or failing pair with all measured values.
pub fn verify_contrast_gate() -> Result<usize, ContrastError> {
    let mut pairs = 0;
    for name in SHIPPED_THEMES {
        let Some(theme) = builtins::load_by_name(builtin_id(name)) else {
            return Err(ContrastError::BuiltinMissing { name: name.into() });
        };
        for role in [
            Role::Text,
            Role::Dim,
            Role::Accent,
            Role::Success,
            Role::Warning,
            Role::Error,
        ] {
            for fill in [Fill::Canvas, Fill::Surface, Fill::Selected] {
                let foreground = contrast_foreground(&theme, name, role, fill);
                let background = theme.color(fill_name(fill));
                let ratio = contrast_ratio(foreground, background);
                pairs += 1;
                if ratio < 4.5 {
                    return Err(contrast_failure(
                        name, role, fill, ratio, 4.5, foreground, background,
                    )
                    .into());
                }
            }
        }
        let faint = contrast_foreground(&theme, name, Role::Faint, Fill::Canvas);
        let canvas = theme.color(fill_name(Fill::Canvas));
        let ratio = contrast_ratio(faint, canvas);
        pairs += 1;
        if ratio < 3.0 {
            return Err(contrast_failure(
                name,
                Role::Faint,
                Fill::Canvas,
                ratio,
                3.0,
                faint,
                canvas,
            )
            .into());
        }
    }
    Ok(pairs)
}

const FLEXOKI_LIGHT_ORANGE_700: OpalineColor = OpalineColor::new(0x9D, 0x43, 0x10);
const FLEXOKI_LIGHT_GREEN_800: OpalineColor = OpalineColor::new(0x50, 0x68, 0x09);
const GITHUB_DARK_DIM: OpalineColor = OpalineColor::new(0x94, 0xA0, 0xAD);
const FLEXOKI_LIGHT_YELLOW_800: OpalineColor = OpalineColor::new(0x7C, 0x5B, 0x00);
const CATPPUCCIN_LATTE_TEXT_800: OpalineColor = OpalineColor::new(0x44, 0x47, 0x5E);
const CATPPUCCIN_LATTE_MAUVE_800: OpalineColor = OpalineColor::new(0x5C, 0x2F, 0xA8);
const CATPPUCCIN_LATTE_GREEN_900: OpalineColor = OpalineColor::new(0x23, 0x5A, 0x17);
const CATPPUCCIN_LATTE_YELLOW_900: OpalineColor = OpalineColor::new(0x71, 0x41, 0x00);
const CATPPUCCIN_LATTE_RED_900: OpalineColor = OpalineColor::new(0x8F, 0x08, 0x24);

fn override_color(theme: &OpalineTheme, name: &str, token: &str) -> OpalineColor {
    match (name, token) {
        ("flexoki-light", "orange_700") => FLEXOKI_LIGHT_ORANGE_700,
        ("flexoki-light", "green_800") => FLEXOKI_LIGHT_GREEN_800,
        ("flexoki-light", "yellow_800") => FLEXOKI_LIGHT_YELLOW_800,
        ("github-dark", "fg_muted_contrast") => GITHUB_DARK_DIM,
        ("catppuccin-latte", "text_800") => CATPPUCCIN_LATTE_TEXT_800,
        ("catppuccin-latte", "mauve_800") => CATPPUCCIN_LATTE_MAUVE_800,
        ("catppuccin-latte", "green_900") => CATPPUCCIN_LATTE_GREEN_900,
        ("catppuccin-latte", "yellow_900") => CATPPUCCIN_LATTE_YELLOW_900,
        ("catppuccin-latte", "red_900") => CATPPUCCIN_LATTE_RED_900,
        _ => theme.color(token),
    }
}

fn contrast_foreground(theme: &OpalineTheme, name: &str, role: Role, fill: Fill) -> OpalineColor {
    OVERRIDES
        .iter()
        .find(|item| item.theme == name && item.role == role && item.fill == fill)
        .map_or_else(
            || token_color(theme, role),
            |item| override_color(theme, name, item.token),
        )
}

fn contrast_failure(
    theme: &str,
    role: Role,
    fill: Fill,
    ratio: f64,
    minimum: f64,
    foreground: OpalineColor,
    background: OpalineColor,
) -> ContrastFailure {
    ContrastFailure {
        theme: theme.into(),
        role,
        fill,
        ratio,
        minimum,
        foreground: foreground.to_hex().into(),
        background: background.to_hex().into(),
    }
}

/// Semantic style role used by TUI components.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Primary readable text.
    Text,
    /// Secondary readable text.
    Dim,
    /// Decorative, non-informational text.
    Faint,
    /// One accent hue.
    Accent,
    /// Successful state.
    Success,
    /// Warning state.
    Warning,
    /// Error state.
    Error,
}

/// Background fill used by contrast verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fill {
    /// Terminal-owned canvas color.
    Canvas,
    /// Neutral panel background.
    Surface,
    /// Neutral selected-row background.
    Selected,
}

/// A loaded shipped theme and the terminal color-depth capability.
#[derive(Debug, Clone)]
pub struct ResolvedTheme {
    name: Option<&'static str>,
    theme: Option<OpalineTheme>,
    mode: ColorMode,
}

impl ResolvedTheme {
    /// Returns the shipped theme name, or `None` for terminal palette mode.
    #[must_use]
    pub const fn name(&self) -> Option<&'static str> {
        self.name
    }

    /// Maps one semantic role to the resolved terminal color depth.
    #[must_use]
    pub fn color(&self, role: Role) -> Color {
        self.color_on(role, Fill::Canvas)
    }

    /// Resolves a semantic role against its actual fill, including contrast overrides.
    #[must_use]
    pub fn color_on(&self, role: Role, fill: Fill) -> Color {
        if self.mode == ColorMode::Never {
            return Color::Reset;
        }
        let source = match (&self.theme, self.name) {
            (Some(theme), Some(name)) => contrast_foreground(theme, name, role, fill).into(),
            _ => palette_color(role),
        };
        match self.mode {
            ColorMode::Truecolor => source,
            ColorMode::Ansi256 => nearest_256(source),
            ColorMode::Sixteen => palette_color(role),
            ColorMode::Never => Color::Reset,
        }
    }

    /// Returns style modifiers that preserve meaning under `NO_COLOR`.
    #[must_use]
    pub const fn modifiers(role: Role) -> Modifier {
        match role {
            Role::Faint | Role::Dim => Modifier::DIM,
            Role::Text | Role::Accent | Role::Success | Role::Warning | Role::Error => {
                Modifier::empty()
            }
        }
    }

    /// Builds a reverse-video selection style that does not depend on color.
    #[must_use]
    pub const fn selection_style() -> Style {
        Style::new().add_modifier(Modifier::REVERSED)
    }
}

/// Loads the built-in theme named by an exact shipped name.
///
/// # Errors
/// Returns the two-line unknown-theme startup error for an unshipped name.
pub fn load(
    request: &ThemeRequest,
    mode: ColorMode,
    background_luminance: Option<f64>,
    colorfgbg: Option<&str>,
) -> Result<ResolvedTheme, TuiError> {
    let selected = match request {
        ThemeRequest::Palette => None,
        ThemeRequest::Named(name) => Some(resolve_name(name)?),
        ThemeRequest::Auto => background_luminance
            .map(|luminance| {
                if luminance <= 0.2 {
                    "flexoki-dark"
                } else {
                    "flexoki-light"
                }
            })
            .or_else(|| colorfgbg.and_then(colorfgbg_theme)),
    };
    let theme = selected.map(builtin_id).and_then(builtins::load_by_name);
    if let Some(name) = selected
        && theme.is_none()
    {
        return Err(crate::term::te_theme(name, None));
    }
    Ok(ResolvedTheme {
        name: selected,
        theme,
        mode,
    })
}

/// Maps a shipped theme name to its opaline built-in id.
///
/// Most shipped names are their own built-in id; `github-dark` loads from
/// the `github-dark-dimmed` built-in, which is the only GitHub dark theme
/// opaline 0.4.2 ships. The shipped name never changes, so user-facing text,
/// configs, and docs keep saying `github-dark`.
#[must_use]
pub fn builtin_id(shipped: &str) -> &str {
    match shipped {
        "github-dark" => "github-dark-dimmed",
        other => other,
    }
}

/// Resolves a name only when it exactly matches the shipped set.
///
/// # Errors
/// Returns an unknown-theme error with a suggestion when a close spelling exists.
pub fn resolve_name(name: &str) -> Result<&'static str, TuiError> {
    if let Some(shipped) = SHIPPED_THEMES
        .iter()
        .copied()
        .find(|shipped| *shipped == name)
    {
        return Ok(shipped);
    }
    let suggestion = suggestion(name);
    Err(crate::term::te_theme(name, suggestion))
}

/// Returns the resolved RGB value for one built-in semantic role.
#[must_use]
pub fn token_color(theme: &OpalineTheme, role: Role) -> OpalineColor {
    theme.color(token_name(role))
}

/// Calculates the WCAG relative luminance for an RGB color.
#[must_use]
pub fn luminance(color: OpalineColor) -> f64 {
    let (red, green, blue) = color.to_rgb_tuple();
    0.2126 * linear(f64::from(red) / 255.0)
        + 0.7152 * linear(f64::from(green) / 255.0)
        + 0.0722 * linear(f64::from(blue) / 255.0)
}

/// Calculates the WCAG contrast ratio for two RGB colors.
#[must_use]
pub fn contrast_ratio(left: OpalineColor, right: OpalineColor) -> f64 {
    let left = luminance(left);
    let right = luminance(right);
    (left.max(right) + 0.05) / (left.min(right) + 0.05)
}

/// Returns the color for a semantic role on one of the opaline token names.
#[must_use]
pub fn theme_color(theme: &OpalineTheme, role: Role) -> Color {
    theme.color(token_name(role)).into()
}

/// Returns the opaline token name for a role.
#[must_use]
pub const fn token_name(role: Role) -> &'static str {
    match role {
        Role::Text => tokens::TEXT_PRIMARY,
        Role::Dim => tokens::TEXT_SECONDARY,
        Role::Faint => tokens::TEXT_DIM,
        Role::Accent => tokens::ACCENT_PRIMARY,
        Role::Success => tokens::SUCCESS,
        Role::Warning => tokens::WARNING,
        Role::Error => tokens::ERROR,
    }
}

/// Returns the opaline token name for a fill.
#[must_use]
pub const fn fill_name(fill: Fill) -> &'static str {
    match fill {
        Fill::Canvas => tokens::BG_BASE,
        Fill::Surface => tokens::BG_PANEL,
        Fill::Selected => tokens::BG_SELECTION,
    }
}

fn suggestion(value: &str) -> Option<&'static str> {
    SHIPPED_THEMES.iter().copied().find(|candidate| {
        edit_distance(value, candidate) <= 2
            || candidate
                .strip_prefix(value)
                .is_some_and(|suffix| suffix.starts_with('-'))
    })
}

fn edit_distance(left: &str, right: &str) -> usize {
    let mut previous: Vec<usize> = (0..=right.chars().count()).collect();
    for (left_index, left_char) in left.chars().enumerate() {
        let mut current = Vec::with_capacity(previous.len());
        current.push(left_index + 1);
        for (right_index, right_char) in right.chars().enumerate() {
            let insert = current[right_index] + 1;
            let delete = previous[right_index + 1] + 1;
            let substitute = previous[right_index] + usize::from(left_char != right_char);
            current.push(insert.min(delete).min(substitute));
        }
        previous = current;
    }
    previous.last().copied().unwrap_or_default()
}

fn colorfgbg_theme(value: &str) -> Option<&'static str> {
    match value.rsplit(';').next()?.parse::<u8>().ok()? {
        0..=6 | 8 => Some("flexoki-dark"),
        7 | 15 => Some("flexoki-light"),
        _ => None,
    }
}

fn palette_color(role: Role) -> Color {
    match role {
        Role::Text => Color::Reset,
        Role::Dim | Role::Faint => Color::DarkGray,
        Role::Accent => Color::Cyan,
        Role::Success => Color::Green,
        Role::Warning => Color::Yellow,
        Role::Error => Color::Red,
    }
}

fn nearest_256(color: Color) -> Color {
    let Color::Rgb(r, g, b) = color else {
        return color;
    };
    let target = [i32::from(r), i32::from(g), i32::from(b)];
    let mut best_index = 0_usize;
    let mut best_distance = i64::MAX;
    for index in 0..256_usize {
        let candidate = palette_rgb(index);
        let distance = target
            .into_iter()
            .zip(candidate)
            .map(|(left, right)| i64::from(left - right).pow(2))
            .sum::<i64>();
        if distance < best_distance {
            best_distance = distance;
            best_index = index;
        }
    }
    u8::try_from(best_index).map_or(Color::Reset, Color::Indexed)
}

fn palette_rgb(index: usize) -> [i32; 3] {
    const BASIC: [[i32; 3]; 16] = [
        [0, 0, 0],
        [205, 0, 0],
        [0, 205, 0],
        [205, 205, 0],
        [0, 0, 238],
        [205, 0, 205],
        [0, 205, 205],
        [229, 229, 229],
        [127, 127, 127],
        [255, 0, 0],
        [0, 255, 0],
        [255, 255, 0],
        [92, 92, 255],
        [255, 0, 255],
        [0, 255, 255],
        [255, 255, 255],
    ];
    if index < 16 {
        return BASIC[index];
    }
    if index < 232 {
        const LEVELS: [i32; 6] = [0, 95, 135, 175, 215, 255];
        let value = index - 16;
        return [
            LEVELS[value / 36],
            LEVELS[(value / 6) % 6],
            LEVELS[value % 6],
        ];
    }
    let grey = 8 + i32::try_from(index - 232).unwrap_or_default() * 10;
    [grey, grey, grey]
}

fn linear(value: f64) -> f64 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Fill, Role, SHIPPED_THEMES, contrast_ratio, fill_name, load, resolve_name, token_color,
        verify_contrast_gate,
    };
    use crate::{ColorMode, ThemeRequest};

    #[test]
    fn exactly_nine_shipped_names_load_the_same_builtins() {
        let loaded: Vec<_> = SHIPPED_THEMES
            .iter()
            .map(|name| {
                load(
                    &ThemeRequest::Named((*name).into()),
                    ColorMode::Truecolor,
                    None,
                    None,
                )
                .ok()
                .and_then(|theme| theme.name())
            })
            .collect();
        assert_eq!(loaded, SHIPPED_THEMES.map(Some));
    }

    #[test]
    fn incomplete_theme_name_gets_the_shipped_suggestion() {
        let error = resolve_name("kanagawa")
            .err()
            .map(|error| error.to_string());
        assert!(error.is_some_and(|text| text.contains("Did you mean kanagawa-wave?")));
    }

    #[test]
    fn every_shipped_core_pair_meets_wcag_contrast_floors() {
        let result = verify_contrast_gate();
        let detail = result.as_ref().err().map(ToString::to_string);
        assert_eq!(result.ok(), Some(171), "{detail:?}");
    }

    #[test]
    fn contrast_formula_matches_independent_wcag_values() {
        let black = opaline::OpalineColor::new(0, 0, 0);
        let white = opaline::OpalineColor::new(255, 255, 255);
        let gray = opaline::OpalineColor::new(119, 119, 119);
        assert!((contrast_ratio(black, white) - 21.0).abs() < 0.0001);
        assert!((contrast_ratio(gray, white) - 4.48).abs() < 0.02);
        assert!((contrast_ratio(white, black) - 21.0).abs() < 0.0001);
    }

    #[test]
    fn roles_and_fills_resolve_through_opaline_tokens() {
        let theme = opaline::builtins::load_by_name("flexoki-dark");
        assert!(
            theme
                .as_ref()
                .is_some_and(|theme| theme.has_token(super::token_name(Role::Text)))
        );
        assert!(
            theme
                .as_ref()
                .is_some_and(|theme| theme.has_token(fill_name(Fill::Canvas)))
        );
        assert!(theme.as_ref().is_some_and(
            |theme| token_color(theme, Role::Text) != token_color(theme, Role::Accent)
        ));
    }
}
