//! Guard table tests.

use super::super::{Config, ConfigError, ConfigProduct};
use super::load;

#[test]
fn guard_unknown_key_reports_bare_name() {
    let error = load(ConfigProduct::Dalgon, "[guard.policies]\nfoo = 1").unwrap_err();
    assert_eq!(error.to_string(), "unknown guard policy key foo",);
    assert!(matches!(
        error,
        ConfigError::UnknownGuardKey { key } if key.as_ref() == "foo"
    ));
}

#[test]
fn guard_unknown_key_at_bands_depth_reports_bare_name() {
    let error = load(ConfigProduct::Dalgon, "[guard.policies.bands]\nfoo = 1").unwrap_err();
    assert_eq!(error.to_string(), "unknown guard policy key foo");
}

#[test]
fn guard_enabled_survives_policies_only_layer() {
    let config = Config::load(
        ConfigProduct::Dalgona,
        std::path::Path::new("/home/test/dalgon"),
        "[guard]\nenabled = true\n[guard.policies]\ng4_enabled = true\n",
        Some("[guard.policies]\nchurn_turn_threshold = 5\n"),
    )
    .unwrap();
    assert!(config.guard.enabled);
    assert!(config.guard.policies.g4_enabled);
    assert_eq!(config.guard.policies.churn_turn_threshold, 5);
}
