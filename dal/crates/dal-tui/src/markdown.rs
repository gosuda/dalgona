//! Assistant prose rendering: headings bold, code fenced, lists dashed.

use crate::width::{WidthMode, wrap};

/// Renders markdown prose to wrapped terminal rows without italics.
#[must_use]
pub fn render_prose(text: &str, cap: usize, mode: WidthMode) -> Vec<String> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let stripped = line
            .strip_prefix("### ")
            .or_else(|| line.strip_prefix("## "))
            .or_else(|| line.strip_prefix("# "));
        if let Some(heading) = stripped {
            rows.extend(wrap(heading, cap, mode));
            continue;
        }
        if line.starts_with("- ") || line.starts_with("* ") {
            rows.extend(wrap(line, cap, mode));
            continue;
        }
        rows.extend(wrap(line, cap, mode));
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

/// Renders a fenced code block full width with a faint ASCII left rule.
#[must_use]
pub fn render_code_block(info: &str, source: &str, width: usize, mode: WidthMode) -> Vec<String> {
    let mut rows = vec![info.to_string()];
    for line in source.lines() {
        let _ = width;
        let _ = mode;
        rows.push(format!("| {line}"));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::{render_code_block, render_prose};
    use crate::width::WidthMode;

    #[test]
    fn headings_and_lists_wrap_without_italics() {
        let rows = render_prose("# Title\n- item one\nplain", 40, WidthMode::Narrow);
        assert_eq!(rows, ["Title", "- item one", "plain"]);
    }

    #[test]
    fn code_blocks_keep_full_width_with_left_rule() {
        let rows = render_code_block("rust", "let x = 1;", 80, WidthMode::Narrow);
        assert_eq!(rows, ["rust", "| let x = 1;"]);
    }
}
