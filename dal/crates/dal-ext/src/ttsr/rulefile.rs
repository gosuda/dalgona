//! Rule files: the hand-written front matter parser and the rule-file door.
//!
//! The front matter is a strict YAML subset with named errors and exact line
//! numbers; no YAML crate. [`split`] only reads the block, and
//! [`parse_rulefile`] validates the supported keys without changing the input.
//! Regular expressions are never compiled here; the
//! set build compiles the kept conditions.

use std::fmt;

use super::matcher::split_shorthand;
use super::scope::{ScopeInput, ScopeValue, compile_globs, parse_scope};
use super::value::{
    ConditionSource, InterruptMode, Name, Origin, Problem, ProblemKind, RepeatMode, Rule,
    ScopeSpec, Severity,
};

/// Most header lines between the opening and the closing `---` line.
pub const HEADER_MAX_LINES: usize = 200;

/// Most conditions of one rule.
const CONDITIONS_MAX: usize = 16;

/// Largest `repeatGap`.
const REPEAT_GAP_MAX: u16 = 1000;

/// The delimiter line that opens and closes the front matter.
const FENCE: &str = "---";

/// Supported rule-file keys, in camelCase.
const KNOWN_KEYS: [&str; 11] = [
    "description",
    "condition",
    "scope",
    "globs",
    "agents",
    "alwaysApply",
    "report",
    "enabled",
    "interruptMode",
    "repeatMode",
    "repeatGap",
];

/// Keys of other rule dialects that dal names as unsupported.
const UNSUPPORTED_KEYS: [&str; 4] = ["astCondition", "question", "ttsr_trigger", "ttsrTrigger"];

const EXPECTED_KEY: &str = "expected \"key: value\"";
const UNTERMINATED_QUOTE: &str = "unterminated quoted string";
const TEXT_AFTER_QUOTE: &str = "text after the closing quote";
const UNTERMINATED_LIST: &str = "unterminated list";
const TEXT_AFTER_LIST: &str = "text after the closing \"]\"";
const MISSING_CLOSE: &str = "missing closing \"---\" line";
const TOO_LONG: &str = "the front matter is longer than 200 lines";
const SKIPPED: &str = "dalgon skipped it.";
const IGNORED: &str = "dal ignores it.";

/// One front matter value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Value {
    /// `true` or `false`.
    Bool(bool),
    /// One to nine decimal digits.
    Int(u32),
    /// A plain, quoted, or block scalar.
    Str(String),
    /// A flow list or an indented `- ` list.
    List(Vec<String>),
}

/// One key of a front matter block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    /// The key, normalized from kebab-case to camelCase.
    pub key: String,
    /// The one-based file line of the key.
    pub line: usize,
    /// The parsed value.
    pub value: Value,
}

/// A parsed front matter block: keys in file order, each key once.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FrontMatter {
    entries: Vec<Entry>,
}

impl FrontMatter {
    /// Returns the keys in file order.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Returns the entry of the normalized `key`, if present.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.key == key)
    }
}

/// What a [`FrontError`] reports, which decides its display.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FrontErrorKind {
    /// A line-level error; displays as `line <n>: <reason>`.
    Line,
    /// A normalized key given twice; displays as `line <n>: <reason>`.
    Duplicate,
    /// An error inside one value; displays bare.
    Value,
    /// An unsupported or unknown key; displays bare and rejects nothing.
    Note,
}

/// One front matter error or key note.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrontError {
    /// The one-based file line after BOM removal and CR stripping.
    pub line: usize,
    /// What the entry reports.
    pub kind: FrontErrorKind,
    /// The reason text, without the line prefix.
    pub reason: String,
}

impl FrontError {
    fn new(line: usize, kind: FrontErrorKind, reason: impl Into<String>) -> Self {
        Self {
            line,
            kind,
            reason: reason.into(),
        }
    }

    /// Reports whether the entry rejects the file.
    #[must_use]
    pub fn is_error(&self) -> bool {
        self.kind != FrontErrorKind::Note
    }
}

impl fmt::Display for FrontError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            FrontErrorKind::Line | FrontErrorKind::Duplicate => {
                write!(formatter, "line {}: {}", self.line, self.reason)
            }
            FrontErrorKind::Value => formatter.write_str(&self.reason),
            FrontErrorKind::Note => write!(formatter, "{}. {}", self.reason, IGNORED),
        }
    }
}

/// Splits rule file text into its front matter block, its body, and the
/// errors and key notes of the block.
///
/// A leading UTF-8 BOM is removed and every CR is stripped first, so LF,
/// CRLF, and BOM text parse equal. Without an opening `---` line the whole
/// text is the body. The body is the text after the closing line, trimmed,
/// and otherwise kept byte for byte. A missing closing line or an over-long
/// header yields one error at line 1, an empty block, and an empty body.
#[must_use]
pub fn split(text: &str) -> (FrontMatter, String, Vec<FrontError>) {
    let text = text
        .strip_prefix('\u{feff}')
        .unwrap_or(text)
        .replace('\r', "");
    let mut lines = text.split('\n');
    if lines.next() != Some(FENCE) {
        return (FrontMatter::default(), text.trim().to_owned(), Vec::new());
    }
    let mut header = Vec::new();
    let mut offset = FENCE.len() + 1;
    let mut body = None;
    for line in lines {
        offset += line.len() + 1;
        if line == FENCE {
            body = Some(text.get(offset..).unwrap_or_default().trim().to_owned());
            break;
        }
        header.push(line);
    }
    let Some(body) = body else {
        let error = FrontError::new(1, FrontErrorKind::Line, MISSING_CLOSE);
        return (FrontMatter::default(), String::new(), vec![error]);
    };
    if header.len() > HEADER_MAX_LINES {
        let error = FrontError::new(1, FrontErrorKind::Line, TOO_LONG);
        return (FrontMatter::default(), String::new(), vec![error]);
    }
    let mut parser = Header {
        lines: header,
        next: 0,
        seen: Vec::new(),
        front: FrontMatter::default(),
        errors: Vec::new(),
    };
    parser.run();
    (parser.front, body, parser.errors)
}

/// The header walk: one pass over the lines between the fences.
struct Header<'a> {
    lines: Vec<&'a str>,
    next: usize,
    seen: Vec<String>,
    front: FrontMatter,
    errors: Vec<FrontError>,
}

impl<'a> Header<'a> {
    /// The file line of header index `index`; the opening fence is line 1.
    const fn line_of(index: usize) -> usize {
        index + 2
    }

    fn take(&mut self) -> Option<(usize, &'a str)> {
        let line = *self.lines.get(self.next)?;
        let number = Self::line_of(self.next);
        self.next += 1;
        Some((number, line))
    }

    fn run(&mut self) {
        while let Some((number, line)) = self.take() {
            let rest = line.trim_start_matches(' ');
            if rest.is_empty() || rest.starts_with('#') {
                continue;
            }
            let Some((raw_key, after)) = split_key(line) else {
                self.errors
                    .push(FrontError::new(number, FrontErrorKind::Line, EXPECTED_KEY));
                continue;
            };
            let key = camel_case(raw_key);
            let value = self.value(&key, number, after);
            if self.seen.contains(&key) {
                self.errors.push(FrontError::new(
                    number,
                    FrontErrorKind::Duplicate,
                    format!("\"{key}\" appears twice"),
                ));
                continue;
            }
            self.seen.push(key.clone());
            if let Some(reason) = key_note(&key) {
                self.errors
                    .push(FrontError::new(number, FrontErrorKind::Note, reason));
            }
            match value {
                Ok(value) => self.front.entries.push(Entry {
                    key,
                    line: number,
                    value,
                }),
                Err(error) => self.errors.push(error),
            }
        }
    }

    /// Parses the value after `key:` at `line`, consuming continuation lines.
    fn value(&mut self, key: &str, line: usize, after: &str) -> Result<Value, FrontError> {
        let text = after.trim_start_matches(' ');
        let comment_only = text.starts_with('#') && text.len() < after.len();
        if text.is_empty() || comment_only {
            return self.dash_list(key, line);
        }
        let value_error = |reason: String| FrontError::new(line, FrontErrorKind::Value, reason);
        if text.starts_with(['"', '\'']) {
            let (string, rest) = quoted(text).map_err(value_error)?;
            only_comment(rest, TEXT_AFTER_QUOTE).map_err(value_error)?;
            return Ok(Value::Str(string));
        }
        if text.starts_with('[') {
            return flow_list(text).map(Value::List).map_err(value_error);
        }
        let plain = plain(text);
        Ok(match plain {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            ">" | ">-" | "|" | "|-" => Value::Str(self.block(plain)),
            _ if (1..=9).contains(&plain.len()) && plain.bytes().all(|b| b.is_ascii_digit()) => {
                Value::Int(
                    plain
                        .bytes()
                        .fold(0, |acc, digit| acc * 10 + u32::from(digit - b'0')),
                )
            }
            _ => Value::Str(plain.to_owned()),
        })
    }

    /// Reads the `- ` item lines after an empty value.
    fn dash_list(&mut self, key: &str, line: usize) -> Result<Value, FrontError> {
        let mut items = Vec::new();
        let mut first_error = None;
        while let Some(item) = self.lines.get(self.next).copied().and_then(dash_item) {
            let item_line = Self::line_of(self.next);
            self.next += 1;
            if plain(item).is_empty() {
                continue;
            }
            match item_scalar(item) {
                Ok(item) => items.push(item),
                Err(reason) => {
                    first_error.get_or_insert_with(|| {
                        FrontError::new(item_line, FrontErrorKind::Value, reason)
                    });
                }
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        if items.is_empty() {
            return Err(FrontError::new(
                line,
                FrontErrorKind::Line,
                format!("\"{key}\" has no value"),
            ));
        }
        Ok(Value::List(items))
    }

    /// Reads a block scalar in `style` (`>`, `>-`, `|`, or `|-`).
    fn block(&mut self, style: &str) -> String {
        let start = self.next;
        while self
            .lines
            .get(self.next)
            .is_some_and(|line| line.is_empty() || line.starts_with(' '))
        {
            self.next += 1;
        }
        let mut rows = self.lines.get(start..self.next).unwrap_or_default();
        while let Some((last, rest)) = rows.split_last()
            && is_blank(last)
        {
            rows = rest;
        }
        let indent = rows
            .iter()
            .filter(|row| !is_blank(row))
            .map(|row| row.len() - row.trim_start_matches(' ').len())
            .min()
            .unwrap_or(0);
        let rows = rows.iter().map(|row| {
            if is_blank(row) {
                ""
            } else {
                row.get(indent..).unwrap_or_default()
            }
        });
        let mut out = String::new();
        if style.starts_with('|') {
            for (index, row) in rows.enumerate() {
                if index > 0 {
                    out.push('\n');
                }
                out.push_str(row);
            }
        } else {
            let mut joins = false;
            for row in rows {
                if row.is_empty() {
                    out.push('\n');
                    joins = false;
                } else {
                    if joins {
                        out.push(' ');
                    }
                    out.push_str(row);
                    joins = true;
                }
            }
        }
        if !out.is_empty() && !style.ends_with('-') {
            out.push('\n');
        }
        out
    }
}

fn is_blank(line: &str) -> bool {
    line.trim_start_matches(' ').is_empty()
}

/// Splits `key:` at column 0, the key matching `[A-Za-z_][A-Za-z0-9_-]*`.
fn split_key(line: &str) -> Option<(&str, &str)> {
    let bytes = line.as_bytes();
    let first = *bytes.first()?;
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return None;
    }
    let end = bytes
        .iter()
        .position(|b| !(b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')))
        .unwrap_or(bytes.len());
    let (key, rest) = line.split_at(end);
    rest.strip_prefix(':').map(|after| (key, after))
}

/// Normalizes kebab-case to camelCase: each `-` is dropped and the next
/// character is upper-cased.
fn camel_case(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    let mut upper = false;
    for ch in key.chars() {
        if ch == '-' {
            upper = true;
        } else if upper {
            out.push(ch.to_ascii_uppercase());
            upper = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// Returns the diagnostic for an unsupported or unknown key.
fn key_note(key: &str) -> Option<String> {
    if KNOWN_KEYS.contains(&key) {
        None
    } else if UNSUPPORTED_KEYS.contains(&key) {
        Some(format!("\"{key}\" is not supported"))
    } else {
        Some(format!("unknown key \"{key}\""))
    }
}

/// Returns a plain scalar: the text up to the first ` #`, trimmed.
fn plain(text: &str) -> &str {
    text.find(" #").map_or(text, |at| &text[..at]).trim()
}

/// Accepts only spaces or a ` #` comment after a closing quote or bracket.
fn only_comment(rest: &str, reason: &str) -> Result<(), String> {
    let text = rest.trim_start_matches(' ');
    if text.is_empty() || (text.starts_with('#') && text.len() < rest.len()) {
        Ok(())
    } else {
        Err(reason.to_owned())
    }
}

/// Reads one quoted scalar at the start of `text`; returns it and the rest
/// after the closing quote.
///
/// Double quotes take the escapes `\"` `\\` `\/` `\n` `\t` `\uXXXX`; single
/// quotes take `''` as a quote and no escapes.
fn quoted(text: &str) -> Result<(String, &str), String> {
    let mut chars = text.char_indices();
    let Some((_, quote)) = chars.next() else {
        return Err(UNTERMINATED_QUOTE.to_owned());
    };
    let mut out = String::new();
    while let Some((at, ch)) = chars.next() {
        if ch == quote {
            let rest = &text[at + 1..];
            if quote == '\'' && rest.starts_with('\'') {
                chars.next();
                out.push('\'');
                continue;
            }
            return Ok((out, rest));
        }
        if ch != '\\' || quote == '\'' {
            out.push(ch);
            continue;
        }
        let Some((_, escape)) = chars.next() else {
            return Err(UNTERMINATED_QUOTE.to_owned());
        };
        out.push(match escape {
            '"' | '\\' | '/' => escape,
            'n' => '\n',
            't' => '\t',
            'u' => {
                let mut hex = String::with_capacity(4);
                while hex.len() < 4
                    && let Some((_, digit)) = chars.clone().next()
                    && digit.is_ascii_hexdigit()
                {
                    hex.push(digit);
                    chars.next();
                }
                let decoded = (hex.len() == 4)
                    .then(|| u32::from_str_radix(&hex, 16).ok())
                    .flatten()
                    .and_then(char::from_u32);
                decoded.ok_or_else(|| format!("bad escape \"\\u{hex}\""))?
            }
            other => return Err(format!("bad escape \"\\{other}\"")),
        });
    }
    Err(UNTERMINATED_QUOTE.to_owned())
}

/// Reads a one-line flow list `[a, "b,c", 'd']`; commas inside quotes do
/// not split, and an empty plain item (`[a,]`, `[a,,b]`) drops, so no empty
/// condition source can reach a rule. A quoted empty item is kept.
fn flow_list(text: &str) -> Result<Vec<String>, String> {
    let bytes = text.as_bytes();
    let mut parts = Vec::new();
    let mut start = 1;
    let mut quote = None;
    let mut close = None;
    let mut at = 1;
    while let Some(&byte) = bytes.get(at) {
        match quote {
            Some(b'"') if byte == b'\\' => at += 1,
            Some(open) if byte == open => quote = None,
            Some(_) => {}
            None => match byte {
                b'"' | b'\'' => quote = Some(byte),
                b',' => {
                    parts.push(&text[start..at]);
                    start = at + 1;
                }
                b']' => {
                    parts.push(&text[start..at]);
                    close = Some(at);
                    break;
                }
                _ => {}
            },
        }
        at += 1;
    }
    let Some(close) = close else {
        return Err(UNTERMINATED_LIST.to_owned());
    };
    only_comment(&text[close + 1..], TEXT_AFTER_LIST)?;
    parts
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .map(|part| item_scalar(part.trim()))
        .collect()
}

/// Returns the text after `- ` of a line matching `^\s*- `, keeping the
/// space so a following `#` reads as a comment.
fn dash_item(line: &str) -> Option<&str> {
    line.trim_start_matches([' ', '\t'])
        .strip_prefix('-')
        .filter(|rest| rest.starts_with(' '))
}

/// Reads a `- ` list item: a quoted or a plain scalar.
fn item_scalar(item: &str) -> Result<String, String> {
    let text = item.trim_start_matches(' ');
    if text.starts_with(['"', '\'']) {
        let (value, rest) = quoted(text)?;
        only_comment(rest, TEXT_AFTER_QUOTE)?;
        Ok(value)
    } else {
        Ok(plain(item).to_owned())
    }
}

/// Parses one rule file into a validated rule.
///
/// `name` is the file name without `.md`; `origin` is the file's origin and
/// is stamped on the rule and on every problem. `known_tools` is the product
/// tool inventory for the scope's unknown-tool notes; `None` skips them. On success the problems are
/// the key notes and the Skipped scope and glob remarks that do not reject
/// the rule.
///
/// # Errors
///
/// Returns every problem when the text is not UTF-8, the name is invalid,
/// the front matter has an error, a key has the wrong type or value, the
/// rule has more than 16 conditions, or the body is empty.
pub fn parse_rulefile(
    name: &str,
    origin: Origin,
    known_tools: Option<&[&str]>,
    bytes: &[u8],
) -> Result<(Rule, Vec<Problem>), Vec<Problem>> {
    let skipped = |reason: String| Problem {
        origin: origin.clone(),
        kind: ProblemKind::Rule,
        reason,
        consequence: SKIPPED.to_owned(),
        severity: Severity::Skipped,
    };
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Err(vec![Problem {
            kind: ProblemKind::File,
            ..skipped("cannot read the file: stream did not contain valid UTF-8".to_owned())
        }]);
    };
    let checked_name = Name::parse(name);
    let mut rejects: Vec<Problem> = checked_name
        .is_none()
        .then(|| skipped(invalid_name(name)))
        .into_iter()
        .collect();
    let (front, body, front_errors) = split(text);
    let (mut header_rejects, mut notes) = front_problems(front_errors, &origin);
    if !header_rejects.is_empty() {
        rejects.append(&mut header_rejects);
        rejects.append(&mut notes);
        return Err(rejects);
    }

    let mut keys = Keys {
        front: &front,
        reasons: Vec::new(),
    };
    let description = keys.string("description");
    let conditions = keys.strings("condition").unwrap_or_default();
    let scope = keys.strings_or_string("scope");
    let globs = keys.strings("globs");
    let agents = keys.strings("agents");
    let always_apply = keys.bool("alwaysApply", false);
    let report = keys.bool("report", false);
    let enabled = keys.bool("enabled", true);
    let interrupt_mode = keys.interrupt_mode();
    let repeat_mode = keys.repeat_mode();
    let repeat_gap = keys.repeat_gap();
    if conditions.len() > CONDITIONS_MAX {
        keys.reasons
            .push("the rule has more than 16 conditions".to_owned());
    }
    if body.is_empty() {
        keys.reasons.push("the body is empty".to_owned());
    }
    rejects.extend(keys.reasons.into_iter().map(&skipped));
    let Some(name) = checked_name.filter(|_| rejects.is_empty()) else {
        rejects.append(&mut notes);
        return Err(rejects);
    };

    let conditions: Vec<ConditionSource> = (0..)
        .zip(conditions)
        .map(|(index, src)| ConditionSource {
            index,
            src: src.into(),
        })
        .collect();
    let shorthand = split_shorthand(&conditions);
    let scope = resolve_scope(
        scope.as_ref(),
        &shorthand.tokens,
        &origin,
        known_tools,
        &mut notes,
    );
    let globs = globs.and_then(|patterns| glob_filter(&patterns, &origin, &mut notes));
    let agents = agents.and_then(|patterns| glob_filter(&patterns, &origin, &mut notes));
    let rule = Rule {
        name,
        origin,
        description,
        conditions: shorthand.conditions,
        scope,
        globs,
        agents,
        always_apply,
        report,
        enabled,
        interrupt_mode,
        repeat_mode,
        repeat_gap,
        judge: None,
        body,
    };
    Ok((rule, notes))
}

/// Maps the errors and key notes of [`split`] to rejecting problems and to
/// notes, in that order.
fn front_problems(errors: Vec<FrontError>, origin: &Origin) -> (Vec<Problem>, Vec<Problem>) {
    let mut rejects = Vec::new();
    let mut notes = Vec::new();
    for error in errors {
        let (reason, consequence, severity, into) = match error.kind {
            FrontErrorKind::Note => (error.reason, IGNORED, Severity::Note, &mut notes),
            FrontErrorKind::Duplicate => (error.reason, SKIPPED, Severity::Skipped, &mut rejects),
            FrontErrorKind::Line | FrontErrorKind::Value => (
                format!("front matter line {}: {}", error.line, error.reason),
                SKIPPED,
                Severity::Skipped,
                &mut rejects,
            ),
        };
        into.push(Problem {
            origin: origin.clone(),
            kind: ProblemKind::Rule,
            reason,
            consequence: consequence.to_owned(),
            severity,
        });
    }
    (rejects, notes)
}

/// The problem reason of a rule name outside the name grammar.
fn invalid_name(name: &str) -> String {
    format!(
        "the name \"{name}\" is invalid; use letters, digits, \".\", \"_\", and \"-\", at most 64 characters, starting with a letter or digit"
    )
}

/// Resolves the written scope plus the condition shorthand tokens.
fn resolve_scope(
    scope: Option<&ScopeText>,
    shorthand: &[Box<str>],
    origin: &Origin,
    known_tools: Option<&[&str]>,
    notes: &mut Vec<Problem>,
) -> ScopeSpec {
    let extra_tokens: Vec<String> = shorthand.iter().map(|token| (**token).to_owned()).collect();
    let value = match scope {
        None => ScopeValue::Missing,
        Some(ScopeText::One(text)) => ScopeValue::String(text),
        Some(ScopeText::Many(items)) => ScopeValue::List(items),
    };
    let (spec, mut problems) = parse_scope(ScopeInput {
        origin,
        value,
        known_tools,
        extra_tokens: &extra_tokens,
    });
    notes.append(&mut problems);
    spec
}

/// Builds a path or agent glob filter; bad globs become Skipped remarks.
///
/// When the set as a whole fails to build, every pattern gets the remark
/// and the filter admits nothing, so the rule never applies unseen.
fn glob_filter(
    patterns: &[String],
    origin: &Origin,
    notes: &mut Vec<Problem>,
) -> Option<globset::GlobSet> {
    if let Ok((set, mut problems)) = compile_globs(patterns, origin) {
        notes.append(&mut problems);
        return set;
    }
    notes.extend(patterns.iter().map(|pattern| Problem {
        origin: origin.clone(),
        kind: ProblemKind::Glob,
        reason: format!("glob \"{pattern}\" is invalid. {SKIPPED}"),
        consequence: String::new(),
        severity: Severity::Skipped,
    }));
    Some(globset::GlobSet::empty())
}

/// A `scope` value as written.
enum ScopeText {
    One(String),
    Many(Vec<String>),
}

/// Read-only typed access to the keys of one block, collecting the
/// `"<key>" must be <type>` reasons in call order.
struct Keys<'f> {
    front: &'f FrontMatter,
    reasons: Vec<String>,
}

impl Keys<'_> {
    fn wrong(&mut self, key: &str, kind: &str) {
        self.reasons.push(format!("\"{key}\" must be {kind}"));
    }

    fn string(&mut self, key: &str) -> Option<String> {
        if let Value::Str(text) = &self.front.get(key)?.value {
            return Some(text.clone());
        }
        self.wrong(key, "a string");
        None
    }

    fn bool(&mut self, key: &str, default: bool) -> bool {
        match self.front.get(key).map(|entry| &entry.value) {
            None => default,
            Some(Value::Bool(flag)) => *flag,
            Some(_) => {
                self.wrong(key, "true or false");
                default
            }
        }
    }

    fn int(&mut self, key: &str) -> Option<u32> {
        if let Value::Int(number) = &self.front.get(key)?.value {
            return Some(*number);
        }
        self.wrong(key, "a whole number");
        None
    }

    fn interrupt_mode(&mut self) -> Option<InterruptMode> {
        let mode = self.string("interruptMode")?;
        let parsed = match mode.as_str() {
            "always" => Some(InterruptMode::Always),
            "prose-only" => Some(InterruptMode::ProseOnly),
            "tool-only" => Some(InterruptMode::ToolOnly),
            "never" => Some(InterruptMode::Never),
            _ => None,
        };
        if parsed.is_none() {
            self.reasons.push(format!(
                "interruptMode \"{mode}\" is invalid; use always, prose-only, tool-only, or never"
            ));
        }
        parsed
    }

    fn repeat_mode(&mut self) -> Option<RepeatMode> {
        let mode = self.string("repeatMode")?;
        let parsed = match mode.as_str() {
            "once" => Some(RepeatMode::Once),
            "after-gap" => Some(RepeatMode::AfterGap),
            _ => None,
        };
        if parsed.is_none() {
            self.reasons.push(format!(
                "repeatMode \"{mode}\" is invalid; use once or after-gap"
            ));
        }
        parsed
    }

    fn repeat_gap(&mut self) -> Option<u16> {
        let gap = self.int("repeatGap")?;
        let parsed = u16::try_from(gap)
            .ok()
            .filter(|gap| (1..=REPEAT_GAP_MAX).contains(gap));
        if parsed.is_none() {
            self.reasons.push(format!(
                "repeatGap {gap} is invalid; use a whole number from 1 to 1000"
            ));
        }
        parsed
    }

    fn strings_or_string(&mut self, key: &str) -> Option<ScopeText> {
        match &self.front.get(key)?.value {
            Value::Str(text) => Some(ScopeText::One(text.clone())),
            Value::List(items) => Some(ScopeText::Many(items.clone())),
            _ => {
                self.wrong(key, "a string or a list of strings");
                None
            }
        }
    }

    fn strings(&mut self, key: &str) -> Option<Vec<String>> {
        Some(match self.strings_or_string(key)? {
            ScopeText::One(text) => vec![text],
            ScopeText::Many(items) => items,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use std::fmt::Write as _;

    use proptest::prelude::*;

    use super::*;
    use crate::ttsr::value::ToolScope;

    /// The rejecting errors of `text`, key notes excluded.
    fn fails(text: &str) -> Vec<FrontError> {
        split(text)
            .2
            .into_iter()
            .filter(FrontError::is_error)
            .collect()
    }

    fn errors(text: &str) -> Vec<String> {
        fails(text).iter().map(ToString::to_string).collect()
    }

    fn value(text: &str) -> Value {
        assert_eq!(fails(text), Vec::new(), "{text:?}");
        split(text).0.entries()[0].value.clone()
    }

    fn origin() -> Origin {
        Origin::Project(PathBuf::from(".dal/rules/r.md"))
    }

    fn reasons(text: &str) -> Vec<String> {
        match parse_rulefile("r", origin(), None, text.as_bytes()) {
            Ok(_) => Vec::new(),
            Err(problems) => problems.into_iter().map(|p| p.reason).collect(),
        }
    }

    #[test]
    fn parser_happy_path() {
        let text = "---\ndescription: Stops a patch that adds a bare TODO marker.\ncondition: [\"a,b\", \"c\\\\d\"]\nscope: tool:patch\ninterruptMode: tool-only\nrepeatMode: after-gap\nrepeatGap: 1\nreport: true\n---\n\nTOOL CALL BLOCKED BEFORE EXECUTION.\n\n";
        let (front, body, errs) = split(text);
        assert!(errs.is_empty());
        let keys: Vec<&str> = front.entries().iter().map(|e| e.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "description",
                "condition",
                "scope",
                "interruptMode",
                "repeatMode",
                "repeatGap",
                "report"
            ]
        );
        assert_eq!(
            front.get("repeatGap").map(|e| &e.value),
            Some(&Value::Int(1))
        );
        assert_eq!(
            front.get("condition").map(|e| &e.value),
            Some(&Value::List(vec!["a,b".to_owned(), "c\\d".to_owned()]))
        );
        assert_eq!(body, "TOOL CALL BLOCKED BEFORE EXECUTION.");
    }

    #[test]
    fn parser_errors() {
        assert_eq!(
            errors("---\ndescription: a\n"),
            ["line 1: missing closing \"---\" line"]
        );
        assert_eq!(
            errors("---\ndescription: a\ndescription: b\n---\nx"),
            ["line 3: \"description\" appears twice"]
        );
        let long = format!("---\n{}---\nx", "# c\n".repeat(201));
        assert_eq!(
            errors(&long),
            ["line 1: the front matter is longer than 200 lines"]
        );
        let fits = format!("---\n{}---\nx", "# c\n".repeat(200));
        assert!(errors(&fits).is_empty());
        assert_eq!(
            errors("---\nx: \"ab\n---\nx"),
            ["unterminated quoted string"]
        );
    }

    #[test]
    fn missing_header_is_body_only() {
        let (front, body, errs) = split("\u{feff}-- \r\nplain\r\n");
        assert!(front.entries().is_empty() && errs.is_empty());
        assert_eq!(body, "-- \nplain");
    }

    #[test]
    fn body_is_byte_faithful_after_closing_line() {
        let (_, body, _) = split("---\n---\n  a  \n\tb # c\n\n");
        assert_eq!(body, "a  \n\tb # c");
    }

    #[test]
    fn crlf_and_bom_parse_equal_to_lf() {
        let lf = "---\nx: |\n  a\n\n  b\ny:\n  - 'q'\n---\nbody\nnext\n";
        let crlf = format!("\u{feff}{}", lf.replace('\n', "\r\n"));
        assert_eq!(split(lf), split(&crlf));
    }

    #[test]
    fn flow_lists_split_on_commas_outside_quotes() {
        assert_eq!(
            value("---\nx: [a, \"b,c\", 'd,''e', \"f\\\"],g\" ] # c\n---\nb"),
            Value::List(vec![
                "a".to_owned(),
                "b,c".to_owned(),
                "d,'e".to_owned(),
                "f\"],g".to_owned()
            ])
        );
        assert_eq!(value("---\nx: [ ]\n---\nb"), Value::List(Vec::new()));
        assert_eq!(
            value("---\nx: [a,]\n---\nb"),
            Value::List(vec!["a".to_owned()])
        );
        assert_eq!(
            value("---\nx: [a,,b]\n---\nb"),
            Value::List(vec!["a".to_owned(), "b".to_owned()])
        );
        assert_eq!(
            value("---\nx: [a, \"\"]\n---\nb"),
            Value::List(vec!["a".to_owned(), String::new()])
        );
        assert_eq!(
            value("---\nx: [TODO #1, 'a #b', x] # c\n---\nb"),
            Value::List(vec!["TODO".to_owned(), "a #b".to_owned(), "x".to_owned()])
        );
        assert_eq!(errors("---\nx: [a\n---\nb"), [UNTERMINATED_LIST]);
        assert_eq!(errors("---\nx: [a, \"b]\"\n---\nb"), [UNTERMINATED_LIST]);
        assert_eq!(errors("---\nx: [a, 'b]\n---\nb"), [UNTERMINATED_LIST]);
        assert_eq!(errors("---\nx: [a] b\n---\nb"), [TEXT_AFTER_LIST]);
        assert_eq!(errors("---\nx: [\"a\" b]\n---\nb"), [TEXT_AFTER_QUOTE]);
    }

    #[test]
    fn quoted_escapes_are_exact() {
        assert_eq!(
            value("---\nx: \"\\\"\\\\\\/\\n\\t\\u00e9\\u0041\"\n---\nb"),
            Value::Str("\"\\/\n\t\u{e9}A".to_owned())
        );
        assert_eq!(
            value("---\nx: 'a\\n''b'\n---\nb"),
            Value::Str("a\\n'b".to_owned())
        );
        assert_eq!(errors("---\nx: \"\\q\"\n---\nb"), ["bad escape \"\\q\""]);
        assert_eq!(
            errors("---\nx: \"\\u12G4\"\n---\nb"),
            ["bad escape \"\\u12\""]
        );
        assert_eq!(
            errors("---\nx: \"\\uD800\"\n---\nb"),
            ["bad escape \"\\uD800\""]
        );
        assert_eq!(
            errors("---\nx: \"\\u00e\"\n---\nb"),
            ["bad escape \"\\u00e\""]
        );
        assert_eq!(errors("---\nx: \"a\\\"\n---\nb"), [UNTERMINATED_QUOTE]);
        assert_eq!(errors("---\nx: 'a'b\n---\nb"), [TEXT_AFTER_QUOTE]);
        assert_eq!(errors("---\nx: \"a\"#b\n---\nb"), [TEXT_AFTER_QUOTE]);
        assert!(errors("---\nx: \"a\"  # b\n---\nb").is_empty());
    }

    #[test]
    fn scalars_and_comments() {
        assert_eq!(value("---\nx: true # c\n---\nb"), Value::Bool(true));
        assert_eq!(value("---\nx: 000000123\n---\nb"), Value::Int(123));
        assert_eq!(
            value("---\nx: 1234567890\n---\nb"),
            Value::Str("1234567890".to_owned())
        );
        assert_eq!(
            value("---\nx: a#b # c\n---\nb"),
            Value::Str("a#b".to_owned())
        );
        assert_eq!(value("---\nx:a\n---\nb"), Value::Str("a".to_owned()));
        assert_eq!(
            value("---\n  # c\nx: True\n---\nb"),
            Value::Str("True".to_owned())
        );
        assert_eq!(
            value("---\nx: # c\n  - a # c\n  - '#b'\n---\nb"),
            Value::List(vec!["a".to_owned(), "#b".to_owned()])
        );
    }

    #[test]
    fn line_errors_carry_exact_lines() {
        assert_eq!(
            errors("---\nx:\ny: 1\n  z: 2\n\tw: 3\n9a: 4\n---\nb"),
            [
                "line 2: \"x\" has no value",
                "line 4: expected \"key: value\"",
                "line 5: expected \"key: value\"",
                "line 6: expected \"key: value\"",
            ]
        );
        assert_eq!(
            fails("---\nx:\n  - a\n  - \"\\q\"\n---\nb"),
            [FrontError::new(
                4,
                FrontErrorKind::Value,
                "bad escape \"\\q\""
            )]
        );
    }

    #[test]
    fn duplicate_keys_are_normalized() {
        assert_eq!(
            errors("---\ninterrupt-mode: always\ninterruptMode: never\n---\nb"),
            ["line 3: \"interruptMode\" appears twice"]
        );
        let (front, _, _) = split("---\nrepeat-gap: 2\n---\nb");
        assert_eq!(front.get("repeatGap").map(|e| e.line), Some(2));
        let (front, _, errs) = split("---\nx: \"ab\nx: ok\n---\nb");
        assert_eq!(
            fails_errs(&errs),
            ["unterminated quoted string", "line 3: \"x\" appears twice"]
        );
        assert!(front.get("x").is_none());
        let (front, _, errs) = split("---\nx:\n  - \n  - b\n---\nb");
        assert!(fails_errs(&errs).is_empty());
        assert_eq!(
            front.get("x").map(|e| &e.value),
            Some(&Value::List(vec!["b".to_owned()]))
        );
    }

    /// The rejecting errors of a split result, as display text.
    fn fails_errs(errors: &[FrontError]) -> Vec<String> {
        errors
            .iter()
            .filter(|error| error.is_error())
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn block_scalars_fold_and_keep() {
        let block = |style: &str| {
            value(&format!(
                "---\nx: {style}\n    a\n    b\n\n     c\n\n\ny: 1\n---\nb"
            ))
        };
        assert_eq!(block(">"), Value::Str("a b\n c\n".to_owned()));
        assert_eq!(block(">-"), Value::Str("a b\n c".to_owned()));
        assert_eq!(block("|"), Value::Str("a\nb\n\n c\n".to_owned()));
        assert_eq!(block("|-"), Value::Str("a\nb\n\n c".to_owned()));
        assert_eq!(value("---\nx: |\ny: 1\n---\nb"), Value::Str(String::new()));
    }

    #[test]
    fn key_notes_do_not_reject() {
        let notes: Vec<String> = split("---\nttsr-trigger: a\nquestion: q\nfoo_bar: 1\n---\nb")
            .2
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            notes,
            [
                "\"ttsrTrigger\" is not supported. dal ignores it.",
                "\"question\" is not supported. dal ignores it.",
                "unknown key \"foo_bar\". dal ignores it.",
            ]
        );
        let (rule, notes) = parse_rulefile("r", origin(), None, b"---\nquestion: q\n---\nbody")
            .unwrap_or_else(|p| panic!("{p:?}"));
        assert_eq!(rule.body, "body");
        assert_eq!(notes.len(), 1);
        assert_eq!(
            (
                notes[0].reason.as_str(),
                notes[0].consequence.as_str(),
                notes[0].severity
            ),
            ("\"question\" is not supported", IGNORED, Severity::Note)
        );
    }

    #[test]
    fn rule_record_errors() {
        assert_eq!(
            reasons("---\ninterruptMode: sometimes\nalwaysApply: \"true\"\n---\n"),
            [
                "\"alwaysApply\" must be true or false",
                "interruptMode \"sometimes\" is invalid; use always, prose-only, tool-only, or never",
                "the body is empty",
            ]
        );
        assert_eq!(
            reasons("---\nrepeatMode: twice\nrepeatGap: 1001\ndescription: [a]\n---\nb"),
            [
                "\"description\" must be a string",
                "repeatMode \"twice\" is invalid; use once or after-gap",
                "repeatGap 1001 is invalid; use a whole number from 1 to 1000",
            ]
        );
        assert_eq!(
            reasons("---\nrepeatGap: 0\n---\nb"),
            ["repeatGap 0 is invalid; use a whole number from 1 to 1000"]
        );
        assert_eq!(
            reasons("---\nscope: 3\n---\nb"),
            ["\"scope\" must be a string or a list of strings"]
        );
        let many = format!("---\ncondition: [{}]\n---\nb", vec!["a"; 17].join(","));
        assert_eq!(reasons(&many), ["the rule has more than 16 conditions"]);
        let problems = parse_rulefile("bad name", origin(), None, b"---\nx: \"a\n---\nb")
            .err()
            .unwrap_or_default();
        let texts: Vec<(&str, &str)> = problems
            .iter()
            .map(|p| (p.reason.as_str(), p.consequence.as_str()))
            .collect();
        assert_eq!(
            texts,
            [
                (
                    "the name \"bad name\" is invalid; use letters, digits, \".\", \"_\", and \"-\", at most 64 characters, starting with a letter or digit",
                    SKIPPED
                ),
                ("front matter line 2: unterminated quoted string", SKIPPED),
                ("unknown key \"x\"", IGNORED),
            ]
        );
        assert_eq!(
            reasons("---\na: 1\na: 2\n---\nb"),
            ["\"a\" appears twice", "unknown key \"a\""]
        );
        assert_eq!(
            parse_rulefile("r", origin(), None, b"\xff")
                .err()
                .map(|p| p[0].kind),
            Some(ProblemKind::File)
        );
    }

    #[test]
    fn valid_rule_fields_and_defaults() {
        let text = "---\ndescription: d\ncondition: '\\bTODO\\b'\nrepeat-mode: after-gap\nrepeatGap: 3\ninterrupt-mode: tool-only\nreport: true\n---\nBody\n";
        let (rule, notes) = parse_rulefile("guard.todo", origin(), None, text.as_bytes())
            .unwrap_or_else(|p| panic!("{p:?}"));
        assert!(notes.is_empty());
        assert_eq!(rule.name.as_str(), "guard.todo");
        assert_eq!(rule.origin, origin());
        assert_eq!(rule.description.as_deref(), Some("d"));
        assert_eq!(
            rule.conditions,
            [ConditionSource {
                index: 0,
                src: "\\bTODO\\b".into()
            }]
        );
        assert_eq!(rule.repeat_mode, Some(RepeatMode::AfterGap));
        assert_eq!(rule.repeat_gap, Some(3));
        assert_eq!(rule.interrupt_mode, Some(InterruptMode::ToolOnly));
        assert!(rule.report && rule.enabled && !rule.always_apply);
        assert!(rule.scope.text && !rule.scope.thinking && rule.scope.tools == ToolScope::All);
        assert!(rule.globs.is_none() && rule.agents.is_none() && rule.judge.is_none());
        assert_eq!(rule.body, "Body");
        let (bare, _) = parse_rulefile("r", origin(), None, b"\r\n just body \r\n")
            .unwrap_or_else(|p| panic!("{p:?}"));
        assert!(bare.conditions.is_empty() && bare.description.is_none());
        assert_eq!(bare.body, "just body");
    }

    /// A generated front matter value, written by `write`.
    #[derive(Clone, Debug)]
    enum Gen {
        Bool(bool),
        Int(u32),
        Str(String),
        Flow(Vec<String>),
        Dashes(Vec<String>),
        Literal(Vec<String>),
    }

    fn quote(text: &str) -> String {
        format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
    }

    /// The oracle writer: one AST, one text; the parsed value it expects.
    fn write(entries: &[(String, Gen)], body: &str) -> (String, Vec<(String, Value)>) {
        let mut text = String::from("---\n");
        let mut expected = Vec::new();
        for (key, generated) in entries {
            let value = match generated {
                Gen::Bool(flag) => {
                    let _ = writeln!(text, "{key}: {flag}");
                    Value::Bool(*flag)
                }
                Gen::Int(number) => {
                    let _ = writeln!(text, "{key}: {number}");
                    Value::Int(*number)
                }
                Gen::Str(item) => {
                    let _ = writeln!(text, "{key}: {} # note", quote(item));
                    Value::Str(item.clone())
                }
                Gen::Flow(items) => {
                    let written: Vec<String> = items.iter().map(|item| quote(item)).collect();
                    let _ = writeln!(text, "{key}: [{}]", written.join(", "));
                    Value::List(items.clone())
                }
                Gen::Dashes(items) => {
                    let _ = writeln!(text, "{key}:");
                    for item in items {
                        let _ = writeln!(text, "  - {}", quote(item));
                    }
                    Value::List(items.clone())
                }
                Gen::Literal(rows) => {
                    let _ = writeln!(text, "{key}: |-");
                    for row in rows {
                        let _ = writeln!(text, "  {row}");
                    }
                    Value::Str(rows.join("\n"))
                }
            };
            expected.push((key.clone(), value));
        }
        text.push_str("---\n");
        text.push_str(body);
        (text, expected)
    }

    fn scalar() -> impl Strategy<Value = String> {
        "[a-z0-9 ,#'\"\\\\\\[\\]é]{0,10}"
    }

    fn generated() -> impl Strategy<Value = Gen> {
        prop_oneof![
            any::<bool>().prop_map(Gen::Bool),
            (0u32..1_000_000_000).prop_map(Gen::Int),
            scalar().prop_map(Gen::Str),
            prop::collection::vec(scalar(), 0..4).prop_map(Gen::Flow),
            prop::collection::vec(scalar(), 1..4).prop_map(Gen::Dashes),
            prop::collection::vec("[a-z]{1,6}( [a-z]{1,6})?", 1..4).prop_map(Gen::Literal),
        ]
    }

    proptest! {
        #[test]
        fn property_line_endings(
            entries in prop::collection::btree_map("[a-z][a-z_]{0,6}", generated(), 0..6),
            body in "[a-z]{1,6}(\n[a-z ]{0,6}){0,3}",
        ) {
            let entries: Vec<(String, Gen)> = entries.into_iter().collect();
            let (lf, expected) = write(&entries, &body);
            let crlf = lf.replace('\n', "\r\n");
            let bom = format!("\u{feff}{lf}");
            let parsed = split(&lf);
            let got: Vec<(String, Value)> = parsed.0.entries().iter().map(|e| (e.key.clone(), e.value.clone())).collect();
            prop_assert_eq!(got, expected);
            prop_assert_eq!(&parsed.1, body.trim());
            prop_assert!(parsed.2.iter().all(|e| !e.is_error()));
            prop_assert_eq!(&split(&crlf), &parsed);
            prop_assert_eq!(&split(&bom), &parsed);
        }
    }
}
