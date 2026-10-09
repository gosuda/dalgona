//! Core layering, keys, and enum spelling tests.

use super::super::{
    ApprovalMode, Config, ConfigError, ConfigProduct, JudgeMode, Mode, ModeProjection, Screen,
};
use super::{DALGONA_DEFAULTS, load, parse_enum};
use crate::model::{ModelPrice, ThinkingLevel};
use std::path::Path;

#[test]
fn mode_spellings_are_closed_and_canonical() {
    for (spelling, mode) in [
        ("normal", Mode::Normal),
        ("eval-first", Mode::EvalFirst),
        ("eval-only", Mode::EvalOnly),
    ] {
        assert_eq!(parse_enum::<Mode>(spelling).unwrap(), mode);
        assert_eq!(mode.as_str(), spelling);
    }
    for spelling in ["NORMAL", "eval_first", "evalfirst", "other"] {
        assert!(parse_enum::<Mode>(spelling).is_err(), "accepted {spelling}");
    }
}

#[test]
fn other_policy_spellings_are_closed_snake_case() {
    for spelling in ["ask", "edits", "all"] {
        assert!(parse_enum::<ApprovalMode>(spelling).is_ok());
    }
    for spelling in ["inline", "fullscreen"] {
        assert!(parse_enum::<Screen>(spelling).is_ok());
    }
    for spelling in ["auto", "on", "off"] {
        assert!(parse_enum::<JudgeMode>(spelling).is_ok());
    }
    for spelling in ["ASK", "eval-first", "full_screen", "enabled"] {
        assert!(parse_enum::<ApprovalMode>(spelling).is_err());
        assert!(parse_enum::<Screen>(spelling).is_err());
        assert!(parse_enum::<JudgeMode>(spelling).is_err());
    }
}

#[test]
fn mode_projection_matches_all_modes() {
    assert_eq!(
        Mode::Normal.projection(),
        ModeProjection {
            eval_first: false,
            non_eval_only: false,
        }
    );
    assert_eq!(
        Mode::EvalFirst.projection(),
        ModeProjection {
            eval_first: true,
            non_eval_only: false,
        }
    );
    assert_eq!(
        Mode::EvalOnly.projection(),
        ModeProjection {
            eval_first: false,
            non_eval_only: true,
        }
    );
}

#[test]
fn config_layers_defaults_and_replaces_whole_values() {
    let user = r#"
mode = "eval-only"
model = "user/model"
plugins = ["user"]
disabled_batteries = ["web"]
experimental_batteries = []

[aliases]
fast = "family/updated"

[serve]
origins = ["https://user.example"]

[prices."family/model"]
cached_input = 0.0
output = 4.0
reasoning = 5.0
"#;
    let config = Config::load(
        ConfigProduct::Dalgona,
        Path::new(super::DATA_ROOT),
        DALGONA_DEFAULTS,
        Some(user),
    )
    .expect("valid product and user layers");
    assert_eq!(config.mode(), Mode::EvalOnly);
    assert_eq!(config.model(), Some("user/model"));
    assert_eq!(config.thinking(), ThinkingLevel::Low);
    assert_eq!(config.approval(), ApprovalMode::Edits);
    assert_eq!(config.screen(), Screen::Fullscreen);
    assert_eq!(config.theme.as_ref(), "dusk");
    assert!(config.sandbox);
    assert!(config.images);
    assert!(!config.motion);
    assert_eq!(config.compact_ratio.to_bits(), 0.75_f64.to_bits());
    assert_eq!(
        config.edit_style,
        crate::config::EditStyleInput::Scalar(Box::<str>::from("balanced"))
    );
    assert!(!config.guard.enabled);
    assert!(!config.search_symbols);
    assert!(config.section("judge").is_none());
    assert_eq!(config.plugins, vec![Box::<str>::from("user")]);
    assert_eq!(config.aliases.len(), 1);
    assert_eq!(
        config.aliases.get("fast").map(AsRef::as_ref),
        Some("family/updated")
    );
    assert!(!config.aliases.contains_key("slow"));
    assert_eq!(config.disabled_batteries, vec![Box::<str>::from("web")]);
    assert_eq!(config.experimental_batteries, []);
    assert_eq!(
        config.serve().origins,
        vec![Box::<str>::from("https://user.example")]
    );
    assert_eq!(config.serve().approval, ApprovalMode::Edits);
    assert_eq!(
        config.price_for_model("family/model"),
        Some(&ModelPrice {
            input: 1.0,
            cached_input: 0.0,
            output: 4.0,
            reasoning: 5.0,
            tiers: Box::default(),
        })
    );

    let without_user = Config::load(
        ConfigProduct::Dalgona,
        Path::new(super::DATA_ROOT),
        DALGONA_DEFAULTS,
        None,
    )
    .expect("valid defaults without user text");
    let with_empty_user = Config::load(
        ConfigProduct::Dalgona,
        Path::new(super::DATA_ROOT),
        DALGONA_DEFAULTS,
        Some(""),
    )
    .expect("empty user text is valid");
    assert_eq!(without_user, with_empty_user);
}

#[test]
fn config_rejects_unknown_and_invalid_values() {
    let error = load(ConfigProduct::Dalgon, "modle = 1").expect_err("unknown key");
    assert!(matches!(
        error,
        ConfigError::UnknownKey { key, suggestion, .. }
            if key.as_ref() == "modle" && suggestion.as_deref() == Some("mode")
    ));

    let error = load(ConfigProduct::Dalgon, "zzz = 1").expect_err("distant unknown key");
    assert!(matches!(
        error,
        ConfigError::UnknownKey {
            suggestion: None,
            ..
        }
    ));

    let error =
        load(ConfigProduct::Dalgon, "mode = \"jit\"").expect_err("unsupported execution mode");
    assert!(matches!(
        error,
        ConfigError::InvalidValue { key, value, expected }
            if key.as_ref() == "mode"
                && value.as_ref() == "jit"
                && expected.as_ref() == "normal, eval-first, eval-only"
    ));

    let error = load(
        ConfigProduct::Dalgon,
        "mode = \"normal\"\nmode = \"eval-first\"",
    )
    .expect_err("duplicate TOML keys");
    assert!(matches!(error, ConfigError::Syntax { line: Some(2), .. }));

    let error = load(ConfigProduct::Dalgon, "[serve]\ntoken = \"secret\"")
        .expect_err("serve.token is not a key");
    assert!(matches!(
        error,
        ConfigError::UnknownKey { key, .. } if key.as_ref() == "serve.token"
    ));

    let error = load(ConfigProduct::Dalgon, "[prices.model]\ninpt = 1.0")
        .expect_err("price rate keys are closed");
    assert!(matches!(
        error,
        ConfigError::UnknownKey { key, suggestion, .. }
            if key.as_ref() == "prices.model.inpt"
                && suggestion.as_deref() == Some("prices.model.input")
    ));
}

#[test]
fn config_closes_product_keys_and_checks_batteries_after_registration() {
    let error = load(ConfigProduct::Dalgon, "disabled_batteries = []")
        .expect_err("dalgon does not support battery keys");
    assert!(matches!(
        error,
        ConfigError::UnknownKey { key, .. }
            if key.as_ref() == "disabled_batteries"
    ));

    let empty = load(
        ConfigProduct::Dalgona,
        "disabled_batteries = []\nexperimental_batteries = []",
    )
    .expect("empty battery lists are valid");
    assert!(empty.validate_battery_names(&[]).is_ok());

    let error = load(ConfigProduct::Dalgona, "disabled_batteries = [\"\"]")
        .expect_err("empty battery names are invalid at parse time");
    assert!(matches!(error, ConfigError::InvalidValue { .. }));

    let config = load(
        ConfigProduct::Dalgona,
        "disabled_batteries = [\"ask\"]\nexperimental_batteries = [\"missing\"]",
    )
    .expect("non-empty battery names parse before registration");
    let error = config
        .validate_battery_names(&["ask", "search"])
        .expect_err("unknown battery names fail after registration");
    assert!(matches!(
        error,
        ConfigError::InvalidValue { key, value, expected }
            if key.as_ref() == "experimental_batteries"
                && value.as_ref() == "missing"
                && expected.as_ref() == "registered battery names: ask, search"
    ));

    let config = load(ConfigProduct::Dalgona, "disabled_batteries = [\"missing\"]")
        .expect("unknown names are deferred until registration");
    assert!(matches!(
        config.validate_battery_names(&["ask"]),
        Err(ConfigError::InvalidValue { key, .. })
            if key.as_ref() == "disabled_batteries"
    ));
}

#[test]
fn judge_non_table_renders_two_lines() {
    let error = load(ConfigProduct::Dalgon, "judge = 3").unwrap_err();
    assert_eq!(
        error.judge_lines(),
        Some([
            "judge must be a table".to_string(),
            "Use a [judge] table with gate, model, timeout_ms, max_concurrent, max_per_turn."
                .to_string(),
        ])
    );
}
