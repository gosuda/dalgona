//! Prices, serve, overrides, and scalar bound tests.

use super::super::{
    ApprovalMode, Config, ConfigError, ConfigOverrides, ConfigProduct, Mode, Screen,
};
use super::{DATA_ROOT, load};
use crate::model::{ModelPrice, PriceTier, ThinkingLevel};
use std::path::{Path, PathBuf};

#[test]
fn config_prices_and_serve_origins_are_typed() {
    let config = load(
        ConfigProduct::Dalgon,
        r#"
approval = "edits"

[serve]
token_file = ""
origins = ["https://one.example", "https://two.example"]

[prices."family/model"]
input = 1.0
cached_input = 0.5
output = 2.0
reasoning = 3.0
"#,
    )
    .expect("valid serve and price values");
    assert_eq!(config.serve().approval, ApprovalMode::Edits);
    assert_eq!(
        config.serve().origins,
        vec![
            Box::<str>::from("https://one.example"),
            Box::<str>::from("https://two.example")
        ]
    );
    assert_eq!(config.serve().token_file, PathBuf::new());
    assert_eq!(
        config.price_for_model("family/model"),
        Some(&ModelPrice {
            input: 1.0,
            cached_input: 0.5,
            output: 2.0,
            reasoning: 3.0,
            tiers: Box::default(),
        })
    );

    let defaults = load(ConfigProduct::Dalgon, "").expect("built-in defaults");
    assert_eq!(defaults.serve().origins, []);
    assert_eq!(defaults.serve().approval, ApprovalMode::Ask);
    assert_eq!(
        defaults.serve().token_file,
        PathBuf::from(DATA_ROOT).join("serve.token")
    );

    for port in ["0", "65535"] {
        let document = format!("[serve]\nport = {port}");
        assert!(load(ConfigProduct::Dalgon, &document).is_ok());
    }
    for port in ["-1", "65536"] {
        let document = format!("[serve]\nport = {port}");
        assert!(matches!(
            load(ConfigProduct::Dalgon, &document),
            Err(ConfigError::InvalidValue { key, .. })
                if key.as_ref() == "serve.port"
        ));
    }

    for rate in ["-1.0", "nan", "inf"] {
        let document = format!(
            "[prices.model]\ninput = {rate}\ncached_input = 0.0\noutput = 0.0\nreasoning = 0.0"
        );
        assert!(matches!(
            load(ConfigProduct::Dalgon, &document),
            Err(ConfigError::InvalidValue { key, expected, .. })
                if key.as_ref() == "prices.model.input"
                    && expected.as_ref() == "finite non-negative USD rate"
        ));
    }

    let zero = load(
        ConfigProduct::Dalgon,
        "[prices.zero]\ninput = 0.0\ncached_input = 0.0\noutput = 0.0\nreasoning = 0.0",
    )
    .expect("explicit zero prices are valid");
    assert_eq!(
        zero.price_for_model("zero"),
        Some(&ModelPrice {
            input: 0.0,
            cached_input: 0.0,
            output: 0.0,
            reasoning: 0.0,
            tiers: Box::default(),
        })
    );
}

#[test]
fn config_merges_price_fields_and_rejects_incomplete_rates() {
    let config = Config::load(
        ConfigProduct::Dalgon,
        Path::new(DATA_ROOT),
        "[prices.partial]\ninput = 1.0\ncached_input = 2.0",
        Some("[prices.partial]\noutput = 3.0\nreasoning = 4.0"),
    )
    .expect("rate fields merge across layers");
    assert_eq!(
        config.price_for_model("partial"),
        Some(&ModelPrice {
            input: 1.0,
            cached_input: 2.0,
            output: 3.0,
            reasoning: 4.0,
            tiers: Box::default(),
        })
    );

    let error = load(ConfigProduct::Dalgon, "[prices.partial]\ninput = 0.0")
        .expect_err("missing rates are not zero");
    assert!(matches!(
        error,
        ConfigError::InvalidValue { key, value, expected }
            if key.as_ref() == "prices.partial"
                && value.as_ref() == "missing cached_input, output, reasoning"
                && expected.as_ref() == "all four rates: input, cached_input, output, reasoning"
    ));
}

#[test]
fn config_price_layers_override_single_rates_including_zero() {
    let config = Config::load(
        ConfigProduct::Dalgon,
        Path::new(DATA_ROOT),
        "[prices.partial]\ninput = 1.0\ncached_input = 2.0\noutput = 3.0\nreasoning = 4.0",
        Some("[prices.partial]\ncached_input = 0.0\nreasoning = 5.0"),
    )
    .expect("later rates replace earlier ones");
    assert_eq!(
        config.price_for_model("partial"),
        Some(&ModelPrice {
            input: 1.0,
            cached_input: 0.0,
            output: 3.0,
            reasoning: 5.0,
            tiers: Box::default(),
        })
    );
}

#[test]
fn config_parses_request_wide_price_tiers() {
    let config = Config::load(
        ConfigProduct::Dalgon,
        Path::new(DATA_ROOT),
        "[prices.tiered]\ninput = 1.0\ncached_input = 0.5\noutput = 2.0\nreasoning = 3.0\ntiers = [{ size = 200000, input = 2.0, output = 4.0 }]",
        None,
    )
    .expect("tiered price table parses");
    assert_eq!(
        config.price_for_model("tiered"),
        Some(&ModelPrice {
            input: 1.0,
            cached_input: 0.5,
            output: 2.0,
            reasoning: 3.0,
            tiers: vec![PriceTier {
                size: 200_000,
                input: Some(2.0),
                cached_input: None,
                output: Some(4.0),
                reasoning: None,
            }]
            .into_boxed_slice(),
        })
    );
}

#[test]
fn config_rejects_non_increasing_price_tier_sizes() {
    let prefix =
        "[prices.tiered]\ninput = 1.0\ncached_input = 0.5\noutput = 2.0\nreasoning = 3.0\n";
    for suffix in [
        "tiers = [{ size = 200000, input = 2.0 }, { size = 200000, output = 4.0 }]",
        "tiers = [{ size = 200000, input = 2.0 }, { size = 199999, output = 4.0 }]",
    ] {
        let error = load(ConfigProduct::Dalgon, &format!("{prefix}{suffix}"))
            .expect_err("non-increasing tier sizes are rejected");
        assert!(matches!(
            error,
            ConfigError::InvalidValue { key, expected, .. }
                if key.as_ref() == "prices.tiered.tiers"
                    && expected.as_ref() == "strictly increasing context tier sizes"
        ));
    }
}

#[test]
fn config_rejects_unknown_price_tier_fields() {
    let error = load(
        ConfigProduct::Dalgon,
        "[prices.tiered]\ninput = 1.0\ncached_input = 0.5\noutput = 2.0\nreasoning = 3.0\ntiers = [{ size = 200000, input = 2.0, typo = 4.0 }]",
    )
    .expect_err("unknown tier fields are rejected");
    assert!(matches!(
        error,
        ConfigError::UnknownKey { key, .. } if key.as_ref() == "prices.tiered.tiers.typo"
    ));
}

#[test]
fn config_rejects_corrupt_compiled_defaults_before_user_text() {
    let error = Config::load(
        ConfigProduct::Dalgon,
        Path::new(DATA_ROOT),
        "mode = [",
        Some("mode = ["),
    )
    .expect_err("compiled default corruption is internal");
    assert!(matches!(
        error,
        ConfigError::DefaultsInvalid { message }
            if message.contains("Syntax")
    ));
}

#[test]
fn config_flag_overrides_are_in_memory() {
    let config = load(
        ConfigProduct::Dalgon,
        "model = \"base/model\"\nmode = \"eval-first\"\napproval = \"edits\"\nscreen = \"inline\"\nsandbox = false",
    )
    .expect("valid base configuration");
    let overrides = ConfigOverrides {
        model: Some(Box::<str>::from("flag/model")),
        mode: Some(Mode::EvalOnly),
        thinking: Some(ThinkingLevel::High),
        approval: Some(ApprovalMode::All),
        screen: Some(Screen::Fullscreen),
        sandbox: Some(true),
    };
    let overlaid = config.with_overrides(&overrides);
    assert_eq!(overlaid.model(), Some("flag/model"));
    assert_eq!(overlaid.mode(), Mode::EvalOnly);
    assert_eq!(overlaid.thinking(), ThinkingLevel::High);
    assert_eq!(overlaid.approval(), ApprovalMode::All);
    assert_eq!(overlaid.screen(), Screen::Fullscreen);
    assert!(overlaid.sandbox);
    assert_eq!(overlaid.serve().approval, ApprovalMode::Edits);
    assert_eq!(config.model(), Some("base/model"));
    assert_eq!(config.approval(), ApprovalMode::Edits);
    assert_eq!(config.serve().approval, ApprovalMode::Edits);

    assert_eq!(config.with_overrides(&ConfigOverrides::default()), config);
}

#[test]
fn compact_ratio_enforces_finite_inclusive_bounds() {
    for ratio in ["0.5", "0.95"] {
        let document = format!("compact_ratio = {ratio}");
        assert!(load(ConfigProduct::Dalgon, &document).is_ok());
    }
    for ratio in ["0.49", "0.96", "nan", "inf"] {
        let document = format!("compact_ratio = {ratio}");
        assert!(matches!(
            load(ConfigProduct::Dalgon, &document),
            Err(ConfigError::InvalidValue { key, .. })
                if key.as_ref() == "compact_ratio"
        ));
    }
}
