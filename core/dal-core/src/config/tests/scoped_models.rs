//! The `scoped_models` key: the ordered `provider/id` list `/model` is limited to.
//!
//! A saved list must load again at the next start, in both products, so the
//! key table accepts the shape the host writes and refuses every shape it
//! could not have written.

use super::super::{ConfigError, ConfigProduct};
use super::load;

#[test]
fn scoped_models_loads_in_both_products() {
    for product in [ConfigProduct::Dalgon, ConfigProduct::Dalgona] {
        load(
            product,
            "scoped_models = [\"beta/gpt-4o\", \"alpha/mini\"]\n",
        )
        .expect("a saved scope list loads");
        load(product, "scoped_models = []\n").expect("a cleared scope list loads");
        load(product, "").expect("an absent key is the empty list");
    }
}

#[test]
fn scoped_models_refuses_what_the_host_never_writes() {
    for text in [
        "scoped_models = \"beta/gpt-4o\"\n",
        "scoped_models = [\"\"]\n",
        "scoped_models = [1]\n",
        "scoped_models = [\"beta/gpt-4o\", \"\"]\n",
    ] {
        let error = load(ConfigProduct::Dalgon, text).expect_err("a malformed scope list fails");
        assert!(
            matches!(
                &error,
                ConfigError::InvalidValue { key, .. } if key.as_ref() == "scoped_models"
            ),
            "{text:?} must be an invalid value for scoped_models, not {error:?}"
        );
    }
}

#[test]
fn scoped_models_keeps_saved_order_and_defaults_empty() {
    for product in [ConfigProduct::Dalgon, ConfigProduct::Dalgona] {
        let none = load(product, "").expect("an absent key loads");
        assert_eq!(none.scoped_models(), []);
        let saved = load(
            product,
            "scoped_models = [\"beta/gpt-4o\", \"openrouter/vendor/model\", \"alpha/mini\"]\n",
        )
        .expect("a saved scope loads");
        assert_eq!(
            saved.scoped_models(),
            ["beta/gpt-4o", "openrouter/vendor/model", "alpha/mini"].map(Box::<str>::from)
        );
    }
}

#[test]
fn updating_scoped_models_keeps_comments_and_unrelated_keys() {
    let mut config = load(ConfigProduct::Dalgon, "").expect("config loads");
    let base = "# kept\nmodel = \"beta/gpt-4o\"\n\n[plugin.user_tool]\nanything = 1 # inline\n";
    let ids = ["beta/gpt-4o", "alpha/mini"].map(Box::<str>::from);
    let saved = config
        .update_scoped_models(&ids, Some(base))
        .expect("the scope writes");
    for kept in [
        "# kept",
        "model = \"beta/gpt-4o\"",
        "[plugin.user_tool]",
        "anything = 1 # inline",
    ] {
        assert!(saved.contains(kept), "dropped {kept:?}: {saved}");
    }
    assert_eq!(config.scoped_models(), ids);
    let reloaded = load(ConfigProduct::Dalgon, &saved).expect("the saved text loads");
    assert_eq!(reloaded.scoped_models(), ids);

    let cleared = config
        .update_scoped_models(&[], Some(saved.as_str()))
        .expect("the scope clears");
    assert!(cleared.contains("scoped_models = []"), "{cleared}");
    assert!(cleared.contains("# kept"), "{cleared}");
    assert_eq!(config.scoped_models(), []);
    let reloaded = load(ConfigProduct::Dalgon, &cleared).expect("the cleared text loads");
    assert_eq!(reloaded.scoped_models(), []);
}

#[test]
fn updating_scoped_models_replaces_an_existing_list_in_place() {
    let mut config = load(ConfigProduct::Dalgon, "").expect("config loads");
    let base = "scoped_models = [\"old/one\", \"old/two\"] # mine\nmodel = \"beta/gpt-4o\"\n";
    let ids = ["new/one"].map(Box::<str>::from);
    let saved = config
        .update_scoped_models(&ids, Some(base))
        .expect("the scope writes");
    assert!(!saved.contains("old/"), "{saved}");
    assert!(saved.contains("# mine"), "{saved}");
    assert_eq!(saved.matches("scoped_models").count(), 1, "{saved}");
}

#[test]
fn updating_scoped_models_refuses_what_it_could_not_load_again() {
    let mut config = load(ConfigProduct::Dalgon, "").expect("config loads");
    let ids = ["beta/gpt-4o"].map(Box::<str>::from);
    let empty = ["beta/gpt-4o", ""].map(Box::<str>::from);
    assert!(matches!(
        config.update_scoped_models(&empty, Some("model = \"a/b\"\n")),
        Err(ConfigError::InvalidValue { key, .. }) if key.as_ref() == "scoped_models"
    ));
    assert!(matches!(
        config.update_scoped_models(&ids, Some("scoped_models = \"beta/gpt-4o\"\n")),
        Err(ConfigError::InvalidValue { key, .. }) if key.as_ref() == "scoped_models"
    ));
    assert!(matches!(
        config.update_scoped_models(&ids, Some("[plugin\n")),
        Err(ConfigError::Syntax { .. })
    ));
    assert!(
        config.scoped_models().is_empty(),
        "a refused write must leave the loaded scope alone"
    );
}
