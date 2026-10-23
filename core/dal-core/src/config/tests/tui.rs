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
fn tui_diagrams_rejects_a_string_with_an_invalid_value_error() {
    let error = load(ConfigProduct::Dalgon, "[tui]\ndiagrams = \"yes\"\n")
        .expect_err("a string diagrams value is rejected");
    assert!(
        matches!(
            &error,
            ConfigError::InvalidValue { key, value, expected }
                if key.as_ref() == "tui.diagrams"
                    && value.as_ref() == "yes"
                    && expected.as_ref() == "boolean"
        ),
        "a string diagrams value is an invalid value, not a syntax error: {error:?}"
    );
    let enabled = load(ConfigProduct::Dalgon, "[tui]\ndiagrams = true\n")
        .expect("a boolean diagrams value loads");
    assert!(enabled.tui().diagrams);
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

#[test]
fn updating_default_model_preserves_comments_and_reloads() {
    let mut config = load(ConfigProduct::Dalgon, "").expect("config loads");
    let updated = config
        .update_default_model(
            "beta/gpt-4o",
            Some("# kept user comment\nmodel = \"alpha/gpt-4o\"\n\n[tui]\n# keep this note\ndiagrams = false\n"),
        )
        .expect("model default updates");
    assert!(updated.contains("# kept user comment"));
    assert!(updated.contains("model = \"beta/gpt-4o\""));
    assert!(!updated.contains("model_route"));
    assert!(!updated.contains("openai-chat/gpt-4o"));
    assert!(updated.contains("# keep this note"));
    let reloaded = load(ConfigProduct::Dalgon, &updated).expect("updated TOML parses");
    assert_eq!(reloaded.model(), Some("beta/gpt-4o"));
    assert_eq!(config.model(), Some("beta/gpt-4o"));
}

#[test]
fn updating_default_model_creates_the_key_when_missing() {
    let mut config = load(ConfigProduct::Dalgon, "").expect("config loads");
    let updated = config
        .update_default_model("beta/gpt-4o", None)
        .expect("model default creates the key");
    assert!(updated.contains("model = \"beta/gpt-4o\""));
    assert!(!updated.contains("model_route"));
    let reloaded = load(ConfigProduct::Dalgon, &updated).expect("updated TOML parses");
    assert_eq!(reloaded.model(), Some("beta/gpt-4o"));
}

#[test]
fn updating_default_model_rejects_empty_spellings() {
    let mut config = load(ConfigProduct::Dalgon, "").expect("config loads");
    assert!(config.update_default_model("", None).is_err());
    assert!(
        config
            .update_default_model("beta/gpt-4o", Some("model = \"\"\n"))
            .is_err()
    );
    assert_eq!(config.model(), None);
}

#[test]
fn updating_default_thinking_mode_and_approval_stay_scoped_to_their_keys() {
    let mut config = load(ConfigProduct::Dalgon, "").expect("config loads");
    let base = "# kept\nmodel = \"beta/gpt-4o\"\n";
    let updated = config
        .update_default_thinking(crate::model::ThinkingLevel::High, Some(base))
        .expect("thinking default updates");
    assert!(updated.contains("thinking = \"high\""));
    assert!(updated.contains("model = \"beta/gpt-4o\""));
    let updated = config
        .update_default_mode(crate::config::Mode::EvalFirst, Some(updated.as_str()))
        .expect("mode default updates");
    assert!(updated.contains("mode = \"eval-first\""));
    assert!(updated.contains("thinking = \"high\""));
    let updated = config
        .update_default_approval(crate::config::ApprovalMode::All, Some(updated.as_str()))
        .expect("approval default updates");
    assert!(updated.contains("approval = \"all\""));
    assert!(updated.contains("mode = \"eval-first\""));
    assert!(!updated.contains("model_route"));
    let reloaded = load(ConfigProduct::Dalgon, &updated).expect("updated TOML parses");
    assert_eq!(reloaded.model(), Some("beta/gpt-4o"));
    assert_eq!(reloaded.thinking(), crate::model::ThinkingLevel::High);
    assert_eq!(reloaded.mode(), crate::config::Mode::EvalFirst);
    assert_eq!(reloaded.approval(), crate::config::ApprovalMode::All);
}
