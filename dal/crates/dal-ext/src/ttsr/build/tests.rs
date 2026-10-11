use super::*;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use super::super::ToolScope;
use super::super::value::{Origin, ProblemKind, Severity};
use dal_core::JudgeMode;
use dal_core::ext::{InterruptMode, Name as PluginName, RepeatMode, RuleRecord, Scope, Site};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    #[expect(
        clippy::create_dir,
        reason = "exclusive test directory creation must reject collisions"
    )]
    fn new() -> Self {
        loop {
            let path = std::env::temp_dir().join(format!(
                "dal-ext-ttsr-build-{}-{}",
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
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn core_name(value: &str) -> PluginName {
    PluginName::parse(value).expect("valid test plugin name")
}

fn write_rule(directory: &Path, name: &str, front: &str, body: &str) -> PathBuf {
    fs::create_dir_all(directory).expect("create rule directory");
    let path = directory.join(format!("{name}.md"));
    let contents = format!("---\n{front}---\n{body}\n");
    fs::write(&path, contents).expect("write rule file");
    path
}

fn build_for<'a>(
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

fn set(data_root: &Path, workspace: &Path, cfg: &RulesConfig, agent: &str) -> RuleSet {
    set_for(&build_for(&[], &[], &[], agent), data_root, workspace, cfg)
}

fn record_source(plugin: &PluginName, name: &str, text: &str) -> RecordSource {
    RecordSource {
        plugin: plugin.clone(),
        site: Site {
            path: PathBuf::from(format!("{}/rules.star", plugin.as_str())),
            line: 1,
            col: 1,
        },
        record: RuleRecord {
            name: PluginName::parse(name).expect("valid record rule name"),
            patterns: vec!["sleep".into()],
            text: text.into(),
            judge: None,
            scope: None,
            globs: None,
            agents: None,
            mode: Some(InterruptMode::Always),
            repeat_mode: Some(RepeatMode::Once),
            repeat_gap: Some(1),
            always_apply: false,
            report: false,
            enabled: true,
        },
    }
}

fn plugin_rule_file(plugin: &str, path: &str, body: &str) -> RuleFile {
    let bytes: Arc<[u8]> = Arc::from(format!("---\ncondition: sleep\n---\n{body}\n").into_bytes());
    RuleFile {
        plugin: core_name(plugin),
        origin: dal_core::ext::Origin::User,
        path: path.into(),
        bytes,
    }
}
fn rule_names(rules: &[Arc<Rule>]) -> Vec<String> {
    rules
        .iter()
        .map(|rule| rule.name.as_str().to_owned())
        .collect()
}

#[test]
fn name_precedence_roots() {
    let temp = TestDir::new();
    let data = temp.0.join("data");
    let workspace = temp.0.join("workspace");
    let user = write_rule(
        &data.join("rules"),
        "no-sleep",
        "condition: sleep\n",
        "User.",
    );
    write_rule(
        &workspace.join(".dal/rules"),
        "no-sleep",
        "condition: sleep\n",
        "Project.",
    );
    let plugin_rule = plugin_rule_file("p", "steer/rules/no-sleep.md", "Plugin.");

    let set = set_for(
        &build_for(&[], std::slice::from_ref(&plugin_rule), &[], "main"),
        &data,
        &workspace,
        &RulesConfig::default(),
    );
    assert_eq!(set.stream.len(), 1);
    assert_eq!(set.stream[0].body, "User.");
    let user_source = user.display().to_string();
    assert!(set.problems.iter().any(|problem| {
        problem.reason == format!("rule \"no-sleep\" from plugin p is replaced by {user_source}")
    }));
    assert!(set.problems.iter().any(|problem| {
        problem.reason
            == format!("rule \"no-sleep\" is also defined by {user_source}, which takes precedence")
            && problem.consequence == "dalgon skipped this one."
    }));
}

#[test]
fn plugin_code_and_name_precedence_are_deterministic() {
    let temp = TestDir::new();
    let data = temp.0.join("data");
    let workspace = temp.0.join("workspace");
    let plugin_rules = [
        plugin_rule_file("b", "workflows/rules/shared.md", "B file."),
        plugin_rule_file("a", "steer/rules/shared.md", "A file."),
    ];
    let plugin_a = core_name("a");
    let plugin_b = core_name("b");
    let records = [
        record_source(&plugin_b, "shared", "B code."),
        record_source(&plugin_a, "shared", "A code."),
    ];
    let set = set_for(
        &build_for(&records, &plugin_rules, &[], "main"),
        &data,
        &workspace,
        &RulesConfig::default(),
    );
    assert_eq!(set.stream.len(), 1);
    assert_eq!(set.stream[0].body, "A code.");
    assert_eq!(set.stream[0].origin, Origin::Record { plugin: "a".into() });
    assert!(
        set.problems
            .iter()
            .filter(|problem| problem.kind == ProblemKind::Set)
            .all(|problem| problem.reason
                == "rule \"shared\" is also defined by plugin:a, which takes precedence")
    );
    assert_eq!(
        set.problems
            .iter()
            .filter(|problem| problem.kind == ProblemKind::Set)
            .count(),
        3
    );
}
#[test]
fn plugin_rule_files_keep_nested_source_identity() {
    let temp = TestDir::new();
    let data = temp.0.join("data");
    let workspace = temp.0.join("workspace");
    let mut file = plugin_rule_file("ttsr-rules", "steer/rules/steer-no-meta.md", "No meta.");
    file.origin = dal_core::ext::Origin::Bundled;
    let bytes = Arc::clone(&file.bytes);

    let set = set_for(
        &build_for(&[], std::slice::from_ref(&file), &[], "main"),
        &data,
        &workspace,
        &RulesConfig::default(),
    );

    let rule = &set.stream[0];
    assert_eq!(
        rule.origin,
        Origin::Plugin {
            plugin: "ttsr-rules".into(),
            path: Some(PathBuf::from("steer/rules/steer-no-meta.md")),
        }
    );
    assert_eq!(rule.origin.source_label(), "plugin:ttsr-rules");
    assert_eq!(set.source_bytes("steer-no-meta"), Some(bytes.as_ref()));
}

#[test]
fn hidden_plugin_markdown_reaches_name_validation() {
    let temp = TestDir::new();
    let file = plugin_rule_file("p", "rules/.hidden.md", "Hidden body.");

    let set = set_for(
        &build_for(&[], std::slice::from_ref(&file), &[], "main"),
        &temp.0.join("data"),
        &temp.0.join("workspace"),
        &RulesConfig::default(),
    );

    assert!(set.problems.iter().any(|problem| {
            problem.reason == "the name \".hidden\" is invalid; use letters, digits, \".\", \"_\", and \"-\", at most 64 characters, starting with a letter or digit"
        }));
}
#[test]
fn plugin_rule_directory_cap_uses_path_order() {
    let temp = TestDir::new();
    let plugin_rules: Vec<RuleFile> = (0..=DIRECTORY_LIMIT)
        .map(|index| plugin_rule_file("p", &format!("steer/rules/r{index:03}.md"), "Rule body."))
        .collect();

    let set = set_for(
        &build_for(&[], &plugin_rules, &[], "main"),
        &temp.0.join("data"),
        &temp.0.join("workspace"),
        &RulesConfig::default(),
    );

    assert_eq!(set.stream.len(), DIRECTORY_LIMIT);
    assert!(set.problems.iter().any(|problem| {
        problem.reason == "the directory holds more than 256 rule files"
            && problem.origin
                == Origin::Plugin {
                    plugin: "p".into(),
                    path: Some(PathBuf::from("steer/rules")),
                }
    }));
}

#[test]
fn invalid_high_priority_file_does_not_claim_name() {
    let temp = TestDir::new();
    let data = temp.0.join("data");
    let workspace = temp.0.join("workspace");
    write_rule(&data.join("rules"), "shared", "condition: sleep\n", "   ");
    let project = write_rule(
        &workspace.join(".dal/rules"),
        "shared",
        "condition: sleep\n",
        "Project rule.",
    );

    let set = set(&data, &workspace, &RulesConfig::default(), "main");
    assert_eq!(set.stream.len(), 1);
    assert_eq!(set.stream[0].origin, Origin::Project(project));
    assert!(
        set.problems
            .iter()
            .any(|problem| problem.reason == "the body is empty")
    );
    assert!(
        !set.problems
            .iter()
            .any(|problem| problem.reason.contains("takes precedence"))
    );
}

#[test]
fn disabled_rule_does_not_claim_name() {
    let temp = TestDir::new();
    let data = temp.0.join("data");
    let workspace = temp.0.join("workspace");
    write_rule(
        &data.join("rules"),
        "shared",
        "enabled: false\ncondition: sleep\n",
        "Disabled.",
    );
    let project = write_rule(
        &workspace.join(".dal/rules"),
        "shared",
        "condition: sleep\n",
        "Project rule.",
    );

    let set = set(&data, &workspace, &RulesConfig::default(), "main");
    assert_eq!(set.stream.len(), 1);
    assert_eq!(set.stream[0].origin, Origin::Project(project));
    assert!(set.problems.iter().any(|problem| {
        problem.reason == "\"enabled: false\" turns this rule off"
            && problem.severity == Severity::Note
    }));
    assert!(
        !set.problems
            .iter()
            .any(|problem| problem.reason.contains("takes precedence"))
    );
}

#[test]
fn unknown_tool_note_keeps_scope() {
    let temp = TestDir::new();
    let data = temp.0.join("data");
    let workspace = temp.0.join("workspace");
    let path = write_rule(
        &data.join("rules"),
        "unknown-tool",
        "scope: tool:patch, tool:missing(*.ml), tool:missing(*.mli)\ncondition: sleep\n",
        "Keep the declared scope.",
    );
    let known_tools = ["patch"];

    let set = set_for(
        &build_for(&[], &[], &known_tools, "main"),
        &data,
        &workspace,
        &RulesConfig::default(),
    );
    assert_eq!(set.stream.len(), 1);
    let ToolScope::Tools(tools) = &set.stream[0].scope.tools else {
        panic!("explicit tool scope must remain a tool list");
    };
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool.tool.as_ref())
            .collect::<Vec<_>>(),
        vec!["patch", "missing", "missing"]
    );
    assert!(tools[0].available);
    assert!(!tools[1].available);
    assert_eq!(
        set.problems
            .iter()
            .filter(|problem| {
                problem.reason == "the scope names the tool \"missing\", which dalgon does not have"
            })
            .count(),
        1
    );
    let note = set
        .problems
        .iter()
        .find(|problem| {
            problem.reason == "the scope names the tool \"missing\", which dalgon does not have"
        })
        .expect("unknown tool gets one note");
    assert_eq!(note.origin, Origin::User(path));
    assert_eq!(note.severity, Severity::Note);
}

#[test]
fn judge_off_drops_records_before_precedence() {
    let temp = TestDir::new();
    let data = temp.0.join("data");
    let workspace = temp.0.join("workspace");
    let plugin = core_name("p");
    let mut judged = record_source(&plugin, "shared", "Judged body.");
    judged.record.judge = Some("Is this a sleep call?".into());
    let project = write_rule(
        &workspace.join(".dal/rules"),
        "shared",
        "condition: sleep\n",
        "Project rule.",
    );
    let cfg = RulesConfig {
        judge: JudgeMode::Off,
        ..RulesConfig::default()
    };

    let set = set_for(
        &build_for(&[judged], &[], &[], "main"),
        &data,
        &workspace,
        &cfg,
    );
    assert_eq!(set.stream.len(), 1);
    assert_eq!(set.stream[0].origin, Origin::Project(project));
    assert!(
        !set.problems
            .iter()
            .any(|problem| problem.reason.contains("takes precedence"))
    );
}

#[test]
fn always_budgets_fall_through_to_rulebook() {
    let temp = TestDir::new();
    let data = temp.0.join("data");
    let workspace = temp.0.join("workspace");
    let user = data.join("rules");
    let body = "x".repeat(9_998);
    for name in ["a", "b", "c", "d"] {
        write_rule(
            &user,
            name,
            "alwaysApply: true\ndescription: retained\n",
            &body,
        );
    }
    write_rule(
        &user,
        "e",
        "alwaysApply: true\ndescription: total overflow\n",
        "x",
    );
    write_rule(
        &user,
        "z",
        "alwaysApply: true\ndescription: per-rule overflow\n",
        &"z".repeat(ALWAYS_BODY_LIMIT + 1),
    );

    let set = set(&data, &workspace, &RulesConfig::default(), "main");
    assert_eq!(
        rule_names(&set.always),
        vec![
            "a".to_owned(),
            "b".to_owned(),
            "c".to_owned(),
            "d".to_owned()
        ]
    );
    assert_eq!(
        rule_names(&set.rulebook),
        vec!["e".to_owned(), "z".to_owned()]
    );
    assert_eq!(
        set.problems
            .iter()
            .filter(|problem| problem
                .reason
                .starts_with("the always-apply budget is exhausted"))
            .count(),
        2
    );
}

#[cfg(unix)]
#[test]
fn unreadable_symlink_claims_no_name() {
    use std::os::unix::fs::symlink;

    let temp = TestDir::new();
    let data = temp.0.join("data");
    let workspace = temp.0.join("workspace");
    let user_dir = data.join("rules");
    fs::create_dir_all(&user_dir).expect("create user rules directory");
    symlink(user_dir.join("missing-target"), user_dir.join("shared.md"))
        .expect("create dangling rule symlink");
    let project = write_rule(
        &workspace.join(".dal/rules"),
        "shared",
        "condition: sleep\n",
        "Project rule.",
    );

    let set = set(&data, &workspace, &RulesConfig::default(), "main");
    assert_eq!(set.stream.len(), 1);
    assert_eq!(set.stream[0].origin, Origin::Project(project));
    assert!(set.problems.iter().any(|problem| {
        problem.kind == ProblemKind::File && problem.reason.starts_with("cannot read the file:")
    }));
}

#[test]
fn oversized_file_is_skipped_without_claiming_name() {
    let temp = TestDir::new();
    let data = temp.0.join("data");
    let workspace = temp.0.join("workspace");
    let user_dir = data.join("rules");
    fs::create_dir_all(&user_dir).expect("create user rules directory");
    fs::write(user_dir.join("shared.md"), vec![b'x'; FILE_LIMIT + 1])
        .expect("write oversized file");
    let project = write_rule(
        &workspace.join(".dal/rules"),
        "shared",
        "condition: sleep\n",
        "Project rule.",
    );

    let set = set(&data, &workspace, &RulesConfig::default(), "main");
    assert_eq!(set.stream.len(), 1);
    assert_eq!(set.stream[0].origin, Origin::Project(project));
    assert!(set.problems.iter().any(|problem| {
        problem.reason == "the file is larger than 65536 bytes" && problem.kind == ProblemKind::File
    }));
}

#[test]
fn rulebook_and_always_texts_are_stable() {
    let temp = TestDir::new();
    let data = temp.0.join("data");
    let workspace = temp.0.join("workspace");
    write_rule(
        &data.join("rules"),
        "a",
        "alwaysApply: true\n",
        "Always body.",
    );
    write_rule(
        &data.join("rules"),
        "b",
        "description: \"first\\nsecond\"\nglobs: [\"*.ml\", \"*.mli\"]\n",
        "Rulebook body.",
    );

    let set = set(&data, &workspace, &RulesConfig::default(), "main");
    assert_eq!(set.always_text(), "Always body.");
    assert_eq!(set.rulebook_text(), "- b (*.ml, *.mli): first second");
}

#[test]
fn glob_shorthand_fallback_has_no_empty_text_note() {
    let temp = TestDir::new();
    let data = temp.0.join("data");
    let workspace = temp.0.join("workspace");
    write_rule(
        &data.join("rules"),
        "glob-rule",
        "condition: *.ml\n",
        "Patch files only.",
    );

    let known_tools = ["patch"];
    let set = set_for(
        &build_for(&[], &[], &known_tools, "main"),
        &data,
        &workspace,
        &RulesConfig::default(),
    );
    assert_eq!(rule_names(&set.stream), vec!["glob-rule".to_owned()]);
    assert_eq!(
        set.compiled_conditions("glob-rule").map(<[_]>::len),
        Some(1)
    );
    assert!(!set.problems.iter().any(|problem| {
        problem.reason.starts_with("condition 1 ") && problem.reason.contains("matches empty text")
    }));
}

#[test]
fn load_order_does_not_change_buckets_or_prompt_text() {
    let left = TestDir::new();
    let right = TestDir::new();
    let left_root = left.0.join("data/rules");
    let right_root = right.0.join("data/rules");
    let rules = [
        ("z", "description: zed\n", "z body"),
        ("a", "alwaysApply: true\n", "a body"),
        ("m", "description: em\n", "m body"),
    ];
    for (name, front, body) in rules {
        write_rule(&left_root, name, front, body);
    }
    for (name, front, body) in rules.into_iter().rev() {
        write_rule(&right_root, name, front, body);
    }

    let left_set = set(
        &left.0.join("data"),
        &left.0.join("workspace"),
        &RulesConfig::default(),
        "main",
    );
    let right_set = set(
        &right.0.join("data"),
        &right.0.join("workspace"),
        &RulesConfig::default(),
        "main",
    );
    assert_eq!(rule_names(&left_set.stream), rule_names(&right_set.stream));
    assert_eq!(rule_names(&left_set.always), rule_names(&right_set.always));
    assert_eq!(
        rule_names(&left_set.rulebook),
        rule_names(&right_set.rulebook)
    );
    assert_eq!(left_set.always_text(), right_set.always_text());
    assert_eq!(left_set.rulebook_text(), right_set.rulebook_text());
}

#[test]
fn directory_cap_and_filters_are_applied() {
    let temp = TestDir::new();
    let data = temp.0.join("data");
    let workspace = temp.0.join("workspace");
    let user_dir = data.join("rules");
    write_rule(
        &user_dir,
        "a",
        "description: disabled\ncondition: x\n",
        "body",
    );
    write_rule(
        &user_dir,
        "r000",
        "description: too large\ncondition: x\n",
        "body",
    );
    fs::write(user_dir.join("r000.md"), vec![b'x'; FILE_LIMIT + 1])
        .expect("replace with oversized file");
    write_rule(
        &user_dir,
        "r001",
        "agents: [sub]\ndescription: child only\ncondition: x\n",
        "body",
    );
    for index in 2..=255 {
        write_rule(
            &user_dir,
            &format!("r{index:03}"),
            "description: listed\ncondition: x\n",
            "body",
        );
    }
    let cfg = RulesConfig {
        watch: false,
        disabled: vec!["a".into()],
        ..RulesConfig::default()
    };

    let parent = set(&data, &workspace, &cfg, "main");
    let child = for_agent(&parent, "sub");
    assert!(!parent.always.iter().any(|rule| rule.name.as_str() == "a"));
    assert!(!parent.rulebook.iter().any(|rule| rule.name.as_str() == "a"));
    assert!(
        child
            .rulebook
            .iter()
            .any(|rule| rule.name.as_str() == "r001")
    );
    assert!(
        parent
            .problems
            .iter()
            .any(|problem| { problem.reason == "the directory holds more than 256 rule files" })
    );
    assert!(
        parent
            .problems
            .iter()
            .any(|problem| { problem.reason == "the file is larger than 65536 bytes" })
    );
    assert!(child.problems.iter().any(|problem| {
        problem.reason == "rules.watch is false, so dalgon does not watch this rule"
    }));
    assert!(
        !parent
            .problems
            .iter()
            .any(|problem| problem.origin.source_label().ends_with("/a.md"))
    );
}

#[test]
fn stream_condition_cap_is_applied_in_name_order() {
    let temp = TestDir::new();
    let plugin = core_name("p");
    let records: Vec<RecordSource> = (0..=STREAM_CONDITION_LIMIT)
        .map(|index| RecordSource {
            plugin: plugin.clone(),
            site: Site {
                path: PathBuf::from("p/rules.star"),
                line: 1,
                col: 1,
            },
            record: RuleRecord {
                name: dal_core::ext::Name::parse(&format!("r{index:03}")).expect("valid rule name"),
                patterns: vec!["x".into()],
                text: "Rule body.".into(),
                judge: None,
                scope: Some(Scope {
                    text: true,
                    thinking: false,
                    tool: false,
                    named_tools: Vec::new(),
                }),
                globs: None,
                agents: None,
                mode: Some(InterruptMode::Always),
                repeat_mode: Some(RepeatMode::Once),
                repeat_gap: Some(1),
                always_apply: false,
                report: false,
                enabled: true,
            },
        })
        .collect();
    let set = set_for(
        &build_for(&records, &[], &[], "main"),
        &temp.0.join("data"),
        &temp.0.join("workspace"),
        &RulesConfig::default(),
    );
    assert_eq!(set.stream.len(), STREAM_CONDITION_LIMIT);
    assert_eq!(set.compiled_conditions("r000").map(<[_]>::len), Some(1));
    assert!(set.problems.iter().any(|problem| {
        problem.reason == "the stream rules already hold 256 conditions"
            && problem.origin.source_label() == "plugin:p"
    }));
}

#[test]
fn child_filter_uses_retained_bytes_after_files_disappear() {
    let temp = TestDir::new();
    let data = temp.0.join("data");
    let workspace = temp.0.join("workspace");
    let child_path = write_rule(
        &data.join("rules"),
        "child-rule",
        "agents: [sub]\ncondition: child-marker\n",
        "Child body.",
    );
    let parent = set(&data, &workspace, &RulesConfig::default(), "main");
    assert!(parent.stream.is_empty());
    assert_eq!(
        parent.source_bytes("child-rule"),
        Some(b"---\nagents: [sub]\ncondition: child-marker\n---\nChild body.\n".as_slice())
    );
    fs::remove_file(child_path).expect("remove source file");
    fs::remove_dir_all(data.join("rules")).expect("remove source directory");

    let child = for_agent(&parent, "sub");
    assert_eq!(rule_names(&child.stream), vec!["child-rule".to_owned()]);
    assert_eq!(
        child.source_bytes("child-rule"),
        parent.source_bytes("child-rule")
    );
}
