//! The JSON argument scanner: incremental validation, string decoding, and
//! the selected-string reader.
//!
//! [`JsonScan`] validates and decodes; a [`JsonSink`] turns structure into
//! [`ReaderEvent`]s. The line-reader layer reuses both for JSON patch
//! envelopes.

use super::{
    ArgReader, KEY_PATH, KEY_SELECTED, MAX_FRAMES, MAX_KEY_BYTES, MAX_PATH_BYTES, REPLACEMENT,
    ReaderEvent,
};
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ObjPhase {
    KeyOrEnd,
    Key,
    Colon,
    Value,
    CommaOrEnd,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArrPhase {
    ValueOrEnd,
    Value,
    CommaOrEnd,
}

#[derive(Clone, Copy, Debug)]
enum Frame {
    Object { phase: ObjPhase, key: u8 },
    Array { phase: ArrPhase, key: u8 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Num {
    Minus,
    Zero,
    Int,
    Dot,
    Frac,
    Exp,
    ExpSign,
    ExpDigits,
}

impl Num {
    fn step(self, b: u8) -> Option<Self> {
        let digit = b.is_ascii_digit();
        match self {
            Self::Minus if b == b'0' => Some(Self::Zero),
            Self::Minus | Self::Int if digit => Some(Self::Int),
            Self::Zero | Self::Int if b == b'.' => Some(Self::Dot),
            Self::Zero | Self::Int | Self::Frac if matches!(b, b'e' | b'E') => Some(Self::Exp),
            Self::Dot | Self::Frac if digit => Some(Self::Frac),
            Self::Exp if matches!(b, b'+' | b'-') => Some(Self::ExpSign),
            Self::Exp | Self::ExpSign | Self::ExpDigits if digit => Some(Self::ExpDigits),
            _ => None,
        }
    }

    fn accepts(self) -> bool {
        matches!(self, Self::Zero | Self::Int | Self::Frac | Self::ExpDigits)
    }
}

#[derive(Clone, Copy, Debug)]
enum Esc {
    None,
    Backslash,
    Unicode { digits: u8, value: u32 },
}

#[derive(Clone, Copy, Debug)]
enum Lex {
    Between,
    Str { key: bool, esc: Esc },
    Num(Num),
    Lit { word: &'static [u8], at: usize },
}

#[derive(Clone, Copy, Debug)]
enum Status {
    Open,
    Stopped,
}

/// Where a string value sits in the document.
#[derive(Clone, Copy, Debug)]
pub(super) struct StrCtx {
    pub(super) key: u8,
    pub(super) depth: usize,
    pub(super) in_object: bool,
}

/// Receives the structure and decoded strings found by [`JsonScan`].
pub(super) trait JsonSink {
    fn classify_key(&self, key: &str) -> u8;
    fn object_start(&mut self, out: &mut Vec<ReaderEvent>);
    fn object_end(&mut self, out: &mut Vec<ReaderEvent>);
    /// Returns whether the decoded text of this string value is wanted.
    fn string_start(&mut self, ctx: StrCtx) -> bool;
    fn string_text(&mut self, text: &str, out: &mut Vec<ReaderEvent>);
    fn string_end(&mut self, out: &mut Vec<ReaderEvent>);
}

#[derive(Debug, Default)]
struct KeyBuf {
    bytes: Vec<u8>,
    over: bool,
}

/// An incremental JSON validator and string decoder with bounded state.
#[derive(Debug)]
pub(super) struct JsonScan {
    frames: Vec<Frame>,
    lex: Lex,
    high: Option<u32>,
    key: KeyBuf,
    deliver: bool,
    seg: Vec<u8>,
    top_done: bool,
    status: Status,
}

impl JsonScan {
    pub(super) fn new() -> Self {
        Self {
            frames: Vec::new(),
            lex: Lex::Between,
            high: None,
            key: KeyBuf::default(),
            deliver: false,
            seg: Vec::new(),
            top_done: false,
            status: Status::Open,
        }
    }

    pub(super) fn stopped(&self) -> bool {
        matches!(self.status, Status::Stopped)
    }

    pub(super) fn feed(
        &mut self,
        bytes: &[u8],
        sink: &mut impl JsonSink,
        out: &mut Vec<ReaderEvent>,
    ) {
        if self.stopped() {
            return;
        }
        for &b in bytes {
            if !self.step(b, sink, out) {
                self.flush(sink, out);
                self.status = Status::Stopped;
                return;
            }
        }
        self.flush(sink, out);
    }

    pub(super) fn close(&mut self, sink: &mut impl JsonSink, out: &mut Vec<ReaderEvent>) {
        if !self.stopped()
            && matches!(self.lex, Lex::Str { key: false, .. })
            && self.deliver
            && self.high.take().is_some()
        {
            self.seg.extend_from_slice(REPLACEMENT.as_bytes());
            self.flush(sink, out);
        }
        self.status = Status::Stopped;
        while let Some(frame) = self.frames.pop() {
            if matches!(frame, Frame::Object { .. }) {
                sink.object_end(out);
            }
        }
    }

    fn flush(&mut self, sink: &mut impl JsonSink, out: &mut Vec<ReaderEvent>) {
        if !self.seg.is_empty() {
            let text = String::from_utf8_lossy(&self.seg).into_owned();
            self.seg.clear();
            sink.string_text(&text, out);
        }
    }

    fn step(&mut self, b: u8, sink: &mut impl JsonSink, out: &mut Vec<ReaderEvent>) -> bool {
        match self.lex {
            Lex::Between => self.between(b, sink, out),
            Lex::Str { key, esc } => self.string_byte(key, esc, b, sink, out),
            Lex::Num(num) => {
                if let Some(next) = num.step(b) {
                    self.lex = Lex::Num(next);
                    true
                } else if num.accepts() {
                    self.lex = Lex::Between;
                    self.value_done();
                    self.between(b, sink, out)
                } else {
                    false
                }
            }
            Lex::Lit { word, at } => {
                if word.get(at) != Some(&b) {
                    return false;
                }
                if at + 1 == word.len() {
                    self.lex = Lex::Between;
                    self.value_done();
                } else {
                    self.lex = Lex::Lit { word, at: at + 1 };
                }
                true
            }
        }
    }

    fn between(&mut self, b: u8, sink: &mut impl JsonSink, out: &mut Vec<ReaderEvent>) -> bool {
        if matches!(b, b' ' | b'\t' | b'\n' | b'\r') {
            return true;
        }
        match self.frames.last().copied() {
            None if self.top_done => false,
            None => self.value_start(b, 0, false, sink, out),
            Some(Frame::Object { phase, key }) => match (phase, b) {
                (ObjPhase::KeyOrEnd | ObjPhase::Key, b'"') => {
                    self.key = KeyBuf::default();
                    self.high = None;
                    self.lex = Lex::Str {
                        key: true,
                        esc: Esc::None,
                    };
                    true
                }
                (ObjPhase::KeyOrEnd | ObjPhase::CommaOrEnd, b'}') => {
                    self.end_container(sink, out);
                    true
                }
                (ObjPhase::Colon, b':') => self.set_object_phase(ObjPhase::Value),
                (ObjPhase::Value, _) => self.value_start(b, key, true, sink, out),
                (ObjPhase::CommaOrEnd, b',') => self.set_object_phase(ObjPhase::Key),
                _ => false,
            },
            Some(Frame::Array { phase, key }) => match (phase, b) {
                (ArrPhase::ValueOrEnd | ArrPhase::CommaOrEnd, b']') => {
                    self.end_container(sink, out);
                    true
                }
                (ArrPhase::ValueOrEnd | ArrPhase::Value, _) => {
                    self.value_start(b, key, false, sink, out)
                }
                (ArrPhase::CommaOrEnd, b',') => {
                    if let Some(Frame::Array { phase, .. }) = self.frames.last_mut() {
                        *phase = ArrPhase::Value;
                    }
                    true
                }
                _ => false,
            },
        }
    }

    fn set_object_phase(&mut self, next: ObjPhase) -> bool {
        if let Some(Frame::Object { phase, .. }) = self.frames.last_mut() {
            *phase = next;
        }
        true
    }

    fn value_start(
        &mut self,
        b: u8,
        key: u8,
        in_object: bool,
        sink: &mut impl JsonSink,
        out: &mut Vec<ReaderEvent>,
    ) -> bool {
        match b {
            b'{' | b'[' => {
                if self.frames.len() >= MAX_FRAMES {
                    return false;
                }
                if b == b'{' {
                    self.frames.push(Frame::Object {
                        phase: ObjPhase::KeyOrEnd,
                        key: 0,
                    });
                    sink.object_start(out);
                } else {
                    self.frames.push(Frame::Array {
                        phase: ArrPhase::ValueOrEnd,
                        key,
                    });
                }
            }
            b'"' => {
                let ctx = StrCtx {
                    key,
                    depth: self.frames.len(),
                    in_object,
                };
                self.deliver = sink.string_start(ctx);
                self.high = None;
                self.lex = Lex::Str {
                    key: false,
                    esc: Esc::None,
                };
            }
            b'-' => self.lex = Lex::Num(Num::Minus),
            b'0' => self.lex = Lex::Num(Num::Zero),
            b'1'..=b'9' => self.lex = Lex::Num(Num::Int),
            b't' => {
                self.lex = Lex::Lit {
                    word: b"true",
                    at: 1,
                }
            }
            b'f' => {
                self.lex = Lex::Lit {
                    word: b"false",
                    at: 1,
                }
            }
            b'n' => {
                self.lex = Lex::Lit {
                    word: b"null",
                    at: 1,
                }
            }
            _ => return false,
        }
        true
    }

    fn value_done(&mut self) {
        match self.frames.last_mut() {
            None => self.top_done = true,
            Some(Frame::Object { phase, .. }) => *phase = ObjPhase::CommaOrEnd,
            Some(Frame::Array { phase, .. }) => *phase = ArrPhase::CommaOrEnd,
        }
    }

    fn end_container(&mut self, sink: &mut impl JsonSink, out: &mut Vec<ReaderEvent>) {
        if let Some(Frame::Object { .. }) = self.frames.pop() {
            sink.object_end(out);
        }
        self.value_done();
    }

    fn string_byte(
        &mut self,
        key: bool,
        esc: Esc,
        b: u8,
        sink: &mut impl JsonSink,
        out: &mut Vec<ReaderEvent>,
    ) -> bool {
        match esc {
            Esc::None => match b {
                b'"' => {
                    self.resolve_high(key);
                    self.lex = Lex::Between;
                    if key {
                        self.key_end(sink);
                    } else {
                        self.flush(sink, out);
                        sink.string_end(out);
                        self.deliver = false;
                        self.value_done();
                    }
                }
                b'\\' => {
                    self.lex = Lex::Str {
                        key,
                        esc: Esc::Backslash,
                    };
                }
                0..=0x1F => return false,
                _ => {
                    self.resolve_high(key);
                    self.push_bytes(key, &[b]);
                }
            },
            Esc::Backslash => {
                let decoded = match b {
                    b'"' | b'\\' | b'/' => b,
                    b'b' => 0x08,
                    b'f' => 0x0C,
                    b'n' => b'\n',
                    b'r' => b'\r',
                    b't' => b'\t',
                    b'u' => {
                        self.lex = Lex::Str {
                            key,
                            esc: Esc::Unicode {
                                digits: 0,
                                value: 0,
                            },
                        };
                        return true;
                    }
                    _ => return false,
                };
                self.resolve_high(key);
                self.push_bytes(key, &[decoded]);
                self.lex = Lex::Str {
                    key,
                    esc: Esc::None,
                };
            }
            Esc::Unicode { digits, value } => {
                let Some(digit) = char::from(b).to_digit(16) else {
                    return false;
                };
                let value = (value << 4) | digit;
                if digits < 3 {
                    self.lex = Lex::Str {
                        key,
                        esc: Esc::Unicode {
                            digits: digits + 1,
                            value,
                        },
                    };
                } else {
                    self.code_unit(key, value);
                    self.lex = Lex::Str {
                        key,
                        esc: Esc::None,
                    };
                }
            }
        }
        true
    }

    /// Decodes one `\uXXXX` code unit, pairing surrogates across splits.
    fn code_unit(&mut self, key: bool, unit: u32) {
        if let Some(high) = self.high.take() {
            if (0xDC00..=0xDFFF).contains(&unit) {
                let scalar = 0x1_0000 + ((high - 0xD800) << 10) + (unit - 0xDC00);
                self.push_char(key, char::from_u32(scalar).unwrap_or('\u{FFFD}'));
                return;
            }
            self.push_char(key, '\u{FFFD}');
        }
        if (0xD800..=0xDBFF).contains(&unit) {
            self.high = Some(unit);
        } else {
            self.push_char(key, char::from_u32(unit).unwrap_or('\u{FFFD}'));
        }
    }

    fn resolve_high(&mut self, key: bool) {
        if self.high.take().is_some() {
            self.push_char(key, '\u{FFFD}');
        }
    }

    fn push_char(&mut self, key: bool, c: char) {
        let mut buf = [0; 4];
        self.push_bytes(key, c.encode_utf8(&mut buf).as_bytes());
    }

    fn push_bytes(&mut self, key: bool, bytes: &[u8]) {
        if key {
            if self.key.over || self.key.bytes.len() + bytes.len() > MAX_KEY_BYTES {
                self.key.over = true;
            } else {
                self.key.bytes.extend_from_slice(bytes);
            }
        } else if self.deliver {
            self.seg.extend_from_slice(bytes);
        }
    }

    fn key_end(&mut self, sink: &impl JsonSink) {
        let flags = if self.key.over {
            0
        } else {
            std::str::from_utf8(&self.key.bytes).map_or(0, |key| sink.classify_key(key))
        };
        if let Some(Frame::Object { phase, key }) = self.frames.last_mut() {
            *phase = ObjPhase::Colon;
            *key = flags;
        }
    }
}

// ---------------------------------------------------------------------------
// JSON reader

#[derive(Clone, Copy, Debug)]
pub(super) enum Select {
    EveryString,
    Members(&'static [&'static str]),
}

#[derive(Debug)]
enum PathState {
    Off,
    Collect(Vec<u8>),
    Over,
}

#[derive(Debug)]
struct JsonEvents {
    select: Select,
    items: Vec<bool>,
    top_added: bool,
    selected: bool,
    emitted: bool,
    path: PathState,
}

impl JsonSink for JsonEvents {
    fn classify_key(&self, key: &str) -> u8 {
        let mut flags = 0;
        if key == "path" {
            flags |= KEY_PATH;
        }
        if let Select::Members(names) = self.select
            && names.contains(&key)
        {
            flags |= KEY_SELECTED;
        }
        flags
    }

    fn object_start(&mut self, out: &mut Vec<ReaderEvent>) {
        self.items.push(false);
        out.push(ReaderEvent::ItemStart);
    }

    fn object_end(&mut self, out: &mut Vec<ReaderEvent>) {
        self.items.pop();
        out.push(ReaderEvent::ItemEnd);
    }

    fn string_start(&mut self, ctx: StrCtx) -> bool {
        self.selected = matches!(self.select, Select::EveryString) || ctx.key & KEY_SELECTED != 0;
        self.emitted = false;
        self.path = if ctx.key & KEY_PATH == 0 {
            PathState::Off
        } else {
            PathState::Collect(Vec::new())
        };
        self.selected || !matches!(self.path, PathState::Off)
    }

    fn string_text(&mut self, text: &str, out: &mut Vec<ReaderEvent>) {
        if let PathState::Collect(path) = &mut self.path {
            if path.len() + text.len() > MAX_PATH_BYTES {
                self.path = PathState::Over;
            } else {
                path.extend_from_slice(text.as_bytes());
            }
        }
        if self.selected && !text.is_empty() {
            if !self.emitted {
                let item = self.items.last_mut().unwrap_or(&mut self.top_added);
                if *item {
                    out.push(ReaderEvent::Added("\n".to_owned()));
                }
                *item = true;
                self.emitted = true;
            }
            out.push(ReaderEvent::Added(text.to_owned()));
        }
    }

    fn string_end(&mut self, out: &mut Vec<ReaderEvent>) {
        if let PathState::Collect(path) = std::mem::replace(&mut self.path, PathState::Off) {
            out.push(ReaderEvent::Path(
                String::from_utf8_lossy(&path).into_owned(),
            ));
        }
        self.selected = false;
    }
}

/// The JSON reader: objects are items, selected strings are added text.
#[derive(Debug)]
pub(super) struct JsonReader {
    scan: JsonScan,
    events: JsonEvents,
}

impl JsonReader {
    pub(super) fn new(select: Select) -> Self {
        Self {
            scan: JsonScan::new(),
            events: JsonEvents {
                select,
                items: Vec::new(),
                top_added: false,
                selected: false,
                emitted: false,
                path: PathState::Off,
            },
        }
    }
}

impl ArgReader for JsonReader {
    fn feed(&mut self, delta: &str, out: &mut Vec<ReaderEvent>) {
        self.scan.feed(delta.as_bytes(), &mut self.events, out);
    }

    fn close(&mut self, out: &mut Vec<ReaderEvent>) {
        self.scan.close(&mut self.events, out);
    }
}
