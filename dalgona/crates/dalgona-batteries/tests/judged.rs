// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! Behavior tests for the judged battery configuration and registration.

use std::error::Error;

use dalgona_batteries::judged::{JudgedConfig, JudgedConfigError};

type TestResult = Result<(), Box<dyn Error>>;

fn section(source: &str) -> Result<toml::Value, Box<dyn Error>> {
    let doc: toml::Value = toml::from_str(&format!("[plugin.judged]\n{source}"))?;
    doc.get("plugin")
        .and_then(|plugin| plugin.get("judged"))
        .cloned()
        .ok_or_else(|| std::io::Error::other("judged section missing").into())
}

#[test]
fn omitted_section_enables_all_features() -> TestResult {
    assert_eq!(JudgedConfig::from_config(None)?, JudgedConfig::default());
    Ok(())
}

#[test]
fn every_feature_can_be_disabled_independently() -> TestResult {
    let section = section(
        "thinking = false\nranking = true\nask_anchor = false\nclaim_check = true\ndedup = false\n",
    )?;
    let config = JudgedConfig::from_config(Some(&section))?;
    assert_eq!(
        config,
        JudgedConfig {
            thinking: false,
            ranking: true,
            ask_anchor: false,
            claim_check: true,
            dedup: false,
        }
    );
    Ok(())
}

#[test]
fn unknown_key_reports_a_near_match() -> TestResult {
    let section = section("thinkng = true\n")?;
    let error = JudgedConfig::from_config(Some(&section)).unwrap_err();
    assert_eq!(
        error.to_string(),
        "judged: [plugin.judged] has an unknown key \"thinkng\". Did you mean \"thinking\"?"
    );
    Ok(())
}

#[test]
fn distant_unknown_key_has_no_suggestion() -> TestResult {
    let section = section("unrelated = true\n")?;
    let error = JudgedConfig::from_config(Some(&section)).unwrap_err();
    assert_eq!(
        error.to_string(),
        "judged: [plugin.judged] has an unknown key \"unrelated\"."
    );
    Ok(())
}

#[test]
fn unknown_keys_are_checked_before_wrong_values() -> TestResult {
    let section = section("unknown = true\nthinking = \"yes\"\n")?;
    let error = JudgedConfig::from_config(Some(&section)).unwrap_err();
    assert!(matches!(error, JudgedConfigError::UnknownKey { .. }));
    Ok(())
}

#[test]
fn wrong_string_value_names_the_feature() -> TestResult {
    let section = section("ranking = \"yes\"\n")?;
    let error = JudgedConfig::from_config(Some(&section)).unwrap_err();
    assert_eq!(
        error.to_string(),
        "judged: [plugin.judged].ranking must be true or false."
    );
    Ok(())
}

#[test]
fn wrong_integer_value_names_the_feature() -> TestResult {
    let section = section("dedup = 1\n")?;
    let error = JudgedConfig::from_config(Some(&section)).unwrap_err();
    assert!(matches!(error, JudgedConfigError::WrongType { .. }));
    Ok(())
}

#[test]
fn wrong_array_value_names_the_feature() -> TestResult {
    let section = section("claim_check = [true]\n")?;
    let error = JudgedConfig::from_config(Some(&section)).unwrap_err();
    assert_eq!(
        error.to_string(),
        "judged: [plugin.judged].claim_check must be true or false."
    );
    Ok(())
}

#[test]
fn non_table_section_fails_closed() {
    let value = toml::Value::String("enabled".to_owned());
    let error = JudgedConfig::from_config(Some(&value)).unwrap_err();
    assert!(matches!(error, JudgedConfigError::WrongSectionType));
}

#[test]
fn config_error_variants_remain_typed() -> TestResult {
    let section = section("thinking = 1\n")?;
    let error = JudgedConfig::from_config(Some(&section)).unwrap_err();
    assert!(matches!(error, JudgedConfigError::WrongType { key } if key.as_ref() == "thinking"));
    Ok(())
}
