#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests fail loudly"
)]

//! Config close-out: config schema, layering, overrides,
//! strict validation, and product-closed battery keys through the public API.
//!
//! Existing unit suites already prove the happy paths (`config/tests/core.rs`,
//! `prices.rs`, `rules.rs`, `edit_style.rs`); every test here pins an
//! uncovered boundary or state-transition edge.

use dal_core::{
    ApprovalMode, Config, ConfigError, ConfigOverrides, ConfigProduct, EditStyleInput, Mode, Screen,
};
use std::path::Path;

const DATA_ROOT: &str = "/data-test";

fn load(product: ConfigProduct, user_toml: &str) -> Result<Config, ConfigError> {
    Config::load(product, Path::new(DATA_ROOT), "", Some(user_toml))
}

fn invalid_key(user_toml: &str) -> (String, String, String) {
    match load(ConfigProduct::Dalgon, user_toml).expect_err("value must be rejected") {
        ConfigError::InvalidValue {
            key,
            value,
            expected,
        } => (key.into(), value.into(), expected.into()),
        error => panic!("expected InvalidValue, got {error:?}"),
    }
}

// ---------------------------------------------------------------------------
// Mode projection
// ---------------------------------------------------------------------------

#[test]
fn mode_projection_covers_all_modes() {
    for (mode, spelling, eval_first, non_eval_only) in [
        (Mode::Normal, "normal", false, false),
        (Mode::EvalFirst, "eval-first", true, false),
        (Mode::EvalOnly, "eval-only", false, true),
    ] {
        assert_eq!(mode.as_str(), spelling);
        let projection = mode.projection();
        assert_eq!(projection.eval_first, eval_first, "{spelling}");
        assert_eq!(projection.non_eval_only, non_eval_only, "{spelling}");
        let round_tripped = load(ConfigProduct::Dalgon, &format!("mode = \"{spelling}\""))
            .expect("canonical spelling loads");
        assert_eq!(round_tripped.mode(), mode);
        assert_eq!(round_tripped.mode().projection(), projection);
    }
}

// ---------------------------------------------------------------------------
// Three-layer loader
// ---------------------------------------------------------------------------

#[test]
fn layers_replace_whole_values_not_merge() {
    let defaults = "plugins = [\"a\", \"b\"]\ndisabled_batteries = [\"ask\"]\n\
        [aliases]\nfast = \"family/old\"\nslow = \"family/gone\"\n\
        [serve]\norigins = [\"https://one.example\"]\n";
    let user = "plugins = [\"c\"]\ndisabled_batteries = []\n\
        [aliases]\nfast = \"family/new\"\n\
        [serve]\norigins = [\"https://two.example\"]\n";
    let config = Config::load(
        ConfigProduct::Dalgona,
        Path::new(DATA_ROOT),
        defaults,
        Some(user),
    )
    .expect("layers merge");
    assert_eq!(config.plugins(), [Box::<str>::from("c")]);
    assert_eq!(config.disabled_batteries(), []);
    assert_eq!(config.aliases().len(), 1, "aliases replace whole");
    assert_eq!(
        config.aliases().get("fast").map(AsRef::as_ref),
        Some("family/new")
    );
    assert_eq!(
        config.serve().origins,
        [Box::<str>::from("https://two.example")]
    );
}

#[test]
fn serve_approval_inherits_top_level_until_explicit() {
    let builtin = load(ConfigProduct::Dalgon, "").expect("builtins load");
    assert_eq!(builtin.approval(), ApprovalMode::Ask);
    assert_eq!(builtin.serve().approval, ApprovalMode::Ask);

    let inherited = load(ConfigProduct::Dalgon, "approval = \"all\"").expect("top approval");
    assert_eq!(inherited.approval(), ApprovalMode::All);
    assert_eq!(inherited.serve().approval, ApprovalMode::All);

    let explicit = load(
        ConfigProduct::Dalgon,
        "approval = \"all\"\n[serve]\napproval = \"ask\"",
    )
    .expect("explicit serve approval");
    assert_eq!(explicit.approval(), ApprovalMode::All);
    assert_eq!(explicit.serve().approval, ApprovalMode::Ask);
}

#[test]
fn prices_merge_by_model_and_rate_field() {
    let config = Config::load(
        ConfigProduct::Dalgon,
        Path::new(DATA_ROOT),
        "[prices.m]\ninput = 1.0\ncached_input = 0.5",
        Some("[prices.m]\noutput = 2.0\nreasoning = 3.0"),
    )
    .expect("rate fields merge across layers");
    let price = config.price_for_model("m").expect("merged price");
    assert_eq!(
        (
            price.input,
            price.cached_input,
            price.output,
            price.reasoning
        ),
        (1.0, 0.5, 2.0, 3.0)
    );

    let zeroed = Config::load(
        ConfigProduct::Dalgon,
        Path::new(DATA_ROOT),
        "[prices.m]\ninput = 1.0\ncached_input = 0.5\noutput = 2.0\nreasoning = 3.0",
        Some("[prices.m]\ncached_input = 0.0"),
    )
    .expect("explicit zero replaces");
    assert_eq!(
        zeroed.price_for_model("m").expect("price").cached_input,
        0.0
    );

    assert!(
        config.price_for_model("absent").is_none(),
        "missing table is not zero"
    );
}

#[test]
fn empty_absent_and_comment_only_user_toml_are_equal() {
    let absent = Config::load(ConfigProduct::Dalgon, Path::new(DATA_ROOT), "", None)
        .expect("absent user TOML");
    let empty = Config::load(ConfigProduct::Dalgon, Path::new(DATA_ROOT), "", Some(""))
        .expect("empty user TOML");
    let comment_only = Config::load(
        ConfigProduct::Dalgon,
        Path::new(DATA_ROOT),
        "",
        Some("# just a comment\n"),
    )
    .expect("comment-only user TOML");
    assert_eq!(absent, empty);
    assert_eq!(absent, comment_only);
}

#[test]
fn dotted_keys_and_inline_tables_parse_as_nested() {
    let dotted = load(ConfigProduct::Dalgon, "serve.port = 9000").expect("dotted key");
    assert_eq!(dotted.serve().port, 9000);

    let inline = load(ConfigProduct::Dalgon, "serve = { port = 9001 }").expect("inline table");
    assert_eq!(inline.serve().port, 9001);

    let dotted_rule = load(ConfigProduct::Dalgon, "rules.repeat_gap = 42").expect("dotted rule");
    assert_eq!(dotted_rule.rules().repeat_gap, 42);
}

// ---------------------------------------------------------------------------
// In-memory overrides
// ---------------------------------------------------------------------------

#[test]
fn overrides_apply_each_field_independently() {
    type OverrideCase = (ConfigOverrides, Box<dyn Fn(&Config)>);
    let base = load(
        ConfigProduct::Dalgon,
        "model = \"base/model\"\nmode = \"normal\"\nthinking = \"medium\"\n\
         approval = \"ask\"\nscreen = \"inline\"\nsandbox = false",
    )
    .expect("base config");
    let cases: Vec<OverrideCase> = vec![
        (
            ConfigOverrides {
                model: Some(Box::<str>::from("flag/model")),
                ..ConfigOverrides::default()
            },
            Box::new(|config: &Config| assert_eq!(config.model(), Some("flag/model"))),
        ),
        (
            ConfigOverrides {
                mode: Some(Mode::EvalOnly),
                ..ConfigOverrides::default()
            },
            Box::new(|config: &Config| assert_eq!(config.mode(), Mode::EvalOnly)),
        ),
        (
            ConfigOverrides {
                thinking: Some(dal_core::ThinkingLevel::Max),
                ..ConfigOverrides::default()
            },
            Box::new(|config: &Config| assert_eq!(config.thinking(), dal_core::ThinkingLevel::Max)),
        ),
        (
            ConfigOverrides {
                approval: Some(ApprovalMode::All),
                ..ConfigOverrides::default()
            },
            Box::new(|config: &Config| assert_eq!(config.approval(), ApprovalMode::All)),
        ),
        (
            ConfigOverrides {
                screen: Some(Screen::Fullscreen),
                ..ConfigOverrides::default()
            },
            Box::new(|config: &Config| assert_eq!(config.screen(), Screen::Fullscreen)),
        ),
        (
            ConfigOverrides {
                sandbox: Some(true),
                ..ConfigOverrides::default()
            },
            Box::new(|config: &Config| assert!(config.sandbox())),
        ),
    ];
    for (overrides, check) in cases {
        let overlaid = base.with_overrides(&overrides);
        check(&overlaid);
    }
    // Untouched fields keep base values under a single-field overlay.
    let overlaid = base.with_overrides(&ConfigOverrides {
        mode: Some(Mode::EvalFirst),
        ..ConfigOverrides::default()
    });
    assert_eq!(overlaid.model(), Some("base/model"));
    assert_eq!(overlaid.approval(), ApprovalMode::Ask);
    assert_eq!(overlaid.screen(), Screen::Inline);
    assert!(!overlaid.sandbox());
}

#[test]
fn overrides_never_mutate_receiver_or_layered_state() {
    let base = load(
        ConfigProduct::Dalgon,
        "model = \"base/model\"\napproval = \"edits\"",
    )
    .expect("base config");
    let snapshot = base.clone();
    let overlaid = base.with_overrides(&ConfigOverrides {
        model: Some(Box::<str>::from("flag/model")),
        approval: Some(ApprovalMode::All),
        ..ConfigOverrides::default()
    });
    assert_eq!(base, snapshot, "receiver unchanged");
    assert_eq!(overlaid.model(), Some("flag/model"));
    assert_eq!(
        overlaid.serve().approval,
        ApprovalMode::Edits,
        "layered serve state untouched"
    );
    assert_eq!(base.with_overrides(&ConfigOverrides::default()), base);
}

#[test]
fn overrides_cannot_bypass_closed_set_typing() {
    // Overrides take typed values, so an invalid spelling cannot be expressed;
    // and a broken file still fails before any overlay applies.
    let error = Config::load(
        ConfigProduct::Dalgon,
        Path::new(DATA_ROOT),
        "",
        Some("mode = \"jit\""),
    )
    .expect_err("invalid file rejected before overrides");
    assert!(matches!(&error, ConfigError::InvalidValue { .. }));
    let base = load(ConfigProduct::Dalgon, "mode = \"normal\"").expect("base config");
    let overlaid = base.with_overrides(&ConfigOverrides {
        mode: Some(Mode::EvalOnly),
        ..ConfigOverrides::default()
    });
    assert_eq!(overlaid.mode(), Mode::EvalOnly);
    assert_eq!(base.mode(), Mode::Normal);
}

// ---------------------------------------------------------------------------
// Strict validation
// ---------------------------------------------------------------------------

#[test]
fn mode_rejects_empty_case_and_type_variants() {
    for spelling in ["", "Normal", "NORMAL", "eval_first", "evalfirst", "jit"] {
        let (key, value, expected) = invalid_key(&format!("mode = \"{spelling}\""));
        assert_eq!(key, "mode", "{spelling}");
        assert_eq!(value, spelling, "{spelling}");
        assert_eq!(expected, "normal, eval-first, eval-only", "{spelling}");
    }
    let (key, _, _) = invalid_key("mode = 1");
    assert_eq!(key, "mode");
}

#[test]
fn scalar_wrong_types_are_typed_errors() {
    for (document, key) in [
        ("sandbox = \"yes\"", "sandbox"),
        ("images = 1", "images"),
        ("motion = \"true\"", "motion"),
        ("theme = 42", "theme"),
        ("model = 42", "model"),
        ("compact_ratio = \"0.85\"", "compact_ratio"),
        ("approval = true", "approval"),
        ("screen = 1", "screen"),
        ("thinking = 1", "thinking"),
        ("guard = true", "guard"),
        ("rules = 1", "rules"),
        ("serve = 1", "serve"),
        ("tui = 1", "tui"),
        ("aliases = \"x\"", "aliases"),
    ] {
        let (found, _, _) = invalid_key(document);
        assert_eq!(found, key, "{document}");
    }
}

#[test]
fn empty_model_and_theme_are_invalid() {
    let (key, value, _) = invalid_key("model = \"\"");
    assert_eq!((key.as_str(), value.as_str()), ("model", ""));
    let (key, value, _) = invalid_key("theme = \"\"");
    assert_eq!((key.as_str(), value.as_str()), ("theme", ""));
    let (key, _, _) = invalid_key("[aliases]\nfast = \"\"");
    assert_eq!(key, "aliases.fast");
}

#[test]
fn serve_numeric_and_string_boundaries() {
    for port in ["0", "65535"] {
        assert!(
            load(ConfigProduct::Dalgon, &format!("[serve]\nport = {port}")).is_ok(),
            "{port}"
        );
    }
    for port in ["-1", "65536", "99999999999", "\"7437\"", "7437.0", "true"] {
        let (key, _, expected) = invalid_key(&format!("[serve]\nport = {port}"));
        assert_eq!(key, "serve.port", "{port}");
        assert_eq!(expected, "integer 0..=65535", "{port}");
    }
    let (key, _, _) = invalid_key("[serve]\nbind = \"\"");
    assert_eq!(key, "serve.bind");
    let (key, _, _) = invalid_key("[serve]\ntoken_file = 1");
    assert_eq!(key, "serve.token_file");
    let (key, _, _) = invalid_key("[serve]\napproval = \"sometimes\"");
    assert_eq!(key, "serve.approval");
    for origins in ["\"x\"", "[1]", "[\"ok\", 2]"] {
        let (key, _, _) = invalid_key(&format!("[serve]\norigins = {origins}"));
        assert_eq!(key, "serve.origins", "{origins}");
    }
    let defaults = load(ConfigProduct::Dalgon, "").expect("builtins");
    assert_eq!(defaults.serve().origins, []);
    assert_eq!(defaults.serve().bind.as_ref(), "127.0.0.1");
    assert_eq!(defaults.serve().port, 7437);
    assert_eq!(
        defaults.serve().token_file,
        Path::new(DATA_ROOT).join("serve.token")
    );
}

#[test]
fn compact_ratio_finite_inclusive_bounds() {
    for ratio in ["0.5", "0.95", "0.85"] {
        assert!(
            load(ConfigProduct::Dalgon, &format!("compact_ratio = {ratio}")).is_ok(),
            "{ratio}"
        );
    }
    for ratio in [
        "0.49", "0.96", "nan", "inf", "-inf", "-0.5", "0", "1", "\"0.85\"",
    ] {
        let (key, _, expected) = invalid_key(&format!("compact_ratio = {ratio}"));
        assert_eq!(key, "compact_ratio", "{ratio}");
        assert_eq!(expected, "finite number in 0.5..=0.95", "{ratio}");
    }
}

#[test]
fn price_rates_must_be_finite_non_negative() {
    for rate in ["-1.0", "nan", "inf", "-inf", "\"1.0\""] {
        let document = format!(
            "[prices.m]\ninput = {rate}\ncached_input = 0.0\noutput = 0.0\nreasoning = 0.0"
        );
        let (key, _, expected) = invalid_key(&document);
        assert_eq!(key, "prices.m.input", "{rate}");
        assert_eq!(expected, "finite non-negative USD rate", "{rate}");
    }
    let huge = load(
        ConfigProduct::Dalgon,
        "[prices.m]\ninput = 1e308\ncached_input = 1e308\noutput = 1e308\nreasoning = 1e308",
    )
    .expect("huge finite rates are valid");
    assert_eq!(huge.price_for_model("m").expect("price").input, 1e308);
}

#[test]
fn incomplete_price_table_is_not_zero() {
    let error = load(ConfigProduct::Dalgon, "[prices.m]\ninput = 0.0").expect_err("incomplete");
    assert!(
        matches!(
            &error,
            ConfigError::InvalidValue { key, value, expected }
                if key.as_ref() == "prices.m"
                    && value.as_ref() == "missing cached_input, output, reasoning"
                    && expected.as_ref() == "all four rates: input, cached_input, output, reasoning"
        ),
        "{error:?}"
    );
    let error = load(ConfigProduct::Dalgon, "prices = 1").expect_err("non-table prices");
    assert!(
        matches!(&error, ConfigError::InvalidValue { key, .. } if key.as_ref().starts_with("prices")),
        "{error:?}"
    );
}

#[test]
fn unknown_nested_keys_use_dotted_paths() {
    for (document, key) in [
        ("[serve]\ntoken = \"secret\"", "serve.token"),
        ("[serve]\nbogus = 1", "serve.bogus"),
        ("[rules]\nbogus = 1", "rules.bogus"),
        ("[tui]\nbogus = true", "tui.bogus"),
        ("[prices.m]\nbogus = 1.0", "prices.m.bogus"),
    ] {
        let error = load(ConfigProduct::Dalgon, document).expect_err("unknown nested key");
        assert!(
            matches!(&error, ConfigError::UnknownKey { key: found, .. } if found.as_ref() == key),
            "{document}: {error:?}"
        );
    }
    let error = load(ConfigProduct::Dalgon, "[tui]\ndiagrams = \"yes\"").expect_err("bad type");
    assert!(
        matches!(&error, ConfigError::InvalidValue { key, .. } if key.as_ref() == "tui.diagrams"),
        "{error:?}"
    );
    let error = load(ConfigProduct::Dalgon, "[guard]\nbogus = true").expect_err("guard key");
    assert!(
        matches!(&error, ConfigError::UnknownGuardKey { .. }),
        "{error:?}"
    );
}

#[test]
fn suggestion_distance_and_table_order_ties() {
    let error = load(ConfigProduct::Dalgon, "modle = 1").expect_err("distance 1");
    assert!(
        matches!(
            &error,
            ConfigError::UnknownKey { key, suggestion, .. }
                if key.as_ref() == "modle" && suggestion.as_deref() == Some("mode")
        ),
        "{error:?}"
    );
    let error = load(ConfigProduct::Dalgon, "mdoe = 1").expect_err("distance 2");
    assert!(
        matches!(
            &error,
            ConfigError::UnknownKey { suggestion, .. } if suggestion.as_deref() == Some("mode")
        ),
        "{error:?}"
    );
    // "mode1" ties "mode" and "model" at distance 1; table order wins.
    let error = load(ConfigProduct::Dalgon, "mode1 = 1").expect_err("tie");
    assert!(
        matches!(
            &error,
            ConfigError::UnknownKey { suggestion, .. } if suggestion.as_deref() == Some("mode")
        ),
        "{error:?}"
    );
    let error = load(ConfigProduct::Dalgon, "zzz = 1").expect_err("too far");
    assert!(
        matches!(
            &error,
            ConfigError::UnknownKey {
                suggestion: None,
                ..
            }
        ),
        "{error:?}"
    );
}

#[test]
fn duplicate_keys_are_syntax_errors_with_line() {
    let error = load(
        ConfigProduct::Dalgon,
        "mode = \"normal\"\nmode = \"eval-first\"",
    )
    .expect_err("top-level duplicate");
    assert!(
        matches!(&error, ConfigError::Syntax { line: Some(_), .. }),
        "{error:?}"
    );

    let error =
        load(ConfigProduct::Dalgon, "[serve]\nport = 1\nport = 2").expect_err("nested duplicate");
    assert!(
        matches!(&error, ConfigError::Syntax { line: Some(3), .. }),
        "{error:?}"
    );
}

#[test]
fn corrupt_defaults_are_internal_not_user_errors() {
    for defaults in ["mode = [", "bogus_key = 1", "mode = \"jit\""] {
        let error = Config::load(
            ConfigProduct::Dalgon,
            Path::new(DATA_ROOT),
            defaults,
            Some(""),
        )
        .expect_err("bad compiled defaults");
        assert!(
            matches!(&error, ConfigError::DefaultsInvalid { .. }),
            "{defaults}: {error:?}"
        );
    }
    // The same text as user TOML is a user error, not internal.
    let error = load(ConfigProduct::Dalgon, "bogus_key = 1").expect_err("user unknown key");
    assert!(
        matches!(&error, ConfigError::UnknownKey { .. }),
        "{error:?}"
    );
}

#[test]
fn aliases_and_plugins_tables_are_typed() {
    for (document, key) in [
        ("plugins = \"x\"", "plugins"),
        ("plugins = [\"\"]", "plugins"),
        ("plugins = [1]", "plugins"),
        ("plugins = [\"ok\", 2]", "plugins"),
    ] {
        let (found, _, expected) = invalid_key(document);
        assert_eq!(found, key, "{document}");
        assert_eq!(expected, "array of non-empty strings", "{document}");
    }
    let error = load(ConfigProduct::Dalgon, "[[plugins]]").expect_err("array of tables");
    assert!(
        matches!(&error, ConfigError::InvalidValue { key, .. } if key.as_ref() == "plugins"),
        "{error:?}"
    );
}

// ---------------------------------------------------------------------------
// Closed product keys and batteries
// ---------------------------------------------------------------------------

#[test]
fn battery_keys_closed_by_product_and_deferred_to_registration() {
    for key in ["disabled_batteries", "experimental_batteries"] {
        let error = load(ConfigProduct::Dalgon, &format!("{key} = []")).expect_err("dalgon closed");
        assert!(
            matches!(&error, ConfigError::UnknownKey { key: found, .. } if found.as_ref() == key),
            "{key}: {error:?}"
        );
        let empty = load(ConfigProduct::Dalgona, &format!("{key} = []")).expect("dalgona open");
        assert!(empty.validate_battery_names(&[]).is_ok());
        let error = load(ConfigProduct::Dalgona, &format!("{key} = [\"\"]"))
            .expect_err("empty battery name");
        assert!(
            matches!(&error, ConfigError::InvalidValue { key: found, .. } if found.as_ref() == key),
            "{key}: {error:?}"
        );
    }

    let config = load(
        ConfigProduct::Dalgona,
        "disabled_batteries = [\"ask\"]\nexperimental_batteries = [\"missing\"]",
    )
    .expect("unknown names parse before registration");
    let error = config
        .validate_battery_names(&["ask", "search"])
        .expect_err("unregistered name fails after registration");
    assert!(
        matches!(
            &error,
            ConfigError::InvalidValue { key, value, expected }
                if key.as_ref() == "experimental_batteries"
                    && value.as_ref() == "missing"
                    && expected.as_ref() == "registered battery names: ask, search"
        ),
        "{error:?}"
    );
    let config =
        load(ConfigProduct::Dalgona, "disabled_batteries = [\"missing\"]").expect("deferred check");
    assert!(config.validate_battery_names(&["ask", "missing"]).is_ok());
    assert!(matches!(
        config.validate_battery_names(&["ask"]),
        Err(ConfigError::InvalidValue { key, .. }) if key.as_ref() == "disabled_batteries"
    ));
}

// ---------------------------------------------------------------------------
// Spec drift pins (code wins)
// ---------------------------------------------------------------------------

#[test]
fn edit_style_aliases_preserved_raw_at_load() {
    // The spec step says aliases resolve at load; the code keeps raw input and
    // leaves validation to the patch consumer.
    for style in [
        "simple",
        "replace",
        "balanced",
        "anchor",
        "strict",
        "hashline",
        "apply_patch",
    ] {
        let config =
            load(ConfigProduct::Dalgon, &format!("edit_style = \"{style}\"")).expect("loads");
        assert_eq!(
            config.edit_style(),
            &EditStyleInput::Scalar(Box::<str>::from(style))
        );
    }
    let config = load(
        ConfigProduct::Dalgon,
        "[edit_style]\ndefault = \"anchor\"\n\"*kimi*\" = \"replace\"\n",
    )
    .expect("table form");
    assert!(
        matches!(config.edit_style(), EditStyleInput::Table(rows) if rows.len() == 2),
        "{:?}",
        config.edit_style()
    );
}

#[test]
fn diagnostics_name_dal_toml() {
    // Current diagnostics say `dal.toml`; the spec text's `config.toml` is drift.
    let error = load(ConfigProduct::Dalgon, "[rules]\nrepeat_gap = 0").expect_err("bad repeat_gap");
    assert_eq!(
        error.rules_lines("dalgon"),
        Some([
            "dalgon: dal.toml: rules.repeat_gap 0 is invalid".to_string(),
            "Use a whole number from 1 to 1000.".to_string(),
        ])
    );
    let error = load(ConfigProduct::Dalgon, "judge = 3").expect_err("non-table judge");
    assert_eq!(
        error.judge_lines(),
        Some([
            "judge must be a table".to_string(),
            "Use a [judge] table with gate, model, timeout_ms, max_concurrent, max_per_turn."
                .to_string(),
        ])
    );
    let error = load(ConfigProduct::Dalgon, "modle = 1").expect_err("unknown key");
    assert_eq!(format!("{error}"), "unknown config key: modle");
    let error =
        load(ConfigProduct::Dalgon, "mode = \"normal\"\nmode = \"x\"").expect_err("duplicate");
    assert_eq!(format!("{error}"), "dal.toml syntax error");
    assert!(
        load(ConfigProduct::Dalgon, "mode = \"jit\"")
            .expect_err("bad mode")
            .rules_lines("dalgon")
            .is_none()
    );
    // Non-rules invalid values still carry typed keys for other consumers.
    let (key, value, expected) = invalid_key("screen = \"wide\"");
    assert_eq!(
        (key.as_str(), value.as_str(), expected.as_str()),
        ("screen", "wide", "inline, fullscreen")
    );
}
