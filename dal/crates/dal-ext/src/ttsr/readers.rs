//! Typed argument readers for TTSR tool sources.
//!
//! A reader turns the raw argument bytes of one tool call into item and
//! added-text events while the call streams. Each byte is examined once; the
//! state is bounded by the nesting, key, path, and header-line limits below.

/// Maximum open JSON containers before emission stops until close.
const MAX_FRAMES: usize = 64;
/// Maximum decoded key bytes that may still match a member name.
const MAX_KEY_BYTES: usize = 256;
/// Maximum decoded path bytes that may still emit a path event.
const MAX_PATH_BYTES: usize = 4096;
/// Maximum bytes of one held patch header candidate line.
const MAX_HOLD_BYTES: usize = MAX_PATH_BYTES + 256;

const KEY_PATH: u8 = 1;
const KEY_SELECTED: u8 = 2;
const REPLACEMENT: &str = "\u{FFFD}";

/// One event read from a tool call's argument stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReaderEvent {
    /// An item begins, for example one file of a patch.
    ItemStart,
    /// The path of the current item.
    Path(String),
    /// Decoded text that the call adds, in stream order.
    Added(String),
    /// The current item ends.
    ItemEnd,
}

/// A reader for the argument stream of one tool call.
pub trait ArgReader {
    /// Reads the next argument delta and appends its events in order.
    fn feed(&mut self, delta: &str, out: &mut Vec<ReaderEvent>);
    /// Ends the call and appends the events still held, closing every open
    /// item innermost first.
    fn close(&mut self, out: &mut Vec<ReaderEvent>);
}

/// The active edit style of the patch tool, one per patch dialect.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EditStyle {
    /// `*** File:` sections with Find and action bodies.
    Anchor,
    /// JSON `changes` entries with `new` and `create` members.
    Replace,
    /// `[path#TAG]` sections with `PUT` row bodies.
    Hashline,
    /// Codex `*** Begin Patch` envelopes.
    ApplyPatch,
}

/// Returns the argument reader for one call of `tool`.
///
/// The patch tool uses the reader of `edit_style`: replace style emits the
/// `new` and `create` members; the line styles emit item start, path, added
/// rows only, and item end. Every other tool uses the JSON reader that emits
/// every string value.
#[must_use]
pub fn reader_for(tool: &str, edit_style: EditStyle) -> Box<dyn ArgReader> {
    if tool != "patch" {
        return Box::new(JsonReader::new(Select::EveryString));
    }
    match edit_style {
        EditStyle::Replace => Box::new(JsonReader::new(Select::Members(&["new", "create"]))),
        EditStyle::Anchor => Box::new(PatchReader::new(LineStyle::Anchor)),
        EditStyle::Hashline => Box::new(PatchReader::new(LineStyle::Hashline)),
        EditStyle::ApplyPatch => Box::new(PatchReader::new(LineStyle::ApplyPatch)),
    }
}

// ---------------------------------------------------------------------------
// JSON scanner

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
struct StrCtx {
    key: u8,
    depth: usize,
    in_object: bool,
}

/// Receives the structure and decoded strings found by [`JsonScan`].
trait JsonSink {
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
struct JsonScan {
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
    fn new() -> Self {
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

    fn stopped(&self) -> bool {
        matches!(self.status, Status::Stopped)
    }

    fn feed(&mut self, bytes: &[u8], sink: &mut impl JsonSink, out: &mut Vec<ReaderEvent>) {
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

    fn close(&mut self, sink: &mut impl JsonSink, out: &mut Vec<ReaderEvent>) {
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
enum Select {
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
struct JsonReader {
    scan: JsonScan,
    events: JsonEvents,
}

impl JsonReader {
    fn new(select: Select) -> Self {
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

// ---------------------------------------------------------------------------
// Line-style patch readers

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LineStyle {
    Anchor,
    Hashline,
    ApplyPatch,
}

#[derive(Clone, Copy, Debug)]
enum LineState {
    Start,
    Lead,
    Row { cr: bool },
    Hold,
    Skip,
}

#[derive(Debug, Default)]
struct Hold {
    bytes: Vec<u8>,
    over: bool,
    trailing_ws: bool,
}

#[derive(Debug, Default)]
struct Item {
    open: bool,
    rows: bool,
}

#[derive(Debug, Default)]
struct Mode {
    body: bool,
    update: bool,
}

/// The optional outer fence of a freeform payload.
#[derive(Debug)]
struct Fence {
    first_line: bool,
    ticks: usize,
    pending_row: bool,
}

impl Default for Fence {
    fn default() -> Self {
        Self {
            first_line: true,
            ticks: 0,
            pending_row: false,
        }
    }
}

impl Fence {
    fn fenced(&self) -> bool {
        self.ticks == 3
    }
}

/// Reads one patch payload line by line and emits its added rows.
#[derive(Debug)]
struct LineMachine {
    style: LineStyle,
    state: LineState,
    col: usize,
    hold: Hold,
    item: Item,
    mode: Mode,
    fence: Fence,
    seg: Vec<u8>,
}

/// The meaning of one complete anchor-style line.
enum AnchorHeader<'a> {
    /// `*** File:` with its path, or `None` for a bare continuation.
    File(Option<&'a [u8]>),
    /// `*** New File:` with its trimmed path; its body is added text.
    NewFile(&'a [u8]),
    /// `*** Delete File:` or `*** Move:`, which end the current item.
    Leave,
    /// `*** Find`, whose body is existing text.
    Find,
    /// An action or `*** Replace File`, whose body is added text.
    Action,
    /// Body text.
    Body,
}

impl LineMachine {
    fn new(style: LineStyle) -> Self {
        Self {
            style,
            state: LineState::Start,
            col: 0,
            hold: Hold::default(),
            item: Item::default(),
            mode: Mode::default(),
            fence: Fence::default(),
            seg: Vec::new(),
        }
    }

    fn feed(&mut self, bytes: &[u8], out: &mut Vec<ReaderEvent>) {
        for &b in bytes {
            self.byte(b, out);
        }
        self.flush(out);
    }

    /// Ends the payload: the last line ends, a closing outer fence is
    /// dropped, and the open item ends.
    fn end_payload(&mut self, out: &mut Vec<ReaderEvent>) {
        match self.state {
            LineState::Row { cr: true } => self.seg.push(b'\r'),
            LineState::Hold => self.hold_end(out),
            _ => {}
        }
        self.end_item(out);
        *self = Self::new(self.style);
    }

    /// Ends the open item without reading the held line.
    fn end_cut(&mut self, out: &mut Vec<ReaderEvent>) {
        self.seg.clear();
        if self.item.open {
            out.push(ReaderEvent::ItemEnd);
        }
        *self = Self::new(self.style);
    }

    fn flush(&mut self, out: &mut Vec<ReaderEvent>) {
        if !self.seg.is_empty() {
            out.push(ReaderEvent::Added(
                String::from_utf8_lossy(&self.seg).into_owned(),
            ));
            self.seg.clear();
        }
    }

    fn start_item(&mut self, path: Option<&[u8]>, out: &mut Vec<ReaderEvent>) {
        self.end_item(out);
        out.push(ReaderEvent::ItemStart);
        self.item.open = true;
        if let Some(path) = path
            && !path.is_empty()
            && path.len() <= MAX_PATH_BYTES
        {
            out.push(ReaderEvent::Path(
                String::from_utf8_lossy(path).into_owned(),
            ));
        }
    }

    fn end_item(&mut self, out: &mut Vec<ReaderEvent>) {
        self.flush(out);
        if self.item.open {
            out.push(ReaderEvent::ItemEnd);
        }
        self.item = Item::default();
    }

    fn row_begin(&mut self) {
        if self.item.rows {
            self.seg.push(b'\n');
        }
        self.item.rows = true;
    }

    fn byte(&mut self, b: u8, out: &mut Vec<ReaderEvent>) {
        if self.fence.pending_row {
            self.fence.pending_row = false;
            self.row_begin();
            self.seg.extend_from_slice(b"```");
        }
        if self.fence.first_line && self.col == self.fence.ticks && self.fence.ticks < 3 {
            if b == b'`' {
                self.fence.ticks += 1;
            } else {
                self.fence.first_line = false;
            }
        }
        match self.state {
            LineState::Start => self.line_start(b),
            LineState::Lead => match b {
                b' ' | b'\t' | b'\r' => {}
                b'*' => self.hold_begin(b),
                b'\n' => self.state = LineState::Start,
                _ => self.state = LineState::Skip,
            },
            LineState::Row { cr } => self.row_byte(cr, b),
            LineState::Hold => {
                if b == b'\n' {
                    self.hold_end(out);
                } else {
                    self.hold_push(b);
                }
            }
            LineState::Skip => {
                if b == b'\n' {
                    self.state = LineState::Start;
                }
            }
        }
        if b == b'\n' {
            self.col = 0;
            self.fence.first_line = false;
        } else {
            self.col = self.col.saturating_add(1);
        }
    }

    fn line_start(&mut self, b: u8) {
        match self.style {
            LineStyle::Hashline => match b {
                b'+' if self.mode.body => {
                    self.row_begin();
                    self.state = LineState::Row { cr: false };
                }
                b'[' | b'P' => {
                    self.mode.body = false;
                    self.hold_begin(b);
                }
                b'\n' => self.mode.body = false,
                _ => {
                    self.mode.body = false;
                    self.state = LineState::Skip;
                }
            },
            LineStyle::ApplyPatch => match b {
                b'+' if self.mode.body => {
                    self.row_begin();
                    self.state = LineState::Row { cr: false };
                }
                b'*' => self.hold_begin(b),
                b' ' | b'\t' | b'\r' if !self.mode.update => self.state = LineState::Lead,
                b'\n' => {}
                _ => self.state = LineState::Skip,
            },
            LineStyle::Anchor => match b {
                b'*' => self.hold_begin(b),
                b'`' if self.mode.body && self.fence.fenced() => {
                    self.hold_begin(b);
                }
                b'\n' if self.mode.body => self.row_begin(),
                b'\n' => {}
                _ if self.mode.body => {
                    self.row_begin();
                    self.row_byte(false, b);
                }
                _ => self.state = LineState::Skip,
            },
        }
    }

    fn row_byte(&mut self, cr: bool, b: u8) {
        if cr && b != b'\n' {
            self.seg.push(b'\r');
        }
        self.state = match b {
            b'\n' => LineState::Start,
            b'\r' => LineState::Row { cr: true },
            _ => {
                self.seg.push(b);
                LineState::Row { cr: false }
            }
        };
    }

    fn hold_begin(&mut self, b: u8) {
        self.hold = Hold::default();
        self.hold.bytes.push(b);
        self.state = LineState::Hold;
    }

    fn hold_push(&mut self, b: u8) {
        if self.hold.over {
            return;
        }
        if self.hold.bytes.len() < MAX_HOLD_BYTES && !self.hold.trailing_ws {
            self.hold.bytes.push(b);
            return;
        }
        if self.style == LineStyle::Anchor && self.mode.body {
            // An added body line too long to be a header candidate: re-emit it.
            self.row_begin();
            let held = std::mem::take(&mut self.hold.bytes);
            self.seg.extend_from_slice(&held);
            self.row_byte(false, b);
            return;
        }
        if matches!(b, b' ' | b'\t' | b'\r') {
            self.hold.trailing_ws = true;
        } else {
            self.hold.over = true;
        }
    }

    fn hold_end(&mut self, out: &mut Vec<ReaderEvent>) {
        self.state = LineState::Start;
        let mut hold = std::mem::take(&mut self.hold);
        if !hold.over && !hold.trailing_ws && hold.bytes.last() == Some(&b'\r') {
            hold.bytes.pop();
        }
        match self.style {
            LineStyle::Hashline => self.hashline_line(&hold, out),
            LineStyle::ApplyPatch => self.apply_patch_line(&hold, out),
            LineStyle::Anchor => self.anchor_line(&mut hold, out),
        }
    }

    fn hashline_line(&mut self, hold: &Hold, out: &mut Vec<ReaderEvent>) {
        let line = hold.bytes.as_slice();
        if line.first() == Some(&b'[') {
            if hold.over || hold.trailing_ws {
                self.start_item(None, out);
            } else if let Some(path) = hashline_header(line) {
                self.start_item(Some(path), out);
            }
        } else if !hold.over
            && !hold.trailing_ws
            && let Some(rest) = line.strip_prefix(b"PUT ")
            && let Some(locator) = rest.strip_suffix(b":")
        {
            self.mode.body = put_locator(locator);
        }
    }

    fn apply_patch_line(&mut self, hold: &Hold, out: &mut Vec<ReaderEvent>) {
        let line = trim(&hold.bytes);
        if hold.over {
            if line.starts_with(b"*** Add File: ") || line.starts_with(b"*** Update File: ") {
                self.mode.update = line.starts_with(b"*** Update File: ");
                self.mode.body = true;
                self.start_item(None, out);
            } else if line.starts_with(b"*** Delete File: ") {
                self.leave_item(out);
            }
            return;
        }
        if line == b"*** End Patch" {
            self.leave_item(out);
        } else if let Some(path) = line.strip_prefix(b"*** Add File: ") {
            self.mode = Mode {
                body: true,
                update: false,
            };
            self.start_item(Some(path), out);
        } else if let Some(path) = line.strip_prefix(b"*** Update File: ") {
            self.mode = Mode {
                body: true,
                update: true,
            };
            self.start_item(Some(path), out);
        } else if line.starts_with(b"*** Delete File: ") {
            self.leave_item(out);
        }
    }

    fn leave_item(&mut self, out: &mut Vec<ReaderEvent>) {
        self.mode = Mode::default();
        self.end_item(out);
    }

    fn anchor_line(&mut self, hold: &mut Hold, out: &mut Vec<ReaderEvent>) {
        if hold.over {
            // Only a non-body line overflows; decide by its header keyword.
            match anchor_keyword(&hold.bytes) {
                Some(b"File:") => {
                    self.mode.body = false;
                    self.start_item(None, out);
                }
                Some(b"New File:") => {
                    self.mode.body = true;
                    self.start_item(None, out);
                }
                Some(b"Delete File:" | b"Move:") => self.leave_item(out),
                _ => {}
            }
            return;
        }
        if hold.trailing_ws {
            hold.bytes.push(b' ');
        }
        match anchor_header(&hold.bytes) {
            AnchorHeader::File(path) => {
                self.mode.body = false;
                if path.is_some() {
                    self.start_item(path, out);
                }
            }
            AnchorHeader::NewFile(path) => {
                self.mode.body = true;
                self.start_item(Some(path), out);
            }
            AnchorHeader::Leave => self.leave_item(out),
            AnchorHeader::Find => self.mode.body = false,
            AnchorHeader::Action => self.mode.body = true,
            AnchorHeader::Body if self.mode.body => {
                if self.fence.fenced() && hold.bytes == b"```" {
                    self.fence.pending_row = true;
                } else {
                    self.row_begin();
                    self.seg.extend_from_slice(&hold.bytes);
                }
            }
            AnchorHeader::Body => {}
        }
    }
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t')
}

fn trim(bytes: &[u8]) -> &[u8] {
    bytes.trim_ascii()
}

fn trim_end_ws(bytes: &[u8]) -> &[u8] {
    let end = bytes.iter().rposition(|&b| !is_ws(b)).map_or(0, |i| i + 1);
    &bytes[..end]
}

fn trim_start_ws(bytes: &[u8]) -> &[u8] {
    let start = bytes.iter().position(|&b| !is_ws(b)).unwrap_or(bytes.len());
    &bytes[start..]
}

fn lid(bytes: &[u8]) -> bool {
    matches!(bytes.first(), Some(b'1'..=b'9')) && bytes.iter().all(u8::is_ascii_digit)
}

fn lid_star(bytes: &[u8]) -> bool {
    bytes.strip_suffix(b"*").is_some_and(lid)
}

fn range(bytes: &[u8]) -> bool {
    let split = |sep: &[u8]| {
        bytes
            .windows(sep.len())
            .position(|w| w == sep)
            .is_some_and(|i| lid(&bytes[..i]) && lid(&bytes[i + sep.len()..]))
    };
    split(b".=") || split(b"-")
}

/// Checks a hashline `put_locator`, accepting the symbol forms too.
fn put_locator(locator: &[u8]) -> bool {
    match locator {
        b">$" => true,
        [b'<', rest @ ..] => lid(rest),
        [b'>', rest @ ..] => lid(rest) || lid_star(rest),
        _ => range(locator) || lid(locator) || lid_star(locator),
    }
}

/// Returns the file name of a `[filename#TAG]` hashline header.
fn hashline_header(line: &[u8]) -> Option<&[u8]> {
    let inner = line.strip_prefix(b"[")?.strip_suffix(b"]")?;
    let hash = inner.iter().position(|&b| b == b'#')?;
    let (name, tag) = (&inner[..hash], &inner[hash + 1..]);
    let tag_ok = tag == b"NEW"
        || (tag.len() == 4
            && tag
                .iter()
                .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(b)));
    (!name.is_empty() && !name.contains(&b'\r') && tag_ok).then_some(name)
}

/// Returns the keyword after `***` and blanks when a line begins with a
/// path-bearing anchor header keyword.
fn anchor_keyword(line: &[u8]) -> Option<&'static [u8]> {
    let rest = line.strip_prefix(b"***")?;
    if !rest.first().copied().is_some_and(is_ws) {
        return None;
    }
    let rest = trim_start_ws(rest);
    [b"File:".as_slice(), b"New File:", b"Delete File:", b"Move:"]
        .into_iter()
        .find(|keyword| rest.starts_with(keyword))
}

/// Classifies one complete anchor line against the anchor grammar's header
/// tokens; anything else is body text.
fn anchor_header(line: &[u8]) -> AnchorHeader<'_> {
    let Some(rest) = line.strip_prefix(b"***") else {
        return AnchorHeader::Body;
    };
    if !rest.first().copied().is_some_and(is_ws) || rest.contains(&b'\r') {
        return AnchorHeader::Body;
    }
    let rest = trim_start_ws(rest);
    if let Some(tail) = rest.strip_prefix(b"File:") {
        return anchor_file(tail);
    }
    if let Some(tail) = rest.strip_prefix(b"New File:") {
        return path_after_blank(tail).map_or(AnchorHeader::Body, AnchorHeader::NewFile);
    }
    if let Some(tail) = rest.strip_prefix(b"Delete File:") {
        return path_after_blank(tail).map_or(AnchorHeader::Body, |_| AnchorHeader::Leave);
    }
    if let Some(tail) = rest.strip_prefix(b"Move:") {
        return if move_tail(tail) {
            AnchorHeader::Leave
        } else {
            AnchorHeader::Body
        };
    }
    if let Some(tail) = rest.strip_prefix(b"Find") {
        return if find_tail(tail) {
            AnchorHeader::Find
        } else {
            AnchorHeader::Body
        };
    }
    for action in [
        b"Replace File".as_slice(),
        b"Replace",
        b"Insert Before",
        b"Insert After",
    ] {
        if let Some(tail) = rest.strip_prefix(action)
            && tail.iter().copied().all(is_ws)
        {
            return AnchorHeader::Action;
        }
    }
    AnchorHeader::Body
}

/// Parses the tail of `*** File:`: an optional path and optional
/// `#XXXXXXXX` tag. A header without a path is a bare continuation.
fn anchor_file(tail: &[u8]) -> AnchorHeader<'_> {
    if tail.iter().copied().all(is_ws) {
        return AnchorHeader::File(None);
    }
    if !tail.first().copied().is_some_and(is_ws) {
        return AnchorHeader::Body;
    }
    let body = trim_start_ws(tail);
    let Some(hash) = body.iter().position(|&b| b == b'#') else {
        return AnchorHeader::File(Some(trim_end_ws(body)));
    };
    let (before, after) = (&body[..hash], &body[hash + 1..]);
    let tag_ok = after.len() >= 8
        && after[..8]
            .iter()
            .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(b))
        && after[8..].iter().copied().all(is_ws);
    if !tag_ok {
        return AnchorHeader::Body;
    }
    if before.is_empty() {
        return AnchorHeader::File(None);
    }
    if !before.last().copied().is_some_and(is_ws) {
        return AnchorHeader::Body;
    }
    AnchorHeader::File(Some(trim_end_ws(before)))
}

/// Returns the trimmed path of `[ \t]+[^\r\n]+`.
fn path_after_blank(tail: &[u8]) -> Option<&[u8]> {
    (tail.len() >= 2 && is_ws(tail[0])).then(|| trim_end_ws(trim_start_ws(tail)))
}

/// Checks `[ \t]+[^\r\n]+[ \t]+->[ \t]+[^\r\n]+`.
fn move_tail(tail: &[u8]) -> bool {
    let Some((&first, rest)) = tail.split_first() else {
        return false;
    };
    if !is_ws(first) {
        return false;
    }
    // Positions: 0 before any source byte, 1 source seen, 2 blank run after
    // source, 3 blank and `-`, 4 blank and `->`, 5 `->` and blank, 6 target.
    let mut state = 0u8;
    for &b in rest {
        state = match (state, b) {
            (2, b'-') => 3,
            (1..=3, _) if is_ws(b) => 2,
            (3, b'>') => 4,
            (4, _) if is_ws(b) => 5,
            (5 | 6, _) => 6,
            _ => 1,
        };
    }
    state == 6
}

/// Checks the tail of `*** Find`: blanks, then `@N`, `N-M`, or `all`.
fn find_tail(tail: &[u8]) -> bool {
    let tail = trim_end_ws(tail);
    if tail.is_empty() {
        return true;
    }
    if !tail.first().copied().is_some_and(is_ws) {
        return false;
    }
    let arg = trim_start_ws(tail);
    arg == b"all"
        || arg.strip_prefix(b"@").is_some_and(lid)
        || arg
            .iter()
            .position(|&b| b == b'-')
            .is_some_and(|i| lid(&arg[..i]) && lid(&arg[i + 1..]))
}

// ---------------------------------------------------------------------------
// Patch reader envelope

#[derive(Debug)]
enum Envelope {
    Undecided,
    Freeform,
    Json(JsonScan),
}

/// Forwards the top-level `input` string of a JSON fallback call.
struct InputSink<'a> {
    lines: &'a mut LineMachine,
    active: &'a mut bool,
}

impl JsonSink for InputSink<'_> {
    fn classify_key(&self, key: &str) -> u8 {
        if key == "input" { KEY_SELECTED } else { 0 }
    }

    fn object_start(&mut self, _out: &mut Vec<ReaderEvent>) {}

    fn object_end(&mut self, _out: &mut Vec<ReaderEvent>) {}

    fn string_start(&mut self, ctx: StrCtx) -> bool {
        *self.active = ctx.depth == 1 && ctx.in_object && ctx.key & KEY_SELECTED != 0;
        *self.active
    }

    fn string_text(&mut self, text: &str, out: &mut Vec<ReaderEvent>) {
        if *self.active {
            self.lines.feed(text.as_bytes(), out);
        }
    }

    fn string_end(&mut self, out: &mut Vec<ReaderEvent>) {
        if std::mem::take(self.active) {
            self.lines.end_payload(out);
        }
    }
}

/// The reader of a line-style patch call, freeform or `{"input": ...}`.
#[derive(Debug)]
struct PatchReader {
    envelope: Envelope,
    lines: LineMachine,
    active: bool,
}

impl PatchReader {
    fn new(style: LineStyle) -> Self {
        Self {
            envelope: Envelope::Undecided,
            lines: LineMachine::new(style),
            active: false,
        }
    }
}

impl ArgReader for PatchReader {
    fn feed(&mut self, delta: &str, out: &mut Vec<ReaderEvent>) {
        let mut bytes = delta.as_bytes();
        if matches!(self.envelope, Envelope::Undecided) {
            let Some(start) = bytes.iter().position(|b| !b.is_ascii_whitespace()) else {
                return;
            };
            bytes = &bytes[start..];
            self.envelope = if bytes[0] == b'{' {
                Envelope::Json(JsonScan::new())
            } else {
                Envelope::Freeform
            };
        }
        match &mut self.envelope {
            Envelope::Undecided => {}
            Envelope::Freeform => self.lines.feed(bytes, out),
            Envelope::Json(scan) => {
                let mut sink = InputSink {
                    lines: &mut self.lines,
                    active: &mut self.active,
                };
                scan.feed(bytes, &mut sink, out);
            }
        }
    }

    fn close(&mut self, out: &mut Vec<ReaderEvent>) {
        match &mut self.envelope {
            Envelope::Undecided => {}
            Envelope::Freeform => self.lines.end_payload(out),
            Envelope::Json(scan) => {
                let stopped = scan.stopped();
                let mut sink = InputSink {
                    lines: &mut self.lines,
                    active: &mut self.active,
                };
                scan.close(&mut sink, out);
                if stopped {
                    self.lines.end_cut(out);
                } else {
                    self.lines.end_payload(out);
                }
            }
        }
        self.envelope = Envelope::Undecided;
        self.active = false;
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        ArgReader, EditStyle, JsonReader, MAX_FRAMES, MAX_KEY_BYTES, MAX_PATH_BYTES, ReaderEvent,
        Select, reader_for,
    };
    use crate::ttsr::scope::{ScopeInput, ScopeValue, admits_tool, parse_scope};
    use crate::ttsr::value::Origin;

    use ReaderEvent::{Added, ItemEnd, ItemStart, Path as PathEv};

    fn added(text: &str) -> ReaderEvent {
        Added(text.to_owned())
    }

    fn path(text: &str) -> ReaderEvent {
        PathEv(text.to_owned())
    }

    /// Coalesces adjacent `Added` events so chunkings compare equal.
    fn merged(events: Vec<ReaderEvent>) -> Vec<ReaderEvent> {
        let mut out: Vec<ReaderEvent> = Vec::new();
        for event in events {
            if let (Some(Added(last)), Added(text)) = (out.last_mut(), &event) {
                last.push_str(text);
                continue;
            }
            if matches!(&event, Added(text) if text.is_empty()) {
                continue;
            }
            out.push(event);
        }
        out
    }

    fn run(mut reader: Box<dyn ArgReader>, chunks: &[&str]) -> Vec<ReaderEvent> {
        let mut out = Vec::new();
        for chunk in chunks {
            reader.feed(chunk, &mut out);
        }
        reader.close(&mut out);
        merged(out)
    }

    /// Feeds `input` whole, split at every char boundary, and one char at a
    /// time; every run must produce `expected`.
    fn assert_chunked(
        make: &dyn Fn() -> Box<dyn ArgReader>,
        input: &str,
        expected: &[ReaderEvent],
    ) {
        assert_eq!(run(make(), &[input]), expected, "whole input");
        for (at, _) in input.char_indices().skip(1) {
            let got = run(make(), &[&input[..at], &input[at..]]);
            assert_eq!(got, expected, "split at byte {at}");
        }
        let chars: Vec<String> = input.chars().map(String::from).collect();
        let chunks: Vec<&str> = chars.iter().map(String::as_str).collect();
        assert_eq!(run(make(), &chunks), expected, "one char per delta");
    }

    fn tool(name: &'static str, style: EditStyle) -> impl Fn() -> Box<dyn ArgReader> {
        move || reader_for(name, style)
    }

    /// A key of exactly [`MAX_KEY_BYTES`] bytes.
    const EDGE_KEY: &str = concat!(
        "kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk",
        "kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk",
        "kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk",
        "kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk",
    );
    /// A key one byte over [`MAX_KEY_BYTES`].
    const LONG_KEY: &str = concat!(
        "kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk",
        "kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk",
        "kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk",
        "kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk",
        "k",
    );

    #[test]
    fn reader_escape_splits() {
        let input = r#"{"command":"sl\u0065ep 5"}"#;
        let expected = [ItemStart, added("sleep 5"), ItemEnd];
        assert_chunked(&tool("exec", EditStyle::Anchor), input, &expected);

        let pair = r#"{"command":"a\ud83d\ude00b\\\"\n\t\/"}"#;
        let expected = [ItemStart, added("a\u{1F600}b\\\"\n\t/"), ItemEnd];
        assert_chunked(&tool("exec", EditStyle::Anchor), pair, &expected);
    }

    /// Drives the D-19 path gating over reader events: a match waits for its
    /// item's path, fires once the path is admitted, and is dropped at item
    /// end. Returns the event index at which the fire lands.
    fn gated_fire(events: &[ReaderEvent], scope_text: &str) -> Option<usize> {
        let origin = Origin::User(PathBuf::from("/rules/no-bad.md"));
        let (scope, _) = parse_scope(ScopeInput {
            origin: &origin,
            value: ScopeValue::String(scope_text),
            known_tools: None,
            extra_tokens: &[],
        });
        let root = Path::new("/ws");
        let mut items: Vec<(Option<String>, String, bool)> = Vec::new();
        for (index, event) in events.iter().enumerate() {
            if items.is_empty() && matches!(event, Added(_) | PathEv(_)) {
                items.push((None, String::new(), false));
            }
            match event {
                ItemStart => items.push((None, String::new(), false)),
                ItemEnd => {
                    items.pop();
                }
                PathEv(p) => {
                    let item = items.last_mut().unwrap();
                    item.0 = Some(p.clone());
                    if item.2 && admits_tool(&scope, "patch", Some(p), root) {
                        return Some(index);
                    }
                    item.2 = false;
                }
                Added(text) => {
                    let item = items.last_mut().unwrap();
                    item.1.push_str(text);
                    if item.1.contains("bad(") {
                        match &item.0 {
                            None => item.2 = true,
                            Some(p) if admits_tool(&scope, "patch", Some(p), root) => {
                                return Some(index);
                            }
                            Some(_) => {}
                        }
                    }
                }
            }
        }
        None
    }

    #[test]
    fn patch_gating() {
        let make = tool("patch", EditStyle::Replace);
        let ml = r#"{"changes":[{"new":"bad()","path":"a.ml"}]}"#;
        let expected = [
            ItemStart,
            ItemStart,
            added("bad()"),
            path("a.ml"),
            ItemEnd,
            ItemEnd,
        ];
        assert_chunked(&make, ml, &expected);
        assert_eq!(gated_fire(&expected, "tool:patch(*.ml)"), Some(3));

        let txt = r#"{"changes":[{"new":"bad()","path":"a.txt"}]}"#;
        let events = run(make(), &[txt]);
        assert!(events.contains(&path("a.txt")));
        assert_eq!(gated_fire(&events, "tool:patch(*.ml)"), None);

        let no_path = r#"{"changes":[{"new":"bad()"},{"path":"a.ml","new":"ok"}]}"#;
        let events = run(make(), &[no_path]);
        assert_eq!(gated_fire(&events, "tool:patch(*.ml)"), None);
    }

    #[test]
    fn replace_style_emits_only_new_and_create() {
        let make = tool("patch", EditStyle::Replace);
        let input = r#"{"changes":[{"path":"a","old":"gone","new":"x","create":"y"},{"path":"b","new":""}]}"#;
        let expected = [
            ItemStart,
            ItemStart,
            path("a"),
            added("x\ny"),
            ItemEnd,
            ItemStart,
            path("b"),
            ItemEnd,
            ItemEnd,
        ];
        assert_chunked(&make, input, &expected);
    }

    #[test]
    fn every_string_covers_arrays_numbers_and_literals() {
        let make = tool("search", EditStyle::Replace);
        let input = r#" {"a":[1,-2.5e+3,true,null,"p"],"b":{"c":"q"},"d":false,"e":0} "#;
        let expected = [
            ItemStart,
            added("p"),
            ItemStart,
            added("q"),
            ItemEnd,
            ItemEnd,
        ];
        assert_chunked(&make, input, &expected);
    }

    #[test]
    fn reader_limits() {
        assert_eq!(EDGE_KEY.len(), MAX_KEY_BYTES);
        assert_eq!(LONG_KEY.len(), MAX_KEY_BYTES + 1);
        let input = format!(r#"{{"{LONG_KEY}":"hit"}}"#);
        let reader = Box::new(JsonReader::new(Select::Members(&[LONG_KEY])));
        assert_eq!(run(reader, &[&input]), [ItemStart, ItemEnd]);
        let input = format!(r#"{{"{EDGE_KEY}":"hit"}}"#);
        let reader = Box::new(JsonReader::new(Select::Members(&[EDGE_KEY])));
        assert_eq!(run(reader, &[&input]), [ItemStart, added("hit"), ItemEnd]);

        let exec = tool("exec", EditStyle::Replace);
        let long = "a".repeat(MAX_PATH_BYTES + 1);
        let events = run(exec(), &[&format!(r#"{{"path":"{long}"}}"#)]);
        assert_eq!(events, [ItemStart, added(&long), ItemEnd]);
        let edge = "a".repeat(MAX_PATH_BYTES);
        let events = run(exec(), &[&format!(r#"{{"path":"{edge}"}}"#)]);
        assert_eq!(events, [ItemStart, added(&edge), path(&edge), ItemEnd]);

        let deep_ok = format!("{}\"x\"{}", "[".repeat(MAX_FRAMES), "]".repeat(MAX_FRAMES));
        assert_eq!(run(exec(), &[&deep_ok]), [added("x")]);
        let deep = format!("{}\"x\"", r#"{"a":"#.repeat(MAX_FRAMES + 1));
        let mut reader = exec();
        let mut out = Vec::new();
        reader.feed(&deep, &mut out);
        assert_eq!(
            out,
            vec![ItemStart; MAX_FRAMES],
            "deep nesting emits nothing more"
        );
        out.clear();
        reader.close(&mut out);
        assert_eq!(out, vec![ItemEnd; MAX_FRAMES]);

        let malformed = r#"{"a":tru "b":"secret"}"#;
        let mut reader = exec();
        let mut out = Vec::new();
        reader.feed(malformed, &mut out);
        reader.feed(r#""more"}"#, &mut out);
        assert_eq!(out, [ItemStart]);
        reader.close(&mut out);
        assert_eq!(out, [ItemStart, ItemEnd]);
        assert_eq!(
            run(exec(), &[r#"{"a":"x"} "y""#]),
            [ItemStart, added("x"), ItemEnd]
        );
        assert_eq!(
            run(exec(), &["{\"a\":\"x\ny\"}"]),
            [ItemStart, added("x"), ItemEnd]
        );

        let surrogate = r#"{"c":"a\ud800b\udc00c\ud800\ud800\udc00d\ud800"}"#;
        let expected = [
            ItemStart,
            added("a\u{FFFD}b\u{FFFD}c\u{FFFD}\u{10000}d\u{FFFD}"),
            ItemEnd,
        ];
        assert_chunked(&exec, surrogate, &expected);
        let cut_high = r#"{"c":"z\ud800"#;
        assert_chunked(&exec, cut_high, &[ItemStart, added("z\u{FFFD}"), ItemEnd]);

        let cut = r#"{"changes":[{"path":"a.ml","new":"bad"#;
        let expected = [
            ItemStart,
            ItemStart,
            path("a.ml"),
            added("bad"),
            ItemEnd,
            ItemEnd,
        ];
        assert_chunked(&tool("patch", EditStyle::Replace), cut, &expected);
    }

    #[test]
    fn hashline_rows_freeform_and_json() {
        let payload = "*** Begin Patch\n[src/a.ml#A1B2]\nPUT 1.=2:\n+one\n+\n+two\nCUT 3\n+not added\nPUT x:\n+bad locator\n[b.rs#NEW]\nPUT >$:\n+three\r\n*** End Patch\n";
        let expected = [
            ItemStart,
            path("src/a.ml"),
            added("one\n\ntwo"),
            ItemEnd,
            ItemStart,
            path("b.rs"),
            added("three"),
            ItemEnd,
        ];
        let make = tool("patch", EditStyle::Hashline);
        assert_chunked(&make, payload, &expected);
        let json = format!(r#"{{"input":{}}}"#, json_string(payload));
        assert_chunked(&make, &json, &expected);

        let bad_header = "[a#abcd]\nPUT 1:\n+x\n[c#ABCD] \nPUT 2*:\n+y\n";
        assert_chunked(&make, bad_header, &[added("x\ny")]);

        let cut = "[a.ml#NEW]\nPUT >$:\n+partial";
        assert_chunked(
            &make,
            cut,
            &[ItemStart, path("a.ml"), added("partial"), ItemEnd],
        );
    }

    #[test]
    fn apply_patch_rows() {
        let payload = "*** Begin Patch\n*** Add File: path/add.py\n+abc\n+def\n*** Delete File: gone.py\n+not added\n*** Update File: u.py \n*** Move to: v.py\n@@ def f():\n-    pass\n+    return 123\n+++\n context\r\n*** End of File\n*** End Patch\n+after end\n";
        let expected = [
            ItemStart,
            path("path/add.py"),
            added("abc\ndef"),
            ItemEnd,
            ItemStart,
            path("u.py"),
            added("    return 123\n++"),
            ItemEnd,
        ];
        let make = tool("patch", EditStyle::ApplyPatch);
        assert_chunked(&make, payload, &expected);
        let json = format!(r#"{{"input":{}}}"#, json_string(payload));
        assert_chunked(&make, &json, &expected);

        let long = format!("*** Add File: {}\n+x\n", "p".repeat(MAX_PATH_BYTES + 1));
        assert_eq!(run(make(), &[&long]), [ItemStart, added("x"), ItemEnd]);
    }

    #[test]
    fn anchor_rows_skip_find_text() {
        let payload = "```\n*** File: src/a.rs #0123ABCD\n*** Find @3\nold text\n*** Replace\nnew one\n\n*** Findings stay\n*** File: b.rs\nx\n*** Find all\nold\n*** Insert After\ntail\n*** Delete File: c.rs\n*** New File:  d.md \n# D\n*** Move: e -> f\nno\n```\n";
        let expected = [
            ItemStart,
            path("src/a.rs"),
            added("new one\n\n*** Findings stay"),
            ItemEnd,
            ItemStart,
            path("b.rs"),
            added("tail"),
            ItemEnd,
            ItemStart,
            path("d.md"),
            added("# D"),
            ItemEnd,
        ];
        let make = tool("patch", EditStyle::Anchor);
        assert_chunked(&make, payload, &expected);

        let second = "*** File: a.rs\n*** Find\nx\n*** Replace File\n```\nbody\n*** File:\n*** Find\ny\n*** Insert Before\nz\n";
        let expected = [ItemStart, path("a.rs"), added("```\nbody\nz"), ItemEnd];
        assert_chunked(&make, second, &expected);
    }

    /// Encodes `text` as a JSON string literal with escapes only.
    fn json_string(text: &str) -> String {
        let mut out = String::from("\"");
        for c in text.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                'e' => out.push_str("\\u0065"),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }
}
