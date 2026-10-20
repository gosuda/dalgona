//! Damage-only terminal painting with a write-once inline transcript.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::image::Rung;
use crate::render::{FrameInput, PixelImage, RenderLink, RenderRow, RenderSpan, frame_rows};
use crate::screen::inline::{Placement, located_top};
use crate::term::{CursorReport, TermIo, TermState};
use crate::theme::{ResolvedTheme, Role};
use crate::transcript::Transcript;
use crate::width::WidthMode;
use crate::{Screen, TuiError};

/// Longest a frame may wait for a caret-position answer before holding.
const POSITION_QUERY_BUDGET: Duration = Duration::from_millis(300);

/// One paint-thread-owned terminal surface.
#[derive(Default)]
pub(crate) struct Painter {
    previous: Vec<RenderRow>,
    /// The next main-screen frame is structural whatever the diff says; set
    /// by `invalidate`, cleared once a main-screen frame lands.
    repaint: bool,
    size: (u16, u16),
    committed: usize,
    /// Screen row, 1-based, where the live block starts: directly under the
    /// last committed row, which is also where the next commit lands.
    anchor: usize,
    /// Screen row, 1-based, where the last successful inline write left the
    /// physical caret: the caret-owning row, or the block top when hidden.
    caret_row: usize,
    /// Screen column, 1-based, where that write left the caret (`c0` for
    /// width-reflow measurement).
    caret_col: usize,
    /// Rows from the live block top to `caret_row` at the last successful
    /// inline paint (`k`): 0 when the caret is hidden at the block top.
    caret_offset: usize,
    /// `TermIo::resize_epoch` accepted at the last paint.
    epoch: u64,
    sync: bool,
    initialized: bool,
    overlay: bool,
    /// The main-screen frame parked while the transcript overlay owns the
    /// alternate screen. The main buffer is never touched meanwhile, so
    /// leaving the overlay diffs against it and repaints only what changed.
    main: Option<MainFrame>,
    /// Terminal cell (row, column), 1-based, where the caret was last placed.
    cursor: Option<(usize, usize)>,
    #[expect(
        clippy::option_option,
        reason = "outer None records unset, inner None records the default theme"
    )]
    theme_name: Option<Option<&'static str>>,
    image_rung: Option<Rung>,
    image_picker: Option<ratatui_image::picker::Picker>,
    image_protocols: HashMap<ImageKey, Option<ratatui_image::protocol::Protocol>>,
    image_order: VecDeque<ImageKey>,
}

const IMAGE_PROTOCOL_CACHE_CAP: usize = 16;

/// The painter's measured decision for one frame: a located stale-block top
/// plus the accepted size and epoch, or a hold.
struct Measured {
    placement: Placement,
    size: (u16, u16),
    epoch: u64,
}

impl Measured {
    /// A frame that writes no live bytes and retries next pass.
    const fn hold() -> Self {
        Self {
            placement: Placement::Hold,
            size: (0, 0),
            epoch: 0,
        }
    }

    /// A measured stale-block top with the accepted size and epoch.
    const fn located(top: u16, size: (u16, u16), epoch: u64) -> Self {
        Self {
            placement: Placement::Located(top),
            size,
            epoch,
        }
    }
}

/// A verified caret answer with the screen size and resize epoch observed
/// alongside it: row, column, size, epoch.
type CaretPosition = (u16, u16, (u16, u16), u64);

/// The size, resize epoch, and placement the frame paints with: size, epoch, placement.
type FramePlacement = ((u16, u16), u64, Placement);

/// The staged bytes and rows one frame writes, with the decisions made
/// about them: the target screen, the measured size and epoch, whether the
/// frame rebuilds structure or only appends commits, and the settled-commit
/// count the frame records when it lands.
struct StagedFrame {
    screen: Screen,
    size: (u16, u16),
    epoch: u64,
    rows: Vec<RenderRow>,
    bytes: Vec<u8>,
    structural: bool,
    transcript_grew: bool,
    overlay: bool,
    committed_len: usize,
}

/// The main-screen state the painter parks across a transcript overlay.
struct MainFrame {
    previous: Vec<RenderRow>,
    size: (u16, u16),
    initialized: bool,
    anchor: usize,
    epoch: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct ImageKey {
    digest: [u8; 32],
    columns: u16,
    rows: u16,
    rung: Rung,
}

impl Painter {
    /// Makes the next main-screen frame structural, so it rewrites the whole
    /// live block. `Ctrl+L`, the external editor, and a suspend consume this:
    /// a diff paint with unchanged rows would otherwise emit zero bytes and
    /// leave mangled rows corrupted. `previous` stays: the structural erase
    /// needs the stale block's height, which differs from the new block's
    /// when an editor returns a shorter draft.
    pub(crate) fn invalidate(&mut self) {
        self.repaint = true;
    }

    /// Changes the synchronized-update capability after the startup probe.
    pub(crate) fn set_sync(&mut self, sync: bool) {
        self.sync = sync;
    }
    pub(crate) fn set_image_rung(&mut self, rung: Option<Rung>) {
        if self.image_rung == rung {
            return;
        }
        self.image_rung = rung;
        self.image_picker = rung.map(|rung| {
            let protocol = match rung {
                Rung::KittyDirect | Rung::KittyPlaceholders => {
                    ratatui_image::picker::ProtocolType::Kitty
                }
                Rung::ITerm2 => ratatui_image::picker::ProtocolType::Iterm2,
                Rung::Sixel => ratatui_image::picker::ProtocolType::Sixel,
            };
            let mut picker = ratatui_image::picker::Picker::halfblocks();
            picker.set_protocol_type(protocol);
            picker
        });
        self.image_protocols.clear();
        self.image_order.clear();
    }

    /// Parks the caret on the last row of the live block, so whatever the
    /// host writes next (the exit line) starts below the block instead of
    /// over one of its rows.
    ///
    /// A hidden caret rests at the block top while the composer is empty, and
    /// the host's `\r\n` text continues from wherever the caret rests. A block
    /// the last frame no longer describes (a resize landed after it) is
    /// bottom-anchored, so the screen's last row stands in for it. Nothing
    /// moves while the transcript overlay owns the alternate screen or before
    /// the first frame.
    pub(crate) fn rest_below_block(&self, io: &dyn TermIo) -> Result<(), TuiError> {
        if self.overlay || !self.initialized || self.previous.is_empty() {
            return Ok(());
        }
        let size = io.size().unwrap_or(self.size);
        let last = if size == self.size {
            self.anchor + self.previous.len() - 1
        } else {
            usize::from(size.1)
        };
        let row = last.clamp(1, usize::from(size.1).max(1));
        io.write(format!("\x1b[{row};1H").as_bytes()).map_err(|error| {
            TuiError::Terminal(format!(
                "dalgon: cannot write the interface: {error}\nCheck that the terminal is still connected."
            ))
        })
    }

    /// Renders one frame; settled rows are committed before the live block moves.
    ///
    /// After any resize the stale live block is located by measuring the
    /// physical caret (DSR-6 + DSR-5 sentinel on Unix, the console API on
    /// Windows), never by deriving rows from the old height. When no trusted
    /// position exists the frame holds: it writes nothing, keeps every queued
    /// commit and keystroke, and measures again next frame.
    pub(crate) fn paint(
        &mut self,
        io: &dyn TermIo,
        state: &Arc<Mutex<TermState>>,
        input: FrameInput<'_>,
        overlay: bool,
    ) -> Result<(), TuiError> {
        let mut size = io.size().unwrap_or((80, 24));
        let mut epoch = io.resize_epoch();

        let relocated =
            self.relocate_block(io, input.screen, input.opts.env.width_mode, size, epoch)?;
        let Some((relocated_size, relocated_epoch, relocated_placement)) = relocated else {
            return Ok(());
        };
        size = relocated_size;
        epoch = relocated_epoch;
        let mut placement = relocated_placement;

        let screen = if overlay {
            Screen::Fullscreen
        } else {
            input.screen
        };
        let theme = input.theme;
        let mut rows = frame_rows(FrameInput { screen, ..input }, size.0, size.1);
        if input.composer.is_empty()
            && !input.dialog.is_open()
            && input.live.expanded().is_none()
            && input.signin.is_none()
            && input.picker.is_none()
            && input.help.is_none()
        {
            for row in &mut rows {
                row.cursor = None;
            }
        }
        let committed_len = input.transcript.rows().len();
        let mut bytes = Vec::new();
        if overlay != self.overlay {
            if overlay {
                (size, epoch, placement) = self.enter_overlay(&mut bytes, size, epoch, placement);
            } else {
                let left = self.leave_overlay(io, size, epoch, placement)?;
                let Some((left_size, left_epoch, left_placement)) = left else {
                    return Ok(());
                };
                size = left_size;
                epoch = left_epoch;
                placement = left_placement;
            }
        }
        let structural = !self.initialized
            || self.repaint
            || !matches!(placement, Placement::Anchor)
            || self.size != size
            || self.previous.len() != rows.len()
            || self.theme_name != Some(theme.name());
        let transcript_grew = !overlay && committed_len > self.committed;
        if screen == Screen::Inline {
            self.inline_bytes(
                &mut bytes,
                size,
                &rows,
                input.transcript,
                self.committed,
                structural,
                placement,
                theme,
            );
        } else {
            self.fullscreen_bytes(&mut bytes, size, &rows, structural, theme);
        }
        self.emit_frame(
            io,
            state,
            input.screen,
            StagedFrame {
                screen,
                size,
                epoch,
                rows,
                bytes,
                structural,
                transcript_grew,
                overlay,
                committed_len,
            },
            theme,
        )
    }

    /// Draws frame images, parks the cursor, and writes the frame. Every model
    /// field that mirrors the screen (rows, size, epoch, commits, theme) moves
    /// only when the frame is known to land: an empty frame is already final,
    /// and a frame discarded on an epoch change leaves the model matching the
    /// untouched screen.
    fn emit_frame(
        &mut self,
        io: &dyn TermIo,
        state: &Arc<Mutex<TermState>>,
        input_screen: Screen,
        staged: StagedFrame,
        theme: &ResolvedTheme,
    ) -> Result<(), TuiError> {
        let StagedFrame {
            screen,
            size,
            epoch,
            rows,
            mut bytes,
            structural,
            transcript_grew,
            overlay,
            committed_len,
        } = staged;
        let image_written =
            self.draw_frame_images(&mut bytes, screen, size, &rows, structural, transcript_grew);
        if image_written && screen == Screen::Inline {
            bytes.extend_from_slice(format!("\x1b[{};1H", size.1).as_bytes());
        }
        let top = if screen == Screen::Inline {
            self.anchor.saturating_sub(1)
        } else {
            usize::from(size.1).saturating_sub(rows.len())
        };
        let park = (screen == Screen::Inline && !overlay).then_some(top + 1);
        self.place_cursor(&mut bytes, size, top, &rows, overlay, park);
        if bytes.is_empty() {
            self.finish_frame(committed_len, size, epoch, rows, overlay, theme);
            return Ok(());
        }
        // A resize signal between measurement and write discards the frame:
        // holding narrows the in-flight race to signal latency.
        if input_screen == Screen::Inline && !overlay && io.resize_epoch() != epoch {
            return Ok(());
        }
        if self.sync {
            bytes.splice(..0, b"\x1b[?2026h".iter().copied());
            bytes.extend_from_slice(b"\x1b[?2026l");
            state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .set_sync_open(true);
        }
        let result = io.write(&bytes).map_err(|error| TuiError::Terminal(format!(
            "dalgon: cannot write the interface: {error}\nCheck that the terminal is still connected."
        )));
        if result.is_ok() {
            if screen == Screen::Inline && !overlay {
                self.caret_row = self.cursor.map_or_else(|| top + 1, |(row, _)| row);
                self.caret_col = self.cursor.map_or(1, |(_, column)| column);
                self.caret_offset = self.caret_row.saturating_sub(self.anchor);
            }
            self.finish_frame(committed_len, size, epoch, rows, overlay, theme);
            let mut modes = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if self.sync {
                modes.set_sync_open(false);
            }
            modes.set_transcript_overlay(overlay);
        }
        result
    }

    /// Relocates the stale live block after a resize signal by measuring the
    /// physical caret, never by deriving rows from the old height. Returns the
    /// size, epoch, and placement the frame paints with, or `None` when no
    /// trusted position exists: the frame holds, writing nothing and keeping
    /// every queued commit and keystroke for the next measure.
    fn relocate_block(
        &mut self,
        io: &dyn TermIo,
        screen: Screen,
        mode: WidthMode,
        size: (u16, u16),
        epoch: u64,
    ) -> Result<Option<FramePlacement>, TuiError> {
        // Relocate only when the session is inline, the main screen is the
        // target (also on an overlay-entry frame, before ?1049h), the block
        // was painted, and something signaled a resize.
        if screen == Screen::Inline
            && !self.overlay
            && self.initialized
            && (self.size != size || self.epoch != epoch)
        {
            match self.locate(io, mode)? {
                // HOLD: keep the stale block and all state untouched; the
                // next tick or resize measures again.
                Measured {
                    placement: Placement::Hold,
                    ..
                } => return Ok(None),
                Measured {
                    placement: Placement::Located(top),
                    size: new_size,
                    epoch: new_epoch,
                } => {
                    self.anchor = usize::from(top);
                    return Ok(Some((new_size, new_epoch, Placement::Located(top))));
                }
                Measured {
                    placement: Placement::Anchor,
                    ..
                } => {}
            }
        }
        Ok(Some((size, epoch, Placement::Anchor)))
    }

    /// Parks the main screen for a transcript overlay entry: erases the live
    /// block down to the bottom so the hidden primary's rows below the saved
    /// caret are blank, parks the saved caret at the erased top, then queues
    /// the switch. The mode flag flips only after the write succeeds.
    /// Returns the size, epoch, and placement the frame paints with.
    fn enter_overlay(
        &mut self,
        bytes: &mut Vec<u8>,
        size: (u16, u16),
        epoch: u64,
        placement: Placement,
    ) -> FramePlacement {
        if self.initialized {
            let top = self.anchor.max(1);
            for y in top..=usize::from(size.1) {
                bytes.extend_from_slice(format!("\x1b[{y};1H\x1b[2K").as_bytes());
            }
            bytes.extend_from_slice(format!("\x1b[{top};1H").as_bytes());
        }
        bytes.extend_from_slice(b"\x1b[?1049h\x1b[?25l");
        self.main = Some(MainFrame {
            previous: std::mem::take(&mut self.previous),
            size: self.size,
            initialized: self.initialized,
            anchor: self.anchor,
            epoch: self.epoch,
        });
        self.initialized = false;
        self.overlay = true;
        (size, epoch, placement)
    }

    /// Leaves the alternate screen and restores the parked main screen, then
    /// measures the restored primary — never querying while still on the
    /// alt. Returns the size, epoch, and placement the frame paints with, or
    /// `None` when the restored main block stays stale for one frame: the
    /// flag already flipped, so the next frame relocates normally.
    fn leave_overlay(
        &mut self,
        io: &dyn TermIo,
        mut size: (u16, u16),
        mut epoch: u64,
        mut placement: Placement,
    ) -> Result<Option<FramePlacement>, TuiError> {
        io.write(b"\x1b[r\x1b[?1049l").map_err(|error| {
            TuiError::Terminal(format!(
                "dalgon: cannot write the interface: {error}\nCheck that the terminal is still connected."
            ))
        })?;
        if let Some(main) = self.main.take() {
            self.previous = main.previous;
            self.size = main.size;
            self.initialized = main.initialized;
            self.anchor = main.anchor;
            self.epoch = main.epoch;
        } else {
            self.previous.clear();
            self.initialized = false;
        }
        // The restored caret is the parked saved cursor clamped to the
        // new screen; a hidden-buffer realloc keeps the window ending
        // at that row. Measure it, then erase from there down.
        match Self::restore_top(io)? {
            Measured {
                placement: Placement::Hold,
                ..
            } => {
                self.overlay = false;
                if self.size == size && self.epoch == epoch {
                    let top = u16::try_from(self.anchor.max(1)).unwrap_or(u16::MAX);
                    return Ok(Some((size, epoch, Placement::Located(top))));
                }
                return Ok(None);
            }
            Measured {
                placement: Placement::Located(top),
                size: new_size,
                epoch: new_epoch,
            } => {
                size = new_size;
                epoch = new_epoch;
                placement = Placement::Located(top);
                self.anchor = usize::from(top);
                self.caret_row = usize::from(top);
                self.caret_col = 1;
                self.caret_offset = 0;
            }
            Measured {
                placement: Placement::Anchor,
                ..
            } => {}
        }
        self.overlay = false;
        Ok(Some((size, epoch, placement)))
    }

    /// Commits the frame's model records once its bytes are final.
    fn finish_frame(
        &mut self,
        committed_len: usize,
        size: (u16, u16),
        epoch: u64,
        rows: Vec<RenderRow>,
        overlay: bool,
        theme: &ResolvedTheme,
    ) {
        self.theme_name = Some(theme.name());
        if !overlay {
            // Rows settled behind the overlay commit on the main screen once it returns.
            self.committed = committed_len;
            self.repaint = false;
        }
        self.size = size;
        self.epoch = epoch;
        self.previous = rows;
        self.initialized = true;
    }

    /// Measures the caret until a verified answer or the deadline, where the
    /// epoch is re-checked while the query is in flight. `None` means hold.
    /// I/O errors propagate.
    fn query_position(
        io: &dyn TermIo,
        deadline: Instant,
    ) -> Result<Option<CaretPosition>, TuiError> {
        loop {
            let epoch = io.resize_epoch();
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            match io.cursor_position(remaining) {
                Ok(CursorReport::Position(row, column)) => {
                    let size_now = io.size().unwrap_or((80, 24));
                    let epoch_now = io.resize_epoch();
                    let verified = epoch_now == epoch
                        && row >= 1
                        && row <= size_now.1
                        && column >= 1
                        && column <= size_now.0;
                    if verified {
                        return Ok(Some((row, column, size_now, epoch_now)));
                    }
                }
                Ok(CursorReport::Unpaired | CursorReport::Silent) | Err(_) => {
                    return Ok(None);
                }
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
        }
    }

    /// Locates the stale live block top after a resize: `T = R - k - p -
    /// reflow_extra` from the measured caret row, the recorded caret offset,
    /// and the width engine's wrap terms. `Placement::Hold` when nothing can
    /// be trusted this frame.
    fn locate(&self, io: &dyn TermIo, mode: WidthMode) -> Result<Measured, TuiError> {
        let deadline = Instant::now() + POSITION_QUERY_BUDGET;
        let Some((row, column, size_now, epoch)) = Self::query_position(io, deadline)? else {
            return Ok(Measured::hold());
        };
        let offset = u16::try_from(self.caret_offset).unwrap_or(u16::MAX);
        if size_now.0 == self.size.0 {
            let top = located_top(row, offset, 0, 0);
            return Ok(Measured::located(top, size_now, epoch));
        }
        match self.reflow_top(row, column, offset, size_now.0, mode) {
            Some(top) => Ok(Measured::located(top, size_now, epoch)),
            None => Ok(Measured::hold()),
        }
    }

    /// Measures the parked saved caret on the primary after `?1049l`: the
    /// stale erased region starts at the reported row.
    fn restore_top(io: &dyn TermIo) -> Result<Measured, TuiError> {
        let deadline = Instant::now() + POSITION_QUERY_BUDGET;
        let Some((row, _column, size_now, epoch)) = Self::query_position(io, deadline)? else {
            return Ok(Measured::hold());
        };
        Ok(Measured::located(row.max(1), size_now, epoch))
    }

    /// Applies the width-change wrap terms to the measured caret position.
    /// Rows the painter painted are the only reflowed content it can know:
    /// each known above-caret row contributes its wrapped expansion, and the
    /// caret row contributes its own wrap piece `p`.
    ///
    /// The caret column discriminates emulator policy when its own line was
    /// wider than the new width and the two policies predict different
    /// columns; a column that matches neither is untrusted. When no
    /// discriminating evidence exists the conservative bound wins: erase at
    /// the truncation top, which never touches committed rows on a truncating
    /// terminal and leaves only inert stale fragments on a reflowing one.
    fn reflow_top(
        &self,
        row: u16,
        column: u16,
        offset: u16,
        width: u16,
        mode: WidthMode,
    ) -> Option<u16> {
        let new_width = usize::from(width).max(1);
        // The caret sits `caret_offset` rows below the block top: its own row
        // index, whether the caret is visible or hidden at the top (k = 0).
        let caret_index = self.caret_offset.min(self.previous.len().saturating_sub(1));
        let above: Vec<usize> = self.previous[..caret_index]
            .iter()
            .map(|row| crate::width::width(&row.text, mode))
            .collect();
        let caret_cells = self
            .previous
            .get(caret_index)
            .map_or(0, |row| crate::width::width(&row.text, mode));
        let cell_before_caret = self.caret_col.saturating_sub(1);
        let reflow_col = cell_before_caret % new_width + 1;
        let truncate_col = self.caret_col.min(new_width);
        let caret_wide = caret_cells > new_width;
        let discriminating = caret_wide && reflow_col != truncate_col;
        if caret_wide && discriminating {
            let matches_reflow = usize::from(column) == reflow_col;
            let matches_truncate = usize::from(column) == truncate_col;
            if !matches_reflow && !matches_truncate {
                return None;
            }
            if matches_truncate {
                return Some(located_top(row, offset, 0, 0));
            }
            let piece = crate::screen::inline::piece(cell_before_caret, new_width);
            let extra = crate::screen::inline::reflow_extra(&above, new_width);
            return Some(located_top(row, offset, piece, extra));
        }
        // Without discriminating evidence the truncation bound wins: it is
        // exact on a truncating terminal and leaves only inert stale
        // fragments above the erase on a reflowing one — never committed rows.
        Some(located_top(row, offset, 0, 0))
    }

    /// Moves the terminal caret onto the row that holds it. When no row owns
    /// the caret, `park` names the 1-based row of column 1 the hidden caret
    /// rests at (the inline block top, so `k = 0` is exact on the next
    /// measurement); the transcript overlay hides the caret and parks the
    /// alternate caret at home column 1 every frame.
    fn place_cursor(
        &mut self,
        out: &mut Vec<u8>,
        size: (u16, u16),
        top: usize,
        rows: &[RenderRow],
        overlay: bool,
        park: Option<usize>,
    ) {
        let place = rows
            .iter()
            .enumerate()
            .find_map(|(index, row)| row.cursor.map(|column| (index, column)))
            .filter(|_| !overlay)
            .map(|(index, column)| {
                let last = usize::from(size.0).saturating_sub(1);
                (top + index + 1, column.min(last) + 1)
            });
        if overlay {
            // Entering the overlay already hid the caret; every overlay frame
            // parks the alternate caret at home column 1 so a shrink saves
            // nothing into shared scrollback.
            self.cursor = None;
            if !out.is_empty() {
                out.extend_from_slice(b"\x1b[1;1H");
            }
            return;
        }
        if place != self.cursor || (place.is_some() && !out.is_empty()) {
            match place {
                Some((row, column)) => {
                    if self.cursor.is_none() {
                        out.extend_from_slice(b"\x1b[?25h");
                    }
                    out.extend_from_slice(format!("\x1b[{row};{column}H").as_bytes());
                }
                None if self.cursor.is_some() => out.extend_from_slice(b"\x1b[?25l"),
                None => {}
            }
        }
        if place.is_none()
            && !out.is_empty()
            && let Some(row) = park
        {
            out.extend_from_slice(format!("\x1b[{row};1H").as_bytes());
        }
        self.cursor = place;
    }

    fn inline_bytes(
        &mut self,
        out: &mut Vec<u8>,
        size: (u16, u16),
        rows: &[RenderRow],
        transcript: &Transcript,
        commit_start: usize,
        structural: bool,
        placement: Placement,
        theme: &ResolvedTheme,
    ) {
        let height = usize::from(size.1);
        let old_height = self.previous.len();
        let new_height = rows.len();
        let commit_len = transcript.rows().len().saturating_sub(commit_start);
        if structural || commit_len > 0 {
            let rebottom = !self.initialized || self.size.1 != size.1;
            // Erases start only where the stale block provably sits: the
            // recorded anchor when nothing resized, or the measured top after
            // a resize. Nothing above that row is ever touched.
            let old_top = match placement {
                _ if !self.initialized => height.saturating_sub(old_height) + 1,
                Placement::Located(top) => usize::from(top),
                Placement::Anchor | Placement::Hold => self.anchor,
            };
            if let Placement::Located(top) = placement {
                for y in usize::from(top)..=height {
                    out.extend_from_slice(format!("\x1b[{y};1H\x1b[2K").as_bytes());
                }
            } else {
                for index in 0..old_height {
                    let y = old_top + index;
                    if y <= height {
                        out.extend_from_slice(format!("\x1b[{y};1H\x1b[2K").as_bytes());
                    }
                }
            }
            let mut next = old_top;
            for index in commit_start..transcript.rows().len() {
                let Some(row) = transcript.render_row(index) else {
                    continue;
                };
                let card_rows = row
                    .image
                    .as_ref()
                    .map_or(1, |image| usize::from(image.rows).max(1));
                scroll_room(out, height, &mut next, card_rows);
                let image_top = u16::try_from(next - 1).unwrap_or(size.1);
                let image_key = row
                    .image
                    .as_ref()
                    .and_then(|image| self.prepare_image(image, image_top, size));
                out.extend_from_slice(format!("\x1b[{next};1H").as_bytes());
                write_row(out, &row, theme, image_key.is_some());
                if let Some(key) = image_key {
                    self.write_image(out, key, image_top);
                    out.extend_from_slice(format!("\x1b[{next};1H").as_bytes());
                }
                out.extend_from_slice(b"\r\n");
                next = (next + 1).min(height);
                if transcript.closes_block(index) {
                    // One blank row after every settled block keeps the
                    // spacing of every turn the same.
                    out.extend_from_slice(format!("\x1b[{next};1H\r\n").as_bytes());
                    next = (next + 1).min(height);
                }
            }
            scroll_room(out, height, &mut next, new_height);
            if rebottom {
                next = next.max(height.saturating_sub(new_height) + 1);
            }
            self.anchor = next;
        }
        let top = self.anchor;
        for (index, row) in rows.iter().enumerate() {
            if !structural && commit_len == 0 && self.previous.get(index) == Some(row) {
                continue;
            }
            let y = top + index;
            let image_top = u16::try_from(y.saturating_sub(1)).unwrap_or(size.1);
            let image_key = row
                .image
                .as_ref()
                .and_then(|image| self.prepare_image(image, image_top, size));
            out.extend_from_slice(format!("\x1b[{y};1H\x1b[2K").as_bytes());
            write_row(out, row, theme, image_key.is_some());
        }
        if !out.is_empty() {
            out.extend_from_slice(format!("\x1b[{};1H", size.1).as_bytes());
        }
    }

    fn fullscreen_bytes(
        &mut self,
        out: &mut Vec<u8>,
        size: (u16, u16),
        rows: &[RenderRow],
        structural: bool,
        theme: &ResolvedTheme,
    ) {
        for (index, row) in rows.iter().enumerate() {
            if !structural && self.previous.get(index) == Some(row) {
                continue;
            }
            let y = index + 1;
            let image_top = u16::try_from(index).unwrap_or(size.1);
            let image_key = row
                .image
                .as_ref()
                .and_then(|image| self.prepare_image(image, image_top, size));
            out.extend_from_slice(format!("\x1b[{y};1H\x1b[2K").as_bytes());
            write_row(out, row, theme, image_key.is_some());
        }
    }
    fn draw_frame_images(
        &mut self,
        out: &mut Vec<u8>,
        screen: Screen,
        size: (u16, u16),
        rows: &[RenderRow],
        structural: bool,
        transcript_grew: bool,
    ) -> bool {
        let frame_top = if screen == Screen::Inline {
            u16::try_from(self.anchor.saturating_sub(1)).unwrap_or(u16::MAX)
        } else {
            0
        };
        let mut wrote = false;
        for (index, row) in rows.iter().enumerate() {
            let Some(image) = row.image.as_ref() else {
                continue;
            };
            if !structural && !transcript_grew && self.previous.get(index) == Some(row) {
                continue;
            }
            let top = frame_top.saturating_add(u16::try_from(index).unwrap_or(u16::MAX));
            let Some(key) = self.prepare_image(image, top, size) else {
                continue;
            };
            wrote |= self.write_image(out, key, top);
        }
        wrote
    }

    fn prepare_image(
        &mut self,
        image: &PixelImage,
        top: u16,
        size: (u16, u16),
    ) -> Option<ImageKey> {
        let rung = self.image_rung?;
        let columns = image.columns.min(size.0);
        let rows = image.rows.min(size.1.saturating_sub(top));
        if columns == 0 || rows == 0 {
            return None;
        }
        let key = ImageKey {
            digest: image.digest,
            columns,
            rows,
            rung,
        };
        if !self.image_protocols.contains_key(&key) {
            if self.image_protocols.len() >= IMAGE_PROTOCOL_CACHE_CAP
                && let Some(oldest) = self.image_order.pop_front()
            {
                self.image_protocols.remove(&oldest);
            }
            let protocol = self.image_picker.as_ref().and_then(|picker| {
                let image = ::image::load_from_memory(&image.bytes).ok()?;
                picker
                    .new_protocol(
                        image,
                        ratatui::layout::Size::new(columns, rows),
                        ratatui_image::Resize::Fit(None),
                    )
                    .ok()
            });
            self.image_protocols.insert(key, protocol);
        }
        if let Some(index) = self.image_order.iter().position(|entry| *entry == key) {
            self.image_order.remove(index);
        }
        self.image_order.push_back(key);
        self.image_protocols.get(&key)?.as_ref().map(|_| key)
    }

    fn write_image(&self, out: &mut Vec<u8>, key: ImageKey, top: u16) -> bool {
        let Some(Some(protocol)) = self.image_protocols.get(&key) else {
            return false;
        };
        let area = ratatui::layout::Rect::new(0, top, key.columns, key.rows);
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(
            ratatui_image::Image::new(protocol).allow_clipping(true),
            area,
            &mut buffer,
        );
        let mut encoded = Vec::new();
        {
            let mut backend = ratatui::backend::CrosstermBackend::new(&mut encoded);
            let cells = area.positions().filter_map(|position| {
                buffer
                    .cell(position)
                    .map(|cell| (position.x, position.y, cell))
            });
            if ratatui::backend::Backend::draw(&mut backend, cells).is_err() {
                return false;
            }
        }
        out.extend_from_slice(&encoded);
        true
    }
}

/// Scrolls the screen up until `rows` rows fit from row `next` down, and
/// moves `next` with the content it scrolled.
fn scroll_room(out: &mut Vec<u8>, height: usize, next: &mut usize, rows: usize) {
    let overflow = (*next + rows).saturating_sub(height + 1);
    for _ in 0..overflow {
        out.extend_from_slice(format!("\x1b[{height};1H\r\n").as_bytes());
    }
    *next = next.saturating_sub(overflow).max(1);
}

fn write_row(out: &mut Vec<u8>, row: &RenderRow, theme: &ResolvedTheme, image_supported: bool) {
    if row.image_tail || (row.image.is_some() && image_supported) {
        return;
    }
    write_styled_selection(
        out,
        &row.text,
        &row.spans,
        &row.selected,
        &row.links,
        row.role,
        theme,
    );
}

#[cfg(test)]
/// Writes one row's text with its spans, links, and role applied.
pub(crate) fn write_styled_text(
    out: &mut Vec<u8>,
    text: &str,
    spans: &[RenderSpan],
    links: &[RenderLink],
    fallback: Role,
    theme: &ResolvedTheme,
) {
    write_styled_selection(out, text, spans, &[], links, fallback, theme);
}

/// Writes one row's text; byte ranges in `selected` are painted with the
/// theme's selection style instead of their span role.
fn write_styled_selection(
    out: &mut Vec<u8>,
    text: &str,
    spans: &[RenderSpan],
    selected: &[std::ops::Range<usize>],
    links: &[RenderLink],
    fallback: Role,
    theme: &ResolvedTheme,
) {
    let mut boundaries = vec![0, text.len()];
    boundaries.extend(
        spans
            .iter()
            .flat_map(|span| [span.range.start, span.range.end]),
    );
    boundaries.extend(selected.iter().flat_map(|range| [range.start, range.end]));
    boundaries.extend(
        links
            .iter()
            .flat_map(|link| [link.range.start, link.range.end]),
    );
    boundaries.retain(|boundary| *boundary <= text.len());
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut painted = false;
    let mut active: Option<&RenderLink> = None;
    for window in boundaries.windows(2) {
        let [start, end] = window else {
            continue;
        };
        if start == end {
            continue;
        }
        // A styled run ends before the next run starts, so no attribute leaks.
        let reset = std::mem::take(&mut painted);
        if reset {
            out.extend_from_slice(b"\x1b[0m");
        }
        let next = links
            .iter()
            .find(|link| link.range.start <= *start && *start < link.range.end);
        let active_range = active.map(|link| link.range.clone());
        let link_changed = match (active_range.as_ref(), next) {
            (Some(current), Some(next)) => current != &next.range,
            (Some(_), None) => true,
            _ => false,
        };
        if link_changed {
            close_link(out);
            active = None;
        }
        let mut opened = false;
        if active.is_none()
            && let Some(link) = next
        {
            open_link(out, &link.url);
            active = Some(link);
            opened = true;
        }
        if reset && active.is_some() && !opened {
            out.extend_from_slice(b"\x1b[4m");
        }
        let in_selection = selected
            .iter()
            .any(|range| range.start <= *start && *start < range.end);
        if in_selection {
            // The selection style is the theme's reverse video; it never
            // depends on color, so no role SGR is written underneath.
            if ResolvedTheme::selection_style()
                .add_modifier
                .contains(ratatui::style::Modifier::REVERSED)
            {
                out.extend_from_slice(b"\x1b[7m");
                painted = true;
            }
        } else {
            let (role, bold) = spans
                .iter()
                .find(|span| span.range.start <= *start && *start < span.range.end)
                .map_or((fallback, false), |span| (span.role, span.bold));
            let sgr = crate::status::style_sgr(theme, role, bold);
            painted = !sgr.is_empty();
            out.extend_from_slice(sgr.as_bytes());
        }
        out.extend_from_slice(&text.as_bytes()[*start..*end]);
    }
    if active.is_some() {
        close_link(out);
    }
    out.extend_from_slice(b"\x1b[0m");
}

/// Opens an OSC 8 link and underlines it: underline is reserved for links.
fn open_link(out: &mut Vec<u8>, url: &str) {
    out.extend_from_slice(b"\x1b]8;;");
    out.extend_from_slice(url.as_bytes());
    out.extend_from_slice(b"\x1b\\\x1b[4m");
}

fn close_link(out: &mut Vec<u8>) {
    out.extend_from_slice(b"\x1b[24m\x1b]8;;\x1b\\");
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::Mutex;

    use crate::image::Rung;
    use crate::render::PixelImage;
    use crate::render::RenderRow;
    use crate::term::TermIo as _;

    use super::Painter;

    #[test]
    fn kitty_pixel_rows_emit_graphics_protocol_bytes() {
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 2, 2);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("PNG header");
            writer
                .write_image_data(&[
                    255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
                ])
                .expect("PNG pixels");
        }
        let bytes: Arc<[u8]> = Arc::from(png.into_boxed_slice());
        let image = PixelImage {
            digest: *blake3::hash(&bytes).as_bytes(),
            bytes,
            columns: 2,
            rows: 2,
        };
        let mut painter = Painter::default();
        painter.set_image_rung(Some(Rung::KittyDirect));
        let key = painter
            .prepare_image(&image, 0, (10, 10))
            .expect("kitty protocol setup");
        let mut output = Vec::new();
        assert!(painter.write_image(&mut output, key, 0));
        assert!(output.windows(3).any(|bytes| bytes == b"\x1b_G"));
    }
    #[test]
    fn styled_file_links_emit_clickable_osc8_sequences() {
        let theme = crate::theme::load(
            &crate::ThemeRequest::Palette,
            crate::ColorMode::Never,
            None,
            None,
        )
        .expect("palette theme loads");
        let row = RenderRow::new("open file:///workspace/main.rs.", crate::theme::Role::Text);
        let mut output = Vec::new();
        super::write_styled_text(
            &mut output,
            &row.text,
            &row.spans,
            &row.links,
            crate::theme::Role::Text,
            &theme,
        );
        let output = String::from_utf8(output).expect("terminal output is UTF-8");
        assert!(output.contains(
            "\x1b]8;;file:///workspace/main.rs\x1b\\\x1b[4mfile:///workspace/main.rs\x1b[24m\x1b]8;;\x1b\\"
        ));
    }

    #[test]
    fn selected_ranges_preserve_text_and_highlight_only_selected_bytes() {
        let theme = crate::theme::load(
            &crate::ThemeRequest::Palette,
            crate::ColorMode::Never,
            None,
            None,
        )
        .expect("palette theme loads");
        let mut row = RenderRow::plain("hello", crate::theme::Role::Text);
        row.selected.push(1..3);
        let mut output = Vec::new();
        super::write_row(&mut output, &row, &theme, false);
        let output = String::from_utf8(output).expect("terminal output is UTF-8");
        let (before, highlighted) = output
            .split_once("\x1b[7m")
            .expect("selection starts reverse video");
        let (selected, after) = highlighted
            .split_once("\x1b[0m")
            .expect("selection ends reverse video");
        assert_eq!(before, "h");
        assert_eq!(selected, "el");
        assert_eq!(after.trim_end_matches("\x1b[0m"), "lo");
    }

    /// Scriptable `TermIo` for relocation tests: settable size and epoch, a
    /// scripted `cursor_position`, and every frame byte captured.
    #[derive(Debug)]
    struct FakeIo {
        size: Mutex<(u16, u16)>,
        epoch: Mutex<u64>,
        reports: Mutex<VecDeque<crate::term::CursorReport>>,
        queries: Mutex<u32>,
        writes: Mutex<Vec<Vec<u8>>>,
    }

    impl FakeIo {
        fn new(size: (u16, u16)) -> Self {
            Self {
                size: Mutex::new(size),
                epoch: Mutex::new(0),
                reports: Mutex::new(VecDeque::new()),
                queries: Mutex::new(0),
                writes: Mutex::new(Vec::new()),
            }
        }
        fn set_size(&self, size: (u16, u16)) {
            *self.size.lock().unwrap() = size;
        }
        fn bump_epoch(&self) {
            *self.epoch.lock().unwrap() += 1;
        }
        fn push_report(&self, report: crate::term::CursorReport) {
            self.reports.lock().unwrap().push_back(report);
        }
        fn queries(&self) -> u32 {
            *self.queries.lock().unwrap()
        }
        fn writes(&self) -> Vec<Vec<u8>> {
            self.writes.lock().unwrap().clone()
        }
    }

    impl crate::term::TermIo for FakeIo {
        fn enable_raw(&self) -> std::io::Result<()> {
            Ok(())
        }
        fn disable_raw(&self) {}
        fn size(&self) -> std::io::Result<(u16, u16)> {
            Ok(*self.size.lock().unwrap())
        }
        fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
            self.writes.lock().unwrap().push(bytes.to_vec());
            Ok(())
        }
        fn read(&self, _timeout: std::time::Duration) -> std::io::Result<Vec<u8>> {
            Ok(Vec::new())
        }
        fn raise_tstp(&self) {}
        fn resize_epoch(&self) -> u64 {
            *self.epoch.lock().unwrap()
        }
        fn cursor_position(
            &self,
            _timeout: std::time::Duration,
        ) -> std::io::Result<crate::term::CursorReport> {
            *self.queries.lock().unwrap() += 1;
            Ok(self
                .reports
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(crate::term::CursorReport::Silent))
        }
    }

    /// Minimal inline fixture: empty view, empty composer, palette theme.
    struct Fixture {
        view: dal_core::View,
        transcript: crate::transcript::Transcript,
        live: crate::live::Live,
        dialog: crate::dialog::DialogUi,
        viewport: crate::screen::fullscreen::Viewport,
        opts: crate::TuiOptions,
        theme: crate::theme::ResolvedTheme,
        cache: crate::diagram::RenderCache,
        state: Arc<Mutex<crate::term::TermState>>,
    }

    impl Fixture {
        fn new() -> Self {
            use std::num::NonZeroU64;
            let workspace = dal_core::Workspace::new(std::env::temp_dir()).unwrap();
            let view = dal_core::View {
                r#gen: dal_core::Gen::new(NonZeroU64::MIN),
                seq: dal_core::Seq::new(NonZeroU64::MIN),
                session: dal_core::SessionInfo {
                    id: dal_core::SessionId::new_v7(),
                    name: None,
                    preview: "".into(),
                    workspace,
                    updated_at: dal_core::Timestamp::UNIX_EPOCH,
                    created_at: Some(dal_core::Timestamp::UNIX_EPOCH),
                    archived: Some(false),
                    last_seq: Some(dal_core::Seq::new(NonZeroU64::MIN)),
                },
                turn: dal_core::TurnState::Idle,
                entries: dal_core::Page {
                    items: Vec::new(),
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
            };
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            Self {
                view,
                transcript: crate::transcript::Transcript::default(),
                live: crate::live::Live::default(),
                dialog: crate::dialog::DialogUi::default(),
                viewport: crate::screen::fullscreen::Viewport::following(),
                opts: crate::TuiOptions {
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
                },
                theme: crate::theme::load(
                    &crate::ThemeRequest::Palette,
                    crate::ColorMode::Never,
                    None,
                    None,
                )
                .unwrap(),
                cache: crate::diagram::RenderCache::default(),
                state: Arc::new(Mutex::new(crate::term::TermState::new())),
            }
        }

        fn input(&self) -> crate::render::FrameInput<'_> {
            self.input_with_composer("", 0)
        }

        fn input_with_composer<'a>(
            &'a self,
            composer: &'a str,
            cursor: usize,
        ) -> crate::render::FrameInput<'a> {
            crate::render::FrameInput {
                view: &self.view,
                screen: crate::Screen::Inline,
                composer,
                cursor,
                exit_draft: false,
                branch: None,
                popup: &[],
                live: &self.live,
                dialog: &self.dialog,
                picker: None,
                help: None,
                signin: None,
                transcript: &self.transcript,
                viewport: &self.viewport,
                opts: &self.opts,
                kitty_keyboard: self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .kitty(),
                theme: &self.theme,
                diagram_settings: crate::diagram::DiagramSettings::default(),
                diagram_cache: &self.cache,
            }
        }
    }

    /// Rows erased by `ESC[y;1H ESC[2K` pairs in emitted frame bytes.
    fn erase_rows(bytes: &[u8]) -> Vec<u16> {
        let mut rows = Vec::new();
        for (index, byte) in bytes.iter().enumerate() {
            if *byte != 0x1b || bytes.get(index + 1) != Some(&b'[') {
                continue;
            }
            let tail = &bytes[index + 2..];
            let digits = tail.iter().take_while(|b| b.is_ascii_digit()).count();
            let after = &tail[digits..];
            if digits > 0 && after.starts_with(b";1H\x1b[2K") {
                let row = std::str::from_utf8(&tail[..digits])
                    .unwrap()
                    .parse::<u16>()
                    .unwrap();
                rows.push(row);
            }
        }
        rows
    }

    /// First paint on `io`, leaving an initialized live block.
    fn first_paint(fixture: &Fixture, io: &FakeIo, painter: &mut Painter) {
        painter
            .paint(io, &fixture.state, fixture.input(), false)
            .unwrap();
    }

    fn last_write(io: &FakeIo) -> Vec<u8> {
        io.writes().pop().unwrap_or_default()
    }

    #[test]
    fn shrink_relocates_at_the_measured_top() {
        let fixture = Fixture::new();
        let io = FakeIo::new((80, 24));
        let mut painter = Painter::default();
        first_paint(&fixture, &io, &mut painter);
        io.set_size((80, 16));
        io.bump_epoch();
        io.push_report(crate::term::CursorReport::Position(12, 1));
        painter
            .paint(&io, &fixture.state, fixture.input(), false)
            .unwrap();
        let bytes = last_write(&io);
        assert!(
            erase_rows(&bytes).iter().all(|row| *row >= 12),
            "no erase above the measured top: {:?}",
            erase_rows(&bytes)
        );
        assert!(
            !bytes.windows(3).any(|w| w == b"\x1b[J"),
            "no erase-display: {bytes:?}"
        );
        assert!(
            !bytes.windows(4).any(|w| w == b"\x1b[3J"),
            "no scrollback clear: {bytes:?}"
        );
    }

    #[test]
    fn grow_relocates_without_resurrecting_rows() {
        let fixture = Fixture::new();
        let io = FakeIo::new((80, 16));
        let mut painter = Painter::default();
        first_paint(&fixture, &io, &mut painter);
        io.set_size((80, 32));
        io.bump_epoch();
        io.push_report(crate::term::CursorReport::Position(9, 1));
        painter
            .paint(&io, &fixture.state, fixture.input(), false)
            .unwrap();
        let bytes = last_write(&io);
        assert!(
            erase_rows(&bytes).iter().all(|row| *row >= 9),
            "no erase above the measured top: {:?}",
            erase_rows(&bytes)
        );
    }

    #[test]
    fn epoch_change_at_same_size_still_measures() {
        let fixture = Fixture::new();
        let io = FakeIo::new((80, 24));
        let mut painter = Painter::default();
        first_paint(&fixture, &io, &mut painter);
        let queries_before = io.queries();
        io.bump_epoch();
        io.push_report(crate::term::CursorReport::Position(20, 1));
        painter
            .paint(&io, &fixture.state, fixture.input(), false)
            .unwrap();
        assert_eq!(io.queries(), queries_before + 1);
    }

    #[test]
    fn silent_terminal_holds_then_retries() {
        let fixture = Fixture::new();
        let io = FakeIo::new((80, 24));
        let mut painter = Painter::default();
        first_paint(&fixture, &io, &mut painter);
        io.set_size((80, 16));
        io.bump_epoch();
        let writes_before = io.writes().len();
        painter
            .paint(&io, &fixture.state, fixture.input(), false)
            .unwrap();
        assert_eq!(
            io.writes().len(),
            writes_before,
            "a held frame writes nothing"
        );
        io.push_report(crate::term::CursorReport::Position(12, 1));
        painter
            .paint(&io, &fixture.state, fixture.input(), false)
            .unwrap();
        assert!(erase_rows(&last_write(&io)).iter().all(|r| *r >= 12));
    }

    #[test]
    fn unpaired_answer_holds_and_next_resize_asks_again() {
        let fixture = Fixture::new();
        let io = FakeIo::new((80, 24));
        let mut painter = Painter::default();
        first_paint(&fixture, &io, &mut painter);
        io.bump_epoch();
        io.push_report(crate::term::CursorReport::Unpaired);
        let writes_before = io.writes().len();
        painter
            .paint(&io, &fixture.state, fixture.input(), false)
            .unwrap();
        assert_eq!(io.writes().len(), writes_before);
        io.push_report(crate::term::CursorReport::Position(21, 1));
        let queries_before = io.queries();
        let writes_held = io.writes().len();
        painter
            .paint(&io, &fixture.state, fixture.input(), false)
            .unwrap();
        assert_eq!(
            io.queries(),
            queries_before + 1,
            "held frame measures again"
        );
        let bytes = last_write(&io);
        assert!(!bytes.is_empty(), "a paired answer paints");
        assert_eq!(
            io.writes().len(),
            writes_held + 1,
            "a paired answer writes exactly one frame"
        );
        let scroll = b"\x1b[24;1H\r\n";
        let split = bytes
            .windows(scroll.len())
            .position(|w| w == scroll)
            .expect("overflow scrolls");
        let pre = erase_rows(&bytes[..split]);
        assert!(pre.contains(&21), "pre-scroll clears measured top: {pre:?}");
        assert!(
            pre.iter().all(|row| *row >= 21),
            "pre-scroll touches nothing above measured top: {pre:?}"
        );
        let anchor = painter.anchor;
        let height = 24_usize;
        assert!(anchor <= 21, "scroll moves block up: anchor {anchor}");
        assert!(
            anchor + painter.previous.len().saturating_sub(1) <= height,
            "scrolled block fits: anchor {anchor}, rows {}, height {height}",
            painter.previous.len()
        );
        let post = erase_rows(&bytes[split..]);
        assert!(
            post.iter()
                .all(|row| usize::from(*row) >= anchor && usize::from(*row) <= height),
            "post-scroll repaint stays in scrolled block: {post:?} anchor {anchor}"
        );
        assert!(
            !bytes.windows(3).any(|w| w == b"\x1b[J"),
            "no erase-display: {bytes:?}"
        );
        assert!(
            !bytes.windows(4).any(|w| w == b"\x1b[3J"),
            "no scrollback clear: {bytes:?}"
        );
    }

    #[test]
    fn overlay_entry_erases_live_block_and_parks_saved_caret() {
        let fixture = Fixture::new();
        let io = FakeIo::new((80, 24));
        let mut painter = Painter::default();
        first_paint(&fixture, &io, &mut painter);
        let anchor = u16::try_from(painter.anchor).unwrap();
        painter
            .paint(&io, &fixture.state, fixture.input(), true)
            .unwrap();
        let bytes = last_write(&io);
        let enter = bytes
            .windows(8)
            .position(|w| w == b"\x1b[?1049h")
            .expect("alt screen entered");
        let erases = erase_rows(&bytes[..enter]);
        assert!(
            erases.iter().all(|row| *row >= anchor),
            "entry erase stays inside the live block: {erases:?}"
        );
        assert!(erases.contains(&anchor));
        let park = format!("\x1b[{anchor};1H").into_bytes();
        let park_at = bytes
            .windows(park.len())
            .position(|w| w == park)
            .expect("saved caret parked at the erased top");
        assert!(park_at < enter);
        assert!(
            bytes.windows(6).any(|w| w == b"\x1b[1;1H"),
            "alt caret parked at home: {bytes:?}"
        );
    }

    #[test]
    fn overlay_exit_measures_the_restored_primary() {
        let fixture = Fixture::new();
        let io = FakeIo::new((80, 24));
        let mut painter = Painter::default();
        first_paint(&fixture, &io, &mut painter);
        painter
            .paint(&io, &fixture.state, fixture.input(), true)
            .unwrap();
        io.set_size((72, 26));
        io.bump_epoch();
        painter
            .paint(&io, &fixture.state, fixture.input(), true)
            .unwrap();
        io.push_report(crate::term::CursorReport::Position(24, 1));
        painter
            .paint(&io, &fixture.state, fixture.input(), false)
            .unwrap();
        let writes = io.writes();
        let exit_write = writes.get(writes.len() - 2).cloned().unwrap_or_default();
        let repaint = writes.last().cloned().unwrap_or_default();
        assert_eq!(exit_write, b"\x1b[r\x1b[?1049l");
        assert_eq!(io.queries(), 1, "one query after ?1049l");
        let scroll = b"\x1b[26;1H\r\n";
        let split = repaint
            .windows(scroll.len())
            .position(|w| w == scroll)
            .expect("restore scrolls");
        let pre = erase_rows(&repaint[..split]);
        assert!(pre.contains(&24), "pre-scroll clears measured row: {pre:?}");
        assert!(
            pre.iter().all(|row| *row >= 24),
            "pre-scroll touches nothing above measured row: {pre:?}"
        );
        let height = usize::from(io.size().unwrap().1);
        let anchor = painter.anchor;
        assert!(
            anchor <= 24,
            "scroll moves restored block up: anchor {anchor}"
        );
        assert!(
            anchor + painter.previous.len().saturating_sub(1) <= height,
            "restored block fits: anchor {anchor}, rows {}, height {height}",
            painter.previous.len()
        );
        let post = erase_rows(&repaint[split..]);
        assert!(
            post.iter()
                .all(|row| usize::from(*row) >= anchor && usize::from(*row) <= height),
            "post-scroll repaint stays in scrolled block: {post:?} anchor {anchor}"
        );
        assert!(
            !repaint.windows(3).any(|w| w == b"\x1b[J"),
            "no erase-display"
        );
        assert!(
            !repaint.windows(4).any(|w| w == b"\x1b[3J"),
            "no scrollback clear"
        );
    }

    #[test]
    fn overlay_exit_at_unchanged_size_falls_back_to_parked_anchor_when_silent() {
        let fixture = Fixture::new();
        let io = FakeIo::new((80, 24));
        let mut painter = Painter::default();
        first_paint(&fixture, &io, &mut painter);
        let parked_anchor = u16::try_from(painter.anchor).expect("anchor fits u16");
        painter
            .paint(&io, &fixture.state, fixture.input(), true)
            .unwrap();
        let writes_before = io.writes().len();
        // No resize occurred while the overlay was open, and the DSR query is silent.
        painter
            .paint(&io, &fixture.state, fixture.input(), false)
            .unwrap();
        let writes = io.writes();
        assert_eq!(
            writes.len(),
            writes_before + 2,
            "leave_overlay writes ?1049l then repaints at parked anchor"
        );
        let repaint = writes.last().expect("repaint write");
        let erases = erase_rows(repaint);
        assert!(erases.contains(&parked_anchor));
        assert!(erases.iter().all(|row| *row >= parked_anchor));
    }

    #[test]
    fn hidden_caret_parks_at_block_top_so_offset_is_zero() {
        let fixture = Fixture::new();
        let io = FakeIo::new((80, 24));
        let mut painter = Painter::default();
        first_paint(&fixture, &io, &mut painter);
        let anchor = u16::try_from(painter.anchor).unwrap();
        let bytes = last_write(&io);
        let park = format!("\x1b[{anchor};1H").into_bytes();
        assert!(
            bytes.windows(park.len()).any(|w| w == park),
            "hidden caret parks at the block top {anchor}: {bytes:?}"
        );
        assert_eq!(painter.caret_offset, 0);
    }

    #[test]
    fn shrink_that_overflows_the_screen_scrolls_the_block_up_instead_of_off_it() {
        let fixture = Fixture::new();
        let io = FakeIo::new((80, 24));
        let mut painter = Painter::default();
        first_paint(&fixture, &io, &mut painter);
        io.set_size((80, 10));
        io.bump_epoch();
        // The terminal keeps the caret on its last row, so the measured top is
        // the bottom row and the block cannot fit below it.
        io.push_report(crate::term::CursorReport::Position(10, 1));
        painter
            .paint(&io, &fixture.state, fixture.input(), false)
            .unwrap();
        let rows = painter.previous.len();
        assert!(rows > 1, "the block spans several rows");
        assert_eq!(
            painter.anchor,
            10 - rows + 1,
            "the block ends on the last screen row"
        );
        assert!(
            erase_rows(&last_write(&io)).iter().all(|row| *row <= 10),
            "no row is painted below the screen: {:?}",
            erase_rows(&last_write(&io))
        );
    }

    #[test]
    fn exit_rest_parks_the_caret_on_the_last_block_row() {
        let fixture = Fixture::new();
        let io = FakeIo::new((80, 24));
        let mut painter = Painter::default();
        painter.rest_below_block(&io).unwrap();
        assert!(io.writes().is_empty(), "nothing is painted before a frame");
        first_paint(&fixture, &io, &mut painter);
        let writes = io.writes().len();
        painter.rest_below_block(&io).unwrap();
        let last_row = painter.anchor + painter.previous.len() - 1;
        assert_ne!(
            last_row, painter.anchor,
            "the empty composer hides the caret at the block top"
        );
        assert_eq!(io.writes().len(), writes + 1);
        assert_eq!(last_write(&io), format!("\x1b[{last_row};1H").into_bytes());
        // A resize after the last frame leaves the block unmeasured: the
        // bottom row stands in for it.
        io.set_size((80, 12));
        painter.rest_below_block(&io).unwrap();
        assert_eq!(last_write(&io), b"\x1b[12;1H");
    }

    #[test]
    fn approval_dialog_rows_emit_no_payload_terminal_controls() {
        let theme = crate::theme::load(
            &crate::ThemeRequest::Palette,
            crate::ColorMode::Never,
            None,
            None,
        )
        .expect("palette theme loads");
        let mut dialog = crate::dialog::DialogUi::default();
        dialog.resync(vec![dal_core::Request {
            id: dal_core::RequestId::new_v7(),
            turn: None,
            owner: dal_core::Owner::Core,
            question: dal_core::Question::Approval {
                tool: "exec".into(),
                preview: dal_core::Preview {
                    title: "command".into(),
                    body: "run: \u{202e}rm -rf /\u{202c}\u{1b}\u{7}\n\u{2066}ok\u{2069}".into(),
                    digest: None,
                },
                grant: None,
                call: None,
            },
            timeout: std::time::Duration::from_secs(30),
            default: dal_core::Answer::Decline,
        }]);
        let rows = dialog.rendered_rows(
            80,
            24,
            crate::WidthMode::Narrow,
            crate::diagram::DiagramSettings::default(),
            &crate::diagram::RenderCache::default(),
        );
        assert_ne!(rows.len(), 0, "the dialog renders rows");
        let mut output = Vec::new();
        for row in &rows {
            super::write_styled_text(
                &mut output,
                &row.text,
                &row.spans,
                &row.links,
                row.role,
                &theme,
            );
        }
        let output = String::from_utf8(output).expect("terminal output is UTF-8");
        assert!(
            output.contains("run: \\u{202e}rm -rf /\\u{202c}\\u{1b}\\a"),
            "the override stays visible in logical order: {output}"
        );
        assert!(output.contains("\\u{2066}ok\\u{2069}"));
        assert!(
            !output.contains('\u{202e}'),
            "no bidi control reaches the terminal"
        );
        assert!(
            !output.contains('\u{2066}'),
            "no isolate reaches the terminal"
        );
        assert!(!output.contains('\u{7}'), "BEL never reaches the terminal");
        assert!(
            !output.contains("\u{1b}]"),
            "no OSC sequence opens from a payload: {output}"
        );
    }

    /// Extracts the row from an `ESC[<row>;<col>H` or `ESC[<row>H` sequence.
    fn parse_csi_row(tail: &[u8]) -> Option<u16> {
        let sep = tail.iter().position(|&b| b == b';' || b == b'H')?;
        let row_digits = &tail[..sep];
        if row_digits.is_empty() || !row_digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        let after_sep = &tail[sep..];
        let h_pos = after_sep.iter().position(|&b| b == b'H')?;
        let col_digits = &after_sep[1..h_pos];
        if sep != h_pos && !col_digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(row_digits).ok()?.parse().ok()
    }

    /// Row numbers referenced by cursor positioning (`ESC[<row>;<col>H`)
    /// or line erase (`ESC[<row>;1H ESC[2K`) sequences in emitted bytes.
    fn targeted_rows(bytes: &[u8]) -> Vec<u16> {
        let mut rows = Vec::new();
        for (index, window) in bytes.windows(2).enumerate() {
            if window != b"\x1b[" {
                continue;
            }
            if let Some(row) = parse_csi_row(&bytes[index + 2..]) {
                rows.push(row);
            }
        }
        rows
    }

    fn test_reasoning_entry(id: u64, text: &str) -> dal_core::EntryView {
        dal_core::EntryView {
            id: dal_core::EntryId::new(std::num::NonZeroU64::new(id).unwrap()),
            parent: None,
            kind: dal_core::EntryKind::Assistant {
                api: dal_core::Family::Chat,
                model: "model".into(),
                content: vec![dal_core::Block::Reasoning {
                    text: text.into(),
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

    #[test]
    fn newest_card_expansion_emits_zero_writes_above_live_block_top() {
        let mut fixture = Fixture::new();

        // 1. Setup committed transcript rows.
        let committed_texts = vec![
            "committed transcript line 1".to_string(),
            "committed transcript line 2".to_string(),
            "committed transcript line 3".to_string(),
            "committed transcript line 4".to_string(),
        ];
        fixture.transcript.commit("entry-0", &committed_texts);

        // 2. Setup an assistant entry with a reasoning block (eligible for card expansion).
        let assistant_entry = test_reasoning_entry(
            1,
            "step one: analyzing problem\nstep two: calculating proof\nstep three: verifying bounds",
        );
        fixture.view.entries.items.push(assistant_entry);

        let io = FakeIo::new((80, 24));
        let mut painter = Painter::default();

        // Initial paint: commits the transcript rows and draws the initial collapsed live block.
        first_paint(&fixture, &io, &mut painter);

        let anchor_before = painter.anchor;
        assert!(
            anchor_before > 1,
            "anchor before expand must leave room for committed transcript"
        );
        assert_eq!(painter.committed, committed_texts.len());

        let writes_before = io.writes().len();

        // Expand the newest card (Ctrl+O reasoning card)
        let newest_entry = fixture
            .view
            .entries
            .items
            .last()
            .expect("assistant entry exists");
        let card =
            crate::render::expanded_card(newest_entry, crate::diagram::DiagramSettings::default())
                .expect("reasoning card expands");
        assert!(matches!(card, crate::live::ExpandedCard::Reasoning { .. }));
        let blob_ids = crate::render::expanded_blob_ids(newest_entry, card);
        fixture.live.open_expand(card, blob_ids);

        // Render the expanded frame in place
        painter
            .paint(&io, &fixture.state, fixture.input(), false)
            .unwrap();

        let writes_after = io.writes();
        assert_eq!(
            writes_after.len(),
            writes_before + 1,
            "expand paint emitted exactly one frame write"
        );
        let expand_bytes = writes_after.last().unwrap();

        let live_block_top = u16::try_from(painter.anchor).unwrap();
        let targeted = targeted_rows(expand_bytes);

        assert!(!targeted.is_empty(), "expand frame emitted targeted rows");

        // Byte-boundary proof: every targeted row is at or below the live block top.
        // There are ZERO writes to rows < live block top.
        for &row in &targeted {
            assert!(
                row >= live_block_top,
                "emitted write to row {row} < live block top {live_block_top}; all targeted: {targeted:?}"
            );
        }

        // Erased rows must also be strictly at or below live block top.
        let erases = erase_rows(expand_bytes);
        for &row in &erases {
            assert!(
                row >= live_block_top,
                "emitted erase to row {row} < live block top {live_block_top}; all erases: {erases:?}"
            );
        }

        // None of the committed transcript text strings may appear in the expand frame bytes.
        for committed in &committed_texts {
            assert!(
                !expand_bytes
                    .windows(committed.len())
                    .any(|w| w == committed.as_bytes()),
                "committed transcript row '{committed}' was repainted during card expand"
            );
        }

        // No erase-display or scrollback clear controls
        assert!(!expand_bytes.windows(3).any(|w| w == b"\x1b[J"));
        assert!(
            !expand_bytes
                .windows(4)
                .any(|w| w == b"\x1b[2J" || w == b"\x1b[3J")
        );

        // Verify that the expanded card content was actually rendered into the frame.
        let expand_output = String::from_utf8_lossy(expand_bytes);
        assert!(
            expand_output.contains("step one") || expand_output.contains("Thinking"),
            "expanded card content was rendered in the live block: {expand_output}"
        );
    }

    #[test]
    fn newest_card_expansion_overflow_scroll_preserves_committed_rows() {
        let mut fixture = Fixture::new();

        // Many committed rows so the live block starts near the bottom of the screen.
        let mut committed_texts = Vec::new();
        for i in 1..=14 {
            committed_texts.push(format!("committed transcript line {i}"));
        }
        fixture.transcript.commit("entry-0", &committed_texts);

        let long_reasoning = (1..=20)
            .map(|i| format!("step {i}: long reasoning step detailing execution"))
            .collect::<Vec<_>>()
            .join("\n");
        let assistant_entry = test_reasoning_entry(1, &long_reasoning);
        fixture.view.entries.items.push(assistant_entry);

        let io = FakeIo::new((80, 24));
        let mut painter = Painter::default();

        first_paint(&fixture, &io, &mut painter);

        let writes_before = io.writes().len();

        let newest_entry = fixture
            .view
            .entries
            .items
            .last()
            .expect("assistant entry exists");
        let card =
            crate::render::expanded_card(newest_entry, crate::diagram::DiagramSettings::default())
                .expect("reasoning card expands");
        let blob_ids = crate::render::expanded_blob_ids(newest_entry, card);
        fixture.live.open_expand(card, blob_ids);

        painter
            .paint(&io, &fixture.state, fixture.input(), false)
            .unwrap();

        let writes_after = io.writes();
        assert_eq!(writes_after.len(), writes_before + 1);
        let expand_bytes = writes_after.last().unwrap();

        let live_block_top = u16::try_from(painter.anchor).unwrap();
        let targeted = targeted_rows(expand_bytes);

        assert_ne!(targeted, Vec::<u16>::new());
        for &row in &targeted {
            assert!(
                row >= live_block_top,
                "emitted write to row {row} < live block top {live_block_top}; all targeted: {targeted:?}"
            );
        }

        let erases = erase_rows(expand_bytes);
        for &row in &erases {
            assert!(
                row >= live_block_top,
                "emitted erase to row {row} < live block top {live_block_top}; all erases: {erases:?}"
            );
        }

        for committed in &committed_texts {
            assert!(
                !expand_bytes
                    .windows(committed.len())
                    .any(|w| w == committed.as_bytes()),
                "committed transcript row '{committed}' was repainted during card expand"
            );
        }

        assert!(!expand_bytes.windows(3).any(|w| w == b"\x1b[J"));
        assert!(
            !expand_bytes
                .windows(4)
                .any(|w| w == b"\x1b[2J" || w == b"\x1b[3J")
        );
    }

    #[test]
    fn newest_card_collapse_emits_zero_writes_above_live_block_top() {
        let mut fixture = Fixture::new();

        let committed_texts = vec![
            "committed transcript line 1".to_string(),
            "committed transcript line 2".to_string(),
        ];
        fixture.transcript.commit("entry-0", &committed_texts);

        let assistant_entry = test_reasoning_entry(1, "step one\nstep two");
        fixture.view.entries.items.push(assistant_entry);

        let io = FakeIo::new((80, 24));
        let mut painter = Painter::default();

        first_paint(&fixture, &io, &mut painter);

        // Expand
        let newest_entry = fixture
            .view
            .entries
            .items
            .last()
            .expect("assistant entry exists");
        let card =
            crate::render::expanded_card(newest_entry, crate::diagram::DiagramSettings::default())
                .expect("reasoning card expands");
        let blob_ids = crate::render::expanded_blob_ids(newest_entry, card);
        fixture.live.open_expand(card, blob_ids);
        painter
            .paint(&io, &fixture.state, fixture.input(), false)
            .unwrap();

        // Collapse (Ctrl+O again)
        let writes_before = io.writes().len();
        assert!(fixture.live.close_expand());
        painter
            .paint(&io, &fixture.state, fixture.input(), false)
            .unwrap();

        let writes_after = io.writes();
        assert_eq!(writes_after.len(), writes_before + 1);
        let collapse_bytes = writes_after.last().unwrap();

        let live_block_top = u16::try_from(painter.anchor).unwrap();
        let targeted = targeted_rows(collapse_bytes);

        assert_ne!(targeted, Vec::<u16>::new());
        for &row in &targeted {
            assert!(
                row >= live_block_top,
                "emitted write to row {row} < live block top {live_block_top}; all targeted: {targeted:?}"
            );
        }

        for committed in &committed_texts {
            assert!(
                !collapse_bytes
                    .windows(committed.len())
                    .any(|w| w == committed.as_bytes()),
                "committed transcript row '{committed}' was repainted during card collapse"
            );
        }

        assert!(!collapse_bytes.windows(3).any(|w| w == b"\x1b[J"));
        assert!(
            !collapse_bytes
                .windows(4)
                .any(|w| w == b"\x1b[2J" || w == b"\x1b[3J")
        );
    }

    /// Scrolling terminal that replays the painter's bytes: a main and an
    /// alternate screen, scrollback, deferred wrap, and the controls the
    /// painter writes. A resize keeps the caret row on screen, so rows above
    /// it scroll into the scrollback and rows below it fall off the bottom;
    /// `pulls` also lets a grow take scrollback rows back, like xterm. Rows
    /// clip at the new width, never reflow.
    struct Sim {
        columns: usize,
        rows: usize,
        main: Vec<Vec<char>>,
        alt: Option<Vec<Vec<char>>>,
        scrollback: Vec<String>,
        row: usize,
        column: usize,
        wrap_pending: bool,
        saved: (usize, usize),
        pulls: bool,
        erase_display: u32,
    }

    impl Sim {
        fn new((columns, rows): (u16, u16), pulls: bool) -> Self {
            let (columns, rows) = (usize::from(columns), usize::from(rows));
            Self {
                columns,
                rows,
                main: vec![vec![' '; columns]; rows],
                alt: None,
                scrollback: Vec::new(),
                row: 0,
                column: 0,
                wrap_pending: false,
                saved: (0, 0),
                pulls,
                erase_display: 0,
            }
        }

        fn grid(&mut self) -> &mut Vec<Vec<char>> {
            self.alt.as_mut().unwrap_or(&mut self.main)
        }

        fn text(line: &[char]) -> String {
            line.iter().collect::<String>().trim_end().to_owned()
        }

        /// The rows a person sees: the alternate screen while it is open.
        fn visible(&self) -> Vec<String> {
            self.alt
                .as_ref()
                .unwrap_or(&self.main)
                .iter()
                .map(|line| Self::text(line))
                .collect()
        }

        fn line_feed(&mut self) {
            if self.row + 1 < self.rows {
                self.row += 1;
                return;
            }
            let columns = self.columns;
            let top = self.grid().remove(0);
            self.grid().push(vec![' '; columns]);
            if self.alt.is_none() {
                self.scrollback.push(Self::text(&top));
            }
        }

        fn put(&mut self, c: char) {
            if self.wrap_pending {
                self.column = 0;
                self.line_feed();
                self.wrap_pending = false;
            }
            let (row, column) = (self.row, self.column);
            self.grid()[row][column] = c;
            if column + 1 == self.columns {
                self.wrap_pending = true;
            } else {
                self.column += 1;
            }
        }

        fn clear(&mut self, row: usize, columns: std::ops::Range<usize>) {
            for cell in &mut self.grid()[row][columns] {
                *cell = ' ';
            }
        }

        fn csi(&mut self, params: &str, last: char) {
            self.wrap_pending = false;
            if params.starts_with('?') {
                match (params, last) {
                    ("?1049", 'h') => {
                        self.saved = (self.row, self.column);
                        self.alt = Some(vec![vec![' '; self.columns]; self.rows]);
                    }
                    ("?1049", 'l') => {
                        self.alt = None;
                        self.row = self.saved.0.min(self.rows - 1);
                        self.column = self.saved.1.min(self.columns - 1);
                    }
                    _ => {}
                }
                return;
            }
            let nums: Vec<usize> = params.split(';').map(|p| p.parse().unwrap_or(0)).collect();
            let arg = |index: usize| nums.get(index).copied().filter(|n| *n > 0).unwrap_or(1);
            let (last_row, last_column) = (self.rows - 1, self.columns - 1);
            match last {
                'H' | 'f' => {
                    self.row = (arg(0) - 1).min(last_row);
                    self.column = (arg(1) - 1).min(last_column);
                }
                'A' => self.row = self.row.saturating_sub(arg(0)),
                'B' => self.row = (self.row + arg(0)).min(last_row),
                'C' => self.column = (self.column + arg(0)).min(last_column),
                'D' => self.column = self.column.saturating_sub(arg(0)),
                'K' => {
                    let (row, column) = (self.row, self.column);
                    match nums[0] {
                        0 => self.clear(row, column..self.columns),
                        1 => self.clear(row, 0..column + 1),
                        _ => self.clear(row, 0..self.columns),
                    }
                }
                'J' => {
                    self.erase_display += 1;
                    for row in 0..self.rows {
                        self.clear(row, 0..self.columns);
                    }
                }
                _ => {}
            }
        }

        fn feed(&mut self, bytes: &[u8]) {
            let text = String::from_utf8_lossy(bytes);
            let mut chars = text.chars().peekable();
            while let Some(c) = chars.next() {
                match c {
                    '\x1b' => match chars.next() {
                        Some('[') => {
                            let mut params = String::new();
                            while let Some(next) = chars.next_if(|c| ('\x20'..='\x3f').contains(c))
                            {
                                params.push(next);
                            }
                            if let Some(last) = chars.next() {
                                self.csi(&params, last);
                            }
                        }
                        Some(']') => {
                            while let Some(next) = chars.next() {
                                if next == '\x07' || (next == '\x1b' && chars.next().is_some()) {
                                    break;
                                }
                            }
                        }
                        _ => {}
                    },
                    '\r' => {
                        self.column = 0;
                        self.wrap_pending = false;
                    }
                    '\n' => {
                        self.wrap_pending = false;
                        self.line_feed();
                    }
                    c if !c.is_control() => self.put(c),
                    _ => {}
                }
            }
        }

        /// Fits the main buffer to `rows`, keeping the `caret` row on screen,
        /// and returns the caret row's new position.
        fn refit(&mut self, caret: usize, rows: usize, columns: usize) -> usize {
            let pulls = self.pulls;
            let blank = vec![' '; columns];
            let scrollback = &mut self.scrollback;
            let grid = &mut self.main;
            let height = grid.len();
            let mut caret = caret;
            if rows < height {
                let dropped = (caret + 1).saturating_sub(rows);
                for line in grid.drain(..dropped) {
                    scrollback.push(Self::text(&line));
                }
                caret -= dropped;
                grid.truncate(rows);
            } else if rows > height {
                let pulled = if pulls {
                    scrollback.len().min(rows - height)
                } else {
                    0
                };
                let from = scrollback.len() - pulled;
                let restored: Vec<Vec<char>> = scrollback
                    .drain(from..)
                    .map(|line| {
                        let mut cells: Vec<char> = line.chars().collect();
                        cells.resize(columns, ' ');
                        cells
                    })
                    .collect();
                grid.splice(..0, restored);
                caret += pulled;
                grid.resize_with(rows, || blank.clone());
            }
            for line in grid.iter_mut() {
                line.resize(columns, ' ');
            }
            caret
        }

        fn resize(&mut self, (columns, rows): (u16, u16)) {
            let (columns, rows) = (usize::from(columns).max(1), usize::from(rows).max(1));
            if self.alt.is_some() {
                self.saved.0 = self.refit(self.saved.0, rows, columns);
                self.alt = Some(vec![vec![' '; columns]; rows]);
            } else {
                self.row = self.refit(self.row, rows, columns);
            }
            self.columns = columns;
            self.rows = rows;
            self.row = self.row.min(rows - 1);
            self.column = self.column.min(columns - 1);
            self.saved = (self.saved.0.min(rows - 1), self.saved.1.min(columns - 1));
            self.wrap_pending = false;
        }
    }

    /// `TermIo` over a [`Sim`]: bytes replay into the emulator, and a caret
    /// query answers with the emulator's real caret.
    struct SimIo {
        sim: Mutex<Sim>,
        epoch: Mutex<u64>,
    }

    impl SimIo {
        fn new(size: (u16, u16), pulls: bool) -> Self {
            Self {
                sim: Mutex::new(Sim::new(size, pulls)),
                epoch: Mutex::new(0),
            }
        }

        fn resize(&self, size: (u16, u16)) {
            self.sim.lock().unwrap().resize(size);
            *self.epoch.lock().unwrap() += 1;
        }

        fn visible(&self) -> Vec<String> {
            self.sim.lock().unwrap().visible()
        }

        fn scrollback(&self) -> Vec<String> {
            self.sim.lock().unwrap().scrollback.clone()
        }
    }

    impl crate::term::TermIo for SimIo {
        fn enable_raw(&self) -> std::io::Result<()> {
            Ok(())
        }
        fn disable_raw(&self) {}
        fn size(&self) -> std::io::Result<(u16, u16)> {
            let sim = self.sim.lock().unwrap();
            Ok((
                u16::try_from(sim.columns).unwrap(),
                u16::try_from(sim.rows).unwrap(),
            ))
        }
        fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
            self.sim.lock().unwrap().feed(bytes);
            Ok(())
        }
        fn read(&self, _timeout: std::time::Duration) -> std::io::Result<Vec<u8>> {
            Ok(Vec::new())
        }
        fn raise_tstp(&self) {}
        fn resize_epoch(&self) -> u64 {
            *self.epoch.lock().unwrap()
        }
        fn cursor_position(
            &self,
            _timeout: std::time::Duration,
        ) -> std::io::Result<crate::term::CursorReport> {
            let sim = self.sim.lock().unwrap();
            Ok(crate::term::CursorReport::Position(
                u16::try_from(sim.row + 1).unwrap(),
                u16::try_from(sim.column + 1).unwrap(),
            ))
        }
    }

    fn paint_on(io: &SimIo, fixture: &Fixture, painter: &mut Painter) {
        painter
            .paint(io, &fixture.state, fixture.input(), false)
            .unwrap();
    }

    /// Paints with a non-empty composer, so the caret stays visible on the
    /// composer row and later resizes must relocate with a nonzero offset.
    fn paint_with_composer(
        io: &SimIo,
        fixture: &Fixture,
        painter: &mut Painter,
        composer: &str,
        cursor: usize,
    ) {
        let input = fixture.input_with_composer(composer, cursor);
        painter.paint(io, &fixture.state, input, false).unwrap();
    }

    fn paint_overlay_draft(io: &SimIo, fixture: &Fixture, painter: &mut Painter, composer: &str) {
        let input = fixture.input_with_composer(composer, 12);
        painter.paint(io, &fixture.state, input, true).unwrap();
    }

    /// Opens the transcript overlay, resizes while on the alternate screen,
    /// settles new rows behind it, then exits back to the main screen.
    fn overlay_resize_sequence(sizes: [(u16, u16); 3], pulls: bool) -> Result<(), String> {
        let mut fixture = Fixture::new();
        let mut committed = committed_rows(8);
        fixture.transcript.commit("entry-0", &committed);
        assert!(
            fixture
                .live
                .apply_update(&tool_started_update("call-1", "exec"))
        );
        fixture.live.notice("local note");
        let io = SimIo::new(sizes[0], pulls);
        let mut painter = Painter::default();
        let draft = "draft preserved across overlay resize";
        paint_with_composer(&io, &fixture, &mut painter, draft, 12);
        let mut faults = live_block_faults(&io, &painter, &committed);
        paint_overlay_draft(&io, &fixture, &mut painter, draft);
        for (step, size) in sizes[1..].iter().enumerate() {
            io.resize(*size);
            let more = vec![format!("settled behind overlay {}", step + 1)];
            fixture
                .transcript
                .commit(&format!("entry-{}", step + 1), &more);
            committed.extend(more);
            paint_overlay_draft(&io, &fixture, &mut painter, draft);
        }
        paint_with_composer(&io, &fixture, &mut painter, draft, 12);
        for fault in live_block_faults(&io, &painter, &committed) {
            faults.push(format!("after overlay exit at {:?}: {fault}", sizes[2]));
        }
        if faults.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "overlay sizes {sizes:?}, pulls {pulls}: {faults:?}
{}",
                dump(&io)
            ))
        }
    }

    /// Shrink then grow with a visible caret mid-block: the composer holds a
    /// draft, a tool works above it, and entries settle across the resizes.
    fn visible_caret_sequence(sizes: [(u16, u16); 3], pulls: bool) -> Result<(), String> {
        let mut fixture = Fixture::new();
        let mut committed = committed_rows(10);
        fixture.transcript.commit("entry-0", &committed);
        assert!(
            fixture
                .live
                .apply_update(&tool_started_update("call-1", "exec"))
        );
        fixture.live.notice("local note");
        let io = SimIo::new(sizes[0], pulls);
        let mut painter = Painter::default();
        let draft = "start typing a draft here";
        paint_with_composer(&io, &fixture, &mut painter, draft, 10);
        if painter.caret_offset == 0 {
            return Err(format!(
                "composer caret stayed hidden, the sequence proves nothing\n{}",
                dump(&io)
            ));
        }
        let mut faults = live_block_faults(&io, &painter, &committed);
        for (step, size) in sizes[1..].iter().enumerate() {
            io.resize(*size);
            let more = vec![format!("settled across resize {}", step + 1)];
            fixture
                .transcript
                .commit(&format!("entry-{}", step + 1), &more);
            committed.extend(more);
            paint_with_composer(&io, &fixture, &mut painter, draft, 10);
            for fault in live_block_faults(&io, &painter, &committed) {
                faults.push(format!("after resize to {size:?}: {fault}"));
            }
        }
        if faults.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "sizes {sizes:?}, pulls {pulls}: {faults:?}\n{}",
                dump(&io)
            ))
        }
    }

    /// Occurrences of `row` across the visible screen and the scrollback.
    fn occurrences(io: &SimIo, row: &str) -> (usize, usize) {
        let on_screen = io.visible().iter().filter(|line| *line == row).count();
        let in_scrollback = io.scrollback().iter().filter(|line| *line == row).count();
        (on_screen, in_scrollback)
    }

    /// Violations of the one-live-block contract on the emulated terminal:
    /// the block's last row (the status line) shows exactly once on screen,
    /// at the row the painter's anchor predicts, and every committed row
    /// exists exactly once on screen or in scrollback.
    fn live_block_faults(io: &SimIo, painter: &Painter, committed: &[String]) -> Vec<String> {
        let mut faults = Vec::new();
        let status = painter
            .previous
            .last()
            .map(|row| row.text.trim_end().to_owned());
        if let Some(status) = status {
            let (screen, scrollback) = occurrences(io, &status);
            if screen != 1 {
                faults.push(format!("live status row shows {screen} times on screen"));
            }
            if scrollback != 0 {
                faults.push(format!(
                    "live status row reached scrollback {scrollback} times"
                ));
            }
            let status_row = io
                .visible()
                .iter()
                .rposition(|line| *line == status)
                .map(|index| index + 1);
            let expected = painter.anchor + painter.previous.len().saturating_sub(1);
            if status_row != Some(expected) {
                faults.push(format!(
                    "live status row on screen at {status_row:?}, anchor predicts {expected}"
                ));
            }
        }
        for row in committed {
            let (screen, scrollback) = occurrences(io, row);
            if screen + scrollback != 1 {
                faults.push(format!(
                    "committed {row:?} shows {screen} on screen and {scrollback} in scrollback"
                ));
            }
        }
        faults
    }

    fn committed_rows(count: usize) -> Vec<String> {
        (1..=count)
            .map(|n| format!("committed transcript line {n}"))
            .collect()
    }

    fn dump(io: &SimIo) -> String {
        io.visible()
            .iter()
            .enumerate()
            .map(|(index, line)| format!("{:>2}|{line}", index + 1))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn delta_update(text: &str) -> dal_core::Update {
        use std::num::NonZeroU64;
        dal_core::Update {
            r#gen: dal_core::Gen::new(NonZeroU64::MIN),
            seq: dal_core::Seq::new(NonZeroU64::MIN),
            kind: dal_core::UpdateKind::Delta {
                turn: dal_core::TurnId::new(NonZeroU64::MIN),
                channel: dal_core::StreamChannel::Text,
                text: text.into(),
            },
        }
    }

    fn tool_started_update(call: &str, tool: &str) -> dal_core::Update {
        use std::num::NonZeroU64;
        dal_core::Update {
            r#gen: dal_core::Gen::new(NonZeroU64::MIN),
            seq: dal_core::Seq::new(NonZeroU64::MIN),
            kind: dal_core::UpdateKind::ToolStarted {
                call: dal_core::CallId::new(call),
                tool: tool.into(),
                args: dal_core::RawJson::null(),
            },
        }
    }

    fn pending_row() -> RenderRow {
        RenderRow::new("pending row about to settle", crate::theme::Role::Text)
    }

    /// One shrink then one grow while the live block carries streamed prose,
    /// a working tool, a notice and pending rows, and the transcript keeps
    /// settling entries across the resizes — including one entry that was
    /// pending when the shrink landed.
    fn live_resize_sequence(sizes: [(u16, u16); 3], pulls: bool) -> Result<(), String> {
        let mut fixture = Fixture::new();
        let mut committed = committed_rows(6);
        fixture.transcript.commit("entry-0", &committed);
        for chunk in [
            "streaming line one\n",
            "a second live line with a very long tail that clips at narrow widths xxxxxxxxxx\n",
            "third live line\n",
        ] {
            assert!(fixture.live.apply_update(&delta_update(chunk)));
        }
        assert!(
            fixture
                .live
                .apply_update(&tool_started_update("call-1", "exec"))
        );
        fixture.live.notice("local note");
        fixture
            .transcript
            .set_pending("pending-1", vec![pending_row()]);
        let io = SimIo::new(sizes[0], pulls);
        let mut painter = Painter::default();
        paint_on(&io, &fixture, &mut painter);
        let mut faults = live_block_faults(&io, &painter, &committed);
        io.resize(sizes[1]);
        let shrunk = vec![
            "settled across the shrink 1".to_owned(),
            "settled across the shrink 2".to_owned(),
        ];
        fixture.transcript.commit("entry-1", &shrunk);
        committed.extend(shrunk);
        fixture
            .transcript
            .commit_rendered("pending-1", &[pending_row()]);
        committed.push("pending row about to settle".to_owned());
        paint_on(&io, &fixture, &mut painter);
        for fault in live_block_faults(&io, &painter, &committed) {
            faults.push(format!("after resize to {:?}: {fault}", sizes[1]));
        }
        io.resize(sizes[2]);
        let grown = vec!["settled across the grow".to_owned()];
        fixture.transcript.commit("entry-2", &grown);
        committed.extend(grown);
        paint_on(&io, &fixture, &mut painter);
        for fault in live_block_faults(&io, &painter, &committed) {
            faults.push(format!("after resize to {:?}: {fault}", sizes[2]));
        }
        if faults.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "sizes {sizes:?}, pulls {pulls}: {faults:?}\n{}",
                dump(&io)
            ))
        }
    }

    /// Commits carrying a row longer than the screen, then shrinks and grows:
    /// the wrapped row must not drift the anchor or duplicate the live block.
    /// The overlong text itself is excluded from the once-only check — only
    /// its effect on the live geometry is asserted.
    fn overlong_commit_sequence(sizes: [(u16, u16); 3], pulls: bool) -> Result<(), String> {
        let mut fixture = Fixture::new();
        let mut committed = committed_rows(18);
        fixture.transcript.commit("entry-0", &committed);
        fixture.transcript.commit(
            "entry-1",
            &["x".repeat(100), "after the long row".to_owned()],
        );
        committed.push("after the long row".to_owned());
        let io = SimIo::new(sizes[0], pulls);
        let mut painter = Painter::default();
        paint_on(&io, &fixture, &mut painter);
        let mut faults = live_block_faults(&io, &painter, &committed);
        for size in &sizes[1..] {
            io.resize(*size);
            paint_on(&io, &fixture, &mut painter);
            for fault in live_block_faults(&io, &painter, &committed) {
                faults.push(format!("after resize to {size:?}: {fault}"));
            }
        }
        if faults.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "sizes {sizes:?}, pulls {pulls}: {faults:?}\n{}",
                dump(&io)
            ))
        }
    }

    /// One shrink then one grow on a terminal that follows its caret.
    fn shrink_then_grow(
        committed: usize,
        sizes: [(u16, u16); 3],
        pulls: bool,
    ) -> Result<(), String> {
        let mut fixture = Fixture::new();
        let rows = committed_rows(committed);
        if !rows.is_empty() {
            fixture.transcript.commit("entry-0", &rows);
        }
        let io = SimIo::new(sizes[0], pulls);
        let mut painter = Painter::default();
        paint_on(&io, &fixture, &mut painter);
        let mut faults = live_block_faults(&io, &painter, &rows);
        for size in &sizes[1..] {
            io.resize(*size);
            paint_on(&io, &fixture, &mut painter);
            for fault in live_block_faults(&io, &painter, &rows) {
                faults.push(format!("after resize to {size:?}: {fault}"));
            }
        }
        if faults.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "committed {committed}, sizes {sizes:?}, pulls {pulls}: {faults:?}\n{}",
                dump(&io)
            ))
        }
    }

    /// An external editor can hand back a shorter draft: the invalidated
    /// repaint must still erase every row of the taller stale block.
    #[test]
    fn invalidate_before_a_shorter_block_erases_the_stale_rows() {
        let fixture = Fixture::new();
        let io = SimIo::new((80, 24), true);
        let mut painter = Painter::default();
        let tall = "first line\nsecond line\nthird line\nfourth line";
        paint_with_composer(&io, &fixture, &mut painter, tall, 3);
        let tall_height = painter.previous.len();
        assert!(
            io.visible().iter().any(|line| line.contains("fourth line")),
            "the tall draft must paint, or the test proves nothing\n{}",
            dump(&io)
        );
        painter.invalidate();
        paint_with_composer(&io, &fixture, &mut painter, "first line", 3);
        let stale: Vec<String> = io
            .visible()
            .into_iter()
            .filter(|line| {
                ["second line", "third line", "fourth line"]
                    .iter()
                    .any(|gone| line.contains(gone))
            })
            .collect();
        assert!(
            stale.is_empty(),
            "stale rows survived the shorter block: {stale:?}\n{}",
            dump(&io)
        );
        assert!(painter.previous.len() < tall_height);
        assert_eq!(live_block_faults(&io, &painter, &[]), Vec::<String>::new());
    }

    /// A row mangled behind the painter's back survives a diff paint that
    /// finds every row unchanged; an invalidated paint rewrites it.
    #[test]
    fn invalidate_rewrites_a_row_damaged_behind_the_painter() {
        let fixture = Fixture::new();
        let io = SimIo::new((80, 24), true);
        let mut painter = Painter::default();
        paint_with_composer(&io, &fixture, &mut painter, "kept draft", 4);
        let row = painter.anchor;
        let intact = io.visible()[row - 1].clone();
        io.write(format!("\x1b[{row};1H\x1b[2Kgarbage left by a child process").as_bytes())
            .unwrap();
        assert_ne!(io.visible()[row - 1], intact);
        painter.invalidate();
        paint_with_composer(&io, &fixture, &mut painter, "kept draft", 4);
        assert_eq!(io.visible()[row - 1], intact, "{}", dump(&io));
        assert!(!painter.repaint, "a landed main frame consumes the flag");
        assert_eq!(live_block_faults(&io, &painter, &[]), Vec::<String>::new());
    }

    #[test]
    fn resize_matrix_keeps_one_live_block() {
        let mut failures = Vec::new();
        for pulls in [true, false] {
            for sizes in [
                [(80, 24), (80, 16), (80, 32)],
                [(80, 24), (80, 10), (80, 40)],
                [(80, 24), (80, 12), (80, 24)],
                [(80, 24), (60, 16), (80, 24)],
            ] {
                for committed in [0, 3, 10, 14, 20, 30] {
                    if let Err(failure) = shrink_then_grow(committed, sizes, pulls) {
                        failures.push(failure);
                    }
                }
            }
            for sizes in [
                [(80, 24), (80, 16), (80, 32)],
                [(80, 24), (40, 24), (100, 24)],
                [(80, 24), (60, 12), (100, 40)],
            ] {
                if let Err(failure) = live_resize_sequence(sizes, pulls) {
                    failures.push(failure);
                }
            }
            for sizes in [
                [(80, 24), (80, 16), (80, 32)],
                [(100, 24), (60, 24), (100, 24)],
            ] {
                if let Err(failure) = overlong_commit_sequence(sizes, pulls) {
                    failures.push(failure);
                }
            }
            for sizes in [
                [(80, 24), (80, 16), (80, 32)],
                [(80, 24), (80, 10), (80, 40)],
                [(80, 24), (60, 24), (100, 24)],
            ] {
                if let Err(failure) = visible_caret_sequence(sizes, pulls) {
                    failures.push(failure);
                }
                if let Err(failure) = overlay_resize_sequence(sizes, pulls) {
                    failures.push(failure);
                }
            }
        }
        assert!(
            failures.is_empty(),
            "{}\n\n{}",
            failures.len(),
            failures.join("\n\n")
        );
    }
}
