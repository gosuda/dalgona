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
use crate::status::StatusData;
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
    /// Byte offset of the caret within `composer`.
    pub(crate) cursor: usize,
    /// Whether the discard-draft question owns the composer and hint rows.
    pub(crate) exit_draft: bool,
    /// The checked-out git branch of the workspace, when it has one.
    pub(crate) branch: Option<&'a str>,
    pub(crate) popup: &'a [String],
    pub(crate) live: &'a Live,
    pub(crate) dialog: &'a DialogUi,
    pub(crate) picker: Option<&'a PickerUi>,
    /// The open sign-in overlay, which owns the composer and hint rows.
    pub(crate) signin: Option<&'a crate::signin::SignIn>,
    pub(crate) transcript: &'a Transcript,
    /// The transcript viewport: follow state, frozen scroll window, search.
    pub(crate) viewport: &'a crate::screen::fullscreen::Viewport,
    pub(crate) opts: &'a TuiOptions,
    /// Whether the terminal confirmed kitty keyboard encoding, so the hint
    /// can advertise the kitty column of the key map.
    pub(crate) kitty_keyboard: bool,
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
    /// Cell column of the caret when this row holds it.
    pub(crate) cursor: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RenderSpan {
    pub(crate) range: Range<usize>,
    pub(crate) role: Role,
    pub(crate) bold: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RenderLink {
    pub(crate) range: Range<usize>,
    pub(crate) url: String,
}

impl RenderRow {
    pub(crate) fn new(text: impl Into<String>, role: Role) -> Self {
        let (text, links) = linked_text(&text.into());
        let mut row = Self::plain(text, role);
        row.links = links;
        row
    }

    /// A row whose text is shown exactly as given: link markup is not read.
    pub(crate) fn plain(text: impl Into<String>, role: Role) -> Self {
        Self {
            text: text.into(),
            role,
            color: ratatui::style::Color::Reset,
            spans: Vec::new(),
            links: Vec::new(),
            pending_diagram: false,
            image: None,
            image_tail: false,
            cursor: None,
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
    let floor = if h < 8 {
        Some(crate::copy::ids::NARROW_ROWS)
    } else if w < 12 {
        Some(crate::copy::ids::NARROW_COLS)
    } else {
        None
    };
    if let Some(warning) = floor {
        return resolve_colors(
            vec![
                RenderRow::new(warning, Role::Warning).clipped(w, mode),
                status.clipped(w, mode),
            ],
            input.theme,
        );
    }

    let mut activity = activity_rows(&input, w, mode, h);
    let notices = input.live.notices();
    let overlay = overlay_height(&input, w, mode);
    let laid = composer_layout(&input, w, mode);
    let budget = RegionBudget::allocate(
        width,
        height,
        RegionRequest {
            notices: notices.len(),
            activity: usize::MAX,
            composer: laid.rows.len(),
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
    let notice_rows = notice_rows(notices, budget.notices, w, mode);
    if input.screen == Screen::Inline {
        // The inline live block opens with its notices.
        bottom.extend(notice_rows.iter().cloned());
    }
    bottom.extend(activity.into_iter().rev().take(budget.activity).rev());
    overlay_rows(&input, &budget, &laid, w, mode, &mut bottom);
    bottom.push(status);
    let bottom: Vec<RenderRow> = bottom.into_iter().map(|row| row.clipped(w, mode)).collect();
    let rows = if input.screen == Screen::Inline {
        bottom
    } else {
        fullscreen_rows(
            &input,
            notice_rows,
            bottom,
            model.as_deref(),
            width,
            height,
            mode,
        )
    };
    resolve_colors(rows, input.theme)
}

/// The rows the interactive tail takes in place of the composer, if any.
fn overlay_height(input: &FrameInput<'_>, w: usize, mode: WidthMode) -> Option<usize> {
    if input.dialog.is_open() {
        Some(6)
    } else if let Some(signin) = input.signin {
        Some(signin.rows(w, mode, usize::MAX).len())
    } else if let Some(picker) = input.picker {
        Some(picker.visible_len().max(1) + 2)
    } else if input.exit_draft {
        Some(1)
    } else {
        None
    }
}

/// The newest `count` notices as clipped dim rows.
fn notice_rows(notices: &[String], count: usize, w: usize, mode: WidthMode) -> Vec<RenderRow> {
    notices
        .iter()
        .rev()
        .take(count)
        .rev()
        .map(|row| RenderRow::new(escape(row), Role::Dim).clipped(w, mode))
        .collect()
}

/// Wraps the draft inside the 2-cell prompt gutter and finds the caret.
fn composer_layout(input: &FrameInput<'_>, w: usize, mode: WidthMode) -> crate::composer::Laid {
    crate::composer::layout(input.composer, input.cursor, w.saturating_sub(2), mode)
}

/// The deck entries of the plain hint row; the newline entry shows the
/// working key for the terminal's keyboard encoding.
fn hint_entries(kitty: bool) -> Vec<&'static str> {
    use crate::copy::ids;
    vec![
        ids::HINT_SEND,
        if kitty {
            ids::HINT_NEWLINE
        } else {
            ids::HINT_NEWLINE_LEGACY
        },
        ids::HINT_INTERRUPT,
        ids::HINT_HELP,
    ]
}

/// Folds a hint row to `w` with the copy deck's fold priority: navigation
/// keys drop first, then help, then newline, then interrupt, then send. The
/// newline entry degrades to the working key before anything drops; state
/// cues never fold away and replace the hint entirely at the smallest
/// budgets.
fn fold_hint(segments: Vec<&'static str>, w: usize, mode: WidthMode) -> String {
    use crate::copy::ids;
    let fits = |segments: &[&str]| crate::width::width(&segments.join(" · "), mode) <= w;
    let mut segments = segments;
    let kitty_newline = segments.contains(&ids::HINT_NEWLINE);
    if kitty_newline && !fits(&segments) {
        for entry in &mut segments {
            if *entry == ids::HINT_NEWLINE {
                *entry = ids::HINT_NEWLINE_LEGACY;
            }
        }
    }
    for entry in [
        ids::HINT_TRANSCRIPT,
        ids::HINT_HELP,
        ids::HINT_NEWLINE_LEGACY,
        ids::HINT_INTERRUPT,
        ids::HINT_SEND,
    ] {
        if !fits(&segments) {
            segments.retain(|candidate| *candidate != entry);
        }
    }
    segments.join(" · ")
}

/// The interactive tail: an open dialog, a picker, or the composer and hint.
fn overlay_rows(
    input: &FrameInput<'_>,
    budget: &RegionBudget,
    laid: &crate::composer::Laid,
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
    } else if let Some(signin) = input.signin {
        bottom.extend(signin.rows(w, mode, budget.overlay));
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
    } else if input.exit_draft {
        bottom.push(RenderRow::plain(
            format!(
                "[y] {}   [n] {}",
                crate::copy::ids::EXIT_DRAFT_TITLE,
                crate::copy::ids::EXIT_KEEP_EDITING
            ),
            Role::Accent,
        ));
    } else {
        if input.composer.is_empty() {
            let mut row = RenderRow::new(
                format!("> {}", escape(crate::copy::ids::COMPOSER_PLACEHOLDER)),
                Role::Text,
            );
            row.cursor = Some(2);
            bottom.push(row);
        } else {
            // The shown rows end at the caret's row, so the caret is always on screen.
            let shown = budget.composer.clamp(1, laid.rows.len());
            let start = (laid.caret.0 + 1).saturating_sub(shown);
            for (index, text) in laid.rows.iter().enumerate().skip(start).take(shown) {
                let mark = if index == 0 { "> " } else { "  " };
                let mut row = RenderRow::plain(format!("{mark}{text}"), Role::Text);
                if index == laid.caret.0 {
                    row.cursor = Some(2 + laid.caret.1);
                }
                bottom.push(row);
            }
        }
        if budget.hint > 0 {
            let viewport_live = input.screen == Screen::Fullscreen;
            let mut segments = hint_entries(input.kitty_keyboard);
            if let Some(cue) = viewport_live.then(|| input.viewport.cue()).flatten() {
                segments.insert(0, cue);
            }
            if viewport_live {
                segments.push(crate::copy::ids::HINT_TRANSCRIPT);
            }
            bottom.push(RenderRow::new(fold_hint(segments, w, mode), Role::Dim));
        }
    }
}

/// Fullscreen layout: header, notices, the search row over the viewport
/// top, the transcript window, then the bottom block.
fn fullscreen_rows(
    input: &FrameInput<'_>,
    notices: Vec<RenderRow>,
    bottom: Vec<RenderRow>,
    model: Option<&str>,
    width: u16,
    height: u16,
    mode: WidthMode,
) -> Vec<RenderRow> {
    let w = usize::from(width);
    let h = usize::from(height);
    let header = crate::screen::fullscreen::header_rows(width, height);
    let search_rows = usize::from(input.viewport.search_text().is_some());
    let above = header + notices.len() + search_rows;
    let available = h.saturating_sub(above + bottom.len());
    let transcript_len = input.transcript.rows().len();
    input.viewport.observe(available);
    let window_top = input.viewport.window_top(transcript_len, available);
    let window_end = (window_top + available).min(transcript_len);
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
    rows.extend(notices);
    if let Some(query) = input.viewport.search_text() {
        rows.push(search_row(input, query));
    }
    rows.extend((window_top..window_end).filter_map(|index| input.transcript.render_row(index)));
    rows.resize_with(above + available, || {
        RenderRow::new(String::new(), Role::Text)
    });
    rows.extend(bottom);
    rows.into_iter().map(|row| row.clipped(w, mode)).collect()
}

/// The filter row over the viewport top: the query and its live match count.
fn search_row(input: &FrameInput<'_>, query: &str) -> RenderRow {
    use crate::copy::ids;
    let mut suffix = String::new();
    if !query.is_empty() {
        let matches = input.transcript.search_matches(query);
        suffix = match matches {
            0 => format!(" · {}", ids::SEARCH_NO_MATCHES),
            count => {
                let count = u64::try_from(count).unwrap_or(u64::MAX);
                format!(
                    " {}",
                    crate::copy::render(ids::SEARCH_MATCHES, &[("n", &count.to_string())], count)
                )
            }
        };
    }
    let text = format!("/{query}{suffix}");
    RenderRow::new(text, Role::Accent)
}

/// The single status row: turn state, spinner, model, path, context.
///
/// The context slot carries its own word (`rising` at 70%, `high` at 90%) and
/// the matching role, so the state survives without color.
fn status_line(
    input: &FrameInput<'_>,
    model: Option<&str>,
    w: usize,
    mode: WidthMode,
) -> RenderRow {
    let home = escape(&crate::status::contract_home(
        &input.view.session.workspace.as_path().display().to_string(),
        input.opts.env.home.as_deref(),
    ));
    let path = match input.branch {
        Some(branch) => format!("{home} ({})", escape(branch)),
        None => home,
    };
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
        .map(crate::status::context_slot);
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
    // Motion advances the braille frames; `DAL_NO_MOTION` freezes the first.
    let spinner = if running && !waiting {
        Some(if input.opts.motion {
            input.live.spinner_cell()
        } else {
            crate::live::SPINNER_FRAMES[0]
        })
    } else {
        None
    };
    let jobs = match input.live.running_jobs() {
        0 => None,
        count => {
            let count = u64::try_from(count).unwrap_or(u64::MAX);
            Some(crate::copy::render(
                crate::copy::ids::STATUS_JOBS,
                &[("n", &count.to_string())],
                count,
            ))
        }
    };
    let (line, spans) = crate::status::render_with_roles(
        StatusData {
            state,
            spinner,
            model,
            path: Some(&path),
            tokens: tokens.as_deref(),
            context: context.as_ref().map(|(text, _)| text.as_str()),
            context_role: context.as_ref().and_then(|(_, role)| *role),
            agents: jobs.as_deref(),
            cost: cost.as_deref(),
        },
        w,
        mode,
    );
    let mut row = RenderRow::new(line, Role::Dim);
    row.spans = spans
        .into_iter()
        .map(|(range, role)| RenderSpan {
            range,
            role,
            bold: false,
        })
        .collect();
    row
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
/// pending transcript rows, and streamed assistant text. Streamed text keeps only
/// its last `limit` rows, the most any frame can show.
fn activity_rows(
    input: &FrameInput<'_>,
    w: usize,
    mode: WidthMode,
    limit: usize,
) -> Vec<RenderRow> {
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
    let text = input.live.assistant_text();
    if !text.is_empty() {
        let cap = prose_cap(w);
        let rows = match wire_block(&input.diagram_settings, input.diagram_cache, text, cap) {
            Some(wired) => wired_rows(wired, cap, mode),
            None => input
                .live
                .assistant_rows(cap, w.saturating_sub(2), mode, limit),
        };
        activity.extend(rows.into_iter().map(gutter_row));
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
                Prose::Verbatim,
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
                        rows.extend(text_rows(
                            text,
                            cap,
                            mode,
                            Prose::Markdown {
                                full: usize::from(columns).saturating_sub(2),
                            },
                            diagram_settings,
                            diagram_cache,
                        ));
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
pub(crate) struct RichLine {
    pub(crate) text: String,
    pub(crate) links: Vec<RenderLink>,
    pub(crate) spans: Vec<RenderSpan>,
}

pub(crate) fn linked_text(text: &str) -> (String, Vec<RenderLink>) {
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

/// The longest byte span a link candidate may cover. A candidate that needs more
/// stays literal text, so a malformed line costs a bounded scan per delimiter
/// instead of a rescan of the whole remaining line.
const LINK_SPAN_MAX: usize = 4096;

/// The prefix of `text` a link candidate may examine, cut on a character boundary.
fn link_window(text: &str) -> &str {
    let mut end = text.len().min(LINK_SPAN_MAX);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn osc8_link(text: &str) -> Option<(usize, &str, &str)> {
    let prefix = "\u{1b}]8;;";
    let terminator = "\u{1b}\\";
    let close = "\u{1b}]8;;\u{1b}\\";
    let rest = link_window(text).strip_prefix(prefix)?;
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
    let text = link_window(text).strip_prefix('[')?;
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
    let window = link_window(text);
    if !window.starts_with("file://") {
        return None;
    }
    let end = match window
        .char_indices()
        .find(|(_, character)| character.is_whitespace() || character.is_control())
    {
        Some((index, _)) => index,
        None if window.len() == text.len() => text.len(),
        None => return None,
    };
    let candidate = window[..end].trim_end_matches(['.', ',', ';', ':', '!', '?']);
    if candidate.is_empty() || !valid_link_url(candidate) {
        return None;
    }
    Some((candidate.len(), candidate.to_owned()))
}

/// Reports whether `url` names a character a terminal could read as a
/// control sequence: a C0/C1 code or DEL.
fn has_terminal_control(url: &str) -> bool {
    url.chars().any(char::is_control)
}

fn valid_link_url(url: &str) -> bool {
    !has_terminal_control(url)
        && url
            .strip_prefix("file://")
            .is_some_and(|path| std::path::Path::new(path).is_absolute())
}

/// Reports whether `url` is an absolute http(s) URL safe to open in the
/// desktop browser and to emit as a terminal hyperlink: the scheme is http
/// or https, and no character is a terminal control or whitespace.
pub(crate) fn valid_http_url(url: &str) -> bool {
    (url.starts_with("https://") || url.starts_with("http://"))
        && !url
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
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
            character if character.is_control() || crate::width::is_bidi_control(character) => {
                use std::fmt::Write as _;
                let _ = write!(escaped, "\\u{{{:x}}}", u32::from(character));
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

/// Wraps `text` as written; no markdown is read.
pub(crate) fn prose_rows(text: &str, cap: usize, mode: WidthMode) -> Vec<RenderRow> {
    let mut rows = Vec::new();
    for logical in text.split('\n') {
        let (text, links) = linked_text(logical);
        let line = RichLine {
            text,
            links,
            spans: Vec::new(),
        };
        rows.extend(wrap_rich_line(&line, cap, mode));
    }
    if rows.is_empty() {
        rows.push(RenderRow::new(String::new(), Role::Text));
    }
    rows
}

pub(crate) fn wrap_rich_line(line: &RichLine, cap: usize, mode: WidthMode) -> Vec<RenderRow> {
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
    let spans = line
        .spans
        .iter()
        .filter_map(|span| {
            let span_start = span.range.start.max(start);
            let span_end = span.range.end.min(end);
            (span_start < span_end).then(|| RenderSpan {
                range: span_start - start..span_end - start,
                ..span.clone()
            })
        })
        .collect();
    let mut row = RenderRow::plain(line.text[start..end].to_owned(), Role::Text);
    row.links = links;
    row.spans = spans;
    row
}

/// How text that is not a diagram becomes rows.
#[derive(Clone, Copy)]
pub(crate) enum Prose {
    /// Shown exactly as written, wrapped at the cap.
    Verbatim,
    /// Markdown: prose wraps at the cap; code blocks and tables wrap at `full` cells.
    Markdown { full: usize },
}

pub(crate) fn text_rows(
    text: &str,
    cap: usize,
    mode: WidthMode,
    prose: Prose,
    settings: DiagramSettings,
    cache: &RenderCache,
) -> Vec<RenderRow> {
    let Some(wired) = wire_block(&settings, cache, text, cap) else {
        return match prose {
            Prose::Verbatim => prose_rows(text, cap, mode),
            Prose::Markdown { full } => crate::markdown::markdown_rows(text, cap, full, mode),
        };
    };
    wired_rows(wired, cap, mode)
}

/// The rows of a rendered diagram block: art, a failure card over its source, an
/// image card, or the pending placeholder.
fn wired_rows(wired: WiredBlock, cap: usize, mode: WidthMode) -> Vec<RenderRow> {
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
                bold: false,
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

pub(crate) fn indent_row(mut row: RenderRow, prefix: &str) -> RenderRow {
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
    fn the_hint_row_folds_by_the_deck_priority() {
        use crate::copy::ids;
        use crate::width::WidthMode;

        // Kitty keys confirmed and wide enough: the kitty column.
        assert_eq!(
            super::fold_hint(super::hint_entries(true), 80, WidthMode::Narrow),
            ids::HINT_IDLE
        );
        // Without kitty keys the hint never promises shift+enter.
        assert_eq!(
            super::fold_hint(super::hint_entries(false), 80, WidthMode::Narrow),
            ids::HINT_IDLE_LEGACY
        );
        // The newline key degrades to the working key before entries drop.
        assert_eq!(
            super::fold_hint(super::hint_entries(true), 54, WidthMode::Narrow),
            ids::HINT_IDLE_LEGACY
        );
        // Then help folds first, per the deck fold priority.
        assert_eq!(
            super::fold_hint(super::hint_entries(true), 44, WidthMode::Narrow),
            "enter send · ctrl+j newline · esc interrupt"
        );
        assert_eq!(
            super::fold_hint(super::hint_entries(false), 21, WidthMode::Narrow),
            ids::HINT_SEND
        );
        // Ambiguous glyphs count wide in CJK mode.
        assert_eq!(
            super::fold_hint(super::hint_entries(true), 56, WidthMode::Cjk),
            ids::HINT_IDLE_LEGACY
        );
    }

    #[test]
    fn hint_fold_priority() {
        use crate::copy::ids;
        use crate::width::WidthMode;

        // T-15: fullscreen, detached scroll, width 40 — the state cue
        // replaces the hint entirely and navigation keys are gone.
        let mut detached = super::hint_entries(false);
        detached.insert(0, ids::FOLLOW_STOPPED);
        detached.push(ids::HINT_TRANSCRIPT);
        let detached = super::fold_hint(detached, 40, WidthMode::Narrow);
        assert_eq!(detached, ids::FOLLOW_STOPPED);
        assert!(!detached.contains("pgup"), "{detached}");
        // Attached at the same width, navigation keys still fold first.
        let mut attached = super::hint_entries(false);
        attached.push(ids::HINT_TRANSCRIPT);
        let attached = super::fold_hint(attached, 40, WidthMode::Narrow);
        assert_eq!(attached, "enter send · esc interrupt");
        // Wide rows keep the cue ahead of every deck entry.
        let mut wide = super::hint_entries(false);
        wide.insert(0, ids::FOLLOW_STOPPED);
        wide.push(ids::HINT_TRANSCRIPT);
        let wide = super::fold_hint(wide, 140, WidthMode::Narrow);
        assert!(wide.starts_with(ids::FOLLOW_STOPPED), "{wide}");
        assert!(wide.ends_with(ids::HINT_TRANSCRIPT), "{wide}");
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
            super::Prose::Verbatim,
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

    #[test]
    fn a_million_unmatched_brackets_stay_literal() {
        let text = "[".repeat(1_000_000);
        let (visible, links) = super::linked_text(&text);
        assert_eq!(visible, text);
        assert_eq!(links.len(), 0);
    }

    #[test]
    fn a_link_stays_whole_below_the_candidate_cap_and_literal_above_it() {
        let fits = format!("[x](file:///{})", "a".repeat(super::LINK_SPAN_MAX - 20));
        let (visible, links) = super::linked_text(&fits);
        assert_eq!(visible, "x");
        assert_eq!(links.len(), 1);

        let over = format!("[x](file:///{})", "a".repeat(super::LINK_SPAN_MAX));
        let (visible, links) = super::linked_text(&over);
        assert_eq!(visible, over);
        assert_eq!(links.len(), 0);
    }

    #[test]
    fn a_plain_url_cut_by_the_cap_is_not_linked_as_a_prefix() {
        let over = format!("file:///{}", "a".repeat(super::LINK_SPAN_MAX));
        let (visible, links) = super::linked_text(&over);
        assert_eq!(visible, over);
        assert_eq!(links.len(), 0);
    }

    #[test]
    fn bidi_override_and_isolate_payloads_stay_visible_in_logical_order() {
        let rows = super::prose_rows(
            "run: mv a b \u{202e}evil\u{202c} \u{2066}ok\u{2069}\u{61c}\u{200e}\u{200f}\u{202a}\u{202b}\u{202d}",
            80,
            WidthMode::Narrow,
        );
        let text: String = rows.iter().map(|row| row.text.as_str()).collect();
        assert_eq!(
            text,
            "run: mv a b \\u{202e}evil\\u{202c} \\u{2066}ok\\u{2069}\\u{61c}\\u{200e}\\u{200f}\\u{202a}\\u{202b}\\u{202d}"
        );
        assert!(
            !text.chars().any(crate::width::is_bidi_control),
            "no bidi control survives: {text}"
        );
    }

    #[test]
    fn bidi_payloads_wrap_by_their_visible_escape_width() {
        let rows = super::prose_rows("pad pad \u{202e}x", 12, WidthMode::Narrow);
        let text: String = rows.iter().map(|row| row.text.as_str()).collect();
        assert!(
            rows.iter()
                .all(|row| crate::width::width(&row.text, WidthMode::Narrow) <= 12),
            "every row fits the cap: {text}"
        );
        assert_eq!(text, "pad pad \\u{202e}x");
        assert!(rows.len() > 1, "the eight-cell escape wraps past the cap");
    }

    #[test]
    fn arabic_and_hebrew_prose_render_untouched() {
        let rows = super::prose_rows("مرحبا שלום", 80, WidthMode::Narrow);
        assert_eq!(rows[0].text, "مرحبا שלום");
    }
}
