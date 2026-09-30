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
    let (empty, empty_problems) = parse_scope(input(&source, ScopeValue::List(&[]), None, &extras));

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
    let (scope, problems) = parse_scope(input(&source, ScopeValue::Missing, Some(&known), &extras));

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
