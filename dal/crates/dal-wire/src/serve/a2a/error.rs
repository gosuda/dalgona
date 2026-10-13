//! A2A failures and their JSON-RPC and HTTP+JSON encodings.

use sonic_rs::Value;

use crate::a2a::{A2aError, error_for};
use crate::serve::Resp;

/// The version-negotiation failure text.
pub(crate) const VERSION_TEXT: &str = "dalgon speaks A2A 1.0: send the header A2A-Version: 1.0";

/// One A2A failure: its stable error kind plus the product text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Fail {
    /// The stable error kind.
    pub kind: A2aError,
    /// The product text.
    pub message: String,
}

impl Fail {
    /// Builds a failure for one stable reason name.
    pub(crate) fn new(reason: &str, message: impl Into<String>) -> Self {
        Self {
            kind: error_for(reason),
            message: message.into(),
        }
    }

    /// Builds an `INVALID_ARGUMENT` failure.
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::new("INVALID_ARGUMENT", message)
    }

    /// Builds an `INTERNAL` failure.
    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self::new("INTERNAL", message)
    }

    /// Builds a `TASK_NOT_FOUND` failure for one task id.
    pub(crate) fn task_not_found(id: &str) -> Self {
        Self::new("TASK_NOT_FOUND", format!("task {id} was not found"))
    }

    /// Builds the `VERSION_NOT_SUPPORTED` failure.
    pub(crate) fn version() -> Self {
        Self::new("VERSION_NOT_SUPPORTED", VERSION_TEXT)
    }

    /// Returns the `google.rpc.ErrorInfo` detail list.
    fn details(&self) -> Value {
        sonic_rs::json!([{
            "@type": "type.googleapis.com/google.rpc.ErrorInfo",
            "reason": self.kind.reason,
            "domain": "a2a-protocol.org",
            "metadata": {},
        }])
    }

    /// Encodes the JSON-RPC error object.
    pub(crate) fn rpc_error(&self) -> Value {
        sonic_rs::json!({
            "code": self.kind.code,
            "message": self.message.as_str(),
            "data": self.details(),
        })
    }

    /// Encodes the complete JSON-RPC error envelope for one request id.
    pub(crate) fn rpc_envelope(&self, id: &Value) -> Value {
        sonic_rs::json!({"jsonrpc": "2.0", "id": id, "error": self.rpc_error()})
    }

    /// Encodes the HTTP+JSON error body.
    pub(crate) fn rest_body(&self) -> Value {
        sonic_rs::json!({"error": {
            "code": self.kind.http,
            "status": self.kind.status,
            "message": self.message.as_str(),
            "details": self.details(),
        }})
    }

    /// Builds the HTTP+JSON error response with its mapped status.
    pub(crate) fn rest(&self) -> Resp {
        Resp::json(self.kind.http, &self.rest_body())
    }
}

/// Returns true when the `A2A-Version` header or query selects 1.0.
pub(crate) fn version_ok(header: Option<&str>, query: &str) -> bool {
    if let Some(version) = header {
        return version.trim() == "1.0";
    }
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .any(|(name, value)| name.eq_ignore_ascii_case("A2A-Version") && value == "1.0")
}
