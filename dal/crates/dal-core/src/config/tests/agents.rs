//! Agents table tests.

use super::super::{ConfigError, ConfigProduct};
use super::load;

#[test]
fn agents_defaults_and_boundaries() {
    let config = load(ConfigProduct::Dalgon, "").unwrap();
    assert_eq!(config.agents(), &crate::config::AgentsConfig::default());
    assert!(!config.agents().enabled);
    assert_eq!(config.agents().max_concurrent.get(), 32);
    assert_eq!(config.agents().max_depth.get(), 1);
    let custom = load(
        ConfigProduct::Dalgon,
        "[agents]\nenabled = true\nmax_concurrent = 4\nmax_depth = 2\n",
    )
    .unwrap();
    assert!(custom.agents().enabled);
    assert_eq!(custom.agents().max_concurrent.get(), 4);
    assert_eq!(custom.agents().max_depth.get(), 2);
    for doc in [
        "[agents]\nmax_concurrent = 0\n",
        "[agents]\nmax_depth = 0\n",
        "[agents]\nmax_concurrent = -1\n",
        "[agents]\nmax_depth = -2\n",
    ] {
        assert!(load(ConfigProduct::Dalgon, doc).is_err(), "{doc}");
    }
    let error = load(ConfigProduct::Dalgon, "[agents]\nfoo = 1\n").unwrap_err();
    assert!(matches!(
        error,
        ConfigError::UnknownKey { key, .. } if key.as_ref() == "agents.foo"
    ));
}

#[test]
fn agents_zero_rejected_with_exact_error() {
    for (doc, key) in [
        ("[agents]\nmax_concurrent = 0\n", "agents.max_concurrent"),
        ("[agents]\nmax_depth = 0\n", "agents.max_depth"),
    ] {
        let error = load(ConfigProduct::Dalgon, doc).unwrap_err();
        assert!(
            matches!(
                error,
                ConfigError::InvalidValue {
                    key: ref error_key,
                    ref value,
                    ref expected,
                } if error_key.as_ref() == key
                    && value.as_ref() == "0"
                    && expected.as_ref() == "Use a positive integer."
            ),
            "{doc}"
        );
    }
}
