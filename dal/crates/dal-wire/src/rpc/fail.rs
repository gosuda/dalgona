//! Wire error objects: params decoding and the typed agent, host and store mappings.

use dal_agent::error::SchemeError;
use dal_agent::{AgentError, HostError};
use dal_store::{BlobError, StoreError};
use sonic_rs::Value;

use crate::jsonrpc::{ErrorObject, Id};

/// Builds a `-32602` error object for one method.
pub(crate) fn invalid_params(method: &str, detail: impl Into<String>) -> ErrorObject {
    ErrorObject {
        code: -32602,
        message: format!("invalid params for {method}: {}", detail.into()),
        data: None,
    }
}

/// Prefixes a bare `-32602` error with `invalid params for <method>`.
///
/// Every `-32602` reply leaves the server in that one shape, whichever
/// handler built it.
pub(crate) fn normalize_invalid_params(method: &str, error: ErrorObject) -> ErrorObject {
    if error.code == -32602 && !error.message.starts_with("invalid params for") {
        invalid_params(method, error.message)
    } else {
        error
    }
}

/// Builds the `-32009` error object for a server that no longer takes requests.
pub(crate) fn server_draining() -> ErrorObject {
    ErrorObject {
        code: -32009,
        message: "the server is shutting down and accepts no new requests".to_owned(),
        data: Some(sonic_rs::json!({
            "hint": "Wait for the server to start again, then reconnect.",
        })),
    }
}

/// Maps one request id to its cancel-table key.
pub(crate) fn id_key(id: &Id) -> String {
    match id {
        Id::Integer(number) => number.to_string(),
        Id::String(text) => text.clone(),
        Id::Null => "null".to_owned(),
    }
}

/// Decodes method params through a JSON round trip with a JSON-path error.
pub(crate) fn decode_params<T>(method: &str, params: &Value) -> Result<T, ErrorObject>
where
    T: serde::de::DeserializeOwned,
{
    let text = sonic_rs::to_string(params).map_err(|_| ErrorObject {
        code: -32603,
        message: "internal error: params are not encodable".to_owned(),
        data: Some(hint_value()),
    })?;
    sonic_rs::from_str::<T>(&text)
        .map_err(|error| invalid_params(method, json_path_error(&error.to_string())))
}

/// Trims a sonic decode error to its JSON-path suffix.
fn json_path_error(text: &str) -> String {
    text.split(" at line")
        .next()
        .unwrap_or(text)
        .trim()
        .to_owned()
}

/// Serializes one serializable value into a sonic [`Value`].
pub(crate) fn to_value<T>(value: &T) -> Result<Value, ErrorObject>
where
    T: serde::Serialize,
{
    let text = sonic_rs::to_string(value).map_err(|error| ErrorObject {
        code: -32603,
        message: format!("internal error: {error}"),
        data: Some(hint_value()),
    })?;
    sonic_rs::from_str::<Value>(&text).map_err(|error| ErrorObject {
        code: -32603,
        message: format!("internal error: {error}"),
        data: Some(hint_value()),
    })
}

/// Builds the `data.hint` value pointing at the process log.
pub(crate) fn hint_value() -> Value {
    Value::from("Report this with the log.")
}

/// Maps an [`AgentError`] to its stable wire error object.
pub(crate) fn agent_error(method: &str, error: AgentError) -> ErrorObject {
    match error {
        AgentError::WrongTurn { expected, actual } => ErrorObject {
            code: -32004,
            message: turn_mismatch_text(expected, &actual),
            data: None,
        },
        AgentError::AlreadyResolved { id, by } => ErrorObject {
            code: -32005,
            message: format!("request {id} was already resolved by {}.", by.as_str()),
            data: None,
        },
        AgentError::SteerFull { turn } => ErrorObject {
            code: -32603,
            message: format!("internal error: steer queue is full (16) for turn {turn}"),
            data: Some(hint_value()),
        },
        AgentError::Invalid(invalid) => invalid_params(method, invalid.to_string()),
        AgentError::SessionClosed { id } => ErrorObject {
            code: -32002,
            message: format!("session {id} is not open on this connection"),
            data: None,
        },
        AgentError::BlobNotFound { id, session } => ErrorObject {
            code: -32002,
            message: format!("blob {id} was not found in session {session}"),
            data: None,
        },
        AgentError::SessionGone { session } => ErrorObject {
            code: -32003,
            message: format!("session {session} was deleted"),
            data: None,
        },
        other => ErrorObject {
            code: -32603,
            message: format!("internal error: {other}"),
            data: Some(hint_value()),
        },
    }
}

/// Renders the `-32004` turn-mismatch text.
fn turn_mismatch_text(
    expected: dal_agent::error::ExpectedTurn,
    actual: &dal_agent::error::ActualTurn,
) -> String {
    use dal_agent::error::{ActualTurn, ExpectedTurn};
    let want = |expected| match expected {
        ExpectedTurn::Turn(turn) => format!("expected turn {turn}"),
        other => format!("expected {other}"),
    };
    match actual {
        ActualTurn::Turn { turn, .. } => match expected {
            ExpectedTurn::NoRunningTurn => {
                format!("expected an idle session, actual turn {turn} running")
            }
            other => format!("{}, actual turn {turn}", want(other)),
        },
        _ => format!("{}, actual session is idle", want(expected)),
    }
}

/// Maps a [`HostError`] to its stable wire error object.
pub(crate) fn host_error(error: HostError) -> ErrorObject {
    match error {
        HostError::SessionBusy { id, pid } => ErrorObject {
            code: -32008,
            message: match pid {
                Some(pid) => format!("session {id} is open in process {pid}"),
                None => format!("session {id} is open in another process"),
            },
            data: Some(sonic_rs::json!({
                "hint": "the session is open in another process; use --connect <addr> to reach it",
            })),
        },
        HostError::NotFound { message } => ErrorObject {
            code: -32002,
            message: message.into_string(),
            data: None,
        },
        HostError::Config { message } => ErrorObject {
            code: -32603,
            message: format!("internal error: {}", message.into_string()),
            data: Some(hint_value()),
        },
        HostError::Io { path, source } => ErrorObject {
            code: -32603,
            message: format!("internal error: {}: {source}", path.display()),
            data: Some(hint_value()),
        },
        HostError::Admission { limit } => ErrorObject {
            code: -32603,
            message: format!("internal error: admission wait expired: no free {limit} slot"),
            data: Some(hint_value()),
        },
        HostError::Closed => server_draining(),
        HostError::Store(error) => store_error(&error),
        HostError::Provider(error) => ErrorObject {
            code: -32603,
            message: format!("internal error: {error}"),
            data: Some(
                error
                    .fix()
                    .map_or_else(hint_value, |fix| Value::from(fix.as_str())),
            ),
        },
        other => ErrorObject {
            code: -32603,
            message: format!("internal error: {other}"),
            data: Some(hint_value()),
        },
    }
}

/// Maps a typed store failure to its stable wire error object.
pub(crate) fn store_error(error: &StoreError) -> ErrorObject {
    let message = error.to_string();
    match error {
        StoreError::Locked { .. } => ErrorObject {
            code: -32008,
            message,
            data: Some(sonic_rs::json!({
                "hint": "the session is open in another process; use --connect <addr> to reach it",
            })),
        },
        StoreError::NotFound { .. }
        | StoreError::NoMatch { .. }
        | StoreError::Ambiguous { .. }
        | StoreError::UnknownEntry { .. }
        | StoreError::Blob(BlobError::NotFound { .. }) => ErrorObject {
            code: -32002,
            message,
            data: None,
        },
        StoreError::Blob(BlobError::Gone) => ErrorObject {
            code: -32003,
            message,
            data: None,
        },
        StoreError::Invalid { .. }
        | StoreError::EmptyRef
        | StoreError::InvalidName
        | StoreError::NameTaken { .. }
        | StoreError::NotUserMessage { .. }
        | StoreError::NothingToClone { .. }
        | StoreError::ListLimit
        | StoreError::MalformedCursor
        | StoreError::Blob(BlobError::TooLarge { .. }) => ErrorObject {
            code: -32602,
            message,
            data: None,
        },
        StoreError::UnknownVersion { .. }
        | StoreError::Damaged { .. }
        | StoreError::WriteFailed { .. }
        | StoreError::Broken { .. }
        | StoreError::Io { .. }
        | StoreError::Journal(_)
        | StoreError::Blob(BlobError::Io { .. })
        | _ => ErrorObject {
            code: -32603,
            message: format!("internal error: {message}"),
            data: Some(hint_value()),
        },
    }
}

/// Maps a typed `docs/read` resolution failure to its wire error object.
pub(crate) fn scheme_error(uri: &str, error: SchemeError) -> ErrorObject {
    match error {
        SchemeError::Unknown { .. } if !uri.contains("://") => ErrorObject {
            code: -32602,
            message: format!("\"{uri}\" is not a document URI"),
            data: None,
        },
        SchemeError::Unknown { scheme } => ErrorObject {
            code: -32002,
            message: format!("no documents at {scheme}://"),
            data: None,
        },
        SchemeError::NotFound { uri } => ErrorObject {
            code: -32002,
            message: format!("no document at {uri}"),
            data: None,
        },
        SchemeError::Near { uri, nearest } => ErrorObject {
            code: -32002,
            message: format!("no document at {uri}"),
            data: Some(sonic_rs::json!({"hint": format!("Did you mean {nearest}?")})),
        },
        SchemeError::Store(error) => store_error(&error),
        other => ErrorObject {
            code: -32603,
            message: format!("internal error: {other}"),
            data: Some(hint_value()),
        },
    }
}
