#![expect(
    dead_code,
    reason = "VT recorder helpers are shared by separate gate targets"
)]
#![expect(
    unreachable_pub,
    reason = "VT recorder is public only within private test modules"
)]

use unicode_width::UnicodeWidthChar;

/// Terminal grid and control-state recorder for the byte subset emitted by dal-tui.
///
/// The model uses xterm cursor and scroll-region semantics, disables reflow, and
/// processes UTF-8 and escape sequences incrementally across arbitrary read chunks.
type AlternateScreen = Option<(Vec<Vec<String>>, usize, usize, usize, usize)>;

#[derive(Debug)]
pub struct VtRecorder {
    columns: usize,
    rows: usize,
    screen: Vec<Vec<String>>,
    scrollback: Vec<Vec<String>>,
    cursor_x: usize,
    cursor_y: usize,
    saved_cursor: (usize, usize),
    scroll_top: usize,
    scroll_bottom: usize,
    wrap_pending: bool,
    alternate: AlternateScreen,
    pending: Vec<u8>,
    sync_open: bool,
    sync_pairs: usize,
    erase_display_sequences: usize,
    erase_line_rows: Vec<usize>,
    apc_sequences: usize,
    written_cells: Vec<(usize, usize, String)>,
}

impl VtRecorder {
    /// Creates a blank terminal with the given fixed geometry.
    #[must_use]
    pub fn new(columns: u16, rows: u16) -> Self {
        let columns = usize::from(columns.max(1));
        let rows = usize::from(rows.max(1));
        Self {
            columns,
            rows,
            screen: blank_screen(columns, rows),
            scrollback: Vec::new(),
            cursor_x: 0,
            cursor_y: 0,
            saved_cursor: (0, 0),
            scroll_top: 0,
            scroll_bottom: rows - 1,
            wrap_pending: false,
            alternate: None,
            pending: Vec::new(),
            sync_open: false,
            sync_pairs: 0,
            erase_display_sequences: 0,
            erase_line_rows: Vec::new(),
            apc_sequences: 0,
            written_cells: Vec::new(),
        }
    }

    /// Feeds another output chunk into the terminal parser.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        let mut cursor = 0;
        while cursor < self.pending.len() {
            let start = cursor;
            let byte = self.pending[cursor];
            if byte == 0x1b {
                let Some(consumed) = self.escape_sequence(cursor) else {
                    break;
                };
                cursor += consumed;
                continue;
            }
            if byte < 0x20 || byte == 0x7f {
                self.control(byte);
                cursor += 1;
                continue;
            }
            let width = utf8_width(byte);
            if cursor + width > self.pending.len() {
                break;
            }
            let character = std::str::from_utf8(&self.pending[cursor..cursor + width])
                .ok()
                .and_then(|text| text.chars().next())
                .unwrap_or('\u{fffd}');
            self.print(character);
            cursor = start + width;
        }
        self.pending.drain(..cursor);
    }

    /// Resizes the screen without reflowing any existing line.
    pub fn resize(&mut self, columns: u16, rows: u16) {
        let columns = usize::from(columns.max(1));
        let rows = usize::from(rows.max(1));
        self.screen.resize_with(rows, || blank_row(columns));
        for line in &mut self.screen {
            line.resize(columns, " ".to_owned());
        }
        self.columns = columns;
        self.rows = rows;
        self.scroll_top = self.scroll_top.min(rows - 1);
        self.scroll_bottom = self.scroll_bottom.min(rows - 1).max(self.scroll_top);
        self.cursor_x = self.cursor_x.min(columns - 1);
        self.cursor_y = self.cursor_y.min(rows - 1);
        self.wrap_pending = false;
    }

    /// Returns visible terminal rows as plain text, without changing the grid.
    #[must_use]
    pub fn screen_rows(&self) -> Vec<String> {
        self.screen.iter().map(|line| line.concat()).collect()
    }

    /// Returns the captured scrollback rows as plain text.
    #[must_use]
    pub fn scrollback_rows(&self) -> Vec<String> {
        self.scrollback.iter().map(|line| line.concat()).collect()
    }

    /// Finds the first visible or scrollback row containing `needle`.
    #[must_use]
    pub fn row_containing(&self, needle: &str) -> Option<String> {
        self.screen_rows()
            .into_iter()
            .chain(self.scrollback_rows())
            .find(|line| line.contains(needle))
    }
    /// Returns all visible or scrollback rows containing `needle`.
    #[must_use]
    pub fn rows_containing(&self, needle: &str) -> Vec<String> {
        self.screen_rows()
            .into_iter()
            .chain(self.scrollback_rows())
            .filter(|line| line.contains(needle))
            .collect()
    }

    /// Returns whether the output currently has an unclosed synchronized update.
    #[must_use]
    pub const fn sync_is_open(&self) -> bool {
        self.sync_open
    }

    /// Returns the number of balanced synchronized-update pairs.
    #[must_use]
    pub const fn sync_pairs(&self) -> usize {
        self.sync_pairs
    }

    /// Returns the number of erase-display control sequences observed.
    #[must_use]
    pub const fn erase_display_sequences(&self) -> usize {
        self.erase_display_sequences
    }

    /// Returns every row touched by an erase-line sequence.
    #[must_use]
    pub fn erase_line_rows(&self) -> &[usize] {
        &self.erase_line_rows
    }

    /// Returns the number of APC sequences observed.
    #[must_use]
    pub const fn apc_sequences(&self) -> usize {
        self.apc_sequences
    }

    /// Returns the terminal coordinates and text of every printable cell write.
    #[must_use]
    pub fn written_cells(&self) -> &[(usize, usize, String)] {
        &self.written_cells
    }

    fn escape_sequence(&mut self, start: usize) -> Option<usize> {
        let second = *self.pending.get(start + 1)?;
        match second {
            b'[' => {
                let final_index = (start + 2..self.pending.len())
                    .find(|index| (0x40..=0x7e).contains(&self.pending[*index]))?;
                let parameters = self.pending[start + 2..final_index].to_vec();
                let final_byte = self.pending[final_index];
                self.csi(&parameters, final_byte);
                Some(final_index - start + 1)
            }
            b']' | b'_' | b'P' | b'^' | b'X' => {
                let is_apc = second == b'_';
                let mut index = start + 2;
                while index < self.pending.len() {
                    if self.pending[index] == 0x07 {
                        if is_apc {
                            self.apc_sequences += 1;
                        }
                        return Some(index - start + 1);
                    }
                    if self.pending[index] == 0x1b && self.pending.get(index + 1) == Some(&b'\\') {
                        if is_apc {
                            self.apc_sequences += 1;
                        }
                        return Some(index - start + 2);
                    }
                    index += 1;
                }
                None
            }
            b'7' => {
                self.saved_cursor = (self.cursor_x, self.cursor_y);
                Some(2)
            }
            b'8' => {
                self.cursor_x = self.saved_cursor.0.min(self.columns - 1);
                self.cursor_y = self.saved_cursor.1.min(self.rows - 1);
                self.wrap_pending = false;
                Some(2)
            }
            b'D' => {
                self.line_feed();
                Some(2)
            }
            b'M' => {
                self.reverse_index();
                Some(2)
            }
            b'c' => {
                self.screen = blank_screen(self.columns, self.rows);
                self.cursor_x = 0;
                self.cursor_y = 0;
                self.scrollback.clear();
                Some(2)
            }
            _ => Some(2),
        }
    }

    fn csi(&mut self, raw: &[u8], final_byte: u8) {
        let text = std::str::from_utf8(raw).unwrap_or_default();
        let (private, text) = text
            .strip_prefix('?')
            .map_or((false, text), |text| (true, text));
        let values = text
            .split(';')
            .map(|value| value.parse::<usize>().unwrap_or(0))
            .collect::<Vec<_>>();
        let first = values.first().copied().unwrap_or(0);
        let amount = first.max(1);
        match final_byte {
            b'A' => self.cursor_y = self.cursor_y.saturating_sub(amount),
            b'B' => self.cursor_y = (self.cursor_y + amount).min(self.rows - 1),
            b'C' => self.cursor_x = (self.cursor_x + amount).min(self.columns - 1),
            b'D' => self.cursor_x = self.cursor_x.saturating_sub(amount),
            b'E' => {
                self.cursor_y = (self.cursor_y + amount).min(self.rows - 1);
                self.cursor_x = 0;
            }
            b'F' => {
                self.cursor_y = self.cursor_y.saturating_sub(amount);
                self.cursor_x = 0;
            }
            b'G' | b'`' => self.cursor_x = first.saturating_sub(1).min(self.columns - 1),
            b'd' => self.cursor_y = first.saturating_sub(1).min(self.rows - 1),
            b'H' | b'f' => {
                self.cursor_y = values
                    .first()
                    .copied()
                    .unwrap_or(1)
                    .saturating_sub(1)
                    .min(self.rows - 1);
                self.cursor_x = values
                    .get(1)
                    .copied()
                    .unwrap_or(1)
                    .saturating_sub(1)
                    .min(self.columns - 1);
            }
            b'J' => self.erase_display(first),
            b'K' => self.erase_line(first),
            b'r' => {
                self.scroll_top = values
                    .first()
                    .copied()
                    .unwrap_or(1)
                    .saturating_sub(1)
                    .min(self.rows - 1);
                self.scroll_bottom = values
                    .get(1)
                    .copied()
                    .unwrap_or(self.rows)
                    .saturating_sub(1)
                    .min(self.rows - 1)
                    .max(self.scroll_top);
                self.cursor_x = 0;
                self.cursor_y = self.scroll_top;
            }
            b's' => self.saved_cursor = (self.cursor_x, self.cursor_y),
            b'u' => {
                self.cursor_x = self.saved_cursor.0.min(self.columns - 1);
                self.cursor_y = self.saved_cursor.1.min(self.rows - 1);
            }
            b'S' => (0..amount).for_each(|_| self.scroll_up()),
            b'T' => (0..amount).for_each(|_| self.scroll_down()),
            b'X' => self.erase_chars(amount),
            b'@' => self.insert_chars(amount),
            b'P' => self.delete_chars(amount),
            b'h' if private => self.set_private_modes(&values, true),
            b'l' if private => self.set_private_modes(&values, false),
            _ => {}
        }
        self.wrap_pending = false;
    }

    fn set_private_modes(&mut self, values: &[usize], enabled: bool) {
        for mode in values {
            match *mode {
                2026 if enabled && !self.sync_open => self.sync_open = true,
                2026 if !enabled && self.sync_open => {
                    self.sync_open = false;
                    self.sync_pairs += 1;
                }
                1049 if enabled && self.alternate.is_none() => {
                    self.alternate = Some((
                        std::mem::replace(&mut self.screen, blank_screen(self.columns, self.rows)),
                        self.cursor_x,
                        self.cursor_y,
                        self.scroll_top,
                        self.scroll_bottom,
                    ));
                    self.cursor_x = 0;
                    self.cursor_y = 0;
                    self.scroll_top = 0;
                    self.scroll_bottom = self.rows - 1;
                }
                1049 if !enabled => {
                    if let Some((screen, x, y, top, bottom)) = self.alternate.take() {
                        self.screen = screen;
                        self.cursor_x = x;
                        self.cursor_y = y;
                        self.scroll_top = top;
                        self.scroll_bottom = bottom;
                    }
                }
                _ => {}
            }
        }
    }

    fn control(&mut self, byte: u8) {
        match byte {
            b'\r' => {
                self.cursor_x = 0;
                self.wrap_pending = false;
            }
            b'\n' => self.line_feed(),
            0x08 => self.cursor_x = self.cursor_x.saturating_sub(1),
            b'\t' => {
                self.cursor_x = (((self.cursor_x / 8) + 1) * 8).min(self.columns - 1);
                self.wrap_pending = false;
            }
            _ => {}
        }
    }

    fn print(&mut self, character: char) {
        let width = UnicodeWidthChar::width(character).unwrap_or(0);
        if width == 0 {
            if self.cursor_x > 0 {
                self.screen[self.cursor_y][self.cursor_x - 1].push(character);
            }
            return;
        }
        if self.wrap_pending || self.cursor_x + width > self.columns {
            self.cursor_x = 0;
            self.line_feed();
        }
        self.screen[self.cursor_y][self.cursor_x] = character.to_string();
        self.written_cells
            .push((self.cursor_y, self.cursor_x, character.to_string()));
        if width > 1 && self.cursor_x + 1 < self.columns {
            self.screen[self.cursor_y][self.cursor_x + 1].clear();
        }
        if self.cursor_x + width >= self.columns {
            self.cursor_x = self.columns - 1;
            self.wrap_pending = true;
        } else {
            self.cursor_x += width;
        }
    }

    fn line_feed(&mut self) {
        if self.cursor_y == self.scroll_bottom {
            self.scroll_up();
        } else {
            self.cursor_y = (self.cursor_y + 1).min(self.rows - 1);
        }
    }

    fn reverse_index(&mut self) {
        if self.cursor_y == self.scroll_top {
            self.scroll_down();
        } else {
            self.cursor_y = self.cursor_y.saturating_sub(1);
        }
    }

    fn scroll_up(&mut self) {
        if self.scroll_top == 0 {
            self.scrollback.push(self.screen.remove(self.scroll_top));
            self.screen
                .insert(self.scroll_bottom, blank_row(self.columns));
        } else {
            self.screen.remove(self.scroll_top);
            self.screen
                .insert(self.scroll_bottom, blank_row(self.columns));
        }
    }

    fn scroll_down(&mut self) {
        self.screen.remove(self.scroll_bottom);
        self.screen.insert(self.scroll_top, blank_row(self.columns));
    }

    fn erase_display(&mut self, mode: usize) {
        self.erase_display_sequences += 1;
        match mode {
            0 => {
                self.erase_line(0);
                for row in self.cursor_y + 1..self.rows {
                    self.screen[row].fill(" ".to_owned());
                }
            }
            1 => {
                for row in 0..self.cursor_y {
                    self.screen[row].fill(" ".to_owned());
                }
                self.erase_line(1);
            }
            2 | 3 => {
                self.screen = blank_screen(self.columns, self.rows);
                if mode == 3 {
                    self.scrollback.clear();
                }
            }
            _ => {}
        }
    }

    fn erase_line(&mut self, mode: usize) {
        self.erase_line_rows.push(self.cursor_y);
        let (start, end) = match mode {
            1 => (0, self.cursor_x + 1),
            2 => (0, self.columns),
            _ => (self.cursor_x, self.columns),
        };
        self.screen[self.cursor_y][start..end.min(self.columns)].fill(" ".to_owned());
    }

    fn erase_chars(&mut self, amount: usize) {
        let end = (self.cursor_x + amount).min(self.columns);
        self.screen[self.cursor_y][self.cursor_x..end].fill(" ".to_owned());
    }

    fn insert_chars(&mut self, amount: usize) {
        let row = &mut self.screen[self.cursor_y];
        for _ in 0..amount.min(self.columns - self.cursor_x) {
            row.insert(self.cursor_x, " ".to_owned());
            row.pop();
        }
    }

    fn delete_chars(&mut self, amount: usize) {
        let row = &mut self.screen[self.cursor_y];
        for _ in 0..amount.min(self.columns - self.cursor_x) {
            row.remove(self.cursor_x);
            row.push(" ".to_owned());
        }
    }
}

fn blank_screen(columns: usize, rows: usize) -> Vec<Vec<String>> {
    (0..rows).map(|_| blank_row(columns)).collect()
}

fn blank_row(columns: usize) -> Vec<String> {
    vec![" ".to_owned(); columns]
}

fn utf8_width(first: u8) -> usize {
    match first {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => 1,
    }
}
