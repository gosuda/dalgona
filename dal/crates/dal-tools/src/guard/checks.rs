use super::{metrics, report, stream::G8Rule};
use crate::parse::{self, Language, Parsed};
use crate::patch::{DiffHunk, DiffLineKind};
use regex::Regex;
use std::collections::BTreeSet;
use std::path::{Component, Path};
use std::sync::LazyLock;
use tree_sitter::{Node, Tree};

static GUARD_WRAP: LazyLock<Result<Regex, regex::Error>> = LazyLock::new(|| {
    Regex::new(r"\b(if|match|try|catch|or_else|unwrap_or)\b|\?\?|\bwith\s+_\s*->")
});
static BROAD_OCAML: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"\|\s*_\s*->|\bwith\s+_\s*->"));
static BROAD_RUST: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"(^|[\s{,(])_\s*=>"));
static BROAD_JS: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"\bdefault\s*:"));
static BROAD_PYTHON: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"^\s*except\s*:|^\s*except\s+Exception(\s+as\s+\w+)?\s*:"));
static BROAD_CPP: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"\bcatch\s*\(\s*\.\.\.\s*\)"));

static COMMENT_OCAML: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"^\(\*\s*(let |type |open |match |try |if |for |while )"));
static COMMENT_PYTHON: LazyLock<Result<Regex, regex::Error>> = LazyLock::new(|| {
    Regex::new(r"^#\s*(def |class |if |elif |for |while |return |import |from |print\()")
});
static COMMENT_JS: LazyLock<Result<Regex, regex::Error>> = LazyLock::new(|| {
    Regex::new(
        r"^(//|/\*)\s*(const |let |var |function |class |if |for |while |return |import |export )",
    )
});
static COMMENT_GO: LazyLock<Result<Regex, regex::Error>> = LazyLock::new(|| {
    Regex::new(r"^(//|/\*)\s*(func |type |var |const |if |for |switch |select |return |import )")
});
static COMMENT_C: LazyLock<Result<Regex, regex::Error>> = LazyLock::new(|| {
    Regex::new(
        r"^(//|/\*)\s*(int |void |char |bool |float |double |if |for |while |switch |return |struct |class )",
    )
});
static COMMENT_RUST: LazyLock<Result<Regex, regex::Error>> = LazyLock::new(|| {
    Regex::new(
        r"^(//|/\*)\s*(let |fn |pub |use |struct |enum |impl |if |for |while |loop |match |return )",
    )
});

/// A deterministic check rejection. Only the parse and placeholder gates create one.
#[derive(Debug, Clone)]
pub(super) struct Rejection(pub(super) String);

pub(super) fn placeholder(added: &str) -> Option<Rejection> {
    const PHRASES: [&str; 6] = [
        "rest of methods",
        "implementation omitted",
        "// ... rest of the code",
        "# ... rest of implementation",
        "-- ... rest of implementation",
        "(* ... rest of implementation *)",
    ];
    let mut matched = false;
    let mut remaining = added.to_owned();
    for phrase in PHRASES {
        if remaining.contains(phrase) {
            matched = true;
            remaining = remaining.replace(phrase, "");
        }
    }
    let mut filtered_lines = Vec::new();
    for line in remaining.lines() {
        if standalone_ellipsis(line) {
            matched = true;
        } else {
            filtered_lines.push(line);
        }
    }
    if !matched {
        return None;
    }
    let non_whitespace = filtered_lines
        .iter()
        .flat_map(|line| line.chars())
        .filter(|character| !character.is_whitespace())
        .count();
    (non_whitespace < 20).then(|| Rejection(report::PLACEHOLDER_REJECT.to_owned()))
}

fn standalone_ellipsis(line: &str) -> bool {
    let line = line.trim();
    if matches!(line, "..." | "…") {
        return true;
    }
    [
        ("//", ""),
        ("#", ""),
        ("--", ""),
        ("/*", "*/"),
        ("(*", "*)"),
    ]
    .iter()
    .any(|(start, end)| {
        let Some(body) = line.strip_prefix(start) else {
            return false;
        };
        let body = if end.is_empty() {
            body
        } else if let Some(body) = body.strip_suffix(end) {
            body
        } else {
            return false;
        };
        matches!(body.trim(), "..." | "…")
    })
}

#[derive(Debug, Clone)]
pub(super) enum GateOutcome<'a> {
    Pass(Option<&'a Parsed>),
    Exempt(Option<&'a Parsed>),
    Reject(Rejection),
    Skipped,
}

pub(super) fn parse_gate<'a>(
    path: &str,
    absolute_path: &Path,
    pre: Option<&[u8]>,
    post: &[u8],
    pre_parsed: Option<&'a Parsed>,
    post_parsed: Option<&'a Parsed>,
) -> GateOutcome<'a> {
    if absolute_path
        .extension()
        .and_then(|extension| extension.to_str())
        == Some("json")
    {
        return match parse_json(post) {
            Ok(()) => GateOutcome::Pass(None),
            Err(error) => {
                let line = u32::try_from(error.line()).unwrap_or(u32::MAX);
                GateOutcome::Reject(Rejection(report::parse_reject(
                    path,
                    line,
                    &error.to_string(),
                )))
            }
        };
    }

    let Some(language) = parse::language(absolute_path) else {
        return GateOutcome::Skipped;
    };
    if std::str::from_utf8(post).is_err() {
        return GateOutcome::Skipped;
    }
    let Some(post_parsed) = post_parsed.filter(|parsed| parsed.lang == language) else {
        return GateOutcome::Skipped;
    };
    let pre_error = if pre.is_none() {
        false
    } else {
        let Some(pre_parsed) = pre_parsed.filter(|parsed| parsed.lang == language) else {
            return GateOutcome::Skipped;
        };
        first_syntax_error(&pre_parsed.tree).is_some()
    };
    if pre_error {
        return GateOutcome::Exempt(Some(post_parsed));
    }
    let Some(error) = first_syntax_error(&post_parsed.tree) else {
        return GateOutcome::Pass(Some(post_parsed));
    };
    let position = error.start_position();
    let line = metrics::one_based(position.row);
    let cause = if error.is_missing() {
        format!(
            "missing {} at line {}, column {}",
            error.kind(),
            line,
            position.column.saturating_add(1)
        )
    } else {
        format!(
            "syntax error at line {}, column {}",
            line,
            position.column.saturating_add(1)
        )
    };
    GateOutcome::Reject(Rejection(report::parse_reject(path, line, &cause)))
}

fn parse_json(bytes: &[u8]) -> Result<(), sonic_rs::Error> {
    sonic_rs::from_slice::<sonic_rs::Value>(bytes).map(|_| ())
}

fn first_syntax_error(tree: &Tree) -> Option<Node<'_>> {
    let mut pending = vec![tree.root_node()];
    while let Some(node) = pending.pop() {
        if node.is_error() || node.is_missing() {
            return Some(node);
        }
        for index in (0..node.child_count()).rev() {
            if let Some(child) = node.child(index) {
                pending.push(child);
            }
        }
    }
    None
}

/// A guard check identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Rule {
    /// The guard-wrap check.
    GuardWrap,
    /// The broad-handler check.
    BroadHandler,
    /// The helper check.
    Helper,
    /// The new-warning check.
    NewWarning,
    /// The commented-out-code check.
    CommentedOutCode,
    /// A stream-class check carrying its G8 rule.
    Stream(G8Rule),
}

impl Rule {
    /// Returns the rule's canonical check name.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::GuardWrap => "guard_wrap",
            Self::BroadHandler => "broad_handler",
            Self::Helper => "helper",
            Self::NewWarning => "new_warning",
            Self::CommentedOutCode => "commented_out_code",
            Self::Stream(rule) => rule.name(),
        }
    }
}

/// One located finding: rule, span, and matched line text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The rule that fired.
    pub rule: Rule,
    /// The 1-based line the finding starts on.
    pub line: u32,
    /// The 1-based line the finding ends on.
    pub line_end: u32,
    /// Whether the finding spans the whole line.
    pub whole_line: bool,
    /// The matched line text.
    pub text: Box<str>,
}

/// A file's per-rule outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// No findings.
    Clean,
    /// At least one finding.
    Findings,
    /// The file was not checked.
    Skipped,
}

/// One file's verdict, findings, and metrics.
#[derive(Debug, Clone, PartialEq)]
pub struct FileFindings {
    /// The checked path.
    pub path: Box<str>,
    /// The file's outcome.
    pub verdict: Verdict,
    /// Located findings in the file.
    pub items: Vec<Finding>,
    /// Metrics collected for the file, when measured.
    pub metrics: Option<metrics::FileMetrics>,
}

pub(super) struct Hunk {
    pub removed: Vec<Box<str>>,
    pub added: Vec<(u32, Box<str>)>,
}

pub(super) fn hunks_from_diff(hunks: &[DiffHunk]) -> Vec<Hunk> {
    hunks
        .iter()
        .map(|hunk| {
            let mut added = Vec::new();
            let mut removed = Vec::new();
            let mut post_line = hunk.new_start;
            for line in &hunk.lines {
                match line.kind {
                    DiffLineKind::Context => post_line = post_line.saturating_add(1),
                    DiffLineKind::Added => {
                        let line_number = u32::try_from(post_line).unwrap_or(u32::MAX);
                        added.push((line_number, line.text.clone()));
                        post_line = post_line.saturating_add(1);
                    }
                    DiffLineKind::Removed => removed.push(line.text.clone()),
                }
            }
            Hunk { removed, added }
        })
        .collect()
}

pub(super) fn guard_wrap(hunks: &[Hunk]) -> Option<Finding> {
    let Ok(pattern) = GUARD_WRAP.as_ref() else {
        return None;
    };
    for hunk in hunks {
        if hunk.removed.is_empty() || hunk.added.is_empty() {
            continue;
        }
        let removed: Vec<_> = hunk.removed.iter().map(|line| line.trim()).collect();
        let added: Vec<_> = hunk.added.iter().map(|(_, line)| line.trim()).collect();
        let Some(start) = added
            .windows(removed.len())
            .position(|window| window == removed.as_slice())
        else {
            continue;
        };
        let mut unmatched_chars = 0_usize;
        for removed_line in &removed {
            if !added.iter().any(|added_line| added_line == removed_line) {
                unmatched_chars = unmatched_chars.saturating_add(code_char_count(removed_line));
            }
        }
        if unmatched_chars >= 5 {
            continue;
        }
        let end = start.saturating_add(removed.len());
        let wrapped = added
            .iter()
            .enumerate()
            .filter(|(index, _)| *index < start || *index >= end)
            .any(|(_, line)| pattern.is_match(line));
        if !wrapped {
            continue;
        }
        let line = hunk.added.first()?.0;
        return Some(Finding {
            rule: Rule::GuardWrap,
            line,
            line_end: line,
            whole_line: false,
            text: report::GUARD_WRAP_NOTICE.into(),
        });
    }
    None
}

fn code_char_count(line: &str) -> usize {
    let end = ["//", "/*", "#", "--", "(*"]
        .iter()
        .filter_map(|marker| line.find(marker))
        .min()
        .unwrap_or(line.len());
    line[..end]
        .chars()
        .filter(|character| !character.is_whitespace())
        .count()
}

pub(super) fn broad_handler(language: Language, hunks: &[Hunk], path: &str) -> Option<Finding> {
    let patterns = match language {
        Language::Ocaml => Some(&BROAD_OCAML),
        Language::Rust => Some(&BROAD_RUST),
        Language::JavaScript | Language::TypeScript | Language::Tsx => Some(&BROAD_JS),
        Language::Python => Some(&BROAD_PYTHON),
        Language::Cpp => Some(&BROAD_CPP),
        Language::Go | Language::C | Language::OcamlInterface => None,
    }?;
    let Ok(pattern) = patterns.as_ref() else {
        return None;
    };
    hunks
        .iter()
        .flat_map(|hunk| &hunk.added)
        .find(|(_, line)| pattern.is_match(line))
        .map(|(line, _)| Finding {
            rule: Rule::BroadHandler,
            line: *line,
            line_end: *line,
            whole_line: false,
            text: report::broad_handler_notice(path, *line).into(),
        })
}

pub(super) fn helper(
    language: Language,
    path: &str,
    pre: Option<&metrics::FileMetrics>,
    post: &Parsed,
    source: &[u8],
    post_metrics: &metrics::FileMetrics,
) -> Vec<Finding> {
    if path_has_test_component(path) {
        return Vec::new();
    }
    let kinds = metrics::table(language);
    let root = post.tree.root_node();
    let mut findings = Vec::new();
    for index in 0..root.child_count() {
        let Some(node) = root.child(index) else {
            continue;
        };
        let Some(name) = metrics::function_name(language, kinds, node, source) else {
            continue;
        };
        let Some(function) = post_metrics.functions.iter().find(|function| {
            function.name == name
                && function.start_line == metrics::one_based(node.start_position().row)
        }) else {
            continue;
        };
        if pre.is_some_and(|before| metrics::match_function(before, function).is_some())
            || function.end_line.saturating_sub(function.start_line) >= 5
        {
            continue;
        }
        let Some(name_node) = metrics::function_name_node(language, node) else {
            continue;
        };
        let Some(body) = node.child_by_field_name("body") else {
            continue;
        };
        if contains_identifier(body, name.as_ref(), kinds, source) {
            continue;
        }
        let mut occurrence = None;
        let mut occurrences = 0_usize;
        let mut pending = vec![root];
        while let Some(current) = pending.pop() {
            if kinds.identifiers.contains(&current.kind())
                && node_text(current, source) == name.as_ref()
                && current.id() != name_node.id()
            {
                occurrences = occurrences.saturating_add(1);
                occurrence = Some(current);
                if occurrences > 1 {
                    break;
                }
            }
            for child_index in (0..current.child_count()).rev() {
                if let Some(child) = current.child(child_index) {
                    pending.push(child);
                }
            }
        }
        let Some(identifier) = occurrence.filter(|_| occurrences == 1) else {
            continue;
        };
        if !is_call_callee(identifier, root, kinds) {
            continue;
        }
        findings.push(Finding {
            rule: Rule::Helper,
            line: function.start_line,
            line_end: function.end_line,
            whole_line: false,
            text: report::HELPER_NOTICE.into(),
        });
    }
    findings
}

fn contains_identifier(node: Node<'_>, name: &str, kinds: &metrics::Table, source: &[u8]) -> bool {
    let mut pending = vec![node];
    while let Some(current) = pending.pop() {
        if kinds.identifiers.contains(&current.kind()) && node_text(current, source) == name {
            return true;
        }
        for index in (0..current.child_count()).rev() {
            if let Some(child) = current.child(index) {
                pending.push(child);
            }
        }
    }
    false
}

fn is_call_callee(identifier: Node<'_>, root: Node<'_>, kinds: &metrics::Table) -> bool {
    let mut pending = vec![root];
    while let Some(current) = pending.pop() {
        if kinds.calls.contains(&current.kind()) {
            let callee = current
                .child_by_field_name("function")
                .or_else(|| current.child(0));
            if callee.is_some_and(|callee| contains_node(callee, identifier.id())) {
                return true;
            }
        }
        for index in (0..current.child_count()).rev() {
            if let Some(child) = current.child(index) {
                pending.push(child);
            }
        }
    }
    false
}

fn contains_node(root: Node<'_>, id: usize) -> bool {
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if node.id() == id {
            return true;
        }
        for index in (0..node.child_count()).rev() {
            if let Some(child) = node.child(index) {
                pending.push(child);
            }
        }
    }
    false
}

fn path_has_test_component(path: &str) -> bool {
    Path::new(path).components().any(|component| {
        let Component::Normal(name) = component else {
            return false;
        };
        matches!(name.to_str(), Some("test" | "tests"))
    })
}

pub(super) fn commented_out(
    language: Language,
    parsed: &Parsed,
    source: &[u8],
    added_lines: &BTreeSet<u32>,
) -> Vec<Finding> {
    let Some(set) = comment_pattern(language) else {
        return Vec::new();
    };
    let Ok(pattern) = set.as_ref() else {
        return Vec::new();
    };
    let kinds = metrics::table(language);
    let mut comments = Vec::new();
    let mut pending = vec![parsed.tree.root_node()];
    while let Some(node) = pending.pop() {
        if kinds.comments.contains(&node.kind()) {
            let line = metrics::one_based(node.start_position().row);
            if added_lines.contains(&line) {
                let text = node_text(node, source);
                if pattern.is_match(text) {
                    let line_end = metrics::one_based(node.end_position().row);
                    let source_line = source_line(source, line);
                    comments.push(Finding {
                        rule: Rule::CommentedOutCode,
                        line,
                        line_end,
                        whole_line: source_line.trim() == text.trim(),
                        text: text.into(),
                    });
                }
            }
        }
        for index in (0..node.child_count()).rev() {
            if let Some(child) = node.child(index) {
                pending.push(child);
            }
        }
    }
    comments
}

fn comment_pattern(language: Language) -> Option<&'static LazyLock<Result<Regex, regex::Error>>> {
    match language {
        Language::Ocaml => Some(&COMMENT_OCAML),
        Language::OcamlInterface => None,
        Language::Python => Some(&COMMENT_PYTHON),
        Language::JavaScript | Language::TypeScript | Language::Tsx => Some(&COMMENT_JS),
        Language::Go => Some(&COMMENT_GO),
        Language::C | Language::Cpp => Some(&COMMENT_C),
        Language::Rust => Some(&COMMENT_RUST),
    }
}

fn source_line(source: &[u8], line: u32) -> &str {
    let Ok(index) = usize::try_from(line.saturating_sub(1)) else {
        return "";
    };
    std::str::from_utf8(source)
        .ok()
        .and_then(|text| text.lines().nth(index))
        .unwrap_or_default()
}

pub(super) fn bypassed(rule: Rule, source: &str, line: u32) -> bool {
    let allow = format!("guard-allow({})", rule.name());
    if source_line(source.as_bytes(), line).contains(&allow) {
        return true;
    }
    let disable = format!("guard-disable-file({})", rule.name());
    source
        .lines()
        .take(20)
        .any(|line| line.contains(&disable) || line.contains("guard-disable-file(all)"))
}

fn node_text<'source>(node: Node<'_>, source: &'source [u8]) -> &'source str {
    std::str::from_utf8(source.get(node.byte_range()).unwrap_or_default()).unwrap_or_default()
}
