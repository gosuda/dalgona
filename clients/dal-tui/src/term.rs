//! Terminal capability probes, raw-input seam, and restoration bytes.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(not(unix))]
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal;
mod probe;
use crate::{ColorMode, EnvFacts, ThemeRequest, TuiError};
pub use probe::{
    Probe, ReplyParser, parse_da1, parse_decrpm, parse_kitty_keyboard, parse_osc11, parse_replies,
    startup_probe,
};

/// The terminal's answer to a caret-position query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorReport {
    /// The 1-based (row, column) the terminal reported directly before the
    /// status sentinel answer.
    Position(u16, u16),
    /// The terminal answered the status sentinel, but no position report sat
    /// directly before it (for example, a key arrived between the answers).
    /// The terminal is alive; asking again later is safe and never latches.
    Unpaired,
    /// No answer arrived within the timeout, or the adapter cannot ask.
    /// Callers hold the frame instead of painting at an unmeasured row.
    Silent,
}

/// I/O boundary for terminal setup, probes, and the input byte stream.
pub trait TermIo: Send {
    /// Enables raw terminal input.
    ///
    /// # Errors
    /// Returns the terminal I/O error when raw mode cannot be entered.
    fn enable_raw(&self) -> io::Result<()>;
    /// Disables raw terminal input without returning an error.
    fn disable_raw(&self);
    /// Returns terminal rows and columns.
    ///
    /// # Errors
    /// Returns the terminal I/O error when the size cannot be read.
    fn size(&self) -> io::Result<(u16, u16)>;
    /// Writes one complete logical output and flushes it once.
    ///
    /// # Errors
    /// Returns the terminal I/O error when the write or flush fails.
    fn write(&self, bytes: &[u8]) -> io::Result<()>;
    /// Reads input for at most `timeout`.
    ///
    /// # Errors
    /// Returns the terminal I/O error when the read fails.
    fn read(&self, timeout: Duration) -> io::Result<Vec<u8>>;
    /// Raises `SIGTSTP` on POSIX; does nothing on Windows.
    fn raise_tstp(&self);
    /// Returns the process-edge shutdown status, when a signal requested exit.
    fn shutdown_code(&self) -> Option<u8> {
        None
    }
    /// Consumes a process-edge resize notification.
    fn take_resize(&self) -> bool {
        false
    }
    /// Counts process-edge resize notifications; never decreases. A change
    /// between two paints means the terminal may have moved the screen even
    /// when the size is unchanged again.
    fn resize_epoch(&self) -> u64 {
        0
    }
    /// Asks the terminal for the caret position, waiting at most `timeout`.
    /// Input read while waiting is kept, in arrival order, for the next
    /// `read`, minus the consumed answer pair or lone status packet.
    ///
    /// The default reports [`CursorReport::Silent`], which maps to hold: no
    /// live bytes are written at an unmeasured position.
    ///
    /// # Errors
    /// Returns the terminal I/O error when the query write or input read fails.
    fn cursor_position(&self, timeout: Duration) -> io::Result<CursorReport> {
        let _ = timeout;
        Ok(CursorReport::Silent)
    }
    /// Asks the desktop to open `url` in a browser. The default declines, so
    /// a terminal without a desktop edge only shows the URL.
    ///
    /// # Errors
    /// Returns the error that kept the browser from starting.
    fn open_url(&self, url: &str) -> io::Result<()> {
        let _ = url;
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

/// Terminal modes captured for idempotent cleanup.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent terminal modes; a bitset loses legibility"
)]
pub struct TermState {
    raw: bool,
    paste: bool,
    kitty: bool,
    grapheme: bool,
    fullscreen: bool,
    transcript_overlay: bool,
    sync_open: bool,
}

impl TermState {
    /// Creates an unstarted terminal state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            raw: false,
            paste: false,
            kitty: false,
            grapheme: false,
            fullscreen: false,
            transcript_overlay: false,
            sync_open: false,
        }
    }

    /// Records that raw mode is active.
    pub fn set_raw(&mut self, enabled: bool) {
        self.raw = enabled;
    }

    /// Records bracketed-paste mode.
    pub fn set_paste(&mut self, enabled: bool) {
        self.paste = enabled;
    }

    /// Records kitty keyboard mode.
    pub fn set_kitty(&mut self, enabled: bool) {
        self.kitty = enabled;
    }

    /// Reports whether kitty keyboard mode is active.
    #[must_use]
    pub const fn kitty(&self) -> bool {
        self.kitty
    }

    /// Records grapheme-width terminal mode.
    pub fn set_grapheme(&mut self, enabled: bool) {
        self.grapheme = enabled;
    }

    /// Reports whether grapheme-width terminal mode is active.
    #[must_use]
    pub const fn grapheme(&self) -> bool {
        self.grapheme
    }

    /// Records alternate-screen ownership.
    pub fn set_fullscreen(&mut self, enabled: bool) {
        self.fullscreen = enabled;
    }

    /// Records the inline transcript overlay's alternate-screen ownership.
    pub fn set_transcript_overlay(&mut self, enabled: bool) {
        self.transcript_overlay = enabled;
    }

    /// Records whether a synchronized-update bracket is open.
    pub fn set_sync_open(&mut self, enabled: bool) {
        self.sync_open = enabled;
    }
}

/// Writes the terminal restore sequence and disables raw mode best-effort.
///
/// Restoring an already restored state is safe: every emitted terminal mode
/// command sets an absolute off/reset state.
pub fn restore(state: &TermState) {
    let bytes = restore_bytes(state);
    let mut stdout = io::stdout().lock();
    let _ = stdout.write_all(&bytes);
    let _ = stdout.flush();
    if state.raw {
        let _ = terminal::disable_raw_mode();
    }
}

/// Builds the byte-exact terminal restore sequence.
#[must_use]
pub(crate) fn restore_bytes(state: &TermState) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(64);
    if state.sync_open {
        bytes.extend_from_slice(b"\x1b[?2026l");
    }
    if state.kitty {
        bytes.extend_from_slice(b"\x1b[<u\x1b[<u\x1b[>4;0m");
    }
    bytes.extend_from_slice(b"\x1b[?2004l");
    if state.grapheme {
        bytes.extend_from_slice(b"\x1b[?2027l");
    }
    bytes.extend_from_slice(b"\x1b[?25h\x1b[0m");
    if state.fullscreen || state.transcript_overlay {
        bytes.extend_from_slice(b"\x1b[r\x1b[?1049l");
    }
    bytes
}

/// Selects a truecolor/256/16/never capability from the captured environment.
#[must_use]
pub fn color_mode(env: &EnvFacts, no_color: bool) -> ColorMode {
    if no_color {
        return ColorMode::Never;
    }
    if env
        .colorterm
        .as_deref()
        .is_some_and(|value| matches!(value, "truecolor" | "24bit"))
    {
        return ColorMode::Truecolor;
    }
    if env
        .term
        .as_deref()
        .is_some_and(|value| value.contains("256color"))
    {
        return ColorMode::Ansi256;
    }
    ColorMode::Sixteen
}

/// Formats the unknown-theme error text.
#[must_use]
pub(crate) fn te_theme(value: &str, suggestion: Option<&str>) -> TuiError {
    let names = crate::theme::SHIPPED_THEMES.join(", ");
    let second = suggestion.map_or_else(
        || format!("Set theme to auto, palette, or one of: {names}."),
        |name| format!("Did you mean {name}? Set theme to auto, palette, or a shipped theme name."),
    );
    TuiError::Terminal(format!(
        "dalgon: theme \"{value}\" is not a shipped theme: the value is unknown\n{second}"
    ))
}

/// Formats the non-terminal standard-input error text.
#[must_use]
pub(crate) fn te_stdin_not_terminal() -> TuiError {
    TuiError::Terminal(
        "dalgon: cannot start the interface: standard input is not a terminal\nRun dalgon in a terminal, or use dal -p for print mode.".to_owned(),
    )
}

/// Formats a raw-mode or terminal-size startup failure.
#[must_use]
pub(crate) fn te_raw_mode_failed(os: &str) -> TuiError {
    TuiError::Terminal(format!(
        "dalgon: cannot start the interface: {os}\nRun dalgon in a terminal, or use dal -p for print mode."
    ))
}

/// Formats an invalid printable-key binding.
#[must_use]
pub(crate) fn te_printable_key(context: &str, action: &str) -> TuiError {
    TuiError::Terminal(format!(
        "dalgon: binding {context}.{action} uses a printable key: printable keys are reserved for typing\nBind {context}.{action} to a key with ctrl, alt, or a function key."
    ))
}

/// Formats a terminal failure from its two-line user-facing message.
#[must_use]
pub(crate) fn terminal_error(message: &str) -> TuiError {
    TuiError::Terminal(format!(
        "dalgon: cannot start the interface: {message}\nRun dalgon in a terminal, or use dal -p for print mode."
    ))
}

/// POSIX and Windows crossterm-backed terminal adapter.
#[derive(Debug)]
pub struct CrosstermTermIo {
    #[cfg(unix)]
    stdin: io::Stdin,
    /// Input bytes consumed during a caret-position query that belong to the
    /// key stream; the next `read` drains them before polling again.
    #[cfg(unix)]
    pending: Mutex<Vec<u8>>,
    /// Resize events observed while reading input (Windows `Event::Resize`).
    resize_epoch: Arc<AtomicU64>,
}

impl CrosstermTermIo {
    /// Builds the adapter with standard input captured by the process edge.
    #[must_use]
    #[cfg_attr(
        not(unix),
        expect(
            clippy::needless_pass_by_value,
            reason = "the uniform constructor captures Stdin; only unix stores it"
        )
    )]
    pub fn new(stdin: io::Stdin) -> Self {
        #[cfg(not(unix))]
        let _ = stdin;
        Self {
            #[cfg(unix)]
            stdin,
            #[cfg(unix)]
            pending: Mutex::new(Vec::new()),
            resize_epoch: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Drains `bytes` into the preserved-input queue in arrival order.
    #[cfg(unix)]
    fn push_pending(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(bytes);
    }

    /// Reads stdin for at most `timeout` (the former unix `read` body).
    #[cfg(unix)]
    fn read_stdin(&self, timeout: Duration) -> io::Result<Vec<u8>> {
        let stdin = self.stdin.lock();
        let timeout = rustix::event::Timespec::try_from(timeout)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let mut descriptors = [rustix::event::PollFd::new(
            &stdin,
            rustix::event::PollFlags::IN,
        )];
        match rustix::event::poll(&mut descriptors, Some(&timeout)) {
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => return Ok(Vec::new()),
            Err(error) => return Err(io::Error::from(error)),
        }
        if !descriptors[0]
            .revents()
            .contains(rustix::event::PollFlags::IN)
        {
            return Ok(Vec::new());
        }
        let mut buffer = [0_u8; 4096];
        let bytes_read = match rustix::io::read(&stdin, &mut buffer) {
            Ok(bytes) => bytes,
            Err(rustix::io::Errno::INTR) => return Ok(Vec::new()),
            Err(error) => return Err(io::Error::from(error)),
        };
        if bytes_read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "terminal input closed",
            ));
        }
        Ok(buffer[..bytes_read].to_vec())
    }
}

impl TermIo for CrosstermTermIo {
    fn enable_raw(&self) -> io::Result<()> {
        terminal::enable_raw_mode()
    }

    fn disable_raw(&self) {
        let _ = terminal::disable_raw_mode();
    }

    fn size(&self) -> io::Result<(u16, u16)> {
        terminal::size()
    }

    fn write(&self, bytes: &[u8]) -> io::Result<()> {
        let mut stdout = io::stdout().lock();
        stdout.write_all(bytes)?;
        stdout.flush()
    }

    fn read(&self, timeout: Duration) -> io::Result<Vec<u8>> {
        #[cfg(unix)]
        {
            let pending = std::mem::take(
                &mut *self
                    .pending
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
            if !pending.is_empty() {
                return Ok(pending);
            }
            self.read_stdin(timeout)
        }
        #[cfg(not(unix))]
        {
            if !event::poll(timeout)? {
                return Ok(Vec::new());
            }
            match event::read()? {
                Event::Key(key)
                    if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                {
                    Ok(encode_key(key.code, key.modifiers))
                }
                Event::Paste(text) => {
                    let mut bytes = b"\x1b[200~".to_vec();
                    bytes.extend_from_slice(text.as_bytes());
                    bytes.extend_from_slice(b"\x1b[201~");
                    Ok(bytes)
                }
                Event::Resize(_, _) => {
                    self.resize_epoch
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(Vec::new())
                }
                _ => Ok(Vec::new()),
            }
        }
    }

    fn resize_epoch(&self) -> u64 {
        self.resize_epoch.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Unix: DSR-6 plus a DSR-5 ordering sentinel in one write, then bounded
    /// polls. Every byte that is not the consumed adjacent answer pair (or
    /// one lone status packet) is queued for the next `read` in arrival
    /// order. Windows: the synchronous console API answers with no wire
    /// bytes, so the input stream is untouched by construction.
    fn cursor_position(&self, timeout: Duration) -> io::Result<CursorReport> {
        #[cfg(unix)]
        {
            self.write(b"\x1b[6n\x1b[5n")?;
            let deadline = Instant::now() + timeout;
            let mut buffered = Vec::new();
            loop {
                match take_position_report(&mut buffered) {
                    PositionScan::Found(row, column) => {
                        self.push_pending(&buffered);
                        return Ok(CursorReport::Position(row, column));
                    }
                    PositionScan::StatusOnly => {
                        self.push_pending(&buffered);
                        return Ok(CursorReport::Unpaired);
                    }
                    PositionScan::Waiting => {}
                }
                let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                    self.push_pending(&buffered);
                    return Ok(CursorReport::Silent);
                };
                match self.read_stdin(remaining) {
                    Ok(chunk) => buffered.extend_from_slice(&chunk),
                    Err(error) => {
                        self.push_pending(&buffered);
                        return Err(error);
                    }
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = timeout;
            // GetConsoleScreenBufferInfo through crossterm: no wire bytes.
            let (column, row) = crossterm::cursor::position()?;
            Ok(CursorReport::Position(
                row.saturating_add(1),
                column.saturating_add(1),
            ))
        }
    }

    fn raise_tstp(&self) {
        #[cfg(unix)]
        {
            let _ = rustix::process::kill_process(
                rustix::process::getpid(),
                rustix::process::Signal::TSTP,
            );
        }
    }
}

#[cfg(not(unix))]
fn encode_key(code: KeyCode, modifiers: KeyModifiers) -> Vec<u8> {
    let mut bytes = match code {
        KeyCode::Char(character) => {
            if modifiers.contains(KeyModifiers::CONTROL) && character.is_ascii() {
                let mut encoded = [0; 4];
                let first = character.encode_utf8(&mut encoded).as_bytes()[0];
                vec![first & 0x1f]
            } else {
                let mut encoded = [0; 4];
                character.encode_utf8(&mut encoded).as_bytes().to_vec()
            }
        }
        KeyCode::Enter if modifiers.contains(KeyModifiers::SHIFT) => b"\x1b[13;2u".to_vec(),
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Tab if modifiers.contains(KeyModifiers::SHIFT) => b"\x1b[Z".to_vec(),
        KeyCode::Tab => vec![b'\t'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::F(1) => b"\x1bOP".to_vec(),
        KeyCode::F(3) => b"\x1bOR".to_vec(),
        _ => Vec::new(),
    };
    if modifiers.contains(KeyModifiers::ALT) && !bytes.is_empty() && bytes[0] != 0x1b {
        bytes.insert(0, 0x1b);
    }
    bytes
}

/// What [`take_position_report`] extracted from queued input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PositionScan {
    /// Removed an adjacent `CSI row ; col R` and `CSI 0n`/`CSI 3n` pair.
    Found(u16, u16),
    /// Removed one status packet that had no adjacent position report.
    StatusOnly,
    /// No consumable answer yet; keep reading until the deadline.
    Waiting,
}

/// Removes from `bytes` the first `CSI row ; column R` that is immediately
/// followed by `CSI 0 n` or `CSI 3 n`, together with that status packet
/// ([`PositionScan::Found`]). Otherwise removes the first status packet that
/// has no adjacent position report ([`PositionScan::StatusOnly`]).
///
/// Bytes inside `CSI 200 ~` … `CSI 201 ~` paste payloads are never scanned.
/// A lone `CSI 1;m R` key or any other input stays in order. Row and column
/// must parse as `u16 >= 1` with no private marker.
pub(crate) fn take_position_report(bytes: &mut Vec<u8>) -> PositionScan {
    let mut index = 0;
    let mut in_paste = false;
    while index < bytes.len() {
        if in_paste {
            match find_bytes(bytes, index, b"\x1b[201~") {
                Some(end) => {
                    index = end + 6;
                    in_paste = false;
                }
                None => return PositionScan::Waiting,
            }
            continue;
        }
        let Some(start) = find_bytes(bytes, index, b"\x1b[") else {
            return PositionScan::Waiting;
        };
        match parse_response_csi(&bytes[start..]) {
            ResponseCsi::Partial => return PositionScan::Waiting,
            ResponseCsi::Aborted => index = start + 2,
            ResponseCsi::PasteOpen(end) => {
                in_paste = true;
                index = start + end;
            }
            ResponseCsi::Other(end) => index = start + end,
            ResponseCsi::Status(end) => {
                bytes.drain(start..start + end);
                return PositionScan::StatusOnly;
            }
            ResponseCsi::Report(row, column, end) => {
                let pair = status_packet_len(&bytes[start + end..]);
                if pair > 0 {
                    bytes.drain(start..start + end + pair);
                    return PositionScan::Found(row, column);
                }
                index = start + end;
            }
        }
    }
    PositionScan::Waiting
}

/// Returns the byte length of a leading `CSI 0n`/`CSI 3n` packet, else 0.
fn status_packet_len(bytes: &[u8]) -> usize {
    match bytes {
        [0x1b, b'[', b'0' | b'3', b'n', ..] => 4,
        _ => 0,
    }
}

/// Returns the first index of `needle` in `haystack` at or after `from`.
fn find_bytes(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| from + offset)
}

/// One CSI sequence recognized while scanning query responses.
enum ResponseCsi {
    /// The sequence has no final byte yet.
    Partial,
    /// Another `ESC` restarted the sequence; the bare `ESC[` is ordinary input.
    Aborted,
    /// `CSI 200~` opened a bracketed paste.
    PasteOpen(usize),
    /// `CSI row ; col R` with both parameters valid `u16 >= 1`.
    Report(u16, u16, usize),
    /// `CSI 0n` or `CSI 3n` device status.
    Status(usize),
    /// Any other complete CSI; length given so the scan can step over it.
    Other(usize),
}

/// Parses the CSI sequence at `bytes[0] == ESC, bytes[1] == '['`.
fn parse_response_csi(bytes: &[u8]) -> ResponseCsi {
    for (offset, byte) in bytes.iter().enumerate().skip(2) {
        match byte {
            0x1b => return ResponseCsi::Aborted,
            b'@'..=b'~' => return classify_response(&bytes[2..offset], *byte, offset + 1),
            _ => {}
        }
    }
    ResponseCsi::Partial
}

/// Classifies a complete CSI by parameter bytes and final byte.
fn classify_response(parameters: &[u8], final_byte: u8, length: usize) -> ResponseCsi {
    if parameters == b"200" && final_byte == b'~' {
        return ResponseCsi::PasteOpen(length);
    }
    if parameters
        .iter()
        .any(|byte| !byte.is_ascii_digit() && *byte != b';')
    {
        return ResponseCsi::Other(length);
    }
    if final_byte == b'n' && matches!(parameters, b"0" | b"3") {
        return ResponseCsi::Status(length);
    }
    if final_byte == b'R'
        && let Some((row, column)) = parse_report_parameters(parameters)
    {
        return ResponseCsi::Report(row, column, length);
    }
    ResponseCsi::Other(length)
}

/// Parses `row ; column` report parameters; both must be `u16 >= 1`.
fn parse_report_parameters(parameters: &[u8]) -> Option<(u16, u16)> {
    let text = std::str::from_utf8(parameters).ok()?;
    let (row, column) = text.split_once(';')?;
    let row = row.parse::<u16>().ok()?;
    let column = column.parse::<u16>().ok()?;
    (row >= 1 && column >= 1).then_some((row, column))
}

/// Creates a deterministic terminal adapter from delayed byte reads.
#[doc(hidden)]
#[must_use]
pub fn fake_term_io(script: Vec<(Duration, Vec<u8>)>) -> impl TermIo {
    ScriptedTermIo {
        origin: Instant::now(),
        reads: Mutex::new(script.into()),
        writes: Arc::new(Mutex::new(Vec::new())),
    }
}

#[derive(Debug)]
struct ScriptedTermIo {
    origin: Instant,
    reads: Mutex<VecDeque<(Duration, Vec<u8>)>>,
    writes: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl TermIo for ScriptedTermIo {
    fn enable_raw(&self) -> io::Result<()> {
        Ok(())
    }
    fn disable_raw(&self) {}
    fn size(&self) -> io::Result<(u16, u16)> {
        Ok((24, 80))
    }
    fn write(&self, bytes: &[u8]) -> io::Result<()> {
        if let Ok(mut writes) = self.writes.lock() {
            writes.push(bytes.to_vec());
        }
        Ok(())
    }
    fn read(&self, timeout: Duration) -> io::Result<Vec<u8>> {
        let now = self.origin.elapsed();
        let mut reads = self
            .reads
            .lock()
            .map_err(|_| io::Error::other("terminal test lock poisoned"))?;
        let Some((at, _)) = reads.front() else {
            std::thread::sleep(timeout);
            return Ok(Vec::new());
        };
        if *at <= now + timeout {
            let delay = at.saturating_sub(now);
            std::thread::sleep(delay);
            return Ok(reads.pop_front().map_or_else(Vec::new, |(_, bytes)| bytes));
        }
        std::thread::sleep(timeout);
        Ok(Vec::new())
    }
    fn raise_tstp(&self) {}
}

/// Resolves an `auto` or named theme request from a probe result and captured environment.
///
/// # Errors
/// Returns [`TuiError`] when the request names a theme the palette does not carry.
pub fn resolve_theme_request(
    request: &ThemeRequest,
    probe: &Probe,
    env: &EnvFacts,
) -> Result<Option<&'static str>, TuiError> {
    match request {
        ThemeRequest::Palette => Ok(None),
        ThemeRequest::Named(name) => crate::theme::resolve_name(name).map(Some),
        ThemeRequest::Auto => {
            if let Some(luminance) = probe.background_luminance {
                return Ok(Some(if luminance <= 0.2 {
                    "flexoki-dark"
                } else {
                    "flexoki-light"
                }));
            }
            Ok(env.colorfgbg.as_deref().and_then(colorfgbg_theme))
        }
    }
}

fn colorfgbg_theme(value: &str) -> Option<&'static str> {
    value
        .rsplit(';')
        .next()
        .and_then(|background| match background.parse::<u8>().ok()? {
            0..=6 | 8 => Some("flexoki-dark"),
            7 | 15 => Some("flexoki-light"),
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::{
        CursorReport, PositionScan, TermIo, TermState, parse_da1, parse_decrpm, parse_osc11,
        parse_replies, restore_bytes, startup_probe, take_position_report,
    };
    use std::time::Duration;

    #[test]
    fn decrpm_and_da1_parse_supported_modes() {
        assert!(parse_decrpm(b"\x1b[?2026;1$y", 2026));
        assert!(!parse_decrpm(b"\x1b[?2026;4$y", 2026));
        assert!(parse_da1(b"\x1b[?62;4c"));
        assert!(!parse_da1(b"\x1b[?62;1c"));
    }

    #[test]
    fn osc11_parses_short_and_long_color_channels() {
        assert!(
            parse_osc11(b"\x1b]11;rgb:0000/0000/0000\x07")
                .is_some_and(|luminance| luminance.abs() < 1e-6)
        );
        assert!(
            parse_osc11(b"\x1b]11;#ffffffffffff\x1b\\")
                .is_some_and(|luminance| (luminance - 1.0).abs() < 1e-6)
        );
    }

    #[test]
    fn gate_probe_sequence_sets_sync() {
        let (probe, replay) = parse_replies(
            b"\x1b[?2026;1$y\x1b[?2027;1$y\x1b[?1u\x1b]11;rgb:0000/0000/0000\x07\x1b[?1;2c",
        );
        assert!(probe.sync_update, "{probe:?}");
        assert_eq!(replay.len(), 0);
    }

    #[test]
    fn reply_parser_replays_only_bytes_outside_replies() {
        let (probe, replay) = parse_replies(b"a\x1b[?2026;1$y\x1b[?62;4c");
        assert!(probe.sync_update);
        assert!(probe.sixel);
        assert_eq!(replay, b"a");
    }

    #[test]
    fn startup_probe_omits_graphics_when_images_are_disabled() {
        assert_eq!(
            startup_probe(false),
            b"\x1b[?2026$p\x1b[?2027$p\x1b[?u\x1b]11;?\x07\x1b[c"
        );
        assert!(
            startup_probe(true)
                .windows(4)
                .any(|window| window == b"_Gi=")
        );
    }

    #[test]
    fn restore_closes_modes_in_order_and_is_stable() {
        let mut state = TermState::new();
        state.set_raw(true);
        state.set_kitty(true);
        state.set_grapheme(true);
        state.set_fullscreen(true);
        state.set_sync_open(true);
        let expected = b"\x1b[?2026l\x1b[<u\x1b[<u\x1b[>4;0m\x1b[?2004l\x1b[?2027l\x1b[?25h\x1b[0m\x1b[r\x1b[?1049l";
        assert_eq!(restore_bytes(&state), expected);
    }

    #[test]
    fn adjacent_report_pair_is_removed_in_order() {
        let mut bytes = b"a\x1b[9;3R\x1b[0nb".to_vec();
        assert_eq!(take_position_report(&mut bytes), PositionScan::Found(9, 3));
        assert_eq!(bytes, b"ab");
    }

    #[test]
    fn preceding_lone_f3_key_stays_while_later_pair_is_found() {
        let mut bytes = b"\x1b[1;5Rx\x1b[9;3R\x1b[0n".to_vec();
        assert_eq!(take_position_report(&mut bytes), PositionScan::Found(9, 3));
        assert_eq!(bytes, b"\x1b[1;5Rx");
    }

    #[test]
    fn key_interleaved_between_answers_yields_status_only() {
        let mut bytes = b"\x1b[9;3Rx\x1b[0n".to_vec();
        assert_eq!(take_position_report(&mut bytes), PositionScan::StatusOnly);
        assert_eq!(bytes, b"\x1b[9;3Rx");
    }

    #[test]
    fn lone_status_packet_is_status_only() {
        let mut bytes = b"k\x1b[0nrest".to_vec();
        assert_eq!(take_position_report(&mut bytes), PositionScan::StatusOnly);
        assert_eq!(bytes, b"krest");
    }

    #[test]
    fn report_pair_inside_paste_is_never_scanned() {
        let mut bytes = b"\x1b[200~\x1b[9;3R\x1b[0n\x1b[201~\x1b[0n".to_vec();
        assert_eq!(take_position_report(&mut bytes), PositionScan::StatusOnly);
        assert_eq!(bytes, b"\x1b[200~\x1b[9;3R\x1b[0n\x1b[201~");
    }

    #[test]
    fn partial_sequence_waits() {
        let mut bytes = b"\x1b[9;3".to_vec();
        assert_eq!(take_position_report(&mut bytes), PositionScan::Waiting);
        assert_eq!(bytes, b"\x1b[9;3");
        let mut malformed = b"\x1b[0;5R\x1b[0n".to_vec();
        assert_eq!(
            take_position_report(&mut malformed),
            PositionScan::StatusOnly
        );
        assert_eq!(malformed, b"\x1b[0;5R");
    }

    #[test]
    fn delayed_probe_bytes_partition_through_the_reply_parser() {
        let delayed = b"\x1b[?2026;1$y\x1b[?1uhello".to_vec();
        let io = super::fake_term_io(vec![(Duration::from_millis(5), delayed)]);
        let bytes = io.read(Duration::from_millis(20)).unwrap_or_default();
        let (probe, replay) = parse_replies(&bytes);
        assert!(probe.sync_update, "{probe:?}");
        assert!(probe.kitty_keyboard, "{probe:?}");
        assert_eq!(replay, b"hello");
    }

    #[test]
    fn default_cursor_position_is_silent() {
        let io = super::fake_term_io(Vec::new());
        assert_eq!(
            io.cursor_position(Duration::from_millis(1))
                .unwrap_or(CursorReport::Unpaired),
            CursorReport::Silent
        );
        assert_eq!(io.resize_epoch(), 0);
    }
}
