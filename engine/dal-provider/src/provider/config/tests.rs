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
fn builtin_bedrock_entry_needs_no_key_env_and_defaults_the_runtime_base() {
    let config = parse("[providers.amazon-bedrock]\n")
        .expect("the built-in Bedrock row resolves without any key material");
    let bedrock = entry(&config, "amazon-bedrock");
    assert_eq!(bedrock.family, Family::Bedrock);
    assert_eq!(bedrock.auth, AuthStyle::Bearer);
    assert!(
        bedrock.key_env.is_none(),
        "the ambient AWS chain is the credential source, so no dummy key_env is required"
    );
    assert_eq!(bedrock.origins.base, Origin::RowDefault);
    assert_eq!(
        &*bedrock.base_url,
        "https://bedrock-runtime.us-east-1.amazonaws.com"
    );
}

#[test]
fn named_bedrock_entry_resolves_the_ambient_aws_chain_without_a_key_env() {
    // The reference signs in to the shared AWS credential chain without
    // storing any key material, so a bedrock entry needs no key_env.
    let config = parse(
        "[providers.bedrock-eu]\napi = 'bedrock'\nbase_url = 'https://bedrock-runtime.eu-west-1.amazonaws.com'\n",
    )
    .expect("the ambient AWS chain is a credential source of its own");
    let row = entry(&config, "bedrock-eu");
    assert_eq!(row.family, Family::Bedrock);
    assert_eq!(row.auth, AuthStyle::Bearer);
    assert!(
        row.key_env.is_none(),
        "an ambient-only entry names no key_env"
    );
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
    // Empty tables decode to default settings, and every built-in row names
    // no `base_url`, `api`, or `auth` of its own, so every origin records a
    // row default: preparation may resolve one from the captured
    // environment.
    assert!(
        config
            .providers
            .iter()
            .all(|provider| provider.settings == ProviderSettings::default())
    );
    assert!(
        config.providers.iter().all(|provider| provider.origins
            == Origins {
                api: Origin::RowDefault,
                base: Origin::RowDefault,
                auth: Origin::RowDefault,
            }),
        "an omitted key is a row default on every built-in row"
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
    assert_eq!(entry(&config, "openai").origins.base, Origin::Explicit);
    assert_eq!(&*entry(&config, "openai").base_url, openai_default);

    let config = parse("[providers.openai]\napi_version = '2025-01-01'\n")
        .expect("a settings-only table is valid");
    assert_eq!(entry(&config, "openai").origins.base, Origin::RowDefault);

    let config = parse(concat!(
        "[providers.zenmux]\napi = 'openai_chat'\n",
        "base_url = 'https://example.test'\nkey_env = 'ZENMUX_API_KEY'\n",
    ))
    .expect("a named provider is valid");
    assert_eq!(entry(&config, "zenmux").origins.base, Origin::Explicit);
}

#[test]
fn origin_records_follow_each_key_presence_independently() {
    // One explicit key marks exactly its own origin.
    let config = parse("[providers.openai]\napi = 'openai_responses'\n")
        .expect("an api-only table is valid");
    let openai = entry(&config, "openai");
    assert_eq!(openai.family, Family::Responses, "the explicit api applies");
    assert_eq!(openai.origins.api, Origin::Explicit);
    assert_eq!(openai.origins.base, Origin::RowDefault);
    assert_eq!(openai.origins.auth, Origin::RowDefault);

    let config =
        parse("[providers.openai]\nauth = 'bearer'\n").expect("an auth-only table is valid");
    let openai = entry(&config, "openai");
    assert_eq!(openai.origins.api, Origin::RowDefault);
    assert_eq!(openai.origins.base, Origin::RowDefault);
    assert_eq!(openai.origins.auth, Origin::Explicit);

    let config = parse("[providers.openai]\nbase_url = 'https://proxy.example.test/v1'\n")
        .expect("a base-only table is valid");
    let openai = entry(&config, "openai");
    assert_eq!(openai.origins.api, Origin::RowDefault);
    assert_eq!(openai.origins.base, Origin::Explicit);
    assert_eq!(openai.origins.auth, Origin::RowDefault);

    // A named provider always names `api` and `base_url`; `auth` per presence.
    let config = parse(concat!(
        "[providers.zenmux]\napi = 'openai_chat'\n",
        "base_url = 'https://example.test'\nkey_env = 'ZENMUX_API_KEY'\nauth = 'bearer'\n",
    ))
    .expect("a named provider with auth is valid");
    let zenmux = entry(&config, "zenmux");
    assert_eq!(
        zenmux.origins,
        Origins {
            api: Origin::Explicit,
            base: Origin::Explicit,
            auth: Origin::Explicit,
        }
    );
}

#[test]
fn cf_aig_authorization_parses_as_an_explicit_auth() {
    let config = parse("[providers.openai]\nauth = 'cf-aig-authorization'\n")
        .expect("the gateway token parses");
    let openai = entry(&config, "openai");
    assert_eq!(openai.auth, AuthStyle::CfAigAuthorization);
    assert_eq!(openai.origins.auth, Origin::Explicit);

    let config = parse(concat!(
        "[providers.zenmux]\napi = 'openai_chat'\n",
        "base_url = 'https://example.test'\nkey_env = 'ZENMUX_API_KEY'\n",
        "auth = 'cf-aig-authorization'\n",
    ))
    .expect("a named gateway entry parses");
    let zenmux = entry(&config, "zenmux");
    assert_eq!(zenmux.auth, AuthStyle::CfAigAuthorization);
    assert_eq!(zenmux.origins.auth, Origin::Explicit);
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
    assert_eq!(zenmux.origins.api, Origin::Explicit);
    assert_eq!(zenmux.origins.base, Origin::Explicit);
    assert_eq!(zenmux.origins.auth, Origin::RowDefault);
    assert_eq!(
        zenmux.settings,
        ProviderSettings {
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
fn auth_accepts_api_key_and_none_and_rejects_everything_else() {
    let bearer = parse(concat!(
        "[providers.zenmux-bearer]\napi = 'openai_chat'\n",
        "base_url = 'https://example.test'\nkey_env = 'ZENMUX_API_KEY'\nauth = 'bearer'\n",
        "[providers.zenmux-x-api-key]\napi = 'openai_chat'\n",
        "base_url = 'https://example.test'\nkey_env = 'ZENMUX_API_KEY'\nauth = 'x-api-key'\n",
        "[providers.zenmux-api-key]\napi = 'openai_chat'\n",
        "base_url = 'https://example.test'\nkey_env = 'ZENMUX_API_KEY'\nauth = 'api-key'\n",
        "[providers.zenmux-none]\napi = 'ollama'\nbase_url = 'http://localhost:11434'\n",
        "auth = 'none'\n",
    ))
    .expect("every supported auth style parses");
    assert_eq!(entry(&bearer, "zenmux-bearer").auth, AuthStyle::Bearer);
    assert_eq!(entry(&bearer, "zenmux-x-api-key").auth, AuthStyle::XApiKey);
    assert_eq!(entry(&bearer, "zenmux-api-key").auth, AuthStyle::ApiKey);
    assert_eq!(entry(&bearer, "zenmux-none").auth, AuthStyle::None);
    assert!(entry(&bearer, "zenmux-none").key_env.is_none());

    let error = parse(concat!(
        "[providers.zenmux]\napi = 'openai_chat'\n",
        "base_url = 'https://example.test'\nkey_env = 'ZENMUX_API_KEY'\nauth = 'basic'\n",
    ))
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "providers.zenmux.auth must be bearer, x-api-key, api-key, cf-aig-authorization, or none"
    );
}

#[test]
fn gemini_cli_named_providers_authenticate_nothing() {
    let config = parse(concat!(
        "[providers.local-gemini]\napi = 'google-gemini-cli'\nbase_url = 'https://example.test'\n",
        "key_env = 'GEMINI_CLI_API_KEY'\n",
    ))
    .expect("a gemini-cli provider is valid");
    let gemini = entry(&config, "local-gemini");
    assert_eq!(gemini.auth, AuthStyle::None);
    // A configured key name stays decodable under `none`: a leftover
    // `key_env`, or a cached login, must not break an explicit no-auth entry.
    assert_eq!(gemini.key_env.as_deref(), Some("GEMINI_CLI_API_KEY"));

    let error = parse(concat!(
        "[providers.local-gemini]\napi = 'google-gemini-cli'\nbase_url = 'https://example.test'\n",
        "key_env = 'GEMINI_CLI_API_KEY'\nauth = 'bearer'\n",
    ))
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "providers.local-gemini.auth must be none for api google-gemini-cli"
    );
}

#[test]
fn the_native_cloud_code_assist_row_signs_in_and_never_claims_the_local_gemini_cli() {
    // The signed-in row is its own family under its own id; the old id is no
    // built-in row, so a table named `google-gemini-cli` stays a user entry.
    assert!(find("google-gemini-cli").is_none());
    let row = find("google-cloud-code-assist").expect("the native row is built in");
    assert_eq!(row.family, Family::GoogleCloudCodeAssist);
    assert!(row.key.is_none(), "the row takes no API key");
    assert!(row.oauth.is_some(), "the row signs in with OAuth");

    let config = parse("").expect("defaults decode");
    let native = entry(&config, "google-cloud-code-assist");
    assert_eq!(native.family, Family::GoogleCloudCodeAssist);
    assert_eq!(native.auth, AuthStyle::Bearer);
    assert_eq!(&*native.base_url, row.base_url);
    assert!(native.def.is_some_and(|def| def.oauth.is_some()));
    assert!(native.key_env.is_none());

    // The row takes its own spelling and nothing that names the subprocess.
    let config = parse("[providers.google-cloud-code-assist]\napi = 'google-cloud-code-assist'\n")
        .expect("the row accepts its own api");
    assert_eq!(
        entry(&config, "google-cloud-code-assist").family,
        Family::GoogleCloudCodeAssist
    );
    let error =
        parse("[providers.google-cloud-code-assist]\napi = 'google-gemini-cli'\nauth = 'none'\n")
            .expect_refused("the subprocess cannot be named through the native id");
    assert_eq!(
        error.to_string(),
        "providers.google-cloud-code-assist.api conflicts with its fixed family"
    );

    // The row signs in with OAuth: `none` and every other style name a
    // credential it could never use, so they are refused rather than ignored.
    for auth in ["none", "x-api-key", "api-key"] {
        let error = parse(&format!(
            "[providers.google-cloud-code-assist]\nauth = '{auth}'\n"
        ))
        .expect_refused("the OAuth-only row takes only its bearer");
        assert_eq!(
            error.to_string(),
            "providers.google-cloud-code-assist.auth must be bearer for api google-cloud-code-assist",
            "auth = {auth}"
        );
    }
    let config = parse("[providers.google-cloud-code-assist]\nauth = 'bearer'\n")
        .expect("an explicit bearer is the default");
    assert_eq!(
        entry(&config, "google-cloud-code-assist").auth,
        AuthStyle::Bearer
    );

    // A user table under the old id with no `api` has nothing to decode: the
    // old id never became a built-in alias for the native row.
    let error = parse("[providers.google-gemini-cli]\nbase_url = 'https://example.test'\n")
        .expect_refused("an old-id table still needs its own api");
    assert!(matches!(
        &error,
        ProviderConfigError::MissingKey { name, key } if &**name == "google-gemini-cli" && *key == "api"
    ));

    // A user entry that names the old api stays the credential-less
    // subprocess, whatever the native row is.
    let config = parse(concat!(
        "[providers.google-gemini-cli]\napi = 'google-gemini-cli'\n",
        "base_url = 'https://example.test'\nkey_env = 'UNUSED'\n",
    ))
    .expect("a user entry may keep the old id and api");
    let local = entry(&config, "google-gemini-cli");
    assert_eq!(local.family, Family::GoogleGeminiCli);
    assert_eq!(local.auth, AuthStyle::None);
    assert!(local.def.is_none());
}

#[test]
fn an_entry_pointed_at_the_native_family_takes_a_bearer_default_and_keeps_its_family() {
    let config = parse("[providers.groq]\napi = 'google-cloud-code-assist'\n")
        .expect("a preset may be pointed at the native family");
    assert_eq!(entry(&config, "groq").family, Family::GoogleCloudCodeAssist);
    assert_eq!(entry(&config, "groq").auth, AuthStyle::Bearer);
    let error = parse("[providers.groq]\napi = 'google-cloud-code-assist'\nauth = 'none'\n")
        .expect_refused("the native family cannot be made credential-less");
    assert_eq!(
        error.to_string(),
        "providers.groq.auth must be bearer for api google-cloud-code-assist"
    );
    let config = parse(concat!(
        "[providers.my-cca]\napi = 'google-cloud-code-assist'\n",
        "base_url = 'https://example.test'\nkey_env = 'MY_CCA_KEY'\n",
    ))
    .expect("a named entry may use the native family");
    let named = entry(&config, "my-cca");
    assert_eq!(named.family, Family::GoogleCloudCodeAssist);
    assert_eq!(named.auth, AuthStyle::Bearer);
    assert_eq!(&*named.base_url, "https://example.test");
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

/// A named `zenmux` provider whose `api` is the TOML basic string `api` (escape
/// sequences allowed) with every required key, plus `extra` lines.
fn zenmux_api(api: &str, extra: &str) -> String {
    format!(
        "[providers.zenmux]\napi = \"{api}\"\nbase_url = 'https://example.test'\nkey_env = 'ZENMUX_API_KEY'\n{extra}\n"
    )
}

/// A named Chat `zenmux` provider with every required key, plus `extra` lines.
fn zenmux(extra: &str) -> String {
    zenmux_api("openai_chat", extra)
}

/// A refusal expected from [`parse`]. A surprise success reports `what` and the
/// number of decoded providers, not the whole decoded configuration.
trait ExpectRefused {
    fn expect_refused(self, what: &str) -> ProviderConfigError;
}

impl ExpectRefused for Result<ProviderConfig, ProviderConfigError> {
    fn expect_refused(self, what: &str) -> ProviderConfigError {
        match self {
            Err(error) => error,
            Ok(config) => panic!("{what}: accepted with {} providers", config.providers.len()),
        }
    }
}

/// Every `api` spelling `parse_api` accepts.
const API_SPELLINGS: [&str; 16] = [
    "openai_chat",
    "openai_responses",
    "anthropic",
    "bedrock",
    "cursor",
    "devin",
    "droid",
    "gitlab",
    "google-generative-ai",
    "google-vertex",
    "google-gemini-cli",
    "google-cloud-code-assist",
    "google-antigravity",
    "mistral",
    "ollama",
    "pi-messages",
];

#[test]
fn retry_limits_accept_both_inclusive_bounds_and_refuse_one_step_outside() {
    for key in ["request_max_retries", "stream_max_retries"] {
        let request = key == "request_max_retries";
        for accepted in [0_i64, 1, 99, 100] {
            let config = parse(&format!("[retry]\n{key} = {accepted}\n"))
                .unwrap_or_else(|error| panic!("{key} = {accepted}: {error}"));
            let (set, untouched) = if request {
                (config.request_max_retries, config.stream_max_retries)
            } else {
                (config.stream_max_retries, config.request_max_retries)
            };
            assert_eq!(i64::from(set), accepted, "{key}");
            assert_eq!(
                untouched,
                if request { 5 } else { 4 },
                "{key} moved the other limit"
            );
        }
        for refused in [-1_i64, 101, i64::MAX, i64::MIN] {
            let error = parse(&format!("[retry]\n{key} = {refused}\n"))
                .expect_refused(&format!("{key} = {refused} is outside 0..=100"));
            assert!(
                matches!(
                    &error,
                    ProviderConfigError::RetryOutOfRange { key: found, value }
                        if *found == key && *value == refused
                ),
                "{key} = {refused}: {error}"
            );
        }
        for wrong in ["3.5", "'3'", "true", "[3]"] {
            let error = parse(&format!("[retry]\n{key} = {wrong}\n"))
                .expect_refused(&format!("{key} = {wrong} is not an integer"));
            assert_eq!(
                error.to_string(),
                format!("retry.{key} must be an integer"),
                "{key} = {wrong}"
            );
        }
    }
}

#[test]
fn concurrency_accepts_one_and_the_documented_maximum_and_refuses_one_step_outside() {
    for id in ["zenmux", "groq"] {
        let input = |value: &str| {
            if id == "zenmux" {
                zenmux(&format!("max_concurrent_requests = {value}"))
            } else {
                format!("[providers.groq]\nmax_concurrent_requests = {value}\n")
            }
        };
        for accepted in [1_u32, 2, 63, 64] {
            let config = parse(&input(&accepted.to_string()))
                .unwrap_or_else(|error| panic!("{id} = {accepted}: {error}"));
            assert_eq!(entry(&config, id).max_concurrent_requests, accepted, "{id}");
        }
        for refused in [0_i64, -1, 65, i64::MIN, i64::MAX] {
            let error = parse(&input(&refused.to_string()))
                .expect_refused(&format!("{id} = {refused} is outside 1..=64"));
            assert!(
                matches!(
                    &error,
                    ProviderConfigError::ConcurrencyOutOfRange { name, value }
                        if &**name == id && *value == refused
                ),
                "{id} = {refused}: {error}"
            );
        }
        for wrong in ["4.0", "'4'", "true"] {
            let error =
                parse(&input(wrong)).expect_refused(&format!("{id} = {wrong} is not an integer"));
            assert_eq!(
                error.to_string(),
                format!("providers.{id}.max_concurrent_requests must be an integer"),
                "{id} = {wrong}"
            );
        }
    }
}

#[test]
fn a_builtin_row_honors_every_connection_override_of_its_own_table() {
    let config = parse(concat!(
        "[providers.openai]\nkey_env = 'MY_OPENAI_KEY'\nauth = 'api-key'\ntransport = 'websocket'\n",
        "base_url = 'https://proxy.example.test/v1'\nmax_concurrent_requests = 9\n",
        "[providers.openai-codex]\ntransport = 'https'\n",
        "[providers.groq]\nauth = 'none'\nkey_env = 'MY_GROQ_KEY'\n",
    ))
    .expect("built-in overrides are valid");
    let openai = entry(&config, "openai");
    assert_eq!(openai.key_env.as_deref(), Some("MY_OPENAI_KEY"));
    assert_eq!(openai.auth, AuthStyle::ApiKey);
    assert_eq!(openai.transport, Transport::Websocket);
    assert_eq!(&*openai.base_url, "https://proxy.example.test/v1");
    assert_eq!(openai.max_concurrent_requests, 9);
    assert_eq!(openai.origins.base, Origin::Explicit);
    assert_eq!(openai.origins.auth, Origin::Explicit);
    assert_eq!(entry(&config, "openai-codex").transport, Transport::Https);
    let groq = entry(&config, "groq");
    assert_eq!(groq.auth, AuthStyle::None);
    assert_eq!(groq.key_env.as_deref(), Some("MY_GROQ_KEY"));
    // A row whose table is absent keeps every default.
    let anthropic = entry(&config, "anthropic");
    assert_eq!(anthropic.key_env, None);
    assert_eq!(anthropic.auth, AuthStyle::XApiKey);
    assert_eq!(anthropic.transport, Transport::Https);
    assert_eq!(anthropic.max_concurrent_requests, 4);
}

#[test]
fn a_builtin_row_refuses_a_bad_connection_override_under_its_own_name() {
    for (input, expected) in [
        (
            "[providers.openai]\ntransport = 'ws'\n",
            "providers.openai.transport must be https or websocket",
        ),
        (
            "[providers.openai]\nauth = 'basic'\n",
            "providers.openai.auth must be bearer, x-api-key, api-key, cf-aig-authorization, or none",
        ),
        (
            "[providers.openai]\nunknown = 1\n",
            "providers.openai.unknown: unknown key",
        ),
        (
            "[providers.groq]\nunknown = 1\n",
            "providers.groq.unknown: unknown key",
        ),
        (
            "[providers.groq]\nmax_concurrent_requests = 0\n",
            "providers.groq.max_concurrent_requests: 0 is outside 1..=64.",
        ),
    ] {
        let error = parse(input).expect_refused(&format!("a bad override {input:?}"));
        assert_eq!(error.to_string(), expected, "{input}");
    }
}

#[test]
fn an_api_value_is_matched_exactly_without_case_folding_or_trimming() {
    for api in [
        "OpenAI_Chat",
        "OPENAI_CHAT",
        " openai_chat",
        "openai_chat ",
        "openai_chat\\n",
        "openai-chat",
        "chat",
        "",
    ] {
        for input in [
            zenmux_api(api, ""),
            format!("[providers.groq]\napi = \"{api}\"\n"),
        ] {
            let error = parse(&input).expect_refused(&format!("a respelled api {api:?}"));
            // The refusal carries the value as written, never a normalised one.
            let written = api.replace("\\n", "\n");
            assert!(
                matches!(
                    &error,
                    ProviderConfigError::UnknownApi { value, .. } if **value == *written
                ),
                "{api:?}: {error}"
            );
        }
    }
}

#[test]
fn a_transport_value_is_matched_exactly_without_case_folding_or_trimming() {
    for transport in [
        "HTTPS",
        "Https",
        " https",
        "https ",
        "WebSocket",
        "websocket\\n",
        "wss",
        "http",
        "",
    ] {
        for (id, input) in [
            ("zenmux", zenmux(&format!("transport = \"{transport}\""))),
            (
                "openai",
                format!("[providers.openai]\ntransport = \"{transport}\"\n"),
            ),
        ] {
            let error = parse(&input)
                .expect_refused(&format!("a respelled transport {transport:?} on {id}"));
            assert_eq!(
                error.to_string(),
                format!("providers.{id}.transport must be https or websocket"),
                "{transport:?}"
            );
        }
    }
}

#[test]
fn an_auth_value_is_matched_exactly_without_case_folding_or_trimming() {
    for auth in [
        "Bearer",
        "BEARER",
        " bearer",
        "bearer ",
        "none\\n",
        "NONE",
        "None",
        "x_api_key",
        "X-Api-Key",
        "apikey",
        "basic",
        "",
    ] {
        for (id, input) in [
            ("zenmux", zenmux(&format!("auth = \"{auth}\""))),
            ("groq", format!("[providers.groq]\nauth = \"{auth}\"\n")),
        ] {
            let error = parse(&input).expect_refused(&format!("a respelled auth {auth:?} on {id}"));
            assert_eq!(
                error.to_string(),
                format!(
                    "providers.{id}.auth must be bearer, x-api-key, api-key, cf-aig-authorization, or none"
                ),
                "{auth:?}"
            );
        }
    }
}

#[test]
fn websocket_is_refused_for_every_named_api_except_openai_responses() {
    for api in API_SPELLINGS {
        let result = parse(&zenmux_api(api, "transport = 'websocket'"));
        if api == "openai_responses" {
            let config = result.expect("Responses speaks WebSocket");
            assert_eq!(entry(&config, "zenmux").transport, Transport::Websocket);
        } else {
            assert!(
                matches!(
                    &result,
                    Err(ProviderConfigError::TransportMismatch { name }) if &**name == "zenmux"
                ),
                "{api}: {:?}",
                result.as_ref().map(|_| "accepted")
            );
        }
    }
}

#[test]
fn websocket_is_refused_for_every_builtin_row_whose_family_has_none() {
    for def in PROVIDERS {
        let result = parse(&format!(
            "[providers.{}]\ntransport = 'websocket'\n",
            def.id
        ));
        if matches!(def.family, Family::Responses | Family::Codex) {
            let config = result.unwrap_or_else(|error| panic!("{}: {error}", def.id));
            assert_eq!(entry(&config, def.id).transport, Transport::Websocket);
        } else {
            assert!(
                matches!(
                    &result,
                    Err(ProviderConfigError::TransportMismatch { name }) if &**name == def.id
                ),
                "{}: {:?}",
                def.id,
                result.as_ref().map(|_| "accepted")
            );
        }
    }
}

#[test]
fn a_native_row_accepts_only_its_own_api_spelling() {
    for (id, own) in [
        ("openai-codex", "openai_codex"),
        ("anthropic", "anthropic"),
        ("factory-droid", "droid"),
        ("gitlab-duo", "gitlab"),
        ("github-copilot", "openai_chat"),
        ("radius", "pi-messages"),
        ("google", "google-generative-ai"),
        ("google-vertex", "google-vertex"),
        ("google-cloud-code-assist", "google-cloud-code-assist"),
        ("google-antigravity", "google-antigravity"),
    ] {
        let row = find(id).expect("a native row");
        let config = parse(&format!("[providers.{id}]\napi = '{own}'\n"))
            .unwrap_or_else(|error| panic!("{id} refuses its own api {own}: {error}"));
        assert_eq!(entry(&config, id).family, row.family, "{id}");
        for other in API_SPELLINGS.iter().chain(&["openai_codex"]) {
            if *other == own {
                continue;
            }
            let error = parse(&format!("[providers.{id}]\napi = '{other}'\n"))
                .expect_refused(&format!("{id} refuses api {other}"));
            assert_eq!(
                error.to_string(),
                format!("providers.{id}.api conflicts with its fixed family"),
                "{id} with api {other}"
            );
        }
    }
    // The Responses-family rows take their documented Chat alternate and
    // nothing else.
    for id in ["openai", "openrouter"] {
        for other in API_SPELLINGS {
            if matches!(other, "openai_responses" | "openai_chat") {
                continue;
            }
            let error = parse(&format!("[providers.{id}]\napi = '{other}'\n"))
                .expect_refused(&format!("{id} refuses api {other}"));
            assert_eq!(
                error.to_string(),
                format!("providers.{id}.api must be openai_responses or openai_chat"),
                "{id} with api {other}"
            );
        }
    }
}

#[test]
fn a_retargeted_preset_takes_the_default_header_of_the_family_it_was_pointed_at() {
    for (id, api, family, auth) in [
        ("groq", "anthropic", Family::Anthropic, AuthStyle::XApiKey),
        (
            "groq",
            "openai_responses",
            Family::Responses,
            AuthStyle::Bearer,
        ),
        ("groq", "openai_chat", Family::Chat, AuthStyle::Bearer),
        ("xai", "openai_chat", Family::Chat, AuthStyle::Bearer),
        // The `umans` row is Anthropic with an `x-api-key` header; pointed at
        // a bearer family it must not keep that header.
        ("umans", "openai_chat", Family::Chat, AuthStyle::Bearer),
        ("umans", "anthropic", Family::Anthropic, AuthStyle::XApiKey),
        (
            "minimax",
            "openai_responses",
            Family::Responses,
            AuthStyle::Bearer,
        ),
        ("minimax", "anthropic", Family::Anthropic, AuthStyle::Bearer),
        ("ollama", "openai_chat", Family::Chat, AuthStyle::Bearer),
    ] {
        let row = find(id).expect("a preset row");
        let config = parse(&format!("[providers.{id}]\napi = '{api}'\n"))
            .unwrap_or_else(|error| panic!("{id} with api {api}: {error}"));
        let retargeted = entry(&config, id);
        assert_eq!(retargeted.family, family, "{id} with api {api}");
        assert_eq!(retargeted.auth, auth, "{id} with api {api}");
        assert_eq!(
            retargeted.shape(),
            Shape::of_family(family),
            "{id} with api {api}"
        );
        assert_eq!(
            &*retargeted.base_url, row.base_url,
            "{id} keeps its endpoint"
        );
        assert_eq!(retargeted.key_env, None, "{id}");
    }
    // An explicit `auth` still wins over the family default.
    let config = parse("[providers.groq]\napi = 'anthropic'\nauth = 'bearer'\n")
        .expect("an explicit auth is valid");
    assert_eq!(entry(&config, "groq").auth, AuthStyle::Bearer);
}

#[test]
fn a_preset_pointed_at_gemini_cli_authenticates_nothing_and_refuses_a_credential_header() {
    let config = parse("[providers.groq]\napi = 'google-gemini-cli'\n")
        .expect("a preset may be pointed at the CLI family");
    assert_eq!(entry(&config, "groq").family, Family::GoogleGeminiCli);
    assert_eq!(entry(&config, "groq").auth, AuthStyle::None);
    let config = parse("[providers.groq]\napi = 'google-gemini-cli'\nauth = 'none'\n")
        .expect("an explicit none is valid");
    assert_eq!(entry(&config, "groq").auth, AuthStyle::None);
    let error = parse("[providers.groq]\napi = 'google-gemini-cli'\nauth = 'bearer'\n")
        .expect_refused("the CLI family sends no credential");
    assert_eq!(
        error.to_string(),
        "providers.groq.auth must be none for api google-gemini-cli"
    );
}

#[test]
fn every_thinking_level_decodes_and_any_other_spelling_is_refused_verbatim() {
    for level in ["off", "minimal", "low", "medium", "high", "xhigh", "max"] {
        let config = parse(&format!("[models]\nthinking = '{level}'\n"))
            .unwrap_or_else(|error| panic!("{level}: {error}"));
        assert_eq!(config.thinking.name(), level);
    }
    for bad in [
        "", "ultra", "HIGH", "High", " high", "high ", "x-high", "none",
    ] {
        let error = parse(&format!("[models]\nthinking = \"{bad}\"\n"))
            .expect_refused(&format!("an unknown level {bad:?}"));
        assert_eq!(
            error.to_string(),
            format!("models.thinking has unsupported value {bad:?}")
        );
    }
}

#[test]
fn wrong_section_and_field_types_name_the_offending_path() {
    for (input, expected) in [
        ("models = 3\n", "models must be a table"),
        ("[models]\ndefault = 3\n", "models.default must be a string"),
        (
            "[models]\nthinking = 3\n",
            "models.thinking must be a string",
        ),
        ("[models]\naliases = 3\n", "models.aliases must be a table"),
        (
            "[models.aliases]\nfast = 3\n",
            "models.aliases.fast must be a string",
        ),
        ("retry = 3\n", "retry must be a table"),
        ("providers = 3\n", "providers must be a table"),
        (
            "[providers]\nzenmux = 3\n",
            "providers.zenmux must be a table",
        ),
        (
            "[providers]\nopenai = 3\n",
            "providers.openai must be a table",
        ),
        (
            "[providers.groq]\nbase_url = 1\n",
            "providers.groq.base_url must be a string",
        ),
        (
            "[providers.groq]\ntransport = 1\n",
            "providers.groq.transport must be a string",
        ),
        (
            "[providers.groq]\nauth = 1\n",
            "providers.groq.auth must be a string",
        ),
        (
            "[providers.groq]\nkey_env = 1\n",
            "providers.groq.key_env must be a string",
        ),
        (
            "[providers.groq]\napi = 1\n",
            "providers.groq.api must be a string",
        ),
    ] {
        let error = parse(input).expect_refused(&format!("a wrong type in {input:?}"));
        assert_eq!(error.to_string(), expected, "{input:?}");
    }
    for (key, expected) in [
        ("api = 1", "providers.zenmux.api must be a string"),
        ("base_url = 1", "providers.zenmux.base_url must be a string"),
        (
            "transport = 1",
            "providers.zenmux.transport must be a string",
        ),
        ("auth = 1", "providers.zenmux.auth must be a string"),
        ("key_env = 1", "providers.zenmux.key_env must be a string"),
    ] {
        let name = key.split(' ').next().expect("a key name");
        let mut lines: Vec<String> = zenmux("")
            .lines()
            .filter(|line| !line.starts_with(&format!("{name} =")))
            .map(String::from)
            .collect();
        lines.push(key.to_owned());
        let error = parse(&lines.join("\n")).expect_refused(&format!("a wrong type for {key}"));
        assert_eq!(error.to_string(), expected, "{key}");
    }
}

#[test]
fn each_missing_required_key_of_a_named_provider_is_named_and_api_is_reported_first() {
    for omitted in ["api", "base_url", "key_env"] {
        let lines: Vec<String> = zenmux("")
            .lines()
            .filter(|line| !line.starts_with(&format!("{omitted} =")))
            .map(String::from)
            .collect();
        let error = parse(&lines.join("\n")).expect_refused(&format!("{omitted} is missing"));
        assert_eq!(
            error.to_string(),
            format!("providers.zenmux: missing key {omitted}."),
            "{omitted}"
        );
    }
    let error = parse("[providers.zenmux]\n").expect_refused("an empty named table is refused");
    assert_eq!(error.to_string(), "providers.zenmux: missing key api.");
}

#[test]
fn blank_settings_are_refused_for_every_unicode_whitespace_spelling_and_padding_is_trimmed() {
    for key in ["api_version", "region", "location"] {
        for blank in ["\\t", "\\u00A0", "\\u3000", " \\u00A0 \\t"] {
            let error = parse(&zenmux(&format!("{key} = \"{blank}\"")))
                .expect_refused(&format!("{key} = {blank} is blank"));
            assert_eq!(
                error.to_string(),
                format!("providers.zenmux.{key} must be a non-empty string"),
                "{key} = {blank}"
            );
        }
    }
    let error = parse(&zenmux("deployments = { prod = \"\\u00A0\" }"))
        .expect_refused("a blank deployment name is refused");
    assert_eq!(
        error.to_string(),
        "providers.zenmux.deployments.prod must be a non-empty string"
    );
    let config = parse(&zenmux(
        "region = \"\\u00A0us-west-2\\u3000\"\ndeployments = { prod = \"\\u00A0blue\\t\" }",
    ))
    .expect("padding is trimmed away");
    let zenmux = entry(&config, "zenmux");
    assert_eq!(zenmux.settings.region.as_deref(), Some("us-west-2"));
    assert_eq!(
        zenmux.settings.deployments.get("prod").map(|name| &**name),
        Some("blue")
    );
}
