//! Terminal capability probes, raw-input seam, and restoration bytes.

use std::collections::VecDeque;
use std::io::{self, Write};
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

    /// Records grapheme-width terminal mode.
    pub fn set_grapheme(&mut self, enabled: bool) {
        self.grapheme = enabled;
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
        }
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
                _ => Ok(Vec::new()),
            }
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
        Probe, TermIo, TermState, parse_da1, parse_decrpm, parse_osc11, parse_replies,
        restore_bytes, startup_probe,
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
        assert_eq!(restore_bytes(&state), expected);
    }

    #[test]
    fn fake_terminal_returns_delayed_probe_bytes() {
        let io = super::fake_term_io(vec![(Duration::from_millis(5), b"x".to_vec())]);
        assert_eq!(io.read(Duration::from_millis(20)).unwrap_or_default(), b"x");
        let _probe = Probe::default();
    }
}
