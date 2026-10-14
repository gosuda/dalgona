use std::path::PathBuf;

use dal_core::RulesConfig;
use dal_core::ext::{InterruptMode, Name as PluginName, RepeatMode, RuleRecord, Site};

use super::*;

fn code_rule(plugin: &PluginName, name: &str, text: &str) -> RecordSource {
    RecordSource {
        plugin: plugin.clone(),
        site: Some(Site {
            path: PathBuf::from("ttsr-rules/rules.star"),
            line: 1,
            col: 1,
        }),
        record: RuleRecord {
            name: PluginName::parse(name).expect("valid rule name"),
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

#[test]
fn rule_scheme_serves_a_code_rule_body_and_refuses_unloaded_names() {
    let plugin = PluginName::parse("ttsr-rules").expect("valid plugin name");
    let records = [code_rule(&plugin, "stop-finish-the-work", "Finish it.")];
    let absent = std::env::temp_dir().join(format!("dal-rule-scheme-{}", std::process::id()));
    let rules = build::set_for(
        &RuleBuildInput {
            records: &records,
            plugin_rules: &[],
            known_tools: &[],
            agent: "main",
        },
        &absent,
        &absent,
        &RulesConfig::default(),
    );

    assert_eq!(
        rule_source(&rules, "stop-finish-the-work"),
        Some(b"Finish it.".to_vec())
    );
    assert_eq!(rule_source(&rules, "stop-nothing"), None);
}
