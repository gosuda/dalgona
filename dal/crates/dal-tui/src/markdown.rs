//! Assistant prose rendering: headings and `**strong**` spans bold, inline code in the
//! accent role, dashed lists with hanging indents, and fenced code at full width behind
//! a faint ASCII rule. Nothing renders in italics.

use crate::render::{
    RenderLink, RenderRow, RenderSpan, RichLine, indent_row, linked_text, wrap_rich_line,
};
use crate::theme::Role;
use crate::width::{WidthMode, escape, wrap};

/// Renders markdown `text`: prose wraps at `cap` cells, code and tables at `full`.
pub(crate) fn markdown_rows(
    text: &str,
    cap: usize,
    full: usize,
    mode: WidthMode,
) -> Vec<RenderRow> {
    let mut rows = Vec::new();
    let mut state = LineState::default();
    for logical in text.split('\n') {
        state.push(logical, cap, full, mode, &mut rows);
    }
    if rows.is_empty() {
        rows.push(RenderRow::plain(String::new(), Role::Text));
    }
    rows
}
/// What the lines read so far leave open for the next one: a fenced block.
#[derive(Debug, Clone, Copy, Default)]
struct LineState {
    open: Option<Fence>,
}

impl LineState {
    /// Renders one source line into `rows` and advances the fence state.
    fn push(
        &mut self,
        logical: &str,
        cap: usize,
        full: usize,
        mode: WidthMode,
        rows: &mut Vec<RenderRow>,
    ) {
        if let Some(fence) = &self.open {
            if fence.closes(logical) {
                self.open = None;
            } else {
                rows.extend(code_rows(logical, full, mode));
            }
        } else if let Some((fence, info)) = Fence::opens(logical) {
            self.open = Some(fence);
            if !info.is_empty() {
                rows.push(RenderRow::plain(escape(info), Role::Dim));
            }
        } else {
            rows.extend(block_rows(logical, cap, full, mode));
        }
    }
}

/// Rows of a markdown text that only grows, rendered once per completed line.
///
/// Rendering is per line with a carried fence state, so the rows of a completed
/// line never change; only the trailing unfinished line is rendered again on
/// each call. A change of layout starts over.
#[derive(Debug, Default)]
pub(crate) struct MarkdownStream {
    layout: Option<(usize, usize, WidthMode)>,
    consumed: usize,
    state: LineState,
    rows: Vec<RenderRow>,
}

impl MarkdownStream {
    /// Forgets every rendered line; the next text starts a new stream.
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    /// The last `limit` rows [`markdown_rows`] gives for `text`, which must extend the
    /// text of the previous call.
    pub(crate) fn rows(
        &mut self,
        text: &str,
        cap: usize,
        full: usize,
        mode: WidthMode,
        limit: usize,
    ) -> Vec<RenderRow> {
        if self.layout != Some((cap, full, mode)) {
            self.reset();
            self.layout = Some((cap, full, mode));
        }
        while let Some(end) = text[self.consumed..].find('\n') {
            let line = &text[self.consumed..self.consumed + end];
            self.state.push(line, cap, full, mode, &mut self.rows);
            self.consumed += end + 1;
        }
        let mut tail = Vec::new();
        let mut state = self.state;
        state.push(&text[self.consumed..], cap, full, mode, &mut tail);
        if self.rows.is_empty() && tail.is_empty() {
            tail.push(RenderRow::plain(String::new(), Role::Text));
        }
        let kept = limit.saturating_sub(tail.len()).min(self.rows.len());
        let mut out = Vec::with_capacity(kept + tail.len());
        out.extend_from_slice(&self.rows[self.rows.len() - kept..]);
        out.extend(tail);
        out.drain(..out.len().saturating_sub(limit));
        out
    }
}

#[derive(Debug, Clone, Copy)]
struct Fence {
    marker: char,
    length: usize,
}

impl Fence {
    fn opens(line: &str) -> Option<(Self, &str)> {
        let trimmed = indent_trimmed(line)?;
        let marker = trimmed.chars().next().filter(|c| matches!(c, '`' | '~'))?;
        let length = trimmed.chars().take_while(|c| *c == marker).count();
        if length < 3 {
            return None;
        }
        let info = trimmed[length..].trim();
        if marker == '`' && info.contains('`') {
            return None;
        }
        Some((Self { marker, length }, info))
    }

    fn closes(&self, line: &str) -> bool {
        let Some(trimmed) = indent_trimmed(line) else {
            return false;
        };
        let run = trimmed.chars().take_while(|c| *c == self.marker).count();
        run >= self.length && trimmed[run..].trim().is_empty()
    }
}

/// Strips up to three leading spaces; four or more make an indented code line.
fn indent_trimmed(line: &str) -> Option<&str> {
    let trimmed = line.trim_start_matches(' ');
    (line.len() - trimmed.len() <= 3).then_some(trimmed)
}

/// One source line of a fenced block: a faint `|` rule, then the line wrapped at `full`.
fn code_rows(line: &str, full: usize, mode: WidthMode) -> Vec<RenderRow> {
    let content = full.saturating_sub(2).max(1);
    let escaped = escape(line);
    wrap(&escaped, content, mode)
        .into_iter()
        .map(|piece| {
            let mut row = RenderRow::plain(
                if piece.is_empty() {
                    "|".to_owned()
                } else {
                    format!("| {piece}")
                },
                Role::Text,
            );
            row.spans.push(RenderSpan {
                range: 0..1,
                role: Role::Faint,
                bold: false,
            });
            row
        })
        .collect()
}

/// One prose line: block prefix, inline spans, wrap, and hanging indent.
fn block_rows(line: &str, cap: usize, full: usize, mode: WidthMode) -> Vec<RenderRow> {
    let (visible, links) = linked_text(line);
    if visible.is_empty() {
        return vec![RenderRow::plain(String::new(), Role::Text)];
    }
    let block = Block::of(&visible);
    let width = if block.table { full } else { cap };
    let indent = crate::width::width(&block.marker, mode);
    let body = &visible[block.consumed..];
    let links = links
        .into_iter()
        .filter(|link| link.range.end > block.consumed)
        .map(|link| RenderLink {
            range: link.range.start.saturating_sub(block.consumed)..link.range.end - block.consumed,
            url: link.url,
        })
        .collect();
    let base = Style {
        role: Role::Text,
        bold: block.bold,
    };
    let line = inline(body, links, base);
    let continuation = " ".repeat(indent);
    wrap_rich_line(&line, width.saturating_sub(indent), mode)
        .into_iter()
        .enumerate()
        .map(|(index, row)| {
            let prefix = if index == 0 {
                block.marker.as_str()
            } else {
                continuation.as_str()
            };
            indent_row(row, prefix)
        })
        .collect()
}

/// The block-level reading of one line: what to strip and what to show instead.
struct Block {
    consumed: usize,
    marker: String,
    bold: bool,
    table: bool,
}

impl Block {
    fn of(line: &str) -> Self {
        let plain = |consumed| Self {
            consumed,
            marker: String::new(),
            bold: false,
            table: line.trim_start().starts_with('|'),
        };
        let hashes = line.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && line[hashes..].starts_with(' ') {
            return Self {
                consumed: hashes + 1,
                marker: String::new(),
                bold: true,
                table: false,
            };
        }
        let spaces = line.len() - line.trim_start_matches(' ').len();
        let rest = &line[spaces..];
        let indent = " ".repeat(spaces);
        let mut chars = rest.chars();
        if let (Some('-' | '*' | '+'), Some(' ')) = (chars.next(), chars.next()) {
            let after = &rest[2..];
            let (mark, extra) = match after.get(..4) {
                Some("[ ] ") => ("[ ] ", 4),
                Some("[x] " | "[X] ") => ("[x] ", 4),
                _ => ("- ", 0),
            };
            return Self {
                consumed: spaces + 2 + extra,
                marker: format!("{indent}{mark}"),
                bold: false,
                table: false,
            };
        }
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if (1..=9).contains(&digits) {
            let tail = &rest[digits..];
            if tail.starts_with(". ") || tail.starts_with(") ") {
                return Self {
                    consumed: spaces + digits + 2,
                    marker: format!("{indent}{}", &rest[..digits + 2]),
                    bold: false,
                    table: false,
                };
            }
        }
        plain(0)
    }
}

/// How one run of text is drawn.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Style {
    role: Role,
    bold: bool,
}

/// Resolves `**strong**` and `` `code` `` in `visible`, removing the markers and
/// moving link ranges onto the shortened text. Every run starts from `base`.
fn inline(visible: &str, links: Vec<RenderLink>, base: Style) -> RichLine {
    let mut line = RichLine {
        text: String::with_capacity(visible.len()),
        links: Vec::new(),
        spans: Vec::new(),
    };
    // `moved[i]` is where byte `i` of `visible` lands in `line.text`.
    let mut moved = vec![0; visible.len() + 1];
    let runs = tick_runs(visible);
    let mut next_run = 0;
    let mut strong = false;
    let mut index = 0;
    while index < visible.len() {
        let rest = &visible[index..];
        let (consumed, lead, content, style) = if rest.starts_with('`') {
            while runs[next_run].start < index {
                next_run += 1;
            }
            tick_step(
                visible,
                &runs,
                next_run,
                Style {
                    bold: base.bold || strong,
                    ..base
                },
            )
        } else if strong_marker(visible, index, strong) {
            strong = !strong;
            (2, 0, "", base)
        } else {
            let len = rest.chars().next().map_or(1, char::len_utf8);
            let style = Style {
                role: base.role,
                bold: base.bold || strong,
            };
            (len, 0, &rest[..len], style)
        };
        moved[index..index + lead].fill(line.text.len());
        for offset in 0..content.len() {
            moved[index + lead + offset] = line.text.len() + offset;
        }
        push(&mut line, content, style);
        moved[index + lead + content.len()..index + consumed].fill(line.text.len());
        index += consumed;
    }
    moved[visible.len()] = line.text.len();
    line.links = links
        .into_iter()
        .filter_map(|link| {
            let range = moved[link.range.start]..moved[link.range.end];
            (range.start < range.end).then_some(RenderLink {
                range,
                url: link.url,
            })
        })
        .collect();
    line
}

/// Reads the backtick run `runs[at]`: a code span through its closing run, or the bare run.
/// Returns bytes consumed, the opening bytes to drop, the text shown, and its style.
fn tick_step<'a>(
    visible: &'a str,
    runs: &[TickRun],
    at: usize,
    base: Style,
) -> (usize, usize, &'a str, Style) {
    let open = &runs[at];
    let Some(closer) = open.closer.map(|closer| &runs[closer]) else {
        return (
            open.len,
            0,
            &visible[open.start..open.start + open.len],
            base,
        );
    };
    let style = Style {
        role: Role::Accent,
        ..base
    };
    let content = &visible[open.start + open.len..closer.start];
    (
        closer.start + closer.len - open.start,
        open.len,
        content,
        style,
    )
}

/// A maximal run of backticks in a line, and the next run of the same length.
struct TickRun {
    start: usize,
    len: usize,
    closer: Option<usize>,
}

/// Every backtick run in `text`, each linked to the next run of equal length.
///
/// A run with no later equal-length run is not an opener: it stays literal as a whole.
fn tick_runs(text: &str) -> Vec<TickRun> {
    let bytes = text.as_bytes();
    let mut runs = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'`' {
            index += 1;
            continue;
        }
        let start = index;
        while bytes.get(index) == Some(&b'`') {
            index += 1;
        }
        runs.push(TickRun {
            start,
            len: index - start,
            closer: None,
        });
    }
    let mut later: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    for at in (0..runs.len()).rev() {
        runs[at].closer = later.insert(runs[at].len, at);
    }
    runs
}

/// Whether the `**` at `index` closes strong text (`strong`) or opens a span that closes later.
fn strong_marker(whole: &str, index: usize, strong: bool) -> bool {
    let rest = &whole[index..];
    if !rest.starts_with("**") {
        return false;
    }
    if strong {
        return whole[..index]
            .chars()
            .next_back()
            .is_some_and(|c| !c.is_whitespace());
    }
    rest[2..].chars().next().is_some_and(|c| !c.is_whitespace()) && has_closer(&rest[2..])
}

/// Whether `tail` holds a `**` that can close strong text.
fn has_closer(tail: &str) -> bool {
    tail.match_indices("**").any(|(at, _)| {
        tail[..at]
            .chars()
            .next_back()
            .is_some_and(|c| !c.is_whitespace())
    })
}

/// Appends `piece` in `style`, merging with the previous span when it continues it.
fn push(line: &mut RichLine, piece: &str, style: Style) {
    if piece.is_empty() {
        return;
    }
    let start = line.text.len();
    line.text.push_str(piece);
    if style.role == Role::Text && !style.bold {
        return;
    }
    if let Some(last) = line.spans.last_mut()
        && last.range.end == start
        && last.role == style.role
        && last.bold == style.bold
    {
        last.range.end = line.text.len();
        return;
    }
    line.spans.push(RenderSpan {
        range: start..line.text.len(),
        role: style.role,
        bold: style.bold,
    });
}

#[cfg(test)]
mod tests {
    use super::{MarkdownStream, markdown_rows};
    use crate::render::RenderRow;
    use crate::theme::Role;
    use crate::width::WidthMode;

    fn rows(text: &str, cap: usize) -> Vec<RenderRow> {
        markdown_rows(text, cap, 40, WidthMode::Narrow)
    }

    fn texts(text: &str, cap: usize) -> Vec<String> {
        rows(text, cap).into_iter().map(|row| row.text).collect()
    }

    fn styled(row: &RenderRow) -> Vec<(&str, Role, bool)> {
        row.spans
            .iter()
            .map(|span| (&row.text[span.range.clone()], span.role, span.bold))
            .collect()
    }

    #[test]
    fn strong_and_inline_code_lose_their_markers_and_keep_their_style() {
        let rows = rows("Use **bold** and `code` here", 40);
        assert_eq!(rows[0].text, "Use bold and code here");
        assert_eq!(
            styled(&rows[0]),
            [("bold", Role::Text, true), ("code", Role::Accent, false)]
        );
    }

    #[test]
    fn headings_are_bold_and_bullets_normalize_to_dashes() {
        let rows = rows(
            "## Plan\n* one\n+ two\n- [x] done\n- [ ] todo\n3. third",
            40,
        );
        let text: Vec<&str> = rows.iter().map(|row| row.text.as_str()).collect();
        assert_eq!(
            text,
            ["Plan", "- one", "- two", "[x] done", "[ ] todo", "3. third"]
        );
        assert_eq!(styled(&rows[0]), [("Plan", Role::Text, true)]);
    }

    #[test]
    fn unmatched_markers_stay_literal() {
        assert_eq!(
            texts("2 ** 3 and `open and **x", 40),
            ["2 ** 3 and `open and **x"]
        );
    }

    #[test]
    fn list_continuations_hang_under_their_text() {
        assert_eq!(
            texts("- aaaa bbbb cccc", 8),
            ["- aaaa b", "  bbb cc", "  cc"]
        );
    }

    #[test]
    fn fenced_code_shows_info_then_ruled_lines_and_ignores_markers() {
        let rows = rows("before\n```rust\nlet **x** = `y`;\n\n```\nafter", 40);
        let text: Vec<&str> = rows.iter().map(|row| row.text.as_str()).collect();
        assert_eq!(text, ["before", "rust", "| let **x** = `y`;", "|", "after"]);
        assert_eq!(rows[1].role, Role::Dim);
        assert_eq!(styled(&rows[2]), [("|", Role::Faint, false)]);
    }

    #[test]
    fn an_open_fence_keeps_streaming_text_as_code() {
        assert_eq!(texts("```\nlet x", 40), ["| let x"]);
    }

    #[test]
    fn code_wraps_at_full_width_not_the_prose_cap() {
        let long = "x".repeat(30);
        let rows = markdown_rows(&format!("```\n{long}\n```"), 10, 40, WidthMode::Narrow);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, format!("| {long}"));
    }

    #[test]
    fn links_survive_marker_removal() {
        let rows = rows("**see** [the docs](file:///tmp/docs) now", 40);
        assert_eq!(rows[0].text, "see the docs now");
        assert_eq!(rows[0].links.len(), 1);
        assert_eq!(&rows[0].text[rows[0].links[0].range.clone()], "the docs");
    }

    #[test]
    fn strong_spans_split_across_wrapped_rows() {
        let rows = rows("**alpha beta gamma**", 11);
        assert_eq!(
            rows.iter().map(|row| row.text.as_str()).collect::<Vec<_>>(),
            ["alpha beta ", "gamma"]
        );
        assert!(
            rows.iter()
                .all(|row| row.spans.iter().all(|span| span.bold))
        );
    }

    #[test]
    fn blank_lines_keep_their_row() {
        assert_eq!(texts("one\n\ntwo", 40), ["one", "", "two"]);
    }

    fn first(text: &str) -> RenderRow {
        markdown_rows(text, 4096, 4096, WidthMode::Narrow).remove(0)
    }

    #[test]
    fn an_unmatched_run_stays_literal_as_a_whole() {
        let row = first("``` a `` b");
        assert_eq!(row.text, "``` a `` b");
        assert_eq!(row.spans.len(), 0);
    }

    #[test]
    fn a_run_closes_only_on_an_equal_run_and_keeps_inner_runs() {
        let row = first("x ``a ` b`` y ` z");
        assert_eq!(row.text, "x a ` b y ` z");
        assert_eq!(row.spans.len(), 1);
        assert_eq!(row.spans[0].role, Role::Accent);
        assert_eq!(&row.text[row.spans[0].range.clone()], "a ` b");
    }

    #[test]
    fn a_long_unmatched_run_renders_in_linear_time() {
        let row = first(&format!("x{}tail", "`".repeat(200_000)));
        assert_eq!(row.text, format!("x{}", "`".repeat(4095)));
        assert_eq!(row.spans.len(), 0);
    }

    const STREAMED: &str = "# Title\nsome **bold** `code` text that wraps around the cap\n```rust\nlet x = 1;\n\n```\n- item one that also wraps\n| a | b |\n~~~\nopen fence\n";

    #[test]
    fn a_stream_matches_a_full_render_at_every_prefix() {
        for (cap, full) in [(10, 20), (40, 40)] {
            let mut stream = MarkdownStream::default();
            let ends = STREAMED
                .char_indices()
                .map(|(index, _)| index)
                .chain([STREAMED.len()]);
            for end in ends {
                let prefix = &STREAMED[..end];
                let whole = markdown_rows(prefix, cap, full, WidthMode::Narrow);
                let all = stream.rows(prefix, cap, full, WidthMode::Narrow, usize::MAX);
                assert_eq!(all, whole, "prefix {prefix:?}");
                let tail = stream.rows(prefix, cap, full, WidthMode::Narrow, 3);
                assert_eq!(
                    tail,
                    whole[whole.len().saturating_sub(3)..],
                    "tail {prefix:?}"
                );
            }
        }
    }

    #[test]
    fn a_stream_renders_each_completed_line_once() {
        let mut stream = MarkdownStream::default();
        let text = "alpha beta gamma\ndelta epsilon\nzeta";
        stream.rows(text, 8, 8, WidthMode::Narrow, usize::MAX);
        assert_eq!(stream.consumed, "alpha beta gamma\ndelta epsilon\n".len());
        let completed = stream.rows.len();
        stream.rows(text, 8, 8, WidthMode::Narrow, usize::MAX);
        assert_eq!(stream.rows.len(), completed);
        stream.rows(
            "alpha beta gamma\ndelta epsilon\nzeta eta\n",
            8,
            8,
            WidthMode::Narrow,
            1,
        );
        assert!(stream.rows.len() > completed);
    }

    #[test]
    fn a_stream_starts_over_when_the_layout_changes() {
        let mut stream = MarkdownStream::default();
        stream.rows(STREAMED, 10, 20, WidthMode::Narrow, usize::MAX);
        let narrow = stream.rows(STREAMED, 40, 40, WidthMode::Narrow, usize::MAX);
        assert_eq!(narrow, markdown_rows(STREAMED, 40, 40, WidthMode::Narrow));
    }
}
