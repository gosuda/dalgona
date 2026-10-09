//! The `dal.rule` record validation boundary.
//!
//! A record is checked once, in a fixed order, and the first failed check is
//! the registration error `rule <name>: <reason>`; the Starlark adapter adds
//! `path:line:col`. Regular expressions are not compiled here: a malformed
//! pattern becomes a Skipped problem at set build, not a plugin-load failure.
//! A record has no description and cannot enter the Rulebook bucket; a record
//! without a usable condition and without `always_apply` is dropped at build.
//!
//! `RuleRecord` fixes the Starlark types of its body, flags, enums, and scope.
//! The adapter validates those inputs before constructing a record. The judge
//! question carries no question kind here, so the adapter rejects non-boolean
//! questions before building the `RuleRecord`.

use std::fmt;

use dal_core::ext::{RuleRecord, Site};

use super::matcher::split_shorthand;
use super::scope::{ScopeInput, ScopeValue, compile_globs, parse_scope};
use super::value::{
    CONDITION_MAX_BYTES, ConditionSource, Name, Origin, Problem, ProblemKind, Rule, Severity,
};

/// Most patterns of one record.
const PATTERNS_MAX: usize = 16;

/// Largest `repeat_gap`.
const REPEAT_GAP_MAX: u16 = 1000;

/// A rejected `dal.rule` record; displays `rule <name>: <reason>`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordError(pub String);

impl RecordError {
    fn new(name: &str, reason: &str) -> Self {
        Self(format!("rule {name}: {reason}"))
    }

    /// Returns the problem-table reason without the `rule <name>: ` prefix.
    ///
    /// A rule name holds no `:`, so the first `": "` ends the prefix.
    #[must_use]
    pub fn reason(&self) -> &str {
        self.0
            .split_once(": ")
            .map_or(self.0.as_str(), |(_, reason)| reason)
    }
}

impl fmt::Display for RecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for RecordError {}

/// One `dal.rule` record with the identity of the plugin that registered it.
///
/// The Starlark adapter builds this at the call, validates it with
/// [`validate_record`], and publishes it; the set build reads the same value.
/// The plugin is given explicitly because a declaration file may sit in a
/// nested module directory, so `site.path` does not name the plugin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordSource {
    /// The registering plugin's checked name.
    pub plugin: dal_core::ext::Name,
    /// The `dal.rule` call site; the adapter prefixes it as `path:line:col`
    /// to a registration error.
    pub site: Site,
    /// The record as registered.
    pub record: RuleRecord,
}

/// Checks one record and maps it to a rule with origin
/// [`Origin::Record`] for its plugin; serves registration and set build.
///
/// Checks run in this order: name grammar; 1 to 16 patterns, each at most
/// 1024 bytes; a non-blank `text`; and `repeat_gap` in `1..=1000`. The same
/// glob shorthand conversion is used for file conditions and record patterns.
/// On success the problems are the Skipped glob and scope remarks that do not
/// reject the record, plus notes for unknown tools.
///
/// `known_tools` is the product's tool inventory: registration passes `None`
/// before the inventory exists, and the set build passes `Some`, so a named
/// tool absent from it stays in the scope as unavailable with one Note.
///
/// # Errors
/// Returns `rule <name>: <reason>` with the problem-table reason of the
/// first failed check.
pub fn validate_record(
    source: &RecordSource,
    known_tools: Option<&[&str]>,
) -> Result<(Rule, Vec<Problem>), RecordError> {
    let rec = &source.record;
    let raw_name = rec.name.as_str();
    let fail = |reason: &str| RecordError::new(raw_name, reason);
    let name = check_name(raw_name).map_err(|reason| fail(&reason))?;
    if rec.patterns.is_empty() && !rec.always_apply {
        return Err(fail(
            "\"pattern\" must be a string or a list of 1 to 16 strings, or set alwaysApply",
        ));
    }
    if rec.patterns.len() > PATTERNS_MAX {
        return Err(fail("the rule has more than 16 conditions"));
    }
    if let Some(index) = rec
        .patterns
        .iter()
        .position(|pattern| pattern.len() > CONDITION_MAX_BYTES)
    {
        return Err(fail(&format!(
            "condition {} is longer than 1024 bytes",
            index + 1
        )));
    }
    if rec.text.trim().is_empty() {
        return Err(fail("the body is empty"));
    }
    if let Some(gap) = rec
        .repeat_gap
        .filter(|gap| !(1..=REPEAT_GAP_MAX).contains(gap))
    {
        return Err(fail(&format!(
            "repeatGap {gap} is invalid; use a whole number from 1 to 1000"
        )));
    }

    let origin = Origin::Record {
        plugin: source.plugin.as_str().into(),
    };
    let mut problems = Vec::new();
    let conditions: Vec<ConditionSource> = rec
        .patterns
        .iter()
        .enumerate()
        .map(|(index, src)| ConditionSource {
            index,
            src: src.clone(),
        })
        .collect();
    let shorthand = split_shorthand(&conditions);
    let extra_tokens: Vec<String> = shorthand
        .tokens
        .iter()
        .map(|token| (**token).to_owned())
        .collect();
    let (scope, mut scope_problems) = parse_scope(ScopeInput {
        origin: &origin,
        value: rec
            .scope
            .as_ref()
            .map_or(ScopeValue::Missing, ScopeValue::Core),
        known_tools,
        extra_tokens: &extra_tokens,
    });
    problems.append(&mut scope_problems);
    let globs = rec
        .globs
        .as_deref()
        .and_then(|patterns| glob_filter(patterns, &origin, &mut problems));
    let agents = rec
        .agents
        .as_deref()
        .and_then(|patterns| glob_filter(patterns, &origin, &mut problems));
    let rule = Rule {
        name,
        origin,
        description: None,
        conditions: shorthand.conditions,
        scope,
        globs,
        agents,
        always_apply: rec.always_apply,
        report: rec.report,
        enabled: rec.enabled,
        interrupt_mode: rec.mode,
        repeat_mode: rec.repeat_mode,
        repeat_gap: rec.repeat_gap,
        judge: rec.judge.clone(),
        body: (*rec.text).to_owned(),
    };
    Ok((rule, problems))
}

/// Checks the rule name grammar; the error is the problem-table reason.
fn check_name(text: &str) -> Result<Name, String> {
    Name::parse(text).ok_or_else(|| {
        format!(
            "the name \"{text}\" is invalid; use letters, digits, \".\", \"_\", and \"-\", at most 64 characters, starting with a letter or digit"
        )
    })
}

/// Builds a path or agent glob filter; bad globs become Skipped remarks.
///
/// When the set as a whole fails to build, every pattern gets the remark and
/// the filter admits nothing, so the rule never applies unseen.
fn glob_filter(
    patterns: &[Box<str>],
    origin: &Origin,
    problems: &mut Vec<Problem>,
) -> Option<globset::GlobSet> {
    let patterns: Vec<String> = patterns
        .iter()
        .map(|pattern| (**pattern).to_owned())
        .collect();
    if let Ok((set, mut remarks)) = compile_globs(&patterns, origin) {
        problems.append(&mut remarks);
        return set;
    }
    problems.extend(patterns.iter().map(|pattern| Problem {
        origin: origin.clone(),
        kind: ProblemKind::Glob,
        reason: format!("glob \"{pattern}\" is invalid"),
        consequence: "dalgon skipped it.".to_owned(),
        severity: Severity::Skipped,
    }));
    Some(globset::GlobSet::empty())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use dal_core::ext::{InterruptMode, RepeatMode, Scope};

    use super::*;
    use crate::ttsr::value::{ToolPat, ToolScope};

    fn core_name(text: &str) -> dal_core::ext::Name {
        dal_core::ext::Name::parse(text).expect("test name is valid")
    }

    fn record(patterns: &[&str]) -> RuleRecord {
        RuleRecord {
            name: core_name("no-sleep"),
            patterns: patterns.iter().map(|&pattern| pattern.into()).collect(),
            text: "Do not sleep.".into(),
            judge: None,
            scope: None,
            globs: None,
            agents: None,
            mode: None,
            repeat_mode: None,
            repeat_gap: None,
            always_apply: false,
            report: false,
            enabled: true,
        }
    }

    fn check(record: RuleRecord) -> Result<(Rule, Vec<Problem>), RecordError> {
        check_with(record, None)
    }

    fn check_with(
        record: RuleRecord,
        known_tools: Option<&[&str]>,
    ) -> Result<(Rule, Vec<Problem>), RecordError> {
        validate_record(
            &RecordSource {
                plugin: core_name("safe"),
                site: Site {
                    path: PathBuf::from("/data/plugins/safe/lib/rules.star"),
                    line: 3,
                    col: 1,
                },
                record,
            },
            known_tools,
        )
    }

    fn reason(rec: RuleRecord) -> String {
        check(rec).expect_err("record is rejected").to_string()
    }

    #[test]
    fn valid_record_maps_to_a_record_rule() {
        let rec = RuleRecord {
            judge: Some("Is the sleep a busy wait?".into()),
            mode: Some(InterruptMode::ToolOnly),
            repeat_mode: Some(RepeatMode::AfterGap),
            repeat_gap: Some(3),
            report: true,
            text: "  Do not sleep.\n".into(),
            ..record(&[r"\bsleep\s+[0-9]", "wait"])
        };
        let (rule, problems) = check(rec).expect("record is valid");
        assert_eq!(problems, []);
        assert_eq!(rule.name.as_str(), "no-sleep");
        assert_eq!(
            rule.origin,
            Origin::Record {
                plugin: "safe".into()
            }
        );
        assert_eq!(rule.description, None);
        assert_eq!(
            rule.conditions,
            [
                ConditionSource {
                    index: 0,
                    src: r"\bsleep\s+[0-9]".into()
                },
                ConditionSource {
                    index: 1,
                    src: "wait".into()
                },
            ]
        );
        assert!(rule.scope.text && !rule.scope.thinking);
        assert_eq!(rule.scope.tools, ToolScope::All);
        assert!(rule.globs.is_none() && rule.agents.is_none());
        assert!(rule.report && rule.enabled && !rule.always_apply);
        assert_eq!(rule.interrupt_mode, Some(InterruptMode::ToolOnly));
        assert_eq!(rule.repeat_mode, Some(RepeatMode::AfterGap));
        assert_eq!(rule.repeat_gap, Some(3));
        assert_eq!(rule.judge.as_deref(), Some("Is the sleep a busy wait?"));
        assert_eq!(rule.body, "  Do not sleep.\n");
    }

    #[test]
    fn origin_is_the_explicit_plugin_not_the_site_directory() {
        let (rule, _) = check(record(&["x"])).expect("record is valid");
        assert_eq!(
            rule.origin,
            Origin::Record {
                plugin: "safe".into()
            }
        );
        assert_eq!(rule.origin.source_label(), "plugin:safe");
    }

    #[test]
    fn name_grammar_reason_and_error_shape() {
        let reason = check_name("bad name").expect_err("space is outside the grammar");
        assert_eq!(
            reason,
            "the name \"bad name\" is invalid; use letters, digits, \".\", \"_\", and \"-\", at most 64 characters, starting with a letter or digit"
        );
        assert!(check_name(&"a".repeat(65)).is_err());
        assert!(check_name(&"a".repeat(64)).is_ok());
        let error = RecordError::new("bad name", &reason);
        assert_eq!(error.to_string(), format!("rule bad name: {reason}"));
        assert_eq!(error.reason(), reason);
    }

    #[test]
    fn condition_count_and_length() {
        assert_eq!(
            reason(record(&[])),
            "rule no-sleep: \"pattern\" must be a string or a list of 1 to 16 strings, or set alwaysApply"
        );
        let many = ["a"; 17];
        assert_eq!(
            reason(record(&many)),
            "rule no-sleep: the rule has more than 16 conditions"
        );
        assert!(check(record(&["a"; 16])).is_ok());
        let unconditional = RuleRecord {
            always_apply: true,
            ..record(&[])
        };
        assert!(check(unconditional).is_ok());
        let long = "a".repeat(CONDITION_MAX_BYTES + 1);
        assert_eq!(
            reason(record(&["a", long.as_str()])),
            "rule no-sleep: condition 2 is longer than 1024 bytes"
        );
        let fits = "a".repeat(CONDITION_MAX_BYTES);
        assert!(check(record(&[fits.as_str()])).is_ok());
    }

    #[test]
    fn malformed_regex_is_left_to_the_set_build() {
        let (rule, problems) =
            check(record(&["(", "(?x)a", "(?=a)b"])).expect("regex is not compiled");
        assert_eq!(problems, []);
        assert_eq!(rule.conditions.len(), 3);
        assert_eq!(&*rule.conditions[0].src, "(");
    }

    #[test]
    fn body_and_gap_checks_follow_the_condition_checks() {
        let blank = RuleRecord {
            text: " \n".into(),
            ..record(&["a"])
        };
        assert_eq!(reason(blank), "rule no-sleep: the body is empty");
        let both = RuleRecord {
            text: String::new().into(),
            ..record(&[])
        };
        assert_eq!(
            reason(both),
            "rule no-sleep: \"pattern\" must be a string or a list of 1 to 16 strings, or set alwaysApply"
        );
        for gap in [0, 1001] {
            let rec = RuleRecord {
                repeat_gap: Some(gap),
                ..record(&["a"])
            };
            assert_eq!(
                reason(rec),
                format!(
                    "rule no-sleep: repeatGap {gap} is invalid; use a whole number from 1 to 1000"
                )
            );
        }
        for gap in [1, 1000] {
            let rec = RuleRecord {
                repeat_gap: Some(gap),
                ..record(&["a"])
            };
            assert!(check(rec).is_ok());
        }
    }

    #[test]
    fn typed_scope_and_glob_shorthand() {
        let rec = RuleRecord {
            scope: Some(Scope {
                text: false,
                thinking: true,
                tool: true,
                named_tools: vec![core_name("exec")],
            }),
            ..record(&["todo", "src/*.rs"])
        };
        let (rule, problems) = check(rec).expect("record is valid");
        assert_eq!(problems, []);
        assert!(!rule.scope.text && rule.scope.thinking);
        let ToolScope::Tools(tools) = &rule.scope.tools else {
            panic!("named tools stay named");
        };
        assert_eq!(tools.len(), 2);
        assert_eq!(
            tools[0],
            ToolPat {
                tool: "exec".into(),
                glob: None,
                available: true,
            }
        );
        assert_eq!(&*tools[1].tool, "patch");
        assert!(tools[1].glob.is_some());
        assert_eq!(
            rule.conditions,
            [ConditionSource {
                index: 0,
                src: "todo".into()
            }]
        );

        let silent = RuleRecord {
            scope: Some(Scope {
                text: false,
                thinking: false,
                tool: false,
                named_tools: Vec::new(),
            }),
            ..record(&["a"])
        };
        let (rule, _) = check(silent).expect("an empty scope registers");
        assert!(!rule.scope.reaches_any());
    }

    #[test]
    fn inventory_marks_an_unknown_named_tool_with_one_note() {
        let rec = RuleRecord {
            scope: Some(Scope {
                text: false,
                thinking: false,
                tool: true,
                named_tools: vec![core_name("exec"), core_name("fly")],
            }),
            ..record(&["a"])
        };
        let (rule, problems) = check_with(rec, Some(&["exec", "read"])).expect("record is valid");
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].severity, Severity::Note);
        assert_eq!(
            problems[0].reason,
            "the scope names the tool \"fly\", which dalgon does not have"
        );
        let ToolScope::Tools(tools) = &rule.scope.tools else {
            panic!("named tools stay named");
        };
        assert_eq!(tools.len(), 2);
        assert!(tools[0].available && !tools[1].available);
    }

    #[test]
    fn bad_glob_is_a_remark_not_a_rejection() {
        let rec = RuleRecord {
            globs: Some(vec!["a[".into(), "*.ml".into()]),
            agents: Some(vec!["sub".into()]),
            ..record(&["a"])
        };
        let (rule, problems) = check(rec).expect("record is valid");
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].kind, ProblemKind::Glob);
        assert_eq!(problems[0].severity, Severity::Skipped);
        assert_eq!(
            problems[0].origin,
            Origin::Record {
                plugin: "safe".into()
            }
        );
        let globs = rule.globs.expect("globs are kept");
        assert!(globs.is_match("lib.ml") && !globs.is_match("lib.rs"));
        assert!(rule.agents.expect("agents are kept").is_match("sub"));
    }
}
