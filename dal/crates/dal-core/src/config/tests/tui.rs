//! TUI renderer configuration tests.

use super::super::{ConfigError, ConfigProduct};
use super::load;

#[test]
fn diagrams_default_off_and_can_be_enabled_for_each_product() {
    for product in [ConfigProduct::Dalgon, ConfigProduct::Dalgona] {
        let default = load(product, "").expect("default config loads");
        assert!(!default.tui().diagrams);
        let enabled = load(product, "[tui]\ndiagrams = true\n").expect("TUI config loads");
        assert!(enabled.tui().diagrams);
    }
}

#[test]
fn tui_table_rejects_unknown_keys() {
    let error = load(ConfigProduct::Dalgon, "[tui]\ndiagrms = true\n")
        .expect_err("unknown TUI key is rejected");
    assert!(matches!(
        error,
        ConfigError::UnknownKey { key, .. } if key.as_ref() == "tui.diagrms"
    ));
}

#[test]
fn tui_diagrams_requires_a_boolean() {
    assert!(load(ConfigProduct::Dalgon, "[tui]\ndiagrams = \"yes\"\n").is_err());
}
#[test]
fn updating_tui_diagrams_preserves_the_rest_of_the_user_document() {
    let mut config = load(ConfigProduct::Dalgon, "").expect("config loads");
    let updated = config
        .update_tui_diagrams(
            true,
            Some("# user config\nmodel = \"openai/gpt-6\"\n\n[tui]\n# keep this note\ndiagrams = false # inline note\n"),
        )
        .expect("TUI preference updates");
    assert!(updated.contains("# user config"));
    assert!(updated.contains("model = \"openai/gpt-6\""));
    assert!(updated.contains("# keep this note"));
    assert!(updated.contains("diagrams = true # inline note"));
    assert!(
        load(ConfigProduct::Dalgon, &updated)
            .expect("updated TOML parses")
            .tui()
            .diagrams
    );
    assert!(config.tui().diagrams);
}

#[test]
fn updating_tui_diagrams_creates_a_user_table_when_missing() {
    let mut config = load(ConfigProduct::Dalgon, "").expect("config loads");
    let updated = config
        .update_tui_diagrams(true, None)
        .expect("TUI preference creates its table");
    assert!(updated.contains("[tui]"));
    assert!(
        load(ConfigProduct::Dalgon, &updated)
            .expect("updated TOML parses")
            .tui()
            .diagrams
    );
}

#[test]
fn updating_tui_diagrams_rejects_invalid_reloaded_documents() {
    let mut config = load(ConfigProduct::Dalgon, "").expect("config loads");
    assert!(matches!(
        config.update_tui_diagrams(true, Some("[tui]\ndiagrams =")),
        Err(ConfigError::Syntax { .. })
    ));
    assert!(!config.tui().diagrams);
}
