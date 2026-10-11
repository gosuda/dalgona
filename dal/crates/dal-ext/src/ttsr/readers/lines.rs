//! Line-style patch readers: anchor, hashline, and apply-patch dialects.
//!
//! [`PatchReader`] detects the envelope per call; JSON envelopes reuse the
//! [`super::json`] scanner with an [`InputSink`] that feeds the line
//! machine. [`LineStyle`] selects the dialect for the patch tool.

use super::json::{JsonScan, JsonSink, StrCtx};
use super::{ArgReader, KEY_SELECTED, MAX_HOLD_BYTES, MAX_PATH_BYTES, ReaderEvent};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LineStyle {
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
pub(super) struct PatchReader {
    envelope: Envelope,
    lines: LineMachine,
    active: bool,
}

impl PatchReader {
    pub(super) fn new(style: LineStyle) -> Self {
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
