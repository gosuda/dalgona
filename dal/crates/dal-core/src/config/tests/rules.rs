//! Rules table tests.

use super::super::{Config, ConfigError, ConfigProduct, JudgeMode, RulesConfig};
use super::{DATA_ROOT, load};
use std::path::Path;

fn rules_error_lines(product: ConfigProduct, rules_body: &str) -> [String; 2] {
    let program = match product {
        ConfigProduct::Dalgon => "dalgon",
        ConfigProduct::Dalgona => "dalgona",
    };
    load(product, &format!("[rules]\n{rules_body}"))
        .expect_err("the rules value is invalid")
        .rules_lines(program)
        .expect("a rules value error has two lines")
}

#[test]
fn rules_defaults_and_layer_precedence() {
    use crate::ext::{InterruptMode, RepeatMode};

    let builtin = load(ConfigProduct::Dalgon, "").unwrap();
    assert_eq!(
        builtin.rules(),
        &RulesConfig {
            watch: true,
            interrupt: InterruptMode::Always,
            repeat: RepeatMode::Once,
            repeat_gap: 10,
            max_retries: 3,
            disabled: Vec::new(),
            judge: JudgeMode::Auto,
        }
    );

    let defaults = "[rules]\nrepeat = \"after-gap\"\nrepeat_gap = 5\ninterrupt = \"never\"\ndisabled = [\"a\"]\n";
    let config = Config::load(
        ConfigProduct::Dalgona,
        Path::new(DATA_ROOT),
        defaults,
        Some("[rules]\nrepeat_gap = 7\nmax_retries = 0\njudge = \"off\"\nwatch = false\n"),
    )
    .unwrap();
    assert_eq!(
        config.rules(),
        &RulesConfig {
            watch: false,
            interrupt: InterruptMode::Never,
            repeat: RepeatMode::AfterGap,
            repeat_gap: 7,
            max_retries: 0,
            disabled: vec![Box::<str>::from("a")],
            judge: JudgeMode::Off,
        }
    );

    let replaced = Config::load(
        ConfigProduct::Dalgon,
        Path::new(DATA_ROOT),
        defaults,
        Some("[rules]\ndisabled = []"),
    )
    .unwrap();
    assert_eq!(replaced.rules().disabled, []);
    assert_eq!(replaced.rules().repeat_gap, 5);

    let top_level_judge = load(ConfigProduct::Dalgon, "[judge]\ngate = \"off\"").unwrap();
    assert_eq!(top_level_judge.rules().judge, JudgeMode::Auto);
}

#[test]
fn rules_integer_bounds_are_inclusive_and_closed() {
    for (key, valid, invalid) in [
        (
            "repeat_gap",
            ["1", "1000"],
            ["0", "1001", "-1", "\"10\"", "10.0"],
        ),
        (
            "max_retries",
            ["0", "20"],
            ["21", "-1", "99999999999", "true", "3.0"],
        ),
    ] {
        for value in valid {
            let document = format!("[rules]\n{key} = {value}");
            assert!(load(ConfigProduct::Dalgon, &document).is_ok(), "{document}");
        }
        for value in invalid {
            let document = format!("[rules]\n{key} = {value}");
            assert!(
                matches!(
                    load(ConfigProduct::Dalgon, &document),
                    Err(ConfigError::InvalidValue { key: found, .. })
                        if found.as_ref() == format!("rules.{key}")
                ),
                "{document}"
            );
        }
    }
}

#[test]
fn rules_disabled_names_follow_rule_name_grammar() {
    let longest = format!("a{}", "b".repeat(63));
    let valid = format!("[rules]\ndisabled = [\"no-sleep\", \"A.b_c-1\", \"0\", \"{longest}\"]");
    let config = load(ConfigProduct::Dalgon, &valid).unwrap();
    assert_eq!(config.rules().disabled.len(), 4);
    assert_eq!(config.rules().disabled[3].as_ref(), longest);

    let too_long = format!("[\"{longest}c\"]");
    for value in [
        "[\"Bad Name\"]",
        "[\"\"]",
        "[\".hidden\"]",
        "[\"-x\"]",
        "[\"_x\"]",
        "[\"caf\u{e9}\"]",
        "[\"ok\", 1]",
        "\"no-sleep\"",
        too_long.as_str(),
    ] {
        let document = format!("[rules]\ndisabled = {value}");
        assert!(
            matches!(
                load(ConfigProduct::Dalgon, &document),
                Err(ConfigError::InvalidValue { key, .. }) if key.as_ref() == "rules.disabled"
            ),
            "{document}"
        );
    }
}

#[test]
fn rules_errors_print_the_exact_two_lines() {
    for (body, first, second) in [
        (
            "interrupt = \"sometimes\"",
            "dalgon: dal.toml: rules.interrupt \"sometimes\" is invalid",
            "Use one of always, prose-only, tool-only, never.",
        ),
        (
            "repeat = \"twice\"",
            "dalgon: dal.toml: rules.repeat \"twice\" is invalid",
            "Use one of once, after-gap.",
        ),
        (
            "repeat_gap = 0",
            "dalgon: dal.toml: rules.repeat_gap 0 is invalid",
            "Use a whole number from 1 to 1000.",
        ),
        (
            "max_retries = 50",
            "dalgon: dal.toml: rules.max_retries 50 is invalid",
            "Use a whole number from 0 to 20.",
        ),
        (
            "watch = \"yes\"",
            "dalgon: dal.toml: rules.watch \"yes\" is invalid",
            "Use true or false.",
        ),
        (
            "disabled = [\"Bad Name\"]",
            "dalgon: dal.toml: rules.disabled [\"Bad Name\"] is invalid",
            "Use a list of rule names, such as [\"no-sleep\"].",
        ),
        (
            "judge = \"maybe\"",
            "dalgon: dal.toml: rules.judge \"maybe\" is invalid",
            "Use one of auto, on, off.",
        ),
    ] {
        assert_eq!(
            rules_error_lines(ConfigProduct::Dalgon, body),
            [first.to_owned(), second.to_owned()]
        );
    }
    assert_eq!(
        rules_error_lines(ConfigProduct::Dalgona, "repeat = \"twice\""),
        [
            "dalgona: dal.toml: rules.repeat \"twice\" is invalid".to_owned(),
            "Use one of once, after-gap.".to_owned(),
        ]
    );
    let other = load(ConfigProduct::Dalgon, "mode = \"jit\"").unwrap_err();
    assert_eq!(other.rules_lines("dalgon"), None);
}

#[test]
fn rules_table_keys_are_closed() {
    let error = load(ConfigProduct::Dalgon, "[rules]\nwach = true").unwrap_err();
    assert!(matches!(
        error,
        ConfigError::UnknownKey { key, suggestion, .. }
            if key.as_ref() == "rules.wach" && suggestion.as_deref() == Some("rules.watch")
    ));
    assert!(matches!(
        load(ConfigProduct::Dalgona, "rules = 1"),
        Err(ConfigError::InvalidValue { key, .. }) if key.as_ref() == "rules"
    ));
}
