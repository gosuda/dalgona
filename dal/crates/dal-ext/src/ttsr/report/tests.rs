//! Behavior tests for the `dalgon rules` report and offline prover.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use dal_core::RulesConfig;
use dal_core::ext::{Name as PluginName, RuleFile, RuleRecord, Site};

use super::super::build::RuleBuildInput;
use super::super::record::RecordSource;
use super::*;

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        loop {
            let path = std::env::temp_dir().join(format!(
                "dal-ext-ttsr-report-{}-{}",
                std::process::id(),
                NEXT_DIR.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("create temporary test directory: {error}"),
            }
        }
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_rule(directory: &Path, name: &str, front: &str, body: &str) -> PathBuf {
    fs::create_dir_all(directory).expect("create rule directory");
    let path = directory.join(format!("{name}.md"));
    fs::write(&path, format!("---\n{front}---\n{body}\n")).expect("write rule file");
    path
}

fn user_dir(data_root: &Path) -> PathBuf {
    data_root.join("rules")
}

fn project_dir(workspace: &Path) -> PathBuf {
    workspace.join(".dal").join("rules")
}

fn input<'a>(
    records: &'a [RecordSource],
    plugin_rules: &'a [RuleFile],
    known_tools: &'a [&'a str],
    agent: &'a str,
) -> RuleBuildInput<'a> {
    RuleBuildInput {
        records,
        plugin_rules,
        known_tools,
        agent,
    }
}

fn plugin_name(value: &str) -> PluginName {
    PluginName::parse(value).expect("valid test plugin name")
}

fn record_source(
    plugin: &PluginName,
    name: &str,
    report: bool,
    judge: Option<&str>,
) -> RecordSource {
    RecordSource {
        plugin: plugin.clone(),
        site: Site {
            path: PathBuf::from(format!("{}/rules.star", plugin.as_str())),
            line: 1,
            col: 1,
        },
        record: RuleRecord {
            name: plugin_name(name),
            patterns: vec!["sleep".into()],
            text: "Nap.".into(),
            judge: judge.map(std::convert::Into::into),
            scope: None,
            globs: None,
            agents: None,
            mode: None,
            repeat_mode: None,
            repeat_gap: None,
            always_apply: false,
            report,
            enabled: true,
        },
    }
}

fn test_flags(source: TestSource, text: &str) -> TestFlags {
    TestFlags {
        source,
        tool: "patch".to_owned(),
        path: None,
        text: text.to_owned(),
    }
}

#[test]
fn no_rules_hint_names_both_roots() {
    let data_root = TestDir::new();
    let workspace = TestDir::new();
    let cfg = RulesConfig::default();
    let report = run_rules(
        &input(&[], &[], &[], "agent"),
        data_root.path(),
        workspace.path(),
        &cfg,
    );
    assert_eq!(
        report.text,
        format!(
            "No rules. Add Markdown files to {} or {}.\n",
            user_dir(data_root.path()).display(),
            project_dir(workspace.path()).display()
        )
    );
    assert_eq!(report.exit, 0);
}

#[test]
fn stream_section_renders_exact_lines() {
    let data_root = TestDir::new();
    let workspace = TestDir::new();
    write_rule(
        &user_dir(data_root.path()),
        "patch-guard",
        "condition: secret-[0-9]+\nscope: text\ninterruptMode: never\nrepeatMode: once\n",
        "Guard the patch.",
    );
    let cfg = RulesConfig::default();
    let report = run_rules(
        &input(&[], &[], &[], "agent"),
        data_root.path(),
        workspace.path(),
        &cfg,
    );
    let source = user_dir(data_root.path())
        .join("patch-guard.md")
        .display()
        .to_string();
    assert_eq!(
        report.text,
        format!(
            "stream (1)\n  patch-guard  {source}\n    interrupt never, repeat once\n    condition: secret-[0-9]+\n    scope: text\n"
        )
    );
    assert_eq!(report.exit, 0);
}

#[test]
fn stream_section_reports_configured_defaults() {
    let data_root = TestDir::new();
    let workspace = TestDir::new();
    write_rule(
        &user_dir(data_root.path()),
        "plain",
        "condition: sleep\n",
        "Plain rule.",
    );
    let cfg = RulesConfig::default();
    let report = run_rules(
        &input(&[], &[], &[], "agent"),
        data_root.path(),
        workspace.path(),
        &cfg,
    );
    assert!(report.text.contains("    interrupt always, repeat once\n"));
    assert!(report.text.contains("    scope: text, tool\n"));
}

#[test]
fn interrupt_modes_report_each_literal() {
    let data_root = TestDir::new();
    let workspace = TestDir::new();
    for (name, mode) in [
        ("m-always", "always"),
        ("m-prose", "prose-only"),
        ("m-tool", "tool-only"),
    ] {
        write_rule(
            &user_dir(data_root.path()),
            name,
            &format!("condition: sleep\ninterruptMode: {mode}\n"),
            "Mode rule.",
        );
    }
    let cfg = RulesConfig::default();
    let report = run_rules(
        &input(&[], &[], &[], "agent"),
        data_root.path(),
        workspace.path(),
        &cfg,
    );
    assert!(report.text.starts_with("stream (3)\n"));
    assert!(report.text.contains("    interrupt always, repeat once\n"));
    assert!(
        report
            .text
            .contains("    interrupt prose-only, repeat once\n")
    );
    assert!(
        report
            .text
            .contains("    interrupt tool-only, repeat once\n")
    );
}
#[test]
fn always_apply_and_rulebook_sections() {
    let data_root = TestDir::new();
    let workspace = TestDir::new();
    write_rule(
        &user_dir(data_root.path()),
        "standing",
        "alwaysApply: true\ndescription: Standing order.\n",
        "Standing order.",
    );
    write_rule(
        &user_dir(data_root.path()),
        "handbook",
        "description: Follow the handbook.\n",
        "Handbook body.",
    );
    let cfg = RulesConfig::default();
    let report = run_rules(
        &input(&[], &[], &[], "agent"),
        data_root.path(),
        workspace.path(),
        &cfg,
    );
    let user = user_dir(data_root.path());
    assert_eq!(
        report.text,
        format!(
            "always-apply (1)\n  standing  {}\nrulebook (1)\n  handbook  {}\n    Follow the handbook.\n",
            user.join("standing.md").display(),
            user.join("handbook.md").display()
        )
    );
    assert_eq!(report.exit, 0);
}

#[test]
fn skipped_problem_sets_exit_one() {
    let data_root = TestDir::new();
    let workspace = TestDir::new();
    let path = write_rule(
        &user_dir(data_root.path()),
        "too-long",
        &format!("condition: {}\n", "x".repeat(2000)),
        "Too long.",
    );
    let cfg = RulesConfig::default();
    let report = run_rules(
        &input(&[], &[], &[], "agent"),
        data_root.path(),
        workspace.path(),
        &cfg,
    );
    assert_eq!(
        report.text,
        format!(
            "problems (2)\n  {}: condition 1 is longer than 1024 bytes\n  {}: the rule has no usable condition, no \"alwaysApply: true\", and no description dalgon skipped it.\n",
            path.display(),
            path.display()
        )
    );
    assert_eq!(report.exit, 1);
}

#[test]
fn note_problem_keeps_exit_zero() {
    let data_root = TestDir::new();
    let workspace = TestDir::new();
    let path = write_rule(
        &user_dir(data_root.path()),
        "noted",
        "condition: sleep\nquestion: why?\n",
        "Noted rule.",
    );
    let cfg = RulesConfig::default();
    let report = run_rules(
        &input(&[], &[], &[], "agent"),
        data_root.path(),
        workspace.path(),
        &cfg,
    );
    assert!(report.text.starts_with("stream (1)\n"));
    assert!(report.text.contains(&format!(
        "  note: {}: \"question\" is not supported. dal ignores it.\n",
        path.display()
    )));
    assert_eq!(report.exit, 0);
}

#[test]
fn record_suffixes_report_only_and_judged() {
    let plugin = plugin_name("t");
    let records = vec![
        record_source(&plugin, "noted-down", true, None),
        record_source(&plugin, "weighed", false, Some("Is this nap a busy wait?")),
    ];
    let data_root = TestDir::new();
    let workspace = TestDir::new();
    let cfg = RulesConfig::default();
    let report = run_rules(
        &input(&records, &[], &[], "agent"),
        data_root.path(),
        workspace.path(),
        &cfg,
    );
    assert_eq!(
        report.text,
        "stream (2)\n  noted-down  plugin:t\n    interrupt always, repeat once, report only\n    condition: sleep\n    scope: text, tool\n  weighed  plugin:t\n    interrupt always, repeat once, judged\n    condition: sleep\n    scope: text, tool\n"
    );
    assert_eq!(report.exit, 0);
}

#[test]
fn offline_test_reports_fire_and_exit_zero() {
    let data_root = TestDir::new();
    let workspace = TestDir::new();
    write_rule(
        &user_dir(data_root.path()),
        "patch-guard",
        "condition: secret-[0-9]+\nscope: text\ninterruptMode: never\nrepeatMode: once\n",
        "Guard the patch.",
    );
    let cfg = RulesConfig::default();
    let proved = run_test(
        &input(&[], &[], &[], "agent"),
        data_root.path(),
        workspace.path(),
        &cfg,
        &test_flags(TestSource::Text, "leaks secret-42 here"),
    )
    .expect("valid offline test");
    assert_eq!(
        proved.text,
        "fired: patch-guard (remind). The reply matched /secret-[0-9]+/.\n"
    );
    assert_eq!(proved.exit, 0);
}

#[test]
fn offline_test_without_fire_exits_one() {
    let data_root = TestDir::new();
    let workspace = TestDir::new();
    write_rule(
        &user_dir(data_root.path()),
        "patch-guard",
        "condition: secret-[0-9]+\nscope: text\n",
        "Guard the patch.",
    );
    let cfg = RulesConfig::default();
    let proved = run_test(
        &input(&[], &[], &[], "agent"),
        data_root.path(),
        workspace.path(),
        &cfg,
        &test_flags(TestSource::Text, "nothing to see"),
    )
    .expect("valid offline test");
    assert_eq!(proved.text, "No rule fired. Checked 1 stream rules.\n");
    assert_eq!(proved.exit, 1);
}

#[test]
fn offline_tool_source_feeds_added_text() {
    let data_root = TestDir::new();
    let workspace = TestDir::new();
    write_rule(
        &user_dir(data_root.path()),
        "no-sleep",
        "condition: sleep\nscope: tool\ninterruptMode: never\n",
        "No sleep calls.",
    );
    let cfg = RulesConfig::default();
    let proved = run_test(
        &input(&[], &[], &["patch"], "agent"),
        data_root.path(),
        workspace.path(),
        &cfg,
        &TestFlags {
            source: TestSource::Tool,
            tool: "patch".to_owned(),
            path: Some("src/main.rs".to_owned()),
            text: "please sleep now".to_owned(),
        },
    )
    .expect("valid offline tool test");
    assert_eq!(
        proved.text,
        "fired: no-sleep (remind). The `patch` call on `src/main.rs` matched /sleep/.\n"
    );
    assert_eq!(proved.exit, 0);
}

#[test]
fn offline_test_usage_errors() {
    let data_root = TestDir::new();
    let workspace = TestDir::new();
    let cfg = RulesConfig::default();
    let empty = input(&[], &[], &[], "agent");

    let tool_without_source = run_test(
        &empty,
        data_root.path(),
        workspace.path(),
        &cfg,
        &TestFlags {
            source: TestSource::Text,
            tool: "exec".to_owned(),
            path: None,
            text: "x".to_owned(),
        },
    );
    assert_eq!(tool_without_source, Err(UsageError::ToolNeedsToolSource));
    assert_eq!(
        UsageError::ToolNeedsToolSource.to_string(),
        "dalgon rules test: --tool needs --source tool."
    );

    let path_without_source = run_test(
        &empty,
        data_root.path(),
        workspace.path(),
        &cfg,
        &TestFlags {
            source: TestSource::Thinking,
            tool: "patch".to_owned(),
            path: Some("a.rs".to_owned()),
            text: "x".to_owned(),
        },
    );
    assert_eq!(path_without_source, Err(UsageError::PathNeedsToolSource));
    assert_eq!(
        UsageError::PathNeedsToolSource.to_string(),
        "dalgon rules test: --path needs --source tool."
    );

    let too_long = run_test(
        &empty,
        data_root.path(),
        workspace.path(),
        &cfg,
        &test_flags(TestSource::Text, &"x".repeat(TEST_TEXT_MAX_BYTES + 1)),
    );
    assert_eq!(too_long, Err(UsageError::TextTooLong));
    assert_eq!(
        UsageError::TextTooLong.to_string(),
        "dalgon rules test: the text is longer than 1048576 bytes."
    );
}
