//! Core layering, keys, and enum spelling tests.

use super::super::{
    ApprovalMode, Config, ConfigError, ConfigProduct, JudgeMode, Mode, ModeProjection, Screen,
};
use super::{DALGONA_DEFAULTS, load, parse_enum};
use crate::model::{Family, ModelPrice, ModelRoute, ThinkingLevel};
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
    for (spelling, approval) in [
        ("ask", ApprovalMode::Ask),
        ("edits", ApprovalMode::Edits),
        ("all", ApprovalMode::All),
    ] {
        assert_eq!(parse_enum::<ApprovalMode>(spelling).unwrap(), approval);
    }
    for (spelling, screen) in [
        ("inline", Screen::Inline),
        ("fullscreen", Screen::Fullscreen),
    ] {
        assert_eq!(parse_enum::<Screen>(spelling).unwrap(), screen);
    }
    for (spelling, judge) in [
        ("auto", JudgeMode::Auto),
        ("on", JudgeMode::On),
        ("off", JudgeMode::Off),
    ] {
        assert_eq!(parse_enum::<JudgeMode>(spelling).unwrap(), judge);
    }
    for spelling in [
        "ASK",
        "eval-first",
        "full_screen",
        "enabled",
        "INLINE",
        "AUTO",
        "full-screen",
    ] {
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
            cache_write: None,
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

fn anthropic_route(provider: Option<&str>) -> ModelRoute {
    ModelRoute::Api {
        family: Family::Anthropic,
        model: "claude-opus".into(),
        provider: provider.map(Into::into),
    }
}

#[test]
fn model_route_with_provider_parses() {
    let config = load(
        ConfigProduct::Dalgona,
        "[model_route]\nkind = \"api\"\nfamily = \"anthropic\"\nmodel = \"claude-opus\"\nprovider = \"acme\"\n",
    )
    .expect("a provider route parses");
    let expected = anthropic_route(Some("acme"));
    assert_eq!(config.model_route(), Some(&expected));
    assert_eq!(config.default_model_route(), Some(expected));
    let legacy = load(
        ConfigProduct::Dalgona,
        "[model_route]\nkind = \"api\"\nfamily = \"anthropic\"\nmodel = \"claude-opus\"\n",
    )
    .expect("a provider-less route parses");
    assert_eq!(legacy.model_route(), Some(&anthropic_route(None)));
}

#[test]
fn model_route_provider_rejections_keep_the_provider_key_shape() {
    for (body, value) in [
        ("provider = \"\"", ""),
        ("provider = 7", "7"),
        ("provider = true", "true"),
    ] {
        let user = format!(
            "[model_route]\nkind = \"api\"\nfamily = \"anthropic\"\nmodel = \"m\"\n{body}\n"
        );
        let error = load(ConfigProduct::Dalgona, &user).expect_err("bad provider is refused");
        assert!(
            matches!(&error, ConfigError::InvalidValue { key, value: got, expected }
                if key.as_ref() == "model_route.provider"
                    && got.as_ref() == value
                    && expected.as_ref() == "non-empty configured provider id"),
            "wrong error for {body}: {error:?}"
        );
    }
    for kind in ["synthetic", "harness"] {
        let id = if kind == "harness" {
            "dalgon/normal"
        } else {
            "a/b"
        };
        let user =
            format!("[model_route]\nkind = \"{kind}\"\nid = \"{id}\"\nprovider = \"acme\"\n");
        let error =
            load(ConfigProduct::Dalgona, &user).expect_err("provider off kind api is refused");
        assert!(
            matches!(&error, ConfigError::InvalidValue { key, expected, .. }
                if key.as_ref() == "model_route.provider"
                    && expected.as_ref() == "absent unless kind api"),
            "wrong error for {kind}: {error:?}"
        );
    }
}

#[test]
fn serve_bind_provenance_follows_the_key_not_the_value() {
    let defaults = load(ConfigProduct::Dalgon, "").expect("built-in defaults");
    assert!(
        !defaults.serve_bind_explicit(),
        "an absent serve.bind is not explicit"
    );

    // The documented default, spelled by the user: still explicit.
    let spelled = load(ConfigProduct::Dalgon, "[serve]\nbind = \"127.0.0.1\"\n")
        .expect("spelled default loads");
    assert_eq!(spelled.serve().bind.as_ref(), "127.0.0.1");
    assert!(spelled.serve_bind_explicit());

    // A different explicit value: explicit, and the value survives.
    let other =
        load(ConfigProduct::Dalgon, "[serve]\nbind = \"localhost\"\n").expect("other bind loads");
    assert_eq!(other.serve().bind.as_ref(), "localhost");
    assert!(other.serve_bind_explicit());

    // A layer that sets other serve keys leaves bind implicit.
    let untouched =
        load(ConfigProduct::Dalgon, "[serve]\nport = 8000\n").expect("port-only serve loads");
    assert_eq!(untouched.serve().bind.as_ref(), "127.0.0.1");
    assert!(!untouched.serve_bind_explicit());

    // The value reaches the effective configuration unchanged.
    let wide =
        load(ConfigProduct::Dalgon, "[serve]\nbind = \"0.0.0.0\"\n").expect("wide bind loads");
    assert_eq!(wide.serve().bind.as_ref(), "0.0.0.0");
    assert!(wide.serve_bind_explicit());
}

const MODEL_USER: &str = "# keep me\nmode = \"normal\"\nmodel = \"old/model\"\n[model_route]\nkind = \"api\"\nfamily = \"openai_chat\"\nmodel = \"old\"\n[serve]\nbind = \"127.0.0.1\"\n";

#[test]
fn update_default_model_writes_provider_and_a_mirror_without_it() {
    let mut config = load(ConfigProduct::Dalgona, MODEL_USER).expect("fixture loads");
    let route = ModelRoute::Api {
        family: Family::Responses,
        model: "gpt-6-luna".into(),
        provider: Some("acme".into()),
    };
    let rendered = config
        .update_default_model(&route, Some(MODEL_USER))
        .expect("the update renders");
    assert!(
        rendered.contains("provider = \"acme\"\n"),
        "provider missing: {rendered}"
    );
    let mirror = rendered
        .lines()
        .find(|line| line.starts_with("model = "))
        .expect("the legacy mirror is written");
    assert_eq!(mirror, "model = \"openai-responses/gpt-6-luna\"");
    assert!(rendered.contains("# keep me\n"), "comment lost: {rendered}");
    assert!(
        rendered.contains("[serve]\nbind = \"127.0.0.1\"\n"),
        "unrelated table lost: {rendered}"
    );
    assert_eq!(config.model(), Some("openai-responses/gpt-6-luna"));
    assert_eq!(config.model_route(), Some(&route));
    let reloaded = load(ConfigProduct::Dalgona, &rendered).expect("rendered output reloads");
    assert_eq!(reloaded.model_route(), Some(&route));
}

#[test]
fn update_default_model_drops_a_stale_provider_key() {
    let mut config = load(ConfigProduct::Dalgona, MODEL_USER).expect("fixture loads");
    let scoped = ModelRoute::Api {
        family: Family::Chat,
        model: "gpt-5".into(),
        provider: Some("acme".into()),
    };
    let with_provider = config
        .update_default_model(&scoped, Some(MODEL_USER))
        .expect("the provider renders");
    assert!(with_provider.contains("provider = \"acme\"\n"));
    let bare = ModelRoute::Api {
        family: Family::Chat,
        model: "gpt-5".into(),
        provider: None,
    };
    let dropped = config
        .update_default_model(&bare, Some(with_provider.as_str()))
        .expect("the replacement renders");
    assert!(
        !dropped.contains("provider"),
        "stale provider survived: {dropped}"
    );
    assert_eq!(config.model_route(), Some(&bare));
    let reloaded = load(ConfigProduct::Dalgona, &dropped).expect("dropped output reloads");
    assert_eq!(reloaded.model_route(), Some(&bare));
}
