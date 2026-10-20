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
fn orchestration_config_accepts_the_isolation_key() {
    let isolation = table([("enabled".to_owned(), toml::Value::Boolean(false))]);
    let config = table([("isolation".to_owned(), isolation)]);
    let parsed = parse_config(Some(&config));
    assert!(parsed.is_ok());
    let parsed = parsed.unwrap_or_else(|error| panic!("valid config failed: {error}"));
    assert!(!parsed.isolation.enabled);
}

#[test]
fn orchestration_config_accepts_only_a_boolean_isolation_value() {
    let isolation = table([("enabled".to_owned(), toml::Value::Integer(1))]);
    let config = table([("isolation".to_owned(), isolation)]);
    assert!(parse_config(Some(&config)).is_err());
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

/// Splits a monitor row such as `50, clamp 1..=200` into its default, low edge, and high edge.
fn monitor_row(name: &str) -> (u64, u64, u64) {
    let rows = super::constants::constant_rows();
    let row = rows
        .iter()
        .find(|row| row.name == name)
        .unwrap_or_else(|| panic!("the constants table has no {name:?} row"));
    let (head, clamp) = row
        .value
        .split_once(", clamp ")
        .unwrap_or_else(|| panic!("{name:?} states no clamp: {:?}", row.value));
    let default = head
        .trim_end_matches(" ms")
        .parse()
        .unwrap_or_else(|_| panic!("{name:?} default is not a number: {head:?}"));
    let (low, high) = clamp
        .split_once("..=")
        .unwrap_or_else(|| panic!("{name:?} clamp is not low..=high: {clamp:?}"));
    (
        default,
        low.parse()
            .unwrap_or_else(|_| panic!("{name:?} low {low:?}")),
        high.parse()
            .unwrap_or_else(|_| panic!("{name:?} high {high:?}")),
    )
}

fn monitor_with(
    key: &str,
    value: u64,
) -> Result<OrchestrationConfig, super::OrchestrationConfigError> {
    let value = i64::try_from(value).map_or(toml::Value::Integer(i64::MAX), toml::Value::Integer);
    let monitor = table([(key.to_owned(), value)]);
    parse_config(Some(&table([("monitor".to_owned(), monitor)])))
}

#[test]
fn the_page_states_the_monitor_clamps_the_config_enforces() {
    let defaults = parse_config(None).expect("the default orchestration config decodes");
    for (row, key, configured_default) in [
        (
            "Monitor coalesce",
            "coalesce_ms",
            defaults.monitor.coalesce_ms,
        ),
        (
            "Monitor rate limit",
            "rate_limit_ms",
            defaults.monitor.rate_limit_ms,
        ),
        (
            "Monitor lines per injection",
            "max_lines",
            u64::try_from(defaults.monitor.max_lines).expect("fits"),
        ),
        (
            "Monitor characters per injection",
            "max_chars",
            u64::try_from(defaults.monitor.max_chars).expect("fits"),
        ),
        (
            "Monitor wake budget",
            "wake_budget",
            u64::from(defaults.monitor.wake_budget),
        ),
    ] {
        let (page_default, low, high) = monitor_row(row);
        assert_eq!(page_default, configured_default, "{row}: page default");
        for edge in [low, high] {
            assert!(
                monitor_with(key, edge).is_ok(),
                "{key} = {edge} is inside the clamp the page states"
            );
        }
        assert!(
            monitor_with(key, low - 1).is_err(),
            "{key} = {} is below the stated clamp",
            low - 1
        );
        assert!(
            monitor_with(key, high + 1).is_err(),
            "{key} = {} is above the stated clamp",
            high + 1
        );
    }
}

#[test]
fn orchestration_mirror_matches_the_served_page() -> Result<(), std::io::Error> {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let mirror = std::fs::read_to_string(format!("{manifest}/../docs/orchestration.md"))?;
    let served = super::orchestration_served_page();
    assert_eq!(mirror, served, "dalgona/docs/orchestration.md drifted");
    Ok(())
}
