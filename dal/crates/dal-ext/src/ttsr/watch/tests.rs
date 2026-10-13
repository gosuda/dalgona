//! Watch regression tests: action resolution, interruption, and delivery.

use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use dal_agent::ext::{TurnInfo, WatchFactory};
use dal_core::ext::Channel;
use dal_core::{EntryId, RulesConfig, SessionId, TurnId};

use super::super::build::{RuleBuildInput, RuleSet, set_for};
use super::super::gate::Gate;
use super::super::readers::EditStyle;
use super::super::texts::{RuleSubject, render_reminder_text};
use super::super::value::{InterruptMode, RuleAction};
use super::*;

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
                "dal-ext-ttsr-watch-{}-{}",
                std::process::id(),
                NEXT_DIR.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("create temporary test directory: {error}"),
            }
        }
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_rule(directory: &Path, name: &str, front: &str, body: &str) {
    std::fs::create_dir_all(directory).expect("create rule directory");
    std::fs::write(
        directory.join(format!("{name}.md")),
        format!("---\n{front}---\n{body}\n"),
    )
    .expect("write rule file");
}

fn turn(value: u64) -> TurnId {
    TurnId::new(NonZeroU64::new(value).expect("test turns are nonzero"))
}

fn entry(value: u64) -> EntryId {
    EntryId::new(NonZeroU64::new(value).expect("test entries are nonzero"))
}

struct Fixture {
    _temp: TestDir,
    set: RuleSet,
    cfg: RulesConfig,
    data: PathBuf,
}

fn fixture(files: &[(&str, &str, &str)]) -> Fixture {
    let temp = TestDir::new();
    let data = temp.0.join("data");
    let workspace = temp.0.join("workspace");
    for (name, front, body) in files {
        write_rule(&data.join("rules"), name, front, body);
    }
    let cfg = RulesConfig::default();
    let input = RuleBuildInput {
        records: &[],
        plugin_rules: &[],
        // Production always supplies the product inventory; without it every
        // path-gated tool is unavailable and tool tests cannot fire.
        known_tools: &["patch", "exec"],
        agent: "main",
    };
    let set = set_for(&input, &data, &workspace, &cfg);
    Fixture {
        _temp: temp,
        set,
        cfg,
        data,
    }
}

fn watch(fixture: &Fixture, turn_value: u64, budget: WatchBudget, style: EditStyle) -> Watch {
    create(
        &fixture.set,
        &Gate::default(),
        turn(turn_value),
        budget,
        &fixture.cfg,
        &fixture.data,
        style,
    )
}

fn watch_with_gate(fixture: &Fixture, gate: &Gate, turn_value: u64, budget: WatchBudget) -> Watch {
    create(
        &fixture.set,
        gate,
        turn(turn_value),
        budget,
        &fixture.cfg,
        &fixture.data,
        EditStyle::Replace,
    )
}

#[test]
fn finish_after_interrupt_is_idempotent() {
    let fixture = fixture(&[("r", "condition: MARK\n", "Body.")]);
    let factory = super::factory::TtsrWatchFactory::new(
        Arc::new(fixture.set.clone()),
        Arc::new(Mutex::new(Gate::default())),
        fixture.cfg.clone(),
        fixture.data.clone(),
        EditStyle::Replace,
    );
    let info = TurnInfo::new(SessionId::new_v7(), turn(12));
    let mut watcher = factory.start(&info).expect("a rule starts a watcher");
    assert!(matches!(
        watcher.feed(Channel::Text, "MARK"),
        dal_core::StreamVerdict::Interrupt { .. }
    ));
    assert_eq!(
        watcher.finish(),
        dal_core::StreamVerdict::Continue,
        "the driver may finish after it stops the response"
    );
    assert_eq!(
        watcher.finish(),
        dal_core::StreamVerdict::Continue,
        "finishing an already-stopped watcher is idempotent"
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one table test walks every resolution row"
)]
fn action_resolution_table() {
    let fixture = fixture(&[
        (
            "r-always",
            "scope: text, thinking, tool\ncondition: ALWAYS123\n",
            "Always body.",
        ),
        (
            "r-prose",
            "scope: text, thinking, tool\ninterruptMode: prose-only\ncondition: PROSE123\n",
            "Prose body.",
        ),
        (
            "r-tool",
            "scope: text, thinking, tool\ninterruptMode: tool-only\ncondition: TOOL123\n",
            "Tool body.",
        ),
        (
            "r-never",
            "scope: text, thinking, tool\ninterruptMode: never\ncondition: NEVER123\n",
            "Never body.",
        ),
        (
            "r-report",
            "scope: text, thinking, tool\nreport: true\ncondition: REPORT123\n",
            "Report body.",
        ),
    ]);
    let cases: &[(&str, &str, WatchBudget, RuleAction, WatchVerdict)] = &[
        (
            "r-always",
            "ALWAYS123",
            WatchBudget::Interrupts,
            RuleAction::Interrupt,
            WatchVerdict::Stop,
        ),
        (
            "r-always",
            "ALWAYS123",
            WatchBudget::RemindersOnly,
            RuleAction::Remind,
            WatchVerdict::Continue,
        ),
        (
            "r-prose",
            "PROSE123",
            WatchBudget::Interrupts,
            RuleAction::Interrupt,
            WatchVerdict::Stop,
        ),
        (
            "r-prose",
            "PROSE123",
            WatchBudget::RemindersOnly,
            RuleAction::Remind,
            WatchVerdict::Continue,
        ),
        (
            "r-tool",
            "TOOL123",
            WatchBudget::Interrupts,
            RuleAction::Remind,
            WatchVerdict::Continue,
        ),
        (
            "r-never",
            "NEVER123",
            WatchBudget::Interrupts,
            RuleAction::Remind,
            WatchVerdict::Continue,
        ),
        (
            "r-never",
            "NEVER123",
            WatchBudget::RemindersOnly,
            RuleAction::Remind,
            WatchVerdict::Continue,
        ),
        (
            "r-report",
            "REPORT123",
            WatchBudget::Interrupts,
            RuleAction::Report,
            WatchVerdict::Continue,
        ),
        (
            "r-report",
            "REPORT123",
            WatchBudget::RemindersOnly,
            RuleAction::Report,
            WatchVerdict::Continue,
        ),
    ];
    for (rule, marker, budget, action, verdict) in cases {
        let mut text_watch = watch(&fixture, 1, *budget, EditStyle::Replace);
        assert_eq!(
            text_watch.feed(SourceKind::Text, marker),
            Ok(*verdict),
            "text {rule} under {budget:?}"
        );
        assert_eq!(text_watch.fires().len(), 1);
        assert_eq!(text_watch.fires()[0].action, *action);
        assert_eq!(text_watch.fires()[0].rule.as_str(), *rule);

        let mut thinking_watch = watch(&fixture, 1, *budget, EditStyle::Replace);
        assert_eq!(
            thinking_watch.feed(SourceKind::Thinking, marker),
            Ok(*verdict),
            "thinking {rule} under {budget:?}"
        );
        assert_eq!(thinking_watch.fires()[0].action, *action);
    }

    let tool_cases: &[(&str, &str, WatchBudget, RuleAction, WatchVerdict)] = &[
        (
            "r-always",
            "ALWAYS123",
            WatchBudget::Interrupts,
            RuleAction::Interrupt,
            WatchVerdict::Stop,
        ),
        (
            "r-always",
            "ALWAYS123",
            WatchBudget::RemindersOnly,
            RuleAction::Remind,
            WatchVerdict::Continue,
        ),
        (
            "r-prose",
            "PROSE123",
            WatchBudget::Interrupts,
            RuleAction::Remind,
            WatchVerdict::Continue,
        ),
        (
            "r-tool",
            "TOOL123",
            WatchBudget::Interrupts,
            RuleAction::Interrupt,
            WatchVerdict::Stop,
        ),
        (
            "r-tool",
            "TOOL123",
            WatchBudget::RemindersOnly,
            RuleAction::Remind,
            WatchVerdict::Continue,
        ),
        (
            "r-never",
            "NEVER123",
            WatchBudget::Interrupts,
            RuleAction::Remind,
            WatchVerdict::Continue,
        ),
        (
            "r-report",
            "REPORT123",
            WatchBudget::Interrupts,
            RuleAction::Report,
            WatchVerdict::Continue,
        ),
    ];
    for (rule, marker, budget, action, verdict) in tool_cases {
        let mut tool_watch = watch(&fixture, 1, *budget, EditStyle::Replace);
        let delta = format!("{{\"command\":\"run {marker}\"}}");
        assert_eq!(
            tool_watch.feed(
                SourceKind::Tool {
                    tool: "exec".into()
                },
                &delta
            ),
            Ok(*verdict),
            "tool {rule} under {budget:?}"
        );
        assert_eq!(tool_watch.fires().len(), 1);
        assert_eq!(tool_watch.fires()[0].action, *action);
        assert_eq!(tool_watch.fires()[0].rule.as_str(), *rule);
        assert_ne!(
            tool_watch.fires()[0].inject.is_some(),
            (*action == RuleAction::Report)
        );
    }
}

#[test]
fn remind_then_interrupt() {
    let fixture = fixture(&[
        (
            "r-remind",
            "interruptMode: never\ncondition: REMINDMK\n",
            "Remind body.",
        ),
        ("r-stop", "condition: STOPMK\n", "Stop body."),
    ]);
    let mut watch = watch(&fixture, 1, WatchBudget::Interrupts, EditStyle::Replace);
    assert_eq!(
        watch.feed(SourceKind::Text, "xx REMINDMK"),
        Ok(WatchVerdict::Continue)
    );
    assert_eq!(watch.fires().len(), 1);
    assert_eq!(watch.fires()[0].action, RuleAction::Remind);
    assert_eq!(
        watch.feed(SourceKind::Text, "yy STOPMK"),
        Ok(WatchVerdict::Stop)
    );
    assert_eq!(watch.fires().len(), 2);
    assert_eq!(watch.fires()[0].rule.as_str(), "r-remind");
    assert_eq!(watch.fires()[1].rule.as_str(), "r-stop");
    assert_eq!(watch.fires()[1].action, RuleAction::Interrupt);

    let rule = fixture
        .set
        .stream
        .iter()
        .find(|rule| rule.name.as_str() == "r-remind")
        .expect("remind rule is admitted");
    let expected = render_reminder_text(rule, RuleSubject::Reply);
    assert_eq!(watch.fires()[0].inject.as_deref(), Some(expected.as_str()));
    assert_eq!(
        watch.feed(SourceKind::Text, "zz"),
        Err(WatchError::WatchStopped)
    );
    assert_eq!(watch.finish(), Err(WatchError::WatchStopped));
}

#[test]
fn empty_feed_and_fresh_finish_stay_open() {
    let fixture = fixture(&[("r", "condition: MARK\n", "Body.")]);
    let mut watch = watch(&fixture, 1, WatchBudget::Interrupts, EditStyle::Replace);
    assert_eq!(watch.feed(SourceKind::Text, ""), Ok(WatchVerdict::Continue));
    assert!(watch.fires().is_empty());
    assert_eq!(watch.finish(), Ok(WatchVerdict::Continue));
    assert_eq!(
        watch.feed(SourceKind::Text, "MARK"),
        Err(WatchError::WatchStopped)
    );
}

#[test]
fn record_body_shape_and_key_order() {
    let fixture = fixture(&[("no-sleep", "condition: '\\bsleep\\s+[0-9]'\n", "make test.")]);
    let mut watch = watch(&fixture, 1, WatchBudget::Interrupts, EditStyle::Replace);
    assert_eq!(
        watch.feed(SourceKind::Text, "make test && sleep 5"),
        Ok(WatchVerdict::Stop)
    );
    let at: jiff::Timestamp = "2026-09-26T10:15:40.001Z".parse().expect("fixed timestamp");
    assert_eq!(
        watch.record_body(0, at, Some(entry(7))).as_ref(),
        "{\"v\":1,\"type\":\"rule_fired\",\"at\":\"2026-09-26T10:15:40.001Z\",\"turn\":1,\"rule\":\"no-sleep\",\"action\":\"interrupt\",\"subject\":\"reply\",\"pattern\":\"\\\\bsleep\\\\s+[0-9]\",\"excerpt\":\"make test && sleep 5\",\"entry\":7}"
    );
}

#[test]
fn record_body_report_has_null_entry() {
    let fixture = fixture(&[("r-note", "report: true\ncondition: MARK\n", "Note body.")]);
    let mut watch = watch(&fixture, 2, WatchBudget::Interrupts, EditStyle::Replace);
    assert_eq!(
        watch.feed(SourceKind::Text, "has MARK here"),
        Ok(WatchVerdict::Continue)
    );
    let at: jiff::Timestamp = "2026-09-26T10:15:40.001Z".parse().expect("fixed timestamp");
    let body = watch.record_body(0, at, None);
    assert!(body.contains("\"action\":\"report\""), "{body}");
    assert!(body.contains("\"entry\":null"), "{body}");
    assert!(body.contains("\"turn\":2"), "{body}");
}

#[test]
fn notice_limit_fires_once_at_budget() {
    let fixture = fixture(&[("r", "condition: MARK\n", "Body.")]);
    let mut watch = watch(&fixture, 1, WatchBudget::Interrupts, EditStyle::Replace);
    assert_eq!(watch.notice_limit(2), None);
    assert_eq!(
        watch.notice_limit(3).as_deref(),
        Some(
            "rules: this turn reached rules.max_retries (3 rule interrupts). Later rule matches in this turn reach the model as reminders."
        )
    );
    assert_eq!(watch.notice_limit(3), None);
    assert_eq!(watch.notice_limit(4), None);
}

#[test]
fn gate_snapshot_filters_repeats() {
    let fixture = fixture(&[
        ("r-once", "repeatMode: once\ncondition: ONCEMK\n", "Once."),
        (
            "r-gap",
            "repeatMode: after-gap\nrepeatGap: 2\ncondition: GAPMK\n",
            "Gap.",
        ),
    ]);
    let mut gate = Gate::default();
    gate.record("r-once", turn(3), entry(1));
    gate.record("r-gap", turn(3), entry(2));

    let mut current = watch_with_gate(&fixture, &gate, 3, WatchBudget::Interrupts);
    assert_eq!(
        current.feed(SourceKind::Text, "ONCEMK GAPMK"),
        Ok(WatchVerdict::Continue)
    );
    assert!(current.fires().is_empty());

    let mut later = watch_with_gate(&fixture, &gate, 5, WatchBudget::Interrupts);
    assert_eq!(
        later.feed(SourceKind::Text, "ONCEMK GAPMK"),
        Ok(WatchVerdict::Stop)
    );
    assert_eq!(later.fires().len(), 1);
    assert_eq!(later.fires()[0].rule.as_str(), "r-gap");
}

#[test]
fn gate_record_orders_visible_before_turn_only() {
    let fixture = fixture(&[("r", "condition: MARK\n", "Body.")]);
    let mut watch = watch(&fixture, 4, WatchBudget::Interrupts, EditStyle::Replace);
    assert_eq!(watch.feed(SourceKind::Text, "MARK"), Ok(WatchVerdict::Stop));
    let mut gate = Gate::default();
    watch.gate_record(&mut gate, 0, Some(entry(12)));
    let repeat = super::super::gate::resolve_cfg(&fixture.set.stream[0], &fixture.cfg);
    assert!(!gate.eligible(&fixture.set.stream[0], turn(4), &repeat));

    let mut turn_only = Gate::default();
    watch.gate_record(&mut turn_only, 0, None);
    assert!(!turn_only.eligible(&fixture.set.stream[0], turn(4), &repeat));
    assert!(turn_only.eligible(&fixture.set.stream[0], turn(5), &repeat));
}

#[test]
fn tool_path_gate_waits_for_path() {
    let fixture = fixture(&[(
        "r-patch",
        "scope: tool:patch(*.ml)\ncondition: sleep\n",
        "Patch body.",
    )]);
    let mut watch = watch(&fixture, 1, WatchBudget::Interrupts, EditStyle::Replace);
    assert_eq!(
        watch.feed(
            SourceKind::Tool {
                tool: "patch".into()
            },
            "{\"new\":\"sleep 5\","
        ),
        Ok(WatchVerdict::Continue)
    );
    assert!(watch.fires().is_empty());
    assert_eq!(
        watch.feed(
            SourceKind::Tool {
                tool: "patch".into()
            },
            "\"path\":\"a.ml\"}"
        ),
        Ok(WatchVerdict::Stop)
    );
    assert_eq!(watch.fires().len(), 1);
    assert_eq!(watch.fires()[0].path.as_deref(), Some("a.ml"));
    assert_eq!(watch.fires()[0].subject.as_ref(), "`patch` call on `a.ml`");
}

#[test]
fn tool_wrong_path_never_fires_and_missing_path_drops() {
    let fixture = fixture(&[(
        "r-patch",
        "scope: tool:patch(*.ml)\ncondition: sleep\n",
        "Patch body.",
    )]);
    let mut wrong = watch(&fixture, 1, WatchBudget::Interrupts, EditStyle::Replace);
    assert_eq!(
        wrong.feed(
            SourceKind::Tool {
                tool: "patch".into()
            },
            "{\"new\":\"sleep 5\",\"path\":\"a.txt\"}"
        ),
        Ok(WatchVerdict::Continue)
    );
    assert!(wrong.fires().is_empty());

    let mut missing = watch(&fixture, 1, WatchBudget::Interrupts, EditStyle::Replace);
    assert_eq!(
        missing.feed(
            SourceKind::Tool {
                tool: "patch".into()
            },
            "{\"new\":\"sleep 5\"}"
        ),
        Ok(WatchVerdict::Continue)
    );
    assert!(missing.fires().is_empty());
    assert_eq!(missing.finish(), Ok(WatchVerdict::Continue));
    assert!(missing.fires().is_empty());
}

#[test]
fn judged_match_degrades_to_report_without_interrupts() {
    use dal_core::ext::{RuleRecord, Site};

    let temp = TestDir::new();
    let data = temp.0.join("data");
    let workspace = temp.0.join("workspace");
    let plugin = dal_core::ext::Name::parse("p").expect("valid plugin name");
    let records = [super::super::record::RecordSource {
        plugin,
        site: Site {
            path: PathBuf::from("p/rules.star"),
            line: 1,
            col: 1,
        },
        record: RuleRecord {
            name: dal_core::ext::Name::parse("r-judged").expect("valid rule name"),
            patterns: vec!["sleep".into()],
            text: "Judged body.".into(),
            judge: Some("Is this a bad sleep?".into()),
            scope: None,
            globs: None,
            agents: None,
            mode: Some(InterruptMode::Always),
            repeat_mode: None,
            repeat_gap: None,
            always_apply: false,
            report: false,
            enabled: true,
        },
    }];
    let cfg = RulesConfig::default();
    let input = RuleBuildInput {
        records: &records,
        plugin_rules: &[],
        known_tools: &[],
        agent: "main",
    };
    let set = set_for(&input, &data, &workspace, &cfg);
    assert_eq!(set.stream.len(), 1);

    let mut reminders = create(
        &set,
        &Gate::default(),
        turn(1),
        WatchBudget::RemindersOnly,
        &cfg,
        &data,
        EditStyle::Replace,
    );
    assert_eq!(
        reminders.feed(SourceKind::Text, "time to sleep now"),
        Ok(WatchVerdict::Continue)
    );
    assert_eq!(reminders.fires().len(), 1);
    assert_eq!(reminders.fires()[0].action, RuleAction::Report);
    assert!(reminders.fires()[0].judged);
    assert_eq!(reminders.fires()[0].inject, None);
    let mut interrupts = create(
        &set,
        &Gate::default(),
        turn(1),
        WatchBudget::Interrupts,
        &cfg,
        &data,
        EditStyle::Replace,
    );
    // A judged Interrupt fire never stops the sync watch: the stream stops
    // when the judged-lane consumer delivers the bool verdict, not here.
    assert_eq!(
        interrupts.feed(SourceKind::Text, "time to sleep now"),
        Ok(WatchVerdict::Continue)
    );
    assert_eq!(interrupts.fires()[0].action, RuleAction::Interrupt);
    assert!(interrupts.fires()[0].judged);
}

#[test]
fn interrupt_mode_default_follows_config() {
    let temp = TestDir::new();
    let data = temp.0.join("data");
    let workspace = temp.0.join("workspace");
    write_rule(&data.join("rules"), "r", "condition: MARK\n", "Body.");
    let cfg = RulesConfig {
        interrupt: InterruptMode::Never,
        ..RulesConfig::default()
    };
    let input = RuleBuildInput {
        records: &[],
        plugin_rules: &[],
        known_tools: &[],
        agent: "main",
    };
    let set = set_for(&input, &data, &workspace, &cfg);
    let mut watch = create(
        &set,
        &Gate::default(),
        turn(1),
        WatchBudget::Interrupts,
        &cfg,
        &data,
        EditStyle::Replace,
    );
    assert_eq!(
        watch.feed(SourceKind::Text, "MARK"),
        Ok(WatchVerdict::Continue)
    );
    assert_eq!(watch.fires()[0].action, RuleAction::Remind);
}
