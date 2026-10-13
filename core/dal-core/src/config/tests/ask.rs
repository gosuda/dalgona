//! Ask table tests.

use std::time::Duration;

use super::super::{ConfigError, ConfigProduct};
use super::load;
use crate::config::AskConfig;

#[test]
fn ask_defaults_and_clamps() {
    let config = load(ConfigProduct::Dalgon, "").unwrap();
    assert_eq!(config.ask(), &AskConfig::default());
    assert_eq!(config.ask().timeout(), Duration::from_mins(30));
    assert_eq!(config.ask_clamp_notice(), None);

    let custom = load(ConfigProduct::Dalgon, "[ask]\ntimeout = \"90m\"\n").unwrap();
    assert_eq!(custom.ask().timeout(), Duration::from_mins(90));
    assert_eq!(custom.ask_clamp_notice(), None);

    let seconds = load(ConfigProduct::Dalgon, "[ask]\ntimeout = 90\n").unwrap();
    assert_eq!(seconds.ask().timeout(), Duration::from_secs(90));
}

#[test]
fn ask_clamps_below_and_above_with_one_notice() {
    let under = load(ConfigProduct::Dalgon, "[ask]\ntimeout = \"10s\"\n").unwrap();
    assert_eq!(under.ask().timeout(), Duration::from_secs(60));
    assert_eq!(
        under.ask_clamp_notice().as_deref(),
        Some("ask: [ask] timeout 10s is under the 1m minimum, clamped up to 1m.")
    );

    let over = load(ConfigProduct::Dalgon, "[ask]\ntimeout = \"5h\"\n").unwrap();
    assert_eq!(over.ask().timeout(), Duration::from_hours(2));
    assert_eq!(
        over.ask_clamp_notice().as_deref(),
        Some("ask: [ask] timeout 5h is over the 120m maximum, clamped down to 120m.")
    );
}

#[test]
fn ask_rejects_unknown_keys_and_bad_values() {
    let error = load(ConfigProduct::Dalgon, "[ask]\nfoo = 1\n").unwrap_err();
    assert!(
        matches!(
            error,
            ConfigError::UnknownKey { ref key, .. } if key.as_ref() == "ask.foo"
        ),
        "{error:?}"
    );
    assert!(
        matches!(
            error,
            ConfigError::UnknownKey { ref known_keys, .. }
                if known_keys.iter().any(|known| known.as_ref() == "ask.timeout")
        ),
        "{error:?}"
    );
    for doc in [
        "[ask]\ntimeout = 0\n",
        "[ask]\ntimeout = \"0m\"\n",
        "[ask]\ntimeout = \"soon\"\n",
        "[ask]\ntimeout = \"1d\"\n",
        "[ask]\ntimeout = true\n",
        "ask = 4\n",
    ] {
        assert!(load(ConfigProduct::Dalgon, doc).is_err(), "{doc}");
    }
}

#[test]
fn ask_rejects_multibyte_duration_units() {
    for timeout in ["1µ", "1秒", "1\u{10000}"] {
        let doc = format!("[ask]\ntimeout = \"{timeout}\"\n");
        let error = load(ConfigProduct::Dalgon, &doc).unwrap_err();
        assert!(
            matches!(
                &error,
                ConfigError::InvalidValue { key, value, .. }
                    if key.as_ref() == "ask.timeout" && value.as_ref() == timeout
            ),
            "{timeout:?}: {error:?}"
        );
    }
}
