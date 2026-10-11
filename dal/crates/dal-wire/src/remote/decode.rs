//! Decoders from version-1 wire values to core values.

use std::num::NonZeroU64;

use dal_core::{Gen, Seq, SessionId, UpdateKind};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sonic_rs::{JsonValueTrait, Value};

use super::RemoteHostUpdate;
use crate::error::WireError;
use crate::jsonrpc::ErrorObject;

/// Maps a JSON-RPC error object to its wire error.
pub(super) fn protocol_error(error: ErrorObject) -> WireError {
    WireError::Protocol {
        code: error.code,
        message: error.message,
    }
}

/// Builds a frame-decoding error naming the offending member.
pub(super) fn malformed(what: &str) -> WireError {
    WireError::Protocol {
        code: -32700,
        message: format!("remote frame has an invalid {what}"),
    }
}

/// Decodes one serde value from a wire value.
///
/// Decoding goes through JSON text: core types with raw-JSON members reject
/// `sonic_rs::from_value`.
pub(super) fn decode<T: DeserializeOwned>(value: &Value, what: &str) -> Result<T, WireError> {
    let text = sonic_rs::to_string(value).map_err(|_| WireError::Frame)?;
    sonic_rs::from_str(&text).map_err(|error| WireError::Protocol {
        code: -32700,
        message: format!("remote frame has an invalid {what}: {error}"),
    })
}

/// Encodes one serde value as a wire value.
pub(super) fn encode<T: Serialize>(value: &T) -> Result<Value, WireError> {
    let text = sonic_rs::to_string(value).map_err(|_| WireError::Frame)?;
    sonic_rs::from_str(&text).map_err(|_| WireError::Frame)
}

/// Reads one required string member.
pub(super) fn string<'a>(value: &'a Value, name: &str) -> Result<&'a str, WireError> {
    value
        .get(name)
        .and_then(|member| member.as_str())
        .ok_or_else(|| malformed(name))
}

/// Reads one optional string member.
pub(super) fn opt_string(value: &Value, name: &str) -> Option<String> {
    value
        .get(name)
        .and_then(|member| member.as_str())
        .map(str::to_owned)
}

/// Reads one required session id member.
pub(super) fn session_id(value: &Value, name: &str) -> Result<SessionId, WireError> {
    SessionId::parse(string(value, name)?).map_err(|_| malformed(name))
}

/// Reads one required nonzero counter member.
fn counter(value: &Value, name: &str) -> Result<NonZeroU64, WireError> {
    value
        .get(name)
        .and_then(JsonValueTrait::as_u64)
        .and_then(NonZeroU64::new)
        .ok_or_else(|| malformed(name))
}

/// Reads the `{gen, seq}` pair of a subscribe reply or update.
pub(super) fn cursor(value: &Value) -> Result<(Gen, Seq), WireError> {
    Ok((
        Gen::new(counter(value, "gen")?),
        Seq::new(counter(value, "seq")?),
    ))
}

/// One decoded `session/update`: session, cursor, and kind (`None` for `resync`).
pub(super) type SessionUpdate = (SessionId, (Gen, Seq), Option<UpdateKind>);

/// Decodes one `session/update`; a `None` kind is a server `resync`.
pub(super) fn session_update(params: &Value) -> Result<SessionUpdate, WireError> {
    let session = session_id(params, "sessionId")?;
    let pair = cursor(params)?;
    let update = params.get("update").ok_or_else(|| malformed("update"))?;
    if update.get("type").and_then(|tag| tag.as_str()) == Some("resync") {
        return Ok((session, pair, None));
    }
    Ok((session, pair, Some(decode(update, "update")?)))
}

/// Decodes one `host/update`; unknown update types decode to `None`.
pub(super) fn host_update(params: &Value) -> Result<Option<RemoteHostUpdate>, WireError> {
    let update = params.get("update").ok_or_else(|| malformed("update"))?;
    let decoded = match string(update, "type")? {
        "session_changed" => RemoteHostUpdate::SessionChanged(Box::new(decode(
            update.get("session").ok_or_else(|| malformed("session"))?,
            "session",
        )?)),
        "session_removed" => RemoteHostUpdate::SessionRemoved(session_id(update, "sessionId")?),
        "child_started" => RemoteHostUpdate::ChildStarted {
            session: session_id(update, "sessionId")?,
            parent: session_id(update, "parentId")?,
        },
        "child_ended" => RemoteHostUpdate::ChildEnded {
            session: session_id(update, "sessionId")?,
            parent: session_id(update, "parentId")?,
        },
        "login_finished" => RemoteHostUpdate::LoginFinished {
            login_id: update
                .get("loginId")
                .and_then(JsonValueTrait::as_u64)
                .ok_or_else(|| malformed("loginId"))?,
            provider: string(update, "provider")?.to_owned(),
            ready: string(update, "state")? == "ready",
            detail: opt_string(update, "detail"),
        },
        other => {
            tracing::debug!(r#type = other, "ignoring unknown host update");
            return Ok(None);
        }
    };
    Ok(Some(decoded))
}
