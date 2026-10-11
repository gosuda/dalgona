use super::*;
use dal_core::{Config, ConfigProduct};
use std::path::Path;

const OPENAI_URL: &str = "https://api.openai.com/v1";
const CODEX_URL: &str = "https://chatgpt.com/backend-api/codex";
const ANTHROPIC_URL: &str = "https://api.anthropic.com";

fn parse(input: &str) -> Result<ProviderConfig, ProviderConfigError> {
    let config = Config::load(
        ConfigProduct::Dalgon,
        Path::new("/tmp/dal-provider-config-test"),
        "",
        Some(input),
    )
    .expect("test dal.toml parses");
    ProviderConfig::from_config(&config)
}

#[test]
fn defaults_return_builtin_providers_in_fixed_order() {
    let config = parse("").expect("empty config uses defaults");
    let ids: Vec<&str> = config
        .providers
        .iter()
        .map(|provider| &*provider.id)
        .collect();
    assert_eq!(ids, ["openai", "openai-codex", "anthropic"]);
}

#[test]
fn builtin_endpoints_and_family_defaults_are_stable() {
    let config = parse("").expect("empty config uses defaults");
    let openai = &config.providers[0];
    let codex = &config.providers[1];
    let anthropic = &config.providers[2];
    assert_eq!(
        (&*openai.base_url, openai.family, openai.transport),
        (OPENAI_URL, Family::Responses, Transport::Https)
    );
    assert_eq!(
        (&*codex.base_url, codex.family, codex.transport),
        (CODEX_URL, Family::Codex, Transport::Websocket)
    );
    assert_eq!(
        (&*anthropic.base_url, anthropic.family, anthropic.auth),
        (ANTHROPIC_URL, Family::Anthropic, AuthStyle::XApiKey)
    );
}

#[test]
fn aliases_keep_toml_insertion_order() {
    let config =
        parse("[models.aliases]\nzeta = 'openai/gpt-5'\nalpha = 'anthropic/claude-sonnet-5'\n")
            .expect("aliases are valid");
    let names: Vec<&str> = config.aliases.iter().map(|(name, _)| &**name).collect();
    assert_eq!(names, ["zeta", "alpha"]);
}
#[test]
fn openai_builtin_can_select_the_chat_family() {
    let config = parse("[providers.openai]\napi = 'openai_chat'\n")
        .expect("OpenAI Chat is a supported built-in family");
    assert_eq!(config.providers[0].family, Family::Chat);
}
#[test]
fn configured_codex_api_must_match_its_builtin_family() {
    let config = parse("[providers.openai-codex]\napi = 'openai_codex'\n")
        .expect("the explicit fixed Codex family is accepted");
    assert_eq!(config.providers[1].family, Family::Codex);

    let error = parse("[providers.openai-codex]\napi = 'openai_responses'\n").unwrap_err();
    assert_eq!(
        error.to_string(),
        "providers.openai-codex.api conflicts with its fixed family"
    );
}

#[test]
fn unknown_nested_provider_keys_are_rejected_with_their_path() {
    let error = parse(
            "[providers.zenmux]\napi = 'openai_chat'\nbase_url = 'https://example.test'\nkey_env = 'ZENMUX_API_KEY'\nunknown = true\n",
        )
        .unwrap_err();
    assert_eq!(error.to_string(), "providers.zenmux.unknown: unknown key");
}

#[test]
fn invalid_provider_field_types_return_a_config_error() {
    let error = parse("[providers.zenmux]\napi = 1\n").unwrap_err();
    assert_eq!(error.to_string(), "providers.zenmux.api must be a string");
}

#[test]
fn unknown_model_keys_are_rejected() {
    let error = parse("[models]\nunknown = true\n").unwrap_err();
    assert_eq!(error.to_string(), "models.unknown: unknown key");
}

#[test]
fn retry_table_rejects_unknown_keys() {
    let error = parse("[retry]\nretries = 3\n").unwrap_err();
    assert_eq!(error.to_string(), "retry.retries: unknown key");
}

#[test]
fn defaults_apply_to_models_retries_and_concurrency() {
    let config = parse("").expect("empty config uses defaults");
    assert_eq!(config.default_model, None);
    assert_eq!(config.thinking, ThinkingLevel::Medium);
    assert_eq!(config.request_max_retries, 4);
    assert_eq!(config.stream_max_retries, 5);
    assert!(
        config.providers.iter().all(|provider| {
            provider.max_concurrent_requests == 4 && provider.key_env.is_none()
        })
    );
}

#[test]
fn named_provider_requires_a_key_environment_name() {
    let error =
        parse("[providers.zenmux]\napi = 'openai_chat'\nbase_url = 'https://example.test'\n")
            .unwrap_err();
    assert_eq!(error.to_string(), "providers.zenmux: missing key key_env.");
}

#[test]
fn dal_provider_id_is_reserved_before_named_fields_are_decoded() {
    let error = parse("[providers.dal]\n").unwrap_err();
    assert_eq!(error.to_string(), "providers.dal: the name is reserved.");
}

#[test]
fn websocket_requires_responses_for_named_providers() {
    let error = parse(
            "[providers.zenmux]\napi = 'openai_chat'\nbase_url = 'https://example.test'\nkey_env = 'ZENMUX_API_KEY'\ntransport = 'websocket'\n",
        )
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "providers.zenmux: transport websocket needs api openai_responses."
    );
}

#[test]
fn unknown_provider_api_uses_the_exact_config_error() {
    let error = parse(
            "[providers.zenmux]\napi = 'unknown'\nbase_url = 'https://example.test'\nkey_env = 'ZENMUX_API_KEY'\n",
        )
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "providers.zenmux: unknown api \"unknown\"; use openai_chat, openai_responses, or anthropic."
    );
}

#[test]
fn request_retry_limit_rejects_values_above_the_bound() {
    let error = parse("[retry]\nrequest_max_retries = 101\n").unwrap_err();
    assert_eq!(
        error.to_string(),
        "retry.request_max_retries: 101 is outside 0..=100."
    );
}

#[test]
fn stream_retry_limit_rejects_values_below_the_bound() {
    let error = parse("[retry]\nstream_max_retries = -1\n").unwrap_err();
    assert_eq!(
        error.to_string(),
        "retry.stream_max_retries: -1 is outside 0..=100."
    );
}

#[test]
fn provider_concurrency_rejects_zero_and_values_above_the_bound() {
    for (configured, expected) in [
        (
            0,
            "providers.zenmux.max_concurrent_requests: 0 is outside 1..=64.",
        ),
        (
            65,
            "providers.zenmux.max_concurrent_requests: 65 is outside 1..=64.",
        ),
    ] {
        let input = format!(
            "[providers.zenmux]\napi = 'openai_chat'\nbase_url = 'https://example.test'\nkey_env = 'ZENMUX_API_KEY'\nmax_concurrent_requests = {configured}\n"
        );
        let error = parse(&input).unwrap_err();
        assert_eq!(error.to_string(), expected);
    }
}

#[test]
fn empty_default_model_is_rejected() {
    let error = parse("[models]\ndefault = ''\n").unwrap_err();
    assert_eq!(
        error.to_string(),
        "models.default must be a non-empty string"
    );
}

#[test]
fn named_anthropic_defaults_to_x_api_key_auth() {
    let config = parse(
            "[providers.zenmux-anthropic]\napi = 'anthropic'\nbase_url = 'https://example.test'\nkey_env = 'ZENMUX_API_KEY'\n",
        )
        .expect("named Anthropic config is valid");
    assert_eq!(config.providers[3].auth, AuthStyle::XApiKey);
}
#[test]
fn openai_chat_websocket_reports_the_exact_transport_error() {
    let error =
        parse("[providers.openai]\napi = 'openai_chat'\ntransport = 'websocket'\n").unwrap_err();
    assert_eq!(
        error.to_string(),
        "providers.openai: transport websocket needs api openai_responses."
    );
}

#[test]
fn named_providers_keep_toml_insertion_order() {
    let config = parse(
            "[providers.zeta]\napi = 'openai_chat'\nbase_url = 'https://zeta.test'\nkey_env = 'ZETA_KEY'\n\n[providers.alpha]\napi = 'anthropic'\nbase_url = 'https://alpha.test'\nkey_env = 'ALPHA_KEY'\n",
        )
        .expect("named providers are valid");
    let ids: Vec<&str> = config.providers[3..]
        .iter()
        .map(|provider| &*provider.id)
        .collect();
    assert_eq!(ids, ["zeta", "alpha"]);
}

#[test]
fn scripted_table_selects_a_fixture_without_a_named_provider() {
    let config = parse("[providers.scripted]\nfixture = 'loop-headless.jsonl'\n")
        .expect("scripted override is valid");
    let scripted = config.scripted.expect("override is set");
    assert_eq!(&*scripted.fixture, "loop-headless.jsonl");
    assert!(
        config
            .providers
            .iter()
            .all(|provider| &*provider.id != "scripted")
    );
}

#[test]
fn scripted_table_requires_a_fixture_key() {
    let error = parse("[providers.scripted]\n").unwrap_err();
    assert!(matches!(
        error,
        ProviderConfigError::MissingKey { ref name, key: "fixture" }
        if &**name == "scripted"
    ));
}

#[test]
fn scripted_table_rejects_unknown_keys() {
    let error = parse("[providers.scripted]\nfixture = 'a.jsonl'\nbase_url = 'https://x.test'\n")
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "providers.scripted.base_url: unknown key"
    );
}

#[test]
fn scripted_fixture_must_be_a_non_empty_string() {
    let error = parse("[providers.scripted]\nfixture = ''\n").unwrap_err();
    assert_eq!(
        error.to_string(),
        "providers.scripted.fixture must be a non-empty string"
    );
    let error = parse("[providers.scripted]\nfixture = 7\n").unwrap_err();
    assert_eq!(
        error.to_string(),
        "providers.scripted.fixture must be a string"
    );
}

#[test]
fn scripted_table_must_be_a_table() {
    let error = parse("[providers]\nscripted = 'loop-headless.jsonl'\n").unwrap_err();
    assert_eq!(error.to_string(), "providers.scripted must be a table");
}

#[test]
fn absent_scripted_table_leaves_no_override() {
    let config = parse("").expect("empty config uses defaults");
    assert!(config.scripted.is_none());
}

#[test]
fn from_config_reads_combined_dal_toml() {
    let config = parse("[models]\ndefault = 'openai/gpt-chat-test'\n[retry]\nrequest_max_retries = 3\n[providers.zenmux]\napi = 'openai_chat'\nbase_url = 'https://example.test'\nkey_env = 'ZENMUX_API_KEY'\n")
            .expect("combined dal.toml decodes");
    assert_eq!(
        config.default_model.as_deref(),
        Some("openai/gpt-chat-test")
    );
    assert_eq!(config.request_max_retries, 3);
    assert_eq!(config.stream_max_retries, 5);
    let zenmux = config
        .providers
        .iter()
        .find(|provider| &*provider.id == "zenmux")
        .expect("named provider decoded");
    assert_eq!(zenmux.family, Family::Chat);
    assert_eq!(&*zenmux.base_url, "https://example.test");
}
