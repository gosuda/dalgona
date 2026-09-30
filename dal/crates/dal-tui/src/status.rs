//! Fixed-width status slots whose meaning remains textual without color.

use crate::width::{WidthMode, take_cells, width};

/// Values displayed by one status row.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatusData<'a> {
    /// Current state word; idle leaves this empty.
    pub state: Option<&'a str>,
    /// Braille spinner cell, or `*` under reduced motion.
    pub spinner: Option<&'a str>,
    /// Model identifier.
    pub model: Option<&'a str>,
    /// Workspace path and branch.
    pub path: Option<&'a str>,
    /// Input and output token labels.
    pub tokens: Option<&'a str>,
    /// Context usage percentage.
    pub context: Option<&'a str>,
    /// Agent or job count.
    pub agents: Option<&'a str>,
    /// Session cost.
    pub cost: Option<&'a str>,
}

/// Renders status segments for a terminal width.
///
/// Numeric fields retain fixed slot widths and right alignment. Segments drop
/// from the right according to the width bands in the TUI design contract.
#[must_use]
pub fn render(data: StatusData<'_>, columns: usize, mode: WidthMode) -> String {
    let state = state_slot(data.spinner, data.state);
    let mut segments = Vec::new();
    if columns >= 120 {
        push(&mut segments, state, 12, false, columns, mode);
        push_opt(&mut segments, data.model, 24, false, columns, mode);
        push_opt(&mut segments, data.path, 24, false, columns, mode);
        push_opt(&mut segments, data.tokens, 13, true, columns, mode);
        push_opt(&mut segments, data.context, 8, true, columns, mode);
        push_opt(&mut segments, data.agents, 10, true, columns, mode);
        push_opt(&mut segments, data.cost, 7, true, columns, mode);
    } else if columns >= 80 {
        push(&mut segments, state, 12, false, columns, mode);
        push_opt(&mut segments, data.model, 24, false, columns, mode);
        push_opt(&mut segments, data.path, 24, false, columns, mode);
        push_opt(&mut segments, data.tokens, 13, true, columns, mode);
        push_opt(&mut segments, data.context, 8, true, columns, mode);
        push_opt(&mut segments, data.agents, 10, true, columns, mode);
    } else if columns >= 60 {
        push(&mut segments, state, 12, false, columns, mode);
        push_opt(&mut segments, data.model, 24, false, columns, mode);
        push_opt(&mut segments, data.path, 24, false, columns, mode);
        let compact_tokens = data.tokens.map(compact_token_pair);
        push_opt(
            &mut segments,
            compact_tokens.as_deref(),
            13,
            true,
            columns,
            mode,
        );
        push_opt(&mut segments, data.context, 8, true, columns, mode);
        push_opt(&mut segments, data.agents, 10, true, columns, mode);
    } else if columns >= 40 {
        push(&mut segments, state, 12, false, columns, mode);
        push_opt(&mut segments, data.model, 24, false, columns, mode);
        let basename = data.path.map(path_basename);
        push_opt(&mut segments, basename.as_deref(), 24, false, columns, mode);
        push_opt(&mut segments, data.context, 8, true, columns, mode);
    } else if columns >= 20 {
        push(&mut segments, state, 12, false, columns, mode);
        push_opt(&mut segments, data.context, 8, true, columns, mode);
    } else {
        let state_width = width(state.as_deref().unwrap_or(""), mode);
        push(&mut segments, state, 12, false, columns, mode);
        if segments.is_empty()
            && let Some(model) = data.model
        {
            let available = columns.saturating_sub(state_width + 1);
            if available > 0 {
                segments.push(format!(
                    "...{}",
                    take_cells(model, available.saturating_sub(3), mode)
                ));
            }
        }
    }

    let line = segments.join(" · ");
    take_cells(&line, columns, mode)
}

fn state_slot(spinner: Option<&str>, state: Option<&str>) -> Option<String> {
    match (spinner, state) {
        (Some(spinner), Some(state)) => Some(format!("{spinner} {state}")),
        (None, Some(state)) => Some(state.to_owned()),
        (Some(spinner), None) => Some(spinner.to_owned()),
        (None, None) => None,
    }
}

fn push_opt(
    segments: &mut Vec<String>,
    value: Option<&str>,
    slot_width: usize,
    right_aligned: bool,
    columns: usize,
    mode: WidthMode,
) {
    if let Some(value) = value {
        push(
            segments,
            Some(value.to_owned()),
            slot_width,
            right_aligned,
            columns,
            mode,
        );
    }
}

fn push(
    segments: &mut Vec<String>,
    value: Option<String>,
    slot_width: usize,
    right_aligned: bool,
    columns: usize,
    mode: WidthMode,
) {
    let Some(value) = value else {
        return;
    };
    let separators = segments.len().saturating_mul(3);
    let remaining =
        columns.saturating_sub(separators + segments.iter().map(|s| width(s, mode)).sum::<usize>());
    if remaining == 0 {
        return;
    }
    let allotted = slot_width.min(remaining);
    let text = take_cells(&value, allotted, mode);
    let padding = allotted.saturating_sub(width(&text, mode));
    if right_aligned {
        segments.push(format!("{}{text}", " ".repeat(padding)));
    } else {
        segments.push(format!("{text}{}", " ".repeat(padding)));
    }
}

fn compact_token_pair(value: &str) -> String {
    let Some((input, output)) = value.split_once(' ') else {
        return value.to_owned();
    };
    let input = input.strip_prefix("in ").unwrap_or(input);
    let output = output.strip_prefix("out ").unwrap_or(output);
    format!("{input}/{output}")
}

/// Builds the SGR prefix for one theme role: the role color at the resolved
/// depth plus its modifier, so meaning survives `NO_COLOR`.
#[must_use]
pub fn role_sgr(theme: &crate::theme::ResolvedTheme, role: crate::theme::Role) -> String {
    use ratatui::style::{Color, Modifier};
    let mut params: Vec<String> = Vec::new();
    if crate::theme::ResolvedTheme::modifiers(role).contains(Modifier::DIM) {
        params.push("2".to_owned());
    }
    let named = |code: u8| Some(code.to_string());
    let foreground = match theme.color(role) {
        Color::Black => named(30),
        Color::Red => named(31),
        Color::Green => named(32),
        Color::Yellow => named(33),
        Color::Blue => named(34),
        Color::Magenta => named(35),
        Color::Cyan => named(36),
        Color::Gray => named(37),
        Color::DarkGray => named(90),
        Color::LightRed => named(91),
        Color::LightGreen => named(92),
        Color::LightYellow => named(93),
        Color::LightBlue => named(94),
        Color::LightMagenta => named(95),
        Color::LightCyan => named(96),
        Color::White => named(97),
        Color::Rgb(red, green, blue) => Some(format!("38;2;{red};{green};{blue}")),
        Color::Indexed(index) => Some(format!("38;5;{index}")),
        Color::Reset => None,
    };
    params.extend(foreground);
    if params.is_empty() {
        String::new()
    } else {
        format!("\x1b[{}m", params.join(";"))
    }
}

fn path_basename(path: &str) -> String {
    let (path, branch) = path
        .split_once(" ( ")
        .map_or((path, None), |(path, rest)| (path, Some(rest)));
    let basename = path.rsplit(['/', '\\']).next().unwrap_or(path);
    branch.map_or_else(
        || basename.to_owned(),
        |branch| format!("{basename} ({branch}"),
    )
}

#[cfg(test)]
mod tests {
    use super::{StatusData, render, role_sgr};
    use crate::theme::Role;
    use crate::width::WidthMode;
    use crate::{ColorMode, ThemeRequest};

    #[test]
    fn role_sgr_follows_color_depth_and_no_color() {
        let truecolor = crate::theme::load(
            &ThemeRequest::Named("flexoki-dark".into()),
            ColorMode::Truecolor,
            None,
            None,
        )
        .expect("shipped theme loads");
        let dim = role_sgr(&truecolor, Role::Dim);
        assert!(dim.starts_with("\x1b[2;38;2;"), "{dim:?}");
        let never = crate::theme::load(&ThemeRequest::Palette, ColorMode::Never, None, None)
            .expect("palette loads");
        assert_eq!(role_sgr(&never, Role::Dim), "\x1b[2m");
        assert_eq!(role_sgr(&never, Role::Text), "");
    }

    #[test]
    fn bands_keep_status_state_and_drop_whole_right_slots() {
        let data = StatusData {
            state: Some("working"),
            spinner: Some("⠋"),
            model: Some("provider/model"),
            path: Some("~/work/project (main)"),
            tokens: Some("in 14k out 4k"),
            context: Some("ctx 47%"),
            agents: Some("3 agents"),
            cost: Some("$0.42"),
        };
        let full = render(data, 120, WidthMode::Narrow);
        assert!(full.contains("$0.42"));
        let medium = render(data, 30, WidthMode::Narrow);
        assert!(medium.contains("working"));
        let narrow = render(data, 15, WidthMode::Narrow);
        assert!(narrow.contains("working"));
        assert!(!narrow.contains("provider/model"));
    }

    #[test]
    fn context_slot_is_kept_in_the_twenty_column_band() {
        let rendered = render(
            StatusData {
                state: Some("working"),
                spinner: Some("*"),
                context: Some("ctx 47%"),
                ..StatusData::default()
            },
            30,
            WidthMode::Narrow,
        );
        assert!(rendered.contains("ctx 47%"));
    }
}
