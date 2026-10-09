// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use super::{OrchestrationConfig, parse_config};

fn table(values: impl IntoIterator<Item = (String, toml::Value)>) -> toml::Value {
    toml::Value::Table(values.into_iter().collect())
}

#[test]
fn orchestration_config_defaults_enable_every_battery() {
    let config = parse_config(None);
    assert!(config.is_ok());
    let config = config.unwrap_or_else(|error| panic!("default config failed: {error}"));
    assert!(config.loop_guard.enabled);
    assert!(config.sleep.enabled);
    assert!(config.monitor.enabled);
    assert!(config.inflight.enabled);
    assert!(config.goal.enabled);
    assert!(config.arbiter.enabled);
    assert!(config.agents.enabled);
    assert!(config.isolation.enabled);
}

#[test]
fn orchestration_config_accepts_a_disabled_battery_and_overrides() {
    let monitor = table([
        ("enabled".to_owned(), toml::Value::Boolean(false)),
        ("max_lines".to_owned(), toml::Value::Integer(1)),
    ]);
    let config = table([("monitor".to_owned(), monitor)]);
    let parsed = parse_config(Some(&config));
    assert!(parsed.is_ok());
    let parsed = parsed.unwrap_or_else(|error| panic!("valid config failed: {error}"));
    assert!(!parsed.monitor.enabled);
    assert_eq!(parsed.monitor.max_lines, 1);
    assert!(parsed.loop_guard.enabled);
}

#[test]
fn orchestration_config_rejects_unknown_root_and_nested_keys() {
    let unknown_root = table([("not_a_battery".to_owned(), toml::Value::Boolean(true))]);
    assert!(parse_config(Some(&unknown_root)).is_err());

    let unknown_monitor = table([("unexpected".to_owned(), toml::Value::Integer(1))]);
    let config = table([("monitor".to_owned(), unknown_monitor)]);
    assert!(parse_config(Some(&config)).is_err());
}

#[test]
fn orchestration_config_rejects_out_of_range_values() {
    let monitor = table([("max_lines".to_owned(), toml::Value::Integer(201))]);
    let config = table([("monitor".to_owned(), monitor)]);
    assert!(parse_config(Some(&config)).is_err());

    let agents = table([("max_runs".to_owned(), toml::Value::Integer(65))]);
    let config = table([("agents".to_owned(), agents)]);
    assert!(parse_config(Some(&config)).is_err());
}

#[test]
fn orchestration_config_keeps_saved_workflow_names_as_data() {
    let workflows = table([("review".to_owned(), toml::Value::String("steps".to_owned()))]);
    let config = table([("workflows".to_owned(), workflows)]);
    let parsed = parse_config(Some(&config));
    assert!(parsed.is_ok_and(|config: OrchestrationConfig| config.workflows.is_some()));
}

#[test]
fn orchestration_config_refuses_a_dependent_battery_turned_off() {
    let arbiter = table([("enabled".to_owned(), toml::Value::Boolean(false))]);
    let config = table([("arbiter".to_owned(), arbiter)]);
    assert_eq!(
        parse_config(Some(&config)).unwrap_err().to_string(),
        "orchestration: [plugin.orchestration.arbiter] cannot be off while goal is on."
    );

    let inflight = table([("enabled".to_owned(), toml::Value::Boolean(false))]);
    let config = table([
        ("inflight".to_owned(), inflight),
        (
            "goal".to_owned(),
            table([("enabled".to_owned(), toml::Value::Boolean(false))]),
        ),
        (
            "monitor".to_owned(),
            table([("enabled".to_owned(), toml::Value::Boolean(false))]),
        ),
        (
            "agents".to_owned(),
            toml::Value::Table(toml::map::Map::new()),
        ),
    ]);
    assert_eq!(
        parse_config(Some(&config)).unwrap_err().to_string(),
        "orchestration: [plugin.orchestration.inflight] cannot be off while agents is on."
    );
}

#[test]
fn orchestration_config_allows_disabling_the_watch_dogs_alone() {
    for battery in ["arbiter", "inflight"] {
        let battery_off = table([("enabled".to_owned(), toml::Value::Boolean(false))]);
        let config = toml::Value::Table(
            [
                (battery.to_owned(), battery_off),
                (
                    "goal".to_owned(),
                    table([("enabled".to_owned(), toml::Value::Boolean(false))]),
                ),
                (
                    "monitor".to_owned(),
                    table([("enabled".to_owned(), toml::Value::Boolean(false))]),
                ),
                (
                    "agents".to_owned(),
                    table([("enabled".to_owned(), toml::Value::Boolean(false))]),
                ),
            ]
            .into_iter()
            .collect(),
        );
        assert!(
            parse_config(Some(&config)).is_ok(),
            "{battery} alone must be disableable"
        );
    }
}
