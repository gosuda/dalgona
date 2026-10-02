//! Incremental UTF-8, escape-sequence, and bracketed-paste decoder.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyModifiers};

use super::Key;

/// Input decoded from the POSIX terminal byte stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    /// One key action.
    Key(Key),
    /// Raw bracketed-paste content.
    Paste(Vec<u8>),
}

#[derive(Debug)]
struct PasteBuffer {
    bytes: Vec<u8>,
    started: Instant,
}

/// Incremental UTF-8, escape-sequence, and bracketed-paste decoder.
#[derive(Debug, Default)]
pub struct KeyDecoder {
    bytes: Vec<u8>,
    paste: Option<PasteBuffer>,
    escape_started: Option<Instant>,
}

impl KeyDecoder {
    /// Adds a terminal byte fragment and emits every complete input event.
    #[must_use]
    pub fn feed(&mut self, bytes: &[u8], now: Instant) -> Vec<InputEvent> {
        self.bytes.extend_from_slice(bytes);
        self.decode_available(now)
    }

    /// Applies the timeout rules when no new bytes arrived.
    #[must_use]
    pub fn tick(&mut self, now: Instant) -> Vec<InputEvent> {
        self.decode_available(now)
    }

    fn decode_available(&mut self, now: Instant) -> Vec<InputEvent> {
        let mut events = Vec::new();
        loop {
            if self.decode_paste(now, &mut events) {
                continue;
            }
            if self.paste.is_some() {
                break;
            }
            if self.bytes.starts_with(b"\x1b[200~") {
                self.bytes.drain(..6);
                self.paste = Some(PasteBuffer {
                    bytes: Vec::new(),
                    started: now,
                });
                continue;
            }
            if is_prefix(&self.bytes, b"\x1b[200~") || is_prefix(&self.bytes, b"\x1b[201~") {
                let started = *self.escape_started.get_or_insert(now);
                if now.saturating_duration_since(started) < Duration::from_millis(50) {
                    break;
                }
            }
            if self.bytes.starts_with(b"\x1b[201~") {
                self.bytes.drain(..6);
                continue;
            }
            if self.bytes.first() == Some(&0x1b) {
                if self.decode_escape(now, &mut events) {
                    continue;
                }
                break;
            }
            if self.bytes.is_empty() {
                break;
            }
            if self.decode_utf8(&mut events) {
                continue;
            }
            break;
        }
        events
    }

    fn decode_paste(&mut self, now: Instant, events: &mut Vec<InputEvent>) -> bool {
        let Some(paste) = self.paste.as_mut() else {
            return false;
        };
        if let Some(end) = find_bytes(&self.bytes, b"\x1b[201~") {
            paste.bytes.extend_from_slice(&self.bytes[..end]);
            self.bytes.drain(..end + 6);
            if let Some(paste) = self.paste.take() {
                events.push(InputEvent::Paste(paste.bytes));
            }
            return true;
        }
        if now.saturating_duration_since(paste.started) >= Duration::from_secs(2) {
            paste.bytes.append(&mut self.bytes);
            if let Some(paste) = self.paste.take() {
                events.push(InputEvent::Paste(paste.bytes));
            }
            return true;
        }
        let suffix = marker_prefix_suffix(&self.bytes, b"\x1b[201~");
        let content_len = self.bytes.len().saturating_sub(suffix);
        paste.bytes.extend_from_slice(&self.bytes[..content_len]);
        self.bytes.drain(..content_len);
        false
    }

    fn decode_escape(&mut self, now: Instant, events: &mut Vec<InputEvent>) -> bool {
        let started = *self.escape_started.get_or_insert(now);
        match parse_escape(&self.bytes) {
            EscapeParse::Key(key, consumed) => {
                self.bytes.drain(..consumed);
                self.escape_started = None;
                events.push(InputEvent::Key(key));
                true
            }
            EscapeParse::Consumed(consumed) => {
                self.bytes.drain(..consumed);
                self.escape_started = None;
                true
            }
            EscapeParse::Partial
                if now.saturating_duration_since(started) >= Duration::from_millis(50) =>
            {
                self.bytes.remove(0);
                self.escape_started = None;
                events.push(InputEvent::Key(Key::new(KeyCode::Esc, KeyModifiers::NONE)));
                true
            }
            EscapeParse::Partial => false,
        }
    }

    fn decode_utf8(&mut self, events: &mut Vec<InputEvent>) -> bool {
        let first = self.bytes[0];
        if let Some(key) = control_key(first) {
            self.bytes.remove(0);
            events.push(InputEvent::Key(key));
            return true;
        }
        match std::str::from_utf8(&self.bytes) {
            Ok(text) => {
                let Some(character) = text.chars().next() else {
                    return false;
                };
                self.bytes.drain(..character.len_utf8());
                events.push(InputEvent::Key(Key::new(
                    KeyCode::Char(character),
                    KeyModifiers::NONE,
                )));
                true
            }
            Err(error) if error.valid_up_to() > 0 => {
                let valid = &self.bytes[..error.valid_up_to()];
                let Ok(valid) = std::str::from_utf8(valid) else {
                    return false;
                };
                let Some(character) = valid.chars().next() else {
                    return false;
                };
                self.bytes.drain(..character.len_utf8());
                events.push(InputEvent::Key(Key::new(
                    KeyCode::Char(character),
                    KeyModifiers::NONE,
                )));
                true
            }
            Err(error) => {
                let Some(invalid_len) = error.error_len() else {
                    return false;
                };
                let mut consumed = invalid_len.max(1).min(self.bytes.len());
                while consumed < self.bytes.len() && (0x80..=0xBF).contains(&self.bytes[consumed]) {
                    consumed += 1;
                }
                self.bytes.drain(..consumed);
                events.push(InputEvent::Key(Key::new(
                    KeyCode::Char('\u{fffd}'),
                    KeyModifiers::NONE,
                )));
                true
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EscapeParse {
    Key(Key, usize),
    Consumed(usize),
    Partial,
}

fn parse_escape(bytes: &[u8]) -> EscapeParse {
    if bytes.len() == 1 {
        return EscapeParse::Partial;
    }
    match bytes[1] {
        b'O' => match bytes.get(2) {
            Some(b'A') => EscapeParse::Key(Key::new(KeyCode::Up, KeyModifiers::NONE), 3),
            Some(b'B') => EscapeParse::Key(Key::new(KeyCode::Down, KeyModifiers::NONE), 3),
            Some(b'C') => EscapeParse::Key(Key::new(KeyCode::Right, KeyModifiers::NONE), 3),
            Some(b'D') => EscapeParse::Key(Key::new(KeyCode::Left, KeyModifiers::NONE), 3),
            Some(b'H') => EscapeParse::Key(Key::new(KeyCode::Home, KeyModifiers::NONE), 3),
            Some(b'F') => EscapeParse::Key(Key::new(KeyCode::End, KeyModifiers::NONE), 3),
            Some(b'P') => EscapeParse::Key(Key::new(KeyCode::F(1), KeyModifiers::NONE), 3),
            Some(b'R') => EscapeParse::Key(Key::new(KeyCode::F(3), KeyModifiers::NONE), 3),
            Some(_) => EscapeParse::Consumed(3.min(bytes.len())),
            None => EscapeParse::Partial,
        },
        b'[' => parse_csi(bytes),
        0x7f => EscapeParse::Key(Key::new(KeyCode::Backspace, KeyModifiers::ALT), 2),
        byte if byte.is_ascii() => EscapeParse::Key(
            Key::new(KeyCode::Char(char::from(byte)), KeyModifiers::ALT),
            2,
        ),
        _ => parse_alt_utf8(bytes),
    }
}

fn parse_alt_utf8(bytes: &[u8]) -> EscapeParse {
    match std::str::from_utf8(&bytes[1..]) {
        Ok(text) => text
            .chars()
            .next()
            .map_or(EscapeParse::Partial, |character| {
                EscapeParse::Key(
                    Key::new(KeyCode::Char(character), KeyModifiers::ALT),
                    1 + character.len_utf8(),
                )
            }),
        Err(error) if error.valid_up_to() > 0 => {
            let valid = &bytes[1..=error.valid_up_to()];
            let Ok(valid) = std::str::from_utf8(valid) else {
                return EscapeParse::Partial;
            };
            let Some(character) = valid.chars().next() else {
                return EscapeParse::Partial;
            };
            EscapeParse::Key(
                Key::new(KeyCode::Char(character), KeyModifiers::ALT),
                1 + character.len_utf8(),
            )
        }
        Err(error) if error.error_len().is_some() => {
            EscapeParse::Key(Key::new(KeyCode::Char('\u{fffd}'), KeyModifiers::ALT), 2)
        }
        Err(_) => EscapeParse::Partial,
    }
}

fn parse_csi(bytes: &[u8]) -> EscapeParse {
    let Some(final_index) = bytes[2..]
        .iter()
        .position(|byte| matches!(*byte, b'@'..=b'~'))
        .map(|index| index + 2)
    else {
        return EscapeParse::Partial;
    };
    let parameters = &bytes[2..final_index];
    if parameters.iter().any(|byte| {
        !matches!(
            *byte,
            b'0'..=b'9' | b';' | b':' | b'?' | b'<' | b'=' | b'>' | b'!'
        )
    }) {
        return EscapeParse::Consumed(final_index + 1);
    }
    let final_byte = bytes[final_index];
    let Some((key_code, modifier_param)) = parse_csi_key(parameters, final_byte) else {
        return EscapeParse::Consumed(final_index + 1);
    };
    let Some(key_code) = key_code else {
        return EscapeParse::Consumed(final_index + 1);
    };
    let modifiers = parse_modifiers(modifier_param);
    EscapeParse::Key(Key::new(key_code, modifiers), final_index + 1)
}

fn parse_csi_key(parameters: &[u8], final_byte: u8) -> Option<(Option<KeyCode>, Option<u16>)> {
    let parameter = std::str::from_utf8(parameters).ok()?;
    let mut values = parameter.split(';');
    let first = values.next().unwrap_or_default();
    let second = values.next().and_then(|value| value.parse::<u16>().ok());
    let code = match final_byte {
        b'A' => Some(KeyCode::Up),
        b'B' => Some(KeyCode::Down),
        b'C' => Some(KeyCode::Right),
        b'D' => Some(KeyCode::Left),
        b'H' => Some(KeyCode::Home),
        b'F' => Some(KeyCode::End),
        b'Z' => Some(KeyCode::BackTab),
        b'~' => match first.parse::<u8>().ok()? {
            5 => Some(KeyCode::PageUp),
            6 => Some(KeyCode::PageDown),
            1 | 7 => Some(KeyCode::Home),
            4 | 8 => Some(KeyCode::End),
            _ => None,
        },
        b'u' => {
            let scalar = first.parse::<u32>().ok()?;
            match scalar {
                13 => Some(KeyCode::Enter),
                9 => Some(KeyCode::Tab),
                27 => Some(KeyCode::Esc),
                value => char::from_u32(value).map(KeyCode::Char),
            }
        }
        _ => None,
    };
    Some((code, second))
}

fn parse_modifiers(value: Option<u16>) -> KeyModifiers {
    let flags = value.unwrap_or(1).saturating_sub(1);
    let mut modifiers = KeyModifiers::NONE;
    if flags & 1 != 0 {
        modifiers.insert(KeyModifiers::SHIFT);
    }
    if flags & 2 != 0 {
        modifiers.insert(KeyModifiers::ALT);
    }
    if flags & 4 != 0 {
        modifiers.insert(KeyModifiers::CONTROL);
    }
    modifiers
}

fn control_key(byte: u8) -> Option<Key> {
    match byte {
        b'\r' => Some(Key::new(KeyCode::Enter, KeyModifiers::NONE)),
        b'\t' => Some(Key::new(KeyCode::Tab, KeyModifiers::NONE)),
        0x7f => Some(Key::new(KeyCode::Backspace, KeyModifiers::NONE)),
        0 => Some(Key::new(KeyCode::Char(' '), KeyModifiers::CONTROL)),
        1..=26 => Some(Key::new(
            KeyCode::Char(char::from(b'a' + byte - 1)),
            KeyModifiers::CONTROL,
        )),
        _ => None,
    }
}

fn is_prefix(bytes: &[u8], marker: &[u8]) -> bool {
    bytes.len() < marker.len() && marker.starts_with(bytes)
}

fn find_bytes(bytes: &[u8], marker: &[u8]) -> Option<usize> {
    bytes
        .windows(marker.len())
        .position(|window| window == marker)
}

fn marker_prefix_suffix(bytes: &[u8], marker: &[u8]) -> usize {
    (1..=bytes.len().min(marker.len().saturating_sub(1)))
        .rev()
        .find(|length| bytes.ends_with(&marker[..*length]))
        .unwrap_or(0)
}
