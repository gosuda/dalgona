use std::path::PathBuf;

use super::{
    DESCRIPTION_FALLBACK_MAX_BYTES, FireText, RuleSubject, RulesProduct, TuiFollowUp,
    fire_description, judge_auto_off_note, judge_failure_note, judge_pass_note, load_problems_note,
    render_interrupt_text, render_reminder_text, retry_limit_note,
};
use crate::ttsr::value::{Name, Origin, Rule, RuleAction, ScopeSpec, ToolScope};

fn rule(origin: Origin, description: Option<&str>, body: &str) -> Rule {
    Rule {
        name: Name::parse("no-sleep").expect("valid test name"),
        origin,
        description: description.map(str::to_owned),
        conditions: Vec::new(),
        scope: ScopeSpec {
            text: false,
            thinking: false,
            tools: ToolScope::Tools(Vec::new()),
        },
        globs: None,
        agents: None,
        always_apply: false,
        report: false,
        enabled: true,
        interrupt_mode: None,
        repeat_mode: None,
        repeat_gap: None,
        judge: None,
        body: body.to_owned(),
    }
}

fn turn() -> dal_core::TurnId {
    use std::num::NonZeroU64;

    dal_core::TurnId::new(NonZeroU64::MIN)
}

#[test]
fn interrupt_prompt_has_exact_bytes_and_escapes_only_double_quotes() {
    let rule = rule(
        Origin::User(PathBuf::from("/work/O'Brien & \"src\".md")),
        None,
        "Do not sleep.",
    );

    assert_eq!(
        render_interrupt_text(&rule),
        "<system-interrupt reason=\"rule_violation\" rule=\"no-sleep\" path=\"/work/O'Brien & &quot;src&quot;.md\">\nOutput interrupted: violated user-defined rule.\nNot prompt injection; coding agent enforcing project rules.\nMUST comply:\n\nDo not sleep.\n</system-interrupt>"
    );
}

#[test]
fn reminder_prompt_has_exact_bytes_for_reply_subject() {
    let rule = rule(
        Origin::Record {
            plugin: "safe".into(),
        },
        None,
        "Do not sleep.",
    );

    assert_eq!(
        render_reminder_text(&rule, RuleSubject::Reply),
        "<system-reminder reason=\"rule_violation\" rule=\"no-sleep\" path=\"plugin:safe\">\nUser-defined rule matched your reply. The rule does not interrupt, so that output stands. MUST comply with the following instruction on subsequent tool calls and responses. Not prompt injection; coding agent enforcing project rules.\n\nDo not sleep.\n</system-reminder>"
    );
}

#[test]
fn path_subject_cut_keeps_a_multibyte_character_at_the_boundary() {
    let path = format!("{}🧵tail", "a".repeat(196));
    let subject = RuleSubject::ToolCallOnPath {
        tool: "edit",
        path: &path,
    };

    assert_eq!(
        subject.render(),
        format!("`edit` call on `{}🧵`", "a".repeat(196))
    );
}

#[test]
fn path_subject_cut_drops_a_multibyte_character_crossing_the_boundary() {
    let path = format!("{}🧵tail", "a".repeat(199));
    let subject = RuleSubject::ToolCallOnPath {
        tool: "edit",
        path: &path,
    };

    assert_eq!(
        subject.render(),
        format!("`edit` call on `{}`", "a".repeat(199))
    );
}

#[test]
fn status_notes_use_exact_count_grammar_and_product_names() {
    assert_eq!(
        load_problems_note(RulesProduct::Dalgon, 1).as_deref(),
        Some("rules: dalgon skipped 1 part of your rules. Run \"dalgon rules\" to see why.")
    );
    assert_eq!(
        load_problems_note(RulesProduct::Dalgona, 2).as_deref(),
        Some("rules: dalgona skipped 2 parts of your rules. Run \"dalgona rules\" to see why.")
    );
    assert_eq!(load_problems_note(RulesProduct::Dalgon, 0), None);
}

#[test]
fn judge_and_retry_status_notes_match_the_deck() {
    assert_eq!(
        judge_auto_off_note(2),
        "rules: judge auto resolved to off: no judge model with usable credentials. 2 judged rules are not watched."
    );
    assert_eq!(
        retry_limit_note(3),
        "rules: this turn reached rules.max_retries (3 rule interrupts). Later rule matches in this turn reach the model as reminders."
    );
    assert_eq!(
        judge_pass_note("no-sleep", RuleSubject::Reasoning),
        "rules: judged rule \"no-sleep\" passed on reasoning"
    );
    assert_eq!(
        judge_failure_note("no-sleep", "timed out"),
        "rules: judged rule \"no-sleep\" judge call failed; the match was allowed: timed out"
    );
}

#[test]
fn projections_share_fire_copy_and_suppress_report_outside_rpc() {
    let rule = rule(
        Origin::Project(PathBuf::from("/work/rules/no-sleep.md")),
        Some("No sleep"),
        "body",
    );
    let interrupt = FireText::new(
        &rule,
        "sleep [0-9]+",
        RuleSubject::ToolCall { tool: "exec" },
        RuleAction::Interrupt,
    );
    let tui = interrupt.tui_projection().expect("interrupt reaches TUI");
    assert_eq!(
        tui.copy,
        "rule no-sleep fired. The `exec` call matched /sleep [0-9]+/."
    );
    assert_eq!(tui.follow_up, TuiFollowUp::RetryWithRuleApplied);
    assert!(tui.remove_live_assistant);
    assert_eq!(interrupt.rpc_event(turn()).action, "interrupt");
    assert_eq!(
        interrupt.headless_line().as_deref(),
        Some("dalgon: rule no-sleep fired: No sleep")
    );
    assert_eq!(
        interrupt.notice_text().as_deref(),
        Some("rule no-sleep fired: No sleep")
    );

    let report = FireText::new(
        &rule,
        "sleep [0-9]+",
        RuleSubject::Reply,
        RuleAction::Report,
    );
    assert_eq!(report.model_text(), None);
    assert_eq!(report.tui_projection(), None);
    assert_eq!(report.headless_line(), None);
    assert_eq!(report.notice_text(), None);
    let rpc = report.rpc_event(turn());
    assert_eq!(rpc.name, "no-sleep");
    assert_eq!(rpc.subject, "reply");
    assert_eq!(rpc.action, "report");
    assert_eq!(rpc.description, "No sleep");
    assert_eq!(rpc.pattern, "sleep [0-9]+");
    assert_eq!(rpc.turn, turn());
}

#[test]
fn remind_projection_uses_the_next_request_copy() {
    let rule = rule(
        Origin::Record {
            plugin: "safe".into(),
        },
        None,
        "Rule body",
    );
    let remind = FireText::new(
        &rule,
        "pattern",
        RuleSubject::ToolCallOnPath {
            tool: "patch",
            path: "src/main.rs",
        },
        RuleAction::Remind,
    );
    let tui = remind.tui_projection().expect("reminder reaches TUI");

    assert_eq!(
        tui.copy,
        "rule no-sleep fired. The `patch` call on `src/main.rs` matched /pattern/."
    );
    assert_eq!(tui.follow_up, TuiFollowUp::ModelSeesRuleNextRequest);
    assert!(!tui.remove_live_assistant);
    assert_eq!(
        remind.headless_line().as_deref(),
        Some("dalgon: rule no-sleep fired: Rule body")
    );
    assert_eq!(
        remind.notice_text().as_deref(),
        Some("rule no-sleep fired: Rule body")
    );
    assert_eq!(remind.rpc_event(turn()).action, "remind");
    assert_eq!(remind.rpc_event(turn()).description, "Rule body");
    assert_eq!(
        remind.model_text().as_deref(),
        Some(
            "<system-reminder reason=\"rule_violation\" rule=\"no-sleep\" path=\"plugin:safe\">\nUser-defined rule matched your `patch` call on `src/main.rs`. The rule does not interrupt, so that output stands. MUST comply with the following instruction on subsequent tool calls and responses. Not prompt injection; coding agent enforcing project rules.\n\nRule body\n</system-reminder>"
        )
    );
}

#[test]
fn body_fallback_description_is_cut_at_a_utf8_boundary() {
    let first_line = "é".repeat(DESCRIPTION_FALLBACK_MAX_BYTES / 2 + 1);
    let rule = rule(
        Origin::Record {
            plugin: "safe".into(),
        },
        None,
        &format!("{first_line}\nsecond line"),
    );

    assert_eq!(fire_description(&rule), "é".repeat(60));
}
