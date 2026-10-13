//! Small codecs and error constructors shared by compaction attempts.
//!
//! Secret redaction lives here so every failure path renders without tokens.

use dal_core::Family;
use serde::Deserialize;

use crate::error::ProviderError;

pub(crate) fn redact<S: AsRef<str>>(message: &mut String, secrets: &[(&str, S)]) {
    for (name, value) in secrets {
        let value = value.as_ref();
        let secret = if *name == "authorization" {
            value.strip_prefix("Bearer ").unwrap_or(value)
        } else {
            value
        };
        if !secret.is_empty() && message.contains(secret) {
            *message = message.replace(secret, "<redacted>");
        }
    }
}

#[derive(Default, Deserialize)]
pub(crate) struct ApiError {
    error: Option<ApiErrorFields>,
    code: Option<String>,
    message: Option<String>,
    detail: Option<String>,
}

#[derive(Deserialize)]
struct ApiErrorFields {
    code: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    message: Option<String>,
}

pub(crate) fn decode_api_error(bytes: &[u8]) -> ParsedApiError {
    let decoded = sonic_rs::from_slice::<ApiError>(bytes).unwrap_or_default();
    let (code, message) = match decoded.error {
        Some(error) => (
            error.code.or(error.kind).or(decoded.code),
            error.message.or(decoded.message).or(decoded.detail),
        ),
        None => (decoded.code, decoded.message.or(decoded.detail)),
    };
    ParsedApiError { code, message }
}

#[derive(Default)]
pub(crate) struct ParsedApiError {
    pub(crate) code: Option<String>,
    pub(crate) message: Option<String>,
}

pub(crate) fn redact_stream_error(error: ProviderError, token: &str) -> ProviderError {
    if token.is_empty() {
        return error;
    }
    let redact = |message: String| {
        if message.contains(token) {
            message.replace(token, "<redacted>")
        } else {
            message
        }
    };
    match error {
        ProviderError::Status {
            family,
            status,
            message,
        } => ProviderError::Status {
            family,
            status,
            message: redact(message),
        },
        ProviderError::ContextOverflow {
            family,
            code,
            message,
        } => ProviderError::ContextOverflow {
            family,
            code,
            message: redact(message),
        },
        ProviderError::RateLimited {
            message,
            retry_after,
        } => ProviderError::RateLimited {
            message: redact(message),
            retry_after,
        },
        ProviderError::UsageNotIncluded { message } => ProviderError::UsageNotIncluded {
            message: redact(message),
        },
        error => error,
    }
}

pub(crate) fn top_level_string(body: &[u8], key: &str) -> Result<String, ProviderError> {
    let body = std::str::from_utf8(body).map_err(|error| invalid_json("request body", error))?;
    for member in sonic_rs::to_object_iter(body) {
        let (name, value) = member.map_err(|error| invalid_json("request body", error))?;
        if name.as_ref() == key {
            let raw = value.as_raw_cow();
            return sonic_rs::from_str(raw.as_ref()).map_err(|error| invalid_json(key, error));
        }
    }
    Err(invalid(format!(
        "provider request body has no {key} member"
    )))
}

/// Gets the source-byte offset of a Sonic raw slice, after verifying its bytes.
pub(crate) fn source_offset(source: &[u8], raw: &str) -> Result<usize, ProviderError> {
    let start = (raw.as_ptr() as usize)
        .checked_sub(source.as_ptr() as usize)
        .ok_or_else(|| invalid("Sonic raw value precedes its request body"))?;
    let end = start
        .checked_add(raw.len())
        .ok_or_else(|| invalid("Sonic raw value offset overflowed"))?;
    if source.get(start..end) != Some(raw.as_bytes()) {
        return Err(invalid(
            "Sonic raw value does not belong to its request body",
        ));
    }
    Ok(start)
}

pub(crate) fn invalid(message: impl Into<String>) -> ProviderError {
    ProviderError::InvalidRequest {
        message: message.into(),
    }
}

pub(crate) fn invalid_json(what: &str, error: impl std::fmt::Display) -> ProviderError {
    invalid(format!("invalid {what} in provider request body: {error}"))
}

pub(crate) fn protocol(family: Family, detail: impl Into<String>) -> ProviderError {
    ProviderError::Protocol {
        family,
        detail: detail.into(),
    }
}

pub(crate) fn missing(family: Family, noun: &'static str) -> ProviderError {
    ProviderError::CompactionMissing { family, noun }
}
