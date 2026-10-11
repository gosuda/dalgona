use serde::{Serialize, Serializer};
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};

use crate::error::WireError;

/// A JSON-RPC request identifier accepted by the dal protocol.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Id {
    /// A signed integer identifier.
    Integer(i64),
    /// A string identifier.
    String(String),
    /// A null identifier.
    Null,
}

impl Serialize for Id {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Integer(value) => serializer.serialize_i64(*value),
            Self::String(value) => serializer.serialize_str(value),
            Self::Null => serializer.serialize_unit(),
        }
    }
}

/// The standard error object in a JSON-RPC error response.
#[derive(Clone, Debug, Serialize)]
pub struct ErrorObject {
    /// The JSON-RPC error code.
    pub code: i32,
    /// The human-readable error message.
    pub message: String,
    /// Optional structured error data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// One decoded JSON-RPC message.
///
/// This enum is an internal Rust representation. The wire encoder writes regular
/// JSON-RPC objects and never serializes an internal discriminator.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum Message {
    /// A request that expects a response.
    Request {
        /// The request identifier.
        id: Id,
        /// The method name.
        method: String,
        /// The method parameters.
        params: Value,
    },
    /// A notification that does not receive a response.
    Notification {
        /// The method name.
        method: String,
        /// The method parameters.
        params: Value,
    },
    /// A successful response.
    Result {
        /// The request identifier.
        id: Id,
        /// The result value.
        result: Value,
    },
    /// An error response.
    Error {
        /// The request identifier.
        id: Id,
        /// The error object.
        error: ErrorObject,
    },
}

/// A JSON-RPC decoding failure with its response identifier and stable code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsonRpcError {
    /// The identifier to place in the error response.
    pub id: Id,
    /// The JSON-RPC error code.
    pub code: i32,
    /// The protocol-defined error text.
    pub message: String,
}

impl JsonRpcError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            id: Id::Null,
            code: -32600,
            message: message.into(),
        }
    }
}

/// Decodes one JSON-RPC object without interpreting method-specific parameters.
///
/// Unknown envelope members are ignored. Missing `params` becomes an empty object.
/// JSON syntax errors use `-32700`; invalid envelopes use `-32600`.
///
/// # Errors
///
/// Returns [`JsonRpcError`] when the frame is not valid JSON or is not a JSON-RPC 2.0 message.
pub fn decode_jsonrpc(frame: &str) -> Result<Message, JsonRpcError> {
    let root = sonic_rs::from_str::<Value>(frame).map_err(|error| JsonRpcError {
        id: Id::Null,
        code: -32700,
        message: format!("frame is not valid JSON: {error}"),
    })?;

    if root.is_array() {
        let empty = root.as_array().is_some_and(sonic_rs::Array::is_empty);
        return Err(JsonRpcError::invalid(if empty {
            "batch is empty"
        } else {
            "batches are not supported"
        }));
    }

    let Some(mut fields) = root.into_object() else {
        return Err(JsonRpcError::invalid("frame is not a JSON-RPC 2.0 message"));
    };
    let Some(version) = fields.remove(&"jsonrpc") else {
        return Err(JsonRpcError::invalid("frame is not a JSON-RPC 2.0 message"));
    };
    if version.as_str() != Some("2.0") {
        return Err(JsonRpcError::invalid("frame is not a JSON-RPC 2.0 message"));
    }

    let id = fields
        .remove(&"id")
        .as_ref()
        .map(parse_id)
        .transpose()
        .map_err(|()| JsonRpcError::invalid("frame is not a JSON-RPC 2.0 message"))?;

    if let Some(method) = fields.remove(&"method") {
        let Some(method) = method.as_str() else {
            return Err(JsonRpcError::invalid("frame is not a JSON-RPC 2.0 message"));
        };
        if fields.get(&"result").is_some() || fields.get(&"error").is_some() {
            return Err(JsonRpcError::invalid("frame is not a JSON-RPC 2.0 message"));
        }
        let params = fields
            .remove(&"params")
            .unwrap_or_else(|| sonic_rs::json!({}));
        let method = method.to_owned();
        return Ok(match id {
            Some(id) => Message::Request { id, method, params },
            None => Message::Notification { method, params },
        });
    }

    if let Some(result) = fields.remove(&"result") {
        if fields.get(&"error").is_some() {
            return Err(JsonRpcError::invalid("frame is not a JSON-RPC 2.0 message"));
        }
        let Some(id) = id else {
            return Err(JsonRpcError::invalid("frame is not a JSON-RPC 2.0 message"));
        };
        return Ok(Message::Result { id, result });
    }

    if let Some(error) = fields.remove(&"error") {
        let Some(id) = id else {
            return Err(JsonRpcError::invalid("frame is not a JSON-RPC 2.0 message"));
        };
        let Some(error) = parse_error_object(error) else {
            return Err(JsonRpcError::invalid("frame is not a JSON-RPC 2.0 message"));
        };
        return Ok(Message::Error { id, error });
    }

    Err(JsonRpcError::invalid("frame is not a JSON-RPC 2.0 message"))
}

fn parse_id(value: &Value) -> Result<Id, ()> {
    if value.is_null() {
        Ok(Id::Null)
    } else if let Some(value) = value.as_i64() {
        Ok(Id::Integer(value))
    } else if let Some(value) = value.as_str() {
        Ok(Id::String(value.to_owned()))
    } else {
        Err(())
    }
}

fn parse_error_object(value: Value) -> Option<ErrorObject> {
    let mut fields = value.into_object()?;
    let code = fields
        .remove(&"code")?
        .as_i64()
        .and_then(|value| i32::try_from(value).ok())?;
    let message = fields.remove(&"message")?.as_str()?.to_owned();
    let data = fields.remove(&"data");
    Some(ErrorObject {
        code,
        message,
        data,
    })
}

/// Encodes one JSON-RPC message with the exact external envelope shape.
///
/// # Errors
///
/// Returns [`WireError::Frame`] when the message cannot be serialized as JSON.
pub fn encode_jsonrpc(message: &Message) -> Result<String, WireError> {
    match message {
        Message::Request { id, method, params } => sonic_rs::to_string(&RequestEnvelope {
            jsonrpc: "2.0",
            id,
            method,
            params,
        })
        .map_err(|_| WireError::Frame),
        Message::Notification { method, params } => sonic_rs::to_string(&NotificationEnvelope {
            jsonrpc: "2.0",
            method,
            params,
        })
        .map_err(|_| WireError::Frame),
        Message::Result { id, result } => sonic_rs::to_string(&ResultEnvelope {
            jsonrpc: "2.0",
            id,
            result,
        })
        .map_err(|_| WireError::Frame),
        Message::Error { id, error } => sonic_rs::to_string(&ErrorEnvelope {
            jsonrpc: "2.0",
            id,
            error,
        })
        .map_err(|_| WireError::Frame),
    }
}

#[derive(Serialize)]
struct RequestEnvelope<'a> {
    jsonrpc: &'static str,
    id: &'a Id,
    method: &'a str,
    params: &'a Value,
}

#[derive(Serialize)]
struct NotificationEnvelope<'a> {
    jsonrpc: &'static str,
    method: &'a str,
    params: &'a Value,
}

#[derive(Serialize)]
struct ResultEnvelope<'a> {
    jsonrpc: &'static str,
    id: &'a Id,
    result: &'a Value,
}

#[derive(Serialize)]
struct ErrorEnvelope<'a> {
    jsonrpc: &'static str,
    id: &'a Id,
    error: &'a ErrorObject,
}
