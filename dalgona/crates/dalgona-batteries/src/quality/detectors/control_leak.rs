// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

const RUN_START: usize = 3;
const RUN_NORMAL: usize = 4;
const RUN_QUOTATION: usize = 8;
const RUN_GAP_MAX_WS: usize = 32;
const EVIDENCE_TTL_CHARS: usize = 2048;
const NAME_CAP_CTRL: usize = 32;
const NAME_CAP_SGML: usize = 16;
const NAME_CAP_BRACKET: usize = 16;
const BRACKET_NAME_MIN: usize = 2;
const START_CONTEXT_CHARS: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Family {
    Ctrl,
    Sgml,
    Bracket,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Context {
    Start,
    Normal,
    Quotation,
}

/// A control-token run verdict with the stream offsets it covers.
#[derive(Clone, Debug, PartialEq)]
pub struct Fire {
    /// Stable machine name of the pattern that matched.
    pub reason: &'static str,
    /// Byte offset where the anomaly begins.
    pub anomaly_start_offset: u64,
    /// Byte offset where the garbage span begins.
    pub garbage_start_offset: u64,
    /// Human-readable description of the match.
    pub detail: String,
}

#[derive(Clone, Debug)]
struct Token {
    id: String,
    start: usize,
    end: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Ground,
    AngleOpen,
    CtrlFirst,
    CtrlName,
    CtrlClose,
    SgmlFirst,
    SgmlName,
    BracketFirst,
    BracketName,
}

#[derive(Default)]
struct LineFlags {
    has_content: bool,
    indent: usize,
    blockquote: bool,
    indented: bool,
}

/// Incremental detector of runs of repeated control tokens in one stream.
pub struct State {
    offset: usize,
    mode: Mode,
    buffer: String,
    buffer_start: usize,
    name_len: usize,
    latched: Option<Fire>,
    run_id: Option<String>,
    run_context: Context,
    run_count: usize,
    run_first_start: usize,
    ws_since_token: usize,
    ws_only_since_token: bool,
    evidence_id: Option<String>,
    evidence_first_payload: Option<usize>,
    evidence_gap: usize,
    evidence_expires: usize,
    in_fence: bool,
    fence_char: Option<char>,
    tick_char: Option<char>,
    tick_count: usize,
    line: LineFlags,
    head_ws_only: bool,
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

impl State {
    /// Creates a detector with no evidence.
    #[must_use]
    pub fn new() -> Self {
        Self {
            offset: 0,
            mode: Mode::Ground,
            buffer: String::with_capacity(40),
            buffer_start: 0,
            name_len: 0,
            latched: None,
            run_id: None,
            run_context: Context::Normal,
            run_count: 0,
            run_first_start: 0,
            ws_since_token: 0,
            ws_only_since_token: true,
            evidence_id: None,
            evidence_first_payload: None,
            evidence_gap: 0,
            evidence_expires: 0,
            in_fence: false,
            fence_char: None,
            tick_char: None,
            tick_count: 0,
            line: LineFlags::default(),
            head_ws_only: true,
        }
    }

    /// Whether the detector has already fired.
    #[must_use]
    pub fn latched(&self) -> bool {
        self.latched.is_some()
    }

    /// Consumes a streamed delta; returns the first control-token run, then stays latched.
    #[must_use]
    pub fn feed(&mut self, delta: &str) -> Option<Fire> {
        if self.latched.is_some() {
            self.offset = self.offset.saturating_add(delta.len());
            return None;
        }
        for (relative, value) in delta.char_indices() {
            let offset = self.offset.saturating_add(relative);
            self.feed_char(value, offset);
            if self.latched.is_some() {
                self.offset = self.offset.saturating_add(delta.len());
                let fire = self.latched.clone()?;
                return Some(fire);
            }
        }
        self.offset = self.offset.saturating_add(delta.len());
        None
    }

    /// Whether the token run supports `fire` as its cause: the first payload after the last
    /// token starts where the collapse anomaly starts, within the evidence window.
    #[must_use]
    pub fn corroborates(&self, fire: &super::collapse::Fire) -> bool {
        let Some(payload) = self.evidence_first_payload else {
            return false;
        };
        if self.offset > self.evidence_expires {
            return false;
        }
        if self.evidence_gap > RUN_GAP_MAX_WS {
            return false;
        }
        let Ok(anomaly) = usize::try_from(fire.anomaly_start_offset) else {
            return false;
        };
        payload == anomaly
    }

    fn feed_char(&mut self, ch: char, offset: usize) {
        let outcomes = self.step_parser(ch, offset);
        for outcome in outcomes {
            match outcome {
                Outcome::Ground(value, at) => self.observe_ground(value, at),
                Outcome::Reject(text, at) => self.observe_reject(&text, at),
                Outcome::Token(token) => self.observe_token(&token),
            }
        }
        if ch == '\n' {
            self.line = LineFlags::default();
        }
    }

    fn step_parser(&mut self, ch: char, offset: usize) -> Outcomes {
        match self.mode {
            Mode::Ground => self.step_ground(ch, offset),
            Mode::AngleOpen => self.step_angle(ch, offset),
            Mode::CtrlFirst | Mode::CtrlName | Mode::CtrlClose => self.step_ctrl(ch, offset),
            Mode::SgmlFirst | Mode::SgmlName => self.step_sgml(ch, offset),
            Mode::BracketFirst | Mode::BracketName => self.step_bracket(ch, offset),
        }
    }

    fn accept(&mut self, ch: char, mode: Mode, name_len: Option<usize>) -> Outcomes {
        self.buffer.push(ch);
        self.mode = mode;
        if let Some(name_len) = name_len {
            self.name_len = name_len;
        }
        Outcomes::default()
    }

    fn extend_name(&mut self, ch: char) -> Outcomes {
        self.buffer.push(ch);
        self.name_len += 1;
        Outcomes::default()
    }

    fn step_ground(&mut self, ch: char, offset: usize) -> Outcomes {
        match ch {
            '<' => {
                self.begin(Mode::AngleOpen, ch, offset);
                Outcomes::default()
            }
            '[' => {
                self.begin(Mode::BracketFirst, ch, offset);
                Outcomes::default()
            }
            _ => Outcomes::one(Outcome::Ground(ch, offset)),
        }
    }

    fn step_angle(&mut self, ch: char, offset: usize) -> Outcomes {
        match ch {
            '|' => self.accept(ch, Mode::CtrlFirst, None),
            '/' => self.accept(ch, Mode::SgmlFirst, None),
            _ if ch.is_ascii_alphabetic() => self.accept(ch, Mode::SgmlName, Some(1)),
            _ => self.reject_with(ch, offset),
        }
    }

    fn step_ctrl(&mut self, ch: char, offset: usize) -> Outcomes {
        match self.mode {
            Mode::CtrlFirst if ch.is_ascii_alphabetic() => self.accept(ch, Mode::CtrlName, Some(1)),
            Mode::CtrlName if ch == '|' => self.accept(ch, Mode::CtrlClose, None),
            Mode::CtrlName if is_name_char(ch) && self.name_len < NAME_CAP_CTRL => {
                self.extend_name(ch)
            }
            Mode::CtrlClose if ch == '>' => Outcomes::one(self.emit(Family::Ctrl, 2, ch, offset)),
            _ => self.reject_with(ch, offset),
        }
    }

    fn step_sgml(&mut self, ch: char, offset: usize) -> Outcomes {
        match self.mode {
            Mode::SgmlFirst if ch.is_ascii_alphabetic() => self.accept(ch, Mode::SgmlName, Some(1)),
            Mode::SgmlName if ch == '>' => Outcomes::one(self.emit(Family::Sgml, 1, ch, offset)),
            Mode::SgmlName if is_name_char(ch) && self.name_len < NAME_CAP_SGML => {
                self.extend_name(ch)
            }
            _ => self.reject_with(ch, offset),
        }
    }

    fn step_bracket(&mut self, ch: char, offset: usize) -> Outcomes {
        match self.mode {
            Mode::BracketFirst if ch.is_ascii_uppercase() => {
                self.accept(ch, Mode::BracketName, Some(1))
            }
            Mode::BracketName if ch == ']' && self.name_len >= BRACKET_NAME_MIN => {
                Outcomes::one(self.emit(Family::Bracket, 1, ch, offset))
            }
            Mode::BracketName if is_bracket_char(ch) && self.name_len < NAME_CAP_BRACKET => {
                self.extend_name(ch)
            }
            _ => self.reject_with(ch, offset),
        }
    }

    fn begin(&mut self, mode: Mode, ch: char, offset: usize) {
        self.mode = mode;
        self.buffer.clear();
        self.buffer.push(ch);
        self.name_len = 0;
        self.buffer_start = offset;
    }

    fn emit(&mut self, family: Family, name_start: usize, closer: char, offset: usize) -> Outcome {
        let id = format!("{}:{}", family_label(family), &self.buffer[name_start..]);
        self.buffer.clear();
        let token = Token {
            id,
            start: self.buffer_start,
            end: offset + closer.len_utf8(),
        };
        self.mode = Mode::Ground;
        self.name_len = 0;
        Outcome::Token(token)
    }

    fn reject_with(&mut self, ch: char, offset: usize) -> Outcomes {
        let text = std::mem::take(&mut self.buffer);
        let start = self.buffer_start;
        self.mode = Mode::Ground;
        self.name_len = 0;
        match ch {
            '<' => {
                self.begin(Mode::AngleOpen, ch, offset);
                Outcomes::one(Outcome::Reject(text, start))
            }
            '[' => {
                self.begin(Mode::BracketFirst, ch, offset);
                Outcomes::one(Outcome::Reject(text, start))
            }
            _ => Outcomes::two(Outcome::Reject(text, start), Outcome::Ground(ch, offset)),
        }
    }

    fn observe_ground(&mut self, ch: char, offset: usize) {
        if ch.is_ascii_whitespace() {
            if self.ws_since_token < RUN_GAP_MAX_WS + 1 {
                self.ws_since_token += 1;
            }
        } else {
            self.ws_only_since_token = false;
            self.note_payload(offset);
        }
        self.track_ground_char(ch);
        if offset < START_CONTEXT_CHARS && !ch.is_ascii_whitespace() {
            self.head_ws_only = false;
        }
    }

    fn observe_reject(&mut self, text: &str, start: usize) {
        self.ws_only_since_token = false;
        self.note_payload(start);
        for (relative, ch) in text.char_indices() {
            self.track_ground_char(ch);
            if start + relative < START_CONTEXT_CHARS && !ch.is_ascii_whitespace() {
                self.head_ws_only = false;
            }
        }
    }

    fn observe_token(&mut self, token: &Token) {
        let context = self.classify(token.start);
        self.note_payload(token.start);
        let continues = self.run_id.as_deref() == Some(&token.id)
            && self.ws_only_since_token
            && self.ws_since_token <= RUN_GAP_MAX_WS;
        if continues {
            self.run_count += 1;
        } else {
            self.run_id = Some(token.id.clone());
            self.run_context = context;
            self.run_first_start = token.start;
            self.run_count = 1;
        }
        self.evidence_id = Some(token.id.clone());
        self.evidence_first_payload = None;
        self.evidence_gap = 0;
        self.evidence_expires = token.end.saturating_add(EVIDENCE_TTL_CHARS);
        self.ws_since_token = 0;
        self.ws_only_since_token = true;
        if self.run_count >= threshold_for(self.run_context) {
            let detail = format!(
                "{}x {} run in {} context",
                self.run_count,
                token.id,
                context_label(self.run_context)
            );
            if let Some(fire) = make_fire(self.run_first_start, detail) {
                self.latched = Some(fire);
            }
        }
    }

    fn note_payload(&mut self, offset: usize) {
        if self.evidence_id.is_some() && self.evidence_first_payload.is_none() {
            self.evidence_gap = self.ws_since_token;
            self.evidence_first_payload = Some(offset);
        }
    }

    fn classify(&self, start: usize) -> Context {
        if self.in_fence || self.line.blockquote || self.line.indented {
            return Context::Quotation;
        }
        if start < START_CONTEXT_CHARS && self.head_ws_only {
            return Context::Start;
        }
        Context::Normal
    }

    fn track_ground_char(&mut self, ch: char) {
        if self.tick_char.is_some() {
            if Some(ch) == self.tick_char {
                self.tick_count += 1;
                return;
            }
            self.resolve_ticks();
        }
        if ch == '`' || ch == '~' {
            self.tick_char = Some(ch);
            self.tick_count = 1;
            return;
        }
        if ch == '\n' {
            return;
        }
        if !self.line.has_content {
            if ch == ' ' {
                self.line.indent += 1;
            } else if ch == '\t' || self.line.indent >= 4 {
                self.line.indented = true;
            } else if !ch.is_ascii_whitespace() {
                self.line.has_content = true;
                if ch == '>' {
                    self.line.blockquote = true;
                }
            }
        }
    }

    fn resolve_ticks(&mut self) {
        let count = self.tick_count;
        let mark = self.tick_char.take().unwrap_or('`');
        self.tick_count = 0;
        if count >= 3 {
            if !self.in_fence {
                self.in_fence = true;
                self.fence_char = Some(mark);
            } else if self.fence_char == Some(mark) {
                self.in_fence = false;
                self.fence_char = None;
            }
        }
    }
}

enum Outcome {
    Ground(char, usize),
    Reject(String, usize),
    Token(Token),
}

#[derive(Default)]
struct Outcomes([Option<Outcome>; 2]);

impl Outcomes {
    fn one(outcome: Outcome) -> Self {
        Self([Some(outcome), None])
    }

    fn two(first: Outcome, second: Outcome) -> Self {
        Self([Some(first), Some(second)])
    }
}

impl IntoIterator for Outcomes {
    type Item = Outcome;
    type IntoIter = std::iter::Flatten<std::array::IntoIter<Option<Outcome>, 2>>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter().flatten()
    }
}

fn family_label(family: Family) -> &'static str {
    match family {
        Family::Ctrl => "ctrl",
        Family::Sgml => "sgml",
        Family::Bracket => "bracket",
    }
}

fn context_label(context: Context) -> &'static str {
    match context {
        Context::Start => "start",
        Context::Normal => "normal",
        Context::Quotation => "quotation",
    }
}

fn threshold_for(context: Context) -> usize {
    match context {
        Context::Start => RUN_START,
        Context::Normal => RUN_NORMAL,
        Context::Quotation => RUN_QUOTATION,
    }
}

fn is_name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn is_bracket_char(ch: char) -> bool {
    ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_'
}

fn make_fire(start: usize, detail: String) -> Option<Fire> {
    let anomaly_start_offset = u64::try_from(start).ok()?;
    Some(Fire {
        reason: "control_token_run",
        anomaly_start_offset,
        garbage_start_offset: anomaly_start_offset,
        detail,
    })
}
