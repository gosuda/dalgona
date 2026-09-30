//! Pure projection from a session view and live front-end state to terminal rows.

use std::ops::Range;
use std::sync::Arc;

use dal_core::{Block, EntryKind, EntryView, JournalPart, TurnState, View};

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
    pub(crate) pending_diagram: bool,
    pub(crate) image: Option<PixelImage>,
    pub(crate) image_tail: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RenderSpan {
    pub(crate) range: Range<usize>,
    pub(crate) role: Role,
}

impl RenderRow {
    pub(crate) fn new(text: impl Into<String>, role: Role) -> Self {
        Self {
            text: text.into(),
            role,
            color: ratatui::style::Color::Reset,
            spans: Vec::new(),
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
        .map(|model| escape(model.id()));
    let path = escape(&input.view.session.workspace.as_path().display().to_string());
    let context = if input.view.usage.context_window > 0 {
        Some(format!(
            "ctx {}%",
            input.view.usage.context_tokens.saturating_mul(100) / input.view.usage.context_window
        ))
    } else {
        None
    };
    let waiting = input.dialog.is_open();
    let running = matches!(
        input.view.turn,
        TurnState::Running { .. } | TurnState::Settling { .. }
    );
    let state = if waiting {
        Some(crate::copy::ids::STATE_WAITING)
    } else if matches!(input.view.turn, TurnState::Compacting { .. }) {
        Some(crate::copy::ids::STATE_COMPACTING)
    } else if let Some(activity) = input.live.activity() {
        Some(activity)
    } else if running {
        Some(crate::copy::ids::STATE_WORKING)
    } else {
        None
    };
    let spinner = if running && !waiting {
        Some(if input.opts.motion { "⠋" } else { "*" })
    } else {
        None
    };
    let status = status::render(
        StatusData {
            state,
            spinner,
            model: model.as_deref(),
            path: Some(&path),
            context: context.as_deref(),
            ..StatusData::default()
        },
        w,
        mode,
    );
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

    let mut activity = Vec::new();
    if input.view.settings.model.is_none() {
        activity.push(RenderRow::new(
            crate::copy::ids::FIRST_RUN_TITLE,
            Role::Accent,
        ));
        activity.push(RenderRow::new(
            crate::copy::ids::FIRST_RUN_ACTION,
            Role::Text,
        ));
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
        activity.extend(text_rows(
            input.live.assistant_text(),
            prose_cap(w),
            mode,
            input.diagram_settings,
            input.diagram_cache,
        ));
    }
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
            composer: 1,
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
        let draft = if input.composer.is_empty() {
            crate::copy::ids::COMPOSER_PLACEHOLDER
        } else {
            input.composer
        };
        bottom.push(RenderRow::new(format!("> {}", escape(draft)), Role::Text));
        if budget.hint > 0 {
            bottom.push(RenderRow::new(
                crate::copy::ids::HINT_IDLE_LEGACY,
                Role::Dim,
            ));
        }
    }
    bottom.push(RenderRow::new(status, Role::Dim));
    let bottom: Vec<RenderRow> = bottom.into_iter().map(|row| row.clipped(w, mode)).collect();
    let rows = if input.screen == Screen::Inline {
        bottom
    } else {
        let header = crate::screen::fullscreen::header_rows(width, height);
        let available = h.saturating_sub(header + bottom.len());
        let mut rows = Vec::with_capacity(h);
        if header > 0 {
            rows.push(RenderRow::new(
                format!(
                    "dal · {} · {}",
                    escape(input.view.session.name.as_deref().unwrap_or("session")),
                    model.as_deref().unwrap_or("no model")
                ),
                Role::Accent,
            ));
        }
        let transcript_len = input.transcript.rows().len();
        let transcript_start = transcript_len.saturating_sub(available);
        rows.extend(
            (transcript_start..transcript_len)
                .filter_map(|index| input.transcript.render_row(index)),
        );
        rows.resize_with(header + available, || {
            RenderRow::new(String::new(), Role::Text)
        });
        rows.extend(bottom);
        rows.into_iter().map(|row| row.clipped(w, mode)).collect()
    };
    resolve_colors(rows, input.theme)
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
                        rows.extend(text_rows(text, cap, mode, diagram_settings, diagram_cache))
                    }
                    Block::Reasoning { .. } => rows.push(RenderRow::new(
                        "Thinking · ctrl+o to show reasoning",
                        Role::Dim,
                    )),
                    Block::ToolCall { name, .. } => rows.push(RenderRow::new(
                        format!("working  {}", escape(name)),
                        Role::Dim,
                    )),
                }
            }
            rows
        }
        EntryKind::ToolResult {
            name, error, parts, ..
        } => {
            let text = parts
                .iter()
                .filter_map(|part| match part {
                    JournalPart::Text { text } => Some(text.as_ref()),
                    _ => None,
                })
                .next()
                .unwrap_or("");
            let word = if *error { "failed" } else { "ok" };
            vec![RenderRow::new(
                take_cells(
                    &format!(
                        "{word}  {} · {}",
                        escape(name),
                        escape(text.lines().next().unwrap_or(""))
                    ),
                    cap,
                    mode,
                ),
                Role::Text,
            )]
        }
        EntryKind::Reminder { .. } => Vec::new(),
        _ => Vec::new(),
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
        return safe_prose(text, cap, mode)
            .into_iter()
            .map(|row| RenderRow::new(row, Role::Text))
            .collect();
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
            rows.extend(
                safe_prose(&source, cap, mode)
                    .into_iter()
                    .map(|row| RenderRow::new(row, Role::Text)),
            );
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
            ArtRole::Edge => Role::Accent,
            ArtRole::EdgeLabel => Role::Warning,
            ArtRole::Title => Role::Accent,
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

fn user_row(mut row: RenderRow) -> RenderRow {
    row.text.insert_str(0, "> ");
    for span in &mut row.spans {
        span.range.start += 2;
        span.range.end += 2;
    }
    row
}

fn safe_prose(text: &str, cap: usize, mode: WidthMode) -> Vec<String> {
    text.split('\n')
        .flat_map(|line| crate::markdown::render_prose(&escape(line), cap, mode))
        .collect()
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
