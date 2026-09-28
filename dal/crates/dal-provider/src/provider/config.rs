//! Strict decoding of the provider layer's effective TOML configuration.

use dal_core::{Family, ThinkingLevel};
use serde::Deserialize;
use thiserror::Error;

/// The transport used to reach a provider.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Transport {
    /// HTTPS requests and streaming.
    Https,
    /// WebSocket transport for Responses and the built-in Codex API.
    Websocket,
}

/// The HTTP authorization header style used for API-key credentials.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthStyle {
    /// Send the credential in an `Authorization: Bearer` header.
    Bearer,
    /// Send the credential in an `x-api-key` header.
    XApiKey,
}

/// Provider-specific settings decoded from the effective configuration.
#[derive(Clone, Debug)]
pub struct ProviderConfig {
    /// The default model reference, if configured.
    pub default_model: Option<Box<str>>,
    /// The default reasoning intensity.
    pub thinking: ThinkingLevel,
    /// Model aliases in TOML insertion order.
    pub aliases: Vec<(Box<str>, Box<str>)>,
    /// Maximum request retries; the accepted range is 0 through 100.
    pub request_max_retries: u32,
    /// Maximum stream retries; the accepted range is 0 through 100.
    pub stream_max_retries: u32,
    /// Built-in providers followed by configured named providers.
    pub providers: Vec<ProviderEntry>,
}

/// One configured provider route and its connection defaults.
#[derive(Clone, Debug)]
pub struct ProviderEntry {
    /// The provider id used in model references.
    pub id: Box<str>,
    /// The API family this provider implements.
    pub family: Family,
    /// The provider endpoint base URL.
    pub base_url: Box<str>,
    /// The provider transport.
    pub transport: Transport,
    /// The environment variable name for a named provider's API key.
    pub key_env: Option<Box<str>>,
    /// The API-key authorization header style.
    pub auth: AuthStyle,
    /// The maximum number of simultaneous requests for this provider.
    pub max_concurrent_requests: u32,
}

/// An error while decoding or validating provider configuration.
#[derive(Debug, Error)]
pub enum ProviderConfigError {
    /// A TOML shape, type, or fixed-family validation error.
    #[error("{message}")]
    InvalidConfig {
        /// The offending path and its validation problem.
        message: Box<str>,
    },
    /// A provider specifies an API name outside the supported configuration set.
    #[error(
        "providers.{name}: unknown api \"{value}\"; use openai_chat, openai_responses, or anthropic."
    )]
    UnknownApi {
        /// The provider id containing the invalid API name.
        name: Box<str>,
        /// The unsupported API value.
        value: Box<str>,
    },
    /// A named provider omits a required setting.
    #[error("providers.{name}: missing key {key}.")]
    MissingKey {
        /// The named provider id.
        name: Box<str>,
        /// The absent configuration key.
        key: &'static str,
    },
    /// A named provider uses an id reserved by the application.
    #[error("providers.{name}: the name is reserved.")]
    ReservedName {
        /// The reserved provider id.
        name: Box<str>,
    },
    /// WebSocket transport was selected for an unsupported API family.
    #[error("providers.{name}: transport websocket needs api openai_responses.")]
    TransportMismatch {
        /// The provider id with the unsupported transport.
        name: Box<str>,
    },
    /// A retry limit is outside its permitted range.
    #[error("retry.{key}: {value} is outside 0..=100.")]
    RetryOutOfRange {
        /// The retry configuration key.
        key: &'static str,
        /// The configured value.
        value: i64,
    },
    /// A provider concurrency limit is outside its permitted range.
    #[error("providers.{name}.max_concurrent_requests: {value} is outside 1..=64.")]
    ConcurrencyOutOfRange {
        /// The provider id with the invalid limit.
        name: Box<str>,
        /// The configured value.
        value: i64,
    },
}

impl ProviderConfig {
    /// Decode provider settings from the already-merged effective TOML value.
    ///
    /// Only `[models]`, `[retry]`, and `[providers]` are read here. Other root
    /// keys belong to the core configuration layer and are intentionally left
    /// untouched.
    pub fn from_toml(value: &toml::Value) -> Result<Self, ProviderConfigError> {
        let root = value.as_table().ok_or_else(|| invalid("config must be a table"))?;
        let (default_model, thinking, aliases) = decode_models(root.get("models"))?;
        let (request_max_retries, stream_max_retries) = decode_retry(root.get("retry"))?;
        let providers = decode_providers(root.get("providers"))?;

        Ok(Self {
            default_model,
            thinking,
            aliases,
            request_max_retries,
            stream_max_retries,
            providers,
        })
    }
}

const OPENAI_URL: &str = "https://api.openai.com/v1";
const CODEX_URL: &str = "https://chatgpt.com/backend-api/codex";
const ANTHROPIC_URL: &str = "https://api.anthropic.com";
pub(super) const MAX_CONCURRENT_REQUESTS: u32 = 64;

const MODEL_KEYS: &[&str] = &["default", "thinking", "aliases"];
const RETRY_KEYS: &[&str] = &["request_max_retries", "stream_max_retries"];
const PROVIDER_KEYS: &[&str] = &[
    "api",
    "base_url",
    "transport",
    "key_env",
    "auth",
    "max_concurrent_requests",
];

fn invalid(message: impl Into<Box<str>>) -> ProviderConfigError {
    ProviderConfigError::InvalidConfig {
        message: message.into(),
    }
}

fn optional_table<'a>(
    value: Option<&'a toml::Value>,
    path: &str,
) -> Result<Option<&'a toml::Table>, ProviderConfigError> {
    match value {
        None => Ok(None),
        Some(toml::Value::Table(table)) => Ok(Some(table)),
        Some(_) => Err(invalid(format!("{path} must be a table"))),
    }
}

fn reject_unknown_keys(
    table: &toml::Table,
    path: &str,
    allowed: &[&str],
) -> Result<(), ProviderConfigError> {
    if let Some(key) = table.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid(format!("{path}.{key}: unknown key")));
    }
    Ok(())
}

fn string_field<'a>(
    table: Option<&'a toml::Table>,
    key: &str,
    path: &str,
) -> Result<Option<&'a str>, ProviderConfigError> {
    let Some(value) = table.and_then(|table| table.get(key)) else {
        return Ok(None);
    };
    value
        .as_str()
        .map(Some)
        .ok_or_else(|| invalid(format!("{path}.{key} must be a string")))
}

fn decode_models(
    value: Option<&toml::Value>,
) -> Result<(Option<Box<str>>, ThinkingLevel, Vec<(Box<str>, Box<str>)>), ProviderConfigError> {
    let Some(table) = optional_table(value, "models")? else {
        return Ok((None, ThinkingLevel::Medium, Vec::new()));
    };
    reject_unknown_keys(table, "models", MODEL_KEYS)?;

    let default_model = string_field(Some(table), "default", "models")?
        .map(|model| {
            if model.is_empty() {
                Err(invalid("models.default must be a non-empty string"))
            } else {
                Ok(model.to_owned().into_boxed_str())
            }
        })
        .transpose()?;

    let thinking = match string_field(Some(table), "thinking", "models")? {
        None => ThinkingLevel::Medium,
        Some(value) => ThinkingLevel::deserialize(
            serde::de::value::StrDeserializer::<serde::de::value::Error>::new(value),
        )
        .map_err(|_| invalid(format!("models.thinking has unsupported value {value:?}")))?,
    };

    let mut aliases = Vec::new();
    if let Some(value) = table.get("aliases") {
        let Some(alias_table) = value.as_table() else {
            return Err(invalid("models.aliases must be a table"));
        };
        aliases.reserve(alias_table.len());
        for (name, target) in alias_table {
            let Some(target) = target.as_str() else {
                return Err(invalid(format!("models.aliases.{name} must be a string")));
            };
            aliases.push((name.to_owned().into_boxed_str(), target.to_owned().into_boxed_str()));
        }
    }

    Ok((default_model, thinking, aliases))
}

fn decode_retry(value: Option<&toml::Value>) -> Result<(u32, u32), ProviderConfigError> {
    let table = optional_table(value, "retry")?;
    if let Some(table) = table {
        reject_unknown_keys(table, "retry", RETRY_KEYS)?;
    }

    let request = retry_limit(table, "request_max_retries", 4)?;
    let stream = retry_limit(table, "stream_max_retries", 5)?;
    Ok((request, stream))
}

fn retry_limit(
    table: Option<&toml::Table>,
    key: &'static str,
    default: u32,
) -> Result<u32, ProviderConfigError> {
    let Some(value) = table.and_then(|table| table.get(key)) else {
        return Ok(default);
    };
    let Some(value) = value.as_integer() else {
        return Err(invalid(format!("retry.{key} must be an integer")));
    };
    if !(0..=100).contains(&value) {
        return Err(ProviderConfigError::RetryOutOfRange { key, value });
    }
    u32::try_from(value).map_err(|_| invalid(format!("retry.{key} must fit in u32")))
}

fn decode_providers(value: Option<&toml::Value>) -> Result<Vec<ProviderEntry>, ProviderConfigError> {
    let table = optional_table(value, "providers")?;
    let mut providers = Vec::with_capacity(3 + table.map_or(0, |table| table.len()));

    providers.push(decode_builtin(
        table.and_then(|table| table.get("openai")),
        "openai",
        Family::Responses,
        OPENAI_URL,
        Transport::Https,
        AuthStyle::Bearer,
    )?);
    providers.push(decode_builtin(
        table.and_then(|table| table.get("openai-codex")),
        "openai-codex",
        Family::Codex,
        CODEX_URL,
        Transport::Websocket,
        AuthStyle::Bearer,
    )?);
    providers.push(decode_builtin(
        table.and_then(|table| table.get("anthropic")),
        "anthropic",
        Family::Anthropic,
        ANTHROPIC_URL,
        Transport::Https,
        AuthStyle::XApiKey,
    )?);

    if let Some(table) = table {
        for (name, value) in table {
            if matches!(name.as_str(), "openai" | "openai-codex" | "anthropic") {
                continue;
            }
            if name == "dal" {
                return Err(ProviderConfigError::ReservedName {
                    name: name.to_owned().into_boxed_str(),
                });
            }
            providers.push(decode_named(name, value)?);
        }
    }

    Ok(providers)
}

fn decode_builtin(
    value: Option<&toml::Value>,
    name: &'static str,
    default_family: Family,
    default_url: &'static str,
    default_transport: Transport,
    default_auth: AuthStyle,
) -> Result<ProviderEntry, ProviderConfigError> {
    let path = format!("providers.{name}");
    let table = optional_table(value, &path)?;
    if let Some(table) = table {
        reject_unknown_keys(table, &path, PROVIDER_KEYS)?;
    }

    let family = match string_field(table, "api", &path)? {
        None => default_family,
        Some(value) if name == "openai" => {
            let family = parse_api(name, value)?;
            if matches!(family, Family::Responses | Family::Chat) {
                family
            } else {
                return Err(invalid(format!(
                    "{path}.api must be openai_responses or openai_chat"
                )));
            }
        }
        Some("openai_codex") if name == "openai-codex" => default_family,
        Some("anthropic") if name == "anthropic" => default_family,
        Some(_) => {
            return Err(invalid(format!("{path}.api conflicts with its fixed family")));
        }
    };

    let url = string_field(table, "base_url", &path)?
        .unwrap_or(default_url)
        .to_owned()
        .into_boxed_str();
    let transport = parse_transport(
        string_field(table, "transport", &path)?,
        default_transport,
        name,
    )?;
    let auth = parse_auth(string_field(table, "auth", &path)?, default_auth, name)?;
    let key_env =
        string_field(table, "key_env", &path)?.map(|value| value.to_owned().into_boxed_str());
    let max_concurrent_requests = concurrency_limit(table, name)?;

    validate_transport(name, family, transport)?;

    Ok(ProviderEntry {
        id: name.into(),
        family,
        base_url: url,
        transport,
        key_env,
        auth,
        max_concurrent_requests,
    })
}

fn decode_named(name: &str, value: &toml::Value) -> Result<ProviderEntry, ProviderConfigError> {
    let path = format!("providers.{name}");
    let Some(table) = value.as_table() else {
        return Err(invalid(format!("{path} must be a table")));
    };
    reject_unknown_keys(table, &path, PROVIDER_KEYS)?;

    let api = string_field(Some(table), "api", &path)?
        .ok_or_else(|| ProviderConfigError::MissingKey {
            name: name.to_owned().into_boxed_str(),
            key: "api",
        })?;
    let family = parse_api(name, api)?;
    let base_url = string_field(Some(table), "base_url", &path)?
        .ok_or_else(|| ProviderConfigError::MissingKey {
            name: name.to_owned().into_boxed_str(),
            key: "base_url",
        })?;
    let key_env = string_field(Some(table), "key_env", &path)?
        .ok_or_else(|| ProviderConfigError::MissingKey {
            name: name.to_owned().into_boxed_str(),
            key: "key_env",
        })?;

    let transport = parse_transport(
        string_field(Some(table), "transport", &path)?,
        Transport::Https,
        name,
    )?;
    let default_auth = if family == Family::Anthropic {
        AuthStyle::XApiKey
    } else {
        AuthStyle::Bearer
    };
    let auth = parse_auth(string_field(Some(table), "auth", &path)?, default_auth, name)?;
    let max_concurrent_requests = concurrency_limit(Some(table), name)?;
    validate_transport(name, family, transport)?;

    Ok(ProviderEntry {
        id: name.to_owned().into_boxed_str(),
        family,
        base_url: base_url.to_owned().into_boxed_str(),
        transport,
        key_env: Some(key_env.to_owned().into_boxed_str()),
        auth,
        max_concurrent_requests,
    })
}

fn parse_api(name: &str, value: &str) -> Result<Family, ProviderConfigError> {
    match value {
        "openai_chat" => Ok(Family::Chat),
        "openai_responses" => Ok(Family::Responses),
        "anthropic" => Ok(Family::Anthropic),
        _ => Err(ProviderConfigError::UnknownApi {
            name: name.to_owned().into_boxed_str(),
            value: value.to_owned().into_boxed_str(),
        }),
    }
}

fn parse_transport(
    value: Option<&str>,
    default: Transport,
    name: &str,
) -> Result<Transport, ProviderConfigError> {
    match value {
        None => Ok(default),
        Some("https") => Ok(Transport::Https),
        Some("websocket") => Ok(Transport::Websocket),
        Some(_) => Err(invalid(format!(
            "providers.{name}.transport must be https or websocket"
        ))),
    }
}

fn parse_auth(
    value: Option<&str>,
    default: AuthStyle,
    name: &str,
) -> Result<AuthStyle, ProviderConfigError> {
    match value {
        None => Ok(default),
        Some("bearer") => Ok(AuthStyle::Bearer),
        Some("x-api-key") => Ok(AuthStyle::XApiKey),
        Some(_) => Err(invalid(format!(
            "providers.{name}.auth must be bearer or x-api-key"
        ))),
    }
}

fn concurrency_limit(
    table: Option<&toml::Table>,
    name: &str,
) -> Result<u32, ProviderConfigError> {
    let Some(value) = table.and_then(|table| table.get("max_concurrent_requests")) else {
        return Ok(4);
    };
    let Some(value) = value.as_integer() else {
        return Err(invalid(format!(
            "providers.{name}.max_concurrent_requests must be an integer"
        )));
    };
    if !(1..=i64::from(MAX_CONCURRENT_REQUESTS)).contains(&value) {
        return Err(ProviderConfigError::ConcurrencyOutOfRange {
            name: name.to_owned().into_boxed_str(),
            value,
        });
    }
    u32::try_from(value).map_err(|_| {
        invalid(format!(
            "providers.{name}.max_concurrent_requests must fit in u32"
        ))
    })
}

fn validate_transport(
    name: &str,
    family: Family,
    transport: Transport,
) -> Result<(), ProviderConfigError> {
    if transport == Transport::Websocket
        && family != Family::Responses
        && !(name == "openai-codex" && family == Family::Codex)
    {
        return Err(ProviderConfigError::TransportMismatch {
            name: name.to_owned().into_boxed_str(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> Result<ProviderConfig, ProviderConfigError> {
        let value: toml::Value = toml::from_str(input).expect("test input is valid TOML");
        ProviderConfig::from_toml(&value)
    }

    #[test]
    fn defaults_return_builtin_providers_in_fixed_order() {
        let config = parse("").expect("empty config uses defaults");
        let ids: Vec<&str> = config.providers.iter().map(|provider| &*provider.id).collect();
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
        let config = parse(
            "[models.aliases]\nzeta = 'openai/gpt-5'\nalpha = 'anthropic/claude-sonnet-5'\n",
        )
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
        assert!(config.providers.iter().all(|provider| {
            provider.max_concurrent_requests == 4 && provider.key_env.is_none()
        }));
    }

    #[test]
    fn named_provider_requires_a_key_environment_name() {
        let error = parse(
            "[providers.zenmux]\napi = 'openai_chat'\nbase_url = 'https://example.test'\n",
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "providers.zenmux: missing key key_env."
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
            (0, "providers.zenmux.max_concurrent_requests: 0 is outside 1..=64."),
            (65, "providers.zenmux.max_concurrent_requests: 65 is outside 1..=64."),
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
        assert_eq!(error.to_string(), "models.default must be a non-empty string");
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
        let error = parse("[providers.openai]\napi = 'openai_chat'\ntransport = 'websocket'\n")
            .unwrap_err();
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
}
