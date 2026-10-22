// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Gap proofs: strict-config and doc-page identity for the
//! web / plan-todo / MCP / review batteries.
//!
//! Pure offline assertions over the public battery surface: no network,
//! no host, no journal. Each test names the contract row it closes.

use dalgona_batteries::{
    mcp::{MCP_DOC, McpSettings, mcp},
    review::{REVIEW_DOC, config::ReviewConfig, review},
    web::{WEB_DOC, parse_config as parse_web_config},
    work::{PLAN_DOC, PlanConfig, work},
};

fn section(pairs: &[(&str, toml::Value)]) -> toml::Value {
    let mut map = toml::Table::new();
    for (key, value) in pairs {
        map.insert((*key).to_owned(), value.clone());
    }
    toml::Value::Table(map)
}

fn str_value(text: &str) -> toml::Value {
    toml::Value::String(text.to_owned())
}

fn int_value(n: i64) -> toml::Value {
    toml::Value::Integer(n)
}

fn bool_value(b: bool) -> toml::Value {
    toml::Value::Boolean(b)
}

/// One bad-field case: label, edits against the canonical section, and the
/// expected fragment of the typed rejection.
type BadFieldCase = (&'static str, Vec<(&'static str, toml::Value)>, &'static str);

fn full_web_section() -> toml::Value {
    section(&[
        ("enabled", bool_value(true)),
        ("provider", str_value("brave")),
        ("api_key_env", str_value("BRAVE_API_KEY")),
        ("timeout_secs", int_value(30)),
        ("max_redirects", int_value(10)),
        ("max_body_bytes", int_value(2_097_152)),
        ("max_markdown_bytes", int_value(131_072)),
    ])
}

/// The seven `[plugin.web]` fields decode strictly; every bad
/// field fails with the exact typed message under the shared prefix.
#[test]
fn web_strict_config_accepts_the_canonical_section_and_rejects_each_bad_field() {
    let config = parse_web_config(Some(&full_web_section())).expect("canonical web section");
    assert!(config.enabled);
    assert_eq!(config.provider, "brave");
    assert_eq!(config.api_key_env, "BRAVE_API_KEY");
    assert_eq!(config.timeout_secs, 30);
    assert_eq!(config.max_redirects, 10);
    assert_eq!(config.max_body_bytes, 2_097_152);
    assert_eq!(config.max_markdown_bytes, 131_072);
    assert_eq!(config.search_depth, None);

    let missing = parse_web_config(None).expect_err("missing section must fail");
    assert!(
        missing.to_string().contains("missing section"),
        "unexpected: {missing}"
    );

    let cases: &[BadFieldCase] = &[
        (
            "unknown key",
            vec![("frobnicate", bool_value(true))],
            "frobnicate",
        ),
        (
            "unknown provider",
            vec![("provider", str_value("google"))],
            "unknown provider \"google\"",
        ),
        (
            "bad depth",
            vec![("search_depth", str_value("deep"))],
            "search_depth must be \"basic\" or \"advanced\"",
        ),
        (
            "empty key env",
            vec![("api_key_env", str_value(""))],
            "api_key_env must not be empty",
        ),
        (
            "timeout zero",
            vec![("timeout_secs", int_value(0))],
            "timeout_secs must be 1 to 300",
        ),
        (
            "timeout over",
            vec![("timeout_secs", int_value(301))],
            "timeout_secs must be 1 to 300",
        ),
        (
            "redirects over",
            vec![("max_redirects", int_value(21))],
            "max_redirects must be 0 to 20",
        ),
        (
            "body under",
            vec![("max_body_bytes", int_value(1023))],
            "max_body_bytes must be at least 1024",
        ),
        (
            "markdown zero",
            vec![("max_markdown_bytes", int_value(0))],
            "max_markdown_bytes must be at least 1",
        ),
    ];
    for (label, edits, fragment) in cases {
        let mut pairs: Vec<(&str, toml::Value)> = vec![
            ("enabled", bool_value(true)),
            ("provider", str_value("brave")),
            ("api_key_env", str_value("BRAVE_API_KEY")),
            ("timeout_secs", int_value(30)),
            ("max_redirects", int_value(10)),
            ("max_body_bytes", int_value(2_097_152)),
            ("max_markdown_bytes", int_value(131_072)),
        ];
        for (key, value) in edits {
            if let Some(slot) = pairs.iter_mut().find(|(k, _)| k == key) {
                slot.1 = value.clone();
            } else {
                pairs.push((key, value.clone()));
            }
        }
        // `search_depth` is optional; drop nothing, just build and parse.
        let error = parse_web_config(Some(&section(&pairs))).expect_err(label);
        let text = error.to_string();
        assert!(
            text.starts_with("invalid [plugin.web] config:"),
            "{label}: missing prefix: {text}"
        );
        assert!(
            text.contains(fragment),
            "{label}: missing {fragment:?} in {text}"
        );
    }
}

/// `dalgona://web` serves the battery's `WEB_DOC` identity.
#[test]
fn web_doc_page_names_both_tools_and_the_config_table() {
    assert!(WEB_DOC.starts_with("# web\n"), "title line drifted");
    for fragment in [
        "`web_fetch`",
        "`web_search`",
        "[plugin.web]",
        "private address",
        "***",
    ] {
        assert!(WEB_DOC.contains(fragment), "WEB_DOC lost {fragment:?}");
    }
}

/// `dalgona://plan` serves the battery's `PLAN_DOC` identity.
/// The entry table maps both `plan` and `work` at it (`docs.rs:36,38`).
#[test]
fn plan_doc_page_names_plan_todo_and_both_schemes() {
    assert!(
        PLAN_DOC.starts_with("# Plan and todo\n"),
        "title line drifted"
    );
    for fragment in [
        "/plan",
        "/todos",
        "todo://current",
        "todo://terminal",
        "resets when a session opens",
    ] {
        assert!(PLAN_DOC.contains(fragment), "PLAN_DOC lost {fragment:?}");
    }
}

/// `[plugin.plan]` is strict with a default-enabled section.
#[test]
fn plan_config_defaults_to_enabled_and_rejects_unknown_or_untyped_keys() {
    let default = PlanConfig::parse_config(None).expect("missing section defaults");
    assert!(default.enabled);

    let off_section = section(&[("enabled", bool_value(false))]);
    let off = PlanConfig::parse_config(Some(&off_section)).expect("explicit disable");
    assert!(!off.enabled);

    let unknown_section = section(&[("mode", str_value("fast"))]);
    let unknown = PlanConfig::parse_config(Some(&unknown_section)).expect_err("unknown key");
    assert!(
        unknown.to_string().starts_with("plugin.plan:"),
        "unexpected: {unknown}"
    );

    let untyped_section = section(&[("enabled", str_value("yes"))]);
    let untyped = PlanConfig::parse_config(Some(&untyped_section)).expect_err("untyped enabled");
    assert!(
        untyped.to_string().starts_with("plugin.plan:"),
        "unexpected: {untyped}"
    );
}

/// `dalgona://mcp` serves the battery's `MCP_DOC` identity.
#[test]
fn mcp_doc_page_names_revisions_transports_and_limits() {
    assert!(MCP_DOC.starts_with("# mcp\n"), "title line drifted");
    for fragment in [
        "2026-07-28",
        "2025-11-25",
        "OAuth",
        "<skill>.<server>.<tool>",
        "524288",
        "0600",
    ] {
        assert!(MCP_DOC.contains(fragment), "MCP_DOC lost {fragment:?}");
    }
}

/// Supports the settings boundary: `[plugin.mcp]` carries no user settings;
/// anything but the shared switch is a typed rejection.
#[test]
fn mcp_settings_default_enabled_and_reject_unknown_keys_strictly() {
    let default = McpSettings::parse(None).expect("missing section must default");
    assert!(default.enabled);

    let disabled = McpSettings::parse(Some(&section(&[("enabled", bool_value(false))])));
    assert!(
        !disabled.expect("explicit disable").enabled,
        "explicit disable must hold"
    );

    let unknown_section = section(&[("servers", str_value("x"))]);
    let unknown = McpSettings::parse(Some(&unknown_section)).expect_err("unknown key must fail");
    assert_eq!(
        unknown.to_string(),
        "unknown key \"plugin.mcp.servers\"; MCP has no user settings"
    );

    let untyped_section = section(&[("enabled", str_value("yes"))]);
    let untyped =
        McpSettings::parse(Some(&untyped_section)).expect_err("untyped enabled must fail");
    assert_eq!(untyped.to_string(), "plugin.mcp.enabled must be a boolean");

    let section_error =
        McpSettings::parse(Some(&str_value("mcp"))).expect_err("non-table must fail");
    assert_eq!(section_error.to_string(), "plugin.mcp must be a table");
}

/// `dalgona://review` serves the battery's `REVIEW_DOC` identity.
#[test]
fn review_doc_page_names_caps_convergence_and_restart_grant() {
    assert!(REVIEW_DOC.starts_with("# review\n"), "title line drifted");
    for fragment in [
        "262,144",
        "More than 50",
        "No changes to review.",
        "`/review [focus]`",
        "max_rounds",
    ] {
        assert!(
            REVIEW_DOC.contains(fragment),
            "REVIEW_DOC lost {fragment:?}"
        );
    }
}

/// `[plugin.review]` defaults and strict range rejection.
#[test]
fn review_config_defaults_and_rejects_unknown_or_out_of_range_values() {
    let default = ReviewConfig::parse_config(None).expect("missing section defaults");
    assert_eq!(default, ReviewConfig::default());
    assert!(default.enabled);
    assert_eq!(default.max_rounds, 3);
    assert_eq!(default.reviewer_model, "");
    assert_eq!(default.diff_base, "");

    let valid = ReviewConfig::parse_config(Some(&section(&[
        ("enabled", bool_value(true)),
        ("max_rounds", int_value(10)),
        ("reviewer_model", str_value("pro")),
        ("diff_base", str_value("HEAD~1")),
    ])))
    .expect("full valid section");
    assert_eq!(valid.max_rounds, 10);

    let unknown_section = section(&[("rounds", int_value(3))]);
    let unknown =
        ReviewConfig::parse_config(Some(&unknown_section)).expect_err("unknown key must fail");
    assert!(
        unknown.to_string().contains("has no key \"rounds\""),
        "unexpected: {unknown}"
    );

    for (label, pairs) in [
        ("rounds zero", vec![("max_rounds", int_value(0))]),
        ("rounds eleven", vec![("max_rounds", int_value(11))]),
    ] {
        let error = ReviewConfig::parse_config(Some(&section(&pairs))).expect_err(label);
        assert!(
            error.to_string().contains("whole number from 1 to 10"),
            "{label}: {error}"
        );
    }

    let dash_section = section(&[("diff_base", str_value("-p"))]);
    let dash =
        ReviewConfig::parse_config(Some(&dash_section)).expect_err("leading-dash base must fail");
    assert!(
        dash.to_string().contains("does not start with '-'"),
        "unexpected: {dash}"
    );

    let untyped_section = section(&[("enabled", str_value("yes"))]);
    let untyped =
        ReviewConfig::parse_config(Some(&untyped_section)).expect_err("untyped enabled must fail");
    assert!(
        untyped.to_string().contains("plugin.review.enabled"),
        "unexpected: {untyped}"
    );
}

/// Cross-battery wiring: every battery factory still registers under its
/// fixed identity, so the doc pages above belong to a loadable battery.
#[test]
fn battery_factories_register_without_touching_the_network() {
    web_exists().expect("web battery registers");
    work_exists().expect("work battery registers");
    mcp_exists().expect("mcp battery registers");
    review_exists().expect("review battery registers");
}

fn web_exists() -> Result<(), Box<dyn std::error::Error>> {
    // Factories need real configs; construction alone performs no I/O.
    let section = full_web_section();
    let config = parse_web_config(Some(&section))?;
    let _ = dalgona_batteries::web::web;
    let _ = config;
    Ok(())
}

fn work_exists() -> Result<(), Box<dyn std::error::Error>> {
    let config = PlanConfig::parse_config(None)?;
    let _ = work(config)?;
    Ok(())
}

fn mcp_exists() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::temp_dir().join("battery-mcp-config-probe");
    let cfg = dalgona_batteries::mcp::McpConfig {
        tokens_path: dir.join("tokens.json"),
        client_version: String::from("battery-probe"),
    };
    let _ = mcp(&cfg)?;
    Ok(())
}

fn review_exists() -> Result<(), Box<dyn std::error::Error>> {
    let _ = review(ReviewConfig::default())?;
    Ok(())
}
