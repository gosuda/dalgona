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
    /// When the incomplete escape prefix at the head of `bytes` first
    /// arrived. It is `Some` only while such a prefix is waiting: every
    /// escape-path consumption of head bytes goes through `consume`, which
    /// drops it, and an empty buffer never arms it, so a consumed marker or
    /// an idle decoder cannot leave a clock running for the next Esc.
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
            if self.bytes.is_empty() {
                break;
            }
            if self.bytes.starts_with(b"\x1b[200~") {
                self.consume(6);
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
                self.consume(6);
                continue;
            }
            if self.bytes.first() == Some(&0x1b) {
                if self.decode_escape(now, &mut events) {
                    continue;
                }
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

    /// Drops `count` bytes from the head of the buffer. The escape window
    /// belongs to the incomplete prefix being waited on, so consuming the
    /// head ends it; whatever follows starts its own window when it is next
    /// seen.
    fn consume(&mut self, count: usize) {
        self.bytes.drain(..count);
        self.escape_started = None;
    }

    fn decode_escape(&mut self, now: Instant, events: &mut Vec<InputEvent>) -> bool {
        let started = *self.escape_started.get_or_insert(now);
        match parse_escape(&self.bytes) {
            EscapeParse::Key(key, consumed) => {
                self.consume(consumed);
                events.push(InputEvent::Key(key));
                true
            }
            EscapeParse::Consumed(consumed) => {
                self.consume(consumed);
                true
            }
            EscapeParse::Partial
                if now.saturating_duration_since(started) >= Duration::from_millis(50) =>
            {
                self.consume(1);
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
        b'O' => parse_ss3(bytes),
        b'[' => parse_csi(bytes),
        0x08 | 0x7f => EscapeParse::Key(Key::new(KeyCode::Backspace, KeyModifiers::ALT), 2),
        // Meta sends a second Esc ahead of a sequence, so `\x1b\x1b[D` is
        // the legacy wire form of Alt+Left and `\x1b\x1bOA` of Alt+Up:
        // decode the inner sequence and attach ALT. Anything else behind
        // the second Esc is a real Esc press that re-parses on its own.
        0x1b => match bytes.get(2) {
            Some(b'[') => alt_of(parse_csi(&bytes[1..])),
            Some(b'O') => alt_of(parse_ss3(&bytes[1..])),
            Some(_) => EscapeParse::Key(Key::new(KeyCode::Esc, KeyModifiers::NONE), 1),
            None => EscapeParse::Partial,
        },
        // Esc ahead of a control byte is an Alt+Ctrl chord (the crossterm
        // convention): `control_key` names the chord and ALT marks it. No
        // Alt+Ctrl action is bound, so the chord stays inert — it must
        // never split into Esc plus the plain control key, which would
        // turn Alt+Enter into a submit and Alt+Ctrl+C into an interrupt.
        byte if byte < 0x20 => control_key(byte).map_or(EscapeParse::Consumed(2), |mut key| {
            key.modifiers.insert(KeyModifiers::ALT);
            EscapeParse::Key(key, 2)
        }),
        byte if byte.is_ascii() => EscapeParse::Key(
            Key::new(KeyCode::Char(char::from(byte)), KeyModifiers::ALT),
            2,
        ),
        _ => parse_alt_utf8(bytes),
    }
}

fn parse_ss3(bytes: &[u8]) -> EscapeParse {
    match bytes.get(2) {
        Some(b'A') => EscapeParse::Key(Key::new(KeyCode::Up, KeyModifiers::NONE), 3),
        Some(b'B') => EscapeParse::Key(Key::new(KeyCode::Down, KeyModifiers::NONE), 3),
        Some(b'C') => EscapeParse::Key(Key::new(KeyCode::Right, KeyModifiers::NONE), 3),
        Some(b'D') => EscapeParse::Key(Key::new(KeyCode::Left, KeyModifiers::NONE), 3),
        Some(b'H') => EscapeParse::Key(Key::new(KeyCode::Home, KeyModifiers::NONE), 3),
        Some(b'F') => EscapeParse::Key(Key::new(KeyCode::End, KeyModifiers::NONE), 3),
        Some(b'P') => EscapeParse::Key(Key::new(KeyCode::F(1), KeyModifiers::NONE), 3),
        Some(b'R') => EscapeParse::Key(Key::new(KeyCode::F(3), KeyModifiers::NONE), 3),
        // An Esc aborts the dead SS3 the way it aborts a dead CSI: drop
        // the `\x1bO` and keep the Esc for its own re-parse.
        Some(0x1b) => EscapeParse::Consumed(2),
        Some(_) => EscapeParse::Consumed(3.min(bytes.len())),
        None => EscapeParse::Partial,
    }
}

/// Attaches ALT to a sequence reached through the meta prefix
/// (`\x1b\x1b[...` / `\x1b\x1bO.`), charging the extra Esc byte.
fn alt_of(parse: EscapeParse) -> EscapeParse {
    match parse {
        EscapeParse::Key(mut key, consumed) => {
            key.modifiers.insert(KeyModifiers::ALT);
            EscapeParse::Key(key, consumed + 1)
        }
        EscapeParse::Consumed(consumed) => EscapeParse::Consumed(consumed + 1),
        EscapeParse::Partial => EscapeParse::Partial,
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
        // An invalid UTF-8 start byte behind Esc (a stray continuation, a
        // C1 control, or a 0xf8+ lead) is not an Alt chord — it is corrupt
        // input that must not eat the Esc. Emit Esc and let the byte
        // re-parse on its own (decode_utf8 reports it as U+FFFD).
        Err(error) if error.error_len().is_some() => {
            EscapeParse::Key(Key::new(KeyCode::Esc, KeyModifiers::NONE), 1)
        }
        Err(_) => EscapeParse::Partial,
    }
}

fn parse_csi(bytes: &[u8]) -> EscapeParse {
    let remainder = &bytes[2..];
    let control_index = remainder.iter().position(|byte| *byte < 0x20);
    let final_index = remainder
        .iter()
        .position(|byte| matches!(*byte, b'@'..=b'~'));
    if let Some(index) = control_index
        && final_index.is_none_or(|final_index| index < final_index)
    {
        if remainder[index] == 0x1b {
            return EscapeParse::Consumed(index + 2);
        }
        let consumed = index + 3;
        return sequence_control_key(remainder[index])
            .map_or(EscapeParse::Consumed(consumed), |key| {
                EscapeParse::Key(key, consumed)
            });
    }
    let Some(final_index) = final_index.map(|index| index + 2) else {
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
    let Some((key_code, modifiers)) = parse_csi_key(parameters, final_byte) else {
        return EscapeParse::Consumed(final_index + 1);
    };
    let Some(key_code) = key_code else {
        return EscapeParse::Consumed(final_index + 1);
    };
    EscapeParse::Key(Key::new(key_code, modifiers), final_index + 1)
}

fn sequence_control_key(byte: u8) -> Option<Key> {
    if byte == 0x1b {
        return Some(Key::new(KeyCode::Esc, KeyModifiers::NONE));
    }
    control_key(byte)
}

fn parse_csi_key(parameters: &[u8], final_byte: u8) -> Option<(Option<KeyCode>, KeyModifiers)> {
    let parameter = std::str::from_utf8(parameters).ok()?;
    let mut values = parameter.split(';');
    // Alternate key fields would arrive as `code:shifted[:base]`; only the
    // unshifted code feeds `Key`, the alternates carry no binding meaning.
    let first = values
        .next()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default();
    // Kitty modifier and event-type fields arrive as `mods:event` inside
    // the second parameter; a bare `m` still parses. Release events are
    // not keys and drop silently; press and repeat emit the key.
    let mut second_field = values.next().unwrap_or_default().split(':');
    let modifier_param = second_field.next().unwrap_or_default();
    if second_field.next().is_some_and(|event| event == "3") {
        return Some((None, KeyModifiers::NONE));
    }
    let modifiers = parse_modifiers(modifier_param.parse::<u16>().ok());
    let code = match final_byte {
        b'A' => Some(KeyCode::Up),
        b'B' => Some(KeyCode::Down),
        b'C' => Some(KeyCode::Right),
        b'D' => Some(KeyCode::Left),
        b'H' => Some(KeyCode::Home),
        b'F' => Some(KeyCode::End),
        b'Z' => Some(KeyCode::BackTab),
        b'~' => match first.parse::<u8>().ok()? {
            3 => Some(KeyCode::Delete),
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
                8 | 127 => Some(KeyCode::Backspace),
                value => char::from_u32(value).map(KeyCode::Char),
            }
        }
        _ => None,
    };
    Some((code, modifiers))
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
        0x08 | 0x7f => Some(Key::new(KeyCode::Backspace, KeyModifiers::NONE)),
        0 => Some(Key::new(KeyCode::Char(' '), KeyModifiers::CONTROL)),
        1..=26 => Some(Key::new(
            KeyCode::Char(char::from(b'a' + byte - 1)),
            KeyModifiers::CONTROL,
        )),
        0x1c => Some(Key::new(KeyCode::Char('\\'), KeyModifiers::CONTROL)),
        0x1d => Some(Key::new(KeyCode::Char(']'), KeyModifiers::CONTROL)),
        0x1e => Some(Key::new(KeyCode::Char('^'), KeyModifiers::CONTROL)),
        0x1f => Some(Key::new(KeyCode::Char('_'), KeyModifiers::CONTROL)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{InputEvent, KeyDecoder};
    use crate::keys::{Action, Key, Owner, resolve_in};
    use crossterm::event::{KeyCode, KeyModifiers};
    use std::time::{Duration, Instant};

    /// Decodes one wire burst into its first key, failing on any other shape.
    fn decoded(bytes: &[u8]) -> Key {
        match KeyDecoder::default().feed(bytes, Instant::now()).as_slice() {
            [InputEvent::Key(key)] => *key,
            other => panic!("one key expected from {bytes:?}, got {other:?}"),
        }
    }

    /// Asserts a wire burst resolves to `action` under `owners` in both modes.
    fn expect_action(bytes: &[u8], owners: &[Owner], action: Action) {
        let key = decoded(bytes);
        for kitty in [false, true] {
            assert_eq!(
                resolve_in(key, kitty, owners),
                Some(action),
                "{bytes:?} kitty={kitty}"
            );
        }
    }

    #[test]
    fn meta_prefix_decodes_alt_sequences_and_keeps_double_esc() {
        let start = Instant::now();
        // Meta sends a second Esc before a sequence: `\x1b\x1b[D` is the
        // legacy Alt+Left and resolves WordLeft, never Esc+arrow.
        assert_eq!(
            decoded(b"\x1b\x1b[D"),
            Key::new(KeyCode::Left, KeyModifiers::ALT)
        );
        let composer = [Owner::Composer];
        expect_action(b"\x1b\x1b[D", &composer, Action::WordLeft);
        assert_eq!(
            decoded(b"\x1b\x1bOA"),
            Key::new(KeyCode::Up, KeyModifiers::ALT)
        );
        assert_eq!(
            decoded(b"\x1b\x1b[98;5u"),
            Key::new(
                KeyCode::Char('b'),
                KeyModifiers::ALT.union(KeyModifiers::CONTROL)
            )
        );
        // A pair of Esc bytes cannot be told from a meta prefix until the
        // next byte or the timeout, so the decoder holds it instead of
        // emitting an Esc early: fed to a fresh decoder, a chord split at
        // any read boundary still decodes whole.
        for wire in [&b"\x1b\x1b[D"[..], b"\x1b\x1bOA", b"\x1b\x1b[98;5u"] {
            let whole = decoded(wire);
            for split in 1..wire.len() {
                let mut decoder = KeyDecoder::default();
                let mut events = decoder.feed(&wire[..split], start);
                events.extend(decoder.feed(&wire[split..], start));
                assert_eq!(
                    events,
                    [InputEvent::Key(whole)],
                    "{wire:?} split at {split}"
                );
            }
        }
        // With nothing behind it the pair is two real Esc presses, resolved
        // only once the ambiguity window closes.
        let esc = || InputEvent::Key(Key::new(KeyCode::Esc, KeyModifiers::NONE));
        let mut double = KeyDecoder::default();
        assert_eq!(double.feed(b"\x1b\x1b", start), []);
        assert_eq!(
            double.tick(start + Duration::from_millis(49)),
            [],
            "the pair stays ambiguous inside the fifty millisecond window"
        );
        let mut settled = double.tick(start + Duration::from_millis(50));
        settled.extend(double.tick(start + Duration::from_millis(100)));
        assert_eq!(settled, [esc(), esc()]);
        assert_eq!(double.tick(start + Duration::from_millis(150)), []);
    }

    #[test]
    fn escape_window_opens_when_the_esc_arrives_not_when_the_decoder_idles() {
        let start = Instant::now();
        let key = |code, modifiers| InputEvent::Key(Key::new(code, modifiers));
        let mut decoder = KeyDecoder::default();
        // The runtime ticks on every idle read, and a decoded key leaves an
        // empty buffer behind; neither may start the clock of an Esc that
        // has not arrived yet.
        assert_eq!(decoder.tick(start), []);
        assert_eq!(
            decoder.feed(b"a", start),
            [key(KeyCode::Char('a'), KeyModifiers::NONE)]
        );
        let press = start + Duration::from_secs(1);
        assert_eq!(decoder.feed(b"\x1b", press), []);
        assert_eq!(
            decoder.feed(b"[D", press + Duration::from_millis(10)),
            [key(KeyCode::Left, KeyModifiers::NONE)]
        );
        // A meta prefix split across reads after more idle time still joins.
        assert_eq!(decoder.tick(press + Duration::from_secs(1)), []);
        let chord = press + Duration::from_secs(2);
        assert_eq!(decoder.feed(b"\x1b\x1b", chord), []);
        assert_eq!(
            decoder.feed(b"[D", chord + Duration::from_millis(10)),
            [key(KeyCode::Left, KeyModifiers::ALT)]
        );
        // After the idle gap a lone Esc still resolves exactly when its own
        // fifty millisecond window closes.
        let lone = chord + Duration::from_secs(1);
        assert_eq!(decoder.tick(lone), []);
        assert_eq!(decoder.feed(b"\x1b", lone), []);
        assert_eq!(decoder.tick(lone + Duration::from_millis(49)), []);
        assert_eq!(
            decoder.tick(lone + Duration::from_millis(50)),
            [key(KeyCode::Esc, KeyModifiers::NONE)]
        );
    }

    #[test]
    fn consumed_paste_markers_leave_no_escape_window_behind() {
        let start = Instant::now();
        let key = |code| InputEvent::Key(Key::new(code, KeyModifiers::NONE));
        // A paste marker split across reads arms the window while it is
        // incomplete. Once the rest arrives and the marker is consumed, an
        // Esc behind it is a new fragment with a window of its own.
        let armed_then_consumed = |tail: &[u8]| {
            let mut decoder = KeyDecoder::default();
            assert_eq!(decoder.feed(b"\x1b[20", start), []);
            let events = decoder.feed(tail, start + Duration::from_millis(40));
            (decoder, events)
        };
        let paste = vec![InputEvent::Paste(b"hi".to_vec())];
        let cases = [
            // The start marker is consumed, the paste closes, an Esc trails.
            (&b"0~hi\x1b[201~\x1b"[..], paste),
            // The marker turns out to be a stray paste end.
            (&b"1~\x1b"[..], vec![]),
        ];
        for (tail, expected) in cases {
            let (mut decoder, events) = armed_then_consumed(tail);
            assert_eq!(events, expected, "{tail:?}");
            assert_eq!(
                decoder.tick(start + Duration::from_millis(60)),
                [],
                "the trailing Esc is twenty milliseconds old: {tail:?}"
            );
            assert_eq!(
                decoder.tick(start + Duration::from_millis(90)),
                [key(KeyCode::Esc)],
                "its own window closes fifty milliseconds after it arrived: {tail:?}"
            );
            let (mut decoder, _) = armed_then_consumed(tail);
            assert_eq!(
                decoder.feed(b"[D", start + Duration::from_millis(70)),
                [key(KeyCode::Left)],
                "{tail:?}"
            );
        }
    }

    #[test]
    fn escape_aborts_a_dead_ss3_without_losing_the_escape() {
        assert_eq!(
            decoded(b"\x1bO\x1b[C"),
            Key::new(KeyCode::Right, KeyModifiers::NONE)
        );
    }

    #[test]
    fn escape_before_a_control_byte_is_an_alt_ctrl_chord() {
        let start = Instant::now();
        // `\x1b\x03` is Alt+Ctrl+C: no action binds it, so it stays inert
        // instead of splitting into Esc plus a plain Ctrl+C interrupt.
        assert_eq!(
            decoded(b"\x1b\x03"),
            Key::new(
                KeyCode::Char('c'),
                KeyModifiers::ALT.union(KeyModifiers::CONTROL)
            )
        );
        // `\x1b\r` is Alt+Enter, not Esc + submit.
        assert_eq!(
            decoded(b"\x1b\r"),
            Key::new(KeyCode::Enter, KeyModifiers::ALT)
        );
        // A stray C1 byte behind Esc is corrupt input: the Esc survives.
        let mut c1 = KeyDecoder::default();
        assert_eq!(
            c1.feed(b"\x1b\x9b", start),
            [
                InputEvent::Key(Key::new(KeyCode::Esc, KeyModifiers::NONE)),
                InputEvent::Key(Key::new(KeyCode::Char('\u{fffd}'), KeyModifiers::NONE)),
            ]
        );
    }

    #[test]
    fn kitty_event_subfields_resolve_modifiers_and_releases_drop() {
        let start = Instant::now();
        let mut decoder = KeyDecoder::default();
        assert_eq!(
            decoder.feed(b"\x1b[98;5:2u", start),
            [InputEvent::Key(Key::new(
                KeyCode::Char('b'),
                KeyModifiers::CONTROL
            ))]
        );
        assert_eq!(
            decoder.feed(b"\x1b[1;5:3A", start),
            [],
            "release events are not keys and must not resolve"
        );
        assert_eq!(
            decoder.feed(b"\x1b[5;2:3~", start),
            [],
            "release events drop for tilde sequences too"
        );
        assert_eq!(
            decoder.feed(b"\x1b[6;2:2~", start),
            [InputEvent::Key(Key::new(
                KeyCode::PageDown,
                KeyModifiers::SHIFT
            ))]
        );
        assert_eq!(
            decoded(b"\x1b[1;5:2A"),
            Key::new(KeyCode::Up, KeyModifiers::CONTROL),
            "repeat events still decode to the key"
        );
        expect_action(b"\x1b[1;5:2A", &[Owner::App], Action::PrevPrompt);
    }

    #[test]
    fn documented_new_bindings_resolve_from_the_wire() {
        let dialog = [Owner::Dialog];
        for bytes in [&b"\x1b[Z"[..], b"\x1b[9;2u"] {
            expect_action(bytes, &dialog, Action::DialogFocusLeft);
        }
        for (bytes, action) in [
            (&b"\x1b[5~"[..], Action::DialogScrollUp),
            (b"\x1b[6~", Action::DialogScrollDown),
        ] {
            expect_action(bytes, &dialog, action);
        }
        let picker = [Owner::Picker];
        for bytes in [&b" "[..], b"\x1b[32;5u", &[0x00][..]] {
            expect_action(bytes, &picker, Action::PickerToggle);
        }
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
