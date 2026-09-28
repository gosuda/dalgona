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
    /// Additional condition-derived scope tokens, such as glob shorthand.
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
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        ScopeInput, ScopeValue, admits_tool, glob_candidates, normalize_path, parse_scope,
        reaches_output,
    };
    use crate::ttsr::value::{Origin, Problem, ProblemKind, ScopeSpec, Severity, ToolScope};

    fn origin() -> Origin {
        Origin::User(PathBuf::from("/rules/scope.md"))
    }

    fn input<'a>(
        origin: &'a Origin,
        value: ScopeValue<'a>,
        known_tools: Option<&'a [&'a str]>,
        extra_tokens: &'a [String],
    ) -> ScopeInput<'a> {
        ScopeInput {
            origin,
            value,
            known_tools,
            extra_tokens,
        }
    }

    #[test]
    fn nested_punctuation_and_quotes_stay_inside_scope_tokens() {
        let source = origin();
        let known = ["patch", "exec", "read"];
        let extras = [];
        let values = [
            "tool:patch(*.{rs,mli}), text".to_owned(),
            "'tool:exec(a,b)'".to_owned(),
            "tool:read([a,b].txt)".to_owned(),
        ];

        let (scope, problems) = parse_scope(input(
            &source,
            ScopeValue::List(&values),
            Some(&known),
            &extras,
        ));

        assert!(problems.is_empty());
        let ToolScope::Tools(tools) = scope.tools else {
            panic!("expected named tool scopes");
        };
        assert_eq!(tools.len(), 3);
        assert_eq!(tools[0].tool.as_ref(), "patch");
        assert_eq!(tools[1].tool.as_ref(), "exec");
        assert_eq!(tools[2].tool.as_ref(), "read");
        assert!(
            tools[2]
                .glob
                .as_ref()
                .is_some_and(|glob| glob.is_match("a.txt"))
        );
        assert!(
            tools[0]
                .glob
                .as_ref()
                .is_some_and(|glob| glob.is_match("module.mli"))
        );
        assert!(
            tools[1]
                .glob
                .as_ref()
                .is_some_and(|glob| glob.is_match("a,b"))
        );
        assert!(scope.text);
    }

    #[test]
    fn quotes_keep_embedded_commas_inside_one_token() {
        let source = origin();
        let extras = [];
        let (scope, problems) = parse_scope(input(
            &source,
            ScopeValue::String("\"text,thinking\""),
            None,
            &extras,
        ));

        assert!(!reaches_output(&scope));
        assert_eq!(
            problems,
            vec![Problem {
                origin: source,
                kind: ProblemKind::Scope,
                reason: "scope token \"text,thinking\" is invalid".into(),
                consequence: "dalgon skipped this token.".into(),
                severity: Severity::Skipped,
            }]
        );
    }

    #[test]
    fn whitespace_inside_quotes_remains_part_of_the_token() {
        let source = origin();
        let extras = [];
        let (scope, problems) = parse_scope(input(
            &source,
            ScopeValue::String("\" text \""),
            None,
            &extras,
        ));

        assert!(!reaches_output(&scope));
        assert_eq!(
            problems,
            vec![Problem {
                origin: source,
                kind: ProblemKind::Scope,
                reason: "scope token \" text \" is invalid".into(),
                consequence: "dalgon skipped this token.".into(),
                severity: Severity::Skipped,
            }]
        );
    }

    #[test]
    fn omitted_scope_defaults_but_explicit_empty_list_does_not() {
        let source = origin();
        let extras = [];
        let (default, default_problems) =
            parse_scope(input(&source, ScopeValue::Missing, None, &extras));
        let (empty, empty_problems) =
            parse_scope(input(&source, ScopeValue::List(&[]), None, &extras));

        assert!(default_problems.is_empty() && empty_problems.is_empty());
        assert!(default.text);
        assert!(matches!(default.tools, ToolScope::All));
        assert!(!reaches_output(&empty));
        assert_eq!(
            empty,
            ScopeSpec {
                text: false,
                thinking: false,
                tools: ToolScope::Tools(Vec::new()),
            }
        );
    }

    #[test]
    fn core_scope_maps_text_thinking_and_named_tools() {
        let source = origin();
        let known = ["patch"];
        let extras = [];
        let Ok(name) = dal_core::ext::Name::parse("patch") else {
            panic!("the test tool name should be valid");
        };
        let core = dal_core::ext::Scope {
            text: true,
            thinking: true,
            tool: true,
            named_tools: vec![name],
        };
        let (scope, problems) = parse_scope(input(
            &source,
            ScopeValue::Core(&core),
            Some(&known),
            &extras,
        ));

        assert!(problems.is_empty());
        assert!(scope.text && scope.thinking);
        let ToolScope::Tools(tools) = scope.tools else {
            panic!("expected the named core tool scope");
        };
        assert_eq!(tools.len(), 1);
        assert_eq!(tools.first().map(|tool| tool.tool.as_ref()), Some("patch"));
    }

    #[test]
    fn condition_scope_tokens_replace_the_missing_scope_default() {
        let source = origin();
        let known = ["patch"];
        let extras = ["tool:patch(src/*.rs)".to_owned()];
        let (scope, problems) =
            parse_scope(input(&source, ScopeValue::Missing, Some(&known), &extras));

        assert!(problems.is_empty());
        assert!(!scope.text);
        assert!(matches!(scope.tools, ToolScope::Tools(_)));
    }

    #[test]
    fn malformed_scope_token_is_skipped_with_its_origin() {
        let source = origin();
        let extras = [];
        let (scope, problems) = parse_scope(input(
            &source,
            ScopeValue::String("mod:patch"),
            None,
            &extras,
        ));

        assert!(!reaches_output(&scope));
        assert_eq!(
            problems,
            vec![Problem {
                origin: source,
                kind: ProblemKind::Scope,
                reason: "scope token \"mod:patch\" is invalid".into(),
                consequence: "dalgon skipped this token.".into(),
                severity: Severity::Skipped,
            }]
        );
    }

    #[test]
    fn unmatched_openers_do_not_swallow_later_scope_tokens() {
        let source = origin();
        let extras = [];
        let (scope, problems) = parse_scope(input(
            &source,
            ScopeValue::String("mod:patch(, text"),
            None,
            &extras,
        ));

        assert!(scope.text);
        assert_eq!(
            problems,
            vec![Problem {
                origin: source,
                kind: ProblemKind::Scope,
                reason: "scope token \"mod:patch(\" is invalid".into(),
                consequence: "dalgon skipped this token.".into(),
                severity: Severity::Skipped,
            }]
        );
    }

    #[test]
    fn duplicate_and_empty_scope_tokens_are_discarded() {
        let source = origin();
        let known = ["patch"];
        let extras = [];
        let (scope, problems) = parse_scope(input(
            &source,
            ScopeValue::String(", TEXT, text, PATCH, tool:patch, ,"),
            Some(&known),
            &extras,
        ));

        assert!(problems.is_empty());
        assert!(scope.text);
        let ToolScope::Tools(tools) = scope.tools else {
            panic!("expected a named tool scope");
        };
        assert_eq!(tools.len(), 1);
        assert_eq!(tools.first().map(|tool| tool.tool.as_ref()), Some("patch"));
    }

    #[test]
    fn duplicate_compiled_globs_deduplicate_by_original_pattern() {
        let source = origin();
        let known = ["patch"];
        let extras = [];
        let (scope, problems) = parse_scope(input(
            &source,
            ScopeValue::String("tool:patch(*.rs), PATCH(*.rs)"),
            Some(&known),
            &extras,
        ));

        assert!(problems.is_empty());
        let ToolScope::Tools(patterns) = scope.tools else {
            panic!("expected a named tool scope");
        };
        assert_eq!(patterns.len(), 1);
        assert_eq!(
            patterns
                .first()
                .and_then(|pattern| pattern.glob.as_ref())
                .map(|glob| glob.glob().glob()),
            Some("*.rs")
        );
    }

    #[test]
    fn duplicate_invalid_path_globs_are_reported_once() {
        let source = origin();
        let known = ["patch"];
        let extras = [];
        let (scope, problems) = parse_scope(input(
            &source,
            ScopeValue::String("tool:patch([z-a]), PATCH([z-a])"),
            Some(&known),
            &extras,
        ));

        assert!(!reaches_output(&scope));
        assert_eq!(
            problems,
            vec![Problem {
                origin: source,
                kind: ProblemKind::Glob,
                reason: "glob \"[z-a]\" is invalid".into(),
                consequence: "dalgon skipped it.".into(),
                severity: Severity::Skipped,
            }]
        );
    }

    #[test]
    fn case_insensitive_duplicate_invalid_tokens_are_reported_once() {
        let source = origin();
        let extras = [];
        let (scope, problems) = parse_scope(input(
            &source,
            ScopeValue::String("mod:patch, MOD:PATCH"),
            None,
            &extras,
        ));

        assert!(!reaches_output(&scope));
        assert_eq!(
            problems,
            vec![Problem {
                origin: source,
                kind: ProblemKind::Scope,
                reason: "scope token \"mod:patch\" is invalid".into(),
                consequence: "dalgon skipped this token.".into(),
                severity: Severity::Skipped,
            }]
        );
    }

    #[test]
    fn unknown_tool_is_not_admitted_and_gets_the_exact_source_note() {
        let source = origin();
        let known = ["patch", "exec"];
        let extras = [];
        let (scope, problems) = parse_scope(input(
            &source,
            ScopeValue::String("tool:Ghost(*.rs)"),
            Some(&known),
            &extras,
        ));

        let ToolScope::Tools(patterns) = &scope.tools else {
            panic!("expected the unavailable tool entry to be preserved");
        };
        assert_eq!(patterns.len(), 1);
        let Some(pattern) = patterns.first() else {
            panic!("the unavailable tool entry should be retained");
        };
        assert_eq!(pattern.tool.as_ref(), "ghost");
        assert!(!pattern.available);
        assert!(!reaches_output(&scope));
        assert!(!admits_tool(
            &scope,
            "ghost",
            Some("x.rs"),
            Path::new("/work")
        ));
        assert_eq!(
            problems,
            vec![Problem {
                origin: source,
                kind: ProblemKind::SetNote,
                reason: "the scope names the tool \"ghost\", which dalgon does not have".into(),
                consequence: String::new(),
                severity: Severity::Note,
            }]
        );
    }

    #[test]
    fn glob_star_does_not_cross_a_path_separator() {
        let source = origin();
        let known = ["patch"];
        let extras = [];
        let (scope, problems) = parse_scope(input(
            &source,
            ScopeValue::String("tool:patch(src/*.rs)"),
            Some(&known),
            &extras,
        ));

        assert!(problems.is_empty());
        assert!(!admits_tool(
            &scope,
            "patch",
            Some("src/nested/file.rs"),
            Path::new("/work")
        ));
    }

    #[test]
    fn recursive_glob_matches_across_path_separators() {
        let source = origin();
        let known = ["patch"];
        let extras = [];
        let (scope, problems) = parse_scope(input(
            &source,
            ScopeValue::String("tool:patch(src/**/*.rs)"),
            Some(&known),
            &extras,
        ));

        assert!(problems.is_empty());
        assert!(admits_tool(
            &scope,
            "PATCH",
            Some("src/nested/file.rs"),
            Path::new("/work")
        ));
    }

    #[test]
    fn normalizes_windows_separators_and_leading_dot_segments() {
        assert_eq!(normalize_path(".\\.\\src\\lib.rs"), "src/lib.rs");
    }

    #[test]
    fn glob_candidates_include_full_root_relative_and_basename_paths() {
        assert_eq!(
            glob_candidates("/work/src/lib.rs", Path::new("/work/")),
            [
                "/work/src/lib.rs".to_owned(),
                "src/lib.rs".to_owned(),
                "lib.rs".to_owned(),
            ]
        );
    }
}
