use super::*;
use crate::provider::find;
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

fn entry<'a>(config: &'a ProviderConfig, id: &str) -> &'a ProviderEntry {
    config
        .providers
        .iter()
        .find(|provider| &*provider.id == id)
        .unwrap_or_else(|| panic!("{id} decoded"))
}

#[test]
fn defaults_return_builtin_providers_in_fixed_order() {
    let config = parse("").expect("empty config uses defaults");
    // No count and no indices: the defaults are exactly the table rows, and
    // the native vendor rows keep their relative order.
    for provider in &config.providers {
        assert!(
            find(&provider.id).is_some(),
            "{} is a default but not a built-in row",
            &*provider.id
        );
    }
    for def in PROVIDERS {
        assert!(
            config
                .providers
                .iter()
                .any(|provider| &*provider.id == def.id),
            "{} has no default entry",
            def.id
        );
    }
    let position = |id: &str| {
        config
            .providers
            .iter()
            .position(|provider| &*provider.id == id)
            .unwrap_or_else(|| panic!("{id} has no default entry"))
    };
    assert!(position("openai") < position("openai-codex"));
    assert!(position("openai-codex") < position("anthropic"));
}

#[test]
fn builtin_endpoints_and_family_defaults_are_stable() {
    let config = parse("").expect("empty config uses defaults");
    let openai = entry(&config, "openai");
    let codex = entry(&config, "openai-codex");
    let anthropic = entry(&config, "anthropic");
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
    assert_eq!(entry(&config, "openai").family, Family::Chat);
}
#[test]
fn configured_codex_api_must_match_its_builtin_family() {
    let config = parse("[providers.openai-codex]\napi = 'openai_codex'\n")
        .expect("the explicit fixed Codex family is accepted");
    assert_eq!(entry(&config, "openai-codex").family, Family::Codex);

    let error = parse("[providers.openai-codex]\napi = 'openai_responses'\n").unwrap_err();
    assert_eq!(
        error.to_string(),
        "providers.openai-codex.api conflicts with its fixed family"
    );
}

#[test]
fn a_compatible_preset_keeps_an_explicit_api_override_from_before_its_row() {
    // Before `synthetic` became a built-in preset, its table decoded through
    // the named-provider path. The new row must accept that same table and
    // build the same route: the Anthropic family, its `x-api-key` default,
    // and the family's header set. The `legacy-shim` control proves the two
    // paths agree field for field.
    let config = parse(concat!(
        "[providers.synthetic]\napi = 'anthropic'\nbase_url = 'https://example.test'\nkey_env = 'SYNTHETIC_API_KEY'\n",
        "[providers.legacy-shim]\napi = 'anthropic'\nbase_url = 'https://example.test'\nkey_env = 'SYNTHETIC_API_KEY'\n",
    ))
    .expect("a preset keeps an explicit api override");
    let preset = entry(&config, "synthetic");
    let named = entry(&config, "legacy-shim");
    for route in [preset, named] {
        assert_eq!(route.family, Family::Anthropic);
        assert_eq!(route.auth, AuthStyle::XApiKey);
        assert_eq!(&*route.base_url, "https://example.test");
        assert_eq!(route.key_env.as_deref(), Some("SYNTHETIC_API_KEY"));
        assert_eq!(route.shape(), Shape::Claude);
    }
    // The preset keeps its row (endpoint default, key material, key-only
    // sign-in); the named route carries none.
    assert_eq!(preset.def.map(|row| row.id), Some("synthetic"));
    assert!(named.def.is_none());
}

#[test]
fn a_compatible_preset_cannot_claim_the_oauth_only_codex_family() {
    let error = parse("[providers.synthetic]\napi = 'openai_codex'\n").unwrap_err();
    assert_eq!(
        error.to_string(),
        "providers.synthetic: unknown api \"openai_codex\"; use openai_chat, openai_responses, or anthropic."
    );
}

#[test]
fn a_compatible_preset_keeps_its_row_header_when_the_api_names_its_own_family() {
    // An explicit `api` that names the row's own family changes nothing: the
    // grounded bearer default of the `minimax` row still wins.
    let config = parse("[providers.minimax]\napi = 'anthropic'\n")
        .expect("the row's own family is accepted");
    let minimax = entry(&config, "minimax");
    assert_eq!(minimax.family, Family::Anthropic);
    assert_eq!(minimax.auth, AuthStyle::Bearer);
    assert_eq!(minimax.shape(), Shape::Claude);
}

#[test]
fn a_native_anthropic_api_mismatch_is_still_refused() {
    let error = parse("[providers.anthropic]\napi = 'openai_chat'\n").unwrap_err();
    assert_eq!(
        error.to_string(),
        "providers.anthropic.api conflicts with its fixed family"
    );
}

#[test]
fn a_native_openai_api_outside_its_documented_pair_is_still_refused() {
    let error = parse("[providers.openai]\napi = 'anthropic'\n").unwrap_err();
    assert_eq!(
        error.to_string(),
        "providers.openai.api must be openai_responses or openai_chat"
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
fn builtin_provider_settings_default_to_empty() {
    let config = parse("").expect("empty config uses defaults");
    // Empty tables carry nothing but the omission record: every built-in row
    // names no `base_url`, so preparation may resolve one from the captured
    // environment.
    let expected = ProviderSettings {
        base_url_defaulted: true,
        ..ProviderSettings::default()
    };
    assert!(
        config
            .providers
            .iter()
            .all(|provider| provider.settings == expected)
    );
}

#[test]
fn base_url_provenance_records_omission_not_equality() {
    let openai_default = find("openai").expect("the openai row exists").base_url;
    let config = parse(&format!(
        "[providers.openai]\nbase_url = '{openai_default}'\n"
    ))
    .expect("an explicit default URL is valid");
    // Explicitly configuring the default still wins: only omission is
    // recorded, never equality with the row default.
    assert!(!entry(&config, "openai").settings.base_url_defaulted);
    assert_eq!(&*entry(&config, "openai").base_url, openai_default);

    let config = parse("[providers.openai]\napi_version = '2025-01-01'\n")
        .expect("a settings-only table is valid");
    assert!(entry(&config, "openai").settings.base_url_defaulted);

    let config = parse(concat!(
        "[providers.zenmux]\napi = 'openai_chat'\n",
        "base_url = 'https://example.test'\nkey_env = 'ZENMUX_API_KEY'\n",
    ))
    .expect("a named provider is valid");
    assert!(!entry(&config, "zenmux").settings.base_url_defaulted);
}

#[test]
fn named_provider_settings_decode_from_their_own_table() {
    let config = parse(concat!(
        "[providers.zenmux]\napi = 'openai_chat'\n",
        "base_url = 'https://example.test'\nkey_env = 'ZENMUX_API_KEY'\n",
        "api_version = ' 2025-01-01 '\n",
        "deployments = { 'gpt-chat' = ' prod-gpt ' }\nregion = 'us-west-2'\n",
        "project = 'zenmux-project'\nlocation = 'us'\nnamespace = 'zenmux'\n",
        "binary = '/usr/local/bin/zenmux'\nintegration = 'vscode'\n",
    ))
    .expect("a named provider keeps its settings");
    let zenmux = entry(&config, "zenmux");
    assert_eq!(
        zenmux.settings,
        ProviderSettings {
            base_url_defaulted: false,
            api_version: Some("2025-01-01".into()),
            deployments: [("gpt-chat".into(), "prod-gpt".into())].into(),
            region: Some("us-west-2".into()),
            project: Some("zenmux-project".into()),
            location: Some("us".into()),
            namespace: Some("zenmux".into()),
            binary: Some("/usr/local/bin/zenmux".into()),
            integration: Some("vscode".into()),
        }
    );
    assert!(zenmux.def.is_none());
}

#[test]
fn deployments_decode_trimmed_values_and_dotted_model_ids() {
    // The parity the obsolete Azure TOML decoder pinned: trimmed values, and
    // a model id holding a dot as a quoted TOML key rather than a table path.
    let config = parse(concat!(
        "[providers.zenmux]\napi = 'openai_chat'\n",
        "base_url = 'https://example.test'\nkey_env = 'ZENMUX_API_KEY'\n",
        "api_version = ' 2025-03-01-preview '\n",
        "[providers.zenmux.deployments]\n\"gpt-5.5\" = ' prod '\nm = 'd2'\n",
    ))
    .expect("a deployments table is valid");
    let zenmux = entry(&config, "zenmux");
    assert_eq!(
        zenmux.settings.api_version.as_deref(),
        Some("2025-03-01-preview")
    );
    assert_eq!(
        zenmux
            .settings
            .deployments
            .get("gpt-5.5")
            .map(|name| &**name),
        Some("prod")
    );
    assert_eq!(
        zenmux.settings.deployments.get("m").map(|name| &**name),
        Some("d2")
    );
}

#[test]
fn builtin_settings_do_not_leak_into_named_providers() {
    let config = parse(concat!(
        "[providers.openai]\napi_version = '2025-01-01'\n",
        "[providers.zenmux]\napi = 'openai_chat'\n",
        "base_url = 'https://example.test'\nkey_env = 'ZENMUX_API_KEY'\n",
    ))
    .expect("a builtin setting and a named provider decode together");
    let openai = entry(&config, "openai");
    assert_eq!(openai.settings.api_version.as_deref(), Some("2025-01-01"));
    assert_eq!(openai.def.map(|row| row.id), Some("openai"));
    let zenmux = entry(&config, "zenmux");
    assert!(zenmux.settings.api_version.is_none());
    assert!(zenmux.def.is_none());
}

#[test]
fn named_provider_without_settings_still_decodes_to_empty_settings() {
    let config = parse(concat!(
        "[providers.zenmux]\napi = 'openai_chat'\n",
        "base_url = 'https://example.test'\nkey_env = 'ZENMUX_API_KEY'\n",
    ))
    .expect("a provider without settings keys is valid");
    assert_eq!(
        entry(&config, "zenmux").settings,
        ProviderSettings::default()
    );
}

#[test]
fn provider_settings_reject_invalid_values() {
    // Settings keys that refuse a wrong type or a blank value, with the
    // message each refusal renders.
    for (key, expected) in [
        (
            "api_version = 7",
            "providers.zenmux.api_version must be a non-empty string",
        ),
        (
            "api_version = ''",
            "providers.zenmux.api_version must be a non-empty string",
        ),
        (
            "api_version = '  '",
            "providers.zenmux.api_version must be a non-empty string",
        ),
        (
            "deployments = 7",
            "providers.zenmux.deployments must be a table",
        ),
        (
            "deployments = { prod = '' }",
            "providers.zenmux.deployments.prod must be a non-empty string",
        ),
        (
            "deployments = { prod = 3 }",
            "providers.zenmux.deployments.prod must be a non-empty string",
        ),
        (
            "region = 7",
            "providers.zenmux.region must be a non-empty string",
        ),
        (
            "project = 7",
            "providers.zenmux.project must be a non-empty string",
        ),
        (
            "location = 7",
            "providers.zenmux.location must be a non-empty string",
        ),
        (
            "namespace = 7",
            "providers.zenmux.namespace must be a non-empty string",
        ),
        (
            "binary = 7",
            "providers.zenmux.binary must be a non-empty string",
        ),
        (
            "integration = 7",
            "providers.zenmux.integration must be a non-empty string",
        ),
    ] {
        let input = format!(
            "[providers.zenmux]\napi = 'openai_chat'\nbase_url = 'https://example.test'\nkey_env = 'ZENMUX_API_KEY'\n{key}\n"
        );
        let error = parse(&input).unwrap_err();
        assert_eq!(error.to_string(), expected, "{key}");
    }
}

#[test]
fn named_anthropic_defaults_to_x_api_key_auth() {
    let config = parse(
            "[providers.zenmux-anthropic]\napi = 'anthropic'\nbase_url = 'https://example.test'\nkey_env = 'ZENMUX_API_KEY'\n",
        )
        .expect("named Anthropic config is valid");
    assert_eq!(entry(&config, "zenmux-anthropic").auth, AuthStyle::XApiKey);
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
    // The built-ins come first; only the named providers after them must keep
    // TOML insertion order, located by name rather than by index.
    let named: Vec<&str> = config
        .providers
        .iter()
        .filter(|provider| find(&provider.id).is_none())
        .map(|provider| &*provider.id)
        .collect();
    assert_eq!(named, ["zeta", "alpha"]);
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
