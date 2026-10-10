//! Damage-only terminal painting with a write-once inline transcript.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use crate::image::Rung;
use crate::render::{FrameInput, PixelImage, RenderLink, RenderRow, RenderSpan, frame_rows};
use crate::term::{TermIo, TermState};
use crate::theme::{ResolvedTheme, Role};
use crate::transcript::Transcript;
use crate::{Screen, TuiError};

/// One paint-thread-owned terminal surface.
#[derive(Default)]
pub(crate) struct Painter {
    previous: Vec<RenderRow>,
    size: (u16, u16),
    committed: usize,
    /// Screen row, 1-based, where the live block starts: directly under the
    /// last committed row, which is also where the next commit lands.
    anchor: usize,
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

/// The main-screen state the painter parks across a transcript overlay.
struct MainFrame {
    previous: Vec<RenderRow>,
    size: (u16, u16),
    initialized: bool,
    anchor: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct ImageKey {
    digest: [u8; 32],
    columns: u16,
    rows: u16,
    rung: Rung,
}

impl Painter {
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

    /// Renders one frame; settled rows are committed before the live block moves.
    pub(crate) fn paint(
        &mut self,
        io: &dyn TermIo,
        state: &Arc<Mutex<TermState>>,
        input: FrameInput<'_>,
        overlay: bool,
    ) -> Result<(), TuiError> {
        let size = io.size().unwrap_or((80, 24));
        let screen = if overlay {
            Screen::Fullscreen
        } else {
            input.screen
        };
        let theme = input.theme;
        let rows = frame_rows(FrameInput { screen, ..input }, size.0, size.1);
        let committed_len = input.transcript.rows().len();
        let mut bytes = Vec::new();
        if overlay != self.overlay {
            if overlay {
                state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .set_transcript_overlay(true);
                bytes.extend_from_slice(b"\x1b[?1049h\x1b[?25l");
                self.main = Some(MainFrame {
                    previous: std::mem::take(&mut self.previous),
                    size: self.size,
                    initialized: self.initialized,
                    anchor: self.anchor,
                });
                self.initialized = false;
            } else {
                bytes.extend_from_slice(b"\x1b[r\x1b[?1049l");
                if let Some(main) = self.main.take() {
                    self.previous = main.previous;
                    self.size = main.size;
                    self.initialized = main.initialized;
                    self.anchor = main.anchor;
                } else {
                    self.previous.clear();
                    self.initialized = false;
                }
            }
            self.overlay = overlay;
        }
        let structural = !self.initialized
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
                theme,
            );
        } else {
            self.fullscreen_bytes(&mut bytes, size, &rows, structural, theme);
        }
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
        self.place_cursor(&mut bytes, size, top, &rows, overlay);
        self.theme_name = Some(theme.name());
        if !overlay {
            // Rows settled behind the overlay commit on the main screen once it returns.
            self.committed = committed_len;
        }
        self.size = size;
        self.previous = rows;
        self.initialized = true;
        if bytes.is_empty() {
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

    /// Moves the terminal caret onto the row that holds it, and hides the caret while
    /// no row does (a dialog or the transcript overlay owns the input).
    fn place_cursor(
        &mut self,
        out: &mut Vec<u8>,
        size: (u16, u16),
        top: usize,
        rows: &[RenderRow],
        overlay: bool,
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
            // Entering the overlay already hid the caret.
            self.cursor = None;
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
        self.cursor = place;
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one paint pass carries every inline render input"
    )]
    fn inline_bytes(
        &mut self,
        out: &mut Vec<u8>,
        size: (u16, u16),
        rows: &[RenderRow],
        transcript: &Transcript,
        commit_start: usize,
        structural: bool,
        theme: &ResolvedTheme,
    ) {
        let height = usize::from(size.1);
        let old_height = self.previous.len();
        let new_height = rows.len();
        let commit_len = transcript.rows().len().saturating_sub(commit_start);
        if structural || commit_len > 0 {
            // A fresh screen, or one whose height changed, knows no live
            // block position: the block starts at the bottom, where the old
            // one ended, and stays on the last rows. A width change keeps
            // the tracked top.
            let rebottom = !self.initialized || self.size.1 != size.1;
            let old_top = if rebottom {
                height.saturating_sub(old_height) + 1
            } else {
                self.anchor
            };
            for index in 0..old_height {
                let y = old_top + index;
                if y <= height {
                    out.extend_from_slice(format!("\x1b[{y};1H\x1b[2K").as_bytes());
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
    write_styled_text(out, &row.text, &row.spans, &row.links, row.role, theme);
}

/// Writes one row's text with its spans, links, and role applied.
pub(crate) fn write_styled_text(
    out: &mut Vec<u8>,
    text: &str,
    spans: &[RenderSpan],
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
        let (role, bold) = spans
            .iter()
            .find(|span| span.range.start <= *start && *start < span.range.end)
            .map_or((fallback, false), |span| (span.role, span.bold));
        let sgr = crate::status::style_sgr(theme, role, bold);
        painted = !sgr.is_empty();
        out.extend_from_slice(sgr.as_bytes());
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
    use std::sync::Arc;

    use crate::image::Rung;
    use crate::render::PixelImage;
    use crate::render::RenderRow;

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
}
