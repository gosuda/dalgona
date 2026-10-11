//! Scope token parsing and tool-path matching for TTSR rules.

use std::collections::HashSet;
use std::path::Path;

use dal_core::ext::Scope;
use globset::{Candidate, Glob, GlobBuilder, GlobMatcher, GlobSet, GlobSetBuilder};

use super::value::{Origin, Problem, ProblemKind, ScopeSpec, Severity, ToolPat, ToolScope};

/// The value supplied to a scope parser.
#[derive(Clone, Copy, Debug)]
pub struct ScopeInput<'a> {
    /// Origin attached to any scope problem.
    pub origin: &'a Origin,
    /// The explicit setting, or [`ScopeValue::Missing`] when no key was supplied.
    pub value: ScopeValue<'a>,
    /// The product's tool names, when its inventory is available.
    ///
    /// `None` means the inventory is not available yet; an empty slice means
    /// the product is known to expose no tools.
    ///
    /// A named tool absent from a supplied inventory remains in the scope
    /// and produces a Note problem.
    pub known_tools: Option<&'a [&'a str]>,
    /// Additional condition-derived tokens, such as file glob shorthand.
    pub extra_tokens: &'a [String],
}

/// A scope setting before it is interpreted by the TTSR grammar.
#[derive(Clone, Copy, Debug)]
pub enum ScopeValue<'a> {
    /// No scope key was supplied.
    Missing,
    /// One comma-separated scope string.
    String(&'a str),
    /// Scope strings whose items each use the same comma splitting rules.
    List(&'a [String]),
    /// A typed scope from a `dal.rule` record.
    Core(&'a Scope),
}

/// Parses a scope setting and returns its diagnostics with the input origin.
///
/// Missing scope defaults to assistant text and every tool only when no
/// condition-derived scope token was supplied. An explicit empty or invalid
/// setting remains an empty scope.
#[must_use]
pub fn parse_scope(input: ScopeInput<'_>) -> (ScopeSpec, Vec<Problem>) {
    let tokens = scope_tokens(&input);
    if matches!(input.value, ScopeValue::Missing) && tokens.is_empty() {
        return (scope_default(), Vec::new());
    }

    let mut parsed = ScopeAccumulator::default();
    if let ScopeValue::Core(scope) = input.value {
        parsed.add_core(scope, input);
    }
    for token in tokens {
        parsed.add_token(token, input);
    }
    parsed.finish()
}
/// Applies the loaded product tool inventory after scope parsing.
///
/// File parsing has no product context; the set builder supplies the
/// inventory here and retains each unknown scope token as an unavailable
/// tool with one Note.
pub(crate) fn apply_tool_inventory(
    scope: &mut ScopeSpec,
    known_tools: &[&str],
    origin: &Origin,
) -> Vec<Problem> {
    let ToolScope::Tools(tools) = &mut scope.tools else {
        return Vec::new();
    };

    let mut problems = Vec::new();
    let mut seen_unknown = HashSet::new();
    for tool in tools {
        if tool.tool.as_ref() == "*" {
            tool.available = true;
            continue;
        }
        tool.available = known_tools
            .iter()
            .any(|known| known.eq_ignore_ascii_case(&tool.tool));
        if !tool.available && seen_unknown.insert(tool.tool.as_ref()) {
            problems.push(unknown_tool_problem(origin, &tool.tool));
        }
    }
    problems
}

#[derive(Default)]
struct ScopeAccumulator {
    text: bool,
    thinking: bool,
    all_tools: bool,
    tools: Vec<ToolPat>,
    problems: Vec<Problem>,
    seen_unknown: HashSet<String>,
    seen_invalid: HashSet<String>,
    seen_invalid_globs: HashSet<(String, String)>,
}

impl ScopeAccumulator {
    fn add_core(&mut self, scope: &Scope, input: ScopeInput<'_>) {
        self.text = scope.text;
        self.thinking = scope.thinking;
        if !scope.tool {
            return;
        }
        if scope.named_tools.is_empty() {
            self.all_tools = true;
            return;
        }
        for name in &scope.named_tools {
            self.add_tool(name.as_str(), None, input);
        }
    }

    fn add_token(&mut self, token: &str, input: ScopeInput<'_>) {
        if token.eq_ignore_ascii_case("text") {
            self.text = true;
            return;
        }
        if token.eq_ignore_ascii_case("thinking") {
            self.thinking = true;
            return;
        }
        if token.eq_ignore_ascii_case("tool") || token.eq_ignore_ascii_case("toolcall") {
            self.all_tools = true;
            return;
        }

        let Some((name, path_glob)) = parse_tool_token(token) else {
            if self.seen_invalid.insert(token.to_ascii_lowercase()) {
                self.problems
                    .push(invalid_scope_problem(input.origin, token));
            }
            return;
        };
        let glob = if let Some(pattern) = path_glob {
            let Ok(glob) = scope_glob(pattern) else {
                let tool = name.unwrap_or("*").to_ascii_lowercase();
                if self.seen_invalid_globs.insert((tool, pattern.to_owned())) {
                    self.problems
                        .push(invalid_glob_problem(input.origin, pattern));
                }
                return;
            };
            Some(glob.compile_matcher())
        } else {
            None
        };
        self.add_tool(name.unwrap_or("*"), glob, input);
    }

    fn add_tool(&mut self, name: &str, glob: Option<GlobMatcher>, input: ScopeInput<'_>) {
        let tool = name.to_ascii_lowercase();
        let unknown = name != "*"
            && input.known_tools.is_some_and(|known| {
                !known
                    .iter()
                    .any(|known_tool| known_tool.eq_ignore_ascii_case(&tool))
            });
        if unknown && self.seen_unknown.insert(tool.clone()) {
            self.problems
                .push(unknown_tool_problem(input.origin, &tool));
        }

        let pattern = ToolPat {
            tool: tool.into_boxed_str(),
            available: !unknown,
            glob,
        };
        if !self.tools.contains(&pattern) {
            self.tools.push(pattern);
        }
    }

    fn finish(self) -> (ScopeSpec, Vec<Problem>) {
        (
            ScopeSpec {
                text: self.text,
                thinking: self.thinking,
                tools: if self.all_tools {
                    ToolScope::All
                } else {
                    ToolScope::Tools(self.tools)
                },
            },
            self.problems,
        )
    }
}

fn scope_tokens<'a>(input: &ScopeInput<'a>) -> Vec<&'a str> {
    let mut raw_tokens = Vec::new();
    match input.value {
        ScopeValue::Missing | ScopeValue::Core(_) => {}
        ScopeValue::String(value) => split_scope_tokens(value, &mut raw_tokens),
        ScopeValue::List(items) => {
            for item in items {
                split_scope_tokens(item, &mut raw_tokens);
            }
        }
    }
    for token in input.extra_tokens {
        split_scope_tokens(token, &mut raw_tokens);
    }

    raw_tokens
        .into_iter()
        .map(unquote)
        .filter(|token| !token.is_empty())
        .collect()
}

fn scope_default() -> ScopeSpec {
    ScopeSpec {
        text: true,
        thinking: false,
        tools: ToolScope::All,
    }
}

/// Reports whether a scope reaches text, thinking, or at least one tool.
#[must_use]
pub fn reaches_output(scope: &ScopeSpec) -> bool {
    scope.reaches_any()
}

/// Reports whether a tool call is admitted by a scope and optional path glob.
///
/// A path-restricted tool scope does not admit a call until its path is known.
#[must_use]
pub fn admits_tool(scope: &ScopeSpec, tool: &str, path: Option<&str>, ws_root: &Path) -> bool {
    let ToolScope::Tools(patterns) = &scope.tools else {
        return true;
    };
    let matches_tool = |pattern: &ToolPat| {
        pattern.available
            && (pattern.tool.as_ref() == "*" || pattern.tool.eq_ignore_ascii_case(tool))
    };
    let mut has_matching_glob = false;
    for pattern in patterns {
        if !matches_tool(pattern) {
            continue;
        }
        if pattern.glob.is_none() {
            return true;
        }
        has_matching_glob = true;
    }
    if !has_matching_glob {
        return false;
    }
    let Some(path) = path else {
        return false;
    };
    let normalized = normalize_path(path);
    let paths = glob_candidates(&normalized, ws_root);
    let candidates: [Candidate<'_>; 3] =
        std::array::from_fn(|index| Candidate::new(paths[index].as_str()));
    for pattern in patterns {
        if !matches_tool(pattern) {
            continue;
        }
        let Some(glob) = &pattern.glob else {
            continue;
        };
        if candidates
            .iter()
            .any(|candidate| glob.is_match_candidate(candidate))
        {
            return true;
        }
    }
    false
}

/// Normalizes tool paths by replacing backslashes and removing leading `./`.
#[must_use]
pub fn normalize_path(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let mut path = normalized.as_str();
    while let Some(rest) = path.strip_prefix("./") {
        path = rest;
    }
    path.to_owned()
}

/// Returns the normalized full path, workspace-relative path, and basename.
#[must_use]
pub fn glob_candidates(normalized: &str, ws_root: &Path) -> [String; 3] {
    let mut root = normalize_path(ws_root.to_string_lossy().as_ref());
    while root.len() > 1 && root.ends_with('/') {
        root.pop();
    }
    let after_root = if normalized == root {
        ""
    } else if root == "/" {
        normalized.strip_prefix('/').unwrap_or(normalized)
    } else if let Some(remainder) = normalized.strip_prefix(&root) {
        remainder
            .strip_prefix('/')
            .map_or(normalized, |relative| relative)
    } else {
        normalized
    };
    let basename = normalized.rsplit('/').next().unwrap_or_default();
    [
        normalized.to_owned(),
        after_root.to_owned(),
        basename.to_owned(),
    ]
}

/// Compiles path or agent glob patterns with the shared TTSR glob settings.
///
/// Invalid individual patterns are returned as Skipped problems. A non-empty
/// input whose patterns are all invalid yields an empty set, which matches no
/// path; an empty input yields `None` and therefore adds no restriction.
///
/// # Errors
/// Returns an error if globset cannot assemble the individually valid patterns.
pub(crate) fn compile_globs(
    patterns: &[String],
    origin: &Origin,
) -> Result<(Option<GlobSet>, Vec<Problem>), globset::Error> {
    if patterns.is_empty() {
        return Ok((None, Vec::new()));
    }

    let mut builder = GlobSetBuilder::new();
    let mut kept = 0;
    let mut problems = Vec::new();
    for pattern in patterns {
        match scope_glob(pattern) {
            Ok(glob) => {
                builder.add(glob);
                kept += 1;
            }
            Err(()) => problems.push(invalid_glob_problem(origin, pattern)),
        }
    }
    if kept == 0 {
        return Ok((Some(GlobSet::empty()), problems));
    }
    builder.build().map(|set| (Some(set), problems))
}

fn split_scope_tokens<'a>(input: &'a str, tokens: &mut Vec<&'a str>) {
    let mut shielded = vec![0_i32; input.len() + 1];
    let mut quote: Option<(char, usize)> = None;
    let mut escaped = false;
    let mut delimiters = Vec::new();

    for (offset, character) in input.char_indices() {
        if let Some((quote_character, quote_start)) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == quote_character {
                mark_balanced_span(&mut shielded, quote_start, offset);
                quote = None;
            }
            continue;
        }

        match character {
            '\'' | '"' => quote = Some((character, offset)),
            '(' | '[' | '{' => delimiters.push((character, offset)),
            ')' | ']' | '}' => {
                let Some((opening, open_offset)) = delimiters.last().copied() else {
                    continue;
                };
                if !matches!((opening, character), ('(', ')') | ('[', ']') | ('{', '}')) {
                    continue;
                }
                delimiters.pop();
                mark_balanced_span(&mut shielded, open_offset, offset);
            }
            _ => {}
        }
    }

    let mut start = 0;
    let mut depth = 0_i32;
    for (offset, character) in input.char_indices() {
        depth += shielded[offset];
        if character == ',' && depth == 0 {
            tokens.push(input[start..offset].trim());
            start = offset + character.len_utf8();
        }
    }
    tokens.push(input[start..].trim());
}

fn mark_balanced_span(shielded: &mut [i32], open: usize, close: usize) {
    shielded[open + 1] += 1;
    shielded[close] -= 1;
}

fn unquote(token: &str) -> &str {
    let token = token.trim();
    let bytes = token.as_bytes();
    if bytes.len() >= 2 && matches!(bytes[0], b'\'' | b'"') && bytes[0] == bytes[bytes.len() - 1] {
        &token[1..token.len() - 1]
    } else {
        token
    }
}

fn parse_tool_token(token: &str) -> Option<(Option<&str>, Option<&str>)> {
    let (prefix, path_glob) = if let Some(open) = token.find('(') {
        if !token.ends_with(')') {
            return None;
        }
        let path_glob = &token[open + 1..token.len() - 1];
        if path_glob.is_empty() || path_glob.contains(')') {
            return None;
        }
        (&token[..open], Some(path_glob))
    } else if token.contains(')') {
        return None;
    } else {
        (token, None)
    };

    if let Some((qualifier, name)) = prefix.split_once(':') {
        if !qualifier.eq_ignore_ascii_case("tool") || !valid_tool_name(name) {
            return None;
        }
        Some((Some(name), path_glob))
    } else if prefix.eq_ignore_ascii_case("tool") {
        Some((None, path_glob))
    } else if valid_tool_name(prefix) {
        Some((Some(prefix), path_glob))
    } else {
        None
    }
}

fn valid_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn scope_glob(pattern: &str) -> Result<Glob, ()> {
    let mut builder = GlobBuilder::new(pattern);
    builder.literal_separator(true);
    builder.build().map_err(|_| ())
}

fn invalid_scope_problem(origin: &Origin, token: &str) -> Problem {
    Problem {
        origin: origin.clone(),
        kind: ProblemKind::Scope,
        reason: format!("scope token \"{token}\" is invalid"),
        consequence: "dalgon skipped this token.".into(),
        severity: Severity::Skipped,
    }
}

fn invalid_glob_problem(origin: &Origin, pattern: &str) -> Problem {
    Problem {
        origin: origin.clone(),
        kind: ProblemKind::Glob,
        reason: format!("glob \"{pattern}\" is invalid"),
        consequence: "dalgon skipped it.".into(),
        severity: Severity::Skipped,
    }
}

fn unknown_tool_problem(origin: &Origin, tool: &str) -> Problem {
    Problem {
        origin: origin.clone(),
        kind: ProblemKind::SetNote,
        reason: format!("the scope names the tool \"{tool}\", which dalgon does not have"),
        consequence: String::new(),
        severity: Severity::Note,
    }
}

#[cfg(test)]
mod tests;
