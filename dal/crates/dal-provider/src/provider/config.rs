//! Strict decoding of the provider layer's effective TOML configuration.

use dal_core::{Config, Family, ThinkingLevel};
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
    /// The global replay-fixture override from `[providers.scripted]`, if set.
    pub scripted: Option<ScriptedSelection>,
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
/// The global replay-fixture override decoded from `[providers.scripted]`.
///
/// When present, [`crate::provider::ProviderSet::provider`] serves one shared
/// replay [`crate::scripted::Script`] loaded from `fixture` for every resolved
/// route instead of building an HTTP provider. It is a test and headless-gate
/// override, never a model route: `scripted` is a reserved provider id and is
/// not resolvable through the catalog.
#[derive(Clone, Debug)]
pub struct ScriptedSelection {
    /// The replay-JSONL fixture path, as configured.
    pub fixture: Box<str>,
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
    /// Decode provider settings from the merged host configuration.
    ///
    /// Reads the `models`, `retry`, and `providers` sections through
    /// [`Config::section`], so the host never touches raw TOML. A section
    /// absent from the configuration decodes as its defaults.
    ///
    /// # Errors
    /// Returns a `ProviderConfigError` when a present section fails to decode.
    pub fn from_config(config: &Config) -> Result<Self, ProviderConfigError> {
        let (default_model, thinking, aliases) = decode_models(config.section("models"))?;
        let (request_max_retries, stream_max_retries) = decode_retry(config.section("retry"))?;
        let (providers, scripted) = decode_providers(config.section("providers"))?;
        Ok(Self {
            default_model,
            thinking,
            aliases,
            request_max_retries,
            stream_max_retries,
            providers,
            scripted,
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

/// The decoded `[models]` table: default model, thinking level, and aliases.
type ModelSettings = (Option<Box<str>>, ThinkingLevel, Vec<(Box<str>, Box<str>)>);

fn decode_models(value: Option<&toml::Value>) -> Result<ModelSettings, ProviderConfigError> {
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
        Some(value) => ThinkingLevel::deserialize(serde::de::value::StrDeserializer::<
            serde::de::value::Error,
        >::new(value))
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
            aliases.push((
                name.to_owned().into_boxed_str(),
                target.to_owned().into_boxed_str(),
            ));
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

fn decode_providers(
    value: Option<&toml::Value>,
) -> Result<(Vec<ProviderEntry>, Option<ScriptedSelection>), ProviderConfigError> {
    let table = optional_table(value, "providers")?;
    let mut providers = Vec::with_capacity(3 + table.map_or(0, toml::map::Map::len));
    let scripted = table
        .and_then(|table| table.get("scripted"))
        .map(decode_scripted)
        .transpose()?;
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
            if matches!(
                name.as_str(),
                "openai" | "openai-codex" | "anthropic" | "scripted"
            ) {
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

    Ok((providers, scripted))
}

const SCRIPTED_KEYS: &[&str] = &["fixture"];

fn decode_scripted(value: &toml::Value) -> Result<ScriptedSelection, ProviderConfigError> {
    let path = "providers.scripted";
    let Some(table) = value.as_table() else {
        return Err(invalid(format!("{path} must be a table")));
    };
    reject_unknown_keys(table, path, SCRIPTED_KEYS)?;
    let Some(fixture) = string_field(Some(table), "fixture", path)? else {
        return Err(ProviderConfigError::MissingKey {
            name: "scripted".into(),
            key: "fixture",
        });
    };
    if fixture.is_empty() {
        return Err(invalid(format!(
            "{path}.fixture must be a non-empty string"
        )));
    }
    Ok(ScriptedSelection {
        fixture: fixture.to_owned().into_boxed_str(),
    })
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
            return Err(invalid(format!(
                "{path}.api conflicts with its fixed family"
            )));
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

    let api = string_field(Some(table), "api", &path)?.ok_or_else(|| {
        ProviderConfigError::MissingKey {
            name: name.to_owned().into_boxed_str(),
            key: "api",
        }
    })?;
    let family = parse_api(name, api)?;
    let base_url = string_field(Some(table), "base_url", &path)?.ok_or_else(|| {
        ProviderConfigError::MissingKey {
            name: name.to_owned().into_boxed_str(),
            key: "base_url",
        }
    })?;
    let key_env = string_field(Some(table), "key_env", &path)?.ok_or_else(|| {
        ProviderConfigError::MissingKey {
            name: name.to_owned().into_boxed_str(),
            key: "key_env",
        }
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
    let auth = parse_auth(
        string_field(Some(table), "auth", &path)?,
        default_auth,
        name,
    )?;
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

fn concurrency_limit(table: Option<&toml::Table>, name: &str) -> Result<u32, ProviderConfigError> {
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
mod tests;
