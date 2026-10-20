//! Pure projection from a session view and live front-end state to terminal rows.

use std::fmt::Write as _;
use std::ops::Range;
use std::sync::Arc;

use dal_core::{BlobId, Block, EntryId, EntryKind, EntryView, JournalPart, TurnState, View};

use crate::diagram::{ArtRole, DiagramKind, DiagramSettings, RenderCache, WiredBlock, wire_block};
use crate::dialog::{DialogUi, DraftChoice};
use crate::frame::{RegionBudget, RegionRequest};
use crate::live::{BlobRead, ExpandedCard, ExpandedView, Live};
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
    /// The discard-draft question's focused verb when it owns the composer
    /// and hint rows; `None` when no draft-confirm is open.
    pub(crate) exit_draft: Option<DraftChoice>,
    /// The checked-out git branch of the workspace, when it has one.
    pub(crate) branch: Option<&'a str>,
    pub(crate) popup: &'a [String],
    pub(crate) live: &'a Live,
    pub(crate) dialog: &'a DialogUi,
    pub(crate) picker: Option<&'a PickerUi>,
    /// The open keyboard-help overlay, which owns the composer and hint rows.
    pub(crate) help: Option<&'a crate::help::HelpUi>,
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
    /// Byte ranges painted with the theme's selection style (reverse video).
    pub(crate) selected: Vec<Range<usize>>,
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
            selected: Vec::new(),
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
        self.selected.retain_mut(|range| {
            if range.start >= end {
                return false;
            }
            range.end = range.end.min(end);
            true
        });
        self
    }
}

/// True while the surface is below the usable floor: the frame shows only
/// the narrow warning plus status and input admission must fail closed.
/// Mirrors the `RegionBudget::floor` thresholds (`h < 8` or `w < 12`) so
/// rendering and admission share one predicate.
pub(crate) fn below_floor(width: u16, height: u16) -> bool {
    height < crate::copy::ids::MIN_ROWS || width < crate::copy::ids::MIN_COLS
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
        .map(|model| match model {
            dal_core::ModelRoute::Api {
                provider: Some(provider),
                model: native,
                ..
            } => {
                let label = format!("{provider}/{native}");
                match dal_core::terminal::escape(&label) {
                    std::borrow::Cow::Borrowed(_) => label,
                    std::borrow::Cow::Owned(escaped) => escaped,
                }
            }
            other => escape(other.id()),
        })
        .or_else(|| input.opts.default_model.as_deref().map(escape));
    let status = status_line(&input, model.as_deref(), w, mode);
    let floor = (height < crate::copy::ids::MIN_ROWS)
        .then_some(crate::copy::ids::NARROW_ROWS)
        .or((width < crate::copy::ids::MIN_COLS).then_some(crate::copy::ids::NARROW_COLS));
    debug_assert_eq!(floor.is_some(), below_floor(width, height));
    if let Some(warning) = floor {
        return resolve_colors(
            vec![
                RenderRow::new(warning, Role::Warning).clipped(w, mode),
                status.clipped(w, mode),
            ],
            input.theme,
        );
    }

    let assistant = live_assistant_rows(&input, w, mode, h);
    let assistant_len = assistant.len();
    let activity = activity_rows(&input, assistant);
    let notices = input.live.notices();
    let armed_delete = armed_delete_line(input.picker);
    let overlay = overlay_height(&input, w, mode);
    let laid = composer_layout(&input, w, mode);
    let budget = RegionBudget::allocate(
        width,
        height,
        RegionRequest {
            notices: notices.len() + usize::from(armed_delete.is_some()),
            activity: usize::MAX,
            composer: laid.rows.len(),
            hint: true,
            overlay,
        },
    );
    let activity = stage_activity_rows(
        &input,
        activity,
        assistant_len,
        budget.activity,
        overlay.is_some(),
    );
    let mut bottom = Vec::with_capacity(budget.cap);
    let notice_rows = notice_rows(notices, armed_delete.as_deref(), budget.notices, w, mode);
    if input.screen == Screen::Inline {
        // The inline live block opens with its notices.
        bottom.extend(notice_rows.iter().cloned());
    }
    bottom.extend(activity.into_iter().rev().take(budget.activity).rev());
    overlay_rows(&input, &budget, &laid, w, mode, &mut bottom);
    bottom.push(status);
    let bottom: Vec<RenderRow> = bottom.into_iter().map(|row| row.clipped(w, mode)).collect();
    let rows = if input.screen == Screen::Fullscreen && input.live.expanded().is_some() {
        expand_view_rows(
            &input,
            notice_rows,
            bottom,
            model.as_deref(),
            width,
            height,
            mode,
        )
    } else if input.screen == Screen::Inline {
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

/// Splices extension rows ahead of the assistant tail, appends popup rows when
/// no overlay owns them, and paints fullscreen selection state. Returns the
/// staged rows.
fn stage_activity_rows(
    input: &FrameInput<'_>,
    mut activity: Vec<RenderRow>,
    assistant_len: usize,
    activity_budget: usize,
    has_overlay: bool,
) -> Vec<RenderRow> {
    let popup_len = if has_overlay { 0 } else { input.popup.len() };
    let occupied = activity
        .len()
        .saturating_add(popup_len)
        .min(activity_budget);
    let ext_allowance = activity_budget.saturating_sub(occupied).min(4);
    let ext_rows: Vec<RenderRow> = input
        .live
        .ext_lines(ext_allowance)
        .into_iter()
        .map(|row| RenderRow::new(row, Role::Dim))
        .collect();
    // The assistant rows sit at the tail of the activity rows; the front
    // splice shifts them by the ext row count.
    let assistant_start = activity.len() - assistant_len + ext_rows.len();
    activity.splice(..0, ext_rows);
    if !has_overlay {
        activity.extend(
            input
                .popup
                .iter()
                .map(|row| RenderRow::new(row.clone(), Role::Dim)),
        );
    }
    paint_fullscreen_selection(
        input,
        &mut activity,
        assistant_start,
        assistant_len,
        activity_budget,
    );
    activity
}

fn paint_fullscreen_selection(
    input: &FrameInput<'_>,
    activity: &mut [RenderRow],
    assistant_start: usize,
    assistant_len: usize,
    activity_budget: usize,
) {
    if input.screen != Screen::Fullscreen || input.live.expanded().is_some() {
        return;
    }
    let kept_from = activity.len().saturating_sub(activity_budget);
    let visible_to = (assistant_start + assistant_len).min(activity.len());
    let visible_from = assistant_start.max(kept_from).min(visible_to);
    let committed_len = input.transcript.rows().len();
    let live: Vec<Box<str>> = activity[visible_from..visible_to]
        .iter()
        .map(|row| row.text.clone().into_boxed_str())
        .collect();
    input.viewport.update_space(
        crate::screen::fullscreen::SelectableSpace::new(committed_len, live),
        input.transcript,
    );
    let selection = input.viewport.selection();
    let first_live = visible_from - assistant_start;
    for (offset, row) in activity[visible_from..visible_to].iter_mut().enumerate() {
        if let Some(range) = selection
            .and_then(|sel| sel.row_span(committed_len + first_live + offset, row.text.len()))
        {
            row.selected.push(range);
        }
    }
}

/// The rows the interactive tail takes in place of the composer, if any.
fn overlay_height(input: &FrameInput<'_>, w: usize, mode: WidthMode) -> Option<usize> {
    if input.dialog.is_open() {
        // Six content rows plus the DESIGN dialog boundary's two blanks.
        Some(8)
    } else if input.screen == Screen::Inline
        && let Some(view) = input.live.expanded()
    {
        // The expanded card owns the composer and hint rows, like a dialog.
        // It requests its measured natural height so live activity rows keep
        // the remainder; the first frame, unmeasured, takes the full
        // allowance and reports the real count back.
        Some(view.measured_rows().unwrap_or(usize::MAX))
    } else if let Some(signin) = input.signin {
        Some(signin.rows(w, mode, usize::MAX).len())
    } else if let Some(picker) = input.picker {
        Some(picker.visible_len().max(1) + 2)
    } else if input.exit_draft.is_some() {
        Some(1)
    } else {
        input.help.map(crate::help::HelpUi::height)
    }
}

fn armed_delete_line(picker: Option<&PickerUi>) -> Option<String> {
    let id = picker?.armed_delete()?;
    Some(crate::copy::render(
        crate::copy::ids::RESUME_ARMED,
        &[("id", &id.to_string())],
        1,
    ))
}

/// The newest `count` notices as clipped rows, each in the role its
/// severity chose at push time so error notices keep their error role.
fn notice_rows(
    notices: &[crate::live::NoticeRow],
    armed_delete: Option<&str>,
    count: usize,
    w: usize,
    mode: WidthMode,
) -> Vec<RenderRow> {
    let armed = armed_delete.map(|text| RenderRow::new(escape(text), Role::Error));
    let recorded = notices
        .iter()
        .rev()
        .map(|notice| RenderRow::new(escape(&notice.text), notice.role));
    let mut rows: Vec<RenderRow> = armed
        .into_iter()
        .chain(recorded)
        .take(count)
        .map(|row| row.clipped(w, mode))
        .collect();
    rows.reverse();
    rows
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

/// The expanded card's overlay window: the whole card when it fits, else the
/// window with one slot kept for the scroll affordance. The view observes the
/// painted window so its page size and scroll bounds match what is painted.
fn expanded_overlay_rows(
    input: &FrameInput<'_>,
    view: &ExpandedView,
    overlay_budget: usize,
    w: usize,
    mode: WidthMode,
) -> Vec<RenderRow> {
    let cap = prose_cap(w).saturating_sub(2).max(1);
    let mut rows = expanded_rows(input, view, cap, mode);
    rows = rows.into_iter().map(|row| row.clipped(w, mode)).collect();
    if rows.len() <= overlay_budget {
        // The whole card fits: no truncation cue, plain window.
        view.observe(overlay_budget, rows.len());
        rows.into_iter()
            .skip(view.top())
            .take(overlay_budget)
            .collect()
    } else {
        // The window keeps one slot for the scroll affordance, and the
        // view observes that smaller window so its page size and scroll
        // bounds match what is painted.
        let shown = overlay_budget.saturating_sub(1).max(1);
        view.observe(shown, rows.len());
        let top = view.top();
        let hidden_below = rows.len().saturating_sub(top + shown);
        let mut out: Vec<RenderRow> = rows.into_iter().skip(top).take(shown).collect();
        let n = u64::try_from(hidden_below).unwrap_or(u64::MAX);
        out.push(
            RenderRow::new(
                crate::copy::render(
                    crate::copy::ids::DIALOG_BODY_MORE,
                    &[("n", &n.to_string())],
                    n,
                ),
                Role::Dim,
            )
            .clipped(w, mode),
        );
        out
    }
}

/// The picker's overlay rows: title, the no-match fallback or visible
/// options, then the hint. The no-match `/model {id}` fallback is a
/// selectable row like the options: it needs the same `>` focus marker, not
/// just a color.
///
/// Focus is reverse video plus the `>` marker, never color alone (DESIGN.md
/// Focus and overlays / Accessibility): the focused row selects its whole
/// text, so `NO_COLOR` still paints the theme's reversed selection style.
///
/// The window never exceeds `overlay_budget`, the sibling arms' contract:
/// title plus hint keep one row each and the middle keeps a selection-
/// centered window of the remainder, so every option stays reachable
/// through the arrows at shrunk sizes and the status row keeps the last row.
fn picker_overlay_rows(picker: &PickerUi, overlay_budget: usize) -> Vec<RenderRow> {
    let title = RenderRow::new(picker.title.clone(), Role::Accent);
    let hint = RenderRow::new(
        if picker.is_settings() {
            crate::copy::ids::SETTINGS_HINT
        } else if picker.is_resume_picker() {
            // The resume picker is the only one with a delete key, so it is
            // the only one whose hint names it.
            crate::copy::ids::RESUME_HINT
        } else {
            crate::copy::ids::PICKER_HINT
        },
        Role::Dim,
    );
    let option_row = |text: String, selected: bool| {
        let mut row = RenderRow::new(text, if selected { Role::Accent } else { Role::Dim });
        if selected {
            row.selected.push(0..row.text.len());
        }
        row
    };
    let mut middle: Vec<RenderRow> = Vec::new();
    let mut selected_pos = 0;
    if let Some(id) = picker.model_filter_fallback() {
        let selected = picker.is_selected(picker.selected_fallback_index());
        middle.push(option_row(
            format!("{} /model {}", if selected { ">" } else { " " }, escape(id)),
            selected,
        ));
    } else {
        let range = picker.visible_range();
        if picker.is_empty() {
            middle.push(RenderRow::new(crate::copy::ids::PICKER_EMPTY, Role::Dim));
        }
        for index in range {
            let Some(option) = picker.visible_option(index) else {
                continue;
            };
            let selected = picker.is_selected(index);
            if selected {
                selected_pos = middle.len();
            }
            // Multi-select rows carry the check marker the request dialogs
            // use ([x]/[ ]); single-select rows keep the plain gutter.
            let check = if picker.is_multi() {
                if picker.is_checked(index) {
                    "[x] "
                } else {
                    "[ ] "
                }
            } else {
                ""
            };
            middle.push(option_row(
                format!(
                    "{} {check}{}",
                    if selected { ">" } else { " " },
                    escape(&option.label)
                ),
                selected,
            ));
        }
    }
    if overlay_budget == 0 {
        return Vec::new();
    }
    if middle.len() + 2 <= overlay_budget {
        let mut rows = Vec::with_capacity(middle.len() + 2);
        rows.push(title);
        rows.extend(middle);
        rows.push(hint);
        return rows;
    }
    if overlay_budget == 1 {
        return vec![title];
    }
    if overlay_budget == 2 {
        return vec![title, hint];
    }
    let allowed = overlay_budget.saturating_sub(2).max(1).min(middle.len());
    let start = selected_pos
        .saturating_sub(allowed / 2)
        .min(middle.len().saturating_sub(allowed));
    let mut rows = Vec::with_capacity(overlay_budget);
    rows.push(title);
    rows.extend(middle.into_iter().skip(start).take(allowed));
    rows.push(hint);
    rows
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
        // DESIGN.md dialog boundary: one blank row above and below the
        // dialog. The blanks come out of the overlay budget — rows outside
        // it would overflow the region — so they only paint once the budget
        // still leaves the dialog three content rows: its title, the row
        // Enter answers, and its actions. Below that the content rows win
        // over the chrome.
        let bordered = budget.overlay >= 5;
        let mut rows = input.dialog.rendered_rows(
            w,
            if bordered {
                budget.overlay - 2
            } else {
                budget.overlay
            },
            mode,
            input.diagram_settings,
            input.diagram_cache,
            input.kitty_keyboard,
        );
        if bordered {
            rows.insert(0, RenderRow::plain(String::new(), Role::Text));
            rows.push(RenderRow::plain(String::new(), Role::Text));
        }
        bottom.extend(rows);
    } else if input.screen == Screen::Inline
        && let Some(view) = input.live.expanded()
    {
        // The expand view takes the overlay rows a dialog or picker would
        // take: the card's `budget.overlay` rows, scrolled by the view. Rows
        // are never capped here — the window plus the view's scroll keys are
        // the named expand path, so every row stays reachable. When content
        // exceeds the window the last slot carries the scroll affordance
        // (the dialog footer's `... {n} more lines · pgdn`), and the view
        // observes the one-row-smaller window so page size and scroll bounds
        // match what is painted.
        bottom.extend(expanded_overlay_rows(input, view, budget.overlay, w, mode));
    } else if let Some(signin) = input.signin {
        bottom.extend(signin.rows(w, mode, budget.overlay));
    } else if let Some(picker) = input.picker {
        bottom.extend(picker_overlay_rows(picker, budget.overlay));
    } else if let Some(choice) = input.exit_draft {
        // The focused verb paints the selection range and leads the row, so
        // a narrow frame can never clip the verb Enter activates. The
        // bracketed key cue is the same token the confirm dialogs carry.
        let discard = format!("[y] {}", crate::copy::ids::EXIT_DRAFT_TITLE);
        let keep = format!("[n] {}", crate::copy::ids::EXIT_KEEP_EDITING);
        let (focused, other) = match choice {
            DraftChoice::Discard => (discard, keep),
            DraftChoice::Keep => (keep, discard),
        };
        let mut row = RenderRow::plain(format!("{focused}   {other}"), Role::Accent);
        row.selected.push(0..focused.len());
        bottom.push(row);
    } else if let Some(help) = input.help {
        bottom.extend(help.rows(budget.overlay));
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
    let selection = input.viewport.selection();
    rows.extend((window_top..window_end).filter_map(|index| {
        let mut row = input.transcript.render_row(index)?;
        if let Some(range) =
            selection.and_then(|selection| selection.row_span(index, row.text.len()))
        {
            row.selected.push(range);
        }
        Some(row)
    }));
    rows.resize_with(above + available, || {
        RenderRow::new(String::new(), Role::Text)
    });
    rows.extend(bottom);
    rows.into_iter().map(|row| row.clipped(w, mode)).collect()
}

/// The card an entry can expand to, when one exists. The scan keeps the last
/// eligible block of an assistant entry, matching the newest-first rule the
/// expand key resolves over.
pub(crate) fn expanded_card(entry: &EntryView, diagrams: DiagramSettings) -> Option<ExpandedCard> {
    let assistant_card = |block, kind: &Block| match kind {
        Block::Reasoning { text, .. } if !text.trim().is_empty() => {
            Some(ExpandedCard::Reasoning { entry: entry.id })
        }
        Block::Text { text, .. } if diagrams.enabled => {
            // The newest eligible completed fence wins: `fence` indexes
            // `fence_spans` over all completed fences, a deterministic scan
            // that stays fixed under append-only text growth.
            let fence = crate::markdown::fence_spans(text)
                .iter()
                .enumerate()
                .rev()
                .find_map(|(index, span)| {
                    (span.routable && DiagramKind::parse(&text[span.info.clone()]).is_some())
                        .then_some(index)
                });
            fence.map(|fence| ExpandedCard::Diagram {
                entry: entry.id,
                block,
                fence,
            })
        }
        _ => None,
    };
    match &entry.kind {
        EntryKind::ToolResult { .. } => Some(ExpandedCard::Tool { entry: entry.id }),
        EntryKind::Assistant { content, .. } => content
            .iter()
            .enumerate()
            .rev()
            .find_map(|(block, kind)| assistant_card(block, kind)),
        _ => None,
    }
}

/// The row a card resolves to when its content can no longer be located in
/// the visible history — the entry left the branch, or the fence ordinal no
/// longer parses against the committed text.
fn expanded_missing_row() -> Vec<RenderRow> {
    vec![RenderRow::new(
        "the expanded card is no longer in the visible history",
        Role::Dim,
    )]
}

/// All rendered rows of the open card, in full on every surface: the painted
/// window plus the view's scroll keys are the named expand path DESIGN
/// requires, so no row is ever dropped behind a truncation footer.
fn expanded_rows(
    input: &FrameInput<'_>,
    expanded: &ExpandedView,
    cap: usize,
    mode: WidthMode,
) -> Vec<RenderRow> {
    let Some(entry) = input
        .view
        .entries
        .items
        .iter()
        .find(|entry| expanded_entry_id(expanded.card) == entry.id)
    else {
        // The entry left the branch while the view was open; the card's
        // header row stays in scrollback, so the view shows the absence.
        return expanded_missing_row();
    };
    match expanded.card {
        ExpandedCard::Reasoning { .. } => expanded_reasoning_rows(entry, cap, mode),
        ExpandedCard::Tool { .. } => expanded_tool_rows(input, expanded, entry, cap, mode),
        ExpandedCard::Diagram { block, fence, .. } => {
            expanded_diagram_rows(entry, block, fence, cap, mode)
        }
    }
}

/// The `TextBlob` digests the card resolves through `TuiAgent::blob` when the
/// view opens. Only tool cards carry stored parts; each digest is requested
/// once. A digest that fails to parse is skipped: the row falls back to the
/// storage stub instead of waiting on a read that can never start.
pub(crate) fn expanded_blob_ids(entry: &EntryView, card: ExpandedCard) -> Vec<BlobId> {
    let ExpandedCard::Tool { .. } = card else {
        return Vec::new();
    };
    let EntryKind::ToolResult { parts, .. } = &entry.kind else {
        return Vec::new();
    };
    let mut ids: Vec<BlobId> = parts
        .iter()
        .filter_map(|part| match part {
            JournalPart::TextBlob { blob, .. } => BlobId::parse(blob).ok(),
            _ => None,
        })
        .collect();
    ids.sort_unstable_by_key(|id| *id.as_bytes());
    ids.dedup();
    ids
}

fn expanded_entry_id(card: ExpandedCard) -> EntryId {
    match card {
        ExpandedCard::Reasoning { entry }
        | ExpandedCard::Tool { entry }
        | ExpandedCard::Diagram { entry, .. } => entry,
    }
}

/// `Thinking · {n} steps · ctrl+o to hide` plus every reasoning row in dim.
/// A long single step still wraps to its full row count — the view window
/// scrolls, so no step is dropped and no step loses its first rows.
fn expanded_reasoning_rows(entry: &EntryView, cap: usize, mode: WidthMode) -> Vec<RenderRow> {
    let EntryKind::Assistant { content, .. } = &entry.kind else {
        return Vec::new();
    };
    let steps: Vec<&str> = content
        .iter()
        .filter_map(|block| match block {
            Block::Reasoning { text, .. } => Some(text.as_ref()),
            _ => None,
        })
        .flat_map(str::lines)
        .filter(|line| !line.trim().is_empty())
        .collect();
    // Reasoning blocks carry no timing, so the duration field stays empty and
    // the double separator collapses.
    let header = crate::copy::render(
        crate::copy::ids::THINKING_OPEN,
        &[("n", &steps.len().to_string()), ("dur", "")],
        u64::try_from(steps.len()).unwrap_or(u64::MAX),
    )
    .replace(" ·  · ", " · ");
    let mut rows = vec![gutter_row(RenderRow::new(header, Role::Dim))];
    for step in &steps {
        rows.extend(dim_prose_rows(step, cap, mode));
    }
    rows
}

/// Guttered dim rows of verbatim text, the role DESIGN assigns expanded
/// reasoning steps.
fn dim_prose_rows(text: &str, cap: usize, mode: WidthMode) -> Vec<RenderRow> {
    prose_rows(text, cap, mode)
        .into_iter()
        .map(|mut row| {
            row.role = Role::Dim;
            gutter_row(row)
        })
        .collect()
}

/// The settled header, then the result's stored text and image notes.
/// `TextBlob` parts resolve through the reads the view opened with; while a
/// read is pending or after it fails the row names the blob honestly instead
/// of dropping the content. `Blob`/`ImageBlob` parts keep their non-text
/// stub: the TUI never decodes a PDF or image payload into text rows.
fn expanded_tool_rows(
    input: &FrameInput<'_>,
    expanded: &ExpandedView,
    entry: &EntryView,
    cap: usize,
    mode: WidthMode,
) -> Vec<RenderRow> {
    let EntryKind::ToolResult {
        call,
        name,
        error,
        parts,
        elapsed_ms,
        ..
    } = &entry.kind
    else {
        return Vec::new();
    };
    let duration = input
        .transcript
        .tool_duration(call.as_str())
        .or_else(|| elapsed_ms.map(std::time::Duration::from_millis));
    let summary = parts
        .iter()
        .find_map(|part| match part {
            JournalPart::Text { text } => Some(text.as_ref()),
            _ => None,
        })
        .unwrap_or("");
    let mut header = tool_card_row(name, *error, summary, duration);
    let _ = write!(header.text, " · {}", crate::copy::ids::TOOL_COLLAPSE);
    let mut rows = vec![gutter_row(header)];
    for part in parts {
        match part {
            JournalPart::Text { text } => {
                rows.extend(prose_rows(text, cap, mode).into_iter().map(gutter_row));
            }
            JournalPart::Image { .. } | JournalPart::ImageBlob { .. } => {
                rows.push(gutter_row(RenderRow::new(
                    crate::copy::ids::IMAGE_OPEN,
                    Role::Dim,
                )));
            }
            JournalPart::TextBlob { blob, bytes } => {
                rows.extend(text_blob_rows(expanded, blob, *bytes, cap, mode));
            }
            JournalPart::Blob { mime, blob, bytes } => {
                rows.push(gutter_row(RenderRow::new(
                    format!("[{bytes} bytes of {mime} stored in blob {blob}]"),
                    Role::Dim,
                )));
            }
        }
    }
    rows
}

/// One `TextBlob` part's rows: the decoded text once the view's read lands,
/// the read's own error text when it fails, or an honest in-flight row.
fn text_blob_rows(
    expanded: &ExpandedView,
    blob: &str,
    bytes: u64,
    cap: usize,
    mode: WidthMode,
) -> Vec<RenderRow> {
    let read = BlobId::parse(blob).ok().and_then(|id| expanded.blob(&id));
    let row = |text: String| gutter_row(RenderRow::new(text, Role::Dim));
    match read {
        Some(BlobRead::Text(text)) => prose_rows(&text, cap, mode)
            .into_iter()
            .map(gutter_row)
            .collect(),
        Some(BlobRead::Failed(reason)) => vec![row(format!(
            "[{bytes} bytes stored in blob {blob} · {}]",
            escape(&reason.replace('\n', " "))
        ))],
        Some(BlobRead::Pending) => {
            vec![row(format!("[reading {bytes} bytes from blob {blob}]"))]
        }
        // No read was requested (an unparseable digest): the storage stub.
        None => vec![row(format!("[{bytes} bytes stored in blob {blob}]"))],
    }
}

/// `diagram {kind} · {n} rows · ctrl+o collapses` over the fenced source,
/// every row on every surface; the view window scrolls through them.
/// `fence` re-resolves through the same `fence_spans` scan `expanded_card`
/// ran; a committed text is frozen, but a mid-stream shrink of the visible
/// entries still degrades to the absence row instead of clipping wrong.
fn expanded_diagram_rows(
    entry: &EntryView,
    block: usize,
    fence: usize,
    cap: usize,
    mode: WidthMode,
) -> Vec<RenderRow> {
    let EntryKind::Assistant { content, .. } = &entry.kind else {
        return expanded_missing_row();
    };
    let Some((kind, source)) = content.get(block).and_then(|block| match block {
        Block::Text { text, .. } => {
            let spans = crate::markdown::fence_spans(text);
            let span = spans.get(fence)?;
            let kind = span
                .routable
                .then(|| DiagramKind::parse(&text[span.info.clone()]))??;
            Some((kind, crate::markdown::fence_body(text, span)))
        }
        _ => None,
    }) else {
        return expanded_missing_row();
    };
    let rows_count = source.lines().count();
    let header = crate::copy::render(
        crate::copy::ids::DIAGRAM_READY_OPEN,
        &[("kind", kind.label()), ("n", &rows_count.to_string())],
        u64::try_from(rows_count).unwrap_or(u64::MAX),
    );
    let mut rows = vec![gutter_row(RenderRow::new(header, Role::Dim))];
    rows.extend(prose_rows(source, cap, mode).into_iter().map(gutter_row));
    rows
}

/// The expand view: the fullscreen skeleton with the card's rows in place of
/// the transcript window, scrolled by the view's own offset. Composer rows
/// paint without a caret since the view owns every key.
fn expand_view_rows(
    input: &FrameInput<'_>,
    notice_rows: Vec<RenderRow>,
    bottom: Vec<RenderRow>,
    model: Option<&str>,
    width: u16,
    height: u16,
    mode: WidthMode,
) -> Vec<RenderRow> {
    let w = usize::from(width);
    let h = usize::from(height);
    let header = crate::screen::fullscreen::header_rows(width, height);
    let above = header + notice_rows.len();
    let available = h.saturating_sub(above + bottom.len());
    let cap = prose_cap(w).saturating_sub(2);
    let Some(view) = input.live.expanded() else {
        return Vec::new();
    };
    let mut content = expanded_rows(input, view, cap, mode);
    content = content
        .into_iter()
        .map(|row| row.clipped(w, mode))
        .collect();
    view.observe(available, content.len());
    let top = view.top();
    let window = content
        .into_iter()
        .skip(top)
        .take(available)
        .collect::<Vec<_>>();
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
    rows.extend(notice_rows);
    rows.extend(window);
    rows.resize_with(above + available, || {
        RenderRow::new(String::new(), Role::Text)
    });
    rows.extend(bottom.into_iter().map(|mut row| {
        row.cursor = None;
        row
    }));
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
    let branch = input.branch.map(escape);
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
            path: Some(&home),
            branch: branch.as_deref(),
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

/// The streamed assistant rows of the live region: the exact rows the
/// fullscreen selection names as its live suffix, so the render and the
/// selection never disagree about the text.
fn live_assistant_rows(
    input: &FrameInput<'_>,
    w: usize,
    mode: WidthMode,
    limit: usize,
) -> Vec<RenderRow> {
    if input.live.assistant_text().is_empty() {
        return Vec::new();
    }
    // The markdown stream wires each completed routable fence itself: the
    // whole-block check is gone, so embedded diagram cards and the pending
    // placeholder reach the live region mid-prose.
    let rows = input.live.assistant_rows(
        prose_cap(w),
        w.saturating_sub(2),
        mode,
        limit,
        Some((&input.diagram_settings, input.diagram_cache)),
    );
    rows.into_iter().map(gutter_row).collect()
}

/// The scrollable middle region: first-run hint, queued steering, tool rows,
/// pending transcript rows, and the streamed assistant rows. Streamed text
/// keeps only its last `limit` rows, the most any frame can show.
fn activity_rows(input: &FrameInput<'_>, assistant: Vec<RenderRow>) -> Vec<RenderRow> {
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
    activity.extend(assistant);
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
            // Only the block `expanded_card` selects may advertise the key.
            // Ctrl+O resolves the newest eligible entry at press time, so a
            // header committed on a now-older entry names the newer card it
            // opens — the review's call-site granularity, not per-entry
            // truth: committed rows freeze.
            let expanded_block = match expanded_card(entry, diagram_settings) {
                Some(ExpandedCard::Diagram { block, .. }) => Some(block),
                _ => None,
            };
            let prose = Prose::Markdown {
                full: usize::from(columns).saturating_sub(2),
            };
            let mut rows = Vec::new();
            for (index, block) in content.iter().enumerate() {
                match block {
                    Block::Text { text, .. } if expanded_block == Some(index) => {
                        rows.extend(expandable_text_rows(
                            text,
                            cap,
                            mode,
                            usize::from(columns).saturating_sub(2),
                            diagram_settings,
                            diagram_cache,
                        ));
                    }
                    Block::Text { text, .. } => {
                        rows.extend(text_rows(
                            text,
                            cap,
                            mode,
                            prose,
                            diagram_settings,
                            diagram_cache,
                        ));
                    }
                    Block::Reasoning { .. } => {
                        rows.push(RenderRow::new(collapsed_thinking_text(), Role::Dim));
                    }
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

#[derive(Debug, Clone)]
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
            // A file URL carries an absolute path by definition; checking
            // Path::is_absolute would reject unix-style paths on Windows.
            .is_some_and(|path| path.starts_with('/'))
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
        dal_core::terminal::push_escaped(&mut escaped, character);
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

/// Wraps one styled line through `width::wrap_ranges`, the same
/// word-boundary policy markdown tables and prose use; each emitted row keeps
/// the spans and links it overlaps.
pub(crate) fn wrap_rich_line(line: &RichLine, cap: usize, mode: WidthMode) -> Vec<RenderRow> {
    crate::width::wrap_ranges(&line.text, cap, mode)
        .into_iter()
        .map(|(start, end)| linked_row(line, start, end))
        .collect()
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
    match prose {
        // The whole-block fast path stays for verbatim user text: a single
        // supported fence still renders straight to its card, and user
        // entries never carry the ctrl+o header because the expand key only
        // resolves committed assistant cards.
        Prose::Verbatim => wire_block(&settings, cache, text, cap).map_or_else(
            || prose_rows(text, cap, mode),
            |(_, wired)| wired_rows(wired, cap, mode),
        ),
        Prose::Markdown { full } => {
            crate::markdown::markdown_wired_rows(text, cap, full, mode, Some((&settings, cache)))
        }
    }
}

/// `text_rows` for a committed assistant block Ctrl+O would open: the ready
/// card of the fence `expanded_card` resolved also carries the DESIGN
/// `diagram {kind} · {n} rows · ctrl+o expands` header at the fence's own
/// position, the promise matching the key that resolves it. User fences,
/// dialog previews and the still-streaming live block use `text_rows`, so
/// they never advertise a key that cannot reach them.
fn expandable_text_rows(
    text: &str,
    cap: usize,
    mode: WidthMode,
    full: usize,
    settings: DiagramSettings,
    cache: &RenderCache,
) -> Vec<RenderRow> {
    let wire = Some((&settings, cache));
    let spans = crate::markdown::fence_spans(text);
    let Some((kind, span)) = spans.iter().rev().find_map(|span| {
        let kind = span
            .routable
            .then(|| DiagramKind::parse(&text[span.info.clone()]))??;
        Some((kind, span))
    }) else {
        return crate::markdown::markdown_wired_rows(text, cap, full, mode, wire);
    };
    // `wire_source` stays the one scheduling rule: the resolved fence wires
    // through the same cache/budget as markdown materialization, and a
    // `Pending` or `Fallback` result needs no expand header. Ready art or
    // pixels get `wired_card_rows` so the header lands exactly at the
    // fence's position inside the surrounding prose.
    let body = crate::markdown::fence_body(text, span);
    let Some(wired) = crate::diagram::wire_source(&settings, cache, kind, body, cap) else {
        return crate::markdown::markdown_wired_rows(text, cap, full, mode, wire);
    };
    if !matches!(wired, WiredBlock::Art(_) | WiredBlock::Pixels(_)) {
        return crate::markdown::markdown_wired_rows(text, cap, full, mode, wire);
    }
    let mut rows = Vec::new();
    if let Some(prefix) = text
        .get(..span.span.start)
        .and_then(|prefix| prefix.strip_suffix('\n'))
        && !prefix.is_empty()
    {
        rows.extend(crate::markdown::markdown_wired_rows(
            prefix, cap, full, mode, wire,
        ));
    }
    rows.extend(wired_card_rows(kind, wired, cap, mode));
    if let Some(suffix) = text.get(span.span.end..)
        && let Some(suffix) = suffix.strip_prefix('\n')
        && !suffix.is_empty()
    {
        rows.extend(crate::markdown::markdown_wired_rows(
            suffix, cap, full, mode, wire,
        ));
    }
    if rows.is_empty() {
        rows.push(RenderRow::plain(String::new(), Role::Text));
    }
    rows
}

/// `wired_rows` plus the settled `ctrl+o expands` header: the card of the
/// exact fence or block Ctrl+O opens. `kind` comes from the caller's scan so
/// the header never re-parses the text the rows were already scheduled from.
fn wired_card_rows(
    kind: DiagramKind,
    wired: WiredBlock,
    cap: usize,
    mode: WidthMode,
) -> Vec<RenderRow> {
    let rows = wired_rows(wired, cap, mode);
    let n = rows.len();
    let header = RenderRow::new(
        crate::copy::render(
            crate::copy::ids::DIAGRAM_READY,
            &[("kind", kind.label()), ("n", &n.to_string())],
            u64::try_from(n).unwrap_or(u64::MAX),
        ),
        Role::Dim,
    );
    let mut card = Vec::with_capacity(n + 1);
    card.push(header);
    card.extend(rows);
    card
}

/// The rows of a rendered diagram block: art, a failure card over its source, an
/// image card, or the pending placeholder.
pub(crate) fn wired_rows(wired: WiredBlock, cap: usize, mode: WidthMode) -> Vec<RenderRow> {
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
                    &[("kind", kind.label())],
                    1,
                ),
                Role::Dim,
            );
            row.pending_diagram = true;
            vec![row]
        }
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
        // DESIGN.md Diagrams: border dim, text text, edge accent, edge label
        // dim, title accent and bold.
        let (role, bold) = match cell.role {
            ArtRole::Text => (Role::Text, false),
            ArtRole::Edge => (Role::Accent, false),
            ArtRole::Border | ArtRole::EdgeLabel => (Role::Dim, false),
            ArtRole::Title => (Role::Accent, true),
        };
        if let Some(span) = row.spans.last_mut()
            && span.role == role
            && span.bold == bold
        {
            span.range.end = row.text.len();
        } else {
            row.spans.push(RenderSpan {
                range: start..row.text.len(),
                role,
                bold,
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

/// The deck's collapsed thinking row. Reasoning blocks carry no timing, so
/// `{dur}` renders empty and its doubled space collapses — the same collapse
/// the expanded header performs for an empty duration.
fn collapsed_thinking_text() -> String {
    crate::copy::render(crate::copy::ids::THINKING_COLLAPSED, &[("dur", "")], 1)
        .replace("  ·", " ·")
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

    use dal_core::{Block, EntryId, EntryKind, EntryView, JournalPart};

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
                .is_some_and(|(_, spans)| !spans.is_empty()),
            "a user card is not Ctrl+O-eligible, so no header pushes the art to the second row"
        );
        assert_eq!(on_cache.renders(), 1);
    }

    #[test]
    fn only_the_committed_assistant_card_advertises_ctrl_o() {
        use dal_core::AssistantStop;

        let fence = "```mermaid\ngraph TD; A-->B\n```";
        let user = EntryView {
            id: EntryId::new(NonZeroU64::MIN),
            parent: None,
            kind: EntryKind::User {
                parts: vec![JournalPart::Text { text: fence.into() }],
            },
        };
        let assistant = EntryView {
            id: EntryId::new(NonZeroU64::new(2).unwrap()),
            parent: None,
            kind: EntryKind::Assistant {
                api: dal_core::Family::Chat,
                model: "model".into(),
                content: vec![Block::Text {
                    text: fence.into(),
                    replay: None,
                }],
                usage: dal_core::Usage {
                    input_tokens: 0,
                    cached_input_tokens: 0,
                    output_tokens: 0,
                    reasoning_tokens: None,
                    cache_write_tokens: 0,
                    premium_requests: None,
                    cost_usd: None,
                },
                stop: AssistantStop::Done,
                route: None,
                replay: None,
            },
        };
        let settings = DiagramSettings { enabled: true };
        let cache = RenderCache::default();
        let hint = "ctrl+o expands";
        assert!(
            !entry_rows(&user, 80, WidthMode::Narrow, settings, &cache)
                .iter()
                .any(|row| row.text.contains(hint)),
            "a user fence card cannot be opened, so it cannot promise ctrl+o"
        );
        assert!(
            entry_rows(&assistant, 80, WidthMode::Narrow, settings, &cache)
                .iter()
                .any(|row| row.text.contains(hint)),
            "the committed assistant card is the one Ctrl+O opens"
        );
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

        // fullscreen, detached scroll, width 40 — the state cue
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
        assert!(text.is_ascii(), "no bidi control survives: {text}");
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

    /// One assistant entry whose text block holds `fences` joined by prose.
    fn assistant_with_fences(fences: &[&str]) -> EntryView {
        use dal_core::AssistantStop;
        EntryView {
            id: EntryId::new(NonZeroU64::MIN),
            parent: None,
            kind: EntryKind::Assistant {
                api: dal_core::Family::Chat,
                model: "model".into(),
                content: vec![Block::Text {
                    text: format!("intro\n{}\noutro", fences.join("\nmiddle\n")).into(),
                    replay: None,
                }],
                usage: dal_core::Usage {
                    input_tokens: 0,
                    cached_input_tokens: 0,
                    output_tokens: 0,
                    reasoning_tokens: None,
                    cache_write_tokens: 0,
                    premium_requests: None,
                    cost_usd: None,
                },
                stop: AssistantStop::Done,
                route: None,
                replay: None,
            },
        }
    }

    #[test]
    fn expansion_targets_the_newest_fence_in_a_multi_fence_block() {
        let entry = assistant_with_fences(&[
            "```mermaid\ngraph TD; A-->B\n```",
            "~~~\nplain code\n~~~",
            "```mermaid\ngraph LR; C-->D\n```",
        ]);
        let card = super::expanded_card(&entry, DiagramSettings { enabled: true });
        // The last eligible completed fence wins: index 2 into all
        // completed fences, skipping the non-routable tilde fence.
        assert!(matches!(
            card,
            Some(super::ExpandedCard::Diagram {
                block: 0,
                fence: 2,
                ..
            })
        ));
        let rows = super::expanded_diagram_rows(&entry, 0, 2, 40, WidthMode::Narrow);
        let texts: Vec<&str> = rows.iter().map(|row| row.text.as_str()).collect();
        assert!(texts.iter().any(|row| row.contains("C-->D")), "{texts:?}");
        assert!(!texts.iter().any(|row| row.contains("A-->B")), "{texts:?}");
        assert!(
            texts[0].contains("diagram mermaid") && texts[0].contains("ctrl+o collapses"),
            "{texts:?}"
        );
        // A fence ordinal that no longer resolves degrades to the absence row.
        let missing = super::expanded_diagram_rows(&entry, 0, 9, 40, WidthMode::Narrow);
        assert_eq!(
            missing[0].text,
            "the expanded card is no longer in the visible history"
        );
    }

    #[test]
    fn an_embedded_ready_fence_carries_the_expand_header_at_its_position() {
        let entry = assistant_with_fences(&["```mermaid\ngraph TD; A-->B\n```"]);
        let rows = entry_rows(
            &entry,
            80,
            WidthMode::Narrow,
            DiagramSettings { enabled: true },
            &RenderCache::default(),
        );
        let texts: Vec<&str> = rows.iter().map(|row| row.text.as_str()).collect();
        let header = texts
            .iter()
            .position(|row| row.contains("ctrl+o expands"))
            .expect("the resolved fence advertises ctrl+o");
        assert_eq!(texts[0], "  intro", "{texts:?}");
        assert!(header > 0, "prose precedes the card header: {texts:?}");
        assert_eq!(texts.last(), Some(&"  outro"), "{texts:?}");
        assert!(
            !texts
                .iter()
                .any(|row| row.contains("```") || row.contains("graph TD")),
            "the fenced source never leaks into transcript rows: {texts:?}"
        );
    }

    /// A `FrameInput` over `live` with one committed `entry` in the view,
    /// optionally carrying `picker` (the transcript overlay picker).
    fn expanded_frame<'a>(
        view: &'a dal_core::View,
        live: &'a crate::live::Live,
        transcript: &'a Transcript,
        viewport: &'a crate::screen::fullscreen::Viewport,
        opts: &'a crate::TuiOptions,
        theme: &'a crate::theme::ResolvedTheme,
        dialog: &'a crate::dialog::DialogUi,
        cache: &'a RenderCache,
        picker: Option<&'a crate::picker::PickerUi>,
    ) -> super::FrameInput<'a> {
        super::FrameInput {
            view,
            screen: crate::Screen::Inline,
            composer: "",
            cursor: 0,
            exit_draft: None,
            branch: None,
            popup: &[],
            live,
            dialog,
            picker,
            help: None,
            signin: None,
            transcript,
            viewport,
            opts,
            kitty_keyboard: false,
            theme,
            diagram_settings: DiagramSettings::default(),
            diagram_cache: cache,
        }
    }

    /// The tool-result entry behind the scroll-path test's paged card: forty
    /// numbered lines, so the first window ends in a scroll affordance.
    fn scroll_path_tool_entry() -> EntryView {
        use dal_core::CallId;

        let lines: Vec<String> = (1..=40).map(|n| format!("line {n:02}")).collect();
        EntryView {
            id: EntryId::new(NonZeroU64::MIN),
            parent: None,
            kind: EntryKind::ToolResult {
                call: CallId::new("call-1"),
                name: "read".into(),
                error: false,
                parts: vec![JournalPart::Text {
                    text: lines.join("\n").into(),
                }],
                changes: Vec::new(),
                elapsed_ms: None,
            },
        }
    }

    /// The small reasoning entry the scroll-path test opens second: it fits
    /// the window, so no truncation footer may paint.
    fn scroll_path_reasoning_entry() -> EntryView {
        EntryView {
            id: EntryId::new(NonZeroU64::new(2).unwrap()),
            parent: None,
            kind: EntryKind::Assistant {
                api: dal_core::Family::Chat,
                model: "model".into(),
                content: vec![Block::Reasoning {
                    text: "step one\nstep two".into(),
                    replay: dal_core::RawJson::null(),
                }],
                usage: dal_core::Usage {
                    input_tokens: 0,
                    cached_input_tokens: 0,
                    output_tokens: 0,
                    reasoning_tokens: None,
                    cache_write_tokens: 0,
                    premium_requests: None,
                    cost_usd: None,
                },
                stop: dal_core::AssistantStop::Done,
                route: None,
                replay: None,
            },
        }
    }

    /// The two-entry idle view behind the scroll-path test.
    fn scroll_path_view(tool_entry: EntryView, reasoning_entry: EntryView) -> dal_core::View {
        use dal_core::{Gen, Seq, SessionId, Timestamp};

        let workspace = dal_core::Workspace::new(std::env::temp_dir()).unwrap();
        dal_core::View {
            r#gen: Gen::new(NonZeroU64::MIN),
            seq: Seq::new(NonZeroU64::MIN),
            session: dal_core::SessionInfo {
                id: SessionId::new_v7(),
                name: None,
                preview: "".into(),
                workspace,
                updated_at: Timestamp::UNIX_EPOCH,
                created_at: Some(Timestamp::UNIX_EPOCH),
                archived: Some(false),
                last_seq: Some(Seq::new(NonZeroU64::MIN)),
                message_count: None,
            },
            turn: dal_core::TurnState::Idle,
            entries: dal_core::Page {
                items: vec![tool_entry, reasoning_entry],
                next_before: None,
            },
            tree: dal_core::TreeOutline {
                branches: Vec::new(),
            },
            settings: dal_core::SettingsView {
                model: None,
                thinking: dal_core::ThinkingLevel::Medium,
                approval: dal_core::ApprovalMode::Ask,
                mode: dal_core::Mode::Normal,
                name: None,
                scoped_models: None,
            },
            open: Vec::new(),
            changes: Vec::new(),
            usage: dal_core::UsageView {
                usage: dal_core::Usage {
                    input_tokens: 0,
                    cached_input_tokens: 0,
                    output_tokens: 0,
                    reasoning_tokens: None,
                    cache_write_tokens: 0,
                    premium_requests: None,
                    cost_usd: None,
                },
                context_tokens: 0,
                context_window: 128_000,
            },
            stats: dal_core::Stats {
                steers_queued: 0,
                follow_ups_queued: 0,
                retries: 0,
                dropped_observations: 0,
                auto_compaction: dal_core::AutoCompaction::Off,
            },
        }
    }

    /// The headless options behind the scroll-path test with the runtime
    /// whose handle they carry; the runtime stays alive beside the options.
    fn scroll_path_opts() -> (tokio::runtime::Runtime, crate::TuiOptions) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let opts = crate::TuiOptions {
            session: dal_agent::SessionRef::Ephemeral {
                workspace: dal_core::Workspace::new(std::env::temp_dir()).unwrap(),
            },
            screen: crate::Screen::Inline,
            theme_request: crate::ThemeRequest::Palette,
            default_model: None,
            images: false,
            diagrams: false,
            motion: false,
            editor: "true".into(),
            color: crate::ColorMode::Never,
            binary: "dal",
            env: crate::EnvFacts::default(),
            rt: runtime.handle().clone(),
        };
        (runtime, opts)
    }

    #[test]
    fn the_inline_expanded_window_names_the_scroll_path() {
        let tool_entry = scroll_path_tool_entry();
        let reasoning_entry = scroll_path_reasoning_entry();
        let view = scroll_path_view(tool_entry, reasoning_entry);
        let (_runtime, opts) = scroll_path_opts();
        let theme = crate::theme::load(
            &crate::ThemeRequest::Palette,
            crate::ColorMode::Never,
            None,
            None,
        )
        .unwrap();
        let dialog = crate::dialog::DialogUi::default();
        let transcript = Transcript::default();
        let viewport = crate::screen::fullscreen::Viewport::following();
        let cache = RenderCache::default();

        let mut live = crate::live::Live::default();
        live.open_expand(
            super::ExpandedCard::Tool {
                entry: EntryId::new(NonZeroU64::MIN),
            },
            Vec::new(),
        );
        let paint = |live: &crate::live::Live| {
            super::frame_rows(
                expanded_frame(
                    &view,
                    live,
                    &transcript,
                    &viewport,
                    &opts,
                    &theme,
                    &dialog,
                    &cache,
                    None,
                ),
                80,
                24,
            )
        };

        // First paint: the window shows a scroll affordance naming pgdn and
        // the hidden-below count.
        let first = paint(&live);
        let footer = first
            .iter()
            .find(|row| row.text.contains("pgdn"))
            .expect("the paged card names its scroll path");
        assert!(footer.text.contains("more line"), "{footer:?}");

        // Every row stays reachable: scrolling down lands the last line.
        let mut rows = first.clone();
        for _ in 0..64 {
            live.expanded().expect("view open").scroll_down();
            rows = paint(&live);
            if rows.iter().any(|row| row.text.contains("line 40")) {
                break;
            }
        }
        assert!(
            rows.iter().any(|row| row.text.contains("line 40")),
            "the final row is reachable through the named scroll path"
        );
        live.close_expand();

        // A card that fits the window paints no truncation footer.
        live.open_expand(
            super::ExpandedCard::Reasoning {
                entry: EntryId::new(NonZeroU64::new(2).unwrap()),
            },
            Vec::new(),
        );
        let small = paint(&live);
        assert!(small.iter().any(|row| row.text.contains("step two")));
        assert!(
            !small.iter().any(|row| row.text.contains("more line")),
            "a fully visible card keeps every row and no footer: {:?}",
            small
                .iter()
                .map(|row| row.text.as_str())
                .collect::<Vec<_>>()
        );
        live.close_expand();
    }

    fn picker_over_scroll_path_rows(picker: &crate::picker::PickerUi) -> Vec<super::RenderRow> {
        let tool_entry = scroll_path_tool_entry();
        let reasoning_entry = scroll_path_reasoning_entry();
        let view = scroll_path_view(tool_entry, reasoning_entry);
        let (_runtime, opts) = scroll_path_opts();
        let theme = crate::theme::load(
            &crate::ThemeRequest::Palette,
            crate::ColorMode::Never,
            None,
            None,
        )
        .unwrap();
        let dialog = crate::dialog::DialogUi::default();
        let transcript = Transcript::default();
        let viewport = crate::screen::fullscreen::Viewport::following();
        let cache = RenderCache::default();
        let live = crate::live::Live::default();
        super::frame_rows(
            expanded_frame(
                &view,
                &live,
                &transcript,
                &viewport,
                &opts,
                &theme,
                &dialog,
                &cache,
                Some(picker),
            ),
            80,
            10,
        )
    }

    #[test]
    fn picker_overlay_rows_stay_within_region_budget_at_shrunk_sizes() {
        use crate::frame::{RegionBudget, RegionRequest};
        use crate::picker::{PickerAction, PickerOption, PickerUi};

        let options: Vec<PickerOption> = (0..20)
            .map(|n| PickerOption {
                label: format!("model-{n:02}"),
                action: PickerAction::Command(dal_core::Command::Run {
                    name: "model".into(),
                    args: format!("provider/model-{n:02}").into(),
                    expected: None,
                }),
            })
            .collect();
        let picker = PickerUi::new("Pick a model", "", options);
        assert_eq!(picker.visible_len(), 20);

        let full = super::picker_overlay_rows(&picker, 15);
        assert_eq!(
            full.len(),
            8,
            "title + 6-option window + hint at usable sizes: {:?}",
            full.iter().map(|row| row.text.as_str()).collect::<Vec<_>>()
        );
        let shrunk = super::picker_overlay_rows(&picker, 5);
        assert_eq!(
            shrunk.len(),
            5,
            "80x10 overlay budget is 5 rows, emission must not exceed it"
        );
        assert!(
            shrunk.iter().any(|row| row.text.contains("model-00")),
            "the selected row stays visible in the shrunk window: {:?}",
            shrunk
                .iter()
                .map(|row| row.text.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(super::picker_overlay_rows(&picker, 1).len(), 1);
        assert_eq!(super::picker_overlay_rows(&picker, 2).len(), 2);

        let mut moved = picker.clone();
        for _ in 0..10 {
            moved.move_selection(true);
        }
        let window = super::picker_overlay_rows(&moved, 5);
        assert_eq!(window.len(), 5);
        assert!(
            window.iter().any(|row| row.text.contains("model-10")),
            "a mid-list selection recenters instead of clipping away: {:?}",
            window
                .iter()
                .map(|row| row.text.as_str())
                .collect::<Vec<_>>()
        );

        let budget = RegionBudget::allocate(
            80,
            10,
            RegionRequest {
                overlay: Some(picker.visible_len().max(1) + 2),
                ..RegionRequest::default()
            },
        );
        assert_eq!(budget.overlay, 5);
        assert_eq!(budget.cap, 6);

        let rows = picker_over_scroll_path_rows(&picker);
        assert!(
            rows.len() <= budget.cap,
            "shrunk live block must not exceed its cap: {} > {}",
            rows.len(),
            budget.cap
        );
        assert!(
            rows.iter().any(|row| row.text.contains("model-")),
            "the picker stays painted at shrunk sizes: {:?}",
            rows.iter().map(|row| row.text.as_str()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn picker_focus_paints_reverse_video_not_color_alone() {
        use crate::picker::{PickerAction, PickerOption, PickerUi};

        let options: Vec<PickerOption> = (0..4)
            .map(|n| PickerOption {
                label: format!("opt-{n}"),
                action: PickerAction::Noop,
            })
            .collect();
        let mut picker = PickerUi::new("Pick", "", options);
        picker.move_selection(true);
        let rows = super::picker_overlay_rows(&picker, 12);
        let selected: Vec<&super::RenderRow> =
            rows.iter().filter(|row| !row.selected.is_empty()).collect();
        assert_eq!(
            selected.len(),
            1,
            "exactly the focused row carries the reverse-video selection"
        );
        assert_eq!(selected[0].selected, vec![0..selected[0].text.len()]);
        assert_eq!(selected[0].text, "> opt-1");
    }

    #[test]
    fn picker_fallback_row_paints_the_same_focus() {
        use crate::picker::model_picker;

        let picker = model_picker(&[], "acme/gpt-6");
        assert_eq!(picker.model_filter_fallback(), Some("acme/gpt-6"));
        let rows = super::picker_overlay_rows(&picker, 12);
        let fallback = rows
            .iter()
            .find(|row| row.text.contains("/model acme/gpt-6"))
            .expect("the typed-id fallback row paints");
        assert_eq!(
            fallback.selected,
            vec![0..fallback.text.len()],
            "the selectable fallback gets the same reverse-video focus: {fallback:?}"
        );
    }

    #[test]
    fn art_cells_follow_the_design_role_map() {
        use crate::diagram::{ArtCell, ArtRole};
        use crate::theme::Role;

        let cells: Vec<ArtCell> = [
            (ArtRole::Border, "+"),
            (ArtRole::Text, "x"),
            (ArtRole::Edge, ">"),
            (ArtRole::EdgeLabel, "label"),
            (ArtRole::Title, "title"),
        ]
        .into_iter()
        .map(|(role, text)| ArtCell {
            text: text.to_owned(),
            role,
        })
        .collect();
        let row = super::art_row(&cells);
        let styled: Vec<(&str, Role, bool)> = row
            .spans
            .iter()
            .map(|span| (&row.text[span.range.clone()], span.role, span.bold))
            .collect();
        assert_eq!(
            styled,
            [
                ("+", Role::Dim, false),
                ("x", Role::Text, false),
                (">", Role::Accent, false),
                ("label", Role::Dim, false),
                ("title", Role::Accent, true),
            ],
            "DESIGN.md Diagrams: border dim, text text, edge accent, edge label dim, title accent+bold"
        );
        // Adjacent same-role cells merge; a bold boundary never does.
        let merged = super::art_row(&[
            ArtCell {
                text: "ab".to_owned(),
                role: ArtRole::EdgeLabel,
            },
            ArtCell {
                text: "cd".to_owned(),
                role: ArtRole::EdgeLabel,
            },
        ]);
        assert_eq!(merged.spans.len(), 1, "same-role cells share one span");
    }

    /// The resume picker is the one picker with a delete key, so its hint row
    /// names it; every other picker keeps the common hint.
    #[test]
    fn the_resume_picker_shows_the_delete_hint() {
        use crate::picker::{PickerAction, PickerOption, PickerUi, ResumeTarget, SessionRow};

        let workspace = dal_core::Workspace::new(std::env::temp_dir()).expect("absolute workspace");
        let resume = crate::picker::resume_picker(
            vec![SessionRow {
                target: ResumeTarget {
                    id: dal_core::SessionId::new_v7(),
                    workspace,
                },
                label: "fix · today · 3 messages · /w".to_owned(),
            }],
            "",
        );
        let hint = |picker: &PickerUi| {
            super::picker_overlay_rows(picker, 12)
                .last()
                .map(|row| row.text.clone())
                .expect("a picker paints at least its hint row")
        };
        assert_eq!(hint(&resume), crate::copy::ids::RESUME_HINT);
        assert!(hint(&resume).contains("d delete"));

        let plain = PickerUi::new(
            "Pick",
            "",
            vec![PickerOption {
                label: "opt".to_owned(),
                action: PickerAction::Noop,
            }],
        );
        assert_eq!(hint(&plain), crate::copy::ids::PICKER_HINT);
    }

    #[test]
    fn a_multi_picker_paints_check_markers_and_the_same_focus() {
        use crate::picker::{PickerAction, PickerOption, PickerUi};

        let options: Vec<PickerOption> = (0..3)
            .map(|n| PickerOption {
                label: format!("opt-{n}"),
                action: PickerAction::Noop,
            })
            .collect();
        let mut picker = PickerUi::new("Pick", "", options);
        picker.with_multi([0]);
        let rows = super::picker_overlay_rows(&picker, 12);
        let texts: Vec<&str> = rows.iter().map(|row| row.text.as_str()).collect();
        assert!(
            texts.iter().any(|text| text.starts_with("> [x] ")),
            "the focused checked row carries focus plus the checked marker: {texts:?}"
        );
        assert!(
            texts
                .iter()
                .filter(|text| text.starts_with("  [ ] "))
                .count()
                == 2,
            "unchecked multi rows paint the empty marker: {texts:?}"
        );
        assert!(
            rows.iter()
                .filter(|row| !row.selected.is_empty())
                .all(|row| {
                    matches!(row.selected.as_slice(), [range]
                        if range.start == 0 && range.end == row.text.len())
                }),
            "reverse video still covers the whole focused row"
        );

        // A single picker keeps the unmarked gutter.
        let single = PickerUi::new(
            "Pick",
            "",
            (0..2)
                .map(|n| PickerOption {
                    label: format!("opt-{n}"),
                    action: PickerAction::Noop,
                })
                .collect(),
        );
        let single_rows = super::picker_overlay_rows(&single, 12);
        let texts: Vec<&str> = single_rows.iter().map(|row| row.text.as_str()).collect();
        assert!(
            !texts
                .iter()
                .any(|text| text.contains("[ ]") || text.contains("[x]")),
            "no check markers on a single picker: {texts:?}"
        );
    }

    #[test]
    fn the_draft_confirm_leads_with_the_focused_verb() {
        let tool_entry = scroll_path_tool_entry();
        let reasoning_entry = scroll_path_reasoning_entry();
        let view = scroll_path_view(tool_entry, reasoning_entry);
        let (_runtime, opts) = scroll_path_opts();
        let theme = crate::theme::load(
            &crate::ThemeRequest::Palette,
            crate::ColorMode::Never,
            None,
            None,
        )
        .unwrap();
        let dialog = crate::dialog::DialogUi::default();
        let transcript = Transcript::default();
        let viewport = crate::screen::fullscreen::Viewport::following();
        let cache = RenderCache::default();
        let live = crate::live::Live::default();
        for choice in [
            crate::dialog::DraftChoice::Keep,
            crate::dialog::DraftChoice::Discard,
        ] {
            let mut input = expanded_frame(
                &view,
                &live,
                &transcript,
                &viewport,
                &opts,
                &theme,
                &dialog,
                &cache,
                None,
            );
            input.exit_draft = Some(choice);
            let rows = super::frame_rows(input, 80, 24);
            let row = rows
                .iter()
                .find(|row| !row.selected.is_empty())
                .expect("the focused verb paints the selection range");
            let focused = &row.text[..row.selected[0].end.min(row.text.len())];
            let want = match choice {
                crate::dialog::DraftChoice::Keep => "[n]",
                crate::dialog::DraftChoice::Discard => "[y]",
            };
            assert!(
                focused.starts_with(want),
                "{choice:?}: the focused verb heads the row so clipping can never hide it: {row:?}"
            );
        }
        // The focused verb still heads the row at a starved width.
        let mut input = expanded_frame(
            &view,
            &live,
            &transcript,
            &viewport,
            &opts,
            &theme,
            &dialog,
            &cache,
            None,
        );
        input.exit_draft = Some(crate::dialog::DraftChoice::Discard);
        let narrow = super::frame_rows(input, 20, 10);
        assert!(
            narrow
                .iter()
                .any(|row| row.text.starts_with("[y]") && !row.selected.is_empty()),
            "narrow frame keeps the focused verb first and selected: {:?}",
            narrow
                .iter()
                .map(|row| row.text.as_str())
                .collect::<Vec<_>>()
        );
    }

    /// An open select keeps its title, the row Enter answers and its actions
    /// inside the region at every supported terminal height. The dialog's
    /// boundary blanks only paint once three content rows remain, so a
    /// four-row overlay carries four rows of content, not a bordered pair.
    #[test]
    fn an_open_select_stays_inside_the_region_at_every_supported_height() {
        use crate::frame::{RegionBudget, RegionRequest};
        use crossterm::event::{KeyCode, KeyModifiers};
        use dal_core::{Choice, Owner, Question, Request, RequestId};

        let view = scroll_path_view(scroll_path_tool_entry(), scroll_path_reasoning_entry());
        let (_runtime, opts) = scroll_path_opts();
        let theme = crate::theme::load(
            &crate::ThemeRequest::Palette,
            crate::ColorMode::Never,
            None,
            None,
        )
        .unwrap();
        let transcript = Transcript::default();
        let viewport = crate::screen::fullscreen::Viewport::following();
        let cache = RenderCache::default();
        let live = crate::live::Live::default();
        let mut dialog = crate::dialog::DialogUi::default();
        dialog.opened(Request {
            id: RequestId::new_v7(),
            turn: None,
            owner: Owner::Core,
            question: Question::Select {
                prompt: "pick".into(),
                options: (1..=7)
                    .map(|index| Choice {
                        label: format!("Option {index}").into(),
                        description: None,
                    })
                    .collect(),
                multi: false,
                preview: None,
            },
            timeout: std::time::Duration::from_secs(30),
            default: dal_core::Answer::Cancel,
        });
        let start = std::time::Instant::now();
        dialog.tick(start);
        dialog.tick(start + crate::dialog::ARM_DELAY);
        for focus in 0..=7 {
            let focused = if focus == 7 {
                "> ( ) Other".to_owned()
            } else {
                format!("> (*) Option {}", focus + 1)
            };
            for height in 8..=14 {
                let cap = RegionBudget::allocate(
                    80,
                    height,
                    RegionRequest {
                        overlay: Some(8),
                        ..RegionRequest::default()
                    },
                )
                .cap;
                let input = expanded_frame(
                    &view,
                    &live,
                    &transcript,
                    &viewport,
                    &opts,
                    &theme,
                    &dialog,
                    &cache,
                    None,
                );
                let rows = super::frame_rows(input, 80, height);
                let texts: Vec<&str> = rows.iter().map(|row| row.text.as_str()).collect();
                let at = format!("focus {focus}, height {height}: {texts:?}");
                assert!(rows.len() <= cap, "{at}");
                assert_eq!(
                    texts.iter().filter(|text| **text == "pick").count(),
                    1,
                    "{at}"
                );
                assert_eq!(
                    texts.iter().filter(|text| **text == focused).count(),
                    1,
                    "{at}"
                );
                assert_eq!(
                    texts.iter().filter(|text| text.starts_with("> ")).count(),
                    1,
                    "{at}"
                );
                assert!(
                    texts.iter().any(|text| text.contains("esc dismiss")),
                    "{at}"
                );
            }
            dialog.key(crate::keys::Key::new(KeyCode::Down, KeyModifiers::NONE));
        }
    }
}
