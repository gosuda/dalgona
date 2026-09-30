use std::path::PathBuf;

use std::fmt::Write as _;

use proptest::prelude::*;

use super::scan::split;
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
    match parse_rulefile("r", origin(), text.as_bytes()) {
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
    let (rule, notes) = parse_rulefile("r", origin(), b"---\nquestion: q\n---\nbody")
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
    let problems = parse_rulefile("bad name", origin(), b"---\nx: \"a\n---\nb")
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
        parse_rulefile("r", origin(), b"\xff")
            .err()
            .map(|p| p[0].kind),
        Some(ProblemKind::File)
    );
}

#[test]
fn valid_rule_fields_and_defaults() {
    let text = "---\ndescription: d\ncondition: '\\bTODO\\b'\nrepeat-mode: after-gap\nrepeatGap: 3\ninterrupt-mode: tool-only\nreport: true\n---\nBody\n";
    let (rule, notes) =
        parse_rulefile("guard.todo", origin(), text.as_bytes()).unwrap_or_else(|p| panic!("{p:?}"));
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
    let (bare, _) =
        parse_rulefile("r", origin(), b"\r\n just body \r\n").unwrap_or_else(|p| panic!("{p:?}"));
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
