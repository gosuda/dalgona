//! The pinned Codex app-server schema fixture, parsed once per process.

use std::sync::OnceLock;

use sonic_rs::{JsonValueTrait, Value};

/// The pinned schema subset embedded at compile time.
const FIXTURE: &str = include_str!("../../tests/fixtures/codex-app-server-schema.json");

/// Returns the parsed schema fixture, or its parse error text.
pub(crate) fn fixture() -> Result<&'static Value, &'static str> {
    static PARSED: OnceLock<Result<Value, String>> = OnceLock::new();
    PARSED
        .get_or_init(|| sonic_rs::from_str(FIXTURE).map_err(|error| error.to_string()))
        .as_ref()
        .map_err(String::as_str)
}

/// Returns whether `method` is listed in one fixture method table.
fn listed(table: &str, method: &str) -> bool {
    match fixture() {
        Ok(schema) => schema
            .get(table)
            .and_then(|entries| entries.get(method))
            .is_some(),
        Err(error) => {
            tracing::error!(%error, "the embedded Codex schema fixture is not valid JSON");
            false
        }
    }
}

/// Returns whether `method` is a pinned client request.
pub(crate) fn is_client_request(method: &str) -> bool {
    listed("clientRequests", method)
}

/// Returns whether `method` is a pinned client notification.
pub(crate) fn is_client_notification(method: &str) -> bool {
    listed("clientNotifications", method)
}
