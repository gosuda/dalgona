//! Pure projection from a session view and live front-end state to terminal rows.

use std::ops::Range;
use std::sync::Arc;

use dal_core::{Block, EntryKind, EntryView, JournalPart, TurnState, View};
use unicode_segmentation::UnicodeSegmentation;

use crate::diagram::{ArtRole, DiagramSettings, RenderCache, WiredBlock, wire_block};
use crate::dialog::DialogUi;
use crate::frame::{RegionBudget, RegionRequest};
use crate::live::Live;
use crate::picker::PickerUi;
use crate::status::{self, StatusData};
use crate::theme::{ResolvedTheme, Role};
use crate::transcript::Transcript;
use crate::width::{WidthMode, escape, take_cells};
use crate::{Screen, TuiOptions};

/// State consumed by one frame projection; terminal I/O never enters this function.
#[derive(Clone, Copy)]
pub(crate) struct FrameInput<'a> {
    pub(crate) view: &'a View,
    pub(crate) screen: Screen,
    pub(crate) composer: &'a str,
    pub(crate) popup: &'a [String],
    pub(crate) live: &'a Live,
    pub(crate) dialog: &'a DialogUi,
    pub(crate) picker: Option<&'a PickerUi>,
    pub(crate) transcript: &'a Transcript,
    pub(crate) opts: &'a TuiOptions,
    pub(crate) theme: &'a ResolvedTheme,
    pub(crate) diagram_settings: DiagramSettings,
    pub(crate) diagram_cache: &'a RenderCache,
}

/// Raster image content retained for the terminal protocol renderer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PixelImage {
    pub(crate) bytes: Arc<[u8]>,
    pub(crate) digest: [u8; 32],
    pub(crate) columns: u16,
    pub(crate) rows: u16,
}

/// One measured row with its semantic role; terminal SGR is applied only after clipping.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RenderRow {
    pub(crate) text: String,
    pub(crate) role: Role,
    pub(crate) color: ratatui::style::Color,
    pub(crate) spans: Vec<RenderSpan>,
    pub(crate) links: Vec<RenderLink>,
    pub(crate) pending_diagram: bool,
    pub(crate) image: Option<PixelImage>,
    pub(crate) image_tail: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RenderSpan {
    pub(crate) range: Range<usize>,
    pub(crate) role: Role,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RenderLink {
    pub(crate) range: Range<usize>,
    pub(crate) url: String,
}

impl RenderRow {
    pub(crate) fn new(text: impl Into<String>, role: Role) -> Self {
        let (text, links) = linked_text(&text.into());
        Self {
            text,
            role,
            color: ratatui::style::Color::Reset,
            spans: Vec::new(),
            links,
            pending_diagram: false,
            image: None,
            image_tail: false,
        }
    }

    pub(crate) fn clipped(mut self, width: usize, mode: WidthMode) -> Self {
        self.text = take_cells(&self.text, width, mode);
        let end = self.text.len();
        self.spans.retain_mut(|span| {
            if span.range.start >= end {
                return false;
            }
            span.range.end = span.range.end.min(end);
            true
        });
        self.links.retain_mut(|link| {
            if link.range.start >= end {
                return false;
            }
            link.range.end = link.range.end.min(end);
            true
        });
        self
    }
}

/// Renders the bottom stack in inline mode or the whole screen in fullscreen mode.
pub(crate) fn frame_rows(input: FrameInput<'_>, width: u16, height: u16) -> Vec<RenderRow> {
    let w = usize::from(width);
    let h = usize::from(height);
    let mode = input.opts.env.width_mode;
    let model = input
        .view
        .settings
        .model
        .as_ref()
        .map(|model| escape(model.id()))
        .or_else(|| input.opts.default_model.as_deref().map(escape));
    let status = status_line(&input, model.as_deref(), w, mode);
    if h < 8 {
        return resolve_colors(
            vec![
                RenderRow::new(crate::copy::ids::NARROW_ROWS, Role::Warning).clipped(w, mode),
                RenderRow::new(status, Role::Dim).clipped(w, mode),
            ],
            input.theme,
        );
    }
    if w < 12 {
        return resolve_colors(
            vec![
                RenderRow::new(crate::copy::ids::NARROW_COLS, Role::Warning).clipped(w, mode),
                RenderRow::new(status, Role::Dim).clipped(w, mode),
            ],
            input.theme,
        );
    }

    let mut activity = activity_rows(&input, w, mode);
    let notices = input.live.notices();
    let picker_rows = input
        .picker
        .map_or(0, |picker| picker.visible_len().max(1) + 2);
    let overlay = if input.dialog.is_open() {
        Some(6)
    } else {
        input.picker.map(|_| picker_rows)
    };
    let budget = RegionBudget::allocate(
        width,
        height,
        RegionRequest {
            notices: notices.len(),
            activity: usize::MAX,
            composer: input.composer.split('\n').count(),
            hint: true,
            overlay,
        },
    );
    let popup_len = if overlay.is_some() {
        0
    } else {
        input.popup.len()
    };
    let occupied = activity
        .len()
        .saturating_add(popup_len)
        .min(budget.activity);
    let ext_allowance = budget.activity.saturating_sub(occupied).min(4);
    activity.splice(
        ..0,
        input
            .live
            .ext_lines(ext_allowance)
            .into_iter()
            .map(|row| RenderRow::new(row, Role::Dim)),
    );
    if overlay.is_none() {
        activity.extend(
            input
                .popup
                .iter()
                .map(|row| RenderRow::new(row.clone(), Role::Dim)),
        );
    }
    let mut bottom = Vec::with_capacity(budget.cap);
    bottom.extend(
        notices
            .iter()
            .rev()
            .take(budget.notices)
            .rev()
            .map(|row| RenderRow::new(escape(row), Role::Dim)),
    );
    bottom.extend(activity.into_iter().rev().take(budget.activity).rev());
    overlay_rows(&input, &budget, w, mode, &mut bottom);
    bottom.push(RenderRow::new(status, Role::Dim));
    let bottom: Vec<RenderRow> = bottom.into_iter().map(|row| row.clipped(w, mode)).collect();
    let rows = if input.screen == Screen::Inline {
        bottom
    } else {
        fullscreen_rows(&input, bottom, model.as_deref(), width, height, mode)
    };
    resolve_colors(rows, input.theme)
}

/// The interactive tail: an open dialog, a picker, or the composer and hint.
fn overlay_rows(
    input: &FrameInput<'_>,
    budget: &RegionBudget,
    w: usize,
    mode: WidthMode,
    bottom: &mut Vec<RenderRow>,
) {
    if input.dialog.is_open() {
        bottom.extend(input.dialog.rendered_rows(
            w,
            budget.overlay,
            mode,
            input.diagram_settings,
            input.diagram_cache,
        ));
    } else if let Some(picker) = input.picker {
        bottom.push(RenderRow::new(picker.title.clone(), Role::Accent));
        if let Some(id) = picker.model_filter_fallback() {
            bottom.push(RenderRow::new(
                format!("/model {}", escape(id)),
                if picker.is_selected(picker.selected_fallback_index()) {
                    Role::Accent
                } else {
                    Role::Dim
                },
            ));
        } else {
            let range = picker.visible_range();
            if picker.is_empty() {
                bottom.push(RenderRow::new(crate::copy::ids::PICKER_EMPTY, Role::Dim));
            }
            for index in range {
                if let Some(option) = picker.visible_option(index) {
                    let selected = picker.is_selected(index);
                    bottom.push(RenderRow::new(
                        format!(
                            "{} {}",
                            if selected { ">" } else { " " },
                            escape(&option.label)
                        ),
                        if selected { Role::Accent } else { Role::Dim },
                    ));
                }
            }
        }
        bottom.push(RenderRow::new(
            if picker.is_settings() {
                crate::copy::ids::SETTINGS_HINT
            } else {
                crate::copy::ids::PICKER_HINT
            },
            Role::Dim,
        ));
    } else {
        if input.composer.is_empty() {
            bottom.push(RenderRow::new(
                format!("> {}", escape(crate::copy::ids::COMPOSER_PLACEHOLDER)),
                Role::Text,
            ));
        } else {
            // A multiline draft shows its last rows, where the caret sits.
            let lines: Vec<&str> = input.composer.split('\n').collect();
            let start = lines.len() - budget.composer.clamp(1, lines.len());
            for (index, line) in lines.iter().enumerate().skip(start) {
                let mark = if index == 0 { "> " } else { "  " };
                bottom.push(RenderRow::new(
                    format!("{mark}{}", escape(line)),
                    Role::Text,
                ));
            }
        }
        if budget.hint > 0 {
            bottom.push(RenderRow::new(
                crate::copy::ids::HINT_IDLE_LEGACY,
                Role::Dim,
            ));
        }
    }
}

/// Fullscreen layout: header, padded transcript tail, then the bottom block.
fn fullscreen_rows(
    input: &FrameInput<'_>,
    bottom: Vec<RenderRow>,
    model: Option<&str>,
    width: u16,
    height: u16,
    mode: WidthMode,
) -> Vec<RenderRow> {
    let w = usize::from(width);
    let h = usize::from(height);
    let header = crate::screen::fullscreen::header_rows(width, height);
    let available = h.saturating_sub(header + bottom.len());
    let mut rows = Vec::with_capacity(h);
    if header > 0 {
        rows.push(RenderRow::new(
            format!(
                "dal · {} · {}",
                escape(input.view.session.name.as_deref().unwrap_or("session")),
                model.unwrap_or("no model")
            ),
            Role::Accent,
        ));
    }
    let transcript_len = input.transcript.rows().len();
    let transcript_start = transcript_len.saturating_sub(available);
    rows.extend(
        (transcript_start..transcript_len).filter_map(|index| input.transcript.render_row(index)),
    );
    rows.resize_with(header + available, || {
        RenderRow::new(String::new(), Role::Text)
    });
    rows.extend(bottom);
    rows.into_iter().map(|row| row.clipped(w, mode)).collect()
}

/// The single status row text: turn state, spinner, model, path, context.
fn status_line(input: &FrameInput<'_>, model: Option<&str>, w: usize, mode: WidthMode) -> String {
    let path = escape(&crate::status::contract_home(
        &input.view.session.workspace.as_path().display().to_string(),
        input.opts.env.home.as_deref(),
    ));
    let usage = &input.view.usage.usage;
    let tokens = (usage.input_tokens > 0 || usage.output_tokens > 0).then(|| {
        crate::copy::render(
            crate::copy::ids::STATUS_TOKENS,
            &[
                ("in", &crate::copy::tokens(usage.input_tokens)),
                ("out", &crate::copy::tokens(usage.output_tokens)),
            ],
            1,
        )
    });
    let cost = usage.cost_usd.map(|cost| format!("${cost:.3}"));
    let context = input
        .view
        .usage
        .context_tokens
        .saturating_mul(100)
        .checked_div(input.view.usage.context_window)
        .map(|percent| format!("ctx {percent}%"));
    let waiting = input.dialog.is_open();
    let running = matches!(
        input.view.turn,
        TurnState::Running { .. } | TurnState::Settling { .. }
    );
    let state = if waiting {
        Some(crate::copy::ids::STATE_WAITING)
    } else if matches!(input.view.turn, TurnState::Compacting { .. }) {
        Some(crate::copy::ids::STATE_COMPACTING)
    } else if running {
        input
            .live
            .activity()
            .or(Some(crate::copy::ids::STATE_WORKING))
    } else {
        None
    };
    let spinner = if running && !waiting {
        Some(if input.opts.motion { "⠋" } else { "*" })
    } else {
        None
    };
    status::render(
        StatusData {
            state,
            spinner,
            model,
            path: Some(&path),
            tokens: tokens.as_deref(),
            context: context.as_deref(),
            cost: cost.as_deref(),
            ..StatusData::default()
        },
        w,
        mode,
    )
}
fn queued_steer_row(turn: TurnState, count: u32) -> Option<RenderRow> {
    if count == 0 || !matches!(turn, TurnState::Running { .. } | TurnState::Settling { .. }) {
        return None;
    }
    let n = count.to_string();
    Some(RenderRow::new(
        crate::copy::render(
            crate::copy::ids::STEER_QUEUED,
            &[("n", &n)],
            u64::from(count),
        ),
        Role::Dim,
    ))
}

/// The scrollable middle region: first-run hint, queued steering, tool rows,
/// pending transcript rows, and streamed assistant text.
fn activity_rows(input: &FrameInput<'_>, w: usize, mode: WidthMode) -> Vec<RenderRow> {
    let mut activity = Vec::new();
    if input.view.settings.model.is_none() && input.opts.default_model.is_none() {
        activity.push(RenderRow::new(
            crate::copy::ids::FIRST_RUN_TITLE,
            Role::Accent,
        ));
        activity.push(RenderRow::new(
            crate::copy::ids::FIRST_RUN_ACTION,
            Role::Text,
        ));
    }
    if let Some(row) = queued_steer_row(input.view.turn, input.view.stats.steers_queued) {
        activity.push(row);
    }
    activity.extend(
        input
            .live
            .running_tool_rows()
            .into_iter()
            .map(|row| RenderRow::new(row, Role::Dim)),
    );
    activity.extend(input.transcript.pending_rows().cloned());
    if !input.live.assistant_text().is_empty() {
        activity.extend(
            text_rows(
                input.live.assistant_text(),
                prose_cap(w),
                mode,
                input.diagram_settings,
                input.diagram_cache,
            )
            .into_iter()
            .map(gutter_row),
        );
    }
    activity
}

fn resolve_colors(mut rows: Vec<RenderRow>, theme: &ResolvedTheme) -> Vec<RenderRow> {
    for row in &mut rows {
        row.color = theme.color(row.role);
    }
    rows
}

/// Renders the settled form of one visible history entry.
pub(crate) fn entry_rows(
    entry: &EntryView,
    columns: u16,
    mode: WidthMode,
    diagram_settings: DiagramSettings,
    diagram_cache: &RenderCache,
) -> Vec<RenderRow> {
    entry_rows_timed(entry, columns, mode, diagram_settings, diagram_cache, None)
}

/// Renders one history entry; `duration` is how long a tool call ran.
pub(crate) fn entry_rows_timed(
    entry: &EntryView,
    columns: u16,
    mode: WidthMode,
    diagram_settings: DiagramSettings,
    diagram_cache: &RenderCache,
    duration: Option<std::time::Duration>,
) -> Vec<RenderRow> {
    let cap = prose_cap(usize::from(columns));
    match &entry.kind {
        EntryKind::User { parts } => {
            let text = parts
                .iter()
                .filter_map(|part| match part {
                    JournalPart::Text { text } => Some(text.as_ref()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            text_rows(
                &text,
                cap.saturating_sub(2),
                mode,
                diagram_settings,
                diagram_cache,
            )
            .into_iter()
            .map(user_row)
            .collect()
        }
        EntryKind::Assistant { content, .. } => {
            let mut rows = Vec::new();
            for block in content {
                match block {
                    Block::Text { text } => {
                        rows.extend(text_rows(text, cap, mode, diagram_settings, diagram_cache));
                    }
                    Block::Reasoning { .. } => rows.push(RenderRow::new(
                        "Thinking · ctrl+o to show reasoning",
                        Role::Dim,
                    )),
                    // The call settles into one card from its result entry.
                    Block::ToolCall { .. } => {}
                }
            }
            rows.into_iter().map(gutter_row).collect()
        }
        EntryKind::ToolResult {
            name, error, parts, ..
        } => {
            let text = parts
                .iter()
                .find_map(|part| match part {
                    JournalPart::Text { text } => Some(text.as_ref()),
                    _ => None,
                })
                .unwrap_or("");
            vec![gutter_row(
                tool_card_row(name, *error, text, duration).clipped(cap, mode),
            )]
        }
        _ => Vec::new(),
    }
}

#[derive(Debug)]
struct RichLine {
    text: String,
    links: Vec<RenderLink>,
}

fn linked_text(text: &str) -> (String, Vec<RenderLink>) {
    let mut visible = String::with_capacity(text.len());
    let mut links = Vec::new();
    let mut cursor = 0;
    while cursor < text.len() {
        if let Some((consumed, label, url)) = osc8_link(&text[cursor..]) {
            let start = visible.len();
            visible.push_str(label);
            links.push(RenderLink {
                range: start..visible.len(),
                url: url.to_owned(),
            });
            cursor += consumed;
            continue;
        }
        if let Some((consumed, label, url)) = markdown_link(&text[cursor..]) {
            let start = visible.len();
            visible.push_str(label);
            links.push(RenderLink {
                range: start..visible.len(),
                url: url.to_owned(),
            });
            cursor += consumed;
            continue;
        }
        if let Some((consumed, url)) = plain_url(&text[cursor..]) {
            let start = visible.len();
            visible.push_str(&text[cursor..cursor + consumed]);
            links.push(RenderLink {
                range: start..visible.len(),
                url,
            });
            cursor += consumed;
            continue;
        }
        let Some(character) = text[cursor..].chars().next() else {
            break;
        };
        visible.push(character);
        cursor += character.len_utf8();
    }
    escape_linked(&visible, links)
}

fn osc8_link(text: &str) -> Option<(usize, &str, &str)> {
    let prefix = "\u{1b}]8;;";
    let terminator = "\u{1b}\\";
    let close = "\u{1b}]8;;\u{1b}\\";
    let rest = text.strip_prefix(prefix)?;
    let url_end = rest.find(terminator)?;
    let url = &rest[..url_end];
    if !valid_link_url(url) {
        return None;
    }
    let label_start = url_end + terminator.len();
    let close_start = rest[label_start..].find(close)?;
    let label = &rest[label_start..label_start + close_start];
    Some((
        prefix.len() + label_start + close_start + close.len(),
        label,
        url,
    ))
}

fn markdown_link(text: &str) -> Option<(usize, &str, &str)> {
    let text = text.strip_prefix('[')?;
    let close_label = text.find(']')?;
    if text.as_bytes().get(close_label + 1) != Some(&b'(') {
        return None;
    }
    let close_url = text[close_label + 2..].find(')')? + close_label + 2;
    let label = &text[..close_label];
    let url = &text[close_label + 2..close_url];
    valid_link_url(url).then_some((close_url + 2, label, url))
}

fn plain_url(text: &str) -> Option<(usize, String)> {
    let valid_start = text.starts_with("file://");
    if !valid_start {
        return None;
    }
    let end = text
        .char_indices()
        .find(|(_, character)| character.is_whitespace() || character.is_control())
        .map_or(text.len(), |(index, _)| index);
    let candidate = text[..end].trim_end_matches(['.', ',', ';', ':', '!', '?']);
    if candidate.is_empty() || !valid_link_url(candidate) {
        return None;
    }
    Some((candidate.len(), candidate.to_owned()))
}

fn valid_link_url(url: &str) -> bool {
    !url.bytes()
        .any(|byte| byte.is_ascii_control() || byte == b'\x7f')
        && url
            .strip_prefix("file://")
            .is_some_and(|path| std::path::Path::new(path).is_absolute())
}

fn escape_linked(text: &str, links: Vec<RenderLink>) -> (String, Vec<RenderLink>) {
    let mut escaped = String::with_capacity(text.len());
    let mut offsets = vec![0; text.len() + 1];
    for (index, character) in text.char_indices() {
        offsets[index] = escaped.len();
        match character {
            '\t' => escaped.push_str("\\t"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\u{7}' => escaped.push_str("\\a"),
            character if character.is_control() => {
                use std::fmt::Write as _;
                let _ = write!(escaped, "\\u{{{}}}", u32::from(character));
            }
            character => escaped.push(character),
        }
    }
    offsets[text.len()] = escaped.len();
    let links = links
        .into_iter()
        .map(|link| RenderLink {
            range: offsets[link.range.start]..offsets[link.range.end],
            url: link.url,
        })
        .collect();
    (escaped, links)
}

pub(crate) fn prose_rows(text: &str, cap: usize, mode: WidthMode) -> Vec<RenderRow> {
    let mut rows = Vec::new();
    for logical in text.split('\n') {
        let (line_text, line_links) = linked_text(logical);
        let mut line = RichLine {
            text: line_text,
            links: line_links,
        };
        for prefix in ["### ", "## ", "# "] {
            if line.text.starts_with(prefix) {
                line.text.drain(..prefix.len());
                for link in &mut line.links {
                    link.range.start = link.range.start.saturating_sub(prefix.len());
                    link.range.end = link.range.end.saturating_sub(prefix.len());
                }
                line.links.retain(|link| link.range.start < link.range.end);
                break;
            }
        }
        rows.extend(wrap_rich_line(&line, cap, mode));
    }
    if rows.is_empty() {
        rows.push(RenderRow::new(String::new(), Role::Text));
    }
    rows
}

fn wrap_rich_line(line: &RichLine, cap: usize, mode: WidthMode) -> Vec<RenderRow> {
    if line.text.is_empty() {
        return vec![RenderRow::new(String::new(), Role::Text)];
    }
    let cap = cap.max(1);
    let mut rows = Vec::new();
    let mut start = 0;
    let mut row_width = 0;
    for (index, cluster) in line.text.grapheme_indices(true) {
        let cluster_width = crate::width::width(cluster, mode);
        if start < index
            && (row_width + cluster_width > cap
                || (cluster_width == 2 && row_width == cap.saturating_sub(1)))
        {
            rows.push(linked_row(line, start, index));
            start = index;
            row_width = 0;
        }
        row_width += cluster_width;
        let end = index + cluster.len();
        if cluster_width > cap {
            rows.push(linked_row(line, start, end));
            start = end;
            row_width = 0;
        }
    }
    if start < line.text.len() || rows.is_empty() {
        rows.push(linked_row(line, start, line.text.len()));
    }
    rows
}

fn linked_row(line: &RichLine, start: usize, end: usize) -> RenderRow {
    let links = line
        .links
        .iter()
        .filter_map(|link| {
            let link_start = link.range.start.max(start);
            let link_end = link.range.end.min(end);
            (link_start < link_end).then(|| RenderLink {
                range: link_start - start..link_end - start,
                url: link.url.clone(),
            })
        })
        .collect();
    RenderRow {
        text: line.text[start..end].to_owned(),
        role: Role::Text,
        color: ratatui::style::Color::Reset,
        spans: Vec::new(),
        links,
        pending_diagram: false,
        image: None,
        image_tail: false,
    }
}

pub(crate) fn text_rows(
    text: &str,
    cap: usize,
    mode: WidthMode,
    settings: DiagramSettings,
    cache: &RenderCache,
) -> Vec<RenderRow> {
    let Some(wired) = wire_block(&settings, cache, text, cap) else {
        return prose_rows(text, cap, mode);
    };
    match wired {
        WiredBlock::Art(art) => art.rows.iter().map(|cells| art_row(cells)).collect(),
        WiredBlock::Fallback {
            kind,
            reason,
            source,
        } => {
            let mut rows = vec![RenderRow::new(
                crate::diagram::fallback_card(kind, &reason),
                Role::Warning,
            )];
            rows.extend(prose_rows(&source, cap, mode));
            rows
        }
        WiredBlock::Pixels(pixels) => pixel_rows(pixels, cap),
        WiredBlock::Pending { kind } => {
            let mut row = RenderRow::new(
                crate::copy::render(
                    crate::copy::ids::DIAGRAM_RENDERING,
                    &[("kind", diagram_kind_name(kind))],
                    1,
                ),
                Role::Dim,
            );
            row.pending_diagram = true;
            vec![row]
        }
    }
}

fn diagram_kind_name(kind: crate::diagram::DiagramKind) -> &'static str {
    match kind {
        crate::diagram::DiagramKind::D2 => "d2",
        crate::diagram::DiagramKind::Nomnoml => "nomnoml",
        crate::diagram::DiagramKind::Dot => "dot",
        crate::diagram::DiagramKind::Mermaid => "mermaid",
    }
}

fn pixel_rows(pixels: Arc<[u8]>, cap: usize) -> Vec<RenderRow> {
    let card = |dimensions| {
        RenderRow::new(
            crate::image::card("diagram", dimensions, pixels.len()),
            Role::Dim,
        )
    };
    let Some(dimensions) = crate::diagram::raster::png_dimensions(&pixels) else {
        return vec![card(None)];
    };
    let columns = u32::try_from(cap).unwrap_or(u32::MAX);
    let (columns, rows) =
        crate::diagram::raster::scale_to_cells(dimensions.0, dimensions.1, 10, 20, columns, 16);
    let (Ok(columns), Ok(rows)) = (u16::try_from(columns), u16::try_from(rows)) else {
        return vec![card(Some(dimensions))];
    };
    if columns == 0 || rows == 0 {
        return vec![card(Some(dimensions))];
    }
    let image = PixelImage {
        digest: *blake3::hash(&pixels).as_bytes(),
        bytes: pixels,
        columns,
        rows,
    };
    let image_rows = usize::from(image.rows);
    let mut first = RenderRow::new(
        crate::image::card("diagram", Some(dimensions), image.bytes.len()),
        Role::Dim,
    );
    first.image = Some(image);
    let mut output = Vec::with_capacity(image_rows);
    output.push(first);
    for _ in 1..image_rows {
        let mut row = RenderRow::new(String::new(), Role::Text);
        row.image_tail = true;
        output.push(row);
    }
    output
}

fn art_row(cells: &[crate::diagram::ArtCell]) -> RenderRow {
    let mut row = RenderRow::new(String::new(), Role::Text);
    for cell in cells {
        let text = escape(&cell.text);
        let start = row.text.len();
        row.text.push_str(&text);
        let role = match cell.role {
            ArtRole::Border => Role::Dim,
            ArtRole::Text => Role::Text,
            ArtRole::Edge | ArtRole::Title => Role::Accent,
            ArtRole::EdgeLabel => Role::Warning,
        };
        if let Some(span) = row.spans.last_mut()
            && span.role == role
        {
            span.range.end = row.text.len();
        } else {
            row.spans.push(RenderSpan {
                range: start..row.text.len(),
                role,
            });
        }
    }
    row
}

/// One settled tool card: `ok  {name} {summary} · {dur}` or
/// `failed  {name} · {reason} · {dur}`; a call nobody timed omits the duration.
fn tool_card_row(
    name: &str,
    error: bool,
    text: &str,
    duration: Option<std::time::Duration>,
) -> RenderRow {
    let detail = escape(
        &text
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
    );
    let name = escape(name);
    let dur = duration.map(crate::copy::dur).unwrap_or_default();
    let (template, field) = if error {
        (crate::copy::ids::TOOL_FAILED, "reason")
    } else {
        (crate::copy::ids::TOOL_OK, "summary")
    };
    let line = crate::copy::render(
        template,
        &[("name", &name), (field, &detail), ("dur", &dur)],
        1,
    );
    let line = line.replace("  · ", " · ");
    let line = if duration.is_none() {
        line.strip_suffix(" · ").unwrap_or(&line).to_owned()
    } else {
        line
    };
    RenderRow::new(line, Role::Text)
}

fn user_row(row: RenderRow) -> RenderRow {
    indent_row(row, "> ")
}

/// Reserves the 2-cell left gutter every transcript row keeps. Rows that carry
/// pixels keep their columns.
pub(crate) fn gutter_row(row: RenderRow) -> RenderRow {
    if row.image.is_some() || row.image_tail {
        return row;
    }
    indent_row(row, "  ")
}

fn indent_row(mut row: RenderRow, prefix: &str) -> RenderRow {
    row.text.insert_str(0, prefix);
    for span in &mut row.spans {
        span.range.start += prefix.len();
        span.range.end += prefix.len();
    }
    for link in &mut row.links {
        link.range.start += prefix.len();
        link.range.end += prefix.len();
    }
    row
}

fn prose_cap(width: usize) -> usize {
    match width {
        120.. => 100,
        80..=119 => width.saturating_sub(8),
        60..=79 => width.saturating_sub(6),
        40..=59 => width.saturating_sub(4),
        _ => width.saturating_sub(2).max(1),
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use dal_core::{EntryId, EntryKind, EntryView, JournalPart};

    use super::entry_rows;
    use crate::WidthMode;
    use crate::diagram::{DiagramSettings, RenderCache};
    use crate::transcript::Transcript;

    #[test]
    fn tool_cards_follow_the_copy_deck_with_a_clean_one_line_summary() {
        use std::time::Duration;

        let ok = super::tool_card_row(
            "read",
            false,
            "1\talpha\n2\tbeta",
            Some(Duration::from_millis(52)),
        );
        assert_eq!(ok.text, "ok  read 1 alpha · 52 ms");
        let failed = super::tool_card_row(
            "exec",
            true,
            "Permission denied: exec was declined.",
            Some(Duration::from_millis(1_500)),
        );
        assert_eq!(
            failed.text,
            "failed  exec · Permission denied: exec was declined. · 1.5s"
        );
        let untimed = super::tool_card_row("exec", false, "done", None);
        assert_eq!(untimed.text, "ok  exec done");
        let silent = super::tool_card_row("exec", false, "", Some(Duration::from_secs(3)));
        assert_eq!(silent.text, "ok  exec · 3.0s");
    }

    #[test]
    fn transcript_diagram_rows_keep_source_off_and_render_art_when_enabled() {
        let entry = EntryView {
            id: EntryId::new(NonZeroU64::MIN),
            parent: None,
            kind: EntryKind::User {
                parts: vec![JournalPart::Text {
                    text: "```mermaid\ngraph TD; A-->B\n```".into(),
                }],
            },
        };
        let off_cache = RenderCache::default();
        let off_rows = entry_rows(
            &entry,
            80,
            WidthMode::Narrow,
            DiagramSettings::default(),
            &off_cache,
        );
        let mut off_transcript = Transcript::default();
        off_transcript.commit_rendered("off", &off_rows);
        assert!(
            off_transcript
                .rows()
                .iter()
                .any(|row| row.contains("graph TD"))
        );
        assert_eq!(off_cache.renders(), 0);

        let on_cache = RenderCache::default();
        let on_rows = entry_rows(
            &entry,
            80,
            WidthMode::Narrow,
            DiagramSettings { enabled: true },
            &on_cache,
        );
        let mut on_transcript = Transcript::default();
        on_transcript.commit_rendered("on", &on_rows);
        assert!(
            !on_transcript
                .rows()
                .iter()
                .any(|row| row.contains("graph TD") || row.contains("```"))
        );
        assert!(
            on_transcript
                .row_data(0)
                .is_some_and(|(_, spans)| !spans.is_empty())
        );
        assert_eq!(on_cache.renders(), 1);
    }
    #[test]
    fn queued_steer_rows_follow_the_authoritative_turn_state() {
        use dal_core::{TurnId, TurnState};

        let turn = TurnId::new(NonZeroU64::MIN);
        let row = super::queued_steer_row(TurnState::Running { turn }, 2)
            .expect("running turns display queued steering");
        assert_eq!(row.text, "2 messages queued for the next reply");
        assert!(super::queued_steer_row(TurnState::Idle, 2).is_none());
    }
    #[test]
    fn wrapped_transcript_links_keep_their_destination() {
        let rows = super::text_rows(
            "before [docs](file:///workspace/reference) after",
            10,
            WidthMode::Narrow,
            DiagramSettings::default(),
            &RenderCache::default(),
        );
        assert!(rows.iter().any(|row| !row.links.is_empty()));
        assert!(
            rows.iter()
                .flat_map(|row| row.links.iter())
                .all(|link| link.url == "file:///workspace/reference")
        );
        assert!(
            rows.iter()
                .all(|row| !row.text.contains("[docs]") && !row.text.contains("(file://"))
        );
    }
    #[test]
    fn clipped_tool_result_links_keep_the_complete_destination() {
        use dal_core::CallId;

        let path = "file:///workspace/project/src/very-long-module-name.rs";
        let entry = dal_core::EntryView {
            id: dal_core::EntryId::new(NonZeroU64::MIN),
            parent: None,
            kind: EntryKind::ToolResult {
                call: CallId::new("call-1"),
                name: "read".into(),
                error: false,
                parts: vec![JournalPart::Text { text: path.into() }],
                changes: Vec::new(),
                elapsed_ms: None,
            },
        };
        let rows = entry_rows(
            &entry,
            32,
            WidthMode::Narrow,
            DiagramSettings::default(),
            &RenderCache::default(),
        );

        let link = rows
            .first()
            .and_then(|row| row.links.first())
            .expect("clipped tool result keeps its file link");
        assert_eq!(link.url, path);
    }
}
