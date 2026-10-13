//! Rule scoping written in `plugin.star` reaches the registered rule record.

use std::collections::BTreeMap;

use dal_core::ext::{InterruptMode, RepeatMode, Scope};
use dal_core::{Name, RuleRecord};
use dal_star::{LoadError, LoadRoots, PluginSystem, PluginsConfig, load};

fn plugin_with_rule(rule: &str) -> String {
    format!(
        r#"load("@dal/v1", "dal")

plugin = dal.plugin(
    name = "style",
    version = "0.1.0",
    rules = {{"no-force-push": {rule}}},
)
"#
    )
}

fn load_rule(rule: &str) -> Result<Result<RuleRecord, LoadError>, Box<dyn std::error::Error>> {
    let data = tempfile::tempdir()?;
    let dir = data.path().join("plugins").join("style");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("plugin.star"), plugin_with_rule(rule))?;
    let roots = LoadRoots {
        data_root: data.path().to_path_buf(),
        bundled: Vec::new(),
    };
    let config = PluginsConfig {
        enabled: Vec::new(),
        limits: dal_core::PluginLimits::default(),
        configs: BTreeMap::new(),
    };
    let generation = match load(&roots, &config) {
        Ok(generation) => generation,
        Err(error) => return Ok(Err(error)),
    };
    let extensions = PluginSystem::new(generation, roots, config).extensions()?;
    let rule = extensions
        .iter()
        .flat_map(dal_agent::ext::Extension::rules)
        .next()
        .cloned()
        .ok_or("the plugin registered no rule")?;
    Ok(Ok(rule))
}

fn load_error(rule: &str) -> Result<String, Box<dyn std::error::Error>> {
    match load_rule(rule)? {
        Ok(record) => Err(format!("the rule loaded: {record:?}").into()),
        Err(error) => Ok(error.render().clone()),
    }
}

#[test]
fn written_scoping_reaches_the_rule_record() -> Result<(), Box<dyn std::error::Error>> {
    let rule = load_rule(
        r#"dal.rule(
        pattern = "push --force",
        text = "Do not force-push.",
        scope = ["thinking", "tool:exec"],
        interrupt_mode = "tool-only",
        repeat_mode = "after-gap",
        repeat_gap = 1000,
    )"#,
    )??;
    assert_eq!(
        rule.scope,
        Some(Scope {
            text: false,
            thinking: true,
            tool: true,
            named_tools: vec![Name::parse("exec")?],
        })
    );
    assert_eq!(rule.mode, Some(InterruptMode::ToolOnly));
    assert_eq!(rule.repeat_mode, Some(RepeatMode::AfterGap));
    assert_eq!(rule.repeat_gap, Some(1000));
    Ok(())
}

#[test]
fn unwritten_scoping_keeps_the_rules_defaults() -> Result<(), Box<dyn std::error::Error>> {
    let rule = load_rule(r#"dal.rule(pattern = "a", text = "b")"#)??;
    assert_eq!(
        (rule.scope, rule.mode, rule.repeat_mode, rule.repeat_gap),
        (None, None, None, None)
    );
    Ok(())
}

#[test]
fn invalid_scoping_fails_the_load_with_the_fix() -> Result<(), Box<dyn std::error::Error>> {
    let cases = [
        (
            r"scope = []",
            "rule `no-force-push`: scope needs one or more of text, thinking, tool, or tool:<name>",
        ),
        (
            r#"scope = ["tool:"]"#,
            "rule `no-force-push`: scope needs one or more of text, thinking, tool, or tool:<name>",
        ),
        (
            r#"interrupt_mode = "prose""#,
            "rule `no-force-push`: interrupt_mode \"prose\" is invalid; use always, prose-only, tool-only, or never",
        ),
        (
            r#"repeat_mode = "after_gap""#,
            "rule `no-force-push`: repeat_mode \"after_gap\" is invalid; use once or after-gap",
        ),
        (
            r"repeat_gap = 0",
            "rule `no-force-push`: repeat_gap 0 is invalid; use a whole number from 1 to 1000",
        ),
        (
            r"repeat_gap = 1001",
            "rule `no-force-push`: repeat_gap 1001 is invalid; use a whole number from 1 to 1000",
        ),
        (
            r"repeat_gap = 65537",
            "rule `no-force-push`: repeat_gap 65537 is invalid; use a whole number from 1 to 1000",
        ),
    ];
    for (field, expected) in cases {
        let rendered = load_error(&format!(r#"dal.rule(pattern = "a", text = "b", {field})"#))?;
        assert!(rendered.contains(expected), "{field}: {rendered}");
    }
    Ok(())
}
