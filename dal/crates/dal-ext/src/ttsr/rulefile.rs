//! Rule files: the hand-written front matter parser and the rule-file door.
//!
//! The front matter is a strict YAML subset with named errors and exact line
//! numbers. [`scan::split`] reads only the block; [`parse_rulefile`] validates the
//! file keys and returns both the rule and its non-rejecting diagnostics.
//! Regular expressions are compiled later when the rule set is built.

use std::fmt;

use super::matcher::split_shorthand;
use super::scope::{ScopeInput, ScopeValue, compile_globs, parse_scope};
use super::value::{
    ConditionSource, InterruptMode, Name, Origin, Problem, ProblemKind, RepeatMode, Rule,
    ScopeSpec, Severity,
};

mod scan;
#[cfg(test)]
mod tests;
pub use scan::split;

/// Most header lines between the opening and the closing `---` line.
pub const HEADER_MAX_LINES: usize = 200;

/// Most conditions of one rule.
const CONDITIONS_MAX: usize = 16;

/// Largest `repeatGap`.
const REPEAT_GAP_MAX: u16 = 1000;

/// The delimiter line that opens and closes the front matter.
const FENCE: &str = "---";

/// The file-format keys, in camelCase.
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

/// Parses one rule file into a validated rule and its notes.
///
/// `name` is the file name without `.md`; `origin` is the discovered source
/// and is stamped on the rule and on every problem. Product-specific tool
/// availability is applied by the rule-set builder after parsing.
///
/// # Errors
///
/// Returns every problem when the text is not UTF-8, the name is invalid,
/// the front matter has an error, a key has the wrong type or value, the
/// rule has more than 16 conditions, or the body is empty.
pub fn parse_rulefile(
    name: &str,
    origin: Origin,
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
    let (front, body, front_errors) = scan::split(text);
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
    let scope = resolve_scope(scope.as_ref(), &shorthand.tokens, &origin, &mut notes);
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
        known_tools: None,
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
